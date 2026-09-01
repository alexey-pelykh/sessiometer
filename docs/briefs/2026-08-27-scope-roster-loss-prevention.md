---
type: scope-brief
date: 2026-08-27
workflow: /scope
items: 8 (#1438, #1439, #1440, #1441, #1442, #1443, #1444, #1445)
prd: docs/requirements/roster-loss-prevention.md
design: docs/design/roster-loss-prevention-solution-design.md
source: forensic investigation run 2026-08-27 (`/investigate`, falsifier round), grounded at c1b9de8 — the report itself lives in gitignored scratch, so every fact its items rest on is replicated in the PRD § 1a and in each issue body
status: final
---

# Scope Brief: Roster Loss Prevention

## What happened

`sessiometer login` found no `config.toml`, treated that as a first run, wrote a one-account roster,
and notified a live daemon still holding six. **Six accounts became one.** The credentials survived
in the Keychain; what was lost is the roster that indexes them — uuid, label, enabled, per account.

Two independent defects had to line up. An **absent config is a first run** everywhere in the write
path (`capture.rs:264` discards the parsed roster, `:689-697` rebuilds one from `Vec::new()`, and
`plan_capture` has only update-and-push arms). And **no guard prevents a write path from narrowing a
live roster** — `reconcile_roster` applies no floor at all.

The original deletion is still **unattributed**. The investigation abstained rather than guess.

## What this scope is

Twelve defect items, user-ratified as a closed set, turned into eight tracked issues plus a PRD, a
locked solution design, and four Gherkin specs. **No code was changed** — that was the explicit
instruction, and the reason the pipeline ran in full rather than jumping to a fix.

## The three things the scope found that the investigation did not

1. **The refusal semantics already exist in this repo.** `perform_config_set`
   (`src/daemon/commands.rs:392-397`) already refuses on an absent config with
   `ConfigSetRejection::NoConfig`, under the comment *"Absent → nothing to edit; unreadable → refuse
   rather than clobber a file we cannot read."* This scope propagates a ratified convention rather
   than inventing one.

2. **`reconcile_roster` has three callers, not two** (`:313`, `:436`, `:470`). The one the
   investigation missed is the argument for enforcing the invariant *inside* the function: a
   per-caller check is one missed caller away from the original defect.

3. **The guard as first framed would not have caught this.** An empty-roster floor never fires on a
   6 → 1 collapse — and, sharper, an append-only verb *by construction* always leaves at least one
   account in the file it saves, so a `login`-triggered reload **can never present zero**. The empty
   floor is inert on every append-only path and fires only on a legitimate `remove`-to-zero. The
   invariant had to become shrink-scoped and intent-partitioned, which costs a wire change. That
   correction and its cost were surfaced and ratified before the PRD was written.

## The keystone: how do you tell a first run from a loss?

An absent config is ambiguous. The PRD deliberately left the mechanism open and named two routes;
**the design chose neither**, because each fails exactly where it matters:

| route | fails where |
|---|---|
| consult the control socket | with the daemon down, "no daemon" ≡ "no prior roster" — the refusal degrades to permissive precisely where nothing would notice |
| refuse on absent config alone | puts a refusal in front of a genuine first run |

Both are dominated by a **prior-configuration witness**: durable state independent of both the
config file and the socket — a `Sessiometer/*` Keychain item, or a non-empty usage sample store.
**Both demonstrably survived this incident.** Witness present → refuse. Witness absent → allow,
byte-for-byte today's behaviour. A reachable populated daemon corroborates; it is never
load-bearing.

The probe costs nothing new: `src/keychain.rs:1293` already enumerates via `security dump-keychain`
*without* `-d` — metadata only, no prompt, no decryption, works against a locked keychain.

## The eight items

| # | What | Order |
|---|---|---|
| #1439 | back up `config.toml` on **qualifying** writes; a bad write neither backs up nor evicts | 1 |
| #1438 | a `roster-reload` event carrying outcome **and both counts** | 2 |
| #1440 | the witness rule + CLI refusal + thread the parsed roster through (**keystone**) | 3 |
| #1441 | same refusal over the socket + four-surface rejection parity | 4 |
| #1442 | never-shrink invariant + reload intent on the wire; retire the blessing test | 5 |
| #1443 | disk↔daemon divergence detection on the existing poll tick | 6 |
| #1444 | `config validate` stops calling an emptied roster valid; `RosterEmpty` copy | 7 |
| #1445 | dedicated config-write lock — **not** a widened `swap.lock` | 8, first to cut |

**Items 1–4 prevent the incident without 5–8.** That ordering is deliberate: if appetite runs out,
what gets cut is defence-in-depth and a latent hazard, never the primary defect.

## Two traps worth naming, because both look like the fix

- **A naive backup makes it worse.** In this incident's own sequence — delete, then `login`, then
  save — the previous contents at save time were *nothing*. Back-up-what-was-there would have
  recorded the empty state and evicted the last good copy: a recoverable loss made unrecoverable.
  Hence the qualifying-write rule.
- **The natural gate for cross-surface parity can never fail.** `panel-goldens` is deliberately
  soft — every step is `continue-on-error`, so it always reports pass. `CaptureAck.swift:101` throws
  on an unrecognized rejection tag, so a new Rust tag breaks the menu-bar button; the assertion has
  to live in `test`.

## What the design changed about the PRD

The PRD contradicted itself and the design stage found it. § 7 row 1 required refusal whenever the
config was absent and no daemon was running; AC-4 requires a genuine first run to succeed. **On a
fresh machine before the daemon is first started, both preconditions hold at once.** The witness
rule dissolves it. Amended at `docs/requirements/roster-loss-prevention.md` § 7a, with provenance —
a ratified document changed by the stage downstream of it, and **the one thing here worth a second
look**.

## Coverage

19/19 requirements, 12/12 ratified items, 8/8 acceptance criteria — checked mechanically. No
phantoms, no additions outside the ratified twelve. AC-1 and AC-4 each span two issues (#1440 +
#1442) and neither alone closes them.

## Still open, and owned elsewhere

- **The cause.** Seven of eight items guard *amplification*; only #1439 guards against the cause
  recurring, and it does so without knowing what the cause was.
- **The APFS snapshot probe** that would settle the on-disk roster count at the moment of loss
  needs `sudo` and expires **2026-08-28**. Operator-owned; offered, not run.
