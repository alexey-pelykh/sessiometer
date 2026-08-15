---
type: prd
scope: menubar accessibility reachability
created: 2026-07-30
workflow: /capture-requirements
source: session context (#817 thread), scoped via /scope 2026-07-30; working notes were transient
  under .tmp/scopes/ and are not part of the repo. This PRD is self-contained — nothing downstream
  needs to dereference that path.
dor_status: passed-with-findings
formulation: {}
features: {}
artifacts: {}
---

# PRD: Menu-bar Accessibility Reachability

## 1. Problem

### 1.1 Statement

The menu-bar app's accessibility affordances are verified **only downstream of their own injection
point**. Every existing gate supplies its own stimulus in-process: `PanelRenderHarness` injects a
`DynamicTypeSize`; `PanelAppearanceVariantTests` pins that the four SwiftUI accessibility environment
keys are get-only and therefore cannot be driven at all. The **OS → app delivery path sits outside
every one of those boundaries**.

The consequence is a state the whole verification apparatus is blind to: an affordance can be
**built, measured, gated — and unreachable by any real user**, with the full suite green.

That is not hypothetical. It is the shipped state of `PanelTypeScale`:

- `StatusPanelView.swift:94` reads `\.dynamicTypeSize`; `:105` derives the factor; `:112` publishes
  `\.panelScale`; subviews consume it. Correct, and gated by `PanelTextMetricsTests` +
  `PanelDynamicTypeLintTests`.
- `PanelRenderHarness.swift:668` is the **only** site in the tree that varies the value — a test
  harness. `StatusItemController.swift:84` constructs `StatusPanelView()` with no injection.
- `PanelTypeScale.factor(.large) == 1.0` (`StatusPanelTypeScale.swift:132`), so every real user
  renders at k=1.0.
- `PanelDynamicTypeLintTests` — the structural gate that would be the natural catcher — **explicitly
  exempts `StatusItemController.swift`** on the #437 brand-lock ground. Deleting the injection (or
  never adding it) leaves the suite green.

### 1.2 Affected users

The operator, on any display or at any visual acuity requiring larger text or reduced transparency.
Single-operator tool: this is a **product-quality directive** (`hq/strategy/design-menubar.md:91`
— "respect Dynamic Type"), not an external compliance obligation. That bounds the appetite; it does
not make the defect unreal.

### 1.3 Why now

Issue #756 shipped the consumer half and #757 gated it; #817 is the declared remainder of the same
defect. Three sibling items (#845, #868, #896) are held in the same blind spot. The cluster resolves
together or not at all — and the reachability gap is *generative*: every future affordance inherits
it until something detects unreachability.

### 1.4 Framing provenance

The narrow statement ("nothing drives Dynamic Type") was **reframed at Phase 0 and the reframe was
user-ratified 2026-07-30**. Two of #817's three load-bearing claims were reclassified from
observation to interpretation — see § 7 Assumption Registry A-1 / A-2.

## 1b. Boundaries

### Appetite

**One `/do-all` batch (≈2 days at observed velocity).** Sized against the #748 batch (20 items, same
window). Not an estimate — a budget.

**Circuit-breaker**: if R-1's measurement finds no OS delivery path **with the R-1b adoption
state established** (a "no delivery" reading taken while the opt-in is unadopted does NOT fire it —
see A-8) AND an in-app preference is
adjudicated unwanted, the driver half is **recorded and dropped**, and the appetite collapses to
{reachability gate + #845 + #896}. This is an explicit live outcome, not a failure mode.

### Out of scope

- **Increased-contrast colour-set variants** — tracked separately as #832; not in the user's
  selected membership.
- **The scaling mechanism itself** — shipped in #756. This PRD does not re-derive it.
- **The panel layout gate for clipping** — shipped in #757. #896 covers only the *overlap* half.
- **The menu-bar status item's own rendering** — bar-locked, must NOT scale (#437 ratified brand
  lock). It appears here only as constraint R-11.
- **VoiceOver / accessibility-tree coverage** — #838/#839/#840 are a different axis.
- **Any daemon behavioural change.** The daemon's `SetTunables` allow-list is 19 *behavioural*
  fields (15 integer scalars + 4 boolean canary overrides); nothing here alters daemon behaviour.
- **Cross-platform** — macOS 13.0+ only (`project.yml:12`); no Linux/Windows surface exists.

## 2. Object Model (OOUX)

| Object | Definition | Core attributes | CTAs |
|---|---|---|---|
| **AccessibilityAffordance** | A capability the app offers a user with an accessibility need | `identity`, `consumerBuilt`, `producerWired`, `verificationTier`, `designReference` | *Reach*, *Verify* |
| **PlatformDeliveryPath** | The route (if any) by which macOS conveys a user's accessibility setting into this process | `stimulusKind` (env-injection \| OS-setting), `activationPolicy` (.accessory \| .regular), `observedEffect` | *Measure* |
| **SizeClassDriver** | Whatever supplies `\.dynamicTypeSize` to the panel | `source` (OS \| in-app \| none), `injectionSite`, `defaultValue` | *Inject*, *Change* |
| **TextSizePreference** | A user-settable size class, if one ships | `value`, `storageLocation`, `wireRepresentation`, `fallback` | *Read*, *Write* |
| **ReachabilityGate** | A gate asserting an affordance's **producer** is wired | `subject`, `predicate`, `canary`, `tier` | *Assert*, *Falsify* |
| **OverlapBudget** | A content-sized element's allowance vs its measured demand | `element`, `allowance`, `demand`, `sizeClass` | *Measure*, *Assert* |
| **DisplaySettingResponse** | The panel's defined rendering under a system display setting | `setting`, `definedRendering`, `ratificationStatus` | *Author*, *Ratify* |
| **DesignReference** | The oracle a surface's output is judged against | `surface`, `path`, `kind`, `ratificationStatus` | *Author*, *Conform* |

**Affordance instances in scope**: PanelTextScale (#756/#817) · SettingsTextScale (#845) ·
ReduceTransparency (#868) · ReduceMotion (#868). *Unbackticked deliberately — these name the
**affordance**, not a code symbol. None exists under that name in `Sources/`; the nearest real type
is the enum `PanelTypeScale`, one letter away from the first, and conflating them would point an
executor at the consumer when the whole scope is about the producer.*

## 3. Requirements (EARS)

### PlatformDeliveryPath

**R-1** *(event-driven)* — **When** the delivery question is investigated, the project **shall**
measure, in a running app, the rendered effect of a system text-size change across a 2×2 of
`stimulusKind` × `activationPolicy`, and record each cell's observed effect.

> Rationale — the 2×2 is not ceremony. #756 measured only *injected environment value* under
> `.accessory`. The app's activation policy is **dynamic**: `.accessory` at `main.swift:252`,
> promoted to `.regular` while Settings is open (`SettingsWindowController.swift:44`), reverted at
> `:90`. If macOS gates delivery on activation policy, that alone reconciles A-1 and A-2 with no
> contradiction. Collapsing the 2×2 re-runs #756's measurement and learns nothing new.

**R-1a** *(ubiquitous)* — The measurement **shall not** be conditioned on the value of the
`LSUIElement` **Info.plist key**. The app *does* declare it — `apps/menubar/Info.plist:25-26` carries
`<key>LSUIElement</key><true/>`, present since the scaffold and wired via `project.yml:45`
(`INFOPLIST_FILE: Info.plist`) — so a probe reading the static key gets `true` and records a **false
positive**: "always an agent app". That is wrong at exactly the moment the question matters. The key
is a *launch-time* declaration and cannot express the app's **dynamic** policy: `main.swift:252` sets
`.accessory`, `SettingsWindowController.swift:44` promotes to `.regular` while Settings is open, and
`:90` reverts. The 2×2's `activationPolicy` axis must therefore be driven by the *observed runtime*
policy, never by the plist.

**R-1b** *(event-driven)* — **When** R-1's 2×2 is run, it **shall** first establish whether this app
has adopted Apple's **"preferred reading size"** opt-in, and **shall** record every cell's result
against that adoption state rather than treating it as fixed.

> Rationale — `StatusPanelTypeScale.swift:75-79` states, committed and unqualified, that macOS's
> system Text Size *"reaches only apps that adopt Apple's 'preferred reading size' opt-in, and this
> app has not adopted it — that wiring is a separate item."* If that is right, **the 2×2 as
> originally shaped cannot see it**: its axes are `stimulusKind` × `activationPolicy`, and neither
> varies opt-in adoption. All four cells would read "no delivery", D-A would be killed, and the
> circuit-breaker would fire — on a probe that never tested the one condition that would have
> enabled delivery. That is the same shape of error this whole PRD exists to catch: a gate green (or
> here, red) on a subject it structurally cannot observe.
>
> This claim is **A-8**, and its provenance is unstated — it is neither measured in this session nor
> traced to an issue, so it is a claim to test, not a fact to build on. Adoption is cheap to probe
> (a throwaway build), so probe it *before* spending the 2×2.

### SizeClassDriver

**R-2** *(state-driven)* — **While** a driver is selected, the app **shall** inject the size class at
the single `StatusPanelView()` construction site, preserving the one `\.dynamicTypeSize` entry point
#756 deliberately left.

**R-3** *(unwanted-behaviour)* — **If** R-1 finds no OS delivery path, **then** the project **shall**
record the finding with its evidence, **and** the decision to ship an in-app preference **shall** be
adjudicated on its own merits at design time.

> **This requirement deliberately supersedes #817's AC-3**, which reads "option 2 ships" — a
> pre-commitment to the answer the measurement exists to inform. Challenged and carried forward at
> `/scope` Stage 0.7; ratified in this PRD. "Ship no driver" is a live outcome.

**R-4** *(ubiquitous)* — The app **shall** render at k=1.0 when no driver value is available, so a
missing, unreadable, or out-of-range preference degrades to today's behaviour rather than failing.

### ReachabilityGate

**R-5** *(ubiquitous)* — The system **shall** carry a gate that fails when the panel's size-class
driver is absent or disconnected from the construction site.

**R-5a** *(CONSTRAINT-A, ubiquitous)* — The gate **shall** be proven falsifiable by **mutation** —
the real driver removed, the gate required to trip — never by inspection.

**R-5b** *(ubiquitous)* — The gate **shall not** be satisfiable by a predicate that also passes when
the driver is absent. A source-text lint over `StatusItemController.swift` is **suspect on two
counts**: that file is excluded from the `MenubarTests` bundle, and `PanelDynamicTypeLintTests`
already exempts it. **Verdict delivered — A-4 is RESOLVED FEASIBLE** (design § 14): the bundle
exclusion bars a *compiled* gate, not a *source-as-data* one, so the gate occupies **T2**. What
remains spiked is the non-literal-source predicate only, not the tier.

**R-5c** *(ubiquitous)* — Closing Matrix row 3 **shall** include authoring a text-size step into the
Appearance-settings manual checklist (`apps/menubar/design/README.md:497-516`). Today that checklist
has four steps — Increase contrast, Reduce transparency, Reduce motion, Light/Dark repeat — and
**none covers text size**, so the T3 obligation the design assigns to row-3 closure currently has no
owner. R-5's T2 gate proves the driver is *wired*; only this step observes it *delivered*. Without
R-5c the T3 half of row 3 is asserted but unscoped — the same built-but-unreachable shape one level
up, applied to the verification step itself.

### OverlapBudget (#896)

**R-6** *(event-driven)* — **When** a roster row is measured at each `DynamicTypeSize` class, each
content-sized element **shall** stay within the allowance the derived budgets assume
(`authColumnAllowance` 60, `statsSignalPillAllowance` 85), and the test **shall** report
required-vs-allowance.

**R-6a** *(ubiquitous)* — The gate **shall not** be a pure frame-arithmetic comparison. #756 scales
**uniformly**, so `k·(default arithmetic)` is a tautology that cannot fail at class N if it passes at
`.large`. Signal lives only in elements sizing to **content**, whose width grows non-linearly with
point size while the allowance grows linearly.

### Screen fit (newly reachable)

**R-7** *(event-driven)* — **When** the panel renders at `PanelTypeScale.ceiling`
(`.accessibility3`, ×2.3529), it **shall** be verified to fit a supported display.

> ~~Currently unverified **because unreachable**.~~ **VERIFIED 2026-08-15 — IT FITS**
> (`PanelCeilingFitTests`, issue #983): **894.50 pt** wide against the **1424 pt** of room a
> 1440 × 900 display leaves, and at the **856 pt** height bound. Measured, **not** ratified — A-6
> (§ 7) carries the resolution and that distinction. The authoring-time rationale that admitted this
> requirement is retained below, and reads as of 2026-07-30.
>
> `StatusPanelTypeScale.swift:68-71` records the
> shipped consequence: at ×2.3529 "the healthy panel is already **894 pt wide** and the Stats tab
> **1322 pt tall**, and the panel has NO `ScrollView`". The panel does **not** hold width fixed —
> `StatusPanelView.swift:60` defines `scaledWidth(_ scale:) = width * scale`, so 380 pt is the width
> at k=1.0 only. `StatusPanelTypeScale.swift:71` calls the popover-height limit "PRE-EXISTING,
> orthogonal", which is true at
> today's default size and stops being a safe deferral once the ceiling is reachable. A driver makes
> it reachable and this question live.
>
> Do **not** cite `:52-53` here: that block sits under the `:48` header "REJECTED ALTERNATIVES, with
> the arithmetic that rejected them", and its 193-of-364 pt → 454 pt arithmetic belongs to the
> *rejected* "fixed panel width, scaled text" option, not to the shipped design.

### SettingsTextScale (#845)

**R-8** *(state-driven)* — **While** the Settings window's fonts scale, its field cells
(`tunableFieldWidth` 96, `accountLabelFieldWidth` 160) **shall** scale by the same factor, or size to
content.

**R-8a** *(ubiquitous)* — R-8's premise **shall** be confirmed by R-1 before implementation. If
Settings' fonts do **not** in fact grow, #845 is *latent*, not live, and its "degrades as the setting
increases" framing must be corrected rather than implemented against.

### DisplaySettingResponse (#868)

**R-9** *(ubiquitous)* — The panel **shall** have a defined rendering under **Reduce Transparency**
and under **Reduce Motion**.

**R-9a** *(ubiquitous)* — Each defined rendering **shall** be **ratified by the operator before
implementation**. Both amend a ratified aesthetic (vibrancy, operator-confirmed 2026-07-07) and
neither is authored by any design reference. Recording as ratification-pending, per #868's own
instruction not to decide them in an implementation PR.

**R-9b** *(ubiquitous)* — #868's verification tier **shall** be recorded as **manual-only**. The four
SwiftUI accessibility environment keys are **get-only** — overriding them is a compile error, measured
and pinned by `PanelAppearanceVariantTests`. No in-process gate can drive these axes.

### TextSizePreference (#817 option 2)

**R-10** *(state-driven)* — **While** an in-app preference is under consideration, its storage
location **shall** be adjudicated between daemon config and client-local, honouring #268's ratified
keystone (the app is a pure IPC client; the daemon owns writes).

> The fork is real: `SetTunables` (`src/config.rs:1226-1276`, whose doc comment at `:1219` reads
> "This type IS the settable allow-list") is **19 daemon-behavioural fields — 15 integer scalars plus
> 4 boolean canary overrides** (`canary_drift_override` #714, `canary_nostashmatch_override` #730,
> `canary_online_probe` / `_strict` #736). Do **not** cite `ConfigWire.swift:47-62` for this: that is
> the *read*-side `TunablesView`'s `CodingKeys` (15 fields); the Swift encode mirror is
> `ConfigWire.swift:152-197`, which carries the 15 integer fields only. The 15 scalars are poll
> cadences, ceilings, velocity, monitor counts and fleet runway. A panel text size is a **client
> display preference** — a different category. #817's "Settings already round-trips config" is true
> of *daemon tunables* and does not settle this.

### Brand lock

**R-11** *(unwanted-behaviour)* — The menu-bar status item **shall not** scale under any driver. Its
template glyph is bar-locked (#437 ratified lock) and sized by AppKit outside the panel's SwiftUI
subtree.

### DesignReference

**R-12** *(ubiquitous)* — The panel at non-default size classes **shall** have a design reference, or
an explicit recorded `none` with rationale.

> Verified absent: `menubar-preview.html` carries 46 `data-frame` entries and **zero** at any
> non-default class. Whatever design decides becomes a **new authored decision**, not conformance.

## 4. Acceptance Criteria (GWT + BUT NOT)

**AC-1 (R-1)** — *Given* a running signed app, *When* the system text-size setting is changed, *Then*
the rendered effect is recorded for each of the four 2×2 cells with evidence.
**BUT NOT** by reading the `LSUIElement` Info.plist key, whose static `true` cannot express the
runtime policy change (R-1a), **and NOT** with the app's "preferred reading size" adoption state
left unestablished — that must be probed first and every cell recorded against it (R-1b), **and NOT**
by re-measuring the
injected environment value alone (that is #756's already-recorded result).

**AC-2 (R-2)** — *Given* a selected driver, *When* the panel is opened in the real app, *Then* it
renders at the corresponding size class — verified against a running app, not only the harness.
**BUT NOT** by adding a second `\.dynamicTypeSize` entry point.

**AC-3 (R-3)** — *Given* R-1 returns "no OS path", *When* the driver decision is made, *Then* the
finding is recorded with evidence and the in-app option is adjudicated with its alternatives.
**BUT NOT** shipped automatically on the strength of #817's superseded AC-3.

**AC-4 (R-5/R-5a)** — *Given* the reachability gate, *When* the driver injection is removed by
mutation, *Then* the gate FAILS at that site; *and* in the same run the unmutated tree stays clean.
**BUT NOT** a predicate that also passes with the driver absent (R-5b), **and NOT** a canary alone —
both directions in one run.

**AC-4a (R-5c)** — *Given* the Appearance-settings manual checklist at
`apps/menubar/design/README.md:497-516`, *When* Matrix row 3 is claimed closed, *Then* that checklist
carries a **text-size step** that opens the panel under a changed system text size and records what
is observed. **BUT NOT** satisfied by the R-1/R-1b spike (that measures the *platform path*, not the
shipped driver), and **BUT NOT** satisfied by AC-4 going green — AC-4's gate is T2 and proves the
driver is *wired*, which is precisely the evidence row 3 already has.

**AC-5 (R-6)** — *Given* each of the twelve `DynamicTypeSize` classes, *When* a roster row's
content-sized elements are measured, *Then* each stays within its allowance and the test reports
required-vs-allowance.
**BUT NOT** by comparing summed frames to panel width — that is `k·(default)` at every class and
cannot fail (R-6a).

**AC-6 (R-7)** — *Given* the panel at `.accessibility3`, *When* rendered on a supported display,
*Then* it fits, or the ceiling is lowered with the measurement recorded.
**BUT NOT** asserted from the existing header comment, which reasons about the limit without
measuring the ceiling itself.

**AC-7 (R-8/R-8a)** — *Given* R-1 confirms Settings' fonts scale, *When* the window renders at a
large class, *Then* its field cells scale by the same factor.
**BUT NOT** implemented before R-1 confirms the premise — if false, #845's framing is corrected
instead (R-8a), **and NOT** by resolving `SettingsTextMetricsTests`' pinned defect test in a way that
leaves the pin asserting a false claim.

**AC-8 (R-9/R-9a/R-9b)** — *Given* Reduce Transparency and Reduce Motion, *When* each rendering is
proposed, *Then* the operator ratifies it before implementation, and the verification tier is
recorded as manual-only.
**BUT NOT** decided inside an implementation PR (#868's own instruction), **and NOT** gated by an
in-process test — the keys are get-only (R-9b).

**AC-9 (R-10)** — *Given* an in-app preference is chosen, *When* its storage is decided, *Then* the
decision records daemon-vs-client with the #268 keystone addressed explicitly.
**BUT NOT** by extending `SetTunables` on the unexamined premise that "Settings already round-trips
config."

**AC-10 (R-11)** — *Given* any driver at any size class, *When* the menu-bar item renders, *Then* it
is byte-identical to its k=1.0 rendering.
**BUT NOT** by removing `StatusItemController.swift`'s lint exemption, which exists for this reason.

**AC-11 (R-12)** — *Given* the panel at non-default classes, *When* a reference is sought, *Then*
either frames are authored into `menubar-preview.html` or an explicit `none` + rationale is recorded
in the design README.
**BUT NOT** silently inheriting the default-class frames as if they governed all classes.

**AC-12 (R-4)** — *Given* a missing, unreadable, or out-of-range preference, *When* the panel
renders, *Then* it renders at k=1.0.
**BUT NOT** failing to open, and **NOT** clamping silently past `PanelTypeScale.ceiling` without the
existing `.dynamicTypeSize(...ceiling)` modifier.

## 5. State Matrix — AccessibilityAffordance reachability

The matrix the current apparatus cannot see. Row 3 is the defect this PRD exists for.

| # | State | consumerBuilt | producerWired | Detected today? | Instance |
|---|---|---|---|---|---|
| 1 | Fully reachable | ✓ | ✓ | n/a | *(none yet)* |
| 2 | Not built | ✗ | ✗ | ✓ (absent feature) | ReduceMotion (#868) |
| 3 | **Built, gated, unreachable** | ✓ | ✗ | ✗ **BLIND** | PanelTextScale (#756) |
| 4 | Wired, consumer absent | ✗ | ✓ | ✓ (no effect) | *(none)* |
| 5 | Built, wired, ungated | ✓ | ✓ | ✗ | overlap (#896) |
| 6 | Built, reachability contested | ✓ | **?** | ✗ | SettingsTextScale (#845) |
| 7 | Built, un-driveable by tests | ✓ | ✗ (get-only) | partial (pinned) | ReduceTransparency (#868) |
| 8 | Reachable past a verified ceiling | ✓ | ✓ | ✗ | screen fit (R-7) |
| 9 | Reachable, no design oracle | ✓ | ✓ | ✗ | non-default classes (R-12) |
| 10 | Locked non-scaling by design | n/a | n/a | ✓ (lint) | status item (#437) |

## 5b. Feature Completeness

| Feature | Verdict | Gap |
|---|---|---|
| Platform delivery measurement (R-1, R-1a, R-1b) | **NEAR-COMPLETE** | R-1b added 2026-07-30 after A-8 surfaced; its adoption pre-probe is specified but unmeasured |
| Driver injection (R-2, R-4) | **NEAR-COMPLETE** | Target driver unknown until R-1 lands — by design |
| Reachability gate (R-5) | **NEAR-COMPLETE** | Tier RESOLVED — T2, A-4 FEASIBLE (design § 14); the non-literal-source predicate remains spiked. R-5c's T3 checklist step is specified but unauthored |
| Overlap gate (R-6) | **COMPLETE** | — |
| Screen fit (R-7) | **COMPLETE** | — |
| Settings scaling (R-8) | **INCOMPLETE** | Premise unconfirmed (A-2). Gated on R-1; may be void |
| Display settings (R-9) | **INCOMPLETE** | Two renderings unauthored + unratified. Blocked on operator (R-9a) |
| Preference storage (R-10) | **NEAR-COMPLETE** | Fork stated, not adjudicated — Stage 2 owns it |
| Brand lock (R-11) | **COMPLETE** | — |
| Design reference (R-12) | **NEAR-COMPLETE** | Depends on R-2's outcome |

## 6. Success Criteria

**North Star** — no accessibility affordance can occupy Matrix State 3 (built, gated, unreachable)
undetected.

| Metric | Type | Planguage |
|---|---|---|
| Reachability coverage | leading | TAG: ReachCov · SCALE: % of shipped affordances with a producer-side gate · METER: count at PR merge · GOAL: 100 % of in-scope · STRETCH: extends to future affordances by construction · **CAVEAT: measures gate PRESENCE, not delivery.** The T2 predicate is syntactic — it proves the injected value derives from a variable, never that the variable's *source* ever moves. Under D-A an OS observation that never fires still renders k=1.0 for every user with ReachCov at 100 %. Read it only alongside the R-1/R-1b delivery result and BlindStates below; a 100 % here is **not** evidence Matrix row 3 is vacated |
| Gate falsifiability | leading | TAG: GateFalsify · SCALE: % of new gates with a passing mutation canary · METER: per-PR review · GOAL: 100 % (CONSTRAINT-A) |
| Blind-state census | lagging | TAG: BlindStates · SCALE: count of Matrix rows still undetected · METER: re-audit at scope close · GOAL: 0 for in-scope rows · Baseline: **6 of 10** — the 5 rows marked ✗ plus row 7, counted because "partial (pinned)" detects the state only for the one affordance a pin names, not for the row |
| Unratified design decisions shipped | lagging | TAG: UnratifiedShipped · SCALE: count reaching main without operator sign-off · METER: per-PR · GOAL: 0 |

**Decision gates** — (a) R-1's 2×2 result selects the driver and confirms-or-voids R-8; (b) a
"no OS path" result — **only once R-1b has established the adoption state** — triggers the § 1b circuit-breaker; (c) R-9a's ratification gates all #868 work.

## 7. Assumption Registry

| ID | Assumption | Origin | Confidence | Cheapest test | Signpost | Hedge |
|---|---|---|---|---|---|---|
| **A-1** | macOS does not deliver a text-size setting to this app | #817 (**AI/issue-inferred**, not measured) | 🔴 | R-1's 2×2 | R-1 cell (OS-setting × .accessory) | Assume unknown; do not pre-commit |
| **A-2** | Settings' system-text-style fonts DO grow with the OS setting | #845 (**inferred** — "therefore") | 🔴 | R-1's 2×2, `.regular` column | Same probe | Treat #845 as premise-gated |
| **A-3** | Activation policy explains A-1↔A-2 | **this pipeline, hypothesis** | 🟡 | R-1's 2×2 | Policy column differs | Do not build on it |
| ~~**A-4**~~ | ~~A producer-side gate is feasible from a test bundle excluding `StatusItemController.swift`~~ | this pipeline | 🟢 | ~~Stage 2 feasibility spike~~ | — | **RESOLVED 2026-07-30 — FEASIBLE at T2** (design § 6 verdict, § 14). The exclusion bars a *compiled* gate, not a *source-as-data* one. No longer an open assumption |
| **A-5** | The operator wants an in-app text-size control if the OS path is absent | **unvalidated** | 🔴 | Ask at the R-3 decision | Circuit-breaker fires | "Ship no driver" stays live |
| ~~**A-6**~~ | ~~The panel at `.accessibility3` fits a supported display~~ | ~~never measured~~ | 🟢 | ~~R-7~~ | ~~Render at ceiling~~ | ~~Ceiling may need lowering~~ **RESOLVED 2026-08-15 — FITS** (`PanelCeilingFitTests`, issue #983): at the ceiling the panel is **894.50 pt** wide against the **1424 pt** of room a 1440 × 900 display leaves, and it meets the **856 pt** height bound there. The ceiling stands. **Measured, not ratified** — that height bound is #818's, carried as *decided in code, pending ratification*, and #1176 carries deriving it from the live screen. No longer an open assumption |
| **A-7** | `design-menubar.md` § D-UX-SETTINGS is authoritative for #845 | design SoT | 🟡 | — | Marked `RATIFICATION-PENDING` (#763) | Re-ratify before conforming |
| **A-9** | `.regularMaterial` is `NSVisualEffectView`-backed, so macOS substitutes the opaque fill itself under Reduce Transparency | **this pipeline, uncited platform premise** | 🔴 | **T3 manual check before designing #868** | Observe the real panel with the setting on | Weakened by: the app drops `NSPopover` (`StatusItemController.swift:90`) and ADR-0031 § Decision 6 finds the renderer ignores system a11y settings — so it is not observable via the render path either |
| **A-8** | macOS Text Size reaches only apps that adopt Apple's **"preferred reading size"** opt-in, and this app has **not** adopted it | `StatusPanelTypeScale.swift:75-79` — **committed in-tree, provenance unstated** (neither measured-here nor issue-traced) | 🔴 | **R-1's pre-probe** (see R-1b) | Adopt the opt-in in a throwaway build and re-read the delivery cell | **Do not let the 2×2 kill D-A without testing this** |

**Ratification status**: A-1 and A-2 are the two claims Phase 0 reclassified from observation to
interpretation. **Neither has been measured.** Every requirement depending on them (R-3, R-8, R-8a)
is explicitly premise-gated rather than written as settled.

## 8. Source Traceability

| Requirement | Source | Reliability |
|---|---|---|
| R-1b | **A-8** (`StatusPanelTypeScale.swift:75-79`) — an in-tree claim with **unstated provenance**; graded 🔴, hence the mandatory pre-probe | **D (in-tree assertion, unverified)** |
| R-1, R-1a | #817 + this session's `main.swift:252` / `SettingsWindowController.swift:44` / `:90` reading, and `Info.plist:25-26` (the declared `LSUIElement` key) | A (direct observation) |
| R-2, R-4, R-11 | #817 AC-2/AC-4; `StatusPanelView.swift:105-118` | A / C |
| R-3 | `/scope` Stage 0.7 challenge to #817 AC-3; **user-ratified 2026-07-30** | B |
| R-5, R-5a, R-5b | This session's measurement of the `PanelDynamicTypeLintTests` exemption | A |
| R-5c | This session's read of `apps/menubar/design/README.md:497-516` — the Appearance-settings checklist has four steps, none covering text size | A |
| R-6, R-6a | #896 body (carries its own measurement) | C |
| R-7 | `StatusPanelTypeScale.swift:68-71` (the shipped ceiling arithmetic; **not** `:52-53`, which is a rejected alternative's) | A |
| R-8, R-8a | #845 + `SettingsTextMetricsTests.swift:71,639` | C (inferred — see A-2) |
| R-9, R-9a, R-9b | #868 + `PanelAppearanceVariantTests` get-only pin | C / A |
| R-10 | #268 keystone + `src/config.rs:1226-1276` (`SetTunables`, the write allow-list) and `ConfigWire.swift:152-197` (its Swift encode mirror) | A / B |
| R-12 | `menubar-preview.html` frame census (46/0) | A |
| Framing | Phase 0 reframe, **user-ratified 2026-07-30** | B |
| Membership | User selection "all enriched", **BINDING** | B |

## 9. Cross-Cutting & Non-Functional

**9.1 Security** — `N/A for new surface area`, with one carried constraint: if R-10 selects daemon
storage, the write path must honour #268's invariant that `config-set` never touches credentials or
roster structure. No new authn/authz surface; no secrets.

**9.2 Compliance & Regulatory** — `N/A — single-operator internal tool.` Accessibility here is a
product-quality directive (`design-menubar.md:91`), not a WCAG/ADA/EN-301-549 obligation with an
external auditor. Recorded explicitly so a later reader does not infer a compliance driver that does
not exist.

**9.3 Reliability & Observability** — R-4 is the reliability requirement (degrade to k=1.0 on any
preference failure). Observability: **the reachability gate IS the observability play** — it makes
Matrix State 3 visible. No new runtime telemetry; a display preference is not an operational signal.

**9.4 Performance & Scalability** — Bounded by `PanelTypeScale.ceiling` (`.accessibility3`), already
clamped via `.dynamicTypeSize(...ceiling)`. R-7 covered the one live risk (screen fit at the
ceiling); it is measured and the panel fits — A-6 (§ 7) carries the verdict.
No scalability dimension — one panel, one user, no load.

**9.5 Operational** — `N/A — no deploy, migration, or runbook impact`, **unless** R-10 selects daemon
storage, which would add a config key and therefore a config-schema consideration. Flagged for Stage
2; not pre-decided here.

**9.6 Lifecycle** — First launch with no preference → R-4's k=1.0 default. Uninstall/reinstall: a
client-local preference is lost, a daemon-stored one persists — a genuine input to R-10's fork.
Deprecation: if R-1 later finds an OS path, an in-app preference would need a migration story; noted,
not designed.

## 10. Requirement Provenance (DoR check 6)

Every requirement partitioned by origin. The pipeline-authored set is what check 6 exists to catch —
a fabricated requirement otherwise passes checks 1–5 by tracing to the very PRD that fabricated it.

| Origin class | Requirements | Ratification |
|---|---|---|
| **user-stated** | Scope membership (8 items); the § 1.4 framing; R-5/R-5a/R-5b, R-10, R-12 (missing-category items 6/7/8, admitted by the "all enriched" selection) | ✅ user-selected 2026-07-30 |
| **issue-traced** | R-6/R-6a (#896 body) · R-9/R-9a/R-9b (#868 body + the measured get-only pin) · R-2, R-11 (#817 AC-2/AC-4) · R-8 (#845 body) | ✅ not pipeline-invented |
| **pipeline-authored — granularly ratified** | **R-3** (supersedes #817 AC-3) · **R-7** (screen fit) · **R-1/R-1a** (the 2×2 spike shape, including R-1a's plist exclusion) · **R-1b** (the A-8 adoption pre-probe, added 2026-07-30 from an in-tree claim, not from an issue) | ✅ ratified individually 2026-07-30, each against its stated alternative |
| **pipeline-authored — disclosed, not separately ratified** | **R-4** (degrade to k=1.0) · **R-8a** (confirm #845's premise first) · **R-5c** (author the missing text-size checklist step, added 2026-07-30 at the post-submit gate) | ⚠ **Finding.** Derivations from measured fact with no alternative to weigh; disclosed to the user at the DoR gate rather than put to a vote. Re-ratify if any is later treated as load-bearing in its own right. R-5c's measured fact is that the Appearance-settings checklist has four steps and none covers text size; the only alternative was to leave the design's T3 obligation unowned. |

**~~#971 + #817 amendment owed (R-1a)~~ — DISCHARGED 2026-07-30** (both bodies carry a dated
`CORRECTED` note; verified on the tracker). Retained for provenance: both issue bodies were filed carrying the *refuted* claim
that this app "declares no `LSUIElement` key at all", and describe the resulting probe error as a
**false negative**. `Info.plist:25-26` declares the key; the error is a **false positive**. Their
acceptance criteria survive unchanged — "not conditioned on the `LSUIElement` key" is still the right
instruction — but the factual claim and the polarity must be corrected in both bodies, or an executor
reading only the issue acts on a refuted premise. **#971 additionally owes the R-1b / A-8 axis.**

**~~#817 amendment owed~~ — DISCHARGED 2026-07-30** (AC-3 struck through in the issue body,
marked `SUPERSEDED 2026-07-30 (PRD R-3, operator-ratified)`). Retained for provenance: R-3 supersedes that issue's AC-3, so #817's body must be amended at Stage 3
so the issue and this PRD do not disagree. Authorized in the same ratification. Recorded here so it
cannot be lost between stages.

## Change Log

| Date | Change |
|---|---|
| 2026-07-30 | Created. `/scope` Stage 1. Framing reframed to verification-tier gap (user-ratified). #817 AC-3 superseded by R-3 (user-ratified). R-7 admitted from the Phase 4 category sweep (user-ratified). R-1 shaped as a 2×2 (user-ratified). DoR: **PASS-WITH-FINDINGS**. |
| 2026-07-30 | Corrected at the pre-submit gate, three rounds. **R-1a inverted** — the app *does* declare `LSUIElement` (`Info.plist:25-26`); a plist read is a false *positive*, not a false negative (propagated to #971 and #817). **R-1b + A-8 added** — `StatusPanelTypeScale.swift:75-79` names a "preferred reading size" opt-in the 2×2's axes cannot vary, so adoption is now a mandatory pre-probe and the circuit-breaker is gated on it. **A-9 added** — the `NSVisualEffectView` substitution premise under #868 was uncited. **R-7 re-based** off `:68-71` (the shipped ceiling arithmetic) rather than `:52-53` (a rejected alternative's). **`SetTunables` corrected** 15 → **19** fields (15 int + 4 boolean canary overrides), and its citation moved off the read-side `TunablesView`. |
| | `dor_status` remains **passed-with-findings**; it was originally recorded against a requirement set that did not contain R-1b, and R-1b's own source (A-8) is graded 🔴, so the verdict is unchanged rather than upgraded. |
| 2026-07-30 | Corrected at the **post-submit** external-review gate (PR #994), which reviewed the PR as a submitted artifact and so could see what four per-file rounds structurally could not: the links *between* artifacts. Every one of its findings was cross-artifact; not one was a defect in the prose, and all 29 `file:line` citations survived a citation-by-citation attack. **A-4 RESOLVED** — the design delivered its FEASIBLE verdict (T2, § 14) while this PRD still said "Stage 2 owes a verdict" in three places; that leak ran *design → PRD*, the one direction the design's own write-back rule had no row for, now added. **R-5c + AC-4a + Cap-1.3 added** — the design assigned row-3 closure a T3 obligation, but no manual checklist covers the text-size axis at all (`design/README.md:497-516` has four steps, none of them text size), so the obligation had no owner; "T3 was never run" was a misdiagnosis of "no T3 step exists". **ADR-A status unified** — § 16b said `ratified` where § 12 says `PROPOSED`. **Appetite anchor 21 → 20 items** (ADR-0031 and #748's own body both say 20). Affordance-instance labels unbackticked — `PanelTextScale` is one letter from the real enum `PanelTypeScale` and is not a code symbol. |
| 2026-08-15 | **A-6 RESOLVED — the panel FITS at its ceiling.** `PanelCeilingFitTests` (issue #983, PR #1274) renders all 22 harness states at `.accessibility3` and measures the footprint against 1440 × 900 pt: **894.50 pt** wide against the **1424 pt** of room the display leaves, and at the **856 pt** height bound (900 − 24 − 20). Struck in § 7 per the design's own write-back rule; R-7's rationale and § 9.4 no longer read as unmeasured. The design followed at its § 5.4 inventory, § 11 Cap-3.1 tier (**T3 → T1** — the delivered gate is an in-process render, not the anticipated manual pass), § 13, § 14 Feasibility Summary + risk row, and § 16. **Measured, not ratified**: the 856 pt bound is #818's, carried as *decided in code, pending ratification*, and #1176 carries deriving it from the live screen — untouched here. |
