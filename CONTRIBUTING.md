# Contributing to sessiometer

Thanks for your interest in improving `sessiometer`. For building and running the
tool see the [README](README.md); for the *why* behind load-bearing technical
decisions see the [ADRs](docs/adr/). This guide covers the one thing most likely
to surprise a new contributor: the project holds a deliberate **minimal-dependency
line**, and several primitives you might expect to be crates are hand-rolled on
purpose.

If you are about to add a dependency, or "helpfully" swap a hand-rolled primitive
for a well-known crate (`clap`, `hex`, `reqwest`, …), please read this first — the
omission is intentional, not an oversight.

## The minimal-dependency line

`sessiometer` reads and rewrites the credential Claude Code stores in your macOS
login keychain. Every dependency is therefore part of a **credential-adjacent
supply chain** — code that ships in a binary handling live secrets. The goal is to
keep that surface small and auditable: few direct dependencies, shallow transitive
trees, and no dependency pulled in for something the crate can do itself in a few
well-understood lines.

Concretely, the tree deliberately has **no** heavy client stacks:

- **No TLS / HTTP client** (`reqwest`, `hyper`, `rustls`, `native-tls`) — the one
  network call rides the system `curl` (see [the transport rule](#system-clis-not-client-crates-the-transport-rule)).
- **No argument-parsing framework** (`clap`) — a zero-dependency argv lexer plus
  our own routing does the job (see [when a crate is warranted](#when-a-crate-is-warranted)).
- **No date/time crate** (`chrono`, `time`) — the civil-date math is hand-rolled.
- **No keychain FFI binding** (`security-framework`) — enforced by a CI guard (see
  [guards](#guards-and-where-the-rationale-lives)).

The authoritative, always-current picture is the code, not this document:

- [`Cargo.toml`](Cargo.toml) — each direct dependency carries a comment explaining
  why it earns its place.
- [`deny.toml`](deny.toml) + `cargo deny check advisories sources licenses` — gates
  that dependencies come from crates.io and carry an allow-listed license.
- `cargo tree` — the actual graph at any moment.

## Hand-rolled primitives (and why)

These live in the crate instead of as dependencies. Each is small, stable, and
well-specified — the kind of thing where a runtime dependency buys little and costs
supply-chain surface:

| Primitive | Home | Why hand-rolled |
|-----------|------|-----------------|
| SHA-256 (FIPS 180-4) | [`src/sha256.rs`](src/sha256.rs) | Derives the keychain service-name suffix (replicating Claude Code's `sha256(CLAUDE_CONFIG_DIR)[..8]`) and a test-only redaction fingerprint. A cryptographic hash is the wrong thing to pull a runtime dependency in for; verified against the NIST vectors in-module. |
| Lowercase hex codec | [`src/hex.rs`](src/hex.rs) | Secrets must stay pure-ASCII so the keychain round-trip renders them as text, not as their own `0x`-hex blob. A two-digit-per-byte codec is the wrong thing to pull a dependency in for. |
| Civil-date math | `days_from_civil` / `civil_from_days` ([`src/usage.rs`](src/usage.rs), [`src/observability.rs`](src/observability.rs)) | Epoch-seconds ↔ civil-date conversion via Howard Hinnant's algorithms — so there is no date crate in the graph. |
| Jitter PRNG (SplitMix64) | [`src/timing.rs`](src/timing.rs) | Poll-cadence decorrelation noise, **not** a security primitive, so a tiny deterministic generator is exactly right — and it keeps the `cargo deny` advisory surface empty. |

The rule of thumb: **hand-roll a small, well-specified primitive rather than pull a
crate for it — but do not hand-roll something a maintained crate does more
correctly.** `unicode-width` (below) is that second clause in practice.

## System CLIs, not client crates (the transport rule)

Where `sessiometer` talks to the outside world, it **prefers a system CLI at an
absolute path, with secrets fed on stdin, over a client crate**:

- **Keychain access** goes through [`/usr/bin/security`](src/keychain.rs) (also
  [`src/stash.rs`](src/stash.rs)), never the Security.framework SDK. Here the reason
  is more than dependency count: writing the item through the SDK as our own code
  identity re-stamps its ACL and evicts the `apple-tool:` entry, breaking Claude
  Code's silent read — the CLI write rides `apple-tool:` and preserves it. The full
  rationale is [ADR-0002](docs/adr/0002-keychain-via-security-cli-zero-ffi.md).
- **The usage poll** rides [`/usr/bin/curl`](src/usage.rs) — **not** an HTTP client
  crate such as `reqwest`. `curl` is always present on macOS, so no TLS/HTTP stack
  enters the dependency graph for a single read-only `GET`.

Two disciplines both calls share, and any new external call should follow:

1. **Absolute path** (`/usr/bin/security`, `/usr/bin/curl`), never `$PATH`-resolved
   — a hijacked `PATH` cannot substitute a different binary for a
   security-sensitive call.
2. **Secrets on stdin, never argv** — the bearer token / secret never appears in
   this process's command line (`curl --config -`; `security -i`), so it cannot leak
   via `ps` or process listings.

## When a crate is warranted

Sometimes a crate genuinely is the right call. When it is, **prefer a zero-/low-
dependency crate over a heavy tree.** Two crates in the current graph are the model:

- **`lexopt`** (0 transitive dependencies) — an argv lexer that makes the argv layer
  strict (unknown flags and malformed usage become clear errors) **without** the
  ~10-crate weight of `clap`. We still own subcommand routing, help text, and error
  wording on top of it. (See issue #175.)
- **`unicode-width`** (0 transitive dependencies) — the canonical UAX #11
  display-width table, which replaced a hand-rolled `wcwidth` approximation that
  mis-measured emoji, ZWJ sequences, regional-indicator flags, skin-tone modifiers,
  and variation selectors. It is *strictly more correct* and has ~nil
  dependency-count impact — the case where reaching for a solved-and-versioned crate
  beats reinventing it. (See issue #176.)

Before adding a dependency, weigh:

- **Transitive weight** — what does `cargo tree` show it dragging in?
- **Credential adjacency** — does it end up in the binary that touches secrets?
- **License** — is it on the `deny.toml` allow-list? A new license fails
  `cargo deny check licenses` until reviewed and added.
- **Source** — it must resolve from crates.io; a git or alternate-registry source
  fails `cargo deny check sources` until vetted.

## Guards and where the rationale lives

- [`scripts/check-no-security-framework.sh`](scripts/check-no-security-framework.sh)
  — a CI guard (the `deny` job in
  [`.github/workflows/ci.yml`](.github/workflows/ci.yml)) that fails the build if
  `security-framework` appears anywhere in the dependency graph, so a refactor
  cannot silently reintroduce the SDK write path.
- [`scripts/check-menubar-zero-egress.sh`](scripts/check-menubar-zero-egress.sh)
  — the Swift-side peer of the guard above (the `swift` job in
  [`.github/workflows/ci.yml`](.github/workflows/ci.yml)). The menu-bar app is a
  pure local-socket client — it reaches the daemon over a raw POSIX AF_UNIX socket
  only ([ADR-0011](docs/adr/0011-menubar-transport-raw-posix-af-unix.md)), never the
  host network or the keychain — and this fails the build if `apps/menubar/Sources`
  grows a `Security`/`Network`-framework import, a host-networking symbol
  (`URLSession`, `NWConnection`, …), or a network entitlement. Like the daemon
  guard it works at the source (build-input) level, not on the linked binary (issue
  #328).
- [`scripts/check-gate-change-ack.sh`](scripts/check-gate-change-ack.sh)
  — a CI guard (the `gate-change-ack` job in
  [`.github/workflows/ci.yml`](.github/workflows/ci.yml)) that fails the build
  when a PR touches a gate-definition path (`.github/workflows/**`, `scripts/**`,
  `deny.toml`, `.cargo/**`) without a `Gate-Change-Acknowledged: <reason>` trailer
  on one of its commits, so a change to the merge gate's own definition lands
  deliberately and auditably rather than slipping through green in this solo repo
  (issue #317).
- `cargo deny check advisories sources licenses` — the supply-chain gates configured
  in [`deny.toml`](deny.toml).
- [`docs/adr/`](docs/adr/) — Architecture Decision Records for the load-bearing
  choices, including [ADR-0002](docs/adr/0002-keychain-via-security-cli-zero-ffi.md)
  on keychain-via-CLI.
