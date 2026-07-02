---
type: architecture-decision-record
number: 4
title: "Incidental libc FFI kept raw (no wrapper crate)"
date: 2026-07-02
status: accepted
decision_makers: [Oleksii PELYKH (maintainer)]
---

# ADR-0004: Incidental `libc` FFI kept raw (no wrapper crate)

## Status

**Accepted** — 2026-07-02. Records a decision to **keep the current behaviour** and
capture the rationale (issue #180, "Decision, not a defect"; priority Low). Same
posture as ADR-0001/0002/0003 — a decision in force, not a code change. The forward
path for the `flock` sites is tracked separately in #257.

## Context

Beyond the load-bearing **security** FFI — `getpeereid` / `getuid` / `getpwuid`
(`src/daemon.rs`, `src/paths.rs`), deliberately raw and **out of scope** here — the
crate makes a handful of **incidental** `unsafe libc` calls. Issue #180 asks whether
they should be replaced by a safe-wrapper crate:

- **Terminal-width probe** — `ioctl(STDOUT_FILENO, TIOCGWINSZ, &mut winsize)` in
  `terminal_cols()` (`src/cli.rs`; 2 `unsafe` blocks).
- **Advisory file locks** — `flock(fd, LOCK_EX | LOCK_NB)` with return/`errno`
  handling, at **three** sites in three files: `SwapLock::acquire` (`src/swap.rs`),
  `StoreLock::acquire` (`src/usage_store.rs`), `InstanceLock::acquire`
  (`src/daemon.rs`).

Two facts frame the decision:

1. **Wrapping does not clear the incidental surface.** Replacing `ioctl` + `flock`
   removes ~5 `unsafe` blocks but leaves the `termios` ECHO-suppression
   (`src/migration.rs`) and a `localtime_r` timestamp (`src/stats.rs`) — the latter
   outside `rustix`'s domain. The crate stays an `unsafe`-using crate either way.
2. **`rustix` is dev-only today.** `cargo tree -i rustix` resolves it solely under
   `tempfile` (`[dev-dependencies]`). Adopting `rustix::fs::flock` / `rustix::termios`
   **promotes a new production dependency** (`rustix` + `bitflags`) into the release
   build — it is not "already there."

This runs against a minimalism posture that is **explicit and CI-enforced**, not
merely implied: `Cargo.toml` justifies `lexopt` over `clap` (the "~10-crate weight")
and admits `unicode-width` only for having ZERO transitive deps; the crate hand-rolls
curl / SHA-256 / hex to avoid dependencies; **ADR-0002** chose the `security`
subprocess over an FFI binding for the same reason.

The soundness of the in-scope calls is not in question. Each `flock` operates on an fd
owned by a `File` that outlives the call plus two constant flags; the `ioctl` writes
into a zero-initialized POD `winsize` and checks the return. No attacker-controlled
input reaches any in-scope site, every one already carries an accurate `// SAFETY:`
comment, and there is **no latent unsoundness** a wrapper would fix.

## Decision

**Keep the incidental `libc` FFI raw. Do not add `rustix` (or `fs2` / `fd-lock` /
`terminal_size`) as a production dependency.** The `unsafe` here is minimal, POD-only,
trivially sound, and already documented; hiding it behind a crate trades a
well-understood in-tree surface for an out-of-tree one, against the crate's deliberate
minimalism and in the same spirit as ADR-0002.

The `ioctl(TIOCGWINSZ)` probe stays raw **permanently**: it has no std equivalent
(unlike `isatty` → `IsTerminal`, #178), so a whole crate for one POD probe is
disproportionate.

For the three `flock` sites, "don't reinvent the wheel" points not at `rustix` but at
**`std::fs::File::try_lock`** (stabilized in Rust 1.89) — std, zero deps, zero
`unsafe` — mirroring the `isatty` → `IsTerminal` (#178) and `unicode-width` wins the
crate already banked. That migration is gated only on an MSRV bump to ≥ 1.89 (current
MSRV is 1.87; CI stable already clears it) and is **tracked in #257**. Until then the
`flock` sites stay raw.

To satisfy issue #180's acceptance criterion that the choice **read as intentional**,
each raw site carries a one-line ADR-pointer above its existing `// SAFETY:` block.

## Alternatives considered

1. **Adopt `rustix` (or `terminal_size` / `fs2` / `fd-lock`); wrap `ioctl` + `flock`.**
   - **Pros**: removes ~5 `unsafe` blocks; a typed lock guard; one shared abstraction.
   - **Cons**: promotes a **new production dependency** into the release build, against
     a minimalism the crate enforces to the point of hand-rolling crypto and HTTP. It
     clears only ~5 of ~10 incidental `unsafe` blocks (`termios` / `localtime_r`
     remain), so it does **not** reach "no incidental `unsafe`". The `unsafe` it removes
     has no soundness defect, so the safety benefit is ~nil — the gain is cosmetic.
     `terminal_size` / `fd-lock` mostly funnel back to `rustix`; `fs2` is effectively
     unmaintained.
   - **Why rejected**: pays a standing dependency cost for a cosmetic reduction that
     does not even clear the incidental surface, and cuts against ADR-0002.
2. **Migrate `flock` to `std::fs::File::try_lock`** (std, zero deps, zero `unsafe`).
   - **Pros**: retires 3 `unsafe` blocks with **no** dependency — the idiomatic "std
     wheel" replacement, consistent with #178 and `unicode-width`.
   - **Cons**: gated on an MSRV bump to ≥ 1.89 (drops 1.87/1.88 support); one site
     (`StoreLock`) has a bounded-retry + `EINTR` contract whose std equivalence must be
     verified.
   - **Why (deferred, not rejected)**: this is the **preferred** long-term shape, but
     it is a code change blocked on the MSRV decision — recorded as its own item (#257),
     not this ADR. This ADR keeps the sites raw *until* that bump.
3. **Keep raw, change nothing (not even comments).**
   - **Cons**: fails #180's acceptance criterion — a future reader cannot tell the raw
     FFI is a deliberate choice rather than an oversight, and may re-litigate it.
   - **Why rejected**: the decision is cheap to make legible.

## Consequences

### Positive

- **Minimalism preserved.** No new production dependency; the release crate graph is
  unchanged. Coherent with ADR-0002 and the `Cargo.toml` dependency justifications.
- **The `unsafe` stays in-tree and auditable.** POD-only `libc` calls with accurate
  `SAFETY` comments are a smaller, more legible trust surface than an external crate.
- **The choice is legible.** The ADR-pointers make #180's finding self-answering for
  the next auditor, and name the tracked std forward path (#257).

### Negative / trade-offs

- **The crate keeps hand-maintained `unsafe`.** The `SAFETY` invariants (fd outlives
  the call; POD is zero-initialized) are enforced by review, not the type system — the
  standing cost of the minimalism posture.
- **`flock` logic stays duplicated across three sites** until the std migration (#257)
  collapses them; three near-identical `SAFETY` comments could drift in the meantime.
- **Bounded to the current platform.** These are Unix/macOS `libc` calls; a future
  non-macOS target would revisit this (as it would most of the crate).

## Related

- **ADR-0002** (keychain via `/usr/bin/security` CLI, zero FFI): the sibling
  minimalism/FFI decision. Same value (minimal, auditable surface), opposite direction
  on FFI.
- **Issue #178**: moved `isatty` → `std::io::IsTerminal` (the precedent this ADR
  extends to `flock` → `std::File::try_lock`).
- **Issue #257**: the tracked `flock` → `std::fs::File::try_lock` migration, gated on
  MSRV ≥ 1.89.
- **Out of scope**: `getpeereid` / `getuid` / `getpwuid` — load-bearing security FFI,
  deliberately raw.
- **Code**: `src/cli.rs` (`terminal_cols`), `src/swap.rs` (`SwapLock`),
  `src/usage_store.rs` (`StoreLock`), `src/daemon.rs` (`InstanceLock`); `Cargo.toml`
  (dependency-minimalism comments).
