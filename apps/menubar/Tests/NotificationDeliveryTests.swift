// Copyright (c) 2026 Oleksii PELYKH
// SPDX-License-Identifier: MIT

// The DELIVERED-notification gate (issue #765) — redaction asserted where the exposure happens.
//
// WHAT WAS MISSING, precisely. `AccountEventNotifierTests` already drives a sentinel label through the
// deriver and asserts it never reaches `event.notificationTitle` / `notificationBody`
// (`testSentinelLabelNeverReachesPostedNotificationContent`). That test is correct and stays. But it
// stops at the `AccountEventPresenter` SEAM: it asserts about the `AccountEvent` a spy recorded, not
// about the object the operating system receives. Everything past the seam — which fields get
// populated, what identity each post carries, whether two posts coalesce into one — lived in
// `UserNotificationPresenter`, which imports `UserNotifications` and so cannot compile into this bundle.
//
// That is the wrong side of the boundary for this particular guarantee. A notification renders on the
// LOCK SCREEN and in Notification Center, strictly more exposed than the in-app panel, and issue #15's
// invariant (the wire carries the operator-chosen label, NEVER an email) is what makes issue #754's
// committed panel goldens safe to commit in the first place. The last mile of that invariant was the one
// layer no gate could see.
//
// WHAT THIS SUITE ASSERTS, and why each part is load-bearing rather than a restatement. Three
// independent things, and the guarantee needs all three — any two without the third is theatre:
//
//   1. COMPLETENESS (`testThePresenterPopulatesNothingTheDeliveryPlanDoesNotCarry`). The plan is the
//      exhaustive description of what reaches `UNUserNotificationCenter`. This reads
//      `UserNotificationPresenter.swift`'s SOURCE and rejects any `content.<field> =` assignment the plan
//      does not carry, plus any request identity not taken from the plan. Without this pin, 2 and 3 below
//      are assertions about an object that may not be what is actually delivered. Same mechanism as
//      `StatusItemChromeTests.testTheUpstreamStateToGlyphTableStillExists` — a link, not a note.
//
//   2. EXHAUSTIVE SCANNING (`NotificationLeakScan`). The leak scan walks the plan by REFLECTION rather
//      than reading a hand-listed set of fields, so a field added to the plan tomorrow is scanned the day
//      it is added. A hand-listed scan would pass, unchanged and green, over a newly-leaking field. And a
//      value whose type the scanner does not recognise is a FAILURE, never a silent skip: a gate that
//      quietly ignores what it cannot read is exactly the degenerate pass this whole batch exists to
//      prevent.
//
//   3. END-TO-END DRIVE (`testASentinelLabelSurvivesNoStepOfTheChainIntoDeliveredContent`). The sentinel
//      enters as an account label on a real `WatchStatusStore` snapshot and travels the PRODUCTION path —
//      `AccountEventNotifier.projection` → `handle(...)` → the presenter seam → `NotificationDelivery`
//      — rather than being handed to the planner directly. Driving the planner directly would prove only
//      that a function given no secret returns no secret.
//
// CONSTRAINT-A (issue #748's batch constraint): every predicate here ships a MUTATION canary that feeds a
// deliberately-broken input through the SAME function the real assertion calls — never an
// inspection-only argument. The precedent this batch keeps citing is issue #437, where three render bugs
// were misread five times as "the DESIGN fails distinctness" and a golden authored then would have
// DEFENDED them. The canaries here are specifically: a plan-shaped type that leaks in an EXTRA field
// (proves the reflection scan grows with the plan), one that leaks in an existing field, one carrying a
// type the scanner cannot classify (proves an unreadable field reddens rather than passing), and a
// synthetic presenter source with an unaudited assignment (proves the source pin can reject).
//
// WHAT STAYS MANUAL, stated rather than left silent. The OS authorization PROMPT, and how Notification
// Center actually renders and stacks what it is given, are not decisions — they are `UNUserNotification
// Center` behaviour. No headless bundle can observe them, and `design/README.md` § Notification
// pre-release checklist is where they live.

#if DEBUG
import Foundation
import XCTest

// MARK: - The leak scanner (the predicate every assertion AND every canary routes through)
//
// Single-sourced deliberately: a canary exercising a PARALLEL copy of this logic would prove nothing
// about the predicate the real assertion uses.

enum NotificationLeakScan {

    /// One field's contribution to the scan.
    enum Contribution: Equatable {
        /// Text this field puts in front of the operator (zero strings for an empty optional).
        case text([String])
        /// A value the scanner cannot read. NOT a pass — the caller must fail on it.
        case unreadable(typeName: String)
    }

    /// Leaf types that provably carry no operator-visible text, so contributing nothing is correct rather
    /// than a blind spot. Deliberately an ALLOWLIST: anything absent from it is `unreadable`, which fails,
    /// so widening the delivery surface with a new value type is a decision someone has to make here.
    private static let textFreeLeafTypes: Set<String> = [
        "Bool", "Int", "Int32", "Int64", "UInt", "UInt8", "Double", "Float", "Date", "TimeInterval",
    ]

    /// Classify one value: the text it contributes, or the fact that it cannot be read.
    ///
    /// Containers (optionals, collections, dictionaries, tuples, and nested aggregates) are walked
    /// through, so a leak nested one level down is found. A childless value that is not a `String` and is
    /// not an allowlisted text-free leaf is `unreadable`.
    static func classify(_ value: Any) -> Contribution {
        if let text = value as? String { return .text([text]) }

        let mirror = Mirror(reflecting: value)
        let typeName = String(describing: type(of: value))

        if mirror.children.isEmpty {
            // An empty optional contributes nothing — that is a real "no text", not an unreadable value.
            if mirror.displayStyle == .optional { return .text([]) }
            return textFreeLeafTypes.contains(typeName) ? .text([]) : .unreadable(typeName: typeName)
        }

        var collected: [String] = []
        for child in mirror.children {
            switch classify(child.value) {
            case .text(let strings):     collected += strings
            case .unreadable(let inner): return .unreadable(typeName: inner)
            }
        }
        return .text(collected)
    }

    /// Every top-level field of `subject`, paired with what it contributes.
    ///
    /// By REFLECTION, never a hand-listed set of names — that is the whole point. A field added to
    /// `NotificationDeliveryPlan` appears here the moment it is declared, with no edit to this file, so a
    /// widened delivery surface cannot outrun its own gate.
    static func fields(of subject: Any) -> [(name: String, contribution: Contribution)] {
        Mirror(reflecting: subject).children.map {
            (name: $0.label ?? "<unlabelled>", contribution: classify($0.value))
        }
    }

    /// The names of fields whose text contains `secret`.
    static func leakedFields(of subject: Any, containing secret: String) -> [String] {
        fields(of: subject).filter { field in
            guard case .text(let strings) = field.contribution else { return false }
            return strings.contains { $0.contains(secret) }
        }.map(\.name)
    }

    /// The names of fields the scanner could not read — each one a hole in every verdict above.
    static func unreadableFields(of subject: Any) -> [String] {
        fields(of: subject).filter {
            if case .unreadable = $0.contribution { return true }
            return false
        }.map(\.name)
    }
}

// MARK: - Tests

@MainActor
final class NotificationDeliveryTests: XCTestCase {

    /// A label no legitimate string can contain, so a match is unambiguous.
    private let sentinel = "SENTINEL-LABEL-DO-NOT-LEAK"

    // MARK: - Scanner integrity: assert the scan is capable of seeing anything at all
    //
    // Every "no leak" verdict below is an ABSENCE claim, and absence is evidence only against a scan that
    // actually ran over a populated subject. These two guards are the `assertKnownPresent` of this file.

    /// The scan reaches EVERY declared field of the shipped plan, and can read all of them.
    func testTheScanReadsEveryFieldOfTheShippedPlan() {
        let plan = NotificationDelivery.plan(for: .swapped, requestIdentifier: "req-1")
        let scanned = NotificationLeakScan.fields(of: plan)

        XCTAssertEqual(Set(scanned.map(\.name)),
                       ["title", "body", "threadIdentifier", "requestIdentifier"], """
            the delivery plan's field set changed. This is not a test to re-tune — a NEW field is a NEW \
            channel to the lock screen, so confirm it carries no account identity, then record it here.
            """)
        XCTAssertEqual(NotificationLeakScan.unreadableFields(of: plan), [], """
            the scanner cannot read part of the shipped plan, so every "no secret leaked" verdict in this \
            file is void for those fields. Extend NotificationLeakScan.textFreeLeafTypes (if the type \
            provably carries no operator-visible text) or teach classify(_:) to walk it.
            """)
        // The scan must actually be looking at text, not at an empty set of strings.
        XCTAssertTrue(scanned.contains { field in
            guard case .text(let strings) = field.contribution else { return false }
            return strings.contains(AccountEvent.swapped.notificationTitle)
        }, "the scan did not find the title it is supposed to be reading — it is inspecting nothing")
    }

    /// CANARY — a field the scanner cannot classify must REDDEN, not pass quietly.
    func testTheScanRejectsAFieldItCannotRead() {
        struct PlanWithUnreadableField {
            let title = "Active account switched"
            /// `Data` is deliberately absent from the text-free allowlist: bytes can carry text.
            let opaquePayload = Data([0x01, 0x02])
        }
        let unreadable = NotificationLeakScan.unreadableFields(of: PlanWithUnreadableField())
        XCTAssertEqual(unreadable, ["opaquePayload"], """
            the scanner silently skipped a field whose type it does not understand. A gate that ignores \
            what it cannot read reports green on exactly the field most likely to hide a payload.
            """)
    }

    // MARK: - AC-2: the sentinel survives no step of the chain into DELIVERED content

    /// The end-to-end drive: the sentinel enters as a real account label on a real store snapshot and
    /// travels the production path all the way to the plan the presenter hands the OS.
    func testASentinelLabelSurvivesNoStepOfTheChainIntoDeliveredContent() {
        let recorder = PlanRecordingPresenter()
        let notifier = makeNotifier(presenter: recorder, enabled: true)

        // Two snapshots whose active-account labels are unique sentinels: the first seeds the baseline,
        // the second is a swap INTO exhaustion, which fires both events.
        for (label, exhausted) in [(sentinel + "-A", false), (sentinel + "-B", true)] {
            let projection = AccountEventNotifier.projection(of: store(activeLabel: label,
                                                                      exhausted: exhausted))
            // The label really is on the snapshot the notifier reads — otherwise the whole drive is
            // vacuous and every assertion below passes on an empty pipeline.
            XCTAssertEqual(projection.activeLabel, label, "the sentinel never reached the projection")
            notifier.handle(connectionState: projection.connectionState,
                            activeLabel: projection.activeLabel,
                            hasNoViableTarget: projection.hasNoViableTarget)
        }

        XCTAssertEqual(recorder.events, [.swapped, .allExhausted],
                       "the chain did not deliver the expected events — the drive proved nothing")
        XCTAssertEqual(recorder.plans.count, 2, "one delivery plan per posted event")

        for plan in recorder.plans {
            XCTAssertEqual(NotificationLeakScan.unreadableFields(of: plan), [],
                           "an unreadable field makes this plan's verdict void")
            // Non-vacuity, in place: the scan of THIS subject must be reading real text, or the two
            // "no leak" verdicts below are statements about an empty walk.
            XCTAssertTrue(NotificationLeakScan.fields(of: plan).contains { field in
                guard case .text(let strings) = field.contribution else { return false }
                return strings.contains(plan.title)
            }, "the scan of this delivered plan found no text at all — its verdict would be vacuous")
            XCTAssertEqual(NotificationLeakScan.leakedFields(of: plan, containing: sentinel), [], """
                the account label reached DELIVERED notification content. A notification renders on the \
                lock screen and in Notification Center — more exposed than the panel — so this is an \
                identity disclosure to anyone who can see the screen.
                  plan: \(plan)
                """)
            // The same scan, against the other shape of identity issue #15 forbids on the wire.
            XCTAssertEqual(NotificationLeakScan.leakedFields(of: plan, containing: "@"), [],
                           "delivered notification content contains an email-shaped token: \(plan)")
        }
    }

    /// CANARY — the leak scan must FIRE on content that does embed the label, or its silence above is
    /// worthless. Driven through `leakedFields`, the same function the assertion above calls.
    func testTheLeakScanFiresOnAPlanThatEmbedsTheLabel() {
        let leaking = NotificationDeliveryPlan(
            title: "Active account switched",
            body: "Sessiometer rotated to \(sentinel).",   // the defect this gate exists to catch
            threadIdentifier: nil,
            requestIdentifier: "req-1")
        XCTAssertEqual(NotificationLeakScan.leakedFields(of: leaking, containing: sentinel), ["body"], """
            the leak scan did not fire on a body that literally contains the sentinel — it cannot detect \
            the defect it exists to detect, so its green verdict on the real plan means nothing.
            """)
    }

    /// CANARY — the reflection scan must reach a field that does not exist today.
    ///
    /// This is the one that matters most over time. A scan reading a hand-listed set of field names would
    /// pass this canary's subject unchanged and green while the leak sat in the new field, so this proves
    /// the gate GROWS with the delivery surface instead of quietly falling behind it.
    func testTheLeakScanReachesAFieldThePlanDoesNotYetHave() {
        struct WidenedPlan {
            let title = "Active account switched"
            let body = "Sessiometer rotated to a different account."
            let threadIdentifier: String? = nil
            let requestIdentifier = "req-1"
            /// A plausible future addition — a subtitle naming the account.
            let subtitle: String
        }
        let widened = WidenedPlan(subtitle: "now on \(sentinel)")
        XCTAssertEqual(NotificationLeakScan.leakedFields(of: widened, containing: sentinel), ["subtitle"], """
            the scan missed a leak in a field that is not in today's plan. It must be driven by reflection \
            over whatever the plan declares — a fixed list of field names silently stops covering the \
            surface the moment someone widens it.
            """)
    }

    /// CONTROL for both canaries: the shipped plan, scanned by the same function, must come back clean —
    /// so the canaries are testing the leak and not merely "the scanner always finds something".
    func testTheLeakScanIsSilentOnTheShippedPlan() {
        for event in [AccountEvent.swapped, .allExhausted] {
            let plan = NotificationDelivery.plan(for: event, requestIdentifier: "req-1")
            XCTAssertEqual(NotificationLeakScan.leakedFields(of: plan, containing: sentinel), [],
                           "the shipped \(event) plan reports a leak of a label it never saw")
        }
    }

    // MARK: - AC-2: the plan is the COMPLETE delivery surface
    //
    // The pin that makes everything above a statement about what the OS receives rather than about a
    // struct that happens to be adjacent to it.

    func testThePresenterPopulatesNothingTheDeliveryPlanDoesNotCarry() throws {
        let source = try presenterSource()

        XCTAssertTrue(source.contains("NotificationDelivery.plan(for: event)"), """
            UserNotificationPresenter no longer builds its content from NotificationDelivery. Every \
            redaction assertion in this file is about the plan, so a presenter that decides for itself \
            again puts the delivered content back outside all of them.
            """)

        let planFields = Set(NotificationLeakScan.fields(of: NotificationDelivery.plan(for: .swapped,
                                                                                       requestIdentifier: "r"))
            .map(\.name))
        let writes = Self.assignedContentWrites(in: source)
        let assigned = Set(writes.keys)
        XCTAssertFalse(assigned.isEmpty,
                       "found no `content.<field> =` assignments at all — the extractor is not matching, "
                       + "so its verdict below would be vacuous")
        XCTAssertTrue(assigned.isSubset(of: planFields), """
            UserNotificationPresenter populates notification fields the delivery plan does not carry:
              unaudited: \(assigned.subtracting(planFields).sorted())
              plan:      \(planFields.sorted())
            Every such field is a channel to the lock screen that no test in this file scans. Add it to \
            NotificationDeliveryPlan (so the leak scan covers it), or stop setting it.
            """)

        // The field NAMES being audited is only half of it. `content.body = "…someone@example.com"` keeps
        // the name `body` and passes the subset check above while the delivered value stops being the
        // plan's — and every leak scan in this file reads the plan. So pin the two text channels to their
        // plan fields by provenance, not just by name.
        for field in ["title", "body"] {
            XCTAssertEqual(writes[field], "plan.\(field)", """
                UserNotificationPresenter sets content.\(field) from something other than plan.\(field) \
                — found `\(writes[field] ?? "<nothing>")`. That value reaches the lock screen without \
                passing through anything this file scans, so the redaction verdict here would no longer \
                be about what is delivered.
                """)
        }

        XCTAssertTrue(source.contains("identifier: plan.requestIdentifier"), """
            the UNNotificationRequest identity no longer comes from the plan, so \
            testEachPostCarriesItsOwnDeliveryIdentity asserts about a value the presenter does not use.
            """)
    }

    /// CANARY — the source extractor must REJECT a presenter that sets an unaudited field. Driven through
    /// `assignedContentFields`, the same extractor the assertion above uses, over a synthetic source.
    func testTheSourcePinFiresOnAnUnauditedContentAssignment() {
        let mutated = """
            content.title = plan.title
            content.body=plan.body
            content . interruptionLevel = .critical
            content.subtitle = event.accountLabel   // the unaudited channel
            if content.title == plan.title { }      // a COMPARISON, not an assignment
            """
        let assigned = Set(Self.assignedContentWrites(in: mutated).keys)
        XCTAssertEqual(assigned, ["title", "body", "interruptionLevel", "subtitle"], """
            the extractor did not see every assignment (or counted the `==` comparison as one), so the \
            real assertion's "no unaudited field" verdict is a statement about what the extractor happened \
            to match, not about the presenter.
              extracted: \(assigned.sorted())
            """)
        XCTAssertFalse(assigned.isSubset(of: Self.planFieldNames),
                       "the pin accepted a presenter that sets a field the plan does not carry")

        // The two indirections that cannot be resolved statically. Both are recorded under names no plan
        // field can match, so they always fail the subset check rather than resolving to something the
        // plan happens to carry — and neither is read past in silence.
        let viaKVC = Set(Self.assignedContentWrites(in: #"content.setValue(label, forKey: "subtitle")"#).keys)
        XCTAssertEqual(viaKVC, ["<KVC setValue(forKey:)>"],
                       "a KVC write to the content object slips past the pin entirely")
        XCTAssertFalse(viaKVC.isSubset(of: Self.planFieldNames), "a KVC write was accepted as audited")

        let viaSubscript = Set(Self.assignedContentWrites(
            in: #"content.userInfo["account"] = event.accountLabel"#).keys)
        XCTAssertEqual(viaSubscript, ["userInfo[…]"], """
            a subscript write slips past the pin entirely. `userInfo` is a real payload on the object \
            handed to UNUserNotificationCenter, and the character after the field name is `[`, not `=` — \
            so without an explicit branch the line is skipped in silence rather than flagged.
            """)
        XCTAssertFalse(viaSubscript.isSubset(of: Self.planFieldNames),
                       "a subscript write was accepted as audited")
    }

    /// CANARY — the pin must reject a presenter that keeps a plan field's NAME but sources its VALUE from
    /// somewhere else. Driven through `assignedContentWrites`, the same extractor the real assertion uses.
    ///
    /// This is the mutation that motivated the provenance half of the pin: the name-level subset check
    /// passes on this source, so without the `writes[field] == "plan.field"` assertion an email reaches
    /// the lock screen with the whole suite green.
    func testTheSourcePinFiresOnAPlanFieldSetFromSomethingOtherThanThePlan() {
        let mutated = """
            content.title = plan.title
            content.body = "Sessiometer rotated to someone@example.com"
            """
        let writes = Self.assignedContentWrites(in: mutated)
        XCTAssertTrue(Set(writes.keys).isSubset(of: Self.planFieldNames), """
            the NAME-level check passes on this mutation — which is the entire reason the provenance \
            assertion below has to exist. If this ever fails, this canary is no longer proving what it \
            claims to prove.
            """)
        XCTAssertNotEqual(writes["body"], "plan.body",
                          "the pin accepted content.body sourced from a literal instead of the plan")
        XCTAssertEqual(writes["title"], "plan.title",
                       "the provenance check must still ACCEPT the shipped form, or it fires on everything")
    }

    /// The plan's field names, as the pin's canaries assert against them. Kept next to the canaries rather
    /// than re-derived per test; the REAL pin reads them off the live plan by reflection instead.
    private static let planFieldNames: Set<String> = ["title", "body", "threadIdentifier",
                                                      "requestIdentifier"]

    // MARK: - AC-3: grouping / threading behaviour

    /// Each post carries its OWN identity, so distinct moments do not coalesce.
    ///
    /// `UNUserNotificationCenter` REPLACES an already-delivered request carrying the same identifier, so a
    /// constant here would mean the second swap of a session silently overwrote the first. This is the
    /// behaviour issue #267's presenter comment claims; nothing asserted it until now.
    func testEachPostCarriesItsOwnDeliveryIdentity() {
        let identities = (0..<64).map { _ in NotificationDelivery.freshRequestIdentifier() }
        XCTAssertEqual(Set(identities).count, identities.count, """
            two posts would share a request identifier, so the later notification REPLACES the earlier one \
            in Notification Center — a swap the operator never sees.
            """)
        for identity in identities {
            XCTAssertFalse(identity.isEmpty, "an empty identity coalesces every post into one")
            // Scanned as a PLAN field, never as a bare string: `Mirror` over a `String` has no children,
            // so scanning one directly would report "no leaks" without looking at anything — a
            // vacuously-satisfiable assertion. `testTheScanReadsEveryFieldOfTheShippedPlan` is what
            // establishes that a plan-shaped subject really is walked.
            let plan = NotificationDelivery.plan(for: .swapped, requestIdentifier: identity)
            XCTAssertEqual(NotificationLeakScan.leakedFields(of: plan, containing: "@"), [],
                           "the delivery identity carries an email-shaped token: \(identity)")
        }
        // And the plan actually threads the caller's identity through rather than minting its own.
        XCTAssertEqual(NotificationDelivery.plan(for: .swapped, requestIdentifier: "req-7").requestIdentifier,
                       "req-7")
    }

    /// The shipped grouping decision: no explicit thread, so macOS groups these under the app itself.
    ///
    /// Pinned as a DECISION rather than left implicit — issue #765 AC-3 asks for grouping to be covered or
    /// named as manual, and the half that is a decision is testable here. What Notification Center renders
    /// from it is the manual half (`design/README.md`).
    func testTheShippedGroupingDecisionIsAppLevelWithNoExplicitThread() {
        for event in [AccountEvent.swapped, .allExhausted] {
            XCTAssertNil(NotificationDelivery.plan(for: event).threadIdentifier, """
                the delivery plan now sets a thread identifier for \(event). That splits the app's \
                notifications into sub-stacks in Notification Center — a deliberate change if you meant \
                it, so update this assertion and the design/README.md checklist together; otherwise the \
                presenter is now writing a field issue #267 never delivered.
                """)
        }
    }

    /// Both event kinds deliver DISTINCT, non-empty content — so grouping them under one app stack still
    /// leaves them individually readable.
    func testTheTwoEventKindsDeliverDistinctNonEmptyContent() {
        let swapped = NotificationDelivery.plan(for: .swapped, requestIdentifier: "a")
        let exhausted = NotificationDelivery.plan(for: .allExhausted, requestIdentifier: "b")

        for plan in [swapped, exhausted] {
            XCTAssertFalse(plan.title.isEmpty, "an empty title renders as a blank notification")
            XCTAssertFalse(plan.body.isEmpty, "an empty body renders as a blank notification")
        }
        XCTAssertNotEqual(swapped.title, exhausted.title,
                          "the two event kinds are indistinguishable in a grouped stack")
        XCTAssertNotEqual(swapped.body, exhausted.body,
                          "the two event kinds are indistinguishable in a grouped stack")
    }

    // MARK: - Helpers

    /// Extract every write to the notification content object from presenter source, as
    /// `field name → the expression assigned to it`.
    ///
    /// A plain scan for `content.<identifier> = <rhs>`, rejecting `==`. Whitespace is stripped first so
    /// `content . subtitle = x` and `content.subtitle=x` are caught alongside the canonical spelling.
    /// `static` so the canaries drive the identical extractor over synthetic source.
    ///
    /// It returns the right-hand side, not just the name, because a name-only verdict is satisfied by
    /// `content.body = "…someone@example.com"` — the field stays audited while the VALUE stops coming
    /// from the plan every scan in this file inspects. Callers pin both halves.
    ///
    /// WHAT THIS IS AND IS NOT. It is a TRIPWIRE against the realistic regression — someone adding a line
    /// that populates another field on the content object — not a proof of non-population. A determined
    /// obfuscation defeats it: assigning through a local alias (`let c = content; c.subtitle = …`), or a
    /// helper in another file, would slip past. Closing those properly needs the presenter compiled into
    /// this bundle, which is exactly what `UNUserNotificationCenter` prevents (`project.yml`) and why
    /// this file exists at all. The two indirections that CANNOT be resolved statically but CAN be
    /// detected — `setValue(_:forKey:)` and a subscript write such as `content.userInfo["account"] = …` —
    /// are recorded below under names no plan field can match, so they always fail the caller's check
    /// rather than being read past.
    ///
    /// Its bias is deliberately toward FALSE POSITIVES: a mention of `content.foo = …` in a comment would
    /// be counted, producing a spurious red that a human then reads. That is the safe direction for a
    /// gate whose subject is an exposure surface — a false red costs a minute, a false green costs the
    /// guarantee.
    private static func assignedContentWrites(in source: String) -> [String: String] {
        var found: [String: String] = [:]
        for rawLine in source.split(separator: "\n", omittingEmptySubsequences: false) {
            let line = rawLine.filter { !$0.isWhitespace }
            guard let dot = line.range(of: "content.") else { continue }
            let rest = line[dot.upperBound...]
            // KVC would set a field under a name no static scan can resolve — treat any attempt as an
            // unaudited field rather than silently reading past it.
            if rest.hasPrefix("setValue(") { found["<KVC setValue(forKey:)>"] = ""; continue }
            let name = rest.prefix { $0.isLetter || $0.isNumber || $0 == "_" }
            guard !name.isEmpty else { continue }
            let after = rest[name.endIndex...]
            // A subscript write — `content.userInfo["account"] = …` — is a real channel into
            // `UNUserNotificationCenter`, keyed by a string this scan cannot resolve. Same treatment as
            // KVC: without this branch it is skipped in silence, because the character after the name is
            // `[` rather than `=`.
            if after.first == "[" { found["\(name)[…]"] = ""; continue }
            guard after.first == "=", after.dropFirst().first != "=" else { continue }
            var rhs = after.dropFirst()
            if let comment = rhs.range(of: "//") { rhs = rhs[..<comment.lowerBound] }
            if rhs.hasSuffix("}") { rhs = rhs.dropLast() }        // the `if let thread = … { … }` one-liner
            found[String(name)] = String(rhs)
        }
        return found
    }

    private func presenterSource() throws -> String {
        let url = URL(fileURLWithPath: #filePath)          // .../Tests/NotificationDeliveryTests.swift
            .deletingLastPathComponent()
            .deletingLastPathComponent()
            .appendingPathComponent("Sources/UserNotificationPresenter.swift")
        return try XCTUnwrap(try? String(contentsOf: url, encoding: .utf8),
                             "could not read \(url.lastPathComponent) — the completeness pin cannot run, "
                             + "so no verdict in this file is about delivered content")
    }

    /// A `.connected` snapshot with one active account under `activeLabel`.
    private func store(activeLabel: String, exhausted: Bool) -> WatchStatusStore {
        WatchStatusStore.preview(
            state: .connected,
            rows: [AccountRow(label: activeLabel, isActive: true, isEnabled: true, isQuarantined: false,
                              isRecovering: false, auth: nil, sessionPct: nil, weeklyPct: nil,
                              sessionResetsAt: nil, weeklyResetsAt: nil, weeklyExhausted: false,
                              isNextSwapTarget: false, blindActive: nil)],
            nextSwap: exhausted ? .noViableTarget(cause: .weekly, resetsAt: 2) : .target(to: "spare", reason: nil),
            generatedAt: 2)
    }

    private func makeNotifier(presenter: AccountEventPresenter, enabled: Bool) -> AccountEventNotifier {
        let suite = "org.sessiometer.menubar.tests.\(UUID().uuidString)"
        addTeardownBlock { UserDefaults().removePersistentDomain(forName: suite) }
        let prefs = NotificationPreferences(defaults: UserDefaults(suiteName: suite)!)
        prefs.isEnabled = enabled
        return AccountEventNotifier(preferences: prefs, presenter: presenter)
    }

    /// A presenter that does exactly what `UserNotificationPresenter` does MINUS the OS call: it builds
    /// the real delivery plan through the real planner and records it.
    ///
    /// This is what makes the drive end-to-end rather than a second model-layer test — the recorded value
    /// is the one the production presenter copies onto `UNMutableNotificationContent`, field for field,
    /// which `testThePresenterPopulatesNothingTheDeliveryPlanDoesNotCarry` pins.
    @MainActor
    private final class PlanRecordingPresenter: AccountEventPresenter {
        private(set) var events: [AccountEvent] = []
        private(set) var plans: [NotificationDeliveryPlan] = []

        func requestAuthorization() {}

        func present(_ event: AccountEvent) {
            events.append(event)
            plans.append(NotificationDelivery.plan(for: event))
        }
    }
}
#endif
