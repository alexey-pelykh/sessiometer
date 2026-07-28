// Copyright (c) 2026 Oleksii PELYKH
// SPDX-License-Identifier: MIT

// The Settings window's SwiftUI form (issue #268): a native grouped `Form` over the daemon's editable
// tunables + roster labels, plus the app-local notification toggle. It is a PURE view over `SettingsModel`
// — every decision (dirty tracking, validation split, apply outcome) lives in the tested model; this file
// only renders phases and binds drafts. AppKit/SwiftUI, so it stays in the app target, the counterpart
// split to `PanelStatsModel` (tested) vs `StatusPanelView` (app-only).
//
// COVERAGE (issue #762). "Untested" used to be true of this file wholesale. It is no longer: every
// operator-visible STRING, and every layout constant a text budget is derived from, now lives in
// `SettingsFormat` (`SettingsModel.swift`) — the Foundation-only layer the headless `MenubarTests` bundle
// already compiles — where `SettingsTextMetricsTests` covers and measures them. Read every
// `SettingsFormat.` reference below as a LINK, not a copy: this file deliberately holds NO
// operator-visible literal of its own, so changing a label or a width means changing it there, which is
// what reddens the gate. (The only string literals remaining below are the five SF Symbol NAMES, the
// `"s"` ⌘S key equivalent, and the `?? ""` empty-tooltip fallback — none of which is text an operator
// reads. The bare `spacing:` / `padding:` numbers that remain are cosmetic ones no budget charges.)
//
// The rule to keep: a new string, or a frame constant any budget would charge, goes in `SettingsFormat`
// FIRST and is referenced from here. Adding it as a literal is not caught by any compiler — it just
// silently leaves the coverage claim above false, which is exactly how the first draft of this seam
// shipped 14 strings behind a header that said they had all moved (`SettingsFormat` § static section
// copy records that).
//
// What is still genuinely unreachable from a headless bundle — the `Form` chrome, the accessibility
// tree, the rendered pixels — is enumerated with its route, as a cardinality rather than a gesture, in
// `SettingsTextMetricsTests`' header (issue #762 AC-4).
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
    /// The launch-at-login model (issue #170) — app-local (`SMAppService.mainApp`, no daemon dependency, no
    /// credential), so it drives the always-visible "General" section above the daemon-config gate. The SAME
    /// instance the not-running panel card observes (app-retained, shared via `main.swift`), so the Settings
    /// toggle and the panel's Start affordance never disagree about registration state.
    @ObservedObject var loginItem: LoginItemModel

    var body: some View {
        VStack(spacing: 0) {
            // ONE always-present grouped Form: the app-local General (launch-at-login) + Notifications sections
            // on top (daemon-independent, so they render in every load phase — issue #573), then the daemon-
            // config surface conditionally below them (loading / honest-disconnected / loaded).
            Form {
                launchAtLoginSection
                notificationsSection
                daemonConfig
            }
            .formStyle(.grouped)
            Divider()
            footer
        }
        .frame(minWidth: CGFloat(SettingsFormat.windowMinContentWidth),
               idealWidth: CGFloat(SettingsFormat.windowContentWidth),
               minHeight: CGFloat(SettingsFormat.windowMinContentHeight),
               idealHeight: CGFloat(SettingsFormat.windowContentHeight))
        // No `.task { load() }` here: loads are driven SOLELY by SettingsWindowController.show() (first open
        // AND reopens), so the form never races two config-get fetches on first open.
    }

    // MARK: - General: launch at login (app-local, always visible — independent of the daemon load phase)

    /// The app login-item toggle (issue #170). Like Notifications it is app-local — `SMAppService.mainApp`
    /// registration carries no daemon dependency and no credential (issue #15) — so it sits ABOVE the load-phase
    /// gate and renders in EVERY phase, reachable even with the daemon stopped. Turning it on registers the app
    /// as a login item; `.requiresApproval` (the user must still approve it in System Settings) reads ON with an
    /// inline hint + a deep-link, never a silent failure. This governs the menu-bar APP only — starting the
    /// daemon at login is the separate Start affordance on the not-running panel card (the #170 keystone
    /// decouples the two owners).
    private var launchAtLoginSection: some View {
        Section {
            Toggle(SettingsFormat.launchAtLoginToggleTitle, isOn: launchAtLoginBinding)
            if loginItem.needsApproval {
                VStack(alignment: .leading, spacing: 4) {
                    Text(SettingsFormat.launchAtLoginApprovalHint)
                        .font(.caption)
                        .foregroundStyle(.secondary)
                    Button(SettingsFormat.launchAtLoginApprovalButtonTitle) { loginItem.openLoginItemsSettings() }
                        .font(.caption)
                }
            }
        } header: {
            Text(SettingsFormat.generalSectionTitle)
        } footer: {
            Text(SettingsFormat.generalSectionFooter)
        }
    }

    /// The launch-at-login toggle's binding: reads the model's derived on-state, and writes the register/
    /// unregister INTENT (`setLaunchAtLogin`, idempotent + status-refreshing in the model). A hand-built
    /// `Binding` rather than `$loginItem.x` because the set is an intent that re-reads the true OS status, not
    /// a stored-property write.
    private var launchAtLoginBinding: Binding<Bool> {
        Binding(get: { loginItem.launchAtLoginEnabled }, set: { loginItem.setLaunchAtLogin($0) })
    }

    // MARK: - Notifications (app-local, always visible — independent of the daemon load phase)

    /// The app-local notification toggle (issue #267). A pure `UserDefaults` preference — fully
    /// daemon-independent (`SettingsModel` supports a nil client) — so it sits ABOVE the load-phase gate and
    /// renders in EVERY phase (loading / honest-disconnected / no-config / loaded): the one control an
    /// operator can always reach, even with the daemon stopped or on a fresh install (issue #573).
    private var notificationsSection: some View {
        Section {
            Toggle(SettingsFormat.notificationsToggleTitle, isOn: $model.notificationsEnabled)
        } header: {
            Text(SettingsFormat.notificationsSectionTitle)
        } footer: {
            Text(SettingsFormat.notificationsSectionFooter)
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
        Section(SettingsFormat.daemonConfigSectionTitle) {
            HStack(spacing: 8) {
                Spacer()
                ProgressView().controlSize(.small)
                Text(SettingsFormat.loadingText).foregroundStyle(.secondary)
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
        Section(SettingsFormat.daemonConfigSectionTitle) {
            VStack(spacing: 10) {
                Image(systemName: "exclamationmark.triangle")
                    .font(.largeTitle)
                    .foregroundStyle(.secondary)
                    .accessibilityHidden(true)  // decorative — the headline + detail carry the state for VoiceOver
                Text(SettingsFormat.loadFailureHeadline(failure)).font(.headline)
                Text(SettingsFormat.loadFailureDetail(failure))
                    .font(.callout)
                    .foregroundStyle(.secondary)
                    .multilineTextAlignment(.center)
                Button(SettingsFormat.retryButtonTitle) { Task { await model.load() } }
                    .padding(.top, 4)
            }
            .frame(maxWidth: .infinity)
            .padding(.vertical, 12)
        }
    }

    // MARK: - Daemon tunables + accounts (the `.loaded` daemon-config sections)

    /// The six grouped tunable sections (issues #268, #692), in display order.
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
            Text(SettingsFormat.accountsSectionTitle)
        } footer: {
            Text(SettingsFormat.accountsSectionFooter)
        }
    }

    private func tunableRow(_ field: TunableField) -> some View {
        let copy = SettingsFormat.copy(for: field)
        return VStack(alignment: .leading, spacing: 2) {
            LabeledContent(copy.title) {
                TextField(copy.title, text: tunableBinding(field))
                    .labelsHidden()
                    .multilineTextAlignment(.trailing)
                    .textFieldStyle(.roundedBorder)
                    .frame(width: CGFloat(SettingsFormat.tunableFieldWidth))
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
            HStack(spacing: CGFloat(SettingsFormat.accountRowInterElementSpacing)) {
                TextField(SettingsFormat.accountLabelFieldPlaceholder, text: labelBinding(account.accountUuid))
                    .textFieldStyle(.roundedBorder)
                    .frame(width: CGFloat(SettingsFormat.accountLabelFieldWidth))
                Text(SettingsFormat.accountStateCue(enabled: account.enabled))
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }
        } label: {
            Text(SettingsFormat.accountRowTitle(enabled: account.enabled))
        }
    }

    // MARK: - Footer: Save + apply status

    private var footer: some View {
        HStack(spacing: CGFloat(SettingsFormat.footerInterElementSpacing)) {
            applyStatus
            Spacer()
            Button(SettingsFormat.saveTitle) { Task { await model.apply() } }
                .keyboardShortcut("s", modifiers: .command)
                .disabled(!SettingsFormat.saveEnabled(isDirty: model.isDirty, applyPhase: model.applyPhase))
        }
        .padding(CGFloat(SettingsFormat.footerPadding))
    }

    @ViewBuilder
    private var applyStatus: some View {
        switch model.applyPhase {
        case .idle:
            if model.restartPending { restartBanner }
        case .applying:
            HStack(spacing: 6) { ProgressView().controlSize(.small); Text(SettingsFormat.savingText) }
                .foregroundStyle(.secondary)
        case .applied(let effect):
            switch effect {
            case .restartRequired: restartBanner
            case .live:
                Label(SettingsFormat.savedText, systemImage: "checkmark.circle").foregroundStyle(.green)
            case .unchanged:
                Label(SettingsFormat.unchangedText, systemImage: "checkmark.circle").foregroundStyle(.green)
            }
        case .invalidInput:
            Label(SettingsFormat.invalidInputText, systemImage: "exclamationmark.triangle")
                .foregroundStyle(.orange)
        case .rejected(let reason, let detail):
            Label(SettingsFormat.rejectionText(reason, detail), systemImage: "xmark.octagon")
                .foregroundStyle(.red)
                .help(detail ?? "")
        case .failed(let failure):
            // `.help` mirrors the `rejected` path: a daemon-error `detail` (the #628 stale-key message,
            // issue #645) can be long, so keep it fully readable on hover.
            //
            // MEASURED (issue #762, `SettingsTextMetricsTests`): "can be long" understates it by an order
            // of magnitude. The #628 detail is serde's `deny_unknown_fields` error, which names EVERY
            // expected tunable, so this reaches AT LEAST ~2 700 pt of text in the 328 pt footer slot (the
            // gate's fixture is a deliberate lower bound). And there is NO `.lineLimit` here — so it WRAPS,
            // to at least 10 lines / 160 pt, rather than truncating, in a window that is not resizable.
            // The overflow is filed as issue #844 rather than fixed inline (this umbrella's standing
            // rule); the gate pins the measured boundary so a fix is verifiable and a regression is loud.
            Label(SettingsFormat.applyFailureText(failure), systemImage: "bolt.horizontal.circle")
                .foregroundStyle(.red)
                .help(SettingsFormat.applyFailureText(failure))
        }
    }

    private var restartBanner: some View {
        Label(SettingsFormat.restartRequiredText, systemImage: "arrow.clockwise.circle")
            .foregroundStyle(.orange)
    }

    // MARK: - Bindings

    private func tunableBinding(_ field: TunableField) -> Binding<String> {
        Binding(get: { model.draft(for: field) }, set: { model.setDraft($0, for: field) })
    }

    private func labelBinding(_ uuid: String) -> Binding<String> {
        Binding(get: { model.labelDraft(for: uuid) }, set: { model.setLabelDraft($0, for: uuid) })
    }
}
