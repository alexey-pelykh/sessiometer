// Copyright (c) 2026 Oleksii PELYKH
// SPDX-License-Identifier: MIT

// Native swap / all-accounts-exhausted notifications for the menu-bar app (issue #267, REQ-MBR-B-017).
//
// A thin observer over the SAME already-redacted store the panel renders (`WatchStatusStore`, #324):
// it posts a native macOS notification when the daemon SWAPS the active account or reports the fleet
// has NO viable swap target left ("all accounts exhausted"). It adds NO new wire field, socket verb,
// credential, or keychain seam — it derives both events purely from deltas between consecutive
// published snapshots, exactly the two signals the honest-state core already carries.
//
// Two design guarantees, both load-bearing:
//
//   * REDACTION (AC): a notification surfaces on the lock screen / Notification Center — MORE exposed
//     than the in-app panel — so it names the EVENT, never the account. The wire never carries an
//     email (issue #15); the redacted LABEL (operator handle) IS on the wire but is excluded here too.
//     The guarantee is STRUCTURAL, not by convention: `AccountEvent` carries no associated value, and
//     its notification content is a fixed constant per case — the label the deriver compares to detect
//     a swap is never threaded into the event, so it physically cannot reach the posted content.
//
//   * HONEST-STATE COUPLING: events are derived ONLY from a fresh `.connected` snapshot (the sole
//     healthy state, `HonestStateMachine`). Across a drop / stale / reconnect the baseline is dropped,
//     so a swap that may have happened during a gap is never mis-attributed, and a stale retained
//     roster never fires. Both events are EDGE-TRIGGERED off that baseline: they fire on the
//     transition, not on every snapshot that observes the condition — so a fleet that is already
//     exhausted at launch, or stays exhausted across many heartbeats, never re-spams.
//
// Functional-core / imperative-shell, mirroring the rest of the app: `AccountEventDeriver` is a pure
// value type (no I/O, no clock, no OS) that turns snapshot deltas into events — exhaustively unit-
// testable — and `AccountEventNotifier` is the thin `@MainActor` shell that observes the store, gates
// on the persisted toggle, and forwards to a `AccountEventPresenter`. The one OS-bound collaborator
// (`UNUserNotificationCenter`, in `UserNotificationPresenter`) sits behind that protocol, so the
// derivation + redaction + toggle-gating core is tested with a spy and never touches the OS.

import Combine
import Foundation

// MARK: - The event + its neutral (redacted) notification content

/// A user-facing account-activity event worth a native notification. Deliberately carries NO
/// associated value — no label, no email, no count — so the redaction AC ("no account email/label or
/// credential appears in the notification") holds by construction: there is nothing account-specific
/// to leak into the content, which is a fixed constant per case below.
enum AccountEvent: Equatable, Sendable {
    /// Sessiometer rotated the active session from one account to another.
    case swapped
    /// No account has a viable swap target left — the whole fleet is out of capacity (the daemon's
    /// `next_swap` = `no_viable_target`, `src/daemon/snapshot.rs` `NextSwap::NoViableTarget`).
    case allExhausted

    /// The notification headline — a neutral, static string (no account identity).
    var notificationTitle: String {
        switch self {
        case .swapped:      return "Active account switched"
        case .allExhausted: return "All accounts exhausted"
        }
    }

    /// The notification body — a neutral, static string summarizing the EVENT, never the account.
    var notificationBody: String {
        switch self {
        case .swapped:      return "Sessiometer rotated to a different account."
        case .allExhausted: return "No account has capacity right now — action needed."
        }
    }
}

// MARK: - The pure event deriver

/// Turns deltas between consecutive published snapshots into `AccountEvent`s. Pure and synchronous —
/// no I/O, no clock — so every transition is unit-tested deterministically.
///
/// It tracks a BASELINE — the last `.connected` snapshot's `(activeLabel, hasNoViableTarget)` — and
/// fires on the EDGE:
///   * `.swapped`      — the active account's label changed between two consecutive `.connected`
///                       snapshots (both non-nil; a transient loss of the active account, active→nil,
///                       is not a swap TO anything).
///   * `.allExhausted` — the snapshot ENTERED the no-viable-target state (false→true).
///
/// The baseline is dropped on any non-`.connected` state, so the first snapshot after a (re)connect
/// re-establishes it silently — a swap that may have happened across a disconnect is never invented,
/// and the first snapshot ever (or the first after a gap) fires nothing.
struct AccountEventDeriver {
    private var baseline: Baseline?

    private struct Baseline: Equatable {
        let activeLabel: String?
        let hasNoViableTarget: Bool
    }

    /// Fold one published snapshot in and return the events it triggered (0, 1, or 2 — a swap INTO the
    /// last viable account that itself leaves no target fires both, in `[.swapped, .allExhausted]`
    /// order). `activeLabel` is `rows.first(where: \.isActive)?.label`; `hasNoViableTarget` is
    /// `nextSwap == .noViableTarget`. The label is used ONLY for the local comparison here — it is
    /// never carried into a returned event (the redaction guarantee).
    mutating func ingest(connectionState: ConnectionState,
                         activeLabel: String?,
                         hasNoViableTarget: Bool) -> [AccountEvent] {
        // Only a fresh, healthy snapshot is a trustworthy event source. Any other state drops the
        // baseline so the next `.connected` re-seeds without inferring a swap across the gap or firing
        // off a stale retained roster.
        guard connectionState == .connected else {
            baseline = nil
            return []
        }
        defer { baseline = Baseline(activeLabel: activeLabel, hasNoViableTarget: hasNoViableTarget) }

        // First healthy snapshot (fresh, or first after a gap): establish the baseline, fire nothing —
        // observing a standing condition at launch is not an event.
        guard let previous = baseline else { return [] }

        var events: [AccountEvent] = []
        if let was = previous.activeLabel, let now = activeLabel, was != now {
            events.append(.swapped)
        }
        if hasNoViableTarget && !previous.hasNoViableTarget {
            events.append(.allExhausted)
        }
        return events
    }
}

// MARK: - The presentation seam (the one OS-bound collaborator, behind a protocol)

/// The OS notification surface, abstracted so the notifier's derivation + gating is testable with a
/// spy. The production conformer (`UserNotificationPresenter`) wraps `UNUserNotificationCenter`; that
/// framework — and the authorization prompt + actual display — is GUI/OS-bound and never exercised in
/// a headless test bundle.
@MainActor
protocol AccountEventPresenter {
    /// Ask the OS for permission to post notifications (idempotent at the OS level; a no-op in tests).
    func requestAuthorization()
    /// Post one event's neutral content as a native notification.
    func present(_ event: AccountEvent)
}

// MARK: - The persisted on/off toggle

/// The persisted enable/disable toggle for account-activity notifications (issue #267). A single
/// `UserDefaults` bool is the minimal home; issue #268's `config.toml` settings UI will later SURFACE
/// this same key (they read/write one source of truth, so they cannot drift). There is no in-app
/// toggle yet — the macOS per-app Notification settings are the interim off-switch.
@MainActor
final class NotificationPreferences {
    /// The `UserDefaults` key #268's settings UI will bind to.
    static let enabledKey = "notifications.accountEvents.enabled"

    private let defaults: UserDefaults

    init(defaults: UserDefaults = .standard) {
        self.defaults = defaults
    }

    /// Whether account-activity notifications are enabled. Defaults to `true` when unset — the OS
    /// permission prompt is the real gate, so the feature is useful out of the box. (`object(forKey:)`,
    /// not `bool(forKey:)`, so an absent key reads as the ON default rather than a false OFF.)
    var isEnabled: Bool {
        get { defaults.object(forKey: Self.enabledKey) as? Bool ?? true }
        set { defaults.set(newValue, forKey: Self.enabledKey) }
    }
}

// MARK: - The notifier shell

/// The `@MainActor` shell that binds the store's published projection to native notifications: it
/// derives events from each snapshot, gates on the persisted toggle, and forwards survivors to the
/// presenter. Thin by design — all the branching lives in the pure `AccountEventDeriver`.
@MainActor
final class AccountEventNotifier {
    private var deriver = AccountEventDeriver()
    private let preferences: NotificationPreferences
    private let presenter: AccountEventPresenter
    private var storeObserver: AnyCancellable?

    init(preferences: NotificationPreferences, presenter: AccountEventPresenter) {
        self.preferences = preferences
        self.presenter = presenter
    }

    /// Fold one published snapshot in and post any resulting events — the testable core. The deriver
    /// ALWAYS runs (so the baseline stays current even while disabled); only the POST is gated, so
    /// enabling the toggle later never replays a backlog of missed deltas (AC: disabled ⇒ none posted).
    func handle(connectionState: ConnectionState, activeLabel: String?, hasNoViableTarget: Bool) {
        let events = deriver.ingest(connectionState: connectionState,
                                    activeLabel: activeLabel,
                                    hasNoViableTarget: hasNoViableTarget)
        guard preferences.isEnabled else { return }
        for event in events { presenter.present(event) }
    }

    /// Begin observing the store and requesting OS authorization (only if enabled — don't prompt for a
    /// feature the operator has turned off). `objectWillChange` fires BEFORE the `@Published` values
    /// settle, so the read is deferred one run-loop turn — the same pattern `StatusItemController`'s
    /// Stats observer uses. Idempotent via the subscription guard. Not itself unit-tested (Combine
    /// glue); the derivation it feeds is covered via `handle(...)`.
    func start(observing store: WatchStatusStore) {
        guard storeObserver == nil else { return }
        if preferences.isEnabled { presenter.requestAuthorization() }
        storeObserver = store.objectWillChange.sink { [weak self, weak store] in
            DispatchQueue.main.async {
                guard let self, let store else { return }
                self.handleCurrent(of: store)
            }
        }
        // Seed from the store's current state so a state already present at attach establishes the
        // baseline (firing nothing — edge-triggered) rather than being missed.
        handleCurrent(of: store)
    }

    /// Read the store's settled projection and fold it in.
    private func handleCurrent(of store: WatchStatusStore) {
        let inputs = Self.projection(of: store)
        handle(connectionState: inputs.connectionState,
               activeLabel: inputs.activeLabel,
               hasNoViableTarget: inputs.hasNoViableTarget)
    }

    /// Project the store's published state into the deriver's inputs — the production store adapter.
    /// The active-account LABEL is extracted here (never leaving this shell — it feeds the deriver's
    /// comparison, never a posted event), and `NextSwap.noViableTarget` is collapsed to the
    /// all-exhausted signal. Internal (not private) so a test can lock these two mappings, which the
    /// direct-input `handle(...)` tests deliberately bypass.
    static func projection(of store: WatchStatusStore)
        -> (connectionState: ConnectionState, activeLabel: String?, hasNoViableTarget: Bool) {
        (store.connectionState,
         store.rows.first(where: \.isActive)?.label,
         isNoViableTarget(store.nextSwap))
    }

    /// Whether the daemon reports no viable swap target — the "all accounts exhausted" signal.
    private static func isNoViableTarget(_ nextSwap: NextSwap?) -> Bool {
        if case .noViableTarget = nextSwap { return true }
        return false
    }
}
