---
title: Account Label Integrity — Solution Design
scope: account-label-integrity
created: 2026-10-26
status: draft  # PROVISIONAL: the PRD's dor_status is failed (checks 1 and 6, approval gaps); the acknowledgment to proceed was supplied by the /scope orchestrator, not the operator, and is ratification-pending (§ 14.1)
source: docs/requirements/account-label-integrity.md
tracks:
  technical-architecture: complete
  data-architecture: complete
  ux-ia: complete
  api-design: complete
  security-architecture: complete
  testing-architecture: complete
---

# Solution Design: Account Label Integrity — Telling the Operator Which Account a Label Names

**PRD DoR**: `failed`. Checks 1 and 6 fail, and both are approval gaps: no requirement in the PRD is
operator-ratified. The `/scope` orchestrator acknowledged the verdict on the operator's behalf, so this
design proceeds as **PROVISIONAL**. That acknowledgment is pipeline-supplied and ratification-pending;
it is not the operator's (§ 14.1). Every decision below is pipeline-decided unless it names an operator
answer, and none does.

**Code citations** resolve against `363698d`. The design and the lens reviews in § 14.9 read the code
at `d9d75ba`, and every line they cite reads the same at both commits, so only the line numbers moved.

## 1. Goals and Drivers

The operator asked whether two roster entries can carry one label, and what then happens. The
investigation found that a label is the only handle most surfaces use, that the CLI's `capture` and
`login`, the socket `capture` and `config-set` can each make a label name two accounts without saying
so, and that nothing downstream can tell those two accounts apart. This design makes every such write
audible at the moment it happens, and gives every downstream surface an identity that a relabel does not
change. It does both without enforcing label uniqueness, which ratified R-6 / AC-6 (`docs/requirements/migration-credential-portability.md`) forbid. Amended 2026-11-03: the operator's `duplicate-label-policy` ruling (§ 12.2) enforces uniqueness at the four write paths, where a write that would make a label name a second account is refused unless `--overwrite` is given; AC-6's bar stands at load time and at `import` until the operator rules on import.

| Feature | Goal of the design |
|---|---|
| F1 `label-collision-notice` | the write that makes a label name a second account says so, on the surface that made it |
| F2 `login-landing-disclosure` | a `capture` or `login` that renames an existing account names the label it had |
| F3 `shared-credential-detection` | a write that puts one credential under two entries is reported, never acted on |
| F4 `identity-keyed-surfaces` | the status wire, panel rows, usage history and swap lines identify an account by more than its label |
| F5 `import-payload-uniqueness` | a payload with two credentials for one uuid, or one for no rostered uuid, is reported before any write |
| F6 `import-identity-check` | an imported credential whose identity names another account is refused, alone |
| F7 `identity-premise-measurement` | the identity premises of PRD § 2.9 are measured on real logins, or recorded as unmeasurable |
| F8 `label-normalization` | what makes two labels "the same" is decided and recorded |
| F9 `uniqueness-doc-accuracy` | committed text that disagrees with the code is amended, never rewritten |
| F10 `orphan-stash-disposition` | an orphan `Sessiometer/<uuid>` item is reaped, reported or accepted, by recorded decision |

Success is the PRD's own measures: MET-T-001 to MET-T-004 within two weeks of each feature, MET-R-001
to MET-R-003 at 30, 60 and 90 days, and MET-NSM-001 as the north star (PRD § 4). One of them is a proxy:
MET-T-004 counts the usage store's key, not what `stats`, `reliability` and the panel's stats view print,
and under D-8 those readers still group by label, so the meter can read 0 while the merge and the split the
operator sees in them remain. The product lens returned that to the PRD (§ 14.6 item 12).

## 1b. Non-Goals

- **Enforcing label uniqueness at load time, or anywhere the operator has not ruled.** AC-6 forbids it.
  Narrowed 2026-11-03: the operator ruled `duplicate-label-policy` as refuse unless `--overwrite` (§ 12.2), which
  enforces uniqueness at the four write paths and re-ratifies AC-6 as narrowed; load time and `import` keep
  AC-6's bar.
- **Choosing, merging, parking or removing either of two entries that share a credential.** ADR-0028 defers
  the only source of that knowledge (#737). F3 reports.
- **Displaying the account identifier on F4's panel rows or in `status`'s rendered output.** F4 carries it
  for identity only (AC-WIRE-001 BUT NOT); a general display affordance is a deferred Bucket-3 candidate.
  Two narrower displays stand apart from it: a notice names a uuid beside an ambiguous label, as
  REQ-NTC-C-001 and REQ-STSH-B-002 require (UX-9), and an ambiguous Settings row's own uuid is built only on
  the operator's explicit answer (`settings-row-shows-uuid`, § 14.6 item 10b). `status --json` prints the
  raw wire, so it shows whatever the wire carries (§ 12.2).
- **A durable event-log record when a label collides or a login relabels.** PRD § 3 defers it.
- **Consuming the new usage key in `stats`, `reliability` or `log`** this batch (D-8). The key is stored and
  carried; what reads it is `stats-consumes-the-key` (§ 14.6).
- **Duplicates that arrive through `config restore` or a hand edit**, the inverse orphan, an online identity
  check, and the Linux and Windows credential stores (#26, #27). PRD § 3 holds each, with what reopens it.

## 2. Constraints

| Constraint | Source | Effect on this design |
|---|---|---|
| Labels are non-unique by ratified design | R-6, AC-6 | no uniqueness check at load; at the four write paths the operator's ruling of 2026-11-03 refuses a write that would make a label name a second account unless `--overwrite` is given (§ 12.2), which re-ratifies AC-6 as narrowed |
| Schema wires version independently | CLAUDE.md § Schema versions | F4 bumps `STATUS_SCHEMA_VERSION` only; `stats`, `log`, `reliability` stay put |
| One `STATUS_SCHEMA_VERSION` minor for F4, shared with #1457's claim | REQ-WIRE-C-003; #1457 | the version number is taken at merge time, not pre-assigned |
| Swift decoders throw on an unknown `result` value or rejection tag | #1474; `CaptureAck.swift` | notices ride additive fields; any new tag lands with its Swift case |
| Issue #15 redaction meter; the email and token never named | REQ-NTC-C-001, REQ-STSH-C-001 | uuids only where a label is ambiguous; no credential-derived value anywhere |
| Issue #160 framing scan for new prose | REQ-NTC-C-002, #1167 | notices are named templates under the advisory audience |
| No new dependency, except a licence-checked one if confusable detection is adopted | REQ-LBL-C-002; `deny.toml` | everything else uses the standard library and existing crates |
| macOS-only credential store; Linux compiles but is not gated | CLAUDE.md; #963, #964 | no new platform call in shared code; Linux compile checked by hand |
| Frozen migration fixtures | REQ-MIG-C-001; `FORMAT_VERSION` | import checks are read-side only |
| Six-week Big Batch with a circuit breaker; every requirement unratified | PRD § 3, § 14c | features land independently; this design stays `draft` |

## 3. Context and Scope

The system is one Rust crate that builds the daemon and the CLI, plus the SwiftUI menu-bar app. The app
talks to the daemon over a local AF_UNIX control socket whose peer check refuses other users. State lives
in `config.toml` (the roster), in the login keychain (one `Sessiometer/<uuid>` stash item per account), in
an append-only event log, and in the usage store (a raw JSONL tier plus account-free rollups). Claude Code
is the one external system: it writes the canonical credential and the `oauthAccount` identity that
`capture`, `login` and `import` read.

In scope are the roster writers (`capture`, `login`, the socket `capture` and `config-set` commands,
`import`), the status/watch wire, the panel's account list, capture card and Settings accounts section,
the usage store, the swap and emergency-swap event lines, the stash namespace, and the committed records
F7 to F10 name. Nothing outside the operator's machine changes: no request to the provider is added.

## 4. Solution Strategy

The system stays a modular monolith with one client. No component is added that runs on its own: every
change is a module inside the crate or the app. These patterns carry the design.

1. **A shared kernel for the collision rule.** Import already counts a label's bearers before and after a
   write. That rule moves to the roster module and every writer calls it (D-1).
2. **One plan seam per capture.** `plan_capture` already runs before the stash write, so it returns the
   previous label and the collisions it computed, and any refusal a policy chooses sits between the plan
   and the write (D-2).
3. **Tolerant-reader wire evolution.** New facts ride optional fields that older readers ignore; a new
   rejection tag is the only breaking addition, and it lands with its Swift case (D-6, D-10).
4. **Expand, then backfill.** New usage samples carry the account uuid; one sentinel-guarded pass
   attributes older samples whose label had exactly one bearer (D-7).
5. **An anticorruption check at import.** The artifact's credential list is checked as a payload before the
   swap lock, and each entry's identity is checked before its write (D-11).

**Delivery order** (it is also the cut order, reversed): #1474 before the socket halves of F1 and F2; F1,
F2 and F3 together (they share the plan seam and the write paths); F5 and F6 together; F4 as separate
items, those that wait on no decision first — the uuid on new samples with the event-line tokens (D-5), including the
tokens on F3's `credential_shared` line (D-9); then, once the mock frame exists (REQ-ROW-C-002), the app-side
duplicate-set items, #1566 (the row fallback, UX-4, and the panel ordinal, UX-3), #1573 (the blocked switch, UX-10)
and #1574 (the Settings ordinal), a split Stage 3 made and left ratification-pending — which remove the duplicate-row defect with no wire change, the Settings half waiting on the
Accounts row anatomy (owner open, § 14.7); then the wire-and-panel item as the tracer bullet, once `account-key-form` is
decided; the store backfill last, and only if the PRD keeps it
(§ 14.6 item 11); F9's amendments any time, except the `LOGIN_USAGE` line, which lands after
`login-mismatch-policy` is answered or is worded to its answer, coordinated with #1379; F7's walk whenever
the operator runs it; F8 and F10 after their decision records. One item is cut first if the circuit
breaker trips: REQ-NTC-B-001, a Could outside the identity theme, whose count wording rides D-3's templates
and leaves every other notice intact.

## 5. Building Blocks — the design decisions

### D-1 — one collision rule, at the roster *(REQ-ROS-B-002, REQ-ROS-Q-001; the F8 hook)*

Import's rule is the rule REQ-ROS-B-002 states: a write creates a collision on a label when that label's
bearers after the write are more than one and more than before it. It lives today inside `apply_import`,
as `label_bearers` (`src/cli.rs:5245`) and `created_ambiguity` (`src/cli.rs:5425`). It moves beside
`Account` (`src/config.rs:360`) as one function over two roster slices, `label_collisions(before, after)`,
returning each collided label with its bearers' uuids; the bearer count is their number.

Every roster writer calls it: `run_capture` and `run_login` through `plan_capture` (D-2), the socket
`config-set` through `apply_settings` (`src/config/settings.rs:37`), which already holds the configuration
before and after the overlay, and `apply_import`, whose behaviour does not change. A comparison-key
parameter is the identity function today; F8 plugs its normalization rule in there, so a confusable
check inherits REQ-ROS-B-002's calibration (REQ-LBL-B-001) instead of growing a second rule.

Calibration falls out of the before/after form. A `config-set` that swaps two labels leaves every count
where it was (AC-ROS-002). A refresh of an entry whose label already had two bearers leaves the count at
two. A relabel to the label an entry already carries changes nothing. A new account under a fresh label
creates no bearer above one. Those are REQ-ROS-Q-001's fixtures.

A source-scan test pins that every function that writes the roster calls the rule. The precedent is
`every_reconcile_roster_call_site_reaches_the_one_shared_floor` (`src/daemon/commands.rs:4723`), which
pins every caller of `reconcile_roster`. It is what stops a fifth writer —
the socket `login` #1021 — shipping without the rule (premortem PM-19; the PM-n numbers index the design stage's pre-mortem pass, whose record is machine-local scratch that this repository does not carry).

**Refuse form (2026-11-03, § 12.3 Terms and C-7).** The shared function carries a per-account calibration
instead of the count rule above: a write creates a collision when it gives an account a label it did not
carry before and another account carries that label after the write. The count rule lets a save move an
account onto a label two others already share; the per-account rule refuses it and still keeps the
label swap of AC-ROS-002 quiet. `apply_import` keeps `label_bearers` and `created_ambiguity` unchanged
until the operator rules on `import`, and the source-scan test lists it as the one writer with its own
rule; the F8 hook plugs into the shared function.

### D-2 — the plan seam returns what the reply needs, before any write *(REQ-ROS-B-001, REQ-IDN-B-001, REQ-IDN-C-002)*

`plan_capture` (`src/capture.rs:635`) is the one place that matches a harvested uuid to an entry, and it
runs before `stash.write` in both `run_capture` (`src/capture.rs:546`) and `run_login` (`src/capture.rs:777`).
Today it overwrites `existing.label` in place, so the previous label is lost. It returns a plan instead:
the outcome, the previous label when the label changed, and the collisions D-1 computes from the roster
before and after its own in-memory edit.

Any refusal a policy chooses is one gate between the plan and the stash write: under `duplicate-label-policy`
= refuse, a plan with a collision stops; under `login-mismatch-policy` = refuse, a plan that renames another
account stops. Nothing is written and nothing is rostered, which is AC-IDN-003 and AC-ROS-001's BUT NOT.
Under the warn and keep options the gate passes everything. A refusal after the write would need a
keychain delete to undo it, and that delete can fail and leave an orphan (PM-14).

### D-3 — notices are templates, and acks carry facts *(REQ-NTC-C-001, REQ-NTC-C-002, REQ-NTC-B-001)*

Every new operator-facing string is a named template in one module, registered with the #160 scanner
under the advisory audience (#1123); none is an inline literal (PM-13). The CLI renders the templates.
Socket acknowledgements carry structured facts — labels, the bearers' uuids, a previous label — and never
rendered prose; the app renders its own strings, scanned by a Swift test that reads
`src/framing_vocabulary.rs` the way `NotificationDeliveryTests` already does.

An account is named by its label. Its uuid is added only where the label alone is ambiguous, meaning it
has more than one bearer at that moment (REQ-NTC-C-001). That rule binds every rendered line: the CLI
reply, the capture card, the Settings note and the event line. The acknowledgement itself carries the
uuids as facts whenever it carries a label, because it is a machine contract on the socket that already
carries every uuid in `config-get`, and the app applies the naming rule when it renders (§ 8.1, § 9.6).
The harvested email is never an input to any
template. The "in rotation" count becomes the number of enabled entries. That is the roster's own
definition: `enabled` says whether an account "participates in the rotation" (`src/config.rs:375-381`); a roster with no parked entry
prints exactly what it prints today (REQ-NTC-B-001, DQ-10).

### D-4 — shared-credential detection at the creating write *(REQ-STSH-B-001, REQ-STSH-B-002, REQ-STSH-Q-001, REQ-STSH-Q-002, REQ-STSH-C-001)*

A refresh rotates the refresh token (A-2), so two byte-identical stashes split at the first refresh of
either. Detection therefore runs at the write that created the pair, on each of the four paths that can roster a new entry's stash: CLI
`capture`, CLI `login`, the socket `capture` and `import`. After the roster is saved and the swap lock is
released, the just-written `Credential` (still in memory) is compared, through
`Credential::matches` (`src/keychain.rs:356`), with each other entry's stash: at most one
`AccountStash::read` per other entry.
Credentials written together by one import are compared with each other in memory first, at no read cost.
Nothing runs on the poll or the swap path.
Two further stash writes exist outside those four paths: the swap engine’s re-stash of the outgoing
account (`outgoing_stash`, `src/swap.rs:835-846`) and the daemon’s re-auth `restash_account`
(`src/daemon/canonical.rs:401-422`). Each rewrites an existing entry’s own stash under that entry’s uuid,
so neither adds a roster entry; whether F3 counts them as covered, or as unable to create a pair, is a
ratification question this decision leaves open.

Reading after the lock means a concurrent daemon swap never waits on these reads
(`SWAP_LOCK_MAX_WAIT`, `src/swap.rs:964`). The CLI already reads every stash outside the daemon:
`export` does, through `stash.read` in `gather_payload` (`src/cli.rs:4968`), so no new keychain
access class appears. A failed read skips that one
comparison and prints nothing: the absence of a report is never a claim that no pair exists.

A detected pair produces a notice on the writing surface and one `credential_shared` event line (D-9),
both naming the two entries by label, and each by its uuid where its own label has more than one bearer —
D-3's rule, which covers REQ-STSH-B-002's equal-label case — in the CLI reply and on the card; the socket
acknowledgement carries both uuids as facts (REQ-STSH-B-002, REQ-NTC-C-001; § 8.1, § 8.5, § 9.6). Nothing derived from the
bytes — no digest, no fingerprint, no length — leaves memory. The roster, the write and the active
account do not change. The socket capture handler has one `Event` slot, already used by the capture's
own line (`src/daemon/commands.rs:389`); F3 widens it to a `Vec<Event>`, as the swap path's
`(SwapAck, Vec<Event>)` return already is (`src/daemon/commands.rs:106`). The canary keeps passing a
canonical that matches the displayed
account's own stash when the same bytes also sit under another entry (REQ-STSH-C-003).

### D-5 — which identity each surface keys on *(REQ-USG-B-001, REQ-EVT-B-001, REQ-WIRE-I-001)*

The account uuid is unique per roster entry, which `Config::validate` enforces — the roster's doc says
"unique `account_uuid`" (`src/config.rs:1074`) — and a relabel never changes it. Local stores key on it directly: the usage sample's
new field (D-7) and the event log's ambiguity tokens (D-9). The uuid already lives in `config.toml`, in
`list`, in the `config-get` view the app already reads, and in the usage-family events
(`ExhaustedSlowPollCleared`, `src/observability.rs:1968-1969`), so neither adds an exposure class.

Only the status/watch wire waits on DQ-1, the form of its identifier: the uuid itself or a value derived
from it (open stub `account-key-form`, § 12). That narrowing is deliberate. It keeps the open fork
off the append-only log and off the store migration, so DQ-1 stays a costly-to-reverse wire choice rather
than a one-way door. DQ-1's own text scopes it to "which identifier F4 carries on the wire".

### D-6 — the status/watch identifier and the next-swap target *(REQ-WIRE-I-001, REQ-WIRE-C-001, REQ-WIRE-C-002, REQ-WIRE-C-003, REQ-ROW-B-001)*

`AccountStatusLine` (`src/daemon/snapshot.rs:737`) gains one additive field carrying the account
identifier; `NextSwap::Target` (`src/daemon/snapshot.rs:878`) gains the target's identifier the same way.
Both are `#[serde(default)]` Options, the convention the target's `reason` already follows (#164): a current
daemon always sends them, an older one omits them. Field names follow DQ-1: `account_uuid` and
`to_account_uuid` for the uuid, `account_key` and `to_account_key` for a derived value.

The next-swap identifier goes beyond REQ-WIRE-I-001's "on each account line". It traces to DQ-4's next-swap
callout (#930). The panel has no per-row next-swap marker: R-2, re-ratified 2026-07-09, made the footer
callout next-swap's one home, as `rowAccessibilityLabel`'s doc records (`apps/menubar/Sources/StatusPanelFormat.swift:2172-2174`),
so with two rows named `work` the callout's "→ work" names neither. The target's identifier lets the callout
find its row and show that row's ordinal (UX-5), and it is the address a switch by identifier would send
(`panel-switches-by-identifier`, § 14.6 item 15).

All of F4's wire change is one minor bump of `STATUS_SCHEMA_VERSION` (`src/daemon/snapshot.rs:538`), with
the status/watch goldens regenerated by `cargo test -- --ignored emit_wire_golden_fixtures`, the
current-minor Swift fixtures moved, and the version-pinned ones left alone. #1457's body pre-assigns the
next minor; whichever change merges second takes the one after, so the number is read at merge time. The
same commit reconciles every statement that the status wire carries no uuid (REQ-WIRE-C-002): the
`canary` field's doc ("account-uuid (issue #15)", `src/daemon/snapshot.rs:666-668`), its Swift mirror, and
the panel's canary-banner doc. #791's acceptance line is tracker text and is reconciled by a comment on
#791 when the change lands.

### D-7 — the usage store: expand, then backfill once *(REQ-USG-B-001, REQ-USG-C-001, REQ-USG-C-002)*

`Sample` (`src/usage_store.rs:135`) gains `account_uuid: Option<String>`, omitted when absent like the
struct's other optionals; `acct` keeps the label. `record_usage_sample` (`src/daemon.rs:679`) receives the
polled account's uuid, so every new sample carries it.

One backfill, `backfill_sample_accounts`, runs in the daemon at start, before the first poll. It takes the
store's own lock, `usage.lock`, which `append_sample` and `compact_and_roll` already take
(`StoreLock::acquire`, `src/usage_store.rs:432` and `src/usage_store.rs:533`), and stops at once if its sentinel
exists. Otherwise it reads the samples, maps each label to its bearers in the current roster, sets
`account_uuid` on each keyless sample whose label has exactly one bearer, rewrites the file with the
store's existing atomic writer, and writes the sentinel last. A crash before the sentinel re-runs it over
the still-keyless samples only. It never creates the samples file and never drops a line, so the
prior-configuration witness, which asks only whether the file is a non-empty regular file
(`is_non_empty`, `src/witness.rs:215`), reads the same before and after.

The sentinel is a sibling file of the raw tier, not a field in the rollup document. An older binary
rewrites the rollup without unknown fields and would erase an in-document marker, so a later upgrade would
backfill again against a later roster and misattribute (PM-21). An older binary never touches a sibling
file. Its own compaction does drop `account_uuid` from the samples it rewrites; those become unattributed,
never misattributed.

Unattributed samples (DQ-2) stay in the raw tier and age out on its 14-day window
(`DEFAULT_RAW_WINDOW_SECS`, `src/usage_store.rs:101`) into the account-free rollups
(`Rollup`, `src/usage_store.rs:223`). Per-account history exists only in that window, so the backfill's whole reach is
at most those 14 days. Under D-8's default no reader consumes a backfilled key before the window ages it
out, so the backfill is owed only under one reading of AC-USG-001; the product lens returned that reading
to the PRD (§ 14.6 item 11), and the backfill is F4's last item, dropped if the turnover reading governs.

### D-8 — `stats`, `reliability` and `log` read what they read today *(REQ-USG-C-003)*

`stats` keeps grouping by the sample's label, and its JSON shape does not change, so neither its own
`JSON_SCHEMA_VERSION` (`src/stats.rs:80`) nor the Swift `StatsWire` mirror moves; AC-USG-002's BUT NOT
("a version bump when the shape did not change") holds. Mapping the key to the current label would carry
history across a relabel, which is the remedy for the harm PRD § 2.6 and A-9 name ("a relabel starts a new
history handle"); its cost is that `stats` output changes after a relabel on a roster with no duplicate
label. Whether F4's invariant reaches `stats` is an
interpretation, not a fact: MET-T-004 states the constraint over its four named surfaces, and `stats`
output is not among them, so the product lens reads continuity as an open product choice. Showing two
same-label accounts separately would change the shape: `PeriodWire.accounts` is a map keyed by label. Both
are operator options, and leaving both unchanged is the pipeline's default (`stats-consumes-the-key`,
§ 14.6 item 8).

### D-9 — the event log names an ambiguous account by its uuid *(REQ-EVT-B-001, REQ-STSH-B-002)*

`event=swap` and `event=emergency_swap` lines gain a trailing `from_uuid=` or `to_uuid=` token for a side
whose label has more than one bearer at write time, and nothing otherwise. Lines for a roster without a
duplicate label stay byte-identical, the trailing-token precedent of the #634 swap projection
(`Swap`, `src/observability.rs:1355`). The readers build a map from token names
(`parse_swap_events`, `src/usage_stats.rs:421`), so unknown tokens are ignored, and `log --json` carries
each line verbatim, so `log`'s own `JSON_SCHEMA_VERSION` (`src/log.rs:167`) does not move. F3 adds one event kind, `credential_shared` (§ 8.4).
The event module's doc says every `Event` field is "a HANDLE (the operator label)" (`src/observability.rs:19`); it
is amended in the same change, as the usage-family events already depart from it.

### D-10 — the acknowledgement contract and its parity gate *(REQ-ROS-I-001, REQ-ROS-C-001, REQ-IDN-I-001)*

`CaptureAck` (`src/daemon/socket.rs:656`) and `ConfigSetAck` (`src/daemon/socket.rs:580`) gain optional
fields only (§ 8.1). The app's probe ignores unknown sibling keys and throws on an unknown `result`, so a
field is safe and a new `result` value is not (#1474). A refusal option adds a tag to
`CaptureRejection` (`src/daemon/socket.rs:672`) or `ConfigSetRejection` (`src/daemon/socket.rs:613`), with its Swift case in
the same commit, gated by `every_rust_rejection_tag_has_a_swift_case` (`src/daemon/socket.rs:1979`).

REQ-ROS-C-001's gate for fields: Rust emits one golden per acknowledgement variant with every optional
field filled, and a Swift test decodes each golden and asserts every field's value. A decode that merely
does not throw proves nothing here, because unknown keys are dropped silently. The gate covers the fields
this PRD adds. Widened to whole bodies, it would fail on a drift that exists today: Rust's `captured` sends
`count`, Swift's `CapturedBody` reads `accounts` (`CaptureAck.swift:145-147`). That drift is outside this
PRD and is reported as a finding (§ 14.7).

### D-11 — import checks the payload, then each entry *(REQ-MIG-B-001, REQ-MIG-B-002, REQ-MIG-B-003, REQ-MIG-C-001)*

`apply_import` (`src/cli.rs:5277`) builds its credential map with a `collect` that keeps the last of two
entries for one uuid (`let secrets`, `src/cli.rs:5313`), before it takes the swap lock. F5's check runs on
`payload.accounts()` before that line: it reports every uuid with more than one credential entry and every
credential entry whose uuid the payload's roster does not name, in the form
`import-duplicate-uuid-policy` selects (§ 12). Today the second kind is silently unused, because the merge
loop walks the payload's roster.

F6 runs per entry, inside the lock, before `write_and_verify` (`src/cli.rs:5451`) writes: it parses the
entry's `oauthAccount` and compares its uuid with the uuid the entry is filed under, byte for byte, as the
roster compares uuids everywhere. A mismatch writes no stash and reports the account `failed` with its
label and both uuids; the other entries continue. One shared comparison function serves `plan_capture` and
F6, so if F7 premise 4 finds case variance the fix lands in one place. The artifact format and its frozen
`FORMAT_VERSION` fixtures do not change; tests run the named ignored emitters only, never
`--include-ignored`.

### D-12 — orphan stashes: a metadata census, and a seam for the policy *(REQ-STSH-C-004, REQ-STSH-B-003, REQ-STSH-B-004)*

An orphan is a `Sessiometer/<uuid>` item with no roster row (origins listed in PRD § 2.12). The census lists
item services from the keychain's metadata dump — the path the witness already uses without decrypting or
prompting (`any_stash_item_present`, `src/keychain.rs:814`) — and subtracts the roster's uuids. Under
`report` that list is the feature. Under `reap` the census feeds a reaper that deletes an item only while
`config.toml` loads, while no entry carries its uuid, and while the item is older than a grace window read
from the same metadata. No lock spans a capture's stash write and its roster save: the save happens after
the swap lock releases; `capture_locked` says so:
"the roster save is the caller's, after the lock releases" (`src/capture.rs:257`). The grace window is what keeps a reaper off an in-flight capture, and spike SP-2
tests whether the metadata carries a usable time (§ 14.4). Under `accept`, nothing is built.

### D-13 — the records *(REQ-MSR-B-001, REQ-MSR-C-001, REQ-LBL-C-001, REQ-DOC-B-001..004, REQ-DOC-C-001, REQ-STSH-C-004)*

F7's results land in `build/version-compat.md`, following its `# Issue #1020 side probe` section, or under
`docs/findings/`. The operator takes every login by hand on spike #130's route, and the walk is shared with
#1020, #1542 and #1546. F8's and F10's decision records land under `docs/adr/`, numbered when they land.
F9's corrections are dated amendment notes that keep the original wording (#1116's precedent): ADR-0032's
note names commit `86ab6aa`, the migration PRD's stale line anchors become symbol anchors (unless #1476
decides the policy first), its A-1 is regraded, and `LOGIN_USAGE` (`src/cli.rs:1010`) says that a label
given for a rostered account renames it, coordinated with #1379's edit of the same constant and landed
after `login-mismatch-policy` is answered, because a refuse answer would make "renames" false
(REQ-DOC-B-003 assumes the keep-rename option; § 14.6 item 14).

*Amended 2026-11-03: `login-mismatch-policy` was answered refuse (§ 12.2), so `LOGIN_USAGE` says what
§ 12.3 C-10 states instead: a label that would rename the landed account is refused unless `--rename` is
given, which renames it, with the rest of that entry; REQ-DOC-B-003 and AC-DOC-003 are re-keyed the same
way, and #1570 carries the sentence.*

### D-14 — platform scope *(PRD § 3; #26, #27)*

The design assumes no stash backend exists on Linux or Windows, because every stash call shells out to
`/usr/bin/security`. There F3 and F10 have no subject. Every new call goes through the existing
`AccountStash` trait or keychain module, so no macOS-only call enters a shared path, and the Linux build is
compiled by hand for any change that touches those paths until #964 gates it.

### Entity model — what each PRD object becomes

| PRD object (§ 9.1) | Store | Key | Change |
|---|---|---|---|
| RosterEntry | `config.toml` `[[accounts]]` | `account_uuid`, unique | none |
| AccountLabel | attribute of RosterEntry | none: non-unique by ratified design | none |
| CapturedIdentity | transient `oauthAccount` read | `accountUuid` | none |
| StashedCredential | keychain `Sessiometer/<uuid>`, `StashedAccount` (`src/stash.rs:68`) | service name | read by F3; listed by F10 |
| MigrationArtifact | encrypted file, `Payload` | the file | read-side checks only |
| UsageHistory | raw JSONL + account-free rollups | `ts` + `acct`; new optional `account_uuid` | additive field + backfill |
| EventLogLine | append-only event log | `ts` + `event` | conditional uuid tokens; `credential_shared` |
| OperatorNotice | not stored; durable only as an event line | — | templates + ack fields |
| AccountStatusLine | status/watch wire | position today; account identifier after F4 | one additive field |
| PanelAccountRow | SwiftUI `AccountRow` | `label` today; account identifier after F4 | row identity |
| IdentityMeasurement, ProjectRecord | committed documents | path + anchor | new and amended records |
| *backfill sentinel* (storage-only) | sibling file of the raw tier | path | new; serves REQ-USG-C-001 |

## 6. Runtime View

### 6a. Task flows

The menu-bar task flows are in § 9.3: adding an account from the panel, renaming one in Settings, and
reading and switching in a list in which two accounts share a label. The CLI flows, under § 6b, are the same writes seen from the
terminal.

### 6b. System interactions

**CLI `capture` or `login` that collides, relabels or shares a credential** (F1, F2, F3):

1. The witness gate admits the write, as today.
2. Under the swap lock: identity read, active-credential read, then `plan_capture` returns the outcome, the
   previous label and the collisions (D-2). A chosen refusal stops here, with nothing written.
3. Still under the lock: `stash.write`. The lock releases.
4. The roster is saved under its own config-write lock (#1445); the daemon is told to reload, best-effort.
5. Outside both locks: F3 compares the written credential with each other entry's stash (D-4).
6. The reply prints the confirmation line (verb, label, previous label when it changed, the enabled count),
   then one line per notice: collision, shared credential. A detected pair also appends one
   `credential_shared` line, best-effort like the login audit line (`emit_login_event`, `src/capture.rs:359`).

**Socket `capture`** (panel): the same steps run inside
`perform_socket_capture` (`src/daemon/commands.rs:280`), which then reconciles the roster and acknowledges `captured` or
`refreshed` with the new optional fields (§ 8.1). F3's event line joins the capture's own line once the
handler returns a `Vec<Event>`.

**Socket `config-set`** (Settings): `perform_config_set` (`src/daemon/commands.rs:478`) calls
`apply_settings`, which now also returns the collisions between the configuration before and after the
overlay. A collision rides `applied.collisions`; under `duplicate-label-policy` = refuse it becomes the
`label-in-use` rejection with nothing saved.

**`import`**: decode the payload; F5's payload check (D-11) reports or refuses before the swap lock; the
merge loop runs F6's identity check per entry before `write_and_verify`; the collision rule runs over the
roster before and after, as today; after the lock releases F3 compares the written credentials, first with
each other in memory, then with every entry the import did not write.

**Daemon snapshot and polling** (F4): each snapshot carries the account identifier on every account line
and on the next-swap target; each poll appends a sample carrying the uuid; once, at start and before the
first poll, the backfill runs under `usage.lock` (D-7). A swap whose label is ambiguous adds its uuid
tokens (D-9).

## 8. Interface Contracts

Conventions follow the repository, not a template: snake_case JSON fields, kebab-case rejection tags,
optional fields omitted when absent, snake_case event names. The golden files under `build/fixtures/` are
the published contract, and the Swift fixtures and decoders are its consumer.

### 8.1 Socket acknowledgements — additive fields only

```json
{"result":"captured","label":"work","count":3,"account_uuid":"<uuid-B>",
 "collisions":[{"label":"work","bearers":["<uuid-A>","<uuid-B>"]}],
 "shared_with":[{"label":"spare","account_uuid":"<uuid-C>"}]}
{"result":"refreshed","label":"work","count":3,"account_uuid":"<uuid-B>",
 "previous_label":"spare",
 "collisions":[{"label":"work","bearers":["<uuid-A>","<uuid-B>"]}]}
{"result":"applied","effect":"live",
 "collisions":[{"label":"work","bearers":["<uuid-A>","<uuid-B>"]}]}
```

| Field | On | Present when | Requirement |
|---|---|---|---|
| `account_uuid` | `captured`, `refreshed` | always: the written entry's uuid, so a notice can mark the entry the write touched | REQ-NTC-C-001, REQ-STSH-B-002 |
| `collisions[]` (`label`, `bearers[]`: the bearers' uuids, in roster order) | `captured`, `refreshed`, `applied` | the write created a collision (D-1) | REQ-ROS-I-001, REQ-ROS-I-002 |
| `previous_label` | `refreshed` | the refresh changed the account's label | REQ-IDN-I-001 |
| `shared_with[]` (`label`, `account_uuid`) | `captured`, `refreshed` | F3 detected a byte-identical pair | REQ-STSH-B-002 |

`count` keeps its meaning, the number of roster entries; DQ-10 changes CLI wording only. The app does not
render `count` today, because Swift reads a field named `accounts` (§ 14.7). The uuids are facts for the
app's renderer, which shows one only beside an ambiguous label (D-3, § 9.6), and their roster order gives
each bearer its ordinal (UX-3); they ride the socket that
already carries every uuid in the `config-get` view, so no exposure class is added (SEC-4). The CLI names
uuids from its own process.

**Refuse form (2026-11-03, § 12.3 C-12).** Under the operator's rulings no completed write creates a
collision or completes a socket rename, so `collisions` on `captured`, `refreshed` and `applied`, and
`previous_label` on `refreshed`, do not ship. `config-set`'s `rejected` arm carries `collisions` beside `reason`
with the reason `label-in-use`. `account_uuid` and `shared_with` are unchanged.

### 8.2 Rejection tags — only for an option the operator chooses

| Tag | Enum | Added only if |
|---|---|---|
| `label-in-use` | `CaptureRejection`, `ConfigSetRejection` | `duplicate-label-policy` = refuse |
| `relabels-another-account` | `CaptureRejection` | `login-mismatch-policy` = refuse, for the socket capture's relabel |

Each tag lands with its Swift case in the same commit. The CLI's refusal is an `Error` with its own
template under the `Error`-template audience (#1139).

Both tags are added: the operator chose refuse for both decisions (§ 12.2). The capture tags carry no other
field, as `docs/specs/capture-rejection-parity.feature.md` requires; their shapes and owners are § 12.3 C-6,
C-7 and C-12.

### 8.3 Status and watch frames — one minor bump

Each account line gains the account identifier beside `label`; the next-swap target gains its identifier
beside `to`. Names: `account_uuid` / `to_account_uuid` if `account-key-form` selects the uuid, `account_key` /
`to_account_key` if it selects a derived value. Both are absent from an older daemon's frames. The app's
`AccountStatusLine` (`apps/menubar/Sources/WireModel.swift:637`) decodes the identifier as optional; a
fixture pinned at the outgoing minor, carrying two account lines with one label, proves the fallback
(REQ-ROW-C-001).

### 8.4 Event lines

Each line keeps the log's existing prefix and grammar; tokens are additive and trailing.

```text
event=swap ... from=work to=work from_uuid=<uuid-A> to_uuid=<uuid-B>
event=credential_shared via=capture account=spare matches=work
event=credential_shared via=socket_capture account=work matches=work account_uuid=<uuid-B> matches_uuid=<uuid-A>
```

A `*_uuid` token appears only for a side whose label has more than one bearer when the line is written.
`via` is one of `capture`, `login`, `socket_capture`, `import`. One line is written per detected pair.

### 8.5 CLI replies

The confirmation line keeps its verb and its count; notices follow it on stdout, one line each.

```text
Captured "work" (now 2 in rotation).
"work" now names 2 accounts: <uuid-B> (this one) and <uuid-A>. Commands that take a label refuse "work" until one is renamed; `sessiometer use <uuid>` reaches either, and `sessiometer capture <new-label>` renames the one you are signed in to.
Refreshed "work", previously "spare" (still 2 in rotation).
Revived "work", previously "spare" (still 2 in rotation).
"spare" and "work" hold the same credential.
"spare" and "work" <uuid-B> hold the same credential.
"work" <uuid-B> (this one) and "work" <uuid-A> hold the same credential.
```

The collision line marks the entry the write touched and, under the warn option, says what the duplicate
costs until one entry is renamed and how to rename from the CLI, which has no rename verb (`ROOT_USAGE`):
`capture <new-label>` relabels the signed-in account, the path
`recapture_updates_the_label_when_a_new_one_is_given` pins, so the hint's wording follows
`login-mismatch-policy`, whose refuse option would close that path. Under a refuse option for
`duplicate-label-policy` the refusal's `Error` template replaces the line (§ 8.2). The shared line follows
D-3's rule: each side carries its uuid where its own label has more than one bearer, which covers
REQ-STSH-B-002's equal-label case and REQ-NTC-C-001's ambiguous one. Import adds a
payload-level notice per finding, worded by `import-duplicate-uuid-policy`, and F6's
per-account line, in #1565's detection-only wording: `` failed `work`: the entry is filed under <uuid-A>, but its
oauthAccount names <uuid-B>. Nothing was written for it. `` (this design's earlier draft said the credential
"belongs to" the second uuid, which the comparison cannot tell; #1565 corrected it). No template contains a
token from the #160 banned list (`BANNED_TOKENS`, `src/framing_vocabulary.rs:182`).

**Refuse form (2026-11-03, § 12.3).** The collision line and the "previously" form above describe the warn
option. Under the rulings the CLI prints a refusal before any write (C-9), a rename made with `--rename`
names both labels, a holder replaced with `--overwrite` is named by its uuid with its stored login deleted
(C-5, C-8), and a derived label is made free with a suffix rather than reported (C-2). The shared-credential
lines and the import lines stand.

### 8.6 What each surface's version does

| Surface | Change | Version effect |
|---|---|---|
| status / watch | identifier on account lines and next-swap target | one minor `STATUS_SCHEMA_VERSION` bump, number read at merge time |
| capture / config-set acks | optional fields; tags only under a refuse option | none for fields; Swift lockstep for a tag |
| `stats --json` | none (D-8) | none |
| `log --json` | lines carried verbatim; new event name | none |
| `reliability --json` | none | none |
| migration artifact | none | none (`FORMAT_VERSION` frozen) |
| CLI text | notices; previous label; enabled count | none (no machine contract) |

## 9. UX Architecture — menu-bar surfaces

The CLI's text is in § 8.5. This section covers the app only: no new screen, no new navigation node and no
new call to action. The existing surfaces in the Screen Inventory gain states and cues. The design mock
(`apps/menubar/design/menubar-preview.html`) is the canonical build reference and authors none of these
states yet, so each is authored there before Swift builds it. The mock authors the Settings window too, as
Group 8 (#763); its Accounts row anatomy is the one open axis there (register R-11, owner open, § 14.7), so the
Settings half of these states waits on it.

### 9.1 Information architecture

| Decision | What | Why |
|---|---|---|
| UX-1 | Notices are inline on the surface that requested the write: never a modal, a toast or a system notification | a warn-policy notice blocks nothing, and a transient element vanishes before it is read (PM-16) |
| UX-2 | A notice stays until the operator's next action on that surface. On the capture card that is a new capture, Esc, the card's dismissal or the panel's close, which already resets the card (`dismissCaptureSurface`, `AccountCaptureModel.swift:138-140`); a `done` card that carries a notice is exempt from the 1.8 s confirmation reset (`scheduleIdleReset`, `AccountCaptureModel.swift:188-190`). In Settings the note follows the roster's state (UX-11) | the card's `done` line resets itself, every panel close resets the card, and opening Settings closes the panel; so the fact the card carries — which entry the write touched — also rides the ordinal, which Settings shows (UX-3) |
| UX-3 | Rows sharing a label differ by an ordinal — "n of N", their position among the rows sharing the label in roster order — drawn as a badge set apart from the label text, meeting WCAG 1.4.3 for its text and 1.4.11 for its boundary, and hidden from VoiceOver where the row's or field's name already speaks the ordinal; it has no tooltip of its own, because a row-level tooltip wins over a child's and #953 AC-3 recorded the active row's silence, so a blocked duplicate row's tooltip explains the duplicate instead (UX-10); by colour slots inside the set that no other row uses while the eight-slot palette has one free, and past that the least-used slot; and by a spoken cue. The ordinal is the duplicate set's one handle on every surface: the rows, the footer callout, the card's lines, the Settings rows and note, and the header's active-account line. A swap result keeps the acknowledgement's labels, because `swapDoneText` never guesses beyond them | the identifier is never displayed on a row (AC-WIRE-001 BUT NOT); colour is never the only cue (WCAG 1.4.1); the status frame (`AccountReading`, `src/daemon/snapshot_build.rs:417`) and `config-get` (`ConfigView`, `src/config/settings.rs:65`) both list accounts in roster order, and Settings does not sort, so every surface computes the same ordinal with no wire change; rosters without a duplicate render unchanged |
| UX-4 | Row identity is the account identifier; with an older daemon, position plus label. The switching state keys on row identity too | two same-label lines must stay two rows (REQ-ROW-C-001); a label-keyed switching state (`pendingTarget`, `apps/menubar/Sources/StatusPanelRoster.swift:256`) would spin on both rows |
| UX-5 | The footer callout, next-swap's one home, names its target by label and, when that label has more than one bearer, by the target row's ordinal, found by the target's identifier: "→ work 2 of 2", spoken "Next swap to work, 2 of 2 accounts named work". With an older daemon it cannot find the row and says "one of 2 accounts named work". Either way it keeps its lead and its why line, adds UX-10's reason as a resting line, and disables its Swap | R-2 (re-ratified 2026-07-09) dropped the per-row next-swap marker, so the callout is the only surface DQ-4's question can land on (#930), and "→ work" alone names neither row |
| UX-6 | The relabel folds into the card's outcome line, "Refreshed ‘work’ (was ‘spare’)", and appears only once #1474 lets the app decode `refreshed`; the card's lines run outcome, collision, shared | AC-IDN-004 BUT NOT; one write reads as one verb, as the CLI's single confirmation line does (§ 8.5) |
| UX-7 | A refusal, if a policy chooses one, reuses the card's Refused state and Settings' rejected-save state (both policies chose one on 2026-11-03: the card's two refused states and Settings' refused save, § 12.3 C-6, C-7, C-9; UX-6's relabel line never appears on the card, since a socket relabel is refused) | same decision shape as the existing rejections |
| UX-8 | Each notice line is announced as a status message without moving focus (WCAG 4.1.3); the announcement carries the ordinal sentence, and the uuids stay in the visible text for reading on demand. Each duplicate row's accessible name carries its ordinal (WCAG 4.1.2), and so does each duplicate Settings label field's accessible label, "Label, 2 of 2", before VoiceOver reads the field's value (WCAG 2.4.6); during an unsaved edit the ordinal still reflects the saved roster; rows without a duplicate keep today's names. Superseded for the Settings refusal on 2026-11-03: no announcement replaces the warn form's, since a colliding save is refused (§ 9.5 Refuse form, § 12.3 C-9); the ordinal and accessible-name clauses stand | the quality-pillar review found these missing (§ 11.4); the UX lens found two duplicate Settings rows indistinguishable to VoiceOver, since each reads "Account", "Label", "work" and "Active" (`accountRow`, `apps/menubar/Sources/SettingsView.swift:228-241`) |
| UX-9 | A notice names an entry by its uuid only beside a label that has more than one bearer, and always next to its ordinal (§ 9.6). The F4 panel rows never show an identifier. An ambiguous Settings row shows its own uuid only if the operator answers `settings-row-shows-uuid` yes; if built, the line belongs to its row's accessibility container, is spoken as a short prefix with the full uuid as the field's hint, and wraps beside the 160 pt field | REQ-NTC-C-001 names the uuid "where the label alone is ambiguous" and REQ-STSH-B-002 (a Must) names both entries by uuid where their labels are equal, on the surface that wrote; AC-WIRE-001's BUT NOT binds the rows and the wire; PRD § 3 defers a display affordance until "The operator adds it", so the row line waits for that answer; both questions return to the PRD (§ 14.6 items 10a and 10b) |
| UX-10 | Switching to a row whose label has more than one bearer is shown blocked before the click. The row takes the existing blocked treatment, a resting reason, "Can’t switch — another account has this label.", and a row tooltip that adds the remedy, "Rename one in Settings."; the check comes first among the `SwitchBlock` cases, because the daemon resolves the target before any viability gate (`perform_socket_swap`, `src/daemon/commands.rs:103-116`). The callout, which has no blocked state today (its Swap is disabled only while a swap is pending), keeps its lead and why line, adds the same reason as a resting line, and disables its Swap. The after-click refusal stays as the fallback for a frame that lags a new duplicate, reworded to match. A duplicate row's spoken label and the Swap button's accessible label carry the ordinal; its tooltip does not, because the badge beside the label already shows the ordinal to a sighted reader (§ 9.6's copy table and #1573 pin the tooltip as reason plus remedy; reconciled by the `/scope` orchestrator on 2026-10-27, the trailing sentence having read "hint and tooltip" since revision 4) | the daemon refuses an ambiguous target (`AmbiguousTarget`, `src/daemon/commands.rs:114`); the wire proves the label is shared, and the panel blocks before the click exactly what the wire proves the daemon will refuse (`SwitchBlock`, `apps/menubar/Sources/StatusPanelFormat.swift:173-178`); today the refusal arrives only after the click. Switching by identifier instead is recorded for the operator (`panel-switches-by-identifier`, § 14.6 item 15) |
| UX-11 | The Settings collision note follows the roster the section holds: it shows while any label has more than one bearer, whichever surface or command made the duplicate, survives unrelated edits, and leaves once one entry is renamed; a save whose `applied.collisions` reports a new collision is also announced (UX-8; superseded 2026-11-03: a colliding save is refused with `label-in-use` and not announced, § 9.5 Refuse form, § 12.3 C-7 and C-9). A window left open keeps the roster it loaded until it is reopened, because only `show()` loads it (`SettingsWindowController`, `apps/menubar/Sources/SettingsView.swift:71-72`) | a note tied only to a save's acknowledgement would miss a duplicate made by a panel capture, `login` or `import`, and an unrelated save would clear it while the duplicate remains; the announcement stays calibrated to the write that created a collision (REQ-ROS-Q-001) |

UX-3 is an author-chosen default. Its alternative re-seeds every row's colour from the identifier, which is
what #445's acceptance criteria first asked for: colours then survive a relabel, but every panel repaints
once and the panel goldens move for rosters with no duplicate (`colour-seed-from-identifier`, § 14.6).

UX-9, UX-10 and UX-11 are also author-chosen. The product lens's first reading found that a uuid-free panel
dropped REQ-STSH-B-002's uuid clause, and that "Rename one in Settings" over two identical rows invites
renaming the wrong account (the PRD § 2.4 harm). The UX lens's first reading found that a uuid alone does
not carry "this one" from the card to Settings, because opening Settings closes the panel and resets the
card; that the panel's next-swap answer leaned on a per-row marker R-2 removed; and that both of the panel's
swap actions are refused after the click on a shared label. The ordinal answers the first two: it is short
enough to carry, uuid-free, and computed the same way on every surface. The uuids stay in the notices
because the PRD's Musts name them there; the Settings row's own uuid waits for the operator, because
PRD § 3 defers that affordance. The alternatives are recorded in § 12b and in § 14.6 items 10a, 10b and 15.

### 9.2 Screen Inventory

| # | Screen name | Type | Primary entity | Entry points | Key actions |
|---|---|---|---|---|---|
| 1 | Panel account list | List | PanelAccountRow | the menu-bar icon | read rows; switch to a row; the footer's next-swap callout and its Swap |
| 2 | Panel capture card | Form | OperatorNotice | "Capture your first account" on an empty roster; "Add account…" from the populated panel's menu (#394) | capture; read the outcome |
| 3 | Settings accounts section | Settings | RosterEntry | "Settings…" in the status item's menu, which closes the panel | edit labels; save |

### 9.3 Task flows

**Refuse form (2026-11-03, § 12.3).** TF-1 and TF-2 below describe the warn form's notices. Under the rulings TF-1's
capture is refused with `label-in-use` or `relabels-another-account` before any write, and the card shows the
refusal naming no label it cannot know for certain (C-6, C-9); TF-2's colliding save is refused (C-7).

**TF-1 — add an account from the panel** (socket `capture`). Actor: the operator. Precondition: the daemon
answers and Claude Code is signed in. (1) The operator requests a capture; the card shows its existing
Pending state. (2) The daemon acknowledges `captured` or `refreshed`; the card's outcome line says
"Captured ‘work’", or "Refreshed ‘work’ (was ‘spare’)" when a refresh changed the label (after #1474), and
the notice lines follow: collision, then shared credential. Each names an entry by its ordinal, and by its
uuid beside a label with more than one bearer (§ 9.6). (3) A card that carries a notice stays until a new
capture, Esc, a dismissal or the panel's close (UX-2). (4) To act on "Rename one in Settings", the operator
opens Settings from the status item's menu, which closes the panel; the Settings rows show the same
ordinals and the note (UX-11), so "this one is work 2 of 2" finds its row; opening it reloads the roster,
while a window left open from before keeps its old roster until reopened. Decision points: a counted
collision adds the collision line; a byte-identical credential under another entry adds the shared line; a
refuse policy turns step 2 into the Refused state with nothing written. Error paths: the daemon is unreachable (existing daemon-down state);
`no-active-account`, `keychain-locked`, `swap-lock-busy`, `prior-configuration` and `failed` keep their
existing presentations; an unmodelled acknowledgement value cannot occur, because the parity gate keeps
every Rust value modelled (AC-ROS-003). Edge cases: a refresh under the same label shows no relabel and
no collision line; an older app ignores the new fields.

**TF-2 — rename an account in Settings** (socket `config-set`). (0) While any label has more than one
bearer, its rows carry their ordinals and the section shows the collision note (UX-11). (1) The operator
edits a label and saves. (2) The daemon answers `applied` with its effect; the section shows the existing
confirmation. A save that creates a collision adds the ordinals and the note, which stays after "Saved"
clears, and announces it once (UX-8; superseded 2026-11-03 by the refusal, § 9.5 Refuse form); a save that resolves one removes them; a save that swaps two labels
creates no collision and announces nothing (AC-ROS-002). Error paths: `invalid`, `unknown-account`,
`no-config`, `config-unreadable`, `save-failed` and `unavailable` keep their presentations; `label-in-use`
exists only under a refuse policy (UX-7).

**TF-3 — read and switch in a list in which two accounts share a label** (`watch`, `swap`). Each frame
renders one row per account line. The rows sharing a label carry ordinals, colour slots and spoken cues;
the footer callout names a shared-label target by its ordinal (UX-5); the header's active-account line
carries the ordinal too, while a swap result keeps the acknowledgement's labels. Switching to either row,
from the row or from the callout's Swap, is shown blocked before the click with its reason and remedy
(UX-10), so a click changes nothing; a frame that lags a new duplicate still gets the daemon's refusal after
the click, worded to match. Switching to any other row, or away from a shared-label row, works as today,
and the daemon's own rotation still reaches either account, because it swaps by entry, not by label. With an
older daemon the rows still render (identity by position) and still carry ordinals, which come from
position, and the callout says "one of 2 accounts named work".

### 9.4 Surface Selection Summary

| Operation | Surface | Rationale |
|---|---|---|
| Collision, relabel or shared-credential notice after a panel capture | an inline line in the capture card's status area | the write completed and nothing needs deciding; the operator is looking at the card; a toast would vanish |
| Collision notice in Settings | a persistent inline note in the accounts section, derived from the roster's state, plus the ordinal on each row whose label is ambiguous | the transient "Saved" clears itself; the rename that resolves it is on the same surface, and the ordinal tells the two rows apart, here and on the panel (UX-3, UX-11) |
| Switching to a shared-label account | the existing blocked row treatment; on the callout, a resting reason line and a disabled Swap | the daemon refuses an ambiguous target; showing the block before the click follows the panel's rule for refusals the wire proves (UX-10) |
| A refusal, only if a refuse policy is chosen | the existing Refused / rejected-save states | the same decision shape as the existing rejections |

### 9.5 Screen specifications

#### Screen: Panel account list

Purpose: show every rostered account with its figures, distinguishably even under one label, and switch
between them. Zones: the existing header; the account rows; the existing footer with the next-swap
callout. Hierarchy: Panel > AccountList > AccountRow (identity: account identifier, or position plus
label) > colour dot, monogram, label, ordinal badge (only on a row whose label has more than one bearer),
figures, switch; Footer > next-swap callout (label, plus the target's ordinal when its label is shared) >
Swap. Next-swap has no per-row marker (R-2). Responsive: none; the panel has a fixed width.

| Data field | Source | Display format | Priority |
|---|---|---|---|
| label | `AccountStatusLine.label` | text | Primary |
| ordinal | the row's position among the lines sharing its label, in roster order | a badge set apart from the label, with no tooltip of its own (the blocked row's tooltip carries the remedy: `badge-tooltip-never-surfaces`, UX-3); only on a shared label | Primary |
| figures | the existing line fields | existing | Primary |
| account identifier | the new line field | never displayed; row identity and the callout's row lookup only | Metadata |
| next-swap target | `NextSwap` target and its identifier | the footer callout: label, plus the ordinal when the label is shared | Secondary |
| switch availability | the bearer count of the row's label | when the label is shared, the existing blocked row treatment (a resting reason and a tooltip with the remedy), and on the callout a resting reason line and a disabled Swap (UX-10) | Secondary |

#### Interaction State Matrix: Panel account list

| State | Applicable | Visual treatment | User action | Recovery |
|---|---|---|---|---|
| Loading | yes | the existing pre-first-frame presentation | wait | — |
| Empty — first use | yes | the existing empty state | capture the first account | — |
| Empty — no results | no: the list is not filtered | — | — | — |
| Populated | yes | one row per line; a duplicate set gets ordinals, colour slots and spoken cues; switching to a duplicate is shown blocked with its reason (UX-10) | read; switch to a row whose label is unique | rename one in Settings |
| Error — network | yes | the existing liveness states | wait or restart the daemon | existing |
| Error — auth | no: the socket's peer check refuses at connect | — | — | — |
| Error — permission | no: the same peer check; a switch blocked by a shared label is a Populated sub-state, not a permission error | — | — | — |
| Partial | yes | older daemon: rows by position; the callout says "one of 2 accounts named work" (UX-5) | read | update the daemon |
| Stale | yes | the existing freshness presentation | — | — |
| Offline | yes | the existing daemon-down presentation | — | existing |

#### Screen: Panel capture card

Purpose: capture the signed-in account and say what the write did. Zones: the existing title; a status
area holding the outcome line, which carries a relabel as "(was ‘spare’)", and at most one notice line of
each kind, in a fixed order (collision, shared); the existing actions. The notice lines wrap rather than
truncate, so no uuid loses characters, and a collision line takes an attention treatment rather than the
green success check. The card does not show the roster count: Swift reads a field the daemon does not
send (§ 14.7), so the count stays hidden, as today, although PRD § 11.2's Populated row lists it.
Responsive: none.

| Data field | Source | Display format | Priority |
|---|---|---|---|
| label | ack `label` | text, in the outcome line | Primary |
| previous label | `refreshed.previous_label` | "(was ‘spare’)" in the outcome line | Primary |
| collisions | ack `collisions[]` (label, the bearers' uuids in roster order) | one line naming each bearer by ordinal and uuid, the written entry's marked "this one", with a warn's cost | Primary |
| shared-with entries | ack `shared_with[]` (label, uuid) | one line; each side's ordinal and uuid only where its label has more than one bearer | Primary |
| written entry's uuid | ack `account_uuid` | locates "this one" among the bearers; shown only inside a line that names that entry | Tertiary |

**Refuse form (2026-11-03, § 12.3).** The previous-label and collisions rows describe the warn form. Under the
rulings the panel's capture never completes a relabel or a collision: the card shows the two refusals instead,
naming the label `L` the panel sent and never the landed account's current label, which the watch snapshot
cannot give for certain (C-6, C-9). The label, shared-with and uuid rows stand.

#### Interaction State Matrix: Panel capture card

| State | Applicable | Visual treatment | User action | Recovery |
|---|---|---|---|---|
| Loading | yes | the existing Pending state | wait | — |
| Empty — first use | no: the card exists only once requested | — | — | — |
| Empty — no results | no: an acknowledgement always carries a result | — | — | — |
| Populated | yes | the outcome plus each notice that applies; a card with a notice stays until a new capture, Esc, a dismissal or the panel's close (UX-2) | read; open Settings from the status item's menu to rename | — |
| Error — network | yes | the existing daemon-down state | retry | existing |
| Error — auth | yes | `no-active-account` | sign in to Claude Code | existing |
| Error — permission | yes | `keychain-locked`, `prior-configuration`; `label-in-use` or `relabels-another-account` only under a refuse policy | follow the reason | existing |
| Partial | yes | `failed`: the stash landed and the roster save did not (an orphan, F10) | retry | existing |
| Stale | no: an acknowledgement answers one request | — | — | — |
| Offline | yes | the existing daemon-down state | — | existing |

#### Screen: Settings accounts section

Purpose: rename accounts; parking, adding and removing stay CLI-only (#268). Entry: "Settings…" in the
status item's menu, which closes the panel. Zones: the existing accounts table, with an ordinal badge
beside the label field of each row whose label has more than one bearer; the existing save status; a
persistent collision note. Responsive: none. The Accounts row anatomy is the mock's open axis (register
R-11, owner open, § 14.7), so the badge and the note are drawn there before Swift builds them.

| Data field | Source | Display format | Priority |
|---|---|---|---|
| ordinal | the row's position among the rows sharing its label, in roster order (`config-get`) | a badge beside the label field, set apart from it; in the field's accessible label | Primary |
| collision note | the roster the section holds (`config-get`): any label with more than one bearer | text naming each bearer by ordinal and uuid, with the cost of a warn | Primary |
| new collision | `applied.collisions[]` | announced once as a status message (UX-8) | Secondary |
| effect | `applied.effect` | existing | Secondary |
| account uuid | `accountUuid`, already each row's identity (`apps/menubar/Sources/SettingsView.swift:199`) | under the label field, only if `settings-row-shows-uuid` is answered yes (UX-9) | Tertiary |

**Refuse form (2026-11-03, § 12.3 C-7, C-9).** The announced collision of UX-8, TF-2's "announces it once", the
`applied.collisions[]` row of this screen's data fields and UX-11's "a warn's cost" describe the warn form. Under
the rulings a save that would create a collision is refused before any write: the form keeps the edit unsaved,
#1558's fixed sentence sits in the rejected-save slot (UX-7), #1560 marks the listed rows the refusal's
`collisions` names and names the label on each in words, no announcement replaces the warn form's, and the
marking lasts until the next save or load. The persistent note for duplicates that already exist stands.

#### Interaction State Matrix: Settings accounts section

| State | Applicable | Visual treatment | User action | Recovery |
|---|---|---|---|---|
| Loading | yes | the existing in-flight save | wait | — |
| Empty — first use | yes | `no-config` | capture the first account | — |
| Empty — no results | no: nothing is filtered | — | — | — |
| Populated | yes | the existing confirmation; while a label has more than one bearer, its rows' ordinals and the persistent collision note (UX-11) | rename one | — |
| Error — network | yes | the existing daemon-down handling | retry | existing |
| Error — auth | no: the peer check refuses at connect | — | — | — |
| Error — permission | yes, only under a refuse policy | `label-in-use`, through the existing rejected-save state (UX-7) | choose another label | existing |
| Partial | no: `config-set` saves all or nothing (`save-failed`) | — | — | — |
| Stale | yes | `unknown-account` | reload | existing |
| Offline | yes | `config-unreadable` or `unavailable` | — | existing |

### 9.6 Content and copy

Every string avoids the #160 banned tokens and is scanned (REQ-NTC-C-002).

| Element | Text (draft) | Ownership |
|---|---|---|
| Outcome line, capture card | Captured ‘work’. · Refreshed ‘work’ (was ‘spare’). (the “was” form is the CLI's, on a revive made with `--rename`, since 2026-11-03; the card never shows it, § 12.3 C-6) | Developer |
| Collision line, capture card | ‘work’ now names 2 accounts: this one is work 2 of 2 (<uuid-B>), the other work 1 of 2 (<uuid-A>). Until one is renamed, you can't switch to either from the panel. Rename one in Settings. | Developer |
| Shared line, capture card | ‘work’ and ‘spare’ hold the same credential. | Copywriter review |
| Shared line, one side shared | ‘spare’ and work 2 of 2 (<uuid-B>) hold the same credential. | Copywriter review |
| Shared line, equal labels | This one, work 2 of 2 (<uuid-B>), and work 1 of 2 (<uuid-A>) hold the same credential. | Copywriter review |
| Collision note, Settings | ‘work’ names 2 accounts: work 1 of 2 (<uuid-A>) and work 2 of 2 (<uuid-B>). Until one is renamed, you can't switch to either from the panel. | Developer |
| Ordinal badge | 1 of 2, 2 of 2, … by position among the rows sharing the label | Developer |
| Blocked switch, resting reason (row and callout) | Can’t switch — another account has this label. | Developer |
| Blocked row tooltip | Can’t switch — another account has this label. Rename one in Settings. | Developer |
| After-click refusal, the fallback | Can’t switch — another account has this label. Rename one in Settings. (replaces “Two accounts share that label — rename one, then switch.”) | Developer |
| Callout, shared target | → work 2 of 2 · spoken “Next swap to work, 2 of 2 accounts named work” · older daemon: “one of 2 accounts named work” | Developer |
| Spoken cue, row and Settings field | “work, 2 of 2 accounts named work” · Settings: “Label, 2 of 2”, then the field's value | Developer |
| Status-message announcement | “‘work’ now names 2 accounts; this one is 2 of 2.” | Developer |
| Header | “3 accounts · work 2 of 2 active” | Developer |
| Swap result | the acknowledgement's labels, unchanged: “Switched work → spare” | Developer |
| Row uuid line, Settings | the row's uuid under the label field, only if `settings-row-shows-uuid` is answered yes | Developer |

Implementation notes: the row identity in `AccountRow`,
`var id: String { label }` (`apps/menubar/Sources/HonestStateMachine.swift:363`), moves from the label to the identifier.
`isNextSwapTarget` (`HonestStateMachine.swift:445`) is computed but no view renders it, since R-2 dropped the
per-row marker; the callout finds its target row by the target's identifier instead, for the ordinal. The
switching state, `isSwitching` (`apps/menubar/Sources/StatusPanelRoster.swift:256`), compares `pendingTarget` with the label and
moves to row identity. The blocked switch is a `SwitchBlock` case checked first (`apps/menubar/Sources/StatusPanelFormat.swift:178`),
and the enum's doc, which limits it to the daemon's viability gates, is amended to say so. The callout's
Swap gains a disabled condition beside the pending one (`swap.phase.isPending`, `apps/menubar/Sources/StatusPanelChrome.swift:243`). The
collision line's ordinals come from the acknowledgement (`collisions[].bearers`, in roster order). The shared
line's ordinals come from the latest `watch` frame, matching the acknowledgement's uuids to the frame's
identifiers, which works when `account-key-form` picks the uuid; under a derived value the shared line names
the uuid without an ordinal, and the Settings note maps it. The header's ordinal comes from the frame
alone: the line marked active and its place among the lines sharing its label. For a byte-identical pair the
daemon reports the first matching entry in roster order as active (`resolve_account_for`, `src/active.rs:43-53`),
so the header can name 1 of 2 while the card's write was 2 of 2; the card's shared line names both.
The colour seed, `accountColorIndex(for:)` (`apps/menubar/Sources/StatusPanelFormat.swift:2755`), stays; the
duplicate-set tie-break is applied by the caller that holds every row and picks a slot no other row uses.
The monogram function, `accountMonograms` (`StatusPanelFormat.swift:2784`), is unchanged. The card's `done` line is
today one green, truncated line (`captureDoneText`, `apps/menubar/Sources/StatusPanelCapture.swift:104-108`); the notice lines are
separate, wrapping lines. Strings are named constants read by a Swift framing test. The Settings ordinal
and note read `config-get`, which already lists the roster in order and keys each row by
`accountUuid` (`apps/menubar/Sources/SettingsView.swift:199`), so no wire change feeds them.

### 9.7 Requirement Provenance

| Design element | Element type | Traces to | Provenance source | Notes |
|---|---|---|---|---|
| Panel account list | Screen | REQ-ROW-B-001 | PRD § 11.4 | existing screen, new states |
| Panel capture card | Screen | REQ-ROS-I-002 | PRD § 11.2 | also REQ-IDN-I-002, REQ-STSH-B-002 |
| Settings accounts section | Screen | REQ-ROS-I-002 | PRD § 11.3 | existing screen |
| Capture (socket) | CTA | REQ-ROS-B-001 | PRD § 9.3, RosterEntry Create | existing CTA, unchanged |
| Save label (`config-set`) | CTA | REQ-ROS-B-001 | PRD § 9.3, RosterEntry Edit | existing CTA, unchanged |
| PanelAccountRow | Entity | REQ-ROW-B-001 | PRD § 9.1 | — |
| AccountStatusLine | Entity | REQ-WIRE-I-001 | PRD § 9.1 | — |
| OperatorNotice | Entity | REQ-ROS-I-002 | PRD § 9.1 | also REQ-IDN-I-002, REQ-STSH-B-002 |
| Collision line and note | Entity | REQ-ROS-I-002 | PRD § 9.3, OperatorNotice View (collision) | ordinals, the bearers' uuids and a warn's cost, at parity with § 8.5; the Settings note follows state (UX-11) |
| Settings row uuid line, only if answered yes | Entity | none | PRD § 9.4, `account_uuid` "where a label is ambiguous" | net-new, conditional: UX-9; not built until `settings-row-shows-uuid` is answered (§ 14.6 item 10b) |
| Relabel in the outcome line | Entity | REQ-IDN-I-002 | PRD § 9.3, OperatorNotice View (relabel) | waits on #1474; folded into the outcome line (UX-6) |
| Shared-credential line | Entity | REQ-STSH-B-002 | PRD § 9.3, OperatorNotice Create (shared credential) | DQ-6: both channels |
| Ordinal badge and spoken cue, on every surface | Entity | REQ-ROW-C-002, REQ-ROS-I-002 | DQ-4 (#939) | the non-colour cue and the duplicate set's one handle (UX-3) |
| Duplicate-set colour tie-break | Entity | REQ-ROW-C-002 | DQ-4; PM-10 | — |
| Next-swap callout ordinal, by the target's identifier | Entity | REQ-ROW-B-001 | DQ-4 (#930); R-2 | the target's identifier goes beyond REQ-WIRE-I-001's wording; D-6 |
| Blocked switch on a shared label | Entity | none | PRD § 2 ("swap to it"); PRD § 8.1, a warn's cost | PHANTOM, kept and flagged: the panel's `SwitchBlock` rule applied to a refusal the wire proves; returns to the PRD (§ 14.6 item 15) |
| Legacy row fallback | Entity | REQ-ROW-C-001 | PRD § 11.4, Partial | — |

## 11. Crosscutting Concepts

### 11.1 Security — a delta threat model of the changed elements

The adversaries in scope are accidental disclosure (the repository is public, and operators paste
`status --json`, `log` and replies into issues), one account's credential acting under another account's
identity, and loss of credential or witness material. A malicious process running as the same user is out
of scope: it can already read `config.toml` and drive `security` itself.

| Data | Class | Where it may appear after this design |
|---|---|---|
| Credential blob | Restricted | the keychain and process memory only (`Credential`, `Zeroizing`, `src/keychain.rs:327`) |
| `oauthAccount` (email, uuids) | Confidential | the keychain, `~/.claude.json`, the artifact; parsed in memory by F6 |
| Harvested email | Confidential (PII) | nowhere: never a template input |
| Account uuid | an identifier, not a secret | where it is today, plus: the status/watch wire (form per `account-key-form`); the socket acknowledgements, as facts; for an ambiguous label, event tokens, CLI notices, the capture card's lines and the Settings note; usage samples; an ambiguous Settings row only if `settings-row-shows-uuid` is answered yes |
| Label | operator-authored | where it is today |

| ID | Threat | Sev. | Lik. | Prio. | Mitigation |
|---|---|---|---|---|---|
| T-03 | A reaper deletes an in-flight capture's stash, or the witness on a machine without `config.toml` | High | Medium if reap is built without guards | P2 | REQ-STSH-B-003's conditions, a grace window from item metadata (SP-2), and a recommendation for `report` (SEC-3) |
| T-02 | An artifact entry filed under one uuid carries another account's identity | High | Low | P3 | F6 refuses the entry before its write; F5 reports duplicates before the lock |
| T-01 | A digest or fingerprint of a credential reaches an ack, a notice or a log line | High | Low | P3 | compare in memory only; meter tests over every new line and field (CAP-09) |
| T-05 | The harvested email reaches a notice, for example under `login-mismatch-policy`'s email-compare option | Medium | Low | P3 | labels and uuids are the only template inputs; the meter's unauthored-email check |
| T-04 | A uuid is disclosed through a pasted `status --json` or `log` | Low | Medium | P3 | `account-key-form`'s derived option; event tokens only when ambiguous; A-6 and REQ-WIRE-C-001 |
| T-06 | F3's keychain reads delay or prompt during a socket capture | Medium | Low | P3 | reads after the lock releases, through the read path the swap engine already uses unattended |

Decisions: SEC-1, nothing derived from credential bytes is persisted or sent; SEC-2, F6 compares uuids byte
for byte; SEC-3 (author-chosen), recommend `report` over `reap` for F10; SEC-4, the uuid is an identifier
already in `config.toml`, in `list`, in the usage-family events and in the `config-get` view on the same
socket, so the acknowledgements' uuids (§ 8.1) add no exposure class and a derived wire value protects only
pasted status output — evidence for the `account-key-form`
stub, not a decision; SEC-5, Linux and Windows stash backends are assumed absent. Accepted residuals: F3
misses a pair that a refresh split first (A-2), a failed read skips one comparison, and the uuid stays in
local stores where it already is. BDD abuse scenarios for T-01, T-02, T-03 and T-05 bind to CAP-09,
CAP-17, CAP-21 and CAP-22 (§ 11.5).

### 11.2 Observability

`credential_shared` makes MET-R-003 countable and makes ADR-0028's trigger visible. Swap lines tell
same-label accounts apart. Collision and relabel notices are not logged, because PRD § 3 defers that
record. Every leading indicator (MET-T-001 to MET-T-004) is measured by hermetic tests, not by telemetry.

### 11.3 Error handling

Errors follow one rule per boundary they cross. Socket rejections are closed vocabularies with Swift cases landed in step.
`import` reports per account, through the existing `failed` outcome. F3 and the backfill swallow and log
their own errors and never surface a false state: a skipped comparison prints nothing, and a failed
backfill leaves samples keyless and retries at the next start.

### 11.4 Accessibility

Quality-pillar finding, closed in § 9.1 as UX-8. Rows sharing a label carry a non-colour cue (WCAG 1.4.1)
and an accessible name with their ordinal (4.1.2); a duplicate Settings label field carries its ordinal in
its accessible label, so two fields named "Label" stay distinct (2.4.6). Notice lines are announced as
status messages without taking focus (4.1.3); the announcement carries the ordinal sentence, and the uuids
stay in the visible text, read as characters only on demand. The spoken distinction is the ordinal, never a
full uuid; if the Settings row's uuid line is built (UX-9), it belongs to its row's accessibility container
(1.3.1) and is spoken as a short prefix. A blocked switch states its reason to VoiceOver before any click
(UX-10). Dynamic Type and Reduce Transparency are outside the mock's authority and are checked by hand. The
Settings window is authored in the mock, as Group 8 (#763); only its Accounts row anatomy is open (register
R-11, owner open, § 14.7), and the ordinal badge, the note and any uuid line are drawn there first
(`apps/menubar/design/README.md`, § The Settings window).

### 11.5 Master Test Plan

**MTP-1 Goals and risk surface.** Goals: no write that collides, relabels or shares a credential is silent
on its own surface; no notice fires on a write that created nothing; every wire, ack and event addition
decodes in Swift or is ignored by an older reader, and output stays byte-identical for rosters without a
duplicate label; no credential byte, digest or harvested email reaches any output; history and the witness
survive the re-key. Out of scope: real-keychain and real-login automation (F7 is the operator's walk) and
the Linux and Windows stores. The capability matrix:

| CAP | Capability | Binds |
| CAP-04 | the capture card and Settings show collision, relabel and shared notices at parity with § 8.5, naming each entry by ordinal and by uuid where its label has more than one bearer, with a warn's cost; a card with a notice survives the 1.8 s reset; the Settings note and ordinals follow the roster's state, including after a capture made elsewhere (since 2026-11-03: the card shows the two refusals instead of collision and relabel notices, naming no label it cannot know; the shared notice and the Settings note stand, § 12.3 C-6, C-8, C-9) | REQ-ROS-I-002, REQ-IDN-I-002, REQ-STSH-B-002, REQ-NTC-C-001 |
| CAP-01 | the rule counts a collision exactly: swap, refresh of a two-bearer label, same-label relabel, fresh label | REQ-ROS-B-002, REQ-ROS-Q-001 |
| CAP-02 | every writer reports a counted collision on its own surface | REQ-ROS-B-001, REQ-ROS-C-002 |
| CAP-03 | acks carry notices as optional fields; the parity gate fails on an unmodelled value or field (since 2026-11-03 the capture refusals carry no field and REQ-IDN-I-001 is vacuous, § 12.3 C-6 and C-12) | REQ-ROS-I-001, REQ-ROS-C-001, REQ-IDN-I-001 |
| CAP-04 | the capture card and Settings show collision, relabel and shared notices at parity with § 8.5, naming each entry by ordinal and by uuid where its label has more than one bearer, with a warn's cost; a card with a notice survives the 1.8 s reset; the Settings note and ordinals follow the roster's state, including after a capture made elsewhere | REQ-ROS-I-002, REQ-IDN-I-002, REQ-STSH-B-002, REQ-NTC-C-001 |
| CAP-05 | a relabelling revive names both labels; onboarding and same-label refreshes do not | REQ-IDN-B-001, REQ-IDN-C-001 |
| CAP-06 | a chosen refusal precedes every stash write | REQ-IDN-C-002 |
| CAP-07 | F3 detects at the creating write on each of the four paths that can roster a new entry's stash and reports both entries by label, each with its uuid where its label has more than one bearer — in the CLI reply and on the card, with both uuids in the acknowledgement's `shared_with` — with one event line, and changes nothing; fixtures cover equal labels and one shared side (`work` A, `work` B, then `spare` holding B's credential) | REQ-STSH-B-001, REQ-STSH-B-002, REQ-STSH-Q-001, REQ-STSH-C-002 |
| CAP-08 | F3 reads at most one stash per other entry per write, none per poll or swap | REQ-STSH-Q-002 |
| CAP-09 | no credential byte or digest in any output; canary verdicts unchanged | REQ-STSH-C-001, REQ-STSH-C-003 |
| CAP-10 | the wire identifier on status and both watch frames, one minor bump, goldens, fixtures, reconciled docs | REQ-WIRE-I-001, REQ-WIRE-C-001, REQ-WIRE-C-002, REQ-WIRE-C-003 |
| CAP-11 | one panel row identity per entry; an older daemon still yields one row per line; the mock frame; the ordinal on every surface; the callout's ordinal found by the target's identifier; a shared-label switch shown blocked before the click; a switching state keyed on row identity | REQ-ROW-B-001, REQ-ROW-C-001, REQ-ROW-C-002 |
| CAP-12 | samples keyed on the uuid; backfill attributes only one-bearer labels; witness unchanged; sentinel | REQ-USG-B-001, REQ-USG-C-001, REQ-USG-C-002 |
| CAP-13 | the `stats --json` shape is unchanged, so its version does not move | REQ-USG-C-003 |
| CAP-14 | swap lines tell same-label accounts apart and stay byte-identical otherwise | REQ-EVT-B-001 |
| CAP-15 | the count agrees with its wording | REQ-NTC-B-001 |
| CAP-16 | payload duplicates and unmatched entries reported before the lock; format byte-identical | REQ-MIG-B-001, REQ-MIG-B-002, REQ-MIG-C-001, REQ-MIG-C-002 |
| CAP-17 | a mismatched entry is refused; the rest import | REQ-MIG-B-003, REQ-MIG-C-003 |
| CAP-18 | the premises are recorded (document review; not automatable) | REQ-MSR-B-001, REQ-MSR-C-001 |
| CAP-19 | the normalization record; confusable detection if adopted; the licence check | REQ-LBL-C-001, REQ-LBL-B-001, REQ-LBL-C-002 |
| CAP-20 | amendments dated, symbol-anchored and citation-gated; the `login --help` text | REQ-DOC-B-001, REQ-DOC-B-002, REQ-DOC-B-003, REQ-DOC-B-004, REQ-DOC-C-001 |
| CAP-21 | the orphan record; the reaper's guards or the metadata-only report | REQ-STSH-C-004, REQ-STSH-B-003, REQ-STSH-B-004 |
| CAP-22 | every new string names accounts by label or uuid only and passes the #15 meter and the #160 scan | REQ-NTC-C-001, REQ-NTC-C-002 |
| CAP-23 | a pre-change failing run is recorded for every behaviour change | REQ-NTC-C-003 |

**MTP-2 Pyramid.** The system is a local daemon, a CLI and one native client, and its risk sits in
deterministic logic and at one language boundary. Rust hermetic tests dominate: the rule, the plan seam,
F3 over an in-memory stash, the import checks, the backfill over a seeded store, the event lines. Contract
tests come second: the byte-pinned goldens, the new ack goldens decoded by Swift with value assertions, the
tag parity. Swift unit tests are thin: row identity, the fallback, the tie-break, the notice lines. Some
checks are manual by design: F7's walk, the armed panel-goldens run with the mock comparison, and the Linux
compile.

**MTP-3 gTAA layers.** Generation: example-based from each criterion's Given/When/Then and BUT NOT, failing
first. Definition: Rust in-module tests and Swift XCTest under `MenubarTests`. Execution: the CI jobs
`test`, `msrv`, `deny` and `swift`, with `panel-goldens` armed locally. Adaptation: the in-memory
`AccountStash` fake plus a read-counting wrapper, injected store paths, and the golden emitters. These are
the repository's existing conventions, so no test ADR is added.

**MTP-4 Environments.** The developer's Mac runs every suite, the armed panel-goldens run and the doc
gates. The CI macOS runner computes `ci-ok` from `.github/workflows/ci.yml`; no infrastructure is
provisioned, so infrastructure-as-code does not apply. A Linux container compiles and tests shared paths
by hand until #964. The operator's own machine hosts F7's walk and nothing else.

**MTP-5 Test data.** All synthetic and public: synthetic rosters and credential bytes in the fake stash,
seeded sample files with one- and two-bearer labels, keyed and keyless lines, hand-built payloads, and
goldens regenerated through their named emitters. F7's real logins leave only results, versions and
dates.

**MTP-6 Traceability.** Each test's doc comment names its requirement ids; REQ-ROS-C-002's oracle names
are used verbatim. Extraction: `grep -rnE 'REQ-[A-Z]+-[A-Z]-[0-9]{3}' src/ apps/menubar/Tests/` against
PRD § 10. An id absent from the PRD is an orphan test; a PRD id with no hit is an orphan requirement. The
audit runs at each feature's pre-merge review.

**MTP-7 Quality gates.**

| Gate | Stage | Exit criteria |
|---|---|---|
| G-RED | before the fix | each new oracle fails on the pre-change tree, and the failing output is recorded on the issue |
| G-LOCAL | before push | the CLAUDE.md pre-push gates; `msrv`; both `deny` checks for any `src/**` change |
| G-SWIFT | before push, for `apps/menubar/**` or `build/fixtures/**` | the `swift` job and an armed panel-goldens run printing its drift line, never "2 tests skipped" |
| G-CI | merge | `ci-ok` green; no bypass exists |
| G-DOC | documents touched | `scripts/check-doc-citations.sh` and `scripts/check-doc-citation-rot.sh` |
| G-LINUX | a shared-path change (F3, F10) | a container compile and test, stated in the pull request |
| G-REVIEW | merge | an independent fresh-context review of the change |

**MTP-8 Frameworks.** cargo's test harness; golden byte-pins plus Swift decode tests for contracts; XCTest
plus armed panel-goldens for the panel; hand-written fakes, no mocking crate. Mutation testing stays manual:
break the rule and the F3 compare once each and watch the suite fail. No property-based crate is added.

**MTP-9 AI-augmented testing.** N/A — no AI/LLM in system.

**MTP-10 Risks.**

| Risk | L | I | Mitigation | Owner |
|---|---|---|---|---|
| A gate goes green over no subjects: an empty filter, or skipped golden comparisons | M | H | assert the `running N tests` count and the drift line, never an exit code alone | implementer |
| Golden churn collides with #1457's bump | M | M | regenerate after rebase; the number is read at merge | whichever change merges second |
| The F3 suite passes while the detector is inert | M | H | the mutation pass; the read-count meter | implementer |
| `--include-ignored` rewrites the frozen migration fixtures | L | H | run named ignored emitters only | implementer |
| A Swift decode test passes over an ignored key | M | M | assert decoded values, never only that decoding did not throw | implementer |

## 12. Architecture Decisions

### 12.1 Decided here — pipeline-decided, ratification-pending

| Decision | Choice | Rejected | Reversibility |
|---|---|---|---|
| D-1 collision rule | one rule beside `Account`, called by every writer, with a comparison-key hook | a check per call site (PM-19) | two-way: an internal function |
| D-2 plan seam | `plan_capture` returns the previous label and collisions; refusals gate before the stash write | refuse after the write and roll back (PM-14) | two-way |
| D-3 notices | named templates under the advisory audience; acks carry facts; enabled count (author-chosen, DQ-10) | prose on the wire; a wording that names the roster | two-way; the count wording is visible text |
| D-4 shared credentials | compare at the creating write, after the lock, at most one read per other entry; no start-up sweep (DQ-5) | inside the lock; a sweep; per poll | two-way |
| D-5 identity per surface | local stores and the event log key on the uuid; only the wire waits on `account-key-form` | one key form for every surface | the log is append-only: one-way for lines written |
| D-6 wire identifier | additive Options on account lines and the next-swap target; one minor bump | a major bump; a second bump; a label-only callout | costly: a second bump plus Swift lockstep |
| D-7 store | expand, then one sentinel-guarded backfill under `usage.lock` (author-chosen sentinel placement) | rollup-document marker; a persisted label-to-uuid map applied by readers | one-way for attributions written; bounded by the 14-day raw window |
| D-8 readers | `stats`, `reliability`, `log` unchanged (author-chosen, DQ-3) | relabel continuity; same-label separation | two-way |
| D-9 event lines | uuid tokens only for an ambiguous side; `credential_shared` with `via` | tokens on every line | one-way for lines written |
| D-10 acks | optional fields, value-asserting parity gate over the new fields | new `result` values; a whole-body gate now | two-way |
| D-11 import | payload check before the lock; per-entry identity check before the write | checks after the lock | two-way |
| D-12 orphans | a metadata census; the reaper only with a grace window | a reaper keyed on locks alone | reaping is irreversible |
| D-13 records | dated amendments; decision records numbered when they land | rewrites; pre-assigned numbers | two-way |
| D-14 platform | no stash backend assumed off macOS | — | two-way |
| UX-3 duplicate set | the ordinal as the one handle on every surface; colour slots no other row uses; spoken cue (author-chosen) | re-seed every row from the identifier; the uuid as the handle | two-way; visible |
| UX-9 uuid where ambiguous | notices name a uuid only beside an ambiguous label; a Settings row's own uuid only on an explicit answer (author-chosen) | uuid-free notices; the row's uuid by default | two-way; visible; returns to the PRD (§ 14.6 items 10a, 10b) |
| UX-10 shared-label switch | blocked before the click, with its reason and remedy (author-chosen) | switch by identifier; the refusal after the click, as today | two-way; returns to the PRD (§ 14.6 item 15) |
| UX-11 Settings note | derived from the roster's state; announced only for a save that created a collision (author-chosen) | tied to the save's acknowledgement | two-way |
| SEC-3 orphan advice | recommend `report` over `reap` (author-chosen) | — | advice only |

### 12.2 Open decision stubs — context and options filled, decision left open

Each stub below is `status: proposed`. None carries a decision, and each is a load-bearing open question
(§ 14.6).

#### `account-key-form` — which identifier the status and watch wire carries (DQ-1)

`status: proposed`

**Context.** REQ-WIRE-I-001 asks for an identifier unique per roster entry and unchanged by a relabel, on
every account line of `status` and both `watch` frames. D-5 already confines this fork to the wire: the
store and the log key on the uuid whichever way it goes. The tree applies issue #15 in conflicting ways. The status
wire's `canary` doc says "never a token, email, or account-uuid" (`src/daemon/snapshot.rs:665-668`), and so
does the canary-drift event (`CanaryDrift` handles, `src/observability.rs:1614-1615`). The usage-family events
carry the uuid and "never the operator `label`" (`ExhaustedSlowPollCleared`, `src/observability.rs:1968-1969`). The app already receives
uuids over the same socket in the `config-get` view
(`AccountView.accountUuid`, `apps/menubar/Sources/ConfigWire.swift:71`), and `list` prints them. #791's acceptance line reads "account
labels only, no identifiers". `status --json` prints the raw wire (`status`, `src/cli.rs:1810`), so
whatever the wire carries can be pasted into a public issue. The meter's entropy backstop flags an
alphanumeric run of 20 or more; a hyphenated uuid's longest run is 12 (A-6). Reversing the choice later
costs a second minor bump, a Swift lockstep, and any operator script that reads the field. Two panel facts
bear on it. Under § 14.6 item 10a's default the app already displays the raw uuid in a notice beside an
ambiguous label, so a derived wire value would keep the uuid out of pasted `status --json` only. And a
switch by identifier (§ 14.6 item 15) sends whatever the wire carries: `resolve_target` matches a uuid as it
is, while a derived key would need `swap` to resolve it.

**Alternatives considered.**

| Option | For | Against |
|---|---|---|
| the `account_uuid` itself | one identity everywhere; matches `list`, `config-get`, the samples, the event tokens and the uuids the panel's notices show; nothing to derive or store; a switch by identifier needs no daemon change | the uuid becomes pastable through `status --json`; #791's line and the canary docs need reconciling (owed anyway, REQ-WIRE-C-002); an unhyphenated uuid, which validation accepts, trips the backstop |
| a value derived one-way from the uuid | keeps the raw uuid out of pasted status output | a derivation and its stability contract; no correlation with `list`, nor with the uuids the panel's notices show; a switch by identifier would need `swap` to resolve it; protects nothing the log and the config already hold; a long unseparated digest trips the same backstop, so the format needs separators or a short length |
| a daemon-local opaque id | nothing leaves the machine that means anything elsewhere | dominated: not stable across restarts or roster edits, so rows re-key on every restart |

**Decision.** Open — the operator's.

**Consequences.** Open. Signposts: the operator's ruling on #791's rule; a report of a uuid pasted publicly;
F7 premise 4, since case variance would give one account two identifiers under the first option.

#### `duplicate-label-policy` — warn or refuse when a write makes a label name a second account (PRD § 8.1)

`status: decided by the operator on 2026-11-03, refuse unless --overwrite; the requirements around it stay ratification-pending`

**Context.** Every roster writer can create a collision, and all but `import` do it silently (PRD § 2.3,
D-1). Ratified AC-6 says "BUT NOT by enforcing label uniqueness". A refused `login` spends the
operator's interactive login, and any refusal must precede the stash write (D-2).

**Alternatives considered.** Warn: a report on the completed write; it extends ratified R-6, and the
duplicate still exists, so resolution keeps refusing it with exit 6 until the operator renames one.
Refuse: needs AC-6 re-ratified first, adds the `label-in-use` tag and its Swift case, and refuses before
any write.

**Decision (the operator, 2026-11-03).** Refuse: a write that would make a label name a second account is refused
before any write on all four paths unless `--overwrite` is given, under which the new account replaces the
existing holder; the operator's words and the orchestrator's reading are in PRD § 8.1. **Consequences.** The
`label-in-use` tag and its Swift case are built; REQ-ROS-C-002's oracles assert the refusal; the collision
notices of F1 (D-1, UX-1 to UX-9) reach only rosters that already hold a duplicate, since no completed write on
the four paths creates one; AC-6 is re-ratified as narrowed (the migration PRD's amendment note); the items that
built the warn default (#1556, #1558, #1559, #1560, #1561) re-enter the ready gate. Settled in § 12.3 (C-5 to
C-7): the replaced account's fate under `--overwrite` and that the socket paths carry no override. Open: `import`.

#### `login-mismatch-policy` — what `login <label>` does when the captured account is not the one the label names

`status: decided by the operator on 2026-11-03, refuse unless --rename; the requirements around it stay ratification-pending`

**Context.** The code refreshes on the uuid and renames on the label, while the ratified stamp REQ-LGN-B-004
says a bare `login` "refreshes it" (PRD § 8.2). REQ-IDN-B-001's disclosure holds under every option.

**Alternatives considered.** Keep the rename, disclosed. Warn on every renaming revive, including deliberate
renames. Refuse a renaming revive: it blocks the rename path that
`recapture_updates_the_label_when_a_new_one_is_given` pins and contradicts REQ-LGN-B-004, and it adds
`relabels-another-account` for the socket path. Compare an email-shaped label with the captured
`emailAddress`: it needs #846's rule for when a label is an address and spike SP-1's premise 3, and only
the comparison's result may be shown.

**Decision (the operator, 2026-11-03).** Refuse a revive that renames an account unless `--rename` is given; the
words and the reading are in PRD § 8.1. **Consequences.** The `relabels-another-account` tag and its Swift case
are built for the socket path; the CLI refuses before the stash write; with the flag the rename proceeds and
REQ-IDN-B-001's disclosure holds; `recapture_updates_the_label_when_a_new_one_is_given` gains the flag;
REQ-LGN-B-004's "refreshes it" holds for the same-label case. #1570's help sentence is re-keyed to the refusal (the scope
brief's `login-mismatch-policy-recorded-on-1570` row), and REQ-DOC-B-003's keep-rename wording with it. Settled in § 12.3
(C-6): the socket `capture` carries no override.

#### `label-normalization-policy` — what makes two labels the same, if anything

`status: proposed`

**Context.** No record decides it (PRD § 2.10). D-1's comparison-key hook is where any rule plugs in.

**Alternatives considered.** None, as today. Case folding, which can create ambiguity in existing rosters.
Punctuation folding, wrong as a default because it merges the operator's real hyphen pairs. Unicode
normalization such as NFC, cheap and invisible without combining marks. Confusable detection that reports
and never merges, through UTS #39 skeletons, a dependency or a table (spike SP-3).

**Decision.** Open. **Consequences.** Open.

#### `import-duplicate-uuid-policy` — refuse or warn on a payload with two credentials for one uuid, or one for no rostered uuid

`status: proposed`

**Context.** Only a hand-built or corrupted artifact reaches this path; the exporter cannot (A-7).

**Alternatives considered.** Refuse the whole payload, the safest. Warn and import that account roster-only,
to be logged in again. Warn and keep today's last-entry-wins, audible but still choosing by position.

**Decision.** Open. **Consequences.** Open.

#### `orphan-stash-policy` — reap, report or accept an orphan `Sessiometer/<uuid>` item

`status: proposed`

**Context.** Nothing reaps or reports one today (A-10). The witness reads this namespace, so a reaper on a
machine without `config.toml` would erase it (ADR-0036, #1466). D-12 shows no lock spans a capture's stash
write and roster save.

**Alternatives considered.** Reap, with D-12's conditions and spike SP-2; it deletes keychain material
irreversibly. Report, a metadata-only listing; cleanup stays with the operator; SEC-3 recommends it.
Accept: no code, and the decision record replaces the comments calling an orphan "harmless".

**Decision.** Open. **Consequences.** Open.

### 12.3 The refuse form — the two rulings' consequences, settled for the re-authoring (2026-11-03)

Pipeline-authored by the `/scope` orchestrator on 2026-11-03 and ratification-pending, like every
requirement in the PRD. It was written as the shared contract for re-authoring #1556, #1558, #1559,
#1560, #1561 and #1570 to the operator's rulings of § 12.2, critiqued in a fresh context before any
item was re-authored on it, and is the single place their cross-item behaviour is fixed; each item
cites it as "design § 12.3, C-N". Where an earlier section of this design describes the warn form, this
section wins for those six items.

Each decision carries its authority:

- `operator` — the operator's own words, quoted verbatim below.
- `orchestrator` — a reading or a pick by the `/scope` orchestrator, confirmed or changed when the
  operator ratifies the PRD. An item built on it says so once, and is not NOT READY for it.

Base of every fact below: `origin/main` at `b9df076`.

#### The operator's words, verbatim (2026-11-03)

1. "refuse unless `--rename` or `--force` is added, be defensive rather than 'oh and btw we renamed shit'"
2. "it's impossible to overwrite an existing ACC_1 with ACC_2 under same label w/o adding `--overwrite` or removing ACC_1 and alike. (or `--force`?)"
3. "proceed as recommended and propagate/update" — accepting the recommendation of `--overwrite` over
   `--force` for the duplicate-label case. `--rename` is the operator's own word, from quote 1.

They are recorded in `docs/requirements/account-label-integrity.md` § 8.1 (the two "Ruling"
paragraphs) and in `docs/design/account-label-integrity-solution-design.md` § 12.2.

#### Code and requirement facts the contract rests on (read at `b9df076`)

- `plan_capture` in `src/capture.rs` is the one roster decision shared by CLI `capture`, CLI `login`
  (through `reconcile_login`) and the daemon's socket `capture`. It matches the captured account by
  `account_uuid`. A match is a refresh (`CaptureOutcome::Refreshed`, login vocabulary `Revived`), and a
  non-empty label given for it overwrites the entry's label. No match appends a new entry (`Captured`,
  login `Onboarded`), with the label given or, when none is given, the uuid itself (`derive_label`).
  Nothing checks whether another entry already carries the label. It has exactly two arms and no arm
  removes (`docs/requirements/roster-loss-prevention.md` I-1).
- CLI `capture` with no label at an interactive terminal offers the harvested email as a pre-filled
  label at a prompt (#447, `prefill_label_from_identity` in `src/capture.rs`), and a confirmed value
  counts as operator-authored.
- `recapture_updates_the_label_when_a_new_one_is_given` in `src/capture.rs` pins the rename on a
  refresh.
- `remove_account` and `apply_remove` in `src/cli.rs`: `remove` resolves its target through
  `resolve_target` (`src/use_account.rs`), which REFUSES a duplicated label (`Error::UseTargetAmbiguous`,
  issue #1005). The order is roster save, stash delete, then
  `notify_daemon_roster_reload(ReloadIntent::Mutating)`. Removing the active account leaves the daemon
  with no active account until another is made active.
- `docs/requirements/roster-loss-prevention.md` R-3 (user-ratified): a reload from an append-only verb
  (`login`, `capture`) is refused when its account count is LOWER than the live count; R-4 lists the
  mutating verbs; tests in `src/capture.rs` pin `capture` and `login` to `ReloadIntent::AppendOnly`,
  and a flip of either to `Mutating` is called out there as reinstating the 2026-08-27 roster collapse.
- R-12 of the same document (user-ratified): a new machine-readable refusal reason is present in the
  Rust `CaptureRejection`, in the Swift mirror and in the panel's authored capture-states reference, in
  the same change.
- `CaptureAck` and `CaptureRejection` in `src/daemon/socket.rs`: `captured` / `refreshed` /
  `rejected { reason }`. `CaptureRejection` is a field-less kebab-case enum; the Swift mirror in
  `apps/menubar/Sources/CaptureAck.swift` is a CLOSED enum that throws on an unknown tag;
  `every_rust_rejection_tag_has_a_swift_case` fails the `test` job when they drift, and the `rust` CI
  filter includes `apps/menubar/Sources/CaptureAck.swift` so a Swift-only edit runs it.
  `docs/specs/capture-rejection-parity.feature.md` states that a rejection tag carries no operator data
  (no path, account label or count).
- `ConfigSetAck` and `ConfigSetRejection` in `src/daemon/socket.rs`: `applied { effect }` or
  `rejected { reason, detail? }`, zero writes on a rejection; the request re-parses under
  `deny_unknown_fields`, and its editable surface is tunables plus the labels of existing accounts:
  it can never add or remove a roster entry or touch a credential (issue #268). The Swift mirror in
  `apps/menubar/Sources/ConfigWire.swift` throws on an unknown reason; no Rust test reads that file and
  the `rust` CI filter does not list it; `apps/menubar/Tests/SettingsTextMetricsTests.swift` iterates
  `ConfigSetRejection.allCases`, so every reason needs its Settings sentence (#762).
- The CLI has no rename verb: `config` offers `path`, `validate`, `show`, `backups` and `restore`
  only. The CLI renames an account only through `capture` or `login` with a new label while signed in
  as it. The Settings window exists on macOS only.
- `docs/requirements/gui-cli-capability-parity.md`: R-1 (a `routable` CLI capability is reachable from
  the menu-bar app), R-3 (a routable capability with no socket command is a tracked parity gap), R-4
  and R-5 (an actionable panel state offers an `act` or `launch` path; inert text naming a command is a
  defect). #1019 (open) routes `remove` to the app, with confirmation and an active-account guard.

#### Terms

- **The captured account** `U`: the account the write lands on, found by `account_uuid`.
- **The label** `L`: the label the write is given. When none is given, `L` is `U`'s current label
  `M` if `U` is rostered, and the uuid-derived label if `U` is new.
- **A rename**: `U` is rostered under `M`, and `L` differs from `M`.
- **A collision**: the write, as given, would give an account a label it did not carry before, and that
  label would then be carried by another account as well. Evaluated on the write as given; an override
  that removes the holder is what consent buys. `orchestrator`: this replaces REQ-ROS-B-002's count rule
  ("above one and above the number before"), which lets a save such as `{B: "c", C: "work"}` on a roster
  `A: work, B: work, C: c` through, although `C` joins `A`'s label; the per-account rule refuses it, and
  still keeps AC-ROS-002's swap `{A: "b", B: "a"}` quiet. The shared `label_collisions` that #1556 adds
  beside `Account` (design D-1) carries this calibration and serves the four write paths; `apply_import`
  keeps `label_bearers` and `created_ambiguity` unchanged until the operator rules on `import` (C-1),
  and the source-scan test that pins every writer to the shared rule lists `apply_import` as the one
  writer that keeps its own. F8's comparison-key hook plugs into the shared function.
- **A holder**: an account other than `U` that carries `L` before the write.

#### C-1 Which writes the rulings govern (orchestrator)

CLI `capture`, CLI `login`, the socket `capture`, and `config-set`. `import` and `config restore` are
untouched: AC-6 of `docs/requirements/migration-credential-portability.md` still answers
warn-and-never-refuse for `import`, and `config restore` stays the lead already recorded in the scope
brief.

#### C-2 A collision is refused unless overwrite consent can and does apply

- `operator`: on CLI `capture` and `login`, a write that would make a label name a second account is
  refused unless `--overwrite` is given.
- `orchestrator`: the refusal precedes every write (no stash, no roster save, no canonical re-point,
  no `~/.claude.json` co-write, no daemon notification); its machine tag is `label-in-use` on the
  socket and in `config-set`; it applies to the socket `capture` and `config-set` too, where no
  override exists (C-6, C-7).
- `orchestrator`: a DERIVED label never collides. A new account captured with no label given takes its
  uuid as its label (`derive_label`), and REQ-LGN-B-003 forbids that path to hard-error. If another
  account already carries that exact string, which a relabel, an import, a restore or a hand edit can
  produce, the derivation appends `-2`, `-3` and so on until the label is free. The write is never refused for a
  derived label and never completes a collision, and its reply prints the label it chose, as every
  capture and login reply already does. With no label given and `--overwrite`, the derived label is
  made free first, so the flag is surplus (C-5).

#### C-3 A rename is refused unless rename consent is given

- `operator`: a `login <label>` that would rename the account the login lands on is refused unless
  `--rename` is given. Quote 1 offers "`--rename` or `--force`"; that `--force` is not also accepted
  as the rename consent is the orchestrator's reading, so that one flag means one thing.
- `orchestrator`: the same gate applies to CLI `capture`, which shares `plan_capture`, and to the
  socket `capture` (which has no consent, C-6), on the operator's own reason, "be defensive rather than
  'oh and btw we renamed shit'".
- With `--rename` the rename happens, and the reply names the label the account carried before and the
  one it carries after (REQ-IDN-B-001).
- A write on a rostered account that gives no label, or gives `M` itself, is a plain refresh: allowed
  and unchanged, so REQ-LGN-B-004's "a bare `login` for an existing label refreshes it (idempotent)"
  holds.
- `orchestrator`: the #447 prompt runs only when the captured account is new. With no label given, a
  rostered account is refreshed without a prompt, so pressing Enter can never rename it.
- `recapture_updates_the_label_when_a_new_one_is_given` keeps pinning the rename, now with rename
  consent; a sibling pins the refusal without it.

#### C-4 A rename onto a label another account carries is never one write (orchestrator)

A `capture` or `login` that would rename `U` onto a label a holder carries cannot proceed whatever
flags are given, because overwrite consent applies only to a new account (C-5). The single refusal
names the account the write landed on, both conditions, and the two-step way through: free the label
first (C-9), then repeat the write with `--rename`. No flag the refusal names can lead to a second
refusal for the same write.

#### C-5 What overwrite consent does (orchestrator, building on the operator's word "overwrite")

- It applies only when `U` is new and exactly one holder `V` carries `L`. Then `U` replaces `V` one for
  one: `V`'s roster entry is replaced in place by `U`'s, at `V`'s position, and `U` joins enabled like
  every freshly captured account (issue #36). The account count does not change.
- Because the count does not change, the reload intent stays `ReloadIntent::AppendOnly`, R-3's floor
  still protects the roster, and `plan_capture` keeps its two arms (I-1): the replacement composes
  `apply_remove` by `V`'s uuid with `plan_capture` in memory, before one roster save. The orchestrator
  records a dated note in `docs/requirements/roster-loss-prevention.md` that `capture --overwrite` and
  `login --overwrite` replace one entry without lowering the count.
- Order: stash `U` under the swap lock; save the roster once, after the lock is released, as
  `the_roster_save_is_reached_only_after_the_swap_lock_has_been_released` in `src/capture.rs` pins;
  notify the daemon (append-only); then delete `V`'s stash, re-taking the lock if #1556 finds the
  delete needs it. A delete failure leaves an orphan stash, which the reply names by uuid
  (`orphan-stash-policy` stays open), and the command still exits 0; the replacement itself has
  completed.
- With two or more holders (a duplicate already in the roster, from an import, a restore or a hand
  edit), the write is refused as ambiguous, the way `remove` refuses a duplicated label (issue #1005),
  and no flag helps: the operator removes the extra holders by uuid first, or picks another label.
- With `U` already rostered, overwrite consent does not apply (C-4).
- The reply names the replaced account by its uuid, since its label is `L` itself, and says its stored
  login was deleted.
- A `login --overwrite` that replaces the signed-in holder does not make `U` active: #274's rule (a
  login becomes active only when it is the current active account, or none is active) stays. The reply
  says the replaced account was the signed-in one, that no rostered account is active now, and names
  `sessiometer use <label>`.
- Surplus consent changes nothing: an `--overwrite` where no collision would happen, or a `--rename`
  where no rename would happen, gives the same write, the same reply and the same event as the command
  without it.

#### C-6 The socket `capture` carries no consent in these items (orchestrator)

- It refuses a collision with `label-in-use` and a rename with `relabels-another-account`, and accepts
  no consent field. A write that meets both carries `label-in-use`.
- The tags carry no operator data, as `docs/specs/capture-rejection-parity.feature.md` requires: no
  label, no uuid, no count. The panel holds what the card may state with certainty: the label `L` it
  sent, and every roster row's label from the watch snapshot, so the card can say which rows carry
  `L`. It cannot know for certain which account the capture landed on: the snapshot's active row is
  the first entry whose stash matches the canonical credential (`src/active.rs`), re-resolved within
  the poll interval, so it can lag an out-of-band sign-in and can name either entry of a
  shared-credential pair, while the capture lands on the account `~/.claude.json` names
  (`src/capture.rs`). So the card never names the landed account's current label.
- R-12: the item that adds the two tags (#1559) adds, in one change, the Rust tags, their Swift cases,
  the card's sentence for each, and their entries in the mock's capture-states reference.
- The socket `capture` reaches `plan_capture` through `capture_locked`, so it refuses as soon as #1556
  merges. Until #1559 lands, `classify_capture_failure` (`src/daemon/classify.rs`) reports that refusal
  with the existing `failed` tag, with zero writes; #1559 ends that interim by mapping the two
  refusals onto their tags.
- A socket `capture` never completes a rename, so the `refreshed` acknowledgement never carries a
  previous label: REQ-IDN-I-001 and REQ-IDN-I-002 are re-keyed (C-13).
- The panel's way to a deliberate rename is Settings, and its way to replace an account is #1019's
  routed `remove` followed by a capture. The one-action replace the CLI offers is a parity gap under
  R-3, recorded against #1019, not built here.

#### C-7 `config-set` refuses a collision and carries no override (orchestrator, from the code)

- A save is refused whole, with zero writes, when any account it relabels would, after the save, carry
  a label another account also carries (the Terms' calibration, over every label the save touches,
  including two accounts given one new label in the same save). Reason tag: `label-in-use`. A Settings
  rename that creates no collision needs no consent of any kind.
- The `rejected` arm carries `collisions` beside `reason`, only with that reason: a list of
  `{label, bearers}`, where `bearers` holds the uuid of every account that would carry the label. The
  orchestrator records the field in design § 8.1.
- The existing reasons keep their precedence: `label-in-use` is reported only for a save that passes
  every check made before the save (`invalid`, `unknown-account`, `no-config`, `config-unreadable`), and
  it precedes the save itself, so it never co-occurs with `save-failed`. Every acknowledgement for a save
  that creates no collision stays byte-identical to today's.
- No override exists: replacing a holder means removing an account and deleting its stored login, and
  `config-set` structurally cannot add or remove a roster entry or touch a credential (issue #268). The
  operator renames or removes the holder first, which is the operator's own "or removing ACC_1 and
  alike".
- A save that gives no account a label another account carries after the save is applied, including
  one that renames an account out of an existing duplicate, or swaps two accounts' labels.
- The item that adds the tag (#1558) adds, in one change, its Swift case, its Settings sentence (the
  `SettingsTextMetricsTests` loop over every reason), and a reason-parity test for
  `ConfigSetRejection` like `every_rust_rejection_tag_has_a_swift_case`, with
  `apps/menubar/Sources/ConfigWire.swift` added to the `rust` CI filter. Editing
  `.github/workflows/ci.yml` owes the `Gate-Change-Acknowledged:` trailer.

#### C-8 What remains of the warn-form notices (orchestrator)

- No completed write on the four paths creates a collision (C-2 covers the derived label). The
  collision lines of the warn form do not ship.
- Duplicates can still exist, from `import`, `config restore` or a hand edit.
  The surfaces that show an existing duplicate stay: the Settings note (#1560), the panel rows (#1566),
  the Settings ordinals (#1574), and label resolution's ambiguity refusal.
- What a completed CLI write still discloses: a rename made with rename consent (both labels), a
  holder replaced with overwrite consent (its uuid and the deleted stored login). A shared credential (F3, #1557 and its surfaces) is unchanged by the
  rulings, on every path.

#### C-9 What each surface shows on a refusal (orchestrator)

- CLI: one refusal message. It names, in this order: the account the write landed on (its current
  label, or "not on the roster" and its uuid); the reason; each flag that would let the write proceed,
  only where one would; and the ways through that fit the case and that the CLI can run. For a rename
  refusal: run it again with no label, which refreshes, or with `--rename`. For a collision: pick
  another label; free the label by removing its holder (`sessiometer remove <uuid>`) or by signing in
  to the holder and running `capture <other-label> --rename`; or, on macOS, rename it in Settings. A `login` refusal also says
  the login was not saved and that, if the landed account is not the one meant, the operator signs in
  to the intended account instead. Exit code: the CLI's existing taxonomy; #1556 picks the code and
  names its precedent.
- Audit: a refused write logs its outcome as refused on the event its path already emits, with the
  reason as a snake_case token (`label_in_use`, `relabels_another_account`): the daemon's capture event
  (`Event::Capture`, built in `src/daemon/commands.rs`) for the socket `capture`, and the login event
  (`emit_login_event` in `src/capture.rs`) for CLI `login`. CLI `capture` emits no event today; #1556
  says whether it gains one.
- Panel capture card: it names the reason in words. For `relabels-another-account` it says the account
  is already on the roster under another label, naming none (C-6). For `label-in-use` it names the
  label `L`, which the panel sent and so knows whatever the snapshot shows, and, when the snapshot
  shows two or more rows carrying `L`, how many; when it shows none, the card still names `L`. The
  `relabels-another-account` card is therefore the sentence plus the remedies, where the CLI refusal
  names the landed account's current label: a parity gap the scope brief records
  (`refuse-form-relabel-card-names-no-label`), closable by a field beside the tag, as `config-set`'s
  `collisions` sits beside `reason`, never by data on the tag. Its remedies are `act` or `launch` paths,
  never inert text naming a command (parity R-4, R-5): edit the label on the card and capture again,
  or open Settings to rename an account. No override button.
- Settings: a refused save stays unsaved in the form. #1558 carries the reason's fixed sentence (the
  Settings client renders reason and detail only); #1560 names the label on each marked row, in words
  and never by colour alone, and marks the listed rows whose uuid is in the refusal's `collisions`; a
  bearer the window does not list gets no row. No announcement replaces the warn form's: the refusal
  is a rejected-save outcome the form already presents. The marking lasts as long as that outcome: the
  next save clears it whatever it returns, and so does the next load. The persistent note for existing
  duplicates stays.
- Every new string passes the repo's framing scans, names accounts by label, ordinal or uuid only,
  never by email or token (issue #15), and quotes no credential. A new Swift string is bound to the
  scan the way `apps/menubar/Tests/NotificationDeliveryTests.swift` does it, by reading
  `src/framing_vocabulary.rs` at test time, never checked by hand.

#### C-10 What changes for #1570 (orchestrator)

`LOGIN_USAGE`'s `<label>` entry describes what #1556 leaves in the tree: a label that would rename the
account the login lands on is refused unless `--rename` is given, which renames it; when the account is
new to the roster, a label one other account carries is refused unless `--overwrite` is given, which
replaces that account and deletes its stored login; a label two or more accounts carry, or one that
would both rename the landed account and take another account's label, is refused and no flag
overrides that. #1570 stays `Blocked by #1556` and owns that sentence and its propagation sweep. #1556
owns both flag lists, `CAPTURE_USAGE`'s `<label>` entry, and every comment in `src/capture.rs` that
describes the rename on a refresh, the label prompt or the append-only shape (`derive_label`,
`plan_capture` and its refresh arm, `run_login`, `prefill_label_from_identity`'s doc, and the
append-only comments beside the daemon notifications, one of which says a login "drops none"),
because it changes what they describe and none of them carries the sweep's search words.

#### C-11 Oracles (the scope brief's `oracle-authorship` rule, unchanged)

A deterministic-output item whose expected outcomes are all decidable under this contract gets its
own `(test)` oracle item, RED before any fix, delivered by a party other than the fix's executor, and
the fix item is `Blocked by` it. An item whose oracle still waits on a seam another item lands, or on
another open operator decision, records that and files nothing. A prose/config item names its existing
checks and owes no oracle (`ready-gate` § Work-Class Split for Oracle Authorship). Authors DRAFT oracle
bodies; the orchestrator files them after the planning pull request merges.

#### C-12 Wire names, fixed so the Rust and Swift halves cannot drift (orchestrator)

- Socket `capture`: two new `CaptureRejection` tags, `label-in-use` and `relabels-another-account`,
  with no other field (C-6). Swift cases `labelInUse = "label-in-use"` and
  `relabelsAnotherAccount = "relabels-another-account"`.
- Socket `capture`, `refreshed` arm: no `previous_label` (C-6). `shared_with` (F3, #1557) is unchanged.
- `config-set`, `rejected` arm: a new `ConfigSetRejection` tag `label-in-use`, with `collisions` (a list
  of `{label, bearers}`, `bearers` in roster order) beside `reason`, present only with that reason. The
  `applied` arm carries no `collisions`. Swift case `labelInUse = "label-in-use"`. This refusal may carry
  labels and uuids where a `CaptureRejection` tag may not, because `config-get` already sends the same
  labels and uuids to the Settings client (design § 8.1); the capture-rejection-parity rule governs
  `CaptureRejection`.
- A present-but-malformed new field drops that fact and keeps the outcome (register row
  `malformed-key-drops-the-fact`).
- The order of `collisions` when one save collides on two labels stays `config-set-ack-collisions`
  Red 2.

#### C-13 What the orchestrator records in the planning documents, in the re-authoring pull request

- PRD § 8.1's two Ruling paragraphs and the migration PRD's AC-6 amendment note: the socket `capture`
  and `config-set` refuse with no override (correcting "unless `--overwrite` is given" there).
- `docs/requirements/roster-loss-prevention.md`: the dated note of C-5.
- PRD criteria the refuse form re-keys: REQ-IDN-I-001, REQ-IDN-I-002, AC-IDN-001's "`work` now names two
  accounts", AC-IDN-002's collision report, and every other one the six authors report.
- Design § 8.1: the `config-set` `collisions` field on `rejected`, and the two capture tags.
- The scope brief: a register row per `orchestrator` decision above, and each item's new verdict.
- The scope brief: the row `refuse-form-relabel-card-names-no-label` for the parity gap C-9's panel
  bullet names.
- PRD § 8.2: REQ-LGN-B-004's ratification stamp of 2026-07-01 ("the accidental-overwrite risk is
  accepted") is superseded by the rulings for the rename and collision cases; its home is outside this
  repository, so the note lands where § 8.2 surfaces it.

#### Open decisions these items still meet, unchanged by the rulings

`panel-notices-name-the-uuid` (design § 14.6 item 10a), `panel-switches-by-identifier` (item 15),
`label-normalization-policy`, `account-key-form` (DQ-1), `settings-accounts-frame-owner`,
`mock-page-restructure`, `swift-red-landing-form`, `orphan-stash-policy`,
`ready-items-before-ratification`, and the operator's ratification of the PRD. An item that holds
under every option of one of them is not blocked by it.

## 12b. Alternatives Considered and Drawbacks

**Rejected approaches.** A collision check written at each call site, because the next writer ships without
it. A refusal taken after the stash write, because undoing it is a keychain delete that can fail. Prose
inside acknowledgements, because it couples the app's display to Rust text. A new `result` value, because
today's decoder throws on it. Shared-credential detection inside the swap lock, at daemon start, or per
poll: the first delays swaps, the second is mostly blind once a refresh rotates the token (A-2), and the
third breaks REQ-STSH-Q-002. One key form for every surface, because it would chain the store migration to
the open wire fork. A backfill marker inside the rollup document, because an older binary erases it. A
label-to-uuid map that readers apply instead of a rewrite: no rewrite, but every reader carries the map,
and REQ-USG-B-001 asks for keyed samples. Relabel continuity in `stats`, because it changes output after a
relabel on a roster with no duplicate. Same-label separation in `stats`, because it reshapes the wire.
Uuid tokens on every swap line, because they change `log` output for every roster. An export-side identity
comparison (DQ-7), deferred until F7's premise 1 measures false or A-8 is observed. A uuid-free panel
notice, because REQ-STSH-B-002 (a Must) and REQ-NTC-C-001 name the uuid where a label is ambiguous; it
stays on record for the operator (§ 14.6 item 10a). The uuid as the duplicate set's handle, because opening
Settings closes the panel and resets the card, so a 36-character string is lost on the way, while the
ordinal is short, uuid-free and computed the same way on every surface. A per-row next-swap marker, because
R-2 dropped it; the callout carries the ordinal instead. A switch that sends the identifier, recorded for
the operator rather than chosen, because it changes what the panel's switch addresses and depends on
`account-key-form` (§ 14.6 item 15). A Settings note tied to the save's acknowledgement, because it misses
a duplicate made elsewhere and clears on an unrelated save.

**Drawbacks of the chosen design.** F3 misses a pair that a refresh splits before any write sees it. The
backfill attributes against the roster at upgrade time, so a label that moved between accounts inside the
14-day window lands on its current bearer. The ack's `count` and the CLI's "in rotation" count different
things. Ordinals follow roster order, so they move when the operator reorders the roster, when a member of
a set of three or more is renamed, or when a relabel joins a set at an earlier position; and Settings reads
the file through `config-get` while the panel reads the daemon's memory, so the two can disagree while a
CLI write's reload lags. The parity gate
covers the new fields only. Until `account-key-form` is decided, F4's wire and panel item cannot start, while its other items can
(§ 4). Under D-8's default the backfill's attributions have no reader inside the raw window (§ 14.6 item
11). A uuid in a card line is long; the announcement carries the ordinal sentence instead, and the uuid is
read on demand. A shared label blocks manual switching to either account from the panel until one is
renamed, as the CLI's `use work` refuses too, although `use <uuid>` still reaches either from the CLI.

## 13. Quality Requirements

| Measure | Target | How the design meets it | Verified by |
|---|---|---|---|
| `CollisionReportPrecision` (REQ-ROS-Q-001) | 0 reports on a write REQ-ROS-B-002 does not count | the before/after rule, D-1 | CAP-01: swap, refresh of a two-bearer label, same-label relabel, fresh label |
| `SharedCredentialDetectionWindow` (REQ-STSH-Q-001) | reported at the write that created the pair | detection runs at that write, D-4 | CAP-07: write a duplicate, then refresh one side |
| `DetectionKeychainCost` (REQ-STSH-Q-002) | at most one read per other entry per write; none per poll or swap | D-4 reads only on the write paths | CAP-08: a read-counting stash |
| Compatibility (REQ-ROS-I-001, REQ-WIRE-C-003, REQ-ROW-C-001, REQ-MIG-C-001) | no decode error; one minor bump; frozen fixtures untouched | D-6, D-10, D-11 | CAP-03, CAP-10, CAP-11, CAP-16 |
| Disclosure (REQ-STSH-C-001, REQ-NTC-C-001, REQ-WIRE-C-001) | nothing credential-derived; no unauthored email; meter-clean wire | D-3, D-4, D-6 | CAP-09, CAP-22, CAP-10 |
| Recoverability (REQ-USG-C-001, REQ-USG-C-002) | no sample lost; witness reads the same | D-7 | CAP-12 |

## 14. Risks and Open Questions

### 14.1 The readiness verdict and who acknowledged it

The PRD records `dor_status: failed`. Check 1 fails because the operator has not validated the problem
statement; check 6 fails because every requirement is pipeline-authored and unratified. The PRD's own
text says the verdict blocks scoping and design until the operator ratifies.

This design proceeds anyway, on an acknowledgment the `/scope` orchestrator gave on the operator's behalf
in a headless run. That acknowledgment is **pipeline-supplied and ratification-pending; it is not the
operator's**. The design is therefore PROVISIONAL: the frontmatter and the brief say `draft`, every
decision in § 14.8 waits for ratification, and nothing here should be scoped into work items until the
operator has ratified the PRD and re-run its readiness gate. PRD § 13 grades F4 INCOMPLETE until Stage 2
answers DQ-1 to DQ-3 and the mock frame exists. This design answers DQ-2 and DQ-3, confines DQ-1 to the
wire (D-5) and leaves it open, and specifies the frame without authoring it; F4 stays INCOMPLETE on those
two.

> **Update before commit (the `/scope` orchestrator, 2026-11-02).** Stage 3 of the same run scoped this design
> into #1551 to #1585 under the scope brief's register row `proceed-past-failed-dor`, with every readiness verdict
> marked provisional on ratification; § 4 and § 14.7 cite those items. The paragraph above stands as the design's
> position at its lock. What it leaves unsettled, now that the items exist, is whether a READY item may be built
> and merged before the operator ratifies: the scope brief's Next Steps and its register row
> `ready-items-before-ratification` record that as an open ruling, and `d9-token-form-one-way-1563` is the one
> case the register settles.

### 14.2 Feasibility summary

| Component | Priority | Verdict | Weakest signal and its evidence |
|---|---|---|---|
| Collision rule | Must | FEASIBLE | import's rule exists (`label_bearers`, `src/cli.rs:5245`) |
| Plan seam and pre-write gate | Must | FEASIBLE | the plan already precedes the write; the witness gate is a pre-write refusal precedent (`admit_append_only`, `src/witness.rs:273`) |
| Notice templates and the #160 scan | Must | FEASIBLE | the advisory audience exists (#1123); a Swift test already reads the vocabulary |
| Shared-credential detector | Must | FEASIBLE | its value may be low (it can be inert), a risk rather than a buildability gap (RK-04) |
| Wire identifier and next-swap identifier | Must | FEASIBLE | additive minor bumps have precedent (#393's `reason`); sequencing with #1457 |
| Panel rows | Must | FEASIBLE | the new cue needs the mock frame first |
| Usage backfill | Must | FEASIBLE | the atomic rewrite under `usage.lock` is tested under injected faults (#188) |
| Event tokens and `credential_shared` | Must / Should | FEASIBLE | the trailing-token precedent (#634) |
| Ack fields and value-asserting Swift tests | Must | FEASIBLE | `refreshed` waits on #1474 |
| Import checks | Must | FEASIBLE | the `failed` outcome exists |
| Orphan census | if `report` | FEASIBLE | the witness's metadata dump |
| Orphan reaper | if `reap` | FEASIBLE-WITH-SPIKE | no lock spans the stash-to-save window; SP-2 |
| F7 measurement | Must | FEASIBLE-WITH-SPIKE | it is the spike; operator-only |
| Confusable detection | if adopted | FEASIBLE-WITH-SPIKE | a dependency or a table; SP-3 |
| Records and amendments | Must | FEASIBLE | the amendment precedent; `scripts/check-doc-citation-rot.sh` exists |

Gate: **PROCEED**. Every Must-Have is FEASIBLE or FEASIBLE-WITH-SPIKE. Recommended tracer bullet: F4's
wire-and-panel item, once `account-key-form` is decided — the identifier from the snapshot through a golden
and the Swift decoder to the row identity and the callout's row lookup, against the mock frame. It is third
in § 4's order: the items that wait on no decision land first, so the tracer bullet is off the critical path
while that decision is open.

### 14.3 Risk register

Likelihood and impact run from 1 to 3; their product reads 7–9 HIGH, 4–6 MEDIUM, 1–3 LOW.

| ID | Risk | L | I | Score | Owner | Mitigation |
|---|---|---|---|---|---|---|
| RK-01 | the design builds on requirements the operator may reject | 2 | 3 | 6 MED | operator | PROVISIONAL; brief `draft`; § 14.8 lists every decision |
| RK-02 | `account-key-form` stays open and holds F4's wire and panel half | 3 | 2 | 6 MED | operator | D-5 keeps the store and the log independent of it |
| RK-03 | a reaper deletes an in-flight stash or the witness (PM-22, T-03) | 2 | 3 | 6 MED | implementer | `report` recommended; if `reap`, SP-2 and the grace window |
| RK-04 | the detector is inert and zero detections read as "no sharing" (PM-3) | 2 | 2 | 4 MED | implementer, operator | the mutation pass; MET-R-003's reading names the blind spot; SP-1 premise 1 |
| RK-05 | #1457 and F4 claim one minor (PM-8) | 2 | 2 | 4 MED | the second to merge | the number is read at merge; goldens regenerated after rebase |
| RK-06 | #1474 has not landed, so `refreshed` notices cannot reach the panel | 2 | 2 | 4 MED | implementer | Rust fields first; `captured`'s collision line does not wait |
| RK-07 | F7's walk never happens | 2 | 2 | 4 MED | operator | MET-R-001's gate; one walk serves #1020, #1542 and #1546 |
| RK-08 | F4 overruns | 2 | 2 | 4 MED | implementer | separate items (§ 4), the decision-free ones first; the other features land alone |
| RK-09 | a shared-path change breaks the uncompiled Linux build (PM-20) | 2 | 2 | 4 MED | implementer | no platform call added; G-LINUX |
| RK-10 | confusable detection, if adopted, flags the hyphen pairs | 2 | 2 | 4 MED | implementer | AC-LBL-001's BUT NOT as a test; SP-3 |
| RK-11 | the backfill empties the store and flips the witness (PM-6) | 1 | 3 | 3 LOW | implementer | atomic writer; never creates; fault-injection tests |
| RK-12 | the parity gate leaves the `count`/`accounts` drift (PM-15) | 3 | 1 | 3 LOW | operator | § 14.7 |
| RK-13 | false notices train dismissal (PM-2) | 1 | 2 | 2 LOW | implementer | REQ-ROS-Q-001's fixtures |
| RK-14 | a refusal lands after the stash write (PM-14) | 1 | 2 | 2 LOW | implementer | D-2; AC-IDN-003 |
| RK-15 | an inline literal escapes the #160 scan (PM-13) | 2 | 1 | 2 LOW | implementer | named templates |
| RK-16 | F3's reads delay a socket reply (PM-4, T-06) | 1 | 2 | 2 LOW | implementer | after the lock; the read-count meter |
| RK-17 | the backfill misattributes after a relabel or a downgrade (PM-5, PM-21) | 1 | 2 | 2 LOW | implementer | sentinel file; 14-day reach |
| RK-18 | key-grouped data lands in a label-keyed map (PM-7) | 1 | 2 | 2 LOW | implementer | D-8 |
| RK-19 | swap tokens change output for ordinary rosters (PM-12) | 1 | 2 | 2 LOW | implementer | conditional tokens; a byte-identity test |
| RK-20 | import checks run after the lock or the first write (PM-18) | 1 | 2 | 2 LOW | implementer | placement pinned by tests |
| RK-21 | uuid case variance makes F6 refuse falsely (PM-17, A-13) | 1 | 2 | 2 LOW | operator | SP-1 premise 4; one comparison function |
| RK-22 | an ordinal reads as a rank or as part of the label, or a colour collides with a third label | 2 | 1 | 2 LOW | implementer | a badge set apart from the label; the blocked row's tooltip explains it; a colour slot no other row uses while one is free; mock review |
| RK-23 | the `LOGIN_USAGE` edit collides with #1379 | 2 | 1 | 2 LOW | implementer | coordination; AC-DOC-003 |
| RK-24 | an older daemon yields duplicate SwiftUI ids (PM-11) | 1 | 1 | 1 LOW | implementer | the position-based fallback |

No HIGH risk. Rabbit holes, each resolved by a constraint: `stats` consuming the key (a redesign) is out
(D-8); widening the socket capture's event slot invites the roster-reload refusal fix
(its one `Event` slot, `src/daemon/commands.rs:386-390`), which stays out; a whole-body parity gate invites the `count` fix,
which is reported instead; re-seeding every colour is out by default; F7 is time-boxed by the walk. The 10x
test: no single overrun sinks the batch, because features land independently and the circuit breaker
returns unlanded ones to `/scope`. Gate: **PASS-WITH-ACTIONS**.

### 14.4 Spikes

- **SP-1 — the identity premises (Must; F7 itself).** Questions: does one isolated login write the
  credential and the `oauthAccount` of one account; does one person's account keep one `account_uuid`
  across two organisations (DQ-8 says it may be unmeasurable with the accounts held); does
  `claude auth login --claudeai` still write `emailAddress`; does Claude Code ever vary a uuid's case.
  Time box: one operator walk, shared with #1020, #1542 and #1546. Unlocks: whether `login-mismatch-policy`'s
  email-compare option is viable; the confidence of A-1, A-3 and A-13. Done when each premise is recorded
  with its result, the Claude Code version and the date, or with why it could not be taken.
- **SP-2 — can a reaper prove an orphan is not in flight (only if `orphan-stash-policy` = reap).** Time
  box: half a day. Questions: does the metadata dump expose a per-item modification time without
  decrypting or prompting; what bounds a capture's stash-to-save window; does reaping change what the
  witness reads where `config.toml` loads. Blocks the reaper item.
- **SP-3 — a confusable source that leaves hyphen pairs alone (only if detection is adopted).** Time box:
  one day. Questions: is there a crates.io source of UTS #39 skeletons under an allow-listed licence; do
  `a-b@x.com` and `ab@x.com` stay distinct under it; what does a table cost if no crate qualifies. Blocks
  REQ-LBL-B-001's item.

### 14.5 Carry-forward register — design-stage rows

Fields per the register: item, importance, evidence, verdict, signpost, hedge, owner, status, stage of
origin, and the stub it references.

| Item | Importance | Evidence | Verdict | Signpost | Hedge | Owner | Status | Stage | Stub |
|---|---|---|---|---|---|---|---|---|---|
| readiness acknowledgment | blocks scoping | PRD § 14c | Surface | the operator ratifies or rewrites PRD § 2 and § 10 | design kept `draft` | operator | ratification-pending | design | — |
| identifier form on the wire (DQ-1) | high: costly to reverse | the tree's #15 conventions; `config-get`; A-6 | Surface | #791 ruling; a pasted uuid; premise 4 | D-5 confines it to the wire | operator | open | requirements | `account-key-form` |
| warn or refuse a collision | high | AC-6; the silent writers (PRD § 2.3) | Surface | AC-6 re-ratified | D-2's gate serves both | operator | ruled 2026-11-03: refuse unless `--overwrite` (§ 12.2) | requirements | `duplicate-label-policy` |
| login that lands elsewhere | high | REQ-LGN-B-004 vs code | Surface | SP-1 premise 3; #846 | disclosure holds under all | operator | ruled 2026-11-03: refuse unless `--rename` (§ 12.2) | requirements | `login-mismatch-policy` |
| label sameness | medium | no record | Surface | a reported confusable pair | D-1's key hook | operator | open | requirements | `label-normalization-policy` |
| duplicate credentials in a payload | medium | A-7 | Surface | A-7 found false | placement fixed (D-11) | operator | open | requirements | `import-duplicate-uuid-policy` |
| orphan stash items | medium | A-10; witness | Surface | DQ-9's census count | the census is shared by both options | operator | open | requirements | `orphan-stash-policy` |
| `stats` consumes the key (DQ-3) | medium | F4 invariant; `PeriodWire.accounts` | Decide (default: no) | the operator asks for relabel continuity or separation | the key is stored now | operator | decided, ratification-pending | design | — |
| start-up sweep (DQ-5) | low | A-2 | Decide (no sweep) | MET-R-003 records a pair a write missed | the compare function serves a sweep later | implementer | decided, ratification-pending | requirements | — |
| channel for F3 (DQ-6) | low | REQ-STSH-B-002 | Decide (both) | — | — | operator | decided, ratification-pending | requirements | — |
| count wording (DQ-10) | low | `enabled` doc | Decide (enabled count) | the operator prefers roster wording | one template | operator | decided, ratification-pending | requirements | — |
| colour seed | low | #445 | Decide (tie-break) | the operator or UX lens prefers identifier seeding | seed function unchanged | operator | decided, ratification-pending | design | — |
| export-side identity check (DQ-7) | low | A-8 unknown | Defer | SP-1 premise 1 false, or A-8 observed | F6 catches it on import | operator | deferred; tracked here and as PRD DQ-7 | requirements | — |
| identity premises | high | PRD § 2.9 | Test (SP-1) | the walk is recorded | only the email option waits | operator | open | requirements | — |
| uuid in panel notices where a label is ambiguous | high: a Must clause | REQ-STSH-B-002, REQ-NTC-C-001, PRD § 9.4 against PRD § 3, AC-WIRE-001 | Decide (default: named) | the operator keeps § 3's exclusion for notices | the notices keep their ordinals and drop the uuids; both requirements' uuid clauses narrow for the panel | operator | decided, ratification-pending; returns to the PRD | design | — |
| a Settings row's own uuid | medium | PRD § 3 defers it until the operator adds it; PRD § 9.4 | Surface (not built until answered) | the operator answers `settings-row-shows-uuid` | the ordinal tells the rows apart without it | operator | open; returns to the PRD | design | — |
| the panel's switch on a shared label | medium | PRD § 2 lists "swap to it"; PRD § 9.3 has no swap action; the daemon refuses an ambiguous target | Decide (default: blocked before the click) | the operator wants switching by identifier | a uuid target resolves with no daemon change; a derived key needs one | operator | decided, ratification-pending; returns to the PRD | design | — |
| the backfill's payoff under D-8 | medium | `DEFAULT_RAW_WINDOW_SECS`; `stats` groups by label | Surface | the PRD settles AC-USG-001's reading | the backfill is F4's last item | operator | returns to the PRD | design | — |
| MET-T-004 as a proxy | low | D-8 | Surface | the operator widens or narrows the meter | — | operator | returns to the PRD | design | — |
| the Serves line | low | PRD § 3 keeps the identifier off the rows | Surface | the operator accepts or narrows it | — | operator | returns to the PRD | design | — |

### 14.6 Open questions

Load-bearing — each blocks implementation of some part, or changes a decision, and each keeps this design
at `draft`:

1. **The readiness acknowledgment** (§ 14.1): only the operator can ratify the PRD.
2. **`account-key-form`** (DQ-1): F4's wire and panel half waits on it. The store, the log and
   every other feature do not.
3. **`duplicate-label-policy`**: decides whether F1's oracles assert a report or a refusal, and whether
   `label-in-use` exists. Ruled 2026-11-03: refuse unless `--overwrite`, so `label-in-use` exists (§ 12.2).
4. **`login-mismatch-policy`**: decides whether a renaming revive is reported, refused or compared. Ruled
   2026-11-03: refused unless `--rename` (§ 12.2).
5. **`label-normalization-policy`**: decides whether REQ-LBL-B-001 applies at all.
6. **`import-duplicate-uuid-policy`**: decides the form of F5's report.
7. **`orphan-stash-policy`**: decides whether F10 builds a report, a reaper or nothing.

Decided by default, ratification-pending — each is a visible outcome the operator may overturn:

8. **`stats-consumes-the-key`**: should `stats` use the stored key, for relabel continuity, same-label
   separation, or both? Default: no, this batch (D-8). Continuity changes output after a relabel;
   separation also bumps `stats`' schema and its Swift mirror.
9. **`colour-seed-from-identifier`**: should every row's colour come from its identifier, as #445 first
   asked? Default: no; only a duplicate set gets distinct slots (UX-3). Yes repaints every panel once and
   moves the panel goldens.

Returned to the PRD by the lenses (§ 14.9), or raised at scope time (item 16), each with the design's provisional answer. Items 10a, 11 and 15
change what F1, F3 and F4 build, so they hold the design at `draft` with items 1 to 7; item 10b builds
nothing until it is answered:

10a. **`panel-notices-name-the-uuid`.** The PRD points two ways. § 3 defers "an operator affordance to see
    the uuid behind each label" and AC-WIRE-001's BUT NOT keeps F4's identifier off the display;
    REQ-NTC-C-001 (a uuid "where the label alone is ambiguous"), REQ-STSH-B-002 (a Must: both entries "by
    account uuid where their labels are equal, on the surface that performed the write") and § 9.4
    (`account_uuid` displayed "where a label is ambiguous") name it. The design follows the second reading
    for notices: the card's lines and the Settings note name an entry's uuid beside an ambiguous label,
    always next to its ordinal (UX-9). Under the first reading the notices keep their ordinals and drop the
    uuids, and both REQ-STSH-B-002's and REQ-NTC-C-001's uuid clauses narrow for the panel; the ordinal still
    tells the entries apart, so "Rename one in Settings" stays. If unanswered, the requirements as written
    govern and the notices name the uuid.
10b. **`settings-row-shows-uuid`.** Should an ambiguous Settings row also show its own uuid under its label
    field? It is the narrowest form of the affordance § 3 defers until "The operator adds it", so it is not
    built until the operator answers yes. Telling the rows apart does not wait on it: the ordinal does that
    on every surface, uuid-free (UX-3). Yes lets a Settings row be matched with `sessiometer list` and the
    event log's uuid tokens; no leaves that match to the note's uuids and to the CLI. Dropping the ordinal as
    well would leave the two rows apart only on the CLI, which the operator's parity ruling of 2026-07-31
    rules out ("Choosing a surface is not accepting reduced capability",
    `docs/requirements/gui-cli-capability-parity.md`).
11. **`backfill-reading-of-ac-usg-001`.** Does "re-keyed" mean the migration moment, the design's reading,
    which owes the backfill (D-7), or the moment a reader switches to the key, under which the raw tier
    re-keys itself by turnover inside `DEFAULT_RAW_WINDOW_SECS` and no backfill is owed? Under D-8's
    default nothing reads a backfilled key before it ages out, so the backfill pays off only if item 8 is
    answered yes inside that window. The migration reading spends a one-way rewrite of a file the witness
    reads (RK-11, RK-17) on attributions nothing reads; the turnover reading drops an item and leaves
    AC-USG-001's "kept under `A`'s identifier" to turnover. If unanswered, the backfill stays F4's last item.
12. **MET-T-004 as a proxy.** The meter reads 0 once the store is keyed, while `stats`, `reliability` and
    the panel's stats view still merge two accounts under one label and split one across a relabel (D-8).
    The operator widens the meter, narrows its claim, or answers item 8. The cheapest mitigation under item
    8's default is a cue in the panel's Stats tab marking a label that more than one account holds, which
    the app knows from the status frame; no requirement asks for it, so it is recorded here, not built.
13. **The Serves line.** "The operator can always tell which account a label names" is delivered on the CLI
    (`list`, the notices) and, on the panel, by the ordinal wherever two accounts share a label, with the
    notices' uuids under item 10a; no requirement delivers the identifier itself on F4's rows. The operator
    accepts or narrows the line when ratifying.
14. **REQ-DOC-B-003's wording** assumes the keep-rename answer to `login-mismatch-policy`; a refuse answer
    makes "renames that account" false, so the `LOGIN_USAGE` edit follows that decision (§ 4, D-13).
    Answered 2026-11-03: refuse (§ 12.2); the wording is re-keyed to § 12.3 C-10.
15. **`panel-switches-by-identifier`.** The PRD lists "swap to it" among the acts a shared label spoils
    (§ 2), but no requirement names the panel's switch, and PRD § 9.3's CTA inventory has no swap action. Today both of the
    panel's swap actions send the label, and the daemon refuses an ambiguous one after the click. The design
    shows that refusal before the click (UX-10): neither account can be switched to from the panel until one
    is renamed, as the CLI's `use work` refuses too, although the CLI's `use <uuid>` reaches either today and
    the daemon's own rotation still swaps onto either. The alternative sends the account identifier as the
    swap target: a uuid needs no daemon change, because `resolve_target` matches a uuid, while a derived key
    needs `swap` to resolve it, so the operator weighs this with `account-key-form`. The operator's parity
    ruling of 2026-07-31 ("Choosing a surface is not accepting reduced capability") favours the alternative
    once the wire carries the uuid, with the blocked switch kept for an older daemon; the blocked switch
    meets the ruling's letter, because a rename makes either account reachable from the app. If unanswered,
    the blocked switch stands.
16. **`stored-key-name-and-form`.** The usage store's per-sample key is named `account_uuid`, holds the
    uuid verbatim and is left out of a line that has none (D-5, D-7). A ruling on the name or the value is
    a two-line edit in #1580's tests and #1562's code; a ruling on the form, a `null` instead of an omitted
    key, also moves two of #1580's expected outputs. Lines already written keep the old form until they
    age out of the raw window. If unanswered, the default stands.

Not load-bearing: the draft wording of every notice (§ 8.5, § 9.6), which copy review may refine within
the #160 vocabulary.

### 14.7 Findings outside this design's goal

- Rust's `captured` acknowledgement sends `count`, and Swift reads a field named `accounts` instead
  (`CapturedBody`, `apps/menubar/Sources/CaptureAck.swift:145-147`), so the app never receives the count.
- The socket capture's single event slot also keeps the roster-reload refusal unlogged
  (one `Event` slot, `src/daemon/commands.rs:386-390`, a known gap that comment assigns elsewhere). F3 widens the slot, which
  makes that fix adjacent; it stays out of this design.
- The Stage 1 seed recorded stale tracker text: #1056's passage saying `enable`, `disable` and `remove`
  take the first match (the code refuses through `resolve_target`); #1059 listing "OQ-1 duplicate-label
  policy" as unresolved; #1020's "BLOCKED BY #1009" banner, though #1009 is closed.
- The repository's `CLAUDE.md` (§ The design mock is the panel's build reference) says the Settings window
  is outside the mock "entirely", while `apps/menubar/design/README.md` says it "IS authored, as Group 8
  (#763)". This design follows the README; the `CLAUDE.md` line is its owner's to correct.
- "Active" means two things across the panel-to-Settings hand-off: signed in, in the panel's header, and
  enabled, in Settings (`accountStateCue`, `apps/menubar/Sources/SettingsModel.swift:703`), where both
  duplicate rows read "Active". It predates this design.
- Added by the `/scope` orchestrator on 2026-10-27, after the lens passes: the Settings **Accounts** section row
  anatomy has no owning issue. `apps/menubar/design/README.md` routes it, with the rest of register R-11's
  Settings row, to #946, but #946's body covers only the launch-at-login approval sub-block and the inline
  per-field format-error row. #1560 and #1574 draw their badge and note in that section first, so both wait
  on a frame nobody is tasked to author. Widening #946 or filing a frame item is the operator's call.

### 14.8 Decisions awaiting ratification

Every decision below was taken by this pipeline. Author-chosen defaults — a value picked where real
alternatives existed — are marked **(default)**.

- The product type (a hybrid: CLI, local daemon, menu-bar client over a local socket) and the tracks run:
  Technical Architecture, Data Architecture, UX/IA (menu-bar only), API Design, Security Architecture,
  Testing Architecture. Omitted: Performance (REQ-STSH-Q-002 is settled by placement and folded into
  § 13), Infrastructure and Integration (no new deployment or external system), UX Prototype (no user
  validation planned), UI/Visual (no new component; folded into UX/IA and REQ-ROW-C-002).
- D-1 to D-14 (§ 5, § 12.1), with D-3's enabled count **(default)**, D-7's sentinel placement **(default)**,
  D-8's unchanged readers **(default)**, and the field names `collisions`, `previous_label`, `shared_with` (of which only `shared_with` and `config-set`'s `collisions` ship under § 12.3 C-12),
  the event name `credential_shared` and its `via` token **(default)**.
- The rejection tag names `label-in-use` and `relabels-another-account`, used only if chosen **(default)**.
- UX-1 to UX-11 (§ 9.1), with UX-3's ordinal as the duplicate set's one handle **(default)**, UX-9's
  uuid-where-ambiguous rule for notices **(default)**, UX-10's blocked switch **(default)**, UX-11's
  state-derived Settings note **(default)**, and the draft copy **(default)**. UX-9 settles, provisionally,
  a conflict inside the PRD; it returns there as § 14.6 items 10a and 10b, and UX-10 as item 15.
- The acknowledgement fields `account_uuid`, `collisions[].bearers` as uuids and `shared_with[].account_uuid`
  **(default)**.
- D-6's next-swap identifier, beyond REQ-WIRE-I-001's wording (§ 16b), ratified together with
  `account-key-form`.
- Returned to the PRD by the lenses: § 14.6 items 10a to 15. The product lens confirmed D-3's enabled count
  against F4's invariant (§ 14.9).
- SEC-3's recommendation of `report` over `reap` **(default)**.
- The answers to DQ-2 (unattributed samples age out unchanged), DQ-3 (no `stats` shape change), DQ-5 (no
  sweep), DQ-6 (both channels) and DQ-10 (enabled count); DQ-7 deferred with its signpost.
- The delivery order and F4's split into separate items (§ 4).
- Treating DQ-1 and the PRD § 8.1 decisions as open stubs (§ 12.2) rather than deciding them. Two of those
  stubs, `duplicate-label-policy` and `login-mismatch-policy`, were decided by the operator on 2026-11-03
  (§ 12.2); the readings recorded beside the rulings are the pipeline's.

### 14.9 Dual-lens ratification

> **Editorial note (the `/scope` orchestrator, before commit).** Absolute local paths inside the quoted lens verdicts were rewritten repo-relative (and the private design repository as `../hq/`), because they named one machine's worktree that no reader can open. Line numbers inside the verdicts’ `file:line` citations were re-resolved against `363698d` on the rebase, as this document’s header says; apart from the five `[anchor: …]` insertions that § 14.9.4's preamble declares, no other byte of any verdict changed. Two citations inside the quoted verdicts sit a line or two off the symbol they name and are left as returned: `swapDoneText` is declared at `StatusPanelFormat.swift:814`, under the doc comment the verdict cites as `:812-813`, and `rowCueBudget` occurs at `StatusPanelRoster.swift:418`, two lines above the `:420-431` block the verdict cites beside it. Two relative references the lenses wrote remain as returned: § 14.9.3's statement that its paths are relative to `.worktrees/scope-account-label-integrity`, and § 14.9.4's baseline `.tmp/design-ali/lens-ux-first.md`; both name machine-local scratch and resolve nowhere in a clone. `../hq/` names the private design repository, a repo-root sibling that no clone holds, so § 14.9.3's `../hq/strategy/design-menubar.md` resolves nowhere in a clone either, and the `.../`-elided paths in § 14.9.1's reading list are the lens's own elision of the repository root. Each verdict graded the revision it names, and the current text has moved past the quotes on several points, among them UX-10's tooltip, D-6's trace to AC-ROW-001, § 8.1's acknowledgement uuids, the per-row next-swap marker and § 11.4's authorship of the mock; where a quote and the current section disagree, the current section is the design and the quote records what was graded.

Both lenses are actors other than this document's author. Each verdict is recorded verbatim, as returned.

#### 14.9.1 Product lens, first reading — `product-strategist` — BLOCKING (baseline)

The verdict below graded the design's first draft. What the second revision changed in answer to it: § 8.1 carries the
uuids as facts (`account_uuid`, `collisions[].bearers`, `shared_with[].account_uuid`); § 8.5 marks the
written entry and states a warn's cost; § 9.1 adds UX-9, § 9.5 and § 9.6 name the uuids where the label is
ambiguous and give an ambiguous Settings row its uuid; § 11.5 restates CAP-04 and CAP-07; § 16's
REQ-STSH-B-002 and REQ-ROS-I-002 rows say what is now asserted; § 15's first finding surfaces the conflict;
§ 14.6 items 10 to 14 (item 10 since split into 10a and 10b) and § 14.8 carry the questions that return to the PRD; § 4 reorders F4 and the
`LOGIN_USAGE` edit; D-7 and D-8 say what the backfill's payoff rests on and present the invariant's reach as
an interpretation; § 1 names MET-T-004 as a proxy.

> VERDICT: BLOCKING — the design's rule in § 8.1 that "no acknowledgement carries a uuid" means one Must clause of REQ-STSH-B-002 is not met on the panel path: naming both entries by uuid where their labels are equal, on the surface that did the write. § 16 and CAP-07 still report that clause as covered.
>
> **Rationale**
>
> I graded the product axis only, against the PRD as written. I did not grade the PRD's ratification. The design says nothing will be scoped into work items until the operator ratifies (§ 14.1), which is consistent with the PRD's § 14c.
>
> What I checked:
> - The design's goals and non-goals (§ 1, § 1b) against the PRD's problem statement, scope, out-of-scope list and never-build list (PRD §§ 1–4).
> - Each of the five operator decisions in PRD § 8.1, and DQ-1, against the design's decisions (§ 5, § 12.1, § 12.2, § 14.8). I looked for any decision that settles one of them or narrows its options.
> - The design's defaults, the items marked "(default)" in § 14.8, against the operator's problem.
> - The § 16 coverage claims against the PRD § 10 wording, for every requirement whose wording sets a scope boundary.
> - Four code facts, read without changes at `d9d75ba`, because they decide how much value the design delivers:
>   - the Settings account row (`accountRow` in `apps/menubar/Sources/SettingsView.swift`);
>   - the panel capture's label field (`.../apps/menubar/Sources/AccountCaptureModel.swift`);
>   - the rollup tiers (`HourBucket` and `DayBucket` in `.../src/usage_store.rs`);
>   - who reads `Sample`: `.../src/usage_stats.rs` groups by `acct`, and `.../src/reliability.rs` joins on it.
>
> What lines up with the PRD:
> - F1 to F10 map one to one onto the PRD's ten gaps. The § 1b non-goals match the PRD's § 3, including no uniqueness enforcement (AC-6) and no automatic choice between entries that share a credential (ADR-0028).
> - No § 8.1 decision is settled, and none loses an option. Each one gets a seam that serves every option:
>   - the gate between plan and stash write (D-2) serves warn, keep and refuse;
>   - the comparison-key hook (D-1) serves every normalization option;
>   - F5 runs before the lock (D-11), which serves all three import options;
>   - report and reap share one census, and accept builds nothing (D-12);
>   - SEC-3 is advice only, and a rejection tag is added only if a refuse option is chosen (§ 8.2).
> - Narrowing DQ-1 to the wire (D-5) is within the design's remit:
>   - DQ-1 is a Stage-2 question in the PRD (§ 8.3).
>   - The uuid meets REQ-USG-B-001 for the local store, and any derived wire value can still be computed from it.
>   - The uuid tokens in the event log follow REQ-NTC-C-001's own rule.
> - Nothing is built beyond scope except the next-swap target identifier. It is flagged (§ 16b) and traces to AC-ROW-001 and #930.
> - Every requirement is bound to a section and a capability (§ 16). One row's status is wrong, as set out below.
>
> **BLOCKING CONCERNS:**
>
> 1. **The panel never names an ambiguous account by uuid, so a Must clause is dropped while § 16 reports it covered.** This concerns § 8.1, § 9.6, the REQ-STSH-B-002 row of § 16, and CAP-07 in § 11.5.
>
>    - **What the PRD requires.**
>      - REQ-STSH-B-002 (Must) says to report both entries "by label, and by account uuid where their labels are equal, on the surface that performed the write". The design answers DQ-6 with "both channels", so the socket `capture` acknowledgement and the capture card are such a surface.
>      - REQ-NTC-C-001 names an account by uuid "where the label alone is ambiguous".
>      - PRD § 9.4 gives `account_uuid` the display context "where a label is ambiguous". It separately says F4's surfaces key on the uuid "without displaying it".
>      - REQ-ROS-I-002 asks for the panel's collision report "at parity with the CLI report".
>    - **What the design builds.**
>      - `collisions[]` and `shared_with[]` carry labels only.
>      - The equal-label line is "‘work’ and another account named ‘work’ hold the same credential."
>      - The CLI collision line lists both uuids (§ 8.5), but the panel line says "‘work’ now names 2 accounts. Rename one in Settings."
>    - **Why this fails the product axis.**
>      - The design took PRD § 3's out-of-scope item and AC-WIRE-001's BUT NOT, both of which are about F4's rows and wire, and turned them into a rule over every panel notice.
>      - This settles a conflict inside the PRD without saying so: § 3 and AC-WIRE-001 point one way, REQ-NTC-C-001, REQ-STSH-B-002 and § 9.4 point the other. The design resolved it in the direction that drops a Must clause.
>      - The design's own consistency report (in § 15) met the equal-label case and resolved it by changing the wording, not by surfacing the conflict.
>      - CAP-07 promises "reports by label (uuid when ambiguous)" on all four paths. The panel path cannot meet that.
>    - **Why it matters to the operator.**
>      - The capture card takes a typed label, so a panel capture can create the collision.
>      - A Settings row shows only a label field and an enabled cue, so two `work` rows look identical.
>      - "Rename one in Settings" invites the operator to rename the wrong row. That makes a label name the wrong account, which is the harm in PRD § 2.4 that this work exists to remove.
>    - **Revision that resolves it.** Choose one of (a) or (b), and also do (c).
>      - (a) Meet the clause. Carry the bearers' uuids in `shared_with[]`, and in `collisions[]` for parity with the CLI line, and show them on the card where the label has more than one bearer. This is not § 3's "affordance to see the uuid behind each label", because § 9.4 keeps the two apart. It adds no new exposure, because the app already receives each `accountUuid` through `config-get` (D-5).
>      - (b) Keep the panel uuid-free, and record that as a deviation that goes back to the PRD:
>        - add it to § 14.8 and § 14.5;
>        - mark the REQ-STSH-B-002 and REQ-ROS-I-002 rows in § 16 as partial, and name the gap;
>        - change CAP-07 to match what the panel path asserts.
>      - (c) Whichever you choose, the panel instruction must name a way to tell the two entries apart, or be dropped. Today only `sessiometer list` can do that. How the rows are told apart is the UX lens's call.
>
> **NON-BLOCKING CONCERNS:**
>
> 1. **Under the design's own D-8 default, nothing reads the backfill's output before it ages out** (D-7, D-8, § 14.6 item 8). This is a dispute about product scope, and it goes back to the PRD.
>    - Rollups carry no account, so per-account history exists only in the 14-day raw tier.
>    - The only readers of samples, `stats` and `reliability`, group by label, and D-8 keeps them that way.
>    - New samples carry the key anyway. After 14 days the raw tier is fully keyed through normal turnover, with no backfill.
>    - So the backfill pays off only if something that reads the key lands within 14 days of the key itself. Meanwhile it is a one-way rewrite of a file the prior-configuration witness reads (REQ-USG-C-002, RK-11, RK-17).
>    - Recommendation: state this coupling in `stats-consumes-the-key`. Then either commit a reader of the key in the same window, or drop the backfill.
>    - Whether dropping it breaks AC-USG-001 depends on the reading. If "re-keyed" means the moment readers switch to the key, a naturally turned-over tier already satisfies AC-USG-001. Under the design's reading it does not. The PRD has to settle which reading governs.
>
> 2. **D-8 presents one reading of the F4 invariant as fact.** It says relabel continuity in `stats` is something the invariant "forbids".
>    - The PRD names "a relabel starts a new history handle" as a harm (§ 2.6, A-9). Continuity across a relabel is the purpose of REQ-USG-B-001.
>    - MET-T-004 states the same constraint over its four named surfaces, and `stats` output is not one of them.
>    - Present this as an interpretation the operator can weigh, not as a fact.
>
> 3. **MET-T-004 will show the usage store as fixed while the problem the operator sees remains.** `stats`, `reliability` and the panel's stats view will still merge two accounts that share a label, and still split one account that was relabelled. That is a proxy in the PRD's own metric, so it goes back to the PRD. The design should note it where § 1 adopts the PRD's measures.
>
> 4. **Ruling on the question the design put to this lens: the "in rotation" count (REQ-NTC-B-001) against the F4 invariant. The design's resolution is right.**
>    - Read with MET-T-004, the invariant covers the identity-keying surfaces.
>    - AC-NTC-001 requires the text to change on a roster that has a parked entry, so the specific requirement governs.
>    - Counting enabled entries changes the reply only where today's text is false.
>    - REQ-NTC-B-001 is a Could and sits outside the identity theme, so cut it first if the circuit breaker trips.
>
> 5. **F4's delivery order starts with the item that waits on an operator decision** (§ 4, § 12b).
>    - The wire-and-panel tracer bullet cannot start until `account-key-form` (the operator's DQ-1 decision) is made.
>    - The event-line item and the key on new samples do not depend on that decision (D-5).
>    - The app-side fallback (UX-4: identify rows by position plus label) fixes the visible duplicate-row defect (A-5, AC-ROW-001) with no wire change.
>    - Let those items start first.
>
> 6. **F9's `LOGIN_USAGE` edit is scheduled for "any time", but it depends on `login-mismatch-policy`.** REQ-DOC-B-003 hard-codes "renames". If the operator chooses to refuse a renaming revive, that text becomes wrong. Land the edit after that decision, or word it to match the policy. That avoids a second edit that would also need coordinating with #1379.
>
> 7. **The CLI collision notice could be more useful** (§ 8.5).
>    - It does not mark which of the two uuids the write touched.
>    - Under the warn option, it does not say that commands resolving `work` will refuse with exit 6 until one entry is renamed. That is the main consequence of a warn (PRD § 8.1).
>    - Both fixes are wording.
>
> 8. **D-6's next-swap target identifier goes beyond REQ-WIRE-I-001's wording.** It is properly flagged (§ 16b). Accept it, and ratify it together with the wire form.
>
> 9. **An observation about the PRD itself, which goes back to the PRD.** Its "Serves" line says "the operator can always tell which account a label names". No requirement delivers that on the panel, because § 3 keeps the identifier off-screen. The operator should accept or narrow that line when ratifying.
>
> **What I did not check.** I did not check feasibility, the testing track, or the security track beyond the facts above. I did not check the UX rendering of any surface, which the UX lens owns. I did not check whether the design's other code citations still match the tree.

#### 14.9.2 Product lens, re-grade of the second revision — `product-strategist` — CONFIRM

It graded the second revision, with 14.9.1 as its baseline. The third revision (14.9.3) also answers its
non-blocking concerns: D-3's rule now governs the shared line (D-4, § 8.5, § 9.5, TF-1, CAP-07, with the
one-shared-side fixture); § 1b, § 11.1 and the `account-key-form` stub state where the uuid is displayed;
§ 14.6 splits item 10 into 10a and 10b and records the parity cost; D-8's harm sentence is corrected; § 14.2
agrees with § 4; § 4 names REQ-NTC-B-001 as the first cut; the card and the Settings note state a warn's
cost; `panel-switches-by-identifier` is recorded beside `account-key-form` (item 15). The product lens has
not graded the third revision. Two grounds the quoted CONFIRM states are superseded by that revision: the
Settings row's uuid line now waits on `settings-row-shows-uuid` (UX-9), and § 16b flags a PHANTOM beside
the two items the verdict names.

> VERDICT: CONFIRM
> BASELINE BLOCKING CONCERN: resolved
>
> **Rationale**
>
> I graded the product axis only, against the PRD as written. I graded the revised document's own text and gave credit only for changes it actually contains.
>
> **What I checked**
> - The baseline verdict, and each change § 14.9.1 lists, against the text it points to. Every listed change is in the text, with one exception: § 9.5 names uuids on the shared-credential line only where the two labels are equal (see `shared-line-names-uuid-only-on-equal-labels`).
> - The revised design in full: `docs/design/account-label-integrity-solution-design.md`.
> - The PRD: `docs/requirements/account-label-integrity.md`. For this verdict I re-read § 2.6, § 3, the Serves line in § 4, § 8.1, § 9.3, § 9.4, and the § 10 and § 12 rows cited below.
> - The operator's GUI-parity ruling of 2026-07-31 (§ 1.3 item 3) and its "same information" boundary (§ 1b): `docs/requirements/gui-cli-capability-parity.md`.
> - Code, read without changes at `d9d75ba`:
>   - `apps/menubar/Sources/SettingsView.swift`, line 199 and `accountRow`. Each Settings row is already keyed by `accountUuid`. It shows only a label field and an enabled cue.
>   - `apps/menubar/Sources/ConfigWire.swift`, `AccountView.accountUuid`. The app already holds every uuid.
>   - `apps/menubar/Sources/StatusPanelRoster.swift`, line 319. A row's switch sends `row.label`.
>   - `src/daemon/socket.rs`. The `swap` target is `"<label|uuid>"`, and `SwapRejection::AmbiguousTarget` refuses a duplicated label ("the daemon never guesses").
>   - `src/daemon/commands.rs`. The `swap` handler resolves its target with `resolve_target`.
>   - `src/use_account.rs`, `resolve_target`. It matches a label or a uuid, and refuses a target that matches more than one entry.
>   - `apps/menubar/Sources/StatusPanelFormat.swift`, line 834. The panel's refusal reads "Two accounts share that label — rename one, then switch."
>
> **The block is resolved**
> - **The Must clause is met on the panel path.**
>   - The acknowledgements carry the uuids as facts (§ 8.1): `account_uuid` on `captured` and `refreshed`, `collisions[].bearers`, and `shared_with[].account_uuid` (since 2026-11-03, § 12.3 C-12: `account_uuid` and `shared_with[].account_uuid` on the capture acknowledgement; `collisions[].bearers` on `config-set`'s refusal instead).
>   - The card's collision line and its equal-label shared line name those uuids, and mark the written entry "(this one)" (§ 9.6).
>   - The CLI marks the written entry too (§ 8.5).
> - **Option (c) is met.** The card says "Rename one in Settings", and Settings now shows each ambiguous row's uuid under its label (UX-9, § 9.5, § 9.6). The operator can therefore match the card's uuid to a Settings row, with no wire change.
> - **The coverage claims are accurate.** § 16's REQ-STSH-B-002 and REQ-ROS-I-002 rows, CAP-04 and CAP-07 now say what the design asserts.
> - **The PRD-internal conflict is surfaced, not settled.**
>   - § 14.6 item 10 states both readings.
>   - § 14.5, § 14.8, § 12.1, § 12b, § 9.7 and § 15's first finding carry the conflict.
>   - Items 10 and 11 hold the design at `draft`, together with items 1 to 7.
>
> **Why the Settings row line does not block, though the design calls it "the narrowest form of the affordance § 3 deferred"**
> - PRD § 9.4 lists "where a label is ambiguous" as a place where a roster entry's `account_uuid` is displayed. The § 3 item is an affordance to see the uuid "behind each label". A uuid shown only while a label is ambiguous falls on § 9.4's side.
> - Any design has to take one reading of this conflict. The baseline offered this reading, and it left the means for (c) to the UX lens.
> - The choice can be reversed, and it is visible. Its fallback is recorded in § 14.5. The draft hold stops anyone building it before the operator answers.
>
> **No other decision forecloses an operator choice**
> - All six § 12.2 stubs still read "Decision. Open".
> - Under a refusal answer to `duplicate-label-policy`, the `Error` template replaces § 8.5's collision line, so that line works under both answers.
> - The `LOGIN_USAGE` edit waits for `login-mismatch-policy` (§ 4, D-13).
> - UX-9 changes the facts behind `account-key-form` but does not decide it (see `uuid-display-statements-stale`).
>
> **Scope.** The design builds nothing beyond the PRD except two flagged items: the next-swap identifier (§ 16b) and the Settings row line (§ 14.6 item 10).
>
> **What I did not check**
> - Feasibility.
> - The testing and security tracks, beyond the facts above.
> - How any surface renders, which belongs to the UX lens.
> - Code citations other than those listed.
> - How the parity PRD classifies `list`.
> - Whether Settings lists rows in the same roster order as the panel.
> - The brief file.
>
> BLOCKING CONCERNS: none
>
> NON-BLOCKING CONCERNS:
>
> 1. **`shared-line-names-uuid-only-on-equal-labels`** — the shared-credential line follows a narrower rule than D-3. This affects D-4, § 8.5, § 9.5, TF-1 in § 9.3, and CAP-07 in § 11.5. It is worth fixing before the operator ratifies.
>    - **D-3's rule.** D-3 puts a uuid beside any label that has more than one bearer. It says "That rule binds every rendered line: the CLI reply, the capture card, the Settings note and the event line". § 8.4 applies that rule to the event line.
>    - **The narrower rule.** D-4, § 8.5 ("The shared line names uuids only where the two labels are equal"), § 9.5's two card rows, TF-1 and CAP-07 add uuids only where the labels are equal.
>    - **Where they differ.** The two rules differ when the labels differ and one of them has two bearers.
>      - Example: entries `work` (A) and `work` (B), then a capture of `spare` whose credential matches B's.
>      - D-3, § 8.4 and REQ-NTC-C-001 give `"spare" and "work" <uuid-B> hold the same credential.` REQ-NTC-C-001 is a Must: the uuid goes "where the label alone is ambiguous".
>      - § 8.5, § 9.5 and CAP-07 give `"spare" and "work" hold the same credential.` That line does not say which `work`.
>    - **Further contradictions.**
>      - D-4 contradicts § 8.4 on the event line.
>      - D-4 and CAP-07 contradict § 8.1 on the acknowledgement, which always carries `shared_with[].account_uuid`.
>      - UX-9's rationale says REQ-NTC-C-001 names the uuid "where labels are equal". Its text says "where the label alone is ambiguous".
>    - **Fix.** Use D-3's rule in D-4, § 8.5, § 9.5, TF-1 and CAP-07; it already covers the equal-label case. Add a CAP-07 fixture for the example above.
>    - **Why it is not blocking.** D-3, the governing rule, is right. The facts reach the app and the event log. REQ-STSH-B-002's equal-label clause, the baseline's block, is met.
>
> 2. **`uuid-display-statements-stale`** — three passages still describe a panel that never shows a uuid.
>    - **§ 1b.** It lists "Displaying the account identifier to the operator in the panel" as a non-goal and calls a display affordance deferred.
>      - Under UX-9, the card and Settings show the uuid beside an ambiguous label.
>      - If `account-key-form` picks the uuid, that uuid is the account identifier.
>      - Narrow the non-goal to F4's rows and `status`'s rendered output, and point to UX-9 and § 14.6 item 10.
>    - **§ 11.1's data table.** It lists where the uuid may appear. It names CLI notices and event tokens, but not the card, the Settings note or the Settings row.
>    - **The `account-key-form` stub in § 12.2.** It says only that the app "receives" uuids through `config-get`. Under item 10's default, the app also displays the raw uuid.
>      - A derived wire value would then hide the uuid only from pasted `status --json`, as SEC-4 already says.
>      - The stub's "no correlation with `list`" would also apply to the card and Settings.
>      - The operator decides DQ-1 from this stub, so it should state both facts.
>
> 3. **`item-10-bundles-two-decisions`** — § 14.6 item 10 puts two choices behind one either-or question, and it leaves out an option and a cost.
>    - **Split it into two questions.**
>      - `panel-notices-name-the-uuid`: do panel notices name a uuid beside an ambiguous label?
>      - `settings-rows-told-apart`: how does Settings tell two same-label rows apart?
>    - **An unrecorded option.** `settings-rows-told-apart` has an option not on record: show the panel's ordinal (UX-3) on Settings' ambiguous rows, and have the card name it.
>      - The ordinal is uuid-free, so under the first reading, where § 3 governs, the "Rename one in Settings" instruction need not be dropped.
>      - So item 10's "cannot then be told apart on the panel" overstates that reading's cost.
>    - **An unrecorded cost of the first reading.**
>      - With no cue in Settings, telling two `work` accounts apart becomes CLI-only (`sessiometer list`).
>      - The operator ruled on 2026-07-31 that "Choosing a surface is not accepting reduced capability". The parity PRD's § 1b says the GUI must reach the same information.
>      - That reading also needs REQ-NTC-C-001's uuid clause narrowed for the panel, not only REQ-STSH-B-002's.
>    - **The default-on-silence wording.**
>      - Item 10 says "If unanswered, the design's reading stands".
>      - The design itself calls the Settings line a form of the affordance § 3 deferred, and § 3 reopens that only when "The operator adds it".
>      - The draft hold already prevents building on silence. Reword item 10 to say that `settings-rows-told-apart` waits for an explicit answer.
>
> 4. **`panel-ordinal-and-settings-uuid-share-no-cue`** — the panel tells duplicate rows apart by ordinal (UX-3); the card and Settings tell them apart by uuid (UX-9).
>    - The panel shows no uuid, and Settings shows no ordinal.
>    - An operator who wants to rename the account behind the panel row `work` 2 cannot tell which Settings row that is.
>    - Renaming the wrong one makes `work` name an account they did not mean. That is a smaller form of the PRD § 2.4 harm.
>    - § 12b says ordinals follow roster order, so a shared cue looks cheap. Which cue to use is the UX lens's call.
>
> 5. **`panel-swap-on-a-duplicate-label`** — two parts.
>    - **Inside the design: a parity gap.**
>      - § 8.5's CLI collision line says commands that take the label refuse it until one entry is renamed.
>      - The card's collision line and the Settings note in § 9.6 do not say this.
>      - The cost is real on the panel too: a row's switch sends the label, and the daemon refuses an ambiguous one.
>      - REQ-ROS-I-002 asks for parity with the CLI report. Add the cost to both lines; it is a wording change.
>    - **A gap in the PRD, which goes back to the PRD.**
>      - UX-3 makes two `work` rows look as if each can be acted on, yet neither can be switched to until one is renamed.
>      - PRD § 2 lists "swap to it" among the acts where the cost lands. PRD § 9.3 has no swap action, and neither document mentions the panel switch on a duplicate label.
>      - One option for the operator: the panel switches by identifier. The socket's `swap` already resolves a uuid target through `resolve_target`, so a uuid identifier needs no daemon change. A derived identifier would need one.
>      - Record this beside `account-key-form` so the operator weighs the two together.
>
> 6. **`d8-inverts-the-relabel-harm`** — D-8 says that mapping the key to the current label "would carry history across a relabel, which PRD § 2.6 and A-9 name as the harm".
>    - The PRD names the opposite harm: "a relabel starts a new history handle" (§ 2.6; A-9).
>    - Carrying history across a relabel is what REQ-USG-B-001 asks for ("unchanged by a relabel").
>    - The default does not change, and § 14.6 item 8 frames the choice neutrally. But this sentence argues against the continuity option from an inverted premise.
>    - Reword it: continuity is the PRD's remedy, and its cost is that `stats` output changes after a relabel.
>
> 7. **`tracer-bullet-sentence-contradicts-delivery-order`** — § 14.2 still recommends "the first increment of F4" as the tracer bullet.
>    - Its tracer bullet carries the identifier from the snapshot through to the row identity and a mock frame.
>    - § 4 now puts that item third: after the items that wait on no decision, and only once `account-key-form` is decided.
>    - Align § 14.2 with § 4.
>
> 8. **`count-cut-first-advice-not-carried`** — the baseline advised cutting REQ-NTC-B-001, a Could, first if the circuit breaker trips.
>    - § 4 does not place it. It rides with D-3's templates, so it lands with F1 to F3, the second group in § 4's order.
>    - The cut order is the delivery order reversed, so REQ-NTC-B-001 would be cut among the last.
>    - Name it in § 4 as the first cut, or say why not.
>
> BASELINE NON-BLOCKING CONCERNS:
> 1: recorded (D-7, § 14.6 item 11 and a § 14.5 row, returned to the PRD; § 4 also makes the backfill F4's last item, dropped if the turnover reading governs, and § 16's REQ-USG-C-001 row names the reading it rests on)
> 2: addressed by revision (D-8 presents the invariant's reach in `stats` as an interpretation and defers to § 14.6 item 8; one D-8 sentence still inverts the PRD's harm, see `d8-inverts-the-relabel-harm`)
> 3: addressed by revision (§ 1 names MET-T-004 a proxy; returned to the PRD as § 14.6 item 12, with a § 14.5 row)
> 4: recorded (§ 14.8 and § 15's second finding record the ruling, which confirmed the design, so no revision was owed; the cut-first advice did not reach § 4, see `count-cut-first-advice-not-carried`)
> 5: addressed by revision (§ 4 starts F4 with the items that wait on no decision, then the UX-4 fallback; RK-08 and § 12b agree; § 14.2 still contradicts it, see `tracer-bullet-sentence-contradicts-delivery-order`)
> 6: addressed by revision (§ 4 and D-13 land the `LOGIN_USAGE` edit after `login-mismatch-policy` or word it to that answer, coordinated with #1379; § 14.6 item 14 records why)
> 7: addressed by revision (§ 8.5's collision line marks "(this one)" and states the warn's cost; the panel's lines carry the marker but not the cost, see `panel-swap-on-a-duplicate-label`)
> 8: recorded (§ 14.8 and § 16b: ratified together with `account-key-form`; D-6 states its trace to AC-ROW-001 and #930)
> 9: recorded (§ 14.6 item 13 and a § 14.5 row, returned to the PRD)

#### 14.9.3 UX lens, first reading — `ux-architect` — BLOCKING (baseline)

An earlier dispatch was killed by a usage limit before it returned anything; nothing of it is recorded.
The verdict below graded the second revision. What the third revision changed in answer to it: UX-5 drops
the per-row marker, and the footer callout carries the target's ordinal, found by the target's identifier;
UX-10 shows a shared-label switch blocked before the click, with switching by identifier recorded as § 14.6
item 15; UX-4 keys the switching state on row identity; UX-3 makes the ordinal the duplicate set's one
handle on every surface, Settings rows, the header and a swap result included; UX-2 exempts a card with a
notice from its reset and states what clears it; UX-11 derives the Settings note from state; UX-8 gives a
duplicate Settings field its ordinal in its accessible label; the Settings row's uuid moves behind an
explicit answer (`settings-row-shows-uuid`); § 9.2, § 9.3, § 9.5 and § 9.6 follow, with the "Error —
permission" rows added; § 9 intro and § 11.4 correct the mock's Settings authority and name #946; the card's
lines wrap, take an attention treatment and fold the relabel into the outcome line; the CLI line names a
rename path; § 14.7 records the stale `CLAUDE.md` line.

> VERDICT: BLOCKING — when two accounts share a label, the next-swap callout is ambiguous and the panel's swap actions are refused, because the design relies on a per-row marker that the ratified R-2 decision removed; the card's "(this one)" is lost on the way to Settings, the route the card itself sends the operator; and duplicate Settings rows have no spoken distinction
>
> **Scope and method.** I graded the revised design (file modified 2026-10-26 21:22 CET) on the UX axis only, against the PRD as written. Repository paths below are relative to `.worktrees/scope-account-label-integrity`. The private HQ record I cite is `../hq/strategy/design-menubar.md`, and the parts that matter are quoted in-band. I read:
> - **Design:** all of § 9, plus D-5, D-6, § 6, § 8.5, § 11.4, § 11.5 (CAP-04, CAP-11), § 12b, § 14.6, § 14.9, and the § 16b row for the next-swap identifier.
> - **PRD:** § 2.4, § 3, § 8.3 (DQ-4), § 9.4, § 10.4 (REQ-ROW-*, AC-ROW-001, AC-WIRE-001), REQ-ROS-I-002, REQ-ROS-B-002, REQ-STSH-B-002, REQ-NTC-C-001, and the § 11.2–§ 11.4 state matrices.
> - **Mock:** `menubar-preview.html` and the README register.
> - **Code:** the Swift the new states land in (panel rows, callout, swap model, capture card and its model, status-item controller, Settings view and model). Also the Rust that resolves a swap target and sets roster order.
>
> **What holds up.**
> - **UX-1:** notices are inline on the surface that made the write, never a toast or a modal.
> - **UX-3 on panel rows:** the ordinal, the colour slot and the spoken cue meet WCAG 1.4.1. A roster without a duplicate renders unchanged.
> - **UX-4, UX-6 and UX-7:** UX-4 keys rows on identity and falls back to position for an older daemon. UX-6 waits on #1474. UX-7 reuses the existing refusal states.
> - **UX-9's direction:** a way to tell rows apart in Settings that appears only while a label is ambiguous.
> - **No new screen or call to action.** The § 9.5 matrices cover every applicable state that PRD § 11.2–§ 11.4 names.
> - **The six § 12.2 stubs stay open,** as required.
> - **D-6's `to_account_uuid` is justified,** but by the callout and the swap, not by a row marker (first concern).
> - **§ 9.6's code anchors all match the tree:** `HonestStateMachine.swift:363`, `:445`, `StatusPanelFormat.swift:2755`, `:2784` and `SettingsView.swift:199`.
>
> **What fails** is the three concerns below. For each, I looked for what would disprove it: a view that renders the row marker, a swap that sends something other than the label, a route to Settings that keeps the panel open, or a spoken distinction specified for Settings rows. None exists.
>
> **What I did not check.**
> - I did not build or run the app. The VoiceOver behaviour in the third concern is read from the SwiftUI code and the spec, not heard. A VoiceOver pass, or an Accessibility Inspector reading, over a Settings window holding two `work` rows would settle it.
> - I did not grade feasibility, the test plan beyond CAP-11, or security. On the Rust side I checked only how a swap target is resolved and how the roster is ordered.
> - I read the product lens's verdict only to see what UX-9 answers.
>
> **Outside this design's goal (one line for its owner):** the repo's `CLAUDE.md` (§ The design mock) says the Settings window is outside the mock "entirely". `apps/menubar/design/README.md:1275` says the Settings window "IS authored, as Group 8 (#763)". § 11.4 copied the stale line.
>
> BLOCKING CONCERNS:
>
> 1. **`next-swap-and-switch-break-on-a-shared-label`**
>
>    *Concerns:* § 9.1 UX-5; § 9.2 row 1; § 9.3 TF-3; § 9.5 panel list (the hierarchy, the "next-swap target … marker" field and the matrix); § 9.6 implementation notes; § 9.7 and § 16b "Next-swap marker by identifier"; § 11.5 CAP-11 ("the marker"); D-6's rationale.
>
>    *What the design says.* It answers DQ-4's next-swap question with a per-row marker:
>    - UX-5: "The next-swap marker matches the target's identifier … the callout still names the label".
>    - TF-3: "the next-swap marker sits on the identified target only".
>    - § 9.5 puts the marker inside `AccountRow`.
>
>    *Why that does not work.*
>    - **The panel has no per-row marker, by ratified decision.** The private HQ record's R-2 annotation of 2026-07-09 says: "the per-row `→ next` marker is DROPPED from the panel (the CLI has none — the footer is next-swap's one home)".
>    - The code agrees. `StatusPanelFormat.swift:2172` says "Next-swap is NOT per-row (R-2 re-ratified 2026-07-09)". `isNextSwapTarget` is computed (`HonestStateMachine.swift:445`), but no view reads it; only render-harness fixtures and tests do.
>    - The mock shows next-swap only in the footer `.swap-row`, never on a row.
>    - **The callout stays ambiguous.** It is next-swap's only surface, and it still reads "→ work" and speaks "Next swap to work." (`swapCalloutAccessibilityLabel`, `StatusPanelFormat.swift:2160`) while two rows are named `work`. Neither a sighted user nor a VoiceOver user can tell which account is meant. DQ-4 names exactly this callout.
>    - **Both swap actions are refused.**
>      - The footer Swap button sends the label (`StatusPanelChrome.swift:228`), and so does the per-row switch (`StatusPanelRoster.swift:319`).
>      - `resolve_target` "never guesses" (`src/use_account.rs:486`), so the daemon answers `SwapRejection::AmbiguousTarget` (`src/daemon/commands.rs:114`).
>      - The operator learns this only after clicking: "Two accounts share that label — rename one, then switch." (`StatusPanelFormat.swift:834`).
>    - **This breaks the panel's own rule:** disable only what the wire proves the daemon will refuse (`SwitchBlock`, `StatusPanelFormat.swift:170-185`). A shared label is provable from the wire.
>    - **The switching state is keyed on the label.** The spinner (`StatusPanelRoster.swift:256`, `StatusPanelChrome.swift:193`) and the hint "Switch to work" use the label, so both duplicate rows would spin and read alike.
>    - **Nothing in § 9.2, TF-3 or the matrix mentions the swap actions,** so this path is unspecified rather than an accepted risk.
>
>    *Revision.*
>    - **(a)** Remove the row marker from every place listed above. Restate D-6: `to_account_uuid` exists to disambiguate the callout and to address the swap.
>    - **(b)** Specify the callout for a shared-label target. It shows the target's ordinal the way the row does and speaks it ("Next swap to work, 2 of 2 accounts named work"). It finds the target row by `to_account_uuid`. With an older daemon, it says "one of 2 accounts named work", and its Swap button is blocked as in (c-ii).
>    - **(c)** Specify the swap for a shared-label row or target, choosing one option:
>      - **(c-i)** Send the identifier as the swap `target`. `resolve_target` already accepts a uuid, and nothing is displayed, so AC-WIRE-001's BUT NOT still holds. Record in the `account-key-form` stub, without deciding it, that a derived key would need `swap` to resolve that key.
>      - **(c-ii)** Give those rows, and the callout's Swap button, the existing blocked treatment, with the reason and the remedy shown before the click. This is the only option with an older daemon.
>    - **(d)** Key the switching state on row identity. For a duplicate, put the ordinal into the hint, the tooltip and the Swap button's accessible label.
>    - **(e)** Add the swap action to § 9.2's key actions, to TF-3 with its refusal path, and to the § 9.5 matrix.
>
> 2. **`this-one-is-lost-on-the-way-to-settings`**
>
>    *Concerns:* § 9.1 UX-2, UX-3, UX-9; § 9.3, where TF-1 hands off to TF-2; § 9.4; § 9.5 capture card and Settings section; § 9.6.
>
>    *What the design says.* Only the card's collision line says which account the write touched: "‘work’ now names 2 accounts: <uuid-B> (this one) and <uuid-A>. Rename one in Settings." UX-9's rationale itself says this instruction, over two identical rows, "invites renaming the wrong account (the PRD § 2.4 harm)".
>
>    *Why that does not work.*
>    - **Following the instruction destroys the card.**
>      - Settings opens only from the status item's secondary-click menu (`StatusItemChrome.swift:168`; `StatusItemController.swift:225,252`).
>      - That menu closes the panel first (`StatusItemController.swift:208`).
>      - Every close calls `dismissCaptureSurface()` (`:345`), which runs `cancelEditing()` and returns the card to `.idle` (`AccountCaptureModel.swift:120-124,138-141`).
>    - **The card also clears itself.** Today's `done` state resets after 1.8 s (`scheduleIdleReset`, `AccountCaptureModel.swift:188`). UX-2 would have to override that reset, and the design never says so.
>    - **UX-2 cannot hold on this route.** Its rule, "a notice stays until the operator's next action on that surface", fails on the very route the notice prescribes.
>    - **Capturing again does not bring the line back.** REQ-ROS-B-002 counts a collision only when the count rises.
>    - **Settings cannot tell the operator which row is theirs.**
>      - Both rows show "Account", a "Label" field holding `work`, and "Active" (`SettingsView.swift:228-241`; `SettingsModel.swift:626,700,703`).
>      - "Active" there means enabled, not signed in.
>      - UX-9 adds only each row's uuid. To pick "this one", the operator must remember a 36-character string from a card that no longer exists.
>    - **The surfaces use unrelated handles.** The panel tells the two accounts apart by ordinal (UX-3), and Settings by uuid (UX-9).
>    - **The Settings distinction depends on the identifier.** It exists only because the identifier is displayed. That is what the lens question asks to avoid, and what the first reading of § 14.6 item 10 would remove.
>
>    *Revision.*
>    - **(a)** Make the ordinal the one handle for a duplicate set on every surface. Show it:
>      - on panel rows;
>      - in the card's collision line and equal-label shared line;
>      - in the callout;
>      - beside each duplicate Settings row's field. Keep the uuid under the field while item 10 stands.
>
>      Example card line: "‘work’ now names 2 accounts: this one is work 2 of 2 (<uuid-B>); the other is work 1 of 2 (<uuid-A>). Rename one in Settings."
>
>      Why the ordinal works:
>      - It needs no wire change. The status frame and `config-get` both list accounts in roster order: `src/daemon/snapshot_build.rs:417` iterates `self.roster` with no sort, and `view()` in `src/config/settings.rs` maps the roster in order.
>      - § 12b already records that ordinals move only if the roster is reordered.
>      - It still works under either answer to item 10.
>      - It also lets the panel's active row point to the right Settings row.
>    - **(b)** State in UX-2 that a `done` card carrying a notice is exempt from the 1.8 s reset. Define the card's "next action": a new capture, Esc, or a dismiss. Say whether a panel close counts. If it does, the ordinal in (a) carries the fact. If it does not, `dismissCaptureSurface` must keep a card that carries a notice.
>
> 3. **`settings-duplicate-rows-sound-identical`**
>
>    *Concerns:* § 9.1 UX-8; § 9.5 Settings section; § 11.4.
>
>    *What the design says.* § 11.4 says "the row's spoken distinction stays the ordinal, never the uuid", and that the Settings window follows "the existing behaviour". UX-8 puts the ordinal into the accessible names of panel rows only, and § 9.5 gives Settings rows no ordinal. So the spec gives duplicate Settings rows no spoken distinction at all.
>
>    *Why that fails.*
>    - Under the existing behaviour, two enabled `work` rows have the same row title, field name, value and cue (`SettingsView.swift:228-241`).
>    - The new uuid line is "secondary read-only text under the label field", with nothing tying it to its field. A VoiceOver user tabbing between the fields would hear the same announcement twice. The VoiceOver cursor reaches the uuid only as a separate element after the field, spelled out character by character.
>    - This is an accessibility failure on the new element whose only job is to tell the rows apart:
>      - WCAG 2.4.6: two fields have one label but different purposes.
>      - WCAG 1.3.1: the tie between a uuid and its field is visual only.
>      - It also falls short of the design's own UX-8 standard.
>    - The exact announcement is inferred, not heard (see "What I did not check").
>
>    *Revision.*
>    - While a label has more than one bearer, put the distinction into each duplicate field's accessible label or value. Use the ordinal from the second concern, for example "Label, work, 2 of 2 accounts named work".
>    - While item 10 stands, add a short uuid prefix, not the full uuid, and expose the full uuid as the field's hint.
>    - Rows without a duplicate keep today's names.
>    - State this in UX-8 and § 11.4, and narrow "never the uuid" to "never the full uuid" for Settings.
>
> NON-BLOCKING CONCERNS:
>
> - **Mock authority for Settings.** § 11.4 says the Settings window is outside the mock's authority. That contradicts the § 9 intro and `apps/menubar/design/README.md:1275`. Only the Accounts section's row anatomy is open (README:1285; register R-11, routed to #946; its prose home is the private HQ's D-UX-SETTINGS). Name #946 as the prerequisite for drawing the uuid line, the ordinal and the note in the mock before Swift builds them.
> - **Missing matrix rows.** The § 9.5 list and Settings matrices omit the PRD's "Error — permission" row, which is N/A in PRD § 11.3 and § 11.4. Add it. In the Settings matrix, use it to place `label-in-use` (refuse policy only), so UX-7 has a row.
> - **Card layout.** Today's `done` line is green, a single line, and truncated (`StatusPanelCapture.swift:103-108`). Specify that:
>   - notice lines wrap, as the failed line does;
>   - no uuid loses characters to truncation;
>   - a collision gets a warning treatment, not the green success check;
>   - the Settings uuid line beside the 160 pt field (`accountLabelFieldWidth`) wraps the same way.
> - **Spoken length.** Under UX-8 (WCAG 4.1.3), a collision line is announced automatically with two full uuids spelled out. Announce the ordinal and a short prefix, and leave the full line for on-demand reading. § 12b names this drawback but gives no mitigation.
> - **Relabel wording.** The card would show "Captured ‘work’" and then "Refreshed ‘work’ (was ‘spare’).", which is two verbs for one write. Fold the relabel into the outcome line, as the CLI's single confirmation line does (§ 8.5). Order the lines outcome, collision, shared.
> - **Parity of cost (REQ-ROS-I-002).** The CLI line says what a warn costs; the card line does not. Once the first concern picks an option, state the panel's cost under (c-ii): "switching to either stays unavailable until one is renamed". Under (c-i), there is no panel cost to state.
> - **CLI remedy (§ 8.5).** "Until one is renamed" names no rename path the CLI can run. `ROOT_USAGE` in `src/cli.rs` has no rename verb, and Linux has no Settings window.
>   - Name `sessiometer capture <new-label>`, run while signed in to that account, as the way to rename it. That relabel is pinned by `recapture_updates_the_label_when_a_new_one_is_given` (PRD § 2.4), and it is subject to `login-mismatch-policy` and `duplicate-label-policy`.
>   - Add that `sessiometer use <uuid>` still works, because `resolve_target` matches a uuid.
> - **RK-22, ordinal badge.** Draw the ordinal as a badge set apart from the label text, so "work 2" is not read as a label to type. Add a tooltip that explains the duplicate and the remedy.
> - **UX-3 colour tie-break.** Prefer a slot no other row uses, so a duplicate's colour cannot match a third label (the other half of RK-22).
> - **Other label-only strings.** These become ambiguous with a duplicate; add the ordinal to both:
>   - the header line that names the active account (private HQ #699 annotation: "3 accounts · Work active");
>   - the swap result "Switched work → work" (`swapDoneText`, `StatusPanelFormat.swift:814`).
> - **Stats tab.** It merges same-label accounts right next to two Status rows (§ 12b; § 14.6 item 12). Mark a label held by more than one account in the app, which already knows this from the status frame, without changing the `stats` wire format.
> - **Settings note.** Derive the collision note from state: show it whenever a label has more than one bearer, not only after a save returns `applied.collisions`. That way the uuid lines that appear after a panel capture, a `login` or an `import` come with an explanation, and an unrelated edit does not clear the note while the collision remains.

#### 14.9.4 UX lens, re-grade of the third revision — `ux-architect` — CONFIRM

It graded the third revision, with 14.9.3 as its baseline, and found every baseline blocking concern
resolved. Five of its source citations carry an editorial anchor in square brackets, `[anchor: …]`: a
symbol that sits at the cited lines, added so the repository's citation-rot gate
(`scripts/check-doc-citation-rot.sh`) can re-derive each range. The verdict's own words are unchanged.

> VERDICT: CONFIRM
>
> BASELINE BLOCKING CONCERNS: `next-swap-and-switch-break-on-a-shared-label`: resolved — no per-row next-swap marker remains, the footer callout names a shared-label target by its ordinal (found by the target's identifier, with older-daemon text), both swap actions are shown blocked before the click with reason and remedy (option c-ii, with c-i recorded as § 14.6 item 15), the switching state keys on row identity, and the swap action is now in § 9.2, TF-3 and the § 9.5 matrix; `this-one-is-lost-on-the-way-to-settings`: resolved — the ordinal is the duplicate set's one handle on every surface (card lines, header, rows, callout, Settings rows and note), UX-2 exempts a card with a notice from the 1.8 s reset and counts the panel's close as its next action, and the ordinal carries "this one" across that close because every surface lists the roster in the same order; `settings-duplicate-rows-sound-identical`: resolved — UX-8 and § 11.4 put the ordinal into each duplicate Settings field's accessible label, and UX-9 ties any uuid line to its row's accessibility container, spoken as a short prefix with the full uuid as the hint.
>
> **Scope and method.** I graded the third revision (file modified 2026-10-26 22:03 CET) on the UX axis only, against the PRD as written. I credited only what the document's own text carries. Repository paths below are relative to `.worktrees/scope-account-label-integrity`, and the baseline is `.tmp/design-ali/lens-ux-first.md` there. I read these sources:
> - **Design:** § 9 in full; § 6; § 8.1, § 8.3 and § 8.5; § 11.3–§ 11.5 (CAP-04, CAP-11); § 12.1; § 12b; D-3; D-6; the `account-key-form` stub; § 14.6; § 14.7; § 14.9.3.
> - **PRD:** § 2, § 3, § 9.4 and the § 11.2–§ 11.4 matrices.
> - **Parity PRD:** § 1.3, § 1b and R-1 to R-3.
> - **Mock:** its callout variants, and the README's Settings register.
> - **Code:** the Swift and Rust each new state lands in.
>
> For each baseline concern I looked for what would disprove "resolved":
> - a row marker that survives;
> - a swap path left unspecified;
> - a route to Settings that loses the ordinal;
> - a Settings row with no spoken distinction.
>
> None exists.
>
> **First baseline concern.**
> - Outside the quoted verdicts in § 14.9, every "marker" in the document says the marker is absent: § 9.5, § 9.6, D-6, § 12b and UX-5. D-6 now justifies the target's identifier by the callout and by a switch by identifier.
> - UX-5 and § 9.6 specify the callout: "→ work 2 of 2", spoken "Next swap to work, 2 of 2 accounts named work", and "one of 2 accounts named work" with an older daemon.
> - UX-10 picks option c-ii. § 14.6 item 15 and the `account-key-form` stub record option c-i.
> - UX-4 keys the switching state on row identity. UX-10 puts the ordinal into the hint, the tooltip and the Swap button's accessible label.
> - The block matches the daemon. `perform_socket_swap` resolves the target before any viability gate and refuses an ambiguous label (`src/daemon/commands.rs:107-128`) [anchor: `resolve_target`].
>
> **Second baseline concern.** I walked TF-1 into TF-2.
> - The card says "this one is work 2 of 2". The header stays visible above the card (`apps/menubar/Sources/StatusPanelView.swift:254-256`). A capture takes the signed-in account (`AccountCaptureModel.swift:143`), so the header names the same entry.
> - Opening Settings closes the panel (`StatusItemController.swift:208`) and resets the card (`:345`). Settings then re-fetches the roster (`SettingsWindowController.swift:53-55`).
> - No surface sorts the roster:
>   - the status frame (`src/daemon/snapshot_build.rs:417-420`) [anchor: `AccountReading`];
>   - `config-get` (`src/config/settings.rs:65-77`) [anchor: `ConfigView`];
>   - the panel rows (`HonestStateMachine.swift:432`);
>   - Settings (`SettingsModel.swift:214`).
> - So "work 2 of 2" finds the same row in Settings, and the flow completes.
>
> **Third baseline concern.**
> - UX-8 gives each duplicate field the accessible label "Label, work, 2 of 2 accounts named work".
> - § 11.4 narrows the rule to "never a full uuid".
> - UX-9 puts any uuid line in its row's accessibility container.
> - I did not hear this, and `settings-field-reads-the-label-twice` below records what I infer.
>
> **The lens question beyond the baseline.**
> - **Task flows:** TF-1, TF-2 and TF-3 each reach an end state, and every error path reuses an existing presentation. No path leaves the operator stuck. The shared-label block costs a rename, and the copy states that cost.
> - **States:** the three § 9.5 matrices cover every state PRD § 11.2–§ 11.4 names.
> - **Surfaces:** notices are inline (UX-1), and each surface says how long a notice persists (UX-2, UX-11). Nothing adds a screen or a call to action.
> - **Telling the accounts apart:**
>   - A sighted user gets the ordinal badge and a colour slot no other row uses.
>   - A VoiceOver user hears the ordinal in row names, Settings field labels, the callout and the announcement.
>   - No row shows an identifier. Uuids appear only in notices, beside an ambiguous label, under the Musts that item 10a returns to the PRD.
> - **Copy:** § 8.5 now names both CLI paths. The gaps below are about wording and sources, not broken flows.
>
> **A correction to my own baseline.** I wrote: "(c-ii) Give those rows, and the callout's Swap button, the existing blocked treatment". That is wrong for the callout.
> - Its Swap button has no blocked state in Swift. It is disabled only while a swap is pending (`apps/menubar/Sources/StatusPanelChrome.swift:243`).
> - My suggested cost line, "switching to either stays unavailable until one is renamed", is too broad. The daemon's automatic swap addresses a uuid-keyed stash (`src/daemon.rs:4572-4574`, `src/config.rs:390-391`) [anchor: `locked_swap`], so it still rotates onto either account.
> - The cause of both errors is the same. I stated facts about a neighbouring component by analogy with the one I had read, without reading the neighbour.
> - The design copied both statements, so `callout-blocked-state-is-unspecified` and `cost-line-reads-as-out-of-rotation` below carry the fixes.
>
> **What I did not check.**
> - I did not build or run the app, and I heard no VoiceOver output.
> - The tooltip behaviour rests on findings #953 and #950, which others measured. Whether a tooltip shows at all in the shipped panel is still pending a capture.
> - I did not grade feasibility, security or the test plan beyond CAP-04 and CAP-11.
> - I did not measure string widths. The overrun in `blocked-copy-overruns-and-diverges` is an estimate.
> - I took R-2's wording from the code's doc comment (`StatusPanelFormat.swift:2172-2173`), not from the private HQ record.
>
> BLOCKING CONCERNS: none
>
> NON-BLOCKING CONCERNS:
>
> 1. `item-15-omits-the-cli-reach-and-the-parity-ruling` (§ 14.6 item 15). Leaving the item open is intended. The defect is two missing facts.
>    - It says the panel blocks "as the CLI's `use work` refuses too", but § 8.5 and § 12b say `sessiometer use <uuid>` reaches either account today.
>    - It omits the operator's ruling of 2026-07-31, "Choosing a surface is not accepting reduced capability", which item 10b applies to Settings.
>    - R-1's letter holds, because a rename makes either account reachable from the app. The ruling's intent favours c-i, with c-ii as the fallback for an older daemon, once `account-key-form` picks the uuid.
>    - Revision: add both facts to item 15.
> 2. `callout-blocked-state-is-unspecified` (UX-5, UX-10, § 9.5).
>    - The callout has no "existing blocked treatment" (`StatusPanelChrome.swift:243-247`).
>    - The mock's only disabled callout is the "Swaps paused" variant (`apps/menubar/design/menubar-preview.html:653-655`, `:1802-1805`). It replaces the lead and why lines. That would mislead here, because the daemon still plans that swap and the why line holds its reason (`StatusPanelFormat.swift:2142`).
>    - The rows show a blocked reason at rest (#955, `StatusPanelFormat.swift:231`).
>    - Revision: keep the lead and the reason, add the blocked reason as a resting line, and author it in the DQ-4 frame.
> 3. `shared-label-block-must-come-first` (§ 9.6, UX-10).
>    - The design calls the block "a third `SwitchBlock` case". The daemon resolves the target before it checks viability (`src/daemon/commands.rs:107-130`).
>    - If the shared-label case does not come first, a quarantined duplicate row shows the "poke" remedy. A weekly-exhausted one waits for a reset that will not unblock it.
>    - The enum's doc limits it to `swap_command_verdict`'s gates (`StatusPanelFormat.swift:166-168`).
>    - Revision: say "first", and amend that doc comment.
> 4. `cost-line-reads-as-out-of-rotation` (§ 9.6 collision line and Settings note).
>    - "Until one is renamed, neither can be switched to" is false for the daemon's rotation and for `use <uuid>`.
>    - The callout can say "→ work 2 of 2" while the card says this.
>    - Revision: "Until one is renamed, you can't switch to either from the panel."
> 5. `blocked-copy-overruns-and-diverges` (§ 9.6, UX-10, TF-3).
>    - The resting cue is one caption line in 348 pt (`StatusPanelRoster.swift:420-431`, `rowCueBudget`). It repeats an email-shaped label (PRD § 2), so it risks truncating.
>    - One fact has three wordings: the badge tooltip, the blocked reason, and the after-click refusal (`StatusPanelFormat.swift:834`).
>    - TF-3's "nothing is refused after it" fails when a frame lags behind a new duplicate.
>    - Revision:
>      - Use "Can’t switch — another account has this label." plus "Rename one in Settings."
>      - Align the three wordings.
>      - Keep the after-click refusal as TF-3's fallback.
> 6. `result-and-header-ordinals-need-a-stated-source` (§ 9.6, § 9.5 card).
>    - **Swap result:** the swap acknowledgement carries labels only (`src/daemon/socket.rs:481`) [anchor: `Accepted`], and `swapDoneText` forbids a client guess (`StatusPanelFormat.swift:812-813`).
>    - **Shared line:** when the duplicate predates the write, the acknowledgement has no bearer list (§ 8.1). The frame can place the uuid only if `account-key-form` picks the uuid.
>    - **Header:** the active account is the first stash whose token matches (`src/active.rs:49-53`). In F3's equal-label case, the header can name 1 of 2 while the card names 2 of 2.
>    - Revision: name each source or a fallback, and say which ordinal the header shows in that edge.
> 7. `badge-tooltip-never-surfaces` (UX-3, RK-22).
>    - Every non-active duplicate row is blocked, and a row-level `.help()` wins over a child's (`StatusPanelRoster.swift:322-328`).
>    - On the active row, a badge tooltip would reverse "NO TOOLTIP HERE, DELIBERATELY (issue #953 AC-3)" (`:275`).
>    - Revision: move the explanation into the blocked row's tooltip, or amend AC-3. Also state the badge's contrast targets (WCAG 1.4.3 and 1.4.11).
> 8. `ordinals-move-for-more-than-a-reorder` (§ 12b).
>    - In a set of three or more, renaming one member renumbers the others. A relabel that joins the set at an earlier position does the same.
>    - `config-get` parses the file (`src/daemon/socket.rs:1402-1404`), while the panel reads the daemon's memory. The two can disagree while a CLI write's reload lags.
>    - Revision: name both in § 12b.
> 9. `settings-field-reads-the-label-twice` (UX-8, § 11.4). This is inferred, not heard.
>    - VoiceOver reads the label, then the value, so "work" would sound twice.
>    - During an unsaved edit, the label would still name the saved text.
>    - Revision:
>      - Carry only the ordinal in the label.
>      - Hide the badge from accessibility.
>      - State what the label says during an edit.
> 10. `already-open-settings-shows-the-old-roster` (TF-1 step 4). Settings loads only in `show()`, and nothing reloads it on focus (`SettingsView.swift:71`). A window reached by Cmd-Tab keeps the roster it had before the capture. Revision: say so, or reload when the window activates.
> 11. `no-free-colour-slot-past-eight-rows` (UX-3). The palette has eight slots (`StatusPanelFormat.swift:2737-2746`), and PRD § 2 records an eight-row status. Revision: state the fallback. The ordinal keeps WCAG 1.4.1 satisfied.
> 12. `card-matrix-omits-the-count` (§ 9.5 card). PRD § 11.2's Populated row lists "the count", which the card does not show (§ 14.7). Revision: say in § 9.5 that the count stays hidden.
> 13. `active-means-two-things-across-the-hand-off`. This predates the design and is outside its goal. In the header, "active" means signed in. In Settings, "Active" means enabled (`SettingsModel.swift:703`), so it shows on both duplicate rows.
>
> BASELINE NON-BLOCKING CONCERNS:
> - Mock authority for Settings: addressed by revision (§ 9 intro, § 9.5 Settings section, § 11.4; the stale `CLAUDE.md` line is recorded in § 14.7)
> - Missing matrix rows: addressed by revision (§ 9.5, all three matrices)
> - Card layout: addressed by revision (§ 9.5 capture card, § 9.6 implementation notes, UX-9)
> - Spoken length: addressed by revision (UX-8, § 11.4, § 9.6, § 12b)
> - Relabel wording: addressed by revision (UX-6, § 9.6, TF-1)
> - Parity of cost: addressed by revision (§ 9.6); its scope is in `cost-line-reads-as-out-of-rotation`
> - CLI remedy: addressed by revision (§ 8.5)
> - RK-22 ordinal badge: addressed by revision (UX-3, § 9.6, RK-22); its tooltip is in `badge-tooltip-never-surfaces`
> - UX-3 colour tie-break: addressed by revision (UX-3, § 9.6); its limit is in `no-free-colour-slot-past-eight-rows`
> - Other label-only strings: addressed by revision (UX-3, TF-3, § 9.6); their sources are in `result-and-header-ordinals-need-a-stated-source`
> - Stats tab: recorded (§ 14.6 item 12)
> - Settings note: addressed by revision (UX-11, TF-2, § 9.4, § 9.5, CAP-04)

#### 14.9.5 Edits after the final verdicts — graded by neither lens

Both final verdicts above are CONFIRM: the product lens's on the second revision, the UX lens's on the
third. A fourth revision then took up the UX re-grade's non-blocking concerns, using the remedies it
proposed, and neither lens has graded it. Two of those concerns corrected statements this design had copied
from the UX lens's first verdict, which that lens retracted in its re-grade. What changed:

- `item-15-omits-the-cli-reach-and-the-parity-ruling`: § 14.6 item 15 adds that `use <uuid>` and the
  daemon's rotation reach either account, and the parity ruling of 2026-07-31.
- `callout-blocked-state-is-unspecified`: UX-5, UX-10, § 9.4 and § 9.5 give the callout a resting reason
  line and a disabled Swap, keeping its lead and why line.
- `shared-label-block-must-come-first`: UX-10 and § 9.6 check the shared-label case first and amend
  `SwitchBlock`'s doc.
- `cost-line-reads-as-out-of-rotation`: the card's collision line and the Settings note now say "you can't
  switch to either from the panel"; TF-3 says the daemon's rotation still reaches either.
- `blocked-copy-overruns-and-diverges`: one wording family for the resting reason, the row tooltip and the
  after-click refusal (§ 9.6); TF-3 keeps the refusal as the fallback for a lagging frame.
- `result-and-header-ordinals-need-a-stated-source`: § 9.6 names each ordinal's source and fallback; a swap
  result keeps the acknowledgement's labels; the header's edge for a byte-identical pair is stated.
- `badge-tooltip-never-surfaces`: UX-3 drops the badge's own tooltip in favour of the blocked row's, and
  states its contrast targets.
- `ordinals-move-for-more-than-a-reorder`: § 12b names the other cases and the `config-get` lag.
- `settings-field-reads-the-label-twice`: UX-8 and § 9.6 make the Settings field's label "Label, 2 of 2",
  hide the badge from VoiceOver, and say what an unsaved edit reads.
- `already-open-settings-shows-the-old-roster`: TF-1 and UX-11 say a window left open keeps its old roster
  until reopened.
- `no-free-colour-slot-past-eight-rows`: UX-3 states the least-used-slot fallback.
- `card-matrix-omits-the-count`: § 9.5 says the card keeps the count hidden, against PRD § 11.2.
- `active-means-two-things-across-the-hand-off`: recorded in § 14.7, outside this design's goal.

The product lens's re-grade concerns were taken up in the third revision, as 14.9.2 lists.

## 15. Glossary

Canonical names are the PRD's object model (§ 9.1); each track's own name is an alias.

| Canonical name | Definition | Aliases by track |
|---|---|---|
| RosterEntry | one account in `config.toml`'s roster | `Account` (TA, `src/config.rs:360`); `[[accounts]]` row (Data); roster (Security) |
| AccountLabel | the operator's handle for an entry; not unique | `label` (all) |
| CapturedIdentity | the account a capture, login or import actually read | harvested `oauthAccount` (TA); upstream identity (Security) |
| StashedCredential | one entry's credential and `oauthAccount` in the keychain | `StashedAccount` (TA, `src/stash.rs:68`); `Sessiometer/<uuid>` item (Data, Security) |
| MigrationArtifact | the encrypted export: rendered config plus credentials by uuid | `Payload` (Data, API) |
| OperatorNotice | a reply line, report or refusal a write produces | notice template (TA); notice line and ack field (API); collision line, relabel line, shared line, Settings note (UX/IA) |
| AccountStatusLine | one account's line on the status/watch wire | same name in Rust and Swift |
| PanelAccountRow | one account row in the panel | `AccountRow` (UX/IA) |
| UsageHistory | usage samples and rollups | `Sample`, rollup (Data); PRD § 9.4's "account key" for it is `account_uuid` |
| EventLogLine | one durable `key=value` line in the event log | event line (API, Data) |
| IdentityMeasurement | a recorded real-login measurement of a premise | the F7 record (Testing) |
| ProjectRecord | a committed document or usage text | record, amendment (TA) |

Terms this design introduces, named here and ratification-pending: **account identifier**, the F4 wire
field whose form `account-key-form` decides; **bearer**, an entry carrying a given label (import's
own word); **duplicate set**, the rows that share one label; **backfill sentinel**, the file that records
the one-time backfill ran; **byte-identical pair**, two stashes holding the same credential bytes (PRD
§ 9.2's phrase); **ambiguous label**, a label with more than one bearer at the moment a line is rendered,
the only place a uuid is shown beside a label (REQ-NTC-C-001); **ordinal**, "n of N", an entry's position
among the entries sharing its label in roster order, the duplicate set's one handle on every surface (UX-3).

**Consistency report.** Tracks were compared pairwise once names were reconciled. Data against UX/IA, Data
against API, Technical Architecture against API, Security against every track and Technical Architecture
against Testing were consistent. Two findings: (1) the UX shared-credential line had no wording for a pair
whose labels are equal, which `shared_with` would repeat. The first resolution reworded the line without a
uuid; the product lens found that this dropped REQ-STSH-B-002's uuid clause on the panel path and settled,
silently, a conflict inside the PRD (§ 3 and AC-WIRE-001 against REQ-NTC-C-001, REQ-STSH-B-002 and § 9.4).
Resolved on revision: the acknowledgement carries the uuids (§ 8.1), the panel names them where the label is
ambiguous (UX-9, § 9.6), and the conflict is recorded for the PRD (§ 14.6 items 10a and 10b). (2) Inside the PRD, F4's
invariant (no text change for a roster without a duplicate label) meets REQ-NTC-B-001, whose count change
shows on a roster with a parked entry. The specific requirement governs, and the enabled-count option keeps
every roster without a parked entry unchanged — acknowledged, put to the product lens, and confirmed by it
(§ 14.9).

## 16. Requirement-to-Track Coverage Matrix

Every requirement in PRD § 10, bound to the section that designs it and the capability (§ 11.5) that tests
it. "Per policy" marks a requirement whose form an open decision fixes; the design serves every option.

| Requirement | Tracks | Covered in | Capability | Status |
|---|---|---|---|---|
| REQ-ROS-B-001 | TA, API, UX/IA | D-1, D-2, § 8.1, § 8.5, § 9.3 | CAP-02 | covered; per policy (`duplicate-label-policy`) |
| REQ-ROS-B-002 | TA | D-1 | CAP-01 | covered |
| REQ-ROS-I-001 | API | D-10, § 8.1 | CAP-03 | covered |
| REQ-ROS-I-002 | UX/IA, API | § 8.1, § 9.1 (UX-1, UX-2, UX-3, UX-9, UX-11), § 9.5, § 9.6 | CAP-04 | covered; parity with § 8.5 includes the ordinals, the bearers' uuids and a warn's cost; the uuid display rule returns to the PRD (§ 14.6 item 10a) |
| REQ-ROS-C-001 | API, Testing | D-10 | CAP-03 | covered |
| REQ-ROS-C-002 | Testing | § 11.5 (MTP-6, G-RED) | CAP-02 | covered; per policy |
| REQ-ROS-Q-001 | TA, Testing | D-1, § 13 | CAP-01 | covered |
| REQ-IDN-B-001 | TA, API | D-2, § 8.5 | CAP-05 | covered |
| REQ-IDN-I-001 | API | D-10, § 8.1 | CAP-03 | covered |
| REQ-IDN-I-002 | UX/IA | § 9.1 (UX-6), § 9.5 | CAP-04 | covered; shown after #1474 |
| REQ-IDN-C-001 | Testing | § 11.5 (G-RED) | CAP-05 | covered |
| REQ-IDN-C-002 | TA, Security | D-2 | CAP-06 | covered where `login-mismatch-policy` refuses |
| REQ-STSH-B-001 | TA, Security | D-4 | CAP-07 | covered |
| REQ-STSH-B-002 | TA, Data, API, UX/IA | D-4, D-9, § 8.1, § 8.4, § 8.5, § 9.6 | CAP-07, CAP-04 | covered: by label, and each by its uuid where its label has more than one bearer — the equal-label case included — in the CLI reply and on the card, with both uuids in the acknowledgement; the display rule returns to the PRD (§ 14.6 item 10a) |
| REQ-STSH-Q-001 | TA, Testing | D-4, § 13 | CAP-07 | covered |
| REQ-STSH-Q-002 | TA, Testing | D-4, § 13 | CAP-08 | covered |
| REQ-STSH-C-001 | Security, TA | D-4, § 11.1 (SEC-1) | CAP-09 | covered |
| REQ-STSH-C-002 | Testing | § 11.5 (G-RED) | CAP-07 | covered |
| REQ-STSH-C-003 | Security, TA | D-4 | CAP-09 | covered |
| REQ-WIRE-I-001 | TA, Data, API | D-5, D-6, § 8.3 | CAP-10 | covered; field form per `account-key-form` |
| REQ-WIRE-C-001 | Security, API | D-6, § 11.1 (T-04) | CAP-10 | covered |
| REQ-WIRE-C-002 | API | D-6 | CAP-10 | covered |
| REQ-WIRE-C-003 | API, Testing | D-6, § 8.6 | CAP-10 | covered |
| REQ-ROW-B-001 | UX/IA, API | § 9.1 (UX-3, UX-4, UX-5, UX-10), D-6 | CAP-11 | covered; the identifier half waits on its form, the position fallback does not |
| REQ-ROW-C-001 | UX/IA, API | § 9.1 (UX-4), § 8.3 | CAP-11 | covered |
| REQ-ROW-C-002 | UX/IA | § 9.1 (UX-3), § 9.5 | CAP-11 | covered as specification; authoring the frame is implementation |
| REQ-USG-B-001 | Data, TA | D-5, D-7 | CAP-12 | covered |
| REQ-USG-C-001 | Data | D-7 | CAP-12 | covered under the migration reading of "re-keyed"; the reading returns to the PRD (§ 14.6 item 11) |
| REQ-USG-C-002 | Data, Security | D-7 | CAP-12 | covered |
| REQ-USG-C-003 | Data, API | D-8 | CAP-13 | covered: the shape does not change, so no bump applies |
| REQ-EVT-B-001 | Data, API | D-9, § 8.4 | CAP-14 | covered |
| REQ-NTC-B-001 | TA, API | D-3, § 8.5 | CAP-15 | covered |
| REQ-MIG-B-001 | TA, Data, Security | D-11 | CAP-16 | covered; per policy (`import-duplicate-uuid-policy`) |
| REQ-MIG-B-002 | Data | D-11 | CAP-16 | covered; per policy |
| REQ-MIG-C-001 | Data | D-11 | CAP-16 | covered |
| REQ-MIG-C-002 | Testing | § 11.5 (G-RED) | CAP-16 | covered |
| REQ-MIG-B-003 | TA, Data, Security | D-11, § 11.1 (T-02) | CAP-17 | covered |
| REQ-MIG-C-003 | Testing | § 11.5 (G-RED) | CAP-17 | covered |
| REQ-MSR-B-001 | TA, Testing | D-13, § 14.4 (SP-1) | CAP-18 | covered; checked by reading the record |
| REQ-MSR-C-001 | Security, Testing | D-13, § 14.4 (SP-1) | CAP-18 | covered; checked by reading the record |
| REQ-LBL-C-001 | TA | D-13, § 12.2 | CAP-19 | covered; the record is implementation work |
| REQ-LBL-B-001 | TA, Security | D-1 (the key hook) | CAP-19 | covered where detection is adopted |
| REQ-LBL-C-002 | Security, Testing | § 2, § 14.4 (SP-3) | CAP-19 | covered where a dependency is needed |
| REQ-DOC-B-001 | TA | D-13 | CAP-20 | covered |
| REQ-DOC-B-002 | TA | D-13 | CAP-20 | covered; #1476's policy governs if decided first |
| REQ-DOC-B-003 | TA, API | D-13 | CAP-20 | covered |
| REQ-DOC-B-004 | TA | D-13 | CAP-20 | covered |
| REQ-DOC-C-001 | TA, Testing | D-13 | CAP-20 | covered |
| REQ-STSH-C-004 | TA, Security | D-12, D-13 | CAP-21 | covered; the record is implementation work |
| REQ-STSH-B-003 | TA, Data, Security | D-12 | CAP-21 | covered where `orphan-stash-policy` reaps; SP-2 |
| REQ-STSH-B-004 | Data, Security | D-12 | CAP-21 | covered where `orphan-stash-policy` reports |
| REQ-NTC-C-001 | TA, Security, API, UX/IA | D-3, § 8.5, § 9.6, § 11.1 | CAP-22, CAP-04 | covered; on the panel the uuid clause returns to the PRD (§ 14.6 item 10a) |
| REQ-NTC-C-002 | TA, Testing | D-3 | CAP-22 | covered |
| REQ-NTC-C-003 | Testing | § 11.5 (G-RED) | CAP-23 | covered |

**UNCOVERED: none.** No requirement lacks an executed track or a capability, so no typed exit was needed.

## 16b. Element-to-Requirement Backward-Coverage Matrix

The audit reads the provenance recorded in § 9.7 for screens, calls to action and UX entities, and the
entity model in § 5 for the rest; it re-derives nothing. Each traced id was checked against PRD § 10.

| Element | Type | Traces to | Provenance source | Status |
|---|---|---|---|---|
| Panel account list | Screen | REQ-ROW-B-001 | § 9.7 | traced |
| Panel capture card | Screen | REQ-ROS-I-002 | § 9.7 | traced |
| Settings accounts section | Screen | REQ-ROS-I-002 | § 9.7 | traced |
| Capture (socket) | CTA | REQ-ROS-B-001 | § 9.7 | traced (existing CTA) |
| Save label (`config-set`) | CTA | REQ-ROS-B-001 | § 9.7 | traced (existing CTA) |
| RosterEntry | Entity | REQ-ROS-B-001 | § 5 entity model | traced |
| AccountLabel | Entity | REQ-ROS-B-002 | § 5 entity model | traced |
| CapturedIdentity | Entity | REQ-IDN-B-001 | § 5 entity model | traced |
| StashedCredential | Entity | REQ-STSH-B-001 | § 5 entity model | traced |
| MigrationArtifact | Entity | REQ-MIG-B-001 | § 5 entity model | traced |
| UsageHistory | Entity | REQ-USG-B-001 | § 5 entity model | traced |
| EventLogLine | Entity | REQ-EVT-B-001 | § 5 entity model | traced |
| OperatorNotice | Entity | REQ-ROS-I-002 | § 9.7 | traced |
| AccountStatusLine | Entity | REQ-WIRE-I-001 | § 9.7 | traced |
| PanelAccountRow | Entity | REQ-ROW-B-001 | § 9.7 | traced |
| IdentityMeasurement | Entity | REQ-MSR-B-001 | § 5 entity model | traced |
| ProjectRecord | Entity | REQ-DOC-B-001 | § 5 entity model | traced |
| Backfill sentinel (storage-only) | Entity | REQ-USG-C-001 | § 5 entity model; D-7 | traced |
| Collision line and note | Entity | REQ-ROS-I-002 | § 9.7 | traced |
| Relabel in the outcome line | Entity | REQ-IDN-I-002 | § 9.7 | traced |
| Shared-credential line | Entity | REQ-STSH-B-002 | § 9.7 | traced |
| Ordinal badge and spoken cue, on every surface | Entity | REQ-ROW-C-002, REQ-ROS-I-002 | § 9.7 | traced |
| Duplicate-set colour tie-break | Entity | REQ-ROW-C-002 | § 9.7 | traced |
| Next-swap callout ordinal, with the target's identifier on the wire | Entity | REQ-ROW-B-001 | § 9.7; D-6 | traced through DQ-4 (#930); the identifier goes beyond REQ-WIRE-I-001's wording, flagged for ratification |
| Blocked switch on a shared label | Entity | none | § 9.7 | PHANTOM, kept and flagged: no requirement names the panel's switch; without it the switch fails after the click; returns to the PRD (§ 14.6 item 15) |
| Legacy row fallback | Entity | REQ-ROW-C-001 | § 9.7 | traced |
| Settings row uuid line | Entity | none | § 9.7 | not built unless `settings-row-shows-uuid` is answered yes (§ 14.6 item 10b); no requirement asks for it |

**PHANTOM: one, kept and flagged** — the blocked switch on a shared label, which no requirement names
(§ 14.6 item 15). Flagged as well: the next-swap target's identifier, which REQ-WIRE-I-001's text ("on each
account line") does not name and DQ-4's callout needs; and the Settings row's uuid line, which is not built
unless the operator asks for it.
