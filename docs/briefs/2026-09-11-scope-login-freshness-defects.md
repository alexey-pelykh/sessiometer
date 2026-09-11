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
*which surface must reflect what, within what latency bound, and for which login class*. A council of
four independent panelists (Rust, product, SRE, architecture), each in a fresh context and blind to
the others, **overturned that verdict unanimously**: three of the defects it found are decidable
against the code and the repo's own ratified prose without knowing the answer. Two of those three the
investigation had not isolated at all.

The blocking question is real and is **still unanswered**. What the council established is that it
gates only the *fourth* item — the visibility workstream — and not the three filed here.

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

Acceptance criteria are **Tier B**: falsifiable, precisely targeted, not yet bound to a test. Five,
six and six respectively. Each issue carries an `## Oracle binding` section making the promotion an
explicit obligation on whoever implements it, with the two repo constraints that govern it — a RED
pre-authored oracle must land `#[ignore]`d, since `ci-ok` cannot merge a failing suite and
`bypass_actors` on the `main-protection` ruleset is empty; and mutate before believing a green, since
every criterion names a dimension the current suite does not cover.

## What was deliberately NOT created

**The visibility workstream was not filed, and nothing was written to #1457.** Per-row observation
age on the wire plus its row-level render is already specified verbatim as that item's AC-2. Whether
it should be split out turns on which surface the operator was actually reading — the one input they
hold and this run does not. Recording it here keeps it discoverable without deciding it.

**No fourth item for a structural guard.** The candidate was a guard against the next seam that
observes without surfacing. Its cleanest form is exactly #1531's recommended fix site, so it was
absorbed rather than filed. Manufacturing an item for a guard the first one already carries would be
scope invented by a gate.

## Two decisions taken without the operator

Both were put to the operator at the Stage 0 gate. No answer came within 24 hours, so both were taken
on the stated recommendation and stand **ratification-pending**. Both are cheaply reversible, and the
three items survive either way.

1. **Stages 1 and 2 were skipped.** The four panelists independently supplied all four signals the
   Rigor Gate requires of a design-document skip: per-component feasibility verdicts, alternatives
   weighed on the load-bearing decisions, a risk assessment, and requirement-to-component
   traceability. Weighing against it: this repo's own precedent authors a PRD, a design and specs
   together at scope time. The discriminator is size — that precedent covers programme work, while
   the flat `(fix) daemon:` class these three belong to carries no PRD and no spec.
2. **Membership is the three unanimous defects.** See above.

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

- **Cite symbols, not line numbers.** The subject revision is `eb17b00`; `origin/main` has since moved
  seven commits ahead and two of them touched these files. Every finding was re-verified against
  `origin/main` and all three hold, but the line numbers in the source investigation have drifted.
- **Sequencing**: #1533, then #1531, then #1532. Neither #1531 nor #1532 runs concurrently with
  #1006, which moves the same CLI render goldens. #1533 does not run concurrently with #1464, which
  reasons about the same seam.
- **No end-to-end test is owed.** Reproducing the live symptom requires running `sessiometer login`,
  which mutates real credential state. Unit plus snapshot-level integration is the required depth.
