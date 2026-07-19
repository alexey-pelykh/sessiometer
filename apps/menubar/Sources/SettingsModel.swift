// Copyright (c) 2026 Oleksii PELYKH
// SPDX-License-Identifier: MIT

// The Settings window's model (issue #268): the `@MainActor` shell that owns the daemon `config-get` load
// AND the batched `config-set` apply over the #358 control-command transport, plus the app-local
// notification toggle. It is the config-EDITING sibling of the read-only `PanelStatsModel` (#446, the same
// one-shot query → idle → loading → loaded → failed shape) and the write-path `AccountSwapModel` (#169, the
// same send → applying → applied / rejected shape) — only the verb + payload differ, the transport does not.
//
// AppKit-free BY DESIGN (Foundation + Combine + os only) so it compiles into the headless `MenubarTests`
// bundle and its load / apply / draft-diffing transitions are driven hermetically against a fake connector —
// no socket, no live daemon (the same testability split `PanelStatsModel` / `AccountSwapModel` use). The
// SwiftUI `SettingsView` + `SettingsWindowController` that render it stay in the app target (untested).
//
// SAFETY BOUNDARY (issue #268, load-bearing — AC 5/6): the write surface is exactly `ConfigWire`'s
// `ConfigSetCommand` = `{tunables, labels}`. A credential, an `[[account]]` add/remove, or any roster
// STRUCTURE change is UNREPRESENTABLE by construction (mirroring the daemon's `deny_unknown_fields`); this
// model can only diff + submit non-secret tunables + labels. Add/remove routes to the CLI (a pointer in the
// view), never a GUI keychain write. NO credential handling of any kind (issue #15).

import Combine
import Foundation
import os

private let settingsLog = Logger(subsystem: "org.sessiometer.menubar", category: "settings")

// MARK: - The 14-tunable field abstraction

/// One editable daemon tunable (issue #268). The `rawValue` IS the literal snake_case wire key (mirroring
/// `TunablesView` / `SetTunables`), so a field round-trips read → draft → write without a second name table.
/// The per-field `value(in:)` / `set(_:in:)` switches are the ONE place the 14 fields are enumerated for I/O;
/// everything else (draft diffing, section grouping, the form) drives off `allCases`.
enum TunableField: String, CaseIterable, Identifiable, Equatable {
    case pollSecs = "poll_secs"
    case exhaustedPollSecs = "exhausted_poll_secs"
    case nearLimitPollSecs = "near_limit_poll_secs"
    case cooldownSecs = "cooldown_secs"
    case targetMaxSessionUsage = "target_max_session_usage"
    case sessionCeiling = "session_ceiling"
    case weeklyCeiling = "weekly_ceiling"
    case sessionBlindSwapSecs = "session_blind_swap_secs"
    case sessionBlindRiskBand = "session_blind_risk_band"
    case sessionVelocityHorizonSecs = "session_velocity_horizon_secs"
    case sessionVelocityMinProjectAbove = "session_velocity_min_project_above"
    case sessionVelocityEmaAlphaPct = "session_velocity_ema_alpha_pct"
    case monitor401N = "monitor_401_n"
    case monitorRecoveryM = "monitor_recovery_m"

    var id: String { rawValue }

    /// The form section this field belongs to (grouping per the #268 design). UI-structural only.
    var section: Section {
        switch self {
        case .pollSecs, .exhaustedPollSecs, .nearLimitPollSecs, .cooldownSecs:
            return .pollingCooldown
        case .targetMaxSessionUsage, .sessionCeiling, .weeklyCeiling:
            return .swapCeilings
        case .sessionBlindSwapSecs, .sessionBlindRiskBand:
            return .blindWindow
        case .sessionVelocityHorizonSecs, .sessionVelocityMinProjectAbove, .sessionVelocityEmaAlphaPct:
            return .velocity
        case .monitor401N, .monitorRecoveryM:
            return .connectionHealth
        }
    }

    /// The current value of this field in a loaded `TunablesView`, widened to `UInt64` (the `UInt8` percent /
    /// count fields promote losslessly). The read side of the round-trip.
    func value(in tunables: TunablesView) -> UInt64 {
        switch self {
        case .pollSecs: return tunables.pollSecs
        case .exhaustedPollSecs: return tunables.exhaustedPollSecs
        case .nearLimitPollSecs: return tunables.nearLimitPollSecs
        case .cooldownSecs: return tunables.cooldownSecs
        case .targetMaxSessionUsage: return UInt64(tunables.targetMaxSessionUsage)
        case .sessionCeiling: return UInt64(tunables.sessionCeiling)
        case .weeklyCeiling: return UInt64(tunables.weeklyCeiling)
        case .sessionBlindSwapSecs: return tunables.sessionBlindSwapSecs
        case .sessionBlindRiskBand: return UInt64(tunables.sessionBlindRiskBand)
        case .sessionVelocityHorizonSecs: return tunables.sessionVelocityHorizonSecs
        case .sessionVelocityMinProjectAbove: return UInt64(tunables.sessionVelocityMinProjectAbove)
        case .sessionVelocityEmaAlphaPct: return UInt64(tunables.sessionVelocityEmaAlphaPct)
        case .monitor401N: return UInt64(tunables.monitor401N)
        case .monitorRecoveryM: return UInt64(tunables.monitorRecoveryM)
        }
    }

    /// Write this field's parsed edit into a `SetTunables` (the write side of the round-trip). Only fields
    /// the model diffs as dirty are ever `set`, so an untouched field stays `nil` and is OMITTED from the
    /// wire (the daemon's per-field allow-list default). The daemon is the range/cross-field authority; this
    /// only carries the number.
    func set(_ value: Int64, in tunables: inout SetTunables) {
        switch self {
        case .pollSecs: tunables.pollSecs = value
        case .exhaustedPollSecs: tunables.exhaustedPollSecs = value
        case .nearLimitPollSecs: tunables.nearLimitPollSecs = value
        case .cooldownSecs: tunables.cooldownSecs = value
        case .targetMaxSessionUsage: tunables.targetMaxSessionUsage = value
        case .sessionCeiling: tunables.sessionCeiling = value
        case .weeklyCeiling: tunables.weeklyCeiling = value
        case .sessionBlindSwapSecs: tunables.sessionBlindSwapSecs = value
        case .sessionBlindRiskBand: tunables.sessionBlindRiskBand = value
        case .sessionVelocityHorizonSecs: tunables.sessionVelocityHorizonSecs = value
        case .sessionVelocityMinProjectAbove: tunables.sessionVelocityMinProjectAbove = value
        case .sessionVelocityEmaAlphaPct: tunables.sessionVelocityEmaAlphaPct = value
        case .monitor401N: tunables.monitor401N = value
        case .monitorRecoveryM: tunables.monitorRecoveryM = value
        }
    }

    /// The form's tunable sections, in display order.
    enum Section: String, CaseIterable, Identifiable {
        case pollingCooldown
        case swapCeilings
        case blindWindow
        case velocity
        case connectionHealth

        var id: String { rawValue }

        /// The fields in this section, in `TunableField.allCases` (declaration) order.
        var fields: [TunableField] { TunableField.allCases.filter { $0.section == self } }

        /// The section header the form renders. Structural grouping per the #268 design memo (hq specifies
        /// no per-field copy — `prd-menubar.md`:25 ratifies "tunables + labels" without a field lexicon —
        /// so these + the field labels in `SettingsView` are inferred, refinable copy, not a locked spec).
        var title: String {
            switch self {
            case .pollingCooldown: return "Polling & Cooldown"
            case .swapCeilings: return "Reserve & Ceilings"
            case .blindWindow: return "Blind-Window Safety"
            case .velocity: return "Velocity Projection"
            case .connectionHealth: return "Connection Health"
            }
        }
    }
}

// MARK: - Failure taxonomy

/// Why a config load or apply did not succeed on the transport / decode axis — the config sibling of
/// `StatsFailure` / `SwapFailure`. Non-secret: the whole config channel is redacted (tunables + labels only,
/// issue #15), so each case carries a plain reason. A daemon `rejected` verdict is NOT here — that is a
/// first-class apply outcome (`ApplyPhase.rejected`), not a transport failure.
enum ConfigFailure: Equatable {
    /// A bounded transport failure (#358 `ControlCommandError`): no daemon (refused), a wedged daemon
    /// (timed out / closed before the reply), or an I/O / encode fault. Honest-disconnected (AC 7).
    case transport(ControlCommandError)
    /// The daemon returned a redacted `{"error":…}` `config-get` envelope — `no config` (no `config.toml`
    /// yet), `config unreadable`, `encode failed`. Surfaced honestly rather than shown as a blank form.
    case daemonError(String)
    /// The reply did not match the `ConfigWire` contract (a buggy / drifted daemon) — degrade loudly.
    case undecodable
    /// No control client — the daemon control-socket path would not resolve (sandboxed / home unresolved),
    /// so config is unavailable from this app instance (AC 7: honest, never a silent local write).
    case unavailable
}

// MARK: - SettingsModel

@MainActor
final class SettingsModel: ObservableObject {

    /// The `config-get` load phase the form observes. `loading` shows only on a first load / retry; the
    /// daemon config is tiny so each window open re-fetches (drafts repopulate from the fresh `ConfigView`).
    enum LoadPhase: Equatable {
        case idle
        case loading
        case loaded(ConfigView)
        case failed(ConfigFailure)
    }

    /// The `config-set` apply outcome the form observes. `invalidInput` is CLIENT-side (a dirty draft did not
    /// parse to a whole number — see `fieldErrors`; NO command was sent, so no partial write); `rejected` is
    /// the DAEMON's verdict (it is the validation authority — range + cross-field). `applied(.live/.unchanged)`
    /// is transient (auto-clears); `applied(.restartRequired)` additionally latches `restartPending`.
    enum ApplyPhase: Equatable {
        case idle
        case applying
        case applied(effect: ConfigSetEffect)
        case rejected(reason: ConfigSetRejection, detail: String?)
        case invalidInput
        case failed(ConfigFailure)
    }

    // MARK: Published state

    @Published private(set) var loadPhase: LoadPhase = .idle
    @Published private(set) var applyPhase: ApplyPhase = .idle

    /// The editable tunable drafts, keyed by field (String-backed, parsed only on Save — never a
    /// `TextField(value:formatter:)`, which fights the operator over intermediate typing states). Repopulated
    /// from the loaded `ConfigView` on every successful load; `pristine` is the last-loaded baseline the
    /// dirty diff compares against.
    @Published private(set) var drafts: [TunableField: String] = [:]

    /// The editable per-account label drafts, keyed by the STABLE `account_uuid` (never the mutable label).
    /// `enabled` is read-only here — parking / add / remove are CLI-only (AC 5).
    @Published private(set) var labelDrafts: [String: String] = [:]

    /// The loaded roster (uuid, current label, enabled) the accounts section renders, in daemon order.
    @Published private(set) var accounts: [AccountView] = []

    /// Per-field CLIENT-side format errors (a draft that is not a whole number ≥ 0, or that is too large for
    /// the wire). Shown inline; cleared as soon as the operator edits that field. Distinct from the daemon's
    /// `rejected` banner.
    @Published private(set) var fieldErrors: [TunableField: String] = [:]

    /// Latches true when an applied edit needs a daemon restart to take effect (`restart_required`), for the
    /// persistent banner. Cleared on the next load (a restart + reopen starts clean) — the transient
    /// `applied` confirmation, by contrast, auto-clears on its own beat.
    @Published private(set) var restartPending: Bool = false

    /// The app-local "post account-activity notifications" toggle (issue #267 `NotificationPreferences`).
    /// IMMEDIATE-apply write-through to `UserDefaults` — NOT part of the `config-set` batch (a different
    /// apply surface with different semantics). Enabling after a launch-off fires `onRequestAuthorization`
    /// so the OS permission prompt appears (it was never asked for while disabled).
    @Published var notificationsEnabled: Bool {
        didSet {
            preferences.isEnabled = notificationsEnabled
            if notificationsEnabled && !oldValue { onRequestAuthorization?() }
        }
    }

    // MARK: Dependencies

    /// The short-lived control-command client for config-get/set, or `nil` when the socket path would not
    /// resolve — in which case load / apply short-circuit to `.unavailable` (honest, never a dead form).
    private let client: ControlCommandClient?
    private let preferences: NotificationPreferences
    /// `@MainActor`-typed: invoked from `didSet` on this `@MainActor` model, and the real hook drives
    /// `UNUserNotificationCenter` authorization (main-thread work), so the type carries that isolation.
    private let onRequestAuthorization: (@MainActor () -> Void)?

    /// The last-loaded baseline the dirty diff compares against (tunables + labels). Not `@Published` — it is
    /// the invisible reference, not rendered.
    private var pristineTunables: [TunableField: UInt64] = [:]
    private var pristineLabels: [String: String] = [:]

    init(
        client: ControlCommandClient?,
        preferences: NotificationPreferences,
        onRequestAuthorization: (@MainActor () -> Void)? = nil
    ) {
        self.client = client
        self.preferences = preferences
        self.onRequestAuthorization = onRequestAuthorization
        self.notificationsEnabled = preferences.isEnabled  // stored-property init: does NOT fire didSet
    }

    // MARK: Dirty tracking

    /// Whether Save has anything to submit — any tunable draft differs from its pristine, or any label does.
    /// Drives the Save button's enabled state.
    var isDirty: Bool { !dirtyTunableFields.isEmpty || !dirtyLabels.isEmpty }

    /// The tunable fields whose draft differs from the last-loaded baseline (canonical-string compared, so
    /// re-typing the same number is not "dirty"). Empty before a successful load.
    var dirtyTunableFields: [TunableField] {
        TunableField.allCases.filter { field in
            guard let pristine = pristineTunables[field] else { return false }
            // Compare by VALUE, not string: "0300" / " 300 " parse to 300 and are NOT edits (no spurious
            // dirty, and no re-dirty of a just-saved field). A draft that does NOT parse (empty, "abc", "-5")
            // counts as dirty so Save stays live to surface the format error on submit — never silently drop.
            if let value = UInt64(normalizedDraft(field)) { return value != pristine }
            return true
        }
    }

    /// The edited labels as a `uuid → newLabel` map (only accounts whose TRIMMED label draft differs from
    /// baseline). Leading/trailing whitespace is trimmed — like the tunable diff — so a stray space is
    /// neither spuriously dirty nor sent space-padded; internal spaces in a label are preserved.
    var dirtyLabels: [String: String] {
        var edited: [String: String] = [:]
        for (uuid, draft) in labelDrafts {
            let trimmed = draft.trimmingCharacters(in: .whitespaces)
            if pristineLabels[uuid] != trimmed { edited[uuid] = trimmed }
        }
        return edited
    }

    // MARK: View-binding helpers (dict-backed drafts → per-field bindings)

    func draft(for field: TunableField) -> String { drafts[field] ?? "" }

    /// Set a tunable draft; editing a field clears its stale inline format error (fix-as-you-type). Once
    /// EVERY flagged field is fixed, the `invalidInput` outcome is dropped too — otherwise the "fix the
    /// highlighted fields" banner would linger after its cause is gone, pointing at nothing.
    func setDraft(_ value: String, for field: TunableField) {
        drafts[field] = value
        if fieldErrors[field] != nil { fieldErrors[field] = nil }
        if fieldErrors.isEmpty, case .invalidInput = applyPhase { applyPhase = .idle }
    }

    func labelDraft(for uuid: String) -> String { labelDrafts[uuid] ?? "" }
    func setLabelDraft(_ value: String, for uuid: String) { labelDrafts[uuid] = value }

    // MARK: Load

    /// Run the one-shot `config-get` query and render loading → loaded / failed, repopulating drafts +
    /// baseline from the fresh `ConfigView`. Called on each window open (fresh fetch, discards unsaved
    /// drafts — a Settings window re-reads on open). A missing client short-circuits to `.failed(.unavailable)`.
    func load() async {
        // A fresh load supersedes any prior apply outcome + the restart latch.
        applyPhase = .idle
        restartPending = false
        fieldErrors = [:]

        guard let client else {
            loadPhase = .failed(.unavailable)
            return
        }
        loadPhase = .loading

        let result = await client.send(ConfigGetCommand())
        switch result {
        case .failure(let error):
            settingsLog.error("config-get: transport failure — \(String(describing: error), privacy: .public)")
            loadPhase = .failed(.transport(error))
        case .success(let line):
            do {
                switch try decodeConfigGetReply(line) {
                case .ok(let view):
                    try adopt(view)
                    loadPhase = .loaded(view)
                case .error(let reason):
                    settingsLog.error("config-get: daemon error — \(reason, privacy: .public)")
                    loadPhase = .failed(.daemonError(reason))
                }
            } catch {
                settingsLog.error("config-get: undecodable reply — \(String(describing: error), privacy: .public)")
                loadPhase = .failed(.undecodable)
            }
        }
    }

    /// Adopt a loaded `ConfigView` as the new baseline + fresh drafts (tunables and labels). THROWS on a
    /// roster carrying a duplicate `account_uuid` (a drifted daemon) so `load` routes it to
    /// `.failed(.undecodable)` — degrade loudly like the wire decoders, NOT a `Dictionary` unique-key trap
    /// (a precondition failure `load`'s `do/catch` could never rescue). Builds every value into locals FIRST,
    /// so a throw leaves the model's state untouched.
    private func adopt(_ view: ConfigView) throws {
        var pristine: [TunableField: UInt64] = [:]
        var freshDrafts: [TunableField: String] = [:]
        for field in TunableField.allCases {
            let value = field.value(in: view.tunables)
            pristine[field] = value
            freshDrafts[field] = String(value)
        }

        var labels: [String: String] = [:]
        for account in view.accounts {
            guard labels[account.accountUuid] == nil else { throw AdoptError.duplicateAccountUUID }
            labels[account.accountUuid] = account.label
        }

        pristineTunables = pristine
        drafts = freshDrafts
        accounts = view.accounts
        pristineLabels = labels
        labelDrafts = labels
    }

    /// A `ConfigView` that violates a roster invariant the wire types can't express (a duplicate
    /// `account_uuid`) — surfaced as `.undecodable`, never a trap.
    private enum AdoptError: Error { case duplicateAccountUUID }

    // MARK: Apply

    /// Validate the dirty drafts client-side, then submit ONE batched `config-set` of only the edited keys
    /// (tunables + labels) and render its outcome. A dirty draft that is not a whole number ≥ 0 — or that is
    /// too large to ride the Int64 wire — is a CLIENT format error (`invalidInput` + inline `fieldErrors`, NO
    /// command sent — no partial write); everything that parses is the daemon's to accept or `reject` (it owns
    /// range + cross-field validation). A missing client short-circuits to `.failed(.unavailable)` —
    /// honest-disconnected, never a silent local config write.
    func apply() async {
        // Re-entrancy guard (mirrors `AccountSwapModel.swap`): a second submit while one is in flight is
        // ignored, so a rapid double Cmd-S — before the view's `saveEnabled` disable (an async SwiftUI
        // re-render) lands — cannot spawn two `config-set` writes.
        if case .applying = applyPhase { return }

        guard let client else {
            applyPhase = .failed(.unavailable)
            return
        }

        // Client-side FORMAT check only (a String draft must become a JSON number to ride the wire). Range +
        // cross-field are the daemon's authority — advisory hints never gate here.
        var edited = SetTunables()
        var formatErrors: [TunableField: String] = [:]
        for field in dirtyTunableFields {
            let raw = normalizedDraft(field)
            guard let unsigned = UInt64(raw) else {
                formatErrors[field] = "Enter a whole number (0 or greater)."
                continue
            }
            // A draft in (Int64.max, UInt64.max] IS a whole number ≥ 0 — it just overflows `SetTunables`'
            // Int64 wire — so it is refused with its OWN message rather than mis-reported as a format error.
            // Pathological (~19 digits; no real tunable is that large), but the copy must not lie.
            guard let signed = Int64(exactly: unsigned) else {
                formatErrors[field] = "That number is too large."
                continue
            }
            field.set(signed, in: &edited)
        }

        guard formatErrors.isEmpty else {
            fieldErrors = formatErrors
            applyPhase = .invalidInput
            return  // NO command sent — no partial write (AC 4).
        }
        fieldErrors = [:]

        let command = ConfigSetCommand(tunables: edited, labels: dirtyLabels)
        applyPhase = .applying

        let result = await client.send(command)
        switch result {
        case .failure(let error):
            settingsLog.error("config-set: transport failure — \(String(describing: error), privacy: .public)")
            applyPhase = .failed(.transport(error))
        case .success(let line):
            do {
                switch try decodeConfigSetAck(line) {
                case .applied(let effect):
                    settleApplied(effect)
                case .rejected(let reason, let detail):
                    settingsLog.error("config-set: rejected — \(reason.rawValue, privacy: .public)")
                    applyPhase = .rejected(reason: reason, detail: detail)
                }
            } catch {
                settingsLog.error("config-set: undecodable ack — \(String(describing: error), privacy: .public)")
                applyPhase = .failed(.undecodable)
            }
        }
    }

    /// Land a successful apply: adopt the just-submitted drafts as the new baseline (so the form is no longer
    /// dirty), latch the restart banner for `restart_required`, and schedule the transient confirmation to
    /// clear itself. A `rejected` / `failed` outcome deliberately does NOT auto-clear — the operator must see
    /// it — and does NOT rebaseline (their edits stay for a fix + retry).
    private func settleApplied(_ effect: ConfigSetEffect) {
        rebaselineFromDrafts()
        if effect == .restartRequired { restartPending = true }
        applyPhase = .applied(effect: effect)
        scheduleApplyReset(effect)
    }

    /// Adopt the current drafts as the baseline (dirty → clean). Every dirty draft parsed in `apply` before
    /// this runs, and untouched drafts already equal their baseline, so each draft is a valid whole number.
    private func rebaselineFromDrafts() {
        for field in TunableField.allCases {
            if let unsigned = UInt64(normalizedDraft(field)) { pristineTunables[field] = unsigned }
        }
        // Baseline labels at their trimmed (submitted) form and canonicalize the drafts to match, so a saved
        // "work " shows as "work" and is not re-flagged dirty.
        let trimmed = labelDrafts.mapValues { $0.trimmingCharacters(in: .whitespaces) }
        labelDrafts = trimmed
        pristineLabels = trimmed
    }

    /// Clear the transient `applied` confirmation after a short beat — but only if the phase is STILL that
    /// same applied outcome (a newer apply supersedes it), mirroring `AccountSwapModel`'s confirmation beat.
    /// The `restartPending` banner is separate and persists.
    private func scheduleApplyReset(_ effect: ConfigSetEffect) {
        Task { [weak self] in
            try? await Task.sleep(for: .seconds(1.8))
            guard let self else { return }
            if self.applyPhase == .applied(effect: effect) { self.applyPhase = .idle }
        }
    }

    // MARK: Helpers

    /// A draft trimmed of surrounding whitespace — the form compares + parses the trimmed value so a stray
    /// space is neither spuriously "dirty" nor a parse failure.
    private func normalizedDraft(_ field: TunableField) -> String {
        (drafts[field] ?? "").trimmingCharacters(in: .whitespaces)
    }
}
