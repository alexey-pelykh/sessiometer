---
title: Roster Loss Prevention — Solution Design
source: docs/requirements/roster-loss-prevention.md
created: 2026-08-27
status: locked  # 2026-08-27 — both open questions resolved in-stage; see § 14 and § Design Lock
tracks:
  technical-architecture: complete
  data-architecture: complete
  api-design: complete
  ux-ia: complete
  security-architecture: complete
  testing-architecture: complete
---

# Solution Design: Roster Loss Prevention — Absent-Config Refusal, the Never-Shrink Invariant, and Roster Durability

**PRD DoR**: `passed-with-findings` — proceeding. The two findings are R-17/R-18, recorded as
enrichment-derived.

## 1. Goals and Drivers

Make it impossible for an append-only verb to shrink a live roster, make an absent config
distinguishable from a first run, and make the loss survivable whatever caused it. The 19
requirements of `docs/requirements/roster-loss-prevention.md` are the contract; § 16 traces each.

The driver is a real incident: `login` found no `config.toml`, wrote a one-account file, notified
the daemon, and the daemon discarded the six live accounts it alone still held. What removed the
file is **unattributed**, which is why durability is a goal and not a nicety.

## 2. Constraints

- **macOS only.** No CI job compiles for another platform; a green run says nothing about
  portability.
- **CI ownership.** Any `src/**` change puts the PR in the `rust` path filter and owes `test`,
  `msrv` **and** `deny`. `apps/menubar/**` additionally owes `swift` and `panel-goldens` — the
  latter a **soft** gate (every step `continue-on-error`) whose verdict lives only in its step
  summary, so it can never tell you the panel drifted.
- **Issue #139 must keep working.** `notify_daemon_roster_reload` exists because a running daemon
  never picked up on-disk roster *additions*. Every change here is in the *narrowing* direction;
  none may suppress the widening one.
- **An empty roster is legitimate at parse time**, by design — `capture` must be able to load a
  tunables-only file to add the first account (`src/config.rs:1045-1051`). No guard may live at
  parse.
- **`config.toml` carries no secret material** — the roster keys accounts by `account_uuid` /
  `label`, never by token or email (`src/config.rs:17-20`) — but is mode `0o600`
  (`FILE_MODE`, `src/paths.rs:56`). Anything derived from it inherits the mode.
- **The Swift rejection decoder fails closed.** `CaptureAck.swift:101` throws
  `DecodeError.unrecognized` on an unknown reason, so a Rust-only variant is a panel decode failure.
- **No new persisted schema version.** The roster's on-disk shape does not change, so no
  `STATUS_SCHEMA_VERSION` / `JSON_SCHEMA_VERSION` bump is implied. `FORMAT_VERSION` and its frozen
  migration fixtures are untouched.

## 3. Context and Scope

Three actors hold or move the roster:

| Actor | Role | Surface |
|---|---|---|
| `config.toml` | the durable copy | `~/Library/Application Support/sessiometer/`, `0o600`, atomic temp+rename |
| the daemon | the live copy — and, in the incident, the **last surviving** copy | in-memory `Vec<Account>`, reconciled by `reconcile_roster` |
| write verbs | the only bridge between them | `login`, `capture` (CLI + socket), `remove`, `enable`, `disable`, `import`, `config set` |

Out of scope, from the PRD and unchanged here: collapsing the two-copy architecture; making the
reload bidirectional; attributing the deletion; credential-store hardening.

## 4. Solution Strategy

Four decisions carry the design, and one of them replaces the fork the PRD left open.

1. **Partition write verbs by intent, and let the reload carry it.** Append-only verbs (`login`,
   `capture`) may never shrink the live roster; mutating verbs (`remove`, `disable`, `enable`,
   `import`, `config set`) may. This is R-3, and it is what makes the invariant correct in *both*
   directions.
2. **Answer "has this ever been configured?" from durable state that is neither the config file nor
   the socket.** This is D-1 — the keystone — and it dominates both routes the PRD posed.
3. **Put the invariant at the shared core, not at the callers.** `reconcile_roster` has three live
   callers; a fourth is a matter of time.
4. **Back up only what is worth restoring.** A write that would replace a good file with a bad one
   must not be allowed to evict the good backup. This is the whole of D-3, and it is what makes
   durability actually work in the incident's own sequence.

## 5. Building Blocks — the design decisions

### D-1 — the keystone: a prior-configuration witness *(R-6; PRD § 7 rows 1–3)*

**Decision.** A write verb that finds no `config.toml` consults a **prior-configuration witness** —
durable local state, independent of both the config file and the control socket — and refuses if the
witness is present.

**Neither route the PRD posed is chosen.** The PRD offered socket-consulting (strongest signal, but
degrades to permissive with the daemon down) versus absent-config-alone (no socket dependency, but
puts a step in front of a genuine first run). Both are dominated, because the system **already
holds durable evidence that it has been configured before, and that evidence demonstrably survived
this very incident**:

| Witness | Survived the incident? | How it is read |
|---|---|---|
| `Sessiometer/*` Keychain items | **Yes** — all six present throughout (PRD I-12) | `security dump-keychain` **without `-d`**: metadata only, so it raises no prompt, decrypts no secret, and works on a **locked** keychain. This is not new machinery — `src/keychain.rs:1293` already does exactly this at daemon start, filtering to a service prefix and never rendering the text |
| `usage-samples.jsonl` / `usage-rollup.json` | **Yes** — the sample series is unbroken across the incident day: 1,150 instants on 2026-08-27, worst gap 306 s against a 300 s poll cadence (PRD I-13) | a non-empty sample store implies a roster existed, because polling requires rostered accounts. `paths::usage_samples()` / `usage_rollup()` |

**Rule.** Witness present → **refuse**. Witness absent → **allow** (a genuine first run has neither).
Where the daemon is *also* reachable and holds accounts, that is a stronger signal still — use it
when available, **never depend on it**.

**Why this dominates both posed routes.**

- It works **with the daemon down**, which is precisely where socket-consulting silently degrades to
  permissive and nothing would notice (PRD Premortem P-6).
- Correctness needs no socket dependency; the socket becomes corroborating, not load-bearing.
- It adds **no step in front of a genuine first run** — a fresh machine has no Keychain items and no
  usage store, so the witness is absent and the verb proceeds exactly as today.
- The evidence class is not speculative: both witnesses are *observed survivors* of the incident this
  design exists to prevent.

> **This resolves a contradiction inside the PRD, and the PRD must be amended.** § 7 row 1 (config
> absent, daemon **not running**) requires *refuse* unconditionally, while AC-4 requires a genuine
> first run to succeed — and on a fresh machine, before the daemon is ever started, **both
> preconditions hold at once**. As written, row 1 forbids what AC-4 mandates. The witness rule
> dissolves it: row 1 becomes *refuse **if** the witness is present, allow if absent*, which
> satisfies AC-4 on a fresh machine and still refuses after a loss. **Surfaced as a design-stage
> correction to a ratified artifact, not applied silently.**

**Alternatives rejected.** *Socket-consulting alone* — fails PRD § 7 row 1 outright. *Absent-config-alone
with an explicit `--first-run` affordance* — correct but taxes every genuine first run, and the
witness makes the tax unnecessary. *A sentinel file written at first configure* — a new artifact that
would have to survive whatever removed `config.toml`, which is exactly the property we cannot assume.

### D-2 — how reload intent travels *(R-3, R-3a)*

**Decision.** Extend the existing `roster-reload` control message with **one optional intent
argument** (`append-only` | `mutating`). A message arriving with **no** intent takes the
**append-only (refusing)** treatment.

`notify_daemon_roster_reload()` takes no arguments today and is called from five sites — a bare
`notify_daemon_roster_reload` at `src/capture.rs:123`, `:859` and at `src/cli.rs:4778`, `:5572`,
`:5674`, all sending an identical message — so the daemon currently cannot tell an append from a removal.

**Alternatives rejected.** *Two distinct message names* — doubles the protocol surface, and an
unknown-message rejection from an older daemon is a **worse** failure than a conservative one.
*Infer intent by diffing UUID sets, no wire change* — attractive, but structurally cannot
distinguish a legitimate `remove` from a loss, which is the exact distinction R-4 requires.

**Why the omitted case is safe, and why it is the point.** The absent argument is simultaneously the
legacy shape (an older CLI against a newer daemon) and what a future verb added without declaring
intent will send (PRD Premortem P-4). Both resolve to *refuse*, which is the fail-closed direction.
R-3a promotes this from convention to requirement precisely so it cannot be quietly relaxed.

### D-3 — what is backed up, when, and what a write may not evict *(R-8, R-9; PRD A-4 / P-2)*

**Decision — the qualifying-write rule.** At replace time, the file **being replaced** is copied to
the backup ring **if and only if it parses as a valid config carrying a non-empty roster**. A file
that is absent, unparseable, or zero-roster is **not** backed up **and does not evict** anything.

**This single rule is what makes durability work in the incident's own sequence.** The naive form —
"retain the previous contents on every write" — fails exactly here: the sequence was *delete → login
→ save*, so the previous contents at save time were **nothing**, and a naive ring would have
faithfully recorded that nothing, possibly evicting a good entry to do it. Under the qualifying-write
rule the same sequence writes no backup at all, and the last **good** backup — from the last
legitimate save — survives untouched. That is the entry an operator actually wants.

**Shape.** A small fixed ring (3 entries) beside the config, newest-first, each at `0o600`
(constraint § 2). Eviction happens **only** when a qualifying backup is written. Restore is an
explicit operator verb that names what it will overwrite and refuses to do so silently (AC-5).

**Alternatives rejected.** *Back up on read* — a read is not a risk event and would churn.
*Unbounded history* — AC-5 forbids unbounded growth. *Back up only on the destructive paths* — the
paths cannot be enumerated, since the cause is unattributed; the rule must key on the **file's own
quality**, not on which verb is running.

### D-4 — the invariant lives at the shared core *(R-3, R-4, R-7)*

**Decision.** Enforce never-shrink inside `reconcile_roster` (`src/daemon/commands.rs:505`), which
takes the intent from D-2, not at its callers.

It has **three** live callers today — `:313` (`perform_socket_capture`), `:436` (`perform_config_set`
on a label change) and `:470` (`adopt_roster_reload`) — and the investigation enumerated only two of
them. One place covers all three and any fourth; per-caller guards structurally cannot, and the
repo has this exact under-scoped-fan-out precedent in issue #427.

The committed test asserting the empty reconcile is *"a degenerate-but-valid runtime state"*
(`:2577-2591`) is rewritten here rather than deleted — it becomes the R-4 case (a mutating verb may
legitimately reach zero) plus its new R-3 counterpart.

### D-5 — the refusal reason and its four surfaces *(R-12)*

**Decision.** Add refusal reasons as redacted kebab-case tags to `CaptureRejection`, and land all
four surfaces in **one change**: the Rust enum, the Swift mirror in
`apps/menubar/Sources/CaptureAck.swift`, the panel mock's authored capture-states reference
(`apps/menubar/design/menubar-preview.html`), and a **mechanical parity assertion**.

The parity assertion is deliberately *not* the `panel-goldens` gate. That gate is soft — every step
`continue-on-error`, verdict only in the step summary — so it always reports pass and cannot tell
you the presentation drifted (PRD Premortem P-5). Parity must be asserted by a test that can fail.

### D-6 — divergence detection *(R-10)*

**Decision.** On the daemon's existing poll tick, compare a cheap signal — the config file's
modification time and its account count — against the in-memory roster; report a change per D-7.
**Never write.** Bounded by the PRD's `DivergenceDetectionLatency` GOAL of one poll cadence.

### D-7 — reload observability *(R-14, R-15)*

**Decision.** Add a roster-reload event to the observability vocabulary carrying the outcome
(adopted / refused / failed) **and both roster counts**, then route the reload's `Err` arm and the
D-6 divergence report to it. R-15 is a strict prerequisite of R-14: `src/observability.rs` carries
capture outcomes but no reload event, so today there is nowhere to write.

`notify_daemon_roster_reload`'s **own** failure paths (`src/capture.rs:335-346`) are the
CLI-side twin of the same defect and get the same treatment — a notify that silently does not arrive
is indistinguishable from one that arrived and was refused.

### D-8 — serialising config writers without misusing the swap lock *(R-16)*

**Decision.** Introduce a **dedicated config-write lock**, distinct from `swap.lock`, held across
the read-modify-write of `config.toml`. The roster save stays **outside** the swap lock.

**This honours the documented intent rather than overriding it.** `reconcile_login`'s doc comment
states the reason precisely: *"The roster (`config.toml`) write is deliberately OUTSIDE the lock: a
swap contends only on the keychain + `~/.claude.json`, never on `config.toml`, so no concurrent swap
can race it"* (`src/capture.rs:811-814`). That rationale is **correct and remains correct** — the
swap lock guards a different resource, and widening it to cover `config.toml` would be scope-creep
on a lock whose contention set deliberately excludes it. What the rationale does *not* address is
**two config writers racing each other** — two CLI invocations, or a CLI and the daemon's
`perform_config_set`. The swap lock was never the right instrument for that; a config-write lock is.

**Ordering invariant preserved.** The same doc comment pins *stash-before-roster*: a crash after the
locked keychain write but before the save must leave a fresh restorable stash, never a roster
referencing an unstashed account. The config-write lock wraps only the config read-modify-write and
must not reorder that.

**Scope note.** R-16 is explicitly **not** this incident's cause — the `.tmp` race produces a
cross-publish or an `Err(Io)`, never an empty roster. It is tracked on its own merits and is first
to cut if appetite runs short (PRD § 1b).

## 6. Runtime View — the login flow, before and after

**Today** (the incident path):

```
login → read config #1 (:264) ─── keep only c.login, DISCARD the parsed roster
      → interactive claude spawn (~87s)
      → read config #2 (:826) ─── absent → Vec::new()  (:689-697)
      → plan_capture ─────────── push → roster.len() == 1
      → save (:856) ──────────── writes a 1-account file
      → notify (:859) ────────── daemon adopts 1, DISCARDS 6
```

**After**:

```
login → read config #1 ────────── carry the parsed roster forward           [R-5]
      → absent? consult the prior-configuration witness                     [D-1, R-6]
          witness present → REFUSE, write nothing, exit non-zero            [R-1]
          witness absent  → genuine first run, proceed
      → interactive claude spawn
      → read config #2 ────────── disagrees with read #1? REFUSE            [R-5]
      → plan_capture ─────────── append-only, by construction
      → qualifying-backup check → back up the file being replaced iff good  [D-3, R-8]
      → save (config-write lock) ──────────────────────────────────────────  [D-8, R-16]
      → notify "roster-reload append-only" ────────────────────────────────  [D-2, R-3]
      → daemon: reconcile_roster(intent=append-only)
          would shrink? REFUSE, retain memory, emit event                   [D-4, D-7, R-3]
          else adopt (issue #139's widening path, unchanged)                [R-18]
```

R-5 is load-bearing twice over: it removes the useless first read *and* turns the two-read window
into a **consistency check** — a disagreement across the ~87-second spawn is itself evidence of the
divergence this design exists to catch.

## 8. Interface Contracts — the control-socket surface

| Message | Today | After | Compatibility |
|---|---|---|---|
| `roster-reload` | no arguments; five senders, all identical | one **optional** intent argument (`append-only` \| `mutating`) | Old CLI → new daemon: bare message → append-only (refusing) treatment, R-3a. New CLI → old daemon: the old daemon ignores an unknown trailing argument and behaves as today — no worse than the status quo |
| `capture` ack `rejected` | closed four-tag vocabulary: `no-active-account`, `keychain-locked`, `swap-lock-busy`, `failed` | plus the D-5 tags | **Fails closed** on the Swift side (`CaptureAck.swift:101`) — all four surfaces land in one change, R-12 |

Redaction is unchanged and binding: a rejection tag is a bare machine code carrying no path, label,
account count, or credential (PRD AC-2).

## 9. UX Architecture — operator-facing surfaces

| Surface | Change | Governing reference |
|---|---|---|
| CLI refusal (`login`, `capture`) | A refusal must say what was observed and what to do — the operator's fleet appears intact in `status` while disk is empty, so the message must name the disagreement, not just decline. No internal issue/ADR numbers: an operator cannot resolve one from a terminal | none — CLI copy is authored in-repo |
| `config validate` | Stop reporting a zero-account config as unqualifiedly valid (R-11). Today `render_config_validate` emits `"{path} is valid (0 accounts)"` at exit 0 (`src/cli.rs:2181-2189`); `require_roster` is called only at `src/use_account.rs:988` and `src/poke.rs:174`, neither of them here | none |
| `Error::RosterEmpty` copy | Stop asserting no account was ever captured (R-13). `"no accounts captured yet — run \`sessiometer capture\`"` (`src/error.rs:279-280`) is true on a first run and actively misleading after a loss — which is exactly when it is read | none |
| Menu-bar capture states | Present the new rejection reasons (D-5) | **`apps/menubar/design/menubar-preview.html`** — probed and confirmed to author this surface (`capture-states-dark` / `-light`, plus a capture interaction-states reference). Oracle **only for what it authors**; hex/pixel values directional. Known divergences: `apps/menubar/design/README.md` § Expected reconciliations |
| Restore affordance | R-9's enumerate-and-restore path, which must work with the daemon **running** — the incident's daemon was up throughout | none |

## 11. Crosscutting Concepts

### Security

No new credential surface. The D-1 witness reads Keychain **metadata only** (`dump-keychain`
without `-d`) — no prompt, no decryption, works on a locked keychain, and the dump text is never
rendered, exactly as `src/keychain.rs:1293` already handles it. D-3 backups inherit `0o600`; a
world-readable backup would be a **new** exposure of the account-uuid/label set even though the file
holds no secrets. D-5 tags stay redacted.

### Observability

D-7 is the whole of it: one reload event carrying outcome and both counts, fed by the reload's `Err`
arm, the D-4 refusal, the D-6 divergence report, and the CLI-side notify failures. **PRD assumption
A-5 is not closed by this**: a durable record nobody reads is the same failure one layer up
(Premortem P-3). The design pairs the record with a surfaced signal on `status` so a divergence is
visible where the operator already looks — this is the mitigation A-5 asked for, and it is why the
divergence work is not "just logging".

### Master Test Plan

**Risk surface (ACC).** Capabilities: `Cap-1` refuse an append-only write against a
prior-configuration witness · `Cap-2` never shrink a live roster on an append-only reload ·
`Cap-3` still widen on #139's path · `Cap-4` retain and restore a qualifying backup · `Cap-5`
detect and report divergence · `Cap-6` Rust↔Swift rejection parity · `Cap-7` operator copy tells the
truth · `Cap-8` concurrent writers cannot publish a partial file.

**Pyramid.** Unit-heavy by system type (a CLI + daemon with pure decision functions): the witness
predicate, the shrink predicate, the qualifying-backup predicate, and the render functions are all
pure and unit-testable — `render_config_validate` is already pure by design and unit-tested without
touching a real config path. Integration covers the socket round-trip (intent argument, rejection
tags) and the fault-injection scenario. The Swift side is covered by the existing `MenubarTests`
bundle plus D-5's parity assertion.

**The named regression test (R-17).** Live six-account daemon + absent config + append-only verb →
must fail against `c1b9de8` and pass after. This is the single test that would have caught the
incident, and it is the plan's centrepiece.

**Traceability.** § 16 binds every requirement to a Capability.

**AI-augmented testing.** N/A — no AI/LLM in system.

**Quality gates.** `test`, `msrv`, `deny` for `src/**`; `swift` + `panel-goldens` for
`apps/menubar/**`. `panel-goldens` is soft and cannot fail, so D-5's parity assertion is a **`test`-job**
assertion, not a golden comparison.

### Error Handling

Every refusal path added here writes **nothing** — that is the property AC-1, AC-2 and AC-3 all
assert, and it is what distinguishes a refusal from a partial application. The existing single-`None`
arm in `load_existing_from` stays exactly as it is: it is already correct (PRD I-3, I-4, § 7 rows
8–9) and is load-bearing for the whole analysis.

## 12. Architecture Decisions

| ADR | Decision | Status |
|---|---|---|
| D-1 | Prior-configuration witness, independent of both config file and socket | **proposed** — supersedes the PRD's two-route framing of R-6; carries a PRD amendment (§ 14 OQ-1) |
| D-2 | Reload intent as one optional argument; omitted ⇒ refusing | proposed |
| D-3 | Qualifying-write backup rule; a bad file neither backs up nor evicts | proposed |
| D-4 | Invariant at `reconcile_roster`, not at its three callers | proposed |
| D-5 | Four-surface rejection parity, asserted by a test that can fail | proposed |
| D-6 | Divergence check on the existing poll tick; report, never write | proposed |
| D-7 | One roster-reload event carrying outcome + both counts | proposed |
| D-8 | Dedicated config-write lock; swap lock unchanged and still correct | proposed |

D-1 warrants a committed ADR under `docs/adr/` on merge — it is the load-bearing choice and it
overturns a fork a ratified PRD posed. The rest are recorded here.

## 13. Quality Requirements

The PRD's five Planguage tags carry unchanged: `RosterSurvivability`, `ReloadObservability`,
`DivergenceDetectionLatency`, `RefusalParity`, `FirstRunFriction`. D-1 is the reason
`FirstRunFriction` holds at its PAST baseline — the witness-absent path is byte-for-byte today's
behaviour, so a genuine first run pays nothing.

## 14. Risks and Open Questions

### Feasibility Summary

Every mechanism this design needs **already exists in this codebase**. Nothing is novel, which is
why no component lands worse than FEASIBLE-WITH-SPIKE.

| Component | Verdict | Precedent |
|---|---|---|
| D-1 prior-configuration witness | **FEASIBLE** | `security dump-keychain` metadata enumeration already runs at daemon start (`src/keychain.rs:1293`); `paths::usage_samples()` / `usage_rollup()` exist |
| D-2 reload intent argument | **FEASIBLE** | one local socket, one binary, both ends ours |
| D-3 backup ring | **FEASIBLE** | `write_private_file`'s atomic temp+rename + `FILE_MODE` already do the hard part |
| D-4 core invariant | **FEASIBLE** | one function, three known callers |
| D-5 four-surface rejection parity | **FEASIBLE-WITH-SPIKE** | the four surfaces are known; what is *not* obvious is how to assert Rust↔Swift enum parity in a way that can actually fail. Spike: pick the mechanism (generated fixture consumed by both sides, or a Rust test that reads the Swift source) — time-boxed, and D-5 is the only thing waiting on it |
| D-6 divergence check | **FEASIBLE** | rides the existing poll tick |
| D-7 reload event | **FEASIBLE** | vocabulary extension in `src/observability.rs` |
| D-8 config-write lock | **FEASIBLE** | `swap.lock` / `daemon.lock` / `usage.lock` are the established pattern |

### Risk Register

| Risk | L×I | Priority | Mitigation |
|---|---|---|---|
| **D-2's wire change touches five call sites plus the handler** and is what D-4 depends on to tell an append from a removal | 2×3=6 | MEDIUM | One message name, one handler, one enforcement point; the omitted argument *is* the fail-closed default (R-3a), so a partial rollout refuses rather than permits. Ordering (§ 17) puts D-1 + D-3 first so the incident is prevented before this lands |
| **D-3's semantics could make durability illusory** — the naive form records the loss instead of preventing it | 2×3=6 | MEDIUM | The qualifying-write rule, plus a test that replays the incident's own sequence (delete → login → save) and asserts the last good backup is still intact and unevicted |
| **D-5 parity drifts undetected** because `panel-goldens` is soft and always reports pass | 3×2=6 | MEDIUM | The parity assertion lives in the `test` job, which can fail — never in the golden comparison |
| **Appetite overrun** — two weeks of evenings across eight decisions | 2×2=4 | MEDIUM | § 17's ordering is also the cut order, reversed: D-8, then Cap-7 (R-11/R-13), then D-6 go first, and none of the three leaves the primary defect open |
| **D-1 false-negative**: a loss that also takes both witnesses | 1×3=3 | LOW | D-3's backup is the second line and does not depend on D-1 |

**Rabbit-hole scan (Shape Up 10× test).** D-5's parity mechanism is the one candidate — but if it
takes 10× longer, the other seven decisions stand without it, so it is a spike, not a rabbit hole.
**The genuine coupling is D-4 → D-2**: without intent on the wire, the core invariant can only
refuse *everything* (blocking legitimate removals) or *nothing*. That coupling is why they ship
together in § 17 step 5, and why steps 1–4 are deliberately arranged to prevent the incident
without either of them.

### Open Questions — both RESOLVED in-stage

Neither needed the operator. The asymmetry test applies to each: *what does the operator know that
this stage does not?* Neither sentence could be written, so surfacing either would have transferred
accountability without transferring capability, and stalled the pipeline behind a question whose
answer the PRD already contained.

- **OQ-1 — the PRD § 7 row 1 contradiction. RESOLVED: amended, not deferred.**
  Row 1 required refusal whenever the config was absent and no daemon was running; AC-4 requires a
  genuine first run to succeed and forbids requiring a hand-made config file. On a fresh machine
  before the daemon is first started **both preconditions hold at once**, so row 1 was internally
  unsatisfiable — not a fork between two defensible options, but a defect admitting exactly one
  coherent resolution. D-1's witness rule supplies it, and it *preserves* row 1's stated rationale
  (a socket-consulting guard degrades to permissive with no daemon; the witness does not).
  **Applied** at `docs/requirements/roster-loss-prevention.md` § 7 row 1 + new § 7a, which records
  the amendment, its provenance, and this derivation. Rows 2–9 unchanged.

- **OQ-2 — the `status` divergence signal. RESOLVED: adopted, and it was never net-new scope.**
  The initial reading — that it traced to no ratified REQ-ID — was wrong on re-reading the PRD.
  Assumption **A-5**'s disposition column reads *"**surface** in design"*, with the mitigation
  *"pair the durable record with a surfaced signal (`status`, panel) rather than the log alone"*.
  The PRD did not omit this decision; it **delegated it to this stage**. Adopting it executes the
  PRD's own instruction, and it sits inside ratified item **E4 (observability)**. Declining it would
  ship R-14's durable record with no reader — precisely the failure premortem **P-3** predicts.
  Traced to R-14 + A-5 in § 16b.

**No load-bearing questions remain.** One informational item, carried from the investigation and not
resolvable by any design: the incident's original deletion is still unattributed (the investigation
ABSTAINed). Seven of the eight decisions guard *amplification*; only D-3 guards against the cause
recurring, and it does so without knowing what the cause was.

## 15. Glossary

| Term | Definition |
|---|---|
| **Prior-configuration witness** | Durable local state proving the machine has been configured before, independent of both `config.toml` and the control socket: any `Sessiometer/*` Keychain item, or a non-empty usage sample store |
| **Append-only verb** | A write verb whose roster mutation can only update-in-place or append — `login`, `capture`. Forced by `plan_capture`'s two arms |
| **Mutating verb** | A write verb that may legitimately reduce the roster — `remove`, `disable`, `enable`, `import`, `config set` |
| **Qualifying backup** | A backup written only from a file that parses as a valid config with a non-empty roster; only a qualifying write may evict a ring entry |
| **Shrink** | A reload presenting a *lower* account count than the live in-memory roster. Distinct from *empty*, which is the special case that never occurs on the append-only path |
| **Divergence** | The two roster copies disagreeing while both exist |

## 16. Requirement-to-Track Coverage Matrix

| PRD Req | Track(s) | Section | Capability | Status |
|---|---|---|---|---|
| R-1 | Technical Arch, API | § 5 D-1, § 6 | Cap-1 | covered |
| R-2 | Technical Arch, API, UX | § 5 D-1/D-5, § 8 | Cap-1, Cap-6 | covered |
| R-3 | Technical Arch, API | § 5 D-2/D-4 | Cap-2 | covered |
| R-3a | API | § 5 D-2, § 8 | Cap-2 | covered |
| R-4 | Technical Arch | § 5 D-4 | Cap-2 | covered |
| R-5 | Technical Arch | § 6 | Cap-1 | covered |
| R-6 | Technical Arch | § 5 D-1 | Cap-1 | covered |
| R-7 | Testing Arch | § 5 D-4, § 11 | Cap-2 | covered |
| R-8 | Data Arch | § 5 D-3 | Cap-4 | covered |
| R-9 | Data Arch, UX | § 5 D-3, § 9 | Cap-4 | covered |
| R-10 | Technical Arch | § 5 D-6 | Cap-5 | covered |
| R-11 | UX | § 9 | Cap-7 | covered |
| R-12 | API, UX | § 5 D-5, § 8 | Cap-6 | covered |
| R-13 | UX | § 9 | Cap-7 | covered |
| R-14 | Technical Arch | § 5 D-7, § 11 | Cap-5 | covered |
| R-15 | Technical Arch | § 5 D-7 | Cap-5 | covered |
| R-16 | Technical Arch | § 5 D-8 | Cap-8 | covered |
| R-17 | Testing Arch | § 11 | Cap-1…Cap-8 | covered |
| R-18 | Testing Arch, API | § 5 D-2, § 11 | Cap-3 | covered |

**19 of 19 covered. No UNCOVERED entries.**

## 16b. Element-to-Requirement Backward-Coverage Matrix

| Design element | Type | Traces to | Status |
|---|---|---|---|
| Prior-configuration witness | mechanism | R-6, R-1 | traced |
| Reload intent argument | wire contract | R-3, R-3a | traced |
| Backup ring + qualifying-write rule | artifact | R-8 | traced |
| Restore affordance | CLI verb | R-9 | traced |
| Never-shrink guard in `reconcile_roster` | mechanism | R-3, R-4 | traced |
| Roster-reload event type | observability | R-15, R-14 | traced |
| Divergence check on the poll tick | mechanism | R-10 | traced |
| New `CaptureRejection` tags | wire contract | R-12, R-2 | traced |
| Cross-language parity assertion | test | R-12 | traced |
| Dedicated config-write lock | mechanism | R-16 | traced |
| `status` divergence signal | UX surface | R-14 + A-5 (PRD disposition: *surface in design*) | traced |

**Eleven of eleven traced.** No phantom elements. The `status` signal was initially read as
untraced; re-reading A-5's disposition column showed the PRD delegated the decision to this stage
rather than omitting it (§ 14 OQ-2).

## 17. Ordering — which is also the cut order, reversed

1. **D-3 backup ring.** Fully independent, and the only item that protects against the *unattributed*
   cause. Highest value per unit of risk; ship first.
2. **D-7 + R-15 reload event vocabulary.** Strict prerequisite for everything observable.
3. **D-1 witness + R-5 threading + R-1 CLI refusal.** Prevents the incident at source.
4. **R-2 socket refusal + D-5 parity.** Depends on 3; introduces the rejection tags, so the
   four-surface change rides here.
5. **D-2 intent + D-4 core invariant + R-7 test rewrite.** Defence-in-depth at the daemon. Ships as
   one unit because D-4 cannot function without D-2.
6. **D-6 divergence detection.** Depends on 2.
7. **R-11 + R-13 — Cap-7 diagnostic honesty (§ 9).** `config validate` must stop calling an emptied
   roster valid, and `RosterEmpty` must stop telling a bereaved operator they never captured
   anything. Independent of 1–6; it does not prevent the loss, it stops the tooling from
   misdescribing one after the fact. Listed because § 16 traces R-11 and R-13 to § 9 rather than to
   a D-id, so an ordering keyed on decisions alone would omit them and anyone cutting to appetite
   via this list would never encounter them.
8. **D-8 config-write lock.** Independent; explicitly the first to cut (PRD § 1b).

Steps 1–4 prevent the incident without steps 5–8. That is deliberate: if appetite runs out, what
remains cut is defence-in-depth and a latent hazard, never the primary defect.

## Design Lock

**LOCKED** — `status: locked`, 2026-08-27.

| Gate | Verdict |
|---|---|
| Forward coverage (§ 16) | 19 / 19 requirements covered, no UNCOVERED |
| Backward coverage (§ 16b) | 11 / 11 elements traced, no phantoms |
| Load-bearing open questions | **zero** — OQ-1 amended into the PRD, OQ-2 adopted per A-5's own disposition |
| Feasibility (§ 14) | no INFEASIBLE, no UNCERTAIN; one FEASIBLE-WITH-SPIKE (D-5's parity mechanism), which blocks only D-5 |

The lock is honest rather than declared: it was withheld while OQ-1 and OQ-2 were open, and both
were **resolved** — not waived, not deferred, not re-labelled. A lock over an unresolved
load-bearing question is a false lock that propagates downstream as authoritative and re-opens at
the most expensive stage; that is the failure this gate exists to prevent, and it did not occur.

**What the operator should still review** (reporting, not gating): the § 7a amendment to a document
they were shown as ratified, and the eight decisions in § 12 — most of all D-1, which chose neither
route the PRD posed.
