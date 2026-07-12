// Copyright (c) 2026 Oleksii PELYKH
// SPDX-License-Identifier: MIT

// Tests for the native swap / all-accounts-exhausted notifications (issue #267). They exercise the
// HEADLESS-TESTABLE core — the pure `AccountEventDeriver`, the neutral (redacted) `AccountEvent`
// content, the toggle gating, and `NotificationPreferences` persistence — against a spy presenter, so
// no `UNUserNotificationCenter` / OS surface is touched (the authorization prompt + actual display in
// `UserNotificationPresenter` are GUI/OS-bound, a manual pre-release step, and are not compiled into
// this bundle). Each test maps to an acceptance criterion; the load-bearing one is the redaction proof
// `testSentinelLabelNeverReachesPostedNotificationContent`.

import XCTest

@MainActor
final class AccountEventNotifierTests: XCTestCase {

    // MARK: - Deriver: swap detection (active account changed between consecutive `.connected` snapshots)

    func testFirstConnectedSnapshotEstablishesBaselineNoEvent() {
        var d = AccountEventDeriver()
        XCTAssertEqual(d.ingest(connectionState: .connected, activeLabel: "work", hasNoViableTarget: false), [])
    }

    func testActiveLabelChangeIsSwap() {
        var d = AccountEventDeriver()
        _ = d.ingest(connectionState: .connected, activeLabel: "work", hasNoViableTarget: false)
        XCTAssertEqual(d.ingest(connectionState: .connected, activeLabel: "personal", hasNoViableTarget: false),
                       [.swapped])
    }

    func testUnchangedActiveDoesNotFire() {
        var d = AccountEventDeriver()
        _ = d.ingest(connectionState: .connected, activeLabel: "work", hasNoViableTarget: false)
        XCTAssertEqual(d.ingest(connectionState: .connected, activeLabel: "work", hasNoViableTarget: false), [])
    }

    func testActiveGoingNilIsNotASwap() {
        // A transient loss of the active account is not a swap TO anything — never fires `.swapped`.
        var d = AccountEventDeriver()
        _ = d.ingest(connectionState: .connected, activeLabel: "work", hasNoViableTarget: false)
        XCTAssertEqual(d.ingest(connectionState: .connected, activeLabel: nil, hasNoViableTarget: false), [])
    }

    func testDisconnectResetsBaselineSoReconnectDoesNotFireSpuriousSwap() {
        var d = AccountEventDeriver()
        _ = d.ingest(connectionState: .connected, activeLabel: "work", hasNoViableTarget: false)
        _ = d.ingest(connectionState: .disconnected(reason: "connection closed (EOF)"),
                     activeLabel: "work", hasNoViableTarget: false)
        // Reconnect with a DIFFERENT active: a swap that may have happened across the gap is not
        // attributed — the first post-reconnect snapshot silently re-establishes the baseline.
        XCTAssertEqual(d.ingest(connectionState: .connected, activeLabel: "personal", hasNoViableTarget: false), [])
        // The NEXT change, now baselined, does fire.
        XCTAssertEqual(d.ingest(connectionState: .connected, activeLabel: "work", hasNoViableTarget: false),
                       [.swapped])
    }

    func testNonConnectedStatesNeverFire() {
        var d = AccountEventDeriver()
        XCTAssertEqual(d.ingest(connectionState: .stale, activeLabel: "work", hasNoViableTarget: true), [])
        XCTAssertEqual(d.ingest(connectionState: .emptyRoster, activeLabel: nil, hasNoViableTarget: false), [])
        XCTAssertEqual(d.ingest(connectionState: .connecting, activeLabel: "work", hasNoViableTarget: false), [])
        XCTAssertEqual(d.ingest(connectionState: .crashLooping, activeLabel: "work", hasNoViableTarget: true), [])
    }

    // MARK: - Deriver: all-accounts-exhausted (edge-triggered on entry into no-viable-target)

    func testEnteringNoViableTargetFiresAllExhaustedOnce() {
        var d = AccountEventDeriver()
        _ = d.ingest(connectionState: .connected, activeLabel: "work", hasNoViableTarget: false)
        XCTAssertEqual(d.ingest(connectionState: .connected, activeLabel: "work", hasNoViableTarget: true),
                       [.allExhausted])
        // Staying exhausted across further snapshots (e.g. heartbeats) does not re-fire.
        XCTAssertEqual(d.ingest(connectionState: .connected, activeLabel: "work", hasNoViableTarget: true), [])
    }

    func testExhaustedReFiresAfterRecovery() {
        var d = AccountEventDeriver()
        _ = d.ingest(connectionState: .connected, activeLabel: "work", hasNoViableTarget: false)
        _ = d.ingest(connectionState: .connected, activeLabel: "work", hasNoViableTarget: true)   // fires
        _ = d.ingest(connectionState: .connected, activeLabel: "work", hasNoViableTarget: false)  // capacity returned
        XCTAssertEqual(d.ingest(connectionState: .connected, activeLabel: "work", hasNoViableTarget: true),
                       [.allExhausted])
    }

    func testFirstSnapshotAlreadyExhaustedDoesNotFire() {
        // Launching into an already-exhausted fleet establishes the baseline only — no launch-time spam.
        var d = AccountEventDeriver()
        XCTAssertEqual(d.ingest(connectionState: .connected, activeLabel: "work", hasNoViableTarget: true), [])
    }

    func testSwapIntoExhaustionFiresBothInOrder() {
        var d = AccountEventDeriver()
        _ = d.ingest(connectionState: .connected, activeLabel: "work", hasNoViableTarget: false)
        // Rotated to the last viable account, which itself leaves no further target.
        XCTAssertEqual(d.ingest(connectionState: .connected, activeLabel: "spare", hasNoViableTarget: true),
                       [.swapped, .allExhausted])
    }

    // MARK: - AC: no account label / email appears in the notification content

    func testEventContentIsNeutralAndNonEmpty() {
        for event in [AccountEvent.swapped, .allExhausted] {
            let text = event.notificationTitle + " " + event.notificationBody
            XCTAssertFalse(text.isEmpty)
            XCTAssertFalse(text.contains("@"), "notification content must never contain an email")
        }
    }

    func testSentinelLabelNeverReachesPostedNotificationContent() {
        let spy = SpyPresenter()
        let notifier = makeNotifier(presenter: spy, enabled: true)
        let secret = "SENTINEL-LABEL-DO-NOT-LEAK"
        // Push snapshots whose active-account labels are unique sentinels through the full pipeline.
        notifier.handle(connectionState: .connected, activeLabel: secret + "-A", hasNoViableTarget: false)
        notifier.handle(connectionState: .connected, activeLabel: secret + "-B", hasNoViableTarget: true)
        XCTAssertEqual(spy.posted, [.swapped, .allExhausted])
        for event in spy.posted {
            let text = event.notificationTitle + " " + event.notificationBody
            XCTAssertFalse(text.contains(secret), "the redacted label must never appear in notification content")
            XCTAssertFalse(text.contains("@"))
        }
    }

    // MARK: - AC: toggle gating (disabled ⇒ none posted)

    func testDisabledSuppressesAllPosts() {
        let spy = SpyPresenter()
        let notifier = makeNotifier(presenter: spy, enabled: false)
        notifier.handle(connectionState: .connected, activeLabel: "work", hasNoViableTarget: false)
        notifier.handle(connectionState: .connected, activeLabel: "personal", hasNoViableTarget: true)
        XCTAssertEqual(spy.posted, [], "no notification is posted while the toggle is off")
    }

    func testEnabledPostsDerivedEvents() {
        let spy = SpyPresenter()
        let notifier = makeNotifier(presenter: spy, enabled: true)
        notifier.handle(connectionState: .connected, activeLabel: "work", hasNoViableTarget: false)
        notifier.handle(connectionState: .connected, activeLabel: "personal", hasNoViableTarget: false)
        XCTAssertEqual(spy.posted, [.swapped])
    }

    func testDisabledStillTracksBaselineSoEnablingDoesNotReplayBacklog() {
        let spy = SpyPresenter()
        let prefs = ephemeralPreferences()
        prefs.isEnabled = false
        let notifier = AccountEventNotifier(preferences: prefs, presenter: spy)
        notifier.handle(connectionState: .connected, activeLabel: "work", hasNoViableTarget: false)
        notifier.handle(connectionState: .connected, activeLabel: "personal", hasNoViableTarget: false) // swap, suppressed
        prefs.isEnabled = true
        // Enabling does not replay the missed swap: an unchanged snapshot posts nothing…
        notifier.handle(connectionState: .connected, activeLabel: "personal", hasNoViableTarget: false)
        XCTAssertEqual(spy.posted, [])
        // …only a NEW delta posts.
        notifier.handle(connectionState: .connected, activeLabel: "work", hasNoViableTarget: false)
        XCTAssertEqual(spy.posted, [.swapped])
    }

    // MARK: - NotificationPreferences persistence

    func testPreferencesDefaultOn() {
        let prefs = ephemeralPreferences()
        XCTAssertTrue(prefs.isEnabled, "notifications default on — the OS permission prompt is the real gate")
    }

    func testPreferencesPersistDisabledAcrossInstances() {
        let suite = "org.sessiometer.menubar.tests.\(UUID().uuidString)"
        defer { UserDefaults().removePersistentDomain(forName: suite) }
        let defaults = UserDefaults(suiteName: suite)!
        NotificationPreferences(defaults: defaults).isEnabled = false
        // A fresh instance over the same suite reads the persisted OFF value (not the ON default).
        XCTAssertFalse(NotificationPreferences(defaults: defaults).isEnabled)
    }

    // MARK: - Store adapter: production projection (locks the two mappings handle(...) tests bypass)

    func testStoreProjectionExtractsActiveLabelAndMapsNoViableTarget() {
        let store = WatchStatusStore.preview(
            state: .connected,
            rows: [row(label: "work", active: false), row(label: "spare", active: true)],
            nextSwap: .noViableTarget(cause: .weekly, resetsAt: 2),
            generatedAt: 2)
        let p = AccountEventNotifier.projection(of: store)
        XCTAssertEqual(p.connectionState, .connected)
        XCTAssertEqual(p.activeLabel, "spare", "the active-row label drives swap detection")
        XCTAssertTrue(p.hasNoViableTarget, "NextSwap.noViableTarget → the all-exhausted signal")
    }

    func testStoreProjectionActiveNilAndTargetIsNotExhausted() {
        let store = WatchStatusStore.preview(
            state: .connected,
            rows: [row(label: "work", active: false)],   // none active
            nextSwap: .target(to: "spare", reason: nil),
            generatedAt: 1)
        let p = AccountEventNotifier.projection(of: store)
        XCTAssertNil(p.activeLabel)
        XCTAssertFalse(p.hasNoViableTarget, "NextSwap.target is not exhausted")
    }

    // MARK: - Helpers

    /// A minimal `.connected`-roster row for the projection tests (only label + active matter here).
    private func row(label: String, active: Bool) -> AccountRow {
        AccountRow(label: label, isActive: active, isEnabled: true, isQuarantined: false,
                   isRecovering: false, auth: nil, sessionPct: nil, weeklyPct: nil,
                   sessionResetsAt: nil, weeklyResetsAt: nil, weeklyExhausted: false,
                   isNextSwapTarget: false)
    }

    private func ephemeralPreferences() -> NotificationPreferences {
        let suite = "org.sessiometer.menubar.tests.\(UUID().uuidString)"
        addTeardownBlock { UserDefaults().removePersistentDomain(forName: suite) }
        return NotificationPreferences(defaults: UserDefaults(suiteName: suite)!)
    }

    private func makeNotifier(presenter: AccountEventPresenter, enabled: Bool) -> AccountEventNotifier {
        let prefs = ephemeralPreferences()
        prefs.isEnabled = enabled
        return AccountEventNotifier(preferences: prefs, presenter: presenter)
    }

    /// An in-memory `AccountEventPresenter` that records what would be posted — no OS surface.
    @MainActor
    private final class SpyPresenter: AccountEventPresenter {
        private(set) var posted: [AccountEvent] = []
        func requestAuthorization() {}
        func present(_ event: AccountEvent) { posted.append(event) }
    }
}
