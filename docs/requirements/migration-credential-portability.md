---
title: Migration Credential Portability
scope: migration-export-import
created: 2026-08-04
status: draft
dor_status: passed-with-findings
source: .tmp/scopes/migration-credential-portability.md (/investigate + /scope 2026-07-31 → 2026-08-04)
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
  # Transient pipeline scratch, deliberately NOT committed — will not resolve in a fresh clone.
  # Provenance only; nothing downstream may dereference them. This PRD and the design doc are
  # self-contained.
  requirements-brief: docs/briefs/2026-08-04-requirements-migration-credential-portability.md   # uncommitted
  scope-working-doc: .tmp/scopes/migration-credential-portability.md   # uncommitted, gitignored
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

**Why now.** The export half shipped and is well-tested (seven migration tests, § 9). The import half
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
- **Local policy is silently overwritable.** `[migration].conflict_policy` records a decision the
  target operator made. Adoption overwrites it — not for the import that adopts it
  (`resolve_import_overwrite` reads the local value first, `src/cli.rs:4628`) but for every one after.

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

**1 week (small batch)** for R-1, R-5, R-6, R-7, R-8 — each is local, evidenced, and independently
shippable.

**Not sized**: R-2, R-4. Each is decision-gated (R-2 on swap semantics, R-4 on whether a format bump
is acceptable). Sizing them before their decision would fabricate precision.

**R-3 is now sized** — it was decision-gated on a merge policy that no longer needs authoring. See
below.

**2 weeks (the security core)** for **R-9 + R-11** together. These two ship as one unit or not at all:
R-9 without R-11 hands the operator a flag that adopts a code-execution path on request, and R-11
without R-9 leaves them unable to decline settings wholesale. Splitting them across releases produces
a strictly worse intermediate state than shipping neither.

**1 week (the hardening tail)** for R-10, R-12 … R-16 — each local, independently shippable, and
none blocking the core.

**Additional circuit breakers**:

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
| **RefreshOutcome** | The classified result of a token exchange: `refreshed` / `dead` / `error`. Carries `rotated`, `window_secs`, `expires_before/after`. | `Classify`, `Log` |

| **ImportScope** | The set of payload classes the operator elected to apply on this import. Derived from CLI flags, **never** from the artifact. Two independent axes: accounts (roster + credentials) and settings (non-roster config). | `Select`, `Constrain` |
| **PortabilityClass** | The system's classification of a single `Config` key: **portable** (may be adopted), **machine-bound** (never adopted — it encodes a fact about *this* machine or *this* operator's choice), or **capability-granting** (never adopted — adoption transfers the ability to execute). Orthogonal to `ImportScope`: scope is what the operator *asked for*, class is what the system *permits*. | `Classify`, `Refuse` |

**The load-bearing relationship**: `import` must satisfy
`ManagedAccount → CredentialSlot{canonical, stash}`, but implements only
`ManagedAccount → CredentialSlot{stash}`. Every symptom in § 1 is a consequence of that one missing
edge, or of the fact that nothing measures the artifact's freshness before traversing it.

## 3. Requirements (EARS)

> **Reading the `Ratification:` item labels.** Each requirement carries an item label (`E*`, `I*`,
> `M*`) naming the enrichment entry the user ratified it under. **Two different enumerated sets are in
> play**, and their labels overlap — `I1`, `I2` and `M1` each exist in both. They are therefore
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
sequence. The warning **shall** fire on **every** credential-bearing import — not conditionally — until
a freshness signal exists to gate it on. `Origin: user-stated` ("warns on staleness").
`Ratification: n/a`.

**R-4a** — *Before* R-4 is implemented as anything richer than an unconditional warning, the system
**shall** determine whether a freshness signal is derivable from **v1 data already carried**.
`Payload` has no timestamp and `FORMAT_VERSION` is frozen at 1 (ADR-0006), so a computed staleness
check requires either a derived proxy or a schema-evolution decision. This is a decision, not an
implementation detail. `Origin: AI-inferred-expansion (feasibility, § 9 F-2)`.
`Ratification: pending-user` (mechanism only; R-4's inclusion is user-stated).

### CredentialSlot

**R-2** — *When* `import` restores an account that is the target machine's **active** account, the
system **shall** promote the imported credential to the canonical `Claude Code-credentials` item, or
**shall** refuse and tell the operator which command completes the adoption. Silently parking bytes
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
silently. `Origin: AI-inferred-expansion`.
`Ratification: user-ratified 2026-08-04 (scope-membership B/first-pass, item I2)`.

**R-6a** — *Where* a duplicate-label roster exists, the system **shall** handle it **consistently**
across **all four** label-resolving commands — `use`, `enable`, `disable` and `remove`. It does not today: `use <label>` refuses with
`Error::UseTargetAmbiguous` (`src/use_account.rs:453`, exit code 6 per `src/error.rs:955`), while
`apply_enabled` backing `enable`/`disable` silently resolves to the earliest entry
(`src/cli.rs:5150-5163`). Which behaviour is correct is a decision, not a design.
`Origin: AI-inferred-expansion`.
`Ratification: pending-user` (the inconsistency's *inclusion* is ratified; its resolution is not).

> **R-6a's command set was corrected on 2026-08-04.** The original wording named only two commands and
> framed the choice as "one of the two is wrong". Both halves were defective:
>
> - **`remove` was omitted, and it is the load-bearing case.** `remove_account` → `apply_remove`
>   (`src/cli.rs:5219-5227`, `src/cli.rs:5195-5211`) resolves a label and **deletes the keychain stash**. It is
>   the only one of the three whose first-match-wins behaviour is **irreversible** — `use` picks the
>   wrong active account (recoverable in one command) and `enable`/`disable` flips the wrong flag
>   (recoverable), but `remove` destroys credential material with no undo. A decision framed over
>   `use` vs `enable`/`disable` alone would settle the two cheap cases and leave the expensive one
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
the **default being everything**, which is today's behaviour byte-for-byte. Today no such gesture
exists: on a fresh target the artifact's whole config is adopted unconditionally
(`src/cli.rs:4744-4750`) and the operator cannot decline it. `Origin: user-stated`.
`Ratification: n/a` (maintainer-proposed).

**R-9a** — *Where* an artifact's scope is determined, it **shall** be derived from payload **presence**
(`config_toml` empty, `accounts` empty) and **shall not** be read from any scope field the artifact
declares about itself. On a `--plaintext` export nothing is authenticated (`src/cli.rs:4471-4479`), so
a declared scope is attacker-controlled: a hostile artifact would assert full scope and the control
would evaporate. The operator's flag is a **ceiling, never a floor** — `import --accounts` against an
artifact containing config ignores that config regardless of what the artifact claims.
`Origin: council-added` (3/3 convergent). `Ratification: user-ratified 2026-08-04 (scope-membership B/amendment, item E5)`.

> **The two presence tests are not symmetric.** `accounts` empty is export-reachable (the config-only
> artifact); `config_toml` empty is **not** — `export` writes `config.render()` unconditionally
> (`src/cli.rs:4532`) and `render()` always emits `[tunables]` (`src/config/render.rs:370`), and AD-5
> deliberately gives `export` no narrowing flag. A roster-only artifact is therefore hand-constructed
> or third-party, never self-minted; `docs/specs/import-scope-selection.feature.md` § `--settings` on
> a roster-only artifact records what the Cap-7.6 test must build. This does **not** trip R-9's
> circuit breaker: scope is still derived from presence and no declared field is required.

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
#24's directory-override ladder (`src/paths.rs:439`, "The CLI flag itself is not wired yet"), and it is
**semantically wrong** — `account` is a field of `RawConfig`, so accounts *are* config and
`sessiometer config show` prints them. `--accounts` matches the vocabulary the tool already uses on
this surface: `IMPORT_USAGE` opens "rehydrate **accounts** from a migration artifact"
(`src/cli.rs:1290`). `roster` is internal Rust vocabulary and barely surfaces to operators.
`Origin: user-stated` (maintainer asked the question; evidence settled it). `Ratification: n/a`.

**R-10** — The system **shall** remove the shipped `export --no-secrets` flag
(`src/cli.rs`, `EXPORT_USAGE`). Roster-without-secrets is not a state this product supports. The
inverse is already unreachable by construction — `apply_import`'s merge loop is over the roster
(`src/cli.rs:4770`) with secrets keyed by uuid (`src/cli.rs:4789`), so a secret with no roster entry is dead
code — and the maintainer has ruled the forward direction out of the model. `Origin: user-stated`.
`Ratification: n/a`.

**R-10a** — *Where* R-10 removes a **shipped** flag, the removal **shall** follow a decided path:
hard-remove with a strict-usage error naming the replacement, or deprecate-then-remove across a
release. Not yet decided. `Origin: enrichment-expanded` (item I1).
`Ratification: user-ratified 2026-08-04 (scope-membership B/amendment, item I1)` — inclusion only; the path is undecided.

**R-10b** — *Where* R-10 lands, **every** artifact carries live credentials, so `PLAINTEXT_WARNING`
(`src/migration.rs:538-541`) is no longer sometimes-moot — it becomes unconditionally true. Its
wording **shall** be re-checked against that, and against R-12's shred mechanism, so the advice it
gives is one the tool can actually perform. `Origin: enrichment-expanded` (item I2).
`Ratification: user-ratified 2026-08-04 (scope-membership B/amendment, item I2)`.

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
local value (a monotonic floor). A fleet may standardize *upward*; nothing may downgrade. This kills
the 8 KiB / 1-iteration downgrade path (`src/config.rs:981-988`) without banning the legitimate case.
Scope note: this governs **adoption on import** only — the KDF's construction and parameters remain
#147's (see § 1b). `Origin: council-added`. `Ratification: user-ratified 2026-08-04 (scope-membership B/amendment, item E7)`.

**R-11c** — `[migration].conflict_policy` **shall not** be adopted. It encodes a decision the *target*
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
Under R-9's model the *typical* artifact becomes roster-only, which is a **pure credential file** —
making this more urgent, not less. `Origin: council-added` (`security-architect`, rounds 1 and 2).
`Ratification: user-ratified 2026-08-04 (scope-membership B/amendment, item E11)`.

**R-13** — *When* `export` runs, the system **shall** determine whether this machine's daemon is
running and surface it. The design's own position is that the staleness hazard is "not detectable at
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
use. It is unvalidated today and is interpolated directly into a keychain service name
(`src/config.rs:370-372`, `format!("{STASH_PREFIX}{}", self.account_uuid)`).
**Bounded, and the bound is verified**: `stash()` never reaches a filesystem path (no call site joins
it into one), and keychain service names are opaque strings rather than hierarchical paths — so
`Sessiometer/../x` is a literal name, not a traversal. The residue is namespace squatting inside the
prefix, an empty uuid yielding the bare prefix, and unbounded length. This is **input-validation
hardening, not a critical finding**, and it is recorded at that severity deliberately.
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
`seeded_rt != after_rt` (`src/refresh.rs:434-437`); a `dead` outcome sets `after_rt = Some("")`, so
`rotated=true` is **true by construction** on every dead line and carries no information.
`Origin: AI-inferred-expansion`.
`Ratification: user-ratified 2026-08-04 (scope-membership B/first-pass, item I1)`.

**R-5a** — *Where* R-5 changes the emitted field, the change **shall** be treated as a **log-format
change** with existing consumers, not a cosmetic edit. `docs/findings/0465-*` derives a published
headline count (`141 rotated=true, 0 rotated=false`) from this field.
**Verified: 0465 is NOT contaminated** — its window ends ~2026-07-11 and the first `dead` line in the
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
> This was the **seventh** site of the same correction and the last one found — after Cap-1.1, a
> building-block row, two runtime-view arrows and two spec scenarios. It is also the most dangerous,
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
*And* running `import` with no scope flag applies everything exactly as it does today.
**BUT NOT** reading any scope declaration from inside the artifact; **BUT NOT** letting an artifact
widen the operator's selection; **BUT NOT** naming the flag `--config`, which is reserved for #24's
directory-override ladder.

**AC-9b (R-9b)** — *Given* a roster-only artifact, *When* it is imported **with `--accounts`** by a
binary whose `RawConfig` would reject an unknown block, *Then* the import succeeds. **BUT NOT** by
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
*Then* they get a strict-usage error naming what replaced it, *And* `PLAINTEXT_WARNING`'s wording has
been re-checked against the fact that every artifact now carries credentials.
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

**AC-11 (R-11, R-11a, R-11b, R-11c, R-11e)** — *Given* an artifact whose config sets
`[refresh].claude_bin = "./x"`, *When* it is imported **with `--settings`**, *Then* the target's saved
config does **not** contain that value, *And* the refusal is visible in the command's output (R-11e),
*And* an incoming `kdf_*` weaker than local is refused while a stronger one is accepted,
*And* `conflict_policy` is not adopted.
**BUT NOT** gated behind a second confirmation flag — an escalation flag becomes the exploit
instruction the error message hands the operator; **BUT NOT** implemented as a denylist;
**BUT NOT** relying on strip-on-export as the control, since the attacker controls the export.

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

**AC-13 (R-13)** — *Given* the source daemon is running, *When* `export` runs, *Then* the operator is
told, because the artifact will be invalidated by the next refresh. **BUT NOT** a warning printed
unconditionally regardless of daemon state, which trains dismissal; **BUT NOT** blocking the export —
the operator may have a reason.

**AC-14 (R-14, R-14a)** — *Given* an export and its later import, *When* their events are read, *Then*
a common artifact digest correlates them, *And* the **import** event carries the operator-requested
scope. **BUT NOT** logging a label, token, or email — the aggregate-only redaction discipline of
`Event::Export` / `Event::Import` holds; **BUT NOT** logging the scope the artifact claims;
**BUT NOT** requiring a requested-scope field on the **export** event, which has no operator-
requestable scope to carry (R-9c, AD-5).

**AC-15 (R-15)** — *Given* a roster entry whose `account_uuid` is empty or malformed, *When* it is
imported, *Then* it is rejected before a keychain service name is derived from it.
**BUT NOT** stated or filed as a path-traversal finding — `stash()` reaches no filesystem path, and
overstating it would manufacture a severity the evidence does not support.

**AC-16 (R-16)** — *Given* an artifact carrying a `[credential]` block, *When* a binary built before
commit `6fe3457` imports it, *Then* the outcome is **decided and documented** — either it succeeds, or
it fails with a message naming the version floor. **BUT NOT** left as today's bare
`deny_unknown_fields` parse error; **BUT NOT** considered closed by R-9b, which repairs only the
roster-only case.

**AC-4 (R-4, R-4a)** — *Given* any credential-bearing import, *When* it runs, *Then* the operator is
warned that a source refresh after export invalidates the artifact, and is given the safe sequence.
**BUT NOT** gated on a freshness computation that does not yet exist; **BUT NOT** implemented via a
`format_version` bump absorbed as an implementation detail rather than decided against ADR-0006.

**AC-5 (R-5, R-5a)** — *Given* a refresh that classifies as `dead` or `error`, *When* the event is
logged, *Then* the line does not present `rotated` as an observation.
**BUT NOT** by deleting the field on the `refreshed` path, where it is meaningful; **BUT NOT** without
checking whether any committed findings note's counts are drawn from a window containing non-`refreshed`
lines (0465 checked: clean).

**AC-6 (R-6, R-6a)** — *Given* a target roster carrying label `L` under uuid `X`, and an artifact
carrying label `L` under uuid `Y`, *When* `import` runs, *Then* the operator is warned that a
duplicate label was created. *And* `use L`, `enable L`, `disable L` **and `remove L`** thereafter agree
on whether a duplicate label is resolvable. **BUT NOT** by enforcing label uniqueness — that
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
METER:   unit test over classify() across {refreshed, dead, error}
MUST:    1.0
PAST:    < 1.0 — true-by-construction on every `dead` line (src/refresh.rs:434-437); 6 such
         lines in the local log from 2026-07-14 onward
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

- **P1 — A `rotated` fix silently re-baselines a published finding.** Mitigated: 0465 verified clean
  (R-5a). Any *future* count over a window containing `dead` lines repeats 0465's methodology onto
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
  a digest + scope to the export/import events; R-11e adds a refusal signal. All three must hold the
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
> `src/cli.rs`, with the running shift applied to every *old* line above each:
>
> | hunk @ old line | 108 | 503 | 514 | 818 | 1081 | 2873 | 8226 | 11507 |
> |---|---|---|---|---|---|---|---|---|
> | shift above it | +7 | +12 | +30 | +34 | +38 | **+52** | **+97** | +193 |
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
> names, and an independent third review pass re-resolved all 137 and found them correct.
>
> **But the recorded form is still `path:line`, and it will drift again.** *Stated plainly
> 2026-08-04 (third pass), because the earlier wording claimed otherwise.* This paragraph previously
> said the citations "have been re-resolved **by symbol** — the durable form, since a symbol survives
> a rebase and a line number does not," which conflates the *method* with the *form*. Symbol lookup
> was the method; line numbers are still what is written down. Exactly one row in this document —
> the seven-test row below — was actually converted to the durable form, and `d1c5f30` shows what the
> next `src/cli.rs` commit does to the other 137. A reader who trusts the durability claim skips the
> re-resolution these citations will need. **Verify before relying on any line number here**; the
> claim that survives is the *symbol names*, not the offsets. Caught by the pre-submit external
> review gate, not by this document's authors — for the third consecutive round.

| Claim | Evidence |
|---|---|
| Import never writes canonical / `~/.claude.json` / requests a swap | `src/cli.rs:4601-4663` — the body reaches `config.save()` + `notify_daemon_roster_reload()` and nothing else |
| Active `EXPIRY` reads canonical; parked reads stash | `src/daemon/snapshot_build.rs:45-53` |
| `rotated` is true-by-construction on `dead` | `src/refresh.rs:434-437` |
| Conflict match is uuid-only | `src/cli.rs:4771-4774` |
| Whole-config merge is acknowledged future work | `src/cli.rs:4737-4741` (verbatim in-code comment) |
| Labels are non-unique by design | `src/cli.rs:5148-5149` |
| `use` refuses on ambiguity; `enable`/`disable` take first | `src/use_account.rs:453`; `src/error.rs:955` (exit 6); `src/cli.rs:5150-5163` |
| `Payload` has no timestamp; `FORMAT_VERSION = 1` | `src/migration.rs:199-210`; `src/migration.rs:97` |
| Existing migration test coverage (7 tests, all in `src/cli.rs`) | `export_encrypted_round_trips_gathered_state_and_hides_it`, `export_no_secrets_omits_every_credential_blob`, `export_plaintext_round_trips_and_carries_secrets_in_the_clear`, `import_round_trips_an_encrypted_export_and_restores_every_account_byte_faithfully`, `a_config_only_artifact_imports_accounts_as_roster_entries_without_a_stash`, `the_import_report_names_labels_only_never_a_token_or_email`, `the_migration_conflict_policy_default_drives_import_behaviour` |
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
| `--config` is reserved and value-bearing for #24, not yet wired | `src/paths.rs:439` (`config_dir_with_override`, `allow(dead_code)` off the test path) |
| `IMPORT_USAGE` uses "accounts" as the operator-facing noun | `src/cli.rs:1290` |
| ADR-0030 governs `claude` resolution **order**, not value provenance — the R-11a refusal does not contradict it, and `CLAUDE_BIN=…` is a documented local escape hatch | `docs/adr/0030-one-resolution-policy-cli-included.md` |
| `import` leaves the source artifact on disk | `src/cli.rs:4602` |
| Daemon liveness is locally probeable, read-only, tri-state | `src/cli.rs:1885` (`daemon_liveness()`, socket-primary + lock-fallback); `src/cli.rs:2137` (`probe_socket_responsive`). NOT `src/capture.rs:335` — that notify is best-effort and returns `()` |

### F-3 — A second claim was FALSIFIED during this amendment

The design's **AD-2** (§ 4.2) concluded "no `format_version` bump" and justified it by pricing the
change as costing "the frozen baseline, a golden-fixture regeneration, and an ADR."

**The justification is wrong.** It priced a **payload** field at the **header** rate. ADR-0006
§ BREAKING(3) explicitly carves out the opposite: *"Ordinary, non-load-bearing additive payload growth
via `Option`/`#[serde(default)]` stays additive."* And `src/migration.rs` carries **zero**
`deny_unknown_fields`, so an added payload field is not breaking at all.

**AD-2's conclusion survives; its stated reason does not.** The correct argument against a staleness
field is the one round 1 identified independently — a mint timestamp buys a heuristic that **cannot
detect this failure** (the token was superseded, not expired; § F-2) and therefore manufactures false
assurance. Stage 2 must re-argue AD-2 on those grounds and delete the cost sentence.

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
9. **AD-2 must be re-argued before Stage 2 closes.** § 9 F-3 falsified its cost reasoning while
   leaving its conclusion standing. A design doc that keeps the falsified sentence is worse than one
   that never made the argument — a reader who checks it against ADR-0006 will find the carve-out and
   reasonably conclude the whole decision is unsound.

**Provenance summary for the amendment set** — R-9, R-9d, R-10 are `user-stated` (maintainer's own
words). R-9a/b/c, R-11 … R-16 are `council-added`, ratified **in-scope** via a second
scope-membership **B** selection over an enumerated 22-item set on 2026-08-04. R-10a, R-10b, R-11d,
R-11e, R-11f are `enrichment-expanded` and carry the same in-scope-only ratification. **No mechanism
in the amendment set is user-ratified**; each remains a reversible pipeline call.
