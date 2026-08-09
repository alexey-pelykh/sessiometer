# Solution Design: Migration Credential Portability

**Requirements**: `docs/requirements/migration-credential-portability.md`
**Status**: `draft` — **six** requirements are decision-gated (R-6a, R-9, R-9a, R-10a, R-11c, R-16 — see § 16),
by **five** open questions (OQ-1, OQ-4, OQ-5, OQ-6, OQ-7). Both are surfaced, not settled, here.
**Date**: 2026-08-04

## 1. Goals and Drivers

| # | Driver | Source |
|---|---|---|
| D-1 | A migration performed correctly leaves zero accounts needing `claude /login` | PRD § 6.1 |
| D-2 | A migration performed *incorrectly* warns **before** the damage, not after | PRD § 6.2 |
| D-3 | `status` never reports a value that contradicts what was imported | PRD § 6.3 |
| D-4 | The #262 spike's residual questions carry recorded, honestly-bounded evidence | PRD § 6.4 |

**The one-sentence design position:**

> The staleness hazard is **not detectable at the target**. It is only **preventable at the source**.
> Every design choice below follows from that, and the choices that pretend otherwise are rejected.

## 2. Constraints

| # | Constraint | Origin |
|---|---|---|
| C-1 | `FORMAT_VERSION = 1` is frozen as the tested baseline; a bump is an ADR decision with a golden-fixture gate | ADR-0006; `src/migration.rs:97` |
| C-2 | The canonical `Claude Code-credentials` item has a single-writer discipline (swap engine #64 + `src/daemon/canonical.rs`) | `src/swap.rs` module doc |
| C-3 | No requirement may cause a credential to reach a log, report, or error string | `the_import_report_names_labels_only_never_a_token_or_email` |
| C-4 | A v1 artifact minted before any change must still import | ADR-0006 additive-evolution policy |
| C-5 | Operator labels are redacted in findings notes | #463; `docs/findings/README.md` |

## 3. Context and Scope

**In scope** — *re-derived 2026-08-04 against the amendment (R-9 … R-16); the pre-amendment wording
covered only the first row and is superseded*:

| Area | Surface | Requirements |
|---|---|---|
| Canonical promotion, staleness, provenance | `sessiometer import` (`src/cli.rs:4601-4663`), `apply_import` (`src/cli.rs:4726-4813`), `classify()`'s `rotated` (`src/refresh.rs:432-472`), `status` EXPIRY provenance (`src/daemon/snapshot_build.rs:40-58`) | R-1 … R-7 |
| Duplicate-label resolution | `use` / `enable` / `disable` / **`remove`** — `remove` is included deliberately: it is the only one of the four whose first-match-wins is **irreversible**, which is why OQ-1 says its irreversibility should drive the answer | R-6a, OQ-1 |
| **Import scope selection** | `--accounts` / `--settings`; presence-derived, never artifact-declared | R-9 … R-9d |
| **Export flag surface** | removal of `--no-secrets`; `PLAINTEXT_WARNING` wording (`src/migration.rs:538`) | R-10, R-10a, R-10b |
| **Config portability** | the allowlist, `claude_bin` refusal, `kdf_*` monotonic floor, `conflict_policy`, rot-guard | R-11 … R-11f |
| **Artifact lifetime** | `import --shred` | R-12 |
| **Source-side prevention** | export-time daemon-liveness probe | R-13 |
| **Observability** | artifact digest on both events + requested scope on the `import` event; refusal signal | R-14, R-14a |
| **Input validation** | `account_uuid` shape | R-15 |
| **Backward-import break** | the `[credential]` block vs `deny_unknown_fields` | R-16 |
| Documents | `docs/findings/0262-*`, the migration runbook | R-1, R-8 |

**Out of scope**: cross-platform migration (#965/#980), the swap decision loop, `[refresh]` cadence
and the keep-warm gate (#468), and the artifact **envelope** (#147).

> **Boundary narrowed 2026-08-04 — KDF is no longer wholly out of scope.** This line previously read
> "the artifact envelope **and KDF** (#147)", which now contradicts R-11b, § 4.8 and Cap-8.2. PRD § 1b
> carries the same narrowing: *how* the KDF is constructed and parameterized remains #147's; whether
> an artifact's `[migration].kdf_*` may be **adopted on import** is R-11b's, and the answer is
> upward-only. Envelope construction stays out; adoption of the parameters comes in.

**The system boundary that matters**: two machines that **cannot see each other**. Machine B has no
channel to learn that machine A refreshed 4 minutes ago. Any design that implies B can detect this is
wrong by construction.

## 4. Solution Strategy

### 4.1 Canonical promotion (R-2, R-2a) — **do not add a second canonical writer; reuse `use`**

The obvious implementation — have `import` write the canonical item — is **rejected**. `src/swap.rs`
already owns a five-step sequence for exactly this transition (re-stash the outgoing account's
drifted blob, atomic `-U` canonical write, `~/.claude.json` co-write, confirming re-read), and it
holds the #64 lock while doing it. A canonical write inside `import` would be a second, uncoordinated
writer racing `src/daemon/canonical.rs`'s reconciler — C-2 forbids it.

**Chosen**: `import` stays a *stash-and-roster* operation, and closes the gap by **telling the truth
about what it did not do**. When the artifact contains the account that is currently active on the
target, import reports that the credential is staged but not adopted, and names the command that
adopts it — **`sessiometer use --force <label>`** — which drives the existing engine under the
existing lock.

> **Corrected 2026-08-04 — `--force` is load-bearing, not decoration.** This section previously named
> `sessiometer use <label>`. For the **active** account that is a **provable no-op**:
> `SwapTarget::resolve` short-circuits on `if account.stash() == active_stash { return
> Ok(GateOutcome::AlreadyActive); }` (`src/use_account.rs:325-326`) — a comparison of **service
> names**, never of contents. The committed test
> `already_active_without_force_is_a_noop_success_with_zero_writes` (`src/use_account.rs:2490-2502`) asserts exactly the
> outcome: `canonical == b"A-token"`, `calls == 0`.
>
> So the original guidance would have left the canonical item holding the **stale** token while both
> `import` and `use` reported success — **reproducing the original failure through its own
> remediation**, and doing it in the one place the operator would trust least to check. Surfaced by
> council round 1; pinned as PRD AC-2a.
>
> This does not disturb the section's actual decision (reuse the engine, add no writer) — the engine
> is reached either way. It changes only *which invocation* is named, and makes the difference
> testable rather than assumed.

**Optional, additive**: an `--activate <label>` flag on `import` that invokes the same forcing `use`
path after the import completes. Sugar over the sanctioned sequence, not a new writer — and it must
force for the same reason, or it inherits the same no-op.

> **For the #1001 implementer: adoption leaves the *stash* stale, and that is not a bug.** When
> `use --force <label>` targets the **already-active** account, `outgoing_stash == incoming_stash`,
> so step 2's re-stash writes the *pre-import* canonical blob back over the freshly-imported stash
> (`src/swap.rs:838`). **Adoption still succeeds**: `incoming` is read at `:807`, *before* that write,
> so `:849` writes the imported credential to canonical — which is the outcome AC-2a asserts. The
> #211 identity guard (`:829-833`) does not fire either, because with identical stashes its condition
> reduces to `!X && X`. Consequence for a test author: **assert post-adoption *canonical* contents,
> not stash contents** — a Cap-1.1/Cap-1.2 test that checks the stash will read the stale token and
> look like a failure when the behaviour is correct.

**Alternatives considered:**

| Option | Verdict |
|---|---|
| (a) `import` writes canonical directly | **Rejected** — violates C-2; races the reconciler; duplicates a proven 5-step sequence |
| (b) `import` reports + names `use --force` | **CHOSEN** — smallest change, no new **canonical** writer, no *additional* lock surface (import already takes the #64 lock around its stash writes — see § 11 Concurrency). The unqualified `use` variant of this option is **not viable** (see correction above) |
| (c) `import` always auto-activates | **Rejected** — the operator asked to import, not to switch accounts. Surprising and irreversible-ish |
| (d) `--activate <label>` opt-in | **Accepted as additive** to (b), forcing |

### 4.2 Staleness (R-4, R-4a) — **unconditional warning; NO format bump**

R-4a asked whether a freshness signal is derivable from v1 data. **It partly is, and the part that
is derivable does not solve the problem.** This is the design's most important finding.

**What v1 already carries.** `ManagedAccount.credential` is the raw blob, byte-identical to the
canonical item. `credential_clocks(blob)` (`src/refresh.rs:833-838`) extracts **both**
`claudeAiOauth.expiresAt` and `claudeAiOauth.refreshTokenExpiresAt` as a pure, non-secret function
over those bytes. So the importer can compute both deadlines per account with **zero format change**.

**Why that does not detect the incident.** Walk the actual numbers:

| Fact | Value |
|---|---|
| Exported blob's `expiresAt` | `2026-07-31T06:50:10Z` |
| Source (A) refreshed, superseding the token | `05:51:09Z` |
| Target (B) imported and replayed | `05:55:23Z` |

At import time the artifact's access token had **~55 minutes of validity remaining**. An
"is it expired?" gate passes cleanly. The token was not *expired* — it was **superseded**, and
supersession leaves no trace in the blob. The refusal came back from the endpoint (`outcome=dead`,
`window_secs=0`), which is the only place that fact existed.

**Would a `format_version` bump adding `exported_at` fix it?** **No.** An `exported_at` would let B say
"this artifact is 4 minutes old"; it could never say "the source has refreshed since." Age is not
supersession. A staleness field would buy a *heuristic* and create **false assurance** — worse than the
current silence, because an operator who sees a freshness check pass will trust it.

> **Correction (2026-08-04) — this decision previously rested on a cost argument that is false.**
> The original wording added that a bump would cost "the frozen baseline, a golden-fixture
> regeneration, and an ADR." **That priced a *payload* field at the *header* rate.** ADR-0006
> § BREAKING(3) carves out the opposite in terms: *"Ordinary, non-load-bearing additive payload growth
> via `Option`/`#[serde(default)]` stays additive."* And `src/migration.rs` carries **zero**
> `deny_unknown_fields`, so a **non-load-bearing** payload field is additive, not breaking.
>
> **Read BREAKING(3)'s rule, not only its parenthetical.** *Corrected 2026-08-04 (sixth pass); this
> sentence used to end "not a breaking change at all — it is nearly free", which inverts the ADR.*
> ADR-0006 uses that **same** absent-`deny_unknown_fields` fact to reach the *opposite* classification
> for load-bearing fields: *"Because unknown payload fields are ignored … an older reader silently
> **drops** any field it doesn't know … so adding one MUST bump `format_version`"*
> (`docs/adr/0006-migration-schema-evolution-policy.md:127-132`). Silent drop is exactly what makes a
> load-bearing addition breaking; the "stays additive" clause is the *exception*, not the rule. **A
> mint timestamp — the field actually under discussion here — is load-bearing on its face**, so it is
> precisely the case that MUST bump. This does not disturb AD-2's conclusion, which § 4.2 already
> rests on false assurance alone (PRD § 9 F-3 carries the same correction).
>
> **The conclusion survives; the reason did not.** AD-2 stands on false assurance alone, which is the
> only leg it ever actually had — and that leg is strong, because it is a claim about what the signal
> can *mean*, not about what it costs. The cost sentence is deleted rather than softened: an argument
> that is both wrong and unnecessary is worse than no argument, because a reader who checks it against
> ADR-0006 will find the carve-out and reasonably discard the whole decision with it.
>
> Recorded in PRD § 9 F-3. Surfaced by council round 1.

**Chosen**:
1. **Unconditional warning** on every credential-bearing import, naming the hazard and the safe
   sequence — **including the forcing form `use --force <label>`, never `use <label>` unqualified**
   (the unqualified form is a provable no-op against the already-active account,
   `src/use_account.rs:325-326`). This is the MUST and it ships without touching the format.
2. **Additive, supplementary**: if a derived deadline shows an account's access token *already*
   expired, say so too — a genuinely different and genuinely detectable failure (an artifact left on
   a USB stick for a day). Must be framed as **an extra symptom, never as an all-clear.**
3. **No `format_version` bump.** Recorded as a decision (§ 12, AD-2), so a future reader sees it was
   considered and declined on reasoning, not overlooked.

**Fail-closed rule (premortem P2)**: the warning must **not** become conditional on the derived
check. Unknown freshness ⇒ warn.

### 4.3 Duplicate labels (R-6, R-6a) — **warn at creation; make resolution consistent**

Two separable pieces:

- **R-6 (import-time warning)** — straightforward. `apply_import` already iterates the incoming
  roster and already has the local roster in hand; a label-collision check against a different
  `account_uuid` is local and cheap. No new state.
- **R-6a (consistency)** — a **product decision this design does not settle**. Today `use <label>`
  refuses (`Error::UseTargetAmbiguous`, exit 6) while `apply_enabled` silently takes the earliest
  entry (`src/cli.rs:5150-5163`). Both behaviours are defensible in isolation; having both is not.

**Options for R-6a**, surfaced for decision (§ 14 Open Questions, OQ-1):

| Option | Consequence |
|---|---|
| (i) `enable` / `disable` / `remove` refuse like `use` | Consistent and safe; breaks any operator muscle-memory relying on first-match |
| (ii) `use` takes first like the other three | Consistent; but silently switching to the *wrong account* is a credential-level mistake, not a config one — and under `remove` it silently **deletes** the wrong account's stash |
| (iii) All four accept an `--account-uuid` disambiguator; label path refuses | Most explicit; largest surface |

**Design lean: (i)** — it moves the *cheaper* commands toward the *safer* one. Refusing an
`enable` costs a re-run; silently switching credentials costs an incident, and silently removing them
costs an unrecoverable one. Not chosen here.

> **CHOSEN: (i), by the operator on 2026-08-06. Delivered under issue #1005.** `apply_enabled` and
> `apply_remove` now resolve through `use_account::resolve_target`, so all six sites share one
> resolver and one policy. Two implementation notes the option table did not price, both of which
> fall out of routing rather than being separable choices:
>
> - **`Error::AccountLabelNotFound` is retired, and the exit codes move with it.** It appeared
>   nowhere in `Error::exit_code`, so it fell through to the generic `_ => 1`: an unmatched
>   `enable`/`disable`/`remove` target now exits **5** (`UseTargetNotFound`) and **6**
>   (`UseTargetAmbiguous`) becomes reachable where nothing previously failed. The retirement is
>   forced, not chosen — with both constructors routed away, a never-constructed variant is
>   `dead_code` and fails `-D warnings`.
> - **Option (iii)'s disambiguator arrives anyway, without its flag.** `resolve_target` matches label
>   **or** account-uuid, so the three verbs now accept a uuid. That is not a tolerated side effect —
>   it is the mechanism by which a refusal is actionable at all. Had (i) been implemented against a
>   label-only resolver, an operator facing a duplicate would have had a refusal and no way to act on
>   it, which is worse than the first-match-wins it replaces. (iii)'s explicit flag remains
>   unimplemented and unneeded.
>
> The import-side half (R-6) landed alongside: a duplicate-label flag on the per-account report row,
> computed by counting each label's bearers **before and after the whole merge** and warning where a
> label ends with more than one bearer AND more than it started with. Both halves of that are
> load-bearing, and they cover different cases. Reading the FINISHED roster (rather than each write)
> catches a collision arriving inside a single artifact on a fresh target, where the target's roster
> is empty and a check written against it finds nothing — and it is also what suppresses a collision
> the merge only passes THROUGH, such as an import that swaps two labels between accounts the target
> already has: mid-loop both are briefly the same, but each ends at one bearer, so `after > 1` is
> false. Comparing against the BEFORE count covers the remaining case and only that one — a duplicate
> the target ALREADY had, overwritten in place, where the label really does have two bearers but the
> count did not move. Each omission produces a false warning of the same class as warning on the
> ordinary cross-machine import, and § P5 says any of them trains dismissal.

> **The fourth command is `remove`, and leaving it out of this table was the defect.** *Corrected
> 2026-08-04 (third pass).* Options (i)/(ii) were framed over `use` vs `enable`/`disable` only, which
> makes (ii) look symmetric with (i) — a wash between two reversible behaviours. It is not: `remove`
> shares the first-match-wins path (`apply_remove`, `src/cli.rs:5219-5227`) and is the only one of the
> four that cannot be undone (`remove_account` deletes the keychain stash, `src/cli.rs:5195-5211`).
> Read with `remove` present, (ii) is not a muscle-memory trade — it standardises on silent
> irreversible deletion. OQ-1 decides this; the table must not pre-frame it as symmetric.

### 4.4 `rotated` telemetry (R-5, R-5a) — **suppress on non-`refreshed`, treat as a contract change**

`classify()` computes `rotated` before the outcome is known (`src/refresh.rs:434-437`); `Dead` is
then *derived* from `after_rt` being `Some("")` (`:445`) — the outcome does not set the token, the
token decides the outcome. So a dead line whose seeded blob carries a parseable, non-empty refresh
token always yields `rotated=true`: the field is true-by-construction there and carries no
information. It is **not** every dead line — `rotated` is `_ => false` for an unparseable seeded blob,
and `"" != ""` → false for an empty seeded token — but that only strengthens the case, since the field
is then *arbitrary* rather than merely uninformative.

**Chosen**: make `rotated` structurally unrepresentable on non-`refreshed` outcomes rather than
merely omitted at the log-formatting layer — carry it *inside* the `refreshed` variant of
`RefreshOutcome` so the type system prevents the meaningless combination, and the emitter cannot
reintroduce it.

**Treat as a log-format contract change** (R-5a): `docs/findings/0465-*` derives a published headline
(`141 rotated=true, 0 rotated=false`) from this field. **0465 is verified for `dead` only — NOT for the whole criterion.** Its window ends
~2026-07-11; the earliest `dead` line is 2026-07-14 — so this is remediation of a forward-looking
trap, not a correction of a published count.

### 4.5 `status` EXPIRY provenance (R-7) — **display-only; do not move authority**

`read_poll_clocks` deliberately reads the canonical for the active account and the stash for parked
ones (`src/daemon/snapshot_build.rs:45-53`). That asymmetry is **correct** — the active account's
truth *is* the canonical item — and § 4.1 does not change it.

The defect is that the two are rendered identically, so a failed adoption looks like a no-op. Fix at
the presentation layer only: make the provenance legible (a marker on the active row, or an explicit
note when a stash and canonical disagree for the same account). Explicitly **not** in scope: changing
which slot is authoritative.

### 4.6 Documentation (R-1, R-1a, R-8)

- **R-1/R-1a** — `docs/findings/0262-anthropic-refresh-token-reuse-behaviour.md`, verdict-first,
  n=1 cardinality stated in the verdict itself (not a footnote), labels redacted (C-5), reconciled
  against 0465.
- **R-8** — the runbook. Per premortem P4, a runbook nothing points at is a document that gates
  nothing: it must be **referenced from `export` and `import` command help**, not merely filed.

### 4.7 Import scope selection (R-9 … R-10b) — **scope is a property of the apply, never of the artifact**

The position in one sentence: **the artifact describes what it carries; the operator decides what is
applied; the operator's decision is a ceiling, never a floor.**

**Why no format change is needed.** `Payload` is exactly two emptiable fields — `config_toml: String`
and `accounts: Vec<ManagedAccount>` (`src/migration.rs:199-210`) — and **every** `RawConfig` field is
`#[serde(default)]`, *including* `account` (`src/config.rs:1377-1396`). So a config carrying only
`[[account]]` parses, and one carrying no roster parses to an empty one. Scope decomposes for free at
the format layer: `FORMAT_VERSION` does not move, golden fixtures do not regenerate, ADR-0006 is not
reached.

**Scope is derived from presence, never declared** (R-9a). The artifact must **not** gain a scope
field. On a `--plaintext` export nothing is authenticated (`src/cli.rs:4471-4479`), so a declared
scope is attacker-controlled: a hostile artifact would assert full scope and the control would
evaporate — converting the feature from a control into theatre. Presence cannot lie, because you
cannot claim content you do not carry.

This also **binds AD-2**: adding a scope field would reintroduce exactly the tamper hazard AD-2
declined for `exported_at`, *and* would force the deletion of AD-2's cost argument (a payload field
cannot be prohibitive here and free there). Presence-derivation is therefore a constraint, not a
preference.

**The model.** Two axes, and the invalid states are unrepresentable:

```rust
struct ImportScope { accounts: bool, settings: bool }   // both true = today's behaviour
```

Secrets are **not** a third axis. They are contained by `accounts`: `apply_import`'s merge loop is
over the roster (`src/cli.rs:4770`) with secrets keyed by uuid (`src/cli.rs:4789`), so a secret with no roster
entry is unreachable code — and R-10 removes the opposite case (`--no-secrets`) from the product.
A flat three-axis model would advertise two states the data model cannot hold.

**Resolution is a lattice meet**: `effective = available(artifact) ∧ permitted(flags)`. The flag can
only ever *remove*. `import --accounts` against a settings-bearing artifact ignores those settings
regardless of what the artifact says; `import --settings` against a roster-only artifact reports
"artifact contains no configuration" rather than erroring or silently no-op'ing.

> **The `available(artifact).settings` operand may not be computable — OQ-6.** A block left at its
> default is byte-indistinguishable from one the artifact withheld (`src/config.rs:1377-1396`), so
> the settings half of this meet is effectively always true for a self-minted artifact, and under
> OQ-6's lean (a) the "artifact contains no configuration" report above is unreachable. The
> *accounts* half is sound (`[[account]]` entries are present or they are not). **Settle OQ-6 before
> implementing #1046** — this paragraph and the § 5 row below both depend on the answer.

**`--accounts` narrow-parses** (R-9b) rather than parse-then-filter. This is not an optimization:
`RawConfig` carries `deny_unknown_fields` (`src/config.rs:1378`), which never fires on blocks outside
the parse path — so narrow-parse *additionally repairs* backward-import for roster-only artifacts
(§ 4.9, R-16). The narrow struct omits `deny_unknown_fields` at the top level while `RawAccount`
(`:1399`) keeps its own, so per-account strictness is preserved and only unknown *blocks* are ignored.

**Export is unchanged** (R-9c), and the asymmetry is principled: **export scope is disclosure hygiene;
import scope is input validation.** Only the latter defends against a hostile artifact, because the
attacker mints the export. Narrowing export would also be actively harmful — since every block
defaults, an omitted block is indistinguishable from a default-valued one, so a receiver cannot tell
*withheld* from *stock*; it would break `Payload`'s losslessness invariant (`src/migration.rs:203-206`), make the
artifact irreversible, and mask R-16's break behind a flag.

**Naming** (R-9d): `--accounts` / `--settings`. `--config` is doubly unavailable — reserved and
value-bearing for issue #24's directory-override ladder (`src/paths.rs:443-444`, the quoted phrase;
`config_dir_with_override` at `:448`), and semantically wrong,
since `account` is a `RawConfig` field and `sessiometer config show` prints the roster. `--accounts`
is the vocabulary `IMPORT_USAGE` already uses ("rehydrate **accounts**", `src/cli.rs:1290`).

**Default stays everything** — on the *scope-selection axis* the default adds no narrowing, so nothing
this section introduces changes what a no-flag `import` applies. That is the **only** axis on which
"today's behaviour" holds: § 4.8's allowlist binds **regardless of the flag**, so on a fresh target —
where the artifact's config was previously adopted wholesale (`src/cli.rs:4744-4750`) — a non-portable
key that used to be adopted is now refused and reported. § 8's `config adoption` row records that as a
behaviour change on exactly this path; do not read "default unchanged" as end-to-end byte-identity.
The safety argument for defaulting to `--accounts` is real but is **absorbed by § 4.8**: with the
capability keys refused unconditionally and `kdf_*` held to a monotonic floor (R-11b — adopt a knob
only if `incoming >= local` for that knob, both required), the delta a narrow default would have
closed is already closed, and changing a
shipped command's default costs more than that
delta is worth. The two decisions are coupled and § 4.8 is decided first.

### 4.8 Portability classification (R-11 … R-11f) — **allowlist, not denylist**

Scope selection answers *what the operator asked for*. This answers *what the system permits*, and it
binds **regardless of `--settings`**.

```rust
enum Portability { Portable, MachineBound, CapabilityGranting }
```

**Default is non-portable.** A key is adopted only if explicitly classified `Portable`. This inversion
is the whole design: a denylist rots — the next spawnable key added to `Config` auto-adopts and nobody
notices until it is exploited — whereas an allowlist forces the decision at **add-time** instead of at
**exploit-time**.

**The decided carve-outs** (the rest of the table is implementation, and R-11d is what forces it to be
complete):

| Key | Class | Why |
|---|---|---|
| `[login].claude_bin`, `[refresh].claude_bin` | **CapabilityGranting** — never adopted | Resolution absolutizes against cwd and accepts any `is_file()`, with no allowlist, no signature, and deliberately no symlink resolution (`src/paths.rs:773-807`); the refresh tick then spawns it (`src/refresh_tick.rs:258` → `:273` → `src/refresh.rs:694` (`SpawnClaude::new`)). Adoption is arbitrary code execution, unattended, on a timer. |
| `[migration].conflict_policy` | **MachineBound** | Encodes the *target* operator's decision. Today an artifact cannot overwrite it; `--settings` would newly allow it — not for the import that adopts it (`resolve_import_overwrite` reads local first, `src/cli.rs:4628`) but for every one after. Resolved conservatively over a recorded dissent (PRD § 9 D-1). |
| `[migration].kdf_*` | **Portable, monotonic floor — per knob** | Adopt a knob only if `incoming >= local` **for that knob**, and refuse the block unless **both** knobs pass. `kdf_*` is **two independent `u32`s** — `kdf_memory_kib` (`8..=1_048_576`) and `kdf_iterations` (`1..=16`), `src/config.rs:985`/`:988` — so "stronger" is a **partial** order, not a total one, and the incomparable case is reachable. A fleet may standardize upward; nothing may downgrade. |

> **"Stronger" is not a total order, and the incomparable case downgrades through the requirement
> written to prevent downgrades.** *Added 2026-08-05 (eleventh pass); every surface stated one scalar
> `incoming >= local`.* `[migration]` carries exactly two independent cost knobs, so an artifact can
> be stronger on one and weaker on the other: `kdf_memory_kib = 1_048_576, kdf_iterations = 1`
> against the shipped defaults `65536 / 3` (`src/config.rs:998-999`) is neither weaker nor stronger.
> A comparator written on the memory knob alone — the knob the prose foregrounds, since it is what
> kills the 8 KiB downgrade path — adopts that block and lands `kdf_iterations = 1`, a downgrade from
> 3. **Compare per knob and require both**; on an incomparable pair, refuse the block and report it
> (R-11e) rather than adopting the half that improved. Cap-8.2 and its scenario stay green through
> this either way, because both only ever feed uniformly-weaker or uniformly-stronger pairs — which
> is why the case has to be written down rather than left to the comparator's author. R-11d exists
> because unenumerated branches rot; this is an unenumerated branch **inside** a carve-out.

**Refusing `claude_bin` costs nothing**, which is what makes it easy: the value is a local path, so on
the target it either does not exist or names a *different* binary — there is no workflow in which
adopting the source machine's is correct. ADR-0030 additionally documents `CLAUDE_BIN=…` as a local
escape hatch, and governs resolution **order** rather than value provenance, so this refusal does not
contradict it.

**Rejected: a second confirmation flag.** `--settings --allow-exec-override` trains escalation — the
error message becomes the exploit instruction. **Rejected: strip-on-export as the control** — the
attacker mints the export, so validation must sit on the boundary we own. (Strip-on-export is still
worth doing on *correctness* grounds: an honest export carrying an absolute home-directory path
produces a broken import elsewhere, and leaks the path. It is a fix, not a control.)

**The guard (R-11d) is the load-bearing half.** An unenforced allowlist is a denylist with extra steps.
Preferred mechanism: an **exhaustive `match`** over a key enum, so adding a `Config` field without
classifying it is a **compile error** rather than a test failure — the strongest available form, and it
cannot be skipped or marked `#[ignore]`. Where the type shape does not permit that, a test asserting
classification-completeness over the field set is the fallback; a lint that warns and passes is not
acceptable.

**Refusals are surfaced** (R-11e). A silently dropped `claude_bin` is indistinguishable from one that
was never present — which would leave the operator believing their config transferred.

**The classification gets its own ADR** (R-11f). ADR-0006 governs *schema evolution* — whether the
format may change. Value **portability** — whether a value may cross machines — is a different
question with no current home, and it is the one a future contributor will need when adding a key.

### 4.9 Lifetime, source-side prevention, and legibility (R-12 … R-16)

**R-12 — artifact lifetime.** `import` reads the file and leaves it (`src/cli.rs:4602`);
`PLAINTEXT_WARNING` advises deleting it with **no mechanism**, and only on the `--plaintext` path,
while an encrypted artifact is still a live-credential file behind one passphrase. Under § 4.7 the
applied payload *can* narrow to the roster, but AD-9 keeps the default at everything — and the file on
disk is unchanged regardless, because scope selection is import-side only (R-9c/AD-5), so `export`
still writes the full rendered config. The artifact is a live-credential file whatever the operator
selects, which is what makes this urgent. Design: `import --shred` unlinks the source after a successful apply.
**Stated honestly**: on APFS, overwrite-in-place does not reliably destroy the prior extent, so this is
`rm` with intent, not forensic erasure. It must be documented as such — claiming secure-erase we do not
deliver is the same false-assurance failure AD-2 declines.

**R-13 — source-side prevention.** The design's own thesis is that the hazard is *"not detectable at
the target — only preventable at the source"*, and there is currently **zero** source-side
implementation: `export` never asks whether this machine's daemon is running (`src/cli.rs:4455-4501`).
Liveness is locally probeable via the existing control socket. Use `daemon_liveness()`
(`src/cli.rs:1885`) — read-only, socket-primary with lock-fallback, already shared by `daemon
status` and `daemon restart`. Do **not** wire this to `notify_daemon_roster_reload()`
(`src/capture.rs:335`): its own doc comment declares it BEST-EFFORT and it returns `()`, so a
connect refusal is indistinguishable from a live daemon. Design:
`export` probes, and warns when the daemon is live — the one moment the operator can still act.
Warning **only** when live, never unconditionally, so it does not train dismissal (RSK-1's failure
mode).

**R-14 — correlation.** `Event::Export` and `Event::Import` both gain a sha256 artifact digest; the
**`import`** event additionally gains the operator-**requested** scope (never the artifact's claimed
scope, per R-9a). `export` has no operator-requested scope to carry — it takes no narrowing flag
(R-9c, AD-5, Cap-7.5) — so export-side correlation rides on the digest alone. Both fit the existing
aggregate-only redaction discipline (`src/observability.rs:1426-1442`) — no label, no token, no email.

**R-15 — input validation.** `account_uuid` is validated for **non-emptiness and uniqueness only**
(`src/config/validate.rs:281-293`, reached from `apply_import`'s parse at `src/cli.rs:4735`) and is
otherwise interpolated into a keychain service name (`src/config.rs:370-372`) with its **shape and
length unchecked**. **Severity is bounded and the bound is verified**: `stash()` reaches no
filesystem path, and keychain service names are opaque strings rather than hierarchical paths, so
`Sessiometer/../x` is a literal name and not a traversal. Residue is **shape and length only** —
namespace squatting (`"../x"` and `" x "` both pass `validate`) and unbounded length. **The empty case
already ships**, so specifying it would be green over unimplemented work (PRD R-15's note).
Validate shape on parse; **do not file or fix this as a
traversal**, which would manufacture a severity the evidence does not support.

**R-16 — the `[credential]` backward-import break.** `RawConfig` carries `deny_unknown_fields`, so a
binary built before commit `6fe3457` (2026-07-29) **rejects** an artifact carrying `[credential]` — a
block added 26 days after ADR-0006 froze v1, and never tracked. The asymmetry worth naming: **we cannot
fix already-released binaries.** So the design is forward-looking in two parts — (a) document the
version floor and make the failure legible rather than a bare parse error, and (b) **stop it
recurring**, by making the artifact-config parse path tolerant of unknown blocks so the *next* block
added does not re-break it. R-9b's narrow-parse delivers (b) for the roster-only case as a side effect;
the full-artifact case needs the same treatment deliberately.

> **This paragraph's premise is wrong on the FLOOR and on the UNIT.** *Corrected 2026-08-09
> (#1053), against the working tree; the text above is left as written since R-16 is ratified.*
> **(1) The floor is not `6fe3457`.** That is where the `[credential]` block arrived, but
> `expiry_cohort_window_secs` landed inside it **14 commits later the same day** (`81bd4f2`, issue
> #879), so a binary built at exactly `6fe3457` still rejects a current artifact. The floor is the
> most recent commit that added a **rendered config key** — today `81bd4f2`. Reproduced in
> `src/config/load.rs::a_build_at_the_blocks_own_commit_still_rejects_a_current_render`.
> **(2) The unit is a key, not a block.** *Every* `Raw*` struct carries `deny_unknown_fields`, not
> only `RawConfig`, so an unknown key at any nesting level is refused exactly as an unknown block
> is. Since the freeze the rendered config has gained one new top-level block and 22 new value
> keys, so "(b) tolerant of unknown **blocks**" would repair 1 case in 23 — an implementer of (b)
> must build **key**-tolerance. See ADR-0006 § Status, and OQ-5 in § 14 Risks and Open Questions.

## 5. Building Blocks

| Block | Change | Requirements |
|---|---|---|
| `src/cli.rs::import` | report non-adoption + name `use --force <label>` (the unqualified form is a no-op on the active account — AC-2a); optional `--activate` | R-2 |
| `src/cli.rs::apply_import` | duplicate-label collision check; staleness warning emission | R-4, R-6 |
| `src/refresh.rs::classify` | reshape `RefreshOutcome` so `rotated` rides inside `Refreshed` — **necessary, not sufficient**; the three renders read a sibling field, see the note below | R-5 |
| `src/refresh.rs::RefreshReport` + the three renders in `src/observability.rs` | **remove the field from the non-`refreshed` renders** — see the note below; reshaping `RefreshOutcome` alone does not do it | R-5 |
| `src/daemon/snapshot_build.rs` + status render | provenance legibility | R-7 |
| `src/use_account.rs::resolve_target` / `src/poke.rs` / `src/daemon/commands.rs` / `src/cli.rs::apply_enabled` / `src/cli.rs::apply_remove` | consistency per OQ-1 — **across two different mechanisms and six call sites**, not one policy over four verbs; see the note below. **Done (#1005)**: the last two now call `resolve_target`, collapsing the two mechanisms into one | R-6a |
| `docs/findings/0262-*.md` | new | R-1, R-1a |
| `docs/*` runbook + command help | new | R-8 |

*Rows below added 2026-08-04 — the amendment's 21 requirements (R-9 … R-16, counting sub-letters) had no building-block
rows, including the entire security core:*

| Block | Change | Requirements |
|---|---|---|
| `src/cli.rs::parse_import` | accept `--accounts` / `--settings`; default (neither) narrows nothing — the flag surface alone leaves today's behaviour intact (§ 4.8's allowlist applies independently) | R-9, R-9c, R-9d |
| `src/cli.rs::import` | narrow-parse under `--accounts`; "artifact contains no configuration" notice under `--settings` against a roster-only artifact (**OQ-6-gated** — settle before building this notice); `--shred` | R-9a, R-9b, R-12 |
| `src/cli.rs::apply_import` | apply the portability allowlist before adopting any non-roster value; emit a refusal line per refused key | R-11, R-11a … R-11c, R-11e |
| **new** `src/config.rs::portability` | the allowlist itself (non-portable by default) + the `kdf_*` monotonic-floor comparator + the compile-time rot-guard that fails when a new `Config` key carries no classification | R-11, R-11b, R-11d |
| `src/cli.rs::parse_export` | **remove** `--no-secrets`; strict-usage error stating roster-without-secrets is no longer supported (there is no replacement to name — R-9c/AD-5) | R-10 (form is OQ-4-gated) |
| `src/cli.rs::export` | daemon-liveness probe via `daemon_liveness()` (`src/cli.rs:1885`) before writing | R-13 |
| `src/migration.rs` | `PLAINTEXT_WARNING` wording (`src/migration.rs:538`); `[credential]` forward-tolerance + version-floor message | R-10b, R-16 |
| `src/observability.rs` | sha256 artifact digest on **both** events + requested scope on the **`import`** event only (export has none — R-9c/AD-5); allowlist-refusal signal | R-14, R-14a |
| `src/config.rs::Account` | `account_uuid` shape validation before it reaches `stash()` | R-15 |

> **There WERE two label-resolution mechanisms in this tree, and OQ-1 was the question of which one
> wins.** *Corrected 2026-08-05 (twelfth pass); every surface said "all four label-resolving commands"
> — naming `use`, `enable`, `disable`, `remove` — which is wrong on both the count and the substance.*
> **Superseded 2026-08-06 by #1005: OQ-1 resolved toward the first row, and the second row's two call
> sites were routed into it — there is now ONE mechanism over six sites.** The table below records the
> pre-fix state, which is what makes the divergence it describes legible; read it in the past tense.
> Derived from source rather than sampled (`.tmp/enumerate.py`):
>
> | Mechanism | Matches | On a duplicate label | Call sites |
> |---|---|---|---|
> | `use_account::resolve_target` (`src/use_account.rs:441-459`) | `label` **or** `account_uuid` | `Error::UseTargetAmbiguous { count }` — **refuses**; its doc says the resolver *"NEVER guesses"* | `use` (`src/use_account.rs:607`), **`poke`** (`src/poke.rs:290`), **the daemon control-socket swap** (`src/daemon/commands.rs:99`) |
> | exact-label `.find()` / `.position()` — **retired by #1005** | `label` only | **silently took the first match** | `enable` / `disable` (`apply_enabled`), `remove` (`apply_remove`) — both now call `resolve_target` |
>
> So on the duplicated label R-6 says `import` can create, `use` and `poke` **refused** while `remove`
> **silently deleted the first match's keychain stash**. That divergence — not a missing verb — is
> what OQ-1 had to settle, and neither `poke` nor the daemon path appeared on any surface.
> `enable`/`disable`/`remove` do not merely *differ in policy*; they never reach the ambiguity-capable
> resolver at all, so "make the four consistent" is not implementable as written.
>
> **How this was found matters.** Three prior passes each reported one more member of this set
> (a fourth enum variant, a fourth outcome, a fifth verb) because an adversarial reader *samples* a
> set. This table is *derived* — re-run `.tmp/enumerate.py` rather than trusting the count here.

**Untouched by design**: `src/swap.rs` (reused, not modified).

> **`rotated=` is emitted from three modules onto three log lines, and reshaping `RefreshOutcome`
> removes it from none of them.** *Added 2026-08-05 (eleventh pass); this table listed
> `classify` alone and closed with the completeness line above.* The field the renders read is
> **`RefreshReport.refresh_token_rotated`** (`src/refresh.rs:284`) — declared a **sibling of**
> `outcome`, not a payload inside it — and it is interpolated unconditionally at
> `src/observability.rs:2155` (`event=refresh`), `:2173` (`event=poll_refresh`) and `:2191`
> (`event=keep_warm`), fed from `src/refresh_tick.rs`, `src/daemon/refresh_fold.rs` and
> `src/daemon/keep_warm.rs`. The code states the multiplicity in terms: *"three separate refresh
> mechanisms, three separate event names"* (`src/observability.rs:2187`).
>
> **And a FOURTH surface, which is not a log line at all.** *Added 2026-08-05 (twelfth pass).*
> `refresh_fold` also folds the value into daemon state on **every** outcome — its comment says
> *"Armed for EVERY outcome, including `Dead` / `Error`"* (`src/daemon/refresh_fold.rs:557`) — and
> `refresh_health_view` projects it onto the **versioned `status`/`watch` wire**:
> `rotated: health.refresh_token_rotated.unwrap_or(false)` (`src/daemon/snapshot.rs:1403`). Its
> consumer is **Swift**: `apps/menubar/Sources/WireModel.swift:98`, asserted in
> `apps/menubar/Tests/WireDecoderTests.swift` and pinned in committed JSON fixtures.
>
> This changes what R-5 costs. The three log lines are free to fix. The wire is not: `.unwrap_or(false)`
> means the cheapest repair leaves `"rotated": false` on every `dead` / `no_change` / `error` account —
> the exact uninformative value R-5 removes, now on a **versioned** surface — while actually dropping
> the field is a `STATUS_SCHEMA_VERSION` change carrying the status/watch goldens plus the Swift
> fixtures and `WireDecoderTests` assertions. **Neither consequence appeared in R-5a's consumer list
> (which names only `docs/findings/0465-*`), § 8's Interface-Change table, the risk register, or
> #1004** — the whole artifact set costed R-5 as a log-format change.
>
> **RESOLVED 2026-08-06 by issue #1070 — the wire took a third path, not either costed one.**
> *Added 2026-08-06; the paragraph above is preserved as the statement of the problem.* #1004 shipped
> the three log lines plus the wire's `.unwrap_or(false)` and stopped there, exactly as R-5a
> reserved. #1070 then made `RefreshHealth.rotated` an `Option<bool>` carrying
> `skip_serializing_if`, so the key is present with a real value where an exchange ran and ABSENT
> everywhere else — AC-5 now holds across all four emitting surfaces, and the wire follows the same
> rule AD-3 gave the log path rather than a fourth rule of its own. Paid: a MINOR
> `STATUS_SCHEMA_VERSION` bump 1.13 → 1.14, the five status/watch goldens regenerated, and the Swift
> mirror + fixtures + decoder assertions swept in lockstep.
>
> Note what the reshape does and does not buy, since AD-3's parallel is close but not exact. On the
> log path the payload moved INSIDE `RefreshOutcome::Refreshed`, so a non-refreshed outcome has no
> field to render — a type-level guarantee. Here `last_ok` and `rotated` remain sibling fields, so
> the LITERAL `RefreshHealth { last_ok: false, rotated: true }` this issue named stops type-checking,
> but `rotated: Some(_)` beside `last_ok: false` is still expressible by a hand-written construction.
> The guarantee is therefore a **constructor** invariant — `refresh_health_view` reads both off one
> outcome — pinned exhaustively across every `RefreshEventOutcome` variant by
> `refresh_health_view_never_pairs_a_rotation_with_a_non_refreshed_outcome`, so a sixth variant
> cannot reach the wire without that test being reopened. Recorded here rather than left implied,
> because Cap-4.1 rejects suppressions a later emitter change can reintroduce and this one is a
> narrower guarantee than AD-3's.
>
> So AD-3's reshape and Cap-4.1 — *"Given the `RefreshOutcome` type / When a non-`refreshed` outcome
> is constructed / Then no rotated value can be attached to it"* — are both **satisfiable with all
> three renders untouched**, and R-5's Planguage meter (*"unit test over `classify()` across all four
> returns"*) measures the type, never the emitted line. An implementer who builds exactly this ships
> `outcome=dead rotated=false` on every keep-warm and poll-refresh line, which is the defect R-5
> exists to remove.
>
> **`keep_warm` is the worst of the three**: its own doc says it renders
> `refreshed_not_restashed` on a real mint and *"never renders `refreshed`"*
> (`src/observability.rs:1282-1284`), so on that line `rotated` is meaningless on **every** outcome
> R-5 targets — while AC-5's carve-out (`refreshed_not_restashed` keeps `rotated`) exempts the one
> outcome where it is real. **R-5 is satisfied only when all three renders drop the field**; assert
> on the rendered line, not on the type.

> **Correction 2026-08-04.** This line previously also listed `src/migration.rs` as untouched, on the
> C-1 "no format change" rationale. C-1 still holds — `FORMAT_VERSION` does not move — but R-10b,
> AC-10 and Cap-7.8 require `PLAINTEXT_WARNING`'s **wording** to change, and R-16 adds
> forward-tolerance for the `[credential]` block. Those are edits to `src/migration.rs` that are not
> format changes. "No format change" and "file untouched" are different claims, and only the first
> one was ever true.

## 6. Runtime View — the corrected migration flow

```
SOURCE (A)                                   TARGET (B)
  stop daemon              ── R-8 ──►  (source no longer rotates)
  export
   ├─ PROBES daemon liveness; WARNS if live, never blocks      (R-13)
   ├─ WARNS on --plaintext (reworded)                          (R-10b)
   ├─ no --no-secrets flag — removed, strict-usage error       (R-10; OQ-4)
   └─ LOGS sha256 digest (export has no operator scope)        (R-14)
         │
      artifact ──────────────────────►  import [--accounts] [--settings] [--shred]
                                          │   (default = everything: no narrowing; § 4.8 still binds)
                                          ├─ scope from PAYLOAD PRESENCE, never self-declared  (R-9a)
                                          ├─ VALIDATES account_uuid shape before stash()       (R-15)
                                          ├─ ALLOWLIST gate on every non-roster value:         (R-11)
                                          │     claude_bin      ─► REFUSED, no flag overrides  (R-11a)
                                          │     kdf_*           ─► adopt iff incoming >= local per knob,
                                          │                        both required; else refuse (R-11b)
                                          │     conflict_policy ─► machine-bound, not adopted  (R-11c)
                                          │     each refusal REPORTED on stdout                (R-11e)
                                          ├─ writes stashes + roster
                                          ├─ WARNS: source must not refresh after export       (R-4)
                                          ├─ WARNS: duplicate label created, if any            (R-6)
                                          ├─ LOGS digest + REQUESTED scope                       (R-14)
                                          ├─ REPORTS: active acct staged, run
                                          │           `use --force <label>`                    (R-2)
                                          └─ --shred: unlink the artifact (rm with intent, not erasure)          (R-12)
                              use --force <label> ──► swap engine (#64 lock) ──► canonical
                                    status ──► EXPIRY with legible provenance                  (R-7)
```

The failure on 2026-07-31 was the **first arrow** never happening: A kept its daemon running and
refreshed 4 minutes before B replayed. R-13's liveness probe is the mechanization of that arrow —
which is why it sits on the **source** side: the hazard is preventable there and undetectable at the
target (§ 1, the one-sentence design position).

> **Re-derived 2026-08-04.** The pre-amendment diagram showed only R-2 / R-4 / R-6 / R-7 / R-8 — no
> scope flags, no allowlist, no refusals, no shred, no export-side probe, no observability. A runtime
> view that omits the security core is not a view of this design.

## 7. Deployment View

No new processes, files, or IPC. All changes are in the existing CLI binary and two new markdown
documents. No migration of on-disk state; no `format_version` change (§ 4.2).

## 8. Interface Contracts

| Surface | Change | Compatibility |
|---|---|---|
| `import` stdout | new warning + report lines | additive; C-3 forbids any credential in them |
| `import` flags | optional `--activate <label>` | additive, opt-in |
| `refresh` / `poll_refresh` / `keep_warm` log lines | `rotated` absent on non-`refreshed` — **three lines, not one** (`src/observability.rs:2155`, `:2173`, `:2191`) | **contract change on all three**; consumer 0465 checked for `dead` only — its 141-count derives from *event type* (86 `refresh` + 31 `keep_warm` + 24 `poll_refresh`), so a `no_change` line would still re-baseline it (open, tracked on #1004) |
| `status` / `watch` wire | `RefreshHealth.rotated` — **a versioned surface with a Swift consumer** (`src/daemon/snapshot.rs` `RefreshHealth` → `apps/menubar/Sources/WireModel.swift` `RefreshHealth`) | **contract change, and the expensive one.** *Costed here as a binary — drop the field (a `STATUS_SCHEMA_VERSION` bump carrying the status/watch goldens and the Swift fixtures) or keep `.unwrap_or(false)` and go on rendering the value R-5 removes. **RESOLVED 2026-08-06 by issue #1070, which took neither branch**: the field became `Option<bool>` with `skip_serializing_if`, so it is present with a real value where an exchange ran and ABSENT elsewhere — the rotation signal is kept, only the fabricated value is dropped. Paid at `STATUS_SCHEMA_VERSION` **1.13 → 1.14** (minor) with the five status/watch goldens regenerated and the Swift mirror re-typed to `Bool?`. **Forward-compat is asymmetric and this is the operationally consequential fact**: a pre-#1070 menubar build typed the key as required, so it DROPS a 1.14 non-refreshed line — daemon and app must be updated together. The Rust `status` client is immune (same binary as the daemon). The reverse is clean: a 1.14 client decodes a ≤1.13 daemon's always-present key as `Some(_)`.* |
| artifact format | **none** | v1 preserved (C-1, C-4) |
| `import` flags | `--accounts`, `--settings`, `--shred` | additive, opt-in; **default unchanged** (AD-9) |
| `export` flags | **`--no-secrets` REMOVED** | **breaking** — the only breaking CLI change in this scope. Path undecided (OQ-4) |
| `export` **stderr** | daemon-liveness warning when the local daemon is live | additive; conditional, never unconditional (R-13). **stderr, never stdout** — with `PATH` omitted `export` writes the artifact itself to stdout (`src/cli.rs:4559-4565`), and the existing `PLAINTEXT_WARNING` already takes this rule with the reason stated in the code: *"Warn on stderr — never stdout, which may carry the artifact"* (`src/cli.rs:4472-4474`). A warning on stdout prepends its bytes to the artifact, which then fails `preamble.magic != MAGIC` (`src/migration.rs:360`) — the warning built to save the migration destroys it, and only on the branch where it fires |
| `import` stdout | per-key refusal lines from the portability allowlist | additive; C-3 applies |
| config adoption | **on an existing-config target**: non-portable keys were already dropped (`apply_import` keeps `local` wholesale) → now refused **and reported**. **On a fresh target**: they were **adopted** → now **refused**, a real behaviour change, not just a new line | **behaviour change** on the fresh-target path (`src/cli.rs:4744-4750`). Reading the Change cell as "only a new report line" is exactly the misreading Cap-8.7 exists to catch |
| `Event::Export` | `+ artifact_sha256` | additive; aggregate-only redaction preserved. **No `+ scope`** — export takes no narrowing flag (R-9c/AD-5), so it has none to log |
| `Event::Import` | `+ artifact_sha256`, `+ scope` | additive; aggregate-only redaction preserved |

## 9. UX Architecture / 10. UI Strategy

**n/a** — CLI and daemon only. No menu-bar surface is touched. Recorded as an explicit negative so
its absence reads as by-design.

## 11. Crosscutting Concepts

**Security.** Every warning, report, and findings note is a credential-adjacent surface. C-3 is
enforced by the existing redaction test; new output lines must be covered by it or an equivalent.

**Concurrency.** § 4.1's central choice is *not to add a **canonical** writer*. `import` already
takes the #64 swap lock — `import` resolves it (`src/cli.rs:4616`), passes it to `apply_import`
(`:4631`), which acquires it whenever the artifact carries secrets (`:4765`) and holds it across the
stash writes. What § 4.1 declines to add is a second writer of the canonical
`Claude Code-credentials` item; the lock guarding the *stash* writes is pre-existing and stays.

> **"Adds no canonical writer" is not "takes no lock" — corrected 2026-08-04 (third pass).** This
> paragraph previously read *"The only lock interaction is the one `use` already performs"*, and the
> Cap-1.2 scenario asserted *"no swap lock was acquired by the import path"*. Both were false and
> corroborated each other. The danger was not the failing test: it is that an implementer trusting
> two agreeing documents would **remove** the lock from the import path to make the assertion pass,
> deleting the single-writer discipline of hard constraint C-2 on the exact keychain writes #64
> added it for.

### Master Test Plan

| Cap | Capability under test | Type | Requirement |
|---|---|---|---|
| Cap-1.1 | Import of the target's **active** account reports non-adoption and names **`use --force <label>`** — asserted on the `--force` token, since the unqualified form is the AC-2a defect and a test that accepts it would pass while shipping the no-op | unit (`apply_import` outcome) | R-2, AC-2a |
| Cap-1.2 | Import adds no canonical writer — canonical byte-unchanged across import | integration | R-2a, C-2 |
| Cap-2.1 | Every credential-bearing import emits the staleness warning, and its safe sequence names **`use --force <label>`** — **asserted on the `--force` token**, since the unqualified form is a provable no-op and a test that accepts it passes while shipping guidance that reproduces the incident | unit | R-4 |
| Cap-2.2 | Warning fires even when derived deadlines are unreadable (fail-closed) | unit | R-4, P2 |
| Cap-2.3 | An already-expired artifact additionally reports expiry | unit | R-4a |
| Cap-3.1 | Same-label/different-uuid import warns — with a target that is **not** a clone of the source | unit | R-6 |
| Cap-3.2 | `use` / `enable` / `disable` / **`remove`** agree on duplicate-label resolution — `remove` is not optional: it is the only irreversible one (deletes a keychain stash), so a test omitting it passes while the case that motivated R-6a stays unasserted | unit | R-6a |
| Cap-4.2 | **All four rotation-emitting surfaces drop the field** — `event=refresh` (`src/observability.rs:2155`), `event=poll_refresh` (`:2173`), `event=keep_warm` (`:2191`), and the **versioned `status`/`watch` wire** (`src/daemon/snapshot.rs:1403`, `rotated: health.refresh_token_rotated.unwrap_or(false)`), whose consumer is Swift (`apps/menubar/Sources/WireModel.swift:98`). The renders read `RefreshReport.refresh_token_rotated` (`src/refresh.rs:284`), a **sibling of** `outcome` — so Cap-4.1 and AD-3's reshape are both satisfiable with all three untouched. Assert the rendered line, not the type | unit (render) | R-5 |
| Cap-4.1 | `rotated` is unrepresentable on **every non-`Refreshed` outcome** — `NoChange`, `Dead`, `Error` (all three, `src/refresh.rs:225-240`). Asserting only `Dead`/`Error` lets an implementation keep `rotated` on `NoChange`, a live outcome (`src/observability.rs:180`) whose `rotated` is derived independently of its expiry test. Excludes `refreshed_not_restashed`, an *event* outcome mapped from `Refreshed` where `rotated` is meaningful | unit (type-level) | R-5 |
| Cap-5.1 | `status` distinguishes canonical-sourced from stash-sourced EXPIRY | unit | R-7 |
| Cap-6.1 | No import output line contains a token or email | unit (extend existing) | C-3 |
| Cap-7.1 | `import --accounts` applies roster + secrets and **no** non-roster block | unit (`apply_import` outcome) | R-9 |
| Cap-7.2 | Default `import` (no scope flag) applies the same payload classes today's `import` applies — **modulo § 4.8's allowlist, which binds on this path too**. Assert scope-equivalence, NOT end-to-end byte-identity: on a fresh target a non-portable key that used to be adopted is now refused (§ 8, `config adoption`). A byte-identity assertion here goes red, and the cheapest way to green it is to exempt the no-flag path from the allowlist — reinstating the § 1 code-execution path | integration (regression) | R-9, AD-9 |
| Cap-7.9 | **`import --settings` applies allowlist-filtered config and writes NO roster entry and NO credential** — the mirror of Cap-7.1, and the only assertion of the `--settings` narrowing. **OQ-7-gated on target state**: its *Then* is satisfiable on a **fresh** target today; on a target that **already has a config** it is unsatisfiable under OQ-7(a), because `apply_import` discards the incoming non-roster blocks entirely (`src/cli.rs:4744-4750`) and there is nothing to apply. Pin the target state in the scenario | unit (`apply_import` outcome) | R-9 |
| Cap-7.3 | A scope flag can only narrow — an artifact cannot widen it | unit | R-9a |
| Cap-7.4 | An artifact whose roster is the payload of interest round-trips through narrow-parse **even when it carries an unknown non-roster block**. Not "a roster-only artifact under an unknown block": *roster-only* is pinned to `[[account]]` entries **and no non-roster block** (`import-scope-selection.feature.md`), which an unknown block contradicts | unit | R-9b, R-16 |
| Cap-7.5 | `export` exposes no config/roster narrowing flag | unit (usage assertion) | R-9c |
| Cap-7.6 | `import --settings` on a roster-only artifact reports "no configuration", not an error | unit | R-9 — **OQ-6-gated**, see § 4.7 |
| Cap-7.11 | **`import --accounts --settings` given together** applies the union both flags name, not whichever the parser assigned last — the fourth cell of the scope 2×2, and the one that does not fail safe: `lexopt` does not reject a combined flag for free, so last-flag-wins silently discards the roster the operator asked for while every other Cap-7.x still passes | unit (flag parse + `apply_import` outcome) | R-9, R-9a |
| Cap-7.10 | `import --accounts` on an artifact with **no `[[account]]` entries** reports it, rather than erroring or silently succeeding having applied nothing — the accounts-axis mirror of Cap-7.6, and **not** OQ-6-gated: the accounts axis *is* presence-derivable (OQ-6's own wording) | unit | R-9 |
| Cap-7.7 | `export --no-secrets` exits with a **strict-usage error stating that roster-without-secrets is no longer supported** — asserted on both halves: non-zero exit AND the explanation present. **Not "names the replacement"**: nothing replaces the flag (R-9c/AD-5 forbid any export-side narrowing flag, and `import --accounts` narrows what is *applied*, not what the file *contains*), so a "replacement named" assertion has no referent. Explicitly asserts the flag is **not** silently accepted-and-ignored | unit (usage assertion) | R-10 (**not** R-10a — see note) |
| Cap-7.8 | `PLAINTEXT_WARNING` reflects that every artifact **with a non-empty roster** now carries credentials, and advises no deletion the tool provides no mechanism for. **Not "every artifact"** unqualified — an empty roster yields zero credentials (`src/cli.rs:4535-4546`), so the guard must be re-expressed over the artifact's credential count, not deleted with the flag | unit (text assertion) | R-10b |
| Cap-8.1 | `[refresh].claude_bin` from an artifact is **never** written to the target config, even with `--settings` — **asserted on a target with no existing config**, since an existing one makes `apply_import` discard the incoming blocks anyway (`src/cli.rs:4744-4750`) and the assertion passes with nothing built | integration | R-11a |
| Cap-8.2 | A weaker incoming `kdf_*` is refused; a stronger one is accepted; **an incomparable pair — stronger on one knob, weaker on the other — is refused as a block**. All three cases, or the third goes unwritten and a one-knob comparator passes the first two | unit | R-11b |
| Cap-8.3 | `[migration].conflict_policy` is not adopted — **on a fresh target**, and with the refusal reported. R-11c's **sole** capability, and it rests on the D-1 dissent, so a free green here is the worst place for one: on an existing-config target every *Then* holds with no allowlist written | unit | R-11c |
| Cap-8.4 | **Adding a `Config` key without a portability classification fails the build** | compile-fail / completeness test | R-11d, **R-11** |
| Cap-8.5 | Every refusal is reported on stdout | unit | R-11e |
| Cap-8.6 | **A non-portable key outside the three named carve-outs is not adopted, `--settings` notwithstanding** — the allowlist's default-deny asserted over an *ordinary* key classified non-portable. **The subject is chosen when the classification table is built**: § 4.8 fixes three carve-out keys and leaves the rest to implementation, so no block is classified non-portable at design time. `[jitter]` and `[credential]` are the two candidates PRD § 1 neither calls freely portable nor carves out — **neither is decided here**. If the built table leaves no non-carve-out block non-portable, **this capability has no subject — escalate to the ADR (#1003)**; a purpose-built fixture key does *not* work (`deny_unknown_fields` rejects it before the allowlist runs — see the spec note), and **never reclassify a real block to green this test** (R-11f puts that call in the ADR) | unit | **R-11** |
| Cap-8.7 | **The allowlist binds with no scope flag at all, on a fresh target** — the shipped hazard and the *default* path (AD-9). Cap-8.1/8.2/8.3 all put `--settings` in their *When*, so an implementation hanging the allowlist off the `--settings` branch passes all three while leaving § 1's code-execution path reachable by default | integration | **R-11**, R-11a, AD-9 |
| Cap-9.1 | `import --shred` removes the source artifact after a successful apply | integration | R-12 |
| Cap-9.2 | Shred is not claimed as secure erase in help or docs | unit (text assertion) | R-12 |
| Cap-10.1 | `export` warns on **`Responsive`, `AliveUnresponsive` and the `Err` arm**, and is quiet **only** on `NotRunning` — **four** branches. `daemon_liveness()` is `Result<DaemonLiveness>` (`src/cli.rs:1885`) over a tri-state enum (`:1870-1878`), so `Err` sits alongside the three `Ok` variants and must be mapped, not left to the implementer. A wedged daemon still holds the lock and still refreshes; an errored probe has not established the daemon is absent — both fail **closed** | integration (see note) | R-13 |
> **Cap-10.1 needs a seam, and the design must name it.** *Added 2026-08-05 (ninth pass); the row was
> typed `unit`.* `daemon_liveness()` takes **no parameters** (`src/cli.rs:1885`) and resolves
> `paths::control_socket()` / `paths::daemon_lock()` from `support_dir()`, which `src/paths.rs:531-532`
> documents as *"**always** at the platform's fixed native location — **never** an env-var override"*,
> deliberately (issue #7). There are currently **zero** tests over it. So none of the four branches is
> hermetically constructible: `Responsive` needs a real listener at the native socket path,
> `AliveUnresponsive` needs the real lock flocked, `Err` is reachable but not from a *hermetic* test
> without the seam (**corrected 2026-08-05, twelfth pass — this read "`Err` is not reachable at all",
> which is false against source and contradicted Cap-10.1 seven lines above**: `daemon_liveness()`
> has three `?` sites — `control_socket()?`, `daemon_lock()?` and `is_held(…)?`, `src/cli.rs:1886-1888`
> — and `is_held` returns `Err(Error::Io(..))` on a non-`NotFound` open error
> (`src/daemon/seams.rs:393`) and a non-`EWOULDBLOCK` flock error (`:408`). Once the seam this same
> note mandates exists, a stub returns `Err` trivially. Reading it as unreachable retires the fourth
> branch the eighth pass added precisely because it was being dropped), and even
> `NotRunning` passes or fails according to whether the developer's own daemon happens to be running.
>
> Implementing R-13 therefore includes **introducing the seam** — take the probe as a parameter (or a
> trait object) at the `export` call site, rather than calling `daemon_liveness()` directly. Do **not**
> weaken `support_dir()` to make this testable: its non-overridability is a deliberate decision (#7),
> and reversing it to serve a test is the same test-pressure-decides-design failure Cap-8.6 warns
> about. Without a seam the test author writes a machine-state-dependent test or silently drops
> branches — losing the four-branch enumeration that IS this capability.

| Cap-10.2 | Export and import events carry a **matching artifact digest**; the **import** event additionally carries the operator-**requested** scope (export has none to carry — R-9c/AD-5) | unit | R-14, R-14a |
| Cap-11.1 | A **malformed or over-length** `account_uuid` is rejected before a stash name is derived — **not** the empty case, which `src/config/validate.rs:281-284` already rejects on the import parse path (asserting it would be green over unimplemented work) | unit | R-15 |
| Cap-11.2 | The documented **version floor** states which releases cannot read a `[credential]`-bearing artifact (**not** OQ-gated), **and — gated on OQ-5 landing at (b) —** the **current** binary tolerates an unknown non-roster block on the artifact-config parse path. Under OQ-5(a) the tolerance half is not a deliverable at all; do not build it until OQ-5 closes | unit + doc assertion | R-16 (assertable half; see note below the Master Test Plan) |

**Coverage gap this closes** (PRD § 4 M2 criterion): the existing
`the_migration_conflict_policy_default_drives_import_behaviour` builds its target as
`src_config.clone()` (`src/cli.rs:10741`), so every uuid matches by construction. Cap-3.1 explicitly
requires a non-clone target.

> **Cap-11.2 asserts only R-16's *assertable* half — the other half is unfixable, and was previously
> asserted anyway.** *Corrected 2026-08-04 (fourth pass).* This row used to read "The
> `[credential]`-block import failure names the version floor", and the Cap-11.2 scenario said *"When
> it is read by a parser **predating that block**"*. **No test in this tree can satisfy that**: the
> failing parser lives in an already-**shipped** binary, which § 4.9 and § 14 both say we cannot
> patch. And the *current* binary parses `[credential]` fine (`src/config.rs:1395`), while
> forward-tolerance (§ 4.9(b)) is designed to make it not fail at all — so neither side of the
> version boundary can produce the asserted failure. What is assertable today is exactly what the row
> now says: a documented version floor, and — **if OQ-5 lands at (b)** — the current binary's
> tolerance. **OQ-5 decides whether the tolerance half is a deliverable at all**, not merely whether
> it is the whole one: option (a) is a version floor *without* tolerance, under which this row's
> second clause is unsatisfiable. The version-floor clause is ungated. Same treatment Cap-7.7 gets for
> OQ-4. Do not write a test against an unpatched historical binary.

## 12. Architecture Decisions

| # | Decision | Rationale |
|---|---|---|
| AD-1 | `import` does not write the canonical item | C-2; `src/swap.rs` already owns the sequence under the #64 lock |
| AD-2 | **No `format_version` bump** for staleness | Age ≠ supersession, so the field buys a heuristic and creates false assurance (§ 4.2). **Rationale corrected 2026-08-04** — the original cost claim was false; see § 4.2 and PRD § 9 F-3 |
| AD-3 | `rotated` moves inside the `refreshed` variant | Makes the meaningless state unrepresentable rather than merely unprinted |
| AD-4 | R-7 is display-only | Which slot is authoritative is correct today; only its legibility is not |
| AD-5 | **Scope is selected on `import` only; `export` is unchanged** | Export scope is disclosure hygiene, import scope is input validation — only the latter defends against an artifact the attacker minted (§ 4.7). Narrowing export would also break `Payload` losslessness and mask R-16 |
| AD-6 | **Scope is derived from payload presence; the artifact carries no scope field** | A declared scope is attacker-controlled on the unauthenticated `--plaintext` path, which would convert the control into theatre. Also keeps AD-2 coherent — see § 4.7 |
| AD-7 | **Portability is an allowlist, not a denylist** | A denylist rots: the next spawnable key auto-adopts. The allowlist forces the decision at add-time, and R-11d makes it fail closed (§ 4.8) |
| AD-8 | **`claude_bin` is refused unconditionally — no flag overrides it** | It is a capability grant, not config; the refused capability has zero legitimate cross-machine use, and ADR-0030 already documents a local escape hatch. A second confirmation flag was rejected: the error message becomes the exploit instruction |
| AD-9 | **Default import scope stays "everything"** | No narrowing on the scope axis — the flag surface alone leaves today's behaviour intact. It is **not** end-to-end byte-identity: AD-7's allowlist binds regardless of the flag and changes the fresh-target outcome (§ 8, `config adoption`). The safety case for defaulting narrow is absorbed by AD-8 + R-11b — capability keys refused, `kdf_*` held to a monotonic floor — which closes the delta a narrow default would have closed. Explicitly **coupled to AD-8**: if AD-8 were reversed, this must be re-decided first |
| AD-10 | **Flags are `--accounts` / `--settings`** | `--config` is reserved and value-bearing for #24, and semantically wrong — accounts *are* config (§ 4.7) |
| AD-11 | **`[migration].conflict_policy` is non-portable** | Resolved conservatively **over a recorded dissent** (PRD § 9 D-1): two panelists reached opposite conclusions from the same verified facts. Normative, not evidence-forced |

**AD-2 is the one a future reader is most likely to want to revisit** — it is recorded as a
considered decline, not an omission. **AD-9 and AD-11 are the next two**, for different reasons: AD-9
is a live trade-off whose correctness *depends on AD-8 holding*, and AD-11 resolves a genuine
disagreement rather than recording a convergence.

## 13. Quality Requirements

Per PRD § 5: `ImportAdoptionCompleteness` MUST 1.0 (Cap-1.1/1.2), `StalenessDisclosure` MUST 1.0
(Cap-2.1/2.2), `RotationSignalFidelity` MUST 1.0 (Cap-4.1).

## 14. Risks and Open Questions

### Feasibility Summary

| Requirement | Feasible? | Evidence |
|---|---|---|
| R-2 / R-2a | ✅ **Yes**, and cheaper than assumed | Reuses `use`; no new writer (§ 4.1) |
| R-3 | ✅ **Yes** | Its merge-policy demand is withdrawn; feasibility now rides entirely on R-9 + R-11 (§ 4.7 + § 4.8), both feasible below |
| R-4 | ✅ **Yes**, no format change | Unconditional warning is pure output |
| R-4a | ⚠️ **Partly — and the derivable part misses the target case** | `credential_clocks` gives both deadlines from v1 bytes, but supersession is invisible in the blob (§ 4.2) |
| R-5 / R-5a | ✅ **Yes**; 0465 checked for `dead` only | `src/refresh.rs:434-437`; 0465's window ends before the first `dead` line — but its 141-count derives from *event type*, not outcome, so the `no_change` question is **open** and tracked as a pre-landing check on #1004 |
| R-6 | ✅ **Yes** | Local check inside an existing loop |
| R-6a | ✅ **Yes** — OQ-1 resolved 2026-08-06 (refuse on ambiguity); delivered under #1005 | § 4.3; route `apply_enabled` / `apply_remove` through `resolve_target` |
| R-7 | ✅ **Yes**, display-only | § 4.5 |
| R-1 / R-1a / R-8 | ✅ **Yes** | Documents; conventions already exist |
| R-9 / R-9a / R-9c | ✅ **Yes, and free at the format layer** — but the *settings*-axis availability test is **OQ-6-gated** | Every `RawConfig` field is `#[serde(default)]` incl. `account`; `Payload`'s two fields are both emptiable (§ 4.7) |
| R-9b | ✅ **Yes**, and it repairs R-16's roster-only case as a side effect | Narrow struct omits top-level `deny_unknown_fields`; `RawAccount` keeps its own |
| R-9d | ✅ **Yes** | `--config` collision verified at `src/paths.rs:443-444` (`config_dir_with_override` at `:448`) |
| R-10 | ✅ **Yes** — but it is the scope's **only breaking CLI change** | Removal is trivial; the *path* is a product call (OQ-4) |
| R-10a | 🚧 **Blocked on a decision**, not on feasibility | OQ-4 — both paths (hard-remove, deprecate-then-remove) are cheap; the choice is a product call on a shipped flag |
| R-11 / R-11a / R-11b | ✅ **Yes** | Classification is a pure function over the config; no new I/O |
| R-11c | 🚧 **Feasible, but decision-gated** | OQ-7 — classification is free, but *whether `--settings` adopts over an existing local config at all* decides whether R-11c protects a live path or a hypothetical one |
| R-11d | ⚠️ **Yes, but mechanism-dependent** | An exhaustive `match` (compile-error) is preferred and may not fit the current type shape; the completeness-test fallback is weaker but sufficient (§ 4.8) |
| R-11e | ✅ **Yes** | A refusal line per refused key on stdout; Cap-8.5 |
| R-11f | ✅ **Yes** | ADR; conventions exist |
| R-10b | ✅ **Yes** | `PLAINTEXT_WARNING` is a string constant (`src/migration.rs:538`); Cap-7.8 |
| R-12 | ⚠️ **Yes as unlink; NOT as secure erase** | APFS gives no reliable overwrite-in-place. Deliverable must not claim more (§ 4.9) |
| R-13 | ✅ **Yes** for the probe; ⚠️ **the capability needs a seam** | `daemon_liveness()` (`src/cli.rs:1885`) already gives a read-only tri-state answer (plus its `Err` arm); reuse it rather than the best-effort notify at `src/capture.rs:335`. But it takes **no parameters** and resolves `paths::control_socket()` / `paths::daemon_lock()` from `support_dir()`, which is *deliberately* never env-overridable (`src/paths.rs:531-532`, issue #7) — so Cap-10.1 is not hermetically constructible as written (see its note) |
| R-14 / R-14a | ✅ **Yes** | Additive event fields; digest is a pure function over the artifact bytes |
| R-15 | ✅ **Yes**, and cheaper than assumed | Parse-time validation; severity bounded — `stash()` reaches no filesystem path |
| R-16 | 🚧 **Partly — the released-binary half is unfixable** | We cannot patch already-shipped binaries; only the version-floor message and forward-tolerance are in reach (OQ-5) |

### Risk Register

| # | Risk | Sev | Mitigation |
|---|---|---|---|
| RSK-1 | The supplementary expiry check reads as an all-clear, and an operator skips the runbook | **High** | § 4.2 fail-closed rule; warning is unconditional and must not be phrased as a verdict |
| RSK-2 | A-3/A-4 (n=1) get restated downstream as guarantees | **High** | R-1's wording constrains it; cardinality belongs in the verdict line, not a footnote |
| RSK-3 | `--activate` reintroduces auto-switching by default through later drift | Medium | Opt-in only; Cap-1.2 asserts canonical is untouched without it |
| RSK-4 | R-6's warning fires often, revealing A-1 is wrong | Low | If it fires often, that *is* the finding (premortem P5) |
| RSK-5 | Runbook filed but unreferenced | Medium | R-8 requires the command-help reference (premortem P4) |
| RSK-6 | **R-9 ships without R-11.** The operator gets a flag that adopts a code-execution path *on request* — strictly worse than today, where at least no gesture advertises it as supported | **Critical** | Appetite sizes them as **one unit** (PRD § 1b). A delivery plan that splits them has broken the requirement, not resequenced it — this is the single most important sequencing constraint in the scope |
| RSK-7 | The allowlist rots — a new `Config` key is added unclassified and auto-adopts | **High** | R-11d, and it must be the compile-error form if the type shape allows. An unenforced allowlist is a denylist with extra steps |
| RSK-8 | AD-9 (default = everything) is silently retained if AD-8 is later reversed or weakened | **High** | AD-9 records the coupling explicitly: reversing AD-8 requires re-deciding AD-9 **first** |
| RSK-9 | `--shred` is read as secure erase and the operator stops treating the artifact as sensitive | Medium | Cap-9.2 asserts the help text does not claim it; § 4.9 states the APFS limit plainly |
| RSK-10 | R-13's liveness warning becomes unconditional through drift, training dismissal — the RSK-1 failure mode on a second surface | Medium | Cap-10.1 asserts the warning fires **only** when the daemon is live |
| RSK-11 | R-15 is restated downstream as a path-traversal finding, manufacturing a severity the evidence does not support | Medium | PRD R-15 and AC-15 both state the bound and forbid the framing; `stash()`'s call sites were swept and reach no filesystem path |

### Open Questions

- **OQ-1 (blocked R-6a) — RESOLVED 2026-08-06 by the operator: refuse on ambiguity, everywhere.**
  Delivered under issue #1005; see § 4.3 for what routing through `resolve_target` entails, including
  the exit-code change and the retirement of `Error::AccountLabelNotFound`. The question and its
  correction history are kept below because the *framing* was twice defective, and that is the
  reusable lesson — not the answer.

  **The question, as asked** — restated 2026-08-04; the original framing was defective on both halves.
  What is the **single** duplicate-label resolution policy across `use`, `enable`, `disable`, **and
  `remove`**? Design lean is refuse-on-ambiguity; **not settled here** — it changes CLI behaviour
  operators may rely on.

  > The original asked whether `enable`/`disable` should "refuse like `use`, or `use` take first like
  > `enable`", and it was wrong twice. **(a)** It omitted **`remove`**, which resolves a label and then
  > **deletes the keychain stash** (`src/cli.rs:5219-5227`, `src/cli.rs:5195-5211`) — the only one of the four
  > whose first-match-wins outcome is **irreversible**. `use` picks the wrong active account and
  > `enable`/`disable` flips the wrong flag; both are recoverable in one command. A decision taken over
  > the three cheap cases would have settled them and left the expensive one to inherit the answer.
  > **(b)** Its second option was a no-op: refuse-on-ambiguity is what `use` **already ships**
  > (`resolve_target`, `src/use_account.rs:441-457`), so "should `use` take first" was proposing a *regression*
  > as if it were a symmetric alternative. `remove`'s irreversibility should drive the answer.

- **OQ-4 (shapes R-10)** — is `--no-secrets` hard-removed with a strict-usage error naming the
  replacement, or deprecated across one release then removed? It is a **shipped** flag. Lean: hard
  remove, since the repo has one operator and a usage error is self-documenting — but this is a
  product call, not a design one, and R-10a records it as undecided.
- **OQ-5 (bounds R-16)** — we cannot fix already-released binaries, so that half is out of reach
  whatever we decide. The question is the **in-reach** half, and it is a **two-way** choice: (a) a
  documented version floor that makes the failure legible, **or** (b) that floor **plus** forward-
  tolerance on the artifact-config parse path, so the next added block does not re-break it. Lean:
  **(b)** — (a) alone leaves the defect free to recur. *(Restated 2026-08-05: this was written as three
  options — "(a), (b) also …, or both" — but (b)'s "also" already subsumes (a), so "both" was a
  duplicate of (b). AC-16 and Cap-11.2 gate their tolerance clause on this landing at (b).)*
  **STILL OPEN — the ungated half has SHIPPED underneath it (2026-08-09, #1053).** The version
  floor is documented (`migration::CONFIG_BLOCK_VERSION_FLOOR`, ADR-0006 § Status, README
  § Exporting state offline), the import path names it instead of surfacing a bare
  `deny_unknown_fields` line (`Error::MigrationImportConfigRejected`), and the incompatibility is
  pinned by two hand-built historical-parser fixtures (`src/config/load.rs`).
  **The tolerance half is NOT built** — AC-16's ordering says OQ-5 closes *before* any tolerance
  code is written, and the recorded lean above is a lean, not a closure, so #1053 shipped (a)'s
  ungated content and left the choice to the maintainer rather than settling it by writing code.
  What is now decided is only *what the answer costs*: (a) is already paid for, so choosing (b)
  is an increment on a landed floor rather than a fresh build.
  **Three facts not in evidence when this question was written, all of which push toward (b).**
  (i) `Config::render` emits every key **unconditionally**, so a break is universal rather than
  settings-dependent. (ii) The repo still carries **no tags and no releases**, so no *released*
  binary predates the floor — the affected population is source-built binaries, which shrinks the
  unfixable half to approximately nobody and leaves recurrence, which is what (b) addresses, as
  the live half of the question rather than the historical one. (iii) **The recurrence is far
  larger than "blocks" suggests, and this is the decisive correction.** Every `Raw*` struct
  carries `deny_unknown_fields`, so the breaking unit is a **key at any nesting level**: since
  the ADR-0006 freeze the rendered config has gained one new top-level block and **22** new value
  keys (ADR-0006 § Status has the per-block breakdown and how to re-derive it). So the wording
  throughout this question — "unknown *blocks*", "the next added *block*" — understates its own
  scope by more than an order of magnitude, and any tolerance built to the letter of it would
  repair 1 case in 23. **If (b) is taken, it must be key-tolerance, not block-tolerance.**
- **OQ-7 (bounds R-9 / R-11c / AD-11)** — **what does `--settings` do on a target that already has a
  config?** The two things this document says are mutually exclusive. § 4.7 models scope as a
  **lattice meet** over `ImportScope { accounts, settings }` with both-true ≡ today's behaviour, and
  "the flag can only ever *remove*" — under which `--settings` cannot cause an adoption the no-flag
  default does not already perform, and today an existing local config makes `apply_import` discard
  the incoming non-roster blocks entirely (`src/cli.rs:4744-4750`). But § 4.8, R-11c and Cap-7.9 all
  say `--settings` **applies** non-roster config and "would newly allow" the `conflict_policy`
  overwrite AD-11 and the D-1 dissent exist to prevent. Both cannot hold: either (a) `--settings` only
  ever *filters*, in which case R-11c prevents nothing on this path and Cap-7.9 is unsatisfiable
  there, or (b) `--settings` adopts over an existing local config — a genuine behaviour addition,
  which then needs its own conflict semantics and re-opens whether the no-flag default does it too
  (AD-9). **Lean: (b)**, since R-11's whole purpose presupposes an adoption to constrain — but it is
  not decided here, and the implementer of #1046 must not decide it by writing code. *Raised
  2026-08-05 (tenth pass).* **OQ-6 does not cover this** — that asks only whether the settings axis is
  presence-*derivable*, not what the flag *does* once selected.
- **OQ-6 (bounds R-9/R-9a)** — **can the *settings* axis be presence-derived at all?** R-9c's own
  argument says no: every `RawConfig` field is `#[serde(default)]` (`src/config.rs:1377-1396`), so a
  block the operator left at its default is byte-indistinguishable from one the artifact withheld,
  making `available(artifact).settings` effectively always true for a self-minted artifact. The
  *accounts* axis is fine — `[[account]]` entries are present or they are not. So is the deliverable
  (a) accept the asymmetry and define `--settings` as "apply the portable non-roster values that are
  there", with no availability test on that axis, or (b) derive settings-availability from something
  narrower (e.g. blocks differing from default), which re-introduces exactly the guesswork R-9a
  exists to avoid? Lean: **(a)** — it is honest about what presence can tell us, and the allowlist
  (R-11) already bounds what may be adopted, so nothing is lost. **This does not trip R-9's circuit
  breaker** (no declared scope field is required, the operator's flag stays a ceiling), but it must
  be settled before #1046 is implemented, because Cap-7.6 and the `--settings` reporting behaviour
  both depend on the answer. *Raised 2026-08-04 by the fourth review pass, which found R-9a's
  presence test naming the wrong carrier for both axes.*
**The two below are not counted in the header's "five".** *Added 2026-08-05 (tenth pass); the list
ran seven bullets under a header claiming five, with nothing marking the split.* OQ-1/4/5/6/7 each
**gate** a requirement or an issue — nothing may be implemented against them until they are settled.
OQ-2 and OQ-3 carry leans that are already good enough to build on and block nothing; they are
recorded so the call is visible, not because it is pending.

- **OQ-2 (shapes R-2)** — should `--activate` exist at all in the first increment, or should the
  reported-and-named-command form ship alone and earn it? Lean: ship (b) alone first.
- **OQ-3 (bounds R-1)** — is a single non-revocation worth recording at all, given A-3's n=1? Lean:
  yes — a recorded n=1 with stated cardinality beats an undocumented belief, which is what #262 left.

## 15. Glossary

| Term | Meaning |
|---|---|
| **Canonical item** | `Claude Code-credentials` — the single keychain item Claude Code reads |
| **Stash** | `Sessiometer/<account_uuid>` — Sessiometer's per-account parking slot |
| **Superseded** | A refresh token replaced by rotation at the endpoint. Invisible in the blob; distinct from **expired** |
| **Adoption** | Making an imported credential the one Claude Code actually reads |

## 16. Requirement-to-Track Coverage Matrix (forward)

| Req | Design § | Capability | Status |
|---|---|---|---|
| R-1, R-1a | 4.6 | — (document) | covered |
| R-2 | 4.1 | Cap-1.1 | covered |
| R-2a | 4.1 | Cap-1.2 | covered |
| R-3 | 4.7 + 4.8 | Cap-7.x, Cap-8.x | **now covered** — was "NOT covered" while R-3 demanded a merge policy; the demand is withdrawn and replaced by scope × class (PRD § 3, R-3) |
| R-4 | 4.2 | Cap-2.1, Cap-2.2 | covered |
| R-4a | 4.2 / AD-2 | Cap-2.3 | covered (resolved as a decline) |
| R-5 | 4.4 | Cap-4.1 (type), **Cap-4.2 (all three rendered lines)** | covered — Cap-4.1 alone is necessary but not sufficient; see § 5 |
| R-5a | 4.4 | — (verification, **partly done** — `dead` checked, `no_change` open, #1004) | covered |
| R-6 | 4.3 | Cap-3.1 | covered |
| R-6a | 4.3 | Cap-3.2 | covered — OQ-1 resolved, delivered under #1005 |
| R-7 | 4.5 | Cap-5.1 | covered |
| R-9 | 4.7 | Cap-7.1 (`--accounts`), **Cap-7.9 (`--settings` — the mirror)**, Cap-7.2 (default), Cap-7.6, **Cap-7.10 (roster-less `--accounts`)**, **Cap-7.11 (both flags together)** | **partly decision-gated** (OQ-6 — the `--settings` availability report; OQ-7 — Cap-7.9's target state) |
| R-9a | 4.7 / AD-6 | Cap-7.3 | **partly decision-gated** (OQ-6 — the settings-axis presence test) |
| R-9b | 4.7 | Cap-7.4 | covered |
| R-9c | 4.7 / AD-5 | Cap-7.5 | covered |
| R-9d | 4.7 / AD-10 | — (naming; asserted by Cap-7.1's flag surface) | covered |
| R-10 | 4.7 | Cap-7.7 | covered — **corrected 2026-08-04**: was Cap-7.5, which asserts only that `export` grows no *scope* flag (R-9c) and never reaches `--no-secrets`. R-10 is the scope's only breaking CLI change; it needs its own capability |
| R-10a | — | — | **decision-gated** (OQ-4) — see the Cap-7.7 note below this table |
| R-10b | 4.9 | Cap-7.8 | covered — **corrected 2026-08-04**: was Cap-9.2, which asserts the *shred* help text makes no secure-erase claim (R-12). Adjacent wording concern, different string |
| R-11 | 4.8 / AD-7 | Cap-8.4, Cap-8.6 (default-deny over an arbitrary key), **Cap-8.7 (the allowlist binds with no flag at all — the shipped, default path)**; Cap-8.1 … Cap-8.3 cover the named carve-outs | covered |
| R-11a | 4.8 / AD-8 | Cap-8.1 (with `--settings`), **Cap-8.7 (no flag, fresh target)** | covered |
| R-11b | 4.8 | Cap-8.2 | covered |
| R-11c | 4.8 / AD-11 | Cap-8.3 | covered (over a recorded dissent) — **and OQ-7-gated**: Cap-8.3 pins a fresh target, which is the only state where adoption happens today. If OQ-7 resolves to (a) (`--settings` never adopts over an existing config), R-11c protects a path that cannot be reached and the dissent D-1 records is moot; if (b), it protects a path this scope newly opens |
| R-11d | 4.8 | Cap-8.4 | covered |
| R-11e | 4.8 | Cap-8.5 | covered |
| R-11f | 4.8 | — (ADR deliverable) | covered |
| R-12 | 4.9 | Cap-9.1, Cap-9.2 | covered |
| R-13 | 4.9 | Cap-10.1 | covered |
| R-14, R-14a | 4.9 | Cap-10.2 | covered |
| R-15 | 4.9 | Cap-11.1 | covered |
| R-16 | 4.9 | Cap-7.4, Cap-11.2 | **partly decision-gated** (OQ-5) |
| R-8 | 4.6 | — (document) | covered |

> **Cap-7.7 covers R-10 only, and presumes OQ-4 resolves to hard-remove.** *Corrected 2026-08-04
> (second pass); relocated out of a table cell (third pass) — block quotes do not nest inside table
> cells, so GitHub rendered a literal `>` and crammed this whole paragraph into one column.* Cap-7.7
> previously also claimed R-10a, contradicting this table's own R-10a row and § 16b, which both
> record R-10a as decision-gated with no capability. R-10 (*the flag goes away*) is decided and
> assertable now; R-10a (*by which path* — hard-remove vs deprecate-then-remove) is **not**, and a
> capability cannot assert an undecided requirement. Cap-7.7's strict-usage-error form is the
> hard-remove branch: **if OQ-4 resolves to deprecate-then-remove, Cap-7.7 must be re-derived** to
> assert a deprecation warning and a zero exit for at least one release, and **PRD AC-10 must be
> re-derived with it** (AC-10 carries the same OQ-4 caveat). Do not implement Cap-7.7 as written
> until OQ-4 is closed.

## 16b. Backward-Coverage Matrix

Every capability traces to a requirement: Cap-1.x→R-2/R-2a/AC-2a, Cap-2.x→R-4/R-4a, Cap-3.x→R-6/R-6a,
Cap-4.1→R-5, Cap-5.1→R-7, Cap-6.1→C-3, Cap-7.1-7.6→R-9/R-9a-c + R-16 (Cap-7.4); R-9d has no capability of its own, **Cap-7.7→R-10**,
**Cap-7.8→R-10b**, **Cap-7.9→R-9 (the `--settings` mirror)**, Cap-8.x→R-11/R-11a-e (Cap-8.6→R-11, **Cap-8.7→R-11/R-11a**), Cap-9.x→R-12, Cap-10.x→R-13/R-14/R-14a,
Cap-11.x→R-15/R-16, **Cap-4.2→R-5**, **Cap-7.10→R-9**, **Cap-7.11→R-9/R-9a**. **No orphan capabilities — all 35.**
*Corrected 2026-08-05 (twelfth pass); this enumerated 32 and omitted exactly the two capabilities
added to close the eleventh pass's findings, so the sentence asserting completeness was the one
place the additions did not reach.*

> **This matrix checks only one direction, and that is why it missed two gaps (corrected 2026-08-04).**
> "No orphan capabilities" asks *does every capability trace to a requirement* — it can never detect a
> requirement whose named capability does not actually **assert its acceptance criterion**. Two did
> not: R-10 was mapped to Cap-7.5, which asserts `export` grows no *scope* flag and never reaches
> `--no-secrets` at all; and R-10b was mapped to Cap-9.2, which asserts the *shred* help text makes no
> secure-erase claim. Both mappings passed this matrix and § 16's coverage column, because both are
> real capabilities tracing to real requirements — just not to *those* requirements. Cap-7.7 and
> Cap-7.8 close the gap. **The forward direction — does each requirement's capability assert that
> requirement's AC — is the one that catches this, and it is a manual read, not a matrix.**

**Six** requirements are covered by something **other than a capability**, recorded explicitly so
their absence from the Cap-list does not read as a coverage gap:

| Requirement | Covered by | Note |
|---|---|---|
| R-1, R-1a | a document (`docs/findings/0262-*`) | § 16 row reads `— (document)` |
| R-5a | a verification, **partly performed** | § 16 row reads `— (verification, partly done)`; the `dead` half is checked, the `no_change` half is open and tracked on #1004 |
| **R-8** | a document (the migration runbook) | § 16 row reads `— (document)` — **see the warning below** |
| R-9d | nothing of its own | flag naming has no behaviour; asserted incidentally by Cap-7.1's flag surface |
| R-11f | a document (the portability ADR) | — |

**R-10a** has neither, because it is undecided (OQ-4).

> **One capability has no spec scenario: Cap-7.7** — 34 of the 35 are pinned by a scenario in
> `docs/specs/`. This one is left unpinned **deliberately**, because its assertion is the hard-remove
> branch of the still-open OQ-4 (see the R-10 row in § 16). Writing the scenario now would pin the
> undecided outcome in the place a test author reads first. Close OQ-4, then add it to
> `docs/specs/artifact-lifetime.feature.md`, which already carries R-10b. *Recorded 2026-08-04 — a
> gap named is not a gap fixed; this one is deferred on purpose and the reason is the decision, not
> the effort.*

> **Corrected 2026-08-04 — this paragraph said "two", and the miscount hid a real gap.** It named
> R-11f and R-9d only, omitting R-1/R-1a, R-5a and — the one that matters — **R-8**. R-8 is the
> migration runbook: the highest-blast-radius surface for this incident class, because it is the one
> artifact a human reads and follows step by step. It has no capability, and until this revision its
> AC placed no constraint on which adoption command the sequence names.
>
> That combination is exactly why R-8 carried the **seventh and last** surviving instance of the
> `use --force` correction (PRD R-8, AC-8). Nothing gated it and nothing accounted for the fact that
> nothing gated it. A requirement covered by a deliverable is not a problem; a requirement covered by
> a deliverable that the coverage accounting **does not list** is, because the manual read this
> section calls for has no complete list to read against.

## 17. Why this design is `draft`, not locked

1. ~~**R-3 has no design.**~~ **Resolved 2026-08-04.** This document previously declined to design the
   non-roster merge policy, on the grounds that choosing it silently was the failure class the PRD's
   provenance warning exists to prevent. That reasoning was right, and the resolution turned out to be
   **not to author the policy at all**: R-3's demand was mis-shaped, because a per-block win/lose rule
   cannot express *"the operator may choose this one, and may never choose that one."* §§ 4.7 + 4.8
   decompose it into scope × class, and § 16 now reads `covered`. The ADR this bullet called for
   still exists as R-11f — but it records the **portability classification**, not a merge policy.
2. **OQ-1 gates R-6a**, and its framing was **corrected** — it now spans `remove`, whose
   first-match-wins is the only irreversible one, and it no longer offers a regression as an
   alternative. OQ-2 shapes R-2's first increment.
3. **AD-2 declines a format bump** on reasoning that a future reader may weigh differently — and its
   stated rationale was **corrected 2026-08-04** after the original cost argument was falsified (§ 4.2,
   PRD § 9 F-3). The conclusion is unchanged; a reader comparing this document against an earlier copy
   should know the change was a correction, not a softening.
4. **AD-11 rests on a recorded dissent** (PRD § 9 D-1), not a convergence. Two panelists reached
   opposite conclusions from the same verified facts; the resolution is conservative and defensible but
   not evidence-forced.
5. **OQ-4 (R-10's deprecation path), OQ-5 (R-16's deliverable), OQ-6 (whether the settings axis
   can be presence-derived at all) and OQ-7 (what `--settings` does on a target that already has a
   config) are open.** OQ-6 gates #1046's `--settings` reporting behaviour and Cap-7.6, and **OQ-7
   gates the same item's `--settings` semantics plus R-11c, Cap-7.9 and Cap-8.3** — both must close
   before that item is implemented. Otherwise neither blocks
   implementation of anything else, and both are product calls rather than design ones.
6. **AC-2 was found defective and corrected** (PRD AC-2a): the planned "run `use <label>`" guidance is
   a provable no-op for the active account, which would have reproduced the original failure through
   its own remediation. § 4.1's choice to reuse `use` stands; the named invocation is now
   `use --force <label>`, and **§ 4.1 has been re-derived against the correction** — including its
   option table and the `--activate` sugar, which inherits the same requirement.

   > **The correction initially reached § 4.1 only — swept repo-wide 2026-08-04.** Six downstream
   > surfaces still carried the falsified unqualified form after § 4.1 was fixed, and the most
   > damaging was **Cap-1.1**, the sole capability gating R-2: it asserted the report "names `use`",
   > which a verbatim implementation satisfies with the no-op form. The gating test encoded the very
   > defect it exists to catch. Also corrected: § 5's building-block row, both § 6 runtime-view
   > arrows, and two scenarios in `docs/specs/import-credential-adoption.feature.md` (one asserted
   > only that *some* command was named; the other dropped the currently-active precondition without
   > which the scenario passes trivially on a parked account). Cap-1.1 now asserts on the `--force`
   > token specifically. A **seventh** site sat outside this document — the PRD's R-8 runbook, the
   > most dangerous of all because it is operator-facing with no capability gating it (PRD § 9).
   > **A correction applied at the site that raised it is not a correction —
   > the claim has to be swept, not the named site.**
