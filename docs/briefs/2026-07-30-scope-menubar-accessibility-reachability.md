---
type: scope-brief
date: 2026-07-30
workflow: /scope
source: session context (#817 thread)
status: final
---

# Scope Brief: Menu-bar Accessibility Reachability

Your question — *"why is that unreachable?"* — turned out to sit on top of a structural gap, not a
missing line of code. Eight items, now tracked, behind a PRD and a solution design.

## Problem

Every accessibility gate in the app supplies its **own in-process stimulus**, so the OS→app delivery
path lies outside all of them. That makes one state invisible to the whole apparatus: **built, gated,
and unreachable**. `PanelTypeScale` is in it today, with the suite green.

## The items

| # | Item | Ready | Blocked by |
|---|---|---|---|
| **#971** | *spike* — measure the delivery path as a **2×2** | ✅ | — · **gates the cluster** |
| **#981** | *spike* — can a predicate tell a variable injection from a literal? | ✅ | — |
| **#982** | *feat* — the producer-side **reachability gate** | ✅ | #971, #981 |
| **#896** | *test* — element overlap at every size class | ✅ | — |
| **#983** | *test* — the panel has never been rendered at its own ceiling | ✅ | — |
| **#984** | *docs* — the mock authors no frame at any scaled class | ✅ | — |
| **#868** | *feat* — the three accessibility display settings | ✅ | — |
| **#817** | *feat* — the driver + storage fork | ⚠️ `SPIKE`-dep | #971 |
| **#845** | *fix* — Settings cells don't scale with their fonts | ⚠️ `ACCEPTED_GAP` | #971 (**premise**) |

Seven can start now. Two wait on #971 — and #845 waits on it for a reason worth reading below.

### Suggested sequence

1. **#971** first, always. It selects the driver, sets #982's polarity, and confirms-or-voids #845.
2. **#981** in parallel — independent, and #982 needs its answer.
3. **#983** and **#984** any time — both independent, both cheap, and #983 should land *before* a
   driver ships so the ceiling is verified before it becomes reachable.
4. **#896** any time.
5. **#982** once both spikes land.
6. **#817**, then **#845**, once #971 decides.
7. **#868** any time — policy is ratified; verify the Reduce Motion premise first.

## Key Decisions

1. **Reframed from "nothing drives Dynamic Type" to a verification-tier gap** (you ratified). The
   narrow statement is a symptom; the root is that gates sit *downstream of their own injection point*.
   This is what makes #982 prevention rather than an add-on.
2. **#817's AC-3 superseded** — it read "option 2 ships", pre-deciding the answer the spike exists to
   inform. Its body is amended, not just commented, because an executor reading only the issue would
   have shipped the pre-committed option.
3. **The gate ships under every outcome — one item, two shapes.** Positive gate if a driver ships;
   **defect pin** if none does. Both have in-tree precedent. This is why "ship no driver" is cheap and
   why refusing to pre-commit costs nothing.
4. **A-4 resolved — the gate is feasible.** The exclusion of `StatusItemController.swift` from the test
   bundle bars a *compiled* gate, not a *source-as-data* one: `PanelDynamicTypeLintTests` reads that
   exact file today, because it must in order to exempt it. Same mechanism, same file.
5. **#868's policy ratified, and both halves reframed as measurement-gated** — see below.
6. **Storage recommendation: client-local**, on one argument — under daemon storage the preference
   becomes unreadable *exactly when the panel is already degraded*. Ratification-pending.

## What the pipeline changed versus the raw issue

Running the stages was not ceremony; five things came out different.

1. **#817's central claim is an inference, not a measurement.** "macOS does not populate
   `\.dynamicTypeSize` from a system setting" was read off a probe that measured the *injected
   environment value* across twelve classes. The **OS setting has never been measured.**
2. **Two committed sources contradict each other and one is wrong.**
   `StatusPanelTypeScale.swift:11-13` measured relative text styles inert;
   `SettingsTextMetricsTests.swift:639` infers they "**therefore** DO grow". A probe result versus a
   *therefore*. That is why #845 is premise-gated rather than scheduled — **it may be latent, not
   live**, and its "degrades as the setting increases" framing may need correcting instead of coding.
3. **The activation policy is the candidate mechanism, and it reconciles them.** The panel is always
   `.accessory`; **Settings promotes to `.regular`** while open. That single fact turns #817's
   open-ended question into a clean 2×2 and lets one spike decide two issues.
4. **#817's spike was pointed at a key that doesn't exist.** It asked "under which Info.plist opt-in
   key" — this app declares no `LSUIElement` key at all; it calls `setActivationPolicy(.accessory)` at
   runtime. A key-hunting probe returns "unavailable" **incorrectly**, and the old AC-3 would have
   converted that error straight into shipped scope. Highest-scored risk in the register, 9/9.
5. **#868 shrank.** Both "blocking questions" were measurement-gated, not taste-gated. Reduce
   Transparency: macOS substitutes the opaque fill *itself*, so the real question is whether the tuned
   elements survive it. Reduce Motion: the affordance is a **stock `ProgressView()`** — no custom
   animation — so the platform may already honour it and that half may close as **not-a-defect**.

## Stats

- **Work items**: 8 (5 new · 1 body-amended · 2 scope-commented) in GitHub
- **Ready**: 7/9 · **Typed exceptions**: 2 (`SPIKE`-dep, `ACCEPTED_GAP`) · **Deferred**: 1 (storage)
- **Assumptions**: 0 green / 4 yellow / 3 red — and two of the reds contradict each other
- Coverage gate: **PASS-WITH-FINDINGS** (3 findings, all pre-tracked) · Feasibility **PASS** · Risk **PASS**

## Artifacts Produced

**Assertion**: 6/6 declared artifacts verified, 0 amended-absent, 0 repaired

- **PRD** — `docs/requirements/menubar-accessibility-reachability.md` (`dor_status: passed-with-findings`)
- **Solution design** — `docs/design/menubar-accessibility-reachability-solution-design.md` (`draft` — 3
  load-bearing open questions remain; **not** a locked design, and marking it `final` would be a false lock)
- **Requirements brief** — `docs/briefs/2026-07-30-requirements-menubar-accessibility-reachability.md`
- **Design brief** — `docs/briefs/2026-07-30-design-menubar-accessibility-reachability.md` (`draft`)
- **Feature files** — 4 in `docs/specs/` + 5 typed exceptions recorded
- **Work items** — 8 in GitHub
- *No `user-stories` document* — requirements are EARS + OOUX; story-shaped work is tracked items.

## Open — needs you, not measurable

- **Ratify client-local storage** (design § 5.3). Only material if the in-app preference ships.
- The design stays `draft` until **#971** lands. That is honest, not incomplete.

## Next Steps

- `/do #971` — the spike that unblocks four items and may void one
- `/do-all` — seven items are ready now
