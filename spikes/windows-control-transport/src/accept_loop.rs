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
//! *"answering how many instances the accept loop keeps outstanding"*; the second half of the
//! question — what happens when they are exhausted — is #1511's own AC3, not the ADR's wording.
//! This module answers both, MEASURED on a real `windows-latest` host rather than reasoned from
//! the API docs — the same standard the #972 proof was held to.
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
//! **`max_instances` is lowered to make a ceiling exist at all.** Production never calls
//! `max_instances`, so it takes tokio's default of `PIPE_UNLIMITED_INSTANCES` — a SENTINEL, not
//! a count: under it Windows bounds instances by the availability of system resources, and 255 is
//! the sentinel's value rather than a limit (`max_instances` asserts `< 255`, so 254 is the
//! largest settable one). This proof pins [`MAX_INSTANCES`] so exhaustion happens after a handful
//! of clients. What generalizes is the BEHAVIOUR when a create is refused, a property of
//! `CreateNamedPipe`; the number does not, and neither does the assumption that production's
//! configuration refuses with the SAME error — nothing here measures that.

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

/// The instance ceiling this proof pins so that a ceiling exists at all.
///
/// Production (`src/control_transport.rs`) sets none, so it takes tokio's default of
/// `PIPE_UNLIMITED_INSTANCES` — a SENTINEL rather than a count, under which Windows bounds
/// instances by system resources. All instances of one pipe must agree on this value, so every
/// create below passes it.
const MAX_INSTANCES: usize = 4;

/// The whole proof is time-boxed so a wedged runner fails the job instead of hanging it. Shorter
/// than `proof.rs`'s budget because nothing here spawns a child process.
const PROOF_TIMEOUT: Duration = Duration::from_secs(20);

/// How long to wait for a frame the server has already pushed. Generous: it bounds a failure,
/// it does not pace a success.
const FRAME_TIMEOUT: Duration = Duration::from_secs(5);

/// The cadence a busy create is retried at — a MIRROR of `INSTANCE_RETRY_INTERVAL` in
/// `src/control_transport.rs`, used by [`AcceptLoop::wait_for_instance`] for the same purpose and
/// also by the proof's own driven-retry passes, which have to pace themselves once an accept can
/// be cut short.
const INSTANCE_RETRY_INTERVAL: Duration = Duration::from_millis(50);

/// How long one driven accept pass waits for a client before the proof takes the loop back. Its
/// expiry is also what puts a newly created instance back into `idle`, via [`PendingAccept`].
const ACCEPT_POLL: Duration = Duration::from_millis(25);

/// How many (poll, wait) passes CHECK 7 gives a freed instance — about three seconds, inside
/// [`PROOF_TIMEOUT`]. A ceiling on a FAILURE, not a pace for a success: the recovery it measures
/// normally lands on the first or second pass.
const RECOVERY_ATTEMPTS: u32 = 40;

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
/// pattern is `clippy::await_holding_refcell_ref`, which is warn-by-default and so an error
/// under the `-D warnings` both crates are linted with.
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
    /// Whether the last refill attempt was denied for want of an instance. The one field NOT in
    /// the production type: this proof reads it to gate CHECK 6a. It is written only by the
    /// post-`connect` refill, which is the single-attempt one production also does not retry —
    /// so recording it costs the mirror nothing.
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

    /// Wait until a listening instance can be created, then create it — the transcription of
    /// `wait_for_instance` in `src/control_transport.rs`, retry cadence included. Untranscribed,
    /// this proof would be measuring a loop the daemon does not run, which is the one thing the
    /// module doc says would invalidate it.
    async fn wait_for_instance(&self) -> io::Result<NamedPipeServer> {
        loop {
            match create_instance(&self.name, false) {
                Ok(server) => return Ok(server),
                Err(err) if is_code(&err, ERROR_PIPE_BUSY) => {
                    tokio::time::sleep(INSTANCE_RETRY_INTERVAL).await;
                }
                Err(err) => return Err(err),
            }
        }
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
            let created = self.wait_for_instance().await?;
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
            // Mirrors the three-arm failed-connect path in `src/control_transport.rs`: replace
            // before releasing, discard outright only when the refusal is itself evidence that
            // another instance is alive, and otherwise keep the failed one listening and pace
            // the failure. Transcribed even though no check here drives a failed `connect` —
            // a mirror that diverges is measuring a loop the daemon does not run, whether or
            // not a check happens to reach the divergence.
            match create_instance(&self.name, false) {
                Ok(next) => pending.server = Some(next),
                Err(create_err) if is_code(&create_err, ERROR_PIPE_BUSY) => pending.server = None,
                Err(_) => tokio::time::sleep(INSTANCE_RETRY_INTERVAL).await,
            }
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
        "{TAG} max_instances      : {MAX_INSTANCES} (production sets none — tokio's \
         PIPE_UNLIMITED_INSTANCES, under which Windows bounds instances by system resources)"
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
    // ASSERTED, not merely printed. Interpolating `listening()` into a PASS line makes the check
    // unfalsifiable: a refill that was denied prints "PASS — … the loop refilled (0 listening…)"
    // and the run goes green having measured the opposite of what the line claims.
    if loop_.listening() != 1 {
        return Err(fail(format!(
            "CHECK 3: the loop did not refill after handing out a connection; listening={}",
            loop_.listening()
        )));
    }
    println!(
        "{TAG} CHECK 3  subscribe  : PASS — client 1 connected and is HELD OPEN (the `watch` \
         shape); the loop refilled (1 listening, 1 connected)"
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

    let started = tokio::time::Instant::now();
    let mut recovered = None;
    for _ in 0..RECOVERY_ATTEMPTS {
        match open_client(&name) {
            Ok(client) => {
                recovered = Some(client);
                break;
            }
            Err(err) if is_code(&err, ERROR_PIPE_BUSY) => {
                // The loop has not refilled yet: drive one accept pass, which is exactly what the
                // daemon's idle `select!` does on its next tick. `accept` blocks until a client
                // arrives, so it is polled with a timeout rather than awaited to completion — and
                // that expiry is also what returns a newly created instance to `idle`, where the
                // next `open_client` can reach it.
                let _ = tokio::time::timeout(ACCEPT_POLL, loop_.accept()).await;
                // Then WAIT, because the pass above can be cut short before it has waited for
                // anything: `accept` now retries inside `wait_for_instance`, and ACCEPT_POLL is
                // shorter than one retry interval, so a timed-out pass may have done nothing but
                // sleep. The FIRST Windows run of this proof is why the wait is here at all —
                // back then `accept` surfaced a denied create on its first poll, `timeout`
                // registered no sleep, and all 40 passes elapsed inside one microsecond window,
                // a budget that read as a second and was in fact zero.
                tokio::time::sleep(INSTANCE_RETRY_INTERVAL).await;
            }
            Err(err) => {
                return Err(fail(format!(
                    "CHECK 7: expected a busy-or-success open after a subscriber left, got {err}"
                )))
            }
        }
    }
    let recovered = recovered.ok_or_else(|| {
        // The elapsed time is part of the finding, because the two ways this check can fail read
        // identically without it: recovery that never happens, and a retry that never waited.
        fail(format!(
            "CHECK 7: no client could connect after a subscriber freed its instance \
             ({RECOVERY_ATTEMPTS} passes over {:.3}s)",
            started.elapsed().as_secs_f64()
        ))
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
        "{TAG} CHECK 7  recovery   : PASS — one subscriber left and a new client connected \
         {:.3}s later; no intervention, no restart. Reported on the PASS and not only on the \
         failure, because the number is the finding: reclaim is not synchronous with the \
         client's disconnect, so a single refill attempt can still be refused",
        started.elapsed().as_secs_f64()
    );
    subscribers.push(recovered);
    served.push(served_recovered);

    // Cancel-safety, which is the property the production accept is STRUCTURED around and the one
    // no reading of the API docs can settle. The daemon's idle `select!` drops the `serve` future
    // whenever another arm wins, which on a busy daemon is most ticks. If a cancelled accept
    // closed its listening instance, a daemon with no other instance would release the NAME —
    // and a client would then see ERROR_FILE_NOT_FOUND and report "no daemon".
    //
    // Get back to exactly one listening instance, and get there WITHOUT passing through zero.
    // Clearing everything at once is the obvious way and it is wrong: at this point the refill
    // after CHECK 7's accept was denied, so nothing is listening, and dropping all the
    // connections leaves the process holding no handles at all — which RELEASES the name. Every
    // assertion below would then be measuring a pipe this proof re-created a moment later, not
    // the one it has been holding, and the release itself would go unnoticed. So free ONE pair,
    // let the loop refill against the instances still open, and only then drop the rest.
    drop(subscribers.pop());
    drop(served.pop());
    let started = tokio::time::Instant::now();
    for _ in 0..RECOVERY_ATTEMPTS {
        if loop_.listening() == 1 {
            break;
        }
        // Same two-part pass as CHECK 7's retry, and for the same reason: drive one accept so a
        // create is attempted and its instance parked in `idle`, then WAIT, since ACCEPT_POLL is
        // shorter than one internal retry interval and a cut-short pass may have created
        // nothing.
        let _ = tokio::time::timeout(ACCEPT_POLL, loop_.accept()).await;
        tokio::time::sleep(INSTANCE_RETRY_INTERVAL).await;
    }
    if loop_.listening() != 1 {
        return Err(fail(format!(
            "CHECK 8: could not get back to exactly one listening instance to cancel against \
             ({RECOVERY_ATTEMPTS} passes over {:.3}s, listening={})",
            started.elapsed().as_secs_f64(),
            loop_.listening()
        )));
    }
    // Now the remaining connections can go: the listening instance keeps the name held.
    subscribers.clear();
    served.clear();
    let probe = open_client(&name).map_err(|err| {
        fail(format!(
            "CHECK 8: the name did not survive draining the connections down to the single \
             listening instance — so the drain passed through zero handles after all: {err}"
        ))
    })?;
    // The probe ATTACHED to the listening instance, so it has to be accepted and closed rather
    // than merely dropped. An un-accepted client leaves that instance already connected, and the
    // cancellation below would then resolve on its first poll against a client that was never
    // supposed to be there — which is a green CHECK 8 that tested nothing, and is exactly what
    // the first run of this probe produced.
    let probe_served = tokio::time::timeout(FRAME_TIMEOUT, loop_.accept())
        .await
        .map_err(|_| fail("CHECK 8: the name-survival probe was never accepted"))?
        .map_err(|err| {
            fail(format!(
                "CHECK 8: accepting the name-survival probe failed: {err}"
            ))
        })?;
    drop(probe);
    drop(probe_served);
    for _ in 0..RECOVERY_ATTEMPTS {
        if loop_.listening() == 1 {
            break;
        }
        let _ = tokio::time::timeout(ACCEPT_POLL, loop_.accept()).await;
        tokio::time::sleep(INSTANCE_RETRY_INTERVAL).await;
    }
    if loop_.listening() != 1 {
        return Err(fail(
            "CHECK 8: could not get back to one listening instance after the name-survival probe",
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

    // The failure mode the accounting is WRITTEN against, measured rather than reasoned. Every
    // guarantee above is conditional on the process holding at least one instance; production's
    // refill is a single attempt, so a refused refill plus the end of the exchange it served can
    // take that count to zero. What happens then is the whole reason the refill order and the
    // failed-connect path are shaped the way they are, and until this check it was asserted by
    // three comments and measured by nothing.
    //
    // Driven here by taking the listening instance out of the loop and dropping it, which is the
    // same terminal state by a shorter road. LAST, because it is destructive: the name does not
    // come back without a fresh create.
    let last = loop_.idle.borrow_mut().take();
    if last.is_none() {
        return Err(fail(
            "CHECK 9: expected a listening instance to drop; the loop had none",
        ));
    }
    drop(last);

    // The name does not disappear the instant the handle does, and finding that out is the point
    // of polling rather than asserting. The FIRST run of this check demanded ERROR_FILE_NOT_FOUND
    // immediately and got ERROR_PIPE_BUSY: teardown is asynchronous, the same way reclaim is
    // (CHECK 7), so for a while the name still resolves with nothing behind it. Both readings are
    // wrong about a daemon that is up; they are wrong in different directions, and a client
    // retrying BUSY — which production's `connect` does, on a budget — can watch one turn into
    // the other underneath it.
    let started = tokio::time::Instant::now();
    let mut saw_busy = false;
    let mut gone = false;
    for _ in 0..RECOVERY_ATTEMPTS {
        match open_client(&name) {
            Ok(_) => {
                return Err(fail(
                    "CHECK 9: the pipe still accepted a client with no instance open — the name \
                     outlives its instances, and the accounting above is written on the \
                     assumption that it does not",
                ))
            }
            Err(err) if is_code(&err, ERROR_PIPE_BUSY) => {
                saw_busy = true;
                tokio::time::sleep(INSTANCE_RETRY_INTERVAL).await;
            }
            Err(err) if is_code(&err, ERROR_FILE_NOT_FOUND) => {
                gone = true;
                break;
            }
            Err(err) => {
                return Err(fail(format!(
                    "CHECK 9: expected busy-then-not-found with no instance open, got {err}"
                )))
            }
        }
    }
    if !gone {
        return Err(fail(format!(
            "CHECK 9: the name never stopped resolving after its last instance was dropped \
             ({RECOVERY_ATTEMPTS} passes over {:.3}s)",
            started.elapsed().as_secs_f64()
        )));
    }
    println!(
        "{TAG} CHECK 9  name gone  : PASS — the last instance dropped, and a client then got \
         {} for {:.3}s before the name stopped resolving at all with ERROR_FILE_NOT_FOUND \
         (= {ERROR_FILE_NOT_FOUND}). So the BUSY-not-NOT-FOUND guarantee holds only while an \
         instance EXISTS. At zero, a running daemon reads first as saturated and then as absent, \
         and the name is free for another process to take",
        if saw_busy {
            "ERROR_PIPE_BUSY first"
        } else {
            "no busy window"
        },
        started.elapsed().as_secs_f64()
    );

    println!(
        "{TAG} ANSWER (ADR-0037, #1511 AC3): the accept loop keeps exactly ONE listening instance \
         outstanding, plus one per live connection — so a `watch` subscriber occupies one for its \
         whole lifetime. At the ceiling the refill is denied ERROR_PIPE_BUSY, nothing listens, and \
         an arriving client is told BUSY rather than NOT-FOUND — but only while an instance still \
         exists (CHECK 9): the refill is a single attempt, so a refused refill plus the end of the \
         exchange it served reaches zero instances, and there a live daemon reads first as \
         saturated and then, once teardown completes, as ABSENT. When any connection ends the loop \
         refills and service resumes unattended — but NOT necessarily on the very next accept, \
         since the instance is not reclaimed synchronously with the client's disconnect. That is \
         what production's `wait_for_instance` retry cadence is for, and CHECK 7 prints how long \
         it actually took on this run."
    );
    Ok(())
}
