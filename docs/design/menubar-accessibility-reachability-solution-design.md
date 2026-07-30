# Solution Design: Menu-bar Accessibility Reachability

**Source PRD**: `docs/requirements/menubar-accessibility-reachability.md` (`dor_status: passed-with-findings`)
**Status**: `draft` — load-bearing open questions remain (§ 14). Locks only when R-1 lands.
**Date**: 2026-07-30

## 1. Goals and Drivers

Make the app's accessibility affordances **reachable**, and — more durably — make *unreachability
itself detectable*, so no future affordance can occupy the blind state (PRD § 5 Matrix row 3).

Design driver: every existing gate supplies its own in-process stimulus, so the OS→app path is
outside all of them. The design must therefore add a **producer-side** verification tier, not another
consumer-side gate.

## 2. Constraints

| Constraint | Source | Effect on design |
|---|---|---|
| macOS 13.0 deployment target | `project.yml:12` | No macOS 14+/26 API may be load-bearing |
| Status item must not scale | #437 ratified brand lock | R-11; `StatusItemController.swift` keeps its lint exemption |
| App is a pure IPC client; daemon owns writes | #268 ratified keystone | Constrains FORK-2, does not settle it |
| Four SwiftUI a11y environment keys are **get-only** | measured, pinned by `PanelAppearanceVariantTests` | #868 is **manual-only**; no in-process gate possible |
| `StatusItemController.swift` excluded from `MenubarTests` | `project.yml`, with rationale | Forbids a *compiled* gate; does **not** forbid a source-as-data lint (§ 14, Feasibility Summary) |
| Panel width is 380 pt **at k=1.0** and scales with the class (`scaledWidth = width * scale`, `StatusPanelView.swift:60`); height is intrinsic, with no `ScrollView` | `StatusPanelTypeScale.swift:68-71` | R-7 screen-fit risk: at the ×2.3529 ceiling the panel is already **894 pt wide**, Stats **1322 pt tall** |
| Single `\.dynamicTypeSize` entry point | #756 deliberate | R-2: driver injects at exactly one site |

## 3. Context and Scope

**In**: the menu-bar app's SwiftUI surfaces (panel + Settings), their verification tiers, and one
possible new preference. **Out**: the daemon's behaviour (unchanged in every option), the status
item's rendering, VoiceOver/accessibility-tree work (#838/#839/#840), increased contrast (#832).

## 4. Solution Strategy

Three strategic commitments, in dependency order:

1. **Measure before choosing.** R-1's 2×2 gates FORK-1 and confirms-or-voids #845. Nothing that
   depends on the driver identity is designed to a specific answer.
2. **Ship the detection regardless of the driver.** The reachability gate (R-5) is the only element
   that is *unconditional* — it has a defined shape under every FORK-1 outcome (§ 5.2), including
   "ship no driver". This is what makes the design robust to its own central unknown.
3. **Author the missing oracles, don't infer them.** Two surfaces have no design reference; #868's
   two renderings need operator ratification (R-9a) before any implementation.

## 5. Building Blocks

### 5.1 FORK-1 — the driver (LIVE OPTION SET, not converged)

Per `progressive-elicitation`, a high-blast fork whose discriminating evidence does not yet exist is
carried as an **option set with a signpost**, not decided early.

| Option | Mechanism | Feasibility | Cost | Kills it |
|---|---|---|---|---|
| **D-A** follow the OS setting | Observe the system text-size setting, map → `DynamicTypeSize`, inject at the construction site | **UNKNOWN** until R-1 | Lowest — no UI, no storage, no new reference | R-1 shows no delivery on `.accessory` **with the opt-in adopted** (R-1b) — an unadopted "no delivery" reading does not kill it |
| **D-B** in-app preference | A `panel_text_size` value the user sets in Settings | FEASIBLE | Highest — storage (FORK-2) + Settings control + design reference (R-12) + #946's third unreferenced surface | Operator does not want it (A-5) |
| **D-C** ship no driver | Record R-1's finding; pin the unreachability | FEASIBLE | Lowest — but #756's mechanism stays unreachable | Operator wants reachability regardless |

**Signpost**: R-1's `(OS-setting × .accessory)` cell. **Correct-by-default is D-A; reliable is D-B;
honest-if-neither is D-C.** Not mutually exclusive: D-A with D-B as an override is a legitimate
composite if R-1 is positive.

> **D-C is a real outcome, not a failure.** Under D-C the reachability gate still ships — as a
> *defect pin* rather than a positive gate (§ 5.2). The blind state (PRD Matrix row 3) becomes a
> **known** state. That is a genuine improvement over today even with no driver, which is precisely
> why R-3 forbids pre-committing to D-B.

### 5.2 ReachabilityGate — one item, two shapes, selected by R-1

The design insight that makes the gate unconditional. The repo already ships **both** shapes:

| FORK-1 outcome | Gate shape | Predicate | Precedent in-tree |
|---|---|---|---|
| D-A or D-B | **Positive reachability gate** | `StatusItemController.swift` injects a size class at the `StatusPanelView()` construction site, from a **non-literal source** | `PanelDynamicTypeLintTests` (source-as-data lint) |
| D-C | **Defect pin** | The construction site injects **nothing** — green while the defect stands, **red the moment it is fixed**, with an inline note to replace the pin | `SettingsTextMetricsTests.testTheSettingsCellsDoNotScaleWithDynamicTypeAndThatIsIssue845` (`:644` — note **845**, not the unrelated open #844 about the apply-failure label wrapping); `PanelAccessibilityTreeTests` pinning #838/#839 |

Both shapes satisfy R-5b (neither passes when the driver's state is the other one). Both are
mutation-falsifiable (R-5a). **The work item is therefore writable before R-1 lands** — only its
predicate polarity is deferred.

**Non-literal-source clause** (R-5b hardening): a positive gate asserting only *"the file contains
`.dynamicTypeSize(`"* would pass against a hardcoded `.dynamicTypeSize(.large)` — semantically dead,
k=1.0, indistinguishable from today. The predicate must require the injected value derive from a
variable/property, not a literal. This is the one genuinely novel predicate in the design and is
routed to a spike (§ 14 SPIKE-2).

**What this gate does NOT prove — stated plainly, because the omission is the same shape as the
defect this whole design exists to catch.** The predicate is **syntactic**: it proves the injected
value *derives from* a variable, never that the variable's **source** ever moves. Under D-A, an OS
observation that never fires yields a variable-sourced injection that still renders k=1.0 for every
user — gate green, PRD Matrix **row 3 still occupied**. The gate's own stimulus is supplied
in-process (it reads source text), so it is structurally blind to delivery, exactly as T1 is blind to
the producer.

Consequences, both load-bearing: PRD § 6's `ReachCov` metric therefore measures gate **presence**,
not reachability, and a 100 % reading is not evidence the blind state is vacated (its row carries
this caveat); and **delivery itself remains a T3 obligation** — the R-1/R-1b measurement and the
manual checklist are what close row 3, not this gate. Do not let a green T2 gate retire the T3 step.
**That step does not exist yet** — no manual checklist covers the text-size axis (§ 11), so R-5c /
Cap-1.3 scopes authoring it. Until R-5c lands, "the manual checklist" in this sentence names an
obligation with no owner, which is why it is a requirement rather than an assumption.

### 5.3 FORK-2 — preference storage (live only under D-B)

| Option | Honors #268? | Survives daemon-down? | Wire cost |
|---|---|---|---|
| **S-A** daemon `SetTunables` | ✅ directly | ❌ **no** | New key + `ConfigWire` mirror + Swift decode + possible schema consideration |
| **S-B** client-local (`UserDefaults`) | ⚠️ arguably — see below | ✅ yes | None |

**Recommended: S-B**, on one discriminating argument rather than preference:

> Under S-A a display preference becomes **unreadable exactly when the panel is already degraded**.
> The app has an honest-disconnected state; if the daemon is down, the panel would fall back to
> k=1.0 — so the user's accessibility setting evaporates during a fault, which is when legibility
> matters most. An accessibility affordance that fails with its transport is not an accessibility
> affordance.

Secondary — **and weaker than first written, so weigh it as secondary only**: `SetTunables`
(`src/config.rs:1226-1276`) is **19 daemon-behavioural fields — 15 integer scalars plus 4 boolean
canary overrides** (#714, #730, #736 ×2). The earlier framing said "15 behavioural *scalars*" and
inferred that a display preference is categorically different because it is not a scalar; the four
booleans are the in-tree precedent that a settable need not be a scalar, so that inference does not
hold. What survives is the *behavioural* half: every one of the 19 changes daemon behaviour, and a
panel text size does not — the 15 scalars are poll cadences, ceilings, velocity, monitor counts and
fleet runway. The daemon would store a value it never
reads. #268's keystone governs *config writes to daemon-owned state*; a client display preference is
not daemon-owned state, so S-B does not violate it — it falls outside it.

**Decision-to-ratify** (`decision-surfacing`): S-B is author-chosen. Recorded, not silently settled.

### 5.4 Component inventory

| Component | Requirement | Type |
|---|---|---|
| `PlatformDeliveryProbe` | R-1, R-1a, **R-1b** | spike artifact: the A-8 adoption pre-probe, then the 2×2 measurement recorded against that state |
| `SizeClassDriver` | R-2, R-4 | production (shape per FORK-1) |
| `ReachabilityGate` | R-5, R-5a, R-5b | test (polarity per FORK-1) |
| `TextSizeManualStep` | **R-5c** | manual checklist authoring — the T3 half of row-3 closure |
| `OverlapBudgetGate` | R-6, R-6a | test |
| `ScreenFitVerification` | R-7 | manual + recorded measurement |
| `SettingsCellScaling` | R-8, R-8a | production, **premise-gated on R-1** |
| `DisplaySettingRenderings` | R-9, R-9a, R-9b | design decision → ratification → manual verification |
| `PanelSizeClassReference` | R-12 | design artifact (mock frames or recorded `none`) |
| `TextSizePreference` | R-10 | production, **only under D-B** |

## 6. Runtime View

**Driver flow (D-A/D-B)**: source (OS observation | preference read) → `StatusItemController`
construction site → `.dynamicTypeSize(value)` → `StatusPanelView` `\.dynamicTypeSize` → `:105`
factor → `\.panelScale` → subviews. Failure at any upstream step → R-4 fallback k=1.0.

**Gate flow**: `PanelScaleLint`-style discovery walks `Sources/` recursively for SwiftUI importers,
reads `StatusItemController.swift` **as data**, applies the polarity predicate, and a mutation canary
drives the same function with the injection reverted in-memory.

## 7. Deployment View

**N/A under the recommended path.** Every element in § 5 ships inside the existing `.app` bundle and
the existing `MenubarTests` logic bundle; nothing alters packaging, signing, or the launchd plist.

**One conditional**: this holds only while FORK-2 resolves to S-B (client-local storage). PRD § 9.5
is stated conditionally for the same reason — *"unless R-10 selects daemon storage"* — and S-A is
still live (ADR-D is PROPOSED, OQ-4 open). If S-A is chosen, the daemon's `SetTunables` allow-list
gains a non-behavioural key and this section must be re-derived.

## 8. Interface Contracts

**Under S-B: none.** No wire change, no schema bump, no `ConfigWire` edit, no Swift/Rust contract
surface. This is a material part of S-B's argument.

**Under S-A** (not recommended): a **20th** `SetTunables` field (the allow-list is 19 today), its snake_case wire key, the
`ConfigWire.swift` `CodingKeys` mirror, `SettingsModel` plumbing — and a decision on whether a
display key belongs in a daemon config schema at all.

## 9. UX Architecture

**Under D-B only**: a text-size control in Settings. It would be the **third** Settings surface with
no design reference (#946 already tracks two), so R-12's authoring obligation extends to it. Placement,
control type (stepper vs picker vs slider), and label copy are all unauthored.

**Under D-A/D-C**: no UX surface at all.

## 10. UI Strategy

Two unauthored surfaces requiring **operator ratification before implementation** (R-9a):

> **⚠️ SUPERSEDED 2026-07-30 — read § 14 OQ-3 before acting on this section.** Both questions below
> are #868's original framing, kept for provenance. Operator ratification of the governing *policy*
> reframed both as **measurement-gated, not taste-gated**, which changes what an implementer does.
> § 16 routes R-9/R-9a/R-9b here, so this banner is what carries a reader on to the resolution.

1. ~~**Reduce Transparency** — what replaces the `.regularMaterial` scrim.~~ **Reframed, conditional
   on A-9**: *if* macOS substitutes the opaque fill itself — **A-9, an unverified platform premise
   graded 🔴 (PRD § 7), to be checked at T3 before designing** — then the live question narrows to
   whether the panel's *tuned* elements survive it. **Measured**: the material is load-bearing
   (0.933727 of frame differs over black vs light, `PanelAppearanceVariantTests.swift:442`). **Not
   measured**: that the strip tints, leading rules and meter tracks were *tuned against it* — that is
   uncited, and ADR-0031 `:78` points the other way for at least one of the three (the #759
   tint-token sweep measures sRGB against an **opaque base**, never a composited frame), so this
   clause is what makes OQ-3's residual risk read larger than the evidence supports. **Ratified
   policy: accept the OS substitution, change only elements failing a stated threshold** (§ 14 OQ-3).
2. ~~**Reduce Motion** — what the `Switching…` affordance becomes (static glyph vs cross-fade).~~
   **Reframed**: the affordance is a stock `ProgressView()` with no custom animation, so the
   platform may already honour Reduce Motion and this half may close as **not-a-defect**.
   **Ratified: premise-gated — verify before designing** (§ 14 OQ-3).

Both amend a ratified aesthetic (vibrancy, operator-confirmed 2026-07-07). R-9a's ratification gate
still governs any pixel change.

## 11. Crosscutting Concepts

**Security** — no new surface. Under S-A only, the write path must honour #268's invariant that
`config-set` never touches credentials or roster structure. S-B has no security surface at all.

**Observability** — the reachability gate *is* the observability play: it converts PRD Matrix row 3
from invisible to detected. No runtime telemetry; a display preference is not an operational signal.

**Error handling** — R-4: any preference failure (missing, unreadable, out-of-range) degrades to
k=1.0, which is today's behaviour. Fail-open by construction.

### Master Test Plan (abridged — Testing Architecture is the central track here)

**Risk Surface (ACC)** — Attributes: *Reachable, Verifiable, Ratified, Locked*.

| ID | Capability | Component | Tier |
|---|---|---|---|
| Cap-1.1 | Driver injects at the construction site from a non-literal source | SizeClassDriver | T2 |
| Cap-1.2 | Gate trips when the injection is removed by mutation | ReachabilityGate | T2 |
| **Cap-1.3** | A manual text-size step exists in the Appearance-settings checklist and observes the driver **delivered** | TextSizeManualStep | **T3** |
| Cap-2.1 | Content-sized elements stay within allowance at all 12 classes — 7 distinct factors measured, the 5 clamp-aliases asserted as aliases | OverlapBudgetGate | T1 |
| Cap-2.2 | The overlap predicate is non-tautological | OverlapBudgetGate | T1 |
| Cap-3.1 | Panel fits a supported display at `.accessibility3` | ScreenFit | T3 |
| Cap-4.1 | Settings cells scale with their fonts | SettingsCellScaling | T1 |
| Cap-5.1 | Display-setting renderings are ratified and match | DisplaySettingRenderings | T3 |
| Cap-6.1 | Status item byte-identical at every class | brand lock | T1 |

**Three verification tiers** — this design's organising cut, orthogonal to ADR-0031's. ADR-0031
partitions gates by *what each is structurally blind to* about the **subject**; this table partitions
them by *where the stimulus originates*, which is what decides whether a gate can see the
**producer** at all:

| Tier | Mechanism | Can it see the producer? | Capabilities |
|---|---|---|---|
| **T1** in-process, driveable | Render harness / metrics, own stimulus | ❌ no | Cap-2.1, 2.2, 4.1, 6.1 |
| **T2** source-as-data lint | Reads excluded sources as bytes | ✅ **yes** | Cap-1.1, 1.2 |
| **T3** manual-only | Real app, real OS setting | ✅ yes | **Cap-1.3**, 3.1, 5.1 |

**T2 is not a new mechanism — it is an existing gate *kind* promoted to a first-class tier row.**
ADR-0031 already recognises it, in two separate passages of § Decision 1 (quoted apart rather than
elided together, since they are not adjacent): at `:77` it files `PanelDynamicTypeLintTests` inside
the Text-metrics row as *"a **structural source lint** … which catches a newly-added `.font(…)`"*,
and at `:107-113` it states the gap — *"the lint is not a weaker measurement — **it is a different
kind of gate**"* — because *"a tier stack organised only by what each gate measures has **no row**
for a gate that measures nothing and constrains the source instead"*. It is one of three deliberate
splits, named at `:86` (*"**gate-kind** (#757)"* — a different line from the two quotes above; cited
separately so the anchor is honest). ADR-A
supplies that row. The new ground is not the mechanism but the **axis**: T2 is the only tier that can
observe the producer, and that is why the PRD's blind state was invisible — T1 structurally cannot
see the producer, and **no T3 checklist covers the text-size axis at all**.

That last clause is deliberately *not* "T3 was never run". Running the existing T3 would have changed
nothing: the Appearance-settings checklist (`apps/menubar/design/README.md:497-516`) is the only
manual pass touching System Settings → Accessibility → Display, and its four steps are Increase
contrast, Reduce transparency, Reduce motion, and a Light/Dark repeat — **no text-size step exists**.
The Settings-window checklist routes the axis away from manual verification entirely (`:571-573`).
The distinction is load-bearing for whoever closes Matrix row 3: the remedy is to **write** that
step, not to execute one that is already there — see R-5c.

Whoever drafts ADR-A should carry this framing verbatim: claiming invention would contradict § 4.1's
own finding that the precedent is *"not analogous; it is the same mechanism on the same file"*, and
would re-open the staleness ADR-0031 § Related already warns about.

**Quality gates** — CONSTRAINT-A applies to every new gate: mutation-proven falsifiable, both
directions in one run. AI-augmented testing: **N/A — no AI/LLM in system.**

## 12. Architecture Decisions

| ADR | Decision | Status |
|---|---|---|
| **ADR-A** | Promote the source-as-data lint (T2) from a gate-kind split inside ADR-0031's text to a first-class tier row, on the producer-visibility axis | **PROPOSED** — extends ADR-0031 § Decision 1 |
| **ADR-B** | The reachability gate ships under every FORK-1 outcome, polarity selected by R-1 | **PROPOSED** |
| **ADR-C** | Driver selection (FORK-1) | **OPEN** — gated on R-1 |
| **ADR-D** | Preference storage: client-local (S-B) | **PROPOSED** — author-chosen, ratification-pending |

## 13. Quality Requirements

Bounded by `PanelTypeScale.ceiling` (`.accessibility3`), already clamped. No latency/throughput
dimension — one panel, one user. R-7 is the sole live performance-adjacent risk (geometry, not speed).

## 14. Risks and Open Questions

### Feasibility Summary (Phase 4.1)

| Component | Verdict | Basis |
|---|---|---|
| `PlatformDeliveryProbe` | **FEASIBLE** | Measurement only; both policies reachable in a running app. R-1b's adoption pre-probe additionally needs a throwaway build adopting the opt-in — cheap, but it is a build step, not just an observation |
| `ReachabilityGate` | **FEASIBLE-WITH-SPIKE** | **A-4 RESOLVED — see verdict below.** Spike is the non-literal-source predicate only |
| `OverlapBudgetGate` | FEASIBLE | #896 already identifies the two allowances and the non-tautological surface |
| `SettingsCellScaling` | **UNCERTAIN → premise-gated** | Blocked on R-1 (A-2). May be void. Not a blocker: R-8a already routes it |
| `DisplaySettingRenderings` | **UNCERTAIN → ratification-gated** | Blocked on operator (R-9a), not on technique |
| `ScreenFitVerification` | FEASIBLE | Manual render at the ceiling |
| `TextSizePreference` | FEASIBLE | Only under D-B |
| `PanelSizeClassReference` | FEASIBLE | Mock authoring is established practice |

> **A-4 FEASIBILITY VERDICT — FEASIBLE.** The concern was that a producer-side gate is impossible
> because `StatusItemController.swift` is excluded from the `MenubarTests` bundle. **The exclusion
> bars a *compiled* gate, not a *source-as-data* one.** `PanelDynamicTypeLintTests` already reads
> that exact file today — it must, in order to exempt it — via mechanical recursive discovery of
> every SwiftUI-importing `.swift` under `Sources/`, walking bytes with Foundation only, adding no
> view to the bundle. The precedent is not analogous; it is the same mechanism on the same file.
> Residual risk is the *predicate*, not the *access* — hence FEASIBLE-WITH-SPIKE, spike scoped to
> the non-literal-source clause (§ 5.2).

**No INFEASIBLE must-have components. Feasibility gate: PASS.**

### Risk Register (Phase 4.2)

| Risk | L×I | Score | Mitigation |
|---|---|---|---|
| **A-8 unprobed → the 2×2 kills D-A on an untested condition.** Its axes cannot vary opt-in adoption, so if `StatusPanelTypeScale.swift:75-79` is right, all four cells read "no delivery" | 3×4 | **12 HIGH** | **R-1b**: establish adoption FIRST, record every cell against it; circuit-breaker gated on it |
| R-1 probe measures the wrong variable → wrong driver shipped. Sharpest form: reading the declared `LSUIElement` key (`Info.plist:25-26`) returns a static `true` and reports "always an agent app" — a **false positive** that hides the runtime `.regular` promotion the 2×2 exists to test | 3×3 | **9 HIGH** | R-1a (the probe reads the *observed runtime* policy, never the plist) + the 2×2 shape + SPIKE-1's explicit cell enumeration |
| Positive gate satisfied by a semantically-dead literal injection | 2×3 | **6 MED** | § 5.2 non-literal-source clause → SPIKE-2 |
| #868 renderings decided in an implementation PR without ratification | 2×3 | **6 MED** | R-9a hard gate; items carry ratification as an AC |
| #845 implemented against a false premise | 2×3 | **6 MED** | R-8a premise-gate; sequenced after R-1 |
| Panel does not fit at the ceiling → ceiling must drop after the driver ships | 2×2 | 4 MED | R-7 verified **before** the driver lands |
| Scope creep into #832 / VoiceOver | 1×2 | 2 LOW | PRD § 1b explicit Out-of-scope |

**No unmitigated HIGH risks. Risk gate: PASS.**

**10x test**: if R-1 takes 10× longer, does the design survive? **Yes** — the gate, overlap, screen
fit, and design-reference work all proceed without it. Only #845 and the driver identity stall. Not
a rabbit hole.

### Open Questions (load-bearing — these hold `status: draft`)

- **OQ-1 — Which FORK-1 option?** Context: needs R-1's 2×2. Impact if deferred: the driver, the
  gate's polarity, FORK-2, and the Settings UX all stay unresolved. *Resolved by SPIKE-1.*
- **OQ-2 — Do Settings' fonts actually grow?** Context: A-1 vs A-2 contradiction. Impact: #845 may
  be latent, not live — implementing it would be building against a false premise. *Resolved by SPIKE-1.*
- ~~**OQ-3** — the #868 renderings.~~ **RESOLVED 2026-07-30 by operator ratification of the governing
  POLICY** (the pixel choices follow from a render, and were never ratifiable in advance).

  Reframed during ratification: **both halves are measurement-gated, not taste-gated.**
  - *Reduce Transparency* — the scrim is `.regularMaterial` (`StatusPanelView.swift:124`, verified).
    The next step — that it is `NSVisualEffectView`-backed and therefore **macOS substitutes the
    opaque fill itself** — is **A-9: an unverified platform premise**, cited to no in-tree source and
    never measured. It is the step that narrows this question, so treat it with the same discipline
    as A-8: *verify before designing*, not as settled. Two reasons it is weaker than it reads:
    `StatusItemController.swift:90` records that the app **drops `NSPopover`**, and ADR-0031
    § Decision 6 finds the renderer *ignores* system accessibility settings — which is why the panel
    goldens are machine-portable — so the substitution is **not observable through the render path
    either**. That makes this a **T3** (manual, real-app) question, and it is named as one here.
    Conditional on A-9 holding, the live question is whether the panel's **tuned** elements survive.
    **Ratified policy: accept the OS substitution; change ONLY elements that fail a stated
    threshold**, reusing the shipped WCAG contrast helper (`StatusPanelFormatTests.swift:2292` and
    its two overloads). **Route new work through `clearsBar` (`:1849-1850`)** — it is
    the predicate the `testTheContrastGateCanFail` canary drives (`:2139, :2145, :2148`, with
    `failingCells` at `:1855`), so assertions written through it inherit the CONSTRAINT-A falsifier
    § 11 requires of every new gate; assertions written around it do not.

    Two scoping caveats, because the obvious reading of `clearsBar`'s doc comment overstates it.
    (a) Its *"every assertion in **this section**"* means the `#388 tint-token CONTRAST` section
    (`:1788`–`:2194`) — and even there it is not literally every one: `:1921` (exact ratio 4.10) and
    `:1996` (relative ordering) are inside that section and do **not** run through it. (b) The four
    raw 3.0 / 4.5 sites (`:1715, :1717, :1720, :1722`) are in a *different* section entirely
    (`Account identity color`, `:1670`–`:1738`) and were never in `clearsBar`'s scope, so they are
    not "raw assertions it supersedes". The routing instruction stands on the canary chain alone. Minimal
    divergence from the ratified aesthetic; every change evidence-backed.
  - *Reduce Motion* — the affordance is a stock `ProgressView()` at six sites across both surfaces
    § 3 scopes in: panel — `StatusPanelCapture.swift:81`, `StatusPanelChrome.swift:231,398`,
    `StatusPanelRoster.swift:446`; Settings — `SettingsView.swift:149,260`
    with no custom `withAnimation` / `repeatForever` — i.e. `NSProgressIndicator`, a system control
    that plausibly already honours Reduce Motion. #868's premise ("a reduce-motion user gets the same
    spinner") may not hold. **Ratified: premise-gated — verify before designing**, the R-8a
    discipline. If the platform already handles it, #868's Reduce Motion half closes as
    not-a-defect and the issue's framing is corrected.
- **OQ-4 — Ratify S-B (client-local storage)?** Context: author-chosen on the daemon-down argument.
  Impact: only material under D-B. *Needs the operator.*

### Spikes

| ID | Question | Time box | Success criteria |
|---|---|---|---|
| **SPIKE-1** | **First** establish "preferred reading size" adoption (R-1b / A-8), **then** the 2×2: (injected-env vs OS-setting) × (`.accessory` vs `.regular`) | 1 session | Adoption state recorded; all four cells recorded with rendered evidence **against that state**; A-1/A-2/**A-8** resolved; FORK-1 decidable |
| **SPIKE-2** | Can a source-as-data predicate distinguish a variable-sourced injection from a literal? | 0.5 session | A predicate that passes the real injection and fails `.dynamicTypeSize(.large)` |

## 15. Glossary

| Canonical | Definition |
|---|---|
| **Reachability** | Whether a real user can cause an affordance's input to change. Distinct from correctness |
| **Producer / consumer** | Producer supplies the stimulus (OS or preference); consumer reads and applies it. #756 built the consumer |
| **T1/T2/T3** | Verification tiers (§ 11) — in-process-driveable / source-as-data / manual-only |
| **Defect pin** | A test green while a defect stands, red when fixed. Established in-tree |
| **Blind state** | PRD Matrix row 3: built + gated + unreachable, undetected |

## 16. Requirement-to-Track Coverage Matrix (forward)

| Requirement | Track(s) | § | Capability | Status |
|---|---|---|---|---|
| R-1, R-1a, R-1b | Testing Arch | § 14 SPIKE-1 | — (spike) | covered |
| R-2, R-4 | Technical Arch | § 5.1, § 6 | Cap-1.1 | covered |
| R-3 | Technical Arch | § 5.1 (D-C live) | — | covered |
| R-5, R-5a, R-5b | Testing Arch | § 5.2, § 11 | Cap-1.1, Cap-1.2 | covered |
| **R-5c** | Testing Arch | § 11 (gap + T3 tier) | **Cap-1.3** | covered |
| R-6, R-6a | Testing Arch | § 11 | Cap-2.1, Cap-2.2 | covered |
| R-7 | Testing Arch | § 11 T3 | Cap-3.1 | covered |
| R-8, R-8a | Technical Arch | § 5.4 | Cap-4.1 | covered |
| R-9, R-9a, R-9b | UI/Visual | § 10, § 11 T3 | Cap-5.1 | covered |
| R-10 | Technical Arch, API | § 5.3, § 8 | — | covered |
| R-11 | Testing Arch | § 11 | Cap-6.1 | covered |
| R-12 | UI/Visual, UX | § 9, § 10 | — | covered |

**No UNCOVERED entries. Forward gate: PASS.**

## 16b. Element-to-Requirement Backward-Coverage Matrix

| Element | Type | Traces to | Status |
|---|---|---|---|
| `SizeClassDriver` | component | R-2, R-4 | traced |
| `ReachabilityGate` | component | R-5 | traced |
| `OverlapBudgetGate` | component | R-6 | traced |
| `SettingsCellScaling` | component | R-8 | traced |
| `DisplaySettingRenderings` | design | R-9 | traced |
| `ScreenFitVerification` | verification | R-7 | traced |
| `TextSizePreference` | component | R-10 | traced |
| `PanelSizeClassReference` | design | R-12 | traced |
| `PlatformDeliveryProbe` | spike | R-1, R-1b | traced |
| **T2 verification tier** | architecture | promotion | **surfaced** — ADR-A promotes ADR-0031's already-named source-lint gate-kind to a tier row on the producer-visibility axis; surfaced, not absorbed, and not claimed as invention. ADR-A is **PROPOSED** (§ 12), so this is not ratified |
| **Non-literal-source clause** | predicate | R-5b | traced (hardening) |
| `TextSizeManualStep` | verification | **R-5c** | traced (T3 half of row-3 closure) |

**No PHANTOM entries. Backward gate: PASS.**

## Design Lock Gate

### Write-back rule (the condition for lifting `draft`)

Resolutions live in registers (§ 14's open-question list and risk table, PRD § 7's assumption
registry); content lives in the numbered body sections. **Nothing else in this document requires a
closed register entry to be written back into the body, or a new PRD requirement to be written
forward into the coverage matrices** — and all three leaks have already happened: OQ-3 closed
while § 10 kept restating the questions it refuted; R-1b was added while § 5.4 / § 14 / § 16 kept
asserting PASS over a set that omitted it; and **A-4 was resolved FEASIBLE here in § 14 while PRD
§ 5b, § 7 and R-5b all still said "Stage 2 owes a verdict"** — caught only by external review, and
the leak that proves the direction matters. The first two leaked *design → design* and
*PRD → design*; A-4 leaked **design → PRD**, the direction with no row in the table below, and the
worst one to lose: the PRD is the artifact the tracker issues cite by path, so a stale PRD is what
an executor actually reads.

So this gate does not merely *count* open questions. Lifting `draft` additionally requires, for each
question that closed:

| When this closes | It must be written back into |
|---|---|
| **OQ-1 / OQ-2** (SPIKE-1) | § 5.1 FORK-1 table incl. the **Kills it** column · § 5.2 gate polarity · § 5.4 · § 8 · § 9 · § 13 · § 16 · PRD § 7 A-1/A-2/A-8 · PRD § 5b · **and `docs/specs/`** — `accessibility-reachability-gate`, `dynamic-type-driver` and `settings-cell-scaling` each carry an OQ-1/OQ-2-dependent 🟥 open item and an Example Mapping count |
| **OQ-4** (storage ratification) | § 5.3 · § 7 Deployment View's S-B conditional · § 12 ADR-D status · § 16 |
| **any new requirement** | PRD § 4 AC · § 5b · § 8 · § 10 · design § 5.4 · § 14 · § 16 · § 16b · the owning tracker issue |
| **any assumption THIS design resolves** | PRD § 7's registry row (strike it, don't just recolour) · PRD § 5b's gap cell · **the requirement whose prose says the design "owes" the verdict** · design § 14 Feasibility Summary. Assumptions live in the *PRD's* register but are discharged *here*, so this row is the only one that writes **backwards** — the design cannot close an assumption by recording it locally |
| **any corrected identifier or count** | every doc site **and** every filed issue — `git grep` the literal old token *and* sweep `gh issue view` for the whole cluster |

**NOT LOCKED.** Both coverage gates pass, but three load-bearing open questions remain (§ 14 —
OQ-1, OQ-2, OQ-4; OQ-3 closed 2026-07-30 by operator ratification), so per
the Open-Questions Lock Gate the brief stays `status: draft`. Dual-lens ratification (product + UX)
is deferred until SPIKE-1 resolves OQ-1/OQ-2 — ratifying a design whose central fork is unresolved
would be a false lock.
