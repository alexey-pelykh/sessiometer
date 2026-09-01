---
type: design-brief
date: 2026-08-27
source: docs/design/roster-loss-prevention-solution-design.md
workflow: /design-solution
status: final   # locked — both open questions resolved in-stage
---

# Design Brief: Roster Loss Prevention

## Problem

`sessiometer login` found no `config.toml` on disk, treated that as a first run, wrote a
one-account file, and notified the live daemon — which discarded the six accounts it alone still
held. Six accounts became one. The credentials survived in the Keychain; what was lost was the
roster that indexes them.

Two independent defects had to line up: an **absent config is a first run** everywhere in the write
path, and **no guard prevents a write path from narrowing a live roster**. Neither is exotic, and
the original deletion is still unattributed — so the design has to assume the cause can recur.

## Key Decisions

1. **A prior-configuration witness decides refusal — not the socket, and not the config's absence
   alone.** The PRD posed two routes and both are dominated. Socket-consulting has the strongest
   signal (the daemon held the surviving copy) but degrades to *permissive* exactly when the daemon
   is down — the case where nothing else would notice either. Absent-config-alone puts a step in
   front of a genuine first run. The witness is durable state independent of both: a `Sessiometer/*`
   Keychain item, or a non-empty usage sample store. **Both demonstrably survived this incident.**
   Witness present → refuse. Witness absent → allow, byte-for-byte today's behaviour. A reachable,
   populated daemon corroborates; it is never load-bearing.

2. **The witness costs nothing new to read.** `src/keychain.rs:1293` already enumerates via
   `security dump-keychain` *without* `-d` — metadata only, no prompt, no decryption, and it works
   against a locked keychain. The mechanism was already in the repo; the design only points it at a
   new question.

3. **Reload intent travels as one optional argument, and omitting it means refusing.**
   `notify_daemon_roster_reload()` takes no arguments across five call sites, so this is a wire
   change — accepted deliberately, because without intent the daemon-side invariant can only refuse
   *everything* (blocking legitimate `remove`) or *nothing*. The omitted argument **is** the
   fail-closed default, so a partial rollout refuses rather than permits.

4. **The empty-roster floor the user's earlier framing implied would not have caught this.** The
   incident was 6 → 1, never 6 → 0. Sharper: an append-only verb by construction always leaves ≥1
   account in the file it saves, so a reload triggered by `login`/`capture` **can never present
   zero** — an empty floor is inert on every append-only path and fires only on a legitimate
   `remove`-to-zero. The invariant is shrink-scoped and intent-partitioned. This correction was
   surfaced and ratified before the PRD was authored.

5. **The invariant lives in `reconcile_roster`, not in its callers.** There are three callers
   (`src/daemon/commands.rs:313`, `:436`, `:470`) — the investigation found two. One enforcement
   point cannot be bypassed by a caller added later.

6. **Back up the file being replaced only if it qualifies, and never let a bad write evict a good
   backup.** A naive backup-on-write fails in this incident's own sequence — delete, then `login`,
   then save — because the previous contents at save time were *nothing*: it would have recorded the
   loss instead of preventing it. Rule: back up iff the file being replaced parses as a valid config
   with a non-empty roster. A non-qualifying write neither backs up nor evicts. Small fixed ring (3),
   each at `0o600`.

7. **A dedicated config-write lock — not `swap.lock`.** `reconcile_login`'s doc comment
   (`src/capture.rs:805-825`) deliberately keeps the roster write *outside* the swap lock, and that
   intent is still correct: a swap contends on the keychain and `~/.claude.json`, never on
   `config.toml`. Widening `swap.lock` would break a documented invariant to fix an unrelated one.
   The stash-before-roster ordering is preserved.

8. **Cross-surface rejection parity is asserted by a test that can fail.** `CaptureAck.swift:101`
   fails closed on an unrecognized tag, so a new Rust rejection reason silently breaks the menu-bar
   button. The assertion goes in the `test` job — **never** in `panel-goldens`, whose every step is
   `continue-on-error` and which therefore always reports pass.

9. **Refusal semantics are not novel here.** `perform_config_set`
   (`src/daemon/commands.rs:392-397`) already refuses on an absent config with
   `ConfigSetRejection::NoConfig`, under the comment *"Absent → nothing to edit; unreadable → refuse
   rather than clobber a file we cannot read."* This design propagates a ratified in-repo convention
   rather than inventing one.

## What the design changed about the PRD

**The PRD contradicts itself, and the design found it rather than implementing around it.** § 7 row
1 (config absent, daemon not running) requires refusal unconditionally; AC-4 requires a genuine
first run to succeed. On a fresh machine before the daemon is ever started, **both preconditions
hold at once** — as written, row 1 forbids what AC-4 mandates. The witness rule dissolves it, but
the amendment is surfaced as OQ-1, not applied silently to a ratified artifact.

One committed test also has to change: `reconcile_roster_to_an_empty_roster_clears_active_and_state`
(`src/daemon/commands.rs:2577-2591`) blesses the empty reconcile as *"a degenerate-but-valid runtime
state"*. It is the codified form of the assumption this whole scope refutes.

## What this costs

Eight decisions; **19 of 19 requirements covered**, no gaps. Every mechanism already exists in the
repo, so nothing lands worse than FEASIBLE — the single FEASIBLE-WITH-SPIKE is *how* to assert
Rust↔Swift enum parity such that it can actually fail, and only D-5 waits on it.

Ordering is also the cut order reversed: backup ring → event vocabulary → witness + CLI refusal →
socket refusal + parity → intent + core invariant → divergence detection → Cap-7 diagnostic honesty
(R-11/R-13) → config-write lock. **Steps 1–4 prevent the incident without steps 5–8**, so an
appetite overrun cuts defence-in-depth and a latent hazard, never the primary defect.

CI cost: every change is `src/**`, so each owes `test`, `msrv` and `deny`. D-5 additionally owes
`swift` and `panel-goldens`.

## Still open

**Nothing load-bearing.** Both questions this design opened were resolved inside it, because neither
passed the asymmetry test — no sentence could be written naming what the operator knows that the
design stage does not.

- **The PRD § 7 row 1 contradiction was amended, not deferred.** Row 1 was internally unsatisfiable
  against AC-4, which is a defect with one coherent resolution rather than a fork between options.
  The amendment and its provenance are recorded at
  `docs/requirements/roster-loss-prevention.md` § 7a. **This is the one thing worth the operator's
  eyes** — a ratified document changed, by the stage downstream of it.
- **The `status` divergence signal was adopted, and was never net-new scope.** Assumption A-5's
  disposition column reads *"surface in design"*; the PRD delegated the decision here rather than
  omitting it. Declining would have shipped R-14's durable record with no reader — the failure
  premortem P-3 predicts.

One informational, carried from the investigation: the original deletion remains unattributed (the
investigation ABSTAINed). Seven of eight decisions guard *amplification*; only decision 6 guards
against the cause recurring, and it does so without knowing what the cause was.
