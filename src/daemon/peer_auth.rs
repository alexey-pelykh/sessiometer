// Copyright (c) 2026 Oleksii PELYKH
// SPDX-License-Identifier: MIT

//! Control-channel peer authentication (issue #64).
//!
//! Splits the same-user gate the control server ([`super::UnixControl`]) applies to a
//! state-affecting command into three testable pieces: the raw peer-identity read
//! ([`peer_euid`] on Unix, [`peer_user_sid`] on Windows), the pure same-user decision
//! ([`is_same_user`]), and the composed stream-level check ([`peer_is_same_user`]).
//! FAIL CLOSED throughout — an unreadable identity is never an identity, so it can never
//! match ours (issue #196). Extracted from `daemon` per the #195 per-concern decomposition;
//! re-exported under `crate::daemon::*`.
//!
//! The identity read is the one PER-TARGET piece, and the arms are not one call under three
//! names (issues #963, #976). macOS/BSD has `getpeereid(3)`, which writes the peer's effective
//! uid and gid through two out-parameters; glibc has no such symbol, and Linux's equivalent is
//! the `SO_PEERCRED` socket option, which yields a whole `struct ucred` (pid, uid, gid) in one
//! read. Windows has neither: the peer of a named pipe is identified by IMPERSONATING it and
//! reading the user SID off the resulting thread token (ADR-0037 § Decision 3). Every arm
//! narrows to `Option<Id>` for its own `Id`, so [`is_same_user`] and every caller above it stay
//! target-neutral.
//!
//! # What the Windows identity PROVES, relative to `getpeereid` (issue #976 AC1)
//!
//! AC1 asks for this comparison in writing, because a weaker guarantee is acceptable only if it
//! is recorded as such. It is weaker on one axis and stronger on another, and neither difference
//! reaches the gate's verdict.
//!
//! **Stronger — the identity itself.** A SID names an account uniquely within its domain and is
//! not reused. A uid is a machine-local integer that a different machine, or the same machine
//! after an account is deleted and recreated, can reuse for somebody else. Nothing in this gate
//! crosses a machine boundary, so the practical difference is small; the direction is not in
//! doubt (ADR-0037 § Consequences → Positive).
//!
//! **Weaker — the peer has a say.** `getpeereid` and `SO_PEERCRED` read a uid the KERNEL captured
//! at `connect(2)` time. It is a property of the CONNECTION, and the peer cannot influence what
//! is reported. A named-pipe client, by contrast, chooses the impersonation level on its own open:
//! a client that opens with `SECURITY_ANONYMOUS` hands the server an anonymous token, whose user
//! SID is not ours. Under the fail-closed comparison below that can only make a client DENY
//! ITSELF — there is no level at which a peer can make itself look like a DIFFERENT account — so
//! it is an asymmetry rather than an escalation path (ADR-0037 § Consequences → Negative).
//!
//! **Unchanged — the TOCTOU property.** Like the Unix uid, the impersonated SID describes the
//! connection's own security context, not a live process looked up after the fact. That is the
//! whole reason `GetNamedPipeClientProcessId` is a DIAGNOSTIC here and never the authentication
//! primitive: a pid is reusable and can name a different process by the time it is compared
//! (ADR-0037 § Decision 3). This module does not read one.
//!
//! **New — the read mutates thread state.** `getpeereid` is a pure read. Impersonation replaces
//! the calling thread's token for the duration of the window, which is a hazard the Unix arms do
//! not have; [`peer_user_sid`] is where it is contained, and its own docs carry the rules.
//!
//! **Net.** The gate's question — "is this peer the same local user?" — is answered at least as
//! precisely on Windows as on Unix, and every failure mode of the Windows read denies. What the
//! Windows arm does NOT reproduce is the Unix REACHABILITY layer around the gate: `0600` on the
//! socket in a `0700` directory means a foreign user cannot reach the endpoint at all, whereas
//! the pipe namespace has no directory (ADR-0037 § Decision 2). The owner-only descriptor #1513
//! landed replaces the mode; nothing replaces the directory, which is why the CLIENT must check
//! the SERVER's owner SID (`crate::control_transport`) — a direction `getpeereid` never had to
//! answer.

// No peer-identity mechanism below is portable beyond the three targets ADR-0029 and ADR-0037
// declare supported, and a fourth target silently taking the wrong arm would be a SECURITY defect
// rather than a build one. Fail at compile time, naming the port that is missing, instead of
// leaving a bare "cannot find function `peer_is_same_user`" at the call site.
#[cfg(not(any(target_os = "macos", target_os = "linux", windows)))]
compile_error!(
    "peer authentication needs a per-target peer-identity read; only macOS (`getpeereid`), \
     Linux (`SO_PEERCRED`) and Windows (`ImpersonateNamedPipeClient`) are ported — see \
     ADR-0029 and ADR-0037"
);

/// The peer's effective uid read from the connected Unix-domain socket `fd` via
/// `getpeereid(2)`, or `None` when the credential cannot be read (the syscall errors —
/// a not-connected socket, a non-socket fd, a bad fd). Split out from the same-user
/// decision ([`is_same_user`]) so the fail-closed error branch is testable without a
/// real failing peer (issue #196). Returning `None` on error IS the fail-closed
/// primitive: an unreadable credential is never a uid, so it can never match ours.
#[cfg(target_os = "macos")]
pub(crate) fn peer_euid(fd: std::os::unix::io::RawFd) -> Option<libc::uid_t> {
    let mut euid: libc::uid_t = 0;
    let mut egid: libc::gid_t = 0;
    // SAFETY: `getpeereid` is a syscall the kernel validates `fd` for itself — a bad,
    // non-socket, or not-connected fd returns `rc != 0` (mapped to `None` below),
    // never UB — and it writes the two out-pointers (stack locals here) ONLY on
    // success (`rc == 0`). No preconditions on `fd`.
    let rc = unsafe { libc::getpeereid(fd, &mut euid, &mut egid) };
    (rc == 0).then_some(euid)
}

/// The peer's effective uid read from the connected Unix-domain socket `fd` via the
/// `SO_PEERCRED` socket option, or `None` when the credential cannot be read (the syscall
/// errors — a not-connected socket, a non-socket fd, a bad fd). Split out from the
/// same-user decision ([`is_same_user`]) so the fail-closed error branch is testable
/// without a real failing peer (issue #196). Returning `None` on error IS the fail-closed
/// primitive: an unreadable credential is never a uid, so it can never match ours.
///
/// The uid `SO_PEERCRED` reports is the peer's EFFECTIVE uid, captured by the kernel at
/// `connect(2)` time — the same quantity `getpeereid` returns on the macOS arm, so the
/// decision [`is_same_user`] makes is identical on both. The extra `pid` and `gid` the
/// `ucred` carries are deliberately discarded: this gate asks one question, and reading
/// only what it asks keeps the two arms' surfaces the same.
#[cfg(target_os = "linux")]
pub(crate) fn peer_euid(fd: std::os::unix::io::RawFd) -> Option<libc::uid_t> {
    let mut cred: libc::ucred = unsafe { std::mem::zeroed() };
    let mut len = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
    // SAFETY: `getsockopt` is a syscall the kernel validates `fd` for itself — a bad,
    // non-socket, or not-connected fd returns `rc != 0` (mapped to `None` below), never
    // UB. The option buffer is a live local of exactly the layout `SO_PEERCRED` writes
    // (`struct ucred`), `len` is its true size, and the kernel writes it ONLY on success
    // (`rc == 0`). The `zeroed()` initialisation is sound for `ucred`: it is a plain
    // `#[repr(C)]` struct of three integers, for which the all-zero bit pattern is valid.
    let rc = unsafe {
        libc::getsockopt(
            fd,
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            (&mut cred as *mut libc::ucred).cast::<libc::c_void>(),
            &mut len,
        )
    };
    (rc == 0).then_some(cred.uid)
}

/// The connected named-pipe peer's USER SID in the SDDL string form
/// `ConvertSidToStringSidW` renders (`S-1-5-21-…`), or `None` when it cannot be resolved —
/// the Windows analogue of [`peer_euid`], and ADR-0037 § Decision 3 (issue #976 AC1).
///
/// `ImpersonateNamedPipeClient` → `OpenThreadToken` → `GetTokenInformation(TokenUser)` →
/// `ConvertSidToStringSidW` → `RevertToSelf`. FAIL CLOSED at every stage, mirroring
/// [`peer_euid`]'s `None`-on-error contract: a value no caller can mistake for an identity.
///
/// Three rules govern the impersonation window, and each is structural here rather than a
/// convention a later edit could quietly drop.
///
/// **NO `.await` between the impersonation and the revert** (ADR-0037 § Consequences →
/// Negative). Impersonation mutates the CALLING THREAD's token, so a suspension point inside
/// the window lets tokio poll other tasks on that thread while it carries the client's token —
/// and on the daemon's `current_thread` runtime (ADR-0001) `UnixControl::serve` spawns exactly
/// such tasks. The type system does not catch it: `trait Control::serve` declares no `Send`
/// bound, so a `!Send` guard would compile. What enforces it here is that this is an ordinary
/// synchronous `fn` — it cannot contain an `.await` at all — and
/// `the_impersonation_window_contains_no_suspension_point` asserts that on every target, so the
/// rule cannot be lost by turning this into an `async fn`.
///
/// **The revert runs on EVERY exit**, via [`Impersonation`]'s `Drop` rather than a call at the
/// end of each arm. An early return or an unwinding panic would otherwise leave the thread
/// carrying the client's token, which is the failure this window exists to bound.
///
/// **`OpenThreadToken` is called with `openasself = TRUE`.** The access check for opening the
/// token is then made against the PROCESS's own security context rather than against the client
/// token this thread is currently wearing — which is the point, since the impersonated client
/// need not have the right to open its own token.
///
/// **Unmeasured, and it is the whole of this arm.** No CI job compiles this target (#978 is that
/// job), and the #972 spike measured the same call sequence only on a runner where every account
/// was one privileged account — so the ADR records a cross-user result, a foreign-account open
/// and a standard-user run as residuals this issue owns and cannot discharge here (see the
/// `#[cfg(windows)]` tests in `crate::daemon::snapshot_build`, which are committed and run the
/// moment #978 turns on).
#[cfg(windows)]
pub(crate) fn peer_user_sid(stream: &crate::control_transport::ControlStream) -> Option<String> {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
    use windows_sys::Win32::Security::TOKEN_QUERY;
    use windows_sys::Win32::System::Pipes::ImpersonateNamedPipeClient;
    use windows_sys::Win32::System::Threading::{GetCurrentThread, OpenThreadToken};

    let pipe = stream.as_raw_handle() as HANDLE;
    // SAFETY: `pipe` is the raw handle of a live, connected `NamedPipeServer` owned by the
    // caller's `stream`, which outlives this call. The API validates the handle itself and
    // returns FALSE for anything it will not impersonate.
    if unsafe { ImpersonateNamedPipeClient(pipe) } == 0 {
        return None;
    }
    // Armed IMMEDIATELY after the successful impersonation and before anything that can fail,
    // so every path below reverts.
    let _revert = Impersonation;

    let mut token: HANDLE = std::ptr::null_mut();
    // SAFETY: `GetCurrentThread` returns a pseudo-handle needing no cleanup; `token` is a live
    // local the kernel writes only on success. `openasself` is TRUE (`1`), so the open is
    // checked against this PROCESS's context rather than the client token now on this thread.
    let opened = unsafe { OpenThreadToken(GetCurrentThread(), TOKEN_QUERY, 1, &mut token) };
    if opened == 0 {
        return None;
    }
    let sid = crate::control_transport::token_user_sid(token).ok();
    // SAFETY: `token` is the handle `OpenThreadToken` just wrote and has not been closed.
    unsafe { CloseHandle(token) };
    sid
}

/// Reverts this thread to its own token when it leaves scope — the impersonation window's
/// closing half, as RAII so that no arm of [`peer_user_sid`] has to remember it.
///
/// A `RevertToSelf` that FAILS is the one condition this type does not simply absorb. Microsoft's
/// own guidance is explicit that an application which fails to revert continues running in the
/// client's context and should shut down; on the daemon's single-threaded runtime that context
/// would additionally be inherited by every co-scheduled task. Absorbing it would leave the
/// daemon running as whoever last connected.
///
/// It is not aborted on blindly, though, because the failure is ambiguous: `RevertToSelf` also
/// returns FALSE for a thread that carries no impersonation token at all — the state an
/// impersonation that silently no-opped would leave, which is benign and must not kill the
/// daemon. So the token is READ BACK first, with exactly the probe the #972 spike used as its
/// negative control (`OpenThreadToken` failing `ERROR_NO_TOKEN` means no token is present). Only
/// a thread that still carries one aborts.
///
/// Believed unreachable in either branch; it is written because the cost of being wrong is a
/// privilege confusion rather than a crash, and because nothing in this repo can execute this
/// path until #978 exists.
#[cfg(windows)]
struct Impersonation;

#[cfg(windows)]
impl Drop for Impersonation {
    fn drop(&mut self) {
        use windows_sys::Win32::Foundation::{CloseHandle, GetLastError, ERROR_NO_TOKEN, HANDLE};
        use windows_sys::Win32::Security::{RevertToSelf, TOKEN_QUERY};
        use windows_sys::Win32::System::Threading::{GetCurrentThread, OpenThreadToken};

        // SAFETY: no preconditions; reverts this thread to its own token.
        if unsafe { RevertToSelf() } != 0 {
            return;
        }
        let mut token: HANDLE = std::ptr::null_mut();
        // SAFETY: as in `peer_user_sid` — a pseudo-handle needing no cleanup, and a live local
        // the kernel writes only on success.
        let opened = unsafe { OpenThreadToken(GetCurrentThread(), TOKEN_QUERY, 1, &mut token) };
        if opened == 0 {
            // SAFETY: reads the last-error slot set by the call above.
            if unsafe { GetLastError() } == ERROR_NO_TOKEN {
                // Nothing to revert: the impersonation left no token on this thread, so the
                // FALSE above says the window was already closed rather than that it is stuck.
                return;
            }
            // The token could not be read either. Not evidence that the thread is clean, so it
            // falls through to the abort below rather than being read as one.
        } else {
            // SAFETY: `token` is the handle `OpenThreadToken` just wrote and has not been closed.
            unsafe { CloseHandle(token) };
        }
        // This thread still carries — or may still carry — the client's token, and there is no
        // second mechanism to remove it. Continuing would run the daemon, and every task
        // co-scheduled on this thread, as the peer.
        std::process::abort();
    }
}

/// The pure peer-auth decision (issue #64): whether a peer bearing identity `peer` — or
/// `None` when its identity could not be read — is the SAME local user as `ours`. Split from
/// the platform read ([`peer_euid`], [`peer_user_sid`]) so every branch is testable without a
/// real foreign peer or root: same-user, a foreign identity, and the unreadable branch
/// (issue #196). FAIL CLOSED — `None` is never the same user, so a failed read denies.
/// Inverting this comparison flips BOTH the foreign-identity and the error branch from deny to
/// allow, which the peer-auth tests catch.
///
/// GENERIC over the identity type since issue #976, because the three arms do not narrow to one:
/// Unix yields a `uid_t`, Windows a SID string. The decision itself is the same equality either
/// way, which is what keeps this function — the one piece a security review has to read — target-
/// neutral and executable on every target rather than split into two cfg'd copies that could
/// drift apart. On Unix it resolves to exactly the `uid_t` comparison it always was.
///
/// Both SID operands come from `ConvertSidToStringSidW`, which renders one canonical spelling per
/// SID, so byte equality is the right comparison and no case folding belongs here — folding would
/// only widen what counts as a match.
pub(crate) fn is_same_user<Id: PartialEq>(peer: Option<Id>, ours: Id) -> bool {
    peer == Some(ours)
}

/// Whether the peer connected on `stream` is the same local user as this process
/// (issue #64). Reads the peer's identity via the platform arm ([`peer_euid`] over
/// `getpeereid(2)` on macOS or `SO_PEERCRED` on Linux; [`peer_user_sid`] over
/// `ImpersonateNamedPipeClient` on Windows — see the module docs) and compares it to our own
/// via [`is_same_user`]. Any failure to read the identity is treated as NOT authenticated —
/// fail closed. Used to gate the state-affecting `manual-swapped` / `roster-reload` commands;
/// the non-secret `status` read is not gated.
///
/// Takes the target-neutral `ControlStream`, which on Unix IS `tokio::net::UnixStream` and on
/// Windows is a connected `NamedPipeServer`, so the call site in `super::UnixControl::serve`
/// needs no `cfg` of its own.
#[cfg(unix)]
pub(crate) fn peer_is_same_user(stream: &crate::control_transport::ControlStream) -> bool {
    use std::os::unix::io::AsRawFd;
    // SAFETY: `getuid` cannot fail and has no preconditions.
    is_same_user(peer_euid(stream.as_raw_fd()), unsafe { libc::getuid() })
}

/// Whether the peer connected on `stream` is the same local user as this process — the Windows
/// arm (issue #976 AC1). See the `#[cfg(unix)]` sibling for the contract, and the module docs for
/// what this identity proves relative to `getpeereid`.
///
/// Our own side is `control_transport::our_user_sid()`, the same process-token read #1513 already
/// uses as the trustee of every pipe instance's owner-only DACL — so the identity the daemon
/// GRANTS access to and the identity it COMPARES a peer against cannot drift apart. A failure to
/// read our OWN identity denies too: it is the same fail-closed direction, and a daemon that
/// cannot say who it is has no business authenticating anyone.
#[cfg(windows)]
pub(crate) fn peer_is_same_user(stream: &crate::control_transport::ControlStream) -> bool {
    match crate::control_transport::our_user_sid() {
        Ok(ours) => is_same_user(peer_user_sid(stream), ours),
        Err(_) => false,
    }
}
