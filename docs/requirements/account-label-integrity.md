---
title: Account Label Integrity — Telling the Operator Which Account a Label Names
scope: account-label-integrity
created: 2026-10-26
profile: full
status: draft
dor_status: failed
source: pipeline-authored by a `/scope` Stage 1 run of `/capture-requirements` on 2026-10-26, from the `/investigate` report on the operator's question of 2026-10-25 (grounded at HEAD 99ab155, timestamp 2026-10-25T10:25:58+01:00) and the `/scope` seed that selected its ten gaps; both are machine-local scratch under `.tmp/`, not committed artifacts, so every value this PRD rests on is replicated in-band in § 2 and was re-verified against the tree at d9d75ba
parent-requirements: private HQ records prd-login (REQ-LGN-B-003, REQ-LGN-B-004) and design-login (D-LGN-3, D-LGN-6), quoted verbatim in § 2.13 because no clone can reach them; the in-repo parents are R-6, R-6a and AC-6 of the migration-credential-portability PRD, quoted there too
formulation: {technical-architecture: complete, data-architecture: complete, ux-ia: complete, api-design: complete, security-architecture: complete, testing-architecture: complete}
features:
  label-collision-notice: {stage: design, tracks: {technical-architecture: complete, api-design: complete, ux-ia: complete, testing-architecture: complete}}
  login-landing-disclosure: {stage: design, tracks: {technical-architecture: complete, api-design: complete, ux-ia: complete, security-architecture: complete, testing-architecture: complete}}
  shared-credential-detection: {stage: design, tracks: {technical-architecture: complete, api-design: complete, ux-ia: complete, security-architecture: complete, testing-architecture: complete}}
  identity-keyed-surfaces: {stage: design, tracks: {technical-architecture: complete, data-architecture: complete, api-design: complete, ux-ia: complete, security-architecture: complete, testing-architecture: complete}}
  import-payload-uniqueness: {stage: design, tracks: {technical-architecture: complete, data-architecture: complete, security-architecture: complete, testing-architecture: complete}}
  import-identity-check: {stage: design, tracks: {technical-architecture: complete, data-architecture: complete, security-architecture: complete, testing-architecture: complete}}
  identity-premise-measurement: {stage: design, tracks: {technical-architecture: complete, security-architecture: complete, testing-architecture: complete}}
  label-normalization: {stage: design, tracks: {technical-architecture: complete, security-architecture: complete, testing-architecture: complete}}
  uniqueness-doc-accuracy: {stage: design, tracks: {technical-architecture: complete, testing-architecture: complete}}
  orphan-stash-disposition: {stage: design, tracks: {technical-architecture: complete, data-architecture: complete, security-architecture: complete, testing-architecture: complete}}
artifacts: {design-doc: docs/design/account-label-integrity-solution-design.md, design-brief: docs/briefs/2026-10-26-design-account-label-integrity.md}
---

# PRD — Account Label Integrity: Telling the Operator Which Account a Label Names

> **Provenance.** An AI pipeline authored this document: `/scope` Stage 1 ran `/capture-requirements`
> headless on 2026-10-26, while the operator could not be asked, from a `/investigate` forensic report
> and the `/scope` seed that selected its gaps. **Two operator-authored messages bear on its content**,
> and § 2.1 quotes both verbatim. The second, "/scope the gaps discovered", ratifies the **class** of work: the
> gaps the investigation discovered. **Everything else is pipeline-authored and ratification-pending**:
> the enumeration into ten gaps, the problem framing (§ 2), the appetite (§ 3), every success metric
> (§ 4), every requirement (§ 10), and every decision listed in § 8.4. Each requirement carries an
> `Origin` tag, and § 14c records the split. The TL;DR (§ 1) and the benefit hypothesis (§ 4) are the
> operator's to write and are left visibly owed. Every code claim below was re-verified against the
> tree at `d9d75ba` rather than inherited from the report, and § 2.14 lists the claims of the report
> and the seed that the tree contradicted. Read a re-verification claim here as "checked once, at that
> commit", never as "cannot be wrong". Its `file:line` citations were re-resolved against `363698d`,
> and each cited line reads the same there as at `d9d75ba`. The line numbers that §§ 2.11 and 2.14
> quote from other documents stay as those documents wrote them.

**Version**: 1.5
**Created**: 2026-10-26
**Last Updated**: 2026-11-03
**Status**: Draft — blocked from scoping by `dor_status: failed` (§ 14c); the run went past that verdict provisionally, under the scope brief's register row `proceed-past-failed-dor`, and § 14b lists the items it filed
**Sources**: two of the operator's messages (§ 2.1); the `/investigate` report grounded at `99ab155` and
the `/scope` seed of 2026-10-26, both machine-local scratch; the tree at `d9d75ba`; the private HQ
records quoted in § 2.13; the tracker items named in § 7, read 2026-10-26.

## 1. TL;DR

> **Pipeline-authored on 2026-11-03, ratification-pending like the rest of this document.** Composed from
> the finished document after the scoping pull request merged, on the operator's ruling of 2026-11-03 that the
> TL;DR is never theirs to write (the rule it replaces is tracked as `alexey-pelykh/.claude` issue 6464).

Sessiometer identifies an account by its server-assigned identifier and lets the operator refer to it by a
free label, and nothing keeps labels honest. CLI `capture`, CLI `login`, the socket `capture` and the Settings
rename can each give one label to two accounts, or move a label onto a different account, without saying so.
A `login` can rename a different account than the one the operator meant. One credential can sit under two
roster entries unnoticed. The panel, the usage history, the `status` and `watch` output and the swap log then
tell accounts apart by label alone, so a shared or moved label makes them merge or confuse the accounts behind
it (§ 2).

**Proposed solution.** Refuse, before any write, a write that would create a collision or rename an account,
unless the operator consents on the CLI with `--overwrite` or `--rename` (ruled 2026-11-03, § 8.1 and design
§ 12.3), and make a write that stores a credential another entry already holds announce it on the surface
that did the write (F1 to F3). Give every surface
downstream of the roster an identity that a relabel does not change (F4). Check an import payload's own
identities before writing (F5, F6). Measure the premises behind the identifier on real logins (F7). Record
the decisions nobody has made: what makes two labels the same (F8) and what becomes of an orphaned keychain
item (F10), and bring the documents that disagree with the code into line (F9).

**Not in scope.** Enforcing unique labels at load time, which ratified AC-6 forbids; choosing between or
disabling accounts that share a credential; an online identity check (§ 3).

**Decisions.** Six were the operator's to make (§ 8.1 and § 8.3). Two are ruled as of 2026-11-03: a write that would
make a label name a second account is refused unless `--overwrite` is given, and a `login` that would rename
a different account than the label names is refused unless `--rename` is given. Four stay open: what makes
two labels the same, how a malformed import is handled, what becomes of an orphan stash, and which
identifier the wire carries.

**Status.** Pipeline-authored from the operator's two messages (§ 2.1); 35 tracked items under #1550
(§ 14b); `dor_status: failed` on approval grounds only (§ 14c), so nothing below is ratified yet. Appetite:
one developer, one big batch (§ 3), with the scope brief's estimate of the must-have scope above it (its
`stage37-appetite-overflow` row), reported rather than cut.

## 2. Problem Statement

> **Pipeline-framed, pending the operator's validation.** The pipeline framed this statement at
> Phase 1 from the operator's question (§ 2.1) and the investigation's findings. The operator has not
> validated it, and readiness check 1 fails on that ground (§ 14c).

**The problem, as framed.** The operator names an account by its label. The system identifies an
account by its server-assigned `account_uuid` and treats the label as a free, non-unique handle. The
roster cannot hold one account twice, because every write looks the uuid up first. But four write
paths can, without telling the operator, give one label to two accounts or move a label onto a
different account, and nothing notices one credential stashed under two roster entries. Downstream,
the menu-bar panel, the usage history, the `status` / `watch` wire and the swap log identify an
account by its label alone. Once a label is shared or moved, those surfaces merge or confuse the
accounts behind it.

**Who has the problem.** The operator, who is the sole user, maintainer and on-call. Their fleet
carries email-shaped labels that differ by a single character (§ 2.2). The cost lands whenever they
act on a label: swap to it, read its usage, or trust its expiry.

**Current state.** Five entry paths can write a label, and only one of them compares it with the
labels the other roster entries carry.

| Entry path | Compares the new label with other rows | Outcome when another account carries the label | Code |
|---|---|---|---|
| CLI `capture [<label>]` | no | a duplicate is created silently | `plan_capture` (`src/capture.rs:635-685`) |
| CLI `login [<label>]` | no | the same, through the same `plan_capture` | `plan_capture` (`src/capture.rs:824-825`) |
| socket `capture` (menu bar) | no | the same, through `capture_locked` | `capture_locked` (`src/daemon/commands.rs:349-356`) |
| socket `config-set` (Settings) | no | the relabel is keyed by uuid and stored as given | `overlay_labels` (`src/config/settings.rs:159-169`) |
| CLI `import` | yes: bearer counts before and after the merge | a warning (R-6) | `label_bearers`, `created_ambiguity` (`src/cli.rs:5419-5438`) |

**Why now.**

1. On 2026-09-09 the operator ran `login` three times under email-shaped labels and got
   `Revived "…" (still 8 in rotation).` each time. That reply cannot say which account each login
   actually landed on (§ 2.2).
2. #1020 plans to move the login spawn to `claude auth login --claudeai`. ADR-0032 records, from a
   static read of the binary, that an inherited bearer can land such a spawn on the wrong account
   while every existing check passes, which is why #1535 must land first. Today a login that lands on
   the wrong account reads, in its reply, exactly like one that landed on the right one.
3. The deferred online identity check (ADR-0028, #737) waits for roster accounts to be seen sharing a
   credential, and nothing in the tree can see that (§ 2.5). Until something detects it, that gate
   cannot open.

**What doing nothing costs.** A row whose label moved onto another account keeps the name while it
tracks the captured account's expiry, health and usage, so a swap "to X" lands on whatever account
now carries X. Two rows under one label share one panel identity and one usage history. Two rows
holding one credential count one real usage limit twice. None of these announces itself.

### Problem framing — what was challenged

| Framing | Verdict | Why |
|---|---|---|
| "One account can be captured twice" | **Rejected** | The uuid key prevents it. A second capture or login of one account refreshes its row in place (`roster.iter_mut().find(…)` in `plan_capture`, `src/capture.rs:658-665`), and every load rejects a duplicate uuid (`uuids.insert`, `src/config/validate.rs:315-320`) |
| "Labels must be unique" | **Rejected** | R-6 and AC-6 (§ 2.13) ratified duplicate labels as an accepted state. The defect is that four paths create one silently, not that the state exists |
| "Login must verify the account it captures" | **Kept as an option, not as the framing** | Verification needs a rule for when a label counts as an address (#846) and a premise only a real login can measure (§ 2.9). It is one option of `login-mismatch-policy` (§ 8.1) |
| "At the moment of a write, the operator cannot tell that a label now names a different or an additional account, and later surfaces cannot tell same-label accounts apart" | **Adopted** | Every gap in § 3 is an instance of it |

### 2.1 The operator's words — the only operator-authored text this document draws on

1. The question that started the investigation (2026-10-25), verbatim:
   "do we have a protection to avoid capturing/loggin-in to an account twice under different label or somehow messing up uniqueness?"
2. The scope request (2026-10-26), verbatim: "/scope the gaps discovered"

The investigation's answer, in one line: **yes** at the roster key, since one account cannot become
two roster entries; and **no** for a label reused for a different account, for a captured account
that does not match the label it was requested under, and for one credential stashed under two
entries. Concurrent logins are a further "no" that #1011 and #1482 already own.

### 2.2 What the 2026-09-09 transcript shows

A pasted terminal session shows three `login` runs, one per typed label, each answered in the same
words. The dispatcher dated it 2026-09-09; the date is not in the paste. It ran a
`./target/release/sessiometer` binary whose commit is not established. One reply, with the operator's label replaced by a synthetic one of the same shape:

    Revived "me@provider.com" (still 8 in rotation).

The other two runs typed two more labels, shown here as the synthetic `me@provider.eu` and `me@provider.fr`, and were
answered the same way. In the run quoted above, between the command and its reply, the login child
printed a resume hint (`Resume this session with:`, then `claude --resume` and a session id,
elided here), which is #1542's subject. A `status` that followed listed eight rows under eight byte-distinct, email-shaped labels.
Among them are two shown here as the synthetic `me@provider.com` and `me@pro-vider.com`, which differ by one
hyphen, and the roster carried further pairs that differ the same way. Every other email-shaped label in this
document, in § 2.10, in the case-folding row of § 8.1 and in AC-LBL-001, is an illustrative stand-in of the same
shape, not an address the operator holds.

Read against the tree, all of the following is inferred, because the binary is tied to no commit:

- `Revived` means each captured uuid was already in the roster. A new account would have printed
  `Onboarded … (now 9 in rotation).`
- The reply prints the label the run has just written to the matched row (the read-back through `stash_name`, `src/capture.rs:850-855`), so it cannot show which account was captured.
- Eight distinct labels afterwards rule out one run that landed on another rostered account and
  stayed uncorrected, because that would have left two rows under one label. They cannot rule out two
  runs that landed on each other's accounts and swapped labels; each row's figures would then belong
  to the other account. The roster before the runs is not in the paste.
- Each hyphen pair is two rows with two uuids, which the roster treats as two accounts. Folding
  punctuation would merge their labels (§ 2.10).

### 2.3 Gap `duplicate-label-silent-at-entry` — four paths create a duplicate label silently

- `plan_capture` looks up only the uuid, then appends a new account under whatever label it was given: `let label = provided.map_or_else(|| derive_label(account_uuid), str::to_owned);` … `roster.push(account);` (`src/capture.rs:670-683`).
- A revive with a label renames the matched account, whatever the other rows carry: `existing.label = l.to_owned();` (`src/capture.rs:658-665`).
- The socket `config-set` relabels by uuid and stores the label it was sent: `account.label = new_label.clone();` in `overlay_labels` (`src/config/settings.rs:159-169`).
- A load accepts duplicate labels. The roster loop of `Config::validate` rejects an empty uuid, a malformed uuid, an empty label and a duplicate uuid, and has no duplicate-label arm (`account_uuid_violation`, `uuids.insert`, `src/config/validate.rs:294-326`).
- `import` is the one path that warns, and the comment above its check says why it stops there: "Duplicate labels stay an ACCEPTED state, so this neither refuses the import nor renames anything; it only makes the creation audible" (the comment above `bearers_after`, `src/cli.rs:5382-5419`).
- Its rule is a count comparison over the whole merge, `after > 1 && after > before`, and the comment records why both halves carry weight: a warning that fires where nothing was created "trains dismissal of the one that matters" (`created_ambiguity`, `src/cli.rs:5404-5425`).
- R-6a makes every site that resolves a label refuse an ambiguous one with `UseTargetAmbiguous`, exit 6 (`resolve_target`, `src/use_account.rs:486-502`).
- No record covers `capture`, `login`, the socket `capture` or `config-set` creating a duplicate
  label. R-6 and AC-6 (§ 2.13) cover `import` and the resolving sites only.
- No test covers it either. `recapture_updates_the_label_when_a_new_one_is_given` pins the relabel
  itself, `apply_import_warns_when_it_creates_a_same_label_different_uuid_entry` pins the import
  warning, and `resolve_target_reports_ambiguous_for_a_duplicated_label_and_never_guesses` pins the
  resolver's refusal.

### 2.4 Gap `login-lands-on-another-account` — a login can relabel a different account, and its reply cannot show it

- `login`'s argument is a label, and its usage text calls it "a name for the new account (auto-derived from its account-uuid if omitted)" (`LOGIN_USAGE`, `src/cli.rs:1010-1015`).
- The identity is the `oauthAccount` that Claude Code writes into the isolated directory's `.claude.json` during the login (`read_oauth_account_from`, `src/login.rs:252-258`).
- The roster decision is keyed on that uuid: `plan_capture(&mut roster, captured.oauth_account.account_uuid(), label)?` (`src/capture.rs:824-825`).
- Nothing compares the argument with that uuid, or with the harvested `emailAddress`.
- So `login X` that lands on an already-rostered account Y renames Y's row to X. The reply reads the label back after the rename (`stash_name`, `src/capture.rs:850-855`).
- The reply then prints `Revived "{label}" (still {count} in rotation).` through `login_confirmation` (`src/capture.rs:506-513`).
- If another row already carried X, two rows now carry X, and § 2.3 applies as well. A login that
  lands on an account not yet rostered onboards it under X, and its reply,
  `Onboarded "{label}" (now {count} in rotation).`, already differs by its verb and its count.
- After such a relabel, the row named X tracks the captured account's expiry and health, because per-account runtime state follows the uuid on every reload (`old.account_uuid == account.account_uuid`, `src/daemon/commands.rs:814-821`).
- What exists protects the pairing of credential and identity, not the label. `capture` reads both under one swap lock (`SwapLock::acquire`, `src/capture.rs:245-258`).
- `login` fails hard if the shared canonical item changed during the spawn (`Error::SharedCredentialMutated`, `src/login.rs:236-238`).
- The spawn removes the inherited credential variables it lists (`SPAWN_ENV_REMOVE`, `src/isolated_spawn.rs:132-139`), and the list names neither `ANTHROPIC_AUTH_TOKEN` (#1535) nor `CLAUDE_CODE_CHILD_SESSION` (#1542).
- The `login` constructor still sets the argv to `/login`, beside `Stdio3::InheritTerminal` (`src/isolated_spawn.rs:208-211`), so ADR-0032's wrong-account hazard under `auth login` is not live today.
- REQ-LGN-B-004 and D-LGN-6 (§ 2.13) decide "one account, one entry" and an optional label. No record
  decides what happens when the label and the captured account disagree. REQ-LGN-B-004's stamp says
  "A bare `login` for an existing label refreshes it", which matches on the label, while the code
  matches on the uuid (§ 8.2).
- No test asserts that `login X` landed on X; `recapture_updates_the_label_when_a_new_one_is_given`
  pins the relabel.

### 2.5 Gap `one-credential-two-entries` — one credential under two roster entries goes undetected

- Each roster account's credential lives in a keychain stash item named after its key: `format!("{STASH_PREFIX}{}", self.account_uuid)` (`Account::stash`, `src/config.rs:390-392`).
- The item is written with `add-generic-password -U` (`extend_from_slice`, `src/stash.rs:336-339`), so one uuid has one stash, a service holding its `credential` and `oauthAccount` items (`ACCT_CREDENTIAL`, `src/stash.rs:61-63`), and a re-capture overwrites it.
- What goes undetected is two different uuids whose items hold byte-identical credentials. Active-account resolution takes the first stash, in roster order, that matches the canonical item (`resolve_account_for`, `src/active.rs:43-57`).
- So the later of two rows sharing a token is never recognised as active.
- The canary passes that case on purpose: "a canonical matching `stash[A]` is never refused, even if the same bytes also sit under another account's stash" (`src/canary.rs:41-44`).
- That pass is pinned by `shared_token_under_both_stashes_passes_via_the_stash_a_first_order`.
- Each non-active account is polled with its own stash token (`RealRosterPoller::poll`, `src/daemon/seams.rs:87-105`). Inferred: two rows holding one account's token poll one real usage limit and count it twice.
- The comparison a detector needs already exists: `Credential::matches` compares the bytes in memory (`src/keychain.rs:356-358`).
- ADR-0028 defers the online identity check (`/api/oauth/profile`), "reserved as an
  operator-enablable escalation only if the shared/duplicate-token falsifier fires", and adds that "if
  roster accounts ever share credential blobs, its false-positive profile degrades and the deferred
  online identity fetch becomes warranted". Nothing detects that sharing, so the trigger can never be
  observed (#737).
- A detector has a window. The migration PRD's A-2 records that "Anthropic rotates the refresh token
  on every exchange" (resolved by spike #262). Two stashes that start byte-identical stop being so
  once either of them is refreshed, so a detector that runs only later, over stored stashes, misses
  every pair a refresh has already split.

### 2.6 Gap `label-keyed-downstream-surfaces` — surfaces downstream of the roster identify an account by its label

- **Menu-bar panel.** A row's SwiftUI identity is its label: `var id: String { label }`
  (`apps/menubar/Sources/HonestStateMachine.swift`, `AccountRow`). A duplicate label "resolves once to
  the same monogram" (`apps/menubar/Sources/StatusPanelFormat.swift`, the monogram loop). Inferred:
  two rows under one label give `ForEach` two elements with one `id`, for which SwiftUI defines no
  rendering. The panel design mock authors no frame for two rows with one label.
- **Usage store.** The stored history key `acct` is "the account's roster label" (`Sample`, `acct`, `src/usage_store.rs:59-62`).
- The daemon records each poll under that label (`record_usage_sample`, `src/daemon.rs:679-689`). Inferred: a duplicate label merges two accounts' histories, and a relabel starts a new history handle.
- **`status` and `watch`.** Each account line carries its label and no uuid (`AccountStatusLine`, `src/daemon/snapshot.rs:737-739`).
- That wire is versioned by `STATUS_SCHEMA_VERSION`, now 1.14 (`src/daemon/snapshot.rs:538-541`), byte-pinned by goldens under `build/fixtures/` and mirrored by Swift fixtures.
- **The swap log.** `event=swap` and `event=emergency_swap` name their accounts by label, "`from`/`to` are HANDLES (operator labels)" (`Event::Swap`, `src/observability.rs:1353-1359`), so a swap between two same-label accounts reads as a swap from a label to itself.
- **"N in rotation"** counts roster rows, disabled ones included: `let count = roster.len();` (`src/capture.rs:587`), and the same line in `run_login` (`src/capture.rs:847`).

The uuid already reaches some of these surfaces, and issue #15 is read two ways about it:

- The `config-get` view sends each account's "non-secret `account_uuid` / `label` / `enabled`" and calls itself "exactly as safe to return over the same-user control socket as the `status` / `watch` snapshots" (`AccountView`, `src/config/settings.rs:60-76`).
- The derived label is the uuid itself, "unique (it is the roster key) and safe to print" (`derive_label`, `src/capture.rs:615-623`), so a uuid already reaches every label surface whenever a new account is rostered with no label: the fallback calls `derive_label` only for a new account whose label was omitted (`src/capture.rs:667-670`), a revive keeps the label it has, and CLI `capture` at a terminal first offers the harvested email and falls back to the uuid only when the operator does not confirm it (`prefill_label_from_identity`, `src/capture.rs:166`).
- Event-log usage-family lines carry the uuid in issue #15's name: "`account` is the account UUID (#15) — matching the usage-family `acct=` (never the free-form label)" (`BlindEnter`, `src/observability.rs:2125-2133`).
- Against that, the status wire's `canary` field is documented "Labels only — never a token, email, or account-uuid (issue #15)" (`CanaryStatus`, `src/daemon/snapshot.rs:655-668`).
- The Swift mirror of that field and the panel's canary-banner formatter say the same
  (`apps/menubar/Sources/WireModel.swift`, `CanaryStatus`; `apps/menubar/Sources/StatusPanelFormat.swift`,
  the canary banner), and #791 carries the acceptance line "Redaction (issue #15): account labels
  only, no identifiers".
- Issue #15's own meter, read 2026-10-26, forbids OAuth token prefixes, a fingerprint of an injected
  credential blob, any long high-entropy run, and any `@`-bearing email-shaped token. It names no uuid.
- Its entropy backstop splits text on every non-alphanumeric character before it measures a run (`is_ascii_alphanumeric`, `src/redaction.rs:359-360`).
- The backstop flags runs of at least `ENTROPY_MIN_RUN` = 20 characters (`src/redaction.rs:176-179`); a hyphenated uuid's longest run is 12.

So the tree holds two readings of issue #15 for the account uuid, and F4 has to settle which one
governs the status wire before it can carry one (DQ-1, § 8.3).

### 2.7 Gap `import-payload-duplicate-uuid` — corrected: the collapse is in the payload's credential list

- A migration artifact's payload is a rendered `config.toml` plus a credential list (`Payload`, `ManagedAccount`, `src/migration.rs:200-210`).
- The roster half cannot carry a duplicate uuid into the target: `import` parses it with `Config::from_toml_str`, which runs `Config::validate` and rejects one (`from_toml_str`, `src/cli.rs:5285-5289`).
- The credential half is collected into a map keyed by uuid, so of two entries for one uuid the last silently wins (`secrets`, `ManagedAccount`, `src/cli.rs:5311-5317`).
- The in-tree exporter cannot produce that: it builds one credential entry per validated roster account (`gather_payload`, `src/cli.rs:4957-4978`). Only a hand-built or corrupted artifact can.
- A sibling in the same class: the restore loop walks the payload's roster and looks each uuid up in the map (`incoming_account`, `secrets.get`, `src/cli.rs:5328-5349`), so a credential entry that matches no roster entry is dropped without a word.
- The artifact container's own `validate` checks only its magic and its encryption parameters (`MigrationBadMagic`, `src/migration.rs:405-409`).
- The `FORMAT_VERSION` fixtures are frozen: the emitter's own doc says freeze once, then only read.

### 2.8 Gap `import-artifact-identity-unchecked` — the export side is now read

- At import, `write_and_verify` builds the stashed account straight from the artifact's `oauthAccount` and compares digests of what it wrote with what it reads back (`write_and_verify`, `src/cli.rs:5451-5472`).
- `write_and_verify` never compares the `oauthAccount`'s uuid with the uuid the entry is filed under.
- At export, `gather_payload` pairs each roster entry's `account_uuid` with the `oauthAccount` its stash holds, also without comparing them (`stashed.oauth_account.raw_json()`, `src/cli.rs:4967-4973`).
- So an artifact carries a mismatch exactly when its source stash does. Whether any write path can
  produce such a stash is not established: `capture` pairs the two halves under the swap lock,
  `login` takes both from the isolated directory (premise 1, § 2.9), and #211's guard covers the
  swap's re-stash.
- `build/version-compat.md` (H2) records what a mismatch would mean: the token governs auth and
  quota, the `oauthAccount` only what is displayed, and "a token/`oauthAccount` mismatch is
  **tolerated** (no re-auth, request still served)". A mismatched entry would spend one account's
  quota under another account's identity, silently.

### 2.9 Gap `identity-premises-unmeasured` — the key is only as good as premises nobody measured

The roster's protection keys every account on the `account_uuid` that Claude Code writes into
`.claude.json` (`oauthAccount.accountUuid`). The tree trusts that one login writes one account's
credential beside that same account's `oauthAccount`:

- "the keychain blob carries no uuid, so identity lives only here" (`reconcile_login`, `src/capture.rs:968-982`).
- The uniqueness of the key is stated only in a comment: "The `account_uuid` is the ONLY non-secret, always-present, unique field the harvested identity exposes" (`derive_label`'s doc, `src/capture.rs:615-623`).
- The migration PRD's A-1 grades "`account_uuid` is stable across machines for one Anthropic account"
  **Validated** on code comments alone.

Only a real login can settle the premises behind that:

1. whether one isolated login always writes the credential and the `oauthAccount` of one account;
2. whether one person's account in two organisations yields one `accountUuid` or two, while
   `organizationUuid` appears nowhere in `src`, `docs` or `build`;
3. whether `claude auth login --claudeai`, the login argv #1020 plans, still writes `emailAddress`
   into the isolated `.claude.json`; the `build/version-compat.md` section #1020 added does not
   measure it.

A fourth premise sits in the same class: whether Claude Code ever varies the letter case of one account's uuid. The key is compared byte for byte and never case-folded wherever it is matched (`resolve_target`, `src/use_account.rs:486-492`, compares `account.account_uuid == query`), and its validation checks only length and character set (`account_uuid_violation`, `src/config/validate.rs:525-541`), so two casings would be two keys.

### 2.10 Gap `label-normalization-undecided` — nothing decides what makes two labels the same

- Labels are compared byte for byte: `account.label == query || account.account_uuid == query` (`resolve_target`, `src/use_account.rs:486-492`).
- Labels are trimmed on the `capture` and `login` argument (`provided`, `src/capture.rs:656`) and on the interactive capture prompt (`trimmed`, `src/capture.rs:205`).
- The Settings app trims a draft before it sends it. The socket `config-set`, `import` and a hand
  edit store a label as given.
- So `Alice@x.com` and `alice@x.com`, or a label with a Cyrillic look-alike letter, are two distinct
  labels the operator cannot tell apart on screen, and nothing flags visually confusable labels.
- Folding punctuation would be wrong as a default: it would merge the hyphen pairs in § 2.2, which
  the roster holds as distinct accounts.
- No requirement, ADR, spec, finding or strategy record decides case folding, Unicode normalization,
  confusables, trailing dots or plus-addressing.

### 2.11 Gap `uniqueness-docs-drift` — committed documents that disagree with the code

| Site | What it says | What the tree says at `d9d75ba` | Owner |
|---|---|---|---|
| ADR-0032 prerequisite 1 (`docs/adr/0032-login-is-daemon-routable-tty-gate-is-ours.md`, lines 96–99) | `SPAWN_ENV_REMOVE` "does **not** remove `CLAUDE_CODE_OAUTH_REFRESH_TOKEN`, `CLAUDE_CODE_OAUTH_SCOPES`, or `CLAUDE_CODE_OAUTH_CLIENT_ID`" | all three are removed (`SPAWN_ENV_REMOVE`, `src/isolated_spawn.rs:132-139`), since commit `86ab6aa`, "(fix) login: scrub the OAuth refresh-token triple (issue #1009) (#1267)" | F9, REQ-DOC-B-001 |
| R-15 in `docs/requirements/migration-credential-portability.md`, and its echoes in `docs/specs/roster-input-validation.feature.md` and `docs/design/migration-credential-portability-solution-design.md` (lines 479–482) | the uuid's "shape and length are unchecked" | both are checked, on read and on write (`account_uuid_violation`, `src/config/validate.rs:525-541`) | #1091, which needs a ratification decision |
| Stale line anchors in the migration PRD | `cli.rs` 5148–5149 for R-6's quoted sentence; `validate.rs` 281–293, 281–284 and 289–293; `render.rs` 808; `cli.rs` 4744–4750, 4735, 4733–4734 and 4790; `config.rs` 370–372 | R-6's sentence survives verbatim only in a test comment beside `resolve_target_reports_ambiguous_for_a_duplicated_label_and_never_guesses`; the roster loop belongs to `Config::validate`; `label =` is written by `render`; the fresh-target arm, the parse, the invariant comment and the first `.stash()` call belong to `apply_import`; the stash name comes from `Account::stash` | F9, REQ-DOC-B-002, unless #1476 decides the corpus-wide policy first |
| The migration PRD's A-1 | "`account_uuid` is stable across machines for one Anthropic account", graded **Validated** on `config.rs` 343 and `capture.rs` 463 | both anchors are stale, both are code comments, and nothing measures the premise (§ 2.9) | F9, REQ-DOC-B-004 |
| `LOGIN_USAGE` (`src/cli.rs:1010-1015`) | `<label>` is "a name for the new account" | a label given for an already-rostered account renames that account (§ 2.4) | F9, REQ-DOC-B-003, coordinated with #1379, which edits the same constant |
| #1415's body | cites the login wiring at `cli.rs` 777 and 825 | it now sits at the `"login"` arm of `parse_subcommand` and the `Command::Login` dispatch | out of scope: tracker text outside this repository |
| The private HQ's cross-platform record, lines 156–157 | Linux and Windows 2.1.220 rows exist in `build/version-compat.md` | no commit in this repository ever carried them | out of scope: the private HQ |

The nearest production statements of R-6's sentence are the doc of `label_bearers` ("Labels are
operator handles and are not required to be unique") and the doc above `apply_enabled` ("Labels are
still operator handles and uniqueness is still not enforced").

### 2.12 Gap `orphan-stash-reaping` — established: nothing reaps or reports an orphan stash

Four paths leave a `Sessiometer/<uuid>` stash item that no roster row names:

1. A crash of CLI `capture` or `login` between its stash write and its roster save. The stash is written first on purpose (`stash.write`, `src/capture.rs:583-585`).
2. The login lost update: the roster was parsed before an interactive login that can run for minutes, and "a concurrent `config set` landing in that window is overwritten here" (`existing`, `src/capture.rs:950-953`). The overwritten writer's stash stays behind when that writer was a `capture`, `login` or `import` that rostered a new account in the window; a `config set` writes no stash (#1482).
3. A socket `capture` whose roster save fails: "A save failure leaves an inert ORPHAN stash" (`save_to`, `src/daemon/commands.rs:359-365`), as `CaptureRejection::Failed` also documents (`src/daemon/socket.rs:683-686`).
4. `remove`, which saves the roster before it deletes the stash, "so any failure past here orphans a harmless stash" (`remove_account`, `src/cli.rs:5990-6000`).

Nothing removes or reports such an item:

- The only production stash delete is `remove_account`'s own (`RealAccountStash`, `src/cli.rs:6000`).
- `STASH_PREFIX` is read in production only by `Account::stash` and by the prior-configuration witness, which reduces the whole namespace to one bool (`any_stash_item_present`, `src/keychain.rs:814-833`).
- The two reapers touch other things: `reap_orphans` reaps isolated-refresh artifacts per rostered uuid (`src/refresh.rs:904-912`), and `reap_login_orphan` reaps the isolated login directory (`src/refresh.rs:985-987`).
- Code comments call such a stash "harmless" and "inert".

The witness bounds any remedy. ADR-0036 makes "any `Sessiometer/…` keychain item" one of two
witnesses that a machine was configured; the other is a non-empty usage store. Reaping orphans on a
machine whose `config.toml` is absent would erase the witness that refuses a first-run write there;
a first-run write over a live roster is how the roster collapsed on 2026-08-27 (#1440). No runtime census of orphan stash items was
taken, because it needs a keychain command, which this run does not execute.

### 2.13 Decision records, quoted in-band

Private HQ records, a repository-root sibling that no clone contains, quoted verbatim as read on
2026-10-26:

- **REQ-LGN-B-003** (prd-login; operator-ratified 2026-07-01): "the account **identity (uuid)**
  SHALL be derived from the harvested credential's `oauthAccount` (same source `capture` uses). The
  **label** is an **optional positional** (`sessiometer login [label]`); when omitted it SHALL
  **auto-derive from the harvested account** (never hard-error, never prompt)."
- **REQ-LGN-B-004** (prd-login, keystone; operator-ratified 2026-07-01): "a `login` for an account
  **already** in the roster SHALL **update (re-stash) the existing entry in place** — never create a
  duplicate". Its ratification stamp: "'update-in-place + un-quarantine, never duplicate' chosen over
  reject-if-exists / require-`--relogin`. A bare `login` for an existing label refreshes it
  (idempotent); the accidental-overwrite risk is accepted (single-user; `--label` is explicit)."
- **D-LGN-6** (design-login; ratified by the operator 2026-07-01): "label = **optional positional**
  (`sessiometer login [label]`); **auto-derive from the harvested account** when omitted — never
  hard-error, never prompt". Its rationale adds: "Local label may default to the account email (fine
  locally; redaction applies only at the public/observability boundary, SUR-001/Q-001)."
- **D-LGN-3** (design-login): "Short-held `swap.lock`, **last-writer-wins**, never across the spawn",
  because "The operator's fresh interactive login is *authoritative intent*".

In-repo records:

- **R-6** (`docs/requirements/migration-credential-portability.md`; user-ratified 2026-08-04):
  "*When* `import` would append a roster entry whose `label` already exists on the target under a
  **different** `account_uuid`, the system **shall** warn at import time. Duplicate labels are a
  documented, accepted state …, so the requirement is **not** to forbid them — it is that import must
  not create one silently — **including when both colliding entries arrive in the same artifact**."
- **AC-6** (same document): "… **BUT NOT** by enforcing label uniqueness — that contradicts the
  documented design position".
- **R-6a** (same document; resolved 2026-08-06 and delivered under #1005): every site that resolves
  an operator label to a single account refuses an ambiguous one.
- **R-5** (`docs/requirements/roster-loss-prevention.md`; user-ratified): "`login` **shall** carry the roster it parses at its first config read through to the point of persistence … Where the two reads disagree, the operation **shall** be refused".
- The code implements R-5 by removing the second read: "One read, carried, is what removes that gap — there is no second read left to disagree with the first" (`reconcile_login`'s doc, `src/capture.rs:961-968`).
- **ADR-0028** and **ADR-0032** are quoted in § 2.5 and § 2.11. **ADR-0036** is the
  prior-configuration witness (§ 2.12); a new capture-time refusal composes with its refusal
  vocabulary, which #1441 gated against the Swift decoder.
- **`docs/requirements/gui-cli-capability-parity.md` R-1**: "Every CLI capability the project
  classifies as `routable` **shall** be reachable by an operator driving only the menu-bar app".

### 2.14 Corrections — what the tree contradicted in the report and the seed

| # | The report or the seed said | The tree says at `d9d75ba` | How this PRD states it |
|---|---|---|---|
| 1 | An import payload carrying two entries with one uuid collapses silently, the last one winning | The collapse is in the payload's credential list. The roster half is validated, and a duplicate uuid there is rejected. The in-tree exporter cannot emit either; and a credential entry that matches no roster entry is also dropped silently | § 2.7; F5 covers both, the second as a same-class addition |
| 2 | R-6's anchor `cli.rs` 5148–5149 now sits at `cli.rs` 5229–5230 | Those lines hold `label_bearers`'s paraphrase. R-6's sentence survives verbatim only in a test comment | § 2.11 |
| 3 | The listed stale anchors are the drift | The list was not exhaustive: `render.rs` 808, `cli.rs` 4744–4750, 4735 and 4790, `config.rs` 370–372 and 343, `capture.rs` 463, and `validate.rs` 281–284 and 289–293 have drifted too | § 2.11 |
| 4 | ADR-0032's stale claim sits at lines 97–98 | It spans lines 96–99: the prerequisite begins on 96 and the quoted sentence ends on 99 | § 2.11 |
| 5 | `login`'s identity read sits at `login.rs` 243–257 | Lines 243–247 read the credential back; the identity read is `read_oauth_account_from` at 252–258 | § 2.4 |
| 6 | Nobody has established whether anything reaps or reports an orphan stash | Established statically: nothing does | § 2.12 |
| 7 | Nobody has read the export side | Read: the exporter copies the stash's `oauthAccount` without a comparison | § 2.8 |
| 8 | A refusal contradicts both AC-6 and D-LGN-6 | D-LGN-6's "never hard-error, never prompt" is scoped in its own text to an omitted label, and `capture` has prompted since #447 (`prefill_label_from_identity`, `src/capture.rs:166-171`). The D-LGN-6 half is an ambiguity; the AC-6 half stands | § 8.1, § 8.2 |
| 9 | REQ-LGN-B-004 decides the login case | Its stamp matches on the label; the code matches on the uuid | 🔴 contradiction, § 8.2, under `login-mismatch-policy`; superseded 2026-11-03 for the rename and collision cases by the rulings (design § 12.3, C-3, C-13): a bare `login` for the account's own label still refreshes it, a login that would rename the account is refused unless `--rename` is given, and the stamp's "accidental-overwrite risk is accepted" no longer holds; the stamp's home is outside this repository, so this row carries the note |
| 10 | #1482's body: a "two-read consistency check" covers the login window | The second read is gone (`reconcile_login`, `src/capture.rs:961-968`), and a concurrent writer's change is still overwritten at save | § 2.13; fixing it stays #1482's |
| 11 | R-15: the uuid's shape and length are unchecked | `account_uuid_violation` checks both, on read and on write | 🔴 contradiction, routed to #1091 |
| 12 | The seed: a uuid on the wire does not trip the issue #15 meter | True of the meter; but the status wire's own `canary` comments say "never … account-uuid (issue #15)" | § 2.6; REQ-WIRE-C-002, DQ-1 |

## 3. Boundaries

### Appetite

**Big Batch — six weeks** of one developer's time. This is pipeline-proposed and
ratification-pending (§ 8.4). The ten features are independent of each other except where § 7 orders
them, so each lands on its own. **Circuit breaker**: at six weeks the batch ships what has landed,
and every feature not yet landed returns to `/scope` rather than extending the batch.
**Uncertainty**: moderate. The largest unknowns are F4's identifier and its history migration
(§ 8.3) and F7's measurements, which only the operator can take, outside the batch's coding time.

### In scope — the ten gaps, one feature each

| Gap (seed slug) | Feature | Requirements |
|---|---|---|
| `duplicate-label-silent-at-entry` | F1 `label-collision-notice` | § 10.1 |
| `login-lands-on-another-account` | F2 `login-landing-disclosure` | § 10.2 |
| `one-credential-two-entries` | F3 `shared-credential-detection` | § 10.3 |
| `label-keyed-downstream-surfaces` | F4 `identity-keyed-surfaces` | § 10.4 |
| `import-payload-duplicate-uuid` | F5 `import-payload-uniqueness` | § 10.5 |
| `import-artifact-identity-unchecked` | F6 `import-identity-check` | § 10.6 |
| `identity-premises-unmeasured` | F7 `identity-premise-measurement` | § 10.7 |
| `label-normalization-undecided` | F8 `label-normalization` | § 10.8 |
| `uniqueness-docs-drift` | F9 `uniqueness-doc-accuracy` | § 10.9 |
| `orphan-stash-reaping` | F10 `orphan-stash-disposition` | § 10.10 |

The constraints that bind every feature are in § 10.11.

### Out of scope, not yet — each with why it is deferred and what reopens it

| Item | Why it is deferred | What reopens it |
|---|---|---|
| A durable event-log record when a duplicate label is created or a login relabels an account | A Bucket-3 enrichment candidate the seed did not select | The operator adds it |
| An operator affordance to see the uuid behind each label | A Bucket-3 enrichment candidate the seed did not select. F4 carries an identifier for identity, never for display | The operator adds it |
| Every item already tracked: #1011, #1482, #1483, #1474, #737, #1535, #1020, #1542, #1546, #1548, #1415, #846, and the closed spike #130 | Each has its own owner; this PRD cites them as dependencies or neighbours (§ 7) and re-scopes none | Not applicable: they are tracked |
| R-15's correction and its two echoes (§ 2.11) | #1091 owns it, and it needs a ratification decision that no pipeline can supply | #1091 |
| The corpus-wide policy for line citations | #1476 owns it | #1476 |
| A duplicate label that arrives through `config restore` or a hand edit of `config.toml` | Both load a whole roster through `Config::validate`, with no before-and-after baseline for a notice, and a rejection there would enforce label uniqueness, which AC-6 forbids | A re-ratification of R-6 and AC-6, or a reported harm from such a duplicate |
| A label-uniqueness rejection at load time, anywhere | It contradicts ratified AC-6 (§ 2.13) | Only a re-ratification of R-6 and AC-6 |
| The inverse orphan: `config restore` restores no stash, so a restored roster can name a uuid whose stash `remove` deleted | Roster integrity rather than label integrity; the investigation recorded it as an ungrounded observation | Its own tracked item |
| Drift sites outside this repository: #1415's body and the private HQ's cross-platform record | This run writes only this document; the tracker and the HQ belong to other owners | Their owners |
| Linux and Windows credential stores (#26, #27) | No stash backend exists there yet, since every stash call shells out to `/usr/bin/security`; F3 and F10 have no subject on those platforms | #26 and #27 landing |
| Changing active-account resolution or polling for two entries that share a credential | Which entry is the real account is what the deferred online identity check would settle (ADR-0028, #737) | #737's decision |
| An online identity check | ADR-0028 defers it; F3 only makes its trigger observable | #737 |

### Explicitly not building

| Item | Why never |
|---|---|
| Naming an account by its harvested email address, a token, or any credential-derived value in a reply, notice or log line | Issue #15, and the contract of `login_confirmation`: "Names the account by its LABEL only, never the email or token (#15)" (`src/capture.rs:506-508`) |
| Choosing between, merging, or disabling two entries that share a credential, automatically | It needs identity knowledge the tree does not have, and ADR-0028 defers the only source of it |
| Editing the private HQ records or any tracker item as part of this work | They belong to other owners; this document quotes them |

## 4. Success Criteria

### Benefit hypothesis

> **Pipeline-authored on 2026-11-03, ratification-pending.** Drafted by the `/scope` orchestrator for the
> operator to accept or rewrite; it was left empty by the rule tracked as `alexey-pelykh/.claude` issue 6464.

If a write that would share or move a label is refused unless the operator overrides it, every write that
shares a credential says so, and every surface downstream of the roster keys on an identity a relabel does not
change, then the operator never acts on the wrong account because of a label: a swap, a usage reading or an
expiry read for "work" belongs to the account they mean, and a label that names two accounts is a state they
chose at a write, or let in through an import or a hand edit, never one a capture or a login created behind their
back.

### Serves

Pipeline-derived and ratification-pending: the operator's question in § 2.1. No write silently
makes a label name a different account or an additional one, and the operator can always tell which
account a label names.

> **Every metric below is pipeline-derived and ratification-pending.** The success metrics are the
> operator's. This run derived candidates at Phase 6 because the operator could not be asked.

HEART coverage: this is a single-operator tool with no acquisition funnel, so Happiness, Engagement
and Adoption have no signal a meter could read. Task Success carries the leading indicators. The
lagging indicators stand in for Retention: they ask whether the protection keeps holding after it
lands.

### Leading indicators — measurable within two weeks of each feature landing

#### MET-T-001: Silent label-collision entry paths

- **TAG**: SilentCollisionPaths
- **SCALE**: number of the four entry paths — CLI `capture`, CLI `login`, socket `capture`, socket
  `config-set` — on which a write that REQ-ROS-B-002 counts as creating a collision produces nothing
  the operator can observe
- **METER**: the RED-first oracles of REQ-ROS-C-002, run in the `test` job; a path counts as silent
  while its oracle fails
- **PAST**: 4 at `d9d75ba`; only `import` reports a collision (§ 2.3)
- **GOAL**: 0
- **STRETCH**: 0, with the menu-bar app presenting the two socket paths' reports (REQ-ROS-I-002)
- **CONSTRAINT**: a write that creates no collision reports nothing (REQ-ROS-Q-001)

#### MET-T-002: Relabel disclosure

- **TAG**: RelabelDisclosure
- **SCALE**: share of relabelling revives — since 2026-11-03 only a CLI `capture` or `login` made with `--rename` that
  matches a rostered account and gives it a different label; a socket `capture` refuses a relabel and is
  outside the population (design § 12.3 C-3, C-6) — whose reply names both the previous label and the
  new one
- **METER**: hermetic tests over each caller of `plan_capture`
- **PAST**: 0% at `d9d75ba`; the reply names only the new label (§ 2.4)
- **GOAL**: 100%
- **CONSTRAINT**: no reply names an email address that the label does not itself carry (REQ-NTC-C-001)

#### MET-T-003: Unreported shared-credential writes

- **TAG**: UnreportedSharedCredentialWrites
- **SCALE**: writes that store a credential byte-identical to one another roster entry already holds,
  without the operator being told at that write
- **METER**: a hermetic test per write path that seeds two stash entries with identical bytes
  (REQ-STSH-C-002)
- **PAST**: every such write at `d9d75ba`, because no detector exists (§ 2.5)
- **GOAL**: 0
- **CONSTRAINT**: no credential byte, digest or fingerprint appears in any output (REQ-STSH-C-001)

#### MET-T-004: Label-only identity surfaces

- **TAG**: LabelOnlyIdentitySurfaces
- **SCALE**: number of the four downstream surfaces — panel row identity, usage-store key,
  `status` / `watch` account line, swap-log line — that identify an account by its label alone
- **METER**: a hermetic test per surface with two accounts under one label, plus a read of each
  surface's identity key
- **PAST**: 4 at `d9d75ba` (§ 2.6)
- **GOAL**: 0
- **CONSTRAINT**: no surface changes the text it displays for a roster that has no duplicate label

### Lagging indicators

#### 30 days — MET-R-001: Identity premises recorded

- **TAG**: PremisesRecorded
- **SCALE**: number of the four identity premises (§ 2.9) that carry a recorded measurement, or a
  recorded reason it could not be taken, naming the Claude Code version
- **METER**: count the entries in `build/version-compat.md` and under `docs/findings/`
- **PAST**: 0
- **GOAL**: 4 within 30 days of the operator's next real-login walk, the walk that #1020, #1542 and
  #1546 also need
- **WISH**: one walk records all four

#### 60 days — MET-R-002: Uniqueness-related drift sites

- **TAG**: UniquenessDriftSites
- **SCALE**: number of the in-repo drift sites that § 2.11 assigns to F9 and that still disagree with
  the tree
- **METER**: re-read each site against the tree; run `scripts/check-doc-citation-rot.sh --audit` for
  the line anchors
- **PAST**: every one of them at `d9d75ba`
- **GOAL**: 0, or each remaining anchor carried by #1476's decision

#### 90 days — MET-R-003: Shared-credential detections

- **TAG**: SharedCredentialDetections
- **SCALE**: shared-credential reports (REQ-STSH-B-002) in the operator's event log over the 90 days
  after F3 lands
- **METER**: search the event log for the report's event name
- **PAST**: unobservable, because no detector existed
- **GOAL**: none set; this is an observation that feeds #737's decision, not a target

### Decision gates

| Gate | Metric | Threshold | Decision |
|---|---|---|---|
| 2 weeks after F1 and F2 land | MET-T-001, MET-T-002 | 0 silent paths; 100% disclosure | Continue; otherwise the feature reopens |
| 30 days after the real-login walk | MET-R-001, premise 1's result | Premise 1 is false: one login can write one account's credential beside another account's identity | Rethink: F3 and F6 become the primary protection, and the uuid key's foundation is re-examined |
| 30 days after the real-login walk | Premise 2's result | One `accountUuid` for one person's account in two organisations | Rethink: the roster then merges separately limited credentials, and a new problem statement is owed |
| 90 days after F3 lands | MET-R-003 | at least one detection | Take #737 to a decision, as ADR-0028 provides; at zero, record that the trigger stayed silent |

### North Star

#### MET-NSM-001: Silent identity divergence at the write

- **TAG**: SilentIdentityDivergence
- **SCALE**: writes, on every entry path, after which a label names more accounts than before, a
  label names a different account than before, or a credential sits under two entries, without the
  operator being told at that write
- **METER**: the union of the oracles behind MET-T-001, MET-T-002 and MET-T-003
- **PAST**: every such write at `d9d75ba`
- **GOAL**: 0
- **Value connection**: the operator's question (§ 2.1) asks whether uniqueness can be "messed up"
  without their knowing; this counts exactly that

## 5. Proposed Solution

At sketch altitude, as the operator would meet each change. The technical design belongs to the
solution design document that Stage 2 writes.

| Scenario | Today | After |
|---|---|---|
| A. The operator captures or logs in under a label another account already carries | The roster silently gains a second bearer of that label | The operator learns it at that write, in the form `duplicate-label-policy` selects (F1) |
| B. `login X` lands on an already-rostered account labelled Y | The reply says `Revived "X"`, and Y's row is now called X | The reply also names Y, the label the account carried before; what else happens follows `login-mismatch-policy` (F2) |
| C. A write stores a credential another roster entry already holds | Nothing notices; the later entry is never recognised as active, and one usage limit counts twice | The operator is told which two entries share it, by label, and nothing is chosen for them (F3) |
| D. Two accounts carry one label | The panel gets two elements with one identity, their usage histories merge, and `status`, `watch` and the swap log cannot tell them apart | Each surface keys the account on an identity a relabel does not change (F4) |
| E. The operator imports a hand-built artifact with two credential entries for one account, or with an entry whose credential belongs to a different account | The last duplicate wins silently, and the mismatched credential is adopted as it is | The duplicate is reported in the form `import-duplicate-uuid-policy` selects (F5); the mismatched entry is refused while the rest of the import continues (F6) |
| F. The operator walks a real login | The premises behind the roster key are unmeasured | Each premise is recorded with its Claude Code version (F7) |
| G. The operator decides | Nothing records what makes two labels the same, or what becomes of an orphan stash | A decision record for each (F8, F10), and committed documents that match the code (F9) |

### Stakeholder perspectives

| Perspective | Concern | Where the requirements answer it |
|---|---|---|
| Product: the operator as owner | Label integrity without reopening R-6's ratified position | Reports rather than a uniqueness rule, unless the operator re-ratifies (§ 8.1) |
| End user: the operator at the terminal or the panel | Knowing which account a write touched, when it happens | F1 and F2 replies; menu-bar parity (REQ-ROS-I-002, REQ-IDN-I-002) |
| Developer | Rust and Swift changing together over closed decoders; a wire bump; a store migration | REQ-ROS-I-001, REQ-WIRE-C-003, REQ-USG-C-001, and the ordering in § 7 |
| QA | Each change provable before it exists | RED-first oracles: REQ-ROS-C-002, REQ-IDN-C-001, REQ-STSH-C-002, REQ-MIG-C-002, REQ-MIG-C-003 |
| Operations | The prior-configuration witness, the frozen migration fixtures, the soft `panel-goldens` gate | REQ-STSH-B-003, REQ-USG-C-002, REQ-MIG-C-001, REQ-ROS-C-001 |
| Security | No secret, digest or email in any new output; the uuid on the wire under issue #15 | REQ-STSH-C-001, REQ-NTC-C-001, REQ-WIRE-C-001, REQ-WIRE-C-002 |
| Support: the operator diagnosing later | Reading back what happened | F3's durable line; the swap-line identity (REQ-EVT-B-001) |
| Data | Usage history kept per account across relabels, nothing lost or merged | REQ-USG-B-001, REQ-USG-C-001 |

Three conflicts between perspectives, and how the requirements resolve each:

- **Privacy against disclosure.** Telling the operator which account a login captured could name it
  by its email, which issue #15 forbids. The label wins: replies name the previous label, never the
  harvested email (REQ-NTC-C-001).
- **Completeness against noise.** A report on every write that touches a duplicate label would train
  the operator to dismiss it. Import's before-and-after rule (§ 2.3) reports only a collision the
  write created (REQ-ROS-B-002).
- **History continuity against re-keying.** Moving the usage store off the label could drop or
  misattribute history. The migration keeps every sample and attributes only what resolves to one
  account (REQ-USG-C-001).

## 6. Risks and Mitigations

| Risk | Likelihood | Impact | Mitigation |
|---|---|---|---|
| Reports fire on writes that created nothing, and the operator learns to dismiss them | Medium | High | REQ-ROS-B-002, REQ-ROS-Q-001 |
| A new acknowledgement value breaks the Swift decoders, which throw on an unknown `result` or reason, while the only gate that compares panels is soft | High without the gate | High: a menu-bar capture shows an error, as #1474 already does for `refreshed` | REQ-ROS-I-001, REQ-ROS-C-001; #1474 first (§ 7) |
| A reply or a report leaks an email or a credential digest | Low | High: a public repository and durable logs | REQ-NTC-C-001, REQ-STSH-C-001; the issue #15 meter |
| F4's `STATUS_SCHEMA_VERSION` minor bump collides with #1457's claim on the next minor, or with #1303 or #1021 | Medium | Medium | REQ-WIRE-C-003; § 7 ordering |
| The usage-store re-key loses or merges history, or changes what the prior-configuration witness reads | Medium | High: the witness guards against the 2026-08-27 roster collapse | REQ-USG-C-001, REQ-USG-C-002 |
| F3 detects by byte equality, which a refresh breaks (A-2), so detection anywhere but the creating write misses pairs | Medium | Medium | REQ-STSH-Q-001 |
| Confusable detection, if adopted, flags the operator's real hyphen pairs | Medium | Medium | REQ-LBL-B-001 and its BUT NOT |
| A refusal, if a policy chooses one, lands after the stash write and leaves an orphan stash | Medium | Medium | REQ-IDN-C-002; AC-ROS-001 |
| A reaper, if chosen, races an in-flight capture between its stash write and its roster save, or erases the witness | Medium | High | REQ-STSH-B-003 |
| F5 or F6 changes the artifact format and breaks the frozen `FORMAT_VERSION` fixtures | Low | High | REQ-MIG-C-001 |
| Ratified text is corrected by a silent rewrite, losing what was ratified | Medium | Medium | REQ-DOC-C-001, after #1116's precedent |
| Doc fixes renumber line anchors, which the next commit breaks again | High | Low | REQ-DOC-B-002 (symbol anchors); #1476 |
| F7's walk never happens, so the premises stay unmeasured and F2's email option stays blocked | Medium | Medium | MET-R-001's gate; one walk serves #1020, #1542 and #1546 too |
| Stage 2 builds on pipeline choices the operator would reject, because every requirement is unratified | Medium | High | `dor_status: failed` blocks scoping until ratification (§ 14c) |

## 7. Dependencies

| Item | Relation | Effect on this PRD |
|---|---|---|
| #1474 (open): the Swift `CaptureAck` decoder has no `refreshed` arm | Prerequisite | The socket halves of F1 and F2 cannot reach the panel on a refresh until it lands |
| #1441 (closed) and ADR-0036 | Constraint | A new capture-time refusal composes with the existing refusal vocabulary and its Swift parity gate |
| #1457, #1303, #1021 (open): claimants on the `status` / `watch` wire; #1457 claims the next minor | Coordination | F4's minor bump is sequenced with them |
| #1535, then #1020 (open) | Ordering | #1535 lands before #1020 moves the login argv; F2's email option, if chosen, needs premise 3 measured on that argv |
| #1020, #1542, #1546 (open) | Shared walk | One operator-run real login serves them and F7 |
| #1091 (open) | Owner | R-15's correction (§ 2.11), and R-15's missing requirement on the `capture` / `login` write edge |
| #1476 (open) | Owner | The corpus-wide line-citation policy; REQ-DOC-B-002 defers to it if it decides first |
| #1379 (open) | Collision | It edits the same `LOGIN_USAGE` constant as REQ-DOC-B-003 |
| #1272, #1424 (open) | Neighbours | The #1009-scrub drift class; #1424 may already hold rows for the stale anchors (not checked) |
| #1415, #1548 (open) | Neighbours | #1415's stale `cli.rs` references stay out of scope; #1548 near-duplicates #1415 |
| #1466 (open) | Constraint | The witness is permanent and machine-global; F4's re-key and F10's reaping both touch a witness source |
| #1011, #1482, #1483 (open) | Neighbours | No `login.lock`, the login lost update, path-keyed locks; F10's orphans partly come from them |
| #737 (open) | Downstream | F3 makes its gate observable; MET-R-003 feeds its decision |
| #846 (open) | Neighbour | Email-shaped labels, and a rule for when a label counts as an address (F2's email option) |
| #939, #930 (open) | Neighbours | Panel identity cues for a degenerate label: the spoken cue and the next-swap callout (REQ-ROW-C-002) |
| #1018 (open) | Precedent | #1018 proposes an `enabled` map on `config-set` keyed by uuid, mirroring the `labels` map, which is keyed by uuid today (`ConfigSetCommand`, `src/daemon/socket.rs:546-551`) |
| #1167 (open) | Constraint | New operator prose reaches no framing scan unless routed to one (REQ-NTC-C-002) |
| #1116 (open) | Precedent | Ratified text is corrected by a dated amendment note |
| #791 (open) | Constraint | "account labels only, no identifiers", reconciled by REQ-WIRE-C-002 |
| #15 (closed) | Constraint | The redaction meter (§ 2.6) |
| #26, #27 (open) | Boundary | The Linux and Windows credential stores |
| ADR-0028 | Constraint | Online identity stays deferred; F3 reports and never chooses |
| ADR-0032 | Corrected by F9 | Its prerequisite 1 is stale (§ 2.11) |
| R-6, R-6a, AC-6 | Parents | F1 extends R-6; a refusal needs AC-6 re-ratified |
| `docs/requirements/gui-cli-capability-parity.md` R-1 | Constraint | Menu-bar parity for every CLI report |
| `docs/specs/capture-rejection-parity.feature.md` | Constraint | Rejection-tag parity, and the soft `panel-goldens` trap |

Ordering inside this PRD:

1. #1474 lands before the socket halves of F1 and F2.
2. F7's measurements come before F2's email option can be chosen, and before any requirement relies
   on premise 1 or premise 2.
3. F8's decision record comes before REQ-LBL-B-001.
4. F10's decision record comes before REQ-STSH-B-003 or REQ-STSH-B-004.
5. The panel mock frame (REQ-ROW-C-002) comes before the panel half of F4.
6. F4's wire identifier (REQ-WIRE-I-001) comes before the panel row can key on it (REQ-ROW-B-001).

## 8. Open Questions

### 8.1 Decisions only the operator can make — each kept open until the operator rules; two ruled on 2026-11-03

No requirement in § 10 assumes an answer to any of these, except REQ-DOC-B-003 (§ 10.9), whose wording
takes the keep-rename option of `login-mismatch-policy` (the solution design's § 14.6 item 14 records the
assumption). Each one names the requirements that read its answer. Two decisions were ruled by the operator on
2026-11-03 and carry a Ruling paragraph below; REQ-DOC-B-003's keep-rename wording is falsified by the refuse
ruling and is re-keyed with #1570 (§ 10.9).

**`duplicate-label-policy`**: warn or refuse when `capture`, `login`, the socket `capture` or
`config-set` creates a duplicate label.

| Option | What it costs |
|---|---|
| Warn: a report on the completed write | It extends ratified R-6 and needs no new decision. The duplicate still exists, and resolution keeps refusing it (exit 6) until the operator renames one |
| Refuse: before any write | It contradicts ratified AC-6, "BUT NOT by enforcing label uniqueness", so AC-6 needs re-ratification first. A refused `login` spends the operator's interactive login; the refusal has to precede the stash write; and it adds a rejection tag the Swift decoder must model |

Under every option, REQ-ROS-B-001 (never silent) and REQ-ROS-B-002 (the calibration) hold. The answer
fixes the assertion form of REQ-ROS-C-002's oracles.

**Ruling (the operator, 2026-11-03).** Refuse. The operator's words, verbatim, from the conversation that drove
the scoping pull request to merge: "refuse unless `--rename` or `--force` is added, be defensive rather than
'oh and btw we renamed shit'", and on this decision: "it's impossible to overwrite an existing ACC_1 with
ACC_2 under same label w/o adding `--overwrite` or removing ACC_1 and alike. (or `--force`?)". Reading
recorded by the `/scope` orchestrator, pipeline-authored and to be confirmed at ratification: a write that
would make a label name a second account is refused before any write, on CLI `capture`, CLI `login`, the socket
`capture` and `config-set`, with the `label-in-use` rejection. The override flag is `--overwrite`, the operator's own word, confirmed when they
accepted the recommendation to prefer it over `--force` ("proceed as recommended and propagate/update",
2026-11-03); the recommendation's ground was that the CLI's `use --force` already means "switch past the safety
check" while its existing `import --overwrite` already means "replace what is there". Under `--overwrite` the new account replaces the existing
holder of the label, read as that holder's roster row removed and its stash deleted in the same write, so
that nothing is orphaned; the other way through is removing or relabelling the holder first. Consequences:
AC-6 of `docs/requirements/migration-credential-portability.md` is re-ratified as narrowed (its amendment
note of 2026-11-03), since the four write paths now enforce uniqueness; REQ-ROS-C-002's oracles assert the
refusal; a refused `login` spends the interactive login, so the refusal precedes the stash write, as the
option's cost line says. Open, carried in the scope brief's register as `duplicate-label-policy-ruling`: the
replaced account's fate under `--overwrite`, which is the orchestrator's reading above; how the socket
`capture` and `config-set` carry the override; and whether `import` follows the ruling, where AC-6 still
answers warn-and-never-refuse.

**Settled for the re-authoring (the orchestrator, 2026-11-03), ratification-pending.** Design § 12.3 fixes
the cross-item behaviour of the items that read this answer, after a fresh-context critique. The override
exists on CLI `capture` and `login` only: the socket `capture` and `config-set` refuse with no override
(C-6, C-7), which corrects the reading above where it implied `--overwrite` reaches all four paths.
Overwrite consent replaces exactly one holder, in place, and only when the captured account is new, so the
roster count never drops and the append-only reload floor of `docs/requirements/roster-loss-prevention.md`
R-3 still holds (C-5). A label two or more accounts carry is refused whatever the flags, the way `remove`
refuses a duplicated label (C-5). A uuid-derived label is made free with a numeric suffix rather than
refused (C-2). `import` stays open.

**`login-mismatch-policy`**: what `login <label>` does when the captured account is not the one the
label names.

| Option | What it costs |
|---|---|
| Keep the rename, as today | Nothing beyond REQ-IDN-B-001's disclosure. The accidental-overwrite risk stays accepted, as REQ-LGN-B-004's stamp accepts it, but that stamp matches on the label (§ 8.2) |
| Warn when a revive renames an account | A report on every relabel, including the deliberate ones, where the operator runs `login <new name>` to rename |
| Refuse a revive that renames an account | It blocks the deliberate rename path that `recapture_updates_the_label_when_a_new_one_is_given` pins as a feature, so the operator renames through `config-set` instead; and it contradicts REQ-LGN-B-004's "refreshes it (idempotent)" |
| Compare an email-shaped label with the captured `emailAddress` | It needs a rule for when a label counts as an address (#846) and premise 3 measured (F7), since the planned `auth login` argv may not write `emailAddress`. The address itself stays out of every output (issue #15), so only the comparison's result can be shown |

REQ-IDN-B-001, which names the previous label, holds under every option, as the seed proposes; under
the first option it is the only change. If the operator wants the reply itself unchanged, REQ-IDN-B-001
is struck.

**Ruling (the operator, 2026-11-03).** Refuse the renaming revive unless `--rename` is given. The operator's
words are quoted under `duplicate-label-policy` above; `--rename` is their word for this case. Reading
recorded by the `/scope` orchestrator, pipeline-authored and to be confirmed at ratification: a
`login <label>` whose captured account is not the one the label names is refused before the stash write
unless `--rename` is given, with the `relabels-another-account` rejection; with the flag, the revive renames
the account and the reply discloses the previous label, so REQ-IDN-B-001 holds. A bare `login <label>` that
lands on the account the label names refreshes it, so REQ-LGN-B-004's "refreshes it (idempotent)" holds for
the same-label case; the deliberate rename path that `recapture_updates_the_label_when_a_new_one_is_given`
pins stays reachable through the flag, so that test gains the flag rather than being struck. Open, carried
in the scope brief's register as `login-mismatch-policy-ruling`: how the socket `capture` carries the
override, since the socket has no `login` verb and relabels through its `capture` (#1559). A `login` that would
both rename an account and make its label name a second account meets both rulings; design § 12.3 C-4
settles it: no flag makes that one write, and the operator frees the label first.

**Settled for the re-authoring (the orchestrator, 2026-11-03), ratification-pending.** The rename gate covers
CLI `capture` and the socket `capture` too, which share `plan_capture` with `login`, on the operator's own
reason (design § 12.3, C-3). The refusal's trigger is a rename: the login lands on a rostered account and
gives it a label different from the one it carries; a login that lands on a new account under a label another
account carries is `duplicate-label-policy`'s case. With no label given, a rostered account is refreshed without
the #447 prompt,
so pressing Enter can never rename it (C-3). The socket `capture` carries no override: the panel renames
through Settings (C-6).

**`label-normalization-policy`**: what normalization means, if anything.

| Option | What it costs |
|---|---|
| None, as today | Byte-distinct look-alikes stay distinct and unflagged |
| Case folding | `Alice@x.com` and `alice@x.com` become one label for resolution; existing rosters may gain an ambiguity |
| Punctuation folding | It merges the operator's real hyphen pairs (§ 2.2), so it is wrong as a default |
| Unicode normalization, NFC for example | Low cost; invisible unless labels carry combining marks |
| Confusable detection that reports and never merges, for example by the skeletons of Unicode Technical Standard #39 | A dependency or a table to maintain, which must not flag the hyphen pairs |

The answer feeds REQ-LBL-C-001's record; REQ-LBL-B-001 applies only where that record adopts
detection.

**`import-duplicate-uuid-policy`**: refuse or warn on an import payload whose credential list carries
two entries for one uuid, and, as a same-class addition, an entry that names no roster account.

| Option | What it costs |
|---|---|
| Refuse the whole payload | The safest; a hand-built artifact must be fixed before any account imports |
| Warn, and import that account roster-only, as "needs re-login" | A partial import; the operator logs that account in again |
| Warn, and keep today's last-entry-wins | Audible, but a credential is still picked silently, by position |

The answer feeds REQ-MIG-B-001, REQ-MIG-B-002 and REQ-MIG-C-002.

**`orphan-stash-policy`**: reap, report or accept an orphan `Sessiometer/<uuid>` stash. It is now
established that nothing reaps or reports one (§ 2.12).

| Option | What it costs |
|---|---|
| Reap | It must never touch a stash an in-flight write is about to roster, nor erase the witness on a machine whose `config.toml` is absent (ADR-0036, #1466); it deletes keychain material irreversibly |
| Report | A metadata-only listing, as the witness already reads that namespace without decrypting; cleanup stays with the operator |
| Accept | No code; a decision record that says so replaces the comments that call an orphan "harmless" (REQ-STSH-C-004) |

The answer feeds REQ-STSH-C-004, REQ-STSH-B-003 and REQ-STSH-B-004.

**`prd-placement`**: recorded here, and ratification-pending; also listed in § 8.4 (item 11) and in the scope brief's register, while #1550's decision list does not carry it. This run wrote a new umbrella PRD at
`docs/requirements/account-label-integrity.md`. The alternatives were merging into
`docs/requirements/roster-loss-prevention.md` (scope `roster-integrity`) or into
`docs/requirements/migration-credential-portability.md`, the home of R-6. The orchestrator chose a
new PRD because these gaps span every entry path, the wire and the panel, and because adding
unratified requirements to a PRD whose requirements are ratified would blur that PRD's status.

### 8.2 Contradictions and ambiguities surfaced, not resolved here

- 🔴 **REQ-LGN-B-004 against the code.** The ratified stamp says "A bare `login` for an existing label
  refreshes it"; the code refreshes on the uuid and renames on the label. Surfaced under
  `login-mismatch-policy`.
- 🔴 **R-15 against the code.** R-15 says the uuid's shape and length are unchecked; the code checks
  both. Routed to #1091.
- 🟡 **D-LGN-6's scope.** Its "never hard-error, never prompt" covers an omitted label, by its own
  text, and `capture` has prompted since #447. A refusal therefore contradicts AC-6, not D-LGN-6.
- 🟡 **R-5's wording against its implementation.** R-5 refuses where "the two reads disagree"; the code
  carries one read, so the guard holds by construction. #1482's body still relies on a two-read check
  that no longer exists; that is a dependency fact, and #1482 owns it.
- 🟡 **The label's documented intent against practice.** The `label` field's doc asks the operator to
  keep it "a **non-PII nickname** (`work`, `spare`), never an email or username (issue #404)", while
  the capture prompt pre-fills the email (#447) and D-LGN-6 allows "Local label may default to the
  account email". This is #846's territory; noted here, not resolved.

### 8.3 Design and investigation questions for Stage 2

- **DQ-1.** Which identifier F4 carries on the wire, the `account_uuid` itself or a value derived from
  it, and how the change reconciles the status wire's "never … account-uuid (issue #15)" comments and
  #791's acceptance line with issue #15's meter, the `config-get` precedent and the uuid-shaped
  derived labels (§ 2.6). The umbrella #1550 and the scope brief name this decision `account-key-form`.
- **DQ-2.** How usage history recorded under a label that maps to no current account, or to several,
  is kept and shown after the re-key.
- **DQ-3.** Whether the `stats --json` shape changes, and so whether `stats`' own schema and the Swift
  `StatsWire` mirror move.
- **DQ-4.** The panel frame for two rows with one label, including its spoken identity cue (#939) and
  the next-swap callout (#930). The mock must author it first.
- **DQ-5.** Whether F3 also sweeps the existing roster once, at daemon start, to catch pairs no refresh
  has split yet, and what that costs in keychain reads.
- **DQ-6.** Which channel carries F3's report for a daemon-side write: the event log, the socket
  `capture` acknowledgement, or both.
- **DQ-7.** Whether `export` should also compare a stash's `oauthAccount` uuid with its entry's uuid,
  so that a mismatch never reaches an artifact.
- **DQ-8.** Whether premise 2 is measurable with the accounts the operator holds, since it needs one
  person's account in two organisations.
- **DQ-9.** How many orphan stash items exist on the operator's machine: a metadata-only census the
  operator can run.
- **DQ-10.** Whether the "in rotation" count becomes the count of enabled entries, or keeps the row
  count under a wording that names the roster (REQ-NTC-B-001).

### 8.4 Pipeline decisions, ratification-pending

1. The enumeration into ten gaps, and the selection "the explicit gaps only, no enrichment" (seed).
2. The problem framing (§ 2).
3. The appetite: Big Batch, six weeks, with its circuit breaker (§ 3).
4. The success metrics and their decision gates (§ 4).
5. One feature per gap, plus these same-class additions: `capture` beside `login` in F2
   (REQ-IDN-B-001), the orphan credential entry in F5 (REQ-MIG-B-002), premise 4 in F7, the swap-line
   identity in F4 (REQ-EVT-B-001), and A-1's regrade in F9 (REQ-DOC-B-004).
6. F3 reports and never chooses; F6 refuses a mismatched entry, as the seed's draft proposes.
7. F4 states an outcome, an identity a relabel does not change, rather than prescribing the uuid.
8. The out-of-scope list and the never-build list (§ 3).
9. The transcript is replicated with synthetic labels of the same shape in place of the operator's
   addresses, and with the session id elided (§ 2.2; the substitution is the scope brief's
   `prd-privacy-synthetic-labels` row of 2026-10-27).
10. Feature anchors are HTML `<a id>` elements, because GitHub renders the template's `{#…}` heading
    syntax literally. When this PRD was written (2026-10-26) no committed PRD here had a feature anchor of
    either kind; `docs/requirements/optional-quota-windows.md` (created 2026-11-01) uses the `{#feature-…}`
    heading form, so both forms are now in the tree.
11. `prd-placement` (§ 8.1).
12. The readings beside the operator's rulings in § 8.1 (2026-11-03): the replaced account's fate under
    `--overwrite`, the socket paths' carriage of the override, and `import`'s standing. The rulings themselves, the
    flag names included, are the operator's.

## 9. Object Model

### 9.1 Objects

| Object | Code | Identity | Description | Lifecycle |
|---|---|---|---|---|
| RosterEntry | ROS | `account_uuid` | One account in `config.toml`'s roster: `account_uuid`, `label`, `enabled` | absent → rostered and enabled ⇄ parked → removed |
| AccountLabel | LBL | the label string of one entry | The operator's handle for an account; non-unique by ratified design | derived (the uuid) or given → relabelled, any number of times; carried by one entry or by several |
| CapturedIdentity | IDN | `oauthAccount.accountUuid`, read at a capture or login | The account a capture or login actually harvested | read → matched to an entry or new → persisted with the stash |
| StashedCredential | STSH | keychain service `Sessiometer/<account_uuid>` | One entry's credential and `oauthAccount`, stored in the keychain | written → refreshed, its bytes rotating → deleted by `remove`; or orphaned, with no roster row |
| MigrationArtifact | MIG | the artifact file | A `Payload`: the rendered `config.toml` plus a credential list keyed by uuid | exported → imported, per entry: imported, overwritten, skipped or failed |
| OperatorNotice | NTC | the write that emitted it, and its kind | A reply line, report or refusal that a write produces | emitted at the write → read by the operator; durable only when also logged |
| AccountStatusLine | WIRE | its position in the frame and its label today; an account identity after F4 | One account's line on the `status` / `watch` wire | rebuilt for every snapshot |
| PanelAccountRow | ROW | its SwiftUI `id`, the label today | One account row in the menu-bar panel | rendered from each `watch` frame |
| UsageHistory | USG | each sample's `acct`, the label today | Usage samples and rollups per account | appended per poll → rolled up → pruned |
| EventLogLine | EVT | timestamp and event name | A durable `key=value` line in the daemon's event log | appended, then read by `log`, `stats` and `reliability` |
| IdentityMeasurement | MSR | premise number and Claude Code version | A recorded real-login measurement of an identity premise | owed → measured, or recorded as unmeasurable → superseded under a later version |
| ProjectRecord | DOC | file path and anchor | A committed document, or a usage text, whose claims the code must match | written → drifted → amended with a dated note |

### 9.2 Relationships

| From | To | Type | Cardinality | Navigation |
|---|---|---|---|---|
| RosterEntry | AccountLabel | contains | 1 : 1 | entry → its label |
| AccountLabel | RosterEntry | references | 1 : many, since labels are not unique | label → its bearers, through resolution |
| RosterEntry | StashedCredential | contains | 1 : 1, by the derived service name | entry → `Account::stash` |
| StashedCredential | RosterEntry | references | 1 : 0..1, an orphan when 0 | none today (F10) |
| CapturedIdentity | RosterEntry | references | 1 : 0..1, new or matched | `plan_capture` |
| StashedCredential | StashedCredential | peer-to-peer | many : many, byte-identical pairs | none today (F3) |
| MigrationArtifact | RosterEntry | contains | 1 : many | the roster half |
| MigrationArtifact | StashedCredential | contains | 1 : many, keyed by uuid | the credential half |
| AccountStatusLine | RosterEntry | references | 1 : 1 | by label only today (F4) |
| PanelAccountRow | AccountStatusLine | references | 1 : 1 | by label today (F4) |
| UsageHistory | AccountLabel | belongs-to | many : 1 | `acct` today (F4) |
| EventLogLine | AccountLabel | references | many : 1..2 | `from=` / `to=` today (F4) |
| OperatorNotice | RosterEntry | references | 1 : 1..2 | by label, and by uuid where labels are equal |
| IdentityMeasurement | CapturedIdentity | references | many : 1 | by premise |
| ProjectRecord | RosterEntry | references | many : many | by the code anchors it cites |

### 9.3 CTA inventory

Each call to action, the feature that owns it, its variants, its precondition and its outcome. A
feature column of `—` marks an existing CTA that no feature here changes.

| Object | CTA | Feature | Variants | Precondition | Outcome |
|---|---|---|---|---|---|
| RosterEntry | Create | F1 | CLI `capture`, CLI `login`, socket `capture`, `import` | a harvested or imported uuid the roster does not hold | the entry is appended under its label; a collision it creates is reported (since 2026-11-03: on the three governed paths a collision is refused before the write, and `import` keeps its own rule, design § 12.3 C-1, C-2) |
| RosterEntry | Edit (relabel) | F1 ! F2 | a revive with a label through `capture`, `login` or socket `capture`; `config-set` | the entry exists | the label is replaced only with rename consent, and never onto a label another account carries (design § 12.3, C-3, C-4); on a CLI revive made with `--rename` the previous label is named (F2); a socket revive that would relabel is refused (C-6) |
| RosterEntry | View | F4 | `list`, `status`, the panel | the daemon serves `status` | one line or row per entry, keyed on an identity a relabel does not change |
| RosterEntry | Delete | — | `remove` | a query that resolves to one entry | the entry is removed, then its stash is deleted; F10 covers the orphan a failure leaves |
| AccountLabel | Compare (exact) | F1 | the before-and-after bearer count | a write that sets a label | a collision is detected when REQ-ROS-B-002 counts one (since 2026-11-03, per account on the governed write paths, design § 12.3 Terms) |
| AccountLabel | Compare (confusable) | F8 | the rule F8's record adopts | the record adopts detection | a confusable pair is detected |
| CapturedIdentity | Match | F2 | `plan_capture`'s uuid lookup | a completed capture or login | the matched entry, or a new one |
| CapturedIdentity | View (disclose) | F2 | the reply on a relabelling revive | a match whose label changes | the previous label is named on a CLI revive made with `--rename`; a socket relabel is refused (design § 12.3, C-3, C-6) |
| CapturedIdentity | Create (measure) | F7 | the operator's real login | the operator's walk | a measured premise |
| StashedCredential | Create (write) | F3 ! F6 | `capture`, `login`, socket `capture`, `import` | a planned entry | the stash is written; F6 refuses a mismatched import entry before this |
| StashedCredential | Compare | F3 | in memory, against every other entry | a credential was just written | a byte-identical pair is detected |
| StashedCredential | View (metadata) | F10 | a listing by account uuid | the policy is to report | orphan items are named |
| StashedCredential | Delete (reap) | F10 | the reaper | the policy is to reap | orphan items are deleted |
| MigrationArtifact | Import (payload check) | F5 | before the swap lock | an artifact is read | a duplicate or unmatched credential entry is reported |
| MigrationArtifact | Import (entry identity check) | F6 | per entry | an entry carries a credential | a mismatched entry is refused |
| OperatorNotice | Create (collision) | F1 | CLI line; socket acknowledgement | F1 detects a collision | the operator is told (since 2026-11-03: the write is refused, and the CLI refusal names the holder, design § 12.3 C-9) |
| OperatorNotice | View (collision) | F1 | terminal; panel | a report was emitted | the label is shown |
| OperatorNotice | Create (relabel) | F2 | CLI reply; `refreshed` acknowledgement | a relabelling revive | the previous label is carried in the CLI reply only; the `refreshed` acknowledgement never carries one (design § 12.3, C-6) |
| OperatorNotice | View (relabel) | F2 | terminal; panel, once #1474 lands | a disclosure was emitted | both labels are shown (since 2026-11-03: only on a CLI revive made with `--rename`; the panel never shows a previous label, design § 12.3 C-6, C-8) |
| OperatorNotice | Create (shared credential) | F3 | the writing surface | F3 detects a pair | the operator is told |
| OperatorNotice | Create (payload) | F5 | `import`'s report | F5 detects a duplicate or unmatched entry | the operator is told |
| OperatorNotice | Create (identity mismatch) | F6 | `import`'s per-account report | F6 refuses an entry | the account is reported failed |
| OperatorNotice | Create (confusable) | F8 | the writing surface | F8's rule detects a pair | the operator is told |
| AccountStatusLine | Create | F4 | `status` reply; `watch` snapshot and heartbeat | the daemon builds a snapshot | each line carries an account identity |
| AccountStatusLine | View | F4 | the `status` render; the Swift decoder | a client reads the wire | lines stay distinct under one label |
| PanelAccountRow | Render | F4 | the account list | a `watch` frame arrives | one row per account line |
| UsageHistory | Append | F4 | each poll | an account was polled | the sample is keyed on the account identity |
| UsageHistory | Migrate | F4 | a one-time re-key | samples keyed by label exist | samples kept, attributed only where unambiguous |
| UsageHistory | View | F4 | `stats` | history exists | history per account |
| EventLogLine | Append (shared credential) | F3 | one line per detection | F3 detects a pair | a durable record |
| EventLogLine | Append (swap identity) | F4 | `event=swap`, `event=emergency_swap` | a swap | same-label accounts are distinguishable |
| IdentityMeasurement | Create | F7 | one record per premise | a walk was taken | the result, version and date |
| IdentityMeasurement | Edit (supersede) | F7 | under a later Claude Code version | a new walk | the newer result, the older kept |
| ProjectRecord | Create (normalization record) | F8 | a decision record | the operator decides | what makes two labels the same |
| ProjectRecord | Edit (amend) | F9 | dated amendment; symbol anchor; usage text | a drift site | the record matches the code |
| ProjectRecord | Create (orphan-stash record) | F10 | a decision record | the operator decides | reap, report or accept |

Excluded CTAs, each with its reason:

- **AccountLabel Create, View, Edit, Delete**: a label is created, shown and changed through its
  entry's CTAs, and never exists without its entry, so it has no Delete of its own.
- **CapturedIdentity Edit, Delete**: an identity is harvested, never edited, and it is transient.
- **StashedCredential Edit (refresh)**: existing behaviour that no feature changes.
- **MigrationArtifact Create (export)**: DQ-7 asks whether `export` compares; no requirement yet.
  **View, Edit, Delete**: the artifact is the operator's encrypted, immutable file.
- **OperatorNotice Edit, Delete**: a notice is ephemeral, or an append-only log line.
- **AccountStatusLine, PanelAccountRow Edit, Delete**: both are derived on every frame.
- **EventLogLine Edit, Delete**: the log is append-only.
- **IdentityMeasurement Delete**: superseded records are kept as history.
- **ProjectRecord Delete**: a record is amended, never deleted.

### 9.4 Attribute priority

| Object | Attribute | Priority | Display context |
|---|---|---|---|
| RosterEntry | `label` | Primary | every list, row, reply and log line |
| RosterEntry | `enabled` | Secondary | `list`, `status`, the panel |
| RosterEntry | `account_uuid` | Tertiary | `list`; where a label is ambiguous; F4 keys surfaces on it without displaying it |
| CapturedIdentity | `accountUuid` | Metadata | never displayed by F2; it keys the match |
| CapturedIdentity | `emailAddress` | never displayed | read only by the capture prompt's pre-fill; issue #15 |
| StashedCredential | the credential bytes | never displayed | compared in memory only (F3) |
| StashedCredential | the service name | Metadata | F10's report names the uuid inside it |
| UsageHistory | usage percentages | Primary | `stats` |
| UsageHistory | the account key | Metadata | not displayed |
| AccountStatusLine | the account identity | Metadata | carried, not displayed (§ 3, Bucket-3) |
| EventLogLine | `from=`, `to=` | Primary | `log`; read by `stats` and `reliability` |
| IdentityMeasurement | result, Claude Code version, date | Primary | the record |

### 9.5 Feature views

| Feature | Objects and CTAs |
|---|---|
| F1 `label-collision-notice` | RosterEntry Create, Edit; AccountLabel Compare (exact); OperatorNotice Create, View (collision) |
| F2 `login-landing-disclosure` | RosterEntry Edit; CapturedIdentity Match, View; OperatorNotice Create, View (relabel) |
| F3 `shared-credential-detection` | StashedCredential Create, Compare; OperatorNotice Create (shared credential); EventLogLine Append (shared credential) |
| F4 `identity-keyed-surfaces` | RosterEntry View; AccountStatusLine Create, View; PanelAccountRow Render; UsageHistory Append, Migrate, View; EventLogLine Append (swap identity) |
| F5 `import-payload-uniqueness` | MigrationArtifact Import (payload check); OperatorNotice Create (payload) |
| F6 `import-identity-check` | MigrationArtifact Import (entry identity check); StashedCredential Create; OperatorNotice Create (identity mismatch) |
| F7 `identity-premise-measurement` | CapturedIdentity Create (measure); IdentityMeasurement Create, Edit |
| F8 `label-normalization` | AccountLabel Compare (confusable); OperatorNotice Create (confusable); ProjectRecord Create |
| F9 `uniqueness-doc-accuracy` | ProjectRecord Edit (amend) |
| F10 `orphan-stash-disposition` | StashedCredential View (metadata), Delete (reap); ProjectRecord Create |

### 9.6 Entity-Feature Matrix

Each cell counts the CTAs at that intersection. `!` marks a cell holding a CTA that two features
claim; the resolution for each follows the table.

| Object | F1 | F2 | F3 | F4 | F5 | F6 | F7 | F8 | F9 | F10 | Total CTAs |
|---|---|---|---|---|---|---|---|---|---|---|---|
| RosterEntry | 2 ! | 1 ! |  | 1 |  |  |  |  |  |  | 4 |
| AccountLabel | 1 |  |  |  |  |  |  | 1 |  |  | 2 |
| CapturedIdentity |  | 2 |  |  |  |  | 1 |  |  |  | 3 |
| StashedCredential |  |  | 2 ! |  |  | 1 ! |  |  |  | 2 | 5 |
| MigrationArtifact |  |  |  |  | 1 | 1 |  |  |  |  | 2 |
| OperatorNotice | 2 | 2 | 1 |  | 1 | 1 |  | 1 |  |  | 8 |
| AccountStatusLine |  |  |  | 2 |  |  |  |  |  |  | 2 |
| PanelAccountRow |  |  |  | 1 |  |  |  |  |  |  | 1 |
| UsageHistory |  |  |  | 3 |  |  |  |  |  |  | 3 |
| EventLogLine |  |  | 1 | 1 |  |  |  |  |  |  | 2 |
| IdentityMeasurement |  |  |  |  |  |  | 2 |  |  |  | 2 |
| ProjectRecord |  |  |  |  |  |  |  | 1 | 1 | 1 | 3 |
| **Total CTAs** | 5 | 5 | 4 | 8 | 2 | 3 | 3 | 3 | 1 | 3 | 37 |

- **RosterEntry Edit, claimed by F1 and F2.** One relabelling revive can both create a collision and
  rename a different account. Resolved as one CTA with conditional behaviour: the one reply carries
  F1's report when REQ-ROS-B-002 counts a collision, and F2's previous label when the revive changed
  the label. Since 2026-11-03 such a write cannot proceed whatever flags are given, because overwrite consent applies
  only to a new account (design § 12.3 C-4, C-5); the way through is to free the label first, then repeat
  the write with `--rename`, so a completed reply carries F2's previous label only, never F1's report.
- **StashedCredential Create, claimed by F3 and F6.** Resolved by order: F6 decides before the write,
  so a refused import entry writes nothing and F3 never compares it; F3 compares every credential that
  is written.
- AccountLabel Compare, MigrationArtifact Import, EventLogLine Append and OperatorNotice Create are
  split into feature-specific variants (§ 9.3), so none of them is a shared claim.

## 10. Requirements

Every requirement below is **pipeline-authored and ratification-pending**. Its `Origin` is
`AI-inferred-expansion`: the operator ratified the class of work ("/scope the gaps discovered") and
not any requirement's form. A requirement that extends a gap to a further site of the same class says
so in its `Origin` cell. Confidence follows the extraction scale: VALIDATED where the defect the
requirement answers is observed in the tree, ASSUMED where it rests on an inference or on the seed's
proposal, SPECULATIVE where it hangs on an open decision or a single inference. Statements use EARS;
acceptance criteria use Given / When / Then with BUT NOT; quality attributes use Planguage.

<a id="feature-label-collision-notice"></a>

### 10.1 F1 — Label collision notice

Answers gap `duplicate-label-silent-at-entry` (§ 2.3). The form of the report is
`duplicate-label-policy`'s to decide (§ 8.1); every requirement here holds under either answer.

| ID | EARS pattern | Statement | Priority | Origin | Confidence |
|---|---|---|---|---|---|
| REQ-ROS-B-001 | Event-driven | When a CLI `capture`, a CLI `login`, a socket `capture` command or a socket `config-set` command would create a label collision as REQ-ROS-B-002 defines it, the system shall report that collision to the operator on the surface that requested the write, naming the label, as a report on the completed write or as a refusal before any write, whichever `duplicate-label-policy` selects. | Must | `AI-inferred-expansion` | VALIDATED: the four paths are observed silent (§ 2.3); the form is the refusal since 2026-11-03 (design § 12.3, C-2), and a uuid-derived label is made free with a suffix rather than reported |
| REQ-ROS-B-002 | Ubiquitous | The system shall count a write as creating a label collision when, for some label, the number of roster entries carrying it after the write is above one and above the number carrying it before the write. Since 2026-11-03 the four governed write paths (CLI `capture`, CLI `login`, socket `capture`, `config-set`) count per account instead: a write creates a collision when it gives an account a label it did not carry before and another account carries that label after the write (design § 12.3, Terms); `import` keeps the count rule above. | Must | `AI-inferred-expansion` | ASSUMED: it reuses import's rule (§ 2.3); refined 2026-11-03 (design § 12.3, Terms): the count rule lets a save move an account onto a label two others already share, so the refuse form counts a collision per account, a label an account did not carry before and another account carries after the write |
| REQ-ROS-I-001 | Optional | Where the socket `capture` or `config-set` command reports a collision, the daemon shall carry the report in a form that the menu-bar app's `CaptureAck` and `ConfigSetAck` decoders, as changed in the same commit, decode without error. | Must | `AI-inferred-expansion` | VALIDATED: both decoders throw on a value they do not model (§ 6) |
| REQ-ROS-I-002 | Optional | Where the daemon reports a collision on the socket, the menu-bar app shall present it on the surface that requested the write, at parity with the CLI report. | Must | `AI-inferred-expansion` | ASSUMED: parity is ratified (`gui-cli-capability-parity` R-1); the presentation is not designed |
| REQ-ROS-C-001 | Ubiquitous | The `test` job shall gate the parity between every new Rust acknowledgement value or field and its Swift decoder case, as `every_rust_rejection_tag_has_a_swift_case` gates rejection tags today. | Must | `AI-inferred-expansion` | VALIDATED: `panel-goldens` runs every step with `continue-on-error` |
| REQ-ROS-C-002 | Ubiquitous | The project shall author four oracles before the change — `onboarding_a_new_uuid_under_a_label_another_account_bears_is_not_silent`, `a_revive_that_relabels_onto_another_accounts_label_is_not_silent`, one socket `capture` test and one `config-set` test of the same shape — each failing against the tree before the change and asserting the form `duplicate-label-policy` selects. | Must | `AI-inferred-expansion` | ASSUMED: the first two names are the investigation's proposal |
| REQ-ROS-Q-001 | Quality | The system shall meet the Planguage measure `CollisionReportPrecision` below. | Must | `AI-inferred-expansion` | ASSUMED |

**REQ-ROS-Q-001 — `CollisionReportPrecision`**

- **TAG**: CollisionReportPrecision
- **SCALE**: reports emitted under REQ-ROS-B-001 on writes that REQ-ROS-B-002 does not count as
  creating a collision
- **METER**: hermetic tests over the four paths, with four fixtures: one `config-set` that swaps the
  labels of two entries; a refresh of an entry whose label already had two bearers; a relabel to the
  label the entry already carries; an ordinary new account under a fresh label
- **PAST**: the four paths report nothing at all; import's rule reports nothing on its own fixtures of
  this kind
- **GOAL**: 0
- **CONSTRAINT**: any report on such a write is a defect, because it trains dismissal of the report
  that matters (§ 2.3)

**AC-ROS-001 — REQ-ROS-B-001, REQ-ROS-B-002**

> **Given** a roster that carries the label `work` on uuid `A`,
> **When** `capture work` harvests a new uuid `B`,
> **Then** the operator learns at that write that `work` now names two accounts, in the form
> `duplicate-label-policy` selects,
> **And** the report names the label `work`.
> **BUT NOT** a report when the harvested uuid is `A` itself, which refreshes the same account;
> **BUT NOT** a refusal that takes effect after the stash is written, which would leave an orphan stash;
> **BUT NOT** renaming either account, or naming either account's email.
> *Re-keyed 2026-11-03 (design § 12.3, C-2, C-5): the form is a refusal before any write, naming `work`,
> unless `--overwrite` is given, which replaces `A` because `B` is new and `A` is the only holder.*

**AC-ROS-002 — REQ-ROS-B-002, REQ-ROS-Q-001**

> **Given** entry `A` labelled `a` and entry `B` labelled `b`,
> **When** one `config-set` sends the labels `{A: "b", B: "a"}`,
> **Then** no collision is reported.
> **BUT NOT** a report for the state the write passes through between its two relabels.

**AC-ROS-003 — REQ-ROS-I-001, REQ-ROS-I-002, REQ-ROS-C-001**

> **Given** a menu-bar capture that creates a collision,
> **When** the daemon acknowledges it,
> **Then** the app decodes the acknowledgement and shows the collision on the capture card,
> **And** the `test` job fails whenever a Rust acknowledgement value has no Swift case.
> **BUT NOT** a decode error, which is what an unmodelled `result` value produces today (#1474);
> **BUT NOT** a `panel-goldens` result standing in for the parity gate.
> *Re-keyed 2026-11-03 (design § 12.3, C-6, C-9): the capture is refused with `label-in-use`, and the card
> shows the refusal; no capture completes with a collision.*

**AC-ROS-004 — REQ-ROS-C-002**

> **Given** the tree before the change,
> **When** the four oracles run,
> **Then** each one fails.
> **BUT NOT** accepted as evidence if any of them passes before the change.

**Invariants.** A label never changes an entry's `account_uuid`. A write that creates no collision
reports nothing.

**Verification.** Rust hermetic tests in the `test` job, with `msrv` and `deny` owed for any `src/**`
change; Swift decoder cases under the `swift` job, and an armed `panel-goldens` run if the panel's
appearance changes; #1474 first for the socket half.

<a id="feature-login-landing-disclosure"></a>

### 10.2 F2 — Login landing disclosure

Answers gap `login-lands-on-another-account` (§ 2.4). What a mismatched login does beyond disclosure
is `login-mismatch-policy`'s to decide (§ 8.1).

| ID | EARS pattern | Statement | Priority | Origin | Confidence |
|---|---|---|---|---|---|
| REQ-IDN-B-001 | Event-driven | When a `capture`, a `login` or a socket `capture` command matches an already-rostered account and gives it a label different from the one it carried, the system shall name in its reply both the label the account carried before the write and the label it carries after it. | Must | `AI-inferred-expansion`; `capture` and the socket `capture` are a same-class addition | VALIDATED: the reply cannot show the previous label (§ 2.4) |
| REQ-IDN-I-001 | Optional | Where the socket `capture` command refreshes an account under a different label, the daemon shall carry the previous label as an additive field of the `refreshed` acknowledgement. | Must | `AI-inferred-expansion` | VACUOUS since 2026-11-03: the socket `capture` never completes a rename (design § 12.3, C-6), so this condition never holds; the refusal's card names no account's label; a write that also collides is refused `label-in-use`, whose card names only the label asked for (C-6, C-9) |
| REQ-IDN-I-002 | Optional | Where the menu-bar app decodes a `refreshed` acknowledgement that carries a previous label, the app shall show both labels on the capture card. | Should | `AI-inferred-expansion` | VACUOUS since 2026-11-03, with REQ-IDN-I-001 (design § 12.3, C-6). Before that: ASSUMED: it waits on #1474 |
| REQ-IDN-C-001 | Ubiquitous | The project shall author, before the change, a test in which `login X` lands on a rostered account labelled `Y` and the reply names `Y`, failing against the tree before the change. | Must | `AI-inferred-expansion` | ASSUMED |
| REQ-IDN-C-002 | Optional | Where `login-mismatch-policy` selects a refusal, the system shall take that refusal before the credential stash is written, leaving no stash and no roster change. | Must, where chosen | `AI-inferred-expansion` | SPECULATIVE until 2026-11-03; `login-mismatch-policy` now selects the refusal (§ 8.1), so this requirement applies |

**AC-IDN-001 — REQ-IDN-B-001**

> **Given** a roster with `work` on uuid `A` and `spare` on uuid `B`,
> **When** `login work` harvests uuid `B`,
> **Then** the reply names `spare` as the label the account carried and `work` as the label it
> carries now,
> **And** REQ-ROS-B-001's report accompanies it, because `work` now names two accounts.
> **BUT NOT** the harvested email address, a token, or any credential-derived value;
> **BUT NOT** a changed reply when the label given equals the one the account already carries.
> *Re-keyed 2026-11-03 (design § 12.3, C-4, C-9): `login work` here renames `B` onto a label `A` carries,
> which no flag makes one write; the refusal names `spare` as the label the account carries and says
> `work` is carried by another account. REQ-IDN-B-001's disclosure is exercised by `login <free-label>
> --rename` landing on `B`.*

**AC-IDN-002 — REQ-IDN-B-001**

> **Given** a roster without uuid `C`,
> **When** `login work` harvests `C`,
> **Then** the reply is `Onboarded "work" (now N in rotation).`, plus REQ-ROS-B-001's report if
> another entry already carries `work`.
> **BUT NOT** a previous-label disclosure, because a new account has none.
> *Re-keyed 2026-11-03 (design § 12.3, C-2, C-5): if another entry already carries `work`, the login is
> refused unless `--overwrite` is given, which replaces that entry when it is the only holder.*

**AC-IDN-003 — REQ-IDN-C-002**

> **Given** `login-mismatch-policy` selects a refusal,
> **When** a login would rename another rostered account,
> **Then** the stash and the roster are left as they were, and the operator is told why.
> **BUT NOT** a refusal that takes effect after the stash write.

**AC-IDN-004 — REQ-IDN-I-001, REQ-IDN-I-002**

> **Given** a menu-bar capture that refreshes an account and changes its label from `spare` to `work`,
> **When** the daemon acknowledges it with `refreshed`,
> **Then** the acknowledgement carries `spare` as the previous label, in a field an older client
> ignores,
> **And** once #1474 lets the app decode `refreshed`, the capture card shows both labels.
> **BUT NOT** a new `result` value for the relabel, which today's decoder would throw on;
> **BUT NOT** the previous label shown before the app can decode `refreshed` at all.
> *Re-keyed 2026-11-03 (design § 12.3, C-6, C-9): the capture is refused with `relabels-another-account`
> before any write; the card names no account's label, since the watch snapshot cannot say for certain which account the capture landed on, and offers Settings to rename.*

**Invariants.** No reply names an email address the label does not carry, or any credential-derived
value. An onboarding reply keeps its verb and its count.

**Verification.** Hermetic tests over each caller of `plan_capture`: `run_capture`, `run_login` and
the socket capture; the `refreshed` field's Swift case lands with or after #1474.

<a id="feature-shared-credential-detection"></a>

### 10.3 F3 — Shared credential detection

Answers gap `one-credential-two-entries` (§ 2.5). It reports and never chooses, and it makes
ADR-0028's deferral trigger observable.

| ID | EARS pattern | Statement | Priority | Origin | Confidence |
|---|---|---|---|---|---|
| REQ-STSH-B-001 | Event-driven | When a `capture`, a `login`, a socket `capture` command or an `import` writes a credential into a roster entry's stash, the system shall compare that credential with the credential stashed under every other roster entry and detect any byte-identical match. | Must | `AI-inferred-expansion` | VALIDATED: no detector exists (§ 2.5) |
| REQ-STSH-B-002 | Event-driven | When REQ-STSH-B-001 detects a match, the system shall report both entries to the operator by label, and by account uuid where their labels are equal, on the surface that performed the write and as one durable event-log line, leaving the roster and the write unchanged. | Must | `AI-inferred-expansion` | ASSUMED: the daemon-side channel is DQ-6 |
| REQ-STSH-Q-001 | Quality | The system shall meet the Planguage measure `SharedCredentialDetectionWindow` below. | Must | `AI-inferred-expansion` | VALIDATED: refresh tokens rotate (A-2) |
| REQ-STSH-Q-002 | Quality | The system shall meet the Planguage measure `DetectionKeychainCost` below. | Must | `AI-inferred-expansion` | ASSUMED |
| REQ-STSH-C-001 | Ubiquitous | The system shall hold the comparison in memory, keeping every credential byte, digest and fingerprint out of every reply, log line, file and wire. | Must | `AI-inferred-expansion` | VALIDATED: issue #15's meter forbids a blob fingerprint |
| REQ-STSH-C-002 | Ubiquitous | The project shall author, before the change, a test that writes a credential byte-identical to another roster entry's and asserts the report, failing against the tree before the change. | Must | `AI-inferred-expansion` | ASSUMED |
| REQ-STSH-C-003 | Ubiquitous | The canary shall keep passing a canonical credential that matches the displayed account's own stash when the same bytes also sit under another entry, as `shared_token_under_both_stashes_passes_via_the_stash_a_first_order` pins. | Must | `AI-inferred-expansion` | VALIDATED: the pass is deliberate (§ 2.5) |

**REQ-STSH-Q-001 — `SharedCredentialDetectionWindow`**

- **TAG**: SharedCredentialDetectionWindow
- **SCALE**: the write at which a byte-identical pair is first reported, counted from the write that
  created the pair
- **METER**: a hermetic test that writes a credential identical to another entry's, then refreshes
  one of the two entries
- **PAST**: never reported (§ 2.5)
- **GOAL**: reported at the write that created the pair, before either entry is refreshed
- **CONSTRAINT**: a pair that a refresh splits before it is reported counts as a miss (A-2)

**REQ-STSH-Q-002 — `DetectionKeychainCost`**

- **TAG**: DetectionKeychainCost
- **SCALE**: keychain reads the detection adds, per write and per poll or swap
- **METER**: count the stash reads in a hermetic test with an instrumented stash
- **PAST**: 0, because nothing detects
- **GOAL**: at most one read per other roster entry per write; 0 per poll; 0 per swap

**AC-STSH-001 — REQ-STSH-B-001, REQ-STSH-B-002, REQ-STSH-C-001, REQ-STSH-C-003**

> **Given** entries `A` and `B`, where `A`'s stash holds the credential bytes `K`,
> **When** a `capture` writes `K` into `B`'s stash,
> **Then** the operator is told at that write, by label, that `A` and `B` hold one credential,
> **And** one event-log line records it.
> **BUT NOT** any byte, digest or fingerprint of `K`;
> **BUT NOT** choosing, merging, disabling or removing either entry;
> **BUT NOT** refusing the write;
> **BUT NOT** a change in any canary verdict.

**AC-STSH-002 — REQ-STSH-Q-002**

> **Given** a daemon that runs with F3,
> **When** it polls or swaps,
> **Then** it performs no keychain read that F3 added.
> **BUT NOT** a sweep that re-reads every stash on each poll.

**Invariants.** F3 reports; it never changes what is stored, nor which entry is active.

**Verification.** Hermetic tests with an in-memory stash; the issue #15 redaction meter over the new
report and the new event-log line.

<a id="feature-identity-keyed-surfaces"></a>

### 10.4 F4 — Identity-keyed surfaces

Answers gap `label-keyed-downstream-surfaces` (§ 2.6). It states the outcome, an identity a relabel
does not change, and leaves the identifier's form to DQ-1.

| ID | EARS pattern | Statement | Priority | Origin | Confidence |
|---|---|---|---|---|---|
| REQ-WIRE-I-001 | Ubiquitous | The `status` reply and both `watch` frames shall carry, on each account line, an identifier that is unique per roster entry and unchanged by a relabel. | Must | `AI-inferred-expansion` | ASSUMED: the identifier's form is DQ-1 |
| REQ-WIRE-C-001 | Ubiquitous | The identifier on the wire shall pass issue #15's redaction meter. | Must | `AI-inferred-expansion` | VALIDATED: the meter runs in CI |
| REQ-WIRE-C-002 | Ubiquitous | The change that adds the identifier shall reconcile, in the same commit, every in-code statement that keeps the account uuid off the status wire — the `canary` field's doc, its Swift mirror and the panel's canary-banner doc — and #791's acceptance line "account labels only, no identifiers". | Must | `AI-inferred-expansion` | VALIDATED: the statements exist (§ 2.6) |
| REQ-WIRE-C-003 | Ubiquitous | The change shall land as a minor `STATUS_SCHEMA_VERSION` bump, with the five status/watch goldens regenerated and the current-minor Swift fixtures updated in the same commit, sequenced with #1457's claim on the next minor. | Must | `AI-inferred-expansion` | VALIDATED: the lockstep is the repository's rule |
| REQ-ROW-B-001 | Ubiquitous | The menu-bar panel shall give each account row an identity that is unique per roster entry and unchanged by a relabel. | Must | `AI-inferred-expansion` | ASSUMED: the collision is inferred (A-5) |
| REQ-ROW-C-001 | State-driven | While the menu-bar app reads a daemon whose status minor predates the identifier, the app shall render one row for each account line the daemon sent, including two lines that carry one label. | Should | `AI-inferred-expansion` | SPECULATIVE |
| REQ-ROW-C-002 | Ubiquitous | The panel design mock shall carry an authored frame for two rows that share one label before the panel half of this feature ships. | Must | `AI-inferred-expansion` | VALIDATED: the mock authors no such frame (§ 2.6) |
| REQ-USG-B-001 | Ubiquitous | The usage store shall key each sample on an identifier that is unique per roster entry and unchanged by a relabel. | Must | `AI-inferred-expansion` | ASSUMED: the merge is inferred (A-9) |
| REQ-USG-C-001 | Event-driven | When the usage store is re-keyed, the system shall keep every existing sample, attributing a sample to an account only where its stored label resolves to exactly one roster entry at migration time, and keeping every other sample, unmerged, as unattributed history. | Must | `AI-inferred-expansion` | ASSUMED: the presentation of unattributed history is DQ-2 |
| REQ-USG-C-002 | Ubiquitous | The re-key shall leave the prior-configuration witness's usage-store source reading the same after the migration as before it. | Must | `AI-inferred-expansion` | VALIDATED: the witness reads the store (§ 2.12) |
| REQ-USG-C-003 | Optional | Where the re-key changes the `stats --json` shape, the change shall bump `stats`' own `JSON_SCHEMA_VERSION` and update the Swift `StatsWire` mirror and its fixtures in the same commit. | Must, where it applies | `AI-inferred-expansion` | SPECULATIVE: DQ-3 |
| REQ-EVT-B-001 | Event-driven | When the daemon writes an `event=swap` or an `event=emergency_swap` line, the daemon shall identify each account on that line by an identifier unique per roster entry, beside its label. | Should | `AI-inferred-expansion`; a same-class addition | SPECULATIVE |
| REQ-NTC-B-001 | Event-driven | When a `capture` or a `login` reports a count "in rotation", the system shall make the count and its wording agree: the number of enabled roster entries under "in rotation", or the number of roster entries under a wording that names the roster. | Could | `AI-inferred-expansion` | SPECULATIVE: DQ-10 |

**AC-WIRE-001 — REQ-WIRE-I-001, REQ-WIRE-C-001**

> **Given** entries `A` and `B`, both labelled `work`,
> **When** a client reads `status`,
> **Then** the two account lines carry different identifiers,
> **And** `A`'s identifier is unchanged after `A` is relabelled `home`.
> **BUT NOT** a change to any line's label text;
> **BUT NOT** a credential, a token or an email address on the wire;
> **BUT NOT** displaying the identifier to the operator, a Bucket-3 candidate this PRD does not select;
> **BUT NOT** a major version bump.

**AC-WIRE-002 — REQ-WIRE-C-002, REQ-WIRE-C-003**

> **Given** the commit that adds the identifier,
> **When** it is reviewed,
> **Then** every statement § 2.6 lists as keeping the uuid off the status wire is reconciled in it,
> **And** the goldens and the current-minor Swift fixtures move with the minor bump.
> **BUT NOT** a statement left asserting what the wire no longer does;
> **BUT NOT** a version-pinned compatibility fixture moved.

**AC-ROW-001 — REQ-ROW-B-001, REQ-ROW-C-002**

> **Given** the same roster,
> **When** the panel renders a `watch` frame,
> **Then** it shows two rows, each with its own identity and its own figures, laid out as the mock's
> authored frame shows.
> **BUT NOT** one row;
> **BUT NOT** one account's figures in the other account's row.

**AC-ROW-002 — REQ-ROW-C-001**

> **Given** a daemon whose status minor predates the identifier,
> **When** it sends two account lines that carry one label,
> **Then** the panel renders two rows.
> **BUT NOT** a decode error or a dropped row.

**AC-USG-001 — REQ-USG-B-001, REQ-USG-C-001, REQ-USG-C-002**

> **Given** samples under `work`, which only `A` carries at migration time, and samples under `team`,
> which both `B` and `C` carry at migration time (an entry bears one label, so a second bearer of
> `team` must be a third account),
> **When** the store is re-keyed,
> **Then** the `work` samples are kept under `A`'s identifier,
> **And** the `team` samples are kept, unmerged, as unattributed history.
> **BUT NOT** any sample dropped;
> **BUT NOT** the `team` samples attributed to either of its bearers;
> **BUT NOT** any change in what the prior-configuration witness reads.

**AC-USG-002 — REQ-USG-C-003**

> **Given** a re-key that changes the shape of `stats --json`,
> **When** the change lands,
> **Then** `stats`' own `JSON_SCHEMA_VERSION` moves, and the Swift `StatsWire` mirror and its fixtures
> move in the same commit.
> **BUT NOT** a changed shape under an unchanged version;
> **BUT NOT** a version bump when the shape did not change.

**AC-EVT-001 — REQ-EVT-B-001**

> **Given** a swap between two accounts that both carry `work`,
> **When** the daemon writes the `event=swap` line,
> **Then** the two accounts are distinguishable on that line.
> **BUT NOT** by an email address or a token;
> **BUT NOT** a change to what `log`, `stats` and `reliability` report for a roster without a
> duplicate label.

**AC-NTC-001 — REQ-NTC-B-001**

> **Given** a roster of three entries, one of them parked,
> **When** a `capture` reports its count,
> **Then** the number and the words around it agree.
> **BUT NOT** "3 in rotation" while two entries rotate.

**Invariants.** A relabel never changes an account's identifier on any surface. No surface changes
the text it displays for a roster without a duplicate label.

**Verification.** The status/watch goldens, regenerated with
`cargo test -- --ignored emit_wire_golden_fixtures`; the Swift fixture sweep at the current minor;
the `swift` job and an armed `panel-goldens` run; a hermetic migration test over a seeded store; the
issue #15 meter over the new field.

<a id="feature-import-payload-uniqueness"></a>

### 10.5 F5 — Import payload uniqueness

Answers gap `import-payload-duplicate-uuid` (§ 2.7), as the tree corrects it. Whether a payload is
refused or warned is `import-duplicate-uuid-policy`'s to decide (§ 8.1).

| ID | EARS pattern | Statement | Priority | Origin | Confidence |
|---|---|---|---|---|---|
| REQ-MIG-B-001 | Event-driven | When `import` reads a payload whose credential list carries two or more entries for one `account_uuid`, the system shall report it to the operator before it takes the swap lock and before any write, in the form `import-duplicate-uuid-policy` selects. | Must | `AI-inferred-expansion` | VALIDATED: the collapse is observed (§ 2.7) |
| REQ-MIG-B-002 | Event-driven | When `import` reads a payload whose credential list carries an entry whose `account_uuid` matches no roster entry in the payload, the system shall report that entry to the operator in the form `import-duplicate-uuid-policy` selects. | Should | `AI-inferred-expansion`; a same-class addition | ASSUMED |
| REQ-MIG-C-001 | Ubiquitous | The migration artifact format and its frozen `FORMAT_VERSION` fixtures shall stay byte-identical. | Must | `AI-inferred-expansion` | VALIDATED: the fixtures are frozen (§ 2.7) |
| REQ-MIG-C-002 | Ubiquitous | The project shall author, before the change, a test that imports a hand-built payload carrying two credential entries for one uuid and asserts the report, failing against the tree before the change. | Must | `AI-inferred-expansion` | ASSUMED |

**AC-MIG-001 — REQ-MIG-B-001, REQ-MIG-C-001, REQ-MIG-C-002**

> **Given** a hand-built payload whose roster names uuid `A` once and whose credential list carries
> two entries for `A`,
> **When** `import` runs,
> **Then** the operator is told before any write, in the form `import-duplicate-uuid-policy` selects.
> **BUT NOT** silently keeping the last entry;
> **BUT NOT** a report that comes after the swap lock is taken;
> **BUT NOT** any change to the artifact format.

**AC-MIG-002 — REQ-MIG-B-002**

> **Given** a payload whose credential list carries an entry for uuid `Z` that no roster entry names,
> **When** `import` runs,
> **Then** the operator is told about `Z`.
> **BUT NOT** silently dropping the entry.

**Invariants.** The exporter's output is unchanged; only a hand-built or corrupted artifact can reach
these paths.

**Verification.** Hermetic `apply_import` tests with hand-built payloads. Filter the run to the test
you want; never run `--include-ignored`, which also runs the ignored `emit_v1_golden_fixtures` and
rewrites the frozen migration fixtures.

<a id="feature-import-identity-check"></a>

### 10.6 F6 — Import identity check

Answers gap `import-artifact-identity-unchecked` (§ 2.8). The refusal is the seed's draft, and it is
ratification-pending like every requirement here. Whether `export` should compare too is DQ-7.

| ID | EARS pattern | Statement | Priority | Origin | Confidence |
|---|---|---|---|---|---|
| REQ-MIG-B-003 | Unwanted behaviour | If `import` restores a credential entry whose `oauthAccount` names a uuid other than the `account_uuid` it is filed under, then the system shall refuse that entry, writing no stash for it and reporting the account as failed by its label and both uuids, while the payload's other accounts continue. | Must | `AI-inferred-expansion`, as the seed's draft proposes | VALIDATED: no comparison exists (§ 2.8) |
| REQ-MIG-C-003 | Ubiquitous | The project shall author, before the change, a test that imports an artifact whose entry's `oauthAccount` names a uuid other than its filing uuid and asserts the refusal, failing against the tree before the change. | Must | `AI-inferred-expansion` | ASSUMED |

**AC-MIG-003 — REQ-MIG-B-003, REQ-MIG-C-003**

> **Given** an artifact entry filed under uuid `A` whose `oauthAccount.accountUuid` is `B`,
> **When** `import` runs,
> **Then** no stash is written for that entry,
> **And** the account is reported failed, with both uuids,
> **And** the artifact's other accounts import.
> **BUT NOT** adopting the credential as it is;
> **BUT NOT** aborting the whole import;
> **BUT NOT** any credential byte in the report.

**Invariants.** An imported stash always holds an `oauthAccount` whose uuid is the entry's own.

**Verification.** A hermetic `apply_import` test with a hand-built artifact; the per-account report's
existing `failed` outcome carries the refusal.

<a id="feature-identity-premise-measurement"></a>

### 10.7 F7 — Identity premise measurement

Answers gap `identity-premises-unmeasured` (§ 2.9). It is a spike the operator runs by hand; its
output is a record, never code.

| ID | EARS pattern | Statement | Priority | Origin | Confidence |
|---|---|---|---|---|---|
| REQ-MSR-B-001 | Ubiquitous | The project record shall hold, for each of the four identity premises in § 2.9, the result of a real-login measurement the operator took, with the Claude Code version and the date, or the reason it could not be taken, in `build/version-compat.md` or under `docs/findings/`. | Must | `AI-inferred-expansion`; premise 4 is a same-class addition | VALIDATED: no record holds any of them (§ 2.9) |
| REQ-MSR-C-001 | Ubiquitous | The operator shall run every login the measurement needs by hand, on the manual real-login route that spike #130 established. | Must | `AI-inferred-expansion` | VALIDATED: only a real login settles them (§ 2.9) |

**AC-MSR-001 — REQ-MSR-B-001, REQ-MSR-C-001**

> **Given** the operator's real-login walk,
> **When** each premise is measured,
> **Then** the record names the premise, its result, the Claude Code version and the date,
> **And** a premise that could not be measured is recorded with the reason.
> **BUT NOT** a measurement taken by an agent session;
> **BUT NOT** a credential, a token or an email address in the record.

**Invariants.** A recorded result names the version it was measured on, and a later version's result
supersedes it without deleting it.

**Verification.** Read the record; MET-R-001 counts it. The walk is shared with #1020, #1542 and
#1546 (§ 7).

<a id="feature-label-normalization"></a>

### 10.8 F8 — Label normalization

Answers gap `label-normalization-undecided` (§ 2.10). What normalization means is
`label-normalization-policy`'s to decide (§ 8.1); possibly nothing.

| ID | EARS pattern | Statement | Priority | Origin | Confidence |
|---|---|---|---|---|---|
| REQ-LBL-C-001 | Ubiquitous | The project shall record, in a decision record, what label normalization means, possibly nothing, covering letter case, punctuation, Unicode normalization form, visually confusable characters and leading or trailing whitespace, and stating its effect on rosters that already exist. | Must | `AI-inferred-expansion` | VALIDATED: no record decides it (§ 2.10) |
| REQ-LBL-B-001 | Complex | Where that record adopts confusable detection, when a write gives an account a label that the record's rule deems confusable with another entry's label, the system shall report it with REQ-ROS-B-002's calibration. | Should, where adopted | `AI-inferred-expansion` | SPECULATIVE: it hangs on an open decision |
| REQ-LBL-C-002 | Optional | Where confusable detection needs a dependency, the dependency shall carry a licence on the `deny.toml` allow-list and resolve from crates.io. | Must, where it applies | `AI-inferred-expansion` | VALIDATED: `cargo deny check advisories sources licenses` enforces it |

**AC-LBL-001 — REQ-LBL-B-001**

> **Given** a record that adopts confusable detection, and an entry labelled `alice`,
> **When** a write gives another account the label `аlice`, whose first letter is Cyrillic,
> **Then** the operator is told that the two labels are confusable.
> **BUT NOT** for `a-b@x.com` beside `ab@x.com`, unless the record makes punctuation insignificant;
> **BUT NOT** merging or renaming either label.

**AC-LBL-002 — REQ-LBL-C-001**

> **Given** the decision record,
> **When** it is read,
> **Then** it states a rule, possibly "none", for each of case, punctuation, Unicode form, confusables
> and surrounding whitespace, and what the rule does to a roster that already exists.
> **BUT NOT** a rule that merges two rostered accounts' labels without saying so.

**AC-LBL-003 — REQ-LBL-C-002**

> **Given** a confusable detector that needs a new dependency,
> **When** `cargo deny check advisories sources licenses` runs over the change,
> **Then** it passes.
> **BUT NOT** a dependency resolved from outside crates.io;
> **BUT NOT** a licence that the `deny.toml` allow-list lacks.

**Invariants.** Normalization never changes an entry's uuid, and never merges two entries.

**Verification.** Read the record. Where detection is adopted, hermetic tests over a confusable pair
and over the hyphen pairs; `cargo deny` for any new dependency.

<a id="feature-uniqueness-doc-accuracy"></a>

### 10.9 F9 — Uniqueness doc accuracy

Answers gap `uniqueness-docs-drift` (§ 2.11) for the in-repo sites. R-15's half belongs to #1091,
and the sites outside this repository are out of scope (§ 3).

| ID | EARS pattern | Statement | Priority | Origin | Confidence |
|---|---|---|---|---|---|
| REQ-DOC-B-001 | Ubiquitous | The project shall amend ADR-0032 with a dated note, keeping its original text, stating that `SPAWN_ENV_REMOVE` has removed the refresh-token triple since commit `86ab6aa`. | Must | `AI-inferred-expansion` | VALIDATED: the drift is observed (§ 2.11) |
| REQ-DOC-B-002 | Ubiquitous | The migration PRD's stale line anchors shall each cite the symbol they mean, as § 2.11 lists them, so that `scripts/check-doc-citation-rot.sh` checks each symbol against its number, unless #1476's policy is decided first and governs instead. | Must | `AI-inferred-expansion` | VALIDATED: the drift is observed (§ 2.11) |
| REQ-DOC-B-003 | Ubiquitous | The `login` usage text, `LOGIN_USAGE`, shall say what a label given for an already-rostered account does, which is to rename that account, coordinated with #1379's edit of the same constant. | Must | `AI-inferred-expansion` | VALIDATED: the text says "a name for the new account" (§ 2.11); the wording "which is to rename that account" predates the 2026-11-03 ruling, under which such a label is refused unless `--rename` is given, so the usage text and this requirement are re-keyed with #1570 (§ 8.1; the refuse-form sentence is design § 12.3, C-10) |
| REQ-DOC-B-004 | Ubiquitous | The migration PRD's A-1 shall be regraded, by a dated amendment, from "Validated" to an unmeasured assumption whose validation plan is F7's premise 2. | Should | `AI-inferred-expansion`; a same-class addition | VALIDATED: it rests on code comments (§ 2.9) |
| REQ-DOC-C-001 | Ubiquitous | The project shall land every correction to ratified text as a dated amendment note that preserves the original wording, as #1116 records. | Must | `AI-inferred-expansion` | VALIDATED: #1116 is the precedent |

**AC-DOC-001 — REQ-DOC-B-001, REQ-DOC-C-001**

> **Given** ADR-0032,
> **When** the amendment lands,
> **Then** its original sentence remains and a dated note after it names commit `86ab6aa`.
> **BUT NOT** a silent rewrite;
> **BUT NOT** a change to the ADR's `date:`.

**AC-DOC-002 — REQ-DOC-B-002**

> **Given** the migration PRD's stale anchors,
> **When** they are corrected,
> **Then** each names a symbol at its cited lines,
> **And** `scripts/check-doc-citation-rot.sh` passes over the edited lines.
> **BUT NOT** renumbering alone, which the next commit to the cited file breaks again.

**AC-DOC-003 — REQ-DOC-B-003**

> **Given** `sessiometer login --help`,
> **When** the operator reads the `<label>` line,
> **Then** it says that a label given for a rostered account renames that account.
> **BUT NOT** a promise that #1379 is removing.
> *Re-keyed 2026-11-03 (design § 12.3, C-10): the line says that a label which would rename the landed
> account is refused unless `--rename` is given, which renames it, and, for an account new to the roster,
> that a label one other account carries is refused unless `--overwrite` is given.*

**AC-DOC-004 — REQ-DOC-B-004, REQ-DOC-C-001**

> **Given** the migration PRD's A-1,
> **When** the amendment lands,
> **Then** the original row remains, and a dated note regrades it and names its validation plan.
> **BUT NOT** deleting the original grade.

**Invariants.** No ratified wording is lost; an amendment carries its date.

**Verification.** `./scripts/check-doc-citations.sh` and `scripts/check-doc-citation-rot.sh` over the
edited documents; the `doc-gates` job; `LOGIN_USAGE` reaches `--help`, so the CLI-render goldens move
only if a rendered help golden carries it.

<a id="feature-orphan-stash-disposition"></a>

### 10.10 F10 — Orphan stash disposition

Answers gap `orphan-stash-reaping` (§ 2.12). It is now established that nothing reaps or reports an
orphan; what to do about it is `orphan-stash-policy`'s to decide (§ 8.1).

| ID | EARS pattern | Statement | Priority | Origin | Confidence |
|---|---|---|---|---|---|
| REQ-STSH-C-004 | Ubiquitous | The project shall record the `orphan-stash-policy` decision — reap, report or accept — naming the four origins of an orphan stash (§ 2.12) and the decision's interaction with the prior-configuration witness. | Must | `AI-inferred-expansion` | VALIDATED: nothing records it, and nothing reaps or reports (§ 2.12) |
| REQ-STSH-B-003 | Optional | Where that decision is to reap, the system shall delete an orphan stash item only while `config.toml` loads, while no roster entry carries the item's account uuid, and while no `capture`, `login` or `import` stands between its stash write and its roster save. | Must, where chosen | `AI-inferred-expansion` | SPECULATIVE: it hangs on an open decision |
| REQ-STSH-B-004 | Optional | Where that decision is to report, the system shall list every orphan stash item by its account uuid, reading keychain metadata only. | Must, where chosen | `AI-inferred-expansion` | SPECULATIVE: it hangs on an open decision |

**AC-STSH-005 — REQ-STSH-C-004**

> **Given** the `orphan-stash-policy` decision record,
> **When** it is read,
> **Then** it names the decision, the four origins of an orphan stash and the decision's effect on the
> prior-configuration witness.
> **BUT NOT** a record that leaves the code comments calling an orphan "harmless" in contradiction
> with it.

**AC-STSH-003 — REQ-STSH-B-003**

> **Given** an orphan item `Sessiometer/Z` and a roster without `Z`,
> **When** the reaper runs while `config.toml` loads,
> **Then** `Z`'s item is deleted.
> **BUT NOT** while `config.toml` is absent, which would erase the witness;
> **BUT NOT** an item that a `capture` has written and not yet rostered;
> **BUT NOT** any item that a roster entry carries.

**AC-STSH-004 — REQ-STSH-B-004**

> **Given** an orphan item `Sessiometer/Z`,
> **When** the report runs,
> **Then** it names `Z`.
> **BUT NOT** any secret the item holds;
> **BUT NOT** a keychain prompt.

**Invariants.** No rostered stash is ever deleted by this feature; the witness reads the same before
and after on a machine whose `config.toml` is absent.

**Verification.** Hermetic tests with an in-memory stash and an injected in-flight write; the
witness's own tests stay green.

<a id="cross-cutting-constraints"></a>

### 10.11 Cross-cutting constraints — binding on every feature

| ID | EARS pattern | Statement | Priority | Origin | Confidence |
|---|---|---|---|---|---|
| REQ-NTC-C-001 | Ubiquitous | The system shall name an account, in every reply, report and log line this PRD adds, by its label, by its account uuid where the label alone is ambiguous, and by no email address the label does not itself carry, no token and no credential-derived value. | Must | `AI-inferred-expansion` | VALIDATED: issue #15 and `login_confirmation`'s contract |
| REQ-NTC-C-002 | Ubiquitous | The system shall route every operator-facing string this PRD adds through the issue #160 framing-vocabulary scanner (`src/framing_vocabulary.rs`), under the audience that matches its register, so that #1167's gap does not widen. | Must | `AI-inferred-expansion` | ASSUMED |
| REQ-NTC-C-003 | Ubiquitous | The project shall give every behaviour change in this PRD a test that fails against the tree before the change, and shall record that failure. | Must | `AI-inferred-expansion` | ASSUMED: the project's RED-first practice |

**AC-NTC-002 — REQ-NTC-C-001, REQ-NTC-C-002**

> **Given** any reply, report or log line this PRD adds,
> **When** the issue #15 meter and the issue #160 framing scan run over it,
> **Then** both pass,
> **And** the line names accounts by label, or by uuid where the label is ambiguous.
> **BUT NOT** the harvested email address, a token, or a digest of a credential.

**AC-NTC-003 — REQ-NTC-C-003, REQ-IDN-C-001, REQ-STSH-C-002**

> **Given** a change that implements any requirement in § 10,
> **When** its test runs against the tree before the change,
> **Then** the test fails, and the change records that failure.
> **BUT NOT** a test that passes before the change, which proves nothing about it.

## 11. State Matrices

Five views show data this PRD changes (§ 9.3): four have a matrix below, and the fifth, the UsageHistory View
(`stats`), has none until DQ-2 (§ 8.3) decides how unattributed history is shown. Each states all ten states; a state that does not apply says
why. Where a state keeps today's presentation, the row says so rather than inventing one.

### 11.1 CLI `capture` / `login` reply

| State | What the operator sees |
|---|---|
| Loading | N/A: the CLI prints one line after the write completes; during a `login` the operator is in Claude Code's own login flow, which this PRD does not change |
| Empty — first use | A first capture on a machine with no roster: `Captured "X" (now 1 in rotation).`; no collision or shared credential is possible; ADR-0036's witness may refuse first, unchanged |
| Empty — no results | N/A: a write always has an outcome, never a result set |
| Populated | `Captured`, `Refreshed`, `Onboarded` or `Revived`, plus F1's collision report, F2's previous label and F3's shared-credential report, each only when it applies (since 2026-11-03, design § 12.3 C-8: F2's previous label only on a CLI revive made with `--rename`, a replaced holder named by uuid under `--overwrite`, and no collision report, since no completed write creates one) |
| Error — network | The login spawn fails: today's error, with no roster change and no report |
| Error — auth | An incomplete login, timed out or cancelled: today's message, with no roster change |
| Error — permission | A locked keychain: today's error, with no write |
| Partial | The stash was written and the roster save failed: today's error, and an orphan stash (F10) |
| Stale | N/A: the reply reports the write it has just made |
| Offline | No daemon running: the write succeeds, the reload notice is best-effort, and the reply is unchanged |

### 11.2 Panel capture card (socket `capture`)

| State | What the operator sees |
|---|---|
| Loading | The capture is in flight: as the mock authors it today; F1 and F2 add no loading state |
| Empty — first use | N/A: the card exists only once a capture has been requested |
| Empty — no results | N/A: an acknowledgement always carries a result |
| Populated | `captured` with the label and the count, or `refreshed`, which decodes as an error until #1474 lands; plus F1's report and F2's previous label (since 2026-11-03, design § 12.3 C-6 and C-8: neither; a capture that would create a collision or relabel an account is `rejected` before any write) |
| Error — network | The daemon is unreachable: the panel's existing daemon-down presentation |
| Error — auth | `no-active-account`: the existing rejection |
| Error — permission | `keychain-locked` or `prior-configuration`: the existing rejections; a refusal under `duplicate-label-policy` would join them as a new tag that Swift models in the same commit (since 2026-11-03 both `label-in-use` and `relabels-another-account` join them, design § 12.3 C-12) |
| Partial | `failed`: the stash landed and the roster save did not (F10) |
| Stale | N/A: an acknowledgement answers one request |
| Offline | The daemon is down: the existing presentation |

### 11.3 Settings label save (socket `config-set`)

| State | What the operator sees |
|---|---|
| Loading | The save is in flight: the existing Settings behaviour, unchanged |
| Empty — first use | `no-config`: the existing rejection, since there is no roster to relabel yet |
| Empty — no results | N/A: a save always has an outcome |
| Populated | `applied`, with the effect `live`, `restart_required` or `unchanged`, plus F1's collision report (since 2026-11-03, design § 12.3 C-7: `applied` carries no collision report; a colliding save is `rejected` with `label-in-use` and `collisions`) |
| Error — network | The daemon is unreachable: the Settings window's existing daemon-down handling |
| Error — auth | N/A: the control socket's peer check refuses another user at connect, before any command |
| Error — permission | N/A: the same peer check; `config-set` has no permission tier of its own |
| Partial | N/A: `config-set` persists the whole change or none of it, and reports `save-failed` with the old file intact |
| Stale | `unknown-account`: a stale client edited a uuid the roster no longer holds |
| Offline | `config-unreadable`, or `unavailable` where the daemon has no wired config path: the existing rejections |

### 11.4 Panel account list

| State | What the operator sees |
|---|---|
| Loading | Before the first `watch` frame: the existing presentation |
| Empty — first use | No accounts: the existing empty state |
| Empty — no results | N/A: the list is not filtered |
| Populated | One row per account line, and two rows for two accounts that share a label (REQ-ROW-B-001), laid out as the mock's owed frame shows (REQ-ROW-C-002) |
| Error — network | The daemon is unreachable: the existing liveness states |
| Error — auth | N/A: the socket's peer check is the only gate, and it refuses at connect |
| Error — permission | N/A: as for auth |
| Partial | A daemon whose status minor predates the identifier: one row per account line (REQ-ROW-C-001) |
| Stale | The existing freshness presentation, unchanged |
| Offline | The existing daemon-down presentation |

## 12. Assumption Registry

| ID | Assumption | Type | Confidence | Evidence | Validation plan | Risk if wrong |
|---|---|---|---|---|---|---|
| A-1 | One real account has exactly one `account_uuid` | External behaviour | 🟡 Assumed | A code comment only (`derive_label`'s doc, § 2.9) | F7, premise 2 | Two uuids for one account make two entries for one account; F3 becomes the only signal, and the uuid key's protection weakens (§ 4, decision gates) |
| A-2 | A refresh rotates the refresh token, so two byte-identical stashes split at the first refresh of either | External behaviour | 🟢 Validated | The migration PRD's A-2, resolved by spike #262 | REQ-STSH-Q-001's METER exercises it hermetically | If tokens did not rotate, a later sweep could detect pairs too, and DQ-5 gets cheaper |
| A-3 | One isolated login writes the credential and the `oauthAccount` of one account | External behaviour | 🟡 Assumed | `reconcile_login`'s comment (§ 2.9); H2 shows the two halves can diverge without error | F7, premise 1 | A login could file one account's credential under another's identity; F3 and F6 become the primary protection (§ 4, decision gates) |
| A-4 | Two rows holding one token poll one real limit and count it twice | Technical inference | 🟡 Assumed | Each non-active entry polls with its own stash token (§ 2.5) | A hermetic poll test with one token under two entries | If something deduplicates elsewhere, F3's urgency drops, not its need |
| A-5 | SwiftUI defines no rendering for two `ForEach` elements with one `id` | Technical inference | 🟡 Assumed; measured on a stand-in, not on the real roster (2026-10-30) | The row's `id` is its label (§ 2.6). A stand-in probe (a `ForEach` keyed on the label over `Button` rows in a `ScrollView`, hosted and walked as the panel tests do; macOS 27.0.1, Swift 6.4; run by #1582's readiness gate on 2026-10-28 and re-run on 2026-10-30) drew the first bearer twice and the second bearer's figures nowhere, while position keys drew every row | The real `RosterView` tree, dumped by #1582 before its tests are written | If SwiftUI renders both rows correctly, REQ-ROW-B-001 still holds for selection and animation identity, at lower urgency |
| A-6 | A hyphenated uuid never forms the 20-character alphanumeric run that the issue #15 entropy backstop flags | Technical | 🟢 Validated at source | The backstop splits on non-alphanumerics; a hyphenated uuid's longest run is 12 (§ 2.6) | The meter test over a corpus that carries the new field | An unhyphenated uuid, which `account_uuid_violation` accepts, would trip the meter; REQ-WIRE-C-001 catches it |
| A-7 | The in-tree exporter cannot emit two credential entries for one uuid | Technical | 🟢 Validated at source | The exporter iterates the validated roster (§ 2.7) | None needed | F5 would guard a path the exporter can reach, which raises its priority |
| A-8 | No write path produces a stash whose `oauthAccount` uuid differs from its filing uuid | Technical and external | 🔴 Unknown | `capture` pairs under the swap lock; `login` rests on A-3; #211 guards the swap's re-stash | F7, premise 1; DQ-7 | Exported artifacts could already carry mismatches, and F6 would refuse them on import |
| A-9 | A duplicate label merges two accounts' usage histories, and a relabel starts a new history handle | Technical inference | 🟡 Assumed | The store keys each sample on the label (§ 2.6) | A hermetic store test with two same-label accounts | If history were keyed elsewhere, REQ-USG-B-001 shrinks to nothing |
| A-10 | Nothing reaps or reports an orphan stash at runtime | Technical | 🟢 Validated statically | § 2.12's read of every production use of `STASH_PREFIX` and every stash delete | DQ-9: a metadata-only census on the operator's machine | If something did, F10 records it instead of adding one |
| A-11 | The 2026-09-09 transcript's binary behaved as the tree reads | Evidence provenance | 🔴 Unknown | The binary is tied to no commit (§ 2.2) | None possible after the fact | Only § 2.2's inferences weaken; no requirement rests on the transcript alone |
| A-12 | The private HQ records quoted in § 2.13 are current | Evidence provenance | 🟢 Read 2026-10-26 | Quoted verbatim from the HQ records | Re-read them at ratification | A changed record changes § 8.2's contradictions |
| A-13 | Claude Code never varies the letter case of one account's uuid | External behaviour | 🟡 Assumed | Nothing measures it; the key is compared byte for byte (§ 2.9) | F7, premise 4 | Two casings would make two entries for one account |
| A-14 | `claude auth login --claudeai` still writes `emailAddress` into the isolated `.claude.json` | External behaviour | 🔴 Unknown | The #1020 section of `build/version-compat.md` does not measure it | F7, premise 3 | F2's email-comparison option has nothing to compare under the planned argv |

## 13. Feature Completeness

| Feature | Requirement IDs | Object sections | Entity coverage | CTA coverage | Specification depth | Open questions | Verdict |
|---|---|---|---|---|---|---|---|
| F1 `label-collision-notice` | REQ-ROS-B-001, REQ-ROS-B-002, REQ-ROS-I-001, REQ-ROS-I-002, REQ-ROS-C-001, REQ-ROS-C-002, REQ-ROS-Q-001 | RosterEntry, AccountLabel, OperatorNotice | 3 of 3 | 5 of 5 | EARS; GWT with BUT NOT; Planguage | `duplicate-label-policy` (ruled 2026-11-03: refuse) | **NEAR-COMPLETE**: the report's form waits on one decision; #1474 gates the socket half |
| F2 `login-landing-disclosure` | REQ-IDN-B-001, REQ-IDN-I-001, REQ-IDN-I-002, REQ-IDN-C-001, REQ-IDN-C-002 | RosterEntry, CapturedIdentity, OperatorNotice | 3 of 3 | 5 of 5 | EARS; GWT with BUT NOT | `login-mismatch-policy` (ruled 2026-11-03: refuse) | **NEAR-COMPLETE**: disclosure holds under every option; the rest waits on one decision |
| F3 `shared-credential-detection` | REQ-STSH-B-001, REQ-STSH-B-002, REQ-STSH-Q-001, REQ-STSH-Q-002, REQ-STSH-C-001, REQ-STSH-C-002, REQ-STSH-C-003 | StashedCredential, OperatorNotice, EventLogLine | 3 of 3 | 4 of 4 | EARS; GWT with BUT NOT; Planguage | DQ-5, DQ-6 | **NEAR-COMPLETE**: the start-up sweep and the daemon-side channel are design calls |
| F4 `identity-keyed-surfaces` | REQ-WIRE-I-001, REQ-WIRE-C-001, REQ-WIRE-C-002, REQ-WIRE-C-003, REQ-ROW-B-001, REQ-ROW-C-001, REQ-ROW-C-002, REQ-USG-B-001, REQ-USG-C-001, REQ-USG-C-002, REQ-USG-C-003, REQ-EVT-B-001, REQ-NTC-B-001 | RosterEntry, AccountStatusLine, PanelAccountRow, UsageHistory, EventLogLine | 5 of 5 | 8 of 8 | EARS; GWT with BUT NOT | DQ-1, DQ-2, DQ-3, DQ-4, DQ-10 | **INCOMPLETE**: see the gap documentation below |
| F5 `import-payload-uniqueness` | REQ-MIG-B-001, REQ-MIG-B-002, REQ-MIG-C-001, REQ-MIG-C-002 | MigrationArtifact, OperatorNotice | 2 of 2 | 2 of 2 | EARS; GWT with BUT NOT | `import-duplicate-uuid-policy` | **NEAR-COMPLETE**: refuse or warn waits on one decision |
| F6 `import-identity-check` | REQ-MIG-B-003, REQ-MIG-C-003 | MigrationArtifact, StashedCredential, OperatorNotice | 3 of 3 | 3 of 3 | EARS; GWT with BUT NOT | DQ-7 | **NEAR-COMPLETE**: whether `export` compares too is open |
| F7 `identity-premise-measurement` | REQ-MSR-B-001, REQ-MSR-C-001 | CapturedIdentity, IdentityMeasurement | 2 of 2 | 3 of 3 | EARS; GWT with BUT NOT | DQ-8 | **NEAR-COMPLETE**: premise 2 may not be measurable with the operator's accounts |
| F8 `label-normalization` | REQ-LBL-C-001, REQ-LBL-B-001, REQ-LBL-C-002 | AccountLabel, OperatorNotice, ProjectRecord | 3 of 3 | 3 of 3 | EARS; GWT with BUT NOT | `label-normalization-policy` | **NEAR-COMPLETE**: the record decides; detection is conditional on it |
| F9 `uniqueness-doc-accuracy` | REQ-DOC-B-001, REQ-DOC-B-002, REQ-DOC-B-003, REQ-DOC-B-004, REQ-DOC-C-001 | ProjectRecord | 1 of 1 | 1 of 1 | EARS; GWT with BUT NOT | none | **COMPLETE** |
| F10 `orphan-stash-disposition` | REQ-STSH-C-004, REQ-STSH-B-003, REQ-STSH-B-004 | StashedCredential, ProjectRecord | 2 of 2 | 3 of 3 | EARS; GWT with BUT NOT | `orphan-stash-policy`, DQ-9 | **NEAR-COMPLETE**: reap, report or accept waits on one decision |
| Cross-cutting constraints (§ 10.11), not a feature | REQ-NTC-C-001, REQ-NTC-C-002, REQ-NTC-C-003 | OperatorNotice | 1 of 1 | every Create of OperatorNotice | EARS; GWT with BUT NOT | none | **COMPLETE**: they bind every feature above |

**F4 gap documentation.** F4 is INCOMPLETE because its open questions decide the shape of its own
requirements, not only their form:

- **DQ-1, the identifier and its issue #15 reconciliation.** It shapes REQ-WIRE-I-001 and
  REQ-WIRE-C-002, and through them REQ-ROW-B-001, REQ-USG-B-001 and REQ-EVT-B-001, which key on it.
- **DQ-2, unattributed history.** REQ-USG-C-001 says what must be kept, not how the operator sees it.
- **DQ-3, the `stats` shape.** It decides whether REQ-USG-C-003 applies at all.
- **DQ-4, the mock frame.** REQ-ROW-C-002 owes it before the panel half can be built against it.
- **DQ-10, the count wording.** It does not block; it chooses between two outcomes REQ-NTC-B-001
  already allows.

What closes the gap: Stage 2's solution design answers DQ-1 to DQ-3, the mock frame is authored, and
this row is graded again.

## 14. Source Traceability

Sources: "seed" is the `/scope` seed's gap of that slug; "report" is the `/investigate` report's
section; "tree" is the re-verification in § 2 at `d9d75ba`. All three are replicated in-band above.

| Req ID | Source | Object | CTA | Feature | Layer | Confidence |
|---|---|---|---|---|---|---|
| REQ-ROS-B-001 | seed `duplicate-label-silent-at-entry`; report § Findings (b); tree § 2.3 | RosterEntry | Create, Edit | F1 | B | VALIDATED |
| REQ-ROS-B-002 | import's rule (tree § 2.3); per account since 2026-11-03 for the four write paths (design § 12.3 Terms) | AccountLabel | Compare (exact) | F1 | B | ASSUMED |
| REQ-ROS-I-001 | seed constraints (#1474, #1441); § 6 | OperatorNotice | Create (collision) | F1 | I | VALIDATED |
| REQ-ROS-I-002 | `gui-cli-capability-parity` R-1 | OperatorNotice | View (collision) | F1 | I | ASSUMED |
| REQ-ROS-C-001 | `docs/specs/capture-rejection-parity.feature.md`; the soft `panel-goldens` job | OperatorNotice | Create (collision) | F1 | C | VALIDATED |
| REQ-ROS-C-002 | report § Readiness Assessment, its owed oracles | RosterEntry | Create, Edit | F1 | C | ASSUMED |
| REQ-ROS-Q-001 | import's calibration comment (tree § 2.3) | OperatorNotice | Create (collision) | F1 | Q | ASSUMED |
| REQ-IDN-B-001 | seed `login-lands-on-another-account`; report § Findings (c); tree § 2.4 | CapturedIdentity | View (disclose) | F2 | B | VALIDATED |
| REQ-IDN-I-001 | seed constraint (#1474) | OperatorNotice | Create (relabel) | F2 | I | ASSUMED |
| REQ-IDN-I-002 | `gui-cli-capability-parity` R-1 | OperatorNotice | View (relabel) | F2 | I | ASSUMED |
| REQ-IDN-C-001 | seed gap body: no test asserts that `login X` landed on X | CapturedIdentity | Match | F2 | C | ASSUMED |
| REQ-IDN-C-002 | premortem (§ 6) | RosterEntry | Edit | F2 | C | SPECULATIVE |
| REQ-STSH-B-001 | seed `one-credential-two-entries`; report § Findings (f); tree § 2.5 | StashedCredential | Compare | F3 | B | VALIDATED |
| REQ-STSH-B-002 | seed gap body; ADR-0028 | OperatorNotice, EventLogLine | Create (shared credential), Append (shared credential) | F3 | B | ASSUMED |
| REQ-STSH-Q-001 | the migration PRD's A-2 (spike #262); premortem | StashedCredential | Create (write) | F3 | Q | VALIDATED |
| REQ-STSH-Q-002 | premortem | StashedCredential | Compare | F3 | Q | ASSUMED |
| REQ-STSH-C-001 | issue #15 | StashedCredential | Compare | F3 | C | VALIDATED |
| REQ-STSH-C-002 | the project's RED-first practice | StashedCredential | Create (write) | F3 | C | ASSUMED |
| REQ-STSH-C-003 | tree § 2.5 (the canary) | StashedCredential | Compare | F3 | C | VALIDATED |
| REQ-WIRE-I-001 | seed `label-keyed-downstream-surfaces`; report § Downstream effects; tree § 2.6 | AccountStatusLine | Create | F4 | I | ASSUMED |
| REQ-WIRE-C-001 | issue #15, read 2026-10-26 | AccountStatusLine | Create | F4 | C | VALIDATED |
| REQ-WIRE-C-002 | tree § 2.6; #791 | AccountStatusLine | Create | F4 | C | VALIDATED |
| REQ-WIRE-C-003 | the repository's schema-version rules; #1457 | AccountStatusLine | View | F4 | C | VALIDATED |
| REQ-ROW-B-001 | seed gap body; tree § 2.6 | PanelAccountRow | Render | F4 | B | ASSUMED |
| REQ-ROW-C-001 | premortem (an older daemon) | PanelAccountRow | Render | F4 | C | SPECULATIVE |
| REQ-ROW-C-002 | `apps/menubar/design/README.md` (the mock is the build reference); tree § 2.6 | PanelAccountRow | Render | F4 | C | VALIDATED |
| REQ-USG-B-001 | seed gap body; tree § 2.6 | UsageHistory | Append | F4 | B | ASSUMED |
| REQ-USG-C-001 | premortem (history continuity) | UsageHistory | Migrate | F4 | C | ASSUMED |
| REQ-USG-C-002 | ADR-0036; #1466 | UsageHistory | Migrate | F4 | C | VALIDATED |
| REQ-USG-C-003 | the repository's schema-version rules | UsageHistory | View | F4 | C | SPECULATIVE |
| REQ-EVT-B-001 | tree § 2.6 (`Event::Swap`); same-class addition | EventLogLine | Append (swap identity) | F4 | B | SPECULATIVE |
| REQ-NTC-B-001 | seed gap body ("N in rotation"); tree § 2.6 | RosterEntry | View | F4 | B | SPECULATIVE |
| REQ-MIG-B-001 | seed `import-payload-duplicate-uuid`, corrected (§ 2.14, row 1); tree § 2.7 | MigrationArtifact | Import (payload check) | F5 | B | VALIDATED |
| REQ-MIG-B-002 | tree § 2.7; same-class addition | MigrationArtifact | Import (payload check) | F5 | B | ASSUMED |
| REQ-MIG-C-001 | the frozen `FORMAT_VERSION` fixtures | MigrationArtifact | Import (payload check) | F5 | C | VALIDATED |
| REQ-MIG-C-002 | the project's RED-first practice | MigrationArtifact | Import (payload check) | F5 | C | ASSUMED |
| REQ-MIG-B-003 | seed `import-artifact-identity-unchecked`; tree § 2.8 | MigrationArtifact | Import (entry identity check) | F6 | B | VALIDATED |
| REQ-MIG-C-003 | the project's RED-first practice | MigrationArtifact | Import (entry identity check) | F6 | C | ASSUMED |
| REQ-MSR-B-001 | seed `identity-premises-unmeasured`; report § Next Steps; tree § 2.9 | IdentityMeasurement | Create | F7 | B | VALIDATED |
| REQ-MSR-C-001 | report § Readiness Assessment, advisory notes; spike #130 | CapturedIdentity | Create (measure) | F7 | C | VALIDATED |
| REQ-LBL-C-001 | seed `label-normalization-undecided`; report § Findings (d); tree § 2.10 | ProjectRecord | Create (normalization record) | F8 | C | VALIDATED |
| REQ-LBL-B-001 | seed gap body | AccountLabel | Compare (confusable) | F8 | B | SPECULATIVE |
| REQ-LBL-C-002 | the repository's dependency rule (`deny.toml`) | AccountLabel | Compare (confusable) | F8 | C | VALIDATED |
| REQ-DOC-B-001 | seed `uniqueness-docs-drift`; tree § 2.11 | ProjectRecord | Edit (amend) | F9 | B | VALIDATED |
| REQ-DOC-B-002 | seed gap body; tree § 2.11; #1476 | ProjectRecord | Edit (amend) | F9 | B | VALIDATED |
| REQ-DOC-B-003 | seed gap body; tree § 2.11; #1379 | ProjectRecord | Edit (amend) | F9 | B | VALIDATED |
| REQ-DOC-B-004 | tree § 2.9; same-class addition | ProjectRecord | Edit (amend) | F9 | B | VALIDATED |
| REQ-DOC-C-001 | #1116 | ProjectRecord | Edit (amend) | F9 | C | VALIDATED |
| REQ-STSH-C-004 | seed `orphan-stash-reaping`; tree § 2.12 | ProjectRecord | Create (orphan-stash record) | F10 | C | VALIDATED |
| REQ-STSH-B-003 | tree § 2.12; ADR-0036 | StashedCredential | Delete (reap) | F10 | B | SPECULATIVE |
| REQ-STSH-B-004 | tree § 2.12 | StashedCredential | View (metadata) | F10 | B | SPECULATIVE |
| REQ-NTC-C-001 | issue #15; `login_confirmation`'s contract | OperatorNotice | every Create | all | C | VALIDATED |
| REQ-NTC-C-002 | issue #160; #1167 | OperatorNotice | every Create | all | C | ASSUMED |
| REQ-NTC-C-003 | the project's RED-first practice | all | all | all | C | ASSUMED |

### 14b. Cross-Repository References

| PRD Req | Implementation repo | Spec or item | Status |
|---|---|---|---|
| all | this repository | parent #1550; F1–F3 #1556–#1561; F4 #1562–#1564, #1566, #1571–#1574, #1578; F5–F6 #1565; F7 #1551; F8 #1575–#1577; F9 #1567–#1570; F10 #1552–#1555; oracle items #1579–#1585 | Filled by `/scope` on 2026-10-27. `dor_status: failed` still stands, so every item is provisional on ratification |

### 14c. Requirement provenance and the readiness verdict

| Set | Members | Status |
|---|---|---|
| Operator-authored | The two messages in § 2.1; through the second, the class "the gaps discovered" | **Ratified**: the class only |
| Pipeline-authored | Every requirement in § 10, each tagged `AI-inferred-expansion`, with the same-class additions marked (§ 8.4); the problem framing; the appetite; every metric; every decision in § 8.4 | **Pending**: none ratified one at a time |

**Definition-of-Ready checks**

| # | Check | Result |
|---|---|---|
| 1 | Validated problem statement | **FAIL**: § 2 is non-empty and carries the current state, who has the problem and why now, but the pipeline framed it and the operator has not validated it |
| 2 | Explicit out-of-scope declarations, with the appetite | **PASS**: § 3 carries the appetite, every deferred item with its reason and what reopens it, and the never-build list |
| 3 | Success and telemetry metrics | **PASS**: § 4 carries leading and lagging indicators, decision gates and a north star; all are pipeline-derived and ratification-pending |
| 4 | Cross-cutting and non-functional concerns | **PASS**: § 15.1 to § 15.6 each carry content or `N/A` with a rationale |
| 5 | Feature-completeness verdict | **PASS**: § 13 grades every feature, and F4's INCOMPLETE carries its gap documentation |
| 6 | Requirement provenance | **FAIL**: every requirement carries its origin tag, and every one is pipeline-authored and unratified |

**Verdict: FAIL**, written as `dor_status: failed`. This document is blocked from scoping and from
design. The run did not stop there: the `/scope` orchestrator acknowledged this verdict on the operator's
behalf and carried Stages 2 to 4 as provisional (the scope brief's register row `proceed-past-failed-dor`),
so the design and the items in § 14b exist, each marked ratification-pending; the verdict is not thereby
cleared. Two things clear it, and only the operator can supply either: validating or rewriting § 2,
and ratifying the requirements in § 10 one at a time, together with the decisions in § 8.1 that shape
them. After that, re-run the readiness gate. On `failed`, `/capture-requirements` writes no run
report and commits nothing.

## 15. Cross-Cutting and Non-Functional Concerns

### Category coverage

Scope context: a general application, a CLI and a daemon with a menu-bar client, so every category is
evaluated.

| Category | ISO/IEC 25010 | Where this PRD covers it | Status |
|---|---|---|---|
| Functional suitability | 1 | F1 to F10 | Covered |
| Performance efficiency | 2 | REQ-STSH-Q-002; § 15.4 | Covered |
| Compatibility | 3 | REQ-ROS-I-001, REQ-IDN-I-001, REQ-WIRE-C-003, REQ-ROW-C-001, REQ-USG-C-003, REQ-MIG-C-001 | Covered |
| Usability — accessibility | 4.4 | REQ-ROW-C-002: the mock frame, including the spoken identity cue (#939, DQ-4) | Covered |
| Usability — i18n / l10n | 4 | N/A: the menu-bar app has no localization layer, with no `.strings` or `.xcstrings` file and no `NSLocalizedString` call, and the CLI is English-only; labels are operator text stored as given | N/A, outside a documented context pattern, so it is flagged for the operator's review |
| Reliability — observability | 5 | REQ-STSH-B-002, REQ-EVT-B-001 | Covered |
| Reliability — recoverability | 5.4 | REQ-USG-C-001, REQ-IDN-C-002, F10 | Covered |
| Security — authentication and authorization | 6.1, 6.2 | No auth surface changes: the control socket's peer check is untouched, and REQ-STSH-C-003 keeps the canary's verdicts | Covered |
| Security — secrets and audit | 6.3, 6.4 | REQ-STSH-C-001, REQ-NTC-C-001, REQ-WIRE-C-001 | Covered |
| Maintainability | 7 | REQ-NTC-C-003; F9; REQ-DOC-C-001 | Covered |
| Portability | 8 | § 15.5: the Linux build keeps compiling; F3 and F10 are macOS-scoped until #26 and #27 | Covered |

| Build-time category | Where this PRD covers it | Status |
|---|---|---|
| Infrastructure scaffolding | N/A: it extends the existing CI jobs, fixtures and goldens | N/A, existing infrastructure |
| Cross-cutting concerns | § 10.11 | Covered |
| UX lifecycle flows | § 11 | Covered |
| UX architecture | REQ-ROW-C-002; no new screen | Covered |
| Operational features | F10's report or reap; F7's measurement procedure | Covered |
| Data concerns | REQ-USG-C-001, REQ-USG-C-002, REQ-MIG-C-001 | Covered |
| Compliance | Subsumed by the risk lane below | Covered by the lane |

**Risk-marker lane.** Three markers are present: credentials are handled, the service runs against
third-party accounts, and the repository is public. Its sub-surfaces:

- **Runtime disclosure**: REQ-NTC-C-001 and REQ-STSH-C-001.
- **The redaction meter on the wire**: REQ-WIRE-C-001 and REQ-WIRE-C-002.
- **Licence and IP posture**: REQ-LBL-C-002, the only place a dependency could enter.
- **Disclosure in this document**: § 2.2 replicates the transcript with synthetic labels of the same
  shape in place of the operator's addresses, and elides the session id.
- **Provider terms of service**: no requirement adds a request to the provider; F3 is offline by
  construction, and F7's logins are the operator's own, taken by hand.
- **Trademark, takedown and counsel readiness**: N/A, because nothing here names, brands or publishes
  anything new.
- **Scope boundary**: § 3.

**Greenfield lane**: N/A. There is no greenfield marker; the work extends existing infrastructure.

**Sweep verdict**: PASS-WITH-FINDINGS, in the category sweep's own vocabulary: every category is
covered or carries an N/A, and the i18n N/A sits outside a documented context pattern, so it is
flagged for the operator's review. This is the sweep's verdict, not the readiness verdict, which § 14c gives.

### 15.1 Security

- No new output carries a credential byte, digest or fingerprint (REQ-STSH-C-001), or an email
  address the label does not carry (REQ-NTC-C-001).
- The identifier on the status wire passes issue #15's meter (REQ-WIRE-C-001), and the change
  reconciles the code comments that read issue #15 as keeping the uuid off that wire (REQ-WIRE-C-002).
- F3 compares in memory and never refuses or chooses, so it opens no new path to a wrong-account
  write; the canary's deliberate pass stays (REQ-STSH-C-003).
- F10's report reads keychain metadata only, as the witness already does (REQ-STSH-B-004), and its
  reaper may never delete a rostered stash or erase the witness (REQ-STSH-B-003).
- F6 closes a path where an artifact could file one account's credential under another's identity
  (REQ-MIG-B-003).

### 15.2 Compliance and Regulatory

N/A: a single-operator local tool with no regulated data. The provider-terms and redaction
sub-surfaces sit in the risk lane above.

### 15.3 Reliability and Observability

- F3 writes one durable event-log line per detection (REQ-STSH-B-002), which makes ADR-0028's trigger
  countable (MET-R-003).
- The swap log distinguishes same-label accounts (REQ-EVT-B-001).
- No report fires on a write that created nothing (REQ-ROS-Q-001).
- A refusal, wherever a policy chooses one, precedes every write (REQ-IDN-C-002, AC-ROS-001), so it
  leaves no orphan stash behind.

### 15.4 Performance and Scalability

- F3 adds at most one stash read per other roster entry per write, and none per poll or swap
  (REQ-STSH-Q-002). Writes are operator-paced, so the cost is bounded by the roster's size at the
  moment of a write.
- F4 adds one identifier per account line to every `status` and `watch` frame, and one key per
  usage sample. The usage-store re-key is a one-time migration.
- Nothing here changes the poll schedule or the request rate.

### 15.5 Operational

- **CI owed**: any `src/**` change owes `test`, `msrv` and `deny`. Any `apps/menubar/**` or
  `build/fixtures/**` change owes `swift`, plus an armed `panel-goldens` run, because the CI job is
  soft and a bare `xcodebuild test` skips both golden comparisons.
- **Goldens**: F4 regenerates the five status/watch goldens with
  `cargo test -- --ignored emit_wire_golden_fixtures`, and a `stats` shape change regenerates its
  golden with `cargo test -- --ignored emit_wire_stats_golden_fixture`. A change to a rendered CLI
  golden needs a `CLI-Goldens-Rebaselined:` trailer, and a panel golden a `Panel-Goldens-Rebaselined:`
  trailer.
- **Portability**: no CI job compiles for Linux (#964), so a change touching the stash or the
  keychain path must keep the Linux build compiling by hand; F3 and F10 stay macOS-scoped until #26
  and #27 land.
- **Runtime check**: the daemon runs from the app bundle, so verifying a daemon-side change in the
  running service needs the bundle rebuilt and the agent restarted, not a `cargo build`.

### 15.6 Lifecycle

- **Wire**: F4 is a minor `STATUS_SCHEMA_VERSION` bump, sequenced with #1457 (REQ-WIRE-C-003); a
  `stats` shape change bumps that wire's own `JSON_SCHEMA_VERSION` (REQ-USG-C-003).
- **Storage**: the usage store migrates once, keeping every sample (REQ-USG-C-001).
- **Migration artifacts**: unchanged; the frozen `FORMAT_VERSION` fixtures stay byte-identical
  (REQ-MIG-C-001).
- **Records**: F8 and F10 add decision records; F9 amends ADR-0032 and the migration PRD with dated
  notes (REQ-DOC-C-001).

## 16. Change Log

| Version | Date | Change | Rationale | Impact |
|---|---|---|---|---|
| 1.0 | 2026-10-26 | Created by `/scope` Stage 1, `/capture-requirements` run headless | The operator asked "/scope the gaps discovered" after the investigation of 2026-10-25 | Blocked from scoping by `dor_status: failed` until checks 1 and 6 pass |
| 1.1 | 2026-10-27 | § 14b filled by `/scope` with the tracker items (#1550–#1585); § 2.2's quoted transcript rewritten with synthetic labels of the same shape (scope brief row `prd-privacy-synthetic-labels`, 2026-10-27 09:46Z) | the items existed, and issue #15 keeps addresses out of a committed record | none on the requirements; `dor_status: failed` stands |
| 1.2 | 2026-10-30 | A-5's confidence and evidence cells updated after #1582's readiness gate (run 2026-10-28, re-run 2026-10-30) | the assumption was measured on a stand-in, not the real roster | none on the requirements |
| 1.3 | 2026-11-02 | `file:line` citations re-resolved against `363698d`; pre-submit polish corrections: § 2.11 row 1 and § 2.14 row 4 (ADR-0032 line range), § 7's #1018 row, § 8.1's REQ-DOC-B-003 exception, § 8.4 items 9 and 10, § 11's view count, § 15's disclosure line; header Version and Last Updated advanced and rows 1.1–1.2 recorded | corrections found by the pre-submit review of the branch | none on the requirements |
| 1.4 | 2026-11-03 | § 1 TL;DR and § 4 benefit hypothesis composed by the `/scope` orchestrator after the operator ruled the TL;DR is never theirs to write; § 8.1 records the operator's rulings on `duplicate-label-policy` (refuse unless `--overwrite`) and `login-mismatch-policy` (refuse unless `--rename`) with the orchestrator's readings marked as such; § 8.4 item 12 | the operator's words of 2026-11-03, recorded where the decisions live | AC-6 of the migration PRD narrowed by an amendment note; the items that built the warn or keep-rename default (#1556, #1558, #1559, #1560, #1561, #1570) re-enter the ready gate; `dor_status` unchanged |
| 1.5 | 2026-11-03 | § 8.1's two Ruling paragraphs gain the readings settled for the re-authoring (design § 12.3); REQ-IDN-I-001 and REQ-IDN-I-002 marked vacuous; AC-ROS-001, AC-ROS-003, AC-IDN-001, AC-IDN-002 and AC-IDN-004 re-keyed to the refuse form | the six items that read the two answers were re-authored on one contract, critiqued in a fresh context first | #1556, #1558, #1559, #1560, #1561 and #1570 re-authored and re-gated; `dor_status` unchanged |
