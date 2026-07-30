---
type: architecture-decision-record
number: 31
title: "UI verification is a tier stack bounded by structural blindness; no gate ships without a proven falsifier"
date: 2026-07-30
status: accepted
decision_makers: [Oleksii PELYKH (maintainer)]
---

# ADR-0031: UI verification is a tier stack bounded by structural blindness; no gate ships without a proven falsifier

## Status

**Accepted** — 2026-07-30 (**#751**, the capstone of the 20-item UI-testing scope **#748**). Records
the tier architecture the scope actually **built**, the boundary rule that decides which tier owns a
question, and the falsifier constraint every gate in it was held to. Documentation only — no code
change; every artifact cited here had already landed when this was written.

## Context

Thirty ADRs existed and the test architecture had none. The split between the menubar's verification
layers was real, load-bearing, and recorded nowhere — so it got re-litigated from first principles
each time someone met it, and it was re-litigated **wrongly**: XCUITest was proposed, in conversation,
as the fix for a label-truncation problem it is structurally incapable of observing.

That misfire is the point of this record. Each tier exists because it can answer a question the
others physically cannot, and — more importantly — each is **blind** to questions that look like its
own. Choosing a tier by what it
*sounds like it covers* rather than by what it can *see* is how a suite ends up green on a broken
subject.

Two premises routed the entire scope before it started, and both turned out to be documentation
standing in for an untested belief:

1. **"`ImageRenderer` needs a windowserver."** `BarGlyphParityTests`' header asserted this as settled
   fact. It was the stated reason `RenderPanelTool` is an app-tool (`--render-panel <dir>`) rather
   than a test, and therefore the reason the *panel* had no automated visual gate while the *bar
   glyph* did. It had **never been executed**. Issue **#749** executed it: `ImageRenderer` rasterizes
   inside the standalone logic-test bundle (`TEST_HOST: ""` — no host app, no `NSApplication`
   bootstrap, no popover) under the exact `xcodebuild test` command CI runs. The stronger
   no-windowserver claim was confirmed separately under `sandbox-exec` denying
   `com.apple.windowserver*` — with `CGSession` nil, `NSScreen.count` 0 and `CGMainDisplayID` 0, the
   same view rasterized to identical bytes. `ImageRendererHeadlessProbeTests` is the kept regression
   tripwire for the in-bundle half; `BarGlyphParityTests.swift:13-31` carries the correction.

2. **"Views are thin, un-screenshot-tested consumers."** True when written, and encoded in four
   places — `StatusPanelRoster.swift:726`, `StatusPanelFormat.swift:915`, `:1063`, `:1979` — plus
   structurally in `apps/menubar/project.yml`, whose `MenubarTests` target is a `TEST_HOST: ""`
   logic bundle that excludes exactly **eight** Swift files it cannot host (`main.swift`,
   `StatusItemController`, `SettingsView` + `SettingsWindowController`, `SMAppServiceLoginItem`,
   `UserNotificationPresenter`, and the two render tools). See § Decision 5 for why the premise no
   longer holds.

The scope was also authored against a live constraint — **CONSTRAINT-A**: no gate ships without a
canary proving that gate can fail, verified by *mutation*, not by inspection. Its precedent is issue
**#437**, where three render bugs collapsed all four bar glyphs into one identical white blob and were
misread **five times** as "the DESIGN fails distinctness". A golden authored in that window would have
blessed the blob and then **defended** it, reporting green. The constraint earned considerably more
than a footnote. Applying it across twenty items produced a catalogue of concrete ways a visual
gate passes on a broken subject, recorded in § Decision 4.

## Decision

**UI verification is a stack of tiers, and a tier's boundary is what it is structurally blind to — not
what it is conventionally named. A tier that cannot see a question is not weak there but *incapable*,
so its green means nothing. And no gate joins the stack until a canary, driven through the real
predicate, has been observed to redden on a deliberately broken subject.** Six parts:

### 1. The tier stack — each tier owns exactly one question it can actually answer

The **`Structurally cannot see`** column is the load-bearing one; when a question falls in it, the
tier is not merely weak there, it is incapable, and a green result means nothing.

| Tier | The question it answers | Structurally cannot see | Landed artifacts |
|---|---|---|---|
| **Format / string** | What the panel *says*; and the CLI's full rendered output byte-for-byte — **including column layout and shedding across terminal widths** | The **panel's** layout, geometry or pixels: that half asserts verdict strings and never renders a view. (The CLI half is *not* blind to layout — see the note below the table.) | `StatusPanelFormatTests` (2364 lines; the bulk pre-existing, and it also hosts the #759 contrast section — one file, two tiers); **#767** full-output CLI render goldens (`build/fixtures/cli-renders/`, 21 at #767, 27 today) |
| **Text metrics** | Does text *fit*; truncation policy; layout budgets | Whether the result *looks* right — it is a MODEL of layout, not an observation of a live view tree | **#750** `PanelTextMetricsTests` and **#762** `SettingsTextMetricsTests`, both driving the shared CoreText predicates in `Tests/TextMetrics.swift` (`CTLineCreateTruncatedLine` / `CTFramesetter`; no oracle, no windowserver, no TCC — extracted by #762 so a second suite could not re-derive them); **#755** `PanelRosterGeometryTests`, deliberately self-contained. **#757** is two halves: a measured sweep, plus a *structural source lint* (`PanelDynamicTypeLintTests`) which catches a newly-added `.font(…)` — see the note below |
| **Colour tokens / contrast** | Do the shipped `Assets.xcassets` colour VALUES clear their WCAG bar at the surface each one paints — resolved through the real `Color.panelAssets` seam, in **both themes** | Legibility over the panel's real vibrancy: it measures sRGB values against an opaque base, never a composited frame. It also pins only the *negative* role→token constraints its bars depend on; the positive mapping is a separate assertion (`:125-152`) | **#759** the #388 tint-token contrast sweep, `StatusPanelFormatTests.swift:1788` — deliberately **two** surfaces, not four: AppKit resolves the `contrast: high` variant from the SYSTEM setting, not the `NSAppearance` name, so a four-surface sweep would be two real measurements and two duplicates presenting as four. `testTheHighContrastVariantsAreNotReachableByAppearance` pins that gap open rather than papering it (#832) |
| **Render goldens** | Drift + distinctness of *modeled* states; and, at row scale, whether an interaction treatment actually changed the pixels — and in the right DIRECTION | Artwork **fidelity** — it renders the same compiled asset the app renders, so a miscompiled-but-distinct glyph bakes into the reference | **#754** `PanelGoldenParityTests` + catalog in `Sources/PanelRenderHarness.swift`; **#753** four pathological rosters (36 → 44 cells = 22 fixtures × 2 themes); `BarGlyphParityTests` (pre-existing); **#766**'s *render lane* (`PanelInteractionStateTests`, `PanelRaster.diffFraction` + the alpha-inclusive `inkMass` direction predicate) |
| **Accessibility tree** | Reachability, ROLE, ENABLED state, element ORDER, and whether `accessibilityHidden` elements genuinely leave the tree | Truncation, overflow, colour, position — a tree walk reports semantic strings, not rendered ones | **#758** `PanelAccessibilityTreeTests` + the `PanelA11y` harness (`:118`); **#762** Settings coverage; **#766**'s *tree lane* (which reuses `PanelA11y`) |
| **Cross-surface** | That the CLI and the panel rank the same fault the same way | Whether either surface's own rendering is correct — it pins agreement, not truth | **#768** `build/fixtures/cross-surface-severity.json`, emitted from `DaemonPayloadFault` and read by both `src/cli.rs` and `CrossSurfaceSeverityParityTests` |
| **Design oracle** | The intended visual result, authored | Nothing automatically — it is a human comparison, deliberately **not** a CI gate | **#752** pathological mock frames + **#763** Settings reference (46 `data-frame` entries: 42 `.pop` panel + 4 Settings); `design/build-comparison.py` pairs by NAME over the 42 |
| **Manual pre-release** | The real `NSPopover` round-trip, TCC-gated captures, system accessibility settings, window lifecycle | Anything automatable — if it can be automated it belongs above | **Six** checklists in `apps/menubar/design/README.md`, indexed at § "The manual checklists, indexed": Appearance settings (#760), VoiceOver (#758), Settings window (#762), Status item + app entry (#764), Capture + notification (#765), Interaction states (#766) |

Three entries above are **split**, each along a different axis — **medium** (format), **lane** (#766),
**gate-kind** (#757). In each case the split is the honest shape, not a filing convenience: the
natural reading of the entry's *name* puts each wholly in the wrong place.

**The format tier is asymmetric.** Its two halves answer the same *kind* of question about two
different media, and only the panel half is render-blind. The CLI half pins the whole rendered frame
as bytes, so terminal **column layout** is squarely inside it: `status-wide-plain.txt` carries six
columns (`ACCOUNT SESSION% RESET WEEKLY% RESET AUTH`) where `status-very-narrow.txt` carries three.
`src/render_golden.rs:61-70` names a *second* invariant these goldens pin — *"a non-TTY `status` must
not shed columns"*, which is why `status-piped` and `status-wide-plain` are byte-identical by design,
and which `each_width_case_exercises_the_degradation_it_claims` (`src/cli.rs:12646`) asserts. So "does
the narrow terminal shed the right columns?" routes **here**, not to the metrics tier. The panel's
equivalent question does not route here; that is what the text-metrics tier exists to cover.

**#766 is two lanes, not one tier**, and `design/README.md` already names them that way. Its **tree
lane** walks the live accessibility tree via `PanelA11y`; its **render lane** rasterizes the real row
twice and diffs. It needed both *because the tree lane could not see what it was testing* (§ Decision 2).
Filing #766 wholly under the accessibility tier would therefore assert exactly the blindness its
render lane exists to defeat, and
would hide the single richest source of § Decision 4's catalogue: wash domination and inversion
blindness are both **render-lane** findings.

**#757 is a measurement half and a lint half**, and the lint is not a weaker measurement — it is a
different kind of gate. `PanelTextMetricsTests` measures a handful of gated cells across the twelve
size classes, which says nothing about the *other* call sites; `PanelDynamicTypeLintTests` is a lint
over the panel's own source text, enforcing the rule structurally so a newly-added `.font(…)` cannot
slip through ungated. #757's AC-2 required exactly that, "rather than hoping a metric assertion
happens to cover the new call site". A tier stack organised only by what each gate *measures* has no
row for a gate that measures nothing and constrains the source instead.

### 2. The non-goal, stated once so it is never re-derived

**XCUITest drives the accessibility tree. A truncated label still reports its full string. XCUITest is
therefore structurally blind to the truncation problem that started this entire scope, and no amount
of XCUITest coverage can close that gap.**

This is not a theoretical caution, and the scope demonstrated it from both directions. The
tree-walking surfaces it built — #758's `PanelA11y` and #766's **tree lane**, which reuses it — walk
the same semantic tree XCUITest exposes, and neither can see a clipped glyph. Meanwhile **every**
truncation finding in the scope came from one of the tiers that measure or rasterize: text metrics
(**#750**) or rendered pixels (**#753** / **#754**).

The sharpest evidence is #766 itself. Hover arming is an *interaction* state — the kind of thing a
tree walk is supposed to own — yet it is gated by **pixels**, because arming is a colour change and no
tree walk reports colour. An interaction gate that needs a rasterizer is the strongest available
refutation of "XCUITest covers interaction, so interaction is handled."

The generalization is the rule for choosing a tier at all: *a semantic tree answers semantic questions;
a visual defect requires a tier that measures or draws — however the defect is filed.*

### 3. The interaction tier is not XCUITest's, though XCUITest works

Two landed results settle this, and they point in opposite directions from the original framing
("XCUITest, if viable"):

- **#761 measured GO, with a hard scope ceiling.** Twenty consecutive runs on `macos-latest`
  (macOS 26.4 / Xcode 26.5, run `30350129101`): **0 % flake**. The expected `NSPopover` flakiness does
  not exist and TCC is not the blocker. Probes A/B/D/E passed 20/20 — the `LSUIElement` app launches
  under XCUITest, its own `statusItems` is reachable and labelled, the panel opens on click, and a
  populated roster is inspectable. Probe **C — reaching the item via
  `XCUIApplication(bundleIdentifier: "com.apple.systemuiserver")` — failed 20/20**, and that is the
  ceiling. The controls were real: `SPIKE761_CANARY=OK` proved the harness could report a failure, and
  `SPIKE761_CONTROL=green` proved the headless suite passes on the same runner in the same job. The
  **developer-machine half is unanswered, not answered negatively** — no valid local run was obtained.
- **#766 then showed the interaction tier did not need XCUITest at all** for the surface #761 was
  priced against. Armed / in-flight / mis-click are gated from the **existing headless bundle** by two
  lanes — a render lane (`ImageRenderer` + `PanelRaster.diffFraction`) and a tree lane (`PanelA11y`) —
  driven through seams added for the purpose (`AccountRowView`'s `armed` / `rowWidth`,
  `AccountSwapModel.pendingPreview(target:)`), so the in-flight window is reachable with no socket, no
  second target, no app host and no cross-process driver.

**The decision that follows: the headless logic bundle owns every interaction question it can reach,
and XCUITest is a proven-viable capability that stays unused until something needs the boundary
crossed.** For the *interaction and lifecycle* half, the boundary is not a judgement call — it is
enumerated in `project.yml` as the **eight files the `TEST_HOST: ""` bundle cannot host**
(§ Context 2). Those surfaces — the real `NSStatusItem` and its click routing, the real popover
round-trip, the Settings *window* lifecycle, the login-item registration, the notification presenter —
are what the Settings-window (#762), status-item (#764), capture/notification (#765) and
interaction-state (#766) checklists own today, and what an XCUITest suite would own if one is ever
built. Adding a file to that exclusion list is the signal that this boundary moved.

**The remaining two checklists sit behind a different kind of boundary entirely, and no `project.yml`
edit would ever signal either.** Both cover a panel that *does* compile into `MenubarTests`, so
neither is hosting-bound — but their reasons differ, and collapsing them would mis-predict when each
becomes automatable:

- **Appearance settings (#760) is blocked by the platform's API.** The accessibility environment keys
  are get-only and `NSAppearance` never reaches SwiftUI (§ Decision 6), so the *variant cannot be
  reached* from a test process at all.
- **VoiceOver (#758) is blocked by subject, not by access.** The tree walk runs fine; what it cannot
  see is VoiceOver's own behaviour — *"the rotor, real focus traversal, and speech are runtime
  features of the screen reader, not attributes of the tree"* (`design/README.md:520-521`). No API
  change would make a tree walk observe a screen reader.

### 4. CONSTRAINT-A — a gate ships only with a canary proving it can fail, verified by mutation

Inspection is not evidence. A canary must be driven through **the same predicate the real assertions
use**, and it must be observed to redden when the subject is deliberately broken. Reading the
assertion and concluding it would catch something is precisely the reasoning that let #437's blob
survive five reviews.

The scope produced a catalogue of ways a plausible visual gate passes on a broken subject. It
generalizes past this repo, and every entry was found by mutation rather than predicted:

- **Composite blindness** — removing **both** the symbol and the text from a row *raised* ink coverage
  0.1196 → 0.1268, because the freed width let the capsule expand. A whole-row assertion cannot see a
  missing element; each construct needing proof gets its own probe with its own subject (**#749**).
- **Wash domination** — deleting the armed-chip brighten left the whole-row test green: the background
  wash outweighs the chip **~500:1**, and the wash alone repaints ~93 % of the row. The chip needed its
  own floor, measured on a blocked row where the wash is held out (**#766**).
- **Inversion blindness** — swapping the tints so arming *dims* the chip kept every **magnitude**
  assertion green. Premultiplied near-black is `(0,0,0)` at every alpha, so an RGB-only predicate
  cannot distinguish `.tertiary` from `.secondary` at all; the fix required a **direction** predicate
  that includes alpha (**#766**).
- **Size-mismatch vacuity** — `diffFraction` returns 1 for mismatched raster dimensions, so a canary
  that perturbs the *size* "passes" while asserting nothing. Size equality must be asserted **before**
  the score is read (**#753**).
- **No-op mutation** — a canary that has silently stopped mutating passes forever. #768's canary guard
  fired for real, catching a band-flip mutation that became a no-op once provenance entered the
  contract.
- **Declaration-vs-render gap** — #768's #575-shaped mutation reddened the **render** observer while
  both **declaration** observers stayed green. An enum-only gate would have missed the original
  cross-surface bug by construction.
- **Degenerate certification** — two self-corrections from #760: a contrast pin whose lever
  (`performAsCurrentDrawingAppearance`) never reached `ImageRenderer`, so its zero would have read
  identically had the assets resolved perfectly; and a canary asserting `XCTAssertNotEqual(b, !b)`,
  true by the type's definition and unfalsifiable under **any** mutation.

The corollary this scope also enforced: **when the premise turns out to be false on measurement, gate
at the measured boundary and say so — never fabricate a failure to satisfy an acceptance criterion.**
#750 hit this (a predicted truncation that measurement showed does not occur), and #752's frames
"gated at the measured boundary" rather than at the predicted one.

### 5. The retired premise, and why it is retired

"Views are thin, un-screenshot-tested consumers" (§ Context 2) described a real property and justified
a real absence. It no longer holds, for two independent reasons:

- **The views accreted layout policy the format layer cannot express.** Truncation modes, frame
  budgets, severity-driven tinting, and per-row geometry now live in the view layer. A verdict-string
  assertion cannot reach them — which is exactly the gap #750, #755 and #757 were built to close.
- **Public distribution removes "the operator will notice" as a control.** #269 shipped the Homebrew
  formula; #172 (cask) is in flight. The premise's unstated backstop was a single operator who would
  spot a broken panel immediately. That backstop is gone.

The premise is therefore **superseded in fact**, and the four comment sites that state it now describe
the reason a *particular* verdict is unit-asserted rather than a claim that the panel is unrenderable
in test. #749 removed the technical basis for the broader reading.

### 6. What is deliberately NOT a tier

- **No appearance-variant tier exists**, and an ADR claiming one would be false. **#760 closed with
  AC-1 and AC-2 recorded NOT MET — infeasible as specified**, not skipped: the accessibility
  environment keys are **get-only** (`.environment(\.accessibilityReduceTransparency, true)` is a
  compile error, not a runtime no-op), the runner's system settings do not reach the renderer, and
  `NSAppearance` never reaches SwiftUI. Its AC-3 (reduce-transparency legibility) closed **PARTIAL** —
  the measurement was delivered, but the verdict needs a ratified design target the mock does not
  define. **#832** records that the shipped high-contrast colour-set variants are consequently
  unreachable and unguarded; **#868** is the product defect behind it.
  A side-finding from that measurement is load-bearing for the render tier: **the renderer ignoring
  system accessibility settings is *why* the panel goldens are machine-portable at all** —
  `PanelGoldenParityTests` had been relying on that property without ever measuring it.
- **The design mock is an oracle, not a gate.** `design/build-comparison.py` slices the mock's `.pop`
  blocks **live** — updating the mock re-baselines it — and it is never run in CI. `hq
  strategy/design-menubar.md` is normative over the mock where they disagree, and the mock is the
  oracle only for what it authors (silence is not authority).
- **`panel-goldens` is a soft CI gate today.** Every step is `continue-on-error`, so it always reports
  pass and cannot tell you the panel drifted. **#790** promotes it to required after N=10 green runs.

## Alternatives considered

1. **Make XCUITest the interaction tier** (rejected — priced, then obsoleted).
   - **Pros**: it is genuinely viable (#761: 20/20, 0 % flake), and it reaches the real popover and the
     real status item, which the headless bundle cannot.
   - **Why rejected**: #766 demonstrated that the surface XCUITest was priced against — armed,
     in-flight, mis-click — is reachable from the **existing** bundle via `PanelA11y`, at no new target,
     no app host, and no cross-process driver. Standing up a second test target to re-cover ground
     already covered buys nothing and adds a lane to maintain. The capability is recorded as proven so
     that a future need crosses a measured boundary rather than re-running the spike; probe C's 20/20
     failure is that boundary's known edge.

2. **One end-to-end screenshot gate over the whole panel** (rejected).
   - **Pros**: conceptually simple; one artifact to review.
   - **Why rejected**: it is the *composite blindness* failure mode as an architecture. A whole-panel
     assertion cannot see a missing element (measured: removing two elements *raised* coverage), so it
     would report green on precisely the defects it was built to catch, while its per-run drift noise
     would swamp the localized changes that matter. The tier stack exists because one subject cannot
     honestly attest to all of it.

3. **Leave the split in code comments and issue threads** (rejected — the status quo this ADR ends).
   - **Why rejected**: it had already failed twice in the observable record. The split was re-litigated
     wrongly (XCUITest for truncation), and two of its premises were untested beliefs stated as settled
     fact for long enough to route the whole scope (§ Context). Comments are where the *first* premise
     lived, and it took a dedicated spike to dislodge it.

4. **Wait for #749's verdict before recording anything** (rejected as filed, and superseded by events).
   - The issue itself noted the render row depended on #749. Deferring the *whole* record for one row
     would have left the four decided rows unrecorded through the entire scope — which is when they
     were being re-derived. In the event the ordering resolved itself: #749, #761 and #766 all landed
     first, so this ADR records measured verdicts for every row rather than a placeholder for one.

## Consequences

### Positive

- **Tier selection has a stated rule.** "Which tier owns this?" is answered by asking what a candidate
  tier is blind to, and the answer is in the table's `Structurally cannot see` column — including for
  the XCUITest-for-truncation question that started the scope (§ Decision 2).
- **Two untested beliefs are retired in writing.** The windowserver claim (#749) and the thin-view
  premise (§ Decision 5) each routed real work while unexamined; both now carry their measurement.
- **CONSTRAINT-A has a reusable catalogue, not just a slogan.** Seven concrete ways a green visual gate
  can be lying, each found by mutation, are available to the next person authoring one — including a
  degenerate-certification pair that produces a canary which cannot fail.
- **The interaction/lifecycle boundary is mechanically visible.** It is the `project.yml` exclusion
  list, so moving it is an edit that shows up in review, rather than a drift nobody notices. The
  other two boundaries (§ Decision 3) — the API-shaped one behind #760 and the subject-shaped one
  behind #758 — are named rather than mechanized, because nothing in the repo can signal either.
- **Honest gaps are recorded as gaps.** No appearance tier is claimed; the mock is named as an oracle
  rather than a gate; `panel-goldens` is named as soft. A reader can tell what is guarded from what
  merely looks guarded.

### Negative / trade-offs

- **Eight tiers is a lot of surface to route between** — and three entries (format, #766, #757) are
  split across media, lanes or gate-kind, so the count understates it. The routing rule requires
  knowing what each tier is blind to — which is what this record supplies, and what a reader who skips
  it will lack. The `Structurally cannot see` column is the mitigation.
- **The metrics tier models layout rather than observing it**, so it can be confidently wrong about a
  real panel. **#938** is a live instance: `rosterLabelBudget` models 171 pt where the panel lays out
  ~216 pt, meaning the tier currently models a *narrower* budget than reality. Accepted because the
  model is headless, deterministic and TCC-free — but a metrics green is not a render green.
- **The render tier's ceiling is calibrated, and one fixture clears it narrowly.** **#937** records
  `degenerate-label` (dark) clearing the drift ceiling by only 1.25×. **#824** records that harness
  warm-up does not reach steady state for un-warmed content (±1/255), and **#911** that the
  `ImageRenderer` settle loop is copied three times across the test and render surfaces. None is
  load-bearing today; all three are the kind of drift that erodes a threshold gate quietly.
- **Cross-surface parity pins agreement, not correctness**, and **#919** records that
  `DaemonPayloadFault::ALL` is not provably exhaustive — a ninth fault could go unranked with both
  cross-surface gates green.
- **The `swift` job carries a known flake** (**#948**,
  `AccountSwapTests.testPendingNamesTheTargetAndIsBusyThenClearsOnDone`). `ci-ok` is the only *required
  status check*, but `swift` sits in `ci-ok.needs`, so a flake there blocks merge all the same — and a
  flaky merge-blocker is corrosive to every gate above it, because it trains readers to re-run rather
  than read.
- **This ADR is a point-in-time record of a scope that is still moving.** Counts cited here (27 CLI
  goldens, 44 golden cells, 46 mock frames, six checklists, eight excluded files) were measured on
  2026-07-30 and will drift; the *boundaries* are the durable content, and the counts are provenance
  for them.

## Related

- **Issue #751** — this ADR. Parent scope **#748** (20 sub-issues, #749–#768, across 5 waves).
  #751's own tier table was stale in two rows by the time it was authored, which this record corrects
  from the artifacts (§ Decision 1, 3).
- **Spikes that retired a premise**: **#749** (`ImageRenderer` headless —
  `ImageRendererHeadlessProbeTests`, kept as a regression tripwire; the correction is in
  `BarGlyphParityTests.swift:13-31`); **#761**
  (XCUITest viability — evidence in `apps/menubar/spikes/README.md`, run `30350129101`).
- **Tier artifacts**: **#750** (`PanelTextMetricsTests`; its predicates were extracted to
  `Tests/TextMetrics.swift` by #762), **#759** (the #388 tint-token contrast sweep),
  **#753** / **#754** (panel goldens +
  `Sources/PanelRenderHarness.swift`), **#755**, **#757**, **#758** (`PanelA11y`), **#762**, **#766**
  (`PanelInteractionStateTests`), **#767** (CLI render goldens), **#768** (cross-surface manifest),
  **#752** / **#763** (mock frames). **#756** (Dynamic Type *support*) is the one sub-issue filed as a
  product defect rather than a test gap; its scale layer landed (PR #822) and #757's gate and lint
  cover it from the inside, but the issue stays open on the end-to-end plumb — nothing in the shipped
  app drives the size class, tracked as **#817**.
- **CONSTRAINT-A precedent**: **#437** — the three render bugs misread five times as a design failure;
  the baseline trap is stated at `BarGlyphParityTests.swift:38`. Related: **#524** (brand glyph),
  **#525** (the parity gate's calibrated metric).
- **Open follow-ups this scope produced**: **#790** (promote the panel golden gate to required),
  **#938** (`rosterLabelBudget` 171 pt vs ~216 pt), **#824** (warm-up steady state), **#911** (settle
  loop copied three times), **#937** (`degenerate-label` clears by 1.25×), **#919**
  (`DaemonPayloadFault::ALL` exhaustiveness), **#948** (flaky `AccountSwapTests`), **#903** (the mock
  authors an armed rule it never instantiates).
- **Recorded gaps**: **#760** (closed with AC-1/AC-2 NOT MET — infeasible), **#832** (high-contrast
  colour-set variants unreachable by `NSAppearance`), **#868** (the panel honours none of the three
  accessibility settings).
- **[ADR-0026](0026-daemon-fault-severity-rank-is-cross-surface.md)** — the cross-surface severity rank
  this scope's #768 turned into an enforced contract; its § Consequences records that enforcement.
- **[ADR-0010](0010-macos-app-repo-topology.md)** (app topology),
  **[ADR-0029](0029-macos-is-the-only-supported-build-target.md)** (macOS-only, so CI green says
  nothing about portability).
- **Code / docs**: `apps/menubar/project.yml` (the `TEST_HOST: ""` bundle and its eight exclusions —
  the interaction boundary), `apps/menubar/design/README.md` (the six manual checklists, § Expected
  reconciliations), `apps/menubar/design/build-comparison.py` (the name-paired mock oracle),
  `apps/menubar/Sources/StatusPanelRoster.swift:726` and `StatusPanelFormat.swift:915` / `:1063` /
  `:1979` (the retired premise), `../hq/strategy/design-menubar.md` (normative over the mock).
