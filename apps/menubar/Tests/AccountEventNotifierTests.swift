// Copyright (c) 2026 Oleksii PELYKH
// SPDX-License-Identifier: MIT

// Tests for the native account-activity notifications: swap / all-accounts-exhausted (issue #267) and
// the expiry-horizon prompt (issue #935). They exercise the HEADLESS-TESTABLE core — the pure
// `AccountEventDeriver`, the neutral (redacted) `AccountEvent` content, the toggle gating, and
// `NotificationPreferences` persistence — against a spy presenter, so no `UNUserNotificationCenter` /
// OS surface is touched (the authorization prompt + actual display in `UserNotificationPresenter` are
// GUI/OS-bound, a manual pre-release step, and are not compiled into this bundle). Each test maps to
// an acceptance criterion; the load-bearing ones are the two redaction proofs
// (`testSentinelLabelNeverReachesPostedNotificationContent`,
// `testSentinelExpiryLabelsNeverReachPostedNotificationContent`) and the composition-invariant pin
// `testEveryNotifiedStateAlsoRendersInThePanel`.
//
// The two event families are derived by DIFFERENT trigger models — swap / exhaustion edge-triggered
// off a baseline, expiry level-triggered off per-account memory — so several tests here exist
// specifically to hold the seam between them: `testAllThreeKindsCoincidingFireInAStableOrder`,
// `testTheExpiryMemorySurvivesADisconnectWhileTheBaselineDoesNot`, and the three
// `testDisabling…` / `testDisabled…` cases, which assert that the toggle treats the two families
// differently ON PURPOSE.

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
        for event in AccountEvent.allCases {
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

    // MARK: - Deriver: the expiry prompt (issue #935) — level-triggered, one at a time, spaced

    /// The clock these tests run against, and the horizon offsets they hang off it. Absolute rather
    /// than `Date()`-relative so every band is a stable byte and nothing flips mid-suite.
    ///
    /// `expiryNow` alone is `nonisolated`: `within(_:inDays:from:)` takes it as a DEFAULT ARGUMENT, which
    /// this target's Swift 5 language mode evaluates outside the actor — so an isolated read there warns on
    /// every build (issue #1109). Safe because it is an immutable `Sendable` `Int64` — the same split
    /// `PanelGeometry` makes for its ceiling arithmetic. `day` and `spacing` are only ever read as
    /// `Self.…` from main-actor bodies, so they stay isolated.
    private nonisolated static let expiryNow: Int64 = 1_893_456_000
    private static let day: Int64 = 86_400
    private static let spacing = AccountEventDeriver.expiryNotificationSpacingSecs

    /// The SHIPPED stagger, pinned as a value rather than only read as a symbol.
    ///
    /// Every other test in this section derives its clock from `Self.spacing`, so all of them stay
    /// green at ANY value — including `1`, which also silently empties the in-stagger sweep in
    /// `testASynchronizedCohortIsNamedOneAtATimeNeverFannedOut`. That is the concrete way this feature
    /// can break with the whole suite passing: the stagger IS the mitigation issue #877 measured, so a
    /// spacing quietly tuned to nothing reproduces the four-minute cohort the constraint exists to
    /// break, and nothing would say so.
    ///
    /// The bound is asserted alongside the exact value, so the reasoning survives a deliberate retune:
    /// the gap must be long enough that an operator does not clear the fleet in one sitting, and short
    /// enough that a six-account fleet is fully named well inside the 7-day default horizon.
    func testTheShippedStaggerIsSixHoursAndFitsAFleetInsideTheDefaultHorizon() {
        XCTAssertEqual(AccountEventDeriver.expiryNotificationSpacingSecs, 6 * 3600)

        let defaultHorizon = 7 * Self.day
        let fleet: Int64 = 6
        let drain = (fleet - 1) * AccountEventDeriver.expiryNotificationSpacingSecs
        XCTAssertLessThan(drain, defaultHorizon / 2,
                          "draining a \(fleet)-account fleet eats more than half the default horizon — "
                          + "the stagger would be spending the foresight it exists to protect")
        XCTAssertGreaterThanOrEqual(AccountEventDeriver.expiryNotificationSpacingSecs, 3600,
                                    "a sub-hour gap is one sitting — it would not de-synchronize the "
                                    + "cohort issue #877 measured at four minutes")
    }

    /// An account whose deadline sits INSIDE the horizon — the one state that notifies.
    private func within(_ label: String, inDays days: Int64 = 5, from now: Int64 = expiryNow) -> ExpiryObservation {
        ExpiryObservation(label: label,
                          expiry: AccountExpiry(expiresAt: now + days * Self.day, horizonState: .within))
    }

    func testAnAccountEnteringTheHorizonIsNamedOnceAndNotReFired() {
        var d = AccountEventDeriver()
        let now = Self.expiryNow
        // Enters the horizon on the very first healthy frame — level-triggered, so no baseline needed.
        XCTAssertEqual(d.ingest(connectionState: .connected, activeLabel: "work", hasNoViableTarget: false,
                                expiries: [within("work")], now: now),
                       [.loginExpiring])
        // Subsequent polls observe the SAME account in the SAME band. Advancing well past the stagger
        // proves the silence is the per-account memory and not merely the spacing gate.
        for step in [Int64(300), 3600, Self.spacing * 4] {
            XCTAssertEqual(d.ingest(connectionState: .connected, activeLabel: "work", hasNoViableTarget: false,
                                    expiries: [within("work")], now: now + step),
                           [], "re-fired for an account already named at step \(step)")
        }
    }

    func testAnAccountLeavingAndReEnteringTheHorizonIsNamedAgain() {
        var d = AccountEventDeriver()
        let now = Self.expiryNow
        _ = d.ingest(connectionState: .connected, activeLabel: "work", hasNoViableTarget: false,
                     expiries: [within("work")], now: now)
        // Re-logged in: the new grant pushes the deadline BEYOND the horizon, so the memory is dropped.
        let beyond = ExpiryObservation(label: "work",
                                       expiry: AccountExpiry(expiresAt: now + 29 * Self.day, horizonState: .beyond))
        XCTAssertEqual(d.ingest(connectionState: .connected, activeLabel: "work", hasNoViableTarget: false,
                                expiries: [beyond], now: now + Self.spacing),
                       [], "an account outside the horizon must not notify")
        // A grant later it comes back round — the deadline is 5 days out from THAT clock, not from the
        // original one, or the render-time staleness rule would read it as already lapsed. Without
        // re-naming, the feature works exactly once per install: every account re-enters roughly every
        // grant, so this is the load-bearing case.
        let laterNow = now + 30 * Self.day
        XCTAssertEqual(d.ingest(connectionState: .connected, activeLabel: "work", hasNoViableTarget: false,
                                expiries: [within("work", from: laterNow)], now: laterNow),
                       [.loginExpiring])
    }

    /// THE STAGGER (issue #877's measured constraint). A whole cohort inside the horizon at once must
    /// NOT fan out — notifying them together is what leads an operator to re-login them back-to-back
    /// and rebuild the four-minute cluster one grant later.
    func testASynchronizedCohortIsNamedOneAtATimeNeverFannedOut() {
        var d = AccountEventDeriver()
        let now = Self.expiryNow
        // Four accounts, all inside the horizon on the same frame — the live fleet's shape.
        let cohort = [within("a", inDays: 5), within("b", inDays: 4),
                      within("c", inDays: 3), within("d", inDays: 6)]

        let first = d.ingest(connectionState: .connected, activeLabel: "a", hasNoViableTarget: false,
                             expiries: cohort, now: now)
        XCTAssertEqual(first, [.loginExpiring], "a cohort must yield ONE notification, not \(first.count)")

        // Every poll before the stagger elapses stays silent, however many remain unnamed.
        var polledInsideTheStagger = 0
        for step in stride(from: Int64(300), to: Self.spacing, by: 1800) {
            XCTAssertEqual(d.ingest(connectionState: .connected, activeLabel: "a", hasNoViableTarget: false,
                                    expiries: cohort, now: now + step),
                           [], "fired again \(step)s in, inside the \(Self.spacing)s stagger")
            polledInsideTheStagger += 1
        }
        // Degenerate-subject guard: shrink the spacing and this loop empties, asserting nothing while
        // still reporting green — the exact shape of a silent coverage loss.
        XCTAssertGreaterThan(polledInsideTheStagger, 1,
                             "the in-stagger sweep ran \(polledInsideTheStagger) times — it is no "
                             + "longer exercising the gate it exists to hold")
        // Once it has, exactly one MORE is named — never the backlog at once.
        XCTAssertEqual(d.ingest(connectionState: .connected, activeLabel: "a", hasNoViableTarget: false,
                                expiries: cohort, now: now + Self.spacing),
                       [.loginExpiring])
    }

    /// Which account a FRESH deriver names first out of `cohort` — OBSERVED from the deriver rather
    /// than modelled by the test.
    ///
    /// The event carries no label (the redaction guarantee), so the only way to ask "which one did you
    /// name?" is to ask whether a candidate is still un-named: after the deriver has named one, a
    /// follow-up frame carrying ONLY `candidate` stays silent iff that candidate is the one already in
    /// `namedInHorizon`, and fires iff it is not. The probe prunes the other labels' memory as a side
    /// effect, which is exactly why each probe gets its own deriver and is discarded after it.
    private func namesFirst(_ cohort: [ExpiryObservation], candidate: String, at now: Int64) -> Bool {
        var d = AccountEventDeriver()
        XCTAssertEqual(d.ingest(connectionState: .connected, activeLabel: "x", hasNoViableTarget: false,
                                expiries: cohort, now: now),
                       [.loginExpiring], "the cohort should have named exactly one account")
        let probe = cohort.filter { $0.label == candidate }
        XCTAssertEqual(probe.count, 1, "candidate '\(candidate)' is not in the cohort")
        return d.ingest(connectionState: .connected, activeLabel: "x", hasNoViableTarget: false,
                        expiries: probe, now: now + Self.spacing).isEmpty
    }

    /// Named in DEADLINE order, soonest first: under a stagger a large fleet cannot all be named
    /// before its wall, so who goes first is a real decision and it belongs to the least lead time.
    ///
    /// This asserts what the DERIVER chose, via `namesFirst`. An earlier draft computed the expected
    /// order with its own `min` and never read the deriver's answer at all — which left the comparator
    /// completely untested: inverting it to name the LATEST deadline first, the exact opposite of the
    /// documented rule, kept the whole suite green.
    func testTheSoonestDeadlineIsNamedFirst() {
        let now = Self.expiryNow
        // `urgent` is listed LAST so the order cannot come from roster position.
        let cohort = [within("calm", inDays: 6), within("mid", inDays: 4), within("urgent", inDays: 1)]

        XCTAssertTrue(namesFirst(cohort, candidate: "urgent", at: now),
                      "the soonest deadline must be named first — it has the least lead time to spend")
        XCTAssertFalse(namesFirst(cohort, candidate: "mid", at: now))
        XCTAssertFalse(namesFirst(cohort, candidate: "calm", at: now))

        // …and the rule holds for the REST of the drain, not just the head: with `urgent` gone, `mid`
        // is next. A comparator that only happened to surface the extreme would pass the head check.
        let remaining = cohort.filter { $0.label != "urgent" }
        XCTAssertTrue(namesFirst(remaining, candidate: "mid", at: now))
        XCTAssertFalse(namesFirst(remaining, candidate: "calm", at: now))
    }

    /// CANARY for the probe above: it must be able to report BOTH answers. A probe wired so that it
    /// always returns `true` would pass every assertion in `testTheSoonestDeadlineIsNamedFirst` while
    /// proving nothing — the failure mode that test was rewritten to escape.
    func testTheOrderingProbeCanReportBothAnswers() {
        let now = Self.expiryNow
        let pair = [within("early", inDays: 1), within("late", inDays: 6)]
        XCTAssertTrue(namesFirst(pair, candidate: "early", at: now))
        XCTAssertFalse(namesFirst(pair, candidate: "late", at: now),
                       "the probe never returns false — it cannot distinguish named from un-named")
    }

    /// `beyond`, `lapsed` and the gap are NOT notifying bands.
    ///
    /// `lapsed` deliberately so: issue #884 recorded that escalating a lapse "would then owe a panel
    /// banner in the same change", and this item leaves `AUTH` and the status-item glyph untouched.
    /// The panel still shows the red `lapsed` row line, so the operator is not left uninformed — what
    /// is deferred is the glance-level cue, tracked separately.
    func testOnlyTheWithinBandNotifies() {
        let now = Self.expiryNow
        let nonNotifying: [(String, AccountExpiry?)] = [
            ("beyond", AccountExpiry(expiresAt: now + 29 * Self.day, horizonState: .beyond)),
            ("lapsed", AccountExpiry(expiresAt: now - Self.day, horizonState: .lapsed)),
            ("lapsed-undeclared", AccountExpiry(expiresAt: nil, horizonState: .lapsed)),
            ("unknown", AccountExpiry(expiresAt: nil, horizonState: .unknown)),
            ("unknown-stray-deadline", AccountExpiry(expiresAt: now + Self.day, horizonState: .unknown)),
            ("unpolled", nil),
        ]
        for (name, expiry) in nonNotifying {
            var d = AccountEventDeriver()
            XCTAssertEqual(d.ingest(connectionState: .connected, activeLabel: "work", hasNoViableTarget: false,
                                    expiries: [ExpiryObservation(label: name, expiry: expiry)], now: now),
                           [], "'\(name)' is not a notifying band")
        }
    }

    /// The render-time staleness rule reaches the notification too: a `within` deadline that has
    /// already passed by the time the client looks is `lapsed`, and must not be named as foresight.
    /// One `expiryView` means the notification cannot be a poll interval behind the row it points at.
    func testAWithinDeadlineAlreadyPastAtIngestDoesNotNotify() {
        var d = AccountEventDeriver()
        let now = Self.expiryNow
        let stale = ExpiryObservation(label: "work",
                                      expiry: AccountExpiry(expiresAt: now - 60, horizonState: .within))
        XCTAssertEqual(d.ingest(connectionState: .connected, activeLabel: "work", hasNoViableTarget: false,
                                expiries: [stale], now: now),
                       [])
    }

    /// The HONEST-STATE coupling covers expiry too: a stale or dropped connection retains its roster,
    /// and a retained roster's deadlines are exactly as untrustworthy as its active account.
    func testExpiryNeverFiresOffANonConnectedSnapshot() {
        let now = Self.expiryNow
        for state: ConnectionState in [.stale, .connecting, .crashLooping, .emptyRoster,
                                       .disconnected(reason: "connection closed (EOF)")] {
            var d = AccountEventDeriver()
            XCTAssertEqual(d.ingest(connectionState: state, activeLabel: "work", hasNoViableTarget: false,
                                    expiries: [within("work")], now: now),
                           [], "fired off a \(state) snapshot")
        }
    }

    /// A disconnect does not UN-TELL the operator: the per-account memory survives the gap, unlike the
    /// swap/exhaustion baseline, which is dropped precisely because a transition across a gap is
    /// unknowable. Level-triggered state has no such problem, and re-naming on every reconnect would
    /// make a flapping socket a notification source.
    func testTheExpiryMemorySurvivesADisconnectWhileTheBaselineDoesNot() {
        var d = AccountEventDeriver()
        let now = Self.expiryNow
        _ = d.ingest(connectionState: .connected, activeLabel: "work", hasNoViableTarget: false,
                     expiries: [within("work")], now: now)
        _ = d.ingest(connectionState: .disconnected(reason: "connection closed (EOF)"),
                     activeLabel: "work", hasNoViableTarget: false, expiries: [within("work")], now: now + 60)
        XCTAssertEqual(d.ingest(connectionState: .connected, activeLabel: "work", hasNoViableTarget: false,
                                expiries: [within("work")], now: now + Self.spacing * 2),
                       [], "re-named an account across a reconnect it had already been told about")
    }

    /// Expiry is LEVEL-triggered, so unlike swap/exhaustion it fires on the FIRST healthy frame.
    ///
    /// The alternative — requiring an observed transition — silently loses the crossing whenever it
    /// happens during a sleep, a disconnect, or before launch, which for a 7-day-wide horizon is most
    /// of the time. That the very first frame notifies is the fix, not an oversight.
    func testAnAlreadyInHorizonFleetIsNamedOnTheFirstHealthyFrame() {
        var d = AccountEventDeriver()
        XCTAssertEqual(d.ingest(connectionState: .connected, activeLabel: "work", hasNoViableTarget: false,
                                expiries: [within("work")], now: Self.expiryNow),
                       [.loginExpiring])
        // …whereas the exhaustion condition standing at launch still fires nothing (unchanged).
        var e = AccountEventDeriver()
        XCTAssertEqual(e.ingest(connectionState: .connected, activeLabel: "work", hasNoViableTarget: true),
                       [])
    }

    /// Ordering when several kinds coincide, pinned so the existing two-event contract is unchanged.
    func testAllThreeKindsCoincidingFireInAStableOrder() {
        var d = AccountEventDeriver()
        let now = Self.expiryNow
        _ = d.ingest(connectionState: .connected, activeLabel: "work", hasNoViableTarget: false, now: now)
        XCTAssertEqual(d.ingest(connectionState: .connected, activeLabel: "spare", hasNoViableTarget: true,
                                expiries: [within("spare")], now: now),
                       [.swapped, .allExhausted, .loginExpiring])
    }

    // MARK: - AC: the composition invariant (both or neither), held by construction

    /// **Every notified state has a panel presence.** The notification cannot name the account (the
    /// redaction guarantee), so the panel is the operator's only route from "a login is expiring" to
    /// "which one" — a notification firing over a silent panel is the exact failure issues
    /// #469/#498/#520/#523 established the both-or-neither invariant against.
    ///
    /// It holds BY CONSTRUCTION, not by review: the deriver fires on
    /// `StatusPanelFormat.expiryWithinHorizon` and the row brackets on the same call. This asserts the
    /// consequence end-to-end anyway — through the deriver, not through the predicate — so the pin
    /// survives someone deciding the two should be "independent" implementations.
    func testEveryNotifiedStateAlsoRendersInThePanel() {
        let now = Self.expiryNow
        // Sweep the horizon, plus the exact boundary shapes, and assert the implication on each.
        let candidates: [AccountExpiry?] = [
            AccountExpiry(expiresAt: now + 1, horizonState: .within),
            AccountExpiry(expiresAt: now + Self.day, horizonState: .within),
            AccountExpiry(expiresAt: now + 6 * Self.day + 23 * 3600, horizonState: .within),
            AccountExpiry(expiresAt: now + 29 * Self.day, horizonState: .beyond),
            AccountExpiry(expiresAt: now - Self.day, horizonState: .lapsed),
            AccountExpiry(expiresAt: nil, horizonState: .unknown),
            AccountExpiry(expiresAt: now, horizonState: .within),
            nil,
        ]
        var notifiedAtLeastOnce = false
        for expiry in candidates {
            var d = AccountEventDeriver()
            let fired = d.ingest(connectionState: .connected, activeLabel: "work", hasNoViableTarget: false,
                                 expiries: [ExpiryObservation(label: "work", expiry: expiry)], now: now)
            guard fired.contains(.loginExpiring) else { continue }
            notifiedAtLeastOnce = true

            // 1. The roster materializes the EXPIRY line at all…
            XCTAssertTrue(StatusPanelFormat.rosterShowsExpiry([expiry], now: now),
                          "notified while the panel would hide the expiry line entirely")
            // 2. …the row's own cell is not the gap…
            XCTAssertNotEqual(StatusPanelFormat.expiryCell(expiry, now: now), StatusPanelFormat.expiryGap,
                              "notified while the row renders the 'not observed' gap")
            // 3. …and it carries the within-horizon MARK, so the operator can pick the account out
            //    without colour — which is the whole route from notification to action.
            let drawn = StatusPanelFormat.expiryLineCell(expiry, now: now)
            XCTAssertTrue(drawn.hasPrefix("[") && drawn.hasSuffix("]"),
                          "notified while the row draws an unmarked '\(drawn)'")
        }
        // Degenerate-subject guard: an implication is vacuously true over an empty antecedent.
        XCTAssertTrue(notifiedAtLeastOnce,
                      "no candidate notified — the implication above was never actually exercised")
    }

    /// The redaction guarantee extended to the new axis: expiry labels are dedup keys and reach the
    /// deriver in bulk, so they are a second, wider channel into the same posted content.
    func testSentinelExpiryLabelsNeverReachPostedNotificationContent() {
        let spy = SpyPresenter()
        let notifier = makeNotifier(presenter: spy, enabled: true)
        let secret = "SENTINEL-EXPIRY-LABEL-DO-NOT-LEAK"
        notifier.handle(connectionState: .connected, activeLabel: nil, hasNoViableTarget: false,
                        expiries: [within(secret + "-A"), within(secret + "-B")], now: Self.expiryNow)
        XCTAssertEqual(spy.posted, [.loginExpiring], "the sentinel roster must have posted exactly once")
        for event in spy.posted {
            let text = event.notificationTitle + " " + event.notificationBody
            XCTAssertFalse(text.contains(secret), "an expiry label reached notification content")
            XCTAssertFalse(text.contains("@"))
        }
    }

    /// The toggle gates expiry posts, and a disabled window does NOT consume the condition.
    ///
    /// This is the one place the expiry rule diverges from how the toggle treats swap/exhaustion, and
    /// the divergence is the point. Those keep deriving while disabled so enabling never replays a
    /// backlog of missed TRANSITIONS. A standing CONDITION is different: the login is still inside its
    /// horizon. Consuming it while the operator cannot see the notification would silence it
    /// permanently, because the memory is only released when an account LEAVES the horizon — which
    /// happens at re-login, the very act the notification exists to prompt. The operator would turn
    /// notifications back on and hear nothing until the credential lapsed.
    func testDisablingDoesNotConsumeAStillStandingExpiryCondition() {
        let spy = SpyPresenter()
        let prefs = ephemeralPreferences()
        prefs.isEnabled = false
        let notifier = AccountEventNotifier(preferences: prefs, presenter: spy)
        let now = Self.expiryNow
        // Several polls go by with the toggle off — nothing is posted…
        for step in [Int64(0), 3600, Self.spacing * 2] {
            notifier.handle(connectionState: .connected, activeLabel: "work", hasNoViableTarget: false,
                            expiries: [within("work")], now: now + step)
        }
        XCTAssertEqual(spy.posted, [], "a disabled toggle must post nothing")

        // …and the account is still waiting to be named once the operator turns them back on.
        prefs.isEnabled = true
        notifier.handle(connectionState: .connected, activeLabel: "work", hasNoViableTarget: false,
                        expiries: [within("work")], now: now + Self.spacing * 3)
        XCTAssertEqual(spy.posted, [.loginExpiring],
                       "the disabled window consumed a login that is still inside its horizon — the "
                       + "operator would hear nothing about it until it lapsed")
    }

    /// The disabled window must not consume the STAGGER either. If `lastExpiryNotificationAt` advanced
    /// while nothing was posted, the first real notification after re-enabling would be held back by a
    /// gap the operator never got the benefit of.
    func testDisablingDoesNotConsumeTheStagger() {
        let spy = SpyPresenter()
        let prefs = ephemeralPreferences()
        prefs.isEnabled = false
        let notifier = AccountEventNotifier(preferences: prefs, presenter: spy)
        let now = Self.expiryNow
        notifier.handle(connectionState: .connected, activeLabel: "a", hasNoViableTarget: false,
                        expiries: [within("a"), within("b")], now: now)
        prefs.isEnabled = true
        // Re-enabled one minute later — well inside the 6 h gap a consumed stagger would have opened.
        notifier.handle(connectionState: .connected, activeLabel: "a", hasNoViableTarget: false,
                        expiries: [within("a"), within("b")], now: now + 60)
        XCTAssertEqual(spy.posted, [.loginExpiring],
                       "re-enabling was held back by a stagger that elapsed while nothing was posted")
    }

    /// The disabled window must still let the deriver FORGET. An account can leave the horizon (a
    /// re-login pushed its deadline out) and come back round a whole grant later entirely inside a
    /// disabled window; if the memory were frozen along with the emit, it would still be marked named
    /// when the operator re-enables and its whole new cycle would be skipped in silence.
    ///
    /// Pruning is the one step that survives the toggle, because forgetting can only ever un-silence.
    func testDisablingStillLetsAnAccountLeaveAndReEnterTheHorizon() {
        let spy = SpyPresenter()
        let prefs = ephemeralPreferences()
        let notifier = AccountEventNotifier(preferences: prefs, presenter: spy)
        let now = Self.expiryNow

        // Named once while enabled.
        notifier.handle(connectionState: .connected, activeLabel: "work", hasNoViableTarget: false,
                        expiries: [within("work")], now: now)
        XCTAssertEqual(spy.posted, [.loginExpiring])

        // Toggle off. The operator re-logs in (deadline pushed BEYOND), and a grant later it is back
        // inside the horizon — both transitions happening while notifications are off.
        prefs.isEnabled = false
        let beyond = ExpiryObservation(label: "work",
                                       expiry: AccountExpiry(expiresAt: now + 29 * Self.day, horizonState: .beyond))
        notifier.handle(connectionState: .connected, activeLabel: "work", hasNoViableTarget: false,
                        expiries: [beyond], now: now + Self.spacing)
        let laterNow = now + 30 * Self.day
        notifier.handle(connectionState: .connected, activeLabel: "work", hasNoViableTarget: false,
                        expiries: [within("work", from: laterNow)], now: laterNow)

        // Back on: the NEW cycle is still owed a notification.
        prefs.isEnabled = true
        notifier.handle(connectionState: .connected, activeLabel: "work", hasNoViableTarget: false,
                        expiries: [within("work", from: laterNow)], now: laterNow + 60)
        XCTAssertEqual(spy.posted, [.loginExpiring, .loginExpiring],
                       "an account that left and re-entered the horizon while disabled stayed marked "
                       + "as named — its whole new cycle would pass in silence")
    }

    /// …while swap and exhaustion keep their original rule: the deriver still tracks them while
    /// disabled, so enabling does NOT replay a transition that went by unheard.
    func testDisabledStillConsumesSwapAndExhaustionDeltas() {
        let spy = SpyPresenter()
        let prefs = ephemeralPreferences()
        prefs.isEnabled = false
        let notifier = AccountEventNotifier(preferences: prefs, presenter: spy)
        notifier.handle(connectionState: .connected, activeLabel: "work", hasNoViableTarget: false)
        notifier.handle(connectionState: .connected, activeLabel: "spare", hasNoViableTarget: true)
        prefs.isEnabled = true
        // The swap and the exhaustion both happened while disabled; the new baseline is current, so a
        // steady snapshot fires nothing rather than re-announcing history.
        notifier.handle(connectionState: .connected, activeLabel: "spare", hasNoViableTarget: true)
        XCTAssertEqual(spy.posted, [], "enabling replayed a transition that had already gone by")
    }

    // MARK: - Store adapter: the expiry projection

    func testStoreProjectionCarriesEveryRowsExpiryNotJustTheActiveOne() {
        let now = Self.expiryNow
        let expiring = AccountExpiry(expiresAt: now + 2 * Self.day, horizonState: .within)
        let store = WatchStatusStore.preview(
            state: .connected,
            rows: [row(label: "work", active: true),
                   row(label: "spare", active: false, expiry: expiring)],
            nextSwap: .target(to: "spare", reason: nil),
            generatedAt: now)
        let p = AccountEventNotifier.projection(of: store)
        XCTAssertEqual(p.expiries,
                       [ExpiryObservation(label: "work", expiry: nil),
                        ExpiryObservation(label: "spare", expiry: expiring)],
                       "a parked spare's login expires on its own schedule — the projection must "
                       + "carry the WHOLE roster, not only the active row")
    }

    // MARK: - Helpers

    /// A minimal `.connected`-roster row for the projection tests (only label + active + expiry matter).
    private func row(label: String, active: Bool, expiry: AccountExpiry? = nil) -> AccountRow {
        AccountRow(label: label, isActive: active, isEnabled: true, isQuarantined: false,
                   isRecovering: false, auth: nil, sessionPct: nil, weeklyPct: nil,
                   sessionResetsAt: nil, weeklyResetsAt: nil, weeklyExhausted: false,
                   isNextSwapTarget: false, blindActive: nil, expiry: expiry)
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
