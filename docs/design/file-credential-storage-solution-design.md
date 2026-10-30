---
title: Selectable File Credential Storage — Implemented Design
source: Conversation request to add file storage alongside existing Keychain storage; evidence and limitations are reproduced in this document.
created: 2026-10-28
updated: 2026-10-30
status: implemented-on-feature-branch
implementation-status: File storage, token-only accounts, automatic inference usage, macOS helper handoff, and live-profile re-login recovery are implemented; real-client and native Linux acceptance remain separate.
release-gate: Real owned macOS helper login/refresh and live-profile re-login acceptance, native Linux acceptance, and outstanding release checks remain separate.
---

# Selectable file credential storage

This document describes the implemented design, its safety boundaries, and its verification
status. Production source and tests are authoritative for implementation details; this is not a
patch plan or a second copy of the code. Operator configuration and commands live in the
[credential-storage guide](../../README.md#credential-storage).

## Reading guide

- [Scope and platform policy](#scope-and-platform-policy)
- [Architecture and maintained implementation](#architecture-and-maintained-implementation)
- [Configuration and profile resolution](#configuration-and-profile-resolution)
- [Swapping credentials and identity](#swapping-credentials-and-identity)
- [Owned login and refresh helpers](#owned-login-and-refresh-helpers)
- [Live-profile re-login recovery](#live-profile-re-login-recovery)
- [File safety and concurrency](#file-safety-and-concurrency)
- [Usage and refresh capabilities](#usage-and-refresh-capabilities)
- [Configuration recovery and transfer](#configuration-recovery-and-transfer)
- [Failure semantics](#failure-semantics)
- [Verification and remaining acceptance](#verification-and-remaining-acceptance)
- [Maintenance obligations](#maintenance-obligations)
- [External source provenance](#external-source-provenance)

## Scope and platform policy

File storage changes where Sessiometer keeps the active credential and saved accounts, not its
swap engine, refresh engine, selection thresholds, or scheduling policy. The existing complete
credential representation and lock ordering remain in use. Token-only identity and scope-based
usage are related extensions, not additional storage backends.

| Platform | Default | Supported selection |
|---|---|---|
| macOS | Keychain | Keychain or file |
| Linux | File | File only; Keychain is rejected before storage I/O |
| Windows and other targets | No supported runtime | Rejected; a file default in the configuration type does not enable the platform |

The CLI and daemon target macOS and Linux; the menu-bar app remains macOS-only. Native Linux
build/test execution and real-Claude persistence acceptance must be established separately from
macOS results. See the [verification record](#verification-and-remaining-acceptance).

The core invariants are:

- One immutable backend/profile selection supplies every credential-dependent collaborator.
- File mode keeps both active credentials and saved accounts in files. Ordinary reads do not
  fall back to Keychain or perform migration.
- Full credential bytes, including refresh tokens and unknown fields, survive swaps and saves.
- Identity may be present or intentionally absent; Sessiometer never invents a provider UUID.
- Source conflicts and unreadable storage are errors, not evidence that credentials are missing.
- macOS file mode has two explicit copy-before-delete operations: owned-helper output handoff
  and known-account live-profile recovery. Neither permanently disables Claude's Keychain use.
- Linux performs no Keychain namespace calculation, probe, read, deletion, witness, or reaping.

This does not introduce a provider registry, profile manager, new launcher, background migration
service, credential journal, sandbox requirement, or runtime opt-in beyond backend selection.
It does not promise immediate replacement of authentication in an already-created request,
permanent suppression of other Claude writers, or protection against a hostile same-user process.

## Architecture and maintained implementation

The existing [CredentialStore](../../src/keychain.rs), [AccountStash](../../src/stash.rs), and
[isolated-session contract](../../src/keychain.rs) remain the integration boundaries. Small enums
in the [storage module](../../src/storage.rs) delegate to concrete adapters; no dynamic backend
framework or second swap/refresh implementation is needed.

| Responsibility | Maintained implementation |
|---|---|
| Backend defaults, validation, config rendering and preservation | [configuration](../../src/config.rs), [config modules](../../src/config/) |
| Pinned profile, identity path, Keychain namespace, canonical/stash/isolated factories | [storage selection](../../src/storage.rs) |
| Active credential file, saved-account envelope, atomic writer, authority guard and file-side handoffs | [file storage](../../src/file_storage.rs) |
| Keychain operations and specific-item deletion | [Keychain adapter](../../src/keychain.rs) |
| Swap ordering, active-account attribution and identity updates | [swap](../../src/swap.rs), [active account](../../src/active.rs), [Claude state](../../src/claude_state.rs) |
| Registration, login, manual selection and refresh | [capture](../../src/capture.rs), [login](../../src/login.rs), [account use](../../src/use_account.rs), [poke](../../src/poke.rs) |
| Isolated execution, renewal and promotion | [launcher](../../src/isolated_spawn.rs), [refresh](../../src/refresh.rs), [periodic refresh](../../src/refresh_tick.rs), [keep-warm](../../src/daemon/keep_warm.rs) |
| Daemon polling, reconciliation and command routing | [composition](../../src/cli.rs), [daemon](../../src/daemon.rs), [daemon modules](../../src/daemon/) |
| Usage method, transport and normalized readings | [usage](../../src/usage.rs), [roster poller](../../src/daemon/seams.rs) |
| Prior-configuration evidence and portable transfer | [witness](../../src/witness.rs), [migration](../../src/migration.rs) |
| Real-client credential-selection probe | [test support](../../tests/support/file_storage_probe.rs) |

Canonical credentials are the active Claude authentication document. A stash is Sessiometer's
saved credential and optional identity for one roster entry. Isolated storage belongs only to a
temporary Sessiometer-owned login/refresh child; it is never the live profile.

## Configuration and profile resolution

The [storage settings](../../src/config.rs) select Keychain or file storage and optionally a Claude
profile directory. The profile override applies to both backends. Setup examples belong in the
[operator guide](../../README.md#credential-storage), rather than being duplicated here.

Selection is resolved once at process startup: explicit configuration wins over the Claude
configuration-directory environment setting, which wins over the native home default. Credential
and identity paths are resolved together, but follow Claude's deliberately different path rules:

| Effective override | Credential directory | Identity state without a legacy state file | macOS service namespace |
|---|---|---|---|
| Absent | Default Claude directory beneath native home | Default state file directly beneath native home | Bare service name |
| Non-empty absolute value | NFC-normalized directory | State beneath the original override spelling | Hash of the NFC-normalized override |
| Relative value, including dot segments | NFC-normalized path resolved against startup working directory | Original override resolved against the same working directory | Hash of the NFC-normalized override, not its resolved absolute path |
| Present but empty | Startup working directory | Default state beneath native home | Bare service name |

A valid regular legacy state file beneath the credential directory takes precedence. An unsafe
or unreadable legacy entry is an error, not permission to select another identity file. Paths
are joined lexically: no filesystem canonicalization, tilde expansion, or environment-variable
expansion inside configuration values. Invalid UTF-8 is rejected; valid Unicode is supported
through the existing Unicode-normalization dependency.

macOS file mode rejects the legacy secure-storage-directory environment override rather than
guessing between competing namespace conventions. Keychain mode retains its existing precedence.
Linux does not use that override to derive a Keychain namespace. See the
[resolver and factories](../../src/storage.rs) and [namespace implementation](../../src/keychain.rs).

**Changing storage settings requires a daemon restart.** The daemon checks its pinned selection
before mutations and reloads; a mismatch refuses credential operations and suspends autonomous
writes. A terminal's later environment change does not retarget the daemon. Daemon-routed commands
use its selection; standalone commands resolve their own startup environment and working directory.
An explicit profile, preferably absolute, avoids different terminal/service resolution.

Claude must be launched against the same profile without higher-priority environment, descriptor,
or other authentication overrides. A separate profile also separates settings/history, not just
credentials. Sessiometer does not inspect or rewrite another running process's environment.
Owned helpers reuse the [existing isolated launcher and shared auth scrub](../../src/isolated_spawn.rs);
file storage does not introduce a second preflight or launch policy.

## Swapping credentials and identity

The [existing swap transaction](../../src/swap.rs) retains its shared lock and sequence:

1. Resolve the accounts and read the required credentials/stashes before mutation.
2. Re-stash the complete outgoing credential with its saved optional identity.
3. Publish the incoming complete credential through the selected canonical store.
4. Set the incoming OAuth identity, or remove only that field when identity is absent.
5. Re-read the canonical credential for confirmation.

The file writer atomically replaces the credential document and advances its millisecond
modification time. The inspected Claude source invalidates cached credentials on a changed file
mtime and supplies the resulting token to the same authentication consumer used after a Keychain
read. The production-backed probe observed A → B → A across later turns of one running client;
it did not establish adoption midway through an existing request.

### Identity-bearing and token-only accounts

The [account model](../../src/config.rs) uses either a genuine Claude UUID or a random stable local
ID. Local IDs are independent of token contents and labels; they are roster/history keys, not
provider identity. The [saved account](../../src/stash.rs) contains complete credentials and an
optional OAuth identity. Explicit null means intentional absence; missing or malformed identity
storage remains an error.

[Token-file registration](../../src/capture.rs) accepts a bounded, owned, private regular file,
never a token supplied as an argument value. It creates an inference-scoped credential with no
fabricated refresh token, expiry, subscription or rate-limit metadata. Registration saves the
stash before the roster and does not read or change the active profile. Ordinary capture/login
still requires real identity, and duplicate labels do not silently replace other accounts.

For example, switching B → C, where C is token-only, writes C's credential and clears B's OAuth
identity. Switching C → B restores B's saved identity. C's stash keeps null; it never inherits B's
UUID. The [field-preserving state writer](../../src/claude_state.rs) leaves device ID, onboarding
and unrelated settings intact. Missing state already satisfies desired absence; malformed state
does not. Reconciliation applies the same present-or-absent intent. Token matching is authoritative;
displayed-identity fallback accepts only genuine Claude UUIDs.

Credential and identity writes are **not an atomic pair**. If the credential commits but the
identity update fails, the swap reports identity update pending through the existing report and
CLI/menu-bar surfaces. Reconciliation retries it. Until that succeeds, Claude may still use the
previous identity metadata; the failure is not reported as a zero-write transaction.

## Owned login and refresh helpers

The [refresh protocol](../../src/refresh.rs) and [login capture](../../src/login.rs) use the selected
isolated adapter without changing minting, classification, compare-and-swap, or promotion rules.
On Linux this is a file-only lifecycle. On macOS, Claude may prefer its isolated Keychain item and
remove the seeded file; the [file adapter](../../src/file_storage.rs) normalizes that output after
the owned child completes.

For a parked account A:

1. Snapshot A's saved credential and seed the backdated credential into an isolated file.
   Login instead starts without a credential seed. File mode never seeds Keychain.
2. Run the existing isolated child with the existing timeout, cancellation and authentication scrub.
3. After completion, read only that helper's derived Keychain item on macOS. A present item wins
   over a remaining file; timestamps are not used to guess which source is newer.
4. Validate and atomically publish the complete returned credential into the isolated file.
5. Delete that isolated item only after publication succeeds, then read through the normal file
   authority guard. Confirmed item absence goes directly to file readback.
6. Continue the existing login capture, refresh classification/CAS, or guarded keep-warm promotion.

An unreadable item, invalid payload, failed copy, failed deletion, or remaining shadow is an error,
not permission to use a stale file or report successful renewal. A result for A still cannot
replace newer stash contents or overwrite B after an intervening account switch.

The handoff's **copy-before-delete order is distinct from session teardown**. Existing best-effort
cleanup runs on success, failure and cancellation and may remove temporary output after a failed
handoff. This is not durable failed-refresh recovery or a transaction across Keychain, temporary
storage and the final stash. Login/keep-warm retain their existing cleanup-before-caller-persistence
boundary.

On macOS, both backend selections reap owned temporary Keychain artifacts as well as owned helper
directories. Linux performs directory-only cleanup. Cleanup targets stay pinned to the session;
no helper guard ever owns the live/default Keychain item. Real installed-Claude helper acceptance
is separate from the fake-backed lifecycle tests below.

## Live-profile re-login recovery

A separate recovery path handles interactive re-login of an **already captured account** in
macOS file mode. Backend selection is its only opt-in. Keychain mode and non-macOS platforms return
without probing or touching Keychain. Ordinary reads/writes retain their authority guard; reads
and status do not import or delete credentials.

The [recovery boundary](../../src/storage.rs), [file import](../../src/file_storage.rs), and
[specific-item deletion](../../src/keychain.rs) implement this order:

1. Probe the configured profile's namespace without requesting secret data.
2. On a shadow conflict, acquire the shared swap lock, recheck storage settings against current
   configuration, and repeat the authority check.
3. Resolve/read the unique matching login-Keychain item and validate the full credential.
4. Identify a roster account using the existing token-first, displayed-account fallback model;
   preserve that account's saved optional identity. This is local attribution, not server proof
   of bearer ownership. Unknown identities are not automatically onboarded.
5. Save the account stash, then atomically publish the complete active credential file.
6. Re-enumerate/reread the source and refuse observed changes or ambiguity. Delete only the
   resolved service/account/keychain item, never a broad namespace or search-list match.
7. Read through the normal file guard and compare bytes. A remaining/recreated shadow or a
   changed/missing file is still a failure.

Both saved copies precede deletion. A later failure deliberately retains any earlier successful
save; recovery is not an all-or-nothing transaction and does not roll back fresh credentials.
Live items never acquire the automatic cleanup semantics of isolated helper items.

### Recovery entry points

- **Startup:** before canonical/display reconciliation and the boot canary.
- **Daemon ticks:** before canonical reads and usage polling. The existing short-cadence external
  login watcher preserves read errors and wakes a tick on a source conflict, even without an
  initial credential baseline; it does not perform the import itself.
- **Socket swaps:** before policy evaluation, reconciling active/health state and preserving
  resulting events even if the requested swap is already active or subsequently refused.
- **Standalone swaps:** after daemon routing falls through and the requested handle is validated.
  A reachable daemon remains the swap writer; ambiguous replies are not retried standalone.
- **Manual refresh:** validate a named target, recover, then resolve the active account. Named
  and all-account modes share this order. The recovered active account remains refused/skipped;
  only parked accounts are refreshed. This CLI path is independent of daemon availability.

[Capture](../../src/capture.rs), new-account onboarding and ordinary status are not additional
recovery entry points. Unknown accounts retain their Keychain item and require managed login.
The wiring lives in [daemon reconciliation](../../src/daemon/canonical.rs),
[socket commands](../../src/daemon/commands.rs), [the idle watcher](../../src/daemon/run_loop.rs),
[manual selection](../../src/use_account.rs), and [manual-refresh preflight](../../src/poke.rs).

**Concurrent-writer limit:** Claude does not share Sessiometer's lock. Rereading before deletion
can detect observed changes but cannot close the read/delete race; Claude can also create another
item afterward. The importer inherits the existing login-Keychain scope. Matching items elsewhere
in the search list remain conflicts, not deletion candidates. This is neither atomic migration
with Claude nor permanent Keychain disablement.

## File safety and concurrency

The [file adapter](../../src/file_storage.rs) applies the same publication machinery to active
credentials and saved-account records. The [security policy](../../SECURITY.md) covers the broader
secret-handling contract.

### Private storage and record format

- The active file is Claude's complete credential document. Saved accounts live in a dedicated
  native-local support directory, not the potentially synchronized configuration directory.
- A saved-account envelope contains a format version, the account selector, and hexadecimal
  encodings of the exact credential and identity bytes. Hex is **not encryption**. File mode
  does not provide Keychain's encryption-at-rest properties.
- SHA-256 of the account selector determines the record filename, avoiding path traversal and
  case-folding collisions; it is not a token fingerprint. Reads validate selector and identity.
- Identity-bearing records use local version 1; explicit null identity uses version 2. Readers
  accept both, but refuse null in version 1. These are not migration-container versions.
- Credential and identity blobs are bounded at 1 MiB each; the envelope permits encoding overhead.
  Oversized data is rejected, never truncated. Raw and encoded secret buffers are zeroized where
  practical; not every parsed allocation is guaranteed wiped.

### Filesystem and commit boundaries

- Secret files must be owned, mode 0600, single-link regular files. No-follow/nonblocking reads
  reject final symlinks, hard links and special files, and remain bounded if a file grows.
- Existing profile directories must be owned and not group/other-writable. Safe 0755/0750
  directories and valid directory aliases are accepted unchanged; Sessiometer never chmods them.
- New owned directories are 0700. A valid first active write may create only a missing profile
  leaf under an existing safe parent; reads and construction create nothing. Refused/invalid
  writes do not bootstrap it. Missing parents and dangling aliases are not recursively repaired.
  A later I/O failure may leave an empty created directory; it is not deleted as rollback.
- Publication uses an owner-only staging file, bounded validation, sync and atomic rename.
  Credential publication verifies a change at millisecond mtime resolution. Rename is the last
  required fallible writer operation; pre-commit failure preserves the previous complete target.
- Parent-directory power-loss durability is not promised. Confirmation or identity failure after
  a successful rename must not be described as a guaranteed no-write failure.

On macOS, the metadata-only authority probe checks the default Keychain search list and treats
any item under the profile service as a conflict. It is bounded to five seconds, never requests a
password, and fails closed when absence cannot be established. Linux skips the probe entirely.
The probe cannot prevent a subsequent write by Claude.

### Shared locking

All shared stash/canonical mutations use the existing swap lock; adapters never acquire it again.
The fixed Sessiometer staging name depends on this discipline. Canonical reconciliation re-reads
under the lock before re-stashing. Account removal preserves config-first ordering, taking the
swap lock only for subsequent stash deletion. Config-save/account-recapture races are not made
atomic, and config-write locks must not be nested inside the swap lock.

Owned isolated sessions have a single Sessiometer publisher, but Claude can still write the final
path. Lock timeout retains the existing zero-write refusal. No file-backend change makes config,
stash, credential, identity and Keychain one transaction. Whole-blob swaps also do not selectively
preserve a separate profile-local set of MCP secrets while replacing only OAuth fields.

## Usage and refresh capabilities

Identity presence, token lifetime, usage scope and refreshability are independent. The
[usage method](../../src/usage.rs) is selected from credential scopes, not from the storage backend
or whether an account has a provider UUID:

| Credential capability | Monitoring method |
|---|---|
| Profile scope present | Existing usage GET |
| Inference scope without profile scope | Automatic minimal inference POST; read quota headers |
| Missing/unknown legacy scope metadata | Existing usage GET |
| Explicit scopes supporting neither method | Local usage-unavailable result, without an HTTP request |

The inference probe is a **real request that consumes provider usage**. It sends a fixed minimal
prompt, not conversations, project content or saved account identity. The existing fixed-endpoint
curl transport carries the bearer through zeroizing stdin configuration, disables ambient curl
configuration, discards model output, and adds no redirects, automatic retries or HTTP-error
fallback from the usage GET. There is no additional runtime flag.

Only successful responses with both finite utilization dimensions produce a normal reading;
reset times remain optional. Missing utilization is an error, not zero. Header utilization is
fractional rather than the usage API's percentage representation. HTTP 429 remains a rate-limit
error even if quota headers are present; a probe 403 is rejection, not a profile-scope diagnosis.
The [ADR-0017 exception](../adr/0017-bounded-blindness-preemptive-swap-not-header-observation.md)
records the method without claiming provider-policy approval.

Active and parked accounts use the existing scheduler, backoff and normalized readings for
meters, reset displays, health and automatic ranking. Standalone viability checks and enabled
online canaries use the same method; viewing daemon status sends no extra request. Deliberate
usage unavailability is not zero usage, a 401 streak, a blind episode or a quota-driven swap.
Without a reading, the account is not an automatic candidate; manual selection remains possible.

Missing expiry stays unknown. Absent/null refresh tokens skip scheduled and reactive renewal;
explicit refresh performs no credential write. Empty-string refresh tokens retain the existing
revoked-token meaning. No refresh credential, account UUID or one-year expiry is synthesized.

## Configuration recovery and transfer

The [prior-configuration witness](../../src/witness.rs) considers file-stash metadata as well as
existing usage-store evidence. Missing configuration with saved accounts present must not become
a destructive fresh-start assumption. Unsafe or unreadable evidence is inconclusive, not absent.
macOS may also use Keychain metadata as historical evidence; Linux never probes Keychain.

[Local configuration rendering](../../src/config/render.rs) records the effective backend and
preserves the optional profile through settings edits and roster backups. Environment-derived
paths are not written back as user configuration. Portable exports omit machine-local storage
settings; [import parsing](../../src/config/load.rs) applies the destination selection before
platform validation, and all import credential operations use that backend.

[Migration](../../src/migration.rs) preserves complete credential/identity data. The token-only
extension writes migration version 2 and still reads version 1, including the frozen encrypted
v1 fixture. Secret-free exports and roster backups contain no credential records. The portable
migration format and the local file-envelope format have separate compatibility rules.

Changing the backend is **not migration**: it does not copy, merge or delete accounts. Transfers
use explicit export/import, preserving existing encrypted-export defaults and destination-local
settings. Verify destination data before any separately authorized source deletion.

Backend/profile selection is configuration-driven; no new menu-bar storage editor is required.
The app remains local-only and receives non-secret status/settings. Credential-file reads,
Keychain access and network authentication stay in the Rust side. Wire changes must follow the
independent schema/fixture rules in [repository guidance](../../CLAUDE.md#schema-versions-are-four-independent-wires).

## Failure semantics

| Observation | Result |
|---|---|
| Active file/profile absent and authority established | Confirmed missing; read creates nothing |
| Unsafe/unreadable file or unknown authority | Refuse; never translate to absence or token rejection |
| macOS Keychain shadow | Ordinary access refuses; only explicit recovery boundaries may import a known account |
| Missing, corrupt or mismatched stash | Incomplete/corrupt record; never infer another account or backend |
| Invalid/oversized input | Reject before publication; never truncate |
| Failed helper transfer/deletion or remaining shadow | Unsuccessful readback, not successful renewal or token death |
| Failed live import after a save | Keep the saved fresh copy; do not claim rollback or all-or-nothing failure |
| Credential committed but identity/confirmation incomplete | Report partial/unconfirmed outcome; reconciliation may retry identity |
| Storage settings differ from the running daemon | Restart required; no mixed-backend mutation |
| Usage unavailable or token not refreshable | Preserve that capability distinction; do not invent capacity or renewal |

Diagnostics remain secret-safe and use the existing [error/control classification](../../src/error.rs),
not new storage-specific account-death semantics.

## Verification and remaining acceptance

### Maintained regression coverage

Tests live beside the production code rather than as copied examples in this document:

| Contract | Test location |
|---|---|
| Platform defaults, profile resolution, pinned factories, config round trips and portability | [storage](../../src/storage.rs), [config modules](../../src/config/), [Keychain namespace tests](../../src/keychain.rs) |
| Complete credential/identity preservation, file bounds/permissions, mtime, first write, staging failure and lock contention | [file storage](../../src/file_storage.rs), [swap](../../src/swap.rs) |
| Helper normalization, missing/stale files, copy-before-delete, error propagation, refresh/CAS and cleanup | [file storage](../../src/file_storage.rs), [refresh](../../src/refresh.rs), [login](../../src/login.rs) |
| Live import ordering, unknown identities, changed sources, remaining shadows and platform/backend no-ops | [file storage](../../src/file_storage.rs), [storage](../../src/storage.rs), [Keychain](../../src/keychain.rs) |
| Watcher wakeup, swap entry points and recovery-before-active-account resolution | [daemon modules](../../src/daemon/), [account use](../../src/use_account.rs), [poke](../../src/poke.rs) |
| Token-only capture, null identity, partial identity failure and B → C → B reconciliation | [capture](../../src/capture.rs), [stash](../../src/stash.rs), [Claude state](../../src/claude_state.rs), [swap](../../src/swap.rs) |
| Automatic usage selection, request redaction, header parsing, HTTP errors and no-refresh behavior | [usage](../../src/usage.rs), [daemon tests](../../src/daemon.rs), [refresh](../../src/refresh.rs) |
| Lost-config protection and cross-backend portable transfer | [witness](../../src/witness.rs), [CLI integration](../../src/cli.rs), [migration](../../src/migration.rs) |

These hermetic tests use temporary files, fake Keychain sources and dummy credentials. They do
not require an operator login, paid request or daemon restart. They are not a blanket assertion
that every adversarial fault injection or real-client negative control has been exercised.

### Recorded production validation

The following are historical local macOS checkpoints, not new runs performed by this document
cleanup. Each checkpoint passed the five repository stable-Rust gates and MSRV build/tests against
the production lockfile. Stable was Rust 1.96.0; MSRV was 1.87.0. Each listed test total applies to
both toolchains, with 15 explicitly ignored tests.

| Date | Implemented checkpoint | Tests passed |
|---|---|---|
| 2026-10-29 | Base file storage and production wiring | 2,460 |
| 2026-10-30 | Automatic inference usage, runtime toggle removed | 2,489 |
| 2026-10-30 | macOS owned-helper handoff | 2,500 |
| 2026-10-30 | Live-profile re-login import | 2,514 |
| 2026-10-30 | Manual-refresh recovery preflight correction | 2,521 |

The latest correction adds seven regressions proving recovery-before-read, parked-only refresh,
named-active refusal, error propagation, lazy recovery for invalid targets, no-op recovery,
remaining-shadow refusal and public-command wiring. It corrects an omitted entry point, not the
refresh engine. A new CLI invocation uses the rebuilt manual-refresh path; a running daemon
still needs a restart to use rebuilt daemon code.

The latest recorded dependency-linkage, documentation-citation, compatibility-range and test-runner
environment guards also passed. These results do not stand in for the full dependency-advisory
check or platform/UI gates. Earlier disposable example-harness checks are not production release
evidence and are no longer maintained as a second implementation.

### Production-backed real-Claude probe and POC retirement

The opt-in [real-client probe](../../tests/support/file_storage_probe.rs) passed against installed
Claude **2.1.278** on macOS: one persistent client made three completed turns with A → B → A
credentials. Every rotation used the production file/stash adapters, swap engine and shared lock.
The loopback mock accepted both dummy credentials throughout and checked outbound Authorization
headers, so success did not depend on a 401 or client restart.

The run used macOS OS confinement, withheld raw credentials/output and removed its temporary
profile. It established client credential selection, **not provider authentication**. The probe
remains explicitly ignored by default, bounded and dummy-only; never retry it unconfined or with
real credentials. Invocation guidance is in [contributor documentation](../../CONTRIBUTING.md).

The standalone spike was retired after this production-backed proof. Neither production nor tests
depend on the spike or its temporary backup. Its useful assertions now live in maintained test
support; there is no second writer, CLI or locking policy to sustain.

### Remaining acceptance and release checks

- Real owned macOS login and refresh must verify normalized output reaches the file stash with
  identity preserved and owned temporary resources cleaned. Keep-warm retains guarded promotion.
- Real macOS interactive re-login followed by in-flight swapping must verify the live-profile
  recovery path. Fake-backed invariants do not close the concurrent-Claude writer race.
- Real setup-token inference acceptance and quota-header availability require an authorized live
  check. Dummy transport responses do not establish those provider behaviors.
- Native Linux build/test execution and real Linux login/refresh are separate acceptance checks.
  The [CI workflow](../../.github/workflows/ci.yml) includes a Linux stable/MSRV build-and-test job
  with stable Clippy in the aggregate; its execution was not established by the local macOS runs.
- The full dependency-advisory/source/license gate remained unavailable locally. Workflow-sensitive
  Swift/panel/formula and CI-needs checks require their own results; green Rust tests do not cover
  them. Consult the current [repository gate instructions](../../CLAUDE.md).
- Real-client negative controls for higher-priority auth sources and auxiliary Claude writers
  remain separate compatibility evidence. File storage does not guarantee those writers stay idle.

Live authentication tests require explicit authorization for a disposable profile/account: they
can rotate refresh tokens or consume usage. No live credential operation, daemon restart, real
helper login/refresh or native Linux execution is implied by the implementation test totals or by
a documentation-only edit. Record only measured combinations in the
[compatibility ledger](../../build/version-compat.md), without broadening older historical claims.

## Maintenance obligations

- New credential-reading/writing entry points must use the selected factories, preserve backend
  consistency, and add file-mode coverage. New recovery triggers must retain known-account and
  copy-before-delete rules rather than hiding migration inside reads.
- Every shared file mutation needs existing-lock coverage; isolated ownership must be explicit.
  Do not nest config-write locks or introduce a second lock policy.
- Changes to Claude path/namespace/save behavior require platform-specific persistence checks,
  not a hardcoded-version capability claim or a new spike dependency.
- Preserve local-versus-portable settings, envelope/migration version distinctions, explicit null
  identity, secret redaction, file bounds and permissions when evolving the formats.
- Update the appropriate Rust/Swift schema and fixtures only when that wire changes. A future
  profile editor must distinguish removing an override from an explicitly empty path.
- Use the current [repository checks](../../CLAUDE.md) and [CI definitions](../../.github/workflows/ci.yml)
  instead of maintaining copied commands or workflow YAML here. Keep measured validation and
  outstanding acceptance separate when updating this document.

## External source provenance

The original Claude implementation evidence came from a supplied private source snapshot, not a
file distributed with this repository and not assumed identical to an installed executable.
These are snapshot-relative locations, retained as provenance rather than broken local links:

| Reference | Snapshot-relative location | Inspected lines |
|---|---|---|
| Storage selector | utils/secureStorage/index.ts | 9–16 |
| Plaintext writer | utils/secureStorage/plainTextStorage.ts | 44–67 |
| macOS fallback storage | utils/secureStorage/fallbackStorage.ts | Whole module |
| Credential-directory rules | utils/envUtils.ts | 5–14 |
| Identity-state rules | utils/env.ts | 13–26 |

The relevant findings are carried in-band: the snapshot selects plaintext storage outside macOS;
macOS prefers Keychain and can remove a secondary file after a successful primary save; the file
writer uses owner-only permissions; the profile/identity paths differ as described above; and a
changed credential-file mtime invalidates cached credentials before a later authentication read.
Both storage readers ultimately supply their token to the same API authentication path. Source
inspection supports the design, while the bounded probe and platform-specific acceptance tests
establish only the behaviors they actually observe.
