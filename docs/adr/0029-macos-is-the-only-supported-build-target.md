---
type: architecture-decision-record
number: 29
title: "macOS is the only supported build target; cross-platform support is tracked, not claimed"
date: 2026-07-28
status: accepted
decision_makers: [Oleksii PELYKH (maintainer)]
---

# ADR-0029: macOS is the only supported build target — cross-platform support is tracked, not claimed

## Status

**Accepted** — 2026-07-28. Records a decision to **keep the current behaviour** and state
it plainly (issue #797, filed deliberately as a question rather than a fix). Same posture
as ADR-0001/0002/0003/0004 — a decision in force, not a code change. The cross-platform
forward path is tracked separately and stays open (#25, #26, #27, #28, #29, #40).

## Context

Two facts, both verified during issue #783 (PR #796) and re-verified here, contradict a
premise two work items (#783, #784) were scoped against.

**1. No CI job compiles the crate on Linux.** The jobs that run `cargo build` / `test` /
`clippy` / `doc` are `test` and `msrv`, and both are `runs-on: macos-latest`. So are
`swift`, `panel-goldens`, and `formula`. Every `ubuntu-latest` job is a gate or a
router, and none of them compiles the crate:

| Job | `runs-on` | Compiles the crate? |
|---|---|---|
| `test` | `macos-latest` | ✅ build + test + clippy + doc |
| `msrv` | `macos-latest` | ✅ build + test |
| `swift`, `panel-goldens`, `formula` | `macos-latest` | ❌ (Swift app, panel goldens, formula lint — no `cargo`) |
| `changes`, `deny`, `ci-ok-needs-complete`, `gate-change-ack`, `ci-ok` | `ubuntu-latest` | ❌ |

**2. `main` already fails to build for a Linux target.** `src/daemon/peer_auth.rs`
(`peer_euid`) calls `libc::getpeereid` with **no `cfg(target_os)` gate**:

```rust
let rc = unsafe { libc::getpeereid(fd, &mut euid, &mut egid) };
```

`getpeereid(3)` is a macOS/BSD call — it is not in glibc. A `cargo check` for a Linux
target exits 101, identically before and after #783, so the breakage is **pre-existing,
not introduced**.

The practical effect is a **silent coverage gap**: a change that breaks Linux compilation
passes every CI job today. Both were nevertheless scoped with *"the resolver is not
`cfg(target_os = "macos")`-gated and CI runs Linux jobs, so the change must not break the
Linux build."* The first clause is true; the second is false, and the conclusion drawn
from it — that Linux is a covered platform — does not hold.

**The product is macOS-only in substance, not merely by accident of CI.** It depends on
`launchd` and `SMAppService` for the background daemon, the macOS **login keychain** for
the credential it swaps, `getpwuid` for the passwd-derived paths, TCC for the menu-bar
app's permissions, Developer ID signing and notarization for distribution, a Swift
menu-bar `.app`, and a Homebrew formula. None of these has a Linux counterpart in tree.
The shipped metadata already says as much: `Cargo.toml` carries
`description = "Manage multiple Claude Code accounts on macOS: poll per-account usage
quota and swap the active credential out-of-band before exhaustion."` and
`keywords = ["claude", "macos", "keychain", "quota", "cli"]`; `README.md` opens with
"Manage multiple Claude Code accounts on macOS" and lists "**macOS**, using the **login
keychain**" as the first prerequisite.

So the gap this ADR closes is not the platform choice — that was made long ago and is
visible everywhere. The gap is that the choice was never **stated as a decision**, which
let work items assert a Linux guarantee no gate enforces. An asserted guarantee that
nothing verifies is strictly worse than a stated non-guarantee: it misleads the next
contributor, who must re-derive the truth (as the #783 executor did) before trusting an
acceptance criterion.

## Decision

**macOS is the only supported build target today. No CI gate enforces any Linux
guarantee, and none is claimed.** Three parts:

1. **The non-support is stated where a contributor will see it** — `README.md`
   (§ Build from source) and `CONTRIBUTING.md`, alongside this ADR; and at the code sites
   that carry the consequence, `src/daemon/peer_auth.rs` (the un-gated `getpeereid`) and
   the three host-dependent tests in `src/paths.rs`, each of which gains a one-line
   ADR-pointer comment above what is already there. A contributor learns the build target
   from the docs, not by discovering a compile failure.
2. **No work item may assert a Linux build or compile guarantee.** Nothing verifies one,
   so such an acceptance criterion is unfalsifiable by construction. Items may still be
   *written for* future platforms — they simply may not claim the present build works
   there.
3. **The door stays open.** Cross-platform support remains tracked, sequenced future work
   and is untouched by this decision: recon (#40) gates the backend-neutral seam (#25),
   which gates the per-OS mechanisms (#26, #27, #28), which gate productionization (#29).
   This ADR records **what is true today**; it does not retire the ambition, and it does
   not close or relabel any of those items.

Deliberately **not** done: `libc::getpeereid` in `src/daemon/peer_auth.rs` is left exactly
as it is — gating or replacing it is Alternative 1 below, rejected for now.

## Alternatives considered

1. **Support Linux: gate or replace `getpeereid`, then add a Linux CI job that compiles.**
   The Linux equivalent of `getpeereid(3)` is `SO_PEERCRED` via `getsockopt`.
   - **Pros**: turns *"must not break the Linux build"* from an aspiration into an
     enforceable claim; catches non-portable constructs at the commit that adds them,
     rather than in a batch later.
   - **Cons**: the larger commitment, and the compile gate is the smallest part of it. A
     green `cargo check` on Linux would buy a *compile* guarantee for a product that still
     could not **run** there — `launchd`, the login keychain, TCC, notarization, and the
     `.app` have no Linux counterpart, and the credential mechanism itself (#26) is
     blocked on recon that has not happened (#40). It also pays standing CI cost on every
     PR for a platform with no user.
   - **Why rejected (for now, not forever)**: it inverts the honest ordering. Recon (#40)
     → mechanism (#26) → CI and packaging (#29) is the sequence already tracked; adding
     the CI gate first would enforce the one property that does not depend on any of the
     decisions still unmade. This alternative is essentially what #29 becomes when the
     track is picked up.
2. **Change nothing; leave the gap as it is.**
   - **Cons**: work items keep asserting a Linux guarantee nothing enforces, and the next
     contributor re-derives the same finding from scratch — the #783 executor already
     spent the effort once.
   - **Why rejected**: the state actively misleads, and stating the truth is cheap.
3. **Declare Linux unsupported *and* retire the cross-platform track** — drop the
   `cross-platform` label, close #25/#26/#27/#28/#29/#40.
   - **Pros**: maximally consistent with this ADR's own thesis — a six-item roadmap that
     has never moved is itself a claim nothing enforces, and closing it would leave the
     tracker saying only what is true today.
   - **Cons**: conflates "not supported today" with "never intended", and would delete a
     deliberately sequenced roadmap (recon gates mechanism gates productionization) that
     still represents the intent. The falsifier in #40 has not been run, so nothing has
     shown the plan to be wrong — only unstarted.
   - **Why rejected**: this decision is about the present, not the ambition. Part 3 of the
     Decision above states the opposite explicitly.

## Consequences

### Positive

- **Claims match gates.** No acceptance criterion asserts a guarantee CI does not enforce,
  so a green build no longer implies more than it verified.
- **The build target is discoverable.** README and CONTRIBUTING state it up front; a
  contributor does not learn it from a compile error on an unsupported host.
- **The cross-platform track keeps its meaning.** #25–#29 and #40 now read unambiguously
  as *future* work rather than as partially-true-today, which is what made the original
  premise plausible enough to be scoped against.
- **Zero regression risk.** No production code changes; the decision is a record plus
  documentation.

### Negative / trade-offs

- **`main` does not compile for a Linux target, and that is now an ACKNOWLEDGED
  consequence rather than an unnoticed defect.** `src/daemon/peer_auth.rs` keeps the
  un-gated `libc::getpeereid`, so a Linux `cargo check` exits 101. Whoever picks up #26
  meets this first — appropriately, since neutralizing exactly this kind of
  platform-bound seam is what #25 exists to do.
- **Non-portable constructs can keep accreting silently.** With no compile gate, each new
  macOS-only call is invisible until someone attempts a Linux build, and the cost is then
  paid all at once. Accepted deliberately: batched portability work is cheaper than
  standing CI cost for a platform with no user, and #25/#29 are where that batch lands.
- **Test-level platform assumptions are documented, not enforced.** Three tests in
  `src/paths.rs` — a live `/bin/sh -l -c /usr/bin/env` spawn, and two that read the host's
  live passwd entry — record their macOS-only assumptions in comments rather than behind
  `#[cfg(target_os = "macos")]`. A per-test gate would be inert today (the crate does not
  build on Linux at all) and would falsely imply the rest of the suite is portable; the
  comments instead tell a future porter precisely what to re-verify. Because the remedy is
  per-site, it is only as complete as the enumeration behind it — a later test that
  reaches for the live passwd entry or a real `/bin/sh` needs the same annotation, and
  nothing enforces that.

  **This reasoning does not extend to the issue-#24 platform scaffolding.** The existing
  `#[cfg(windows)]` / `#[cfg(target_os = "macos")]` gates in `src/paths.rs` (the per-OS
  path strategy, and the `[target.'cfg(windows)'.dependencies]` block in `Cargo.toml`) are
  also inert on a macOS build, but they are deliberate structure for a tracked future
  platform — not a support claim, and not something this ADR argues for removing. The
  "inert" objection above is specific to bolting a *per-test* gate onto a suite whose
  portability nothing else asserts.

## Related

- **Issue #797**: this decision (branch **(b)**, "Linux is not supported"), filed as a
  question with both branches costed.
- **Issue #783** (PR #796): the login-shell PATH harvest whose executor surfaced both
  facts by refusing to accept the stated premise; also the origin of the two residual
  tests named above. Downstream: **#784**, **#786**.
- **Cross-platform track — OPEN and unchanged in intent**: **#40** (per-platform
  credential-store recon, the build prerequisite and critical falsifier), **#25**
  (backend-neutral credential-store seam), **#26** (Linux credential swap mechanism),
  **#27** (Windows credential swap mechanism), **#28** (cross-platform credential at-rest
  hygiene), **#29** (cross-platform productionization — per-OS CI matrix, packaging,
  docs).
- **ADR-0004** (incidental `libc` FFI kept raw): the topically adjacent decision.
  `getpeereid` is one of the load-bearing security-FFI sites ADR-0004 explicitly holds
  **out of scope**, and its own trade-offs already noted that the raw `libc` surface is
  "bounded to the current platform… a future non-macOS target would revisit this." This
  ADR settles which platform that is today, and shares its posture — a decision in force,
  not a code change.
- **Prior art — the macOS-bound decisions this one makes explicit as a posture**:
  **ADR-0002** (keychain via the `/usr/bin/security` CLI), **ADR-0010** (macOS app repo
  topology), **ADR-0021** (Homebrew tap topology), **ADR-0027** (macOS app bundle
  identity).
- **Code**: `src/daemon/peer_auth.rs` (`peer_euid` — the un-gated `getpeereid`, the site
  that keeps `main` from compiling for Linux), `src/paths.rs` (`login_shell` and the
  login-shell PATH harvest; the documented macOS-only tests; and the pre-existing
  issue-#24 `#[cfg(windows)]` / `#[cfg(target_os = "macos")]` path-strategy gates, which
  this ADR leaves untouched), `.github/workflows/ci.yml` (the job/runner mapping),
  `Cargo.toml` (`description`, `keywords`, and the Windows-only `etcetera` target block
  from #24), `README.md`, `CONTRIBUTING.md`.
