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
// what reddens the gate. (The only string literals remaining below are the five SF Symbol NAMES and the
// `"s"` ⌘S key equivalent — neither of which is text an operator reads. The `?? ""` empty-tooltip
// fallback this list used to name is gone: issue #944 replaced it with `rejectionTooltip`, because an
// empty string IS what an operator read on the four rejection reasons the daemon sends no `detail` for.
// The bare `spacing:` / `padding:` numbers that remain are cosmetic ones no budget charges.)
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
        // TOP-ALIGNED, not `HStack`'s default `.center` (issue #1118). The build reference authors this:
        // `menubar-preview.html`'s `.win-foot` rule carries `align-items:flex-start`, under a comment that
        // maps the rule to this exact row (`applyStatus | Spacer | Save`, `.padding(12)`, 12 pt gaps), and
        // the `settings-loaded-{light,dark}` frames render it against a two-line status. So it is an
        // AUTHORED value on a rule modelling this footer, not incidental CSS — read it as the oracle, per
        // `design/README.md` § "The Settings window (#763)".
        //
        // WHY IT ONLY MATTERS NOW: the two alignments differ only when the status label exceeds one line.
        // Before #844 that state was reachable but unbounded (the label wrapped to 10+ lines), so this was
        // the least of that row's problems; after #844 clamped it, the TWO-line label is the bounded steady
        // state for the ordinary version-skew trigger — the normal rendering rather than a pathological one.
        // It covers every `applyStatus` arm, including `.rejected`, which #944 clamped to the same limit.
        //
        // MEASURED, not eyeballed, by rendering THIS view — loaded form, a dirtied tunable so Save is
        // enabled, apply landed on the `.daemonError` version-skew string — at the shipped 460×560 through
        // `ImageRenderer` and reading the drawn pixels, `.center` against `.top`. That is an OFFLINE render,
        // not a screenshot of the live window. Against the two-line label the Save bezel's top moves UP
        // 4.0 pt while the label itself does not move, landing the bezel 12.5 pt under the footer's
        // `Divider()` — `footerPadding`, i.e. flush with the top of the content box, which is exactly what
        // `align-items:flex-start` does. It is NOT inert at one line: there the bezel does not move and the
        // STATUS LABEL rises 4.0 pt instead, from centred-against-the-taller-button to top-aligned. That is
        // the mock's treatment too — `align-items:flex-start` governs BOTH children of `.win-foot`.
        //
        // THE OTHER HALF OF #1118, measured rather than assumed, and it needs no code: the mock also sets
        // `align-items:flex-start` on `.win-status` itself (icon-to-text, with `margin-top:1px` on the SVG).
        // SwiftUI's `Label` already carries that TREATMENT — against a two-line title the icon's ink top
        // sits 0.5 pt ABOVE the title's, so it rides the FIRST LINE rather than centring on the block
        // (centred would put it 8.5 pt lower). It is not pixel-identical: the mock's extra `margin-top:1px`
        // nudges its icon DOWN where SwiftUI's sits marginally up, ~1.5 pt apart. That is the mock's
        // hex/pixel values being directional rather than targets, not a divergence to close — so no
        // `.alignmentGuide` or `.firstTextBaseline` is warranted, and adding one would be inventing a
        // precision the reference does not claim.
        //
        // No automated gate reaches any of this: `SettingsView` is outside the `MenubarTests` bundle by
        // design (`project.yml` — a separate window surface, not the panel), and the Settings window has no
        // render harness, so `design/build-comparison.py` has no `STATES` row for it by construction.
        HStack(alignment: .top, spacing: CGFloat(SettingsFormat.footerInterElementSpacing)) {
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
            // BOUNDED BY GEOMETRY, NEVER EDITED (issue #944) — the same ratified rule the `.failed` arm
            // below implements (issue #844). `design/README.md` § "The Settings window (#763)" states it
            // for the apply-status slot as a whole, and `menubar-preview.html`'s `.win-status .txt` names
            // `.failed` and `.rejected` together. Read off that RULE, not off a frame: NO frame renders
            // this arm (that reference's own unauthored register R-11 → issue #946).
            //
            // SHARPER HERE THAN BELOW, and measured (`SettingsTextMetricsTests`): `applyFailureText` wraps
            // the daemon's text in a fixed app sentence, but `rejectionText(.invalid, detail)` RETURNS the
            // detail — on that path the daemon's whole message IS the label. `src/config/validate.rs`
            // spells its cross-field remedies out, and the `target_max_session_usage = 0` rejection (the
            // documented issue #414 operator trap: 0 is the natural wrong guess for "no restriction", its
            // exact opposite) is 169 characters / ~1 027 pt of text in the 328 pt slot — four wrapped
            // lines, in a window `SettingsWindowController` builds with no `.resizable`, so that height
            // came out of the form above rather than clipping.
            //
            // `.help` is the recovery and carries BOTH surfaces the design rule names — the hover tooltip
            // AND `accessibilityHelp` (there is no SwiftUI modifier of that name; the AX attribute is the
            // one `.help` sets). It takes `rejectionTooltip`, which is the rendered label PLUS the
            // daemon's `detail` when the label does not already carry it, replacing the `detail ?? ""`
            // this arm used to pass.
            //
            // THE DETAIL IS SENT ON TWO REASONS, NOT ONE, and that is what makes the tooltip a distinct
            // string rather than a copy of the label. `ConfigSetAck::Rejected`'s doc
            // (`src/daemon/socket.rs`) carries the non-secret message for `invalid` AND for
            // `config-unreadable` — the baseline TOML parse error (issue #628), which
            // `classify_config_set_failure` (`src/daemon/classify.rs`) attaches so a malformed on-disk
            // config is diagnosable rather than a bare envelope. `rejectionText` returns `detail` only on
            // `.invalid`, so on `.configUnreadable` the parse error would be dropped — and nothing else
            // in this app surfaces it. The other four reasons carry no detail at all; for them the
            // tooltip is the arm's own sentence, which is what `detail ?? ""` failed to show.
            Label(SettingsFormat.rejectionText(reason, detail), systemImage: "xmark.octagon")
                .foregroundStyle(.red)
                .lineLimit(SettingsFormat.applyStatusLineLimit)
                .truncationMode(.tail)
                .help(SettingsFormat.rejectionTooltip(reason, detail))
        case .failed(let failure):
            // BOUNDED BY GEOMETRY, NEVER EDITED (issue #844) — the rule `design/README.md`
            // § "The Settings window (#763)" ratifies and whose `loaded` frames render.
            //
            // MEASURED (issue #762, `SettingsTextMetricsTests`): the #628 `detail` this interpolates is
            // serde's `deny_unknown_fields` error, which names EVERY expected tunable, so it reaches AT
            // LEAST ~2 700 pt of text in the 328 pt footer slot (the gate's fixture is a deliberate lower
            // bound). Unclamped it WRAPPED rather than truncating — to at least 10 lines — and
            // `SettingsWindowController` builds the window with `[.titled, .closable]` and no `.resizable`,
            // so that height came out of the form above it. The clamp caps the drawing at two lines; the
            // message handed to `.help` is never shortened.
            //
            // `.help` is the recovery, and it carries BOTH surfaces the design rule names: the hover
            // tooltip AND `accessibilityHelp` — there is no SwiftUI `.accessibilityHelp` modifier, and the
            // AX attribute of that name (the one `PanelAccessibilityTreeTests` walks) is what `.help` sets.
            // `SettingsTextMetricsTests` gates all three parts: that the clamp BINDS on this string, that
            // the string handed to `.help` is still the daemon's message in full, and — through a stand-in
            // label in the AX tree — that a clamped `.help` really does publish the whole message.
            Label(SettingsFormat.applyFailureText(failure), systemImage: "bolt.horizontal.circle")
                .foregroundStyle(.red)
                .lineLimit(SettingsFormat.applyStatusLineLimit)
                .truncationMode(.tail)
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
