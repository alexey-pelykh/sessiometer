---
type: architecture-decision-record
number: 29
title: "Linux is a supported build target for the CLI and daemon; the menu-bar app stays macOS-only"
date: 2026-09-08
status: accepted
decision_makers: [Oleksii PELYKH (maintainer)]
---

# ADR-0029: Linux is a supported build target for the CLI and daemon — the menu-bar app stays macOS-only

## Status

**Accepted** — 2026-09-08 (issue #962, under umbrella #961). Amended in place; the number and the
`accepted` status are unchanged, the title and filename are not.

> **Arriving from issue #797's closing comment, which describes this ADR as recording "macOS is the
> only supported build target"?** That decision — accepted here on 2026-07-28, branch (b) of #797 —
> is reversed, and this file was amended in place rather than superseded:
> `git log -- docs/adr/0029-macos-is-the-only-supported-build-target.md` carries the superseded text
> in full, under the name this file had until then.

The reversal is a **decision in force, not a landed port**. See § Decision part 4: the crate does not
build for Linux today, and nothing enforces that it will until #964 lands. This ADR states what is
decided; it does not claim a guarantee no gate checks — which was the one thing its superseded text
got right, and is preserved here deliberately.

## Context

Two facts reversed the 2026-07-28 decision. Both were measured under umbrella #961, in Docker
(`rust:1.96-bookworm`, aarch64) with the repo mounted read-only.

### 1. The cost premise was wrong

The 2026-07-28 branches were weighed partly on porting cost — a "larger commitment" whose compile
gate was "the smallest part of it". Measured against the 120,693-line crate, the entire Linux build
barrier is **two files, roughly 45 lines**:

| Gate | Unpatched `main` | Two files patched |
|---|---|---|
| `cargo check --all-targets` | **1** error | 0 errors |
| `cargo test` (run **non-root**) | — | **1928 pass / 1 fail** (macOS baseline: 1945 pass) |
| `clippy --all-targets --all-features -D warnings` | — | 1 error (`keychain.rs:291` — `for_test` is dead code on Linux) |
| `RUSTDOCFLAGS="-D warnings" cargo doc` | — | clean |

Two measurement traps produce false readings, and both were hit while taking the numbers above:

- **The container must run non-root.** As root, 9 further tests "fail": the canary/drift tests
  `chmod 0500` a tempdir to block a co-write, root ignores DAC, the heal succeeds, and the verdict
  legitimately flips `Drift` → `Ok`. Nothing is wrong with the code — and the CI job (#964) must not
  run as root either.
- **`cargo test`, never `cargo check` alone.** Why, immediately below.

### 2. The superseded Context named one portability site; there are two

That text named `libc::getpeereid` in `src/daemon/peer_auth.rs` as *the* hole. Both sites, in full:

**Site 1 — `src/daemon/peer_auth.rs` (`peer_euid`).** `libc::getpeereid` called with no
`cfg(target_os)` gate:

```rust
let rc = unsafe { libc::getpeereid(fd, &mut euid, &mut egid) };
```

`getpeereid(3)` is a macOS/BSD call and is not in glibc. This is the **1 error** a Linux
`cargo check --all-targets` reports on unpatched `main`. The Linux equivalent is `SO_PEERCRED` via
`getsockopt`.

**Site 2 — `src/contract.rs`.** Three Mach clock symbols hand-declared in an `extern "C"` block, for
the sleep-inclusive clock of issue #624:

```rust
extern "C" {
    fn mach_absolute_time() -> u64;
    fn mach_continuous_time() -> u64;
    fn mach_timebase_info(info: *mut MachTimebaseInfo) -> i32;
}
```

**`cargo check` cannot detect site 2.** An `extern` block declares a symbol; it resolves at **link**,
not at type-check. So `cargo check` compiles this clean on a Linux target and it fails only when
something actually links — under `cargo test`, or `cargo build`. Issue #797 and the superseded text
of this ADR were both verified with `check`, which is exactly why both named only site 1 and
described it as the sole hole.

That has a standing consequence beyond this reversal, and it is the sharpest thing in this record:
**a `check`-only Linux CI job would report green on a crate that cannot link.** That is a false
gate — strictly worse than the honest, stated gap the superseded decision documented, because a
false green is believed. #964 carries "builds and tests, not just checks" as an explicit acceptance
criterion for precisely this reason.

### 3. Headless Linux is the primary target, and it simplifies the port

WSL2, servers, containers and CI are all headless. Three properties, each verified under #961, make
that the easier target rather than the harder one:

- **`login` already works headless by design.** `src/login.rs` inherits the operator's terminal
  ("never a mediated pty") and gates on stdout being a TTY, so the OAuth URL renders to the operator,
  who completes the browser handoff on whatever machine they are sitting at. No browser is needed on
  the Linux host.
- **`CLAUDE_CONFIG_DIR` relocates Claude Code's state wholesale** — config, projects, backups and
  sessions all follow it, with nothing left at the default location. Per-directory isolation is
  therefore structurally simpler on Linux than the macOS scheme.
- **Two macOS surfaces have no headless Linux counterpart and are product decisions, not ports**:
  `KeychainLocked` cannot occur on Linux, and `osascript` notifications have no headless surface.
  Both are tracked separately.

### 4. What did not change

The menu-bar app is a SwiftUI/AppKit application with no Linux analogue. `launchd`/`SMAppService`,
TCC, Developer ID signing and notarization, and the Homebrew formula remain macOS-only surfaces. The
superseded text was right that the *product* is macOS-bound in substance; what it over-generalised
was the **crate** — the CLI and daemon do not depend on any of those to build, link or test.

## Decision

**Linux is a supported build target for the CLI and daemon.** Four parts:

1. **The supported-target boundary, exactly.** The **CLI and daemon** (`src/**`, the Rust crate at
   the repo root) build, link, test, clippy and doc on **Linux** and on **macOS**. The **menu-bar
   app** (`apps/menubar/**`) stays **macOS-only** and is untouched by this decision. **Windows is
   unchanged and out of scope** — #27 stays blocked on its own recon and #40's Windows half stays
   open. Linux first.

2. **Both portability sites are named, and the second one is named as the trap it is.** `cargo check`
   resolves site 1 and is structurally blind to site 2, so a `check`-only verification of Linux
   support is a false green. Any claim about the Linux build must be backed by something that
   **links** — `cargo test` or `cargo build`.

3. **The support claim is stated where a contributor will see it** — `README.md`
   (§ Build from source) and `CONTRIBUTING.md`, alongside this ADR, and at the `getpeereid` call
   site in `src/daemon/peer_auth.rs`, whose comment now records a port that is owed rather than a
   consequence that was accepted.

4. **Nothing here claims the crate builds on Linux today, because it does not.** The port is #963
   and the enforcing CI job is #964; both are open as this lands. Until #964 is green, **no gate
   enforces a Linux guarantee** — so an acceptance criterion asserting one is still unfalsifiable,
   and still must not be written. That prohibition is inherited unchanged from the superseded text
   and is retired by #964 landing, not by this ADR.

Deliberately **not** done here: no code change beyond the comment in part 3. The syscall port is
#963; the CI job is #964; `docs/adr/0006` (migration artifact) is silent on platform and its
mac-export → Linux-import gap is #965. Splitting them is the point — a green CI job on an unported
crate and a port with no gate are both failure modes, and one issue covering both would hide
whichever it did second.

## Alternatives considered

1. **Write a successor ADR and mark 0029 `Superseded`.** The repo has that convention
   (ADR-0022 → ADR-0023, with `status: superseded` + `superseded_by:` / `supersedes:` frontmatter).
   - **Pros**: matches the § Conventions text in `docs/adr/README.md` literally, and leaves the
     2026-07-28 reasoning readable in place rather than only in git.
   - **Cons**: that precedent does not fit. ADR-0023 states outright that it *"preserves that
     decision and supersedes only its record of the meaning"* — 0022 still carries live content, so
     both documents earn their place. This is a **full reversal**: nothing in the original survives.
     A superseded 0029 would be a document titled "macOS is the only supported build target" whose
     entire content is "this is no longer true, see 0030". That is a redirect, not a record — and
     git already versions the file, so the supersession chain would duplicate history the repo keeps
     anyway.
   - **Why rejected**: the repo does not supersede reflexively (ADR-0005 documents a deliberate
     decision *not* to), and four index rows already record in-place amendment as house practice
     (0006, 0012, 0020, and 0002 by a successor). The filename was changed with `git mv` for the
     same reason the successor was rejected: a filename that contradicts its contents is worse than
     the stub being avoided.

2. **Land the port (#963) and the CI job (#964) in this change, so the decision and its enforcement
   arrive together.**
   - **Pros**: no window in which the record claims a supported target that nothing builds or
     verifies.
   - **Cons**: three unrelated review surfaces in one diff — a decision record, two syscall sites,
     and a CI matrix change — where the CI job is the one that must be demonstrated RED against the
     unported crate before it means anything. Bundling makes that demonstration impossible to see.
   - **Why rejected**: the window is closed honestly instead. Decision part 4 states that no gate
     enforces the guarantee yet, which is the same discipline the superseded text established and
     the reason this ADR does not simply assert Linux support.

3. **Keep macOS-only; treat the two-file measurement as insufficient reason to reverse.**
   - **Cons**: the measurement falsifies the premise the original rejection rested on. "The larger
     commitment" was ~45 lines across two files, and the resulting suite passes 1928 of 1929 tests
     non-root. Holding the decision would leave the tracker's own cross-platform roadmap (#40 → #25
     → #26/#28) sequenced behind a barrier that was measured and found not to be there.
   - **Why rejected**: the falsifier was run, and it came back against the decision.

## Consequences

### Positive

- **The cost of the port is known, not estimated.** Two files, ~45 lines, with the full gate matrix
  measured on both sides of the patch. #963 and #964 are scoped against numbers rather than against
  a guess.
- **The `check`-is-blind trap is recorded before anyone can fall into it again.** It has already cost
  two verifications (#797 and this ADR's superseded text). #964's acceptance criterion is derived
  from it directly.
- **The cross-platform track is unblocked with its constraints intact.** #40's Linux half is
  answered, #25/#26/#28 are unblocked, and the ratified constraints travel with them: `etcetera` for
  path resolution (`directories` and `dirs` were evaluated and rejected), the `keyring` crate stays
  banned, files `0600` and directories `0700` on every platform, solely-controlled hosts only.
- **Claims still match gates.** The reversal did not license a Linux guarantee; part 4 states the
  absence of one explicitly. A green CI run today still says nothing about Linux.

### Negative / trade-offs

- **There is a window where the record and the tree disagree, and it is deliberate.** `main` does not
  build for Linux until #963 lands, and nothing enforces that it stays building until #964 lands.
  Anyone reading only the title of this ADR gets a decision, not a capability. Part 4 exists to close
  that gap in the reader's mind; nothing closes it mechanically until #964.
- **Standing CI cost on every PR for a second platform.** Accepted: this is what buys the enforcement
  the superseded decision explicitly did not have, and the alternative — batched portability work
  discovered all at once — is what let two portability sites accrete with only one of them noticed.
- **The claim that credentials "ride the encrypted OS keychain" is macOS-only and must not be
  repeated in any Linux-facing doc, help text or README section.** Linux has no keychain here; the
  stash stays plaintext at `0600`, matching Claude Code's own posture on Linux. Self-encryption is
  explicitly not in scope (#28). Nothing enforces this — it is a review obligation.
- **`src/paths.rs` carries three macOS-only test assumptions whose stated reasoning cites the
  superseded decision, and this change leaves them exactly as they are.** Their premise — "the crate
  does not compile for Linux at all, so a `#[cfg(target_os = "macos")]` gate would be inert today" —
  is still true as this ADR lands, so the conclusion those comments reach survives on it. **#963 is
  what falsifies that premise**, and re-deciding those three gates against a crate that does build on
  Linux belongs to that item. Recorded here so it is inherited rather than rediscovered: a live
  `/bin/sh -l -c /usr/bin/env` spawn (`dash` does not treat `-l` as macOS's `/bin/sh` does), and two
  tests reading the host's live passwd entry (a minimal container image need not populate it).
- **One test and one clippy finding are known-red on Linux before #963 opens.** The measurement
  recorded 1 failing test out of 1929 and one `-D warnings` clippy error at `keychain.rs:291`
  (`for_test` is dead code on Linux). Neither is a surprise to be discovered mid-port.

## Related

- **Issue #962**: this amendment. **Umbrella #961**: the ratified direction, the measured evidence,
  and the constraints that travel with every child item.
- **Issue #797**: the platform question, closed on branch (b) — the decision this ADR previously
  recorded and now reverses. Its closing comment carries the reversal notice and points here.
- **The port and its gate**: **#963** (port the two macOS-only syscall sites so the crate builds and
  links on Linux), **#964** (a Linux CI job that builds and tests, not just checks — deliberately
  separate from #963).
- **Credential tier, unblocked by the #961 recon**: **#40** (per-platform credential-store recon —
  Linux half answered, stays open for Windows), **#25** (backend-neutral credential-store seam),
  **#26** (Linux credential swap — collapses to an atomic file replace), **#28** (cross-platform
  at-rest hygiene — settled as plaintext `0600`, no self-encryption). **#27** (Windows credential
  swap) stays blocked; **#29** retains packaging and distribution beyond CI.
- **Issue #965**: a mac-exported migration artifact must import correctly on Linux. `docs/adr/0006`
  (migration-artifact schema-evolution policy) is silent on platform; that gap is #965's, and is
  deliberately not folded in here.
- **ADR-0004** (incidental `libc` FFI kept raw): the topically adjacent decision. `getpeereid` is one
  of the load-bearing security-FFI sites ADR-0004 explicitly holds out of scope, and its own
  trade-offs already noted that the raw `libc` surface is "bounded to the current platform… a future
  non-macOS target would revisit this." #963 is that revisit.
- **The macOS-bound decisions this one does *not* disturb**: **ADR-0002** (keychain via the
  `/usr/bin/security` CLI), **ADR-0010** (macOS app repo topology), **ADR-0021** (Homebrew tap
  topology), **ADR-0027** (macOS app bundle identity). Each governs a surface that stays macOS-only.
- **Code**: `src/daemon/peer_auth.rs` (`peer_euid` — site 1), `src/contract.rs` (the `extern "C"`
  Mach block — site 2, invisible to `cargo check`), `src/paths.rs` (the three documented macOS-only
  test assumptions, and the pre-existing issue-#24 `#[cfg(windows)]` / `#[cfg(target_os = "macos")]`
  path-strategy gates, all left untouched), `.github/workflows/ci.yml` (the job/runner mapping #964
  extends), `README.md` (§ Build from source), `CONTRIBUTING.md` (§ Supported platforms).
