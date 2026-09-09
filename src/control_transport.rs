// Copyright (c) 2026 Oleksii PELYKH
// SPDX-License-Identifier: MIT

//! The control channel's BYTE TRANSPORT, one arm per target (issue #1511, ADR-0037).
//!
//! Everything above this module — the newline-delimited `{"cmd":"…"}` framing, the
//! `serde` decode, [`crate::daemon`]'s `socket::serve_control` and every client verb — is pure
//! Rust with no OS surface. This module is the whole of the per-target seam under it: how a
//! listening endpoint is created, how one connection is accepted, and how a client opens one.
//!
//! Unix (macOS, Linux) keeps the `0600` Unix-domain socket verbatim: `ControlStream` IS
//! `tokio::net::UnixStream`, `ControlListener` is a thin newtype over `tokio::net::UnixListener`
//! whose `accept` drops the peer address the daemon never read, and `ControlListener::bind` is
//! the remove→bind→chmod dance moved here unchanged from `cli::bind_control_socket`. Nothing
//! about the Unix behaviour changes — it is relocated, not rewritten.
//!
//! Windows is a NAMED PIPE (ADR-0037 § Decision 1), driven through
//! `tokio::net::windows::named_pipe`. Four things about that arm are decisions rather
//! than mechanics, each recorded in the ADR:
//!
//! - **One client per INSTANCE.** There is no listening socket that accepts repeatedly. The
//!   server creates a new instance per accept and only the FIRST carries `first_pipe_instance`
//!   (ADR-0037 § Consequences → Negative). `ControlListener::accept` is where that structural
//!   difference lives; its own docs carry the instance accounting.
//! - **The framing survives unchanged.** The pipe stays in BYTE mode
//!   (`PipeMode::Byte`) — set explicitly rather than inherited from tokio's default, the same
//!   argument ADR-0037 § Decision 2 applies to `reject_remote_clients`: a future port off tokio
//!   must not silently lose it. Message mode would impose datagram boundaries the newline
//!   framing does not need.
//! - **Every CLIENT open sets `SECURITY_SQOS_PRESENT | SECURITY_IDENTIFICATION`**
//!   (`connect`), so a server that wins the pipe-name race cannot impersonate the CLI. The
//!   pipe namespace has no `0700` directory to protect the name, so first-creator-wins cuts
//!   both ways; ADR-0037 records this flag pair as NOT optional, and warns that
//!   `SECURITY_IDENTIFICATION` without `SECURITY_SQOS_PRESENT` is not requested at all.
//! - **The name is derived from the control-socket path**, so the two ends cannot drift
//!   (`windows_pipe_name`, in the Windows arm below).
//!
//! The item names above are code spans rather than intra-doc links on purpose. Each lives in a
//! private per-target `imp` module, which is not in this module's link scope, and
//! `windows_pipe_name` does not exist at all on Unix — so a link would either never resolve or
//! resolve on one target only, and `RUSTDOCFLAGS="-D warnings"` turns that into a failed build.
//! The shape used instead — link the module that IS reachable, name the item beside it in a code
//! span — is the one `crate::canary`'s `reconcile_on_start` reference already takes.
//!
//! NOT here: the peer's identity, which ADR-0037 assigns to **#976** and this item's Boundaries
//! exclude (`crate::daemon::peer_auth` is the Unix half). The one identity-protecting piece that
//! IS here is the client's SQOS flag pair, because it rides on the client's own open call and so
//! is transport code — ADR-0037 § Consequences → Negative splits them the same way.
//!
//! ALSO not here, and on a different footing: the pipe's owner-only security descriptor. That one
//! is not a residual anybody was assigned — ADR-0037 § Decision 2 mandates it in the same sentence
//! as `first_pipe_instance` and `reject_remote_clients`, and this port implements those two and
//! not it. It is tracked at **#1513**; until that lands an instance carries the pipe namespace's
//! DEFAULT descriptor, and the `0600` socket's guarantee has no Windows counterpart.

// The control transport is per-target and neither arm below is portable beyond the targets
// ADR-0029 and ADR-0037 declare. Fail at compile time naming the missing port, rather than
// leaving a bare "cannot find type `ControlStream`" at a dozen call sites — the same #963
// design `crate::daemon::peer_auth` and `crate::contract` apply to their own per-target seams.
#[cfg(not(any(unix, windows)))]
compile_error!(
    "the daemon control channel needs a per-target byte transport; only Unix (a `0600` \
     Unix-domain socket) and Windows (a named pipe) are ported — see ADR-0029 and ADR-0037"
);

#[cfg(unix)]
mod imp {
    use std::io;
    use std::path::Path;

    /// The SERVER side of one accepted control connection.
    ///
    /// On Unix this is the same `tokio::net::UnixStream` the daemon has always served — an
    /// alias, so every existing value flows through unchanged and the Windows arm is the only
    /// place the type actually differs.
    pub(crate) type ControlStream = tokio::net::UnixStream;

    /// The CLIENT side of a control connection — what [`connect`] returns.
    ///
    /// On Unix the two ends are the same type; on Windows they are not (`NamedPipeServer` vs
    /// `NamedPipeClient`), which is why they are named separately here.
    pub(crate) type ControlClient = tokio::net::UnixStream;

    /// The bound control endpoint the daemon accepts on.
    ///
    /// A thin newtype over `tokio::net::UnixListener` so the Windows arm — which has real
    /// per-accept state — can present the same two-method surface.
    pub(crate) struct ControlListener {
        inner: tokio::net::UnixListener,
    }

    impl ControlListener {
        /// Bind the `0600` Unix-domain control socket at `path`, removing any stale socket left
        /// by a previous run first (the single-instance lock guarantees no live daemon owns it).
        /// The enclosing support dir is `0700`, so the socket is owner-only-reachable even during
        /// the bind→chmod window.
        ///
        /// Moved here VERBATIM from `cli::bind_control_socket` (issue #1511): same three steps,
        /// same order, same fail-on-`remove_file`-error branch. The relocation is what lets the
        /// caller stay target-neutral.
        pub(crate) fn bind(path: &Path) -> io::Result<Self> {
            use std::os::unix::fs::PermissionsExt;

            // A leftover socket file makes `bind` fail with EADDRINUSE; the lock we hold
            // means it cannot belong to a running daemon, so remove it. A genuinely
            // absent file is not an error.
            match std::fs::remove_file(path) {
                Ok(()) => {}
                Err(err) if err.kind() == io::ErrorKind::NotFound => {}
                Err(err) => return Err(err),
            }
            let inner = tokio::net::UnixListener::bind(path)?;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
            Ok(Self { inner })
        }

        /// Accept one control connection.
        ///
        /// CANCEL-SAFE, and that is load-bearing rather than incidental: the run loop's idle
        /// `select!` drops this future every time another arm wins (a shutdown, the refresh
        /// tick, the poll wait elapsing), so a cancelled accept must leave the endpoint exactly
        /// as it found it. `UnixListener::accept` is cancel-safe by construction — the listener
        /// is behind `&self` and nothing is taken out of it. The Windows arm has to work for the
        /// same property; see its own docs.
        ///
        /// The peer ADDRESS is discarded: a Unix-domain client is unnamed, and the peer's
        /// identity comes from its credential (`crate::daemon::peer_is_same_user`), never from
        /// an address.
        pub(crate) async fn accept(&self) -> io::Result<ControlStream> {
            self.inner.accept().await.map(|(stream, _addr)| stream)
        }
    }

    /// Connect to the daemon's control endpoint at `path`.
    ///
    /// The error KINDS callers key on are unchanged: an absent socket file, or a stale one with
    /// no listener, is `NotFound` / `ConnectionRefused` — which every client here maps to its own
    /// "no daemon" answer (`Error::DaemonNotRunning`, `Error::UseNextRequiresDaemon`, a cache
    /// miss, or a quiet `None`).
    pub(crate) async fn connect(path: &Path) -> io::Result<ControlClient> {
        tokio::net::UnixStream::connect(path).await
    }

    /// Whether `err` from [`connect`] means the daemon is UP but had no capacity to spare.
    ///
    /// Always `false` here, and not as a stub: a Unix listener has no per-client endpoint to run
    /// out of, so `connect` either reaches the daemon or does not. It exists so the callers that
    /// distinguish "no daemon" from "daemon busy" can be written once, target-neutrally — on this
    /// target the arm is dead code the compiler folds away, and the behaviour is unchanged.
    pub(crate) fn is_saturated(_err: &io::Error) -> bool {
        false
    }

    /// Best-effort removal of the endpoint on the way out.
    ///
    /// On Unix the socket is a real file that outlives the process, so a clean shutdown unlinks
    /// it. Discarded on purpose: the single-instance lock (which releases as the daemon exits)
    /// is what actually guarantees no second daemon, and the next `bind` removes a leftover
    /// anyway — so a failure here is cosmetic.
    pub(crate) fn cleanup(path: &Path) {
        let _ = std::fs::remove_file(path);
    }
}

#[cfg(windows)]
mod imp {
    use std::cell::RefCell;
    use std::ffi::OsString;
    use std::io;
    use std::path::Path;
    use std::time::Duration;

    use tokio::net::windows::named_pipe::{
        ClientOptions, NamedPipeClient, NamedPipeServer, PipeMode, ServerOptions,
    };
    use windows_sys::Win32::Foundation::ERROR_PIPE_BUSY;
    use windows_sys::Win32::Storage::FileSystem::{SECURITY_IDENTIFICATION, SECURITY_SQOS_PRESENT};

    /// The SERVER side of one accepted control connection: a connected pipe INSTANCE.
    pub(crate) type ControlStream = NamedPipeServer;

    /// The CLIENT side of a control connection.
    pub(crate) type ControlClient = NamedPipeClient;

    /// The pipe-namespace prefix every control endpoint sits under.
    ///
    /// `\\.\pipe\` is the local machine's namespace; `reject_remote_clients` below is what
    /// closes the remote half (a `\\<host>\pipe\` open from another machine).
    const PIPE_NAME_PREFIX: &str = r"\\.\pipe\sessiometer-control-";

    /// How long to wait between attempts to create a listening instance when the pipe is at its
    /// instance ceiling. Short enough that a freed instance is picked up promptly, long enough
    /// that a saturated daemon does not spin.
    const INSTANCE_RETRY_INTERVAL: Duration = Duration::from_millis(50);

    /// How long a CLIENT open keeps retrying `ERROR_PIPE_BUSY` before surfacing it.
    ///
    /// Busy means the daemon is up and holds the name but has no free instance right now — the
    /// documented signal to retry, not an error (ADR-0037 § Consequences → Negative). In the
    /// accept loop below the window is a single `create` call wide, so this budget exists for
    /// the pathological saturated case rather than the normal one. BOUNDED here rather than left
    /// to the caller because not every client bounds itself: `cli::query_status` and `poke`'s
    /// best-effort read wrap no timeout of their own around the exchange.
    /// `ControlSocketCache::query_status` reads as a third and is not one — `use_account` wraps
    /// it in `CONTROL_SOCKET_TIMEOUT` at the call site.
    const CLIENT_BUSY_BUDGET: Duration = Duration::from_secs(1);

    /// How long a CLIENT sleeps between `ERROR_PIPE_BUSY` retries.
    const CLIENT_BUSY_RETRY_INTERVAL: Duration = Duration::from_millis(25);

    /// Whether `err` is the "all instances are in use right now" signal.
    ///
    /// Matched on the RAW OS code rather than on `io::ErrorKind`: the kind Rust maps
    /// `ERROR_PIPE_BUSY` to is a std implementation detail that has moved before, and this
    /// predicate decides whether to retry or to surface — too load-bearing to key on a mapping
    /// nothing in this repo pins.
    fn is_pipe_busy(err: &io::Error) -> bool {
        err.raw_os_error() == Some(ERROR_PIPE_BUSY as i32)
    }

    /// Whether `err` from [`connect`] means the daemon is UP but had no capacity to spare.
    ///
    /// The public half of [`is_pipe_busy`], for callers that cannot key on `io::ErrorKind`. A
    /// busy error only reaches a caller after [`CLIENT_BUSY_BUDGET`] has already been spent
    /// retrying, so seeing one means the saturation outlasted that budget — still "the daemon is
    /// running", which is the distinction the caller needs.
    pub(crate) fn is_saturated(err: &io::Error) -> bool {
        is_pipe_busy(err)
    }

    /// The pipe name for the control endpoint whose Unix-side path is `path`.
    ///
    /// The pipe namespace is MACHINE-GLOBAL and flat — it has no per-user directory, which is
    /// exactly the asymmetry ADR-0037 § Decision 2 records against the Unix `0700` support dir.
    /// So the name has to carry the per-user discriminator itself, or two users' daemons would
    /// race for one name on a shared machine and the loser would be denied.
    ///
    /// It is DERIVED from the endpoint path rather than declared separately, so the daemon that
    /// binds and the client that opens cannot drift: both call `paths::control_socket()` and both
    /// arrive here. The path is already per-user (it sits under the user's own support dir), so
    /// hashing it yields the discriminator for free.
    ///
    /// The digest is this crate's own SHA-256 over the path's UTF-16 code units — exact rather
    /// than a lossy `to_string_lossy`, and STABLE across compiler and binary versions, which
    /// `std`'s `DefaultHasher` explicitly is not. That matters because an older CLI may talk to a
    /// newer daemon: a hash that changed with the toolchain would silently point the two ends at
    /// different names. 32 hex characters (128 bits) is far past any collision concern here and
    /// keeps the whole name well inside the 256-character limit.
    ///
    /// Windows paths are case-insensitive while this digest is not, so two spellings of one path
    /// would hash apart. That is inert here because both ends resolve the path through the same
    /// function rather than accepting one from the operator.
    pub(crate) fn windows_pipe_name(path: &Path) -> OsString {
        use std::os::windows::ffi::OsStrExt;

        let mut bytes = Vec::new();
        for unit in path.as_os_str().encode_wide() {
            bytes.extend_from_slice(&unit.to_le_bytes());
        }
        let digest = crate::sha256::sha256_hex(&bytes);
        let mut name = String::with_capacity(PIPE_NAME_PREFIX.len() + 32);
        name.push_str(PIPE_NAME_PREFIX);
        name.push_str(&digest[..32]);
        OsString::from(name)
    }

    /// Create one server instance of the pipe called `name`.
    ///
    /// `first` is the `first_pipe_instance` flag, and it may be set on the FIRST instance only:
    /// a second create that also sets it against a held name is denied `ERROR_ACCESS_DENIED`,
    /// which is precisely the kernel-enforced name reservation ADR-0037 § Decision 2 measured
    /// (spike CHECK 2). Every later instance therefore omits it.
    ///
    /// Two options are set EXPLICITLY although both are already tokio's default, for the reason
    /// ADR-0037 § Decision 2 gives about `reject_remote_clients`: on the raw Win32 API they are
    /// opt-in, so a future port that stops going through tokio must not silently lose them.
    /// `reject_remote_clients` keeps the control channel off the network — the same posture
    /// `CONTRIBUTING.md`'s transport rule and ADR-0011 hold everywhere else — and `PipeMode::Byte`
    /// is what keeps the newline framing meaning what it means (ADR-0037 § Decision 5).
    ///
    /// NOT set here: the owner-only security descriptor
    /// (`create_with_security_attributes_raw` with `D:P(A;;GA;;;<our user SID>)`, the analogue of
    /// the Unix `0600` chmod). ADR-0037 § Decision 2 mandates it in the same sentence as the two
    /// flags above, so it is a gap in that decision rather than a boundary this port respects —
    /// it belongs to no residual the ADR assigns and is tracked at **#1513**. Until it lands the
    /// instance carries the pipe namespace's DEFAULT descriptor, which is one reason the whole
    /// Windows tier lands together behind the #978 CI job and nothing ships from this item
    /// alone.
    fn create_instance(name: &OsString, first: bool) -> io::Result<NamedPipeServer> {
        ServerOptions::new()
            .first_pipe_instance(first)
            .reject_remote_clients(true)
            .pipe_mode(PipeMode::Byte)
            .create(name)
    }

    /// Holds the listening instance OUT of [`ControlListener::idle`] for the duration of one
    /// `connect().await`, and puts it BACK if that await is cancelled.
    ///
    /// This is the cancel-safety mechanism, and it is a guard rather than the more obvious
    /// borrow-across-the-await because that pattern is `clippy::await_holding_refcell_ref`, which
    /// is warn-by-default and so an error under CI's `-D warnings`. Same RAII shape the #972 spike uses to close its impersonation window: the
    /// property must not depend on every arm remembering to restore, since an early return or an
    /// unwinding panic would then leave the endpoint with nothing listening.
    struct PendingAccept<'a> {
        idle: &'a RefCell<Option<NamedPipeServer>>,
        /// The instance being connected, and whatever [`ControlListener::accept`] leaves here is
        /// what [`Drop`] parks back in `idle`. `None` once the caller has claimed it (a completed
        /// accept), or where a failed `connect` deliberately discarded it — which is one of the
        /// three ways that failure resolves, not the only one.
        server: Option<NamedPipeServer>,
    }

    impl Drop for PendingAccept<'_> {
        fn drop(&mut self) {
            if let Some(server) = self.server.take() {
                // Cancelled mid-connect: the instance is untouched and still listening, so put it
                // back rather than closing it. Closing the last instance would release the pipe NAME,
                // and an arriving client would then read "no daemon" from `ERROR_FILE_NOT_FOUND`.
                *self.idle.borrow_mut() = Some(server);
            }
        }
    }

    /// The bound control endpoint the daemon accepts on.
    ///
    /// Holds the pipe NAME (so later instances can be created from it) and the one instance
    /// that is currently listening. `RefCell` because [`Control::serve`](crate::daemon::Control)
    /// takes `&self` while a named-pipe accept genuinely mutates state — there is no listening
    /// socket to accept repeatedly from. Sound without a lock: the daemon is a `current_thread`
    /// runtime (ADR-0001) and the run loop is the only caller, so the borrows below never
    /// overlap, and none is held across an `.await` — [`PendingAccept`] is what carries the
    /// instance through the one await that would otherwise need it.
    pub(crate) struct ControlListener {
        name: OsString,
        /// The created-but-not-yet-connected instance. `None` in two windows: after an instance
        /// was handed out and its replacement was refused, and after a `connect` failed and its
        /// replacement was refused as busy. The next [`ControlListener::accept`] waits for one —
        /// for as long as the refusal keeps being `ERROR_PIPE_BUSY`; any other error surfaces.
        idle: RefCell<Option<NamedPipeServer>>,
    }

    impl ControlListener {
        /// Create the control pipe and its first instance.
        ///
        /// The analogue of the Unix `bind`, and simpler in one way ADR-0037 § Consequences →
        /// Positive names: the pipe namespace is not the filesystem, so there is no stale
        /// endpoint to unlink first.
        ///
        /// The first instance carries `first_pipe_instance`, so this call FAILS
        /// (`ERROR_ACCESS_DENIED`) if the name is already held — a kernel-enforced half of the
        /// single-instance guarantee that the Unix side gets only from its lock file.
        pub(crate) fn bind(path: &Path) -> io::Result<Self> {
            let name = windows_pipe_name(path);
            let first = create_instance(&name, true)?;
            Ok(Self {
                name,
                idle: RefCell::new(Some(first)),
            })
        }

        /// Wait until a listening instance can be created, then create it.
        ///
        /// `ERROR_PIPE_BUSY` from `create` means no instance is available right now. It does NOT
        /// mean a ceiling of 255 was reached, and an earlier revision of this comment said it
        /// did: this code never calls `ServerOptions::max_instances`, so it takes tokio's default
        /// of `PIPE_UNLIMITED_INSTANCES`, which is a SENTINEL rather than a count — Windows
        /// documents the number of instances under it as limited only by the availability of
        /// system resources. 255 is the value of that sentinel and is the one number
        /// `max_instances` refuses outright (`assert!(instances < 255)`, so 254 is the largest
        /// ceiling that can be set at all).
        ///
        /// The retry predicate is correspondingly narrower than "at capacity": it keys on
        /// `ERROR_PIPE_BUSY` and surfaces everything else. Whether exhausting system resources
        /// reports busy or reports something else is UNMEASURED — the #972 spike pinned a small
        /// `max_instances` so that a ceiling was reachable inside a CI run, which is not this
        /// configuration. A non-busy error surfaces, and `UnixControl::serve` turns a failed
        /// accept into an event the run loop re-arms immediately — so it is PACED before it is
        /// surfaced, which is the only thing between it and a hot loop on the daemon's single
        /// thread. It is still a failure nothing logs or counts; #978 is where it first becomes
        /// observable at all.
        ///
        /// It WAITS rather than erroring because the run loop treats a resolved `serve` as an
        /// event: returning `Err` at the ceiling would resolve the select arm immediately and
        /// spin the idle loop hot for as long as the saturation lasted. Waiting keeps `accept`
        /// pending exactly like a Unix `accept` with no client, which is the semantics the select
        /// is written against.
        async fn wait_for_instance(&self) -> io::Result<NamedPipeServer> {
            loop {
                match create_instance(&self.name, false) {
                    Ok(server) => return Ok(server),
                    Err(err) if is_pipe_busy(&err) => {
                        tokio::time::sleep(INSTANCE_RETRY_INTERVAL).await;
                    }
                    // Paced for the same reason the failed-`connect` arm below is, and this one
                    // is MORE likely to be reached: under the default `PIPE_UNLIMITED_INSTANCES`
                    // an exhausted create is at least as likely to report something other than
                    // `ERROR_PIPE_BUSY`, which lands here. Surfacing it un-paced is a hot loop
                    // rather than a retry — `create_instance` is synchronous, so nothing in
                    // `accept` suspends before the `?`, and the run loop's `select!` is `biased`
                    // with `serve` ahead of the timer, so a `serve` that resolves immediately
                    // starves every other arm and every spawned task on the one thread.
                    Err(err) => {
                        tokio::time::sleep(INSTANCE_RETRY_INTERVAL).await;
                        return Err(err);
                    }
                }
            }
        }

        /// Accept one control connection.
        ///
        /// THE INSTANCE ACCOUNTING, which is what ADR-0037 § Consequences → Negative calls the
        /// largest single piece of work the decision implies:
        ///
        /// - **Exactly ONE instance is listening at a time.** The endpoint keeps a single
        ///   created-but-unconnected instance; a client connects to it, and its replacement is
        ///   created immediately.
        /// - **Outstanding instances = 1 listening + one per live exchange.** A one-shot command
        ///   holds its instance for the length of the exchange. A `watch` subscription (#165)
        ///   holds one for its whole lifetime, which is where one-client-per-instance bites
        ///   hardest.
        /// - **There is no fixed ceiling.** `max_instances` is never set, so instances are
        ///   bounded by system resources rather than by a number — see
        ///   [`ControlListener::wait_for_instance`], which also records what that costs the retry
        ///   predicate. When none can be created the replacement
        ///   fails `ERROR_PIPE_BUSY`; this leaves no listening instance, so an arriving client
        ///   also gets `ERROR_PIPE_BUSY` — which [`connect`] retries — and the next `accept`
        ///   waits (see [`ControlListener::wait_for_instance`]) until a subscriber disconnects.
        ///   Nothing is
        ///   dropped and no client is refused outright; service resumes on its own.
        /// - **The NAME is held whenever an instance exists, and one normally does** — but
        ///   NOT unconditionally, and an earlier revision of this list claimed otherwise. A
        ///   released name is the worst outcome available here: a client sees
        ///   `ERROR_FILE_NOT_FOUND` and reports "no daemon" against a running daemon, and any
        ///   local process may then take the name. Two places therefore hold it deliberately —
        ///   the replacement is created BEFORE the connected instance is handed out, and a
        ///   `connect` that FAILS does not drop its instance until a replacement exists, since
        ///   `idle` is already empty by then.
        ///
        ///   The hole both of those leave open is the REFILL, which is a single attempt on
        ///   purpose (below). If it is refused, `idle` is empty and the handed-out instance is
        ///   the only handle the process holds; when that exchange ends and the stream drops,
        ///   the count reaches zero and the name goes with it. Serving the client now rather
        ///   than stalling it behind a saturated pipe is worth that window, but the window is
        ///   real: under the default `PIPE_UNLIMITED_INSTANCES` it takes resource exhaustion to
        ///   reach, and NOTHING here measures it. Do not restate the guarantee as absolute.
        ///
        /// CANCEL-SAFE, and the structure below is what buys it. The run loop's idle `select!`
        /// drops this future whenever another arm wins, which on a busy daemon is most ticks. The
        /// listening instance is therefore carried through `connect().await` by [`PendingAccept`],
        /// whose `Drop` puts it back — so a cancelled accept leaves the endpoint exactly as it
        /// found it. Simply closing it would release the pipe NAME, and a client would then read
        /// "no daemon" out of `ERROR_FILE_NOT_FOUND`. Cancellation AFTER `connect()` resolved is
        /// harmless too: the client stays attached and the next `accept` re-awaits `connect()` on
        /// the same instance, which returns immediately for an already-connected pipe.
        pub(crate) async fn accept(&self) -> io::Result<ControlStream> {
            // Make sure something is listening. The `Ref` temporary in the condition is dropped
            // before the block runs, so nothing is held across the await inside it.
            if self.idle.borrow().is_none() {
                let created = self.wait_for_instance().await?;
                *self.idle.borrow_mut() = Some(created);
            }

            // Take the instance out under the restore guard, then await the connection. A
            // cancelled accept drops the guard, which puts the instance back listening.
            let mut pending = PendingAccept {
                idle: &self.idle,
                server: self.idle.borrow_mut().take(),
            };
            if let Err(err) = pending
                .server
                .as_ref()
                .expect("a listening instance was just ensured")
                .connect()
                .await
            {
                // Discarding this instance is the right move — re-awaiting a `connect` that just
                // failed is an error that spins rather than recovers, because the run loop
                // re-arms `serve` as soon as it resolves. But `idle` was emptied above, so with
                // no live connection this instance is the ONLY one the process holds, and
                // dropping it first would release the NAME (see the accounting on this method).
                // So replace before releasing, and let the failure decide how:
                match create_instance(&self.name, false) {
                    // A fresh instance is listening. Assigning it here drops the failed one, and
                    // the guard parks the replacement in `idle` — the name is held throughout.
                    Ok(next) => pending.server = Some(next),
                    // Busy means some OTHER instance is alive, so the name does not depend on
                    // this one: discard it, exactly as before.
                    Err(create_err) if is_pipe_busy(&create_err) => pending.server = None,
                    // Anything else says nothing about whether another instance exists. Keep
                    // the failed one listening rather than gamble the name on it — and PACE the
                    // failure, because nothing else will. `UnixControl::serve` maps a failed
                    // accept to `ControlYield::Signal(None)` and the run loop re-arms `serve`
                    // the moment it resolves, with no suspension point anywhere in between: a
                    // non-`WouldBlock` `connect` error returns on the first poll and
                    // `create_instance` is synchronous, so without this sleep the arm is a hot
                    // loop on the daemon's single thread, not a retry. Even paced it is a POOR
                    // outcome, and it should be read as one: nothing logs or counts it today, so
                    // it is invisible until #978 makes the path executable. It is chosen only
                    // against the alternative of releasing the name, which is silent too and
                    // additionally lets another process take it.
                    Err(_) => tokio::time::sleep(INSTANCE_RETRY_INTERVAL).await,
                }
                return Err(err);
            }
            let connected = pending
                .server
                .take()
                .expect("the connected instance is claimed exactly once");

            // Replace it before handing the connected one out, so the name is never unheld. A
            // single attempt: at the ceiling this leaves `idle` empty and the NEXT accept waits,
            // which serves this client now instead of stalling it behind a saturated pipe. Any
            // other error is likewise deferred to that accept, which surfaces it.
            if let Ok(next) = create_instance(&self.name, false) {
                *self.idle.borrow_mut() = Some(next);
            }

            Ok(connected)
        }
    }

    /// Connect to the daemon's control endpoint at `path`.
    ///
    /// `SECURITY_SQOS_PRESENT | SECURITY_IDENTIFICATION` is set on EVERY open (ADR-0037
    /// § Consequences → Negative, restated in § What this spike did NOT establish at the same
    /// strength): the pipe namespace has no directory to protect the name, so a foreign local
    /// process that creates `\\.\pipe\sessiometer-control-…` first can stand a server in front of
    /// the CLI. These flags cap what such a server may do with the token it impersonates at
    /// IDENTIFICATION — query it, never act as us. It is a binary the port either sets or does
    /// not, and nothing enforces it; `SECURITY_IDENTIFICATION` alone is not even requested
    /// without `SECURITY_SQOS_PRESENT`, so BOTH are named here rather than relying on tokio's
    /// default — which today happens to be exactly this pair, and which `security_qos_flags`
    /// would silently replace if a caller ever passed something narrower.
    ///
    /// The CLIENT-side check of the SERVER's owner SID — the other half of that squat defence —
    /// is **#976**'s and is not here.
    ///
    /// The error KINDS callers key on line up with the Unix arm: a daemon that is not running
    /// leaves no pipe name, so `CreateFile` fails `ERROR_FILE_NOT_FOUND` → `NotFound`, which is
    /// already what every client maps to its own "no daemon" answer. `ERROR_PIPE_BUSY` is
    /// different in kind — the daemon IS up and holds the name, it just has no free instance
    /// this instant — so it is retried here rather than surfaced as a failure, bounded by
    /// [`CLIENT_BUSY_BUDGET`]. If the budget runs out the busy error is returned as-is: it is
    /// NOT `NotFound`, and [`is_saturated`] is what a caller that discards the kind can key on
    /// instead.
    ///
    /// That is NOT a clean dichotomy, and `ControlListener::accept`'s accounting says why: a
    /// daemon whose instances reach zero stops holding the name, and a client then gets busy for
    /// a moment and `NotFound` after — a running daemon reported as absent, with the whole busy
    /// budget potentially spent watching it change. THREE callers discard the kind
    /// (`crate::poke`, `use_account`'s cache, and `socket::request_swap`); only the last takes
    /// the guard, because only it can double-write on a wrong answer. The other two degrade into
    /// an extra live poll and a quieter message.
    pub(crate) async fn connect(path: &Path) -> io::Result<ControlClient> {
        let name = windows_pipe_name(path);
        let deadline = tokio::time::Instant::now() + CLIENT_BUSY_BUDGET;
        loop {
            let attempt = ClientOptions::new()
                .security_qos_flags(SECURITY_SQOS_PRESENT | SECURITY_IDENTIFICATION)
                .pipe_mode(PipeMode::Byte)
                .open(&name);
            match attempt {
                Ok(client) => return Ok(client),
                Err(err) if is_pipe_busy(&err) => {
                    if tokio::time::Instant::now() >= deadline {
                        return Err(err);
                    }
                    tokio::time::sleep(CLIENT_BUSY_RETRY_INTERVAL).await;
                }
                Err(err) => return Err(err),
            }
        }
    }

    /// Best-effort removal of the endpoint on the way out — a NO-OP on this target.
    ///
    /// The pipe namespace is not the filesystem: the name exists only while an instance handle
    /// is open and disappears with the process, so there is nothing to unlink (ADR-0037
    /// § Consequences → Positive). Kept as a call rather than cfg'd away at the one call site so
    /// the daemon's shutdown path stays target-neutral.
    pub(crate) fn cleanup(_path: &Path) {}
}

// `ControlClient` is deliberately NOT re-exported: it is `connect`'s return type and every
// caller infers it, so naming it here would be an unused import under `-D warnings`. Its
// definition inside each arm is what documents the client/server type split.
pub(crate) use imp::{cleanup, connect, is_saturated, ControlListener, ControlStream};

#[cfg(all(test, unix))]
mod unix_tests {
    use super::ControlListener;
    use std::os::unix::fs::PermissionsExt;

    /// The `0600` mode is the WHOLE of the Unix control channel's access control — nothing checks
    /// the peer before the bytes are read — and until this test nothing anywhere asserted it. The
    /// relocation in #1511 moved the `chmod` between files; had it dropped the call, every gate in
    /// the repo would still have gone green, and "relocated, not rewritten" would have rested on
    /// reading the two versions side by side.
    #[tokio::test]
    async fn bind_leaves_the_socket_owner_only() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("daemon.sock");
        let listener = ControlListener::bind(&path).expect("bind");
        assert_eq!(
            mode_of(&path),
            0o600,
            "the control socket must be owner-only"
        );
        drop(listener);
    }

    /// `bind` is documented as safe to run over a leftover socket, which is what makes the
    /// single-instance lock sufficient on its own. Exercised here because the branch is otherwise
    /// dead in every test: the daemon only reaches it after an unclean exit. The leftover is
    /// seeded world-writable so that inheriting its mode would be visible rather than incidental.
    #[tokio::test]
    async fn bind_replaces_a_leftover_socket_and_still_chmods_it() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("daemon.sock");
        std::fs::write(&path, b"leftover").expect("seed a stale socket path");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o666))
            .expect("make the leftover deliberately too permissive");
        let listener = ControlListener::bind(&path).expect("bind over the leftover");
        assert_eq!(
            mode_of(&path),
            0o600,
            "a leftover must not carry its mode over"
        );
        drop(listener);
    }

    fn mode_of(path: &std::path::Path) -> u32 {
        std::fs::metadata(path).expect("stat").permissions().mode() & 0o777
    }
}

#[cfg(all(test, windows))]
mod windows_tests {
    use super::imp::windows_pipe_name;
    use std::path::Path;

    #[test]
    fn the_pipe_name_is_derived_from_the_path_and_is_stable() {
        let a = windows_pipe_name(Path::new(
            r"C:\Users\alice\AppData\Local\sessiometer\daemon.sock",
        ));
        let b = windows_pipe_name(Path::new(
            r"C:\Users\alice\AppData\Local\sessiometer\daemon.sock",
        ));
        let other = windows_pipe_name(Path::new(
            r"C:\Users\bob\AppData\Local\sessiometer\daemon.sock",
        ));

        // Same path → same name, or the daemon and the CLI would look at different pipes.
        assert_eq!(a, b);
        // Different user → different name: the namespace is machine-global and flat, so the
        // per-user discriminator has to come from here (ADR-0037 § Decision 2).
        assert_ne!(a, other);

        let rendered = a.to_string_lossy().into_owned();
        assert!(
            rendered.starts_with(r"\\.\pipe\sessiometer-control-"),
            "unexpected pipe name: {rendered}"
        );
        // Well inside the 256-character limit, and the digest is fixed-width regardless of how
        // long the support-dir path is.
        assert_eq!(rendered.len(), r"\\.\pipe\sessiometer-control-".len() + 32);
    }
}
