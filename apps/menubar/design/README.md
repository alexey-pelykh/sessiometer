# Menubar design reference

The canonical **visual** build-reference for the SwiftUI menubar panel (see #168 / #169).
`menubar-preview.html` is a single self-contained mock of **all 9 launch-or-attach states**
(light + dark) in the intended native macOS language, plus a **capture-affordance interaction-states**
reference card (pending / done / error) for the in-app "Capture active account" action (#360).

![All 9 menubar states, light + dark](renders/all-states.png)

## Viewing it

- **Interactive / most faithful** — open the HTML in a browser: `open menubar-preview.html`
- **At a glance** — `renders/all-states.png` above, rendered from the HTML.

## Regenerating the render

The mock uses `backdrop-filter` vibrancy, which needs **GPU compositing**. Render with a
GPU-enabled headless Chrome — do **not** pass `--disable-gpu` (it forces software rendering and
blacks out the vibrancy). Run from this directory:

```sh
"/Applications/Google Chrome.app/Contents/MacOS/Google Chrome" \
  --headless=new --hide-scrollbars --force-device-scale-factor=1.25 \
  --window-size=1200,12500 --screenshot=renders/all-states.png \
  menubar-preview.html
```

(Bump the `--window-size` height if the page ever grows past it — but **not past the per-scale cap
below**. The committed render is 1500×15625 at this `12500` height × the `1.25` device scale; a
shorter height clips the notes. The mock ends at ~11050 CSS px as of #778, so `12500` leaves
~1450 px before the notes clip — of which only ~600 px is actually bumpable, per the cap.)

**Why `1.25` and not `1.5`** — the render height in *device* pixels (CSS height × scale) must stay
under the GPU's 16384 px max texture dimension. Past it the GPU process dies mid-render
(`Restarting GPU process due to unrecoverable error`) and no PNG is written. At `1.5` the 34-frame
mock needs 16536 device px, which is over; `1.25` needs 13776 and fits. Measured empirically at
#778 (at `1.5`): 16350 device px renders, 17100 fails.

In the units of the knob you actually turn — the maximum `--window-size` height is **13107** at
`1.25`, **10922** at `1.5`, **16384** at `1.0` (16384 ÷ scale). Past that the bump *itself* kills
the render, so growing the page eventually means lowering the scale, not just raising the height.
`1.5` is already unreachable: its 10922 cap is below the mock's own ~11050.

If another Chrome is already running, add `--user-data-dir=<a scratch dir>` — without it the render
can block on the profile lock instead of exiting. Chrome also tends to write the screenshot and then
hang rather than exit, so run it under `timeout 180` and check the output file, not the exit code.

## Rendering the BUILT panel (design-parity check)

The mock is the reference; the **built** SwiftUI panel is what ships. To verify the panel actually
matches the mock — the check whose absence let the panel drift (#355) — render the real
`StatusPanelView` to PNG and diff it against the mock's **Healthy · Status** section.

The panel is an `NSPopover` view that can't be opened programmatically or screen-captured without
Screen-Recording permission, so a DEBUG-only tool (`RenderPanelTool`, wired in `AppDelegate`) draws
it straight to a bitmap with SwiftUI `ImageRenderer` — no popover, no screen capture, no TCC:

```sh
# from apps/menubar, after a Debug build (xcodegen generate && xcodebuild build -scheme Menubar …)
BIN=".build/xcode/Build/Products/Debug/Sessiometer.app/Contents/MacOS/Sessiometer"
"$BIN" --render-panel "$PWD/design/renders"
```

Output: `renders/panel-healthy-{light,dark}.png` — the built app (distinct from `all-states.png`,
which is the mock). These are safe to commit: `RenderPanelTool` pins a fixture roster (`Work` /
`Personal` / `Temp`), and the wire carries only the operator-chosen label, never an email (#15).
Light shown here:

![Built panel — healthy, light](renders/panel-healthy-light.png)

**Expected reconciliations** — the built panel intentionally differs from the mock in these spots:

- no provider secondary line — the wire carries no `provider` field yet (#173)
- the footer reads "updated <1m ago" — the panel mirrors the `status` CLI (R-2 state-parity), not
  the mock's illustrative "snapshot 12s old". Resets no longer diverge: the mock now uses the CLI's
  compact duration form too ("2h14m" / "3d"), not a day-name (#387)
- the **Swap** button is LIVE as of #169 (it sends the displayed `next_swap` target over the daemon's
  `swap` command). Each non-active roster row is also a manual switch — as of #448 a **persistent, quiet
  trailing chip** (neutral `.tertiary` at rest, brightening to `.secondary` when the row is armed on
  hover/focus), which the mock now specs (the resting chip on every switchable row); at rest the row
  keeps a trailing action slot for it, which is why the auth glyph sits ~37 pt further left than in the
  mock (the #448-widened 28 pt slot + its 9 pt spacing)
- the third fixture account is `Temp`, where the mock illustrates `Scratch` — re-picked (#709) so all
  three healthy labels hash to **distinct** #445 identity slots (the mock's `Personal` and `Scratch`
  both land on slot 5 / ochre under the shared 8-slot `label` hash, so the built roster would otherwise
  render two of three rows in one colour). The committed oracle now demonstrates the per-account cue
  across three visibly-distinct colours — violet / ochre / teal — not two; the hues are hash-derived
  and, like every colour here, needn't match the mock's illustrative ones

(Capture placement is now reconciled with the mock, not a difference: the **populated** panel carries
no capture bar — capture is **empty-roster / first-run only**, and Add account lives off-panel in the
status-item right-click menu (#394). So `panel-healthy-*.png` correctly shows no capture bar.)

**Harness limitation — the capture field is NOT RASTERIZED by the tool (but it IS verified elsewhere,
#765).** SwiftUI `ImageRenderer` cannot rasterize the AppKit-backed `TextField` in the #360 capture
affordance (the operator-label input on the empty-roster / first-run onboarding card): it draws a blank
placeholder box, not the real field. So `--render-panel` faithfully captures every state's layout,
color, and typography **except** that one label field. Treat a blank/placeholder capture-field box in
the PNGs as a known tool artifact, not a panel defect.

What issue #765 changed is the sentence that used to follow. This note previously concluded that the
field therefore "needs a manual check against the mock in a real popover", and listed the #394 "Add
account…" surface as a second manual check. That conflated two different limitations: `ImageRenderer`
cannot RASTERIZE the field, which is still true, but rasterizing is one way to observe a view and not
the only one. `Tests/PanelCaptureCardTests` now verifies the card through two lanes that never need a
pixel, both inside the required `swift` CI job:

- **the accessibility tree** (#758's in-process walker) — the field's reachability, `AXTextField` role
  and enablement, at **every** capture phase: idle, in-flight, done, failed. Those phases are new
  coverage even against #758, which sees the one `empty-roster` render fixture at its resting phase. The
  disabled-while-pending state is a property a raster cannot express at all. Both entry points publish
  the same affordance and both are covered, so the #394 surface's *content* is no longer a manual check
  — its live-panel presentation still is (step 1 below).
- **CoreText metrics** (#750/#762 `TextMetrics`) — every shipped capture string measured against the
  card's derived text budget, across all 12 Dynamic Type classes.

What is still manual is the **live-panel interaction**: focus, keystroke routing, Return-to-submit and
Esc-to-cancel need a real `FloatingPanel` (the borderless non-activating `NSPanel` the status item hosts
— not an `NSPopover`, see issue #808), and the fidelity comparison against the mock's onboarding frames
is still a human eye. Those are steps 1–2 of the *Capture + notification pre-release checklist* below.

Two measured facts from building that gate, recorded because they are easy to re-lose:

- The button's **rendered** title ("Capture active account") never reaches the accessibility tree — its
  `.accessibilityLabel` ("Capture **the** active account") replaces it. A query keyed on the rendered
  string returns nothing and reads as "the button is absent". This is why every absence claim in that
  suite pins a known-present anchor first.
- The card's single-line strings use at most **43 %** of the card at every Dynamic Type class, so the
  "scaled font in an unscaled cell" mutation that falsifies `PanelTextMetricsTests`' sweep cannot trip
  that half of this one. Its falsifier is an over-wide fixture instead; the wrapping half keeps the
  scale mutation, where it trips decisively (2 lines → 4). Neither number is left to this paragraph:
  the sweep re-derives the worst ratio every run and reddens if copy growth closes the margin, and
  `testTheScaleMutationIsProvablyInertOnTheSingleLineLane` re-measures the mutated case (~92 % of an
  unscaled card) that makes the over-wide fixture the honest falsifier here.

**Harness limitation — ARMED / in-flight states are NOT captured.** `ImageRenderer` draws one resting
frame. As of #448 the per-row manual-switch chip is PERSISTENT, so a render captures its resting glyph
(`arrow.left.arrow.right`, or the `nosign` on a non-viable row) at its quiet `.tertiary` emphasis — the
committed `panel-healthy-*.png` show it on every switchable row. What a single resting frame still
can't show is the ARMED state — the hover/focus brighten to `.secondary`, the row wash, the
`pointingHand` cursor — nor the in-flight `Switching…` spinner; those are interaction states, so they
stay a manual operator check (#380) — as does the real-popover swap round-trip.

**Correction (issue #749): `ImageRenderer` rasterizes headlessly.** This note previously said
regenerating the committed PNGs "needs a GUI / windowserver session — not something headless CI can
do", and that belief (echoed in `BarGlyphParityTests`) was the stated reason the panel had no automated
visual gate while the bar glyph did. It was never tested, and it is wrong:
`ImageRendererHeadlessProbeTests` rasterizes SwiftUI inside the standalone `MenubarTests` bundle
(`TEST_HOST: ""` — no host app, no `NSApplication`, no window) under `xcodebuild test`, across a bare
view, an SF Symbol, and the `@EnvironmentObject` / `@Published` environment-injection path that issue
#749 flagged as the likelier failure point. So an in-bundle panel golden gate IS reachable — and issue
#754 built it (see *Panel golden drift gate* below). (The stronger no-*windowserver* claim was confirmed
separately under `sandbox-exec` denying `com.apple.windowserver*` — `CGSession` nil, `NSScreen.count` 0,
identical output bytes — but only the in-bundle claim above is what CI re-runs.)

What stays a **manual pre-release step** is this `--render-panel` pass and the eyeball comparison it
feeds, for the ordinary reason that it needs a local Debug build of the app (`RenderPanelTool` is
`#if DEBUG`) and a human judging fidelity against the mock — not because of any headless limitation. The
automated half (drift against committed goldens) does run headless in CI, and it is a different question:
*has the panel changed?*, not *does the panel match the design?*

### Design vs. capture, screen by screen

`build-comparison.py` assembles a single self-contained page that puts the mock's **live** `.pop`
blocks next to the built-panel captures, state by state — the fastest way to eyeball parity across the
eight connection-states the panel implements, plus the active-account **blind** modifier (OK / DEGRADED,
#479/#485) and the four **daemon-fault** ranks (#592). Neither of those last two is a connection-state:
`blind` is a per-row modifier and a fault banner is resolved *over* a connected snapshot, which is why
both pair against a healthy green roster. The four fault ranks pair in the worst-first order the panel
resolves them (keychain-locked › scrub-exhausted › systemic › recovering), so a severity **inversion** —
a visual claim the format-layer unit tests cannot reach — shows up by reading them in sequence:

```sh
# from apps/menubar, after a Debug build
BIN=".build/xcode/Build/Products/Debug/Sessiometer.app/Contents/MacOS/Sessiometer"
"$BIN" --render-panel /tmp/panelcaps                         # render every state + the blind modifier, both themes
python3 design/build-comparison.py /tmp/panelcaps /tmp/design-vs-capture.html
open /tmp/design-vs-capture.html
```

**This harness re-baselines itself, by design — which is why the golden gate below exists.**
`build-comparison.py` slices the mock's `.pop` blocks **live** at comparison time, so editing
`menubar-preview.html` silently changes what the built panel is compared against. That is correct for
this tool (the mock is the reference; it *should* always read the current mock), but it means the tool
cannot detect PANEL drift — nothing here is committed, so nothing here shows in a diff. The panel
golden gate is the other half.

Frames are paired **by name**, never by position: every `.pop` block carries a `data-frame` (e.g.
`blind-ok-light`), and each `STATES` entry names the frame it pairs with. So add, remove, or reorder
frames freely — a mock frame and its Swift fixture no longer have to land in one commit (#581).

Name a new frame when you add it: kebab-case the `fcap` caption down to what distinguishes the frame —
state, then variant — and always suffix the theme (`Active blind · OK · Light` → `blind-ok-light`;
"Active" is filler, so it goes). That last step is judgment, not a transform, so match the neighbours —
nothing enforces the convention. What the script *does* enforce is presence and uniqueness: it exits
non-zero on an untagged block, a duplicate name, or a `STATES` entry pointing at a name the mock no
longer carries — naming the frame, or the line for an untagged block.

### Panel golden drift gate (#754)

The harness above is the **fidelity** path (a human eye, against the live mock). This is the **drift**
path: `Tests/PanelGoldenParityTests` re-renders every panel state in-process — SwiftUI `ImageRenderer`
inside the headless `MenubarTests` bundle, which issue #749 measured as viable — and diffs the fresh
renders against committed goldens under `renders/panel-goldens/`. **34 goldens** (17 fixtures × light/dark,
`panel-<state>-<theme>.png`, @2x), rendered through the same `PanelRenderHarness` the app's
`--render-panel` tool uses, so the automated gate and the human oracle can never render different states.

Read a green as "the panel's appearance has not changed since it was last blessed" — **never** as "the
panel matches the mock". The built panel intentionally differs from the mock on the axes listed under
*Expected reconciliations* above, and those differences are baked into the goldens.

**Regenerating (re-baselining) the goldens** — an explicit command, never a side effect:

```sh
# from apps/menubar
xcodegen generate
TEST_RUNNER_SESSIOMETER_PANEL_GOLDENS=update xcodebuild test \
  -project Menubar.xcodeproj -scheme Menubar -configuration Debug \
  -destination 'platform=macOS' CODE_SIGNING_ALLOWED=NO \
  -only-testing:MenubarTests/PanelGoldenParityTests/testRegenerateGoldensWhenExplicitlyRequested
```

The `TEST_RUNNER_` prefix is required: `xcodebuild` forwards only prefixed variables into the test
process (stripping the prefix). A bare `SESSIOMETER_PANEL_GOLDENS=update` reaches `xcodebuild` and not the
test, which then **skips** — writing nothing while exiting 0.

Then **look at the new renders** (a reference you have not looked at is not a reference) and record why
they changed:

```sh
git commit --trailer 'Panel-Goldens-Rebaselined: <what changed in the panel and why>'
```

`scripts/check-panel-golden-rebaseline.sh` enforces that trailer in CI on any PR touching
`renders/panel-goldens/**` — add, modify, or delete. Its falsifier peer
(`scripts/check-panel-golden-rebaseline.test.sh`) proves the guard goes red without it.

**Re-deriving the thresholds** (the measurement table in the suite's header):

```sh
TEST_RUNNER_SESSIOMETER_PANEL_MEASURE=1 xcodebuild test \
  -project Menubar.xcodeproj -scheme Menubar -configuration Debug \
  -destination 'platform=macOS' CODE_SIGNING_ALLOWED=NO \
  -only-testing:MenubarTests/PanelGoldenParityTests/testMeasureSeparations
```

Two of the header's rows are asserted by always-on tests rather than printed by that command (the
aqua-vs-darkAqua row, and the app-tool-vs-goldens row measured out of band by running `--render-panel`
into a scratch directory and `diff -rq`-ing it against `renders/panel-goldens/`), so re-deriving the
whole table means running the default suite too.

**What the relative gate does and does not cover.** The primary check —
`testEachFreshRenderIsNearestToItsOwnGolden` — asks that a fresh render's closest same-size golden be
itself, which needs no cross-machine calibration and catches one state morphing into another. It only has
power where a same-size golden of a *different* fixture exists to lose to, and goldens are sized by
content: 5 of the 17 fixtures (`stats`, `disconnected`, `not-running`, `empty-roster`, `blind-cornered`)
own a unique height, so their size group holds only their own two themes, ~0.97 apart. For those **10 of
34 cells the relative check is trivially satisfied** and the absolute ceiling — the cross-machine
*unvalidated* half — is the only thing defending them. The suite asserts that count rather than merely
noting it, and prints it on every run alongside the weakest real margin (measured **0.002513**), so the
promotion decision in issue #790 has the number in front of it.

**What runs where.** Only the two committed-golden comparisons are cross-machine sensitive (goldens
rasterized on one machine, re-rendered on an unpinned `macos-latest`), so they are env-gated off by
default and run only in the **non-required** `panel-goldens` CI job:

| Assertion class | Where | Required? |
|---|---|---|
| renders succeed · non-blank · deterministic · clock-drift window · host-appearance independent · pairwise distinct · health-tint assets resolve · both canaries · PNG round-trip | `swift` job (default suite) | **yes** — all same-run comparisons, cross-machine immune |
| `testEveryRenderMatchesItsCommittedGolden` · `testEachFreshRenderIsNearestToItsOwnGolden` | `panel-goldens` job (`SESSIOMETER_PANEL_GOLDEN_GATE=1`) | **no** — soft-landing |
| `Panel-Goldens-Rebaselined:` trailer | `gate-change-ack` job | **yes** — pure git, cannot be flaky |

**A golden can change bytes without changing pixels.** The gate compares decoded PIXELS, never file
bytes — which is why the suite normalizes both sides into one sRGB/RGBA8 buffer. `NSBitmapImageRep`'s PNG
encoder is byte-stable for a given OS + Xcode but not across versions, so re-blessing on a newer machine
can produce a diff with no visible change. Measured instance: the committed `renders/panel-healthy-*.png`
oracle (rendered on an older toolchain) has different bytes from a fresh render and **0.000000** pixel
drift. Do not read "the PNG changed" as "the panel changed"; the gate's own verdict is the answer.

**On THIS toolchain the goldens are byte-reproducible, and that is deliberate.** Two independent
`SESSIOMETER_PANEL_GOLDENS=update` runs produce byte-identical files, and the app's own `--render-panel`
output is byte-identical to all 34 goldens. It did not start out that way: the first renders in a process
disagree with the steady state by ±1/255 on ~0.03 % of bytes — a rasterization warm-up artifact, found by
rendering one fixture six times (renders 0–1 agree with each other, renders 2–5 agree with each other,
the two groups differ) and ruled out as a clock effect because renders seeded seconds apart are
byte-identical. `PanelRenderHarness` now discards renders until two consecutive ones agree, so both the
app tool and the in-bundle gate rasterize from the steady state. None of this changes a VERDICT — the
gate metric ignores channel deltas under 64/255 either way — it protects the AUDIT TRAIL: without it a
re-bless rewrites files that did not change, and the real change hides among the churn in exactly the
diff `Panel-Goldens-Rebaselined:` exists to make readable. `PanelGoldenParityTests` asserts the
byte-exactness directly, so a regression here fails loudly rather than quietly returning the churn.

**Promotion to required: N = 10 consecutive green `panel-goldens` runs on `main`.** Ten rather than three
because the risk under measurement is antialiasing drift on an *unpinned* runner image, and the sample
must span at least one image roll. EVERY step in the job is `continue-on-error` — not just the gate step,
because `ci-ok` fails on any un-guarded step in a job it needs, so a `brew install xcodegen` network blip
would otherwise block a merge through the very job whose whole point is not to. The job's conclusion is always
success; the countable signal is the `PANEL_GOLDEN_GATE_RESULT=green|red` line the reporting step prints,
alongside the `max drift` figure the promotion decision needs. **The promotion decision — the tally, the
re-calibration question it must answer first, and the mechanics — is recorded in issue #790.**

## Accessibility (#758)

Accessibility *labels* have long been deliberate and unit-tested as **strings**
(`StatusPanelFormatTests`). What issue #758 added is a gate over everything about a11y that is **not** a
string — reachability, role, enabled state, element order, and whether `accessibilityHidden(true)`
decorations genuinely *leave* the tree. A correct label on an unreachable or mis-typed element is not
accessible.

### The tooling question, answered

Issue #758 was written around "`performAccessibilityAudit` needs a macOS 14 host, but the app targets
13.0 — can we audit at all?" Measured 2026-07-28, that framing names the wrong blocker three times:

1. **The host was never it.** Dev machine macOS 26.5.2 / Xcode 26.6; CI's `macos-latest` resolved to
   macOS 26.4 on the #749 and #761 runs.
2. **The test bundle can outrank the app.** Adding `deploymentTarget: "14.0"` to the `MenubarTests`
   target yields `MACOSX_DEPLOYMENT_TARGET = 14.0` for it while the `Menubar` app target still reads
   `13.0` — XcodeGen's per-target key overrides `options.deploymentTarget` for that target only. So the
   app's shipping floor never had to move.
3. **…and it is not needed.** `performAccessibilityAudit` hangs off `XCUIApplication`, i.e. a UI-test
   bundle, with all of issue #761's costs (own scheme, dead on a locked session, prose-coupled queries).
   The accessibility tree turns out to be reachable **in-process** from the existing headless bundle.

So the automated branch is a **GO**, by a cheaper route than the issue anticipated, and `project.yml`'s
`macOS: "13.0"` is untouched.

### The automated gate

`Tests/PanelAccessibilityTreeTests` hosts the panel in an `NSHostingView` and walks the live AppKit
accessibility tree across all 17 render fixtures — no XCUITest, no scheme risk, no TCC grant
(`AXIsProcessTrusted()` is **false** in that bundle), ~0.6 s, inside the required `swift` job. It asserts
interactive elements publish `AXButton`, blocked rows publish `enabled=false`, decorative elements are
**absent** from the tree, no focusable element is silent, navigation order runs header → tabs → roster →
footer, and each fixture's role histogram is unchanged.

Two constraints make its green trustworthy, both learned the hard way locally:

- **Every absence claim pins a known-present anchor in the same dump.** An empty tree satisfies "nothing
  leaked" perfectly, so absence is evidence only against a populated tree — the trap issue #761's spike
  fell into when it read a filtered tree as a complete one.
- **Every predicate has a mutation canary.** Each is fed a deliberately-broken view through the *same*
  function the real assertion calls, because a gate authored against passing code can be one that cannot
  fail — issue #437's three render bugs are the local precedent for what that costs.

Two known defects are pinned as set **equality**, so fixing either turns the suite red and says so:
issue #838 (a decorative Stats icon reaching the tree) and issue #839 (non-interactive rows publishing
`AXUnknown`). The Settings window is **not** covered — see issue #840.

### VoiceOver pre-release checklist (manual)

The tree walk cannot see VoiceOver's own behaviour: the rotor, real focus traversal, and speech are
runtime features of the screen reader, not attributes of the tree. Those stay manual. Run this before a
release, with VoiceOver on (`⌘F5`):

**Status panel** — open it from the menu bar, then:

- [ ] `VO`+arrow traverses header → Status tab → Stats tab → each roster row → next-swap callout → switch
      chip → footer, with nothing skipped and nothing announced twice.
- [ ] Each roster row speaks its whole sentence (label, active/auth state, both percents and resets).
- [ ] The switch chip announces as a **button**, not as text, and speaks its target account.
- [ ] On the `blind-cornered`-shaped state, a weekly-exhausted row announces as **dimmed / unavailable**,
      not merely with "Can't switch" buried in its sentence.
- [ ] No glyph, meter, sparkline, capsule or signal pill is ever focused — VoiceOver should never say
      "image" inside the panel. (Issue #838 is a known live exception in the Stats tab until fixed.)
- [ ] Rotor (`VO`+U): the roster is navigable by control type; note which rows are missing from the
      button/text lists (this is the observation issue #839 is tracking — confirm or correct it here).
- [ ] Switching Status ↔ Stats moves focus somewhere sensible rather than dumping it at the panel root.

**Settings window** — `⌘,`:

- [ ] The window announces its title and is fully reachable by `Tab` and by `VO`+arrow.
- [ ] Every `Toggle`, `TextField` and `Button` announces its role and current value.
- [ ] The per-field help text is spoken (it maps to `accessibilityHelp`).
- [ ] The decorative icon in the load-failure state is never focused.

Not on this list on purpose: the **armed / hover** states. Hovering drives only the row wash, the chip
tint step and the cursor — none of which is an accessibility attribute, so an armed row and a resting row
are byte-identical in the tree and VoiceOver cannot distinguish them either. That surface belongs to
issue #766's interaction-state checklist, not here.

### Settings window pre-release checklist (manual, non-VoiceOver)

Issue #762 put the Settings window's copy and its two hardcoded field widths under an automated gate
(`Tests/SettingsTextMetricsTests`). Three surfaces stayed out of reach of that gate — window/activation
lifecycle, the `⌘S` key event, and runtime affordances (spinner, focus ring, hover tooltip) — and they
are listed here rather than left silent, since the issue's AC-4 is explicit that an untestable surface
must be named as such. They expand to the five steps below because two of them have a stopped-daemon and
a running-daemon half. Everything else the gate could not reach has a tracked owner instead: the
accessibility tree is issue #840, the missing design reference is issue #763, and Dynamic Type is the
pinned defect issue #845.

Run these with the daemon RUNNING (so the form loads) unless a step says otherwise:

- [ ] **Window lifecycle.** Open Settings (`⌘,`), close it, reopen it. The window reappears with the same
      size and position, exactly one Settings window ever exists, and each open re-reads the daemon (edit a
      tunable in `config.toml` externally, reopen, confirm the form shows the new value).
- [ ] **Activation policy.** While Settings is open the app has a Dock icon and is `⌘`-Tab reachable; after
      closing it the Dock icon goes away again. A lingering Dock icon means the app is stranded in
      `.regular` — the failure `SettingsWindowController` deliberately avoids by omitting `.miniaturizable`.
- [ ] **⌘S.** With a pending edit, `⌘S` saves without touching the Save button. With no pending edit — and
      while a save is in flight — it does nothing. (The predicate behind the button is gated automatically;
      that the *key event* reaches it is not.)
- [ ] **Loading + focus.** With the daemon STOPPED, open Settings: the Notifications and General sections
      are still usable, and the daemon section shows the honest failure rather than a blank or fabricated
      form. Tab through the form and confirm every field shows a visible focus ring and the spinner
      animates on a slow first load.
- [ ] **Hover tooltips.** Hover each tunable field and confirm its `.help` text appears. This is the only
      place a field's unit and meaning are explained, and hover is not an accessibility attribute — no
      tree walk can see it. (VoiceOver users get the same copy via `accessibilityHelp`, which the
      VoiceOver checklist above covers; this bullet is the sighted-mouse half.)

Not on this list on purpose: the per-field COPY, the Save enable/disable rule, and whether a value or
label fits its field. Those are measured in `SettingsTextMetricsTests` and do not need a human.

### Status item + app entry pre-release checklist (manual)

Issue #764 put the status item's and the app entry point's pure DECISIONS under automated gates
(`Tests/StatusItemChromeTests`, `Tests/AppLaunchPlanTests`). What is left is the imperative shell those
decisions drive — `NSStatusBar`, `NSPanel`, the global event monitor, and the top-level `NSApplication`
bootstrap — none of which a headless logic bundle can stand up. `StatusItemController.swift` and
`main.swift` therefore remain excluded from `MenubarTests` and always will be: the controller owns real
menu-bar chrome, and `main.swift` is top-level entry code that cannot live in a unit-test bundle at all.

Here is where every surface of those two files went. **25 rows: 9 covered by this item, 6 already covered
elsewhere (one of those with an OS-wiring half that only a human can see, so it also appears below), 8
routed to the 8-step manual checklist below, and 2 filed as defects.** Nothing is left silent.

| Surface | Disposition |
|---|---|
| Panel placement (centering, both-axis clamp, #446 bottom floor) | `StatusItemChromeTests` |
| Degenerate `fittingSize` fallback | `StatusItemChromeTests` |
| Primary vs secondary click classification | `StatusItemChromeTests` |
| Lifecycle-menu rows, order, copy, shortcuts | `StatusItemChromeTests` |
| Outside-click disposition (own-icon, in-flight retain) | `StatusItemChromeTests` |
| Open-panel precondition | `StatusItemChromeTests` |
| Launch-mode dispatch + precedence + gallery opt-in | `AppLaunchPlanTests` |
| Degrade wording + per-call-site timeout budgets | `AppLaunchPlanTests` |
| Degrade FEED shape (exactly one `.disconnected`, then finish) | `AppLaunchPlanTests` |
| `ConnectionState` → glyph projection | Already: `HonestStateMachineTests` (exhaustive 10-row table) |
| One distinct silhouette per state (the brand lock) | Already: `StatusGaugeTests` + `BarGlyphParityTests` |
| First-launch login-item registration, `canStartDaemon` | Already: `LoginItemModelTests` (#170) |
| Daemon-agent re-registration after an app update | Already: `LoginItemModelTests` (#788) |
| Sleep/wake dwell suspension logic | Already: `WatchStatusStoreTests` (#526) — the OS wiring is step 6 below |
| Socket-path resolution + its failure taxonomy | Already: `SocketPathResolverTests` (ADR-0011 tripwire) |
| Status-item creation, button target/action, panel + vibrancy chrome | Manual, step 1 |
| Panel show/hide, key focus, `FloatingPanel.canBecomeKey` | Manual, steps 2 + 8 |
| Transient-menu presentation mechanism | Manual, step 3 |
| Global outside-click monitor install/remove | Manual, step 2 |
| Stats re-size observer (`objectWillChange` → re-fit while open, #446) | Manual, step 4 |
| `start()` presentation-stream consumption + its idempotence guard | Manual, step 1 |
| Activation policy (`.accessory`), `applicationWillTerminate` | Manual, step 7 |
| Notifier-before-`store.start` ordering | Manual, step 5 |
| Tool flag with no output directory falls through to a normal launch | Filed: issue #850 |
| Control-client `.failure → nil` degrade mapping (×4 call sites) | Filed: issue #853 |

Two rows deserve their reasoning stated, because both were originally mis-filed here and the correction
is the useful part. The **socket-path** row covers `resolve()` returning the right `ResolveError` — NOT
what the four call sites do with it; that `.failure → nil` branch is a genuinely uncovered honesty path
(issue #853), and the obvious manual step does not reach it, because a merely *stopped* daemon still
resolves its socket path and the client is constructed and then times out. And the **state → glyph** row
is this item's entire answer to AC-1, so `StatusItemChromeTests.testTheUpstreamStateToGlyphTableStill`
`Exists` reads that suite's source and reddens if the table is deleted or narrowed — a link, not a note.

Run these against a real build with the daemon RUNNING unless a step says otherwise:

- [ ] **The item appears, and tracks state.** Launch the app: exactly one status item appears, showing a
      monochrome gauge that tints correctly in a light AND a dark menu bar. Stop the daemon and confirm the
      glyph changes, then restart it and confirm the glyph follows — the item must keep tracking, and must
      never end up with two subscriptions double-applying. (Which glyph each state selects is gated
      automatically; that the item exists at all, that the image is applied as a template, and that the
      presentation stream is consumed exactly once, are not.)
- [ ] **Click to toggle, and click away.** Primary-click the icon: the panel opens below it with a visible
      gap, and the icon stays visible and clickable. Primary-click the icon again: it closes. Open it, then
      click elsewhere on screen: it closes. Start a swap and click away mid-flight: it must NOT close.
      (The decision is gated; that the global monitor is actually installed and removed is not.)
- [ ] **Secondary-click menu.** Right-click and control-click the icon: the lifecycle menu appears
      positioned under the item, and the panel closes first if it was open. Pick each row and confirm it
      acts. Then primary-click the icon: the panel must still toggle — a menu left permanently assigned to
      the status item would hijack the primary click (#325/#326).
- [ ] **Stats tab re-size.** With the panel open on Status, switch to Stats: the panel must GROW to fit
      the taller content rather than clipping it, and must stay fully on screen. Switch back to Status: it
      must shrink to the original size. Then close the panel, switch nothing, and reopen — it opens on
      Status at the small size. (The arithmetic is gated automatically; that the observer fires and defers
      the re-fit to the next run-loop turn is only observable here — #446.)
- [ ] **Cold start with the daemon stopped.** Launch with no daemon: the item shows the disconnected
      glyph, the panel opens and reads honestly, and no notification is missed on the first snapshot when
      the daemon comes up (the notifier is installed BEFORE the store starts consuming — an ordering the
      wiring, not a model, guarantees).
- [ ] **Sleep/wake.** Close the lid overnight (or `pmset sleepnow`) with the daemon running, and check the
      item on wake: a benign long disconnect must NOT have escalated to Attention. The dwell logic itself
      is unit-tested; that the `NSWorkspace` notifications reach it is only observable here (#526).
- [ ] **Agent shape.** Confirm no Dock icon and no app-switcher entry while only the status item is up,
      and that Quit terminates the app while `sessiometer status` still answers — Quit is a pure-client
      control and must never stop the daemon.
- [ ] **VoiceOver on the item.** With VoiceOver on, navigate to the status item and confirm it speaks the
      current state sentence, and that focus moves INTO the panel when it opens. (`FloatingPanel` overrides
      `canBecomeKey` precisely so this works; a plain borderless window would leave the panel unreachable.)

Not on this list on purpose: the panel's own contents. Those are covered by the panel golden gate, the
accessibility-tree gate (#758) and the VoiceOver checklist above.

### Capture + notification pre-release checklist (manual)

Issue #765 put the two surfaces the render harness structurally cannot see under automated gates —
`Tests/PanelCaptureCardTests` for the capture card (see the *Harness limitation* note above for what each
lane proves) and `Tests/NotificationDeliveryTests` for delivered notification content. What is left is
the part that is genuinely not a decision: a live `NSPanel`'s focus and key routing, and
`UNUserNotificationCenter`'s own prompting and rendering. Neither is observable from a headless bundle at
any effort, so they are listed here rather than left silent.

Here is where every surface of those two went. **13 rows: 8 covered by this item, 1 already covered
elsewhere, 4 routed to the checklist below.** Nothing is left silent.

| Surface | Disposition |
|---|---|
| Capture field reachable, `AXTextField`, enabled at rest | `PanelCaptureCardTests` (tree lane) |
| Field + button DISABLED while a capture is in flight | `PanelCaptureCardTests` (tree lane) |
| Done / failed status copy reaches the tree | `PanelCaptureCardTests` (tree lane) |
| Both entry points (#360 onboarding, #394 Add account) publish the same affordance | `PanelCaptureCardTests` (tree lane) |
| Card tree SHAPE (no decorative leak into the card) | `PanelCaptureCardTests` (tree lane) |
| Every capture string fits / wraps within the card, at every Dynamic Type class | `PanelCaptureCardTests` (metrics lane) |
| Delivered notification content carries no account label or email | `NotificationDeliveryTests` (end-to-end, past the seam) |
| Delivery identity is fresh per post (no coalescing) + the app-level grouping decision | `NotificationDeliveryTests` |
| Swap / exhaustion detection, redaction at the model layer, toggle gating | Already: `AccountEventNotifierTests` (#267) |
| Live-panel focus, keystroke routing, Return / Esc | Manual, step 1 |
| Capture-card fidelity against the mock's onboarding frames | Manual, step 2 |
| The OS authorization prompt, and a denial | Manual, step 3 |
| How Notification Center actually renders and stacks a delivered notification | Manual, step 4 |

Run these against a real build:

- [ ] **Capture in a live panel.** With an EMPTY roster, open the panel: the onboarding card appears
      with a real, focusable label field (not the blank box the PNGs show). Click into it and type — the
      keystrokes must land, which is the whole point of `AccountCaptureModel.panelKeyRequest` re-asserting
      the panel key. Press Return to submit, and on a second attempt press Esc to cancel: Esc must resign
      focus and return the card to idle so an outside click can dismiss the panel again. Then repeat the
      whole step through the status item's right-click **Add account…** surface (#394). (Every phase's
      tree is gated automatically; that a live `FloatingPanel` routes keys into it is not — it overrides
      `canBecomeKey` precisely so this works.)
- [ ] **Capture-card fidelity.** Compare that same first-run card against the mock's onboarding frames in
      `menubar-preview.html`. The gate measures that the copy FITS and that the controls are reachable;
      whether the card LOOKS like the ratified design is a human judgement, and the field is the one
      element `build-comparison.py` cannot show you.
- [ ] **Notification authorization.** On a fresh install, confirm the OS permission prompt appears once.
      Then DENY it and confirm the app stays usable and silent — no in-app re-prompt, no error banner.
      Finally, with notifications toggled off at launch, turn them on in Settings (`⌘,`) and confirm the
      prompt fires at that point (the toggle drives `onRequestAuthorization`).
- [ ] **Notification rendering.** Force a swap (`sessiometer swap`) and confirm a notification appears
      reading "Active account switched" / "Sessiometer rotated to a different account", and **that it
      names no account** — this is the manual half of the redaction guarantee, on the lock screen, which
      is where the exposure actually is. Then trigger a second swap and confirm you now have TWO
      notifications stacked under Sessiometer rather than one replacing the other (the per-post identity),
      and that they group under the app rather than into sub-threads.

Not on this list on purpose: what the notification SAYS, and whether a label could leak into it. Both are
measured in `NotificationDeliveryTests` against the `NotificationDeliveryPlan` the presenter copies onto
the notification, with a source pin holding the presenter to that plan — so neither needs a human. Read
the pin for what it does and does not cover: it catches an added field, a KVC or `userInfo` write, and a
substituted value on `title`/`body`; it cannot see an assignment made through a local alias or a helper in
another file, because `UNUserNotificationCenter` keeps the presenter out of the test bundle.

## It's a mock, not code

The mock approximates native treatments in HTML/CSS. When building the SwiftUI panel, translate
each to its native equivalent rather than copying the CSS literally:

| Mock (HTML/CSS)              | Native (SwiftUI / AppKit)                    |
|------------------------------|----------------------------------------------|
| `backdrop-filter` vibrancy   | `NSVisualEffectView` material                |
| hex colors                   | system semantic `Color` / `NSColor` — **except** the health / warning tints (see below) |
| tabular numerals             | `.monospacedDigit()`                          |
| health glyph (drawn SVG)     | SF Symbol **template** image (shape, not color) |

The hex values and pixel metrics are **directional**, not targets — with one exception.

**Exception — the health / warning tints are exact tokens (#388).** The system semantic warm colors
(`.yellow` / `.orange` / `.red`) fail WCAG non-text/text contrast on the translucent vibrancy (system
yellow ≈ 1.2:1 there), so the in-panel auth-glyph tint (`healthColor`), its dead cue, and the meter
`%`-text (`pctColor`) resolve to **asset-catalog color sets** — `HealthOK` / `UtilGreen` / `UtilAmber`
/ `UtilOrange` / `UtilRed`, mirroring the mock's `--ok` / `--ut-*` families with Any/Dark **plus
Increased-Contrast** variants. For these, the mock hex values ARE the targets, not directional. The
meter **bar fill** (`barColor`) stays on the bright system colors (≈ the mock's `--u-*` fill family): a
bar is a non-text fill (3:1), so it needs no darker tint. The menu-bar status-item glyph is unaffected —
it is a monochrome **template** image (shape-encoded, `StatusGauge`), never health-tinted.

## The 9 states

Healthy (status + stats, both themes), daemon-starting, not-running, crash-looping,
disconnected (stale), stale-snapshot, keychain-locked, version-skew, empty-roster/first-run.
Each state has a **distinct panel message + affordance** under the shared **4-state glance
glyph** (#524: ✓ healthy · … connecting · ! attention · ∅ no-runway) — several panel states
share one glyph; the panel never renders healthy on a degraded daemon.

## Design constraints the mock honors

- **Identity** — each row leads with the account's operator-chosen **label** (never the email;
  defaults to the account UUID when unset), provider on a quieter secondary line.
- **Provider-neutral** — the badge carries a per-account **identity color + smart 2-char monogram** (#445):
  a generic low-chroma disambiguation cue (a fixed palette, accent hue excluded, seeded from the `label`),
  **not** a provider brand color/logo, and never color-alone (always paired with the monogram + label text,
  WCAG 1.4.1). Same-local-part rosters stay legible via the monogram's distinguishing token + middle-truncation.
- **Capture is a real action; copy-command only where the app can't act** — first-run / empty-roster
  onboarding captures the active account in-app (#360), sending the verb over the #358 control socket
  and rendering an honest pending → done → error (redacted ack; no credential ever reaches the client);
  the captured row arrives on its own via the live `watch` stream (the affordance never inserts it). It's
  an onboarding affordance: a populated panel carries no capture bar, so adding an account lives off-panel.
  Version-skew still offers a `brew upgrade sessiometer` **copy-command** (the app can't self-update), and
  daemon-starting shows a static "forming" glyph — the app fakes no progress it isn't doing.
- **Honest state** — disconnected rows are dimmed + "stale", never frozen-as-live.

## The status-item glyphs (#437)

Distinct from the panel above: the four **menu-bar status-item** glyphs (the #524 attention axis —
healthy / connecting / attention / no-runway) are the bespoke **Cycle-Gauge** mark redrawn at bar size, a
shared open-arc + arrowhead **chassis** plus one bold interior mark (`✓` / `…` / `!` / `⊘`). They ship as
custom SF Symbol `.symbolset`s — emitted by `brand/generate.sh` into `Sources/Assets.xcassets`, loaded by
`StatusGauge.swift` via `NSImage(named:)` as monochrome **template** images.

`status-glyph-preview.html` renders that artwork (the same 24-grid SVG the `.symbolset`s ship), light +
dark, at bar-relevant sizes:

```sh
open status-glyph-preview.html            # interactive
# committed raster (GPU headless Chrome, per the render note above):
"/Applications/Google Chrome.app/Contents/MacOS/Google Chrome" \
  --headless=new --hide-scrollbars --force-device-scale-factor=2 \
  --window-size=1080,560 --screenshot=renders/status-glyphs.png status-glyph-preview.html
```

![Bespoke status-item glyphs, light + dark](renders/status-glyphs.png)

**⚠ The preview is an artwork reference, NOT the distinctness gate.** A browser raster can't settle bar-size
legibility; #437's PRIORITY-1 acceptance test is an on-device **16 px `NSStatusItem`** capture (light + dark,
Increase Contrast, over a bright wallpaper, beside system icons). Capture it from the app's DEBUG gallery —
`SESSIOMETER_GLYPH_GALLERY=1` installs the four real glyphs side by side in the menu bar to screenshot:

```sh
# from apps/menubar, after a Debug build:
BIN=".build/xcode/Build/Products/Debug/Sessiometer.app/Contents/MacOS/Sessiometer"
SESSIOMETER_GLYPH_GALLERY=1 "$BIN"        # then screenshot the menu bar; needs a GUI session
```

By design the shared chassis owns most of the ink, so the four glyphs are close in silhouette at bar size —
whether that is legible enough is the operator's on-device call, not something this proxy decides.

### Bar-glyph render-parity (#525)

The preview above is an artwork reference; the **render-parity pass** below is the automated gate. It
renders each bar glyph as it actually appears in the menu bar — the AppKit **template tinting** that the
system applies and that `RenderPanelTool` / SwiftUI `ImageRenderer` cannot capture — and diffs the fresh
renders against committed references so the set stays green as the mark / geometry evolves.

Three tint **contexts** × four glyphs × {@1x, @2x} = **24** references under `renders/bar-glyphs/`
(`bar-<glyph>-<context>@<scale>x.png`):

- **light** — `labelColor` (near-black) on a light bar;
- **dark** — `labelColor` (near-white) on a dark bar;
- **menuOpen** — the inverted highlight: `selectedMenuItemTextColor` (white) over the pinned accent.

<img src="renders/bar-glyphs/bar-healthy-light@2x.png" width="48"> <img src="renders/bar-glyphs/bar-healthy-dark@2x.png" width="48"> <img src="renders/bar-glyphs/bar-healthy-menuOpen@2x.png" width="48"> — healthy, the three contexts @2x (the other three glyphs mirror this).

Regenerate the references with a DEBUG-only tool (`RenderBarGlyphTool`, wired in `AppDelegate` beside
`--render-panel`). It rasterizes headless via `NSBitmapImageRep` + `NSImage.draw`, and it runs inside
the app so `Bundle.main` carries the compiled catalog. (This previously read "unlike the panel renderer
it needs no windowserver" — a false contrast, corrected above per issue #749: the panel's `ImageRenderer`
rasterizes headless too. What actually sets this path apart is the template tinting named above.)

```sh
# from apps/menubar, after a Debug build (xcodegen generate && xcodebuild build -scheme Menubar …)
BIN=".build/xcode/Build/Products/Debug/Sessiometer.app/Contents/MacOS/Sessiometer"
"$BIN" --render-bar-glyphs "$PWD/design/renders/bar-glyphs"
```

These renders carry **no account data** (they are bare glyphs), so they are safe to commit, and are.

The drift gate is `Tests/BarGlyphParityTests` (CI-enforced under the `swift` job, headless): it re-renders
every cell and asserts each matches its reference, that the four glyphs stay pairwise distinct in every
context (the inherited #437 shape-distinctness check, now automated — including the risky **light** /
black-ink case), and that a deliberately perturbed render trips the gate (a golden test that cannot fail is
not evidence). If a shape change is intentional, regenerate the references with the command above and
re-eyeball them before committing — a reference you have not looked at is not a reference.
