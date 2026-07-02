# Security Policy

`sessiometer` takes custody of Claude Code credentials on macOS, so credential
safety is a first-class concern. This document covers how to report a
vulnerability and the security posture the tool is built to.

## Reporting a vulnerability

**Please do not open a public issue for a security problem.** Report it privately
to the maintainer at **oleksii@pelykhconsulting.eu**.

Include, as far as you can:

- what the issue is and the impact you see — for example credential disclosure, a
  weakened keychain ACL, or a secret reaching `argv` / a log line;
- the version (`sessiometer --version`) and your macOS and Claude Code versions;
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
- **Minimal, audited dependencies.** The single outbound call is a read-only usage
  `GET` via `curl` (no HTTP-client crate), and the supply-chain gates
  `cargo deny check advisories sources licenses` run in CI.

For where credentials and state actually live on your machine, see
[What it stores](README.md#what-it-stores).

## Scope and known limitations

- `sessiometer` is **macOS-only** and operates on the login keychain.
- It manages credentials for accounts **you own**; you are responsible for
  complying with each provider's Terms of Service.
- `--plaintext` export intentionally writes unencrypted credentials — treat and
  delete such a file like a password.
- A `claude /login` landing at the exact moment the daemon is mid-swap is a known,
  documented `0.1.0` race (last-writer-wins, reconciled on the next start); see
  [Edge cases & resilience](README.md#edge-cases--resilience).
