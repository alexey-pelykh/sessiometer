---
title: Stats Honesty — Solution Design
source: docs/requirements/stats-honesty-cross-surface.md
created: 2026-08-04
status: locked   # operator-ratified; dual-lens gate was single-actor (see § Design Lock)
tracks:
  technical-architecture: complete
  ux-ia: complete
  ui-visual: complete
  testing-architecture: complete
---

# Solution Design: Stats Honesty — Coverage Gating, Runway Degeneracy, Cross-Surface Parity

**Input PRD**: `docs/requirements/stats-honesty-cross-surface.md` — `dor_status: passed-with-findings`
(proceeded; findings are three INCOMPLETE features, all of which this design resolves or spikes).
**Tree**: `cb3eaca`. **Requirements**: R-1 … R-21. **Items**: 11.

> **Ratification status.** The Design Lock Gate normally dispatches `product-strategist` and
> `ux-architect` as two independent ratifying lenses. **Agent dispatch was unavailable in this session**,
> so both lenses were applied analytically by a single actor and the operator is the ratifying authority.
> This is recorded as **single-actor ratification**, not claimed as independent dual-lens — see § Design Lock.

## 1. Goals and Drivers

Every roster-level figure the operator reads is either correct or says it does not know — and stays
*measurable* in the condition it exists to report. The driver is a lived failure: on a week of total
fleet downtime, the census fabricated a zero, the runway fabricated 648427 days, and the one metric
that explained the downtime was CLI-only.

## 2. Constraints

| Constraint | Source | Effect on design |
|---|---|---|
| No schema/wire change | PRD § 1b.3 (verified) | All render work consumes fields already serialized |
| No forecast language | REQ-STA-B-006 + SUR-001 `[keystone]` | Bans "project", "expect", "will last"; descriptive phrasing only |
| Hold duration is a BOUND | REQ-STA-B-011 | Panel capacity-holds render `≥10h22m`, never an exact figure |
| STATE-parity, not glyph-parity | `design-menubar.md` R-2 | Per-medium vocabulary may differ; the STATE may not |
| Panel goldens / CLI goldens | project CLAUDE.md § If you touch X | `Panel-Goldens-Rebaselined:` / `CLI-Goldens-Rebaselined:` trailers |
| Daemon fail-open | REQ-STA-B-001 | Fault recording must never block, delay, or alter the swap loop |
| Never wrap a row | `design-stats.md:134` | CLI degradation discipline; precedent for the panel, not binding on it |

## 3. Context and Scope

Two render surfaces (`stats` CLI `roster_line`/`fleet_line`; SwiftUI Stats tab `StatusPanelFormat`)
over one aggregation core (`usage_stats.rs`) reached through one wire (`RosterWire`/`FleetRunway`),
plus one parity contract (`cross_surface.rs` + byte-pinned manifest) and one build reference
(`menubar-preview.html`). The daemon consumes the same runway via `current_fleet_runway()`.

## 4. Solution Strategy

**One principle, applied at three layers.** Every reported figure has a *precondition for being
meaningful*. The strategy is to make each precondition (a) **computable honestly** (R-18 — the census
was structurally blind), (b) **consulted at every render** (R-1/R-8/R-9/R-17), and (c) **pinned by a
contract neither surface can move alone** (R-11/R-12).

Layering matters: fixing renders without (a) yields a permanently-UNKNOWN census; fixing renders
without (c) leaves the next metric to repeat the defect.

## 5. Building Blocks — the design decisions

### D-A (was D2/D3) — Runway degeneracy: refuse, never clamp

`stats.rs:1107` today: `(total_rate > 0.0).then(|| (total_headroom / total_rate).round() as i64)`.

**Decision.** Replace the exactly-zero test with a *result-side* plausibility gate, not an input-side
epsilon, and eliminate the saturating cast:

1. Compute the quotient in `f64`.
2. Reject unless the result is **finite** and within a **derived** plausibility bound.
3. Convert with a checked/`try_into` path; any failure → `None`. Never `as i64`.

**Why result-side rather than an input epsilon.** An absolute epsilon on `total_rate` is arbitrary and
unit-coupled (the EMA's native unit is fraction-per-second, so a "small" constant is unreadable and
drifts if the unit changes). The result-side bound is *derivable* and self-documenting: weekly windows
**reset**, so a runway materially beyond one such window is not a longer runway — it is a statement the
metric cannot make. The exact bound is derived immediately below.

**Bound — A-3 RESOLVED 2026-08-04: ONE weekly window (~7 d), not roster × window.**

Derivation, from `stats.rs:986`: `weekly_headroom = (weekly_ceiling - last.weekly).max(0.0)` — a usage
**fraction**, and `weekly_rate` is fraction-per-second, so `headroom / rate` is seconds (dimensionally
sound). The computation **ignores replenishment entirely**: it asks "how long to drain the pooled
head-room at the pooled burn", as if no window ever reset. But every account's weekly quota resets on
its own ~7-day cycle. **Therefore any runway exceeding one weekly window asserts the fleet drains with
no reset intervening — which cannot happen.** That is the principled ceiling, and it is independent of
roster size.

*(An earlier draft of this design proposed `roster_size × weekly_window` = 42 d. That was too loose by
a factor of the roster size: pooling head-room across N accounts does not extend the horizon past the
first reset, because the resets are what refill the pool.)*

A tighter variant is available if wanted — the soonest carried `weekly_resets_at` among counted
accounts — but the fixed one-window bound needs no additional data and cannot go stale.

**This is a refusal threshold, not a clamp** (premortem P7): the figure is never displayed *at* the
bound; it is withheld.

**Rejected**: clamping to a large plausible number (P7 — converts an obvious lie into a credible one);
`saturating_cast` (same defect, renamed); widening to `i128` (moves the boundary, does not remove it).

### D-B (was D4) — Unrepresentative counted set: honesty first, correctness spiked

**Decision — ship the honesty half now, spike the correctness half.**

- **Now (R-5, R-20)**: state the counted set wherever the runway is reported. Already correct on the
  CLI (`(1 of 6 counted)`); the work is panel-side (R-17).
- **Now (R-6)**: refuse when the counted subset excludes every account at or near its weekly ceiling.
- **Spiked (A-7, HIGH)**: whether that predicate leaves the runway reportable at all.

**The spike is mandatory and its shape is fixed**: replay the real on-disk history through the
candidate predicate and count the fraction of windows in which it would still report. A predicate that
reports in <20% of windows is a feature removal (premortem P4), and DG-2 then applies: descope to
R-5 + R-20 (state the counted set; print the line) and stop.

**Operationalising "at or near its weekly ceiling"**: reuse the daemon's **own** viability boundary —
the shared helper REQ-STA-B-010 already mandates (`weekly ≥ weekly_effective_ceiling`). Do **not**
introduce a second, independently-chosen water; that is the exact drift B-010 exists to prevent.

### D-C (was D1) — UNKNOWN vocabulary, both surfaces

**Constraints given by the operator, both verified against code:**
- **R-21** — no implementation vocabulary. `covered` is a field name; the CLI's `, 64% covered`
  (`stats.rs:1453`) has the same defect as the proposed panel copy did.
- **R-20** — the line is printed in every state. `fleet_line` (`stats.rs:1729`) currently emits the
  runway **only** under `runway_secs: Some(_)`, so R-3 would otherwise make it vanish more often.

**Decision — author-chosen, explicitly correctable** (the operator declined to pick from a menu; these
satisfy the two constraints they did state):

| State | Both surfaces render |
|---|---|
| Census, unmeasurable | `not measurable — never saw all 6 at once` |
| Census, partially seen | `3 episodes (1h40m) — all 6 in view 64% of the week` |
| Census, genuinely quiet | `0 episodes (0s)` |
| Runway, no measurable burn | `accounts last: unknown — no measurable combined burn (1 of 6 counted)` |
| Runway, implausible/saturated | `accounts last: unknown — implausible result, recorded as a fault (1 of 6 counted)` |
| Runway, unrepresentative subset | `accounts last: unknown — counted accounts are not the ones near their limits (1 of 6 counted)` |

Rationale: names the actual condition in reader terms; identical on both surfaces so no hover is
needed (issue #950 found `.help()` unreliable on a disabled control, so a hover-dependent design would
be load-bearing on a known-shaky affordance); contains no forecast verb, clearing REQ-STA-B-006.
"never saw all 6 at once" derives its count from the roster, so it stays true as accounts are added.

**Open**: the exact strings are copy, not architecture — correct them freely without reopening this design.

### D-D (was D5) — Panel roster-block layout: ONE decision for R-9 + R-17

The block goes from one fact to three. **Decision**: a stacked label/value list, one fact per row,
rather than the CLI's single `·`-joined line. Rationale: the three facts now carry variable-length
qualifiers (D-C), which is exactly what forces wrapping in a joined line; the panel has vertical room
the terminal row does not; and AC-10 forbids wrap/truncation. The CLI keeps its joined line and its
existing degradation discipline — this is per-medium vocabulary, permitted by R-2, with the STATE identical.

### D-E (was D6) — Parity contract: PIN, with a mandatory UNKNOWN case

**Decision: PIN**, not declare-uncovered. Declaring it uncovered is honest but leaves the root cause
(R-11's whole purpose) unaddressed — the manifest exists so neither surface can move alone, and this
axis is precisely where they moved apart. **R-12 is binding**: at least one pinned case must expect
UNKNOWN, or parity would pass with both surfaces printing `0`.

**Pinned cases (minimum set)**: census {unmeasurable, partially seen, quiet} × capacity holds
{unmeasurable, held} × runway {unknown-no-burn, unknown-implausible, finite}. DG-3 gate: confirm
pinning needs no wire change before committing; if it does, surface rather than absorb.

### D-F (was D7) — Fault recording

**Decision**: record at the aggregation boundary, not the render boundary — one fault per computation,
not one per surface. Must use the existing fail-open diagnostic path (REQ-STA-B-001). Note explicitly
in the work item that `fleet_runway_low` is **not** a backstop: a saturated runway is a KNOWN value
above any threshold, so the check silently passes and would emit the RECOVERED leave-edge.

### D-G (was D8) — Test strategy

**Decision**: three layers, semantic assertions only.
1. **Property** — `RunwayPlausibility` over rate ∈ [1e-300, 1e3] × headroom ∈ [0, roster-max]:
   *no input yields a rendered figure outside the bound*. This is the test that would have caught F2.
2. **Unit** — one per state-matrix row (11 rows), asserting **absence of a numeric value** when the
   precondition fails, never a golden string (premortem P6).
3. **Contract** — the two parity conformers over the pinned manifest.

Existing property `prop_all_high_time_never_exceeds_the_jointly_covered_time` must be **extended, not
replaced**, to hold under R-18's widened denominator. AC-9 gate: every test fails against `cb3eaca`.

### D-H (was D10) — Build-reference frames

**Decision**: `menubar-preview.html` gains frames for every panel-reachable state — census
{unmeasurable, partial, quiet}, holds {unmeasurable, held}, runway {unknown×3, finite}. Depends on
D-C and D-D. Rebaselines panel goldens → `Panel-Goldens-Rebaselined:` trailer.

### D-I — Census window anchoring (R-18/R-19), the algorithmic item

**Decision**: mirror `blocked_windows()`'s anchoring in the census path — extend a reading's validity
to its **own carried expiry** where it carries one, exactly as REQ-STA-B-010 ratified for holds.

**Asymmetry is intended**, and it runs in the direction the guarantee does: session utilisation only
climbs within a window, so a saturated peer's `session_resets_at` makes its reading true until that
instant, while a low reading can cross the water at any moment and carries no equivalent. Coverage
extends exactly as far as a reading's own statement reaches, and no further.

> **Rationale corrected by #1097 (2026-08-10); the decision is unchanged.** This section previously
> justified the asymmetry with a second clause — *a low peer "is in rotation and polled every
> `poll_secs`, so it was never blind"* — that the frozen replay corpus contradicts. A peer can be
> session-low *and* polled on the widened exhausted cadence simultaneously, held out of rotation by
> its **weekly** dimension: `a4`/`a5`/`a6` in `build/fixtures/capacity-replay-corpus.tsv` carry
> 44 / 44 / 43 readings across 172 800 s at session peaks 0.03 / 0.15 / 0.00 — low **and** ~7.6 %
> covered. Whether that class can carry a session guarantee of its own is **decided** by
> **ADR-0033**: it stays UNKNOWN, because the candidate guarantee is falsified on the same corpus
> — 17 of 274 weekly-pinned same-session-window pairs show session climbing across the whole roster,
> and, restricted to this class alone, **3 of 28** on `a4`/`a5`/`a6`, every one of them across a
> widened-cadence gap: `a5` climbs 0.05 → 0.15 over 4109 s at weekly 0.99 → 1.00. The roster-wide
> figure is the wider population, so read the class-restricted one as the refutation — and because
> anchoring low readings buys a measured 1.61 % of joint coverage for an assertion of
> known-and-not-high over ~55 pp of unobserved window per starved peer. Mirrored at
> `docs/specs/census-validity-anchoring.feature.md` Rule 3 and
> `docs/requirements/stats-honesty-cross-surface.md` § R-18.

**Invariants that must survive**: UNKNOWN is refined, never repealed — a genuinely dead daemon still
reads UNKNOWN; `all_high_secs ≤ all_high_covered_secs` still holds; no window extends past an expiry
the reading did not itself carry.

**Rejected**: widening `stale_after_secs` globally — weakens coverage uniformly for every account, so a
dead daemon would start reading as covered, cutting against REQ-STA-B-008's entire purpose.

**R-19**: amend the HQ PRD to carry the rule at requirement level, in REQ-STA-B-010's form.

## 6. Runtime View

Unchanged. No new I/O, no new async, no daemon-loop change. R-18 alters interval arithmetic inside an
existing pure synchronous aggregation (REQ-STA-B-005); D-F adds one fail-open diagnostic write.

## 9. UX Architecture

Panel roster block, three stacked facts (D-D), each rendering its own precondition-gated state (D-C).
Vocabulary is shared with the CLI at the **STATE** level; layout differs by medium, per R-2.

## 12. Architecture Decisions

| ADR stub | Decision | Status |
|---|---|---|
| ADR-S1 | Runway refuses on a result-side plausibility bound, never clamps | DECIDED (D-A) |
| ADR-S2 | Census anchors validity to carried expiry, mirroring B-010 | DECIDED (D-I), HQ amendment required |
| ADR-S3 | Parity manifest PINs the roster axis with ≥1 UNKNOWN case | DECIDED (D-E) |
| ADR-S4 | Panel roster block is a stacked list, not a joined line | DECIDED (D-D) |
| ADR-S5 | Unrepresentative-subset predicate reuses the daemon's viability boundary | DECIDED (D-B), gated by the A-7 spike |

## 14. Risks and Open Questions

### Feasibility

| Component | Verdict | Note |
|---|---|---|
| Panel decode + render (R-8, R-9, R-17) | **FEASIBLE** | #804→#805 is an exact in-repo precedent |
| Runway refusal (R-3/R-4/R-7) | **FEASIBLE** | local change to one expression |
| Parity pin (R-11/R-12) | **FEASIBLE-WITH-SPIKE** | DG-3: confirm no wire change |
| Unrepresentative predicate (R-6) | **UNCERTAIN → SPIKE** | A-7; may remove the feature (P4) |
| Census anchoring (R-18) | **FEASIBLE-WITH-SPIKE** | pattern exists (`blocked_windows`); ⊆ invariant must survive |
| HQ amendment (R-19) | **FEASIBLE** | documentation change in a repo we control |

### Risk register

| Risk | L×I | Mitigation |
|---|---|---|
| R-6 makes the runway never report (P4) | 3×3=**9 HIGH** | Mandatory replay spike; DG-2 descope path pre-agreed |
| R-18 breaks the ⊆ invariant | 2×3=6 MED | Extend the existing property test before changing the aggregation path |
| R-18 weakens UNKNOWN | 2×3=6 MED | Explicit AC-11 BUT NOT clause; dead-daemon test case |
| Parity pins the defect (P3) | 2×3=6 MED | D9 ordering — parity lands after the render fixes |
| A-3 bound is wrong | 2×2=4 MED | Verify `weekly_headroom` derivation before pinning the constant |

### Open questions

- ~~**A-3** — the plausibility bound~~ → **RESOLVED 2026-08-04**: one weekly window, derived in D-A
  from the `weekly_headroom` semantics. No longer load-bearing.
- ~~**A-7** — does R-6's predicate leave the runway reportable?~~ → **RECLASSIFIED as SPIKE-1**, not an
  open question. It is a feasibility unknown, and feasibility unknowns route to time-boxed spikes
  rather than blocking a design lock. Specification below.
- **#865** — should the census refuse to report under roster fallback? **Not answered here**, and not
  load-bearing for this design. R-18 changes the measurability landscape #865 was raised against; it
  must be re-read after R-18 lands, never closed by implication.

### SPIKE-1 — Does R-6's predicate leave the runway reportable?

| Field | Value |
|---|---|
| **Question** | With R-6 refusing whenever the counted subset excludes every account at/near its weekly ceiling, what fraction of real windows still report a runway? |
| **Method** | Replay the on-disk history store through the candidate predicate over rolling weekly windows; count reporting windows ÷ total windows. Pure offline read — the same store `stats` already reads, no daemon interaction. |
| **Time box** | One evening. |
| **Success criterion** | A measured reporting rate, with the counted-set composition in the reporting windows. |
| **Decision rule** | ≥ ~20 % of windows report → ship R-6 as designed. Below that → **DG-2 fires**: descope to R-5 + R-20 (state the counted set, always print the line) and close R-6 as "honesty shipped, correctness not reachable in appetite". |
| **Why it cannot be answered analytically** | The predicate's behaviour depends on the empirical joint distribution of staleness and ceiling-proximity across the roster — precisely the thing that surprised us once already (the census blindness). |

## 16. Requirement-to-Track Coverage

| Requirement | Track | Design section |
|---|---|---|
| R-1, R-2, R-8 | Tech Arch, UX/IA | D-C |
| R-3, R-4, R-7 | Tech Arch | D-A |
| R-5, R-6 | Tech Arch | D-B |
| R-9, R-17, R-10 | UX/IA, UI | D-D, D-C |
| R-11, R-12 | Tech Arch | D-E |
| R-13 | UI/Visual | D-H |
| R-14 | — | tracker correction, no design needed |
| R-15 | Tech Arch | D-F |
| R-16 | Testing Arch | D-G |
| R-18, R-19 | Tech Arch | D-I |
| R-20, R-21 | UX/IA | D-C |

No UNCOVERED entries. No PHANTOM elements — every design element above traces to a listed requirement.

## 17. Ordering (D9)

```
1. R-3, R-4, R-7, R-20  (runway refusal + always-print)   ← nothing depends on the defect surviving
2. R-8, R-21            (census gate + de-jargoned copy)
3. R-18, R-19           (census anchoring + HQ amendment) ← makes the census measurable
4. R-9, R-17, R-13      (panel: holds + runway + mock frames, ONE layout pass)
5. R-6                  (unrepresentative predicate)      ← after the A-7 spike
6. R-11, R-12           (parity pin)                      ← LAST, so it pins corrected behaviour
   R-14, R-16 run alongside throughout
```

## Design Lock

**LOCKED** — with one stated caveat.

| Lock condition | Status |
|---|---|
| § 16 forward coverage — no UNCOVERED | ✅ every requirement R-1…R-21 maps to a track |
| § 16b backward coverage — no PHANTOM | ✅ every design element traces to a listed requirement |
| Open-Questions Lock Gate — no load-bearing question open | ✅ A-3 resolved by derivation; A-7 reclassified to SPIKE-1; #865 explicitly not load-bearing here |
| Dual-lens ratification | ⚠️ **single-actor** — see below |

**Caveat, stated rather than papered over.** The gate calls for two *independent* lenses
(`product-strategist` for "is this the right thing to build", `ux-architect` for "is the structure
sound"). Agent dispatch was unavailable in this session, so both lenses were applied analytically by
one actor, with the operator as ratifying authority. Two operator interventions during this stage
(R-18/R-19 scope extension; R-20/R-21 copy defects) materially improved the design and are the closest
thing this run had to an independent lens — but they are not a substitute for the gate as specified.
**Treat the lock as operator-ratified, not independently ratified.**
