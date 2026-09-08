// Copyright (c) 2026 Oleksii PELYKH
// SPDX-License-Identifier: MIT

//! The issue-#1511 accept-loop / `watch` proof — the second measurement this spike package
//! carries, and the one ADR-0037 says the transport port owes before the `watch` subscription
//! is ported.
//!
//! ADR-0037 § What this spike did NOT establish is explicit that the #972 proof round-trips
//! *"exactly one message on one connection"*, and that the long-lived `watch` subscription (#165)
//! *"is untested here — and it is where one-client-per-instance (§ Negative) bites hardest, since
//! a subscriber occupies an instance for its whole lifetime"*. It assigns the gap to **#1511**,
//! *"answering how many instances the accept loop keeps outstanding and what happens when they
//! are exhausted"*. This module is that answer, MEASURED on a real `windows-latest` host rather
//! than reasoned from the API docs — the same standard the #972 proof was held to.
//!
//! **It is a separate mode, not an extension of the #972 proof.** ADR-0037 quotes that proof's
//! output and says it is *"the whole of what the program printed"*; adding checks to `cargo run`
//! would quietly falsify that sentence for every later run. `cargo run -- watch` prints its own
//! block instead, and the default mode is byte-for-byte what the ADR records (modulo the per-run
//! pids it already flags).
//!
//! **What it exercises is the production algorithm, transcribed.** The spike is a standalone
//! package outside the root crate's build graph (see `Cargo.toml`), so it cannot `use`
//! `src/control_transport.rs`. [`AcceptLoop`] below is therefore a MIRROR of it — same order of
//! operations, same cancel-safety structure, same single-attempt refill — stated here with its
//! provenance rather than approximated, exactly as `MAX_CONTROL_LINE_BYTES` is in `proof.rs`. If
//! the two ever disagree this proof is measuring a loop the daemon does not run.
//!
//! **`max_instances` is lowered to make the ceiling reachable.** Production sets no limit, which
//! means the OS maximum of 255 — unreachable in a bounded CI run. This proof pins
//! [`MAX_INSTANCES`] so exhaustion happens after a handful of clients. What generalizes is the
//! BEHAVIOUR at the ceiling, which is a property of `CreateNamedPipe` and not of the number.

use std::cell::RefCell;
use std::ffi::OsString;
use std::io;
use std::process::ExitCode;
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, AsyncWriteExt};
use tokio::net::windows::named_pipe::{
    ClientOptions, NamedPipeClient, NamedPipeServer, PipeMode, ServerOptions,
};

use windows_sys::Win32::Foundation::{ERROR_ACCESS_DENIED, ERROR_FILE_NOT_FOUND, ERROR_PIPE_BUSY};
use windows_sys::Win32::Storage::FileSystem::{SECURITY_IDENTIFICATION, SECURITY_SQOS_PRESENT};

/// The instance ceiling this proof pins so exhaustion is reachable in a bounded run.
///
/// Production (`src/control_transport.rs`) sets none, which is tokio's
/// `PIPE_UNLIMITED_INSTANCES` — the OS maximum, 255. All instances of one pipe must agree on
/// this value, so every create below passes it.
const MAX_INSTANCES: usize = 4;

/// The whole proof is time-boxed so a wedged runner fails the job instead of hanging it. Shorter
/// than `proof.rs`'s budget because nothing here spawns a child process.
const PROOF_TIMEOUT: Duration = Duration::from_secs(20);

/// How long to wait for a frame the server has already pushed. Generous: it bounds a failure,
/// it does not pace a success.
const FRAME_TIMEOUT: Duration = Duration::from_secs(5);

/// The tag every line of this proof carries, so its output is never confused with #972's.
const TAG: &str = "[spike-1511]";

/// A proof failure: the check that failed and what it saw.
struct Failure(String);

type Checked<T> = Result<T, Failure>;

fn fail(message: impl Into<String>) -> Failure {
    Failure(message.into())
}

/// Whether `err` is the "all instances are in use right now" signal.
///
/// Matched on the RAW OS code, mirroring `is_pipe_busy` in `src/control_transport.rs`: the
/// `io::ErrorKind` std maps `ERROR_PIPE_BUSY` to is an implementation detail nothing in this repo
/// pins, and this predicate decides retry-vs-surface.
fn is_code(err: &io::Error, code: u32) -> bool {
    err.raw_os_error() == Some(code as i32)
}

/// Create one server instance of `name`.
///
/// A MIRROR of `create_instance` in `src/control_transport.rs`, plus the pinned
/// [`MAX_INSTANCES`]: `first_pipe_instance` on the first only, `reject_remote_clients` and
/// `PipeMode::Byte` set explicitly although both are already tokio's defaults.
fn create_instance(name: &OsString, first: bool) -> io::Result<NamedPipeServer> {
    ServerOptions::new()
        .first_pipe_instance(first)
        .reject_remote_clients(true)
        .pipe_mode(PipeMode::Byte)
        .max_instances(MAX_INSTANCES)
        .create(name)
}

/// Open a client connection, exactly as the production client does.
///
/// `SECURITY_SQOS_PRESENT | SECURITY_IDENTIFICATION` is set here for the same reason
/// `src/control_transport.rs` sets it — ADR-0037 records the pair as not optional. This proof
/// does NOT retry a busy open: distinguishing busy from not-found is the measurement (CHECK 6),
/// so a retry loop would hide the very answer it exists to produce.
fn open_client(name: &OsString) -> io::Result<NamedPipeClient> {
    ClientOptions::new()
        .security_qos_flags(SECURITY_SQOS_PRESENT | SECURITY_IDENTIFICATION)
        .pipe_mode(PipeMode::Byte)
        .open(name)
}

/// Holds the listening instance OUT of [`AcceptLoop::idle`] for one `connect().await` and puts it
/// BACK if that await is cancelled — the transcription of `PendingAccept` in
/// `src/control_transport.rs`. A guard rather than a borrow held across the await, because that
/// pattern is `clippy::await_holding_refcell_ref` and both crates deny it.
struct PendingAccept<'a> {
    idle: &'a RefCell<Option<NamedPipeServer>>,
    server: Option<NamedPipeServer>,
}

impl Drop for PendingAccept<'_> {
    fn drop(&mut self) {
        if let Some(server) = self.server.take() {
            *self.idle.borrow_mut() = Some(server);
        }
    }
}

/// The production accept loop, transcribed.
///
/// Holds the pipe name and the ONE created-but-unconnected instance, and creates the replacement
/// before handing a connected one out — see [`AcceptLoop::accept`]. `RefCell` for the same reason
/// the production type uses one: the run loop's `Control::serve` takes `&self`.
struct AcceptLoop {
    name: OsString,
    idle: RefCell<Option<NamedPipeServer>>,
    /// Whether the last refill attempt was denied for want of an instance. Not part of the
    /// production type — this proof reads it to gate CHECK 6a, where production simply defers the
    /// create to the next accept.
    refill_was_busy: RefCell<bool>,
}

impl AcceptLoop {
    fn bind(name: OsString) -> io::Result<Self> {
        let first = create_instance(&name, true)?;
        Ok(Self {
            name,
            idle: RefCell::new(Some(first)),
            refill_was_busy: RefCell::new(false),
        })
    }

    /// Accept one connection.
    ///
    /// The transcription of `ControlListener::accept`, and the two properties that make it what
    /// it is are both structural rather than incidental:
    ///
    /// - the listening instance is carried through `connect().await` by [`PendingAccept`], whose
    ///   `Drop` restores it, so a dropped accept future leaves it listening rather than closing it
    ///   (CHECK 8 measures this);
    /// - the replacement is created BEFORE the connected instance is returned, so at least one
    ///   instance always exists and the name is never released.
    async fn accept(&self) -> io::Result<NamedPipeServer> {
        if self.idle.borrow().is_none() {
            let created = create_instance(&self.name, false)?;
            *self.idle.borrow_mut() = Some(created);
        }
        let mut pending = PendingAccept {
            idle: &self.idle,
            server: self.idle.borrow_mut().take(),
        };
        if let Err(err) = pending
            .server
            .as_ref()
            .expect("just ensured")
            .connect()
            .await
        {
            pending.server = None;
            return Err(err);
        }
        let connected = pending.server.take().expect("claimed exactly once");
        match create_instance(&self.name, false) {
            Ok(next) => {
                *self.idle.borrow_mut() = Some(next);
                *self.refill_was_busy.borrow_mut() = false;
            }
            Err(err) if is_code(&err, ERROR_PIPE_BUSY) => {
                *self.refill_was_busy.borrow_mut() = true;
            }
            Err(_) => {
                *self.refill_was_busy.borrow_mut() = false;
            }
        }
        Ok(connected)
    }

    /// Instances this loop is holding open: the listening one, if any, plus whatever the caller
    /// still holds. The caller supplies its own count because the loop hands connections away.
    fn listening(&self) -> usize {
        usize::from(self.idle.borrow().is_some())
    }
}

/// Entry point for `cargo run -- watch`.
pub(crate) async fn watch_main() -> ExitCode {
    match tokio::time::timeout(PROOF_TIMEOUT, proof()).await {
        Ok(Ok(())) => {
            println!("{TAG} VERDICT: PASS — every gated check succeeded.");
            ExitCode::SUCCESS
        }
        Ok(Err(Failure(why))) => {
            eprintln!("{TAG} VERDICT: FAIL — {why}");
            ExitCode::from(1)
        }
        Err(_) => {
            eprintln!("{TAG} VERDICT: FAIL — the proof did not finish within {PROOF_TIMEOUT:?}.");
            ExitCode::from(1)
        }
    }
}

async fn proof() -> Checked<()> {
    let pid = std::process::id();
    // The host pid keeps concurrent runs on one machine off each other's name, exactly as the
    // #972 proof does. It moves every run and is not evidence.
    let name = OsString::from(format!(r"\\.\pipe\sessiometer-spike-1511-{pid}"));
    println!("{TAG} host pid           : {pid}");
    println!("{TAG} pipe name          : {}", name.to_string_lossy());
    println!(
        "{TAG} max_instances      : {MAX_INSTANCES} (production sets none — the OS maximum, 255)"
    );

    let loop_ = AcceptLoop::bind(name.clone()).map_err(|err| {
        fail(format!(
            "CHECK 1: the first instance could not be created: {err}"
        ))
    })?;
    println!(
        "{TAG} CHECK 1  bind       : PASS — first instance created with first_pipe_instance \
         ({} listening, 0 connected)",
        loop_.listening()
    );

    // The #972 reservation still holds at a pinned max_instances — worth re-gating here rather
    // than inheriting, because this proof changes the create options and CHECK 2 is what says a
    // second daemon cannot take the name from under us.
    match create_instance(&name, true) {
        Ok(_) => {
            return Err(fail(
                "CHECK 2: a second first_pipe_instance create SUCCEEDED against a held name — \
                 the kernel name reservation ADR-0037 § Decision 2 relies on does not hold",
            ))
        }
        Err(err) if is_code(&err, ERROR_ACCESS_DENIED) => {
            println!(
                "{TAG} CHECK 2  squat      : PASS — a second first_pipe_instance create is denied \
                 (ERROR_ACCESS_DENIED = {ERROR_ACCESS_DENIED})"
            );
        }
        Err(err) => {
            return Err(fail(format!(
                "CHECK 2: expected ERROR_ACCESS_DENIED from a second first_pipe_instance create, got {err}"
            )))
        }
    }

    // --- the `watch` shape: a subscriber that holds its instance open -------------------------
    //
    // This is the question ADR-0037 leaves open. A subscriber occupies an instance for its whole
    // lifetime, so the load-bearing property is that a SECOND client can still be served while
    // the first is still streaming. A server that reused one instance would refuse it.
    let mut subscribers: Vec<NamedPipeClient> = Vec::new();
    let mut served: Vec<NamedPipeServer> = Vec::new();

    let subscriber = open_client(&name).map_err(|err| {
        fail(format!(
            "CHECK 3: the first client could not open the pipe: {err}"
        ))
    })?;
    let mut served_first = loop_
        .accept()
        .await
        .map_err(|err| fail(format!("CHECK 3: the accept loop failed: {err}")))?;
    println!(
        "{TAG} CHECK 3  subscribe  : PASS — client 1 connected and is HELD OPEN (the `watch` \
         shape); the loop refilled ({} listening, 1 connected)",
        loop_.listening()
    );

    // Push several frames down the held connection and read them back in order: the `watch`
    // stream's own shape, which the #972 proof's single request/reply exchange never exercised.
    // Same framing as the daemon: one newline-terminated JSON object per frame, flushed.
    let mut reader = tokio::io::BufReader::new(subscriber);
    for nth in 1..=3u32 {
        let frame = format!("{{\"frame\":{nth}}}\n");
        served_first
            .write_all(frame.as_bytes())
            .await
            .map_err(|err| fail(format!("CHECK 4: pushing frame {nth} failed: {err}")))?;
        served_first
            .flush()
            .await
            .map_err(|err| fail(format!("CHECK 4: flushing frame {nth} failed: {err}")))?;
        let mut line = String::new();
        match tokio::time::timeout(FRAME_TIMEOUT, reader.read_line(&mut line)).await {
            Ok(Ok(0)) => return Err(fail(format!("CHECK 4: EOF instead of frame {nth}"))),
            Ok(Ok(_)) => {}
            Ok(Err(err)) => {
                return Err(fail(format!("CHECK 4: reading frame {nth} failed: {err}")))
            }
            Err(_) => {
                return Err(fail(format!(
                    "CHECK 4: frame {nth} did not arrive within {FRAME_TIMEOUT:?}"
                )))
            }
        }
        let parsed: serde_json::Value = serde_json::from_str(line.trim_end()).map_err(|err| {
            fail(format!(
                "CHECK 4: frame {nth} is not one JSON object: {err}"
            ))
        })?;
        if parsed.get("frame").and_then(serde_json::Value::as_u64) != Some(u64::from(nth)) {
            return Err(fail(format!(
                "CHECK 4: frame {nth} arrived out of order or altered: {}",
                line.trim_end()
            )));
        }
    }
    println!(
        "{TAG} CHECK 4  stream     : PASS — 3 newline-delimited JSON frames pushed to the held \
         subscriber and read back IN ORDER over one connection"
    );
    subscribers.push(reader.into_inner());
    served.push(served_first);

    // Fill the pipe to its ceiling. Each iteration is one more subscriber that never disconnects.
    // MAX_INSTANCES total instances exist once the last client connects, and the refill after it
    // is the one that must fail.
    while subscribers.len() < MAX_INSTANCES {
        let nth = subscribers.len() + 1;
        let client = open_client(&name).map_err(|err| {
            fail(format!(
                "CHECK 5: client {nth} could not open the pipe while {} were streaming: {err}",
                subscribers.len()
            ))
        })?;
        let server = loop_
            .accept()
            .await
            .map_err(|err| fail(format!("CHECK 5: accepting client {nth} failed: {err}")))?;
        subscribers.push(client);
        served.push(server);
    }
    println!(
        "{TAG} CHECK 5  concurrent : PASS — {} subscribers are connected AT ONCE, each holding its \
         own instance; a one-instance server would have refused every one after the first",
        subscribers.len()
    );
    println!(
        "{TAG} MEASUREMENT outstanding instances: {} connected + {} listening = {} of \
         max_instances={MAX_INSTANCES}",
        subscribers.len(),
        loop_.listening(),
        subscribers.len() + loop_.listening()
    );

    if !*loop_.refill_was_busy.borrow() || loop_.listening() != 0 {
        return Err(fail(format!(
            "CHECK 6: expected the refill after the last client to be denied for want of an \
             instance, leaving nothing listening; saw refill_was_busy={} listening={}",
            loop_.refill_was_busy.borrow(),
            loop_.listening()
        )));
    }
    println!(
        "{TAG} CHECK 6a exhaustion : PASS — with all {MAX_INSTANCES} instances connected, creating \
         the replacement is denied ERROR_PIPE_BUSY and NOTHING is listening"
    );

    // The distinction that matters to every caller: a saturated daemon is not an absent one. If a
    // client saw ERROR_FILE_NOT_FOUND here, `use --next` would report "no daemon" and `status`
    // would print the friendly empty state — both wrong, and both un-retryable.
    match open_client(&name) {
        Ok(_) => {
            return Err(fail(
                "CHECK 6b: a client opened the pipe with every instance connected and none \
                 listening — the ceiling is not what it claims to be",
            ))
        }
        Err(err) if is_code(&err, ERROR_PIPE_BUSY) => {
            println!(
                "{TAG} CHECK 6b client busy: PASS — an arriving client gets ERROR_PIPE_BUSY \
                 (= {ERROR_PIPE_BUSY}), the documented RETRY signal — NOT ERROR_FILE_NOT_FOUND \
                 (= {ERROR_FILE_NOT_FOUND}), so a saturated daemon is never read as an absent one"
            );
        }
        Err(err) => {
            return Err(fail(format!(
                "CHECK 6b: expected ERROR_PIPE_BUSY from a client at the ceiling, got {err}"
            )))
        }
    }

    // Recovery: a subscriber disconnecting frees its instance, the loop refills on its next
    // accept, and service resumes with no intervention.
    let departing = subscribers.remove(0);
    drop(departing);
    drop(served.remove(0));

    let mut recovered = None;
    for _ in 0..40u32 {
        match open_client(&name) {
            Ok(client) => {
                recovered = Some(client);
                break;
            }
            Err(err) if is_code(&err, ERROR_PIPE_BUSY) => {
                // The loop has not refilled yet: drive one accept attempt, which is exactly what
                // the daemon's idle select does on its next pass. `accept` blocks until a client
                // arrives, so it is polled with a timeout rather than awaited to completion.
                let _ = tokio::time::timeout(Duration::from_millis(25), loop_.accept()).await;
            }
            Err(err) => {
                return Err(fail(format!(
                    "CHECK 7: expected a busy-or-success open after a subscriber left, got {err}"
                )))
            }
        }
    }
    let recovered = recovered.ok_or_else(|| {
        fail("CHECK 7: no client could connect after a subscriber freed its instance")
    })?;
    let served_recovered = tokio::time::timeout(FRAME_TIMEOUT, loop_.accept())
        .await
        .map_err(|_| fail("CHECK 7: the recovered client never got accepted"))?
        .map_err(|err| {
            fail(format!(
                "CHECK 7: accepting the recovered client failed: {err}"
            ))
        })?;
    println!(
        "{TAG} CHECK 7  recovery   : PASS — one subscriber left, the loop created a replacement \
         instance on its next accept, and a new client connected; no intervention, no restart"
    );
    subscribers.push(recovered);
    served.push(served_recovered);

    // Cancel-safety, which is the property the production accept is STRUCTURED around and the one
    // no reading of the API docs can settle. The daemon's idle `select!` drops the `serve` future
    // whenever another arm wins, which on a busy daemon is most ticks. If a cancelled accept
    // closed its listening instance, a daemon with no other instance would release the NAME —
    // and a client would then see ERROR_FILE_NOT_FOUND and report "no daemon".
    //
    // Free every instance first, so the loop is back to exactly one listening and the name is
    // held by that instance ALONE. Then cancel an accept against it and require a client to
    // still connect.
    subscribers.clear();
    served.clear();
    for _ in 0..40u32 {
        if loop_.listening() == 1 {
            break;
        }
        let _ = tokio::time::timeout(Duration::from_millis(25), loop_.accept()).await;
    }
    if loop_.listening() != 1 {
        return Err(fail(
            "CHECK 8: could not get back to exactly one listening instance to cancel against",
        ));
    }
    // One listening instance, no client: `accept` cannot resolve, so the timeout DROPS the
    // future mid-`connect()` — the exact cancellation the idle select performs.
    let cancelled = tokio::time::timeout(Duration::from_millis(50), loop_.accept()).await;
    if cancelled.is_ok() {
        return Err(fail(
            "CHECK 8: the accept resolved with no client attached — the cancellation under test \
             never happened",
        ));
    }
    if loop_.listening() != 1 {
        return Err(fail(
            "CHECK 8: the cancelled accept did not leave its listening instance in place",
        ));
    }
    let after_cancel = open_client(&name).map_err(|err| {
        fail(format!(
            "CHECK 8: a client could not open the pipe after an accept was cancelled — the name \
             was released or the instance was closed: {err}"
        ))
    })?;
    let served_after_cancel = tokio::time::timeout(FRAME_TIMEOUT, loop_.accept())
        .await
        .map_err(|_| fail("CHECK 8: the post-cancellation client never got accepted"))?
        .map_err(|err| {
            fail(format!(
                "CHECK 8: accepting after a cancellation failed: {err}"
            ))
        })?;
    drop(after_cancel);
    drop(served_after_cancel);
    println!(
        "{TAG} CHECK 8  cancel-safe: PASS — an accept dropped mid-connect left its listening \
         instance alive and the name held; the next client connected on the SAME instance"
    );

    println!(
        "{TAG} ANSWER (ADR-0037, #1511 AC3): the accept loop keeps exactly ONE listening instance \
         outstanding, plus one per live connection — so a `watch` subscriber occupies one for its \
         whole lifetime. At the ceiling the refill is denied ERROR_PIPE_BUSY, nothing listens, and \
         an arriving client is told BUSY rather than NOT-FOUND; when any connection ends the loop \
         refills on its next accept and service resumes unattended."
    );
    Ok(())
}
