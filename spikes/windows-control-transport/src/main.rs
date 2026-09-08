// Copyright (c) 2026 Oleksii PELYKH
// SPDX-License-Identifier: MIT

//! The issue-#972 spike proof: a minimal Windows control transport over a named pipe.
//!
//! Throwaway knowledge-acquisition, not production code — it exists to answer four questions
//! empirically ON WINDOWS, because the repo has no Windows host and `cargo check` against the
//! `x86_64-pc-windows-msvc` target type-checks without linking or running (the blindness class
//! ADR-0029 records for the Mach `extern` block). The answers land in ADR-0037.
//!
//! What it proves, in the order the run performs it:
//!
//! 1. **A server accepts a connection.** [`ServerOptions::create_with_security_attributes_raw`]
//!    with an explicit owner-only security descriptor, `first_pipe_instance`, and
//!    `reject_remote_clients`; then `NamedPipeServer::connect().await`.
//! 2. **The name cannot be squatted while we hold it.** A second `first_pipe_instance` create
//!    against the same name must fail with `ERROR_ACCESS_DENIED`.
//! 3. **One framed message round-trips.** The daemon's exact framing — `BufReader` +
//!    `.take(MAX_CONTROL_LINE_BYTES)` + `read_line`, one `serde_json` parse, one reply line
//!    terminated by `b"\n"` — over a `NamedPipeServer` instead of a `UnixStream`.
//! 4. **The peer's identity resolves.** Two mechanisms, both measured:
//!    - `GetNamedPipeClientProcessId` — a PID. Reusable and TOCTOU-prone; recorded as a
//!      DIAGNOSTIC, never as the authentication primitive.
//!    - `ImpersonateNamedPipeClient` -> `OpenThreadToken` -> `GetTokenInformation(TokenUser)` ->
//!      `ConvertSidToStringSidW` -> `RevertToSelf` — the peer's USER SID. This is the analogue of
//!      `getpeereid`'s effective uid.
//!
//!    The impersonation is attempted TWICE on purpose: once BEFORE any read and once after. On the
//!    spike's FIRST run the pre-read attempt was un-gated, so the run MEASURED whether the
//!    documented "security context of the last message read from the pipe" wording implies a
//!    read-first ordering constraint, instead of the ADR asserting one. It does not, and ADR-0037
//!    § Decision 4 records that — so the attempt is GATED from there on, because a recorded
//!    decision no check enforces is one a later run can regress in silence.
//!
//!    Both attempts are bracketed by a NEGATIVE CONTROL, because without one point 4 is not
//!    evidence: the client runs as the same user as the server, so "we impersonated the peer and
//!    read its SID" and "the impersonation did nothing and we read our own" produce an identical
//!    string. `OpenThreadToken` fails `ERROR_NO_TOKEN` on a thread carrying no impersonation token,
//!    so the run proves that failure on BOTH SIDES OF EACH WINDOW — before the first, between the
//!    two, and after the second. Bracketing only the last one would leave a hole the exact shape of
//!    the thing the control exists to exclude: a revert that silently failed would leave a residual
//!    token for the next window to read back, and the SID would look right.
//!
//!    The window itself is closed by an RAII guard rather than by every arm remembering to revert,
//!    so an early return or an unwinding panic cannot leave the thread running as the client.
//!
//! The client is a CHILD PROCESS of the server (this same binary, `client` mode), so the resolved
//! PID is provably not our own — which is what makes the PID-vs-SID distinction in point 4
//! observable rather than asserted.
//!
//! A SECOND, separate proof rides in this package under `cargo run -- watch`: the issue-#1511
//! accept-loop / `watch` measurement (`accept_loop.rs`), which answers the residual ADR-0037
//! § What this spike did NOT establish assigns to that item — how many instances the accept loop
//! keeps outstanding and what happens when they are exhausted. It is a separate MODE rather than
//! extra checks in the default one on purpose: ADR-0037 quotes this proof's output and says it is
//! "the whole of what the program printed", and appending to that block would quietly falsify the
//! sentence for every later run.
//!
//! Run: `cargo run` (server; spawns its own client). `cargo run -- watch` is the #1511
//! accept-loop proof. `cargo run -- client <pipe-name>` is the #972 child half and is not meant
//! to be invoked by hand.

#[cfg(windows)]
mod accept_loop;

#[cfg(windows)]
mod proof;

#[cfg(windows)]
fn main() -> std::process::ExitCode {
    proof::run()
}

#[cfg(not(windows))]
fn main() -> std::process::ExitCode {
    eprintln!(
        "spike-972: this proof is Windows-only by construction — it exercises \
         `tokio::net::windows::named_pipe` and the Win32 peer-identity calls. \
         On a non-Windows host the useful check is \
         `cargo check --target x86_64-pc-windows-msvc`, which TYPE-CHECKS ONLY \
         (it neither links nor runs — see ADR-0029). The running proof is the \
         `windows-latest` job in .github/workflows/spike-972-windows-transport.yml."
    );
    std::process::ExitCode::from(2)
}
