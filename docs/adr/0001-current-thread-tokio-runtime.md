---
type: architecture-decision-record
number: 1
title: "Single-threaded (current_thread) Tokio runtime"
date: 2026-07-02
status: accepted
decision_makers: [Oleksii PELYKH (maintainer)]
---

# ADR-0001: Single-threaded (`current_thread`) Tokio runtime

## Status

**Accepted** — 2026-07-02. Records a decision already in force in `src/main.rs`;
this ADR captures the rationale rather than changing behavior (see #201, #195/A3).

## Context

`sessiometer` is a foreground, single-process "daemon-monolith": one process polls
each account's usage quota and swaps the active credential out-of-band before an
account is exhausted (`src/main.rs`, `src/daemon.rs`).

- It genuinely needs an async runtime — it drives `/usr/bin/security` subprocesses,
  per-tick timers and jitter, a Unix control socket, and SIGINT/SIGTERM handling.
  These map cleanly onto Tokio (`tokio` features `rt, macros, time, process,
  io-util, net, signal` in `Cargo.toml`).
- The workload is **I/O-bound and low-concurrency**: one account is polled per tick
  in a staggered round-robin (#80), with an occasional swap. There is no CPU-bound
  parallel work anywhere in the loop.
- A primary design goal is **hermetic testability**. `Daemon` is generic over four
  trait seams — `RosterPoller`, `CredentialStore`, `AccountStash`, `Clock` — so the
  whole poll/swap loop runs against in-memory fakes with no live quota, no keychain,
  no real time, and no sockets. `daemon.rs` is ~67% tests (per #195), so the test
  ergonomics of the runtime choice are load-bearing, not incidental.

## Decision

Run on Tokio's **current-thread** runtime — `#[tokio::main(flavor = "current_thread")]`
in `src/main.rs` — a single-threaded executor, rather than the default multi-thread
work-stealing runtime.

## Alternatives considered

1. **Multi-thread (work-stealing) runtime** — Tokio's default (`#[tokio::main]`).
   - **Pros**: CPU-bound parallelism; futures may migrate across worker threads.
   - **Cons**: requires every future held across an `.await` — and therefore every
     trait seam — to be `Send`. The in-memory test fakes deliberately use
     single-threaded interior mutability (`Rc<Cell<…>>`, `Rc<RefCell<…>>` in the
     `daemon` test module), which is `!Send`; a `Send` bound would force
     `Arc<Mutex<…>>` and cross-thread synchronization the real workload never needs.
   - **Cons (soundness, not just ergonomics)**: the crate resolves the user's home and
     login name through `getpwuid` (`src/paths.rs`) — a non-reentrant `libc` call that
     returns a pointer into a process-wide **static buffer**. Its `// SAFETY:` argument
     rests on this runtime being single-threaded, with `paths.rs` the crate's *only*
     `getpw*` caller, so no concurrent `getpw*` can race that buffer. A work-stealing
     runtime could poll that FFI from multiple worker threads, racing the shared buffer
     into **undefined behavior** — a strictly graver failure than the `Send`-bound
     friction above, and one the `Send` bound does **not** catch (the calls are
     synchronous, so nothing is held across an `.await` for the bound to reject).
     Multi-threading would first require moving these sites to the reentrant `getpwuid_r`.
   - **Why rejected**: it buys parallelism the workload has none of, taxes the seam/test
     architecture that is this module's main quality lever, and would void the
     single-thread soundness invariant the `getpwuid` security FFI depends on
     (`src/paths.rs`; the FFI itself is scoped in ADR-0004).
2. **No async runtime (blocking, thread-per-concern)** — hand-manage threads for the
   timer, subprocess I/O, socket server, and signals.
   - **Pros**: no async machinery at all.
   - **Cons**: re-implements what Tokio provides uniformly (`tokio::process`,
     `tokio::net`, `tokio::signal`, and the paused-time test driver
     `#[tokio::test(start_paused = true)]` used by the #105 timeout test); manual
     coordination across the socket server, poll loop, and signal handling.
   - **Why rejected**: more moving parts for identical behavior, and it loses the
     paused-time driver that keeps timing tests fast and deterministic.

## Consequences

### Positive

- The async seams stay **free of `Send` bounds** (`src/daemon.rs` module docs;
  `Daemon::resolve_active`'s future is `Send`-free by construction). That is exactly
  what lets the entire loop be exercised hermetically against `Rc`-based fakes — the
  property that keeps the ~67%-test daemon module fully testable *without* a split
  (#195).
- Matches the actual workload (I/O-bound, low-concurrency); no work-stealing
  scheduler overhead and no cross-thread data races in application code.
- Simpler mental model: one thread, cooperative scheduling.

### Negative / trade-offs

- **No CPU parallelism, and a blocking call stalls everything.** On a single thread,
  any synchronous blocking operation halts the poll loop, socket serving, and signal
  observation together. This is a standing constraint: every wait must be
  cooperative. The swap lock honors it — it polls `flock(LOCK_EX|LOCK_NB)` and
  `await`s an async sleep between tries rather than busy-spinning or blocking the OS
  thread, so the daemon stays responsive and `use` stays interruptible while it waits
  (`SwapLock::acquire` in `src/swap.rs`; see ADR-0003).
- A future that accidentally introduces heavy compute or a blocking syscall would
  degrade responsiveness and would need `spawn_blocking` or explicit yielding.
- **The runtime is load-bearing for FFI soundness, not only test ergonomics.** The
  `getpwuid` security FFI (`src/paths.rs`) is sound *because* the crate is
  single-threaded (see its `// SAFETY:` blocks). A later "just switch to `multi_thread`
  for throughput" is therefore not a drop-in change — it would need `getpwuid_r` first
  (see Alternatives §1). Recorded here so the constraint is legible at the decision
  level, not only at the call site.

## Related

- ADR-0003 (no-torn-swap): the swap lock's cooperative async wait depends on this
  runtime choice.
- ADR-0004 (incidental libc FFI kept raw): scopes the `getpwuid` / `getpeereid` /
  `getuid` **security** FFI out as "deliberately raw"; their single-thread **soundness**
  dependency is recorded here, with the per-site `// SAFETY:` detail in `src/paths.rs`.
- Code: `src/main.rs` (`main`), `src/daemon.rs` (module docs, `Daemon`),
  `src/paths.rs`.
