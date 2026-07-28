// Copyright (c) 2026 Oleksii PELYKH
// SPDX-License-Identifier: MIT

// The notification DELIVERY decision (issue #765) — the pure functional core `UserNotificationPresenter`
// is the imperative shell of.
//
// WHY THIS FILE EXISTS. Issue #267 already put the notification MODEL under test: `AccountEventNotifier`
// derives events, `AccountEvent` carries no associated value, and `AccountEventNotifierTests` drives a
// sentinel label through the deriver and asserts it never reaches `event.notificationTitle` /
// `notificationBody`. That is a real guarantee, and it stops at the `AccountEventPresenter` seam.
// Everything AFTER the seam — what fields are actually populated on the object handed to the OS, what
// identity each post carries, whether posts coalesce — lived only in `UserNotificationPresenter`, which
// imports `UserNotifications` and therefore cannot compile into the headless `MenubarTests` bundle.
//
// So the redaction claim was asserted one layer ABOVE where the exposure actually happens. A
// notification renders on the lock screen and in Notification Center — strictly MORE exposed than the
// in-app panel — and the layer that decides what goes on it was the one layer no test could see.
//
// The fix is the same one this batch applied to `StatusItemController` / `main.swift` (issue #764,
// `StatusItemChrome` + `AppLaunchPlan`), to `SettingsView` (issue #762, `SettingsFormat`), and to the
// panel's layout constants (issue #750, `StatusPanelFormat`): the OS-bound file stays out of the bundle,
// and the DECISION moves down into a Foundation-only type the bundle already compiles.
// `UserNotificationPresenter` is left as a field-by-field copier with no judgement in it.
//
// WHAT MAKES THIS A COMPLETE SURFACE, and not just a second place to assert the same two strings.
// `NotificationDeliveryPlan` is the EXHAUSTIVE description of what reaches `UNUserNotificationCenter`:
// every field the presenter populates is a property here, and a field the presenter sets WITHOUT a
// property here would be an unaudited channel. That is not left to good intent —
// `NotificationDeliveryTests` reads `UserNotificationPresenter.swift`'s source and reddens if it assigns
// a field this plan does not carry, including through the two indirections a static scan cannot resolve
// but can detect (`setValue(_:forKey:)`, and a `content.userInfo[…]` subscript, which it records as
// unauditable rather than reading past). It also pins `title` and `body` by the EXPRESSION assigned, not
// only by name, so a field cannot keep its audited name while its value stops coming from this plan.
//
// What the pin does NOT prove is non-population through a local alias or a helper in another file — that
// needs the presenter compiled into the test bundle, which is precisely what `UNUserNotificationCenter`
// prevents. Within that boundary, a leak scan over the plan is a statement about what the operating
// system actually receives; outside it, this is a tripwire, and its own doc comment says so.

import Foundation

// MARK: - The plan

/// Everything that reaches `UNUserNotificationCenter` for one posted event.
///
/// Read the property list as a CONTRACT, not as a convenience struct: it is the complete set of values
/// `UserNotificationPresenter` is allowed to populate, and the source pin in `NotificationDeliveryTests`
/// is the tripwire holding it there — a tripwire against the realistic regression, not a proof (this
/// file's header states exactly what it cannot see). Adding a field here is how you widen the delivery
/// surface; adding one to the presenter without adding it here reddens the gate.
///
/// Every field is a NON-SECRET by construction, and the construction is the point: `plan(for:)` takes an
/// `AccountEvent`, which carries no associated value (issue #267), so there is no account-specific value
/// in scope at the moment the strings are chosen. The account label the deriver compares to notice a swap
/// never enters this file at all.
struct NotificationDeliveryPlan: Equatable, Sendable {

    /// The notification headline — the event's fixed constant string.
    let title: String

    /// The notification body — the event's fixed constant string.
    let body: String

    /// The notification's grouping thread, or `nil` for "do not set one".
    ///
    /// `nil` is what ships, and it is a decision rather than an omission: with no thread identifier macOS
    /// groups an app's notifications under the app itself in Notification Center, which is the grouping
    /// this app wants (two event kinds, both about the same fleet — sub-threading them would scatter a
    /// short list across two stacks for no gain). Modelled as an `Optional` rather than as `""` so the
    /// presenter assigns nothing at all in the shipping case, keeping the delivered object byte-identical
    /// to what issue #267 shipped.
    let threadIdentifier: String?

    /// The `UNNotificationRequest` identity for this post.
    ///
    /// Load-bearing, and the reason it is a plan field rather than an inline `UUID()` call at the post
    /// site: `UNUserNotificationCenter` REPLACES an already-delivered request that carries the same
    /// identifier. A constant here would therefore silently coalesce — the second swap of a session would
    /// overwrite the first instead of surfacing — so distinct swap and exhaustion moments each getting
    /// their own notification depends entirely on this being fresh per post. `NotificationDeliveryTests`
    /// asserts the freshness rather than trusting the comment.
    let requestIdentifier: String
}

// MARK: - The planner

/// The pure decision layer behind the notification seam — a caseless namespace of total functions, the
/// same shape as `StatusGauge` / `AppLaunchPlan` / `StatusItemChrome`.
enum NotificationDelivery {

    /// The grouping thread every post carries. See `NotificationDeliveryPlan.threadIdentifier` for why the
    /// shipping value is "none".
    static let threadIdentifier: String? = nil

    /// A fresh delivery identity. A v4 UUID: unique per call, and carrying no operator, account or host
    /// information — so a distinct identity costs nothing in exposure.
    ///
    /// Separated from `plan(for:requestIdentifier:)` so the planner itself stays a pure function of its
    /// inputs and the one non-deterministic step is isolated where a test can drive it directly.
    static func freshRequestIdentifier() -> String {
        UUID().uuidString
    }

    /// The complete delivery plan for one event.
    ///
    /// Pure: same inputs, same plan. The only values it can reach are `event`'s own fixed constants and
    /// the caller's identifier, which is why the redaction property is structural rather than reviewed —
    /// there is no account-bearing value in scope to leak even by accident.
    static func plan(for event: AccountEvent, requestIdentifier: String) -> NotificationDeliveryPlan {
        NotificationDeliveryPlan(title: event.notificationTitle,
                                 body: event.notificationBody,
                                 threadIdentifier: threadIdentifier,
                                 requestIdentifier: requestIdentifier)
    }

    /// The plan for one event under a freshly-minted identity — what the production presenter calls.
    static func plan(for event: AccountEvent) -> NotificationDeliveryPlan {
        plan(for: event, requestIdentifier: freshRequestIdentifier())
    }
}
