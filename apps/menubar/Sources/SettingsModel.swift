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

// MARK: - The 15-tunable field abstraction

/// One editable daemon tunable (issue #268). The `rawValue` IS the literal snake_case wire key (mirroring
/// `TunablesView` / `SetTunables`), so a field round-trips read → draft → write without a second name table.
/// The per-field `value(in:)` / `set(_:in:)` switches are the ONE place the 15 fields are enumerated for I/O;
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
    case fleetRunwayWarnSecs = "fleet_runway_warn_secs"

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
        case .fleetRunwayWarnSecs:
            return .fleetRunway
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
        case .fleetRunwayWarnSecs: return tunables.fleetRunwayWarnSecs
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
        case .fleetRunwayWarnSecs: tunables.fleetRunwayWarnSecs = value
        }
    }

    /// The form's tunable sections, in display order.
    enum Section: String, CaseIterable, Identifiable {
        case pollingCooldown
        case swapCeilings
        case blindWindow
        case velocity
        case connectionHealth
        case fleetRunway

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
            case .fleetRunway: return "Fleet Runway"
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
    /// The daemon returned a redacted `{"error":…}` envelope. On the `config-get` LOAD path: the bare reason
    /// — `no config` (no `config.toml` yet), `config unreadable`, `encode failed` — surfaced honestly rather
    /// than shown as a blank form. On the `config-set` APPLY path (issue #645): the `{"error":…,"detail":…}`
    /// envelope the daemon writes when it refuses the write BEFORE the run loop — a version-skewed edit whose
    /// renamed/stale tunable its strict re-parse rejected (issue #628 threads the offending key into
    /// `detail`), or an unauthenticated peer; the carried string is that key-naming `detail` when present
    /// (the actionable "this app is out of date" hint), else the bare reason. Surfaced honestly rather than
    /// collapsed to `.undecodable`.
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
                switch try decodeConfigSetReply(line) {
                case .ack(.applied(let effect)):
                    settleApplied(effect)
                case .ack(.rejected(let reason, let detail)):
                    settingsLog.error("config-set: rejected — \(reason.rawValue, privacy: .public)")
                    applyPhase = .rejected(reason: reason, detail: detail)
                case .error(let reason, let detail):
                    // A redacted `{"error":…,"detail":…}` envelope (issue #645): the daemon refused the write
                    // BEFORE the run loop — a version-skewed edit whose renamed/stale tunable serde rejected
                    // (issue #628 threads the offending key into `detail`), or an unauthenticated peer.
                    // Surface the key-naming `detail` (the actionable "this app is out of date" hint) rather
                    // than collapsing to the opaque `.undecodable` the missing-`result` decode used to yield.
                    settingsLog.error("config-set: daemon error — \(reason, privacy: .public)")
                    applyPhase = .failed(.daemonError(detail ?? reason))
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

// MARK: - SettingsFormat — the Settings window's Foundation-only presentation layer

/// Every string the Settings form renders and every width it lays out against, as pure `Foundation`
/// values (issue #762). The config-editing sibling of `StatusPanelFormat`, and it exists for the same
/// reason: a number the view lays out with, or a sentence the operator reads, must not be a second copy
/// that can drift from the one a test checks.
///
/// WHY IT LIVES IN THIS FILE rather than its own. `MenubarTests` enumerates its SOURCE files one by one
/// (`project.yml`) but takes `Tests` WHOLESALE, so a new test file costs nothing while a new source file
/// costs a `project.yml` edit. Issue #750 hit the identical wall for the panel's cell widths and resolved
/// it the identical way — move the Foundation-only values DOWN into a file the headless bundle already
/// compiles, rather than pull a SwiftUI view UP into it. `SettingsModel.swift` is that file for this
/// surface: it already owns `TunableField`, `ConfigFailure` and `ApplyPhase`, the three types every
/// function below switches over. That is a BUNDLE-MEMBERSHIP constraint, NOT a layering claim — nothing
/// here is model state. Compiling `SettingsView` into the bundle is issue #840's AC-1 to decide (it says
/// so explicitly, to keep the two items from adding it twice); if that lands, this can move to its own
/// file with no behaviour change.
///
/// TWO KINDS OF WIDTH live here and the difference is load-bearing — do not read the second as the first
/// (the same taxonomy `StatusPanelFormat` § Text-cell layout budgets established):
///
///   * LINKED — `SettingsView` / `SettingsWindowController` lay out with this exact constant, so there is
///     no second copy: `windowContentWidth`, `windowContentHeight`, `windowMinContentWidth`,
///     `windowMinContentHeight`, `tunableFieldWidth`, `accountLabelFieldWidth`,
///     `accountRowInterElementSpacing`, `footerPadding`, `footerInterElementSpacing`.
///   * ALLOWANCE — no view site exists to link, because the element sizes to its own content and has no
///     fixed frame: `roundedBorderTextInset`, `saveButtonAllowance`, `statusLabelIconAllowance`,
///     `formRowHorizontalInset`. These are RESERVED budgets, good to about ±10 pt, which is why
///     `SettingsTextMetricsTests` asserts HEADROOM and reports measured numbers rather than pinning an
///     exact fit.
///
/// The copy below is INFERRED product copy, not a locked spec: hq specifies no per-field lexicon
/// (`prd-menubar.md`:25 ratifies "tunables + labels" without one), and `SettingsView`'s layout is absent
/// from `menubar-preview.html` — issue #763 owns closing that design-provenance hole. Extracting these
/// strings does not ratify them; it only makes them reachable from a test.
enum SettingsFormat {

    // MARK: Per-field copy (label with unit + hover help)

    /// Human label (with unit) + hover help for a tunable. INFERRED from the field name + `src/config.rs`
    /// semantics. The daemon is the validation authority; these strings never gate a value.
    static func copy(for field: TunableField) -> (title: String, help: String) {
        switch field {
        case .pollSecs: return ("Poll interval (s)", "How often the daemon checks usage.")
        case .exhaustedPollSecs: return ("Exhausted poll (s)", "Slower poll while every account is exhausted.")
        case .nearLimitPollSecs: return ("Near-limit poll (s)", "Faster poll when an account is close to a limit.")
        case .cooldownSecs: return ("Swap cooldown (s)", "Minimum time between automatic swaps.")
        case .targetMaxSessionUsage: return ("Target session usage (%)", "Aim to keep session usage below this.")
        case .sessionCeiling: return ("Session ceiling (%)", "Swap away early enough that session usage lands below this.")
        case .weeklyCeiling: return ("Weekly ceiling (%)", "Swap away early enough that weekly usage lands below this.")
        case .sessionBlindSwapSecs: return ("Blind swap delay (s)", "Wait this long before a preemptive swap while usage is blind (429).")
        case .sessionBlindRiskBand: return ("Blind risk band (%)", "Retained usage that counts as risky while blind.")
        case .sessionVelocityHorizonSecs: return ("Velocity horizon (s)", "Look-ahead window for the usage-velocity projection.")
        case .sessionVelocityMinProjectAbove: return ("Velocity floor (%)", "Only project a swap when usage is above this.")
        case .sessionVelocityEmaAlphaPct: return ("Velocity smoothing (%)", "EMA smoothing factor for usage velocity.")
        case .monitor401N: return ("401 tolerance", "Consecutive 401s before an account is treated as needing re-login.")
        case .monitorRecoveryM: return ("Recovery threshold", "Consecutive good checks before an account is considered recovered.")
        case .fleetRunwayWarnSecs: return ("Runway warning (s)", "Warn when the whole fleet’s combined runway drops below this many seconds. 0 turns the warning off.")
        }
    }

    // MARK: Load-phase copy (the honest-disconnected / no-config states, AC 7 of issue #268)

    static func loadFailureHeadline(_ failure: ConfigFailure) -> String {
        switch failure {
        case .daemonError(ConfigGetErrorReason.noConfig): return "No configuration yet"
        case .daemonError(ConfigGetErrorReason.unreadable): return "Configuration unreadable"
        case .daemonError: return "Configuration unavailable"
        case .transport, .unavailable: return "Sessiometer isn’t connected"
        case .undecodable: return "Unexpected response"
        }
    }

    static func loadFailureDetail(_ failure: ConfigFailure) -> String {
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

    /// The loading + failure sections' shared header. One constant so the three daemon-config sections
    /// cannot drift apart — the footer of the Notifications section points at "the daemon configuration
    /// below" and means exactly this heading.
    static let daemonConfigSectionTitle = "Daemon Configuration"

    // MARK: Static section copy (headers, footers, control titles)

    // These carry no logic, so they were nearly left in the view as literals. They are here because the
    // alternative is a coverage claim with a hole in it: an adversarial review of the first draft found
    // 14 operator-visible strings still hardcoded while this file's header claimed every string had
    // moved. Narrowing the claim was the other option; completing the seam is the better one, because a
    // string that lives here is one a test can assert is present, non-empty and distinct — and `saveTitle`
    // in particular MUST live here, since `applyStatusBudget` reserves width for that exact button and a
    // second copy would let the two drift apart silently.

    static let generalSectionTitle = "General"
    static let launchAtLoginToggleTitle = "Launch Sessiometer at login"
    static let launchAtLoginApprovalHint =
        "Approve Sessiometer in System Settings › General › Login Items to finish enabling this."
    static let launchAtLoginApprovalButtonTitle = "Open Login Items settings"
    static let generalSectionFooter =
        "Start Sessiometer automatically when you log in. This launches the menu-bar app only — "
        + "the background daemon is started separately."

    static let notificationsSectionTitle = "Notifications"
    static let notificationsToggleTitle = "Notify on account swaps and exhaustion"
    static let notificationsSectionFooter =
        "A local macOS notification when the active account changes or every account is exhausted. "
        + "This is an app preference — it isn’t part of the daemon configuration below."

    static let loadingText = "Loading settings…"
    static let retryButtonTitle = "Try Again"

    static let accountsSectionTitle = "Accounts"
    static let accountLabelFieldPlaceholder = "Label"
    static let accountsSectionFooter =
        "Rename accounts here. Add, remove, or re-authenticate accounts with the sessiometer CLI — "
        + "the settings window never touches credentials."

    /// The footer's commit button. LINKED to `saveButtonAllowance` — the width every footer budget
    /// reserves is reserved for THIS string, so renaming the button without re-measuring the allowance is
    /// exactly the drift this seam exists to make impossible.
    static let saveTitle = "Save"

    // MARK: Apply-outcome copy (the footer status)

    /// The daemon's own `rejected` verdict, rendered. `detail` is the daemon's non-secret message when it
    /// supplied one (issue #645); the fallbacks are this app's.
    static func rejectionText(_ reason: ConfigSetRejection, _ detail: String?) -> String {
        switch reason {
        case .invalid: return detail ?? "That value isn’t allowed."
        case .unknownAccount: return "That account is no longer in the roster — reopen Settings."
        case .noConfig: return "No configuration to update — capture an account with the CLI first."
        case .configUnreadable: return "The configuration file couldn’t be read — it was left unchanged."
        case .saveFailed: return "The configuration couldn’t be saved — the old file is intact."
        case .unavailable: return "The daemon can’t change configuration right now."
        }
    }

    /// A transport / decode failure on the APPLY path, rendered.
    ///
    /// UNBOUNDED BY CONSTRUCTION on the `.daemonError` arm: the string it interpolates is the daemon's
    /// `detail` (issue #628 threads serde's own message through, and serde's `deny_unknown_fields` error
    /// names EVERY expected field), so this can reach several hundred characters from a version-skewed
    /// app. `SettingsTextMetricsTests` measures exactly that against `applyStatusBudget` rather than
    /// leaving it to the hover tooltip.
    static func applyFailureText(_ failure: ConfigFailure) -> String {
        switch failure {
        case .transport, .unavailable: return "Not saved — Sessiometer isn’t connected."
        case .undecodable: return "Not saved — the daemon sent an unexpected reply."
        case .daemonError(let reason): return "Not saved — \(reason)."
        }
    }

    static let savingText = "Saving…"
    static let savedText = "Saved"
    /// The daemon applied nothing because the submitted values already matched (a stale baseline or a
    /// concurrent change) — "Saved" would imply a write that didn't happen.
    static let unchangedText = "Already up to date"
    static let invalidInputText = "Fix the highlighted fields."
    static let restartRequiredText = "Saved — restart the daemon to apply."

    // MARK: Roster-row copy

    /// A roster row's leading label. `enabled` is read-only in this window — parking / add / remove are
    /// CLI-only (issue #268 AC 5) — so the parked state is stated, never offered as an edit.
    static func accountRowTitle(enabled: Bool) -> String { enabled ? "Account" : "Account (parked)" }

    /// The trailing state cue beside a roster row's label field.
    static func accountStateCue(enabled: Bool) -> String { enabled ? "Active" : "Parked" }

    // MARK: Enable/disable predicates

    /// Save is live only when there is a clean edit to submit and no apply is in flight.
    ///
    /// The in-flight half is not cosmetic: `SettingsModel.apply()` carries its own re-entrancy guard
    /// precisely because a rapid double Cmd-S can outrun SwiftUI's re-render, so this predicate and that
    /// guard are two layers of the same contract — and this is the layer a test can reach.
    static func saveEnabled(isDirty: Bool, applyPhase: SettingsModel.ApplyPhase) -> Bool {
        guard isDirty else { return false }
        if case .applying = applyPhase { return false }
        return true
    }

    // MARK: Layout — the widths the form lays out with

    /// LINKED — `SettingsWindowController.setContentSize`. The window is NOT resizable
    /// (`styleMask == [.titled, .closable]`), so this is the width the form actually gets on screen.
    static let windowContentWidth: Double = 460

    /// LINKED — `SettingsWindowController.setContentSize`.
    static let windowContentHeight: Double = 560

    /// LINKED — `SettingsView`'s `.frame(minWidth:)`. Unreachable on today's non-resizable window, but it
    /// is the floor the view itself declares, so it is the width a future `.resizable` would expose. The
    /// two content-width budgets below (`applyStatusBudget`, `formRowTextBudget`) do NOT default to it —
    /// see the first, which measured the conservative choice and rejected it — but both take it as a
    /// parameter, and this is the value the latent-defect tests pass in.
    static let windowMinContentWidth: Double = 440

    /// LINKED — `SettingsView`'s `.frame(minHeight:)`.
    static let windowMinContentHeight: Double = 420

    /// LINKED — the tunable row's value `TextField`'s `.frame(width:)`.
    static let tunableFieldWidth: Double = 96

    /// LINKED — the account row's label `TextField`'s `.frame(width:)`.
    static let accountLabelFieldWidth: Double = 160

    /// LINKED — the account row's `HStack` spacing, charged once between the label field and its trailing
    /// Active/Parked cue. Hoisted for the same reason as `footerInterElementSpacing`: the gate charges this
    /// gap when it measures whether the row's whole value column fits the window.
    static let accountRowInterElementSpacing: Double = 8

    /// LINKED — the footer's `.padding(12)`, charged on every side.
    static let footerPadding: Double = 12

    /// LINKED — the footer `HStack`'s spacing, charged between each adjacent pair (status | Spacer | Save).
    static let footerInterElementSpacing: Double = 12

    /// ALLOWANCE — a `.roundedBorder` text field's internal text inset per side (bezel + text margin).
    /// No view site to link: the inset belongs to the AppKit control's own drawing, not to a modifier.
    static let roundedBorderTextInset: Double = 5

    /// ALLOWANCE — the footer's trailing `Button("Save")` total width, bezel included. The button sizes to
    /// its own title, so there is no frame to link; `SettingsTextMetricsTests` measures the title and
    /// asserts this allowance actually clears it with bezel room, so it cannot quietly become nonsense.
    static let saveButtonAllowance: Double = 62

    /// ALLOWANCE — a `Label`'s leading SF Symbol plus the gap to its title, at footer text size.
    static let statusLabelIconAllowance: Double = 22

    /// ALLOWANCE — a grouped `Form` row's own horizontal inset per side (the section inset plus the row's
    /// content padding). No view site to link: `.formStyle(.grouped)` owns this, not a modifier this app
    /// writes, so it is a reserved budget rather than a pin.
    static let formRowHorizontalInset: Double = 24

    /// The text a `.roundedBorder` `TextField` of `width` can show AT ONCE.
    ///
    /// A text field scrolls rather than truncates, so exceeding this is a LEGIBILITY limit (the operator
    /// must scroll to read or verify the value), not clipping. Naming the difference matters: the
    /// truncation gate issue #750 built is about text that is *lost*; this is about text that is merely
    /// *off-screen*, and the two deserve different verdicts.
    static func fieldTextBudget(_ width: Double) -> Double { width - 2 * roundedBorderTextInset }

    /// The width a full-span element inside a `Form` row gets — the inline field error under a tunable, and
    /// the row's own label + value columns together. Unlike `fieldTextBudget` this is NOT a `.frame(width:)`
    /// a view declares; it is the content width less `formRowHorizontalInset` on each side.
    ///
    /// `contentWidth` is REQUIRED, deliberately unlike `applyStatusBudget`'s: that one defaults to the
    /// shipped 460 because measurement forced it to, and a sibling here quietly defaulting the other way
    /// would be the easiest possible way to derive a row budget 20 pt too generous by accident.
    static func formRowTextBudget(contentWidth: Double) -> Double {
        contentWidth - 2 * formRowHorizontalInset
    }

    /// The width available to the footer's apply-status `Label` TITLE.
    ///
    /// Derived, never hand-tuned: the content width less both footer paddings, the two `HStack` gaps, the
    /// Save button and the `Label`'s own leading icon. At the SHIPPED 460 pt window that is
    /// 460 − 24 − 24 − 62 − 22 = **328 pt**. Its weakest inputs are the two allowances above, so treat it
    /// as ±10 pt rather than exact.
    ///
    /// WHY IT DEFAULTS TO THE SHIPPED WIDTH AND NOT THE DECLARED FLOOR (issue #762, measured). The floor
    /// would be the conservative choice, and it was the first one taken — but measurement rejected it:
    /// at 440 pt the budget is 308 pt and the widest ORDINARY failure sentence ("Not saved — the daemon
    /// sent an unexpected reply.", 310.53 pt) does not fit, so a floor-derived gate would have opened red
    /// on shipped, correct copy. The window carries no `.resizable` in its style mask, so 460 is the width
    /// the form actually gets and 440 is unreachable today. The 2.53 pt shortfall at the declared floor is
    /// not swept away for that: `SettingsTextMetricsTests` asserts it explicitly as a LATENT defect that
    /// goes live the moment anyone makes the window resizable.
    static func applyStatusBudget(contentWidth: Double = windowContentWidth) -> Double {
        contentWidth
            - 2 * footerPadding
            - 2 * footerInterElementSpacing
            - saveButtonAllowance
            - statusLabelIconAllowance
    }
}
