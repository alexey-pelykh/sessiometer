# Solution Design: Panel & Brand Presentation Reference Coverage

**Source PRD**: `docs/requirements/panel-presentation-reference-coverage.md` (`dor_status: passed-with-findings`)
**Date**: 2026-07-30 · **Status**: `draft` — see § 17 for the two reasons this is not locked.

## 1. Goals and Drivers

Close six live presentation defects across the menu-bar panel and the macOS app icon, and close the
mechanism that let all six ship invisibly: coverage is asserted per element against whatever the build
reference happens to author, never against the set of axes an element needs decided.

> **Status of the six, as of 2026-07-31** — this document was authored 2026-07-30 and **three of the six
> have since shipped**; it is recorded here rather than rewritten, so the design reads as the decision
> record it was. **Delivered**: R-1/R-10 expiry gutter (#951, `c5f851d`) · R-2/R-2a app-icon grid (#952,
> `12ee1c4`) · R-3a/R-3b chip resting contrast (#949 measured, #956 fixed, `0ab82fc` — see § 4.4).
> **Still live**: R-4/R-11 row affordance (#955, blocked on spike #950) · R-5 tooltip scope (#953) ·
> R-6 the `.help()` spike (#950). The **durable half is untouched** and is the part that matters most:
> R-7 reference upkeep (#957), R-8 axis disposition (#954). R-9 is CUT (§ 13). Instance fixes landing
> does **not** discharge the mechanism — R-8 is precisely what stops a seventh instance.

Driver: three independent surfaces now carry the same root cause (this panel, the app icon, and the
Settings window per `83a275d`/#763). Instance-patching predicts a fourth.

## 2. Constraints

- **Appetite 1 week** for the non-decision-gated half; the decision-gated items are unsized (PRD § 1b).
- **macOS deployment target 13.0** (`apps/menubar/project.yml:11-12`). Anything macOS-26-only is additive,
  never a fix.
- **`/scope` produces planning artifacts only** — this document specifies; it does not implement.
- **No new CI job** unless an existing enforced moment cannot carry the check (`ci-ok.needs` coupling).
- **macOS-only support** — portability is not a design axis here (repo `CLAUDE.md`).

## 3. Context and Scope

Three surfaces, one bounded context (the presentation layer of the menu-bar app plus the brand pipeline
that feeds it):

```
brand/src/icon.svg ──┬─→ generate.sh → AppIcon.appiconset/*.png → actool → Assets.car → macOS app icon
                     ├─→ derive() → _ih/_iw/_ic/_is.svg  (status colour variants)
                     └─→ apple-touch-icon.png → sessiometer.github.io       ← MUST STAY FULL-BLEED

design/menubar-preview.html ──(sliced LIVE by build-comparison.py)──→ render-parity report [SOFT]
        │
        └─(oracle for)─→ apps/menubar/Sources/StatusPanel*.swift ──→ panel render
                                        │
                          (parity)──────┴──→ src/cli.rs render_status  [ACCOUNT SESSION% RESET WEEKLY% RESET EXPIRY AUTH]
```

**Out of scope**: the daemon, the wire schemas, credentials, Settings window, Dynamic Type, Reduce
Transparency, localization (verified absent), meter tints (#830/#831).

## 4. Solution Strategy

Four decisions carry the design. Three are settled on evidence; one is deliberately left open.

### 4.1 Expiry value placement — **merge the percent+reset cells into one right-aligned value cell**

`UsageMeter` pins `52 / flex / 40 .trailing / 52 .trailing`. `ExpiryLine` shares only the 52 pt label.

| Option | Verdict |
|---|---|
| (a) **Merge the 40 + 9 + 52 = 101 pt tail into one right-aligned cell** | **CHOSEN** |
| (b) Give expiry its own 52 pt cell, leaving the percent slot present-but-empty | Rejected — PRD R-10 forbids the permanent empty gap; and 52 pt is tight for the bracketed form |
| (c) Move the credential line into a visually separate block below the meters | Rejected for this appetite — a larger IA change, and it abandons the alignment the CLI already models |

**Discriminating evidence**: the CLI treats `EXPIRY` as its own column *after* `RESET`
(`ACCOUNT SESSION% RESET WEEKLY% RESET EXPIRY AUTH`), so right-of-reset is the parity-correct home. And
`expiryLineCell` returns a **variable-width** form — `[cell]` when within the warning horizon (#934/#935)
— so the cell must absorb two extra characters without clipping. 101 pt right-aligned satisfies both;
52 pt does not. Reversibility: **two-way door** (a layout constant).

### 4.2 Icon grid — **inset at emit time, app-icon output only**

| Option | Verdict |
|---|---|
| (a) Inset inside `brand/src/icon.svg` | **REJECTED — would break two other consumers** |
| (b) **Add a padding stage in `generate.sh`, scoped to the AppIcon PNG set** | **CHOSEN** |
| (c) Ship an Icon Composer `.icon` asset and let macOS inset it | **REJECTED as a fix** — macOS 26+ only vs a 13.0 target; it would leave every macOS 13–25 user on the full-bleed raster. Viable later as *additive* polish only. |

**This is the design stage earning its keep.** `icon.svg` is a **shared master with four consumers**, only
one of which wants the inset:

| Consumer | Line | Needs inset? |
|---|---|---|
| `AppIcon.appiconset/icon_${s}.png` | `generate.sh:188` | **YES** |
| `derive()` → `_ih/_iw/_ic/_is.svg` status variants | `generate.sh:237-240` | No |
| `apple-touch-icon.png` → `sessiometer.github.io` | `generate.sh:266` | **No — Apple touch icons are full-bleed by convention; the OS masks them** |
| `favicon.svg` (separate source) | `generate.sh:264` | n/a |

Option (a) — the obvious single-source edit — would have silently degraded the website's touch icon and
all four derived status variants. **Mechanism for (b) needs no new tooling**: `generate.sh` already
transforms SVG textually via `derive()` (`sed`), so the padding stage emits a wrapper SVG embedding the
mark at the grid fraction and rasterises that. The baked `rx="229"` radius must also be dropped from the
app-icon path — the system applies its own mask, and stacking the two is what produces the over-rounded
"circular" read.

### 4.3 Axis-disposition mechanism — ~~a trailer gate in `gate-change-ack`~~ → **a scope-time DoR question**

> **AMENDED 2026-07-30 — option (c) below was CUT by necropsy; see § 18.1.** The trailer gate's trigger (a
> new `struct … : View`) catches **1 of 5** defects across **1 of 3** surfaces and misses `authView`
> (`StatusPanelRoster.swift:477`) — a computed property carrying 2 of the 6 findings. Declaration shapes in
> that file run **40 computed vars to 32 structs**, so the trigger keys on the minority idiom. The
> maintainer chose the replacement: **ask the five axes at readiness time**, in the work item's DoR — which
> is earlier, cheaper, and reaches all three surfaces because it is asked of the *item*, not of the *diff*.
> The option table below is retained as the decision record; read (c) as **cut**, not chosen.

| Option | Verdict |
|---|---|
| (a) Attach to `project.yml` / Xcode wiring | Rejected — a build-wiring moment, not a design moment |
| (b) Make `build-comparison.py` fail on an element absent from the mock | **Rejected — but the original reason given here was wrong.** It said "structurally cannot enforce" on the softness of `panel-goldens` (correctly: **5** steps, **5** `continue-on-error` markers — `ci.yml:233,235,238,243,270`; `:396` says its conclusion "is always success and it cannot block"). That softness is one config change away from reversible, so it is not a structural argument. The **real** reason is categorical: `build-comparison.py` is a **drift** detector — it pairs capture-to-mock **by name** (#581) and reports differences. A drift gate cannot see an axis that was **never covered on either side**; a missing element is absent from both, so there is nothing to diff. Keep it as the *visibility* companion it is. |
| (c) **A `check-*.sh` + trailer, hosted in the existing `gate-change-ack` job** | **CHOSEN** |
| (d) Require an ADR per new panel element | Rejected — heavier than the appetite, and ADRs are for decisions, not for a coverage checklist |

**Discriminating evidence — the repo's own precedent, three instances deep**: `scripts/` carries **7
`check-*.sh` gates, 5 with paired `.test.sh` files**, and `ci.yml:349-380` already hosts *three* trailer
checks in one `gate-change-ack` job (`Gate-Change-Acknowledged:`, `Panel-Goldens-Rebaselined:`,
`CLI-Goldens-Rebaselined:`). Adding a fourth follows the pattern exactly, needs **no new job**, and
`ci-ok.needs` already covers it.

**Trigger must be narrow, or the gate becomes ceremony.** Firing on any `StatusPanel*.swift` path change
would demand the trailer on nearly every panel PR → reflexive trailers → a field that gates nothing
(the "Ceremony status-field" failure). Fire instead on the diff **adding a `struct … : View`** in
`apps/menubar/Sources/StatusPanel*.swift` — a genuinely-new-element signal that stays rare.

> **Stated limit, not overclaimed**: this heuristic misses an element introduced as a computed property or
> a `@ViewBuilder` func rather than a struct. It is a **net visibility improvement, not a proof of
> coverage.** Recording that here so no reader treats a green gate as an axis-coverage guarantee.

Recursive detail: adding a script under `scripts/**` itself requires a `Gate-Change-Acknowledged:` trailer.

### 4.4 Chip affordance channel — **RESOLVED 2026-07-30 to option (d)**

> **This fork is closed. Recorded after the fact; the prose below is preserved as authored.** The block
> was "the discriminating evidence does not exist yet" — it exists now. Issue **#949** measured the
> as-shipped resting chip on a **built** panel: **1.91:1 light / 2.70:1 dark**, with 0 of 243 light and
> 0 of 433 dark chip pixels clearing the WCAG 1.4.11 3:1 floor. Armed measured 4.73:1 and needed no
> change. Issue **#956** then shipped **option (d)** — `.resting` moved to a `SwapChipResting` colour
> set (#828282 light / #808080 dark plus the Increase-Contrast pair), measuring **3.34:1 in both
> appearances**, landed on `main` as `0ab82fc` (PR #995). `.armed` deliberately did **not** move: the
> shipped rest→armed step is 1.42×.
>
> Two things worth carrying forward, because neither is visible from the option table alone:
> **(1)** the reason no gate caught this is *structural*, not an oversight — `HierarchicalShapeStyle
> .resolve(in:)` returns `Never` and the macOS 13 deployment target has no `resolve` API, so no test
> could ever read `.tertiary`'s value; the pre-existing gate could only assert the *relation*
> `armed > resting` plus a magnitude floor, both of which a below-floor resting state satisfies. That
> is the same shape as this scope's root cause, one layer down.
> **(2)** option (d) beat (a) on the axis the table only half-states: an asset token keeps the
> Increase-Contrast escalation (#832) *and* is numerically readable in tests today.

Three live options were framed, plus (d) added late. **This design did not converge at authoring time**,
because the discriminating evidence did not exist yet (PRD R-3a: the as-shipped resting contrast was
unmeasured; the 2.10:1 figure is the *mock's* token, and the codebase claimed only "≈" between them).

| Option | Gains | Costs |
|---|---|---|
| (a) Pin explicit `FillRGBA` tokens for resting/armed | Measurable; assertable ≥ 3.0 with the existing #445 helper | Loses system adaptation (Increase Contrast, appearance, future macOS tint changes) |
| (b) **Keep system tints; carry the at-rest signal on a non-colour channel** (border, outline, symbol weight) whose contrast *is* pinnable | Satisfies WCAG 1.4.11 through a measurable channel **without** giving up system adaptation | A visual change to the chip; needs mock authoring |
| (c) Keep system tints, document a knowing WCAG deviation | Zero change | A deviation on the one at-rest indicator that a row is actionable |
| **(d) `PanelTint.asset(String)` colorsets** ⟵ *added 2026-07-30* | Numerically readable in tests **today** (`assetRGB`, `StatusPanelFormatTests.swift:2202-2209`) **and** OS-driven light/dark **and** declared high-contrast variants — i.e. (a)'s measurability without (a)'s cost | A mock amendment; one more colorset pair |

> **AMENDED 2026-07-30 — the option set as framed was partly unsound; see § 18.2.**
>
> - **(a) was never "instrument the same visual."** `HierarchicalShapeStyle.resolve(in:)` returns
>   **`Never`**, so `.tertiary` is unresolvable *by construction on every macOS version*, and the
>   deployment target (macOS 13.0) has no `resolve` API at all. Any pinning is a **redesign**. Compare
>   `Color.red.resolve(in:)`, which yields readable components.
> - **(b) is dead as stated.** `nosign` — the blocked-state glyph in the same slot — **is already a ring**,
>   so a hairline ring / capsule collapses actionable-vs-blocked into ring-vs-ring and makes the newly-found
>   confusability defect worse. The leading-edge inset rule is also occupied: it carries fault severity
>   (`menubar-preview.html:279,287`).
> - **(d) is the live answer**, and it is the mechanism the repo already reached for: `PanelTint.asset`
>   (`StatusPanelFormat.swift:671`) exists precisely because "a raw system `Color` fails WCAG
>   non-text/text contrast on the translucent vibrancy — system yellow ≈ 1.2:1 there" (`:667`). The chip is
>   the **last** glyph in its slot still on a raw system tint; its neighbour is already
>   `.asset("HealthOK")`. Per **#832**, AppKit selects the high-contrast variant from the **system**
>   Increase-Contrast setting, so the adaptation is real in the live app — only the *test seam* cannot
>   reach it. Watch for the stale comment at `StatusPanelFormat.swift:1106` ("`MenubarTests` compiles no
>   `.xcassets`") — true when authored (#388), **falsified by #525**; `project.yml:184` puts
>   `Assets.xcassets` in the test target today.
> - **Measured, so the fork is narrower than it looks**: resting is **1.93–2.06:1**, **0 of 70 pixels**
>   clear 3:1. And usage context cuts the stakes — the maintainer uses the chip **rarely** and the footer
>   Swap **never**; the CLI is the real swap path. This is a low-priority fix, not a design programme.

**Signpost that reopens this**: R-3a's measurement lands → choose. **Open ADR stub emitted** (§ 12,
ADR-STUB-1), Context + Options recorded, Decision left OPEN.

## 5. Building Blocks

| Component | Change | Track |
|---|---|---|
| `StatusPanelRoster.ExpiryLine` | Merged 101 pt right-aligned value cell | UX/IA + UI |
| `StatusPanelFormat` | A merged-tail cell-width constant, derived from the existing 40/9/52 constants rather than a fourth magic number | UI |
| `brand/generate.sh` | App-icon-only padding stage + radius drop | Technical Arch |
| `StatusPanelRoster.authView` | Hover treatment + `.help()` (shape gated on 4.4 and R-6) | UX/IA |
| `StatusPanelRoster` row branches | Chip-scoped tooltip + a row-level fallback (R-11) | UX/IA |
| `design/menubar-preview.html` | New frames: expiry line, auth-glyph affordance, active-row hover | Build reference |
| `scripts/check-panel-element-axes.sh` + `.test.sh` | New trailer gate | Testing Arch |
| `Tests/StatusPanelFormatTests.swift` | Absolute contrast assertion for the chip (post-4.4) | Testing Arch |

## 6. Runtime View — Task Flows

**Flow: user decides whether a row is actionable.** Today: the only at-rest signal is the chip's tint,
whose contrast is ~~unmeasured and probably ~2.1~~ **measured at 1.91:1 light / 2.70:1 dark (#949) and
since lifted to 3.34:1 both appearances (#956, `0ab82fc`)**. Hovering produces a real response (+57.9 % chip ink,
−7.50 lum wash, ~10× the control) — but hover is *discovery-by-accident*. Five of ten row states have no
tooltip at all; six have no hover response (PRD § 7). Target: every state has at least one at-rest signal
and one hover response.

**Flow: user reads a credential expiry.** Today the value sits in the bar column, so the eye scanning the
right-hand value gutter skips it. Target: right-aligned in the merged tail, bracket intact.

## 7. Deployment View

N/A — no deployment change. One build-pipeline consequence: `generate.sh` output feeds `actool` →
`Assets.car`. Per `symbolset-fills-never-strokes`, `actool` reinterpretation is only catchable on-device,
so verification must measure the **emitted raster**, never the SVG source.

## 8. Interface Contracts

No wire change. `STATUS_SCHEMA_VERSION`, `JSON_SCHEMA_VERSION` (×3), and `FORMAT_VERSION` are all
untouched — this design is presentation-only. The one contract in play is the **CLI↔panel column parity**
(informal, unenforced): 4.1 conforms to it.

## 9. UX Architecture

**9a. Information architecture.** The account row is a two-tier object: identity tier (email, status
glyph, swap affordance) and fact tier (session meter, weekly meter, credential expiry). The defect in 4.1
is a tier-boundary error — a *fact-tier* row borrowed the label cell from the meters but not their value
grid. The correct IA statement: **all fact-tier values share the right-hand value gutter**, regardless of
whether the fact has a bar.

**9b. Affordance coverage matrix** — the artifact that makes the root cause visible. PRD § 7 holds the
current state (5 states with no tooltip, 6 with no hover). Target: no empty cells, or an explicit
"deliberately none" with rationale.

## 10. UI Strategy

Tokens over ad-hoc values. The chip is the live question (4.4). One firm constraint either way:
**raising the resting contrast toward 3.0 compresses the rest→armed step the existing gate measures**
(armed is 4.53:1). Both ends must move together, or the fix trades a contrast defect for a lost
affordance. Target: resting ≥ 3.0 with armed retaining a ≥ 1.3× further step.

## 11. Crosscutting Concepts

**Security** — N/A. No credential material, no transport, no authz surface.

**Observability** — the gates *are* the observability, and their weakness is the finding.
`PanelInteractionStateTests` asserts a relation (`armed > resting`) and a floor (`chipOnlyFloor = 0.001`
at `4/255`), never an absolute. `panel-goldens` is soft by construction. No runtime telemetry exists for
panel presentation, and none is proposed.

**Error handling** — N/A; no new failure paths.

**Accessibility** — the spine of this design. WCAG **1.4.11** (3:1 non-text) is 4.4's normative basis;
**AA 4.5:1** already has 13 assertion sites for the palette (#445). The auth glyph is
`.accessibilityHidden(true)` in every variant — worth noting that R-4's fix should consider whether the
glyph's *meaning* is exposed to assistive tech at all, not only whether it has a tooltip.

### Master Test Plan

**1. Goals & Risk Surface (ACC)**

| ID | Capability | Attribute | Coverage note (added 2026-07-30) |
|---|---|---|---|
| Cap-1.1 | Fact-tier values align in the right-hand gutter | Correctness | |
| Cap-1.2 | The `[…]` within-horizon bracket renders unclipped | Correctness | |
| Cap-1.3 | CLI↔panel column parity holds | Consistency | **⚠ over-claimed — see § 18.3.** Only *state* parity is gated (`StatusPanelFormatTests.swift:1440,1454` — `rosterShowsExpiry` mirrors `status_columns`). Column **order/placement** is asserted **nowhere**, which is why this defect shipped green. |
| Cap-2.1 | Interactive indicators meet absolute non-text contrast | Accessibility | |
| Cap-2.2 | A perceptible rest→armed step survives the contrast fix | Accessibility | |
| Cap-3.1 | ~~Every row state has a hover response and a tooltip~~ → **a blocked row states its reason in persistent text** | Accessibility | **Restated — the original was the wrong metric** (§ 18.4). Six of ten states are *correctly* empty; counting missing tooltips would manufacture them. Exactly one state (blocked) needs coverage, and hover is the wrong channel for it. |
| Cap-3.2 | Tooltips are scoped to the element they describe | Correctness | |
| Cap-4.1 | Emitted app-icon rasters conform to the platform grid at every size | Platform fit | |
| Cap-4.2 | Non-app-icon `icon.svg` consumers are unchanged | Non-regression | **⚠ not CI-verifiable — see § 18.3.** `brand/**` matches **no** path filter in `ci.yml`; an edit there runs zero jobs. Reframe as a *static mechanism assertion*. Conversely **Cap-4.1 is** testable: the AppIcon PNGs are committed (11 files) and the `swift` job gates on `apps/menubar/**`. |
| Cap-5.1 | A newly authored panel element surfaces its un-dispositioned axes | Maintainability | **Mechanism changed** (§ 18.1): a scope-time DoR question, not a CI trailer gate. The capability stands; its detector was cut by necropsy. |
| Cap-6.1 | ~~A mock amendment is acknowledged as a fleet re-baseline~~ | Maintainability | **CUT with R-9** (§ 18.1) — `scripts/check-panel-golden-rebaseline.sh` already meets this hazard for committed goldens (#754). |

**2. Pyramid** — unit/format-level dominates (contrast math, cell-width arithmetic, bracket formatting are
all pure); render-comparison mid-tier (`RenderPanelTool` captures); zero E2E. Cap-4.1/4.2 are
pipeline-output assertions, not app tests.

**3. gTAA layers** — reuse in place: the #445 contrast helper (has a raw-sRGB init for composited results),
`PanelRenderHarness`, `build-comparison.py`. **No new framework.**

**4. Environment** — `xcodebuild test -destination 'platform=macOS' CODE_SIGNING_ALLOWED=NO`; icon
assertions run headless against emitted PNGs.

**5. Test data** — existing `Fixtures.swift` roster fixtures; a within-horizon expiry fixture is needed for
Cap-1.2 (verify one exists before assuming).

**6. Traceability** — § 16 / § 16b below.

**7. Quality gates** — Cap-1.x, 2.x, 3.x → the `swift` job (hard). Cap-4.x → a new assertion; host it in an
existing job, not a new one. Cap-5.1, 6.1 → the `gate-change-ack` job (hard).
**Explicitly noted**: Cap-2.1 cannot be asserted at all under option 4.4(c) — that is the cost of (c),
and it must be stated in the ADR rather than discovered later.

**8. Standards** — Swift XCTest; shell gates get a paired `.test.sh` per the 5 existing precedents.

**9. AI-augmented testing** — N/A, no AI/LLM in system.

**10. Risks** — § 14.

## 12. Architecture Decisions

| ID | Decision | Status |
|---|---|---|
| ADR-1 | Fact-tier values share the right-hand value gutter; expiry uses the merged 101 pt tail | proposed (4.1) |
| ADR-2 | The app-icon grid inset is applied at emit time, scoped to the AppIcon output; `icon.svg` stays a full-bleed shared master | proposed (4.2) |
| ADR-3 | Icon Composer `.icon` is not a fix at deployment target 13.0; additive only | proposed (4.2) |
| ADR-4 | Axis disposition is enforced by a trailer gate in the existing `gate-change-ack` job, triggered on a new `struct … : View` in the panel sources | proposed (4.3) |
| **ADR-STUB-1** | **Chip affordance channel: pinned tokens vs non-colour channel vs documented deviation** | ✅ **DECIDED 2026-07-30 → option (d)**, asset colour set. ~~OPEN — blocked on R-3a~~; R-3a was answered by #949 and the decision shipped in #956 (`0ab82fc`). See § 4.4. |

## 13. Quality Requirements

Inherited from PRD § 5 unchanged: `RestingAffordanceContrast` (MUST ≥ 3.0), `AxisCoverage` (new elements
5/5 dispositioned), `IconGridConformance` (within tolerance at every emitted size).

## 14. Risks and Open Questions

### Feasibility Summary (Phase 4.1)

| Component | Verdict | Note |
|---|---|---|
| Merged expiry value cell | **FEASIBLE** | Precedent: `UsageMeter`'s grid already does this |
| App-icon padding stage | **FEASIBLE** | No new tooling — `derive()` already transforms SVG via `sed` |
| Chip contrast remediation | **FEASIBLE-WITH-SPIKE** | Two spikes: R-3a (measure shipped contrast), A5 (Increase-Contrast / appearance safety of pinned tokens) |
| Auth-glyph affordance | **FEASIBLE** | Feasibility is not the blocker; *desirability* is — gated on a decision, not a spike |
| `.help()` on disabled Button | **spike itself** (R-6) | One live hover probe |
| Axes trailer gate | **FEASIBLE** | 5 tested `check-*.sh` precedents |
| Mock frame authoring | **FEASIBLE** | Depends on 4.1/4.4 decisions landing first |

No Must-Have component is UNCERTAIN or INFEASIBLE. **Feasibility gate: PASS.**

### Risk Register (Phase 4.2)

| ID | Risk | L×I | Priority | Mitigation |
|---|---|---|---|---|
| RR-1 | Icon inset breaks `apple-touch-icon.png` and the 4 derived status variants | 3×3 = **9** | **HIGH** | **Mitigated by design**: ADR-2 scopes the inset to the AppIcon output only. Cap-4.2 asserts the non-regression. This risk is exactly what killed option 4.2(a). |
| RR-2 | Mock amendment silently re-baselines every panel frame, letting an unrelated regression ride in | 2×3 = 6 | MEDIUM | Isolate mock edits in their own commit + `Panel-Goldens-Rebaselined:` trailer (PRD R-9) |
| RR-3 | Pinned chip tokens break Increase Contrast / appearance adaptation | 2×3 = 6 | MEDIUM | Spike A5 before committing to 4.4(a); option 4.4(b) sidesteps it |
| RR-4 | The axes gate degenerates into a reflexive trailer | 3×2 = 6 | MEDIUM | Narrow trigger to a new `struct … : View`; residual blind spot stated in 4.3 |
| RR-5 | ~~Raising resting contrast flattens the hover step~~ **RETIRED — the premise was false** (§ 18.2) | ~~2×2 = 4~~ | ~~MEDIUM~~ | Resting at 3.0 vs armed 4.53 is a **1.51×** step, already clearing the ≥1.3× floor with armed unmoved; and `RowSwitchButtonStyle.wash` is a second, independently-gated channel at ΔL\* **+3.21**. The affordance cannot vanish from a chip-token change. |
| RR-6 | The bracketed expiry form overflows the merged cell at a long duration | 2×2 = 4 | MEDIUM | Cap-1.2 fixture must include a within-horizon *and* long-duration case |

No unmitigated HIGH risks. **Risk gate: PASS.**

**Rabbit hole (10× test)**: 4.4 is the only component whose blow-up could kill the design — if pinning is
unsafe *and* system tints measure ~2.1, options (a) and (c) are both bad. Option (b) is the named escape
hatch, which is why it must not be dropped from the option set.

### Open Questions

- ~~**Load-bearing** — ADR-STUB-1 (4.4): which chip affordance channel?~~ ✅ **CLOSED 2026-07-30.**
  R-3a's measurement (#949) resolved it exactly as this line predicted; option (d) shipped in #956
  (`0ab82fc`). The architectural tension it named — system-adaptive vs pinned panel colours — was
  dissolved rather than traded: an asset colour set is *both*.
- **Load-bearing** — R-6: does `.help()` surface on a disabled Button? Determines whether R-4 grows a
  blocked-row branch. Resolved by one live probe.
- **Load-bearing** — A1/R-2a: what is Apple's published app-icon grid? Determines ADR-2's inset value.
  Resolved by reading Apple's HIG.
- **Non-load-bearing** — does a within-horizon long-duration expiry fixture already exist? A test-data
  detail; discoverable during implementation.

## 15. Glossary

| Canonical | Definition | UX/IA | UI | Testing |
|---|---|---|---|---|
| Fact tier | The per-account row region carrying measured facts (meters + expiry) | fact tier | — | Cap-1.1 |
| Value gutter | The right-hand region where all fact-tier values right-align | value gutter | merged 101 pt tail | Cap-1.1 |
| Swap chip | The per-row affordance indicating the row is a switch target | swap chip | `.tertiary`/`.secondary` or pinned token | Cap-2.1 |
| Presentation axis | One decidable dimension of an element (placement · resting contrast · hover · tooltip · a11y) | axis | — | Cap-5.1 |
| Build reference | The oracle authoring an axis's target value | reference / oracle | mock | Cap-6.1 |
| App-icon grid | The platform-expected inset fraction of the icon body within its canvas | — | grid | Cap-4.1 |

## 16. Requirement-to-Track Coverage Matrix (forward)

| PRD Req | Track(s) | § | ACC | Status |
|---|---|---|---|---|
| R-1 | UX/IA, UI | 4.1, 9a | Cap-1.1, Cap-1.3 | covered |
| R-2 | Technical Arch | 4.2 | Cap-4.1, Cap-4.2 | covered |
| R-2a | Technical Arch | 4.2, 14-OQ | Cap-4.1 | covered (spike) |
| R-3a | Testing Arch | 4.4, 14 | Cap-2.1 | covered (spike) |
| R-3b | UI, Testing Arch | 4.4, 10, 12 | Cap-2.1, Cap-2.2 | covered — **decision OPEN** |
| R-4 | UX/IA | 6, 9b | Cap-3.1 | covered |
| R-5 | UX/IA | 5, 6 | Cap-3.2 | covered |
| R-6 | Testing Arch | 14-OQ | Cap-3.1 | covered (spike) |
| R-7 | UX/IA (build reference) | 5, 9b | Cap-6.1 | covered |
| R-8 | Testing Arch | 4.3 | Cap-5.1 | covered |
| R-9 | Testing Arch | 11, 14 RR-2 | Cap-6.1 | covered |
| R-10 | UX/IA, UI | 4.1 | Cap-1.2 | covered |
| R-11 | UX/IA | 5, 6 | Cap-3.1 | covered |

**Zero UNCOVERED.** All 13 requirements map to ≥ 1 executed track and ≥ 1 ACC capability.

## 16b. Element-to-Requirement Backward-Coverage Matrix

| Design element | Type | Traces to | Status |
|---|---|---|---|
| Merged 101 pt value cell | Screen element | R-1, R-10 | traced |
| Fact-tier / value-gutter IA rule | IA rule | R-1 | traced |
| App-icon padding stage (`generate.sh`) | Pipeline element | R-2 | traced |
| Radius drop on the app-icon path | Pipeline element | R-2 | traced |
| Chip token option set (a/b/c) | Option set — not yet committed | R-3b | traced, **decision open** |
| Non-colour affordance channel | Option (4.4b) | R-3b | traced as an option, not an element |
| Auth-glyph hover + `.help()` | Screen element | R-4 | traced |
| Chip-scoped tooltip | Screen element | R-5 | traced |
| Row-level tooltip fallback | Screen element | R-11 | traced |
| New mock frames (expiry, affordance, active row) | Build reference | R-7 | traced |
| `check-panel-element-axes.sh` + `.test.sh` | Gate | R-8 | traced |
| `Panel-Element-Axes:` trailer | Gate artifact | R-8 | traced |
| Absolute chip contrast assertion | Test | R-3b | traced |
| Cap-4.2 non-regression assertion | Test | R-2 | traced |

**Zero PHANTOM.** No element traces to nothing; the one un-committed item (4.4b) is recorded as an
*option* within an open ADR, not smuggled in as a decided element.

## 17. Why this design is `draft`, not locked

Two independent reasons were given. **As of 2026-07-31 the first is discharged and the second is not,
so the document stays `draft` on reason 2 alone** — one of two reasons lapsing does not lift the gate.

1. ~~**A load-bearing Open Question remains** — ADR-STUB-1 (§ 4.4) is unresolved and affects an
   architectural choice.~~ ✅ **DISCHARGED 2026-07-30**: #949 supplied the measurement, #956 shipped
   option (d) (`0ab82fc`). The prediction in this line held — it did re-open at implementation time,
   which is why it was right not to mark it final.
2. ~~**The dual-lens ratification gate has NOT been run.**~~ **RUN 2026-07-30 — and it did not pass.**
   A `/council` dispatched both required lenses (a product lens and a UX lens) plus three others. Both
   returned **blocking** findings against this document, listed in § 18. So the reason this design stays
   `draft` changed from **procedural** (the gate was never run) to **substantive** (the gate ran and
   surfaced defects). One of the product lens's own HIGH-confidence claims — that the auth glyphs'
   `.accessibilityHidden(true)` was an accessibility gap — was **falsified** on inspection
   (`StatusPanelRoster.swift:283-287`); recorded so no reader inherits it.

The two coverage gates (§ 16, § 16b) *do* pass. Traceability is green; desirability-and-soundness
ratification is not yet done.

---

## 18. Post-Council Amendments (2026-07-30)

The § 17 dual-lens gate ran as a `/council` (five lenses: product, UX, platform, visual, verifiability).
It returned blocking findings against this document. Each is inlined at its site; this section states the
evidence once. **The design remains `draft` — now for substantive rather than procedural reasons.**

### 18.1 The axis-disposition gate (§ 4.3) was cut by necropsy

A validated *fix* is not a validated *diagnosis*. That new items would pass the proposed trailer gate is
**confirmatory** — a rubber stamp that always says PASS also passes N/N. The **discriminating** test is
whether the detector would have **FAILED the actual corpse**. Run against the six findings:

| Finding | Surface | Detector (`+struct … : View` in `StatusPanel*.swift`) |
|---|---|---|
| F-1 EXPIRY column | `ExpiryLine` **struct**, `StatusPanelRoster.swift:586` | **fires** |
| F-3 chip contrast | no new declaration | misses |
| F-4 auth-glyph affordance | `authView` **computed property**, `:477` | **misses** |
| F-5 tooltip scope | modifier placement, `:275-276` | misses |
| F-2 app icon | `brand/**` — outside the path filter entirely | misses |
| #763 Settings window | different surface, no matching path | misses |

**1 of 5 panel defects; 1 of 3 surfaces.** And the miss is not incidental: declaration shapes in
`StatusPanelRoster.swift` run **40 computed vars to 32 structs**, so the trigger keys on the *minority*
idiom. A historical check confirms the false-positive side too — `4d8ffa4` added 27 structs in one commit
and would have demanded the trailer for a change that authored no new panel element.

Fair concession recorded: trailers **are** empirically cheap here (31 across the repo's life; ~16 commits
per month touch panel paths). The cut is on *coverage*, not cost.

**Replacement, at the maintainer's direction**: ask the five axes at **readiness time**, as a DoR question
on the work item. Earlier, cheaper, and surface-agnostic — it reaches `brand/**` and the Settings window,
which no `StatusPanel*.swift` path filter can. **Critical falsifier to watch**: does panel work in this
repo actually pass through a DoR step? If items routinely skip it, the mechanism inherits the same
ceremony risk and should escalate to an executable coverage test.

**R-9 / Cap-6.1 CUT.** `scripts/check-panel-golden-rebaseline.sh` already meets the re-baseline hazard for
committed goldens (#754); its own header states the case.

### 18.2 The chip fork (§ 4.4) was framed on two unsound options

- `.tertiary` is unrepresentable: `HierarchicalShapeStyle.resolve(in:)` → **`Never`**, on every macOS
  version; macOS 13.0 has no `resolve` API at all. So option (a) was never instrumentation — it is a
  redesign, and the type system says so.
- Option (b)'s "non-colour channel" is occupied: `nosign` is already a ring (so a ring/capsule makes the
  confusability defect worse), and the leading-edge inset carries fault severity.
- **Option (d)** — `PanelTint.asset` colorsets — is the live answer, and it is what the repo already
  reached for when a raw system tint failed contrast on the vibrancy (`StatusPanelFormat.swift:667`).
- The **coupled-contrast constraint was wrong three ways** (see PRD § 13.3); **RR-5 is retired**.
- **Measured**: resting **1.93–2.06:1**, **0 of 70 pixels** clear 3:1 — below the mock's own 2.10:1
  ceiling. ~~Dark appearance unmeasured.~~ **Completed by #949: 1.91:1 light / 2.70:1 dark, 0 of 243 /
  433 pixels clearing; armed 4.73:1, unchanged. Option (d) shipped in #956 at 3.34:1 (`0ab82fc`).**
- **Priority context**: the chip is used *rarely*, the footer Swap *never*. A newly-found defect —
  `arrow.left.arrow.right` vs `nosign` are near-interchangeable at rest — outranks it and is filed
  separately.

### 18.3 Two capability claims were over-stated

- **Cap-1.3** — only *state* parity is gated; column **order/placement** is asserted nowhere. That gap is
  the reason F-1 shipped under a green `swift` job.
- **Cap-4.2** — `brand/**` matches **no** path filter in `ci.yml`; an edit there runs **zero** jobs, so the
  capability cannot be CI-verified as written. Reframe as a static mechanism assertion.
- **Cap-4.1 upgrade (good news)** — the AppIcon PNGs are **committed** (11 files under
  `apps/menubar/Sources/Assets.xcassets/AppIcon.appiconset/`) and the `swift` job gates on
  `apps/menubar/**` (`ci.yml:164-169`), so grid conformance **is** testable in a job that already runs, with
  no new gate path and no `Gate-Change-Acknowledged:` trailer.

### 18.4 The affordance capability (Cap-3.1) counted the wrong thing

"Every row state has a hover response and a tooltip" would manufacture six tooltips for states that are
**correctly** empty. Exactly one state (blocked) carries a load-bearing reason no resting render conveys,
and its remedy is **persistent text** — a load-bearing fact must not be hover-only. Two related
corrections: the mock authors **more** than this design credited (28 `.acct active`, the blocked copy
verbatim ×4, a deliberate 42-vs-0 `title=` pattern), and `.accessibilityHidden(true)` on the auth glyphs is
**correct** — the row collapses to one AX element whose label speaks every verdict plus its remedy.

### 18.5 The § 4.3(b) rejection reasoning was wrong (the verdict held)

"Structurally cannot enforce" rested on `panel-goldens` being soft — 5 steps, 5 `continue-on-error`
markers, `ci.yml:396` — which is one config change from reversible, hence not structural. The correct
reason is **categorical**: `build-comparison.py` is a **drift** detector pairing capture-to-mock by name;
a never-covered axis is absent from both sides, so there is nothing to diff. Right answer, wrong argument
— recorded because the wrong argument would have been reused.
