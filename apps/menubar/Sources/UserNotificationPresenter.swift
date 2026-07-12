// Copyright (c) 2026 Oleksii PELYKH
// SPDX-License-Identifier: MIT

// The production `AccountEventPresenter` (issue #267): the one OS-bound collaborator behind the
// notification seam, wrapping `UNUserNotificationCenter`. Kept in its own file — like `main.swift`
// and `StatusItemController` — so it is compiled ONLY into the app target, never the headless
// `MenubarTests` bundle (which exercises the pure derivation + redaction + toggle-gating core via a
// spy). The authorization prompt and the actual notification DISPLAY are GUI/OS-bound and are a
// manual pre-release verification step; nothing here is unit-testable.
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

    /// Post one event's neutral content immediately (a `nil` trigger delivers now). A fresh identifier
    /// per post so distinct swap / exhaustion moments each surface rather than coalescing. The content
    /// is the event's fixed constant strings — no account label or email is ever set (the redaction AC).
    func present(_ event: AccountEvent) {
        let content = UNMutableNotificationContent()
        content.title = event.notificationTitle
        content.body = event.notificationBody
        let request = UNNotificationRequest(identifier: UUID().uuidString, content: content, trigger: nil)
        center.add(request) { error in
            if let error {
                notifyLog.error("failed to post notification: \(String(describing: error), privacy: .public)")
            }
        }
    }
}
