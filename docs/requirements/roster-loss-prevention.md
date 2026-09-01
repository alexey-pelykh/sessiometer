---
title: Roster Loss Prevention — Absent-Config Refusal, the Never-Shrink Invariant, and Roster Durability
scope: roster-integrity
created: 2026-08-27
status: draft
dor_status: passed-with-findings
source: forensic investigation run 2026-08-27 (`/investigate`, falsifier round), grounded at c1b9de8 — the report itself lives in gitignored scratch and is not dereferenceable from here, so every fact its requirements rest on is replicated in § 1a and § 10
formulation: {technical-architecture: complete, data-architecture: complete, api-design: complete, ux-ia: complete, security-architecture: complete, testing-architecture: complete}
features:
  absent-config-refusal: {stage: design, tracks: {technical-architecture: complete, api-design: complete, ux-ia: complete, security-architecture: complete, testing-architecture: complete}}
  never-shrink-invariant: {stage: design, tracks: {technical-architecture: complete, api-design: complete, testing-architecture: complete}}
  roster-durability: {stage: design, tracks: {data-architecture: complete, ux-ia: complete, security-architecture: complete, testing-architecture: complete}}
  divergence-detection: {stage: design, tracks: {technical-architecture: complete, testing-architecture: complete}}
  reload-observability: {stage: design, tracks: {technical-architecture: complete, testing-architecture: complete}}
  rejection-vocabulary-parity: {stage: design, tracks: {api-design: complete, ux-ia: complete, testing-architecture: complete}}
  write-path-serialization: {stage: design, tracks: {technical-architecture: complete, testing-architecture: complete}}
artifacts:
  requirements-brief: docs/briefs/2026-08-27-requirements-roster-loss-prevention.md
  design-doc: docs/design/roster-loss-prevention-solution-design.md
  design-brief: docs/briefs/2026-08-27-design-roster-loss-prevention.md
---

# PRD — Roster Loss Prevention: Absent-Config Refusal, the Never-Shrink Invariant, and Roster Durability

## 0. Why this PRD exists in the code repo, and what the investigation contributed

This PRD originates in a **production incident on the maintainer's own fleet** on 2026-08-27, not
in an upstream requirement family. There is no HQ parent to trace to, so the `parent-requirements`
key is absent rather than pointed at a referent that never existed.

Its evidence base is a three-round forensic investigation whose report is **gitignored scratch**
(`.tmp/investigations/`, reclaimed by its producer). A committed document may not rest its argument
on an unreachable source, so § 1a replicates every load-bearing forensic finding **in-band**, with
its code anchor, and § 10 records the traceability. The report is provenance; this document is the
record.

**What this PRD owns**: the requirements. The *solution* — specifically which layer hosts the
never-shrink invariant, and whether a write verb may consult the control socket before committing —
is deliberately deferred to `docs/design/roster-loss-prevention-solution-design.md`. Two
requirements below (R-1, R-6) are stated as outcomes precisely so the design stage can choose the
mechanism without this document pre-empting it.

## 1. Problem Statement

**Current state.** `sessiometer` keeps the account roster in two places: `config.toml` on disk, and
the daemon's in-memory `Vec<Account>`. The two are synchronised in exactly one direction, on exactly
one trigger — a write verb persists to disk and then notifies the daemon to re-read it — and the
daemon adopts whatever it reads, unconditionally, with no record that it did.

On 2026-08-27 a `login` found no `config.toml`, treated that as a first run, wrote a
**one-account** file, and notified the daemon. The daemon adopted the one-account roster and
discarded the **six live accounts it alone still held**. The operator's fleet was destroyed by a
verb that only ever appends.

**Affected users.** Every operator of a multi-account fleet. The daemon is the only surface that
shows the roster (`status` is a pure control-socket client and never reads disk), so the operator
has no way to observe that the on-disk copy has diverged — or vanished — until a write verb
resolves the divergence destructively.

**Why now.** Three properties failed at once, and any one of them alone would have been sufficient
to prevent the loss:

1. no write path distinguishes *"never configured"* from *"configuration disappeared"*;
2. no invariant prevents an append-only verb's reload from **shrinking** a live roster;
3. no durable prior copy of the roster exists anywhere.

The third matters most for the future, because **what removed the file is still unattributed** — the
investigation ruled out every `sessiometer` code path, directory-level deletion, four distinct
path-divergence mechanisms, `cargo test`, and every Claude Code session, and closed on a stated
ABSTAIN. A fix that only hardens the write paths leaves the system one unattributed deletion away
from silently losing its only copy again, discovering it at the next daemon restart.

### 1a. The incident, replicated in-band

Facts below are the investigation's, each carried with its code anchor so this document stands
without the report. Grounded at `c1b9de8`; `src/capture.rs` last changed 2026-08-15, so the audited
code is the code that ran.

| # | Finding | Anchor |
|---|---|---|
| I-1 | `plan_capture` has exactly two arms — update-in-place for a known uuid, `roster.push` for a new one. **No arm removes, truncates, replaces or clears.** | `src/capture.rs:521-571` |
| I-2 | The reported count is `roster.len()` taken *after* `plan_capture`, so `(now 1 in rotation)` forces `len == 0` on the roster that was read in. Arithmetic, not inference. | `src/capture.rs:722` |
| I-3 | `load_existing_from` returns `None` on **exactly one** condition — `Error::ConfigNotFound` — and `Config::load_path` raises that **only** for `ErrorKind::NotFound`. Every other I/O error and every parse failure propagates and aborts the verb. | `src/capture.rs:420-426`; `src/config/load.rs:39-49` |
| I-4 | `Config::validate`'s account loop makes every malformed branch a hard `Err`; it never *filters* accounts. Six well-formed accounts on disk therefore cannot load as zero. | `src/config/validate.rs:295-326` |
| I-5 | `login` reads the config **twice**: at `:264` it keeps only `c.login` and **discards the parsed roster**; at `:826` it re-reads, and that read feeds `run_login`. Between them sits the ~87-second interactive spawn. | `src/capture.rs:264`, `:826` |
| I-6 | On the absent read, `login` falls through to `existing.unwrap_or_else(\|\| Config { roster: Vec::new(), … })`. | `src/capture.rs:689-697` |
| I-7 | `reconcile_login` calls `save()` then `notify_daemon_roster_reload()`. **The write was not the harm; the notify was.** | `src/capture.rs:856`, `:859` |
| I-8 | The daemon's handler adopts on `Ok` and **keeps its in-memory roster on `Err`**. The drop to one account therefore proves the reload *succeeded* — the daemon discarded the six accounts it alone held. | `src/daemon/commands.rs:465-476` |
| I-9 | The `Err` arm reports via `eprintln!` only. This machine has **no launchd installation**; the daemon runs from a manual `run`, so the message goes to a terminal nobody watches. Under `service install` it *does* land durably — `daemon_stderr_log` is threaded into the plist's `StandardErrorPath` and resolves to `logs_dir()/daemon.err.log` — but that channel is **ungoverned and opt-in**: raw process stderr, surfaced only by `log --channel diag`. Neither path puts the failure where an operator would see it. | `src/daemon/commands.rs:470-474`; `src/service.rs:89`, `:533`; `src/paths.rs:489` |
| I-10 | The daemon loaded its roster **at start and never re-synced**. Every `notify_daemon_roster_reload` trigger is a write verb, and none ran between 2026-08-05 and the incident — so disk was free to diverge, unobserved, for at least 21 days. | `src/capture.rs:123`, `:859`; `src/cli.rs:4778`, `:5572`, `:5674` |
| I-11 | **Nothing in the codebase removes `config.toml`.** Every removal site — `remove_file` or `remove_dir_all` — targets a `.tmp` sibling, an ephemeral refresh dir, an isolated spawn dir, a stale socket, or the launchd plist. What removed the file is **unattributed** — a stated ABSTAIN, not a guess. | `src/paths.rs:1192`, `:1233`, `:264`; `src/refresh.rs:949`; `src/isolated_spawn.rs:424`, `:436`; `src/cli.rs:1651`, `:1665`; `src/service.rs:370` |
| I-12 | The credentials survived: all six `Sessiometer/<uuid>` Keychain items were present throughout. **What was lost was the index, not the secrets** — which is why a roster backup is a complete remedy for the loss, and why it carries no credential-exposure risk. | investigation F7 / F12; `src/config.rs:17-20` |
| I-13 | A **second** durable witness survived alongside the Keychain, and its series is unbroken across the incident day: the usage sample store holds 1,150 distinct sample instants on 2026-08-27, 00:00:09 to 23:58:51, worst inter-sample gap 306 s. Read that against the **per-account** cadence (`DEFAULT_POLL_SECS`, 300 s), not a store-wide one: issue #80 spreads the roster one account per `poll_secs / N` sub-interval, so six accounts sample store-wide at ~50 s and no per-account cadence was missed. A non-empty store implies a roster existed, because the poll has no accounts to sample without one. Measured directly on the affected machine on 2026-09-01 — **not** taken from the report. It is re-derivable only until the raw tier ages out: `DEFAULT_STATS_RAW_RETENTION_SECS` is 14 days, so 2026-08-27's raw samples expire around 2026-09-10 and this row becomes the durable record. | `paths::usage_samples()` (`src/paths.rs:609`), `usage_rollup()` (`:620`); `DEFAULT_POLL_SECS` (`src/config.rs:70`); `DEFAULT_STATS_RAW_RETENTION_SECS` (`src/config.rs:281`) |

**The transition that actually occurred was 6 → 1, not 6 → 0.** This is the single most important
correction to make before designing a guard: an *empty-roster floor* would not have fired on this
incident. See R-3.

### Problem framing — what was challenged

- **Rejected framing: *"`login` has a bug."*** `login` behaved exactly as written, and its
  append-only shape (I-1) is correct. The defect is that a verb which cannot shrink a roster was
  nonetheless able to shrink the live one, because the reload discards the distinction.
- **Rejected framing: *"reject an empty roster on load."*** An empty roster is legitimate **by
  design**: `Config`'s own doc comment states the roster is *"possibly empty — the daemon's 'at
  least one' precondition is `Config::require_roster`, not a parse-time rule, so `capture` can load
  a tunables-only file to add the first account"* (`src/config.rs:1045-1051`). That arm exists for
  the first-account case and must keep working. The guard belongs at the **narrowing boundary**, not
  at parse.
- **Rejected framing: *"this needs novel refusal semantics."*** It does not. `perform_config_set`
  **already** refuses on an absent config — `ErrorKind::NotFound` → `ConfigSetRejection::NoConfig`,
  commented *"Absent → nothing to edit; unreadable → refuse rather than clobber a file we cannot
  read"* (`src/daemon/commands.rs:392-397`). One of three write paths already has the convention;
  two lack it. This is an **inconsistency to propagate**, not a design to invent.
- **Prevention over solution.** Because the deletion is unattributed (I-11), hardening the write
  paths bounds the *amplification* but not the *cause*. Durability (R-8) and divergence detection
  (R-10) are what make an unattributed deletion survivable, whatever it turns out to be. They are in
  scope for that reason, not for completeness.
- **Chesterton's fence on the reload.** `notify_daemon_roster_reload` was introduced by `02e37f1` as
  the fix for issue #139 (*"a running daemon never picks up on-disk roster changes until restart"*).
  It was scoped for the **widening** direction. Any guard must preserve #139's behaviour; disabling
  the reload is a regression, not a fix.

## 1b. Boundaries

### Appetite

**Two weeks of focused evenings**, split roughly: the refusal + never-shrink invariant and their
tests are the majority; durability is a self-contained afternoon; divergence detection and the
observability work are the tail.

Uncertainty is **low-to-moderate**: the failure is fully understood at source, the refusal
convention already exists in-repo to copy, and the blast radius is three call sites of one function.
The moderate half is the reload-intent wire change (R-3) and the cross-language rejection vocabulary
(R-12), both of which cross a process or language boundary.

If the appetite is exceeded, the order to cut from is the **reverse** of the dependency order in
§ 5b: divergence detection and write-path serialisation are the two that can slip without leaving
the primary defect open.

### Out of Scope

- **Attributing the deletion.** Ruled out at investigation and left as a stated ABSTAIN (I-11). This
  PRD makes the deletion *survivable*; it does not identify it. A one-off APFS-snapshot probe exists
  to rule the last candidate in or out, and expires 2026-08-28 — it is operator-run and is not work
  this PRD tracks.
- **Recovering the specific lost state.** Already complete: seven accounts restored, all healthy,
  zero orphaned Keychain items. Permanently lost and NOT recoverable by any work here: the prior
  `enabled` flags, non-roster config-section customisations, and event-log history before the
  incident.
- **Changing the two-copy architecture.** The disk/daemon split stays. Collapsing it — a single
  source of truth, or a daemon that owns the file exclusively — is a larger redesign this PRD
  explicitly does not open.
- **Making the reload bidirectional.** The daemon will not gain a disk-writing path. Divergence
  detection (R-10) **reports**; it never reconciles by writing.
- **Migration-artifact durability.** `FORMAT_VERSION` and its frozen fixtures (`src/migration.rs`)
  are untouched.
- **Credential-store hardening.** The Keychain items survived (I-12) and are not implicated.
- **Anything outside the twelve ratified scope items.** Scope membership was ratified by the
  operator on 2026-08-27 as a closed set; nothing is added downstream by interpretation.

## 2. ORCA Object Model

| Object | Definition | Where it lives |
|---|---|---|
| `Roster` | The ordered set of `Account` rows (`account_uuid` / `label` / `enabled`). Exists in **two copies**, and the whole problem is their relationship. | `config.toml` `[[account]]` rows; daemon `Vec<Account>` |
| `ConfigFile` | The durable `config.toml`, mode `0o600`, replaced by atomic temp+rename. Carries the roster plus six non-roster sections and **no secret material**. | `~/Library/Application Support/sessiometer/config.toml` |
| `WriteVerb` | Any verb that persists a roster and may notify the daemon. Partitioned by **intent**: append-only (`login`, `capture`) vs mutating (`remove`, `enable`, `disable`, `import`, `config set`). | `src/capture.rs`, `src/cli.rs`, `src/daemon/commands.rs` |
| `RosterReload` | The daemon-side adoption of a freshly-written on-disk roster. | `adopt_roster_reload` → `reconcile_roster` |
| `RosterBackup` | A durable prior copy of `ConfigFile`, retained across writes. **Does not exist today.** | new |
| `RejectionReason` | The redacted machine tag on a refusal, mirrored across the Rust↔Swift boundary. | `CaptureRejection` (Rust + `apps/menubar/Sources/CaptureAck.swift`) |
| `DivergenceReport` | An observation that the two `Roster` copies disagree. **Does not exist today.** | new |

**CTA inventory**

| Object | CTAs |
|---|---|
| `Roster` | Load · Persist · Reconcile · Restore |
| `ConfigFile` | Read · Write · Back up · Validate |
| `WriteVerb` | Refuse · Commit · Declare intent |
| `RosterReload` | Adopt · Refuse · Report |
| `RosterBackup` | Write · Enumerate · Restore |
| `RejectionReason` | Emit · Decode · Present |
| `DivergenceReport` | Detect · Report |

## 3. Requirements (EARS)

**Origin tags**: `user-ratified` — the operator ratified this item explicitly on 2026-08-27 as part
of the closed 12-item scope set. `pipeline-corrected` — the pipeline found the ratified item's
stated form would not have caught the incident and restated it; **these require explicit operator
ratification before the DoR gate can pass** (§ 12). `enrichment-derived` — a sub-requirement the
pipeline derived while structuring a ratified item, adding no new scope.

### Roster — the never-shrink invariant

**R-1** *(E1, user-ratified — outcome-stated so the design stage chooses the mechanism)*
If a write verb that can only append to the roster finds no `config.toml` on disk while a live
daemon holds a non-empty roster, then the system **shall** refuse the operation before any write,
exit non-zero, and leave both the on-disk file and the daemon's in-memory roster unmodified.

**R-2** *(E2, user-ratified)*
The socket-borne `capture` path **shall** satisfy R-1 identically to the CLI `login` path — the
refusal is a property of the operation, not of the entry point.

**R-3** *(B2.2, user-ratified — the pipeline corrected the ratified form on evidence; the operator
ratified the correction on 2026-08-27, including the fail-closed default in R-3a)*
When a roster reload originates from an **append-only** verb (`login`, `capture`), the daemon
**shall** refuse to adopt a roster whose account count is **lower** than the live in-memory count,
retain the in-memory roster, and report the refusal per R-14.

> **Why this was corrected, and what the operator ratified.** B2.2 was ratified as *"floor the
> never-narrow-to-**empty** invariant at `reconcile_roster`"*. The incident's transition was
> **6 → 1**, not 6 → 0 (I-2, I-7): `login` wrote a one-account file and the daemon adopted it. An
> empty-roster floor **would not have fired on this incident.** The invariant has to be
> *shrink*-scoped, not *empty*-scoped.
>
> Scoping it by originating intent is what keeps it correct in the other direction: `remove` of the
> last account is a legitimate 1 → 0, and `disable`/`remove` legitimately shrink. A blanket
> never-shrink rule would break them. **Consequence the design stage must resolve:** the reload
> notification does not currently carry the originating verb's intent, so R-3 implies a
> notification-payload change — a wire change, not a local guard.
>
> **The empty floor is worse than merely insufficient — it fires only where it is wrong.** An
> append-only verb, by construction (I-1), always leaves **at least one** account in the file it
> saves. So a reload *triggered by* `login` or `capture` can never present zero, and an
> empty-scoped floor can never fire on the append-only path at all — not on the incident (row 2 of
> § 7, 6 → 1), and not on its close sibling (row 4, a zero-account file plus an append, also → 1).
> The only trigger that *can* present zero is a removal verb — where reaching zero is the operator's
> legitimate intent (R-4). An empty floor is therefore inert on every path that needs it and
> obstructive on the one path that does not.

**R-3a** *(user-ratified 2026-08-27, with R-3)*
If a roster-reload notification arrives carrying **no** declared intent, then the daemon **shall**
apply R-3's append-only treatment — the refusing one. The default is fail-closed by requirement, not
by convention: `notify_daemon_roster_reload()` takes no arguments today, so an intent-less
notification is both the legacy shape and what a future verb added without declaring intent will
send (Premortem P-4).

**R-4** *(enrichment-derived from R-3)*
Where a reload originates from a **mutating** verb (`remove`, `disable`, `enable`, `import`,
`config set`), the daemon **shall** adopt the roster as read, including a legitimate reduction to
zero — R-3's refusal **shall not** apply.

**R-5** *(E3, user-ratified)*
`login` **shall** carry the roster it parses at its first config read through to the point of
persistence, rather than discarding all but `c.login` and re-reading (I-5). Where the two reads
disagree, the operation **shall** be refused per R-1 rather than silently preferring either.

**R-6** *(E6 keystone, user-ratified — deliberately outcome-stated; the mechanism is a design decision)*
The system **shall** distinguish *"the operator has never configured this"* from *"the operator's
configuration has disappeared"* before any append-only write commits, and **shall** treat only the
first as a first run.

> **This is the keystone, and it is stated as an outcome on purpose.** *"May a write verb depend on
> the control socket before committing?"* is a genuine architectural fork, and the design stage owns
> it. What this PRD fixes is the outcome, not the route. Two candidate routes, with the tension
> stated so the design stage cannot ignore it:
>
> - **Socket-consulting.** Ask the daemon whether it holds a roster. Strongest signal — it is the
>   surviving copy. **But** with no daemon running, *"no daemon"* is indistinguishable from *"no
>   prior roster"*, so the refusal degrades to permissive exactly where nothing would notice.
> - **Absent-config-alone.** Refuse on the absent file regardless of daemon state, with an explicit
>   first-run affordance. This is what `perform_config_set` already does
>   (`src/daemon/commands.rs:392-397`) — an in-repo, already-ratified precedent needing no socket
>   dependency — at the cost of putting a step in front of a genuine first run.

**R-7** *(E7, user-ratified)*
The committed test asserting that reconciling to an empty roster is *"a degenerate-but-valid runtime
state"* (`src/daemon/commands.rs:2577-2591`) **shall** be revised to assert the R-3/R-4 behaviour
instead. A test that blesses the failure mode is a gate that certifies the defect.

### RosterBackup — durability

**R-8** *(B3.1, user-ratified)*
When the system replaces `config.toml`, it **shall** first retain a durable copy of the previous
contents, and that copy **shall** carry the same `0o600` mode as the original (`FILE_MODE`,
`src/paths.rs:56`).

**R-9** *(B3.1, user-ratified)*
The system **shall** provide an operator-invocable path to enumerate retained backups and restore
one, without requiring the operator to hand-edit `config.toml`.

> Backups are credential-safe: `config.toml` keys accounts by `account_uuid` / `label` and carries
> no secret material (`src/config.rs:17-20`), and
> the Keychain items survived the incident untouched (I-12). Retention depth and location are
> design decisions, not requirements.

### DivergenceReport — detection

**R-10** *(B3.2, user-ratified)*
While the daemon is running, it **shall** detect that its in-memory roster and the on-disk roster
disagree, and **shall** report the divergence per R-14. It **shall NOT** resolve the divergence by
writing (§ 1b Out of Scope).

### RosterReload — observability

**R-11** *(E4, user-ratified)*
`config validate` **shall NOT** report a config carrying zero accounts as unqualifiedly valid.
Today it emits `"{path} is valid (0 accounts)"` at exit 0 from
`render_config_validate` (`src/cli.rs:2181-2189`). Nothing on that path calls
`require_roster`. Its three call sites are `src/use_account.rs:988`, `src/poke.rs:174`, and the
`config.require_roster()` in `run` at `src/cli.rs:1389` — the startup gate `src/error.rs:272` names.

**R-12** *(B3.4, user-ratified)*
Where a new machine-readable refusal reason is introduced, it **shall** be present in the Rust
`CaptureRejection`, in the Swift mirror, and in the panel's authored capture-states reference,
in the same change. `CaptureAck.swift:101` throws `DecodeError.unrecognized` on an unknown reason —
the decoder **fails closed**, so a Rust-only addition lands as a panel decode failure, not as a
graceful unknown.

**R-13** *(E4, user-ratified)*
`Error::RosterEmpty`'s operator-facing message **shall NOT** assert that no accounts have ever been
captured. Today it reads *"no accounts captured yet — run `sessiometer capture`"*
on `RosterEmpty` (`src/error.rs:279-280`) — true for a first run, and actively misleading after a loss, which is
precisely when an operator reads it.

**R-14** *(E4, user-ratified)*
The daemon **shall** report a roster-reload outcome — adopted, refused, or failed — to a durable
destination an operator can inspect after the fact. `eprintln!` to the launching terminal is not
such a destination (I-9).

**R-15** *(B3.3, user-ratified — prerequisite for R-14)*
The observability event vocabulary **shall** carry a roster-reload event type recording the outcome
and both roster counts. None exists today: `src/observability.rs` carries capture outcomes but no
reload event, so R-14 currently has nowhere to write to.

### WritePath — serialisation

**R-16** *(E5, user-ratified — latent hazard, NOT this incident's cause)*
Concurrent `config.toml` writers **shall NOT** be able to publish a partial or interleaved file.
`reconcile_login`'s save sits **outside** the swap lock, deliberately and by documented intent
(`src/capture.rs:812-816`), matching the identical note on the `AccountImport` merge path
(`AccountImport`, `src/cli.rs:4935-4941`). The investigation established this produces
a cross-publish or an `Err(Io)` — **never an empty roster** — so it is explicitly *not* the cause
here, and is tracked separately on its own merits.

### Cross-cutting — verification

**R-17** *(enrichment-derived)*
Every requirement above that changes daemon or CLI behaviour **shall** be covered by a test that
fails against the pre-change code. The incident's specific transition — a live six-account daemon,
an absent config, an append-only verb, a resulting one-account file — **shall** be a named
regression test.

**R-18** *(enrichment-derived)*
Changes **shall** preserve issue #139's behaviour: a running daemon still picks up on-disk roster
**additions** without a restart. A guard that suppresses the reload is a regression.

## 4. Acceptance Criteria (GWT + BUT NOT)

**AC-1 — the incident, refused** *(R-1, R-3, R-17)*
**Given** a running daemon holding six accounts, **and** no `config.toml` on disk,
**When** the operator runs `sessiometer login <label>`,
**Then** the verb refuses before writing, exits non-zero, names the disagreement in operator-facing
terms, and the daemon still holds six accounts.
**BUT NOT** by disabling the roster reload; **BUT NOT** by refusing when the daemon's roster is
genuinely empty and this is a first run; **BUT NOT** by leaving a partially-written `config.toml`.

**AC-2 — the same refusal over the socket** *(R-2, R-12)*
**Given** the same preconditions, **When** the capture is requested over the control socket,
**Then** the daemon refuses with a redacted machine reason, writes nothing, and the menu-bar panel
**decodes and presents** that reason.
**BUT NOT** as an `unrecognized` decode error; **BUT NOT** with a reason absent from the Swift
mirror; **BUT NOT** with a raw path, label set, or account count in the redacted tag.

**AC-3 — shrink refused, removal allowed** *(R-3, R-4, R-7)*
**Given** a daemon holding six accounts, **When** a reload originating from `login` or `capture`
presents a roster of fewer than six, **Then** the daemon retains its six and reports the refusal.
**And Given** the same daemon, **When** a reload originating from `remove` presents a smaller
roster — including zero — **Then** the daemon adopts it.
**BUT NOT** by keying the decision on emptiness alone (the incident was 6 → 1); **BUT NOT** by
blocking a legitimate `remove` of the last account.

**AC-4 — first run still works** *(R-6, R-18)*
**Given** no `config.toml` and no daemon roster — a genuine first run — **When** the operator
captures their first account, **Then** it succeeds and the roster contains one account.
**And When** a second account is captured against a running daemon, **Then** the daemon picks it up
without a restart (#139).
**BUT NOT** requiring a daemon restart; **BUT NOT** requiring the operator to pre-create a config
file by hand.

**AC-5 — the loss is survivable** *(R-8, R-9)*
**Given** a healthy multi-account roster, **When** `config.toml` is replaced or removed **by any
means, including one this project cannot attribute**, **Then** a prior copy at mode `0o600` remains,
and the operator can enumerate and restore it without hand-editing TOML.
**BUT NOT** by retaining a backup readable by another user; **BUT NOT** by growing without bound;
**BUT NOT** by restoring silently over a roster the operator has since changed.

**AC-6 — divergence is visible** *(R-10, R-14, R-15)*
**Given** a running daemon whose in-memory roster no longer matches disk, **When** the divergence
persists, **Then** it is reported to a durable destination naming both counts.
**BUT NOT** by writing to disk to resolve it; **BUT NOT** via `eprintln!` alone; **BUT NOT** only at
the moment a write verb happens to run — the incident's divergence went unobserved for ≥21 days
precisely because nothing checked between write verbs (I-10).

**AC-7 — an emptied roster reads as a problem** *(R-11, R-13)*
**Given** a `config.toml` parsing cleanly with zero accounts, **When** the operator runs
`config validate`, **Then** the output does not present it as unqualifiedly valid.
**And When** an operator hits the empty-roster error after a loss, **Then** the message does not
assert that no account was ever captured.
**BUT NOT** by making a zero-account config a parse error — it is legitimate for a first run
(`src/config.rs:1045-1051`); **BUT NOT** by breaking `capture`'s ability to load a tunables-only
file to add the first account.

**AC-8 — concurrent writers cannot publish a partial file** *(R-16)*
**Given** two processes writing `config.toml` concurrently, **When** both complete, **Then** the
file on disk is exactly one writer's complete, valid output.
**BUT NOT** by serialising the daemon's read path behind a write lock; **BUT NOT** by moving
`reconcile_login`'s save inside the swap lock without addressing why it was deliberately placed
outside (`src/capture.rs:812-816`).

## 5. Quality Attributes (Planguage)

```
TAG:     RosterSurvivability
SCALE:   whether a healthy roster survives an arbitrary, unattributed removal of config.toml
METER:   fault-injection — delete config.toml out from under a running daemon, then run each
         append-only write verb; roster count before vs after
GOAL:    live roster count unchanged, and a restorable prior copy exists
PAST:    roster destroyed (6 → 1) with no prior copy and no record
```

```
TAG:     ReloadObservability
SCALE:   time from a roster-reload refusal or failure to an operator being able to see it
METER:   inspect the durable event destination after an injected reload failure
GOAL:    observable immediately after the event, with no terminal attached
PAST:    not observable in practice — eprintln! to an unattended terminal on a manual
         `run`; under `service install`, only in the ungoverned opt-in stderr log (I-9)
```

```
TAG:     DivergenceDetectionLatency
SCALE:   elapsed time between disk/memory roster divergence arising and being reported
METER:   remove an account row from config.toml under a running daemon; time to report
GOAL:    ≤ one poll cadence
PAST:    unbounded — ≥ 21 days in the incident, and only resolved destructively (I-10)
```

```
TAG:     RefusalParity
SCALE:   proportion of daemon refusal reasons the menu-bar panel decodes and presents
METER:   enumerate the Rust CaptureRejection variants; assert each has a Swift mirror
GOAL:    100% — a Rust-only variant is a panel decode failure (CaptureAck.swift:101)
PAST:    100% across the existing four tags; the gate is that it stays there
```

```
TAG:     FirstRunFriction
SCALE:   operator steps to capture a first account on a clean machine
METER:   count steps on a machine with no config.toml and no daemon roster
GOAL:    no more than today
PAST:    baseline — this is the budget R-6's chosen mechanism spends against
```

## 5b. Feature Completeness

| Feature | Requirements | Verdict | Gap |
|---|---|---|---|
| `absent-config-refusal` | R-1, R-2, R-5, R-6 | **NEAR-COMPLETE** | R-6's mechanism is deliberately open — a design decision, not a requirements gap. R-1/R-2/R-5 are complete as outcomes. |
| `never-shrink-invariant` | R-3, R-3a, R-4, R-7, R-18 | **COMPLETE** | R-3's correction is ratified and its notification-payload consequence is accepted. *How* intent is carried on the wire is a design choice, not a requirements gap. |
| `roster-durability` | R-8, R-9 | **COMPLETE** | Retention depth and location are design choices; the requirement is fully stated. |
| `divergence-detection` | R-10 | **COMPLETE** | Cadence is a design choice bounded by the Planguage GOAL above. |
| `reload-observability` | R-14, R-15 | **COMPLETE** | R-15 is an explicit prerequisite of R-14; the ordering is stated. |
| `rejection-vocabulary-parity` | R-12 | **COMPLETE** | Four-surface propagation enumerated, with the fail-closed decoder as the reason. |
| `write-path-serialization` | R-16 | **NEAR-COMPLETE** | Scoped as a latent hazard explicitly NOT this cause. Its own root — why the save sits outside the swap lock — is documented intent that Stage 2 must engage rather than override. |
| *(cross-cutting)* | R-11, R-13, R-17 | **COMPLETE** | — |

## 6. Success Criteria

**Leading indicators** (observable during and immediately after the work)

1. A regression test reproducing the incident's exact transition — live six-account daemon, absent
   config, append-only verb — fails against `c1b9de8` and passes after. *(R-17)*
2. Every Rust `CaptureRejection` variant has a Swift mirror, asserted mechanically rather than by
   review. *(R-12)*
3. The test blessing empty reconcile as degenerate-but-valid no longer exists in that form. *(R-7)*
4. A fault-injection run — delete `config.toml` under a live daemon, then run each append-only write
   verb — leaves the live roster intact for every verb. *(R-1, R-2, R-3)*

**Lagging indicators** (observable over subsequent operation)

5. No further roster-loss incident. Given a fleet of one operator this is a **weak** signal on its
   own — it is listed because it is the actual goal, not because absence of recurrence would
   evidence the fix.
6. A roster divergence, should one arise from the still-unattributed cause, is **reported** rather
   than discovered destructively. This is the honest test of R-10 + R-14, and unlike (5) it does not
   require the cause to recur to be informative: injected divergence exercises it.

**Decision gates**

- R-6's mechanism is chosen and recorded in the design doc **before** any work on R-1/R-2 begins —
  the refusal's shape depends on it.
- R-3's ratification (§ 12) lands **before** the never-shrink work is scoped into items; if the
  operator prefers the empty-floor form as originally ratified, that is their call to make
  explicitly, with the 6 → 1 consequence stated.
- R-15 lands before R-14 — there is otherwise no event type to write to.

## 7. State Matrix — config presence × daemon roster × verb intent

For an **append-only** verb (`login`, `capture`). "Today" is the behaviour at `c1b9de8`.

| # | `config.toml` | Daemon | Today | Required | Req |
|---|---|---|---|---|---|
| 1 | absent | not running | fresh roster, write, no notify | **refuse iff a prior-configuration witness is present**; allow if absent. Amended 2026-08-27 — see § 7a | R-1, R-6 |
| 2 | absent | running, **populated** | fresh roster, write 1 account, notify → **live roster destroyed** | **refuse**, write nothing, daemon unchanged | R-1, R-2, R-3 |
| 3 | absent | running, empty | fresh roster, write, notify | **allow** — a genuine first run | R-6, AC-4 |
| 4 | present, zero accounts | running, populated | append, write, notify → daemon **shrinks to 1** | **refuse** the reload; the file disagreeing with a populated daemon is a divergence, not an instruction | R-3, R-10 |
| 5 | present, zero accounts | running, empty | append, write, notify | **allow** — first account into a tunables-only file, the case `src/config.rs:1045-1051` exists for | R-4, AC-7 |
| 6 | present, populated | running, populated | append, write, notify → daemon widens | **allow, unchanged** — this is issue #139's intended path | R-18, AC-4 |
| 7 | present, populated | not running | append, write, no notify | **allow, unchanged** | — |
| 8 | present, unreadable | any | `Err(Io)` → verb aborts | **unchanged** — already correct (I-3) | — |
| 9 | present, malformed | any | parse error → verb aborts | **unchanged** — already correct (I-3, I-4) | — |

### 7a. Amendment — row 1, 2026-08-27 (design stage)

**Row 1 as originally written contradicted AC-4, and the design stage found it.** Row 1 required
refusal whenever the config was absent and no daemon was running; AC-4 requires a genuine first run
to succeed and explicitly forbids requiring a pre-created config file. **On a fresh machine before
the daemon is ever started, both preconditions hold at once** — so row 1 forbade exactly what AC-4
mandates, and the two could not both be satisfied by any implementation.

The design's R-6 mechanism dissolves it. A **prior-configuration witness** — a `Sessiometer/*`
Keychain item, or a non-empty usage sample store — is durable state independent of both the config
file and the socket, and it distinguishes the two cases row 1 called "indistinguishable":

| Fresh machine (AC-4) | After a loss (the incident) |
|---|---|
| no Keychain items, empty usage store → **witness absent → allow** | six Keychain items survived, usage store populated → **witness present → refuse** |

Row 1's original rationale — *"permissiveness here is what a socket-consulting guard would silently
fall back to"* — is preserved, not weakened: the witness is read without the socket, so the refusal
holds precisely where a socket-consulting guard would have degraded to permissive.

**Provenance**: authored by the design stage, applied rather than deferred because the row as
written was internally unsatisfiable and admits exactly one coherent resolution. Rows 2–9 are
unchanged. Full derivation: `docs/design/roster-loss-prevention-solution-design.md` § 5 D-1.

Rows 2 and 4 are the incident. Row 3 is why R-1 cannot simply be *"refuse when the config is
absent"* without R-6 resolving how a first run is recognised. Rows 8 and 9 are recorded to pin that
they are **already correct** and must not regress — the single `None` arm is load-bearing.

For a **mutating** verb (`remove`, `disable`, `enable`, `import`, `config set`), rows 1–5 are
unchanged from today: a reduction, including to zero, is the operator's stated intent and is adopted
(R-4). `config set` already refuses on an absent config with `ConfigSetRejection::NoConfig`
(`src/daemon/commands.rs:392-397`) — the
precedent R-6 can propagate.

## 8. Assumption Registry

| ID | Assumption | Importance | Evidence | Verdict | Signpost | Hedge |
|---|---|---|---|---|---|---|
| A-1 | The file's removal was external to `sessiometer` and will not recur through a code path | **High** | 🔴 weak — exhaustive enumeration found no removing path (I-11), but the cause is a stated ABSTAIN | **test** — the operator-run APFS snapshot probe, expiring 2026-08-28 | A second unexplained disappearance | Build durability (R-8) as if it *will* recur; do not let A-1 justify skipping it |
| A-2 | A roster backup fully remedies the loss | **High** | 🟢 strong — credentials survived in Keychain; the roster carries no secrets (I-12, `src/config.rs:17-20`) | **decide** | A loss where Keychain items are also gone | If ever falsified, durability scope widens to the credential store — out of scope today |
| A-3 | Two weeks of evenings covers the twelve items | **Medium** | 🟡 moderate — blast radius is three call sites of one function, but R-3 and R-12 each cross a boundary | **defer** | R-3's wire change exceeding a day | Cut from the § 1b reverse order — divergence detection and serialisation slip first |
| A-4 | Backup-on-write captures a **good** roster before the loss | **High** | 🔴 weak — see Premortem P-2; the mechanism only helps if a good save preceded the deletion | **test** during design | A restore attempt yielding an already-empty file | Design must state what is backed up and when — this is R-8's real risk, not retention depth |
| A-5 | The event log is a destination an operator actually inspects | **Medium** | 🟡 moderate — durable and inspectable, but nothing prompts a read | **surface** in design | R-14 shipping and a divergence still going unnoticed | Pair the durable record with a surfaced signal (`status`, panel) rather than the log alone |
| A-6 | Reload-originating intent can be carried without breaking existing clients | **Medium** | 🟡 moderate — the notify is a local socket message between versions of one binary | **decide** — ratified 2026-08-27; the fail-closed default is now R-3a | A mixed-version daemon/CLI failing to parse | R-3a *is* the hedge, promoted to a requirement |

### Premortem (Phase 0, de-anchored — findings the ISO sweep cannot enumerate)

*Assume all twelve items shipped and the fleet was lost again. What happened?*

- **P-1 — the guard was worked around.** The refusal fires during a real outage; the operator, unable
  to log in, disables it or reaches for a `--force`. The workaround becomes the new destructive path.
  → Any override must be loud, single-use, and cannot be the path of least resistance. Feeds R-6.
- **P-2 — the backup captured the loss.** Backup-on-write retains the *previous* contents at save
  time. In the incident's own sequence — delete, then `login`, then save — the previous contents at
  save time were **nothing**. A naive backup-on-write would have retained an absent or empty file
  and dutifully overwritten nothing useful. **The backup that matters is the one written at the last
  *good* save, which must survive subsequent bad ones.** This is the sharpest finding here and no
  category checklist produces it. → A-4; R-8's design must state what is retained and what a write
  is forbidden to evict.
- **P-3 — the report had no reader.** R-14 routes reload failures to the event log instead of
  `eprintln!` — and nothing prompts anyone to read the event log either. Same failure, one layer up.
  → A-5.
- **P-4 — a new verb defaulted open.** A verb added later does not declare its intent, and the
  reload treats an intent-less notification as mutating. The invariant silently stops applying to the
  newest code. → A-6: the default must be the refusing treatment.
- **P-5 — the panel drifted unnoticed.** The Swift mirror lands, but `panel-goldens` is a
  deliberately **soft** gate (every step `continue-on-error`, per the repo's own CI notes) so it
  always reports pass. The refusal renders wrongly and no gate says so. → R-12's mechanical assertion
  must not be the golden gate alone.
- **P-6 — the keystone chose the degrading option.** R-6 resolves to socket-consulting; on the day
  it matters the daemon is down; *"no daemon"* reads as *"no prior roster"*; the refusal degrades to
  permissive, exactly as row 1 of § 7 warns. → This is why row 1 requires refusal rather than
  deferring to daemon state.

## 9. Cross-Cutting & Non-Functional Concerns

**9.1 Security.** No new credential surface. `config.toml` carries no secret material
(`src/config.rs:17-20`), so R-8's backups are credential-safe — **but** they must carry the same
`0o600` mode as the original (`FILE_MODE`, `src/paths.rs:56`); a world-readable backup would be a
new exposure of the account-uuid/label set. R-12's refusal reasons are redacted machine tags by
construction and must stay so — a bare machine code carrying no path, label, account count, or
credential in the tag itself (AC-2; the design's § 8 states the same enumeration). D-1's second
witness does **not** widen this surface: the usage store's `acct` field (`src/usage_store.rs:143`)
is the same roster `label` that `config.toml` already carries (`src/config.rs:356`), and may be an
operator-authored email since #444/#447 — but D-1 reads only non-emptiness, so no witness content
reaches a tag at all.

**9.2 Compliance & Regulatory.** N/A — a single-operator local tool holding no personal data beyond
the operator's own account labels, with no data leaving the machine.

**9.3 Reliability & Observability.** The whole PRD. Reliability: R-1..R-7 (the roster survives),
R-8/R-9 (it is restorable), R-16 (writes cannot interleave). Observability: R-10 (divergence is
detected), R-14/R-15 (outcomes are durably recorded), R-11/R-13 (the operator-facing copy tells the
truth). The governing measure is `DivergenceDetectionLatency` (§ 5), whose PAST is unbounded.

**9.4 Performance & Scalability.** Negligible by construction. Rosters are a handful of accounts —
`reconcile_roster`'s own comment notes the per-account scan is *"inconsequential"*. The one real
budget is R-10's cadence: a divergence check that stats or re-reads a small file each poll must not
perturb the poll loop. Bounded by the `DivergenceDetectionLatency` GOAL of one poll cadence.

**9.5 Operational.** macOS-only; no CI job compiles for another platform, so a green run says nothing
about portability. Any `src/**` change puts the PR in the `rust` path filter and owes the `test`,
`msrv` **and** `deny` jobs — `msrv` re-runs build+test on a different toolchain, and `cargo-deny` is
not part of the toolchain. Changes touching `apps/menubar/**` additionally owe `swift` and
`panel-goldens`; the latter is a soft gate whose verdict is only in its step summary (P-5). R-9's
restore path is an operator-facing affordance and must work with the daemon **running** — the
incident's daemon was up throughout.

**9.6 Lifecycle.** No new persisted schema version: the roster's on-disk shape is unchanged, so no
`STATUS_SCHEMA_VERSION` or `JSON_SCHEMA_VERSION` bump is implied. Two lifecycle surfaces do move:
R-15 adds an observability event type, and R-8 introduces a **new on-disk artifact** whose retention
and cleanup are design decisions — a backup that grows without bound is a defect (AC-5).
`FORMAT_VERSION` and its frozen migration fixtures are explicitly untouched (§ 1b).

## 10. Source Traceability

| Source | Reliability | Contributed |
|---|---|---|
| Forensic investigation, three rounds incl. an operator-falsifier round, 2026-08-27, grounded at `c1b9de8` | **C — secondary, but code-anchored** | Findings I-1..I-12. Every claim is re-verified against a cited source line in this document rather than trusted from the report — the report is gitignored scratch and not dereferenceable from here (§ 0) |
| Operator's own falsifier — *"login for some reason erased old accounts"* | **B — user-authoritative** | Overturned the investigation's first verdict. The operator was **right about the verb**; the report had exonerated `login` |
| Direct measurement of the live usage store, 2026-09-01 | **A — self-verifying** | Finding I-13 — the second prior-configuration witness the design's keystone rests on. Added because the design cited a survival claim this document did not carry; it is now measured here rather than inherited, so it survives the report being unreachable |
| Direct code read at `c1b9de8` | **A — self-verifying** | Every line anchor in § 1a, § 3 and § 7; the `perform_config_set` precedent; the `Config` empty-roster rationale; `FILE_MODE`; the `CaptureAck` fail-closed decoder |
| Operator scope ratification, 2026-08-27 | **B — user-authoritative** | The closed 12-item scope set; the boundary that nothing outside it is added |
| `git log` archaeology | **A — self-verifying** | `02e37f1` wired the reload as issue #139's fix — the Chesterton's fence on the reload |

## 11. Related Work — Generalize, Do Not Duplicate

- **#139** *(closed)* — *"a running daemon never picks up on-disk roster changes until restart"*. Its
  fix (`02e37f1`) introduced the reload this PRD guards. **Scoped for widening only**; narrowing was
  never considered. R-18 exists to keep #139 working.
- **#35** *(closed)* — removed the `MAX_ACCOUNTS=5` cap, which is why `plan_capture` is
  unconditionally append-only (I-1). That property is what makes R-3's intent partition sound.
- **#359** — established the capture rejection vocabulary as *"a bare machine error tag"* and the
  four current tags. R-12 extends that contract rather than inventing one.
- **#427** — recorded the same under-scoped fan-out shape: work scoped to one surface that in fact
  spanned CLI **and** daemon. The enrichment sweep here found a third `reconcile_roster` caller
  (a `reconcile_roster` call at `src/daemon/commands.rs:436`, on `perform_config_set`'s label
  path) that the investigation did not
  enumerate; § 7's mutating-verb row covers it.

## 12. Definition-of-Ready Verdict

| # | Check | Result |
|---|---|---|
| 1 | Validated problem statement | **PASS** — § 1 traced to Phase 0 framing, with three rejected framings recorded |
| 2 | Explicit out-of-scope declarations | **PASS** — § 1b, seven declarations plus appetite |
| 3 | Success and telemetry metrics | **PASS** — § 6: four leading, two lagging, three decision gates. Lagging indicator (5) is explicitly labelled weak rather than presented as evidence |
| 4 | Cross-cutting & non-functional concerns | **PASS** — § 9, all six subsections with content or `N/A` + rationale |
| 5 | Feature completeness verdict | **PASS** — § 5b, seven features verdicted, two NEAR-COMPLETE with their gaps named |
| 6 | Requirement provenance | **PASS-WITH-FINDINGS** — see below |

**Check 6 — provenance, and what was ratified when.** Seventeen of the nineteen requirements trace
to an operator ratification, in two rounds; the remaining two are the finding below:

- **Round 1 (scope membership).** The operator ratified the closed 12-item scope set, which carries
  R-1, R-2, R-5, R-6, R-7, R-8, R-9, R-10, R-11, R-12, R-13, R-14, R-15 and R-16.
- **Round 2 (the B2.2 correction).** The pipeline established that B2.2's ratified form — an
  *empty*-scoped floor — is **inert on every append-only path** and fires only on a legitimate
  removal-to-zero (see R-3's note). The operator was shown that evidence alongside the two
  alternatives, including the option of keeping the ratified form, and ratified the shrink-scoped,
  intent-partitioned invariant **together with its wire-change cost**. This carries **R-3**, **R-3a**
  and **R-4** (R-4 is R-3's necessary complement — without it, a legitimate `remove` would be
  blocked).

**Findings (non-blocking).** **R-17** and **R-18** are `enrichment-derived` and were not ratified
individually. Neither adds scope: R-17 requires that behaviour changes carry a test that fails
against the pre-change code, and R-18 requires that issue #139's widening path keep working. They
are recorded here rather than folded silently into the ratified set, and are flagged for the
operator to strike at the design stage if either is unwanted.

**Verdict: PASS-WITH-FINDINGS.** `dor_status: passed-with-findings`. The gate that mattered was
check 6, and it mattered for a real reason: R-3 contradicted what the operator had actually
approved, and no other check could have caught that — checks 1–5 validate each requirement's own
properties and would have passed a corrected-but-unratified R-3 without comment.
