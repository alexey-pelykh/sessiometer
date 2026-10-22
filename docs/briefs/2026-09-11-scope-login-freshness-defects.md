---
type: scope-brief
date: 2026-09-11
workflow: /scope
items: 3 (#1531, #1532, #1533)
source: investigation run 2026-09-09 (`/investigate`) plus a four-panelist `/council` synthesis on the same subject, both grounded at eb17b00 — the report and the panel results live in gitignored scratch, so every fact the items rest on is replicated in each issue body
status: final
---

# Scope Brief: What a Login Does Not Tell the Operator

## What happened

The operator revived three accounts with `sessiometer login`, ran `status`, and the freshly revived
rows still showed a bracketed near-deadline in the EXPIRY column. The question was why the next
`status` did not reveal the fresh state.

An investigation answered it and then **blocked** — on an operator-owned question it phrased as
*which surface must reflect what, within what latency bound, and for which login class*. Its own
readiness assessment had already isolated two of the three defects below as filable without the
answer — the restash-edge write (#1531) and the unconditional `last_good` clear (#1533) — and had
named the non-activating revive seam as needing its own item without locating it in code. A council
of four independent panelists (Rust, product, SRE, architecture), each in a fresh context and blind
to the others, **overturned the blocked verdict unanimously**: all three defects are decidable against
the code and the repo's own ratified prose without knowing the answer. The council's own
contributions were isolating the third defect at `reconcile_restored` (#1532) and re-characterising
the other two — a missing write rather than a write to the wrong field, an availability loss rather
than a lost log line, five `last_good` sites rather than one.

The blocking question is real. What the council established is that it gates only the *fourth*
candidate — the visibility workstream — and not the three filed here. It was **retired unanswered on
2026-10-22**; § What was deliberately NOT created records why.

## The three defects

They share one root, which is worth stating before the list: the daemon couples *observing a
deadline* to *advancing what the operator sees* by **co-location inside the poll block**, not by
design. Every seam that observes outside that block inherits the gap.

| # | Title | The mechanism |
|---|---|---|
| **#1531** | `(fix) daemon: the re-stash edge advances the expiry baseline but not the field status renders` | `fold_expiry_observation` writes the fresh deadline into `refresh_token_expires_at_baseline`. The renderer reads the other field, `refresh_token_expires_at`. Roughly two lines; the value is already the function's argument |
| **#1532** | `(fix) daemon: a restored revive makes no refresh-token-deadline observation at all` | `reconcile_restored` has two arms and **neither** folds an expiry observation. The larger half of the symptom, and no network request is needed to fix it |
| **#1533** | `(fix) daemon: a re-login of the already-active account clears last_good, disarming the blind-swap gate` | `reconcile_canonical_change` clears `last_good` unconditionally, destroying the anchor the ADR-0017 bounded-blindness gate is keyed on — and with it `blind_secs`, today the wire's only per-row observation age. Four of the five `last_good` sites in the tree already guard the clear on a real change of active; this is the fifth |

Read #1533 first. It restores a ratified availability protection, where the other two repair a
displayed value. It also shares a function with #1531, which is why #1531's recommended fix site is
`src/daemon/snapshot_build.rs` rather than the call site — that choice also makes the structural
guard, since a future caller then cannot advance one field without the other.

Acceptance criteria are **Tier B**: falsifiable, precisely targeted, not yet bound to a test. Each
issue carries an `## Oracle binding` section making the promotion an explicit obligation on whoever
implements it, with the two repo constraints that govern it — a RED pre-authored oracle must land
`#[ignore]`d, since `ci-ok` cannot merge a failing suite and `bypass_actors` on the `main-protection`
ruleset is empty; and mutate before believing a green, since every criterion names a dimension the
current suite does not cover.

## What was deliberately NOT created

**The visibility workstream was not filed, and nothing was written to #1457.** Per-row observation
age on the wire plus its row-level render is already specified verbatim as that item's AC-2. Whether
it should be split out turned on which surface the operator was actually reading — the one input
they held and this run did not. The question was **retired without an answer on 2026-10-22**, on an
independent advisory the operator approved, because both readings are already covered. Under the
EXPIRY reading, #1531 and #1532 make the cell honest. Under the row-as-a-whole reading, #1533
restores `blind_secs` — today the wire's only per-row observation age — on the one row that lost it,
and the general per-row age is #1457 AC-2 as written. The AC-2 split is decidable on engineering
grounds whenever #1457 is next prioritised, with nobody's recollection of a terminal glance needed:
AC-2 carries no `{D}` term, so the `{D}` value that blocks AC-1 does not reach it. That is also the
first of two spec defects in #1457 that travel with the split rather than being written to that item
now; the second is that AC-2, an always-present per-row age, cannot hold together with AC-4's
byte-identical healthy frame, which `skip_serializing_if` cannot reconcile, so AC-4 needs amending.

**No fourth item for a structural guard.** The candidate was a guard against the next seam that
observes without surfacing. Its cleanest form is exactly #1531's recommended fix site, so it was
absorbed rather than filed. Manufacturing an item for a guard the first one already carries would be
scope invented by a gate.

**No docs item for an ADR-0012 § Status amendment.** The architecture panelist recommended recording
the fix as an amendment to that ADR's § Status, as #1454 did for the sibling poll-schedule guard. It
was weighed and not filed: #1531 § Neither ADR blocks this records why no amendment is a
precondition — Decision 3 scopes itself to requests, timers and pollers, R-2 of the
observation-continuity requirements restates that as a rate-neutrality invariant, and #1452 shipped
under that reading. Recorded here so the recommendation reads as weighed rather than dropped.

## Two decisions taken without the operator, ratified since

Both were put to the operator at the Stage 0 gate. No answer came within 24 hours, so both were taken
on the stated recommendation and recorded as ratification-pending. **Both were ratified on
2026-10-22.** Three independent advisors — process, product and engineering lenses, each in a fresh
context, given the enumerated state and blind to this run's reasoning — converged on ratifying both,
and the operator approved acting on that advice. The grounds are recorded with each.

1. **Stages 1 and 2 were skipped.** The four council outputs, written for a different question,
   supplied the four signals the Rigor Gate requires of a design-document skip — per-component
   feasibility verdicts, alternatives weighed on the load-bearing decisions, a risk assessment, and
   requirement-to-component traceability — collectively rather than each per panelist, and the
   traceability leg was short until this run's Completion patched the governing spec (#1006) into
   #1531 and #1532. Weighing against the skip: this repo's own precedent authors a PRD, a design and
   specs together at scope time. The discriminator is not size but what a scope carries. The
   2026-09-01 observation-continuity scope ran Stages 1 and 2 because it carried policy values, an
   ADR amendment and a wire bump; these three carry no wire change, no config key, no new mechanism
   and no policy value, and every requirement their fixes conform to is already ratified in
   committed artifacts the issues cite. Ratified on that discriminator.
2. **Membership is the three unanimous defects.** See above. Ratified on the partition: every login
   class the code has — an activating re-auth, a non-activating revive, and an onboarding, which
   renders no expiry cell by design — reaches one of the fixed seams, and the absorbed structural
   guard is carried by #1531 provided its display write is ungated, which #1531 now records.

## One finding about this scope run itself

The Stage 0 corpus read swept the tracker and **missed the repo's own committed specification
corpus**. `docs/specs/status-expiry-provenance.feature.md` — issue #1006, PRD R-7 — governs the same
EXPIRY cell and was absent from the design-reference register through the coverage gate, which
therefore passed against a denominator short by one.

It was found at Completion and it was not cosmetic. That spec ratifies the credential source
asymmetry: canonical for the active account, the per-account stash for parked ones, *"that asymmetry
is correct and stays."* The visible in-function precedent for a zero-network credential read reads the
**stash** — and that arm is parked-only by construction, under the issue #253 exclusion. The other
arm is where the active account lands. An implementer copying the visible precedent would have read
the wrong slot for the active account, contradicted a ratified spec, and kept every local test green
while doing it. #1532's fix is now specified as a **source selection**, not a read, and both affected
issues carry the reference.

## For whoever picks these up

- **Cite symbols, not line numbers.** The subject revision is `eb17b00`; `origin/main` has moved on
  since, and commits in that range touched these files. Every finding was re-verified against
  `origin/main` — at `49561e8` on 2026-09-11 and again at `38731a5` on 2026-10-22 — and all three
  hold, but the line numbers in the source investigation have drifted.
- **Sequencing**: #1533, then #1531, then #1532. Neither #1531 nor #1532 runs concurrently with
  #1006, which moves the same CLI render goldens. #1533 and #1464 reason about the same seam but edit
  different code — #1464's repair lives in the poll schedule, #1533's is one predicate in
  `reconcile_canonical_change` — so each needs a semantic read of the other, not serialisation. PR
  #1538, open since 2026-09-27, makes the #584 velocity projection a third swap-arming arm of
  `blind_swap`; that gate requires the `last_good` anchor before any arm is evaluated, so the
  unguarded clear #1533 fixes disarms the new arm too. No rebase collision, but #1533's AC-4 test
  should exercise whichever arms exist when it lands.
- **No end-to-end test is owed.** Reproducing the live symptom requires running `sessiometer login`,
  which mutates real credential state. Unit plus snapshot-level integration is the required depth.
