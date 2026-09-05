---
type: scope-brief
date: 2026-09-04
workflow: /scope
source: docs/requirements/daemon-diagnostic-integrity.md
status: final
---

# Scope Brief: Daemon Diagnostic Integrity and the Observation-Gap Readout

## What happened

The operator asked why so many **5-hour session limits** had been missed in recent days. The
investigation answered a different question than the one asked, and the answer is the reason this
scope run exists.

**The recent pain was the weekly limit, not the session limit.** On 2026-08-29 and 08-30 the fleet
spent 11h51m (49.4% of the day) and 9h18m (38.8%) with no serving capacity, and **session-attributable
no-capacity hold time on both days was 0h00m** — all of it weekly. Corroborated on the fleet-state
probe: 4 of 7 accounts at weekly ≥ 0.98 at 2026-08-29 12:00Z. The verdict was **capacity,
weekly-caused**, which is neither of the two readings the investigation set out to test.

Read the **hold-time** figures above and not the session-window counts, on the investigation's own
instruction. The tempting *"most session windows went unused, so the fleet was weekly-blocked"*
reading **inverts**: the observed `peak<10%` share on 08-29 is 38/47 = 80.9%, which sits *below* the
naive structural bound of 6/7 = 85.7% that follows from having one active account — so it shows
**less** window under-use than structure predicts, not more, and it is not evidence for the verdict.
The weekly anchors are what carry it: 12 of 12 anchor transitions measured at exactly 7.0000 d, and
`grep -rn "hard reset\|weekly anchor" src/` returns **no matches** — the daemon holds
`weekly_resets_at` as an opaque API-supplied instant and models no weekly anchor at all.

That verdict routes to `#726`, which already records Q1 as NO-GO and Q2 as
provisioning-deficit-dominated, with `#792` as its named falsifier. **No new item was filed against
it** — no scheduling or diagnostic change reaches a provisioning deficit, and filing one would have
manufactured motion.

What the investigation *did* surface was that answering the question at all was disproportionately
expensive, and that one diagnostic actively lied along the way. Those are what got scoped.

## What was created

**Four new items:**

| # | Item | Why |
|---|---|---|
| **#1486** | `(feat) daemon: stamp the running build's identity into the event log at startup` | Establishing which binary produced the 08-28..08-31 behaviour took **three independent indirect proofs**, two needing a live process. The daemon has no `build.rs`, no version stamp, and no startup event at all |
| **#1487** | `(fix) daemon: near_limit_poll_coverage reports the cap, not the sub-interval the scheduler applied` | Reports 60 s while the scheduler applies `min(300/8, 60)` = **37.5 s**. Emitted 624 times, at band entry — the regime an operator inspects during an incident |
| **#1488** | `(feat) reliability: a durable first-sight readout for the observation-gap event pair` | The SLI is specified and instrumented but reaches no JSON wire, so nothing moves when a fix lands |
| **#1489** | `(docs) deploy: a runbook for replacing the running daemon — cargo build replaces nothing` | The daemon is an `SMAppService` agent inside the app bundle; `cargo build` into `target/` replaces nothing that runs |

**Three edits to existing items** — the third was made after an independent review round, and is the
one to read if you are picking up #1488:

- **#1232** — the `BLOCKED BY #1231` banner was stale from 2026-08-11 to 2026-09-04. #1231 returned
  **GO** with all six ACs met and its own closing comment already recorded #1232 as *"now
  unblocked"*. Replaced with the spike's actual findings (a warm-up cycle opens a window in 130 of
  131 cycles; cost ≲0.1 pp per cycle; **the build is smaller than #1232 assumes** — it is tightening
  an existing 7.01 h cadence, not one cycle per hold), plus a note that a session warm-up cannot buy
  weekly quota and so is not a remedy for the 08-29/08-30 class.
- **#1488** — its body claimed the derivation was *"exactly the § 6 METER"*. It is not: the event
  pair is edge-triggered past the bound, so with `T = 2*poll_secs / N` the emitted set is
  `{latency > T}`. Filtering that on `> 2T` **cannot produce a false negative** — an empty result
  conclusively means no `FAIL` — but it is an **upper bound rather than an exact count**, because the
  entry anchor is `observed.max(designated)`, so a mid-tenure gap on a long-active account passes the
  same filter while sitting outside § 6's post-swap `SCALE`. It is not a source for `GOAL`'s
  `p95 <= T` at all. The body's **Derivation** now states that upper-bound relation and names the
  anchor as its cause, in those words, and points at the design's § 7 and § 11 OQ-3. **Three consequences
  for whoever picks it up**: build the `FAIL` detector at `2T`, never at `T`; do not treat a
  non-empty result as a certain `FAIL`; and its AC-6 (*"the § 6b row is accurate once this lands"*)
  has quietly changed meaning — it now means *leave the row at `NEAR-COMPLETE`*, because the `GOAL`
  half has no source at all.
- **#1464** — enriched with **production incidence**, which its fixture-based measurement lacked.
  Locus: a **comment** dated 2026-09-04, not the item body — so `/do 1464`, which loads the body,
  does not see these figures unless the reader opens the thread. Named here because a pointer that
  does not say where it points is not one. The figures:
  16 of 140 (11%) consecutive-swap gaps below the 525 s sweep, **12 of the 16 on 09-01..09-03**;
  13 of 81 (16.0%) since 08-28 against 33 of 470 (7.0%) lifetime. Reported as three populations with
  their denominators on purpose — the joint finding is that the rate is *materially higher in the
  recent regime*, which collapsing them to one number destroys.

**Documents:** a PRD (`docs/requirements/daemon-diagnostic-integrity.md`), a solution design
(`docs/design/daemon-diagnostic-integrity-solution-design.md`), and two spec stubs under
`docs/specs/`.

## Key decisions

1. **The PRD covers three items, not four.** The observation-gap readout is *already*
   `active-account-observation-continuity` **R-4/R-5**, both `must`. That PRD's § 6b deferred only
   *which readout carries it* — a Stage 2 decision. Authoring a second home for one requirement is
   the redundancy failure, so #1488 rides the existing PRD and this run supplied the missing
   decision instead.

2. **Build identity is stamped from runtime facts, not a `build.rs` git SHA** — and this is the
   load-bearing call, not a cost saving. The operator builds from a working checkout, so a `build.rs`
   stamps the last commit *regardless of uncommitted changes*. The case where attribution matters
   most — *"was this the build with my fix in it?"* — is exactly the one a bare SHA answers
   **wrongly**, producing a confident false attribution. Worse than none. `CARGO_PKG_VERSION` plus
   the resolved exe path/size/mtime reproduces the proof that actually worked, from the log, after
   the process is gone.

3. **The readout lands on `reliability`, schema 12 → 13, mirroring `BlindEpisodesWire`.** The metric
   is a windowed distribution, which is what that wire is for; and `observation_gap_enter/exit` is
   structurally the same enter/exit pair as `blind_enter/exit`, whose census already solves all four
   pathologies this faces (severed pair, restart orphan, open-at-horizon, malformed). Percentiles are
   **nullable** — a window with no change of active is an empty subject, and `p95 = 0` would assert
   perfect latency where nothing was measured.

4. **#1487 must not reach for `near_limit_poll_secs`.** Making the diagnostic truthful and making the
   schedule faster are different changes. The cap is *shared*, so lowering it tightens the whole
   tick — a rate change `ADR-0012` Decision 3 forbids buying silently, and it is `#1458`'s scope,
   conditional on operator-owned `{T}`/`{D}`. Bounded explicitly on the item and in its spec stub.

5. **Nothing was filed against the capacity question.** See above.

## Assumptions and risks

- 🟡 **The 624-emission count and the 45,655-line corpus are machine-local** and not re-derivable
  from a clone. The `near_limit_poll_coverage` defect itself is proven **at source**
  (`Event::NearLimitPollCoverage` emits, `next_subinterval` computes, both in `src/daemon.rs`)
  and does not depend on any count.
- 🟡 **#1464's collapse is a projection, not an observation.** `invalidate_poll_schedule` has never
  executed on this host — the running daemon predates the whole #1451–#1455 series. **Consequence:
  deploy-and-observe is not a safe diagnostic instrument for that question**; verification must stay
  hermetic.
- 🟡 **#1489's verification step depends on #1486.** Without the build stamp the check degrades to
  inference — the very failure the family is about. It is a *dependency, not a blocker*: the runbook
  ships with an inference-based check and a marked TODO if #1486 has not landed.
- 🟡 **The runbook documents a script that can change.** Nothing reconciles prose against a script,
  and this run did not add such a gate. Citing rather than paraphrasing bounds the rot; it does not
  stop it.
- 🟢 **The two #1455 characterization locks are safe.** #1464's AC-3 *already* requires them inverted
  into positive oracles rather than deleted — verified before editing, and deliberately left
  untouched so the constraint is not duplicated into two places that can disagree.

## What was deliberately not done

`#1456` (correctly BLOCKED on operator-owned `{T}`), `#1458` (the tunables lever), `ADR-0019`'s
settled `exhausted_slow_polling` trade, and any release automation for a single-operator
single-machine daemon.

## Provenance and its limits

Every empirical claim in the PRD's § 2 was **re-verified against HEAD `9faa61b` during authoring**
rather than inherited from the investigation report — the emit/compute site pair, the absence of
`build.rs` and of any startup event, and the `--sign-only` flag. The runtime counts are carried as
assumptions (A-1, A-6) rather than asserted.

**Read that authoring pass as necessary and not sufficient.** Independent fresh-context review rounds
against the submitted PR falsified several claims it had passed — `version_line()`'s return shape,
`base` being a constant rather than a per-sub-interval jitter draw, and the relationship between the
gap-entry edge and § 6's `FAIL` threshold. All are corrected in place. The lesson is the general one:
a re-verification claim in these documents means *checked more than once*, never *cannot be wrong*.

The **scope membership** is operator-ratified (2026-09-04, "all enriched"). Every requirement's
specific *form*, the appetite, and all Planguage GOAL/FAIL values are **pipeline-authored and
ratification-pending** — recorded as pending in the PRD's § 11 rather than presented as agreed. The
PRD's `dor_status` is `passed-with-findings` for exactly this reason, and the honest statement of it
is in that section.

## Next steps

- `/do 1486` — the highest-leverage of the four: it is pure prevention, costs one log line, and
  retires a class of future forensic work. It also unblocks #1489's verification step.
- `/do 1487` — smallest, fully specified, RED oracle is exact.
- `/do 1488` — largest; read the design § 7 first for the wire constraint.
- `/do 1489` — docs only; better after #1486.
