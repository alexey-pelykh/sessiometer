// Copyright (c) 2026 Oleksii PELYKH
// SPDX-License-Identifier: MIT

// The in-app "Capture active account" affordance's model (issue #360): the thin `@MainActor` shell that
// owns the ONE-SHOT capture exchange over the #358 control-command transport and exposes its
// idle → pending → done → failed phase to the SwiftUI panel. It is the capture SIBLING of the read-only
// `WatchStatusStore` (#324): the store PROJECTS the daemon's `watch` stream; this model SENDS one write
// command and renders its redacted ack — it never touches the roster. The captured row arrives on its own
// via the `watch` snapshot (command → daemon → `watch` → UI); the affordance never inserts a row itself
// (issue #360 AC), and on success it just returns to idle.
//
// AppKit-free BY DESIGN (Foundation + Combine + os only) so it compiles into the headless `MenubarTests`
// bundle and its phase transitions are driven hermetically against a fake connector — no socket, no live
// daemon (the same testability split the store uses). The panel-key re-assertion — a non-activating
// `FloatingPanel` (hidesOnDeactivate=false) needs `makeKey` re-asserted when the label field takes focus,
// or the `TextField` cannot accept keystrokes — is injected as a plain closure (`panelKeyRequest`) so this
// model names no AppKit type; `StatusItemController` supplies `{ panel.makeKey() }`.
//
// NO credential handling of any kind (C-001 / issue #15): it serializes a verb + an optional non-secret
// label and receives a redacted ack — the whole channel carries no token, email, or oauth blob.

import Combine
import Foundation
import os

private let captureLog = Logger(subsystem: "org.sessiometer.menubar", category: "capture")

/// The `capture` control-command request (issue #359 wire): `{"cmd":"capture","label":"<label>"}`, or
/// `{"cmd":"capture"}` when the operator left the label blank — an OMITTED key, so the daemon derives the
/// handle from the account UUID (never the email — issue #15 / #134). A `nil` `label` omits the key by
/// construction (`JSONEncoder` drops a nil optional), so a blank field is a label-LESS capture, not an
/// empty-string one. The verb + label are the only bytes on the wire — no credential (C-001).
struct CaptureCommand: Encodable, Sendable {
    let cmd = "capture"
    let label: String?
}

/// Why a capture did not succeed — a non-secret verdict the panel maps to human copy
/// (`StatusPanelFormat.captureErrorText`). Unifies the daemon's redacted refusal, the shared error ack,
/// the transport's bounded failures, an undecodable ack (wire drift), and the no-client degraded case.
enum CaptureFailure: Equatable {
    /// The daemon refused with a known redacted machine reason (#359).
    case rejected(CaptureRejection)
    /// The shared redacted `{"error":…}` ack (e.g. `unauthorized`) — the same-user local peer should never
    /// see it, but it is surfaced honestly rather than swallowed.
    case daemonError(String)
    /// A bounded transport failure (#358 `ControlCommandError`): no daemon (refused), a wedged daemon
    /// (timed out / closed before ack), or an I/O / encode fault.
    case transport(ControlCommandError)
    /// The ack line did not match the capture wire contract (a buggy / drifted daemon) — degrade loudly.
    case undecodable
    /// No control client — the daemon control-socket path would not resolve (sandboxed / home unresolved),
    /// so capture is unavailable from this app instance (mirrors the watch transport's loud degrade).
    case unavailable
}

/// The affordance's interaction phase. `done` and `failed` carry the structured facts the panel renders
/// via `StatusPanelFormat` (so the copy stays PURE + tested); the view never invents its own strings.
enum CapturePhase: Equatable {
    case idle
    case pending
    case done(label: String)
    case failed(CaptureFailure)

    /// Whether a capture is in flight — one half of the panel-retain predicate (`isBusy`).
    var isPending: Bool { if case .pending = self { return true } else { return false } }
}

@MainActor
final class AccountCaptureModel: ObservableObject {
    /// The short-lived control-command client, or `nil` when the socket path would not resolve — in which
    /// case a capture attempt short-circuits to `.failed(.unavailable)` (honest, never a dead button).
    private let client: ControlCommandClient?

    /// The current interaction phase the panel observes.
    @Published private(set) var phase: CapturePhase = .idle

    /// Whether the operator is editing the label field. Set by the view's `@FocusState` bridge; combined
    /// with `phase == .pending` into `isBusy` — the predicate that GATES the panel's outside-click dismiss
    /// (`StatusItemController`) so a typed-but-unsubmitted label or an in-flight capture is never lost to an
    /// accidental click outside (issue #360 AC).
    @Published private(set) var isEditing = false

    /// Whether the operator explicitly asked to add an account from the status-item right-click menu
    /// (issue #394). When true, the panel presents the capture surface OVER a populated roster — the only
    /// populated-panel path to capture now that the persistent add-account bar is gone (capture inline is
    /// empty-roster / first-run only). The capture surface REUSES the panel's own key/first-responder
    /// plumbing (this model's `panelKeyRequest`, the label-field focus bridge) rather than a second popover
    /// / window / alert, so the nonactivating-panel first-responder problem the panel already solved is not
    /// re-opened. Reset when the panel is dismissed (`dismissCaptureSurface`), so a subsequent left-click
    /// opens the normal roster, never a lingering capture surface.
    @Published private(set) var captureSurfaceRequested = false

    /// Re-assert the host panel as key window. A non-activating `FloatingPanel` can lose key when focus
    /// moves, leaving the SwiftUI `TextField` unable to accept keystrokes; the controller injects
    /// `{ panel.makeKey() }` here, invoked when the field takes focus. A plain closure so this model names
    /// no AppKit type (headless-test-compatible).
    var panelKeyRequest: (() -> Void)?

    init(client: ControlCommandClient?) {
        self.client = client
    }

    /// The panel-retain predicate: an outside click must NOT dismiss while the operator is mid-edit or a
    /// capture is in flight (issue #360 AC — the active field / in-flight capture is not lost).
    var isBusy: Bool { isEditing || phase.isPending }

    /// The field took / lost focus (driven by the view's `@FocusState`). On focus, re-assert the panel key
    /// so keystrokes land, and clear any lingering error so a fresh edit starts from a clean field.
    func setEditing(_ editing: Bool) {
        isEditing = editing
        if editing {
            panelKeyRequest?()
            if case .failed = phase { phase = .idle }   // a fresh edit clears a prior error
        }
    }

    /// Cancel an edit (the Esc path): drop focus intent and clear a lingering error/done back to idle so an
    /// outside click can dismiss again. A no-op while a capture is in flight (that must run to completion).
    func cancelEditing() {
        guard !phase.isPending else { return }
        isEditing = false
        phase = .idle
    }

    /// Present the capture surface in the panel — the status-item "Add account…" menu path (issue #394).
    /// Sets the flag the panel observes; the controller opens (and keys) the panel alongside, and the label
    /// field's focus bridge re-asserts key when it takes focus, so keystrokes land through the SAME plumbing
    /// the empty-roster onboarding field already uses.
    func requestCaptureSurface() {
        captureSurfaceRequested = true
    }

    /// Dismiss the capture surface back to the normal panel (called on every panel close). Clears the flag
    /// so the next open shows the roster, and releases the outside-click retain predicate via `cancelEditing`
    /// (a no-op while a capture is in flight — that runs to completion; the flag still clears so a reopened
    /// panel shows the roster while the capture settles in the background and its row arrives via `watch`).
    func dismissCaptureSurface() {
        captureSurfaceRequested = false
        cancelEditing()
    }

    /// Submit a capture of the currently-active account under `rawLabel` (trimmed; blank → label-less, so
    /// the daemon derives the handle from the account UUID). Renders pending → done / failed. Never mutates
    /// a roster — the captured row arrives via the `watch` snapshot; on `done` the affordance auto-resets to
    /// idle after a short confirmation beat (mirroring the panel's other transient confirmations). A
    /// double-submit while already in flight is ignored.
    func capture(rawLabel: String) async {
        guard !phase.isPending else { return }
        let trimmed = rawLabel.trimmingCharacters(in: .whitespacesAndNewlines)
        let label = trimmed.isEmpty ? nil : trimmed

        guard let client else {
            phase = .failed(.unavailable)
            return
        }

        phase = .pending
        let result = await client.send(CaptureCommand(label: label))

        switch result {
        case .failure(let error):
            captureLog.error("capture: transport failure — \(String(describing: error), privacy: .public)")
            phase = .failed(.transport(error))
        case .success(let line):
            do {
                switch try CaptureAck.decode(line) {
                case .captured(let ackLabel, _):
                    phase = .done(label: ackLabel)
                    scheduleIdleReset(for: ackLabel)
                case .rejected(let reason):
                    captureLog.error("capture: rejected — \(reason.rawValue, privacy: .public)")
                    phase = .failed(.rejected(reason))
                case .error(let reason):
                    captureLog.error("capture: daemon error — \(reason, privacy: .public)")
                    phase = .failed(.daemonError(reason))
                }
            } catch {
                captureLog.error("capture: undecodable ack — \(String(describing: error), privacy: .public)")
                phase = .failed(.undecodable)
            }
        }
    }

    /// Reset a `done(label:)` phase back to idle after a short confirmation beat — but only if the phase is
    /// STILL that same `done` (a new capture / edit supersedes it), so a racing transition is never
    /// clobbered. The window mirrors the panel's other transient confirmations.
    private func scheduleIdleReset(for label: String) {
        Task { [weak self] in
            try? await Task.sleep(for: .seconds(1.8))
            guard let self else { return }
            if self.phase == .done(label: label) { self.phase = .idle }
        }
    }
}
