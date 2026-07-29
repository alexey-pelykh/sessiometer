// Copyright (c) 2026 Oleksii PELYKH
// SPDX-License-Identifier: MIT

// Native account-activity notifications for the menu-bar app (issue #267, REQ-MBR-B-017; extended by
// issue #935).
//
// A thin observer over the SAME already-redacted store the panel renders (`WatchStatusStore`, #324):
// it posts a native macOS notification when the daemon SWAPS the active account, reports the fleet has
// NO viable swap target left ("all accounts exhausted"), or shows a login inside its configured expiry
// horizon. It adds NO new wire field, socket verb, credential, or keychain seam — every event is read
// out of the published snapshots the honest-state core already carries.
//
// Two design guarantees, both load-bearing:
//
//   * REDACTION (AC): a notification surfaces on the lock screen / Notification Center — MORE exposed
//     than the in-app panel — so it names the EVENT, never the account. The wire never carries an
//     email (issue #15); the redacted LABEL (operator handle) IS on the wire but is excluded here too.
//     The guarantee is STRUCTURAL, not by convention: `AccountEvent` carries no associated value, and
//     its notification content is a fixed constant per case — the labels the deriver compares (to
//     detect a swap, and to de-duplicate expiry) are never threaded into an event, so they physically
//     cannot reach the posted content.
//
//   * HONEST-STATE COUPLING: events are derived ONLY from a fresh `.connected` snapshot (the sole
//     healthy state, `HonestStateMachine`). Across a drop / stale / reconnect the baseline is dropped,
//     so a swap that may have happened during a gap is never mis-attributed, and a stale retained
//     roster never fires.
//
// TWO TRIGGER MODELS, not one, and which applies is a property of the fact rather than a style choice.
// `.swapped` / `.allExhausted` are EDGE-TRIGGERED off that baseline: they are transitions, knowable
// only by comparing consecutive snapshots, so they fire on the change and not on every snapshot that
// observes the condition — a fleet already exhausted at launch, or exhausted across many heartbeats,
// never re-spams. `.loginExpiring` is LEVEL-TRIGGERED off per-account memory: "this deadline is inside
// the horizon" is a standing condition readable from ONE frame, so requiring an observed transition
// would silently lose the crossing whenever it happened during a sleep, a disconnect, or before
// launch. `AccountEventDeriver`'s own doc carries the full argument for the split.
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
///
/// `CaseIterable` so the suites that assert a property of EVERY event — neutrality, non-emptiness,
/// pairwise distinctness, the grouping decision — enumerate the enum instead of a hand-written list.
/// A hand-written list is exactly how issue #935's third case would have shipped uncovered by four
/// existing gates that each looked green.
enum AccountEvent: Equatable, Sendable, CaseIterable {
    /// Sessiometer rotated the active session from one account to another.
    case swapped
    /// No account has a viable swap target left — the whole fleet is out of capacity (the daemon's
    /// `next_swap` = `no_viable_target`, `src/daemon/snapshot.rs` `NextSwap::NoViableTarget`).
    case allExhausted
    /// An account's REFRESH-token deadline has entered the operator's configured expiry horizon
    /// (`[credential].expiry_horizon_secs`, issue #935) — the login itself is approaching the wall no
    /// refresh moves. Fired ONE ACCOUNT AT A TIME (see `AccountEventDeriver`), never per cohort.
    case loginExpiring

    /// The notification headline — a neutral, static string (no account identity).
    var notificationTitle: String {
        switch self {
        case .swapped:        return "Active account switched"
        case .allExhausted:   return "All accounts exhausted"
        case .loginExpiring:  return "A login is inside its expiry horizon"
        }
    }

    /// The notification body — a neutral, static string summarizing the EVENT, never the account.
    ///
    /// `.loginExpiring` states the fact and NAMES THE VERB that replaces the credential, in that order
    /// and in the indicative — the §D-STA-6 / SUR-001 neutral-framing firewall binds operator-facing
    /// strings, so no "you must", no "act now", no forecast. Naming the verb is not performing it:
    /// `sessiometer login` is interactive by construction (`design-login.md` C1 — isolated temp dir,
    /// real browser exchange), and nothing on this path acquires a credential headlessly.
    ///
    /// It also points at the panel, which is load-bearing rather than helpful. The redaction guarantee
    /// means this string CANNOT name the account, so the operator's only route from "a login is
    /// expiring" to "which one" is the panel's per-row `EXPIRY` line — the same both-or-neither
    /// composition invariant issues #469/#498/#520/#523 established, read from the other direction.
    var notificationBody: String {
        switch self {
        case .swapped:      return "Sessiometer rotated to a different account."
        case .allExhausted: return "No account has capacity right now — action needed."
        case .loginExpiring:
            return "One account's refresh token expires within the configured horizon. "
                 + "The panel names it; sessiometer login replaces the credential."
        }
    }
}

// MARK: - The pure event deriver

/// One account's expiry modifier as the deriver sees it (issue #935) — the minimum input the expiry
/// rule needs, and deliberately not the whole `AccountRow`.
///
/// `label` is the DEDUP KEY ONLY. It is compared inside the deriver exactly as `activeLabel` is, and
/// like it is never threaded into a returned event: `AccountEvent` carries no associated value, so
/// there is nowhere for it to go. Non-secret either way — the wire never carries an email (issue #15).
struct ExpiryObservation: Equatable, Sendable {
    let label: String
    let expiry: AccountExpiry?

    init(label: String, expiry: AccountExpiry?) {
        self.label = label
        self.expiry = expiry
    }
}

/// Turns published snapshots into `AccountEvent`s. Pure and synchronous — no I/O, no clock of its own
/// (the caller passes `now`) — so every transition is unit-tested deterministically.
///
/// TWO TRIGGER MODELS, and which one applies is a property of the fact rather than a style choice.
///
/// **Edge-triggered, off a BASELINE** — `.swapped` and `.allExhausted`. These are TRANSITIONS: nothing
/// in a single snapshot says "a swap just happened", so they are knowable only by comparing the last
/// `.connected` snapshot's `(activeLabel, hasNoViableTarget)` against this one.
///   * `.swapped`      — the active account's label changed between two consecutive `.connected`
///                       snapshots (both non-nil; a transient loss of the active account, active→nil,
///                       is not a swap TO anything).
///   * `.allExhausted` — the snapshot ENTERED the no-viable-target state (false→true).
///
/// The baseline is dropped on any non-`.connected` state, so the first snapshot after a (re)connect
/// re-establishes it silently — a swap that may have happened across a disconnect is never invented,
/// and the first snapshot ever (or the first after a gap) fires nothing.
///
/// **Level-triggered, off PER-ACCOUNT MEMORY** — `.loginExpiring` (issue #935). "This deadline is
/// inside the horizon" is a STANDING CONDITION carried in the snapshot itself; there is no transition
/// to infer and nothing to be dishonest about, so it is read from the current frame and de-duplicated
/// by remembering which labels have already been named. Using the baseline here would silently LOSE
/// the crossing whenever it happened during a sleep, a disconnect, or before launch — precisely the
/// gaps a 7-day-wide horizon spans — and "observing a standing condition at launch is not an event"
/// is right for exhaustion (the daemon owns that, and the operator would be spammed every launch)
/// while being exactly wrong here: the standing condition IS the thing the operator needs told.
///
/// The memory is in-process, matching the baseline's, so a relaunch may re-name one still-expiring
/// account. That is bounded (at most one notification, then the spacing gate) and honest — the login
/// really is inside its horizon — and it is the cost of keeping this a pure value type with no
/// persisted state, which §3 of the credential-continuity design asks for.
struct AccountEventDeriver {
    private var baseline: Baseline?
    /// Labels currently inside the horizon that have ALREADY been named. Pruned to the in-horizon set
    /// on every ingest, so an account that leaves (a re-login pushed its deadline out, or it dropped
    /// off the roster) is forgotten and a later re-entry is named again — the repeatability the whole
    /// feature depends on, since every account re-enters roughly every grant.
    private var namedInHorizon: Set<String> = []
    /// When the last `.loginExpiring` was emitted, epoch seconds — the stagger's only state.
    private var lastExpiryNotificationAt: Int64?

    /// The minimum gap between two `.loginExpiring` notifications — the STAGGER constraint, and the
    /// reason this rule exists at all rather than just fanning out the in-horizon set.
    ///
    /// Issue #877 measured that each grant runs from ITS OWN LOGIN INSTANT (~28 d 11 h). So a cohort
    /// re-logged back-to-back in one sitting reproduces itself one grant later, and on the live fleet
    /// that is not hypothetical: four accounts sat inside a FOUR-MINUTE window. Notifying every
    /// in-horizon account at once is what leads an operator to do exactly that, which makes the fan-out
    /// the mechanism that rebuilds the wall. Spacing the notifications in TIME is what de-synchronizes
    /// the fleet, because the re-logins inherit the spacing and so does the next cohort.
    ///
    /// SIX HOURS, and the arithmetic is the argument. Against the 7-day default horizon a six-account
    /// fleet is fully named inside 30 h, so even the last account keeps ~5.7 days of lead — the stagger
    /// never eats the foresight it exists to spend. And at the far end it turns a 4-minute cluster into
    /// deaths ~6 h apart, which is the difference between "the fleet is down" and "one account is down
    /// while five work". Deliberately not derived from fleet size: a spacing that widened with the
    /// roster would push the last account's deadline out of the horizon on a large fleet, failing
    /// closed on exactly the fleets that need it most.
    static let expiryNotificationSpacingSecs: Int64 = 6 * 3600

    private struct Baseline: Equatable {
        let activeLabel: String?
        let hasNoViableTarget: Bool
    }

    /// Fold one published snapshot in and return the events it triggered, in
    /// `[.swapped, .allExhausted, .loginExpiring]` order (a swap INTO the last viable account that
    /// itself leaves no target fires the first two together).
    ///
    /// `activeLabel` is `rows.first(where: \.isActive)?.label`; `hasNoViableTarget` is
    /// `nextSwap == .noViableTarget`; `expiries` is one entry per roster row. Every label here — the
    /// active one and the expiry ones alike — is used ONLY for local comparison and is never carried
    /// into a returned event (the redaction guarantee).
    ///
    /// `expiries` defaults to empty and `now` to `0`: with no expiry axis observed there are no expiry
    /// events and `now` is never read, so the two swap/exhaustion callers stay unchanged. Defaulting it
    /// is not inert on deriver STATE, though — an empty roster prunes `namedInHorizon` to empty, exactly
    /// as a roster that legitimately lost every deadline would. A third caller that omitted `expiries`
    /// while the expiry axis was live would therefore re-arm accounts already named: the spacing gate
    /// still throttles the re-post, but the level trigger's memory of them is gone.
    ///
    /// `notificationsEnabled` is the operator's toggle, and it reaches the DERIVER rather than only the
    /// post because the two trigger models want opposite things from it — see `expiryEvents`.
    mutating func ingest(connectionState: ConnectionState,
                         activeLabel: String?,
                         hasNoViableTarget: Bool,
                         expiries: [ExpiryObservation] = [],
                         now: Int64 = 0,
                         notificationsEnabled: Bool = true) -> [AccountEvent] {
        // Only a fresh, healthy snapshot is a trustworthy event source, for BOTH trigger models: a
        // stale retained roster's deadlines are as untrustworthy as its active account. Any other
        // state drops the baseline so the next `.connected` re-seeds without inferring a swap across
        // the gap. The expiry memory deliberately SURVIVES the gap — it records what the operator has
        // been told, which a disconnect does not un-tell.
        guard connectionState == .connected else {
            baseline = nil
            return []
        }

        var events: [AccountEvent] = []
        // Edge-triggered. A missing baseline is the first healthy snapshot (fresh, or first after a
        // gap): seed it and fire nothing — observing a standing transition-less frame is not an event.
        if let previous = baseline {
            if let was = previous.activeLabel, let now = activeLabel, was != now {
                events.append(.swapped)
            }
            if hasNoViableTarget && !previous.hasNoViableTarget {
                events.append(.allExhausted)
            }
        }
        baseline = Baseline(activeLabel: activeLabel, hasNoViableTarget: hasNoViableTarget)

        // Level-triggered, and so evaluated on EVERY healthy frame including the first.
        events.append(contentsOf: expiryEvents(expiries, now: now, enabled: notificationsEnabled))
        return events
    }

    /// The expiry rule: at most ONE `.loginExpiring` per call, and only once the stagger has elapsed.
    ///
    /// Ordered by DEADLINE, soonest first, so the account with the least lead time is the one named —
    /// under a stagger that matters, since a fleet larger than the horizon divided by the spacing
    /// cannot all be named in time and the order decides who is.
    ///
    /// **A disabled toggle SKIPS this rule entirely rather than deriving into a bin**, which is the one
    /// place this deriver deliberately diverges from how the toggle treats swap and exhaustion. Those
    /// keep deriving while disabled so that enabling later does not replay a backlog of missed
    /// TRANSITIONS — a transition that went by while nobody was listening is genuinely gone, and
    /// re-announcing it would be inventing history. A standing CONDITION is not gone: the login is
    /// still inside its horizon. Marking it named while the operator cannot see the notification would
    /// consume it permanently, because `namedInHorizon` forgets an account only when it LEAVES the
    /// horizon — which happens at re-login, the very act the notification exists to prompt. The
    /// operator would re-enable notifications and hear nothing until the credential lapsed. Skipping
    /// costs nothing instead: the condition is re-derived from the next healthy frame.
    ///
    /// The PRUNE is the one step that still runs while disabled, and the asymmetry is deliberate:
    /// pruning only ever FORGETS, it never consumes. Holding it behind the toggle would strand an
    /// account that left and re-entered the horizon entirely inside a disabled window — re-logged in,
    /// then a whole grant later back inside — as still-named, silently skipping its next cycle.
    private mutating func expiryEvents(_ observations: [ExpiryObservation],
                                       now: Int64,
                                       enabled: Bool) -> [AccountEvent] {
        // Inside the horizon by the SAME predicate the panel brackets on — one function, so the
        // notification and the row it points at cannot disagree about which accounts are in play.
        let inHorizon = observations.filter { StatusPanelFormat.expiryWithinHorizon($0.expiry, now: now) }
        namedInHorizon.formIntersection(Set(inHorizon.map(\.label)))

        guard enabled else { return [] }

        let unnamed = inHorizon
            .filter { !namedInHorizon.contains($0.label) }
            .sorted { (Self.deadline(of: $0, now: now), $0.label) < (Self.deadline(of: $1, now: now), $1.label) }
        guard let next = unnamed.first else { return [] }

        // ONE AT A TIME, SPACED. Everything else stays unnamed rather than queued: the next healthy
        // frame re-derives the whole set from scratch, so a held account is picked up when the gap has
        // elapsed and one that left the horizon in the meantime is simply never named.
        if let last = lastExpiryNotificationAt, now - last < Self.expiryNotificationSpacingSecs {
            return []
        }
        namedInHorizon.insert(next.label)
        lastExpiryNotificationAt = now
        return [.loginExpiring]
    }

    /// The observed deadline behind an in-horizon observation, for ordering only.
    ///
    /// `.max` is unreachable for anything `expiryWithinHorizon` admitted (that predicate matches only
    /// `.live`, which carries a deadline); it orders such a row last rather than trapping, because a
    /// sort comparator is the wrong place to assert an invariant the caller already enforced.
    private static func deadline(of observation: ExpiryObservation, now: Int64) -> Int64 {
        if case .live(let at, _) = StatusPanelFormat.expiryView(observation.expiry, now: now) { return at }
        return .max
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
    /// ALWAYS runs (so the baseline stays current even while disabled) and the POST is gated, so
    /// enabling the toggle later never replays a backlog of missed deltas (AC: disabled ⇒ none posted).
    /// The toggle ALSO reaches the deriver, because the expiry rule must not consume a still-standing
    /// condition it was not allowed to announce — see `AccountEventDeriver.expiryEvents`.
    ///
    /// `now` is injected rather than read here so the expiry stagger is drivable from a test without a
    /// clock; the production caller passes the wall clock in `handleCurrent(of:)`.
    func handle(connectionState: ConnectionState,
                activeLabel: String?,
                hasNoViableTarget: Bool,
                expiries: [ExpiryObservation] = [],
                now: Int64 = 0) {
        let enabled = preferences.isEnabled
        let events = deriver.ingest(connectionState: connectionState,
                                    activeLabel: activeLabel,
                                    hasNoViableTarget: hasNoViableTarget,
                                    expiries: expiries,
                                    now: now,
                                    notificationsEnabled: enabled)
        guard enabled else { return }
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

    /// Read the store's settled projection and fold it in, against the wall clock the expiry rule
    /// needs. This is the ONLY place a clock is read on this path — everything below it is pure.
    private func handleCurrent(of store: WatchStatusStore) {
        let inputs = Self.projection(of: store)
        handle(connectionState: inputs.connectionState,
               activeLabel: inputs.activeLabel,
               hasNoViableTarget: inputs.hasNoViableTarget,
               expiries: inputs.expiries,
               now: Int64(Date().timeIntervalSince1970))
    }

    /// Project the store's published state into the deriver's inputs — the production store adapter.
    /// The active-account LABEL is extracted here (never leaving this shell — it feeds the deriver's
    /// comparison, never a posted event), `NextSwap.noViableTarget` is collapsed to the all-exhausted
    /// signal, and every row's expiry modifier is paired with its label for the per-account expiry
    /// memory. Internal (not private) so a test can lock these mappings, which the direct-input
    /// `handle(...)` tests deliberately bypass.
    ///
    /// The expiry projection is over the WHOLE roster, not just the active row: an account's login
    /// expires on its own schedule whether or not it happens to be serving traffic, and the parked
    /// spare whose credential dies unnoticed is the one that costs capacity at the next swap.
    static func projection(of store: WatchStatusStore)
        -> (connectionState: ConnectionState, activeLabel: String?, hasNoViableTarget: Bool,
            expiries: [ExpiryObservation]) {
        (store.connectionState,
         store.rows.first(where: \.isActive)?.label,
         isNoViableTarget(store.nextSwap),
         store.rows.map { ExpiryObservation(label: $0.label, expiry: $0.expiry) })
    }

    /// Whether the daemon reports no viable swap target — the "all accounts exhausted" signal.
    private static func isNoViableTarget(_ nextSwap: NextSwap?) -> Bool {
        if case .noViableTarget = nextSwap { return true }
        return false
    }
}
