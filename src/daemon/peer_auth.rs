// Copyright (c) 2026 Oleksii PELYKH
// SPDX-License-Identifier: MIT

//! Unix-domain-socket peer authentication for the control socket (issue #64).
//!
//! Splits the same-user gate the control server ([`super::UnixControl`]) applies to a
//! state-affecting command into three testable pieces: the raw `getpeereid(2)` read
//! ([`peer_euid`]), the pure same-user decision ([`is_same_user`]), and the composed
//! stream-level check ([`peer_is_same_user`]). FAIL CLOSED throughout — an unreadable
//! credential is never a uid, so it can never match ours (issue #196). Extracted from
//! `daemon` per the #195 per-concern decomposition; re-exported under `crate::daemon::*`.

/// The peer's effective uid read from the connected Unix-domain socket `fd` via
/// `getpeereid(2)`, or `None` when the credential cannot be read (the syscall errors —
/// a not-connected socket, a non-socket fd, a bad fd). Split out from the same-user
/// decision ([`is_same_user`]) so the fail-closed error branch is testable without a
/// real failing peer (issue #196). Returning `None` on error IS the fail-closed
/// primitive: an unreadable credential is never a uid, so it can never match ours.
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

/// The pure peer-auth decision (issue #64): whether a peer bearing effective uid
/// `peer_euid` — or `None` when its credential could not be read — is the SAME local
/// user as `our_uid`. Split from the syscall ([`peer_euid`]) so every branch is testable
/// without a real foreign-uid peer or root: same-user, a foreign uid, and the
/// unreadable-credential branch (issue #196). FAIL CLOSED — `None` is never the same
/// user, so a `getpeereid` error denies. Inverting this comparison flips BOTH the
/// foreign-uid and the error branch from deny to allow, which the peer-auth tests catch.
pub(crate) fn is_same_user(peer_euid: Option<libc::uid_t>, our_uid: libc::uid_t) -> bool {
    peer_euid == Some(our_uid)
}

/// Whether the peer connected on `stream` is the same local user as this process
/// (issue #64). Reads the peer's effective uid via [`peer_euid`] (`getpeereid(2)`, the
/// portable BSD/macOS peer-credential call for a Unix-domain socket) and compares it to
/// our own `getuid()` via [`is_same_user`]. Any failure to read the credential is
/// treated as NOT authenticated — fail closed. Used to gate the state-affecting
/// `manual-swapped` / `roster-reload` commands; the non-secret `status` read is not
/// gated.
pub(crate) fn peer_is_same_user(stream: &tokio::net::UnixStream) -> bool {
    use std::os::unix::io::AsRawFd;
    // SAFETY: `getuid` cannot fail and has no preconditions.
    is_same_user(peer_euid(stream.as_raw_fd()), unsafe { libc::getuid() })
}
