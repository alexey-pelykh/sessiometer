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
//! than mechanics. The first three are recorded in the ADR and cite it; the fourth is decided
//! HERE, and says so:
//!
//! - **One client per INSTANCE.** There is no listening socket that accepts repeatedly. The
//!   server creates a new instance per accept and only the FIRST carries `first_pipe_instance`
//!   (ADR-0037 § Consequences → Negative). `ControlListener::accept` is where that structural
//!   difference lives; its own docs carry the instance accounting.
//! - **The framing survives unchanged.** The pipe stays in BYTE mode
//!   (`PipeMode::Byte`) — set explicitly rather than inherited. This is NOT the
//!   `reject_remote_clients` argument ADR-0037 § Decision 2 makes, though an earlier revision of
//!   this comment borrowed it: byte mode is the raw Win32 DEFAULT too (`PIPE_TYPE_BYTE` and
//!   `PIPE_READMODE_BYTE` are both `0`; `PIPE_TYPE_MESSAGE` is what is opt-in), so a port off
//!   tokio could not silently lose it. It is written out because the default is TOKIO's to change
//!   and the wire compatibility resting on it is ours. Message mode would impose datagram
//!   boundaries the newline framing does not need.
//! - **Every CLIENT open sets `SECURITY_SQOS_PRESENT | SECURITY_IDENTIFICATION`**
//!   (`connect`), so a server that wins the pipe-name race cannot impersonate the CLI. The
//!   pipe namespace has no `0700` directory to protect the name, so first-creator-wins cuts
//!   both ways; ADR-0037 records this flag pair as NOT optional, and warns that
//!   `SECURITY_IDENTIFICATION` without `SECURITY_SQOS_PRESENT` is not requested at all.
//! - **The name is derived from the control-socket path**, so the two ends cannot drift
//!   (`windows_pipe_name`, in the Windows arm below). NOT an ADR decision: ADR-0037 only ever
//!   writes the name elided (`\\.\pipe\sessiometer-...`) and fixes no derivation, so the
//!   digest, its truncation and the prefix are chosen in this module and carry their rationale
//!   there rather than a `§` citation.
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
    /// to the caller because not every client bounds itself — and boundedness is a property of
    /// the CALL SITE, not of the function. `poke`'s best-effort read wraps no timeout of its own,
    /// and neither does the plain `status` verb, which awaits `cli::query_status` directly. That
    /// function's OTHER caller does bound it: `probe_socket_responsive` — the liveness probe
    /// `daemon status` and `daemon restart` share — wraps it in `DAEMON_STATUS_SOCKET_TIMEOUT`.
    /// `ControlSocketCache::query_status` is bounded that same way, by `use_account`'s
    /// `CONTROL_SOCKET_TIMEOUT`. One unbounded site is enough to need this budget.
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
    /// hash apart — and that is NOT inert, though an earlier revision of this comment said it
    /// was, on the grounds that both ends resolve the path through the same function. They do;
    /// the function does not return the same STRING in every context. `src/paths.rs` records that
    /// the Windows resolver is env-first and that "the never-overridable invariant is NOT yet
    /// delivered on that target", so a daemon started in a service context and a CLI started in
    /// an interactive shell can hold two spellings Windows opens as one directory. Case is not
    /// even the only axis: a trailing separator, an 8.3 short name and a UNC-versus-drive
    /// spelling all diverge with no case difference at all.
    ///
    /// Unix does not have this because the socket path IS the rendezvous and the KERNEL
    /// dereferences it; digesting the path moves resolution into this function and removes that
    /// absorption. The failure it produces is a CLI reporting "no daemon" against a live one
    /// while `daemon.lock` — reached by path, under that same directory — still resolves. Tracked
    /// at **#1516**; nothing here normalizes the spelling yet, and this comment is not a claim
    /// that it does.
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
    /// Two options are set EXPLICITLY although both are already tokio's default, and they are set
    /// for DIFFERENT reasons — an earlier revision of this comment gave them one reason, which was
    /// true of only the first. `reject_remote_clients` keeps the control channel off the network,
    /// the same posture `CONTRIBUTING.md`'s transport rule and ADR-0011 hold everywhere else, and
    /// it is spelled out for the reason ADR-0037 § Decision 2 gives about it in particular: on the
    /// raw Win32 API it is opt-in (`PIPE_REJECT_REMOTE_CLIENTS` is `8`,
    /// `PIPE_ACCEPT_REMOTE_CLIENTS` is `0`), so a port that stops going through tokio would
    /// silently lose it. That argument does NOT extend to `PipeMode::Byte`, which is the raw Win32
    /// default as well (`PIPE_TYPE_BYTE` and `PIPE_READMODE_BYTE` are both `0`; `PIPE_TYPE_MESSAGE`
    /// is the opt-in). Byte mode is written out because it is what keeps the newline framing
    /// meaning what it means (ADR-0037 § Decision 5), and because the default is tokio's to change.
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
        /// what [`Drop`] parks back in `idle`. `None` ONLY once the caller has claimed it (a
        /// completed accept): the failed-`connect` path resolves two ways and neither discards,
        /// so it always leaves an instance here for [`Drop`] to park.
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
        /// The created-but-not-yet-connected instance. `None` in ONE window: after an instance
        /// was handed out and its replacement was refused. A failed `connect` no longer leaves it
        /// empty — that path keeps its own instance whenever no replacement can be made, so
        /// [`Drop`] always parks one back. The next [`ControlListener::accept`] waits for one —
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
        /// system resources. 255 is the value of that sentinel, and it is the SMALLEST value
        /// `max_instances` refuses — `assert!(instances < 255)` on a `usize` refuses it and
        /// everything above it, leaving 254 as the largest ceiling that can be set at all.
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
                    // Paced for the same reason the failed-`connect` arm below is. How OFTEN it
                    // is reached is not stated here, and an earlier revision of this comment did
                    // state it — as a comparative against the busy arm, on the question
                    // [`ControlListener::wait_for_instance`] fifteen lines up declares UNMEASURED:
                    // what an exhausted create reports under the default `PIPE_UNLIMITED_INSTANCES`
                    // is exactly what nobody here has measured. Surfacing it un-paced is a hot loop
                    // rather than a retry — `create_instance` is synchronous, so nothing in
                    // `accept` suspends before the `?`, and the run loop's `select!` is `biased`
                    // with `serve` ahead of the timer, so a `serve` that resolves immediately
                    // starves every arm BELOW it and every spawned task on the one thread.
                    // `shutdown.requested()` is polled ABOVE `serve` and so is not starved.
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
        ///   dropped and no client is refused outright; service resumes on its own. That is the
        ///   BUSY outcome, and the only one this bullet describes: under
        ///   `PIPE_UNLIMITED_INSTANCES` whether exhaustion reports busy AT ALL is unmeasured,
        ///   and a non-busy refusal does NOT resume unattended —
        ///   [`ControlListener::wait_for_instance`] paces it and SURFACES it, and the failed
        ///   accept becomes an event the run loop re-arms.
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
        /// "no daemon" out of `ERROR_FILE_NOT_FOUND`. There is no second window to reason about:
        /// once `connect()` resolves, nothing between it and the `Ok` suspends — the `take`, the
        /// single refill attempt and the return are all synchronous — so a cancellation cannot
        /// land there. An earlier revision of this comment claimed one could and dismissed it on
        /// the premise that re-awaiting `connect()` returns immediately for an already-connected
        /// pipe. That premise does not hold either: mio maps both `ERROR_PIPE_CONNECTED` and
        /// `ERROR_NO_DATA` to success, so a re-await can resolve `Ok` on an instance whose peer
        /// has already gone, and nothing here calls `DisconnectNamedPipe` to reset one.
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
                // Not re-awaiting this `connect` is the right move — re-awaiting one that just
                // failed spins rather than recovers, because the run loop re-arms `serve` as soon
                // as it resolves. But `idle` was emptied above, so with no live connection this
                // instance is the ONLY one the process holds, and dropping it would release the
                // NAME (see the accounting on this method). So try to REPLACE it, and keep it
                // when the replacement cannot be made:
                match create_instance(&self.name, false) {
                    // A fresh instance is listening. Assigning it here drops the failed one, and
                    // the guard parks the replacement in `idle` — the name is held throughout.
                    Ok(next) => pending.server = Some(next),
                    // NO failed create says another instance exists — `ERROR_PIPE_BUSY` included,
                    // and an earlier revision of this arm discarded the instance on the premise
                    // that it did. Busy carries that meaning only under a PINNED `max_instances`,
                    // which production never sets: it takes tokio's default
                    // `PIPE_UNLIMITED_INSTANCES`, under which whether exhaustion even REPORTS
                    // busy is unmeasured — [`ControlListener::wait_for_instance`] records this,
                    // and reading busy as "some other instance is alive" here contradicted it.
                    // So keep the failed one listening rather than gamble the name on an
                    // inference this module elsewhere declines to make — and PACE the failure,
                    // because nothing else will. `UnixControl::serve` maps a failed
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

            // Replace it before handing the connected one out, so the HANDOVER never leaves the
            // name unheld. That is narrower than the name always being held, and the doc above
            // says why: a single attempt on purpose, so a refused one leaves `idle` empty and the
            // handed-out instance is the only handle left. At the ceiling that leaves the NEXT
            // accept waiting, which serves this client now instead of stalling it behind a
            // saturated pipe. Any other error is likewise deferred to that accept, which
            // surfaces it.
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
    /// not, and nothing enforces it. Both are named here rather than left to tokio's default,
    /// which today is exactly this pair (`ClientOptions::new`) — but the default is tokio's to
    /// change, and naming them makes the impersonation level this module's decision.
    ///
    /// Two things about that call are worth stating precisely, because an earlier revision of
    /// this comment got the direction of the hazard backwards. `SECURITY_SQOS_PRESENT` cannot be
    /// lost through `security_qos_flags` at all: tokio ORs it in unconditionally
    /// (`self.security_qos_flags = flags | SECURITY_SQOS_PRESENT`), so the presence bit is not
    /// what the explicit call buys. What it pins is the LEVEL, and the dangerous direction there
    /// is WIDER, not narrower — `SECURITY_IMPERSONATION` or `SECURITY_DELEGATION` would let a
    /// squatting server act as us, which is the whole exposure. `SECURITY_ANONYMOUS` is `0` and
    /// fails closed. The setter is last-write-wins, so a second call appended to this chain would
    /// override this one silently; the source guard at the foot of this file counts the calls
    /// against the opens for that reason, and cannot do better than counting.
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
    /// budget potentially spent watching it change. Every caller that discards the kind inherits
    /// that ambiguity: `crate::poke`, `use_account`'s cache, `use_account`'s manual-hold notifier
    /// (`ControlSocketNotifier::notify`), `socket::notify_restored`, and `socket::request_swap`.
    /// An earlier revision of this comment named the first, second and last and called that the
    /// whole set. Only `request_swap` takes [`is_saturated`], because only it can double-write on
    /// a wrong answer; the cache and `poke` degrade into an extra live poll and a quieter message.
    /// The two notifiers are the ones to read carefully: both print "is the daemon running?"
    /// (`use_account.rs`'s manual-hold path and `capture.rs`'s restore path), which on Windows can
    /// now be printed about a daemon that is running — a saturated one past the budget, or one in
    /// the zero-instance window. Both are best-effort notifications whose authoritative write has
    /// already landed, so the message misleads without changing an outcome.
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

    /// The `0600` mode is the Unix control channel's REACHABILITY control — who can open the
    /// socket at all — which `src/daemon/socket.rs` calls defense-in-depth beside the peer check
    /// it runs BEFORE any state-affecting command. Until this test nothing anywhere asserted the
    /// mode, which is what makes it worth pinning even though it is not the only layer. The
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
    /// dead in every test: the daemon only reaches it after an unclean exit. What the seed buys
    /// is the BRANCH, not the mode: `bind` unlinks the leftover and creates a fresh socket, so
    /// none of the old file survives to be inherited. Deleting the `chmod` was measured against
    /// this test rather than assumed — the mode comes back `0o755`, derived from the process
    /// umask, NOT the `0o666` seeded below. The seed stays conspicuous so an implementation that
    /// did inherit would be distinguishable from that one, but the umask case is the assertion's
    /// teeth.
    ///
    /// The seed is a REAL leftover socket rather than a regular file standing in for one. Nothing
    /// in `bind` discriminates — `remove_file` unlinks either — so the branch would be reached
    /// the same way. It is a socket because the test says it is: a seed that does not match the
    /// name is how a test quietly stops describing the scenario it is kept for. Dropping a
    /// `UnixListener` does NOT unlink its path, which is the whole reason `bind` needs the
    /// `remove_file` this test exercises, so binding and dropping leaves exactly the artifact an
    /// unclean exit leaves behind.
    #[tokio::test]
    async fn bind_replaces_a_leftover_socket_and_still_chmods_it() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("daemon.sock");
        drop(
            std::os::unix::net::UnixListener::bind(&path)
                .expect("seed a real leftover socket at the control path"),
        );
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

/// The pipe options this port is contractually required to set, pinned by SPELLING because
/// nothing in this repo can pin them by behaviour: no CI job compiles the Windows arm — #978 is
/// the item that would — so deleting the `security_qos_flags` call builds, lints, tests and
/// merges green on every gate that actually runs. `first_pipe_instance`,
/// `reject_remote_clients` and the explicit byte mode stand the same way. Scanning source text is
/// this repo's existing answer for a claim its test target cannot reach (`src/witness.rs`'s
/// forbidden-token sweep, `src/usage.rs`'s egress scan), and unlike the module it guards it runs
/// on every target.
///
/// What a green here means, exactly, and the bound is not a formality: the calls are WRITTEN, at
/// the arity and spelling asserted below. It is not evidence that Windows honours them, that they
/// achieve what ADR-0037 says they achieve, or that the transport works at all — an executable
/// round-trip is **#1514**'s, behind the #978 job. AC5 is a claim about what every open DOES;
/// this is the weaker claim that every open is written to. Read the test names as naming the
/// clause each one is derived from, never as discharging it.
///
/// Two mutations were run against it rather than assumed, because a spelling guard's whole value
/// is which edits it survives. Deleting any pinned call goes red, and so does adding a second
/// client open without the flags. Appending a SECOND `security_qos_flags` to the same builder
/// chain — which compiles, since the setter takes `&mut self` and is last-write-wins — defeats an
/// exact-spelling match on its own, and is why the flag test also counts the calls by NAME.
#[cfg(test)]
mod windows_option_source_guard {
    /// This file's own source up to the start of this guard, comment-only lines dropped and
    /// whitespace collapsed.
    ///
    /// Three reductions, each load-bearing. Dropping comment-only lines is what keeps the counts
    /// honest: every option below is named several times in the prose above, so a scan of the raw
    /// text would be satisfied by doc comments alone. The file carries no trailing comments, so
    /// dropping whole lines is complete; should one ever appear carrying one of these names the
    /// count inflates and this guard goes RED — an alarm that points here, never a silent pass.
    /// Collapsing whitespace keeps it stable under `cargo fmt`: a call the formatter wraps across
    /// lines still matches. And cutting the text at this guard's own module header is what stops
    /// the needles below from counting THEMSELVES — a scan that matches its own literals stays
    /// green with the code it guards deleted, which was measured, not supposed.
    fn transport_code() -> String {
        collapse(&read_source(&module_path()))
            .split_once("mod windows_option_source_guard {")
            .expect("source scan is broken: did not find this guard's own module header")
            .0
            .to_owned()
    }

    fn module_path() -> std::path::PathBuf {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("src")
            .join("control_transport.rs")
    }

    fn read_source(path: &std::path::Path) -> String {
        std::fs::read_to_string(path).unwrap_or_else(|err| panic!("read {}: {err}", path.display()))
    }

    /// Comment-only lines dropped, then whitespace collapsed — the reduction described above.
    fn collapse(text: &str) -> String {
        text.lines()
            .filter(|line| !line.trim_start().starts_with("//"))
            .flat_map(str::split_whitespace)
            .collect::<Vec<_>>()
            .join(" ")
    }

    /// Every `.rs` file under `src/` EXCEPT this one, reduced the same way.
    ///
    /// This is what makes the "no client open outside this module" test a statement about the
    /// crate rather than about one file. Modelled on `src/usage.rs`'s walk, canary included.
    fn other_sources() -> Vec<(std::path::PathBuf, String)> {
        fn walk(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
            for entry in std::fs::read_dir(dir).expect("read_dir under src") {
                let path = entry.expect("dir entry").path();
                if path.is_dir() {
                    walk(&path, out);
                } else if path.extension().is_some_and(|ext| ext == "rs") {
                    out.push(path);
                }
            }
        }
        let mut paths = Vec::new();
        walk(
            &std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src"),
            &mut paths,
        );
        let this = module_path();
        assert!(
            paths.contains(&this),
            "source scan is broken: the walk under src/ did not reach control_transport.rs itself"
        );
        paths
            .into_iter()
            .filter(|path| path != &this)
            .map(|path| {
                let text = collapse(&read_source(&path));
                (path, text)
            })
            .collect()
    }

    /// `(client opens, server instance creations)` in the scanned text, with the canary that
    /// makes a zero read as "the scan broke" rather than "the calls are gone".
    fn builders(code: &str) -> (usize, usize) {
        let clients = code.matches("ClientOptions::new()").count();
        let servers = code.matches("ServerOptions::new()").count();
        assert!(
            clients > 0 && servers > 0,
            "source scan is broken: found no pipe option builders in src/control_transport.rs"
        );
        (clients, servers)
    }

    /// AC5's flags, and the only one of these options the issue states as an acceptance criterion:
    /// a client open that hands a squatting server an identification-level token, which it can
    /// query and cannot act with (ADR-0037 § Consequences → Negative).
    ///
    /// Asserted twice on purpose, because one assertion each way is what the two mutations need.
    /// The whole call is matched rather than the constants, since both names are also
    /// `use`-imported at the top of the Windows arm and a bare constant search stays green over a
    /// deleted call; matching through the closing paren is what makes a WIDENED level
    /// (`| SECURITY_IMPERSONATION`) fail rather than match as a prefix. And the calls are counted
    /// by NAME as well, because the setter is last-write-wins: a second one appended to the chain
    /// silently overrides the first while leaving the exact-spelling count untouched.
    #[test]
    fn every_client_open_sets_the_ac5_security_qos_flags() {
        let code = transport_code();
        let (clients, _) = builders(&code);
        assert_eq!(
            code.matches(".security_qos_flags(SECURITY_SQOS_PRESENT | SECURITY_IDENTIFICATION)")
                .count(),
            clients,
            "every client-side pipe open must set the AC5 flags (issue #1511 AC5)"
        );
        assert_eq!(
            code.matches(".security_qos_flags(").count(),
            clients,
            "a second security_qos_flags call would override the first — one per client open"
        );
    }

    /// AC5 says EVERY client-side open, so the count above is only half the claim: it would hold
    /// while a second module opened a pipe with no flags at all. Every client today routes through
    /// [`connect`], and this is what keeps that true.
    #[test]
    fn no_client_pipe_open_exists_outside_this_module() {
        let offenders: Vec<_> = other_sources()
            .into_iter()
            .filter(|(_, text)| text.contains("ClientOptions::"))
            .map(|(path, _)| path)
            .collect();
        assert!(
            offenders.is_empty(),
            "a client pipe open outside control_transport.rs is unguarded by AC5's flags: {offenders:?}"
        );
    }

    /// ADR-0037 § Decision 2 mandates this in the same breath as the flags: the pipe namespace is
    /// machine-global, so an instance created without it is reachable over SMB from another host.
    #[test]
    fn every_server_instance_rejects_remote_clients() {
        let code = transport_code();
        let (_, servers) = builders(&code);
        assert_eq!(
            code.matches(".reject_remote_clients(true)").count(),
            servers,
            "every pipe instance must reject remote clients (ADR-0037 § Decision)"
        );
    }

    /// The third option ADR-0037 § Decision 2 mandates, and #1511's AC1 with it. Only its PRESENCE
    /// is pinned: the call takes a variable, and "on the first instance only" is carried by that
    /// argument, which no source scan can evaluate.
    #[test]
    fn instance_creation_passes_the_first_pipe_instance_flag() {
        let code = transport_code();
        let (_, servers) = builders(&code);
        assert_eq!(
            code.matches(".first_pipe_instance(").count(),
            servers,
            "the first-instance name reservation must be set per instance creation (AC1)"
        );
    }

    /// Byte mode on BOTH ends. Message mode would frame the wire, and leaving the wire format
    /// alone is the constraint this whole port is built around (ADR-0037 § Decision). It is the
    /// default in tokio AND on the raw Win32 API, which is exactly why the explicit call is worth
    /// pinning: the default is tokio's to change, and the wire compatibility resting on it is ours.
    #[test]
    fn both_ends_pin_byte_mode_rather_than_inheriting_it() {
        let code = transport_code();
        let (clients, servers) = builders(&code);
        assert_eq!(
            code.matches(".pipe_mode(PipeMode::Byte)").count(),
            clients + servers,
            "both the client open and the server instance must pin byte mode"
        );
    }
}
