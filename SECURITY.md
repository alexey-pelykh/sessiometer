# Security Policy

`sessiometer` takes custody of Claude Code credentials on macOS and Linux, so credential
safety is a first-class concern. This document covers how to report a
vulnerability and the security posture the tool is built to.

## Reporting a vulnerability

**Please do not open a public issue for a security problem.** Report it privately
to the maintainer at **oleksii@pelykhconsulting.eu**.

Include, as far as you can:

- what the issue is and the impact you see — for example credential disclosure, a
  weakened keychain ACL, or a secret reaching `argv` / a log line;
- the version (`sessiometer --version`) and your OS and Claude Code versions;
- steps to reproduce; and
- any proof-of-concept.

You will get an acknowledgement once the maintainer sees the report. This is a
solo-maintained, pre-1.0 project, so please allow reasonable time for a fix before
public disclosure — coordinated disclosure is appreciated, and credit is given in
the fix unless you would rather stay anonymous.

## Supported versions

`sessiometer` is at `0.1.0`, an early scaffold. Only the newest release and the
`main` branch receive security fixes; there is no back-port branch yet.

| Version | Supported |
|---------|-----------|
| newest release / `main` | :white_check_mark: |
| anything older | :x: |

## Security posture

The credential-safety properties `sessiometer` is designed around:

- **Keychain via the system CLI, never the SDK.** All keychain access goes through
  `/usr/bin/security` at its absolute path, never the Security.framework SDK.
  Writing the credential through the SDK would re-stamp the item's ACL and evict
  the `apple-tool:` entry Claude Code's silent read relies on; the CLI write
  preserves it. A CI guard fails the build if `security-framework` enters the
  dependency graph. See
  [ADR-0002](docs/adr/0002-keychain-via-security-cli-zero-ffi.md).
- **Secrets never on the command line.** The bearer token is fed to `security` and
  `curl` on **stdin** (`security -i`, `curl --config -`), never as an argument, so
  it cannot leak through `ps` or a process listing.
- **Secrets zeroized in memory.** Credential blobs are held in `Zeroizing` buffers
  that wipe on drop, and the credential type carries no `Debug` implementation, so
  no secret is printable.
- **Least-privilege files.** State files are `0600` and directories `0700`, each
  checked to be owned by the current user; the home directory is resolved from the
  password database rather than `$HOME`, so a spoofed environment cannot redirect
  writes.
- **Redacted diagnostics.** The event log and the verbose diagnostic channel carry
  only handles, enums, percentages, and timestamps — never a token or email — and
  a CI redaction check scans every rendered line.
- **Encrypted export by default.** `export` protects the migration artifact with
  Argon2id + XChaCha20-Poly1305, and the passphrase is never taken from `argv`.
  `--plaintext` is an explicit opt-out that prints a warning and writes usable
  credentials in the clear.
- **Minimal, audited dependencies.** Usage traffic uses system curl, with no HTTP-client
  crate. Credential scopes select a read-only usage GET or, for inference-only tokens,
  a minimal Messages POST that consumes provider usage. Only quota metadata is
  retained from that probe, not model output. Both endpoints are fixed, ambient curl
  configuration is disabled, and request failures never cause a paid fallback. The supply-chain gates
  `cargo deny check advisories sources licenses` run in CI.

For where credentials and state actually live on your machine, see
[What it stores](README.md#what-it-stores).

## Scope and known limitations

- macOS defaults to Keychain; file storage is available on macOS/Linux. Windows is unsupported.
- File credentials and saved accounts are plaintext protected by owner-only file permissions,
  not Keychain encryption. Put them on a trusted local filesystem; same-user hostile races
  and directory-entry persistence after power loss are not claimed security boundaries.
- Ordinary file reads/writes retain the metadata-only Keychain shadow guard. Only explicit
  macOS file-mode reconciliation imports a matching live-profile item: preserve the known
  account's stash and active file before deleting the resolved service/account/login-Keychain
  item. Unknown accounts and unreadable/ambiguous sources are not auto-adopted. Other Keychain
  search-list items remain guarded; this does not change Keychain-mode storage.
- Live imports have no RAII deletion or rollback of fresh saved credentials. A failure after
  either file write may leave that write committed; deletion failures retain the source.
  The pre-delete reread is not an atomic compare-and-delete with a running Claude. The swap
  lock serializes Sessiometer writers only, and Claude can recreate the item after recovery.
- An owned macOS login/refresh helper may save output to its isolated Keychain item.
  After the child finishes, Sessiometer validates and atomically writes those complete bytes
  to the isolated file before deleting that item and performing normal checked file readback.
  This helper path never touches live-profile credentials; live import is the separate,
  explicitly selected file-mode operation above. Neither path deletes unrelated profiles.
- Existing best-effort session cleanup applies after failures, including failed handoffs.
  Copy-before-delete ordering does not add durable failed-refresh recovery or a transaction
  across Keychain and the final account store. Real macOS helper acceptance remains pending.
- It manages credentials for accounts **you own**; you are responsible for
  complying with each provider's Terms of Service.
- `--plaintext` export intentionally writes unencrypted credentials — treat and
  delete such a file like a password.
- A `claude /login` landing at the exact moment the daemon is mid-swap is a known,
  documented `0.1.0` race (last-writer-wins, reconciled on the next start); see
  [Edge cases & resilience](README.md#edge-cases--resilience).
