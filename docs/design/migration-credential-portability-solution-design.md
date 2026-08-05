# Solution Design: Migration Credential Portability

**Requirements**: `docs/requirements/migration-credential-portability.md`
**Status**: `draft` — three requirements are decision-gated and are surfaced, not settled, here.
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

> **Amended 2026-08-04 for R-9 … R-16.** This section, § 5 and § 7 were written against R-1 … R-8 and
> are brought forward here. If a statement in them conflicts with § 8, § 11, § 12, § 14 or § 16, those
> sections are newer and win.

**In scope**: `sessiometer import` (`import()` in `src/cli.rs`), `apply_import()`, the
`rotated` field in `classify()` (`src/refresh.rs:432-472`), `status`'s EXPIRY provenance
(`src/daemon/snapshot_build.rs:40-58`), duplicate-label resolution across `use` / `enable` / `disable` / `remove`,
and two documents from the original scope (`docs/findings/0262-*`, a migration runbook).

**Also in scope, added by the R-9 … R-16 amendment**: `sessiometer export` (scope selection R-9, the
`--no-secrets` removal R-10, and the source-side liveness probe R-13); **config adoption** governed by
the portability allowlist (R-11, § 4.8); migration **observability** fields (R-14); roster input
validation (R-15); and a **third** document — the portability-classification ADR (R-11f, issue #1003).

**Out of scope**: the artifact envelope, and the KDF's *construction, parameters and envelope
security* (#147); cross-platform migration (#965/#980); the swap decision loop; `[refresh]` cadence
and the keep-warm gate (#468).

> **The KDF boundary was NARROWED, and § 4.8 depends on the narrowing.** It previously read
> "Encryption, KDF, and envelope security" and excluded KDF outright. **R-11b now governs whether an
> artifact's `[migration].kdf_*` may be ADOPTED on import** — the monotonic-floor rule that closes the
> 8 KiB / 1-iteration downgrade. How the KDF is *built* stays #147's; whether an incoming one may be
> *adopted* is decided here. An executor who reads the old exclusion and skips R-11b/Cap-8.2 leaves
> that downgrade open.

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
> Ok(GateOutcome::AlreadyActive); }` (`SwapTarget::resolve`'s `AlreadyActive` short-circuit in `src/use_account.rs`) — a comparison of **service
> names**, never of contents. The committed test
> `already_active_without_force_is_a_noop_success_with_zero_writes` asserts exactly the
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

**Alternatives considered:**

| Option | Verdict |
|---|---|
| (a) `import` writes canonical directly | **Rejected** — violates C-2; races the reconciler; duplicates a proven 5-step sequence |
| (b) `import` reports + names `use --force` | **CHOSEN** — smallest change, no new writer, no lock surface. The unqualified `use` variant of this option is **not viable** (see correction above) |
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
> `deny_unknown_fields`, so an added payload field is not a breaking change at all — it is nearly free.
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
   sequence. This is the MUST and it ships without touching the format.
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
  entry (`apply_enabled()` in `src/cli.rs`). Both behaviours are defensible in isolation; having both is not.

**The policy spans FOUR commands, not three.** `remove` also resolves a label first-match, and it
**deletes the keychain stash** — the only one of the four whose first-match behaviour is
*irreversible*. Settling `use` / `enable` / `disable` and letting `remove` inherit the result gets the
argument backwards: `remove`'s irreversibility is the strongest reason to pick a policy at all, so it
belongs in the decision rather than downstream of it (PRD § R-6a; OQ-1 is stated over all four).

**Options for R-6a**, surfaced for decision (§ 14 Open Questions, OQ-1):

| Option | Consequence |
|---|---|
| (i) `enable`/`disable`/`remove` refuse like `use` | Consistent and safe; breaks any operator muscle-memory relying on first-match. Strongest for `remove`, where first-match deletes the wrong stash |
| (ii) `use` takes first like `enable` | Consistent; but silently switching to the *wrong account* is a credential-level mistake, not a config one — and it would make `remove` silently delete the wrong stash, which is unrecoverable |
| (iii) All four accept an `--account-uuid` disambiguator; label path refuses | Most explicit; largest surface |

**Design lean: (i)** — it moves the *cheaper* commands toward the *safer* one. Refusing an `enable`
costs a re-run; silently switching credentials costs an incident; silently removing the wrong stash
costs the credential outright. Not chosen here.

### 4.4 `rotated` telemetry (R-5, R-5a) — **suppress on non-`refreshed`, treat as a contract change**

`classify()` computes `rotated` before the outcome is known
(`src/refresh.rs:434-437`), so a `dead` outcome — which sets `after_rt = Some("")` — always yields
`rotated=true`. The field is true-by-construction on every dead line.

**Chosen**: make `rotated` structurally unrepresentable on non-`refreshed` outcomes rather than
merely omitted at the log-formatting layer — carry it *inside* the `refreshed` variant of
`RefreshOutcome` so the type system prevents the meaningless combination, and the emitter cannot
reintroduce it.

**Treat as a log-format contract change** (R-5a): `docs/findings/0465-*` derives a published headline
(`141 rotated=true, 0 rotated=false`) from this field. **0465 is verified clean** — its window ends
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
field. On a `--plaintext` export nothing is authenticated (`resolve_encryption()` in `src/cli.rs`), so a declared
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
over the roster (`apply_import()`'s roster loop in `src/cli.rs`) with secrets keyed by uuid (secrets keyed by uuid in the same fn), so a secret with no roster
entry is unreachable code — and R-10 removes the opposite case (`--no-secrets`) from the product.
A flat three-axis model would advertise two states the data model cannot hold.

**Resolution is a lattice meet**: `effective = available(artifact) ∧ permitted(flags)`. The flag can
only ever *remove*. `import --accounts` against a settings-bearing artifact ignores those settings
regardless of what the artifact says; `import --settings` against a roster-only artifact reports
"artifact contains no configuration" rather than erroring or silently no-op'ing.

**`--accounts` narrow-parses** (R-9b) rather than parse-then-filter. This is not an optimization:
`RawConfig` carries `deny_unknown_fields` (`src/config.rs:1378`), which never fires on blocks outside
the parse path — so narrow-parse *additionally repairs* backward-import for roster-only artifacts
(§ 4.9, R-16). The narrow struct omits `deny_unknown_fields` at the top level while `RawAccount`
(`:1399`) keeps its own, so per-account strictness is preserved and only unknown *blocks* are ignored.

**Export is unchanged** (R-9c), and the asymmetry is principled: **export scope is disclosure hygiene;
import scope is input validation.** Only the latter defends against a hostile artifact, because the
attacker mints the export. Narrowing export would also be actively harmful — since every block
defaults, an omitted block is indistinguishable from a default-valued one, so a receiver cannot tell
*withheld* from *stock*; it would break `Payload`'s losslessness invariant (`:203-206`), make the
artifact irreversible, and mask R-16's break behind a flag.

**Naming** (R-9d): `--accounts` / `--settings`. `--config` is doubly unavailable — reserved and
value-bearing for issue #24's directory-override ladder (`src/paths.rs:439`), and semantically wrong,
since `account` is a `RawConfig` field and `sessiometer config show` prints the roster. `--accounts`
is the vocabulary `IMPORT_USAGE` already uses ("rehydrate **accounts**", `IMPORT_USAGE` in `src/cli.rs`).

**Default stays everything** — today's behaviour byte-for-byte. The safety argument for defaulting to
`--accounts` is real but is **absorbed by § 4.8**: with the capability keys refused unconditionally,
the residual delta is a KDF downgrade, and changing a shipped command's default costs more than that
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
| `[login].claude_bin`, `[refresh].claude_bin` | **CapabilityGranting** — never adopted | Resolution absolutizes against cwd and accepts any `is_file()`, with no allowlist, no signature, and deliberately no symlink resolution (`src/paths.rs:773-807`); the refresh tick then spawns it (`resolve_binary()` in `src/refresh_tick.rs`, spawned via `Command::new(binary)` in `src/isolated_spawn.rs`). Adoption is arbitrary code execution, unattended, on a timer. |
| `[migration].conflict_policy` | **MachineBound** | Encodes the *target* operator's decision. Today an artifact cannot overwrite it; `--settings` would newly allow it — not for the import that adopts it (`resolve_import_overwrite` reads local first, `resolve_import_overwrite()` in `src/cli.rs`) but for every one after. Resolved conservatively over a recorded dissent (PRD § 9 D-1). |
| `[migration].kdf_*` | **Portable, monotonic floor** | Adopt only if `incoming >= local`. A fleet may standardize upward; nothing may downgrade (`src/config.rs:981-988`). |

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

**R-12 — artifact lifetime.** `import` reads the file and leaves it (`import()` in `src/cli.rs`);
`PLAINTEXT_WARNING` advises deleting it with **no mechanism**, and only on the `--plaintext` path,
while an encrypted artifact is still a live-credential file behind one passphrase — which is the whole
argument, and it needs no further premise. **A stronger claim was withdrawn**: this paragraph used to
add that under § 4.7 the *typical* artifact becomes roster-only. It cannot. `gather_payload()` sets
`config_toml = config.render()` unconditionally and `render()` emits `[credential]` unconditionally,
while R-9c/AD-5 forbid an export-side narrowing flag — so no tool-minted artifact is ever roster-only,
and artifact scope is not derivable from block presence for anything this tool produces.
Design: `import --shred` unlinks the source after a successful apply.
**Stated honestly**: on APFS, overwrite-in-place does not reliably destroy the prior extent, so this is
`rm` with intent, not forensic erasure. It must be documented as such — claiming secure-erase we do not
deliver is the same false-assurance failure AD-2 declines.

**R-13 — source-side prevention.** The design's own thesis is that the hazard is *"not detectable at
the target — only preventable at the source"*, and there is currently **zero** source-side
implementation: `export` never asks whether this machine's daemon is running (`export()` in `src/cli.rs`).
Liveness is locally probeable via the existing control socket (`notify_daemon_roster_reload()` in `src/capture.rs`, which already opens `paths::control_socket()` from a non-daemon verb). Design:
`export` probes, and warns when the daemon is live — the one moment the operator can still act.
Warning **only** when live, never unconditionally, so it does not train dismissal (RSK-1's failure
mode).

**R-14 — correlation.** `Event::Export` / `Event::Import` gain a sha256 artifact digest and the
operator-**requested** scope (never the artifact's claimed scope, per R-9a). Both fit the existing
aggregate-only redaction discipline (`src/observability.rs:1426-1442`) — no label, no token, no email.

**R-15 — input validation.** `account_uuid` is unvalidated and interpolated into a keychain service
name (`src/config.rs:370-372`). **Severity is bounded and the bound is verified**: `stash()` reaches no
filesystem path, and keychain service names are opaque strings rather than hierarchical paths, so
`Sessiometer/../x` is a literal name and not a traversal. Residue is namespace squatting, an empty uuid
yielding the bare prefix, and unbounded length. Validate on parse; **do not file or fix this as a
traversal**, which would manufacture a severity the evidence does not support.

**R-16 — the `[credential]` backward-import break.** `RawConfig` carries `deny_unknown_fields`, so a
binary built before commit `6fe3457` (2026-07-29) **rejects** an artifact carrying `[credential]` — a
block added 26 days after ADR-0006 froze v1, and never tracked. The asymmetry worth naming: **we cannot
fix already-released binaries.** So the design is forward-looking in two parts — (a) document the
version floor and make the failure legible rather than a bare parse error, and (b) **stop it
recurring**, by making the artifact-config parse path tolerant of unknown blocks so the *next* block
added does not re-break it. R-9b's narrow-parse delivers (b) for the roster-only case as a side effect;
the full-artifact case needs the same treatment deliberately.

## 5. Building Blocks

| Block | Change | Requirements |
|---|---|---|
| `src/cli.rs::import` | report non-adoption + name `use --force <label>`; optional `--activate` (which must force too) | R-2 |
| `src/cli.rs::apply_import` | duplicate-label collision check; staleness warning emission | R-4, R-6 |
| `src/refresh.rs::classify` | move `rotated` inside the `refreshed` variant | R-5 |
| `src/daemon/snapshot_build.rs` + status render | provenance legibility | R-7 |
| `src/use_account.rs` / `src/cli.rs::apply_enabled` | consistency per OQ-1 | R-6a |
| `docs/findings/0262-*.md` | new | R-1, R-1a |
| `docs/*` runbook + command help | new | R-8 |
| `src/cli.rs::import` scope flags (`--accounts` / `--settings`) + narrow-parse | R-9, R-9a, R-9b |
| `src/cli.rs::export` — remove `--no-secrets`; add the daemon-liveness probe | R-10, R-10a, R-13 |
| **new** portability-allowlist module — classify every `Config` key, fail closed on an unclassified one | R-11, R-11a … R-11e |
| `src/cli.rs::import` — `--shred` | R-12 |
| `src/migration.rs::PLAINTEXT_WARNING` — re-word for the `--shred` mechanism | R-10b |
| `src/observability.rs` — artifact-identity fields on `Event::Export` / `Event::Import` | R-14, R-14a |
| roster input validation on `account_uuid` before `stash()` interpolation | R-15 |
| a new ADR under `docs/adr/`, numbered on creation — the config-portability classification | new | R-11f (issue #1003) |

**Untouched by design**: `src/swap.rs` (reused, not modified).

> **`src/migration.rs` is no longer untouched.** This row previously listed it, on the grounds that
> C-1 is preserved and no format change occurs. Both remain true, but **R-10b / Cap-9.2 require
> re-wording `PLAINTEXT_WARNING`**, which is defined there. The file's *format* is untouched; the file
> is not. An executor on #1049 reading the old row would ship `--shred` and leave the warning still
> advising a deletion the tool now performs.

## 6. Runtime View — the corrected migration flow

```
SOURCE (A)                          TARGET (B)
  stop daemon        ── R-8 ──►  (source no longer rotates)
  export  ─────────── artifact ──►  import
                                      ├─ writes stashes + roster
                                      ├─ WARNS: source must not refresh after export   (R-4)
                                      ├─ WARNS: duplicate label created, if any        (R-6)
                                      └─ REPORTS: active account staged, run `use --force <label>` (R-2)
                              use --force <label>  ──► swap engine (#64 lock) ──► canonical
                                    status ──► EXPIRY with legible provenance          (R-7)
```

The failure on 2026-07-31 was the **first arrow** never happening: A kept its daemon running and
refreshed 4 minutes before B replayed.

## 7. Deployment View

No new processes and no migration of on-disk state; no `format_version` change (§ 4.2). All code
changes are in the existing CLI binary.

**Two corrections from the R-9 … R-16 amendment**, because this section previously said "no new
processes, files, or IPC … and two new markdown documents":

- **Three** new documents, not two — R-11f adds the portability-classification ADR (issue #1003).
- **R-13 adds new IPC.** `export` today never opens a socket; the daemon-liveness probe makes it a
  control-socket client. The socket and its client pattern already exist
  (`notify_daemon_roster_reload()` in `src/capture.rs`), so this is a new *caller* on an existing
  path rather than a new transport — but "no new IPC" is no longer accurate for `export`.

## 8. Interface Contracts

| Surface | Change | Compatibility |
|---|---|---|
| `import` stdout | new warning + report lines | additive; C-3 forbids any credential in them |
| `import` flags | optional `--activate <label>` | additive, opt-in |
| refresh log line | `rotated` absent on non-`refreshed` | **contract change**; consumer 0465 verified unaffected |
| artifact format | **none** | v1 preserved (C-1, C-4) |
| `import` flags | `--accounts`, `--settings`, `--shred` | additive, opt-in; **default unchanged** (AD-9) |
| `export` flags | **`--no-secrets` REMOVED** | **breaking** — the only breaking CLI change in this scope. Path undecided (OQ-4) |
| `export` stdout | daemon-liveness warning when the local daemon is live | additive; conditional, never unconditional (R-13) |
| `import` stdout | per-key refusal lines from the portability allowlist | additive; C-3 applies |
| config adoption | non-portable keys silently dropped → **now refused and reported** | **behaviour change** on the fresh-target path, where the artifact's config was previously adopted wholesale |
| `Event::Export` / `Event::Import` | `+ artifact_sha256`, `+ scope` | additive; aggregate-only redaction preserved |

## 9. UX Architecture / 10. UI Strategy

**n/a** — CLI and daemon only. No menu-bar surface is touched. Recorded as an explicit negative so
its absence reads as by-design.

## 11. Crosscutting Concepts

**Security.** Every warning, report, and findings note is a credential-adjacent surface. C-3 is
enforced by the existing redaction test; new output lines must be covered by it or an equivalent.

**Concurrency.** § 4.1's central choice is *not to add a writer*. The only lock interaction is the
one `use` already performs.

### Master Test Plan

| Cap | Capability under test | Type | Requirement |
|---|---|---|---|
| Cap-1.1 | Import of the target's **active** account reports non-adoption and names `use --force <label>` — **not** bare `use`, which is a provable no-op on the active account (PRD AC-2a) | unit (`apply_import` outcome) | R-2 |
| Cap-1.2 | Import adds no canonical writer — canonical byte-unchanged across import | integration | R-2a, C-2 |
| Cap-2.1 | Every credential-bearing import emits the staleness warning | unit | R-4 |
| Cap-2.2 | Warning fires even when derived deadlines are unreadable (fail-closed) | unit | R-4, P2 |
| Cap-2.3 | An already-expired artifact additionally reports expiry | unit | R-4a |
| Cap-3.1 | Same-label/different-uuid import warns — with a target that is **not** a clone of the source | unit | R-6 |
| Cap-3.2 | `use` / `enable` / `disable` / **`remove`** agree on duplicate-label resolution | unit | R-6a |
| Cap-4.1 | `rotated` is unrepresentable on `dead` / `error` | unit (type-level) | R-5 |
| Cap-5.1 | `status` distinguishes canonical-sourced from stash-sourced EXPIRY | unit | R-7 |
| Cap-6.1 | No import output line contains a token or email | unit (extend existing) | C-3 |
| Cap-7.1 | `import --accounts` applies roster + secrets and **no** non-roster block | unit (`apply_import` outcome) | R-9 |
| Cap-7.2 | Default `import` (no scope flag) is byte-identical to today's behaviour | integration (regression) | R-9, AD-9 |
| Cap-7.3 | A scope flag can only narrow — an artifact cannot widen it | unit | R-9a |
| Cap-7.4 | A roster-only artifact round-trips through narrow-parse, incl. under an unknown block — a **hand-crafted or hostile** artifact, since this tool cannot mint one | unit | R-9b, R-16 |
| Cap-7.5 | `export` exposes no config/roster narrowing flag | unit (usage assertion) | R-9c |
| Cap-7.6 | `import --settings` on a roster-only artifact reports "no configuration", not an error — same hand-crafted-artifact case as Cap-7.4, **not** a shape `export` can produce | unit | R-9 |
| Cap-7.7 | `export --no-secrets` is rejected with a strict-usage error naming what replaced it | unit (usage assertion) | **R-10** |
| Cap-8.1 | `[refresh].claude_bin` from an artifact is **never** written to the target config, even with `--settings` | integration | R-11a |
| Cap-8.2 | A weaker incoming `kdf_*` is refused; a stronger one is accepted | unit | R-11b |
| Cap-8.3 | `[migration].conflict_policy` is not adopted | unit | R-11c |
| Cap-8.4 | **Adding a `Config` key without a portability classification fails the build** | compile-fail / completeness test | R-11d |
| Cap-8.5 | Every refusal is reported on stdout | unit | R-11e |
| Cap-9.1 | `import --shred` removes the source artifact after a successful apply | integration | R-12 |
| Cap-9.2 | Shred is not claimed as secure erase in help or docs | unit (text assertion) | R-12 |
| Cap-10.1 | `export` warns **only** when the local daemon is live | unit | R-13 |
| Cap-10.2 | Export and import events carry a matching artifact digest + requested scope | unit | R-14, R-14a |
| Cap-11.1 | A malformed / empty `account_uuid` is rejected before a stash name is derived | unit | R-15 |
| Cap-11.2 | The `[credential]`-block import failure names the version floor | unit | R-16 |

**Coverage gap this closes** (PRD § 4 M2 criterion): the existing
`the_migration_conflict_policy_default_drives_import_behaviour` builds its target as
`src_config.clone()` (`the_migration_conflict_policy_default_drives_import_behaviour`), so every uuid matches by construction. Cap-3.1 explicitly
requires a non-clone target.

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
| AD-9 | **Default import scope stays "everything"** | Today's behaviour byte-for-byte. The safety case for defaulting narrow is absorbed by AD-8 — with capability keys refused, the residual delta is a KDF downgrade. Explicitly **coupled to AD-8**: if AD-8 were reversed, this must be re-decided first |
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
| R-4 | ✅ **Yes**, no format change | Unconditional warning is pure output |
| R-4a | ⚠️ **Partly — and the derivable part misses the target case** | `credential_clocks` gives both deadlines from v1 bytes, but supersession is invisible in the blob (§ 4.2) |
| R-5 / R-5a | ✅ **Yes**; 0465 verified unaffected | `src/refresh.rs:434-437`; 0465 window ends before the first dead line |
| R-6 | ✅ **Yes** | Local check inside an existing loop |
| R-6a | 🚧 **Blocked on a decision**, not on feasibility | OQ-1 |
| R-7 | ✅ **Yes**, display-only | § 4.5 |
| R-1 / R-1a / R-8 | ✅ **Yes** | Documents; conventions already exist |
| R-9 / R-9a / R-9c | ✅ **Yes, and free at the format layer** | Every `RawConfig` field is `#[serde(default)]` incl. `account`; `Payload`'s two fields are both emptiable (§ 4.7) |
| R-9b | ✅ **Yes**, and it repairs R-16's roster-only case as a side effect | Narrow struct omits top-level `deny_unknown_fields`; `RawAccount` keeps its own |
| R-9d | ✅ **Yes** | `--config` collision verified at `src/paths.rs:439` |
| R-10 | ✅ **Yes** — but it is the scope's **only breaking CLI change** | Removal is trivial; the *path* is a product call (OQ-4) |
| R-11 / R-11a / R-11b / R-11c | ✅ **Yes** | Classification is a pure function over the config; no new I/O |
| R-11d | ⚠️ **Yes, but mechanism-dependent** | An exhaustive `match` (compile-error) is preferred and may not fit the current type shape; the completeness-test fallback is weaker but sufficient (§ 4.8) |
| R-11f | ✅ **Yes** | ADR; conventions exist |
| R-12 | ⚠️ **Yes as unlink; NOT as secure erase** | APFS gives no reliable overwrite-in-place. Deliverable must not claim more (§ 4.9) |
| R-13 | ✅ **Yes** | Control socket already exposes liveness (`notify_daemon_roster_reload()` in `src/capture.rs`, which already opens `paths::control_socket()` from a non-daemon verb) |
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

- **OQ-1 (blocks R-6a)** — **restated 2026-08-04; the original framing was defective on both halves.**
  What is the **single** duplicate-label resolution policy across `use`, `enable`, `disable`, **and
  `remove`**? Design lean is refuse-on-ambiguity; **not settled here** — it changes CLI behaviour
  operators may rely on.

  > The original asked whether `enable`/`disable` should "refuse like `use`, or `use` take first like
  > `enable`", and it was wrong twice. **(a)** It omitted **`remove`**, which resolves a label and then
  > **deletes the keychain stash** (the `remove` path in `src/cli.rs` (`remove_confirmation()` and its caller)) — the only one of the four
  > whose first-match-wins outcome is **irreversible**. `use` picks the wrong active account and
  > `enable`/`disable` flips the wrong flag; both are recoverable in one command. A decision taken over
  > the three cheap cases would have settled them and left the expensive one to inherit the answer.
  > **(b)** Its second option was a no-op: refuse-on-ambiguity is what `use` **already ships**
  > (`resolve_target`, `src/use_account.rs`), so "should `use` take first" was proposing a *regression*
  > as if it were a symmetric alternative. `remove`'s irreversibility should drive the answer.

- **OQ-4 (shapes R-10)** — is `--no-secrets` hard-removed with a strict-usage error naming the
  replacement, or deprecated across one release then removed? It is a **shipped** flag. Lean: hard
  remove, since the repo has one operator and a usage error is self-documenting — but this is a
  product call, not a design one, and R-10a records it as undecided.
- **OQ-5 (bounds R-16)** — we cannot fix already-released binaries, so is the deliverable (a) documenting
  a version floor and making the failure legible, (b) also making the artifact-config parse path
  tolerant so the next added block does not re-break it, or both? Lean: both — (a) alone leaves the
  defect free to recur.
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
| R-5 | 4.4 | Cap-4.1 | covered |
| R-5a | 4.4 | — (verification, done) | covered |
| R-6 | 4.3 | Cap-3.1 | covered |
| R-6a | 4.3 | Cap-3.2 | **decision-gated** (OQ-1) |
| R-7 | 4.5 | Cap-5.1 | covered |
| R-9 | 4.7 | Cap-7.1, Cap-7.2, Cap-7.6 | covered |
| R-9a | 4.7 / AD-6 | Cap-7.3 | covered |
| R-9b | 4.7 | Cap-7.4 | covered |
| R-9c | 4.7 / AD-5 | Cap-7.5 | covered |
| R-9d | 4.7 / AD-10 | — (naming; asserted by Cap-7.1's flag surface) | covered |
| R-10 | 4.7 | **Cap-7.7** | covered — was mapped to Cap-7.5, which asserts `export` has no *config/roster* narrowing flag (R-9c). `--no-secrets` is a *secrets* flag, and § 4.7 states secrets are not a third axis, so Cap-7.5 passed green while `--no-secrets` still shipped. |
| R-10a | — | — | **decision-gated** (OQ-4) |
| R-10b | 4.9 | Cap-9.2 | covered |
| R-11 | 4.8 / AD-7 | Cap-8.1 … Cap-8.5 | covered |
| R-11a | 4.8 / AD-8 | Cap-8.1 | covered |
| R-11b | 4.8 | Cap-8.2 | covered |
| R-11c | 4.8 / AD-11 | Cap-8.3 | covered (over a recorded dissent) |
| R-11d | 4.8 | Cap-8.4 | covered |
| R-11e | 4.8 | Cap-8.5 | covered |
| R-11f | 4.8 | — (ADR deliverable) | covered |
| R-12 | 4.9 | Cap-9.1, Cap-9.2 | covered |
| R-13 | 4.9 | Cap-10.1 | covered |
| R-14, R-14a | 4.9 | Cap-10.2 | covered |
| R-15 | 4.9 | Cap-11.1 | covered |
| R-16 | 4.9 | Cap-7.4, Cap-11.2 | **partly decision-gated** (OQ-5) |
| R-8 | 4.6 | — (document) | covered |

## 16b. Backward-Coverage Matrix

Every capability traces to a requirement: Cap-1.x→R-2/R-2a, Cap-2.x→R-4/R-4a, Cap-3.x→R-6/R-6a,
Cap-4.1→R-5, Cap-5.1→R-7, Cap-6.1→C-3, Cap-7.x→R-9/R-9a-d/R-10, Cap-8.x→R-11/R-11a-e,
Cap-7.7→R-10, Cap-9.x→R-12/R-10b, Cap-10.x→R-13/R-14/R-14a, Cap-11.x→R-15/R-16. **No orphan capabilities.**

Two requirements are covered by a **deliverable rather than a capability**, recorded explicitly so
their absence from the Cap-list does not read as a coverage gap: **R-11f** (the portability ADR) and
**R-9d** (flag naming, which has no behaviour of its own — it is asserted incidentally by Cap-7.1's
flag surface). **R-10a** has neither, because it is undecided (OQ-4).

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
5. **OQ-4 (R-10's deprecation path) and OQ-5 (R-16's deliverable) are open.** Neither blocks
   implementation of anything else, and both are product calls rather than design ones.
6. **AC-2 was found defective and corrected** (PRD AC-2a): the planned "run `use <label>`" guidance is
   a provable no-op for the active account, which would have reproduced the original failure through
   its own remediation. § 4.1's choice to reuse `use` stands; the named invocation is now
   `use --force <label>`, and **§ 4.1, § 5, § 6 and Cap-1.1 have all been re-derived against the correction** — including § 4.1's
   option table and the `--activate` sugar, which inherits the same requirement.
