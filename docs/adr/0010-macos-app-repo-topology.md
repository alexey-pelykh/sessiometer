---
type: architecture-decision-record
number: 10
title: "macOS app repo topology — monorepo, first-party daemon, Rust crate at root"
date: 2026-07-06
status: accepted
decision_makers: [Oleksii PELYKH (maintainer)]
---

# ADR-0010: macOS app repo topology — monorepo, first-party daemon, Rust crate at root

## Status

**Accepted** — 2026-07-06. Records how this repo is structured to house the
macOS Swift menubar app (**#168**) alongside the existing Rust crate, and why
introducing `apps/menubar/` does **not** drag the crate into a Cargo workspace.

Unlike ADR-0008 and ADR-0009 — which record **shipped** behavior changes — this
ADR records a **topology decision that precedes the code**: #168's Swift app and
#171's packaging do not exist yet, and this record governs where they will land.
In that sense it is closer to ADR-0007's **decided-against** register: it decides
**against** a separate app repo, **against** relocating the crate, and **against**
a Cargo `[workspace]` introduced now. The outcome was reached across three
deliberations — a **repo-topology council**, a **layout council**, and an
adversarial `/evaluate` that selected **Option B** (keep the crate at root).

## Context

sessiometer is today a **single Rust crate at the repo root**: a bare
`[package]` in `Cargo.toml` (no `[workspace]`), producing the CLI and the daemon.
The macOS menubar app (**#168**) is the next surface: a status-bar app that is a
**pure local-socket client**. It talks to the daemon over **AF_UNIX** using the
**frozen, versioned JSON status snapshot** (**#164** — `schema_version` +
`generated_at`); it links **no** Rust. The socket contract is the entire boundary
between the two languages — there is no FFI and no shared build graph.

**#171** then packages the two together: it **embeds** the daemon binary inside
the `.app` and **signs + notarizes + staples** the whole thing **as one unit** —
a single, version-locked release artifact for the GUI channel. (The same crate
also ships headless via the **#269** Homebrew formula, whose source tarball is
rooted at the crate.)

Two questions fell out of that shape, and this ADR answers both:

1. **Where does the Swift app live** — in **this** repo, or a separate one?
2. **Does adding `apps/menubar/` imply a Cargo `[workspace]`** — moving the crate
   under a parallel path (e.g. `crates/daemon/`) for "symmetry" with the app?

## Decision

1. **Monorepo — the Swift menubar app lives in *this* repo.** #168 and the UI
   chain are directories here, not a separate repository. #171 notarizes the app
   and the embedded daemon **as one unit**, so the shipped artifact is a single,
   version-locked thing; a single artifact belongs in a single repo, built and
   released together.

2. **First-party daemon — an implementation detail of the app, not a separate
   product.** The daemon is **embedded** in the `.app` and notarized with it
   (#171). It is **not** an independently-distributed, separately-versioned
   product living in its own repo with its own release cadence. It is version-
   locked to the app in the GUI channel (and the very same crate, at root, feeds
   the #269 headless channel).

3. **The Rust crate stays at the repo root; the Swift app goes in
   `apps/menubar/`; no Cargo `[workspace]` is introduced.** The crate does **not**
   move. `apps/menubar/` is a **directory** convention for the Swift surface — it
   is **not** a Cargo workspace member and can never be one (it is Swift). The
   desired "symmetry" is therefore a matter of directories, not of Cargo.

4. **Defer the Cargo workspace (and any crate relocation) until a *second* Rust
   crate actually exists.** A single-member workspace is ceremony; a workspace
   earns its keep only with real members.
   - **DEFER trigger**: a **2nd Rust crate** appears — for example a
     `core`/`daemon` split extracted for #171's embed, or an `xtask` build
     helper. At that point, introduce the `[workspace]` and relocate as needed,
     with genuine members to justify it (and set `resolver = "2"` explicitly — see
     the trade-off below).

## Alternatives considered

1. **Separate repo for the Swift app (polyrepo).** The app and its UI chain live
   in their own repository; the daemon is consumed as a released dependency.
   - **Pros**: each language gets a "clean" single-toolchain repo; CI for Rust
     and CI for Xcode never share a tree.
   - **Cons**: #171 ships the app **and** the embedded daemon as **one**
     notarized, version-locked artifact. A polyrepo splits the source of a single
     shipped unit across two repos, forcing cross-repo version pinning and release
     choreography for every build — friction with no offsetting benefit, since the
     socket contract (#164) already decouples the two at runtime.
   - **Why rejected**: one shipped artifact wants one repo. The runtime boundary
     is the socket, not a repo boundary; splitting the repo buys nothing and costs
     release coordination on every notarized build.

2. **Introduce a Cargo `[workspace]` now** — relocate the crate (e.g. to
   `crates/daemon/`) so it sits under a parallel path "symmetric" with
   `apps/menubar/`.
   - **Pros**: superficial visual symmetry — every component under a parallel
     top-level directory; room "pre-built" for a future second crate.
   - **Cons**: `apps/menubar/` is **Swift** and can never be a workspace member,
     so the workspace would have exactly **one** member — pure ceremony. Worse,
     relocating the crate is **not free**: it breaks the **#271** no-telemetry
     security test, which reads `Cargo.lock` via `env!("CARGO_MANIFEST_DIR")`
     (`src/usage.rs:1286`, `src/usage.rs:1379`) — an anchor a move disturbs; it
     risks a **`resolver = "1"` downgrade** that is easy to miss (a *virtual*
     `[workspace]` root defaults the resolver to `1` even with edition-2021
     members — Cargo warns, but the behaviour change is quiet — unless
     `resolver = "2"` is set explicitly); and it couples the **#269** Homebrew
     formula's source tarball to a new workspace root. All of that for cosmetic
     symmetry.
   - **Why rejected**: a single-member workspace is ceremony, and the relocation
     carries real, avoidable costs (a broken security test, a resolver-downgrade
     trap, a re-rooted release tarball). Keeping the crate at root avoids every one
     of them and **tells the truth**: the app embeds the daemon; they are not
     peers, so a peer-crate layout would misrepresent the relationship.

## Consequences

### Positive

- **One repo, one notarized artifact.** #171 builds and notarizes the app + the
  embedded daemon as a single version-locked unit from a single tree — no
  cross-repo pinning, no split-source release choreography.
- **The socket contract is the only coupling.** The Swift app links no Rust and
  shares no build graph; the frozen `#164` snapshot over AF_UNIX is the entire
  boundary. The monorepo carries two toolchains **without** entangling their build
  graphs.
- **Zero migration cost, no hidden traps.** Keeping the crate at root leaves the
  #271 `CARGO_MANIFEST_DIR` anchor and the #269 source-tarball root **unchanged**,
  and sidesteps the `resolver = "1"` downgrade a new *virtual* workspace root
  would invite.
- **Honest structure.** `apps/menubar/` is a directory convention; the crate at
  root says plainly that the app **embeds** the daemon rather than implying two
  peer crates via a workspace.
- **A concrete, cheap future path.** The DEFER trigger names exactly when a
  workspace earns its keep (a real 2nd Rust crate) — the same restraint the repo
  already applies to *dependency* crates (`CONTRIBUTING.md` § *When a crate is
  warranted*) and to *ceremony* crates (ADR-0002, ADR-0004), here extended to an
  internal workspace.

### Negative / trade-offs

- **Directory asymmetry.** `apps/menubar/` (Swift) sits beside a Rust crate rooted
  at the repo top, not under a parallel `crates/` path. Accepted: the asymmetry
  **matches reality** (the two are not peers) and avoids every workspace cost above.
- **A future crate split defers, not eliminates, the relocation.** When a 2nd Rust
  crate appears (the DEFER trigger), the `[workspace]` + relocation work must be
  done then. Accepted: paying it **once**, when a real second member exists, is
  cheaper than maintaining a speculative single-member workspace now — and the
  trigger says precisely when to pay it (remembering `resolver = "2"`).
- **The monorepo carries two toolchains.** Rust **and** Swift/Xcode, and their CI,
  live in one tree — a heavier repo than a pure-Rust one. Accepted: the single
  notarized artifact and the socket-only coupling make one repo the correct home;
  this CI cost is inherent to shipping the app at all, not a consequence of the
  layout.

## Related

- Issues: **#310** (this ADR). Governs **#168** (the Swift menubar socket-client
  app — **open**) and **#171** (embed + notarize the daemon in the `.app` as one
  unit — **open**). Rationale touchpoints: **#271** (no-telemetry test reading
  `Cargo.lock` via `CARGO_MANIFEST_DIR` — **closed**), **#269** (Homebrew formula
  whose source tarball is rooted at the crate — **open**), **#164** (the frozen
  status-snapshot wire contract the app consumes — **closed**).
- Layout / code: root `Cargo.toml` is a bare `[package]` — **no** `[workspace]`;
  the Rust crate stays at the repo root and the Swift app will live in
  `apps/menubar/`. `src/usage.rs` anchors repo-file reads at
  `env!("CARGO_MANIFEST_DIR")` (~L1286) and reads `Cargo.lock` (~L1379) — the
  anchor a crate relocation would disturb.
- Prior art: ADR-0002 (keychain via CLI, **zero FFI**) and ADR-0004 (incidental
  `libc` FFI kept raw, **no wrapper crate**) — the same "no unnecessary
  build-graph coupling, no ceremony crate" ethos the socket-only boundary and the
  deferred workspace extend. `CONTRIBUTING.md` § *When a crate is warranted*
  applies that restraint to third-party *dependency* crates — the bar the DEFER
  trigger extends, by analogy, to an internal Cargo workspace.
