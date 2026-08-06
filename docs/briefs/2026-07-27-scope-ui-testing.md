---
type: scope-brief
date: 2026-07-27
workflow: /scope
status: final
umbrella: 748
---

# Scope Brief: UI Testing — depth and breadth

## Problem

The menubar GUI has no automated verification of *rendered* output beyond the 16 px status glyph.
Format-layer semantics are richly covered (`StatusPanelFormatTests.swift`, 1,401 LOC); visual, layout
and accessibility properties are covered nowhere, and three view surfaces have zero coverage of any
kind. The mechanism currently substituting for tests — the developer opening the panel daily — expires
as the app moves to public distribution (issue #269 shipped, issue #172 in flight).

Root cause: the test architecture deliberately excludes every SwiftUI/AppKit file from `MenubarTests`,
treating views as "thin, un-screenshot-tested consumers" (`StatusPanelRoster.swift:564`). Sound while
views were thin — but views have since accreted layout policy (truncation modes, frame budgets,
severity-driven tinting) the format layer cannot express.

## What's In Scope

**Wave 1 — Foundation**
1. issue #749 — [SPIKE] is `ImageRenderer` gate-able headless in CI? *(routing fork for the whole render lane)*
2. issue #750 — text-metrics layout gate *(no oracle, no windowserver, no TCC — cheapest real coverage)*
3. issue #751 — ADR: UI test-tier architecture

**Wave 2 — Content edge cases**
4. issue #752 — [hq/design] pathological-content frames in the mock *(the missing oracle)*
5. issue #753 — stress fixtures in `RenderPanelTool` — blocked by #752, #749
6. issue #754 — panel golden gate + re-baseline discipline — blocked by #749
7. issue #755 — panel geometry across roster sizes — blocked by #750

**Wave 3 — Accessibility**
8. issue #756 — **Dynamic Type support** *(a shipped defect, not a test gap)*
9. issue #757 — Dynamic Type layout gate — blocked by #756, #750
10. issue #758 — VoiceOver / accessibility audit
11. issue #759 — contrast breadth: WCAG AA across the #388 tint tokens
12. issue #760 — appearance variants: increased contrast / reduce transparency / reduce motion — blocked by #749

**Wave 4 — Coverage expansion**
13. issue #761 — [SPIKE] XCUITest viability under TCC + `LSUIElement`
14. issue #762 — `SettingsView` coverage
15. issue #763 — [hq/design] Settings-window design reference
16. issue #764 — `StatusItemController` + `main.swift` coverage
17. issue #765 — capture card + notification presentation
18. issue #766 — interaction states: armed / hover / in-flight — blocked by #761

**Wave 5 — CLI + parity**
19. issue #767 — CLI full-output goldens
20. issue #768 — CLI-to-panel render parity — blocked by #754, #767

## Key Decisions

1. **BUILD, but the steel-man nearly won.** The strongest case against: the highest-value visual
   testing is *already done* — the 16 px glyph, the one surface where a defect is invisible to the
   operator, has a pixel gate; everything else targets defects a daily user catches by looking. What
   flipped it to BUILD is that the premise is expiring — public distribution removes "the operator will
   notice" as a control.

2. **XCUITest reshaped from a build to a spike (CONSTRAINT-B).** It drives the accessibility tree, so it
   is structurally blind to truncation and overflow — the problem that started this. Combined with
   CI-flake risk on an `LSUIElement` popover behind Accessibility TCC and zero
   `accessibilityIdentifier`s in the codebase, viability is unproven. A NO-GO from issue #761 is an
   explicitly valuable outcome, not a failure.

3. **Text metrics before screenshots.** A screenshot diff says "something changed" and needs a human to
   adjudicate; a metric says "this cell needs 61 pt in a 52 pt slot". Metrics need no oracle, no
   windowserver and no re-baselining, which is why issue #750 leads.

4. **CONSTRAINT-A — no gate without a proven falsifier.** Every gate item must ship a canary proving it
   can fail. Precedent: issue #437's three render bugs were misread five times as "the DESIGN fails
   distinctness"; a golden authored then would have defended them.

5. **Stress renders get a real oracle, not a self-baseline.** The mock has 25 frames and zero
   pathological ones, so issue #753 is blocked on issue #752 authoring them. Rejected the cheaper
   self-oracle route precisely because it walks into the documented baseline trap.

6. **Dynamic Type kept in scope despite being a fix, not a test.** Zero `@ScaledMetric` anywhere;
   raising system text size changes nothing in the panel. Surfaced by the sweep, so dropping it would
   have re-buried it.

7. **The CLI is *more* rigorous than the GUI on this axis.** It already has UAX #11 display widths,
   narrow-terminal degradation tests, ASCII fallback and a colour gate. Its actual gap is narrow —
   substring assertions instead of full-output goldens (issue #767).

8. **Two un-ratified design surfaces surfaced.** `SettingsView` (360 lines) and Dynamic Type layout have
   no design reference at all — their decisions were silently authored by the implementer. issue #763
   ratifies Settings, and requires enumerating divergences rather than adopting the implementation as
   the reference.

## Stats

- **Work items**: 21 in GitHub (umbrella issue #748 + 20 subs, #749-#768)
- **Ready**: 20/20
- **Typed exceptions**: 2 (SPIKE — issues #749, #761)
- **Gaps accepted**: 0
- **Deferred**: 0
- **Flagged likely-to-split at execution**: 2 (issues #756, #764)
- **Requirements**: 12 EARS (R1-R12), all traced to items
- **Coverage gate**: PASS-WITH-FINDINGS — 2 findings, both remediated inline

## Out of Scope (binding)

- Localization / pseudo-localization — app is English-only, no `.lproj`
- Cross-platform UI verification — gated on issues #25-#29, #40
- Redesigning the panel — this is verification, not design
- Fixing design defects present in the mock itself — routes to hq

## Next Steps

- `/do 750` — text-metrics gate: no oracle, no windowserver, no TCC dependency
- `/do 749` — the spike that routes the entire render lane
- `/do-all` — batch execute; note the two hq/design items (#752, #763) are not code work
