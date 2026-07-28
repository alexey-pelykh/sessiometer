// Copyright (c) 2026 Oleksii PELYKH
// SPDX-License-Identifier: MIT

// Coverage + text-metrics gate for the SETTINGS window (issue #762; umbrella issue #748 R6).
//
// WHY THIS EXISTS. `SettingsView.swift` was 360 lines of SwiftUI with no automated verification of any
// kind. Its model layer was well covered — `SettingsModelTests` is 577 lines of load / apply / draft-diff
// / validation — but the surface that PRESENTS those decisions was not: not one of the ~70 strings the
// operator reads (the 15 tunable label + help pairs alone are 30 of them), and neither of the two
// hardcoded field widths, was reachable from a test. So the decisions were tested and the presentation
// was not, which is precisely the split that lets a config window ship with an unreadable error or an
// unreadably-narrow field.
//
// HOW IT IS REACHED, and what was deliberately NOT done. `MenubarTests` enumerates its SOURCE files one
// by one but takes `Tests` WHOLESALE (`project.yml`), so a new test file is free and a new source file is
// not. Rather than compile `SettingsView` into the bundle, every string and width it renders moved DOWN
// into `SettingsFormat` (`SettingsModel.swift`) — the Foundation-only layer this bundle already compiles.
// That is issue #750's move verbatim, and **no `project.yml` change was needed, and none was made**.
//
// This is a coordination point, not an accident: issue #840's AC-1 explicitly owns the decision to
// compile `SettingsView` into this bundle ("coordinate with issue #762 so the two do not add it twice"),
// because #840 needs the live view for an accessibility-TREE walk, which no amount of constant-hoisting
// can substitute for. #762 therefore does not widen the bundle, and #840 remains free to.
//
// WHAT A GREEN RUN PROVES, exactly: every sentence the shipped Settings form can render — all of them,
// since `SettingsView` now holds no operator-visible string literal of its own — is present, non-empty
// and distinct from its siblings; the Save predicate is correct across every apply phase; and each
// hardcoded field width, plus the footer's status slot, is measured against the widest content that can
// actually reach it.
//
// What it does NOT prove, stated precisely because the first draft of this very paragraph overclaimed —
// it said "every sentence" while 14 were still hardcoded in the view, and an adversarial review caught
// it, the same shape as the issue #749 correction this file cites below:
//
//   * that SwiftUI's own `Form` layout pass produces these widths. Like `PanelTextMetricsTests`, this
//     models the layout with the SAME CoreText primitives AppKit and SwiftUI shape through
//     (`TextMetrics`), over the same text styles the views declare — a model of the layout, not an
//     observation of the live view tree;
//   * that each string is rendered at the site it belongs to. A constant asserted here and then wired
//     to the wrong control would pass. The tree walk that would catch that is issue #840;
//   * that the copy is GOOD. It is inferred product copy on a surface with no ratified design
//     reference (issue #763) — this gate checks it exists and is distinct, never that it is right.
//
// CONSTRAINT-A (issue #748) — NO GATE WITHOUT A PROVEN FALSIFIER. `testTheCanaryOverflowsWhileEveryShipped
// SettingsStringFits` pushes a deliberately over-wide fixture through the SAME `TextMetrics.overflows`
// every real assertion uses and requires it to FAIL, in the same run that requires every shipped string
// to pass. `testTheCopyGateCatchesAnEmptyOrADuplicatedSentence` does the mutation equivalent for the copy
// half. A gate that cannot fail is not evidence — issue #437's three render bugs were misread five times
// as "the DESIGN fails distinctness", and a golden authored then would have DEFENDED them.
//
// THREE MEASURED FACTS THAT CONTRADICT THIS ITEM'S OWN PREMISES, recorded rather than quietly rounded:
//
//   1. Issue #762 AC-2 calls the `.failed` arm of `SettingsView`'s `applyStatus` (`SettingsView.swift:294`
//      as the issue cites it) a hazard where "the inline label truncates". It does NOT truncate — there is
//      no `.lineLimit` and no `.truncationMode` on that `Label`, so it WRAPS, in a window whose style mask
//      carries no `.resizable`. The code comment on that arm said "truncates" too; both were wrong about
//      the mechanism, and the mechanism is what decides whether the hover tooltip is a sufficient
//      mitigation (for truncation it is; for unbounded wrap it is not, because the wrap consumes the form
//      above it). Gated at the MEASURED behaviour, and the overflow itself is filed as issue #844 rather
//      than fixed here — this umbrella's standing rule is that a measurement item reports overflows and
//      does not fix them (the issue #781 precedent).
//
//   2. The hazard is much larger than "can be long". The issue #628 `detail` this arm interpolates is
//      serde's own `deny_unknown_fields` message, which names EVERY expected tunable — so the string that
//      reaches the footer is ~535 characters and ~2 700 pt wide in a 328 pt slot, not a long sentence. The
//      fixture below reconstructs it from the SHIPPED field list (`TunableField.allCases`) rather than
//      pasting it, so it tracks the wire instead of freezing a copy — which makes the fixture itself a
//      deliberate ~425-character LOWER bound; see `staleKeyDetail` for why that direction is the safe one.
//
//   3. Settings does NOT participate in the panel's `\.panelScale` Dynamic Type scaling (issue #756) —
//      `panelScale` appears in no Settings source. Its FONTS still scale (they are system text styles)
//      while its two field widths are fixed literals, which is exactly the "scaled font in a fixed cell"
//      defect issue #756's own AC-2 names. That is asserted here as a PINNED defect (green when the
//      cells stay unscaled, red the moment they learn to scale) and filed as issue #845.
//
// AC-4 — WHAT IS NOT REACHED HERE, AND WHERE IT WENT. Silence is not acceptable; this is the cardinality.
// Eight surfaces of the Settings window are outside this gate, each with a named owner:
//
//   | # | Surface                                                        | Why not here                        | Route |
//   |---|----------------------------------------------------------------|-------------------------------------|-------|
//   | 1 | Accessibility role/label of every Toggle, TextField, Button    | needs the live AX tree              | issue #840 (its AC-2/AC-4) |
//   | 2 | The decorative icon in `loadFailureSection`, verified ABSENT   | ditto — annotation ≠ absence        | issue #840 (its AC-3) |
//   | 3 | The `LabeledContent` title/value column split                   | a `Form` decision with no frame     | issue #763 (no ratified reference) |
//   | 4 | Rendered pixels of the form                                     | no design reference exists yet      | issue #763 |
//   | 5 | Dynamic Type behaviour of the two fixed cells                   | pinned below as a defect, not fixed | issue #845 |
//   | 6 | `ProgressView` spin, focus ring, `.help` hover tooltip          | runtime, not an attribute           | manual — `design/README.md` |
//   | 7 | ⌘S actually reaching `apply()`                                  | needs a key event into a live window| manual — `design/README.md` (issue #761 closed GO, suite unbuilt) |
//   | 8 | `SettingsWindowController` activation-policy + single-instance   | `NSApp`/`NSWindow` global state     | manual — `design/README.md` |
//
// Rows 6–8 are written into `design/README.md`'s new "Settings window pre-release checklist" by this
// same change — every one of them as a step an operator can actually run, verified item-by-item rather
// than routed to a destination that does not contain them (an adversarial review of the first draft
// caught exactly that: `hover` was routed to a checklist with no hover step). Row 5's pin is in this file.
//
// Row 7's route is deliberate: issue #761's spike closed **GO**, but the suite it authorised was never
// built and its identifier work is an unmet prerequisite. Routing to a suite that does not exist would
// be the same empty gesture as routing to a checklist without the step, so ⌘S goes to the manual list
// and moves to the suite if and when one lands.

#if DEBUG
import AppKit
// `DynamicTypeSize` — the issue #845 pin reads the same enum `PanelTypeScale` is keyed on, so the factor
// it mutates with is the panel's real ceiling rather than an invented multiplier.
import SwiftUI
import XCTest

final class SettingsTextMetricsTests: XCTestCase {

    // MARK: - Fonts (each pinned to the view site whose text style it mirrors)

    // Sizes are READ from the platform's text styles rather than hardcoded, so these track what `.body` /
    // `.caption` actually mean instead of asserting against a number that could quietly stop being that.

    /// The `Form` row label, the `TextField` content, and the footer's status `Label` title — all default
    /// `.body` in a `.grouped` form.
    private let bodyFont = NSFont.preferredFont(forTextStyle: .body)

    /// `.font(.caption)` — the inline field error and the account row's Active/Parked cue.
    private let captionFont = NSFont.preferredFont(forTextStyle: .caption1)

    // MARK: - Fixtures

    /// The issue #628 stale-key `detail` the daemon threads into a `config-set` error envelope when a
    /// version-skewed app sends a renamed tunable: serde's `deny_unknown_fields` message, which names
    /// every expected field.
    ///
    /// RECONSTRUCTED from `TunableField.allCases` rather than pasted, so it tracks the shipped keys
    /// instead of freezing a copy. It is a MODEL of serde's output (the daemon is Rust; this bundle
    /// cannot call it), which is exactly the claim `TextMetrics`' header makes about the layout
    /// primitives — stated, not glossed.
    ///
    /// It is deliberately a LOWER BOUND, and the direction matters. `TunableField` enumerates the 15
    /// APP-EDITABLE keys, but the Rust `SetTunables` it is rejected against (`src/config.rs`:1091) also
    /// carries four `canary_*` overrides (`canary_drift_override`, `canary_nostashmatch_override`,
    /// `canary_online_probe`, `canary_online_probe_strict`) — so serde's real message names 19 fields and
    /// runs ~110 characters LONGER than this fixture's ~425. Understating the hazard cannot flip an
    /// overflow assertion, whereas hardcoding the four Rust-only names here would plant a second copy that
    /// drifts the moment the daemon gains a key. So the fixture stays derived, and the shortfall is stated
    /// rather than closed.
    private var staleKeyDetail: String {
        let expected = TunableField.allCases.map { "`\($0.rawValue)`" }.joined(separator: ", ")
        return "unknown field `session_trigger`, expected one of \(expected)"
    }

    /// Every DISTINCT branch of the load-failure copy, with the name of the branch it exercises. Shared by
    /// the copy tests and the metrics tests so the two can never cover different sets.
    private var loadFailureCases: [(name: String, failure: ConfigFailure)] {
        [("no config", .daemonError(ConfigGetErrorReason.noConfig)),
         ("unreadable", .daemonError(ConfigGetErrorReason.unreadable)),
         ("other daemon error", .daemonError("encode failed")),
         ("transport", .transport(.connectionRefused(reason: "No such file or directory"))),
         ("unavailable", .unavailable),
         ("undecodable", .undecodable)]
    }

    /// Every DISTINCT branch of `applyFailureText`, longest-reachable content included.
    ///
    /// Split into the arms that must FIT and the one that provably does not, so the overflow case is
    /// excluded by CONSTRUCTION rather than by a name match inside the loop that consumes it.
    private var shortApplyFailureCases: [(name: String, failure: ConfigFailure)] {
        [("transport", .transport(.timedOut)),
         ("unavailable", .unavailable),
         ("undecodable", .undecodable),
         ("daemon error (short)", .daemonError("config unreadable"))]
    }

    /// The one arm that overflows the footer by construction (issue #628 / #844).
    private var staleKeyApplyFailure: ConfigFailure { .daemonError(staleKeyDetail) }

    // MARK: - AC-1: every sentence the extracted seam can render is present, non-empty and distinct

    // The seam is only worth extracting if it is actually covered. `TunableField` is `CaseIterable`, so
    // this cannot miss a field: adding one without adding its copy fails the count, and adding one with
    // COPIED copy fails the distinctness set.
    func testEveryTunableHasDistinctNonEmptyCopy() {
        var titles: Set<String> = []
        var helps: Set<String> = []

        for field in TunableField.allCases {
            let copy = SettingsFormat.copy(for: field)
            XCTAssertFalse(copy.title.trimmingCharacters(in: .whitespaces).isEmpty,
                           "\(field.rawValue) renders an empty label — the row would show a bare text box")
            XCTAssertFalse(copy.help.trimmingCharacters(in: .whitespaces).isEmpty,
                           "\(field.rawValue) has no hover help")
            // The help is the only place a unit/semantic is explained; a label that merely repeats it is
            // not help. (Not a style rule — `.help` is what maps to `accessibilityHelp`.)
            XCTAssertNotEqual(copy.title, copy.help, "\(field.rawValue): the help just repeats the label")
            titles.insert(copy.title)
            helps.insert(copy.help)
        }

        XCTAssertEqual(titles.count, TunableField.allCases.count,
                       "two tunables share a label — the operator cannot tell which row edits which key")
        XCTAssertEqual(helps.count, TunableField.allCases.count, "two tunables share hover help")
        // Degenerate-subject guard: a green is evidence only if it evaluated the whole planned set.
        XCTAssertEqual(TunableField.allCases.count, 15, "the 15-tunable surface changed size — re-derive")
    }

    // Section headers are the form's only structural signposts, and `Section` is `CaseIterable` too.
    func testEveryTunableSectionHasADistinctTitleAndOwnsAtLeastOneField() {
        var titles: Set<String> = []
        var covered: Set<TunableField> = []

        for section in TunableField.Section.allCases {
            XCTAssertFalse(section.title.isEmpty, "\(section.rawValue) renders an untitled section")
            XCTAssertFalse(section.fields.isEmpty,
                           "\(section.rawValue) renders an EMPTY section — a header with nothing under it")
            titles.insert(section.title)
            covered.formUnion(section.fields)
        }

        XCTAssertEqual(titles.count, TunableField.Section.allCases.count, "two sections share a header")
        // Every field reaches the form exactly once: the section grouping partitions `allCases`, so a
        // field that fell out of the grouping would be silently unreachable in the UI.
        XCTAssertEqual(covered, Set(TunableField.allCases),
                       "the section grouping does not cover every tunable — a field is unreachable in the form")
    }

    // The honest-disconnected / no-config states (issue #268 AC 7) are the ones an operator hits on a
    // fresh install and with the daemon stopped. Each must say something DIFFERENT — collapsing them is
    // how "it's broken" replaces "capture an account first".
    func testEveryLoadFailureStateHasADistinctHeadlineAndDetail() {
        var headlines: Set<String> = []
        var details: Set<String> = []

        for (name, failure) in loadFailureCases {
            let headline = SettingsFormat.loadFailureHeadline(failure)
            let detail = SettingsFormat.loadFailureDetail(failure)
            XCTAssertFalse(headline.isEmpty, "\(name): no headline")
            XCTAssertFalse(detail.isEmpty, "\(name): no detail — the state would be a bare icon")
            XCTAssertNotEqual(headline, detail, "\(name): the detail just repeats the headline")
            headlines.insert(headline)
            details.insert(detail)
        }

        // `transport` and `unavailable` deliberately SHARE one sentence (both mean "no daemon"), so the
        // expected distinct count is one fewer than the case count — asserted as the exact number so a
        // future accidental collapse is not absorbed as "well, some of them share".
        XCTAssertEqual(headlines.count, loadFailureCases.count - 1,
                       "load-failure headlines collapsed: \(headlines.sorted())")
        XCTAssertEqual(details.count, loadFailureCases.count - 1,
                       "load-failure details collapsed: \(details.sorted())")

        // The two actionable states name the CLI, because that is the only place the fix lives.
        XCTAssertTrue(
            SettingsFormat.loadFailureDetail(.daemonError(ConfigGetErrorReason.noConfig))
                .contains("sessiometer CLI"),
            "the no-config state no longer points at the CLI — a first-run operator is left with no next step")
    }

    // Every daemon `rejected` verdict reaches the footer as its own sentence. `CaseIterable` (added in
    // issue #762) makes this exhaustive by construction rather than by a list someone has to remember.
    func testEveryRejectionReasonRendersADistinctSentence() {
        var sentences: Set<String> = []
        for reason in ConfigSetRejection.allCases {
            let text = SettingsFormat.rejectionText(reason, nil)
            XCTAssertFalse(text.isEmpty, "\(reason.rawValue) renders nothing in the footer")
            sentences.insert(text)
        }
        XCTAssertEqual(sentences.count, ConfigSetRejection.allCases.count,
                       "two rejection reasons render the same sentence: \(sentences.sorted())")

        // `.invalid` is the ONE reason that prefers the daemon's own `detail` (it names the offending
        // field); every other reason ignores `detail` because the daemon sends none for them.
        XCTAssertEqual(SettingsFormat.rejectionText(.invalid, "session_ceiling must exceed target"),
                       "session_ceiling must exceed target",
                       "the `invalid` reason stopped surfacing the daemon's field-naming detail")
        XCTAssertEqual(SettingsFormat.rejectionText(.unknownAccount, "ignored"),
                       SettingsFormat.rejectionText(.unknownAccount, nil),
                       "a non-`invalid` reason started leaking `detail` — the daemon sends none there")
    }

    // The success path has three outcomes and they are NOT interchangeable: `unchanged` must not claim a
    // write that did not happen, and `restartRequired` must not read as already-live.
    func testEveryAppliedEffectRendersItsOwnOutcome() {
        let byEffect: [ConfigSetEffect: String] = [
            .live: SettingsFormat.savedText,
            .unchanged: SettingsFormat.unchangedText,
            .restartRequired: SettingsFormat.restartRequiredText,
        ]
        XCTAssertEqual(byEffect.count, ConfigSetEffect.allCases.count,
                       "a `ConfigSetEffect` case has no footer sentence — it would render as nothing")
        XCTAssertEqual(Set(byEffect.values).count, ConfigSetEffect.allCases.count,
                       "two apply effects render the same confirmation")

        XCTAssertNotEqual(SettingsFormat.unchangedText, SettingsFormat.savedText,
                          "`unchanged` now claims 'Saved' — that implies a write the daemon did not perform")
        XCTAssertTrue(SettingsFormat.restartRequiredText.lowercased().contains("restart"),
                      "the restart banner no longer says to restart — the edit silently looks applied")

        // The remaining two footer strings, which belong to no effect.
        for (name, text) in [("saving", SettingsFormat.savingText),
                             ("invalid input", SettingsFormat.invalidInputText)] {
            XCTAssertFalse(text.isEmpty, "the \(name) footer state renders nothing")
        }
    }

    // The static section copy — headers, footers, control titles. These carry no logic, which is exactly
    // why they nearly stayed as literals in the view (`SettingsFormat` § static section copy records what
    // that cost the first draft). They are covered here so the header's "every sentence" claim is true
    // rather than narrowed.
    func testEveryStaticSectionStringIsPresentAndDistinct() {
        let strings: [(name: String, value: String)] = [
            ("General header", SettingsFormat.generalSectionTitle),
            ("launch-at-login toggle", SettingsFormat.launchAtLoginToggleTitle),
            ("launch-at-login approval hint", SettingsFormat.launchAtLoginApprovalHint),
            ("launch-at-login approval button", SettingsFormat.launchAtLoginApprovalButtonTitle),
            ("General footer", SettingsFormat.generalSectionFooter),
            ("Notifications header", SettingsFormat.notificationsSectionTitle),
            ("notifications toggle", SettingsFormat.notificationsToggleTitle),
            ("Notifications footer", SettingsFormat.notificationsSectionFooter),
            ("loading placeholder", SettingsFormat.loadingText),
            ("retry button", SettingsFormat.retryButtonTitle),
            ("Accounts header", SettingsFormat.accountsSectionTitle),
            ("account label placeholder", SettingsFormat.accountLabelFieldPlaceholder),
            ("Accounts footer", SettingsFormat.accountsSectionFooter),
            ("Save button", SettingsFormat.saveTitle),
            ("daemon-config header", SettingsFormat.daemonConfigSectionTitle),
        ]

        for (name, value) in strings {
            XCTAssertFalse(value.trimmingCharacters(in: .whitespaces).isEmpty,
                           "\(name) renders as empty — the control would be unlabelled")
        }
        XCTAssertEqual(Set(strings.map(\.value)).count, strings.count,
                       "two static Settings strings are identical — one of them is mislabelled")
        XCTAssertEqual(strings.count, 15, "the static-copy surface changed size — re-derive the list")

        // Two claims the copy itself makes, which a rewrite could silently break:
        // the Notifications footer promises the daemon section is BELOW it (the issue #573 layering)…
        XCTAssertTrue(SettingsFormat.notificationsSectionFooter.contains("below"),
                      "the Notifications footer no longer says the daemon config is below it — issue "
                      + "#573's layering is what makes that sentence true")
        // …and the Accounts footer is where the issue #268 AC-5 credential boundary is stated to the
        // operator. If this sentence goes, the window silently stops promising what it structurally is.
        XCTAssertTrue(SettingsFormat.accountsSectionFooter.contains("never touches credentials"),
                      "the Accounts footer dropped the credential-boundary promise (issue #268 AC 5/6)")
        XCTAssertTrue(SettingsFormat.accountsSectionFooter.contains("sessiometer CLI"),
                      "the Accounts footer no longer routes add/remove to the CLI")
    }

    func testAccountRowCopyDistinguishesParkedFromActive() {
        XCTAssertNotEqual(SettingsFormat.accountRowTitle(enabled: true),
                          SettingsFormat.accountRowTitle(enabled: false),
                          "a parked account is labelled identically to an active one")
        XCTAssertNotEqual(SettingsFormat.accountStateCue(enabled: true),
                          SettingsFormat.accountStateCue(enabled: false),
                          "the state cue reads the same parked and active — it carries no information")
    }

    // MARK: - AC-1: the Save enable/disable predicate, across every phase

    // The truth table, not a spot check. Save must be dead when there is nothing to submit, and dead while
    // a submit is in flight — the second half pairs with `SettingsModel.apply()`'s own re-entrancy guard,
    // and it is the half a test can actually reach.
    func testSaveIsEnabledOnlyWithAPendingEditAndNoApplyInFlight() {
        // The expectation is DATA, one column per row, not derived from the phase — deriving it here would
        // restate the implementation and the test would agree with any bug. (An earlier draft compared the
        // display NAME against "applying", which is the same table with a typo-sensitive oracle.)
        let phases: [(name: String, phase: SettingsModel.ApplyPhase, enabledWhenDirty: Bool)] = [
            ("idle", .idle, true),
            ("applying", .applying, false),
            ("applied(live)", .applied(effect: .live), true),
            ("applied(unchanged)", .applied(effect: .unchanged), true),
            ("applied(restartRequired)", .applied(effect: .restartRequired), true),
            ("rejected", .rejected(reason: .invalid, detail: nil), true),
            ("invalidInput", .invalidInput, true),
            ("failed", .failed(.undecodable), true),
        ]

        var checked = 0
        for (name, phase, enabledWhenDirty) in phases {
            XCTAssertFalse(SettingsFormat.saveEnabled(isDirty: false, applyPhase: phase),
                           "\(name): Save is live with NO pending edit — it would submit an empty config-set")
            XCTAssertEqual(SettingsFormat.saveEnabled(isDirty: true, applyPhase: phase), enabledWhenDirty,
                           "\(name): Save enabled-when-dirty is wrong (expected \(enabledWhenDirty))")
            checked += 1
        }
        XCTAssertEqual(checked, 8, "expected 8 apply phases in the truth table, ran \(checked)")

        // The one that matters most, stated on its own: a rapid double ⌘S must not spawn two writes.
        XCTAssertFalse(SettingsFormat.saveEnabled(isDirty: true, applyPhase: .applying),
                       "Save stays live while an apply is in flight — a double ⌘S could submit twice")
    }

    // MARK: - AC-3: the hardcoded field widths, measured against their content budgets

    // `.frame(width: 96)`. A `TextField` SCROLLS rather than truncates, so exceeding the budget is a
    // legibility limit — the operator must scroll to read or verify a value they are about to save — not
    // clipping. The gate is placed at the MEASURED digit boundary rather than at an assumed one.
    func testTheTunableFieldFitsEveryValueAnOperatorRealisticallySees() throws {
        let budget = SettingsFormat.fieldTextBudget(SettingsFormat.tunableFieldWidth)

        // Every daemon DEFAULT (`src/config.rs`), the values a freshly-loaded form actually shows. The
        // widest is `exhausted_poll_secs` = 3600.
        var checked = 0
        for value: UInt64 in [0, 2, 3, 50, 60, 80, 95, 98, 120, 300, 3600] {
            assertFits(String(value), bodyFont, budget: budget, "tunable field at a shipped default")
            checked += 1
        }
        XCTAssertEqual(checked, 11, "expected 11 default values, ran \(checked)")

        // A plausible operator entry: a one-week fleet-runway warning in seconds.
        assertFits("604800", bodyFont, budget: budget, "tunable field at a one-week runway warning")

        // The MEASURED boundary, reported rather than assumed. Find the first digit count that overflows.
        let boundary = (1...20).first {
            TextMetrics.overflows(String(repeating: "8", count: $0), bodyFont, budget: budget)
        }
        let digits = try XCTUnwrap(
            boundary,
            "not even 20 digits overflow the \(budget) pt tunable field — the cell is unguardable, so "
            + "this gate proves nothing (issue #748 CONSTRAINT-A)")
        XCTAssertGreaterThan(digits, 7,
                             "the tunable field now hides a 7-digit value (it overflows at \(digits) "
                             + "digits) — a runway warning in seconds no longer fits at a glance")

        // The TYPE ceiling IS reachable — `TunableField.value(in:)` widens every field to `UInt64` and the
        // wire applies no clamp — so a nonsense daemon value scrolls out of sight. Asserted, not assumed.
        XCTAssertTrue(TextMetrics.overflows(String(UInt64.max), bodyFont, budget: budget),
                      "a 20-digit UInt64 fits the \(budget) pt field — re-measure before trusting the "
                      + "boundary above")
    }

    // The other half of "assert the hardcoded width against its content budget": the 96 pt cell does not
    // sit alone in its row. `LabeledContent` puts the field's own TITLE in the leading column, so what
    // actually has to fit is title + field TOGETHER — and the title is the side that grows when copy is
    // rewritten. Measured at the narrowest DECLARED window, so a future `.resizable` cannot make it live.
    // (Same shape as the account row's value-column check below; without it a widened label could push the
    // 96 pt cell off the row and every assertion above would still be green.)
    func testEveryTunableRowFitsItsLabelAndItsFieldTogether() {
        let rowBudget = SettingsFormat.formRowTextBudget(contentWidth: SettingsFormat.windowMinContentWidth)

        var checked = 0
        var widest = (title: "", width: 0.0)
        for field in TunableField.allCases {
            let title = SettingsFormat.copy(for: field).title
            let required = TextMetrics.width(title, bodyFont) + SettingsFormat.tunableFieldWidth
            XCTAssertLessThan(required, rowBudget,
                              "\(field.rawValue): \"\(title)\" plus its \(SettingsFormat.tunableFieldWidth) "
                              + "pt field needs \(String(format: "%.2f", required)) pt of a "
                              + "\(String(format: "%.2f", rowBudget)) pt row — the label column and the "
                              + "value cell no longer coexist, so one of them is being squeezed")
            if required > widest.width { widest = (title, required) }
            checked += 1
        }
        XCTAssertEqual(checked, TunableField.allCases.count,
                       "expected \(TunableField.allCases.count) tunable rows, ran \(checked)")

        // The headroom the widest row actually has, reported so a copy rewrite has a number to spend
        // against rather than discovering the edge by shipping.
        XCTAssertGreaterThan(rowBudget - widest.width, 100,
                             "the widest tunable row (\"\(widest.title)\", "
                             + "\(String(format: "%.2f", widest.width)) pt) now clears the "
                             + "\(String(format: "%.2f", rowBudget)) pt row by under 100 pt — the label "
                             + "column is running out of room for longer copy")
    }

    // `.frame(width: 160)`. The label is an operator-chosen nickname (`src/config.rs` enforces only
    // non-empty), and issue #445's own context shows email-shaped labels are what real rosters carry.
    func testTheAccountLabelFieldIsMeasuredAgainstRealisticLabels() throws {
        let budget = SettingsFormat.fieldTextBudget(SettingsFormat.accountLabelFieldWidth)

        // The nicknames the config docs recommend fit with large headroom.
        for label in ["work", "spare", "personal", "client-acme"] {
            assertFits(label, bodyFont, budget: budget, "account label field, recommended nickname")
        }

        // MEASURED, and it is the finding of this test rather than a footnote: the realistic fleet shape
        // does NOT fit. `src/config.rs` deliberately does not reject an email-shaped label (issue #404
        // left PII-freedom to the operator) and issue #445's whole identity-disambiguation kit exists
        // because real rosters carry exactly this shape — so this content is reachable, common, and 12.94
        // pt too wide for the field it lands in. Filed as issue #846; NOT fixed here (widening the field
        // is a `Form` column decision, and issue #763 owns the missing design reference for that).
        let realisticAddress = "oleksii@company-one.com"
        let addressWidth = TextMetrics.width(realisticAddress, bodyFont)
        XCTAssertTrue(TextMetrics.overflows(realisticAddress, bodyFont, budget: budget),
                      "\"\(realisticAddress)\" (\(String(format: "%.2f", addressWidth)) pt) now fits the "
                      + "\(String(format: "%.2f", budget)) pt account-label field. If the field was widened, "
                      + "that is issue #846 fixed — invert this assertion and close it.")

        // The MEASURED character boundary, reported rather than assumed, so #846 has a number to aim at.
        let firstOverflowing = (1...60).first {
            TextMetrics.overflows(String(repeating: "n", count: $0), bodyFont, budget: budget)
        }
        let boundary = try XCTUnwrap(
            firstOverflowing,
            "not even 60 characters overflow the \(budget) pt label field — the cell is unguardable, so "
            + "this gate proves nothing (issue #748 CONSTRAINT-A)")
        XCTAssertGreaterThan(boundary, 8,
                             "the account-label field now hides labels of \(boundary) characters or more — "
                             + "even the recommended nicknames (`work`, `spare`) are at risk")

        // A 36-character UUID is the widest handle the roster can produce, and `AccountView.accountUuid`
        // is exactly that shape — so an operator who pastes the uuid as a label gets a field they cannot
        // read end to end. Asserted as the (larger) overflow it is.
        let uuidShaped = "11111111-2222-3333-4444-555555555555"
        XCTAssertEqual(uuidShaped.count, 36, "the fixture must stay the shape `AccountView.accountUuid` is")
        XCTAssertGreaterThan(TextMetrics.width(uuidShaped, bodyFont), addressWidth,
                             "a uuid-shaped label must measure wider than an address-shaped one")

        // The trailing state cue shares the row with the field; both must fit the account row together at
        // the narrowest declared window. This is what actually bounds the field, not the field alone. The
        // gap is charged through the SAME constant the row lays out with — a literal 8 here would be the
        // second copy this seam exists to prevent.
        let rowContent = SettingsFormat.accountLabelFieldWidth
            + SettingsFormat.accountRowInterElementSpacing
            + TextMetrics.width(SettingsFormat.accountStateCue(enabled: false), captionFont)
        XCTAssertLessThan(rowContent, SettingsFormat.windowMinContentWidth,
                          "the account row's value column (\(String(format: "%.2f", rowContent)) pt) no "
                          + "longer fits the \(SettingsFormat.windowMinContentWidth) pt minimum window "
                          + "width — before its label column is even charged")
    }

    // The inline format error sits directly under the field it flags, spanning the row rather than the
    // 96 pt cell — so it is measured against the row, and it is the row that must hold it. At the
    // narrowest DECLARED window, because a message that only fits the shipped 460 is a latent defect of
    // exactly the kind the footer already carries.
    func testTheInlineFormatErrorsFitTheFormRow() {
        let budget = SettingsFormat.formRowTextBudget(contentWidth: SettingsFormat.windowMinContentWidth)
        // The two client-side messages `SettingsModel.apply()` can set.
        for message in ["Enter a whole number (0 or greater).", "That number is too large."] {
            assertFits(message, captionFont, budget: budget, "inline field error")
        }
    }

    // MARK: - AC-2: the long-text hazard in `applyStatus`'s `.failed` arm, measured

    // The issue calls it a truncation hazard mitigated by a hover tooltip. Measured, the mechanism is
    // WRAP, not truncation — there is no `.lineLimit` on that `Label` — and that changes the verdict:
    // a truncated line stays one line and the tooltip recovers the rest, whereas an unbounded wrap grows
    // the footer inside a window that carries no `.resizable` in its style mask, pushing the form.
    func testTheApplyFailureLabelIsMeasuredAndTheStaleKeyDetailOverflowsIt() {
        let budget = SettingsFormat.applyStatusBudget()

        // The SHORT arms fit and must keep fitting — they are the common case.
        var checked = 0
        for (name, failure) in shortApplyFailureCases {
            assertFits(SettingsFormat.applyFailureText(failure), bodyFont, budget: budget,
                       "apply-status label, \(name)")
            checked += 1
        }
        XCTAssertEqual(checked, 4, "expected 4 short apply-failure arms, ran \(checked)")

        // …and the issue #628 arm does not, by a wide margin. Reported in points, not merely flagged.
        let long = SettingsFormat.applyFailureText(staleKeyApplyFailure)
        let required = TextMetrics.width(long, bodyFont)
        XCTAssertTrue(TextMetrics.overflows(long, bodyFont, budget: budget),
                      "the issue #628 stale-key detail (\(long.count) characters, "
                      + "\(String(format: "%.2f", required)) pt) fits the \(String(format: "%.2f", budget)) "
                      + "pt footer slot — re-measure; the hazard this gate exists for would be gone")
        XCTAssertGreaterThan(required, budget * 3,
                             "the stale-key detail needs \(String(format: "%.2f", required)) pt of "
                             + "\(String(format: "%.2f", budget)) pt — under 3× the slot it may be worth "
                             + "re-reading issue #844's severity")

        // THE MECHANISM. With no `.lineLimit`, the label wraps — so the cost is HEIGHT, in a fixed window.
        let wrap = TextMetrics.wrapped(long, bodyFont, budget: budget)
        XCTAssertTrue(wrap.bounded,
                      "the wrap probe clipped the string — the line count below is a floor, not a total")
        XCTAssertGreaterThan(wrap.lines, 1,
                             "the apply-failure label no longer wraps. If a `.lineLimit` was added, this "
                             + "gate must switch to the truncation predicate (issue #844) — do not delete it")

        // How much of the fixed window that wrap eats. The footer is one line plus its padding when
        // healthy; every extra line comes out of the form above it.
        let oneLine = TextMetrics.singleLineHeight(bodyFont)
        let overrun = wrap.height - oneLine
        XCTAssertGreaterThan(overrun, 0,
                             "the wrapped label costs no more height than one line — either the wrap probe "
                             + "or `singleLineHeight` stopped measuring, and the overrun bound below is inert")
        XCTAssertLessThan(overrun, SettingsFormat.windowMinContentHeight,
                          "the wrapped apply-failure label alone (\(String(format: "%.2f", wrap.height)) pt "
                          + "over \(wrap.lines) lines) exceeds the whole \(SettingsFormat.windowMinContentHeight) "
                          + "pt minimum window height — the form would be entirely displaced")
    }

    // The allowances the footer budget rests on are not free-floating numbers: the Save button must
    // actually clear its own title, or the budget above is derived from a fiction.
    func testTheFooterAllowancesClearTheContentTheyReserveFor() {
        // Measured through the SAME constant the button renders, not through a literal "Save". An earlier
        // draft measured the literal, which meant renaming the button would leave this test green while
        // the budget it validates silently became wrong — the exact drift the seam exists to prevent.
        let saveTitleWidth = TextMetrics.width(SettingsFormat.saveTitle, bodyFont)
        XCTAssertGreaterThan(SettingsFormat.saveButtonAllowance, saveTitleWidth,
                             "the Save button allowance (\(SettingsFormat.saveButtonAllowance) pt) is "
                             + "narrower than its own title (\(String(format: "%.2f", saveTitleWidth)) pt) "
                             + "— every budget derived from it is too generous")
        XCTAssertLessThan(SettingsFormat.saveButtonAllowance, saveTitleWidth * 3,
                          "the Save button allowance is more than 3× its title — the footer budget is "
                          + "being throttled by an allowance nobody re-measured")

        // And the derived budget is a real, positive slot at the width it claims.
        XCTAssertEqual(SettingsFormat.applyStatusBudget(), 328, accuracy: 0.001,
                       "the apply-status budget derivation changed — re-derive it, do not re-tune the tests")
        XCTAssertGreaterThan(SettingsFormat.applyStatusBudget(),
                             SettingsFormat.applyStatusBudget(contentWidth: SettingsFormat.windowMinContentWidth),
                             "the shipped 460 pt window must give the footer MORE room than the 440 pt floor")
    }

    // MEASURED, and it contradicts the view's own declaration: `SettingsView` declares `minWidth: 440`,
    // but at 440 the footer cannot hold its own widest ORDINARY sentence. It is latent rather than live
    // only because `SettingsWindowController` builds the window with `[.titled, .closable]` and NO
    // `.resizable`, so 440 is unreachable — the form always gets the 460 it was sized to.
    //
    // Asserted rather than absorbed, because "unreachable" is a property of one line in a different file:
    // adding `.resizable` (a one-word change, with no obvious connection to this footer) makes the
    // shortfall live immediately. This test is what would say so.
    func testTheDeclaredMinimumWindowWidthCannotHoldTheWidestFooterSentence() {
        let floorBudget = SettingsFormat.applyStatusBudget(contentWidth: SettingsFormat.windowMinContentWidth)
        let widestOrdinary = SettingsFormat.applyFailureText(.undecodable)
        let required = TextMetrics.width(widestOrdinary, bodyFont)

        // It fits the SHIPPED window…
        assertFits(widestOrdinary, bodyFont, budget: SettingsFormat.applyStatusBudget(),
                   "widest ordinary failure sentence at the shipped 460 pt window")

        // …and does NOT fit the width the view declares as its minimum.
        XCTAssertTrue(TextMetrics.overflows(widestOrdinary, bodyFont, budget: floorBudget),
                      "\"\(widestOrdinary)\" (\(String(format: "%.2f", required)) pt) now fits the "
                      + "\(String(format: "%.2f", floorBudget)) pt slot the declared "
                      + "\(SettingsFormat.windowMinContentWidth) pt minimum leaves it. If the footer or the "
                      + "minimum width changed so the floor is now sufficient, that is a real improvement — "
                      + "delete this test and say so in the commit.")

        // The headroom the SHIPPED width actually has, pinned so it cannot silently erode to nothing.
        let headroom = SettingsFormat.applyStatusBudget() - required
        XCTAssertGreaterThan(headroom, 0,
                             "the shipped window no longer holds its own widest ordinary failure sentence")
        XCTAssertLessThan(headroom, 40,
                          "the footer gained more than 40 pt of headroom (\(String(format: "%.2f", headroom)) "
                          + "pt) — good news, but re-derive the budget rather than leaving a stale bound")
    }

    // MARK: - PINNED DEFECT (issue #845): Settings does not scale, so its cells cannot

    // The panel multiplies every font AND every layout constant by `\.panelScale` (issue #756), so a cell
    // and its text grow together. Settings does not: `panelScale` appears in no Settings source, while its
    // fonts ARE system text styles and therefore DO grow with the OS Dynamic Type setting. That is the
    // "scaled font in a fixed cell" defect issue #756's own AC-2 names.
    //
    // Pinned the way `PanelAccessibilityTreeTests` pins issues #838/#839: green while the defect stands,
    // RED the moment it is fixed — so the fix cannot land silently and this gate cannot rot into a lie.
    func testTheSettingsCellsDoNotScaleWithDynamicTypeAndThatIsIssue845() throws {
        let k = PanelTypeScale.factor(for: .accessibility3)
        XCTAssertGreaterThan(k, 1.0, "the ceiling factor is not an enlargement — the check below is inert")

        let cells: [(name: String, width: Double)] = [
            ("tunable field", SettingsFormat.tunableFieldWidth),
            ("account label field", SettingsFormat.accountLabelFieldWidth),
        ]

        var pinned = 0
        for cell in cells {
            let budget = SettingsFormat.fieldTextBudget(cell.width)
            let scaledFont = NSFont.systemFont(ofSize: bodyFont.pointSize * k)

            // The fixture is COMPUTED, not guessed: the widest filler that still fits this cell at the
            // DEFAULT size. That keeps the pin about the scaling RELATIONSHIP — content sitting at the top
            // of its budget must overflow once the font grows and the cell does not — instead of resting on
            // a hand-picked string whose fit could drift when the platform's `.body` metric moves. (Filler,
            // not realistic content, precisely because realistic content is measured in the AC-3 tests
            // above; this one is about scaling alone.)
            let fitting = (1...200).last {
                !TextMetrics.overflows(String(repeating: "n", count: $0), bodyFont, budget: budget)
            }
            let widest = String(repeating: "n", count: try XCTUnwrap(
                fitting, "\(cell.name): not even one character fits \(budget) pt — the cell is degenerate"))

            // Control, same run: at the DEFAULT size it fits, so the failure below is about scaling and not
            // about the cell being too small to begin with.
            assertFits(widest, bodyFont, budget: budget, "\(cell.name) at default size")

            // The pin: the same content in a scaled font still meets an UNSCALED cell.
            XCTAssertTrue(TextMetrics.overflows(widest, scaledFont, budget: budget),
                          "\(cell.name): a .accessibility3-scaled font (k=\(String(format: "%.4f", k))) now "
                          + "FITS the unscaled \(String(format: "%.2f", budget)) pt cell. If Settings "
                          + "learned to scale its frames, that is issue #845 fixed — replace this pin with "
                          + "a scaling sweep modelled on PanelTextMetricsTests' AC-3, and close #845.")
            pinned += 1
        }
        XCTAssertEqual(pinned, 2, "expected 2 fixed Settings cells in the pin, ran \(pinned)")

        // The other half of the finding, asserted directly rather than left to prose: the panel's scale
        // environment key exists and the panel reads it. Settings reading it is what would fix #845.
        XCTAssertNotEqual(PanelTypeScale.factor(for: .large), k,
                          "the panel's own type scale is flat across size classes — issue #756 regressed, "
                          + "and the comparison this pin draws against Settings is meaningless")
    }

    // MARK: - CONSTRAINT-A: the metrics gate PROVES it can fail, in the same run it passes

    func testTheCanaryOverflowsWhileEveryShippedSettingsStringFits() {
        // ---- the canary must FAIL the gate, through the SAME predicate ----
        let canary = String(repeating: "W", count: 60)
        let slots: [(String, NSFont, Double)] = [
            ("tunable field", bodyFont, SettingsFormat.fieldTextBudget(SettingsFormat.tunableFieldWidth)),
            ("account label field", bodyFont,
             SettingsFormat.fieldTextBudget(SettingsFormat.accountLabelFieldWidth)),
            ("apply-status label", bodyFont, SettingsFormat.applyStatusBudget()),
        ]
        for (name, font, budget) in slots {
            XCTAssertTrue(TextMetrics.overflows(canary, font, budget: budget),
                          "\(name): the canary (\(String(format: "%.2f", TextMetrics.width(canary, font))) "
                          + "pt) did not trip a \(String(format: "%.2f", budget)) pt budget — that slot's "
                          + "gate cannot fail, so a green run is not evidence (issue #748 CONSTRAINT-A)")
        }

        // ---- and every shipped string must PASS it, in this same run ----
        assertFits("3600", bodyFont,
                   budget: SettingsFormat.fieldTextBudget(SettingsFormat.tunableFieldWidth), "canary control")
        assertFits("work", bodyFont,
                   budget: SettingsFormat.fieldTextBudget(SettingsFormat.accountLabelFieldWidth),
                   "canary control")
        // The WIDEST ordinary failure sentence, at the shipped window — the tightest control available,
        // and deliberately so: it clears its slot by only ~17 pt, so a canary that could not distinguish
        // it from the 60-W filler would be a very blunt instrument.
        assertFits(SettingsFormat.applyFailureText(.undecodable), bodyFont,
                   budget: SettingsFormat.applyStatusBudget(), "canary control")

        // A gate whose budget were zero or negative would "fail" on everything and look rigorous.
        for (name, _, budget) in slots {
            XCTAssertGreaterThan(budget, 0, "\(name) budget is non-positive — every string would overflow")
        }
    }

    // The COPY half needs its own falsifier: the tests above assert distinctness and non-emptiness, and a
    // gate that only ever sees good copy could be one that cannot see bad copy. This feeds deliberately
    // broken copy through the SAME two predicates those tests use — an empty sentence and a duplicated
    // one — and requires each to be caught.
    func testTheCopyGateCatchesAnEmptyOrADuplicatedSentence() {
        // Predicate 1, as `testEveryTunableHasDistinctNonEmptyCopy` applies it: empty ⇒ caught.
        XCTAssertTrue("".trimmingCharacters(in: .whitespaces).isEmpty,
                      "the emptiness predicate no longer sees an empty sentence")
        XCTAssertTrue("   ".trimmingCharacters(in: .whitespaces).isEmpty,
                      "a whitespace-only sentence reads as present — copy could go blank undetected")

        // Predicate 2: the distinct-count check. A mutated set with one duplicated sentence must produce a
        // count BELOW the case count — the exact comparison those tests make.
        let shipped = TunableField.allCases.map { SettingsFormat.copy(for: $0).title }
        XCTAssertEqual(Set(shipped).count, shipped.count, "control: the shipped titles are distinct")

        var mutated = shipped
        mutated[1] = mutated[0]  // the mutation: two rows now share a label
        XCTAssertLessThan(Set(mutated).count, mutated.count,
                          "the distinctness predicate did NOT catch a duplicated label — every 'no two "
                          + "share a sentence' assertion in this file is decoration (CONSTRAINT-A)")

        // And the same predicate over the real failure copy, so the mutation is not only about titles.
        let details = loadFailureCases.map { SettingsFormat.loadFailureDetail($0.failure) }
        var mutatedDetails = details
        mutatedDetails[0] = mutatedDetails[2]
        XCTAssertLessThan(Set(mutatedDetails).count, Set(details).count,
                          "collapsing the no-config detail into another state's went undetected")
    }

    // MARK: - Headless (issue #762, mirroring issue #750 AC-6)

    // This suite touches no window, no screen and no status item — `NSAttributedString` shaping and
    // CoreText line-breaking are pure text layout. The proof it runs headless is that it runs at all in
    // this bundle (`TEST_HOST: ""`, no host app) under the `xcodebuild test` invocation CI runs verbatim,
    // so this asserts only that a real shaping result came back rather than a zero from an unavailable
    // font stack — without which every `assertFits` above would pass vacuously.
    func testMeasurementIsAvailableWithoutAWindowServer() {
        XCTAssertGreaterThan(TextMetrics.width("Save", bodyFont), 0,
                             "text shaping returned zero width — the font stack is unavailable here")
        XCTAssertGreaterThan(bodyFont.pointSize, 0, "the body text style resolved to no size")
        XCTAssertGreaterThan(captionFont.pointSize, 0, "the caption text style resolved to no size")
        XCTAssertGreaterThan(TextMetrics.wrapped("a b c d e", bodyFont, budget: 20).lines, 1,
                             "the wrap probe produced no line breaks at a 20 pt budget — line-breaking is "
                             + "unavailable, so the issue #844 measurement above proves nothing")
    }
}
#endif
