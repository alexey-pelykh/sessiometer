# Finding #981 — can a source-as-data predicate tell a variable-sourced injection from a literal one?

**Verdict: YES, within three stated bounds — and only after four traps that the design could not see
from the outside.**

A text predicate over `StatusItemController.swift` can decide whether the panel's construction site
injects a size class from a non-literal source. It is implemented and gating in
`apps/menubar/Tests/PanelReachabilityLintTests.swift` as `PanelReachabilityLint.verdict(in:)`. The
design's non-literal-source clause (§ 5.2) is therefore **satisfiable at T2**, and issue #982 does not
need to fall back to the T3 manual tier for its predicate.

The four traps are the substance of this finding. Three of them produce a **false PASS** — the severe
direction — and two of those are invisible under the positive-gate framing but fatal under the
**defect-pin** polarity that design § 5.2 keeps live pending issue #971, because there `.absent` is
the *green* arm. A predicate that merely fails to see an injection is not merely noisy there; it is
wrong in the direction that lets the defect through.

## What was asked

The reachability gate wants to assert that the construction site injects a size class. The predicate
that suggests itself — *"the file contains `.dynamicTypeSize(`"* — is satisfied by a hardcoded
`.dynamicTypeSize(.large)`. `.large` maps to factor exactly **1.0** (`PanelTypeScale.factor`,
`case .large: return 1.0`), so the panel renders identically to today: the gate would be green over
the exact defect it exists to catch. The design routed this here rather than assume it was tractable
(§ 14, SPIKE-2).

## The core distinction is decidable

A leading-dot argument (`.large`) is an enum-case member reference — a compile-time constant. An
argument starting with an identifier (`sizeClass`, `store.textSize`, `flag ? .large : .xLarge`)
derives from something the program can vary. That is a real textual difference and it is the
live/dead boundary the clause is about.

Three verdicts, all driven by **mutation of the real file** through the same function:

| Mutation spliced into the real chain | Verdict | Gate |
|---|---|---|
| *(none — the shipped file)* | `.absent` | no driver exists yet (issue #817) |
| `.dynamicTypeSize(.large)` | `.deadLiteral` | **FAILS** |
| `.dynamicTypeSize(panelSizeClass)` | `.reachable` | **PASSES** |

## Trap 1 — the range overload, and the spelling that matters is not the obvious one

`.dynamicTypeSize(_:)` is **overloaded**: one arm takes a value (an injection), the other a **range**
(a clamp, which drives nothing). Which way a naive predicate fails depends on how the range is
written, and the two spellings fail in opposite directions:

| Spelling | Leads with | A dot-prefix test reads it as | Direction |
|---|---|---|---|
| `...PanelTypeScale.ceiling` | `.` (trivially — `...` starts with a dot) | literal → **FAILS** | safe |
| `PanelTypeScale.floor...PanelTypeScale.ceiling` | identifier | variable → **PASSES** | **false accept** |

The first is the form `StatusPanelView.swift` ships today. It is *not* the trap: it fails safe. The
second is, and it is why `classify` tests for `...` before anything else and why `clampOnly` is a
first-class verdict rather than an invented case.

> **Correction of record.** The first revision of this finding asserted the opposite — that
> `...PanelTypeScale.ceiling` "does not start with a dot" and so defeats the naive test. That is
> false, and the same claim had propagated into the file header, the commit message, and the doc
> comment justifying the branch order. It is recorded rather than quietly edited because the wrong
> rationale was attached to a **correct** guard: a reader who checked the named example, found it
> already handled one line later, and concluded the range branch was redundant would delete a guard
> that is genuinely load-bearing for the other spelling. A wrong reason on a right guard is how right
> guards get removed.

## Trap 2 — a trailing-closure modifier truncates a naive chain walk

Restricting the predicate to the construction expression means walking the modifier chain. A walk
that only consumes `.identifier( … )` groups stops dead at the first `.onAppear { }`, `.task { }` or
`.background { }` — ubiquitous in SwiftUI, and the shape a #817 driver installing an observer is most
likely to take. Everything after the closure becomes invisible.

Both directions are wrong: a live injection behind `.onAppear` reads `.absent`, and so does a **dead**
`.dynamicTypeSize(.large)` — so chain order alone would decide whether the defect is visible. Under
the defect-pin polarity the first case is a false green. The walk now consumes brace groups, including
the `.onReceive(pub) { … }` form that is a paren group *and* a trailing closure.

## Trap 3 — the construction expression's byte span is not the chain

Filtering injections by whether they fall inside the construction expression's byte range is not the
same as requiring them to be *on* the panel. That span also contains every subview built inside a
modifier's argument list, so `.overlay(Badge().dynamicTypeSize(panelSizeClass))` reads as a live panel
injection while the size class actually reaches `Badge`. A false PASS. Only modifiers at chain
depth 0 count.

## Trap 4 — there are two spellings that reach the panel, not one

`.environment(\.dynamicTypeSize, x)` writes the same environment value as `.dynamicTypeSize(x)`, and
for a value the controller computes it is at least as likely. A predicate keyed on the token
`.dynamicTypeSize(` never matches it — the next character is a comma — so a live key-path injection
reports `.absent`, the defect pin's green arm. Both spellings are recognised, and a dead
`.environment(\.dynamicTypeSize, .large)` is still `.deadLiteral`.

## The bounds that remain — stated, and one of them asserted

Three spellings are accepted as `.reachable` although they are dead. None can be closed without type
resolution, which is the T3 tier's job:

1. **A constant bound through a `let`** — `let dead = DynamicTypeSize.large` … `.dynamicTypeSize(dead)`.
   No text predicate can follow a binding. Asserted by
   `testAConstantBoundToALetIsTheDocumentedBlindSpot`, which splices the binding **and** the injection
   so the assertion actually depends on it — if the predicate ever gains binding resolution, that test
   fails and this note gets updated.
2. **A constant reached through a typealias** — `typealias DTS = DynamicTypeSize` … `DTS.large`. Same
   family, same remedy.
3. **A variable whose source never moves** — see below.

**Severity: low, and bounded by construction.** Bounds 1 and 2 require someone to deliberately
indirect a constant and inject the indirection. The ordinary dead spelling — a literal at the call
site — is caught, including the parenthesised (`(.large)`), whitespace-separated
(`DynamicTypeSize .large`), module-qualified (`SwiftUI.DynamicTypeSize.large`) and cast
(`.large as DynamicTypeSize`) forms.

## What this does NOT prove

The predicate is **syntactic**. It proves the injected value *derives from* a variable; it never
proves the variable's **source** ever moves. Under FORK-1 outcome D-A, an OS observation that never
fires yields a variable-sourced injection rendering k=1.0 for every user: gate green, PRD Matrix
**row 3 still occupied**. Design § 5.2 states this at length and it is restated here because a
finding gets read alone: **a green verdict means WIRED, never DELIVERED.** Delivery is the T3
obligation that issue **#971** measures and **R-5c** scopes the manual checklist step for.

## Provenance

- **Method**: mutation of the real `apps/menubar/Sources/StatusItemController.swift` read from disk,
  in memory, through one function. 29 tests, all green in the full 872-test menu-bar suite.
- **The new guards were themselves canaried.** Reverting each one — trailing-closure consumption,
  `.environment` recognition, argument normalisation, and the range-test ordering — turns exactly the
  tests that cover it red and nothing else. CONSTRAINT-A (issue #748) applies to the fixes, not only
  to the original three outcomes.
- **All eight `.indeterminate` reasons are driven** by a test. `.absent` is a *measurement* that no
  injection is present; `.indeterminate` is the absence of a measurement. Every construct the walk
  cannot traverse — an unbalanced group, an unclosed closure, a chain running off the end of the
  file, a property access mid-chain — resolves to `.indeterminate` rather than ending the walk
  quietly, because a silently-truncated walk reports `.absent`, and `.absent` is green under one of
  the two live polarities.
- **Mechanism reused, not reinvented**: comment/string redaction comes from `PanelScaleLint.scan`
  (`PanelDynamicTypeLintTests.swift`), so the two lints over the same tree cannot drift in what they
  consider code.
- **Access**: `StatusItemController.swift` is excluded from `MenubarTests` in `project.yml`. That bars
  a *compiled* gate, not a *source-as-data* one — the same file is already read this way by
  `PanelDynamicTypeLintTests`, which must, in order to exempt it. Confirms design assumption **A-4**
  (FEASIBLE) empirically rather than by argument.
- **How the four traps were found**: a fresh-context adversarial review of PR #1055 that extracted the
  predicate into a standalone harness and drove ~40 spellings through it. Traps 2–4 and the Trap 1
  correction all come from that pass; the first revision of this note had none of them and claimed
  "one blind spot and one trap". Recorded because it is the load-bearing methodological fact here:
  the author's own four polish rounds found none of them, and the predicate looked finished.
- **Cross-checks**: design § 5.2 (non-literal-source clause, both gate polarities, and the syntactic
  bound); `docs/specs/accessibility-reachability-gate.feature.md`; PRD R-5 / R-5a / R-5b.
- **Scope**: the predicate is decided here. The **gate itself** is issue #982, and its *polarity* —
  positive gate vs defect pin — still waits on issue #971.
