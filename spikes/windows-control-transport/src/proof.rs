// Copyright (c) 2026 Oleksii PELYKH
// SPDX-License-Identifier: MIT

//! The Windows half of the issue-#972 proof — everything that only exists on Windows.
//!
//! `main.rs` states WHAT is measured; this module is HOW, and it is deliberately one file so the
//! whole proof is readable top to bottom. Nothing here is production code: it is throwaway
//! knowledge-acquisition whose only consumers are ADR-0037 and the `windows-latest` CI job that
//! runs it (`.github/workflows/spike-972-windows-transport.yml`).
//!
//! **The process is its own client.** The server half spawns this same binary in `client` mode as a
//! CHILD PROCESS, so `GetNamedPipeClientProcessId` resolves a pid that is provably not our own and
//! is independently known (the `Child::id()` the spawn returned). Without that, "the peer's pid
//! resolved" would be indistinguishable from "we read our own pid back".
//!
//! **Every check is fail-loud.** `run` returns a non-zero [`ExitCode`] if any gated check fails, so
//! the CI job's own pass/fail IS the proof's verdict rather than a log a human has to read. The two
//! `MEASUREMENT` lines print the resolved SIDs because ADR-0037 quotes them; they are reports, not
//! gates, and the properties they report are gated beside them (CHECK 6, CHECK 5). On the spike's
//! FIRST run the pre-read one was deliberately un-gated — its outcome was the open question, so
//! asserting an answer would have assumed the finding. ADR-0037 § Decision 4 records the answer, and
//! a recorded decision no check enforces is one a later run can regress in silence.

use std::ffi::c_void;
use std::os::windows::io::AsRawHandle;
use std::process::ExitCode;
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt};
use tokio::net::windows::named_pipe::{ClientOptions, ServerOptions};

use windows_sys::Win32::Foundation::{
    CloseHandle, GetLastError, LocalFree, ERROR_ACCESS_DENIED, ERROR_NO_TOKEN, ERROR_PIPE_BUSY,
    HANDLE,
};
use windows_sys::Win32::Security::Authorization::{
    ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
};
use windows_sys::Win32::Security::{
    GetTokenInformation, RevertToSelf, TokenUser, SECURITY_ATTRIBUTES, TOKEN_QUERY, TOKEN_USER,
};
use windows_sys::Win32::System::Pipes::{GetNamedPipeClientProcessId, ImpersonateNamedPipeClient};
use windows_sys::Win32::System::Threading::{
    GetCurrentProcess, GetCurrentThread, OpenProcessToken, OpenThreadToken,
};

/// Mirror of `MAX_CONTROL_LINE_BYTES` in `src/daemon/socket.rs`. A COPY, not an import — the spike
/// is a standalone package outside the root crate's build graph on purpose (see `Cargo.toml`), so
/// it cannot `use` the daemon's constant. If the two ever disagree the spike is measuring a framing
/// the daemon does not use, which is why the value is stated here with its provenance rather than
/// picked.
const MAX_CONTROL_LINE_BYTES: u64 = 8 * 1024;

/// The whole exchange is time-boxed so a wedged runner fails the job instead of hanging it. Far
/// looser than the daemon's own `CONTROL_EXCHANGE_TIMEOUT` (2s) because this window also covers
/// spawning a child PROCESS, which the daemon's does not.
const PROOF_TIMEOUT: Duration = Duration::from_secs(30);

/// The peer's user SID, or the stage at which resolving it failed. Not a `Result<String, io::Error>`
/// because WHICH of the four calls failed is the finding — "impersonation is not permitted here" and
/// "the token has no user" are different answers to ADR-0037's question.
#[derive(Debug, Clone)]
enum PeerSid {
    /// The peer's user SID in SDDL string form (`S-1-5-21-...`).
    Resolved(String),
    /// `stage` is the Win32 call that failed; `code` is its `GetLastError()`.
    Failed { stage: &'static str, code: u32 },
}

impl std::fmt::Display for PeerSid {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Resolved(sid) => write!(f, "{sid}"),
            // `code: 0` is ERROR_SUCCESS, and printing it beside FAILED reads as a contradiction.
            // The one arm that has no Win32 error to report says so instead.
            Self::Failed { stage, code: 0 } => write!(f, "FAILED at {stage}"),
            Self::Failed { stage, code } => write!(f, "FAILED at {stage} (GetLastError={code})"),
        }
    }
}

/// Entry point from `main.rs`. Server mode with no arguments; `client <pipe-name>` is the child half
/// the server spawns and is not meant to be run by hand.
pub(crate) fn run() -> ExitCode {
    let mut args = std::env::args().skip(1);
    let runtime = match tokio::runtime::Builder::new_current_thread()
        // `current_thread`, matching the daemon (ADR-0001). Not incidental: the impersonation window
        // below mutates the CALLING THREAD's token, so a single-threaded runtime is what makes
        // "no `.await` between `ImpersonateNamedPipeClient` and `RevertToSelf`" a sufficient rule
        // rather than a necessary-but-not-sufficient one.
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(err) => {
            eprintln!("[spike-972] FATAL: could not build the tokio runtime: {err}");
            return ExitCode::from(1);
        }
    };

    match args.next().as_deref() {
        None => runtime.block_on(server_main()),
        Some("client") => match args.next() {
            Some(name) => runtime.block_on(client_main(&name)),
            None => {
                eprintln!("[spike-972] FATAL: `client` mode needs the pipe name as its argument.");
                ExitCode::from(1)
            }
        },
        Some(other) => {
            eprintln!("[spike-972] FATAL: unknown mode {other:?}; expected no argument (server) or `client <pipe-name>`.");
            ExitCode::from(1)
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Server half — the proof itself.
// ---------------------------------------------------------------------------------------------

async fn server_main() -> ExitCode {
    match tokio::time::timeout(PROOF_TIMEOUT, proof()).await {
        Ok(Ok(())) => {
            println!("[spike-972] VERDICT: PASS — every gated check succeeded.");
            ExitCode::SUCCESS
        }
        Ok(Err(err)) => {
            eprintln!("[spike-972] VERDICT: FAIL — {err}");
            ExitCode::from(1)
        }
        Err(_) => {
            eprintln!(
                "[spike-972] VERDICT: FAIL — the proof did not finish within {PROOF_TIMEOUT:?}."
            );
            ExitCode::from(1)
        }
    }
}

async fn proof() -> Result<(), String> {
    let our_pid = std::process::id();
    let our_sid =
        our_user_sid().map_err(|err| format!("could not read our own user SID: {err}"))?;
    // Unique per run: two concurrent CI jobs (or a re-run overlapping a cancelled one) must not
    // collide on the name, and a collision would surface as CHECK 2 passing for the wrong reason.
    let pipe_name = format!(r"\\.\pipe\sessiometer-spike-972-{our_pid}");

    println!("[spike-972] host pid           : {our_pid}");
    println!("[spike-972] host user SID      : {our_sid}");
    println!("[spike-972] pipe name          : {pipe_name}");

    // -- CHECK 1: a server instance exists, owner-only, and accepts a connection. ---------------
    //
    // The security descriptor is the named-pipe analogue of `bind_control_socket`'s `0600` chmod
    // (`src/cli.rs`, `fn bind_control_socket`): a DACL granting GENERIC_ALL to exactly one SID —
    // ours — and nothing else. `P` makes it PROTECTED, so nothing is inherited from the pipe
    // namespace's default ACL; without it the descriptor would be a floor, not a ceiling.
    let sddl = format!("D:P(A;;GA;;;{our_sid})");
    let descriptor = security_descriptor_from_sddl(&sddl)
        .map_err(|code| format!("ConvertStringSecurityDescriptorToSecurityDescriptorW({sddl}) failed: GetLastError={code}"))?;
    let mut attributes = SECURITY_ATTRIBUTES {
        nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: descriptor,
        // The daemon's control socket is never inherited by a child; neither is this.
        bInheritHandle: 0,
    };

    let server = {
        let mut options = ServerOptions::new();
        options
            // The single-instance guarantee. The daemon gets this from its own lockfile plus an
            // unlink-then-bind; a named pipe can enforce it in the kernel instead — see CHECK 2.
            .first_pipe_instance(true)
            // Already tokio's default (`ServerOptions::new`), stated anyway: on the raw Win32 API
            // it is opt-in, so a future port that stops going through tokio must set it by hand.
            .reject_remote_clients(true);
        // SAFETY: `attributes` is a live, correctly-sized `SECURITY_ATTRIBUTES` on this stack frame
        // whose `lpSecurityDescriptor` came from `ConvertStringSecurityDescriptorToSecurityDescriptorW`
        // (a valid self-relative descriptor: that call returned TRUE, and the API documents its
        // out-parameter as non-null on success — this code tests the BOOL, not the pointer). It
        // outlives the call,
        // which is all `CreateNamedPipeW` requires — the descriptor is COPIED into the kernel object,
        // which is why the free below is correct.
        let created = unsafe {
            options.create_with_security_attributes_raw(
                &pipe_name,
                (&mut attributes as *mut SECURITY_ATTRIBUTES).cast::<c_void>(),
            )
        };
        // SAFETY: `descriptor` is the pointer that call returned and has not been freed; the pipe
        // holds its own copy from here on, so freeing ours cannot dangle the kernel object.
        unsafe { LocalFree(descriptor) };
        created.map_err(|err| format!("create_with_security_attributes_raw failed: {err}"))?
    };
    // Captured BEFORE the server is moved into the `BufReader`: the peer-identity calls take the raw
    // pipe HANDLE, and the handle stays valid for as long as the `NamedPipeServer` does.
    let pipe: HANDLE = server.as_raw_handle().cast::<c_void>();
    println!("[spike-972] CHECK 1a create    : PASS — owner-only server instance created ({sddl})");

    // -- CHECK 2: the name cannot be squatted while we hold it. --------------------------------
    //
    // A second `first_pipe_instance` create against the same name must fail ERROR_ACCESS_DENIED.
    // This is the property the Unix side gets from the filesystem: a `0700` support dir means no
    // other user can drop a socket at our path. The pipe namespace has no directory to protect, so
    // the guarantee has to come from the create flag instead — which is exactly why it is measured
    // rather than assumed.
    match ServerOptions::new()
        .first_pipe_instance(true)
        .create(&pipe_name)
    {
        Ok(_) => {
            let detail = "a SECOND first_pipe_instance create SUCCEEDED — the name is squattable \
                          while we hold it";
            return Err(format!("CHECK 2 squat against {pipe_name}: {detail}"));
        }
        Err(err) if err.raw_os_error() == Some(ERROR_ACCESS_DENIED as i32) => {
            println!(
                "[spike-972] CHECK 2  squat     : PASS — second first_pipe_instance create denied \
                 (ERROR_ACCESS_DENIED = {ERROR_ACCESS_DENIED})"
            );
        }
        Err(err) => {
            let code = err.raw_os_error();
            return Err(format!(
                "CHECK 2 squat: the second create failed, but with {err:?} (raw_os_error={code:?}) \
                 rather than ERROR_ACCESS_DENIED = {ERROR_ACCESS_DENIED}"
            ));
        }
    }

    // -- Spawn the child client. ---------------------------------------------------------------
    let exe = std::env::current_exe().map_err(|err| format!("current_exe failed: {err}"))?;
    let mut child = tokio::process::Command::new(&exe)
        .arg("client")
        .arg(&pipe_name)
        // Piped stdio is the HANDSHAKE CHANNEL, and it is what makes CHECK 6 a controlled
        // measurement instead of a race. Without it, the child opens the pipe and writes its
        // request immediately, so by the time the server impersonates, request bytes may or may not
        // already be sitting in the pipe — unsynchronised, and different on every run. Since the
        // documented wording is "the security context of the last message READ from the pipe", a
        // run that cannot say whether bytes were available answers only one branch of the question
        // it claims to settle. Out-of-band on stdio, never on the pipe, or the synchronisation
        // would be the very traffic it exists to exclude.
        .stdout(std::process::Stdio::piped())
        .stdin(std::process::Stdio::piped())
        // Every check below can return early, and a client left running would sit in its own
        // 30-second timeout after this process is gone.
        .kill_on_drop(true)
        .spawn()
        .map_err(|err| format!("could not spawn the client child process: {err}"))?;
    let child_pid = child
        .id()
        .ok_or_else(|| "the client child exited before its pid could be read".to_string())?;
    println!("[spike-972] client child pid   : {child_pid}");
    let mut child_says = tokio::io::BufReader::new(
        child
            .stdout
            .take()
            .ok_or_else(|| "the client child has no piped stdout".to_string())?,
    );
    let mut tell_child = child
        .stdin
        .take()
        .ok_or_else(|| "the client child has no piped stdin".to_string())?;

    server
        .connect()
        .await
        .map_err(|err| format!("CHECK 1 accept: NamedPipeServer::connect failed: {err}"))?;
    println!("[spike-972] CHECK 1b accept    : PASS — connect() returned; a client is attached");

    // -- CHECK 1c: the client is attached AND has written nothing yet. -------------------------
    //
    // The child announces on stdout the moment its `open()` returns, then blocks until we release
    // it. Reading that line here establishes the state CHECK 6 needs: a connected peer with ZERO
    // bytes in the pipe. Every impersonation below happens in that state, so "no read had occurred"
    // is joined by the stronger "nothing was there to read".
    let mut announced = String::new();
    (&mut child_says)
        .take(64)
        .read_line(&mut announced)
        .await
        .map_err(|err| {
            format!("CHECK 1c handshake: could not read the child's announcement: {err}")
        })?;
    if announced.trim_end() != "opened" {
        return Err(format!(
            "CHECK 1c handshake: the child announced {announced:?}, not \"opened\" — the pipe's \
             state at impersonation time is not established, so CHECK 6 would be measuring a race"
        ));
    }
    println!("[spike-972] CHECK 1c handshake : PASS — the client has opened the pipe and written NOTHING (it is blocked awaiting our release)");

    // -- CHECK 0a: the impersonation instrument can tell its two states apart. -----------------
    //
    // Without this, CHECK 5 is not evidence. The client is a child of the server and therefore runs
    // as the SAME user, so "we impersonated the peer and read its SID" and "the impersonation did
    // nothing and we read our own" produce an IDENTICAL string — a check whose canary cannot come
    // back empty is not a check. `OpenThreadToken` fails ERROR_NO_TOKEN on a thread carrying no
    // impersonation token, so proving that failure HERE is what makes its success below meaningful.
    no_impersonation_token("CHECK 0a canary (before any impersonation)")?;
    println!("[spike-972] CHECK 0a canary   : PASS — this thread carries NO impersonation token yet (OpenThreadToken -> ERROR_NO_TOKEN = {ERROR_NO_TOKEN})");

    // -- CHECK 6 + MEASUREMENT: impersonation BEFORE any read. ---------------------------------
    //
    // `ImpersonateNamedPipeClient` is documented to give "the security context of the last message
    // read from the pipe". Whether that wording IMPLIES a read-first ordering constraint on a
    // byte-mode pipe was this spike's open question, and on its FIRST run this attempt was
    // deliberately un-gated: asserting an answer would have assumed the finding.
    //
    // It resolved, and ADR-0037 § Decision 4 now records that as a decision in force — the daemon's
    // `UnixControl::serve` computes `peer_authenticated` BEFORE `serve_control` reads, and a
    // read-first constraint would force that split apart. So the property is GATED from here on:
    // a re-run where pre-read impersonation regresses must redden the job, not print quietly. The
    // MEASUREMENT line stays because the resolved value is the evidence the ADR quotes.
    let sid_before_read = peer_user_sid(pipe);
    println!("[spike-972] MEASUREMENT pre-read impersonation : {sid_before_read}");
    match &sid_before_read {
        PeerSid::Resolved(sid) if *sid == our_sid => println!(
            "[spike-972] CHECK 6  pre-read   : PASS — the peer's SID resolved with NO read having \
             occurred, so the documented wording imposes no read-first ordering constraint"
        ),
        PeerSid::Resolved(sid) => {
            return Err(format!(
                "CHECK 6 pre-read: impersonation before the read resolved {sid}, which is not our \
                 own {our_sid} — the child runs as us, so this is a defect in the resolution"
            ))
        }
        PeerSid::Failed { stage, code } => {
            return Err(format!(
                "CHECK 6 pre-read: impersonation before any read failed at {stage} \
                 (GetLastError={code}). ADR-0037 Decision 4 records that it SUCCEEDS, and the \
                 daemon authenticates before it serves — this is a regression against that decision, \
                 not a new open question"
            ))
        }
    }

    // -- CHECK 0b: the canary again, between the two impersonation windows. --------------------
    //
    // Bracketing only the LAST window leaves the control with a hole exactly the shape of the thing
    // it exists to exclude: if window 1's revert silently failed to clear the token, window 2's
    // impersonation could succeed-but-do-nothing and `OpenThreadToken` would read window 1's
    // RESIDUAL token — whose SID is the client's, so CHECK 5 would print PASS off a stale reading.
    // Every window is bracketed on both ends, so no window's exit is taken on trust.
    no_impersonation_token("CHECK 0b canary (between the two impersonation windows)")?;
    println!("[spike-972] CHECK 0b canary   : PASS — the impersonation token is gone again after the pre-read window");

    // -- CHECK 4: the peer's pid (DIAGNOSTIC only). --------------------------------------------
    let peer_pid = client_process_id(pipe).map_err(|code| {
        format!("CHECK 4 pid: GetNamedPipeClientProcessId failed: GetLastError={code}")
    })?;
    if peer_pid == our_pid {
        return Err(format!(
            "CHECK 4 pid: the resolved peer pid {peer_pid} is our OWN pid — the call read back the \
             server side, so it proves nothing about the caller"
        ));
    }
    if peer_pid != child_pid {
        return Err(format!(
            "CHECK 4 pid: the resolved peer pid {peer_pid} is neither ours ({our_pid}) nor the child \
             we spawned ({child_pid})"
        ));
    }
    println!(
        "[spike-972] CHECK 4  peer pid  : PASS — {peer_pid} == the spawned child, != our own {our_pid} \
         (DIAGNOSTIC: a pid is reusable and TOCTOU-prone, never the authentication primitive)"
    );

    // -- Release the child: every pre-read measurement is now taken. ---------------------------
    tell_child
        .write_all(b"go\n")
        .await
        .map_err(|err| format!("could not release the client child: {err}"))?;
    tell_child
        .flush()
        .await
        .map_err(|err| format!("could not flush the release to the client child: {err}"))?;

    // -- CHECK 3: one framed message round-trips. ----------------------------------------------
    //
    // The daemon's exact framing, transcribed from `serve_control` in `src/daemon/socket.rs`:
    // `BufReader` + `.take(MAX_CONTROL_LINE_BYTES)` + `read_line`, ONE `serde_json` parse of the
    // trimmed line, then one reply line terminated by `b"\n"` and flushed.
    let mut buffered = tokio::io::BufReader::new(server);
    let mut line = String::new();
    (&mut buffered)
        .take(MAX_CONTROL_LINE_BYTES)
        .read_line(&mut line)
        .await
        .map_err(|err| format!("CHECK 3 framing: read_line failed: {err}"))?;
    let trimmed = line.trim_end();
    let request: serde_json::Value = serde_json::from_str(trimmed).map_err(|err| {
        format!("CHECK 3 framing: the request line {trimmed:?} is not JSON: {err}")
    })?;
    let cmd = request.get("cmd").and_then(serde_json::Value::as_str);
    if cmd != Some("status") {
        return Err(format!(
            "CHECK 3 framing: parsed the request line but its `cmd` was {cmd:?}, not \"status\""
        ));
    }
    println!("[spike-972] CHECK 3a request   : PASS — read one framed line, one serde_json parse, cmd=\"status\" ({} bytes on the wire)", line.len());

    // -- MEASUREMENT + CHECK 5: impersonation AFTER the read. ----------------------------------
    let sid_after_read = peer_user_sid(pipe);
    println!("[spike-972] MEASUREMENT post-read impersonation: {sid_after_read}");
    let peer_sid = match &sid_after_read {
        PeerSid::Resolved(sid) => sid.clone(),
        PeerSid::Failed { stage, code } => {
            return Err(format!(
                "CHECK 5 peer SID: impersonation after the read still failed at {stage} \
                 (GetLastError={code}) — the transport cannot answer `getpeereid`'s question at all"
            ))
        }
    };
    if peer_sid != our_sid {
        return Err(format!(
            "CHECK 5 peer SID: the peer resolved to {peer_sid}, which is not our own {our_sid} — \
             the child runs as us, so this is a defect in the resolution, not a foreign caller"
        ));
    }
    println!(
        "[spike-972] CHECK 5  peer SID  : PASS — {peer_sid} == our own SID (the `getpeereid` analogue: \
         a per-USER identity, not a per-process one)"
    );

    // -- CHECK 0c: the canary's other end — the post-read window closed too. -------------------
    //
    // A thread left impersonating would do the rest of its work under the peer's identity, and the
    // failure would be silent. `peer_user_sid` aborts if `RevertToSelf` returns FALSE; this proves
    // the stronger thing the return value alone does not — that the token is GONE afterwards.
    no_impersonation_token("CHECK 0c canary (after the post-read window)")?;
    println!("[spike-972] CHECK 0c canary   : PASS — the impersonation token is gone again after the post-read window");

    // -- Reply, and let the client read it. ----------------------------------------------------
    // Transcribed from `serve_control`'s one-shot arm, INCLUDING its error handling, which is the
    // part that is easy to get wrong: the daemon writes the ack inline and BEST-EFFORT and discards
    // the result (`let _ = ack;`). It is deliberate — a peer that hung up first would make this
    // write fail, and propagating that would discard the `ControlSignal` at `UnixControl::serve`'s
    // error arm, silently cancelling an action the operator had already authenticated. A `?` here
    // would be a lookalike, not a transcription, and on a named pipe it is exactly where the two
    // diverge: a client that gave up yields `ERROR_BROKEN_PIPE` rather than `EPIPE`.
    let reply = r#"{"ok":true,"proof":"spike-972"}"#;
    let ack = async {
        buffered.write_all(reply.as_bytes()).await?;
        buffered.write_all(b"\n").await?;
        buffered.flush().await
    }
    .await;
    let _ = ack;
    println!("[spike-972] CHECK 3b reply     : PASS — one reply line attempted inline and best-effort, exactly as the daemon writes its ack (delivery is proven by CHECK 3c, not by this write)");

    let status = child
        .wait()
        .await
        .map_err(|err| format!("waiting on the client child failed: {err}"))?;
    if !status.success() {
        return Err(format!(
            "CHECK 3 framing: the client child exited {status} — it did not accept the reply frame"
        ));
    }
    println!(
        "[spike-972] CHECK 3c client    : PASS — the child parsed the reply frame and exited 0"
    );

    Ok(())
}

// ---------------------------------------------------------------------------------------------
// Client half — a child process, so the pid the server resolves is provably not its own.
// ---------------------------------------------------------------------------------------------

async fn client_main(pipe_name: &str) -> ExitCode {
    match tokio::time::timeout(PROOF_TIMEOUT, client_exchange(pipe_name)).await {
        Ok(Ok(())) => ExitCode::SUCCESS,
        Ok(Err(err)) => {
            eprintln!("[spike-972/client] FAIL — {err}");
            ExitCode::from(1)
        }
        Err(_) => {
            eprintln!("[spike-972/client] FAIL — no reply within {PROOF_TIMEOUT:?}");
            ExitCode::from(1)
        }
    }
}

async fn client_exchange(pipe_name: &str) -> Result<(), String> {
    // A named pipe with no free instance answers ERROR_PIPE_BUSY rather than blocking, so the
    // idiomatic client is a retry loop. Unix `connect(2)` on a bound socket has no equivalent — a
    // difference the real port inherits, noted in ADR-0037.
    let client = loop {
        match ClientOptions::new().open(pipe_name) {
            Ok(client) => break client,
            Err(err) if err.raw_os_error() == Some(ERROR_PIPE_BUSY as i32) => {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            Err(err) => return Err(format!("ClientOptions::open({pipe_name}) failed: {err}")),
        }
    };

    // Announce that the pipe is OPEN and that nothing has been written to it, then block until the
    // server releases us. This is what lets the server's pre-read impersonation happen in a known
    // state rather than in a race with this write. Blocking `std` stdio on purpose: this process
    // has nothing else to do, and it keeps the spike off tokio's `io-std` feature.
    {
        use std::io::{BufRead, Write};
        println!("opened");
        std::io::stdout()
            .flush()
            .map_err(|err| format!("could not flush the announcement: {err}"))?;
        let mut release = String::new();
        std::io::stdin()
            .lock()
            .read_line(&mut release)
            .map_err(|err| format!("could not wait for the server's release: {err}"))?;
        if release.trim_end() != "go" {
            return Err(format!(
                "the server released us with {release:?}, not \"go\""
            ));
        }
    }

    let mut buffered = tokio::io::BufReader::new(client);
    write_line(&mut buffered, r#"{"cmd":"status"}"#)
        .await
        .map_err(|err| format!("writing the request line failed: {err}"))?;

    let mut line = String::new();
    (&mut buffered)
        .take(MAX_CONTROL_LINE_BYTES)
        .read_line(&mut line)
        .await
        .map_err(|err| format!("reading the reply line failed: {err}"))?;
    let reply: serde_json::Value = serde_json::from_str(line.trim_end())
        .map_err(|err| format!("the reply line {line:?} is not JSON: {err}"))?;
    if reply.get("ok").and_then(serde_json::Value::as_bool) != Some(true) {
        return Err(format!("the reply frame parsed but said {reply}"));
    }
    Ok(())
}

/// The daemon's `write_line` (`src/daemon/socket.rs`), transcribed: the payload, then `b"\n"`, then
/// a flush. Kept a separate function for the same reason it is one there — the frame terminator is
/// the contract, and inlining it is how a `\r\n` creeps in on a Windows port.
///
/// NOT the daemon's one-shot ack writer, and the distinction is deliberate on both sides. There,
/// `write_line` serves the `watch` stream and the inline REJECTION replies, while the one-shot ack
/// is written inline and best-effort so an `EPIPE` cannot discard an authenticated
/// `ControlSignal`. Here it carries the client's request and nothing else; the server's reply is
/// written inline, the same way, for the same reason.
async fn write_line<W>(writer: &mut W, line: &str) -> std::io::Result<()>
where
    W: tokio::io::AsyncWrite + Unpin,
{
    writer.write_all(line.as_bytes()).await?;
    writer.write_all(b"\n").await?;
    writer.flush().await?;
    Ok(())
}

// ---------------------------------------------------------------------------------------------
// Peer identity.
// ---------------------------------------------------------------------------------------------

/// The connected client's process id — the `GetNamedPipeClientProcessId` answer. A DIAGNOSTIC, never
/// the authentication primitive: a pid is reusable and the peer may exit and be replaced between the
/// read and the decision, which is exactly the TOCTOU window `getpeereid` does not have (its uid is
/// captured by the kernel at connect time and is a property of the connection, not of a live pid).
fn client_process_id(pipe: HANDLE) -> Result<u32, u32> {
    let mut pid: u32 = 0;
    // SAFETY: `pipe` is a live named-pipe HANDLE owned by the `NamedPipeServer` still in scope, and
    // `pid` is a live local the kernel writes only on success. A bad handle returns FALSE, not UB.
    if unsafe { GetNamedPipeClientProcessId(pipe, &mut pid) } == 0 {
        // SAFETY: no preconditions; reads this thread's last-error slot, set by the call above.
        return Err(unsafe { GetLastError() });
    }
    Ok(pid)
}

/// RAII closure of the impersonation window.
///
/// `ImpersonateNamedPipeClient` replaces the CALLING THREAD's token, and the window has to end on
/// EVERY path out — including an early `return` and an unwinding panic, which
/// [`token_user_sid`] can raise because it allocates. A `Drop` impl closes it BY CONSTRUCTION,
/// where a "every arm below remembers to revert" convention closes it by inspection; the
/// difference is the whole point, and it is the rule the real port inherits rather than an
/// accident of this file.
///
/// `!Send` on purpose (via the `PhantomData`): a thread token belongs to one thread, so a guard
/// that could be moved to another would be a guard for the wrong thread. That makes any future
/// holding one across an `.await` a COMPILE error on a multi-thread runtime instead of a silent
/// hazard — the daemon is `current_thread` (ADR-0001), but the type system should not depend on
/// that staying true.
struct Impersonation {
    _not_send: std::marker::PhantomData<*const ()>,
}

impl Impersonation {
    /// Take on the connected client's identity, or report the `GetLastError()` that prevented it.
    fn begin(pipe: HANDLE) -> Result<Self, u32> {
        // SAFETY: `pipe` is a live named-pipe HANDLE owned by a `NamedPipeServer` still in scope.
        if unsafe { ImpersonateNamedPipeClient(pipe) } == 0 {
            // SAFETY: no preconditions; reads the last-error slot set by the call above.
            return Err(unsafe { GetLastError() });
        }
        Ok(Self {
            _not_send: std::marker::PhantomData,
        })
    }
}

impl Drop for Impersonation {
    fn drop(&mut self) {
        // SAFETY: no preconditions; drops any impersonation token on the calling thread.
        if unsafe { RevertToSelf() } == 0 {
            // SAFETY: reads the last-error slot set by the call above.
            let code = unsafe { GetLastError() };
            // Not recoverable in any useful sense: the thread would keep running as the client, and
            // a failure a caller can ignore is how that becomes silent. Never observed; the abort
            // exists so that if it ever happens the proof says so rather than producing an answer
            // under the wrong identity.
            eprintln!("[spike-972] FATAL: RevertToSelf failed (GetLastError={code}) — this thread is still impersonating the client; aborting rather than continuing under its identity.");
            std::process::abort();
        }
    }
}

/// The connected client's USER SID — the `getpeereid` analogue, and the load-bearing half of AC3.
///
/// FULLY SYNCHRONOUS ON PURPOSE. `ImpersonateNamedPipeClient` replaces the CALLING THREAD's token;
/// an `.await` inside that window could (on a multi-thread runtime) resume elsewhere, leaving one
/// thread impersonating forever and doing the work under the wrong identity. There is no async work
/// to do between the two calls, so the rule costs nothing — but it is a rule the real port has to
/// keep, not an accident of this file.
///
/// Fail-closed by construction: every arm returns [`PeerSid::Failed`], which no caller can mistake
/// for an identity. That mirrors `peer_euid`'s `None`-on-error contract in `src/daemon/peer_auth.rs`.
fn peer_user_sid(pipe: HANDLE) -> PeerSid {
    // From here to the end of this function the thread carries the CLIENT's token; `_window`'s
    // `Drop` is what ends it, on every path including an unwind.
    let _window = match Impersonation::begin(pipe) {
        Ok(window) => window,
        Err(code) => {
            return PeerSid::Failed {
                stage: "ImpersonateNamedPipeClient",
                code,
            }
        }
    };

    let mut token: HANDLE = std::ptr::null_mut();
    // SAFETY: `GetCurrentThread` returns a pseudo-handle needing no cleanup; `token` is a live local
    // the kernel writes only on success. `openasself = TRUE` performs the access check against the
    // PROCESS's context rather than the impersonation token we just took on — required, or a
    // low-privilege client's token could deny us the open.
    let opened = unsafe { OpenThreadToken(GetCurrentThread(), TOKEN_QUERY, 1, &mut token) };
    if opened == 0 {
        // SAFETY: reads the last-error slot set by `OpenThreadToken`. Read HERE, because the
        // `RevertToSelf` in `_window`'s `Drop` runs after this expression and would clobber it.
        let code = unsafe { GetLastError() };
        return PeerSid::Failed {
            stage: "OpenThreadToken",
            code,
        };
    }

    let result = token_user_sid(token);
    // SAFETY: `token` is the handle `OpenThreadToken` just wrote and has not been closed.
    unsafe { CloseHandle(token) };
    result
}

/// Assert that the calling thread carries NO impersonation token — `OpenThreadToken` must fail with
/// `ERROR_NO_TOKEN`. The negative control for [`peer_user_sid`]: it is what distinguishes "the
/// impersonation took effect" from "nothing happened and we read our own primary token", which the
/// resolved SID alone cannot, because this proof's client runs as the same user as its server.
///
/// A SUCCESS here is the failure: it means a token was already present where none should be.
fn no_impersonation_token(label: &str) -> Result<(), String> {
    let mut token: HANDLE = std::ptr::null_mut();
    // SAFETY: `GetCurrentThread` returns a pseudo-handle needing no cleanup; `token` is a live local
    // the kernel writes only on success.
    let opened = unsafe { OpenThreadToken(GetCurrentThread(), TOKEN_QUERY, 1, &mut token) };
    if opened != 0 {
        // SAFETY: `token` is the handle the call just wrote and has not been closed.
        unsafe { CloseHandle(token) };
        return Err(format!(
            "{label}: OpenThreadToken SUCCEEDED on a thread that should carry no impersonation \
             token — the SID comparison below cannot distinguish the peer's identity from our own"
        ));
    }
    // SAFETY: reads the last-error slot set by the call above.
    let code = unsafe { GetLastError() };
    if code != ERROR_NO_TOKEN {
        return Err(format!(
            "{label}: OpenThreadToken failed with GetLastError={code} rather than \
             ERROR_NO_TOKEN = {ERROR_NO_TOKEN}, so the instrument's default state is not what the \
             control assumes"
        ));
    }
    Ok(())
}

/// Our OWN user SID, read from the process token by the same two calls the peer path uses. Two jobs:
/// it seeds the pipe's owner-only DACL, and it is what CHECK 5 compares the peer's SID against.
fn our_user_sid() -> Result<String, String> {
    let mut token: HANDLE = std::ptr::null_mut();
    // SAFETY: `GetCurrentProcess` returns a pseudo-handle needing no cleanup; `token` is a live
    // local the kernel writes only on success.
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) } == 0 {
        // SAFETY: reads the last-error slot set by the call above.
        return Err(format!(
            "OpenProcessToken failed: GetLastError={}",
            unsafe { GetLastError() }
        ));
    }
    let result = token_user_sid(token);
    // SAFETY: `token` is the handle `OpenProcessToken` just wrote and has not been closed.
    unsafe { CloseHandle(token) };
    match result {
        PeerSid::Resolved(sid) => Ok(sid),
        PeerSid::Failed { stage, code } => Err(format!("{stage} failed: GetLastError={code}")),
    }
}

/// `GetTokenInformation(TokenUser)` on `token`, rendered as an SDDL SID string.
///
/// The buffer is a `Vec<u64>`, not a `Vec<u8>`, and that is load-bearing rather than fussy: the
/// kernel writes a `TOKEN_USER` here, whose `Sid` member is a pointer, so reading it out of a
/// 1-byte-aligned allocation is undefined behaviour on a technicality that happens to work. A `u64`
/// element type makes the allocation 8-byte aligned, which is at least `align_of::<TOKEN_USER>()`.
fn token_user_sid(token: HANDLE) -> PeerSid {
    let mut needed: u32 = 0;
    // First call sizes the buffer; it is EXPECTED to fail with ERROR_INSUFFICIENT_BUFFER, so its
    // return value is deliberately ignored and only `needed` is read.
    // SAFETY: a null buffer with length 0 is the documented sizing form; `needed` is a live local.
    unsafe { GetTokenInformation(token, TokenUser, std::ptr::null_mut(), 0, &mut needed) };
    if needed == 0 {
        // SAFETY: reads the last-error slot set by the sizing call above.
        return PeerSid::Failed {
            stage: "GetTokenInformation(TokenUser, sizing)",
            code: unsafe { GetLastError() },
        };
    }

    let words = (needed as usize)
        .div_ceil(std::mem::size_of::<u64>())
        .max(1);
    let mut buffer = vec![0u64; words];
    // SAFETY: the buffer is `words * 8 >= needed` bytes of live, 8-byte-aligned, initialised memory.
    // The length passed is `needed`, which UNDER-reports the allocation by up to seven bytes — the
    // safe direction, since the kernel is told it has less room than it does. Rust evaluates call
    // arguments left to right, so the by-value 4th argument copies `needed` BEFORE the `&mut needed`
    // 5th exists; the out-write lands after and is never read again. Written only on success.
    let ok = unsafe {
        GetTokenInformation(
            token,
            TokenUser,
            buffer.as_mut_ptr().cast::<c_void>(),
            needed,
            &mut needed,
        )
    };
    if ok == 0 {
        // SAFETY: reads the last-error slot set by the call above.
        return PeerSid::Failed {
            stage: "GetTokenInformation(TokenUser)",
            code: unsafe { GetLastError() },
        };
    }

    // SAFETY: on success the kernel wrote a `TOKEN_USER` at the start of `buffer`, which is
    // correctly aligned for it (see the doc comment) and large enough (`needed` bytes). The `Sid` it
    // carries points INTO that same buffer, so it stays valid while `buffer` is alive — which it is
    // for the whole of `sid_to_string` below.
    let sid = unsafe { (*buffer.as_ptr().cast::<TOKEN_USER>()).User.Sid };
    if sid.is_null() {
        return PeerSid::Failed {
            stage: "TOKEN_USER.User.Sid (null)",
            code: 0,
        };
    }
    match sid_to_string(sid) {
        Ok(string) => PeerSid::Resolved(string),
        Err(code) => PeerSid::Failed {
            stage: "ConvertSidToStringSidW",
            code,
        },
    }
}

/// `ConvertSidToStringSidW`, with the `LocalFree` the API requires of its caller.
fn sid_to_string(sid: *mut c_void) -> Result<String, u32> {
    let mut wide: *mut u16 = std::ptr::null_mut();
    // SAFETY: `sid` is a non-null pointer to a valid SID inside a live buffer (checked by the
    // caller); `wide` is a live local the API writes only on success.
    if unsafe { ConvertSidToStringSidW(sid, &mut wide) } == 0 {
        // SAFETY: reads the last-error slot set by the call above.
        return Err(unsafe { GetLastError() });
    }
    // SAFETY: on success `wide` is a valid NUL-terminated UTF-16 string allocated with `LocalAlloc`.
    let string = unsafe { wide_to_string(wide) };
    // SAFETY: `wide` is exactly the `LocalAlloc`-ed pointer the call returned, freed once.
    unsafe { LocalFree(wide.cast::<c_void>()) };
    Ok(string)
}

/// A self-relative security descriptor built from an SDDL string. The returned pointer is
/// `LocalAlloc`-ed and the CALLER owns it — `CreateNamedPipeW` copies it, so freeing it right after
/// the pipe exists is correct and is what the caller does.
fn security_descriptor_from_sddl(sddl: &str) -> Result<*mut c_void, u32> {
    let wide: Vec<u16> = sddl.encode_utf16().chain(std::iter::once(0)).collect();
    let mut descriptor: *mut c_void = std::ptr::null_mut();
    // SAFETY: `wide` is a live, NUL-terminated UTF-16 buffer that outlives the call; `descriptor` is
    // a live local the API writes only on success; a null size out-parameter is documented as "do
    // not report the size".
    let ok = unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            wide.as_ptr(),
            SDDL_REVISION_1,
            &mut descriptor,
            std::ptr::null_mut(),
        )
    };
    if ok == 0 {
        // SAFETY: reads the last-error slot set by the call above.
        return Err(unsafe { GetLastError() });
    }
    Ok(descriptor)
}

/// A NUL-terminated UTF-16 Win32 string as a Rust `String`.
///
/// # Safety
///
/// `ptr` must be non-null and point at a NUL-terminated UTF-16 sequence that stays valid for the
/// duration of the call.
unsafe fn wide_to_string(ptr: *const u16) -> String {
    let mut len = 0usize;
    // SAFETY: the caller guarantees a NUL terminator, so this walk stops inside the allocation.
    while unsafe { *ptr.add(len) } != 0 {
        len += 1;
    }
    // SAFETY: `ptr[..len]` is exactly the sequence walked above, all within the caller's allocation.
    String::from_utf16_lossy(unsafe { std::slice::from_raw_parts(ptr, len) })
}
