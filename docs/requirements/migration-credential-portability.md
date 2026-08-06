---
title: Migration Credential Portability
scope: migration-export-import
created: 2026-08-04
status: draft
dor_status: passed-with-findings
source: /investigate + /scope, 2026-07-31 → 2026-08-04; working notes were transient scratch, not retained
# Provenance note, not a path: the HQ is a private repo-root sibling that no clone of this repository
# contains, so no depth correction makes it resolvable. R-10 SUPERSEDES REQ-MIG-B-007; see R-10.
parent-requirements: private HQ, REQ-MIG-* family (prd-export-import) — not dereferenceable from here
formulation:
  technical-architecture: complete
  testing-architecture: complete
  ux-ia: n/a          # no UI surface — CLI + daemon only
  ui-visual: n/a
features:
  canonical-promotion:   {stage: requirements, tracks: {technical-architecture: complete}}
  config-block-adoption: {stage: requirements, tracks: {technical-architecture: complete}}
  staleness-warning:     {stage: requirements, tracks: {technical-architecture: complete}}
  rotation-telemetry:    {stage: requirements, tracks: {technical-architecture: complete}}
  duplicate-label:       {stage: requirements, tracks: {technical-architecture: complete}}
  status-provenance:     {stage: requirements, tracks: {technical-architecture: complete}}
  migration-runbook:     {stage: requirements, tracks: {technical-architecture: complete}}
  # Added by the 2026-08-04 amendment pass (R-9 … R-16).
  import-scope-selection:  {stage: requirements, tracks: {technical-architecture: complete}}
  portability-allowlist:   {stage: requirements, tracks: {technical-architecture: complete}}
  no-secrets-removal:      {stage: requirements, tracks: {technical-architecture: complete}}
  artifact-lifetime:       {stage: requirements, tracks: {technical-architecture: complete}}
  export-liveness-probe:   {stage: requirements, tracks: {technical-architecture: complete}}
  migration-observability: {stage: requirements, tracks: {technical-architecture: complete}}
  uuid-validation:         {stage: requirements, tracks: {technical-architecture: complete}}
  credential-block-compat: {stage: requirements, tracks: {technical-architecture: complete}}
artifacts:
  design-doc: docs/design/migration-credential-portability-solution-design.md
  requirements-brief: docs/briefs/2026-08-04-requirements-migration-scope-and-portability.md
---

# PRD — Migration Credential Portability

> **Provenance warning, read before acting.** This PRD was authored by an AI pipeline (`/scope` Stage 1)
> from a `/investigate` run the operator drove on 2026-07-31. Two requirement families have different
> standing and the difference is load-bearing:
>
> - **R-1 … R-4** trace to what the operator explicitly asked to be written up. `Ratification: n/a`.
> - **R-5 … R-8** are pipeline-enriched. They were **ratified as in-scope** on 2026-08-04 when the
>   operator selected scope-membership option **B (all enriched)** — a bounded selection over an
>   enumerated set. That ratifies their *inclusion*, **not** their specific wording, thresholds, or
>   chosen mechanisms, each of which remains a reversible pipeline call. See § 11.
> - **R-9 … R-16** were added on 2026-08-04 by a second amendment pass, and their standing splits
>   again. **R-9, R-9d, R-10 are `user-stated`** — the maintainer proposed scope-splitting, ruled on
>   naming, and directed the `--no-secrets` removal in their own words. **R-11 … R-16 are
>   `council-added`**: they come from two `/council` rounds (`rust-architect`, `technical-architect`,
>   `security-architect`) and are ratified as *in-scope* via a second scope-membership **B** selection
>   over an enumerated 22-item set. Their mechanisms remain reversible pipeline calls.
>
> A second claim was **falsified during this amendment** — the design's AD-2 cost argument. It is
> recorded in § 9 (F-3), not quietly corrected.
>
> One claim carried into this pipeline was **falsified during authoring** and is recorded as such in
> § 9 rather than quietly dropped. Read § 9 before trusting any narrative of the incident.

## 1. Problem Statement

**Current state.** On 2026-07-31 the operator ran `sessiometer export` on machine A and
`sessiometer import --overwrite` on machine B. Two outcomes, both wrong, both silent:

1. `status` on B showed the **active** account's `EXPIRY` unchanged — the import appeared not to have
   landed at all.
2. Within four minutes, two imported accounts on B went `dead` and began demanding `claude /login`.
   The import had not failed. It had **succeeded**, and the success is what killed them.

**Affected users.** Every operator who moves a roster between machines — the single use case
`export`/`import` exists to serve (#145, #146, #148).

**Why now.** The export half shipped and is well-tested (**45** migration tests — 16 in `src/cli.rs`'s
#148/#149/#150 sections and 29 in `src/migration.rs`; § 9 names the seven this scope reasons about). The import half
was completed to the point of *restoring bytes* and stopped there. Nothing since has exercised it
against a **live, still-running source** — which is the only configuration a real migration ever has.

**The problem, stated as a mechanism rather than a list of bugs:**

> The artifact is treated as a **transferable credential**. It is not. It is a **point-in-time snapshot
> of a rotating secret**, and the source keeps rotating after the snapshot is taken. Nothing in the
> format, the CLI, or the docs represents that the snapshot has a shelf life — so the operator is
> handed a live grenade with no pin indicator.

Three branches, all live, all confirmed against the code:

- **(i) Import writes the wrong half of the credential store.** `import` writes each account's
  per-account stash (`Sessiometer/<account_uuid>`) and then `config.save()` +
  `notify_daemon_roster_reload()` (`src/cli.rs:4601-4663`). It **never** writes the canonical
  `Claude Code-credentials` item that Claude Code itself reads, never writes `~/.claude.json`, and
  never requests a swap. So the imported bytes are parked, not adopted — and the account the operator
  is *currently using* is the one account the import cannot reach.
- **(ii) The staleness is unrepresentable.** `Payload` carries exactly two fields — `config_toml` and
  `accounts` (`src/migration.rs:199-210`). There is **no export timestamp**, and `FORMAT_VERSION` is
  frozen at `1` (`src/migration.rs:97`; ADR-0006 pins v1 as the tested baseline). The artifact
  therefore cannot state when it was minted, and the importer cannot compute how far the source has
  rotated past it. Anthropic's token endpoint rotates the refresh token on every exchange (#262), so
  the window in which an artifact is valid is bounded by the source daemon's next refresh — measured
  at **under 4 minutes 14 seconds** in this incident.
- **(iii) The reporting surface hides (i).** `status`'s `EXPIRY` for the **active** account is read
  from the canonical item; for every **parked** account it is read from that account's stash
  (`src/daemon/snapshot_build.rs:45-53`). Since import writes only stashes, parked accounts appear to
  update (on their next poll, ≤ ~300 s) and the active one never does. The operator's first
  observation — "others updated and active did not" — is this asymmetry, exactly.

**A second mechanism, orthogonal to the first** (added 2026-08-04, council rounds 1–2):

The mechanism above explains branches (i)–(iii) completely — they are all consequences of a rotating
secret being snapshotted. It explains **none** of what the council then found, because those defects
are not about credentials at all:

> The artifact carries three payload classes with **different portability and trust properties**, and
> applies all three at **one trust level**. Rotating **credentials** are point-in-time. **Roster
> identity** is machine-independent and genuinely portable. **Settings** are not one kind of thing at
> all — they range from freely portable (`[tunables]`, `[stats]`), through machine-bound
> (`[migration].conflict_policy`, whose whole purpose is to encode *this* operator's choice), to
> **capability-granting** (`[login].claude_bin`, `[refresh].claude_bin`). The operator has no control
> over which classes apply, and the system enforces no floor on what may *ever* apply.

That single conflation produces three further defects, none visible from the incident:

- **Config adoption is an unattended code-execution path.** `claude_bin` resolves by absolutizing
  against cwd and accepting any `is_file()` — no allowlist, no signature, deliberately no symlink
  resolution (`src/paths.rs:773-807`). On a fresh target `import` adopts the artifact's whole config
  unconditionally (`src/cli.rs:4744-4750`), and the daemon then spawns that path on a timer.
- **A security parameter is downgradable.** `[migration].kdf_*` is adopted like a preference, so an
  artifact can weaken every *future* export from the target.
- **Local policy would become silently overwritable.** `[migration].conflict_policy` records a
  decision the target operator made. **Today it cannot be overwritten at all**: with an existing local
  config `apply_import` keeps that config and discards the incoming non-roster blocks entirely
  (`src/cli.rs:4744-4750`). `--settings` is what would newly allow adoption to overwrite it — and even
  then not for the import that adopts it (`resolve_import_overwrite` reads the local value first,
  `src/cli.rs:4628`) but for every one after. *(Corrected 2026-08-05: this read "is silently
  overwritable", describing a live defect. R-11c prevents a regression `--settings` would introduce;
  the distinction materially changes how D-1's recorded dissent reads.)*

**The two axes this scope adds are the two halves of that fix**: an **operator-selected scope** (which
classes apply — R-9) and a **system-enforced portability classification** (what may *ever* apply,
regardless of what the operator selects — R-11). Neither substitutes for the other: scope selection
without an allowlist is a blank cheque, because there is no artifact-inspection subcommand and so no
disclosure to make the consent informed; an allowlist without scope selection leaves the operator
still unable to say "roster only".

**Framing challenged, two alternatives rejected:**

- *"This is a keychain-permissions bug."* Rejected: the import wrote successfully, and the bytes were
  verifiably present — the accounts died from **replaying a superseded refresh token**, not from a
  failed write. The `dead` classification came back from Anthropic's endpoint (`window_secs=0`).
- *"This is server-side family revocation (RFC 9700 §2.2.2)."* Rejected on evidence: machine A
  refreshed normally at 12:51:54Z, ~7 h **after** B's rejected replay, with no intervening
  `credential_health` or `poll_refresh` event. Nothing revoked A's family. See R-1.

## 1b. Boundaries

### Appetite

**1 week (small batch)** for R-1, **R-2**, **R-4**, R-5, R-6, R-7, R-8 — each is local, evidenced, and
independently shippable. R-2 and R-4 join this batch now that AD-1 and AD-2 have closed their gates
(below): R-2 is the `use --force` gesture plus its swap-semantics note, R-4 an unconditional warning
string with no format change. Both are pure-output work, which is why they fit the same week.

**Not sized**: none. R-2 and R-4 were decision-gated when this section was written; **both gates
closed inside this PR**, so both are now sizeable.

- **R-2** (swap semantics) is settled by **AD-1 / design § 4.1**: `import` adds no canonical writer and
  names `use --force` as the gesture. Design § 14 rates it "✅ **Yes**, and cheaper than assumed".
- **R-4** (whether a `format_version` bump is acceptable) is settled by **AD-2 / design § 4.2**: no
  bump. Design § 16 marks R-4a "covered (resolved as a decline)".

*Corrected 2026-08-05 (seventh pass); this read "**Not sized**: R-2, R-4 … sizing them before their
decision would fabricate precision." The adjacent R-3 bullet below was updated for exactly this reason
and its two siblings were not — a delivery planner reading the stale line defers the two requirements
that actually explain § 1's incident branches (i) and (ii) out of the first wave.* **Amended
2026-08-05 (eighth pass): declaring them *sizeable* did not size them.** The first correction closed
the gates and left both out of every appetite bucket, so the planner it was written to protect still
had no appetite for either — the same defect one step along. They are now in the small-batch week
above.

**R-3 is now sized** — it was decision-gated on a merge policy that no longer needs authoring, and
its appetite **is** the 2-week R-9 + R-11 core below: scope selection plus the portability allowlist
are exactly what replaced its demand for a merge policy, so R-3 carries no separate bucket.

**2 weeks (the security core)** for **R-9 + R-11** together. These two ship as one unit or not at all:
R-9 without R-11 hands the operator a flag that adopts a code-execution path on request, and R-11
without R-9 leaves them unable to decline settings wholesale. Splitting them across releases produces
a strictly worse intermediate state than shipping neither.

**1 week (the hardening tail)** for R-10, R-12 … R-16 — each local, independently shippable, and
none blocking the core.

**Additional circuit breakers** — these extend the base list under § 1b's *Circuit breakers* heading
below (hit one, the item converts to a spike):

- **R-9**: if scope selection cannot be expressed by payload *presence* and requires a declared scope
  field, **stop**. A declared scope is attacker-controlled on the `--plaintext` path, which converts
  the feature from a control into theatre, and it would additionally invalidate the design's own AD-2
  cost argument (§ 9 F-3).
- **R-11**: if the portability classification cannot be made to fail closed on a newly-added `Config`
  key (R-11d), **stop and re-decide**. An allowlist that rots is a denylist with extra steps, and
  denylist-rot is the specific failure the allowlist was chosen to avoid.

**Circuit breakers** — hit one, the item converts to a spike rather than expanding:

- R-4: if a staleness signal cannot be derived from **v1 data already carried**, stop. A
  `format_version` bump is an ADR-0006 schema-evolution decision, not an implementation detail, and
  it must be decided rather than absorbed.
- R-2: if promoting an imported credential to canonical turns out to require re-entering the swap
  engine's lock discipline (#64) rather than reusing it, stop — that is a swap-engine change wearing
  an import-flag costume.

### Out of Scope

- **Any code, config, or asset change.** This PRD is a planning artifact. Implementation is `/do`'s.
- **Cross-platform migration.** #965 (mac-exported artifact importing on Linux) and #980 (retiring the
  off-macOS loud-failure guard) own that axis. This scope is platform-agnostic and must not
  re-file it.
- **Encryption and envelope security.** #147 owns them; the incident touched neither.
  **Narrowed 2026-08-04**: this boundary previously read "Encryption, KDF, and envelope security" and
  excluded KDF outright. R-11b now governs whether an artifact's `[migration].kdf_*` may be **adopted
  on import** — a *portability* question, not a cryptographic one. The KDF's construction, parameters,
  and envelope format remain #147's and stay out of scope; only the adopt/refuse decision is here.
- **Server-side reuse-detection behaviour as a *product* concern.** R-1 records what was observed; it
  does not propose depending on it. Anthropic may add family revocation at any time.
- **Any change to `[refresh]` cadence or the keep-warm gate (#468).** The source refreshing is correct
  behaviour, not the defect. The defect is that migration ignores it.
- **Recovering the two accounts killed on 2026-07-31.** Operational, not scoped: they need
  `sessiometer login <label>` on that machine.

## 2. ORCA Object Model

| Object | Definition | CTAs |
|---|---|---|
| **MigrationArtifact** | The versioned, optionally-encrypted container (`magic` + header + `Payload`). Carries `config_toml` + per-account stashes. Has a **shelf life** it cannot currently express. | `Mint`, `Read`, `DeclareFreshness` |
| **ManagedAccount** | One account's restorable secret material inside the artifact: `account_uuid`, `credential` blob, `oauth_account` block. | `Restore`, `Promote` |
| **CredentialSlot** | A place a credential can live on a machine. Two instances, and the distinction is the whole defect: the **canonical** `Claude Code-credentials` item (what Claude Code reads) and the per-account **stash** `Sessiometer/<account_uuid>` (what Sessiometer parks). | `Write`, `Read`, `Swap` |
| **RosterEntry** | A `[[account]]` config entry keyed by `account_uuid` — the *Claude* account uuid, stable across machines. Carries a mutable, **non-unique** `label`. | `Match`, `Append`, `Overwrite` |
| **RefreshOutcome** | The classified result of a token exchange: `refreshed` / `no_change` / `dead` / `error` (`src/refresh.rs:225-240`; the `RefreshEventOutcome` log vocabulary adds a fifth, `refreshed_not_restashed`, mapped from `refreshed`). Carries `rotated`, `window_secs`, `expires_before/after`. | `Classify`, `Log` |
| **ImportScope** | The set of payload classes the operator elected to apply on this import. Derived from CLI flags, **never** from the artifact. Two independent axes: accounts (roster + credentials) and settings (non-roster config). | `Select`, `Constrain` |
| **PortabilityClass** | The system's classification of a single `Config` key: **portable** (may be adopted), **machine-bound** (never adopted — it encodes a fact about *this* machine or *this* operator's choice), or **capability-granting** (never adopted — adoption transfers the ability to execute). Orthogonal to `ImportScope`: scope is what the operator *asked for*, class is what the system *permits*. | `Classify`, `Refuse` |

**The load-bearing relationship**: `import` must satisfy
`ManagedAccount → CredentialSlot{canonical, stash}`, but implements only
`ManagedAccount → CredentialSlot{stash}`. Every symptom in § 1 is a consequence of that one missing
edge, or of the fact that nothing measures the artifact's freshness before traversing it.

## 3. Requirements (EARS)

> **Reading the `Ratification:` item labels.** Each requirement ratified **through an enumerated
> selection** carries an item label (`E*`, `I*`, `M*`) naming the entry the user ratified it under.
> Two classes carry none, and their absence is meaningful rather than missing: the `user-stated`
> requirements R-1, R-2, R-4, R-9 and R-10 read `Ratification: n/a` because the maintainer stated
> them directly rather than selecting them off a list. **R-9d is `user-stated` but reads
> `user-ratified`** — the maintainer did not merely state it, they *ruled on the naming*, which is a
> ratification; see R-9d's own reconciliation note. *(Corrected 2026-08-05: this sentence listed R-9d
> among the `n/a` requirements while R-9d itself reads `user-ratified` and cites this preamble as its
> authority — a fourth incompatibility inside the note written to reconcile the first three.)*
> R-2a / R-4a / R-5a / R-6a read
> `pending-user` because they are not ratified at all yet. **Two different enumerated sets are in
> play**, and their labels overlap — `I1`, `I2`, `M1` and `M2` each exist in both. They are therefore
> qualified by namespace:
>
> - **`scope-membership B/first-pass`** — the 2026-08-04 first pass, which opened R-1 … R-8 and
>   issues #999–#1007.
> - **`scope-membership B/amendment`** — the 2026-08-04 amendment (this one), a 22-item selection
>   that added R-9 … R-16 and issues #1045–#1053.
>
> Without the qualifier a provenance audit cannot tell which selection a label indexes — the labels
> were never a single namespace, and the earlier unqualified form implied they were.
>
> **The namespace is not a number range, and reading it as one misattributes three requirements.**
> *Corrected 2026-08-04.* The qualifier records the selection a requirement was **ratified under**,
> which is not always the one that opened its number. **R-1a, R-3 and R-7 carry `B/amendment`
> despite sitting in the first pass's numeric range**, because the amendment re-opened them — most
> visibly R-3, whose original demand for a documented config-merge policy the amendment *replaced*
> with scope selection plus a portability allowlist. Read each requirement's own `Ratification:`
> line; do not infer provenance from where its number falls.

### MigrationArtifact

**R-1** — The system **shall** record, as a findings note under `docs/findings/`, the two
reuse-behaviour observations this incident produced: (a) **no family revocation was observed** — the
source refreshed normally ~7 h after the target's rejected replay; and (b) the **grace window on a
superseded refresh token is under 4 m 14 s**. Both are **n=1**, and the note **shall** mark them as
measured-not-modeled at that cardinality, per `docs/findings/README.md`.
`Origin: user-stated` ("record the #262 observation"). `Ratification: n/a`.

**R-1a** — *Where* R-1's note is filed, it **shall** be numbered `0262-*` after the originating spike
(#262), notwithstanding that #262 is CLOSED, and **shall** redact operator-chosen account labels per
the #463 public-safety rule. `Origin: AI-inferred-expansion (repo convention)`.
`Ratification: user-ratified 2026-08-04 (scope-membership B/amendment; M3 folded into E1)`.

**R-4** — *When* `import` reads an artifact, the system **shall** warn that the artifact's credentials
are invalidated by any refresh the source performs after the export, and **shall** state the safe
sequence — **naming the forcing form `use --force <label>`, and never `use <label>` unqualified**.
The warning **shall** fire on **every** credential-bearing import **that actually applies a
credential** — not conditionally on a freshness heuristic, which does not exist — until a freshness
signal exists to gate it on.

> **`--settings` applies no credential, so the warning has no referent there.** *Corrected 2026-08-05
> (twelfth pass); this read "every credential-bearing import", which the artifact's contents decide,
> not the operator's scope.* R-9a makes the flag a ceiling in both directions: `--accounts` ignores
> the artifact's config, and symmetrically `--settings` applies no roster entry and no credential
> (Cap-7.9). On `import --settings` against a credential-bearing artifact the artifact *bears*
> credentials, so R-4 as previously written fires verbatim and instructs `use --force <label>` — but
> nothing was imported, and on a fresh target the roster is empty, so that command returns
> `UseTargetNotFound` (`src/use_account.rs:449`). **The one warning this PRD treats as load-bearing
> would name a command that cannot succeed**, which is the dismissal-training failure RSK-1 exists to
> prevent, on the surface the whole scope is built around.
>
> The gate is *"did this import write a credential"*, which `apply_import` knows for free — not a
> freshness signal, which is R-4a's separate and still-open question. Neither R-9/R-9a-c, AC-9,
> Cap-7.9 nor R-4/AC-4/Cap-2.1 mentioned the other before this pass.

> **`--force` is mandatory in this string, and this is the highest-traffic site of that correction.**
> *Added 2026-08-05 (tenth pass); `--force` appeared in R-4, AC-4, § 4.2, Cap-2.1 and the spec **zero**
> times.* `use <label>` against the already-active account is a provable no-op — `SwapTarget::resolve`
> short-circuits on service-name equality (`src/use_account.rs:325-326`), pinned by
> `already_active_without_force_is_a_noop_success_with_zero_writes` (`:2490-2502`, asserting
> `canonical == b"A-token"`, `calls == 0`). R-8 makes the same demand for the *runbook*; this is the
> **runtime** string, emitted on every credential-bearing import, so an operator meets it far more
> often than the document. A warning written to prevent the incident must not instruct the operator to
> reproduce it.

`Origin: user-stated` ("warns on staleness"). `Ratification: n/a`.

**R-4a** — *Before* R-4 is implemented as anything richer than an unconditional warning, the system
**shall** determine whether a freshness signal is derivable from **v1 data already carried**.
`Payload` has no timestamp and `FORMAT_VERSION` is frozen at 1 (ADR-0006), so a computed staleness
check requires either a derived proxy or a schema-evolution decision. This is a decision, not an
implementation detail. `Origin: AI-inferred-expansion (feasibility, § 9 F-2)`.
`Ratification: pending-user` (mechanism only; R-4's inclusion is user-stated).

### CredentialSlot

**R-2** — *When* `import` restores an account that is the target machine's **active** account, the
system **shall** promote the imported credential to the canonical `Claude Code-credentials` item, or
**shall** refuse and tell the operator which command completes the adoption — **the forcing form
`use --force <label>`, never `use <label>` unqualified**, which is a provable no-op against the
already-active account (AC-2a). Silently parking bytes
the active slot will never read is the defect.
`Origin: user-stated` ("import does not promote to canonical"). `Ratification: n/a`.

**R-2a** — *Where* R-2 promotes to canonical, the promotion **shall** reuse the swap engine's existing
single-writer lock discipline (#64) rather than introducing a second writer to the canonical item.
`src/daemon/canonical.rs` already re-stashes an out-of-band canonical change into its owning account's
stash; a second uncoordinated writer would race it.
`Origin: AI-inferred-expansion`. `Ratification: pending-user`.

**R-7** — *Where* `status` reports `EXPIRY`, the system **shall** make the value's **provenance**
distinguishable, so that an active account served from the canonical item and a parked account served
from its stash are not silently conflated. Today both render identically
(`src/daemon/snapshot_build.rs:45-53`), which is what made the failed import look like a no-op.
`Origin: AI-inferred-expansion`. `Ratification: user-ratified 2026-08-04 (scope-membership B/amendment, item I3)`.

### RosterEntry

**R-3** — *When* `import` applies an artifact's non-roster blocks, the system **shall not** author a
per-block merge policy. It **shall** instead decompose the decision into two independent mechanisms:
an **operator-selected scope** (R-9) and a **system-enforced portability classification** (R-11).

> **R-3 was rewritten on 2026-08-04 and its original demand is withdrawn.** It previously required a
> "decided, documented policy" for the non-roster blocks, and § 17 of the design deliberately left it
> undesigned pending an ADR. The council found the demand itself mis-shaped: a merge policy answers
> *"which side wins, per block"*, which presupposes every block is the same **kind** of thing. They are
> not. `[tunables]` is a preference, `[migration].kdf_*` is a security parameter, and
> `[login].claude_bin` / `[refresh].claude_bin` are capability grants. **No win/lose policy can express
> "the operator may choose this one, and may never choose that one"** — which is the actual
> requirement. Decomposing into scope × class expresses it directly and needs no ADR for the merge
> question (R-11 still warrants its own ADR for the *classification*, per R-11f).
>
> The original in-code acknowledgement stands unchanged as evidence of the gap
> (`src/cli.rs:4737-4741`, "remains future work").

`Origin: user-stated` (the original demand, and the maintainer's own scope-splitting proposal that
replaced it) + `council-resolved 2026-08-04` (the two-axis decomposition).
`Ratification: user-stated` — the maintainer proposed the split in their own words; the allowlist half
is `council-added`, ratified in-scope via scope-membership B/amendment (item E7).

**R-6** — *When* `import` would append a roster entry whose `label` already exists on the target under
a **different** `account_uuid`, the system **shall** warn at import time. Duplicate labels are a
documented, accepted state (`src/cli.rs:5148-5149`: "labels are operator handles; uniqueness is not
enforced"), so the requirement is **not** to forbid them — it is that import must not create one
silently — **including when both colliding entries arrive in the same artifact**. `Origin: AI-inferred-expansion`.
`Ratification: user-ratified 2026-08-04 (scope-membership B/first-pass, item I2)`.

> **The collision can be inside the artifact, and on a fresh target that is the only place it can
> be.** *Added 2026-08-05 (eleventh pass); this said "already exists **on the target**", and every
> criterion and scenario put the collision between the target roster and the artifact.*
> `Config::validate` rejects an empty label and a duplicate `account_uuid` but **never** a duplicate
> label (`src/config/validate.rs:281-293`), and `render` writes `label =` per account
> (`src/config/render.rs:808`) — so a roster that already carries the documented, accepted collision
> mints an artifact carrying it internally. On a fresh target `apply_import` starts from an empty
> roster (`src/cli.rs:4744-4750`) and appends both. An implementer who reads "on the target"
> literally checks the incoming label against `local`'s roster, finds `local` is `None`, skips the
> check, and creates in one shot exactly the state R-6 exists to prevent — with all three Cap-3.1
> scenarios green.

**R-6a** — *Where* a duplicate-label roster exists, the system **shall** handle it **consistently**
across every site that resolves an operator label — which is **two different mechanisms over six call
sites**, not four commands. It does not today: `use` refuses with
`Error::UseTargetAmbiguous` (`src/use_account.rs:453`, exit code 6 per `src/error.rs:955`), and so do
**`poke`** (`src/poke.rs:290`) and **the daemon's control-socket swap** (`src/daemon/commands.rs:99`),
because all three share `resolve_target` — whose doc states it *"NEVER guesses"*
(`src/use_account.rs:438-441`). `enable`/`disable` (`apply_enabled`, `src/cli.rs:5152`) and `remove`
(`apply_remove`, `src/cli.rs:5221`) do not reach that resolver **at all**: each does an exact-label
`.find()`/`.position()` and silently takes the **first** match. Which behaviour is correct is a
decision, not a design.

> **The count and the mechanism were both wrong, and the mechanism is what matters.**
> *Corrected 2026-08-05 (twelfth pass); this read "all four label-resolving commands — `use`,
> `enable`, `disable` and `remove`".* On the duplicate label R-6 says `import` can create, `use` and
> `poke` refuse — while **`remove L` deletes the first match's roster entry and then its keychain
> stash** (`src/cli.rs:5195-5211`), silently, with no ambiguity check anywhere in that path. That is
> the concrete harm OQ-1 has to price, and no surface stated it.
>
> "Make the four consistent" is also not implementable as written: `enable`/`disable`/`remove` do not
> merely apply a *different policy* at the resolver, they never call it. Consistency here means
> routing them through `resolve_target` (or deliberately not), which is a code change with its own
> blast radius — `AccountLabelNotFound` and `UseTargetNotFound` are distinct errors with distinct exit
> codes.
>
> Enumerated from source rather than sampled — re-run `.tmp/enumerate.py`. Three prior passes each
> reported one more member of this set, which is what sampling a finite set looks like.

`Origin: AI-inferred-expansion`.
`Ratification: pending-user` (the inconsistency's *inclusion* is ratified; its resolution is not).

> **R-6a's command set was corrected on 2026-08-04.** The original wording named only two commands and
> framed the choice as "one of the two is wrong". Both halves were defective:
>
> - **`remove` was omitted, and it is the load-bearing case.** `remove_account`
>   (`src/cli.rs:5195-5211`) → `apply_remove` (`src/cli.rs:5219-5227`) resolves a label and **deletes
>   the keychain stash**. It is
>   the only one of the four whose first-match-wins behaviour is **irreversible** — `use` picks the
>   wrong active account (recoverable in one command) and `enable`/`disable` flips the wrong flag
>   (recoverable), but `remove` destroys credential material with no undo. A decision framed over
>   `use` vs `enable`/`disable` alone would settle the three cheap cases and leave the expensive one
>   to inherit whichever answer happened to win.
> - **The option set was wrong.** The original framing offered "make `enable`/`disable` refuse like
>   `use`" as a *change*, but refusing-on-ambiguity is what `use` already ships
>   (`resolve_target`, `src/use_account.rs:441-457`) — so that option is a no-op for `use` and the real
>   question is only whether the other two adopt it.
>
> Restated: the decision is **one policy across `use` / `enable` / `disable` / `remove`**, and
> `remove`'s irreversibility is the argument that should drive it.

### ImportScope

**R-9** — *When* `import` applies an artifact, the operator **shall** be able to select which payload
classes are applied — `--accounts` (roster + credentials) and `--settings` (non-roster config) — with
the **default being everything** — no narrowing on the scope axis, so this requirement alone leaves
today's behaviour intact. That is **scope-equivalence, not end-to-end byte-identity**: R-11's
allowlist binds regardless of the flag and independently changes the fresh-target outcome. Today no
such gesture exists: on a fresh target the artifact's whole config is adopted unconditionally
(`src/cli.rs:4744-4750`) and the operator cannot decline it — which is precisely the adoption R-11
now constrains. `Origin: user-stated`.
`Ratification: n/a` (maintainer-proposed).

**R-9a** — *Where* an artifact's scope is determined, it **shall** be derived from payload **presence**
— the **accounts** axis from the `[[account]]` entries parsed out of `config_toml` (credentials
additionally from `Payload.accounts`), the **settings** axis from the non-roster blocks parsed out of
`config_toml` — and **shall not** be read from any scope field the artifact declares about itself. On a `--plaintext` export nothing is authenticated (`src/cli.rs:4471-4479`), so
a declared scope is attacker-controlled: a hostile artifact would assert full scope and the control
would evaporate. The operator's flag is a **ceiling, never a floor** — `import --accounts` against an
artifact containing config ignores that config regardless of what the artifact claims.
`Origin: council-added` (3/3 convergent). `Ratification: user-ratified 2026-08-04 (scope-membership B/amendment, item E5)`.

> **Both axes live in `config_toml`; `Payload.accounts` is secrets only.** *Corrected 2026-08-04
> (fourth pass) — the previous wording of this requirement, and the note that defended it, were both
> wrong.* R-9a used to name the presence test as "(`config_toml` empty, `accounts` empty)", which
> mis-maps **both** axes:
>
> - **`accounts` empty ⇏ no roster.** `Payload.accounts` carries per-account *secret material* keyed
>   by uuid — `ManagedAccount` is `{account_uuid, credential, oauth_account}` (`src/migration.rs:220-232`)
>   with **no label and no `enabled`**, so a roster entry is not reconstructible from it. The roster
>   travels inside `config_toml` as `[[account]]` entries (`src/migration.rs:199-210`), and
>   `apply_import` iterates `incoming.roster` parsed from there (`src/cli.rs:4735`, `:4770`). The
>   committed test `a_config_only_artifact_imports_accounts_as_roster_entries_without_a_stash`
>   (`src/cli.rs:10619-10638`) builds a payload with **empty `accounts`** and asserts **two** roster
>   entries import. Deriving "the accounts axis is unavailable" from `accounts.is_empty()` would make
>   `import --accounts` skip a roster the tool imports today — a regression against a committed test.
> - **`config_toml` empty ⇏ roster-only.** Roster *and* settings both live there, so an empty
>   `config_toml` means **neither** is available. The artifact an earlier draft of Cap-7.6's spec
>   asked an author to hand-build was roster-**less**, not roster-only.
>
> **An earlier note here also argued the asymmetry was "not a defect" because `accounts` empty is
> export-reachable. That premise is retired by this very scope**: `gather_payload` produces an empty
> `accounts` only under `no_secrets` (`src/cli.rs:4533-4534`), and R-10 removes that flag — R-10b says
> it outright ("every artifact **with a non-empty roster** carries live credentials"). After R-10,
> neither field is empty on a self-minted artifact **whose roster is non-empty**; an empty roster
> still yields an empty `accounts`, because `gather_payload`'s roster loop builds one entry per
> roster account and a roster-less config is a supported state (see R-10b's note).
>
> **What survives.** R-9's circuit breaker still does not trip — scope is derived from parsed
> *content*, not from a field the artifact declares, and the operator's flag remains a ceiling. But
> the **settings axis cannot be reliably presence-derived**, for R-9c's own reason: a defaulted block
> is indistinguishable from a withheld one (`src/config.rs:1377-1396`), so `available(artifact).settings`
> is effectively always true for any self-minted artifact. That is tracked as **OQ-6** and must be
> settled before #1046 is implemented; it does not require a declared scope field, so it is a
> precision question, not a breaker.

**R-9b** — *Where* `--accounts` is selected, the config **shall** be **narrow-parsed** — deserialized
into a struct carrying only `account` — rather than fully parsed and then filtered. This is not an
optimization: `RawConfig` carries `#[serde(deny_unknown_fields)]` (`src/config.rs:1378`), which never
fires on blocks outside the parse path, so narrow-parse additionally repairs backward-import for
roster-only artifacts (R-16). The narrow struct must therefore **omit** `deny_unknown_fields` at the
top level; `RawAccount` retains its own (`src/config.rs:1399`), so per-account strictness is preserved.
`Origin: council-added` (`technical-architect`). `Ratification: user-ratified 2026-08-04 (scope-membership B/amendment, item E6)`.

**R-9c** — *Where* scope selection is added, `export` **shall not** gain a corresponding
config/roster narrowing flag. Every `RawConfig` field is `#[serde(default)]`
(`src/config.rs:1377-1396`), so an omitted block is indistinguishable from a default-valued one and a
receiver cannot tell *withheld* from *stock*. Narrowing export would break `Payload`'s documented
losslessness invariant (`src/migration.rs:203-206`), make the artifact irreversible, and mask R-16's
backward-import break behind a flag. The asymmetry is principled: **export scope is disclosure
hygiene; import scope is input validation**, and only the latter defends against an
attacker-supplied artifact — because the attacker controls the export.
`Origin: council-added` (`technical-architect`, `security-architect` concurring).
`Ratification: user-ratified 2026-08-04 (scope-membership B/amendment, item E4)`.

**R-9d** — *Where* the scope flags are named, they **shall** be `--accounts` and `--settings`.
`--config` is unavailable on two independent grounds: it is **reserved and value-bearing** for issue
#24's directory-override ladder (`src/paths.rs:443-444`, "The CLI flag itself is not wired yet"), and it is
**semantically wrong** — `account` is a field of `RawConfig`, so accounts *are* config and
`sessiometer config show` prints them. `--accounts` matches the vocabulary the tool already uses on
this surface: `IMPORT_USAGE` opens "rehydrate **accounts** from a migration artifact"
(`src/cli.rs:1290`). `roster` is internal Rust vocabulary and barely surfaces to operators.
`Origin: user-stated` (maintainer asked the question; evidence settled it).
`Ratification: user-ratified 2026-08-04 — the maintainer **ruled on the naming**, per § 3's preamble.`

> **R-9d's provenance was recorded three incompatible ways; this is the reconciliation.** *Corrected
> 2026-08-04 (fourth pass).* § 3's preamble says the maintainer *ruled on naming*; this requirement
> said `Ratification: n/a` (evidence settled it); § 11 says *no mechanism in the amendment set is
> user-ratified*. A later reader asking "may `--accounts`/`--settings` be renamed?" got three answers.
> The reconciliation: the **name is maintainer-ruled and is not a free pipeline call** — the evidence
> (`--config` is reserved for #24, and `account` is a `RawConfig` field so accounts *are* config)
> informed the ruling rather than replacing it. § 11's "mechanisms remain reversible" therefore
> carries **one carve-out: this flag name**. Renaming needs the maintainer, not a pipeline decision.

**R-10** — The system **shall** remove the shipped `export --no-secrets` flag
(`src/cli.rs`, `EXPORT_USAGE`). Roster-without-secrets is not a state this product supports. The
inverse is already unreachable by construction — `apply_import`'s merge loop is over the roster
(`src/cli.rs:4770`) with secrets keyed by uuid (`src/cli.rs:4789`), so a secret with no roster entry is dead
code — and the maintainer has ruled the forward direction out of the model. `Origin: user-stated`.
`Ratification: n/a`.

> **R-10 SUPERSEDES `REQ-MIG-B-007`, and that reversal must not be silent.** The HQ decision record
> (`../hq/strategy/prd-export-import.md`) specifies roster-only mode as *"the **V-001-failure
> fallback** AND a **first-class user choice** (migrate config without moving secrets)"*. R-10 deletes
> both halves. The maintainer may reverse a ratified record — but sixteen executors build against
> **this** document and none of them will read that one, so the reversal is stated here rather than
> left to be discovered.
>
> **Disposition of the V-001 fallback**: no longer needed. #145 is CLOSED and portability is
> confirmed, so the failure mode roster-only was the fallback *for* does not arise. **Disposition of
> the first-class user choice**: withdrawn by the maintainer's ruling — note that R-10's own evidence
> (`apply_import`'s roster loop) argues only the *inverse* case, so the forward direction rests on
> the ruling alone, not on code.
>
> *Recovered 2026-08-05 (twelfth pass) from PR #1057, which authored these twelve files in parallel
> from the same `/scope` run and merged first. That branch carried the HQ provenance layer; this one
> had lost it. Everything else #1057 held is present here or superseded by a later review pass — the
> reconciliation is recorded in the PR body.*

**R-10a** — *Where* R-10 removes a **shipped** flag, the removal **shall** follow a decided path:
hard-remove with a strict-usage error stating that roster-without-secrets is no longer supported, or
deprecate-then-remove across a release. Not yet decided. **There is no replacement to name** — R-9c
and AD-5 forbid any export-side narrowing flag, and `import --accounts` narrows what is *applied*,
not what the file *contains*, so it does not yield a secret-free artifact. Any wording that promises
one is unsatisfiable. `Origin: enrichment-expanded` (item I1).
`Ratification: user-ratified 2026-08-04 (scope-membership B/amendment, item I1)` — inclusion only; the path is undecided.

**R-10b** — *Where* R-10 lands, every artifact **with a non-empty roster** carries live credentials, so
`PLAINTEXT_WARNING` (`src/migration.rs:538-541`) is no longer moot for the flag reason. Its wording
**shall** be re-checked against that, and against R-12's shred mechanism, so the advice it gives is
one the tool can actually perform. **And** *where* R-10 removes `no_secrets`, the existing warning
guard **shall** be re-expressed over the artifact's actual credential count rather than deleted with
the flag. `Origin: enrichment-expanded` (item I2).
`Ratification: user-ratified 2026-08-04 (scope-membership B/amendment, item I2)`.

> **R-10 deletes the condition that keeps this warning honest — do not let it delete the guard.**
> *Added 2026-08-04 (fifth pass).* `export` today prints the warning as
> `if !no_secrets { eprintln!("{PLAINTEXT_WARNING}"); }` (`src/cli.rs:4475`), with the in-code reason
> *"nothing to protect, so the warning would misinform"*. Removing `no_secrets` removes the
> condition, not the hazard: **an empty roster also yields zero credentials**. `gather_payload`'s
> roster loop builds one entry per roster account (`src/cli.rs:4535-4546`, the `else` branch;
> `:4533-4534` is the `if no_secrets` arm that returns `Vec::new()`), so an empty roster
> produces an empty `accounts` with `no_secrets == false`. A roster-less config is a first-class
> supported state — `require_roster()` is enforced only at `run` (`src/config.rs:1145-1158`) and the
> committed test `accepts_a_roster_less_config_and_preserves_tunables`
> (`src/config/validate.rs:1089`) pins it — and `export` calls neither guard. Reached by removing the
> last account, or on a #58 capture-bootstrap file. The operator then gets *"it contains usable
> Claude Code account credentials in the clear"* over an artifact containing **none** — exactly the
> misinformation the existing comment was written to prevent. The guard must survive R-10 in the form
> *"warn iff the artifact carries at least one credential"*.

### PortabilityClass

**R-11** — *When* `import` applies non-roster config, the system **shall** classify every `Config` key
as portable, machine-bound, or capability-granting, and **shall** apply only the portable set —
**regardless of `--settings`**. The classification **shall** be implemented as an **allowlist**
(non-portable by default, portable only where explicitly marked), never a denylist.
`Origin: council-added` (`security-architect`).
`Ratification: user-ratified 2026-08-04 (scope-membership B/amendment, item E7)`.

**R-11a** — `[login].claude_bin` and `[refresh].claude_bin` **shall never** be adopted from an
artifact, regardless of any flag. Resolution absolutizes against cwd and accepts any `is_file()`, with
no allowlist, no signature, and deliberately no symlink resolution (`src/paths.rs:773-807`); the daemon
then spawns the result on a timer (`src/refresh_tick.rs:258` → `:273` → `src/refresh.rs:694` (`SpawnClaude::new`)). The refused capability has **zero
legitimate cross-machine use** — the value is a local path, so on the target it either does not exist
or names a *different* binary — and ADR-0030 documents `CLAUDE_BIN=…` as a trivially-available local
escape hatch, so refusal costs a genuine operator nothing. `Origin: council-added`.
`Ratification: user-ratified 2026-08-04 (scope-membership B/amendment, item E7)`.

**R-11b** — `[migration].kdf_*` **shall** be adopted only when the incoming value is **at least** the
local value **on every knob** (a monotonic floor, applied per knob); an incoming block that is
stronger on one knob and weaker on another **shall** be refused whole and reported, not adopted in
part. A fleet may standardize *upward*; nothing may downgrade. This kills
the 8 KiB / 1-iteration downgrade path (`src/config.rs:981-988`) without banning the legitimate case.

> **`kdf_*` is two knobs, so "at least" is a partial order.** *Added 2026-08-05 (eleventh pass); this
> read as one scalar comparison.* `kdf_memory_kib` (`8..=1_048_576`) and `kdf_iterations` (`1..=16`)
> are independent `u32`s (`src/config.rs:985`, `:988`). `1_048_576 / 1` against the shipped defaults
> `65536 / 3` (`:998-999`) is neither weaker nor stronger; a comparator written on the memory knob
> alone — the knob this very sentence foregrounds — adopts it and downgrades iterations 3 → 1 through
> the requirement written to prevent downgrades.

Scope note: this governs **adoption on import** only — the KDF's construction and parameters remain
#147's (see § 1b). `Origin: council-added`. `Ratification: user-ratified 2026-08-04 (scope-membership B/amendment, item E7)`.

**R-11c** — `[migration].conflict_policy` **shall not** be adopted. **Decision-gated on OQ-7** (design
§ 14): whether `--settings` adopts over an existing local config at all decides whether this clause
protects a live path or a hypothetical one — do not settle it by writing code. It encodes a decision the *target*
operator made. Today an artifact cannot overwrite it; `--settings` would newly allow it — affecting not
the import that adopts it (`resolve_import_overwrite` reads the local value first, `src/cli.rs:4628`)
but every import after. `Origin: council-added` (`technical-architect`; `rust-architect` dissented —
see § 9 D-1). `Ratification: user-ratified 2026-08-04 (scope-membership B/amendment, item E7)`.

**R-11d** — *Where* R-11's allowlist exists, a **mechanical check shall fail** when a new `Config` key
is added without a portability classification. Without it the allowlist rots exactly as a denylist
would — the next spawnable key auto-adopts and nobody notices until it is exploited — which is the
specific failure the allowlist was chosen to avoid. An unenforced allowlist is a denylist with extra
steps. `Origin: enrichment-expanded` (category: security, item M1).
`Ratification: user-ratified 2026-08-04 (scope-membership B/amendment, item M1)`.

**R-11e** — *Where* R-11 refuses a key, the refusal **shall** be surfaced to the operator. A silently
dropped `claude_bin` is indistinguishable from one that was never present.
`Origin: enrichment-expanded` (category: observability, item M2).
`Ratification: user-ratified 2026-08-04 (scope-membership B/amendment, item M2)`.

**R-11f** — *Where* R-11 establishes a portability classification, it **shall** be recorded as an ADR.
ADR-0006 governs *schema evolution* — whether the format may change — and is silent on *value
portability* — whether a value may cross machines. These are different questions and the second has no
home. `Origin: enrichment-expanded` (item I5).
`Ratification: user-ratified 2026-08-04 (scope-membership B/amendment, item I5)`.

### MigrationArtifact (lifecycle)

**R-12** — *When* `import` completes, the system **shall** provide a mechanism to destroy the source
artifact. Today `import` reads the file and leaves it (`src/cli.rs:4602`), while `PLAINTEXT_WARNING`
advises "delete it as soon as the import is done" — advice with no mechanism, printed only on the
`--plaintext` path, while an encrypted artifact is still a live-credential file behind one passphrase.
Under R-9's model the applied payload *can* narrow to the roster, but the default is everything
(AD-9) — and the file on disk is unchanged regardless, because scope selection is import-side only
(R-9c/AD-5). Whatever the operator selects, the artifact is a
**live-credential file**, which is what makes this urgent. `Origin: council-added` (`security-architect`, rounds 1 and 2).
`Ratification: user-ratified 2026-08-04 (scope-membership B/amendment, item E11)`.

**R-13** — *When* `export` runs, the system **shall** determine whether this machine's daemon is
running and surface it **on stderr**.

> **stderr, never stdout — the stream is load-bearing, not a detail.** *Added 2026-08-05 (twelfth
> pass); no surface named a stream, and the design's Interface-Change table said `export` **stdout**.*
> With `PATH` omitted, `export` writes the **artifact itself** to stdout
> (`src/cli.rs:4559-4565`; `EXPORT_USAGE` at `:1282` documents *"stdout if omitted"*). The existing
> `PLAINTEXT_WARNING` already takes this rule with the reason stated in the code: *"Warn on stderr —
> never stdout, which may carry the artifact"* (`src/cli.rs:4472-4474`). A liveness warning on stdout
> prepends its bytes to the artifact stream, which then fails `preamble.magic != MAGIC`
> (`src/migration.rs:360`) on import. The warning written to save the migration would destroy it —
> and only on the branch where it fires, so every no-daemon test stays green.

The design's own position is that the staleness hazard is "not detectable at
the target — it is only **preventable at the source**", and there is currently **zero** source-side
implementation of that thesis (`src/cli.rs:4455-4501` never asks). Liveness is locally probeable via
the existing control socket, via the read-only `daemon_liveness()` probe (`src/cli.rs:1885`) already
shared by `daemon status` and `daemon restart` — **not** `notify_daemon_roster_reload()`
(`src/capture.rs:335`), which is documented BEST-EFFORT, returns `()`, and swallows a connect
refusal, so it cannot answer the tri-state question this requirement asks.
`Origin: council-added` (`security-architect`).
`Ratification: user-ratified 2026-08-04 (scope-membership B/amendment, item E12)`.

**R-14** — *Where* `export` and `import` emit observability events, they **shall** both carry a
**sha256 digest** of the artifact, so an export and its corresponding import can be correlated after
the fact; and the `import` event **shall** additionally carry the **operator-requested scope**. This
fits the existing aggregate-only redaction discipline of
`Event::Export` / `Event::Import` (`src/observability.rs:1426-1442`) — no label, no token, no email.

> **The scope field is import-only, and the export half had no constructible value.** *Corrected
> 2026-08-04 (third pass).* This requirement previously demanded the operator-requested scope on
> **both** events, but there is no operator-requestable scope on `export` — R-9c, AD-5, AC-9c and
> Cap-7.5 all require `export` to gain no narrowing flag, and the one scope-ish export field
> (`mode: ExportMode`, `src/observability.rs:1429`) is driven by `no_secrets`, which R-10 removes,
> leaving it the constant `Full`. An implementer had only two readings, both wrong: log an inert
> constant (a field that gates nothing — the design's own ceremony anti-pattern, carrying none of the
> correlation value claimed), or add an export scope flag and violate R-9c/AD-5/Cap-7.5 in the same
> change. **Export-side correlation rides on the digest alone**, which is sufficient for it.

`Origin: council-added`. `Ratification: user-ratified 2026-08-04 (scope-membership B/amendment, item E13)`.

**R-14a** — *Where* R-14 logs a scope, it **shall** log the scope the operator **requested**, never the
scope the artifact **claims** — per R-9a. `Origin: council-added`.
`Ratification: user-ratified 2026-08-04 (scope-membership B/amendment, item E13)`.

**R-15** — *When* `import` reads a roster entry, the system **shall** validate `account_uuid` before
use. Today it is validated for **non-emptiness and uniqueness only**
(`src/config/validate.rs:281-293`) and is otherwise interpolated directly into a keychain service name
(`src/config.rs:370-372`, `format!("{STASH_PREFIX}{}", self.account_uuid)`) — its **shape and length
are unchecked**.
**Bounded, and the bound is verified**: `stash()` never reaches a filesystem path (no call site joins
it into one), and keychain service names are opaque strings rather than hierarchical paths — so
`Sessiometer/../x` is a literal name, not a traversal. The residue is **shape and length only**:
namespace squatting inside the prefix (`account_uuid = "../x"` and `" x "` both pass `validate`) and
unbounded length.

> **The empty-uuid case is already handled — do not re-specify it.** *Corrected 2026-08-04 (sixth
> pass); this requirement used to say `account_uuid` is "unvalidated today" and list "an empty uuid
> yielding the bare prefix" as residue.* `apply_import` parses the incoming roster through
> `Config::from_toml_str` (`src/cli.rs:4735`) → `Config::validate`, which rejects an empty-or-
> whitespace uuid at `src/config/validate.rs:281-284` and duplicates at `:289-293`; `apply_import`'s
> own comment states the invariant (*"unique non-empty account_uuid"*, `src/cli.rs:4733-4734`), and
> the first `.stash()` call is at `:4790`, **after** that parse. So the empty case cannot reach a
> keychain service name today. Specifying it as work to do would produce a test that is **green over
> unimplemented work** — the same failure class as the original `use <label>` no-op. Note also that
> shipped behaviour rejects the **whole artifact** with `ConfigInvalid`, not the offending entry, so
> an AC promising per-entry rejection would describe a behaviour change nobody scoped.

This is **input-validation hardening, not a critical finding**, and it is recorded at that severity
deliberately.
`Origin: council-added` (`security-architect` named the shape and explicitly declined to assert the
finding; the bound was verified during this amendment).
`Ratification: user-ratified 2026-08-04 (scope-membership B/amendment, item E14)`.

**R-16** — The system **shall** decide how a v1 artifact minted **after** commit `6fe3457` imports on a
binary built **before** it. `RawConfig` carries `#[serde(deny_unknown_fields)]`
(`src/config.rs:1378`), so the older binary **rejects** the `[credential]` block outright. That block
was added on 2026-07-29 — 26 days after ADR-0006 froze v1 as the tested baseline — and the break has
never been tracked. R-9b's narrow-parse repairs the roster-only case as a side effect; the
full-artifact case is unresolved. `Origin: council-added` (`rust-architect`, surfaced but never filed).
`Ratification: user-ratified 2026-08-04 (scope-membership B/amendment, item I4)`.

### RefreshOutcome

**R-5** — *Where* a refresh outcome is not `refreshed`, the system **shall not** emit a `rotated` value
that reads as a meaningful observation. `classify()` computes `rotated` as
`seeded_rt != after_rt` (`src/refresh.rs:434-437`) **before** the outcome is known; `Dead` is then
*derived* from `after_rt` being `Some("")` (`src/refresh.rs:445`), not the other way round. So on any
dead line whose seeded blob carries a parseable, non-empty refresh token, `rotated=true` is **true by
construction** and carries no information. (It is **not** every dead line: `rotated` falls through to
`_ => false` when the seeded blob is unparseable, and an empty seeded token gives `"" != ""` → false.
The remedy is unaffected — making the field unrepresentable removes it from *all* dead lines
regardless of which value they would have carried.)
`Origin: AI-inferred-expansion`.
`Ratification: user-ratified 2026-08-04 (scope-membership B/first-pass, item I1)`.

**R-5a** — *Where* R-5 changes the emitted field, the change **shall** be treated as a change with
existing consumers on **four** surfaces — three log lines **and one versioned wire** — not a
log-format change and not a cosmetic edit. `docs/findings/0465-*` derives a published
headline count (`141 rotated=true, 0 rotated=false`) from this field.

> **The fourth consumer is a versioned wire with a cross-language client, and it prices R-5
> differently from the other three.** *Added 2026-08-05 (twelfth pass); this requirement named
> `docs/findings/0465-*` as the consumer, and the whole artifact set costed R-5 as a log-format
> change.* `refresh_fold` folds the value into daemon state on **every** outcome — its own comment
> says *"Armed for EVERY outcome, including `Dead` / `Error`"* (`src/daemon/refresh_fold.rs:557`) —
> and `refresh_health_view` projects it onto the `status`/`watch` wire as
> `rotated: health.refresh_token_rotated.unwrap_or(false)` (`src/daemon/snapshot.rs:1403`). The
> consumer is Swift: `apps/menubar/Sources/WireModel.swift:98`, asserted in
> `apps/menubar/Tests/WireDecoderTests.swift` and pinned in committed JSON fixtures.
>
> The three log lines are free to fix. This one is not, and both available paths cost something:
> `.unwrap_or(false)` means the cheapest repair leaves `"rotated": false` on every `dead` /
> `no_change` / `error` account — **the exact uninformative value R-5 removes, now on a versioned
> surface** — while genuinely dropping the field is a `STATUS_SCHEMA_VERSION` change carrying the
> status/watch goldens plus the Swift fixtures and decoder assertions. **Which path is taken is a
> decision this scope has not made**; it is not resolvable by an implementer choosing the compiling
> one.

**Partially verified: 0465 carries no `dead` line** — its window ends ~2026-07-11 and the first `dead` line in the
local log is 2026-07-14, so no dead line is inside its sample. The requirement is forward-looking.
`Origin: AI-inferred-expansion (premortem P1)`. `Ratification: pending-user`.

### Operator Documentation

**R-8** — The system **shall** publish a migration runbook stating the safe sequence — **halt the
source's refresh (stop the source daemon) → export → import → `use --force <label>` → start the
target; never resume the source against the same credentials** — and stating why (the source's next
refresh invalidates the artifact). The runbook **shall** name the **forcing** form, and **shall not**
name `use <label>` unqualified. No such document exists.
`Origin: AI-inferred-expansion (category: docs)`.

> **`--force` is mandatory here, not stylistic (corrected 2026-08-04).** This sequence read
> `→ use →`. The runbook's whole subject is the migration case, and § 1 states that the account the
> operator is *currently using* is the one account the import cannot reach — so the runbook's reader
> is, by construction, in the active-account case where the unqualified form is the provable no-op
> this very PRD documents (AC-2a). An operator following the sequence literally would have restarted
> the target still holding the **stale** canonical token, with every command reporting success.
>
> This was the **seventh** site of the same correction found at the time — **not the last, and not the
> most dangerous**. *Corrected 2026-08-05 (tenth pass).* The R-4 chain (R-4, AC-4, design § 4.2,
> Cap-2.1, `import-staleness-warning.feature.md`) carried the unqualified form on **five** further
> surfaces, and that chain produces a **runtime string on every credential-bearing import** — an
> operator meets it far more often than this document, which a human must go find. Both superlatives
> are struck; the sweep is a claim to re-run, not a state to assert. It remains dangerous,
> because it is the only one a human reads and follows step by step. It survived the earlier sweep
> because that sweep searched the *design* for the claim; R-8 states it as **runbook prose in the
> PRD**, and R-8 is the one requirement with **no capability** gating it (§ 16 records it as
> `— (document)`), so no test could have caught it either.

`Ratification: user-ratified 2026-08-04 (scope-membership B/first-pass, item M1)`.

## 4. Acceptance Criteria (GWT + BUT NOT)

**AC-1 (R-1, R-1a)** — *Given* the 2026-07-31 log evidence, *When* the findings note is filed, *Then*
it states both observations, marks each **n=1 / measured-not-modeled**, and reconciles explicitly with
`docs/findings/0465-*`. **BUT NOT** asserting reuse-revocation is absent as a *property* of the
endpoint — one non-revocation is not a guarantee; **BUT NOT** carrying an unredacted operator label;
**BUT NOT** filed under a number other than `0262` on the grounds that #262 is closed.

**AC-2 (R-2, R-2a)** — *Given* an artifact containing the target's currently-active account, *When*
`import` runs, *Then* either the canonical item reflects the imported credential, or the operator is
told in the command's own output which command completes the adoption.
**BUT NOT** satisfied by writing only the stash and reporting success; **BUT NOT** by introducing a
canonical writer that bypasses the #64 swap lock; **BUT NOT** by promoting silently when the operator
asked only to import; **BUT NOT** — added 2026-08-04 — by naming a command that does not in fact
complete the adoption (see AC-2a).

**AC-2a (R-2, corrective)** — *Given* the imported account is the target's **currently-active** one,
*When* `import` names the command that completes adoption, *Then* the named command **shall** actually
replace the canonical bytes. **BUT NOT** `use <label>` unqualified.

> **Why this AC exists.** `import`'s planned guidance was to tell the operator to run `use <label>`.
> For the active account that is a **provable no-op**: `SwapTarget::resolve` short-circuits on
> `if account.stash() == active_stash { return Ok(GateOutcome::AlreadyActive); }`
> (`src/use_account.rs:325-326`) — a comparison of **service names**, never of contents. The committed
> test `already_active_without_force_is_a_noop_success_with_zero_writes` (`src/use_account.rs:2490-2502`) asserts exactly
> this: `canonical == b"A-token"` and `calls == 0`. So the canonical item keeps the **stale** token
> while both `import` and `use` report success — the original failure mode, reproduced through the
> remediation. The named command must therefore be `use --force <label>`, or adoption must not be
> delegated to `use` at all. Surfaced by council round 1; AC-2 passed in spirit and failed in letter.

**AC-3 (R-3)** — *Given* an artifact whose non-roster blocks differ from the target's, *When* `import`
applies it, *Then* the outcome is determined by **two independently inspectable** mechanisms — the
operator's selected scope, and the system's portability classification — and **not** by a per-block
win/lose policy. **BUT NOT** satisfied by authoring the merge policy R-3 originally demanded;
**BUT NOT** by a classification that lives only in a source comment; **BUT NOT** by leaving the current
implicit "local always wins" in place on the fresh-target path, where it does not hold at all
(`src/cli.rs:4744-4750` adopts the artifact's config wholesale).

**AC-9 (R-9, R-9a, R-9d)** — *Given* an artifact carrying both roster and settings, *When* the operator
runs `import --accounts`, *Then* the roster and credentials are applied and **no** non-roster block is,
*And* — the mirror — when the operator runs `import --settings`, the allowlist-filtered non-roster
config is applied and **no roster entry and no credential** is,
*And* running `import` with no scope flag applies **the same payload classes** it applies today.
**BUT NOT** asserting byte-identity with the pre-change target state — R-11's allowlist binds
regardless of the flag and independently changes the fresh-target outcome (§ 8, `config adoption`);
this AC's scope-equivalence is on the *scope-selection axis only*, exactly as Cap-7.2 now reads;
**BUT NOT** reading any scope declaration from inside the artifact; **BUT NOT** letting an artifact
widen the operator's selection; **BUT NOT** naming the flag `--config`, which is reserved for #24's
directory-override ladder.

**AC-9b (R-9b)** — *Given* an artifact whose **only payload of interest is its roster**, carrying
`[[account]]` entries plus a non-roster block the parser does not know, *When* it is imported **with
`--accounts`** by a binary whose `RawConfig` would reject that block, *Then* the import succeeds.
**BUT NOT** by calling it a *roster-only artifact* — `import-scope-selection.feature.md` pins that
term to `[[account]]` entries and **no** non-roster block, and an unknown block *is* a non-roster
block, so the two cannot both hold. **BUT NOT** by
relaxing `deny_unknown_fields` on the full-parse path; **BUT NOT** by removing `RawAccount`'s own
strictness; **BUT NOT** by asserting this on the default path, where the full parse still runs and
`deny_unknown_fields` (`src/config.rs:1378`) still rejects — that is OQ-5's question, not this AC's.

> *`--accounts` added to the* When *2026-08-04 (third pass).* R-9b scopes narrow-parse to
> `--accounts`; without the flag in the precondition this AC is unsatisfiable as literally written.
> The spec scenario had it right (`docs/specs/import-scope-selection.feature.md`); the AC did not.

**AC-9c (R-9c)** — *Given* the scope feature ships, *When* `export --help` is read, *Then* it offers no
config/roster narrowing flag. **BUT NOT** justified as symmetry with import — export scope is
disclosure hygiene, import scope is input validation, and only the latter defends against an artifact
the attacker minted.

**AC-10 (R-10, R-10a, R-10b)** — *Given* `--no-secrets` is removed, *When* an operator passes it,
*Then* they get a strict-usage error stating that roster-without-secrets is no longer supported,
*And* `PLAINTEXT_WARNING`'s wording has
been re-checked against the fact that every artifact with a non-empty roster now carries
credentials, *And* the warning guard is re-expressed over the artifact's credential count rather than
deleted along with the flag.
**BUT NOT** silently accepting-and-ignoring the flag; **BUT NOT** leaving the warning advising a
deletion the tool provides no mechanism for (R-12).

> **The strict-usage-error half of this AC is gated on OQ-4 — do not implement it until OQ-4 closes.**
> *Added 2026-08-04 (third pass).* R-10a records the removal **path** as undecided (hard-remove vs
> deprecate-then-remove), and design § 16 gives Cap-7.7 the same caveat and withholds its spec
> scenario for it. This AC did not carry the caveat, and an AC is *upstream* of the capability: if
> OQ-4 resolves to deprecate-then-remove, Cap-7.7 gets re-derived per its note and AC-10 would not,
> leaving an acceptance criterion demanding a non-zero usage error while the shipped behaviour is a
> deprecation warning with exit 0. The `PLAINTEXT_WARNING` half (R-10b) is **not** gated and stands
> as written.

**AC-11 (R-11, R-11a, R-11b, R-11c, R-11e)** — *Given* **a target with no existing config** and an
artifact whose config sets `[refresh].claude_bin = "./x"`, *When* it is imported **with `--settings`**,
*Then* the target's saved config does **not** contain that value, *And* the refusal is visible in the
command's output (R-11e), *And* an incoming `kdf_*` weaker than local **on any knob** is refused
while one stronger on every knob is accepted, *And* `conflict_policy` is not adopted (**R-11c is
OQ-7-gated** — this criterion asserts the fresh-target path, which is the only one where adoption
happens today; what `--settings` does on an existing-config target is undecided).
**BUT NOT** asserted against a target that **already has a config** — `apply_import` then keeps the
local config and discards the incoming non-roster blocks entirely (`src/cli.rs:4744-4750`), so every
clause above holds with **no allowlist implemented at all**. The fresh-target path is where adoption
happens and therefore the only one where a refusal is observable.
**BUT NOT** gated behind a second confirmation flag — an escalation flag becomes the exploit
instruction the error message hands the operator; **BUT NOT** implemented as a denylist;
**BUT NOT** relying on strip-on-export as the control, since the attacker controls the export.

> **The free-green trap, on the requirement resting on a recorded dissent.** *Added 2026-08-05 (tenth
> pass).* Cap-8.3 is R-11c's **sole** capability and R-11c is the clause D-1's dissent is about, so an
> assertion that passes with nothing built is worst here. This is the same class the docs caught four
> times elsewhere — R-15's empty uuid, AC-2a's `use` no-op, Cap-11.2's `[credential]`, Cap-7.4's
> roster-only — and missed on the allowlist's own criteria.

**AC-11d (R-11d)** — *Given* a new key is added to `Config` with no portability classification,
*When* the test suite runs, *Then* it **fails**. **BUT NOT** a lint that warns and passes;
**BUT NOT** a hand-maintained list that a reviewer is expected to notice.

**AC-11f (R-11f)** — *Given* the portability classification is implemented, *When* a reader asks why a
given key is portable, *Then* an ADR states the classification rule and its rationale.
**BUT NOT** folded into ADR-0006, which governs schema evolution — whether the *format* may change —
and is silent on value portability, whether a *value* may cross machines; **BUT NOT** satisfied by a
doc-comment on the allowlist constant, which records what was decided but not why.

**AC-12 (R-12)** — *Given* an import completes, *When* the operator asked for the artifact to be
destroyed, *Then* it is. **BUT NOT** advice printed without a mechanism; **BUT NOT** restricted to the
`--plaintext` path, since an encrypted artifact is still a live-credential file behind one passphrase.

**AC-13 (R-13)** — *Given* the source daemon is **`Responsive` or `AliveUnresponsive`, or the probe
returns `Err`** — three of `daemon_liveness()`'s **four** outcomes; it is `Result<DaemonLiveness>`
(`src/cli.rs:1885`) over a tri-state enum (`:1870-1878`) — *When*
`export` runs, *Then* the operator is told, because the artifact will be invalidated by the next
refresh. **BUT NOT** a warning printed unconditionally regardless of daemon state, which trains
dismissal; **BUT NOT** blocking the export — the operator may have a reason; **BUT NOT** treating
`AliveUnresponsive` as "not running"; **BUT NOT** mapping the `Err` arm to the quiet branch — see below.

> **The probe has FOUR outcomes, and this AC fails closed on the two that are not `NotRunning`.**
> *`Err` added 2026-08-05 (tenth pass); the eighth pass added it to the spec and #1050 and left this
> AC — and an AC is upstream of its capability, the rule this doc already applies to AC-10/OQ-4 and
> AC-16/OQ-5.* `daemon_liveness()` returns `Result<DaemonLiveness>` (`src/cli.rs:1885`), so the `Err`
> arm sits alongside the three `Ok` variants. An errored probe has **not** established the daemon is
> absent; if it is in fact running it will refresh and invalidate the artifact — so it warns, on the
> same fail-closed reasoning this note already makes for `AliveUnresponsive`. Only `NotRunning` is
> quiet, which is what keeps RSK-1's dismissal-training failure closed.
>
> *Original note (sixth pass); every AC, capability and scenario here was previously two-state:*
> `DaemonLiveness` (`src/cli.rs:1870-1878`) is `Responsive` / `AliveUnresponsive` / `NotRunning`, and
> the middle variant's own doc says it is *"a live daemon not answering yet (starting up, or wedged).
> Reported honestly, NOT as 'not running'"*. A wedged or still-starting daemon **holds the lock and
> will still refresh**, so it invalidates the artifact exactly as a responsive one does: it **warns**.
> Leaving the variant unassigned would make an implementer invent the fail-open choice at the one
> point where being wrong is silent — the operator ships a migration the source is about to
> invalidate. This is the decision, recorded rather than left to the implementer.

**AC-14 (R-14, R-14a)** — *Given* an export and its later import, *When* their events are read, *Then*
a common artifact digest correlates them, *And* the **import** event carries the operator-requested
scope. **BUT NOT** logging a label, token, or email — the aggregate-only redaction discipline of
`Event::Export` / `Event::Import` holds; **BUT NOT** logging the scope the artifact claims;
**BUT NOT** requiring a requested-scope field on the **export** event, which has no operator-
requestable scope to carry (R-9c, AD-5).

**AC-15 (R-15)** — *Given* a roster entry whose `account_uuid` is **malformed or over-length** (the
empty case is already rejected — `src/config/validate.rs:281-284`), *When* it is imported, *Then* it is
rejected before a keychain service name is derived from it.
**BUT NOT** by re-asserting the empty-uuid check that already ships, which would be green over
unimplemented work;
**BUT NOT** stated or filed as a path-traversal finding — `stash()` reaches no filesystem path, and
overstating it would manufacture a severity the evidence does not support.

**AC-16 (R-16)** — *Given* an artifact carrying a non-roster block the **current** parser does not
know, *When* the current binary imports it, *Then* the **version floor** — which released binaries
cannot read a `[credential]`-bearing artifact — is documented, *And* **— gated on OQ-5 —** that block
is tolerated on the artifact-config parse path rather than aborting the import.
**BUT NOT** left as today's bare `deny_unknown_fields` parse error; **BUT NOT** considered closed by
R-9b, which repairs only the roster-only case; **BUT NOT** asserting what an *already-shipped* binary
prints — that half is **unfixable by construction** (design § 4.9, § 14), not undecided, so no
decision will ever make it assertable; **BUT NOT** using `[credential]` as the unknown block —
`RawConfig` carries `credential: RawCredential` (`src/config.rs:1395`), so the current parser
**knows** it and a test built that way is green over unimplemented work. `[credential]` is the subject
of the *version-floor* half only.

> **The forward-tolerance clause is the OQ-5-gated half — do not implement it until OQ-5 closes.**
> *Corrected 2026-08-05 (seventh pass); this AC previously required tolerance unconditionally and hung
> its only OQ-5 caveat on the released-binary clause.* Those are two different things: the
> released-binary half is **unfixable** (we cannot patch shipped binaries), while OQ-5 decides the
> **in-reach** half — § 14's R-16 row scopes it to "the version-floor message and forward-tolerance".
> OQ-5's option (a) is a version floor **without** tolerance; under (a) this AC's tolerance clause and
> Cap-11.2's first half are unsatisfiable. The version-floor clause is **not** gated and stands as
> written. This mirrors the treatment AC-10 already carries for OQ-4.

**AC-4 (R-4, R-4a)** — *Given* an import that **actually applies a credential** (so: not
`import --settings`, which applies none — Cap-7.9), *When* it runs, *Then* the operator is
warned that a source refresh after export invalidates the artifact, and is given the safe sequence
**naming `use --force <label>`**.
**BUT NOT** naming `use <label>` unqualified — a provable no-op against the already-active account
(`src/use_account.rs:325-326`), which would make this warning instruct the operator to reproduce the
incident it exists to prevent;
**BUT NOT** gated on a freshness computation that does not yet exist; **BUT NOT** implemented via a
`format_version` bump absorbed as an implementation detail rather than decided against ADR-0006.

**AC-5 (R-5, R-5a)** — *Given* a refresh that classifies as anything other than `Refreshed` — that is
`NoChange`, `Dead` **or** `Error`, the three non-`Refreshed` variants of `RefreshOutcome`
(`src/refresh.rs:225-240`) — *When* the event is logged, *Then* **none of the four emitting surfaces**
presents `rotated` as an observation: the `refresh`, `poll_refresh` and `keep_warm` lines
(`src/observability.rs:2155`, `:2173`, `:2191`) **and the versioned `status`/`watch` wire**
(`src/daemon/snapshot.rs:1403`).
**BUT NOT** by deleting the field on the `refreshed` path, where it is meaningful; **BUT NOT** by
enumerating only `dead` and `error`, which silently exempts `NoChange`; **BUT NOT** without
checking whether any committed findings note's counts are drawn from a window containing non-`refreshed`
lines — **0465 is checked for `dead` only, which is NOT the whole criterion** (see R-5a's note).

> **`NoChange` is the variant this AC most easily loses, and it is not benign.** `rotated` is decided
> by the *token* differing (`src/refresh.rs:434-437`) while `NoChange` is decided by the *expiry*
> failing to move past the seeded marker (`:448-452`) — two independent comparisons, so a `no_change`
> line can carry `rotated=true` for exactly the same reason a `dead` line can. It is a live emitted
> outcome: `src/observability.rs:180` renders it as `"no_change"`.
>
> Do **not** extend this to `refreshed_not_restashed`. That is an *event* outcome
> (`RefreshEventOutcome`, `src/observability.rs:160`) mapped **from** `RefreshOutcome::Refreshed`
> (`src/refresh_tick.rs:843`) — the token did rotate and was simply not re-stashed, so `rotated`
> carries real information there. The event vocabulary has five values; the classification has four;
> this AC scopes to the three non-`Refreshed` *classifications*.

**AC-6 (R-6, R-6a)** — *Given* a target roster carrying label `L` under uuid `X`, and an artifact
carrying label `L` under uuid `Y`, *When* `import` runs, *Then* the operator is warned that a
duplicate label was created. *And* `use L`, `poke L`, `enable L`, `disable L`, `remove L` **and the daemon's
control-socket swap** thereafter agree on whether a duplicate label is resolvable — all six sites,
across both resolution mechanisms. **BUT NOT** by enforcing label uniqueness — that
contradicts the documented design position; **BUT NOT** by leaving `use` refusing while `enable`
silently picks first; **BUT NOT** by omitting `remove`, whose first-match-wins deletes a keychain
stash irreversibly (`apply_remove`, `src/cli.rs:5219-5227`) and is therefore the case that should
drive the policy, not the one left untested.
**Test-coverage criterion (M2)**: a case whose target roster is **not** a clone of the source config —
`the_migration_conflict_policy_default_drives_import_behaviour` builds its target as
`src_config.clone()` (`src/cli.rs:10741`), so every uuid matches by construction and this branch is
unreachable in it.

**AC-7 (R-7)** — *Given* a `status` render with one active and ≥ 1 parked account, *When* `EXPIRY` is
read, *Then* an operator can tell which slot each value came from.
**BUT NOT** by changing which slot is authoritative for the active account — that is R-2's decision,
not a display change.

**AC-8 (R-8)** — *Given* an operator about to migrate, *When* they consult the docs, *Then* the safe
sequence and its rationale are present, *And* the adoption step names **`use --force <label>`**.
**BUT NOT** as a comment in `src/migration.rs`; **BUT NOT** omitting the "never resume the source"
step, which is the step this incident violated; **BUT NOT** naming `use <label>` unqualified — the
runbook's reader is by construction in the active-account case, where that form is the AC-2a no-op.

**Coverage criterion spanning R-2 / R-4 (M2)** — the existing suite covers byte-faithful round-trip,
config-only artifacts, report redaction, and conflict policy (§ 9). It does **not** cover: the
active-account path, or an import of an artifact the source has rotated past. Both are required.

## 5. Quality Attributes (Planguage)

```
TAG:     ImportAdoptionCompleteness
SCALE:   fraction of imported accounts whose credential is readable by the consumer that needs it
         (Claude Code for the active account; the daemon for parked accounts)
METER:   integration test asserting canonical + stash state after import, for both cases
MUST:    1.0  — every imported account is readable by its consumer, or the operator is told it is not
GOAL:    1.0
PAST:    < 1.0 — the active account is never adopted (src/cli.rs:4601-4663)
```

```
TAG:     StalenessDisclosure
SCALE:   fraction of credential-bearing imports that disclose the artifact's shelf-life hazard
METER:   assertion on the import command's output
MUST:    1.0  (unconditional warning — R-4)
GOAL:    1.0
PAST:    0.0  — no warning exists on any path
```

```
TAG:     RotationSignalFidelity
SCALE:   fraction of emitted `rotated` values that carry information
METER:   assertion on each of the FOUR EMITTED surfaces, for every non-refreshed
         outcome: event=refresh (observability.rs:2155), event=poll_refresh
         (:2173), event=keep_warm (:2191), and the versioned status/watch wire
         (daemon/snapshot.rs:1403 -> WireModel.swift:98).
         NOT a unit test over classify(): the SCALE above is the EMITTED values,
         and classify() is the type. Measuring the type reads 1.0 with three of
         the four surfaces still emitting -- which is exactly what Cap-4.2 exists
         to catch, so the meter that certifies R-5 must not be blind to it.
         Outcome coverage {refreshed, no_change, dead, error}: omitting no_change
         exempts a live outcome (src/observability.rs:180) whose rotated value is
         derived independently of its expiry test (refresh.rs:434-437 vs :448-452)
MUST:    1.0
PAST:    < 1.0 — true-by-construction on any `dead` line with a parseable non-empty
         seeded token (src/refresh.rs:434-437); 6 such lines in the local log
         from 2026-07-14 onward
```

## 6. Success Criteria

1. A migration performed per R-8's runbook leaves **zero** accounts requiring `claude /login`.
2. A migration performed **against** the runbook (source left running) produces a **warning before**
   the damage, not a `dead` classification four minutes after it.
3. `status` after an import never shows a value that contradicts what was imported (R-7).
4. The #262 spike's two open sub-questions carry recorded n=1 evidence (R-1).

## 7. Assumption Registry

| # | Assumption | Risk | Validation |
|---|---|---|---|
| A-1 | `account_uuid` is stable across machines for one Anthropic account | 🟢 | **Validated** — `src/config.rs:341` ("the Claude `account_uuid`"), sourced from `oauth_account` (`src/capture.rs:449`). This is what makes R-6 a *narrow* case, not the incident's cause (§ 9 F-1) |
| A-2 | Anthropic rotates the refresh token on every exchange | 🟢 | Resolved by spike #262; corroborated by this incident's `window_secs=25246` then `window_secs=0` pair |
| A-3 | No family revocation on replay of a superseded token | 🔴 | **n=1 only.** A refreshed normally ~7 h after B's replay. One observation; the endpoint may change. R-1 must not overstate it |
| A-4 | Grace window < 4 m 14 s | 🔴 | **n=1 only.** Derived from one interval, not a swept bound. The true window may be far smaller |
| A-5 | A freshness signal is derivable without a `format_version` bump | 🔴 | **Unvalidated** — R-4a exists to settle it. `Payload` has no timestamp (`src/migration.rs:199-210`) |
| A-6 | Promoting to canonical can reuse the #64 swap lock | 🟡 | Plausible — `src/daemon/canonical.rs` already reconciles out-of-band canonical writes — but unverified against the import path's lock scope |

### Premortem (de-anchored — failure modes the requirement list does not enumerate)

- **P1 — A `rotated` fix silently re-baselines a published finding.** Mitigated **only for `dead`**:
  0465 carries no `dead` line (R-5a), but its 141-count is derived from *event type*, not outcome, so
  a `no_change` line inside it would still re-baseline. Tracked on #1004 as a pre-implementation check. Any *future* count over a window containing `dead` lines repeats 0465's methodology onto
  contaminated data.
- **P2 — R-4's unconditional warning becomes noise and is tuned out.** The operator migrates rarely;
  a warning on every import is cheap. But if R-4a later gates it on a computed signal, the gate must
  fail **closed** (warn when freshness is unknown), or the warning disappears exactly when the format
  is oldest.
- **P3 — R-2 promotes the wrong account.** "The target machine's active account" is a runtime fact the
  importer must read, not assume. If the daemon is mid-swap during import, the answer moves — which is
  precisely why R-2a routes through the existing lock.
- **P4 — The runbook (R-8) is written and never found.** A runbook that lives only in `docs/` and is
  not referenced from the `export`/`import` command help is a document that gates nothing.
- **P5 — R-6's warning fires on the common case and trains dismissal.** Given A-1, same-label/
  different-uuid should be **rare**. If it fires often, A-1 is wrong and that is the finding.

## 8. Cross-Cutting & Non-Functional Concerns

- **Security.** Every artifact in scope carries live bearer credentials. No requirement here may cause
  a credential to be written to a log, a report, or an error string — `the_import_report_names_labels_only_never_a_token_or_email`
  is the existing guard and must keep passing. R-1's findings note carries the #463 label-redaction rule.
- **Schema evolution.** R-4a is the only requirement that can reach `FORMAT_VERSION`. ADR-0006 governs;
  a bump is a decision with an ADR, not an implementation choice.
- **Concurrency.** R-2/R-2a touch the canonical item, which the swap engine (#64) and
  `src/daemon/canonical.rs` already write. Single-writer discipline is a hard constraint.
- **Backward compatibility.** A v1 artifact minted before any change must still import. R-4's warning
  is additive; R-3's policy must state what happens to an artifact whose blocks predate the policy.
- **Observability.** R-5 changes an emitted log field. `src/observability.rs` enumerates the event
  vocabulary; the change is a contract change with at least one documented consumer (0465). R-14 adds
  a digest to both the export and import events, plus a requested scope to the **import** event
  only; R-11e adds a refusal signal. All three must hold the
  existing aggregate-only redaction discipline.

**Added 2026-08-04 (amendment pass):**

- **Code execution is now an in-scope threat.** R-11a exists because config adoption is an unattended
  code-execution path, not a preferences merge. This is the first requirement in this PRD whose
  failure mode is arbitrary code execution rather than credential loss, and it changes the review bar:
  a reviewer who reads R-11 as "tidy up config handling" has misread it.
- **Consent without disclosure is not a control.** There is **no artifact-inspection subcommand** —
  `import` reads and applies (`src/cli.rs:4601-4663`), with no dry-run and no dump. So `--settings`
  means "adopt whatever config is in this file", not "adopt this specific `claude_bin`". This is
  precisely why R-11's allowlist, and not R-9's flag, is the security control. If a future change adds
  pre-apply disclosure, R-11's *rationale* weakens but its requirement does not: an operator reading
  `claude_bin = "./x"` mid-migration parses it as a path setting, not an execution grant.
- **Two fail-open surfaces share one root, and only one should stay open.** `Payload` carries no
  `deny_unknown_fields` (`src/migration.rs:199-210`) — which is what makes additive payload growth
  cheap, and there fail-open is a **feature**. Config adoption is *also* fail-open on new keys, and
  there it is a **vulnerability** (R-11d). The design must state which surfaces are deliberately
  fail-open and why; today both are fail-open by accident of the same omission.
- **Schema evolution, restated.** R-9a's presence-derived scope is what keeps this whole amendment
  inside `FORMAT_VERSION = 1`. Adding a scope *field* would reach ADR-0006 **and** invalidate AD-2's
  own cost argument (§ 9 F-3) — so the presence-derived form is a constraint on the design, not a
  preference. R-16 is a separate, pre-existing compatibility question that ADR-0006's freeze did not
  anticipate.
- **Irreversibility.** R-6a's decision now spans `remove`, which destroys keychain material with no
  undo. It is the only command in that set whose wrong-resolution is unrecoverable, and it should
  drive the decision rather than inherit it.

## 9. Source Traceability

Every claim below was verified against the working tree, and every **line citation** was re-resolved
by symbol against `HEAD` on 2026-08-04 before this document was committed.

> **Attestation corrected 2026-08-04 — the earlier wording was false, and the failure is instructive.**
> This paragraph previously read *"Every claim below was re-verified against the working tree … not
> carried from session memory."* The **claims** were verified and all of them held. The **line
> numbers** were not: commit `d1c5f30` (`(feat) cli: advance to the next account in the swap chain`,
> +228/−35 to `src/cli.rs`) landed hours before this document and shifted nearly every citation. Of
> the 21 distinct `src/cli.rs` citations in this file exactly one still resolved. An implementer
> following `src/cli.rs:4549-4611` for `import` would have landed in `write_export`.
>
> **The shift is a cumulative step function, not a two-band rule** — and the earlier attempt to state
> it as one is what let a second round of stale citations survive. `d1c5f30`'s eight hunks on
> `src/cli.rs`, with the running shift applied to every *old* line **below** each (a line takes the
> shift of the nearest hunk **above** it):
>
> | hunk @ old line | 108 | 503 | 514 | 818 | 1081 | 2873 | 8226 | 11507 |
> |---|---|---|---|---|---|---|---|---|
> | cumulative shift below it | +7 | +12 | +30 | +34 | +38 | **+52** | **+97** | +193 |
>
> The superseded wording said "+52 in the 4400–5300 band and **+97 above 10600**". The +52 band was
> right by accident; the +97 boundary was wrong at **both** ends — it begins at 8226, not 10600, and
> ends at 11507, above which the shift is +193. That mis-stated floor is the direct cause of the
> second residue round, though not its whole cause: **five** of the six stale entries (10180, 10217,
> 10357, 10522, 10566) sit *below* the claimed 10600 threshold, so the stated rule marked them exempt
> and they were left alone when they were in fact in the +97 band. The sixth, **10636, sits above
> 10600** — the superseded rule should have caught it and did not; it lands inside
> `a_config_only_artifact_imports_accounts_as_roster_entries_without_a_stash` (declared 10619) rather
> than the test it named. And the seventh entry, 10201, was correct throughout. So the bad threshold
> explains most of the residue but not all of it, which is the more useful lesson: **a rule of thumb
> about a diff is not the diff, and a rule that explains most of the evidence is still wrong.** The
> durable fix is not a better rule — it is not citing lines at all where a symbol will do: the
> seven-test row in the table below now names its tests, which no rebase can invalidate.
>
> Three citations were not merely offset but pointed at unrelated code that reads plausibly:
> `resolve_target` was cited to `src/cli.rs:438-455` (that is `parse_config`; the function lives in
> `src/use_account.rs:441-457`), exit code 6 to `src/error.rs:892` (it is `:955`), and — worst — the
> zero-writes test backing AC-2a's entire correction to `src/use_account.rs:2202-2213`, which is an
> unrelated `SwapAck` test. The real test is at `:2490-2502`.
>
> **The lesson is about the attestation, not the offsets.** "Verified" was written once and then aged
> silently, because nothing re-checked it when the tree moved underneath. A verification claim is
> itself a claim with a timestamp, and a document that asserts its own freshness is asserting
> something it cannot know about the future. Every citation in this file and the design doc has now
> been re-resolved **by symbol lookup** on 2026-08-04 — each was checked by reading the symbol it
> names, and independent review passes re-resolved them and found them correct — across all twelve files, counting both `path:line` forms and the bare `:NNN` continuations that share a backtick span with an earlier path. **No total is quoted here deliberately**: every commit changes it, and an attestation carrying a stale self-count is the exact failure § 9 exists to record. The bare-continuation form is called out because it is the regex blind spot that produced two residue rounds — a sweep that matches only the first entry in a span is not a sweep.
>
> **But the recorded form is still `path:line`, and it will drift again.** *Stated plainly
> 2026-08-04 (third pass), because the earlier wording claimed otherwise.* This paragraph previously
> said the citations "have been re-resolved **by symbol** — the durable form, since a symbol survives
> a rebase and a line number does not," which conflates the *method* with the *form*. Symbol lookup
> was the method; line numbers are still what is written down. Exactly one row in this document —
> the seven-test row below — was actually converted to the durable form, and `d1c5f30` shows what the
> next `src/cli.rs` commit does to the rest. A reader who trusts the durability claim skips the
> re-resolution these citations will need. **Verify before relying on any line number here**; the
> claim that survives is the *symbol names*, not the offsets. Caught by the pre-submit external
> review gate, not by this document's authors — for the third consecutive round.

| Claim | Evidence |
|---|---|
| Import never writes canonical / `~/.claude.json` / requests a swap | `src/cli.rs:4601-4663` — the body reaches `config.save()` + `notify_daemon_roster_reload()` and nothing else |
| Active `EXPIRY` reads canonical; parked reads stash | `src/daemon/snapshot_build.rs:45-53` |
| `rotated` is true-by-construction on `dead` (parseable non-empty seeded token) | `src/refresh.rs:434-437` |
| Conflict match is uuid-only | `src/cli.rs:4771-4774` |
| Whole-config merge is acknowledged future work | `src/cli.rs:4737-4741` (verbatim in-code comment) |
| Labels are non-unique by design | `src/cli.rs:5148-5149` |
| `use` refuses on ambiguity; `enable`/`disable` take first | `src/use_account.rs:453`; `src/error.rs:955` (exit 6); `src/cli.rs:5150-5163` |
| `Payload` has no timestamp; `FORMAT_VERSION = 1` | `src/migration.rs:199-210`; `src/migration.rs:97` |
| Existing migration test coverage — the **7 this scope reasons about**, of **45** total (16 in `src/cli.rs`'s #148/#149/#150 sections, 29 in `src/migration.rs` incl. the frozen-fixture gates C-1 depends on, `src/migration.rs:1730`, `:1767`) | `export_encrypted_round_trips_gathered_state_and_hides_it`, `export_no_secrets_omits_every_credential_blob`, `export_plaintext_round_trips_and_carries_secrets_in_the_clear`, `import_round_trips_an_encrypted_export_and_restores_every_account_byte_faithfully`, `a_config_only_artifact_imports_accounts_as_roster_entries_without_a_stash`, `the_import_report_names_labels_only_never_a_token_or_email`, `the_migration_conflict_policy_default_drives_import_behaviour` |
| Conflict test's target is a clone of the source | `src/cli.rs:10741` |

**Added 2026-08-04 — every row's claim verified against the working tree during this amendment (not
carried from the council transcripts), and every line citation re-resolved by symbol before commit
(see the attestation correction above):**

| Claim | Evidence |
|---|---|
| Every `RawConfig` field is `#[serde(default)]`, **including** `account` — so partial configs parse and scope decomposition is free at the format layer | `src/config.rs:1377-1396` |
| `RawAccount` keeps its own `deny_unknown_fields` — narrow-parse preserves per-account strictness | `src/config.rs:1398-1400` |
| `Payload` is exactly two emptiable fields, so scope is expressible by presence | `src/migration.rs:199-210` |
| `claude_bin` resolution: absolutize against cwd, accept any `is_file()`, no allowlist, no symlink resolution | `src/paths.rs:773-807` |
| The resolved binary is spawned by the refresh tick | `src/refresh_tick.rs:258` → `:273` → `src/refresh.rs:694` (`SpawnClaude::new`) |
| `import` reads the local conflict policy **before** apply — an adopted policy affects later imports, not its own | `src/cli.rs:4628` (`resolve_import_overwrite(overwrite, local.as_ref())`) |
| Fresh-target import adopts the artifact's whole config unconditionally | `src/cli.rs:4744-4750` |
| `stash()` interpolates an unvalidated `account_uuid` into a keychain service name | `src/config.rs:370-372`; `STASH_PREFIX` at `:325` |
| `stash()` reaches **no** filesystem path — bounding R-15's severity | grep of every `stash()` call site against path/join/file/dir: zero matches |
| `use` short-circuits on service-name equality, never contents | `src/use_account.rs:325-326`; test `src/use_account.rs:2490-2502` asserts `canonical == b"A-token"`, `calls == 0` |
| `remove` deletes the keychain stash — the only irreversible label-resolving command | `src/cli.rs:5219-5227`, `src/cli.rs:5195-5211` |
| `--config` is reserved and value-bearing for #24, not yet wired | `src/paths.rs:443-444` (the quoted phrase; `config_dir_with_override` at `:448`, `allow(dead_code)` off the test path) |
| `IMPORT_USAGE` uses "accounts" as the operator-facing noun | `src/cli.rs:1290` |
| ADR-0030 governs `claude` resolution **order**, not value provenance — the R-11a refusal does not contradict it, and `CLAUDE_BIN=…` is a documented local escape hatch | `docs/adr/0030-one-resolution-policy-cli-included.md` |
| `import` leaves the source artifact on disk | `src/cli.rs:4602` |
| Daemon liveness is locally probeable, read-only, tri-state — **plus a fourth, `Err`, outcome** (`daemon_liveness()` returns `Result<DaemonLiveness>`) | `src/cli.rs:1885` (`daemon_liveness()`, socket-primary + lock-fallback); `src/cli.rs:2137` (`probe_socket_responsive`). NOT `src/capture.rs:335` — that notify is best-effort and returns `()` |

### F-3 — A second claim was FALSIFIED during this amendment

The design's **AD-2** (§ 4.2) concluded "no `format_version` bump" and justified it by pricing the
change as costing "the frozen baseline, a golden-fixture regeneration, and an ADR."

**The justification is wrong.** It priced a **payload** field at the **header** rate. ADR-0006
§ BREAKING(3) explicitly carves out the opposite: *"Ordinary, non-load-bearing additive payload growth
via `Option`/`#[serde(default)]` stays additive."* And `src/migration.rs` carries **zero**
`deny_unknown_fields`, so a **non-load-bearing** payload field is additive, not breaking.

> **Read BREAKING(3)'s rule, not only its parenthetical — corrected 2026-08-04 (fourth pass).** An
> earlier wording here said the absent `deny_unknown_fields` makes an added payload field *"not
> breaking at all"*, which inverts ADR-0006's reasoning: the ADR uses that **same fact** to reach the
> opposite classification for *load-bearing* fields — *"Because unknown payload fields are ignored (no
> `deny_unknown_fields`), an older reader silently **drops** any field it doesn't know … so adding one
> MUST bump `format_version`"* (`docs/adr/0006-migration-schema-evolution-policy.md:127-132`). Silent
> drop is precisely what makes a load-bearing addition breaking. The sentence F-3 quotes is the
> *exception*; the rule it excepts is the one that governs. **Whether a staleness field would be
> load-bearing is the whole question, and it is not asked here** — a mint timestamp the importer
> reasons about is load-bearing on its face, so it would need the bump. This does not disturb F-3's
> finding (AD-2's *stated cost reason* is still wrong) or its conclusion (AD-2 survives on the
> false-assurance argument, § 4.2) — it removes a general licence an implementer could read as
> permission to add any payload field without a version bump.

**AD-2's conclusion survives; its stated reason does not.** The correct argument against a staleness
field is the one round 1 identified independently — a mint timestamp buys a heuristic that **cannot
detect this failure** (the token was superseded, not expired; § F-2) and therefore manufactures false
assurance. Stage 2 must re-argue AD-2 on those grounds and delete the cost sentence. **Done** —
the design deletes it rather than softening it (§ 4.2; the AD-2 row records "Rationale corrected
2026-08-04"), so this instruction is closed, not outstanding.

**Second-order, and it constrains the design**: if scope were carried as a payload *field*, AD-2's cost
argument would have to be deleted regardless — you cannot price a payload field as prohibitive while
adding one. R-9a's presence-derived form keeps AD-2 coherent, which is why it is a requirement rather
than an implementation preference.

### D-1 — A dissent recorded rather than resolved

On `[migration].conflict_policy` (R-11c) the panel split, and **both sides are factually correct**:

- `rust-architect`: adopting it under an explicit `--settings` is what the operator asked for, and it
  cannot affect the import that adopted it — `resolve_import_overwrite` reads the local value first
  (`src/cli.rs:4628`).
- `technical-architect`: scoping makes it **strictly worse** than today. At present an artifact cannot
  overwrite a policy the target operator deliberately set; `--settings` would newly allow it, silently
  changing every *subsequent* import.

The disagreement is **normative, not factual** — whether silently altering future-import behaviour is
an acceptable consequence of an explicit flag. R-11c resolves it conservatively (non-portable) on the
grounds that the key's entire purpose is to encode *this* operator's choice. Recorded here because the
resolution is a judgement call, not a finding, and a later reader may reasonably revisit it.

> **OQ-7 is upstream of this dissent and can dissolve it.** *Added 2026-08-05 (eleventh pass).* Both
> positions above presuppose that `--settings` adopts over an **existing** local config — which is
> exactly what OQ-7 asks and does not answer. Under OQ-7(a) (`--settings` never adopts over an
> existing config, the reading § 4.7's "the flag can only ever *remove*" supports) `technical-architect`'s
> "strictly worse than today" cannot arise, `rust-architect`'s "what the operator asked for" has no
> occasion, and R-11c protects a path that cannot be reached. Under (b) the split stands as recorded.
> **Settle OQ-7 before re-litigating D-1**; a reader who revisits this dissent without noticing the
> gate will argue a normative question whose factual premise is still open.

### F-1 — A carried claim was FALSIFIED during authoring

The `/investigate` run advanced, and the scope seed carried, this claim:

> "`--overwrite` conflict-matches on `account_uuid` only, so it is **inert** against same-label
> accounts — which is why the operator's `--overwrite` did not take."

**This is wrong**, and it was the working explanation of the incident. `account_uuid` is the *Claude*
account uuid (A-1), stable across machines. In the incident the uuids **matched**, `--overwrite`
**did** fire, and the stashes **were** replaced — which is exactly how machine A's token reached
machine B to be replayed and refused. The incident is fully explained by branch (i)+(ii) of § 1 alone.

What survives is narrower and is R-6: import can silently *create* a duplicate-label roster when one
label maps to two different Anthropic accounts. Recorded here rather than dropped, because a reader
who encounters the original framing elsewhere needs to know it was tested and failed.

### F-2 — A feasibility constraint discovered during authoring

R-4 ("warn on staleness") cannot be implemented as a *computed* check without new data: the artifact
carries no mint time. This was not visible from the incident and materially reshapes R-4 into
R-4 (unconditional warning, shippable now) + R-4a (the decision about whether more is affordable).

## 10. Related Work — Generalize, Do Not Duplicate

- **#262** (CLOSED, `question`) — the rotation spike. R-1 files against it; does not reopen it.
- **#465 / #468 / #463** (CLOSED) — rotation-interference characterization and the keep-warm gate.
  R-5a reconciles against 0465's counts rather than re-deriving them.
- **#965 / #980** (OPEN) — migration across *platforms*. Adjacent axis; explicitly out of scope.
- **#145 / #146 / #148** (CLOSED) — the portability spike, the artifact format, and `export`.
  This scope completes the **import** half those left at byte-restoration.
- **#64** — the swap lock R-2a must reuse.

## 11. Definition-of-Ready Verdict

**Verdict: `passed-with-findings`.**

Ready: the problem is grounded in re-verified code and dated log evidence; requirements are EARS-shaped
and testable; acceptance criteria carry BUT-NOT clauses; the object model names the one missing edge
that explains every symptom; a falsified claim is recorded rather than buried.

**Findings that do not block Stage 2, but must not be lost:**

1. **A-3 and A-4 are n=1.** They are the entire evidentiary basis for R-1. R-1's own wording already
   constrains how they may be stated; a reader who strengthens them later has broken the requirement.
2. **R-4a is unresolved and gates R-4's ambition.** Stage 2 must either find a v1-derivable signal or
   surface the `format_version` question as an ADR-0006 decision.
3. **R-6a's resolution is a decision, not a design.** Which of the four label-resolving commands'
   behaviours is correct — `use`'s refusal or `enable`/`disable`/`remove`'s first-match-wins — is a
   product call the pipeline must not settle silently, and `remove`'s irreversibility is the argument
   that should drive it (OQ-1).
4. **Ratification asymmetry.** R-5 … R-8 were ratified as *in-scope*; their mechanisms were not. R-2a,
   R-4a, R-5a, R-6a are all `pending-user` and each is reversible.

### Amendment findings (2026-08-04, R-9 … R-16)

**Verdict unchanged: `passed-with-findings`.** The amendment adds eight requirement families, all
EARS-shaped, all with BUT-NOT acceptance criteria, all traced to re-verified code. Five further
findings, none blocking Stage 2:

5. **R-9 and R-11 must ship together.** Each without the other produces a state strictly worse than
   shipping neither: R-9 alone hands the operator a flag that adopts a code-execution path *on
   request*; R-11 alone leaves them still unable to decline settings. The appetite (§ 1b) sizes them
   as one unit deliberately, and a delivery plan that splits them across releases has broken the
   requirement, not merely resequenced it.
6. **R-10a is undecided and it gates R-10.** Whether `--no-secrets` is hard-removed or
   deprecated-then-removed is a product call on a **shipped** flag. Included, not settled.
7. **R-11c rests on a recorded dissent (D-1), not a convergence.** Two panelists reached opposite
   conclusions from the same verified facts. The resolution is conservative and defensible; it is not
   evidence-forced, and § 9 D-1 says so explicitly so a later reader can revisit it without
   re-deriving the split.
8. **R-16 is a pre-existing defect this scope discovered but does not resolve.** The `[credential]`
   backward-import break predates every requirement here — it was introduced 26 days after ADR-0006
   froze v1 and was never tracked. R-9b repairs only the roster-only path. Filing it here is correct;
   *closing* it here would be scope creep, and leaving it unfiled would lose it entirely.
9. **AD-2 was re-argued — CLOSED.** § 9 F-3 falsified its cost reasoning while leaving its conclusion
   standing, and a design doc that keeps a falsified sentence is worse than one that never made the
   argument: a reader who checks it against ADR-0006 finds the carve-out and reasonably concludes the
   whole decision is unsound. The design in this same commit range **deletes** the sentence rather
   than softening it (§ 4.2), and its AD-2 row records "Rationale corrected 2026-08-04". *Recorded as
   closed 2026-08-05 (ninth pass): it was still listed among the open findings, sending a reader of
   this list to chase work already done.*

**Provenance summary for the amendment set** — R-9, R-9d, R-10 are `user-stated` (maintainer's own
words). R-9a/b/c, R-11 … R-16 are `council-added`, ratified **in-scope** via a second
scope-membership **B** selection over an enumerated 22-item set on 2026-08-04. R-10a, R-10b, R-11d,
R-11e, R-11f are `enrichment-expanded` and carry the same in-scope-only ratification. **No mechanism
in the amendment set is user-ratified** — *with three carve-outs: **R-9's scope-splitting**, **R-9d's
flag names**, and **R-10's `--no-secrets` removal***, which a pipeline may not reverse. Every other
mechanism remains a reversible pipeline call.

> **The carve-out list is three, not one.** *Corrected 2026-08-05 (seventh pass); this paragraph
> previously named only R-9d.* § 3's own preamble records all three as `user-stated` — "the maintainer
> **proposed scope-splitting**, ruled on naming, and **directed the `--no-secrets` removal** in their
> own words" — and R-3's ratification line repeats it. Scope-splitting (R-9) and the `--no-secrets`
> removal (R-10) are mechanisms, maintainer-originated by that same account, so R-9d's own logic
> applies to them identically. Left as written, a reader asking "may the pipeline drop `--settings`?"
> got "yes, reversible" here and "no, the maintainer proposed it" from § 3. This is the
> three-ways-recorded defect the fourth pass fixed for R-9d, unswept to its two siblings.
>
> **What the carve-out does and does not cover**: it protects the mechanism the maintainer originated
> — that scope selection exists, what the flags are called, that `--no-secrets` goes. It does **not**
> freeze the details the pipeline supplied around them: R-9a's presence-derivation, R-9c's no-export-
> flag rule, R-10a's removal *path* (explicitly undecided, OQ-4). Those remain reversible.
