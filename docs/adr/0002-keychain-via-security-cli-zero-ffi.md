---
type: architecture-decision-record
number: 2
title: "Keychain access via /usr/bin/security CLI (zero FFI)"
date: 2026-07-02
status: accepted
decision_makers: [Oleksii PELYKH (maintainer)]
---

# ADR-0002: Keychain access via `/usr/bin/security` CLI (zero FFI)

## Status

**Accepted** — 2026-07-02. Records a decision already in force in `src/keychain.rs`
and guarded in CI (`scripts/check-no-security-framework.sh`); this ADR captures the
rationale (see #201, #195/A3; originating #1, #2).

## Context

`sessiometer` reads and rewrites the macOS **login-keychain** generic-password item
whose service is `Claude Code-credentials` — the item Claude Code creates on sign-in
and reads **silently** (no user prompt) on every use (`src/keychain.rs` module docs;
README § Prerequisites).

- That silent read depends on the item's ACL. The credential's ACL partition list
  carries an **`apple-tool:`** entry that authorizes Claude Code's own tool identity
  to read the item without prompting.
- There are two ways to touch the keychain from Rust: the **Security.framework SDK**
  (an FFI binding such as the `security-framework` crate) or the **`/usr/bin/security`
  CLI** driven as a subprocess.
- The facts this decision rests on were verified **empirically before implementation**
  and recorded in the issue-#16 ledger (`build/version-compat.md`): the store is the
  legacy file-based `login.keychain-db`; `add-generic-password -U` is an atomic
  in-place update; and the CLI write rides the `apple-tool:` identity.

## Decision

**All keychain access goes through the `/usr/bin/security` CLI, driven as a
subprocess. The Security.framework SDK is never linked — zero FFI for keychain
access** (`src/keychain.rs`; `const SECURITY: &str = "/usr/bin/security"`).

A CI guard, `scripts/check-no-security-framework.sh` (wired in
`.github/workflows/ci.yml`), fails the build if `security-framework` appears anywhere
in the dependency graph, so a future refactor cannot silently reintroduce the SDK
write path (#1, #2).

## Alternatives considered

1. **Security.framework SDK via FFI** (e.g. the `security-framework` crate).
   - **Pros**: no subprocess fork/exec; a typed API; no text-output parsing.
   - **Cons** — the load-bearing one: writing the item through the SDK **as our own
     code identity** re-stamps the keychain item's ACL partition list to our team ID
     and **evicts the `apple-tool:` entry**, breaking Claude Code's silent read. The
     CLI write, by contrast, rides `apple-tool:` and preserves it.
   - **Why rejected**: it destroys the exact property the product depends on —
     rotating a credential that Claude Code will keep reading silently.
2. **Shell out to `security` via `$PATH`** (bare `security`) rather than an absolute
   path.
   - **Pros**: marginally shorter.
   - **Cons**: a hijacked `PATH` could substitute a different binary for a
     security-sensitive call.
   - **Why rejected**: pinning the absolute `/usr/bin/security` removes that
     substitution vector.

## Consequences

### Positive

- **Preserves Claude Code's silent-read `apple-tool:` ACL entry** — the whole point;
  the CLI write rides it (`src/keychain.rs` module docs).
- **Zero FFI / zero `unsafe`** for keychain access, and the dependency graph stays
  minimal (the crate even hand-rolls its SHA-256 and hex primitives to avoid pulling
  crates — `src/sha256`, `src/hex`).
- Every call pins the legacy `login.keychain-db` path explicitly (keeps the item on
  the classic-ACL path), and `add-generic-password -U` is atomic in place — the
  foundation the no-torn-swap invariant builds on (ADR-0003).
- The secret is fed to `security -i` on **stdin**, never argv, so the blob never
  appears in this process's command line (#39).
- The `apple-tool:`-ride and atomicity are guarded by CI (the SDK-linkage guard) and
  re-checked manually against a live token on Claude Code auth bumps (`src/swap.rs`
  § deferred live checks).

### Negative / trade-offs

- **Subprocess overhead** (fork/exec) per keychain operation, and **parsing text**
  (`security dump-keychain` output — handling both quoted-string and `0x`-hex
  attribute rendering) instead of a typed API. Accepted: operation frequency is low
  (poll/swap cadence) and correctness dominates.
- **Couples to the `security` CLI's observable behavior and output format** — a macOS
  or Claude Code change could shift it. Mitigated by the empirical ledger
  (`build/version-compat.md`) and the deferred live oracles.
- macOS / login-keychain specific by construction.

## Related

- ADR-0003 (no-torn-swap): the atomic `add-generic-password -U` write is the
  invariant's commit point.
- Code: `src/keychain.rs`, `scripts/check-no-security-framework.sh`,
  `.github/workflows/ci.yml`, `build/version-compat.md`.
