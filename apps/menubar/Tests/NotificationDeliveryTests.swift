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

// MARK: - The FRAMING vocabulary + its scanner (DERIVED from Rust, never hand-copied)
//
// WHY THIS EXISTS (issue #1219). This file used to carry its own list —
// `["buy", "purchase", "upgrade", "cancel", "bypass", "beat", "need more"]` — maintained by hand in a
// language that cannot reach `crate::framing_vocabulary`. That is the exact hazard issue #918 hoisted
// the vocabulary to end and #1134 closed on the Rust side: a second definition of the banned set that
// nothing asserts agrees with the first. The cross-language boundary made it worse rather than better —
// no Rust change could red this copy.
//
// It was NARROWER than "a snapshot of `src/cli.rs`'s pre-#1134 `EXPIRY_HELP_BANNED_TOKENS`" suggests,
// and the difference is why the fix is a derivation rather than a top-up. Read against that constant as
// it stood at 7efaad1^, this list carried only its FIRST group — issue #885's enumerated acquisitive
// call — and none of the recommendation (`should`, `must`, `recommend`, …) or alarmist projection
// (`forecast`, `imminent`, `soon`, …) groups that constant already had. The value judgements
// (`healthy`, `critical`, `risk`, …) were absent from both, and arrived on the central list. Its
// seventh member came from a third place again: `need more` was a member of the sibling
// `EXPIRY_HELP_BANNED_PHRASES`, so folding a two-word phrase into a token list is this file's own
// conflation rather than one it inherited.
//
// THE DECISION, of the three shapes issue #1219 put up. This is shape 1 (single source of truth), but
// realised by READING `src/framing_vocabulary.rs` at test time rather than by emitting a JSON fixture
// under `build/fixtures/`. A fixture is a SECOND artifact whose agreement with the first then needs its
// own gate; reading the source leaves exactly ONE definition, so "the two lists disagree" is not a state
// this repo can be in. It is also the pattern this very file already uses — `presenterSource()` reads
// `Sources/UserNotificationPresenter.swift` for the completeness pin, as `StatusItemChromeTests` does
// for the glyph table.
//
// The fixture shape had one real advantage over this and it is worth stating rather than burying:
// `build/fixtures/**` is in the `swift` job's path filter, so a re-emitted fixture re-runs this guard in
// the same PR that changes the vocabulary — which is precisely the residual recorded below and kept.
// The trade taken is one definition plus a stated detection latency, over two definitions plus a gate
// holding them equal.
//
// Shape 3 — "the two guards are legitimately independent" — was WEIGHED AND REFUSED for the banned
// list, and ACCEPTED for the imperative one. §D-STA-6 / SUR-001 is one firewall, and
// `framing_vocabulary`'s module doc names its subject as "the FRAMING contract every operator-facing
// prose surface owes". A notification renders on the LOCK SCREEN; it is such a surface, so its
// vocabulary is shared, not merely similar. The imperative list below is the other way round: the
// central list bans four groups and the imperative MOOD is deliberately not one of them (that module's
// own words: "ZERO central tokens, because BANNED_TOKENS never banned the imperative MOOD"), so it has
// no central counterpart to derive from and stays local — recorded here rather than left implicit.
//
// WHAT THIS SURFACE IS MEASURED AGAINST, and it is a measurement rather than a guess — issue #1219 names
// that measurement as the first step of any fix, since it had not been taken. Every shipped title+body
// was scanned against the whole central vocabulary and both phrases through the central tokenizer: zero
// hits. So this audience takes `BANNED_TOKENS` WHOLE with NO exemption set — the strictest row of
// `framing_vocabulary`'s table, alongside stats and the `Error` templates — and the screen below is not
// carrying a carve-out it never earned. The count is deliberately not written down here: the screen
// re-runs that measurement on every run, which a number in a comment cannot.
//
// WHAT THIS DOES NOT REACH. `src/**` is not in the `swift` job's path filter (`apps/menubar/**`,
// `build/fixtures/**`, `.github/workflows/ci.yml`), so a PR that edits `BANNED_TOKENS` alone does not
// re-run this guard: a central token that a notification string spends would redden the NEXT
// `apps/menubar/**` PR instead of the one that introduced it. That is a detection-LATENCY residual, not
// a drift one — there is still only one list — and it is strictly better than the copy it replaces,
// which could not redden at all. Closing it is a one-line addition of this file's dependency to that
// filter; it is deliberately not taken here, because a path-filter edit is a change to the gates
// themselves (`CLAUDE.md` § Before you push) and now also has to move that file's verbatim enumeration
// of the filters, so it is a change to argue on its own rather than ride in on a test fix.

enum FramingVocabularyScan {

    /// A parse that produced no usable vocabulary. Thrown, never returned as an empty list: a screen
    /// derived from nothing passes everything, which is the degenerate green this whole suite exists to
    /// refuse (`NotificationLeakScan`'s `unreadable` is the same decision one layer down).
    enum ParseFailure: Error, CustomStringConvertible {
        case constantNotFound(String)
        case unterminated(String)
        case noMembers(String)

        var description: String {
            switch self {
            case .constantNotFound(let name):
                return "src/framing_vocabulary.rs declares no `pub const \(name): &[&str] = &[` — the "
                     + "constant was renamed or restructured, so this file's framing screen has no "
                     + "vocabulary and would pass everything. Re-point the parser at its new shape."
            case .unterminated(let name):
                return "`\(name)` in src/framing_vocabulary.rs is not terminated by `];` — the parse "
                     + "cannot tell where the list ends."
            case .noMembers(let name):
                return "`\(name)` in src/framing_vocabulary.rs parsed to ZERO members. A screen with an "
                     + "empty vocabulary is green over every input, so this is a failure and not a pass."
            }
        }
    }

    /// `src/framing_vocabulary.rs` — the ONE definition of this contract, resolved relative to this
    /// source file exactly as `presenterSource()` resolves its sibling.
    static func vocabularySourceURL() -> URL {
        URL(fileURLWithPath: #filePath)      // apps/menubar/Tests/NotificationDeliveryTests.swift
            .deletingLastPathComponent()     // apps/menubar/Tests
            .deletingLastPathComponent()     // apps/menubar
            .deletingLastPathComponent()     // apps
            .deletingLastPathComponent()     // <repo root>
            .appendingPathComponent("src/framing_vocabulary.rs")
    }

    /// The members of a `pub const <name>: &[&str] = &[ … ];` in Rust source.
    ///
    /// Line comments are stripped BEFORE any literal is extracted, and that ordering is load-bearing
    /// rather than tidy: `BANNED_TOKENS`' own group comments quote the very words they introduce
    /// (`// Imperatives / recommended actions (issue #160: "add / buy / upgrade / cancel /`), so
    /// extracting first would admit `add / buy / upgrade / cancel /` as a single "token" — an entry no
    /// tokenizer can ever match, silently widening the list with a dead member.
    static func rustStringList(named name: String, in source: String) throws -> [String] {
        let uncommented = source
            .split(separator: "\n", omittingEmptySubsequences: false)
            .map { line -> Substring in
                guard let comment = line.range(of: "//") else { return line }
                return line[..<comment.lowerBound]
            }
            .joined(separator: "\n")

        guard let decl = uncommented.range(of: "pub const \(name): &[&str] = &[") else {
            throw ParseFailure.constantNotFound(name)
        }
        guard let end = uncommented.range(of: "];", range: decl.upperBound..<uncommented.endIndex) else {
            throw ParseFailure.unterminated(name)
        }
        // Odd-indexed components of a split on `"` are the quoted literals.
        let parts = String(uncommented[decl.upperBound..<end.lowerBound]).components(separatedBy: "\"")
        guard parts.count >= 3, parts.count % 2 == 1 else { throw ParseFailure.noMembers(name) }
        return stride(from: 1, to: parts.count, by: 2).map { parts[$0] }
    }

    /// The tokenizer, in parity with `crate::framing_vocabulary::words_of`: ANSI SGR runs dropped first,
    /// then whole lowercase words split on non-ASCII-alphanumeric boundaries, in READING ORDER.
    ///
    /// The SGR half is inert on this surface — a notification body carries no escape codes — and is
    /// implemented anyway because parity is the point of the whole change, not a nicety: a second
    /// answer to "what counts as a word" is half of what issue #1219 filed. It is covered rather than
    /// merely written, by the colour-wrapped case in `testTheSharedScannerMatchesTheRustWordSemantics`.
    static func words(of text: String) -> [String] {
        var plain = ""
        var inEscape = false
        for character in text {
            if inEscape {
                if character == "m" { inEscape = false }
            } else if character == "\u{1B}" {
                inEscape = true
            } else {
                plain.append(character)
            }
        }
        return plain
            .split(whereSeparator: { !($0.isASCII && ($0.isLetter || $0.isNumber)) })
            .map { $0.lowercased() }
    }

    /// Whether `phrase`'s own words appear as ADJACENT words in `words` — parity with
    /// `phrase_present`, so a neutral render never false-trips (`laptop update` is not `top up`).
    ///
    /// The pattern goes through `words(of:)` too, which is what lets this carry the local imperative
    /// patterns unchanged: `don't forget` tokenizes on both sides identically, and `please` no longer
    /// needs the trailing space the retired substring scan used to keep it off `pleasant`.
    static func phrasePresent(_ phrase: String, in words: [String]) -> Bool {
        let parts = self.words(of: phrase)
        guard !parts.isEmpty, words.count >= parts.count else { return false }
        return (0...(words.count - parts.count)).contains {
            Array(words[$0..<($0 + parts.count)]) == parts
        }
    }

    /// EVERY hit in `text`: single banned words in list order, then adjacent-word phrases — parity with
    /// `scan_all_with`. Every hit rather than the first, so a second banned token cannot hide behind a
    /// pinned first one, and so a failure names WHICH word it tripped on.
    static func hits(in text: String, tokens: [String], phrases: [String]) -> [String] {
        let textWords = words(of: text)
        return tokens.filter { token in textWords.contains(token) }
             + phrases.filter { phrasePresent($0, in: textWords) }
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

    /// The SAME drive for the expiry channel (issue #935), past the same seam.
    ///
    /// Worth its own case rather than a row in the one above: expiry opened a SECOND and wider label
    /// channel into the deriver. `ExpiryObservation` carries one label per ROSTER ROW — not just the
    /// active account — so a fleet's worth of handles now reaches the derivation layer on every frame,
    /// where before only the active label did. The model-layer proof lives in `AccountEventNotifierTests`;
    /// this file exists because that proof stops at the `AccountEventPresenter` seam, and the exposure
    /// happens past it.
    func testASentinelExpiryLabelSurvivesNoStepOfTheChainIntoDeliveredContent() throws {
        let recorder = PlanRecordingPresenter()
        let notifier = makeNotifier(presenter: recorder, enabled: true)
        let now = Int64(1_893_456_000)
        let expiring = AccountExpiry(expiresAt: now + 3 * 86_400, horizonState: .within)

        let projection = AccountEventNotifier.projection(of: store(activeLabel: sentinel + "-EXPIRY",
                                                                  exhausted: false,
                                                                  expiry: expiring))
        // The sentinel really is on the expiry channel the notifier reads — otherwise the drive is
        // vacuous and every assertion below passes on an empty pipeline.
        XCTAssertEqual(projection.expiries.map(\.label), [sentinel + "-EXPIRY"],
                       "the sentinel never reached the expiry projection")
        notifier.handle(connectionState: projection.connectionState,
                        activeLabel: projection.activeLabel,
                        hasNoViableTarget: projection.hasNoViableTarget,
                        expiries: projection.expiries,
                        now: now)

        XCTAssertEqual(recorder.events, [.loginExpiring],
                       "the chain did not deliver the expiry event — the drive proved nothing")
        let delivered = try XCTUnwrap(recorder.plans.first, "one delivery plan per posted event")
        XCTAssertEqual(NotificationLeakScan.unreadableFields(of: delivered), [],
                       "an unreadable field makes this plan's verdict void")
        XCTAssertTrue(NotificationLeakScan.fields(of: delivered).contains { field in
            guard case .text(let strings) = field.contribution else { return false }
            return strings.contains(delivered.title)
        }, "the scan of this delivered plan found no text at all — its verdict would be vacuous")
        XCTAssertEqual(NotificationLeakScan.leakedFields(of: delivered, containing: sentinel), [], """
            an account label reached DELIVERED notification content through the EXPIRY channel. A \
            notification renders on the lock screen and in Notification Center — more exposed than the \
            panel — so this is an identity disclosure to anyone who can see the screen.
              plan: \(delivered)
            """)
        XCTAssertEqual(NotificationLeakScan.leakedFields(of: delivered, containing: "@"), [],
                       "delivered notification content contains an email-shaped token: \(delivered)")
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
        for event in AccountEvent.allCases {
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

        let planFields = Self.planFieldNames
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

    /// The shipped plan's field names, read off a LIVE plan by the same reflection the leak scan uses —
    /// what the real pin above and both of its canaries assert against.
    ///
    /// Deliberately NOT a hand-listed copy sitting beside the canaries: a canary judged against a parallel
    /// list would keep passing against yesterday's surface the day the plan widens, which is precisely the
    /// failure `testTheLeakScanReachesAFieldThePlanDoesNotYetHave` exists to rule out one layer up. The set
    /// is spelled out in exactly ONE place — `testTheScanReadsEveryFieldOfTheShippedPlan` — where recording
    /// a new field is a deliberate act rather than a silent widening.
    private static var planFieldNames: Set<String> {
        Set(NotificationLeakScan.fields(of: NotificationDelivery.plan(for: .swapped,
                                                                      requestIdentifier: "req-1"))
            .map(\.name))
    }

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
                       "req-7", """
            the planner minted its own identity instead of threading the caller's, so the uniqueness \
            asserted above is a property of freshRequestIdentifier() and not of what a post carries.
            """)
    }

    /// The shipped grouping decision: no explicit thread, so macOS groups these under the app itself.
    ///
    /// Pinned as a DECISION rather than left implicit — issue #765 AC-3 asks for grouping to be covered or
    /// named as manual, and the half that is a decision is testable here. What Notification Center renders
    /// from it is the manual half (`design/README.md`).
    func testTheShippedGroupingDecisionIsAppLevelWithNoExplicitThread() {
        for event in AccountEvent.allCases {
            XCTAssertNil(NotificationDelivery.plan(for: event).threadIdentifier, """
                the delivery plan now sets a thread identifier for \(event). That splits the app's \
                notifications into sub-stacks in Notification Center — a deliberate change if you meant \
                it, so update this assertion and the design/README.md checklist together; otherwise the \
                presenter is now writing a field issue #267 never delivered.
                """)
        }
    }

    /// EVERY event kind delivers DISTINCT, non-empty content — so grouping them under one app stack
    /// still leaves them individually readable.
    ///
    /// Enumerated over `AccountEvent.allCases` rather than a hand-written pair, and that is the whole
    /// point of the conformance: issue #935's third case would otherwise have been admitted to the
    /// grouped stack by a test that named only the first two and stayed green.
    func testEveryEventKindDeliversDistinctNonEmptyContent() {
        let plans = AccountEvent.allCases.map { (event: $0, plan: NotificationDelivery.plan(for: $0)) }
        XCTAssertGreaterThan(plans.count, 1, "a single-case enum cannot exercise distinctness")

        for (event, plan) in plans {
            XCTAssertFalse(plan.title.isEmpty, "\(event) has an empty title — renders as a blank notification")
            XCTAssertFalse(plan.body.isEmpty, "\(event) has an empty body — renders as a blank notification")
        }
        for i in plans.indices {
            for j in plans.indices where j > i {
                XCTAssertNotEqual(plans[i].plan.title, plans[j].plan.title,
                                  "\(plans[i].event) and \(plans[j].event) share a title — "
                                  + "indistinguishable in a grouped stack")
                XCTAssertNotEqual(plans[i].plan.body, plans[j].plan.body,
                                  "\(plans[i].event) and \(plans[j].event) share a body — "
                                  + "indistinguishable in a grouped stack")
            }
        }
    }

    /// The tokens this surface ADDS to the central vocabulary, and the whole of what it contributes
    /// beyond it — mirroring `src/cli.rs`'s `EXPIRY_HELP_EXTRA_TOKENS`, which is the same derivation
    /// run the same way round (add to the central set, never re-list it).
    ///
    /// `beat` is a circumvention CALL of the same class as the central `bypass`, which `BANNED_TOKENS`
    /// carries. This one it does not, and issue #1134 records why it stays local rather than joining:
    /// `beat` is a HOMOGRAPH this crate spends neutrally — `src/daemon/socket.rs` calls the `watch`
    /// liveness frame a "beat" in its comments and names its interval local that — and the central list
    /// is scanned by an audience with no exemption set at all. On a notification the word has one
    /// reading, outrunning a limit, which is where the ban is earned, so that is where it lives.
    /// `src/cli.rs`'s `EXPIRY_HELP_EXTRA_TOKENS` holds that reasoning and is also where §D-STA-6's
    /// enumeration ("beat/bypass limits") is quoted from — the design record itself is not reachable
    /// from this clone, so this is a second-hand reading and says so.
    ///
    /// It is the one member of the retired hand copy with NO central equivalent at all: five of the
    /// other six are central verbatim, and `need more` has one in the token `need`, which catches the
    /// same sentence a word earlier. `testTheDerivedVocabularyStillCarriesEveryTokenTheHandCopyDid`
    /// drives all three of those rather than leaving them as prose.
    private static let extraTokens = ["beat"]

    /// Imperative / urgency framings this project has already refused on operator-facing surfaces.
    ///
    /// LOCAL on purpose, and this is the one place issue #1219's third shape — "the two guards are
    /// legitimately independent" — is the right answer. `framing_vocabulary` bans four groups and the
    /// imperative MOOD is deliberately not among them ("ZERO central tokens, because `BANNED_TOKENS`
    /// never banned the imperative MOOD"), so there is no central list to derive these from; deriving
    /// them would mean inventing one. They are patterns rather than tokens and run through the shared
    /// scanner's adjacent-word path, which is why `please` has lost the trailing space the retired
    /// substring scan needed to keep it off `pleasant` — a word boundary does that correctly, and also
    /// catches it at the end of a sentence, where `"please "` did not.
    private static let imperativePatterns = ["you must", "act now", "immediately", "right away",
                                             "don't forget", "please"]

    /// `src/framing_vocabulary.rs`'s `BANNED_TOKENS` and `BANNED_PHRASES`, parsed from the source that
    /// declares them. Throws rather than degrading to an empty list — see `ParseFailure`.
    private func centralVocabulary() throws -> (tokens: [String], phrases: [String]) {
        let url = FramingVocabularyScan.vocabularySourceURL()
        let source = try XCTUnwrap(try? String(contentsOf: url, encoding: .utf8),
                                   "could not read \(url.path) — the framing screen cannot derive its "
                                   + "vocabulary, so no framing verdict in this file is about the "
                                   + "contract src/framing_vocabulary.rs defines")
        return (tokens: try FramingVocabularyScan.rustStringList(named: "BANNED_TOKENS", in: source),
                phrases: try FramingVocabularyScan.rustStringList(named: "BANNED_PHRASES", in: source))
    }

    /// The derivation reaches the WHOLE central list, and every member it returns is one the shared
    /// tokenizer can actually match.
    ///
    /// A parser that truncated — at the first group comment, at the first blank line — would return a
    /// plausible-looking list and leave the screen silently narrower than it reads. So this asserts a
    /// representative of each of the four editorial groups `BANNED_TOKENS` documents, in list order:
    /// `add` opens the imperatives, `healthy` the value judgements, `should` the recommendations, and
    /// `soon` is the alarmist group's last entry and the list's, which is the tail canary.
    func testTheFramingVocabularyIsDerivedFromTheRustSourceOfTruth() throws {
        let (tokens, phrases) = try centralVocabulary()

        for (group, representative) in [("imperatives (first entry)", "add"),
                                        ("imperatives", "buy"),
                                        ("value judgements", "healthy"),
                                        ("recommendation framing", "should"),
                                        ("alarmist projection (last entry)", "soon")] {
            XCTAssertTrue(tokens.contains(representative),
                          "the parse of BANNED_TOKENS is missing '\(representative)' from its \(group) "
                          + "group — it did not read the whole list, so this file's framing screen is "
                          + "narrower than it reads")
        }

        // A comment fragment admitted as a token would be multi-word, so it could never match — a dead
        // entry that widens the list on paper and not in fact.
        for token in tokens {
            XCTAssertEqual(FramingVocabularyScan.words(of: token), [token],
                           "'\(token)' does not survive the shared tokenizer as one whole lowercase "
                           + "word, so it can never match: the parse admitted something that is not a "
                           + "token (a comment fragment, or a member the tokenizer would split)")
        }

        for phrase in ["top up", "get more"] {
            XCTAssertTrue(phrases.contains(phrase),
                          "BANNED_PHRASES no longer carries the acquisitive call '\(phrase)' — if that "
                          + "is deliberate centrally, this assertion is the record that it changed")
        }
        for phrase in phrases {
            XCTAssertGreaterThan(FramingVocabularyScan.words(of: phrase).count, 1,
                                 "'\(phrase)' is a single word in BANNED_PHRASES — it belongs in "
                                 + "BANNED_TOKENS, and matched here it is a phrase that cannot span")
        }
    }

    /// The derived list gives up NOTHING the hand copy this change retired was carrying.
    ///
    /// The retired list was `["buy", "purchase", "upgrade", "cancel", "bypass", "beat", "need more"]`.
    /// Five of those are central verbatim and are asserted so here, so a central deletion cannot
    /// silently narrow this surface. The remaining two are the interesting ones
    /// and neither is asserted in prose: `beat` is the local extra above, and `need more` is SUBSUMED —
    /// any text where that phrase matched contains the word `need`, which the central list carries, so
    /// it is still caught, one word earlier, and now also when spelled without `more`. That is the same
    /// reasoning `src/cli.rs`'s `scan_expiry_help` records for dropping its own `need more`, and it is
    /// proven below by driving the sentence rather than by repeating the argument.
    func testTheDerivedVocabularyStillCarriesEveryTokenTheHandCopyDid() throws {
        let (tokens, _) = try centralVocabulary()

        for token in ["buy", "purchase", "upgrade", "cancel", "bypass"] {
            XCTAssertTrue(tokens.contains(token),
                          "BANNED_TOKENS no longer carries '\(token)', which the hand-copied list "
                          + "this file retired was screening for: deriving the vocabulary must not "
                          + "have cost this surface coverage it already had")
        }

        XCTAssertFalse(tokens.contains("beat"),
                       "'beat' is now central. Issue #1134 kept it local because src/daemon/socket.rs "
                       + "spends the word neutrally for the watch liveness frame — if that judgement "
                       + "was revisited centrally, drop it from `extraTokens` rather than screening "
                       + "for it twice")
        XCTAssertTrue(Self.extraTokens.contains("beat"),
                      "'beat' is neither central nor a local extra — the retired hand copy screened "
                      + "for it and this change would have dropped it")

        XCTAssertEqual(FramingVocabularyScan.hits(in: "we need more capacity",
                                                  tokens: tokens + Self.extraTokens, phrases: []),
                       ["need"],
                       "the retired list's 'need more' is only safe to drop because the central 'need' "
                       + "catches the same sentence one word earlier — it does not")
    }

    /// The §D-STA-6 / SUR-001 neutral-framing firewall, applied to the surface that renders on a LOCK
    /// SCREEN (issue #935). Operator-facing strings state facts; they do not instruct, forecast, or
    /// reach for the vocabulary of buying capacity.
    ///
    /// Enumerated over every case for the same reason as the suites above — a firewall that only covers
    /// the strings that existed when it was written is not a firewall. It remains deliberately
    /// mechanical: it cannot judge tone, and the imperative half is a pattern list rather than a
    /// grammar. What it guarantees is that the vocabulary this project has already ruled out cannot
    /// reappear in a notification without reddening — and since issue #1219 that vocabulary is the
    /// central one, whole, rather than a hand-copied subset of it.
    func testNoEventContentReachesForABannedOrImperativeFraming() throws {
        let (tokens, phrases) = try centralVocabulary()
        let banned = tokens + Self.extraTokens

        for event in AccountEvent.allCases {
            let text = event.notificationTitle + " " + event.notificationBody
            XCTAssertEqual(FramingVocabularyScan.hits(in: text, tokens: banned, phrases: phrases), [],
                           "\(event) content spends §D-STA-6 banned framing: \(text)")
            XCTAssertEqual(FramingVocabularyScan.hits(in: text, tokens: [],
                                                      phrases: Self.imperativePatterns), [],
                           "\(event) content is imperative-framed: \(text)")
        }
    }

    /// CANARY for the screen above: it must be able to FAIL, and to fail for the STATED reason.
    ///
    /// It routes the offending string through the SAME `FramingVocabularyScan.hits` and the SAME
    /// derived vocabulary the screen consumes — a local copy of either would make this vacuous in the
    /// one case it exists to cover, staying green over an offending string while the screen's own
    /// derivation sat broken. And it asserts WHICH words came back rather than that something did: a
    /// scan that reported a hit for the wrong reason is a screen whose verdicts cannot be read.
    func testTheNeutralFramingScreenCatchesABannedFraming() throws {
        let (tokens, phrases) = try centralVocabulary()
        let offending = "You must act now — upgrade the plan. Top up to get more, and beat the limit."

        let banned = FramingVocabularyScan.hits(in: offending, tokens: tokens + Self.extraTokens,
                                                phrases: phrases)
        for expected in ["must", "upgrade", "beat", "top up", "get more"] {
            XCTAssertTrue(banned.contains(expected),
                          "the derived banned vocabulary does not catch '\(expected)' in a "
                          + "deliberately offending string; it reported \(banned)")
        }

        let imperative = FramingVocabularyScan.hits(in: offending, tokens: [],
                                                    phrases: Self.imperativePatterns)
        for expected in ["you must", "act now"] {
            XCTAssertTrue(imperative.contains(expected),
                          "the imperative pattern list does not catch '\(expected)' in a deliberately "
                          + "offending string; it reported \(imperative)")
        }
    }

    /// The shared scanner decides what a WORD is the way `crate::framing_vocabulary::words_of` does,
    /// pinned on the cases that module and `src/cli.rs` pin for themselves.
    ///
    /// This is a parity of pinned BEHAVIOUR, not a proof of equivalence — nothing here can execute the
    /// Rust function. What it buys is that a divergence introduced on either side reddens on one of
    /// them rather than on neither.
    ///
    /// The middle block is why the tokenizer had to change in the same commit as the vocabulary, and it
    /// is measured rather than argued: under the retired SUBSTRING scan, widening this file to the
    /// central list would have reddened two of the three shipped events — `rotated` containing `rotate`
    /// and `needed` containing `need` — both of them spuriously. The two assertions that the substring
    /// test does match are the positive control for that claim; without them "the word scan returns
    /// nothing" is equally consistent with a body that spends nothing banned in any sense. That the
    /// scanner itself is not universally empty is pinned by the `at-risk` / `At Risk` / `risk!`
    /// assertions above, which require it to return a hit — not by these two, which never invoke it.
    func testTheSharedScannerMatchesTheRustWordSemantics() throws {
        let (tokens, phrases) = try centralVocabulary()

        // Whole words on non-alphanumeric boundaries: the three spellings framing_vocabulary pins.
        for spelling in ["at-risk", "At Risk", "risk!"] {
            XCTAssertEqual(FramingVocabularyScan.hits(in: spelling, tokens: tokens, phrases: []),
                           ["risk"], "'\(spelling)' must tokenize to the banned word 'risk'")
        }
        XCTAssertEqual(FramingVocabularyScan.hits(in: "saturated", tokens: tokens, phrases: []), [],
                       "'saturated' contains no banned WORD — a substring scan is what trips here")

        // The two shipped bodies whose acceptance depends on this decision, and the retired scan's
        // verdict on the same text.
        for body in ["Sessiometer rotated to a different account.",
                     "No account has capacity right now — action needed."] {
            XCTAssertEqual(FramingVocabularyScan.hits(in: body, tokens: tokens, phrases: []), [],
                           "the shipped body '\(body)' spends no banned WORD")
        }
        XCTAssertTrue("sessiometer rotated to a different account.".contains("rotate"),
                      "the retired substring scan would NOT have tripped on 'rotated' — if that is so, "
                      + "the tokenizer change carries no weight and this rationale is wrong")
        XCTAssertTrue("no account has capacity right now — action needed.".contains("need"),
                      "the retired substring scan would NOT have tripped on 'needed' — same")

        // Adjacent-word phrases, never raw substrings.
        XCTAssertEqual(FramingVocabularyScan.hits(in: "laptop update", tokens: [], phrases: phrases), [],
                       "'laptop update' is not the acquisitive call 'top up'")
        XCTAssertEqual(FramingVocabularyScan.hits(in: "please top up now", tokens: [], phrases: phrases),
                       ["top up"], "an adjacent-word acquisitive call must match")

        // ANSI SGR runs are stripped, so a colour-wrapped banned word tokenizes intact rather than as
        // the single word `31mupgrade` (src/cli.rs pins the same case).
        XCTAssertEqual(FramingVocabularyScan.hits(in: "\u{1B}[31mupgrade\u{1B}[0m", tokens: tokens,
                                                  phrases: []),
                       ["upgrade"], "an SGR-wrapped banned word must still tokenize to that word")
    }

    /// CANARY for the derivation itself: a vocabulary that cannot be parsed must REDDEN, never degrade
    /// into an empty list. An empty list is the degenerate green — a screen with no vocabulary passes
    /// every string, and reports it as a pass.
    ///
    /// The final case is the positive control: the same parser over well-formed synthetic source
    /// returns its members AND drops the one quoted inside a comment, so the three failures above are
    /// about the defect each names rather than about a parser that rejects everything.
    func testTheVocabularyParserFailsLoudRatherThanReturningNothing() {
        XCTAssertThrowsError(
            try FramingVocabularyScan.rustStringList(named: "BANNED_TOKENS",
                                                     in: "pub const OTHER: &[&str] = &[\"buy\"];"),
            "a renamed or restructured constant must fail, not yield an empty screen")
        XCTAssertThrowsError(
            try FramingVocabularyScan.rustStringList(named: "BANNED_TOKENS",
                                                     in: "pub const BANNED_TOKENS: &[&str] = &[];"),
            "an empty central list must fail — it would pass every notification string")
        XCTAssertThrowsError(
            try FramingVocabularyScan.rustStringList(
                named: "BANNED_TOKENS",
                in: "pub const BANNED_TOKENS: &[&str] = &[\n    // \"buy\",\n];"),
            "a list whose every member is commented out is empty, not clean")
        XCTAssertThrowsError(
            try FramingVocabularyScan.rustStringList(
                named: "BANNED_TOKENS",
                in: "pub const BANNED_TOKENS: &[&str] = &[\"buy\","),
            "an unterminated list must fail — the parse cannot tell where the list ends")

        XCTAssertEqual(
            try FramingVocabularyScan.rustStringList(
                named: "BANNED_TOKENS",
                in: "pub const BANNED_TOKENS: &[&str] = &[\n    \"buy\", // \"upgrade\"\n    \"cancel\",\n];"),
            ["buy", "cancel"],
            "the parser must return the declared members and admit no word quoted in a comment")
    }

    /// `.loginExpiring` NAMES THE VERB that replaces the credential, and names no other (issue #935).
    ///
    /// The design record's hard boundary is that the prompt names the verb but never performs it —
    /// `sessiometer login` is interactive by construction. So the string must actually carry the verb
    /// (a notification that says "something expires" and leaves the operator to guess the remedy is
    /// the failure this AC exists to prevent), and it must be THAT verb: `poke` refreshes an access
    /// token and cannot move a refresh-token deadline, so naming it here would send the operator to
    /// a command that reports success and changes nothing.
    func testTheExpiryEventNamesTheReplacementVerbAndNoOther() {
        let body = AccountEvent.loginExpiring.notificationBody
        XCTAssertTrue(body.contains("sessiometer login"),
                      "the expiry notification must name the verb that replaces the credential: \(body)")
        XCTAssertFalse(body.contains("poke"),
                       "`poke` refreshes an ACCESS token and cannot move a refresh-token deadline: \(body)")
        // And it points at the surface that CAN name the account, which the redaction guarantee stops
        // this string from doing itself — the composition invariant, read from the notification side.
        XCTAssertTrue(body.lowercased().contains("panel"),
                      "the expiry notification must point at the panel, its only route to which "
                      + "account is expiring: \(body)")
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

    /// A `.connected` snapshot with one active account under `activeLabel`, optionally carrying an
    /// expiry modifier so the issue #935 channel can be driven down the same chain.
    private func store(activeLabel: String, exhausted: Bool,
                       expiry: AccountExpiry? = nil) -> WatchStatusStore {
        WatchStatusStore.preview(
            state: .connected,
            rows: [AccountRow(label: activeLabel, isActive: true, isEnabled: true, isQuarantined: false,
                              isRecovering: false, auth: nil, sessionPct: nil, weeklyPct: nil,
                              sessionResetsAt: nil, weeklyResetsAt: nil, weeklyExhausted: false,
                              isNextSwapTarget: false, blindActive: nil, expiry: expiry)],
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
