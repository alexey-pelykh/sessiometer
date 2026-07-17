// Copyright (c) 2026 Oleksii PELYKH
// SPDX-License-Identifier: MIT

// The Settings window's SwiftUI form (issue #268): a native grouped `Form` over the daemon's editable
// tunables + roster labels, plus the app-local notification toggle. It is a PURE view over `SettingsModel`
// — every decision (dirty tracking, validation split, apply outcome) lives in the tested model; this file
// only renders phases and binds drafts. AppKit/SwiftUI, so it stays in the app target (untested), the
// counterpart split to `PanelStatsModel` (tested) vs `StatusPanelView` (app-only).
//
// LAYERING (issue #573, load-bearing): the two surfaces here have DIFFERENT dependencies, so they sit on
// different sides of the load-phase gate. The Notifications toggle is app-local (`UserDefaults`, nil-client
// safe) and renders ALWAYS, above the gate; only the daemon-config surface (tunables + accounts) is gated on
// `config-get` and renders below it. Nesting the toggle inside the gated form — as #268 shipped — made an
// app preference unreachable whenever the daemon was stopped or unconfigured, a diff-invisible UX gap.
//
// Scope (RATIFIED, prd-menubar.md:25 — "edits tunables + labels, never account capture/credentials"): the
// accounts section edits LABELS only; add / remove / capture stay in the CLI (a pointer, never a GUI
// keychain write — AC 5/6). macOS 13 floor: `ObservableObject` (not `@Observable`), `.formStyle(.grouped)`
// + `LabeledContent` (both 13.0), `@FocusState` ok.

import SwiftUI

struct SettingsView: View {
    @ObservedObject var model: SettingsModel

    var body: some View {
        VStack(spacing: 0) {
            // ONE always-present grouped Form: the app-local Notifications section on top (daemon-independent,
            // so it renders in every load phase — issue #573), then the daemon-config surface conditionally
            // below it (loading / honest-disconnected / loaded).
            Form {
                notificationsSection
                daemonConfig
            }
            .formStyle(.grouped)
            Divider()
            footer
        }
        .frame(minWidth: 440, idealWidth: 460, minHeight: 420, idealHeight: 560)
        // No `.task { load() }` here: loads are driven SOLELY by SettingsWindowController.show() (first open
        // AND reopens), so the form never races two config-get fetches on first open.
    }

    // MARK: - Notifications (app-local, always visible — independent of the daemon load phase)

    /// The app-local notification toggle (issue #267). A pure `UserDefaults` preference — fully
    /// daemon-independent (`SettingsModel` supports a nil client) — so it sits ABOVE the load-phase gate and
    /// renders in EVERY phase (loading / honest-disconnected / no-config / loaded): the one control an
    /// operator can always reach, even with the daemon stopped or on a fresh install (issue #573).
    private var notificationsSection: some View {
        Section {
            Toggle("Notify on account swaps and exhaustion", isOn: $model.notificationsEnabled)
        } header: {
            Text("Notifications")
        } footer: {
            Text("A local macOS notification when the active account changes or every account is exhausted. "
                + "This is an app preference — it isn’t part of the daemon configuration below.")
        }
    }

    // MARK: - Daemon configuration (load-phase gated, shown BELOW the always-present Notifications section)

    /// The daemon-config surface (tunables + accounts), gated on the `config-get` load phase and rendered
    /// below the always-present Notifications section (issue #573): a loading placeholder, the honest-
    /// disconnected / no-config states (AC 7), or the editable tunables + accounts.
    @ViewBuilder
    private var daemonConfig: some View {
        switch model.loadPhase {
        case .idle, .loading:
            loadingSection
        case .failed(let failure):
            loadFailureSection(failure)
        case .loaded:
            tunableSections
            accountsSection
        }
    }

    /// The loading placeholder — headed "Daemon Configuration" so a slow first fetch reads as the daemon
    /// area filling in below the (already usable) Notifications toggle, not a stalled window.
    private var loadingSection: some View {
        Section("Daemon Configuration") {
            HStack(spacing: 8) {
                Spacer()
                ProgressView().controlSize(.small)
                Text("Loading settings…").foregroundStyle(.secondary)
                Spacer()
            }
            .padding(.vertical, 8)
        }
    }

    /// The honest-disconnected / no-config states (AC 7) — never a blank or fabricated form. Headed "Daemon
    /// Configuration" so the failure clearly scopes to the daemon surface below Notifications (the toggle
    /// stays live), matching the Notifications footer's "the daemon configuration below".
    @ViewBuilder
    private func loadFailureSection(_ failure: ConfigFailure) -> some View {
        Section("Daemon Configuration") {
            VStack(spacing: 10) {
                Image(systemName: "exclamationmark.triangle")
                    .font(.largeTitle)
                    .foregroundStyle(.secondary)
                    .accessibilityHidden(true)  // decorative — the headline + detail carry the state for VoiceOver
                Text(loadFailureHeadline(failure)).font(.headline)
                Text(loadFailureDetail(failure))
                    .font(.callout)
                    .foregroundStyle(.secondary)
                    .multilineTextAlignment(.center)
                Button("Try Again") { Task { await model.load() } }
                    .padding(.top, 4)
            }
            .frame(maxWidth: .infinity)
            .padding(.vertical, 12)
        }
    }

    private func loadFailureHeadline(_ failure: ConfigFailure) -> String {
        switch failure {
        case .daemonError(ConfigGetErrorReason.noConfig): return "No configuration yet"
        case .daemonError(ConfigGetErrorReason.unreadable): return "Configuration unreadable"
        case .daemonError: return "Configuration unavailable"
        case .transport, .unavailable: return "Sessiometer isn’t connected"
        case .undecodable: return "Unexpected response"
        }
    }

    private func loadFailureDetail(_ failure: ConfigFailure) -> String {
        switch failure {
        case .daemonError(ConfigGetErrorReason.noConfig):
            return "Capture your first account with the sessiometer CLI, then reopen Settings."
        case .daemonError(ConfigGetErrorReason.unreadable):
            return "Sessiometer’s configuration file exists but couldn’t be read — it may be malformed. "
                + "Fix or re-capture it with the sessiometer CLI, then reopen Settings."
        case .daemonError(let reason):
            return "The daemon reported: \(reason)."
        case .transport, .unavailable:
            return "Start the sessiometer daemon, then try again. Settings edits the running daemon’s configuration."
        case .undecodable:
            return "The daemon sent a reply this app doesn’t understand — it may be a different version."
        }
    }

    // MARK: - Daemon tunables + accounts (the `.loaded` daemon-config sections)

    /// The five grouped tunable sections (issue #268), in display order.
    private var tunableSections: some View {
        ForEach(TunableField.Section.allCases) { section in
            Section(section.title) {
                ForEach(section.fields) { field in
                    tunableRow(field)
                }
            }
        }
    }

    /// The roster label-edit section (issue #268) — LABELS only; add / remove / capture stay in the CLI.
    private var accountsSection: some View {
        Section {
            ForEach(model.accounts, id: \.accountUuid) { account in
                accountRow(account)
            }
        } header: {
            Text("Accounts")
        } footer: {
            Text("Rename accounts here. Add, remove, or re-authenticate accounts with the sessiometer CLI — "
                + "the settings window never touches credentials.")
        }
    }

    private func tunableRow(_ field: TunableField) -> some View {
        let copy = Self.copy(for: field)
        return VStack(alignment: .leading, spacing: 2) {
            LabeledContent(copy.title) {
                TextField(copy.title, text: tunableBinding(field))
                    .labelsHidden()
                    .multilineTextAlignment(.trailing)
                    .textFieldStyle(.roundedBorder)
                    .frame(width: 96)
                    .help(copy.help)
            }
            if let error = model.fieldErrors[field] {
                Text(error)
                    .font(.caption)
                    .foregroundStyle(.red)
            }
        }
    }

    private func accountRow(_ account: AccountView) -> some View {
        LabeledContent {
            HStack(spacing: 8) {
                TextField("Label", text: labelBinding(account.accountUuid))
                    .textFieldStyle(.roundedBorder)
                    .frame(width: 160)
                Text(account.enabled ? "Active" : "Parked")
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }
        } label: {
            Text(account.enabled ? "Account" : "Account (parked)")
        }
    }

    // MARK: - Footer: Save + apply status

    private var footer: some View {
        HStack(spacing: 12) {
            applyStatus
            Spacer()
            Button("Save") { Task { await model.apply() } }
                .keyboardShortcut("s", modifiers: .command)
                .disabled(!saveEnabled)
        }
        .padding(12)
    }

    /// Save is live only when there is a clean edit to submit and no apply is in flight.
    private var saveEnabled: Bool {
        guard model.isDirty else { return false }
        if case .applying = model.applyPhase { return false }
        return true
    }

    @ViewBuilder
    private var applyStatus: some View {
        switch model.applyPhase {
        case .idle:
            if model.restartPending { restartBanner }
        case .applying:
            HStack(spacing: 6) { ProgressView().controlSize(.small); Text("Saving…") }
                .foregroundStyle(.secondary)
        case .applied(let effect):
            switch effect {
            case .restartRequired: restartBanner
            case .live:
                Label("Saved", systemImage: "checkmark.circle").foregroundStyle(.green)
            case .unchanged:
                // The daemon applied nothing because the submitted values already matched (a stale baseline
                // or a concurrent change) — "Saved" would imply a write that didn't happen.
                Label("Already up to date", systemImage: "checkmark.circle").foregroundStyle(.green)
            }
        case .invalidInput:
            Label("Fix the highlighted fields.", systemImage: "exclamationmark.triangle")
                .foregroundStyle(.orange)
        case .rejected(let reason, let detail):
            Label(rejectionText(reason, detail), systemImage: "xmark.octagon")
                .foregroundStyle(.red)
                .help(detail ?? "")
        case .failed(let failure):
            Label(applyFailureText(failure), systemImage: "bolt.horizontal.circle")
                .foregroundStyle(.red)
        }
    }

    private var restartBanner: some View {
        Label("Saved — restart the daemon to apply.", systemImage: "arrow.clockwise.circle")
            .foregroundStyle(.orange)
    }

    private func rejectionText(_ reason: ConfigSetRejection, _ detail: String?) -> String {
        switch reason {
        case .invalid: return detail ?? "That value isn’t allowed."
        case .unknownAccount: return "That account is no longer in the roster — reopen Settings."
        case .noConfig: return "No configuration to update — capture an account with the CLI first."
        case .configUnreadable: return "The configuration file couldn’t be read — it was left unchanged."
        case .saveFailed: return "The configuration couldn’t be saved — the old file is intact."
        case .unavailable: return "The daemon can’t change configuration right now."
        }
    }

    private func applyFailureText(_ failure: ConfigFailure) -> String {
        switch failure {
        case .transport, .unavailable: return "Not saved — Sessiometer isn’t connected."
        case .undecodable: return "Not saved — the daemon sent an unexpected reply."
        case .daemonError(let reason): return "Not saved — \(reason)."
        }
    }

    // MARK: - Bindings

    private func tunableBinding(_ field: TunableField) -> Binding<String> {
        Binding(get: { model.draft(for: field) }, set: { model.setDraft($0, for: field) })
    }

    private func labelBinding(_ uuid: String) -> Binding<String> {
        Binding(get: { model.labelDraft(for: uuid) }, set: { model.setLabelDraft($0, for: uuid) })
    }

    // MARK: - Inferred per-field copy

    /// Human label (with unit) + hover help for a tunable. INFERRED from the field name + `src/config.rs`
    /// semantics — hq specifies no per-field lexicon (`prd-menubar.md`:25), so this is refinable product
    /// copy, deliberately kept in the view (not the tested model). The daemon is the validation authority;
    /// these strings never gate a value.
    static func copy(for field: TunableField) -> (title: String, help: String) {
        switch field {
        case .pollSecs: return ("Poll interval (s)", "How often the daemon checks usage.")
        case .exhaustedPollSecs: return ("Exhausted poll (s)", "Slower poll while every account is exhausted.")
        case .nearLimitPollSecs: return ("Near-limit poll (s)", "Faster poll when an account is close to a limit.")
        case .cooldownSecs: return ("Swap cooldown (s)", "Minimum time between automatic swaps.")
        case .targetMaxSessionUsage: return ("Target session usage (%)", "Aim to keep session usage below this.")
        case .sessionTrigger: return ("Session trigger (%)", "Swap when session usage reaches this.")
        case .weeklyTrigger: return ("Weekly trigger (%)", "Swap when weekly usage reaches this.")
        case .sessionBlindSwapSecs: return ("Blind swap delay (s)", "Wait this long before a preemptive swap while usage is blind (429).")
        case .sessionBlindRiskBand: return ("Blind risk band (%)", "Retained usage that counts as risky while blind.")
        case .sessionVelocityHorizonSecs: return ("Velocity horizon (s)", "Look-ahead window for the usage-velocity projection.")
        case .sessionVelocityMinProjectAbove: return ("Velocity floor (%)", "Only project a swap when usage is above this.")
        case .sessionVelocityEmaAlphaPct: return ("Velocity smoothing (%)", "EMA smoothing factor for usage velocity.")
        case .monitor401N: return ("401 tolerance", "Consecutive 401s before an account is treated as needing re-login.")
        case .monitorRecoveryM: return ("Recovery threshold", "Consecutive good checks before an account is considered recovered.")
        }
    }
}
