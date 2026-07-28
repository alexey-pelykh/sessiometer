// Copyright (c) 2026 Oleksii PELYKH
// SPDX-License-Identifier: MIT

// The production `AccountEventPresenter` (issue #267): the one OS-bound collaborator behind the
// notification seam, wrapping `UNUserNotificationCenter`. Kept in its own file — like `main.swift`
// and `StatusItemController` — so it is compiled ONLY into the app target, never the headless
// `MenubarTests` bundle (which exercises the pure derivation + redaction + toggle-gating core via a
// spy). The authorization PROMPT and the actual notification DISPLAY are GUI/OS-bound and remain a
// manual pre-release verification step.
//
// WHAT CHANGED AT ISSUE #765, and why the last sentence of that paragraph used to be wider. This file
// previously also DECIDED what to deliver — it picked the fields, and minted the request identity, in
// the same breath as talking to the OS — and its header concluded "nothing here is unit-testable",
// which was true of the file and therefore true of the decision. That put the redaction guarantee's
// last mile in the one place no gate could see, on the surface (lock screen, Notification Center) that
// is MORE exposed than the panel. The decision now lives in `NotificationDelivery` — Foundation-only,
// compiled into `MenubarTests`, and gated by `NotificationDeliveryTests` — and what is left here is a
// field-by-field copy with no judgement in it. Keep it that way: a value chosen in this file rather
// than read off the plan is a value no test can reach. The gate enforces this literally, by reading
// this file's source: it rejects any `content.<field> =` the plan does not carry, and additionally
// requires `title` and `body` to be assigned `plan.title` / `plan.body` themselves — a field that keeps
// its audited name while its value comes from anywhere else is the cheap way to put an email back on
// the lock screen with the suite green.
//
// Zero egress (ADR-0011, #328): `UserNotifications` is a LOCAL OS delivery API — no host networking,
// no `URLSession`/`NWConnection`, no keychain, no store read — so the menu-bar app stays a pure
// local-socket client and the `check-menubar-zero-egress.sh` guard stays green.

import UserNotifications
import os

private let notifyLog = Logger(subsystem: "org.sessiometer.menubar", category: "notify")

@MainActor
final class UserNotificationPresenter: AccountEventPresenter {
    private let center = UNUserNotificationCenter.current()

    /// Request alert + sound permission. The OS shows its prompt at most once per install; a denial is
    /// the operator's choice (re-enabling is a System Settings action, not an in-app re-prompt).
    func requestAuthorization() {
        center.requestAuthorization(options: [.alert, .sound]) { granted, error in
            if let error {
                notifyLog.error("notification authorization request failed: \(String(describing: error), privacy: .public)")
            } else {
                notifyLog.info("notification authorization granted=\(granted, privacy: .public)")
            }
        }
    }

    /// Post one event's neutral content immediately (a `nil` trigger delivers now).
    ///
    /// Every value comes off `NotificationDelivery.plan(for:)` — the title and body (the event's fixed
    /// constant strings, so no account label or email can be set: the redaction AC), the grouping thread,
    /// and the per-post request identity that keeps distinct swap / exhaustion moments from coalescing.
    /// This method chooses NOTHING; see the file header for why that is a constraint and not a style.
    func present(_ event: AccountEvent) {
        let plan = NotificationDelivery.plan(for: event)
        let content = UNMutableNotificationContent()
        content.title = plan.title
        content.body = plan.body
        // Assigned only when the plan carries one: the shipping plan's `nil` means "leave the OS default",
        // so nothing is written here and the delivered object stays exactly what issue #267 shipped.
        if let thread = plan.threadIdentifier { content.threadIdentifier = thread }
        let request = UNNotificationRequest(identifier: plan.requestIdentifier, content: content, trigger: nil)
        center.add(request) { error in
            if let error {
                notifyLog.error("failed to post notification: \(String(describing: error), privacy: .public)")
            }
        }
    }
}
