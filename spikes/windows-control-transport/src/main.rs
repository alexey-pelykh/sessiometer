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
//!    The impersonation is attempted TWICE on purpose: once BEFORE any read and once after, so the
//!    run MEASURES whether the documented "security context of the last message read from the
//!    pipe" wording implies a read-first ordering constraint, instead of the ADR asserting one.
//!
//!    Both attempts are bracketed by a NEGATIVE CONTROL, because without one point 4 is not
//!    evidence: the client runs as the same user as the server, so "we impersonated the peer and
//!    read its SID" and "the impersonation did nothing and we read our own" produce an identical
//!    string. `OpenThreadToken` fails `ERROR_NO_TOKEN` on a thread carrying no impersonation token,
//!    so the run proves that failure BEFORE impersonating and again AFTER `RevertToSelf` — which is
//!    what makes the success in between mean something, and what proves the revert reverted.
//!
//! The client is a CHILD PROCESS of the server (this same binary, `client` mode), so the resolved
//! PID is provably not our own — which is what makes the PID-vs-SID distinction in point 4
//! observable rather than asserted.
//!
//! Run: `cargo run` (server; spawns its own client). `cargo run -- client <pipe-name>` is the
//! child half and is not meant to be invoked by hand.

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
