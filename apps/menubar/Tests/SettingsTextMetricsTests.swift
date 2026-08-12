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
//     to the wrong control would pass. The tree walk that would catch that is issue #840. TWO sites are
//     now narrower than that: BOTH apply-status labels' own modifier chains — `.failed` (issue #844) and
//     `.rejected` (issue #944) — are read out of `SettingsView.swift` AS DATA (§ AC-2 cont.), the route
//     `PanelReachabilityLintTests` established for a source file this bundle deliberately does not
//     compile. That proves the clamp and its `.help` recovery are WRITTEN on each chain with the shared
//     constants as their arguments — never that SwiftUI honours them, which stays #840's;
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
//   1. Issue #762 AC-2 called the `.failed` arm of `SettingsView`'s `applyStatus` a hazard where "the
//      inline label truncates". It did NOT truncate — there was no `.lineLimit` and no `.truncationMode`
//      on that `Label`, so it WRAPPED, in a window whose style mask carries no `.resizable`. The code
//      comment on that arm said "truncates" too; both were wrong about the mechanism, and the mechanism is
//      what decides whether the hover tooltip is a sufficient mitigation (for truncation it is; for
//      unbounded wrap it is not, because the wrap consumes the form above it). Gated at the MEASURED
//      behaviour, and the overflow itself was filed as issue #844 rather than fixed there — this
//      umbrella's standing rule is that a measurement item reports overflows and does not fix them (the
//      issue #781 precedent).
//
//      ISSUE #844 HAS SINCE LANDED, so the mechanism is now truncation for real: the label carries
//      `.lineLimit(SettingsFormat.applyStatusLineLimit)` + `.truncationMode(.tail)`, and this file's AC-2
//      section switched to the truncation predicate exactly as the old assertion's own failure message
//      instructed. ISSUE #944 HAS SINCE LANDED TOO, so the `.rejected` arm four lines above it carries the
//      same two modifiers and reads the same constant — and because one constant now serves two chains,
//      the chain lint below is scoped per ARM and driven in BOTH directions, so neither arm's clamp can
//      satisfy the other's gate.
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
//   | 6 | `ProgressView` spin, focus ring, `.help` HOVER rendering        | runtime, not an attribute           | manual — `design/README.md` |
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
    /// APP-EDITABLE keys, but the Rust `SetTunables` it is rejected against (`src/config.rs`) also
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

    /// The issue #414 remedy the daemon returns as a `config-set` ack `detail` when an operator saves
    /// `target_max_session_usage = 0` — authored in `src/config/validate.rs` and threaded through
    /// VERBATIM by `classify_config_set_failure` (`Error::ConfigInvalid(msg) => (Invalid, Some(msg))`),
    /// so what `rejectionText(.invalid, …)` renders is this exact string.
    ///
    /// The two KEY NAMES are read from `TunableField` rather than typed, so the fixture tracks the
    /// shipped wire keys instead of freezing a copy — the discipline `staleKeyDetail` above applies to
    /// serde's field list. The prose between them is a MODEL of a Rust string literal this bundle cannot
    /// call, which is the same claim `TextMetrics`' header makes about the layout primitives: stated,
    /// not glossed.
    ///
    /// `1..=95` is the SHIPPED default ceiling (`DEFAULT_SESSION_CEILING`, `src/config.rs`); the daemon
    /// interpolates whatever the operator submitted. Measured, that value's own digits do not move the
    /// verdict — at a ceiling of 90 the string is the same length and 0.09 pt wider.
    ///
    /// It is NOT the only `ConfigInvalid` remedy that overruns the slot, only the WIDEST and the one
    /// issue #944 names. Two others reach three lines by the same route, and each figure is quoted WITH
    /// the interpolation that produces it, because both messages interpolate the submitted value and are
    /// otherwise not re-derivable: `exhausted_poll_secs` at the default `poll_secs = 300` with `got 0`
    /// measures 887.38 pt, and `fleet_runway_warn_secs` with a nine-digit `got` measures 848.16 pt. The
    /// two are NOT on a common basis — 848.16 is that message's widest reachable form, while
    /// `exhausted_poll_secs` keeps growing to 953.08 pt at a nine-digit `got` — so neither is a bound on
    /// the other. One fixture regardless, because each extra transcription is another copy of a Rust
    /// literal that can drift unobserved.
    private var zeroTargetRemedyDetail: String {
        "\(TunableField.targetMaxSessionUsage.rawValue) = 0 admits no swap target and silently disables "
            + "proactive swapping; it must be in 1..=95. Raise it toward "
            + "\(TunableField.sessionCeiling.rawValue) to admit more targets."
    }

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

    // Issue #762 called it a truncation hazard mitigated by a hover tooltip. Measured, the mechanism was
    // WRAP, not truncation — there was no `.lineLimit` on that `Label` — and that changed the verdict: a
    // truncated line stays one line and the tooltip recovers the rest, whereas an unbounded wrap grew the
    // footer inside a window that carries no `.resizable` in its style mask, pushing the form. Filed as
    // issue #844 rather than fixed here, per this umbrella's standing measure-don't-fix rule.
    //
    // ISSUE #844 IS NOW FIXED, so this gate has switched to the truncation predicate — the switch its own
    // failure message instructed, because after the clamp the property worth gating is a different one.
    // The label carries `.lineLimit(SettingsFormat.applyStatusLineLimit)` + `.truncationMode(.tail)`, so
    // the cost is no longer height; what must hold instead is that the clamp BINDS on the reachable
    // string (else the tooltip is decoration), that the bound it imposes is small, and that the message
    // the tooltip recovers is still the daemon's in full — bound the geometry, never edit the message
    // (`design/README.md` § "The Settings window (#763)").
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

        // THE MECHANISM, part 1 — THE CLAMP BINDS. Wrapped to the footer slot this string needs strictly
        // more lines than `.lineLimit` allows, so the truncation is not hypothetical: it happens on the
        // ordinary reachable trigger (a daemon upgraded past the app). This is the assertion the tooltip's
        // load-bearingness rests on — if the clamp stopped binding, the recovery would be decoration.
        let wrap = TextMetrics.wrapped(long, bodyFont, budget: budget)
        XCTAssertTrue(wrap.bounded,
                      "the wrap probe clipped the string — the line count below is a floor, not a total")
        XCTAssertGreaterThan(wrap.lines, SettingsFormat.applyStatusLineLimit,
                             "the stale-key detail now lays out in \(wrap.lines) lines, within the "
                             + "\(SettingsFormat.applyStatusLineLimit)-line clamp — nothing is truncated, so "
                             + "the `.help` recovery this gate protects is no longer load-bearing. "
                             + "Re-measure before relaxing it (issue #844)")

        // FALSIFIER, same run: the predicate must also be able to say NO. The short arms are the common
        // case and they fit inside the clamp, so "binds" above is a real discrimination rather than a
        // predicate that fires on everything (issue #748 CONSTRAINT-A).
        for (name, failure) in shortApplyFailureCases {
            let short = TextMetrics.wrapped(SettingsFormat.applyFailureText(failure), bodyFont, budget: budget)
            XCTAssertLessThanOrEqual(short.lines, SettingsFormat.applyStatusLineLimit,
                                     "apply-status label, \(name): an ORDINARY failure sentence needs "
                                     + "\(short.lines) lines and would itself be truncated — the clamp is "
                                     + "biting the common case, not just the #628 hazard")
        }

        // THE MECHANISM, part 2 — WHAT THE CLAMP BUYS. Unclamped this label cost `wrap.height` in a window
        // that cannot be resized; clamped it can cost at most this. Asserted as a real reduction so a clamp
        // that stopped bounding anything could not pass as a fix.
        let oneLine = TextMetrics.singleLineHeight(bodyFont)
        let clampedHeight = oneLine * Double(SettingsFormat.applyStatusLineLimit)
        XCTAssertGreaterThan(oneLine, 0,
                             "`singleLineHeight` stopped measuring — the height bound below is inert")
        XCTAssertLessThan(clampedHeight, wrap.height,
                          "the \(SettingsFormat.applyStatusLineLimit)-line clamp "
                          + "(\(String(format: "%.2f", clampedHeight)) pt) does not bound the unclamped wrap "
                          + "(\(String(format: "%.2f", wrap.height)) pt over \(wrap.lines) lines) — the fix "
                          + "buys nothing")
        XCTAssertLessThan(clampedHeight, SettingsFormat.windowMinContentHeight / 4,
                          "the clamped apply-failure label costs \(String(format: "%.2f", clampedHeight)) pt "
                          + "of a \(SettingsFormat.windowMinContentHeight) pt window — a footer status line "
                          + "taking a quarter of the shortest declared window is no longer a bound")

        // THE MECHANISM, part 3 — CLAMP THE DRAWING, NEVER THE TRUTH. The recovery is only a recovery if
        // the string handed to `.help` is the daemon's message in FULL. That is the half a future "fix"
        // would most plausibly get wrong — shortening the message at this layer instead of bounding the
        // drawing at the view — and it is testable here precisely because it is a format-layer property.
        XCTAssertTrue(long.contains(staleKeyDetail),
                      "`applyFailureText` no longer carries the daemon's `detail` verbatim — the message is "
                      + "being edited rather than the geometry bounded, which inverts the #763 rule the "
                      + "clamp implements")
    }

    // MARK: - AC-2 (issue #944): the same hazard on the SIBLING `.rejected` arm, measured

    // The `.rejected` arm carried the identical defect and was deliberately left live by issue #844 — no
    // `.lineLimit`, no `.truncationMode`, in the same non-resizable window. It is measured here on the
    // same three properties, against the same budget, through the same predicates.
    //
    // WHY IT IS THE SHARPER OF THE TWO, and this is the fact the whole item turns on: `applyFailureText`
    // wraps the daemon's text in a fixed app sentence ("Not saved — …"), so an app string is always part
    // of what is drawn. `rejectionText(.invalid, detail)` RETURNS `detail` — on that path the daemon's
    // whole message IS the label, and the app contributes nothing but the icon.
    //
    // And the trigger is ordinary rather than exotic. `src/config/validate.rs` deliberately spells its
    // cross-field remedies out instead of emitting a bare range, and `target_max_session_usage = 0` is a
    // DOCUMENTED operator trap — issue #414 records 0 as "the natural wrong guess for no restriction, its
    // exact opposite". So the message explaining an ordinary mistake is the thing that broke the layout.
    func testTheRejectionLabelIsMeasuredAndTheZeroTargetRemedyOverflowsIt() throws {
        let budget = SettingsFormat.applyStatusBudget()

        // THE STRUCTURAL FACT, first, because every measurement below rests on it: on the `.invalid` path
        // the label is the daemon's message ITSELF. This is also this arm's "clamp the drawing, never the
        // truth" assertion — its strongest possible form, since here the two strings are one.
        XCTAssertEqual(SettingsFormat.rejectionText(.invalid, zeroTargetRemedyDetail), zeroTargetRemedyDetail,
                       "the `invalid` reason no longer renders the daemon's `detail` as the label — if the "
                       + "message is now being edited or wrapped, re-derive this whole section; the hazard "
                       + "it measures is that the daemon's own text has no app sentence bounding it")

        // FALSIFIER, and it is a real discrimination rather than a formality: every FIXED app sentence
        // must lay out INSIDE the clamp, so the clamp never truncates the app's own copy. Probed with no
        // `detail` for all six reasons — which is what four of them ALWAYS draw (the daemon sends those
        // none), what `.configUnreadable` draws regardless (`rejectionText` ignores its detail), and what
        // `.invalid` falls back to. MEASURED, not assumed: four of the six need TWO lines at the shipped
        // window — `unknown-account`, `no-config`, `config-unreadable`, `save-failed` — so this copy sits
        // flush against the clamp rather than comfortably inside it. Note those are NOT the same four as
        // the detail-free reasons: `unavailable` is detail-free but fits one line, `config-unreadable`
        // takes two while carrying a detail. Equal counts, different sets.
        var checked = 0
        var widest = (sentence: "", width: 0.0)
        for reason in ConfigSetRejection.allCases {
            let text = SettingsFormat.rejectionText(reason, nil)
            let wrap = TextMetrics.wrapped(text, bodyFont, budget: budget)
            XCTAssertTrue(wrap.bounded, "\(reason.rawValue): the wrap probe clipped the sentence")
            XCTAssertLessThanOrEqual(wrap.lines, SettingsFormat.applyStatusLineLimit,
                                     "\(reason.rawValue): the app's OWN rejection sentence needs "
                                     + "\(wrap.lines) lines and would itself be truncated by the "
                                     + "\(SettingsFormat.applyStatusLineLimit)-line clamp — the clamp is "
                                     + "biting this app's copy, not just the daemon's remedies")
            let width = TextMetrics.width(text, bodyFont)
            if width > widest.width { widest = (text, width) }
            checked += 1
        }
        XCTAssertEqual(checked, ConfigSetRejection.allCases.count,
                       "expected \(ConfigSetRejection.allCases.count) rejection sentences, ran \(checked)")

        // The MEASURED slack, computed rather than guessed, so a copy rewrite has a number to spend
        // instead of discovering the edge by shipping: how much longer the widest app sentence can get
        // before it needs a third line. Ordinary word-shaped filler, because that is what copy is — a run
        // of unbreakable characters would break early and understate the room.
        let fitting = (0...120).last {
            TextMetrics.wrapped(widest.sentence + String(repeating: " nn", count: $0),
                                bodyFont, budget: budget).lines <= SettingsFormat.applyStatusLineLimit
        }
        let slack = try XCTUnwrap(fitting, "the widest rejection sentence does not fit the clamp even "
                                  + "unextended — the loop below has no baseline")
        XCTAssertGreaterThan(slack, 0,
                             "the widest rejection sentence (\"\(widest.sentence)\", "
                             + "\(String(format: "%.2f", widest.width)) pt) is at the clamp's edge: one "
                             + "more word makes the app's own copy truncate. Shorten it or re-derive the "
                             + "clamp against the reference (`design/README.md` § The Settings window)")

        // …and the daemon's remedy does NOT fit, by a wide margin. Reported in points, not merely flagged.
        let long = SettingsFormat.rejectionText(.invalid, zeroTargetRemedyDetail)
        let required = TextMetrics.width(long, bodyFont)
        XCTAssertTrue(TextMetrics.overflows(long, bodyFont, budget: budget),
                      "the issue #414 zero-target remedy (\(long.count) characters, "
                      + "\(String(format: "%.2f", required)) pt) fits the \(String(format: "%.2f", budget)) "
                      + "pt footer slot — re-measure; the hazard this gate exists for would be gone")
        // Against the CLAMP rather than an invented multiple: the message needs more width than the clamp
        // can EVER draw, so no relaxation of the line limit short of unbounding it recovers the text.
        XCTAssertGreaterThan(required, budget * Double(SettingsFormat.applyStatusLineLimit),
                             "the remedy needs \(String(format: "%.2f", required)) pt, which the "
                             + "\(SettingsFormat.applyStatusLineLimit)-line clamp could draw in full — the "
                             + "`.help` recovery would no longer be carrying anything")

        // THE MECHANISM, part 1 — THE CLAMP BINDS on the reachable trigger, so the truncation is not
        // hypothetical and the `.help` recovery is load-bearing rather than decoration.
        let wrap = TextMetrics.wrapped(long, bodyFont, budget: budget)
        XCTAssertTrue(wrap.bounded,
                      "the wrap probe clipped the string — the line count below is a floor, not a total")
        XCTAssertGreaterThan(wrap.lines, SettingsFormat.applyStatusLineLimit,
                             "the zero-target remedy now lays out in \(wrap.lines) lines, within the "
                             + "\(SettingsFormat.applyStatusLineLimit)-line clamp — nothing is truncated, "
                             + "so the `.help` recovery this gate protects is no longer load-bearing. "
                             + "Re-measure before relaxing it (issue #944)")

        // THE MECHANISM, part 2 — WHAT THE CLAMP BUYS. Unclamped this label cost `wrap.height` in a window
        // that cannot be resized; clamped it can cost at most this.
        let oneLine = TextMetrics.singleLineHeight(bodyFont)
        let clampedHeight = oneLine * Double(SettingsFormat.applyStatusLineLimit)
        XCTAssertGreaterThan(oneLine, 0,
                             "`singleLineHeight` stopped measuring — the height bound below is inert")
        XCTAssertLessThan(clampedHeight, wrap.height,
                          "the \(SettingsFormat.applyStatusLineLimit)-line clamp "
                          + "(\(String(format: "%.2f", clampedHeight)) pt) does not bound the unclamped "
                          + "wrap (\(String(format: "%.2f", wrap.height)) pt over \(wrap.lines) lines) — "
                          + "the fix buys nothing")
        XCTAssertLessThan(clampedHeight, SettingsFormat.windowMinContentHeight / 4,
                          "the clamped rejection label costs \(String(format: "%.2f", clampedHeight)) pt of "
                          + "a \(SettingsFormat.windowMinContentHeight) pt window — a footer status line "
                          + "taking a quarter of the shortest declared window is no longer a bound")

        // THE EMPTY TOOLTIP, resolved (issue #944's third acceptance). `.help(detail ?? "")` published
        // NOTHING on the four reasons the daemon sends no `detail` for, and on the two it does it was no
        // better: on `.invalid` it repeated the label verbatim (there the detail IS the label), and on
        // `.configUnreadable` it published the raw parse error with no sentence naming what had failed.
        // The chain lint pins WHICH expression replaced it; what is
        // testable at this layer is that the replacement is never empty, for every reason, with a detail
        // and without one. Both functions, because the label and the tooltip are now distinct strings.
        for reason in ConfigSetRejection.allCases {
            XCTAssertFalse(SettingsFormat.rejectionText(reason, nil).isEmpty,
                           "\(reason.rawValue): the LABEL is EMPTY with no detail")
            XCTAssertFalse(SettingsFormat.rejectionTooltip(reason, nil).isEmpty,
                           "\(reason.rawValue): the tooltip the clamp relies on is EMPTY with no detail")
            XCTAssertFalse(SettingsFormat.rejectionTooltip(reason, zeroTargetRemedyDetail).isEmpty,
                           "\(reason.rawValue): the tooltip the clamp relies on is EMPTY with a detail")
        }
    }

    /// A baseline TOML parse error of the shape `Error::ConfigParse` carries — what the daemon attaches as
    /// `detail` on a `config-unreadable` rejection. A MODEL of the `toml` crate's message (this bundle
    /// cannot call the daemon), same claim as the fixtures above; only its SHAPE matters here — that it
    /// names a location the fixed sentence cannot.
    private var configParseDetail: String {
        "TOML parse error at line 12, column 24\n  |\n12 | session_ceiling =\n  |"
            + "                        ^\nexpected a value"
    }

    // THE REGRESSION THIS PINS CLOSED, and why the tooltip is not simply the label.
    //
    // The daemon attaches a `detail` on TWO reasons. `ConfigSetAck::Rejected`'s doc (`src/daemon/socket.rs`)
    // states it carries the non-secret message for `invalid` AND for `config-unreadable` — the baseline
    // TOML parse error of issue #628 — and `classify_config_set_failure` (`src/daemon/classify.rs`) maps
    // `Error::ConfigParse` to `(ConfigUnreadable, Some(err.to_string()))` deliberately, "so a stale /
    // version-skewed on-disk config is diagnosable, not a bare envelope"; the daemon's own command test
    // panics with "expected ConfigUnreadable with a parse detail" if it goes missing.
    //
    // `rejectionText` returns `detail` only on `.invalid`. So routing this arm's `.help` at `rejectionText`
    // — which is what the first draft of issue #944 did — would show the operator the same fixed sentence
    // twice and DROP the parse error, on the one path where the daemon went out of its way to send it.
    // Nothing else in the app surfaces it: the apply-path log prints the reason alone, and the load path's
    // `loadFailureDetail(.daemonError(.unreadable))` is a fixed sentence too.
    //
    // That would also invert the rule this whole change implements: on that path the fixed sentence
    // already fits, so bounding the geometry buys nothing and the only effect is editing the message away.
    func testTheConfigUnreadableTooltipCarriesTheParseErrorTheLabelDiscards() {
        let label = SettingsFormat.rejectionText(.configUnreadable, configParseDetail)
        let tooltip = SettingsFormat.rejectionTooltip(.configUnreadable, configParseDetail)

        // The LABEL discards it — asserted, not assumed, because it is the premise of everything below.
        XCTAssertFalse(label.contains("line 12, column 24"),
                       "`rejectionText(.configUnreadable, …)` now carries the parse error itself, so the "
                       + "tooltip below is no longer the only surface for it — re-derive this gate rather "
                       + "than deleting it")

        // …and the TOOLTIP carries it, in full and verbatim.
        XCTAssertTrue(tooltip.contains(configParseDetail),
                      "the `config-unreadable` tooltip does not carry the daemon's parse error. The "
                      + "operator is told their config is unreadable and never told WHERE — the daemon "
                      + "sent the location and this app dropped it (issue #628 / #944). Tooltip: \(tooltip)")
        XCTAssertTrue(tooltip.hasPrefix(label),
                      "the tooltip no longer leads with the arm's own sentence — the detail must be "
                      + "APPENDED to the message, never replace it")
        XCTAssertGreaterThan(tooltip.count, label.count,
                             "the tooltip is no longer than the label it is meant to extend")

        // The `.invalid` path must NOT double the message: there the label already IS the detail, and a
        // tooltip repeating it would be the "merely repeated the label" defect in a new costume.
        let invalidTooltip = SettingsFormat.rejectionTooltip(.invalid, zeroTargetRemedyDetail)
        XCTAssertEqual(invalidTooltip, zeroTargetRemedyDetail,
                       "on `.invalid` the label IS the daemon's message, so the tooltip must be that "
                       + "message once — got: \(invalidTooltip)")

        // And the four detail-free reasons fall back to their own sentence rather than to nothing, which
        // is the `detail ?? ""` defect issue #944 replaced.
        var detailFree = 0
        for reason in ConfigSetRejection.allCases where reason != .invalid && reason != .configUnreadable {
            XCTAssertEqual(SettingsFormat.rejectionTooltip(reason, nil),
                           SettingsFormat.rejectionText(reason, nil),
                           "\(reason.rawValue): a reason the daemon sends no detail for must tool-tip its "
                           + "own sentence")
            detailFree += 1
        }
        XCTAssertEqual(detailFree, 4,
                       "the daemon sends a detail on `invalid` and `config-unreadable`, leaving FOUR "
                       + "detail-free reasons — this ran \(detailFree). If the rejection taxonomy changed, "
                       + "re-read `ConfigSetAck::Rejected` before re-tuning the count")
    }

    // WHAT THE THREE ASSERTIONS ABOVE DO NOT REACH, and the one thing that can be added.
    //
    // They are all format-layer, and deliberately: `SettingsView` is absent from this bundle
    // (`project.yml`), compiling it in is issue #840's call rather than this gate's, so NOTHING here
    // observes the view APPLYING the clamp or the tooltip. That gap is stated rather than papered over.
    //
    // What IS reachable is the MECHANISM the recovery rests on, which was otherwise an unverified claim in
    // a code comment. `design/README.md` § "The Settings window (#763)" names `accessibilityHelp` as the
    // second recovery surface beside the hover tooltip — but there is no SwiftUI modifier of that name, so
    // the rule only holds if `.help` is what sets the AppKit AX attribute, and if a CLAMPED label still
    // publishes the whole message rather than the truncated drawing. Both are measured here through
    // `PanelA11y` — the in-process tree walker issue #758 built and wrote surface-agnostic — instead of
    // being believed.
    //
    // The subject is a STAND-IN mirroring the shipped modifier stack, not the shipped view. A stand-in
    // that drifted from `SettingsView`'s stack would pass this and still fail the operator; catching that
    // needs the live view in the tree, which is exactly issue #840.
    //
    // BOTH ARMS' TOOLTIP SHAPES, because issue #944 made them differ. The `.failed` arm passes one flat
    // sentence; the `.rejected` arm now passes `rejectionTooltip`, which JOINS the label and the daemon's
    // detail with a newline. That is a shape this gate had never exercised, and it is not a formality: an
    // AX layer that stopped at the first line would publish the sentence and silently drop the parse
    // error, leaving VoiceOver users with exactly the surface `rejectionTooltip` exists to restore. The
    // mechanism itself does not vary by string, so this is one parameterised test rather than two.
    @MainActor
    func testAClampedLabelPublishesTheWholeMessageThroughHelpAndNotTheTruncatedDrawing() throws {
        let size = CGSize(width: SettingsFormat.applyStatusBudget(), height: 120)
        let shapes: [(name: String, text: String)] = [
            (".failed, one flat sentence", SettingsFormat.applyFailureText(staleKeyApplyFailure)),
            (".rejected, label + newline + daemon detail",
             SettingsFormat.rejectionTooltip(.configUnreadable, configParseDetail)),
        ]
        XCTAssertTrue(shapes[1].text.contains("\n"),
                      "the multi-line shape has no newline in it — this test is not exercising the join "
                      + "`rejectionTooltip` performs, and the interesting half of it is inert")

        var covered = 0
        for shape in shapes {
            let text = shape.text
            let clamped = PanelA11y.tree(
                for: Label(text, systemImage: "bolt.horizontal.circle")
                    .lineLimit(SettingsFormat.applyStatusLineLimit)
                    .truncationMode(.tail)
                    .help(text),
                size: size)

            // THE ABSENCE TRAP, as this suite's neighbour states it as a rule: a claim about a tree is
            // evidence only if the tree is non-empty and the query actually ran. Both halves, before any
            // verdict — an empty tree would make the canary below pass vacuously. The presence probe uses
            // the message's FIRST LINE, which is what both shapes render.
            XCTAssertFalse(clamped.isEmpty,
                           "\(shape.name): the accessibility tree is empty — activation failed, so every "
                           + "verdict here is vacuous (see `PanelAccessibilityTreeTests`' activation "
                           + "recipe)")
            let firstLine = try XCTUnwrap(text.components(separatedBy: "\n").first,
                                          "\(shape.name): the fixture has no first line")
            XCTAssertNotNil(clamped.firstContaining(firstLine),
                            "\(shape.name): the label's own text is not in the tree at all — the walk "
                            + "found something else, and the help assertion below would be measuring the "
                            + "wrong element. Tree: " + clamped.map(\.description).joined(separator: "\n"))

            // THE CLAIM: the whole message, not the lines that were drawn.
            XCTAssertTrue(clamped.contains { $0.help == text },
                          "\(shape.name): no element publishes the full \(text.count)-character message "
                          + "as `accessibilityHelp`. The #763 rule's second recovery surface does not "
                          + "exist, so the clamp is discarding text with only a hover tooltip behind it. "
                          + "Tree: "
                          + clamped.map { "\($0.description) help='\($0.help)'" }.joined(separator: "\n"))
            covered += 1
        }
        XCTAssertEqual(covered, 2, "expected BOTH tooltip shapes, ran \(covered)")

        // The canary below runs on the `.failed` shape; the mechanism it falsifies is shared.
        let text = shapes[0].text

        // CANARY, through the SAME predicate: drop `.help` and the attribute must go away. Without this a
        // tree that published the message as help for some unrelated reason would read as a passing gate,
        // and the modifier this whole mitigation names would be untested.
        let unhelped = PanelA11y.tree(
            for: Label(text, systemImage: "bolt.horizontal.circle")
                .lineLimit(SettingsFormat.applyStatusLineLimit)
                .truncationMode(.tail),
            size: size)
        XCTAssertFalse(unhelped.isEmpty, "the canary tree is empty — it cannot falsify anything")
        XCTAssertFalse(unhelped.contains { $0.help == text },
                       "a label with NO `.help` still publishes the message as `accessibilityHelp` — the "
                       + "assertion above passes for a reason other than the modifier it is about")
    }

    // MARK: - AC-2 (cont.): the VIEW's own chain, read as data (issues #844 + #944)

    // The last gap the two sections above leave open is the one that matters most: nothing yet observes
    // `SettingsView` APPLYING the clamp. The constants can be perfect and the mechanism proven while the
    // view never calls either.
    //
    // Compiling `SettingsView` into this bundle would close it properly, and that is issue #840's call,
    // not this gate's — the file is deliberately excluded (`project.yml`). So this uses the OTHER route
    // this codebase already established for an excluded source file: read it as DATA and lint its text,
    // exactly as `PanelReachabilityLintTests` reads `StatusItemController.swift`.
    //
    // SCOPE IS EACH ARM'S OWN CHAIN, and since issue #944 that scoping is load-bearing in BOTH directions
    // rather than one. A file-wide `contains(".lineLimit(")` is the trap here, and it is no longer a
    // hypothetical one: two sibling arms four lines apart now carry the BYTE-IDENTICAL clamp line reading
    // the SAME constant, so a file-wide predicate goes green whenever EITHER survives — the exact false
    // pass issue #844 anticipated and issue #944 made reachable. Each chain is therefore taken from its own
    // `Label(` construction and nowhere else, and the canaries below drive that distinction with the REAL
    // sibling clamp rather than a synthetic splice: strip one arm's clamp, leave the other's standing, and
    // require the stripped arm's gate to report none while the sibling's still reports the constant. Run
    // once per arm, so neither can cover for the other.
    //
    // THE HONEST BOUND, stated because a text predicate invites over-reading: a green here means the
    // modifiers are WRITTEN on that chain with the constant as their argument. It cannot mean SwiftUI
    // honours them, that the label bounds the footer on screen, or that this is the chain the operator
    // sees. That residue needs the live view in a tree — issue #840 — and it is the same WIRED-not-
    // DELIVERED bound `PanelReachabilityLintTests` states for its own verdict.

    /// One apply-status arm the ratified clamp binds, and the two strings that identify it in the source.
    ///
    /// A TABLE rather than two hand-written tests, because the property at stake is symmetric: every
    /// assertion and every canary below runs once per entry, so an arm cannot be gated in one direction
    /// and forgotten in the other. Adding a third clamped arm is an entry, not a new test.
    private struct ApplyStatusArm {
        /// The `applyPhase` case it renders, for failure messages.
        let name: String
        /// The `Label(` construction that identifies THIS arm — required to be unique in the file.
        let construction: String
        /// The argument its `.help` recovery must carry: the arm's own message IN FULL, never a shortened
        /// one, and on `.rejected` never the `detail ?? ""` issue #944 replaced.
        let helpArgument: String
    }

    private var failedArm: ApplyStatusArm {
        ApplyStatusArm(name: ".failed",
                       construction: "Label(SettingsFormat.applyFailureText(",
                       helpArgument: "SettingsFormat.applyFailureText(failure)")
    }

    private var rejectedArm: ApplyStatusArm {
        ApplyStatusArm(name: ".rejected",
                       construction: "Label(SettingsFormat.rejectionText(",
                       helpArgument: "SettingsFormat.rejectionTooltip(reason, detail)")
    }

    /// Both arms the #763 rule binds and both issues have now fixed. The suite asserts this cardinality
    /// wherever it loops, so a silently emptied or halved table cannot read as a clean run.
    private var applyStatusArms: [ApplyStatusArm] { [failedArm, rejectedArm] }

    /// One apply-status `Label`'s modifier chain, extracted from `SettingsView.swift`'s text.
    ///
    /// `chain` is the run of consecutive `.`-leading lines under the construction, comments stripped, so
    /// a `.lineLimit` mentioned in the arm's PROSE cannot satisfy the gate that its code should.
    private struct ApplyStatusArmChain {
        let chain: [String]
        var lineLimitArgument: String? { argument(of: ".lineLimit(") }
        var truncationModeArgument: String? { argument(of: ".truncationMode(") }
        var helpArgument: String? { argument(of: ".help(") }

        private func argument(of modifier: String) -> String? {
            guard let line = chain.first(where: { $0.hasPrefix(modifier) }) else { return nil }
            return String(line.dropFirst(modifier.count).dropLast())   // trailing `)`
        }
    }

    /// The one construction that is this arm's title, plus the chain hanging off it.
    /// `nil` when the site is missing or ambiguous — never "no violation", which is the degenerate
    /// subject this suite refuses to score as a pass.
    private func armChain(_ arm: ApplyStatusArm, in source: String) -> ApplyStatusArmChain? {
        let lines = source.components(separatedBy: "\n")
        let sites = lines.indices.filter { lines[$0].contains(arm.construction) }
        guard sites.count == 1, let site = sites.first else { return nil }

        var chain: [String] = []
        for line in lines.dropFirst(site + 1) {
            let code = line.components(separatedBy: "//")[0].trimmingCharacters(in: .whitespaces)
            // A blank or comment-only line INSIDE the chain is not the chain ending — Swift permits it
            // and annotating a modifier is exactly what a reader of this arm would do. Breaking here
            // instead reported all three modifiers ABSENT while all three were present: fail-closed, so
            // never a false pass, but it misdiagnoses the cause for precisely the person who caused it.
            // Skipping is safe because the real terminator is the NEXT `case` (or the `}` closing the
            // `switch`), which is neither empty nor `.`-prefixed.
            if code.isEmpty { continue }
            guard code.hasPrefix("."), code.hasSuffix(")") else { break }
            chain.append(code)
        }
        return chain.isEmpty ? nil : ApplyStatusArmChain(chain: chain)
    }

    /// `SettingsView.swift` with the `.lineLimit` line removed from ONE arm's chain, its sibling's left
    /// standing — the mutual-exclusion mutation, and the reason it is written this way rather than as a
    /// `replacingOccurrences`.
    ///
    /// Issue #844's canary spliced a clamp INTO the `.rejected` arm because that arm had none to borrow.
    /// It cannot survive issue #944: both arms now carry the byte-identical line, so a global replace
    /// strips or rewrites BOTH and the isolation the canary claims is gone. Walking to the named arm and
    /// dropping the clamp inside its own chain restores that isolation — and improves on it, since the
    /// surviving clamp is now the sibling's REAL one rather than a synthetic insertion.
    private func removingClamp(from arm: ApplyStatusArm, in source: String) -> String {
        var lines = source.components(separatedBy: "\n")
        guard let site = lines.firstIndex(where: { $0.contains(arm.construction) }) else { return source }

        var target: Int?
        for index in (site + 1)..<lines.count {
            let code = lines[index].components(separatedBy: "//")[0].trimmingCharacters(in: .whitespaces)
            if code.isEmpty { continue }
            guard code.hasPrefix("."), code.hasSuffix(")") else { break }
            if code.hasPrefix(".lineLimit(") { target = index; break }
        }
        guard let target else { return source }
        lines.remove(at: target)
        return lines.joined(separator: "\n")
    }

    private var settingsViewURL: URL {
        URL(fileURLWithPath: #filePath)      // …/apps/menubar/Tests/SettingsTextMetricsTests.swift
            .deletingLastPathComponent()     // …/apps/menubar/Tests
            .deletingLastPathComponent()     // …/apps/menubar
            .appendingPathComponent("Sources")
            .appendingPathComponent("SettingsView.swift")
    }

    private func settingsViewSource() throws -> String {
        try XCTUnwrap(try? String(contentsOf: settingsViewURL, encoding: .utf8),
                      "SettingsView.swift must be readable as data — this gate reads it, it does not "
                      + "compile it (the file is excluded from this bundle by `project.yml`)")
    }

    func testBothApplyStatusArmsCarryTheClampTheTruncationModeAndTheHelpRecovery() throws {
        let source = try settingsViewSource()
        // DEGENERATE SUBJECT, first: an unreadable file or a moved construction would make every verdict
        // below vacuous, and "no chain found" must never read as "no violation".
        XCTAssertGreaterThan(source.utf8.count, 2000,
                             "read a real file, not a stub — SettingsView is a substantial source")

        var checked = 0
        for arm in applyStatusArms {
            let extracted = try XCTUnwrap(
                armChain(arm, in: source),
                "\(arm.name): no single `\(arm.construction)` construction with a modifier chain — the "
                + "gate has no subject for this arm, so it cannot have a verdict")

            // The clamp, through the CONSTANT — a bare `2` here would be the second copy the whole
            // `SettingsFormat` seam exists to prevent, and canary 2 below proves this tells them apart.
            XCTAssertEqual(extracted.lineLimitArgument, "SettingsFormat.applyStatusLineLimit",
                           "\(arm.name): the chain is \(extracted.chain) — it must clamp through the "
                           + "shared constant, which is the value `SettingsTextMetricsTests` measures "
                           + "against")
            // Explicit, not defaulted: `.tail` IS the default truncation mode, so writing it changes no
            // pixel — it states which end is sacrificed at the site where a future reader decides.
            XCTAssertEqual(extracted.truncationModeArgument, ".tail",
                           "\(arm.name): the label must name its truncation mode; chain: \(extracted.chain)")
            // And the recovery the clamp makes load-bearing: the FULL text, never a shortened one.
            XCTAssertEqual(extracted.helpArgument, arm.helpArgument,
                           "\(arm.name): the `.help` recovery must carry this arm's whole message — a "
                           + "clamp whose tooltip is itself edited (or empty) loses the text outright; "
                           + "chain: \(extracted.chain)")
            checked += 1
        }
        XCTAssertEqual(checked, 2, "expected BOTH clamped apply-status arms, ran \(checked)")
    }

    // CONSTRAINT-A (issue #748): the lint is driven by MUTATION of the REAL file, through the SAME
    // extractor the assertion above uses. Four mutations, four distinct false-pass shapes — and the third
    // runs once per arm, because "the two arms must not cover for each other" is a symmetric claim and a
    // canary that only ever checks one direction leaves the other ungated.
    func testTheChainLintFailsOnAStrippedClampALiteralClampAndASiblingArmsClamp() throws {
        let real = try settingsViewSource()

        // 1. The regression it exists for: the clamp deleted outright. Global, so since issue #944 this is
        //    the WHOLE-FILE shape — what a careless refactor produces — and mutation 3 is the one that
        //    discriminates between the arms.
        let stripped = real.replacingOccurrences(
            of: ".lineLimit(SettingsFormat.applyStatusLineLimit)\n", with: "")
        XCTAssertNotEqual(stripped, real, "the mutation changed nothing — the canary is inert")
        for arm in applyStatusArms {
            XCTAssertNil(try XCTUnwrap(armChain(arm, in: stripped)).lineLimitArgument,
                         "\(arm.name): a `SettingsView.swift` with no `.lineLimit` on this chain still "
                         + "reports one — this gate cannot fail, so its green is not evidence")
        }

        // 2. The drift it exists for: the constant replaced by the literal it happens to equal today.
        let literal = real.replacingOccurrences(
            of: ".lineLimit(SettingsFormat.applyStatusLineLimit)", with: ".lineLimit(2)")
        for arm in applyStatusArms {
            XCTAssertEqual(try XCTUnwrap(armChain(arm, in: literal)).lineLimitArgument, "2",
                           "\(arm.name): the extractor cannot tell the shared constant from a hardcoded "
                           + "literal, so the assertion above would pass over a second copy of the value")
        }

        // 3. MUTUAL EXCLUSION, in BOTH directions. Issue #844 spliced a clamp into the then-unclamped
        //    `.rejected` arm to prove `.failed`'s gate would not accept it; with #944 landed the sibling
        //    clamp is REAL, so the mutation inverts — take one arm's clamp away and leave the other's — and
        //    it now runs for each arm in turn. Three assertions per direction, because the claim needs all
        //    three: the mutation landed, the file still contains a clamp (so a file-wide predicate WOULD go
        //    green), and this arm's gate says none anyway.
        var directions = 0
        for arm in applyStatusArms {
            let sibling = try XCTUnwrap(applyStatusArms.first { $0.name != arm.name },
                                        "\(arm.name) has no sibling — the mutual-exclusion claim is "
                                        + "meaningless with fewer than two arms")
            let siblingOnly = removingClamp(from: arm, in: real)
            XCTAssertNotEqual(siblingOnly, real,
                              "\(arm.name): the mutation removed nothing — this direction is inert")
            XCTAssertTrue(siblingOnly.contains(".lineLimit(SettingsFormat.applyStatusLineLimit)"),
                          "\(arm.name): the mutated file contains NO clamp at all, so a file-wide "
                          + "predicate would go red on its own and this direction proves nothing about "
                          + "scoping")
            XCTAssertEqual(try XCTUnwrap(armChain(sibling, in: siblingOnly)).lineLimitArgument,
                           "SettingsFormat.applyStatusLineLimit",
                           "\(arm.name): the mutation took \(sibling.name)'s clamp too — the surviving "
                           + "clamp is what this direction needs the gate to REFUSE to accept")
            XCTAssertNil(try XCTUnwrap(armChain(arm, in: siblingOnly)).lineLimitArgument,
                         "\(sibling.name)'s clamp satisfies \(arm.name)'s gate — the two arms cover for "
                         + "each other, which is exactly the false pass this scoping exists to prevent")
            directions += 1
        }
        XCTAssertEqual(directions, 2,
                       "mutual exclusion must be driven in BOTH directions, ran \(directions)")

        // 4. The `.help` regression issue #944 fixed, since the gate above now pins a DIFFERENT expression
        //    per arm and could otherwise be passing on shape alone: restore `.help(detail ?? "")` on the
        //    `.rejected` arm — empty on the four reasons the daemon sends no `detail` for, a duplicate of
        //    the label on `.invalid`, and the bare parse error on `.configUnreadable` — and require the
        //    extractor to report it as what it is.
        let emptyHelp = real.replacingOccurrences(
            of: ".help(SettingsFormat.rejectionTooltip(reason, detail))", with: ".help(detail ?? \"\")")
        XCTAssertNotEqual(emptyHelp, real, "the `.help` mutation changed nothing — canary 4 is inert")
        XCTAssertEqual(try XCTUnwrap(armChain(rejectedArm, in: emptyHelp)).helpArgument, "detail ?? \"\"",
                       "the extractor cannot tell the full-message recovery from the `detail ?? \"\"` it "
                       + "replaced, so an empty tooltip on the four detail-free reasons would pass the "
                       + "gate above with the clamp still discarding text")
    }

    // The extractor's own robustness, driven by the SAME real-file mutation the canaries use. A gate
    // that goes red for the wrong reason costs a reader the same round-trip a false pass does, and the
    // reader who trips this one is the reader who just annotated the arm the gate is about.
    func testAnAnnotatedChainIsStillReadWhole() throws {
        let annotated = try settingsViewSource().replacingOccurrences(
            of: "                .lineLimit(SettingsFormat.applyStatusLineLimit)",
            with: "                // two lines, per the #763 ratified rule\n\n"
                + "                .lineLimit(SettingsFormat.applyStatusLineLimit)")
        XCTAssertTrue(annotated.contains("// two lines, per the #763 ratified rule"),
                      "the annotation did not land — this test is not exercising what it claims")

        var checked = 0
        for arm in applyStatusArms {
            let extracted = try XCTUnwrap(
                armChain(arm, in: annotated),
                "\(arm.name): a comment plus a blank line between two modifiers made the whole chain "
                + "unfindable — the extractor reads legal Swift as the chain ending")
            XCTAssertEqual(extracted.lineLimitArgument, "SettingsFormat.applyStatusLineLimit",
                           "\(arm.name): an annotated chain lost its clamp; read: \(extracted.chain)")
            XCTAssertEqual(extracted.truncationModeArgument, ".tail",
                           "\(arm.name): an annotated chain lost its truncation mode; read: \(extracted.chain)")
            XCTAssertEqual(extracted.helpArgument, arm.helpArgument,
                           "\(arm.name): an annotated chain lost its `.help` recovery; read: \(extracted.chain)")
            checked += 1
        }
        XCTAssertEqual(checked, 2, "expected BOTH arms to survive annotation, ran \(checked)")
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

        // The slot's OTHER dimension, pinned to the same standard. Everything else about the clamp is
        // gated RELATIVE to this constant — the chain lint proves the view references it, and the AC-2
        // assertions read it — so the one thing none of them can see is the constant's own VALUE: at 3,
        // 4, 5 or 6 the whole suite still passes while the shipped label no longer matches the reference
        // it is documented as conforming to. This is the pin that makes that edit loud.
        XCTAssertEqual(SettingsFormat.applyStatusLineLimit, 2,
                       "the #763 reference ratifies TWO lines (`-webkit-line-clamp:2` on "
                       + "`menubar-preview.html`'s `.win-status .txt`, under a comment naming `.failed` "
                       + "and `.rejected` together) and `design/README.md` now states that BOTH shipped "
                       + "arms conform — changing this needs the reference amended first, not the pin "
                       + "re-tuned, and it would move both arms at once")
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
