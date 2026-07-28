// Copyright (c) 2026 Oleksii PELYKH
// SPDX-License-Identifier: MIT

// The status panel's in-app capture surface (issue #360), split out of `StatusPanelView` by #640: the shared
// affordance (a capture button plus an inline operator-label field) and the card that hosts it. Its TWO entry
// points — the empty-roster / first-run onboarding state and the status-item "Add account…" surface (issue #394) —
// differ only in title. The client still originates NO credential (C-005 held): a verb plus a non-secret label out
// over the #358 control socket, a redacted ack back. It never inserts the captured row itself — that arrives on its
// own via the `watch` snapshot.

import SwiftUI

// MARK: - Capture affordance

/// The in-app capture affordance (issue #360) — a "Capture active account" button + an inline operator-
/// label field, hosted by BOTH capture surfaces: the empty-roster / first-run onboarding card and the
/// status-item "Add account…" menu surface (issue #394). It sends `{"cmd":"capture","label":…}` over the
/// #358 control-command transport (via `AccountCaptureModel`) and renders the redacted ack's
/// idle → pending → done → error phase. It NEVER inserts the captured row — that arrives on its own via the
/// `watch` snapshot (issue #360 AC); on success the affordance just returns to idle. The client still
/// originates NO credential (C-005 held): a verb + non-secret label out, a redacted ack back.
///
/// Capture snapshots the account currently logged into Claude Code — it is NOT an account picker. To add a
/// DIFFERENT account the operator runs `claude /login` first, then captures (the honest scope boundary,
/// surfaced as the secondary hint). An already-active-and-rostered account is an idempotent refresh.
private struct CaptureAffordance: View {
    /// The panel's uniform Dynamic Type multiplier (issue #756), injected once by `StatusPanelView`.
    @Environment(\.panelScale) private var scale
    @EnvironmentObject private var capture: AccountCaptureModel
    @State private var label = ""
    @FocusState private var fieldFocused: Bool

    var body: some View {
        // The prominent, stacked treatment — the field, the primary Capture button, the status line, then
        // the scope hint. Both capture surfaces (#360 onboarding, #394 menu) use this one treatment.
        VStack(alignment: .leading, spacing: 9 * scale) {
            field
            HStack(spacing: 8 * scale) {
                button
                Spacer(minLength: 0)
            }
            status
            Text("To add a different account, run claude /login first, then capture.")
                .font(.panel(10.5, scale: scale))
                .foregroundStyle(.tertiary)
                .fixedSize(horizontal: false, vertical: true)
        }
        // Bridge the field's focus to the model — the panel-retain predicate (`isBusy`) gates the outside-
        // click dismiss on it, and focusing re-asserts the panel key so keystrokes land (issue #360).
        .onChange(of: fieldFocused) { focused in capture.setEditing(focused) }
        // Esc cancels: resign focus + clear back to idle so an outside click can dismiss again (Return
        // submits via the field's `.onSubmit`).
        .onExitCommand {
            fieldFocused = false
            capture.cancelEditing()
        }
        // A completed capture consumed its label; blank the field so the next capture starts clean.
        .onChange(of: capture.phase) { phase in
            if case .done = phase { label = "" }
        }
    }

    /// The label field — the placeholder invites an OPTIONAL label; blank → the daemon derives the handle
    /// from the account UUID (never the email). Disabled while a capture is in flight.
    private var field: some View {
        TextField("e.g. Work, Personal", text: $label)
            .textFieldStyle(.roundedBorder)
            .font(.panel(12, scale: scale))
            .focused($fieldFocused)
            .onSubmit(submit)
            .disabled(capture.phase.isPending)
            .accessibilityLabel("Account label, optional")
    }

    /// The "Capture active account" button — the primary action; disabled and spinner-labelled while
    /// pending (a real pending state is honest now that capture is a real daemon-routed action).
    private var button: some View {
        Button(action: submit) {
            if capture.phase.isPending {
                HStack(spacing: 5 * scale) {
                    ProgressView().controlSize(PanelTypeScale.controlSize(for: scale))
                    Text(StatusPanelFormat.capturePendingText)
                }
            } else {
                Label("Capture active account", systemImage: "rectangle.badge.plus")
            }
        }
        .font(.panel(12, .semibold, scale: scale))
        .controlSize(PanelTypeScale.controlSize(for: scale))
        .buttonStyle(.borderedProminent)
        .disabled(capture.phase.isPending)
        .accessibilityLabel(capture.phase.isPending ? "Capturing the active account"
                                                     : "Capture the active account")
    }

    /// The done / error status line — rendered from the PURE `StatusPanelFormat` copy, never a string the
    /// view invents. Pending is shown on the button itself; idle has no status.
    @ViewBuilder
    private var status: some View {
        switch capture.phase {
        case .idle, .pending:
            EmptyView()
        case .done(let doneLabel):
            Label(StatusPanelFormat.captureDoneText(label: doneLabel), systemImage: "checkmark.circle.fill")
                .font(.panel(11, scale: scale))
                .foregroundStyle(.green)
                .lineLimit(1)
                .truncationMode(StatusPanelFormat.identityElision.truncationMode)
        case .failed(let failure):
            Label(StatusPanelFormat.captureErrorText(failure), systemImage: "exclamationmark.triangle.fill")
                .font(.panel(11, scale: scale))
                .foregroundStyle(.red)
                .fixedSize(horizontal: false, vertical: true)
        }
    }

    /// Submit a capture of the currently-active account under the field's label (blank → the daemon derives
    /// the handle). The model owns the pending → done / failed transitions.
    private func submit() {
        let text = label
        Task { await capture.capture(rawLabel: text) }
    }
}

// MARK: - Capture card

/// The capture card — an explanatory title + line, plus the shared `CaptureAffordance`. Its TWO entry
/// points differ only in `title`: the empty-roster / first-run onboarding state (issue #326 / #360:
/// "Capture your first account", visually distinct from daemon-down) and the status-item "Add account…"
/// menu surface (issue #394: "Add account", the populated-panel path now that the persistent capture bar
/// is gone). The capture mechanics + honest pending → done → error are identical either way — the affordance
/// sends the command over the #358 transport and renders the redacted ack; the captured row then arrives on
/// its own via the `watch` snapshot (the app still originates no credential — a verb + label out, a redacted
/// ack back).
struct CaptureCard: View {
    /// The panel's uniform Dynamic Type multiplier (issue #756), injected once by `StatusPanelView`.
    @Environment(\.panelScale) private var scale
    let title: String
    @Environment(\.colorScheme) private var colorScheme

    var body: some View {
        VStack(alignment: .leading, spacing: 9 * scale) {
            Text(title)
                .font(.panel(style: .subheadline, .semibold, scale: scale))
            Text("Capture the account you’re signed into — the daemon adds it to the roster and starts tracking it here.")
                .font(.panel(style: .caption1, scale: scale))
                .foregroundStyle(.secondary)
                .fixedSize(horizontal: false, vertical: true)

            CaptureAffordance()
        }
        .padding(12 * scale)
        .frame(maxWidth: .infinity, alignment: .leading)
        // Mock `--card-bg` neutral fill (#388) — replaces a washed `Color.secondary.opacity(0.08)`.
        .background(RoundedRectangle(cornerRadius: 10 * scale).fill(Color.panelFill(.card, dark: colorScheme == .dark)))
    }
}
