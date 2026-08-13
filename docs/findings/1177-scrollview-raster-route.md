# Finding #1177 — can anything rasterize the panel's scroll boundary, and is it worth switching to?

**Verdict: YES — `ImageRenderer`'s blind spot is not the platform's.** `NSHostingView` drawn into an
explicitly-sized `NSBitmapImageRep` rasterizes a `ScrollView`'s content that `ImageRenderer` renders
blank, at a scale that does not depend on the display. Issue #1177's **route 1 is live**, and the two
alternatives it offered are not competitive against it.

**Decision: take route 1. The gap is NOT accepted.** Execution is deferred to **#1261**: the two rigs
disagree by ~22× the golden gate's own drift ceiling on identical non-scrolling content, so the 44
committed goldens are expected to re-baseline and the suites that pin constants off the render path to be
re-derived. That is a migration, not a spike's tail.

This is a findings note rather than an ADR deliberately: the decision **selects a route and commissions
work**, and nothing about the shipped architecture has changed yet. An ADR written now would record a
rasterizer the repo does not use. If #1261 warrants one, it belongs to that change.

## What was asked

Issue #818 shipped the panel's scroll boundary and, with it, a bypass: `PanelRenderHarness` rasterizes the
panel with `\.panelScrollBoundaryEnabled` **false**, because rendering with it produces 44 PNGs of a header
and tab bar over an empty body — and blank bodies diff clean against each other, so the golden gate would
report green while seeing nothing. The consequence is that **no raster gate covers the boundary**: not that
it clips where it should, not that its scroll indicators look right, not that the pinned chrome sits where
the design expects against a scrolled body.

Issue #1177 offered three routes and said outright that none was obviously right. Route 1 was empirically
settleable and had never been settled.

## The measurement

Subject throughout: six rows of black text over white, 200 pt wide — the same body #818 measured, so the
`ImageRenderer` column below is a reproduction rather than a fresh claim. Ink is `PanelRaster.inkCoverage`
(departure from the corner pixel, summed-RGB delta > 0.15 × 255).

**Rig A** is `ImageRenderer`, what the harness ships. **Rig B** is `NSHostingView` + `cacheDisplay(in:to:)`
into an `NSBitmapImageRep` sized as the next section describes — the rig the committed test runs. Every
rig-B figure in the table is from that one construction; the two places further down that cite the earlier,
windowless construction say so explicitly.

| subject | rig A `ImageRenderer` @2x | rig B `NSHostingView` @1x | rig B @2x |
|---|---|---|---|
| plain body, no scroll view (**CONTROL**) | **0.0988** | **0.1081** | **0.1480** |
| in a `ScrollView`, `fixedSize` | 0.0000 | — | — |
| in a `ScrollView`, viewport 200×120 — *shorter* than content | 0.0000 | **0.1099** | **0.1505** |
| in a `ScrollView`, viewport 200×300 — ***taller*** than content | 0.0000 | **0.0440** | **0.0602** |

**Both controls carry ink, which is the load-bearing half.** A rig that drew nothing at all would score
zero on a `ScrollView` too, and that zero would look exactly like evidence. Rig A's zeros mean something
only because rig A draws the same body at 0.0988; rig B's non-zeros mean something only because rig B is
measured on that same body in the same run. (Ink rises with scale because antialiasing puts more partially
inked pixels over the threshold — it is not a second subject.)

The taller-viewport figure is worth its own line, because it is the internal consistency check. The body
lays out at **122 pt** (`NSHostingView.fittingSize`). Diluting the control across a 300 pt viewport predicts
`0.1081 × 122/300 = 0.0440` — which is exactly what was measured. Rig B is not drawing *something* in
there; it is drawing the content, at the top, with the remainder correctly empty.

Rig A's blankness is the container, not the bound: a viewport *taller* than its content renders just as
blank as one shorter than it. That verdict is unchanged and stays pinned by
`PanelScrollBoundaryTests.testImageRendererStillCannotDrawAScrollView`.

### Scale does not have to come from the display

The golden gate needs machine-**independent** rasters. AppKit's own backing scale follows the screen, and
on the measuring machine `NSScreen.main.backingScaleFactor` is **1.0** — so a rig that inherited it would
emit @1x goldens here and @2x on a Retina machine. Measured: a windowless hosting view returns a 200×300 px
rep for a 200×300 pt view, and putting it in an offscreen `NSWindow` does not help (that window also reports
1.0).

Sizing the rep in **pixels** while setting its **point** size separately removes the display from the
answer. This is not a new technique — `BarGlyphRenderer.newRep` already rasterizes the status-item glyph
that way, which is most of why route 1 is credible rather than merely possible.

Measured, the raster dimensions then track the argument exactly: a 200×300 pt view rasterizes 200×300 px at
scale 1 and **400×600 px at scale 2**, on the display described above. The dimensions are the assertion —
the ink figures in the table above are only there to show the larger raster is not blank.
`cacheDisplay(in:to:)` and `displayIgnoringOpacity(_:in:)` produced identical figures on every cell they
were both run against (control and `ScrollView` 200×300, at both scales).

The earlier, simpler construction — `bitmapImageRepForCachingDisplay(in:)`, which lets AppKit size the rep
— is what exposed the problem: it drew the `ScrollView` fine (0.1077 control, 0.0438 at 200×300) but only
ever @1x, which is how the display dependency surfaced at all.

### Determinism

Six consecutive renders of the same `ScrollView` — through the windowless construction, before the scale
problem was solved — were **byte-identical**: 0 bytes differing, no settling. `PanelRenderHarness`
currently carries settling machinery for `ImageRenderer`'s start-up transient (#824), where the first
renders in a process differ from later ones by ±1/255 on ~905 of 2 729 920 bytes. On this subject the
AppKit rig has no such transient. **Measured on the synthetic body, on the construction the committed test
does not use, and at a sample depth of six** — so it bears on #1261's scoping and settles nothing; see the
boundary below.

Sample depth is the sharpest of those three bounds, because six renders cannot reach the class of transient
#824 actually found, which is not confined to start-up: `PanelRenderHarness` records that a plateau can
appear at ANY point in a long run, on content already rendered many times in the same process — surveyed
across a whole suite in measurement mode, its plateaus land at rasterizer passes **#48, #648, #656 and
#1872**, the deepest two orders of magnitude past six. A run that shallow would have missed every one of
them on the rig they were measured on. "No such transient" here therefore means none was seen at that
depth, not that the rig has none.

## What it costs: the goldens re-baseline

The tempting hope is that the two rigs agree on ordinary content and only diverge on `ScrollView`, which
would leave the committed goldens untouched. They do not.

On identical **non-`ScrollView`** content, at the same scale, at the same raster dimensions (400×244 px):

- **47 862 of 390 400 bytes differ — 12.26 %**
- **`PanelRaster.diffFraction` = 0.044744**, the golden gate's own metric

Against that gate's `driftCeiling` of **0.002** — set just under the measured closest real fixture pair,
**0.002513** — the disagreement is **~22× the ceiling** and ~18× the separation between two genuinely
different panel states.

**That the 44 committed goldens all move is an inference, not a measurement, and is marked
as one here** — measuring it directly means rendering the panel through rig B, which is the migration.
What is measured is that the two rigs disagree by 22× the ceiling on the *simplest content there is*, text
over a flat background, which is what the goldens are largely made of. The inference is strong enough to
size the work and it is the same one #1177's own acceptance criteria already assume ("the goldens are
re-blessed"), but a reader should not cite this note as having rasterized them.

Eight test suites reference `PanelRenderHarness.render` or `.scale`; most pin constants measured off the
rig, and those are re-derivations rather than re-baselines. Not all do — `PanelTypeScaleTests`' render-path
assertions are relations and arithmetic off `PanelMetrics.width` and the *published* Dynamic Type
progression — so the eight is the surface to audit, not a count of constants to re-derive.

That is the whole reason this finding does not also carry the fix.

## Why not the other two routes

- **Route 2 — render the boundary's content at natural size and compose.** It makes the golden show what
  *scrolling would reveal*, which is a different artifact from what the popover shows. The gap #1177 names
  is that nobody checks the boundary *clips where it should*, its indicators, and the pinned chrome against
  a scrolled body — and a composed full-height body has no clip, no indicators, and no scrolled body. It
  would answer a question nobody asked while leaving all three open, and it would need its own bespoke
  composition code that the shipped panel never runs. Route 1 needs neither: it rasterizes the real tree.
- **Route 3 — accept the gap, gate it manually.** Legitimate before this measurement, and the honest answer
  had rig B come back blank. It does not: a mechanical gate is available, using a technique already in this
  repo, so trading it for a recurring human obligation would be choosing the weaker instrument on cost
  grounds alone. Route 3 also compounds — the manual pre-release checklists are already long, and every
  item added to them is paid on every release forever, whereas route 1's cost is paid once.

**The interim is unchanged and un-gated.** Until #1261 lands, the boundary's appearance has no raster gate
and the design-parity capture at `.accessibility3` still shows more than the popover does. No manual
checklist entry was added, deliberately: a checklist is route 3's deliverable, and adding it alongside a
route-1 decision would record two contradictory answers to the same question.

## Boundary: what this did NOT measure

Stated explicitly, because each one could invert the cost side of the decision and none is settled:

- **Whether the whole panel renders through rig B at all.** The subject was a synthetic body. Reaching the
  real panel requires a seam into `PanelRenderHarness`'s view construction — migration work, not
  measurement — and the panel carries environment injection (`.statusPanelEnvironment`), a pinned `.tint`,
  `colorScheme` and `dynamicTypeSize` that all have to survive the swap. **#1261 must establish this
  first**; if the panel does not render faithfully, the rest of that item is moot. Nothing here licenses
  the claim that it will.
- **Colour space.** Rig B's rep is `.deviceRGB`; the goldens normalize to sRGB. The round-trip is untested,
  and it is a plausible source of the cross-machine drift the current rig is not known to have.
- **Cross-machine behaviour.** One machine, one OS, one Xcode. The `driftCeiling` comment already records
  that cross-machine antialiasing drift is unmeasurable from a single machine; that limit applies here too.
- **Whether the settle machinery can go.** The determinism result is about the synthetic body. Re-measure
  on the panel before removing anything.

## Provenance

- Measured at `aa17e72`, macOS 26.5.2 (25F84), Xcode 26.6 (17F113), on a display reporting
  `backingScaleFactor` 1.0.
- The controlled rig-A-vs-rig-B comparison is committed and executable as
  `PanelScrollBoundaryTests.testAppKitDrawsScrollContentWhereImageRendererDoesNot`, with both canaries.
  It was mutation-checked on each of the rig's three load-bearing lines: removing rig B's `cacheDisplay`
  reddens the control and every `ScrollView` case; dropping the scale multiplier reddens only the scale
  assertion; deleting `rep.size = size` reddens the rep's point-size assertion and the cross-scale ink
  check, and nothing else. That third line is why the test pins the POINT-size half of the technique as
  well as the pixel half — without it the rep reports its pixel dimensions as its point size, the content
  is drawn at unit scale into a corner of a correctly-sized raster, and a dimensions-only gate stays green
  (measured: ink coverage 0.0440 → 0.2486 from scale 1 to 2, against 0.0440 → 0.0602 for the correct rig).
- The cross-rig churn figures (12.26 %, 0.044744) and the scale/determinism sweeps were measured by
  standalone probes and are **not** committed — they bear on the migration's cost, which #1261 owns and
  will re-measure on the real panel. The committed test pins the facts the decision rests on.
- `driftCeiling` (0.002), the closest real pair (0.002513) and the golden count (44) are read from
  `PanelGoldenParityTests` and the committed golden directory, not restated from memory.
- Cross-checks: #818 (the boundary and its bypass), #755 (the roster-cardinality axis),
  `apps/menubar/design/README.md` § The scroll boundary, ADR 0031 (UI verification tiers bounded by
  structural blindness).
