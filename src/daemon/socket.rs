// Copyright (c) 2026 Oleksii PELYKH
// SPDX-License-Identifier: MIT

//! The control socket: the `0600` Unix-domain server the daemon answers `status` /
//! `manual-swapped` / `roster-reload` on, plus the client-side reload notify (issues #15, #64,
//! #139; the #195 per-concern decomposition).
//!
//! [`UnixControl`] is the production [`Control`] seam the run loop's idle select drives between
//! polls; [`serve_control`] is its core, testable over an in-memory duplex and bounded in space
//! ([`MAX_CONTROL_LINE_BYTES`]) and time ([`CONTROL_EXCHANGE_TIMEOUT`]); [`control_reply`] is the
//! pure request->(reply, signal) mapping. State-affecting commands are gated on the peer being
//! the same local user ([`super::peer_is_same_user`], issue #64). A served exchange hands the run
//! loop any [`ControlSignal`] to apply where `&mut Daemon` is available. Re-exported under
//! `crate::daemon::*`, so relocating them is source-compatible for cli / capture and the
//! in-module test suite (`mod tests`' `use super::*`).

use serde::Deserialize;
use tokio::net::UnixListener;

use super::*;

/// A side effect a served control connection asks the run loop to apply after the
/// reply is sent. `status` produces none (a pure read); the only variant today is
/// the manual-hold signal (issue #64). Returned by [`Control::serve`] so the
/// mutation lands on the daemon's decision state in the run loop, where `&mut
/// Daemon` is available — `serve` itself only borrows the read-only snapshot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ControlSignal {
    /// A manual `use` swap committed and notified the daemon (issue #64). The run
    /// loop adopts it ([`Daemon::adopt_manual_swap`]): arm the post-swap cooldown
    /// (#10) so the very next poll does not immediately revert the operator's
    /// choice, and re-resolve the active account from the canonical item. A
    /// cooldown-only signal — it carries no credential and no write target, and
    /// never becomes a write command.
    ManualSwapped,
    /// A roster write on disk (`capture` / `login` / `remove`) committed and notified
    /// the daemon (issue #139). The run loop reloads it
    /// ([`Daemon::adopt_roster_reload`]): re-read `config.toml` and reconcile the
    /// in-memory roster (add onboarded/relogged-in accounts, drop removed ones),
    /// preserving per-account health/decision state for accounts that persist. Like
    /// [`ManualSwapped`](ControlSignal::ManualSwapped) it carries no payload — the
    /// authoritative new roster is the on-disk `config.toml`, re-read from scratch — so
    /// a duplicate or out-of-order notification at worst re-reads an unchanged file.
    RosterReloadRequested,
}

/// Control seam: serve control-socket connections. The production impl
/// ([`UnixControl`]) accepts on a `UnixListener`; the run loop's idle select
/// drives it between polls. The test no-op never resolves, so it never wins the
/// select. A served connection may return a [`ControlSignal`] for the run loop to
/// apply (`None` for a pure `status` read).
pub(crate) trait Control {
    /// Serve at most one control connection from `snapshot`, then resolve to any
    /// [`ControlSignal`] the exchange produced (`None` if none).
    async fn serve(&self, snapshot: &StatusSnapshot) -> Option<ControlSignal>;
}

/// Production control: accept one client at a time on the bound socket and answer
/// from the latest snapshot.
pub(crate) struct UnixControl {
    listener: UnixListener,
}

impl UnixControl {
    pub(crate) fn new(listener: UnixListener) -> Self {
        Self { listener }
    }
}

impl Control for UnixControl {
    async fn serve(&self, snapshot: &StatusSnapshot) -> Option<ControlSignal> {
        match self.listener.accept().await {
            Ok((stream, _addr)) => {
                // Authenticate the peer as the SAME local user (issue #64): a
                // state-affecting command (`manual-swapped`) is honored only from
                // our own uid. The socket is already `0600` in a `0700` dir, so
                // this is defense-in-depth — but the manual-hold receive path must
                // be authenticated, never trust-by-reachability. Peer creds are read
                // from the real fd here; `serve_control` takes the verdict as a
                // plain bool so it stays testable over an in-memory duplex.
                let peer_authenticated = peer_is_same_user(&stream);
                // Best-effort: a malformed or disconnected client must never crash
                // the daemon — drop the exchange (the reply carries nothing secret).
                serve_control(stream, snapshot, peer_authenticated)
                    .await
                    .unwrap_or(None)
            }
            Err(_) => None,
        }
    }
}

/// The `{"cmd": "..."}` control request.
#[derive(Deserialize)]
struct ControlRequest {
    cmd: String,
}

/// Build the one-line reply to a control request line, plus any [`ControlSignal`]
/// the run loop must apply afterward. Pure (no I/O, no clock), so the
/// request→(reply, signal) mapping is unit-testable; `peer_authenticated` is
/// passed in (computed from the real fd by the caller) rather than read here, for
/// the same testability reason `in_cooldown` is a parameter elsewhere.
///
/// `status` is a non-secret read, answered for any peer. `manual-swapped` (issue
/// #64) is state-affecting, so it is honored ONLY for an authenticated same-user
/// peer; an unauthenticated one gets an error and produces NO signal (the cooldown
/// is never armed by a stranger).
pub(crate) fn control_reply(
    line: &str,
    snapshot: &StatusSnapshot,
    peer_authenticated: bool,
) -> (String, Option<ControlSignal>) {
    match serde_json::from_str::<ControlRequest>(line) {
        Ok(request) if request.cmd == "status" => (
            serde_json::to_string(&status_response(snapshot))
                .unwrap_or_else(|_| r#"{"error":"encode failed"}"#.to_owned()),
            None,
        ),
        Ok(request) if request.cmd == "manual-swapped" => {
            if peer_authenticated {
                (
                    r#"{"ok":true}"#.to_owned(),
                    Some(ControlSignal::ManualSwapped),
                )
            } else {
                (r#"{"error":"unauthorized"}"#.to_owned(), None)
            }
        }
        // `roster-reload` (issue #139) is state-affecting — it makes the daemon adopt a
        // new on-disk roster — so, like `manual-swapped`, it is honored ONLY for an
        // authenticated same-user peer; an unauthenticated one gets an error and
        // produces NO signal (a stranger can never make the daemon re-read its config).
        Ok(request) if request.cmd == "roster-reload" => {
            if peer_authenticated {
                (
                    r#"{"ok":true}"#.to_owned(),
                    Some(ControlSignal::RosterReloadRequested),
                )
            } else {
                (r#"{"error":"unauthorized"}"#.to_owned(), None)
            }
        }
        Ok(_) => (r#"{"error":"unknown command"}"#.to_owned(), None),
        Err(_) => (r#"{"error":"malformed request"}"#.to_owned(), None),
    }
}

/// Upper bound on a single control-socket request line. A control request is one
/// short JSON command (`{"cmd":"status"}` / `{"cmd":"manual-swapped"}`); capping the
/// read keeps a misbehaving same-uid client from growing the daemon's buffer without
/// bound (issue #64 — the receive path must be BOUNDED).
pub(crate) const MAX_CONTROL_LINE_BYTES: u64 = 8 * 1024;

/// Upper bound on one whole control exchange (read request + write reply). Mirrors
/// the `use`-side `CONTROL_SOCKET_TIMEOUT` so a peer that never completes its line
/// cannot hold the serve arm; the run-loop select also drops this future at the next
/// poll tick, so this is the tighter, dedicated time bound (issue #64).
const CONTROL_EXCHANGE_TIMEOUT: Duration = Duration::from_secs(2);

/// Serve one control exchange: read one newline-delimited JSON request and write
/// one newline-delimited JSON reply, returning any [`ControlSignal`] the request
/// produced. Generic over the stream so it is testable over an in-memory duplex
/// without binding a real socket; `peer_authenticated` is the caller's
/// peer-credential verdict (issue #64), gating the state-affecting commands. The
/// receive path is BOUNDED in space (the read is capped at [`MAX_CONTROL_LINE_BYTES`])
/// and in time (the exchange is wrapped in [`CONTROL_EXCHANGE_TIMEOUT`]).
pub(crate) async fn serve_control<RW>(
    stream: RW,
    snapshot: &StatusSnapshot,
    peer_authenticated: bool,
) -> Result<Option<ControlSignal>>
where
    RW: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt};

    let exchange = async {
        // Cap the request read: a control request is one short line, so a peer that
        // streams more — or never sends a newline — is bounded here (EOF at the
        // limit) instead of growing `line` without limit.
        let mut buffered = tokio::io::BufReader::new(stream);
        let mut line = String::new();
        (&mut buffered)
            .take(MAX_CONTROL_LINE_BYTES)
            .read_line(&mut line)
            .await?;
        let (reply, signal) = control_reply(line.trim_end(), snapshot, peer_authenticated);
        buffered.write_all(reply.as_bytes()).await?;
        buffered.write_all(b"\n").await?;
        buffered.flush().await?;
        Ok::<_, Error>(signal)
    };
    // A peer that stalls mid-line must not hold the exchange open: time-box it and
    // drop on elapse. The reply carries nothing secret, so a dropped exchange is
    // harmless — the caller maps both a timeout and an error to "no signal".
    match tokio::time::timeout(CONTROL_EXCHANGE_TIMEOUT, exchange).await {
        Ok(result) => result,
        Err(_elapsed) => Ok(None),
    }
}

/// Upper bound on the client-side `roster-reload` notify exchange (issue #139) — the
/// CLI-verb counterpart of the server's [`CONTROL_EXCHANGE_TIMEOUT`]. Mirrors the
/// `use`-side manual-hold notify (#64): a missing / wedged daemon must never hang the
/// `capture` / `login` / `remove` verb, so the whole connect→send→ack exchange is
/// time-boxed and any failure degrades to a logged best-effort skip.
const ROSTER_RELOAD_NOTIFY_TIMEOUT: Duration = Duration::from_secs(2);

/// Notify a running daemon that the on-disk roster changed (issue #139), so it
/// re-reads `config.toml` and reconciles its in-memory rotation WITHOUT a restart.
/// The CLI-verb counterpart of the daemon's `roster-reload` control handler
/// ([`control_reply`]); sends one newline-delimited `{"cmd":"roster-reload"}` request
/// and reads the one-line ack so the daemon has RECEIVED it before returning.
///
/// BEST-EFFORT by contract, exactly like the `use` manual-hold notify (#64): the
/// on-disk `config.toml` is authoritative (the write already succeeded), so a notify
/// failure — no daemon running (connect refused / socket absent), a timeout, an I/O
/// error — is for the CALLER to log and ignore, never fatal. Bounded by
/// [`ROSTER_RELOAD_NOTIFY_TIMEOUT`] so a missing / wedged daemon can never hang the
/// verb. Carries NO credential and NO write target — a pure reload signal (the daemon
/// re-reads the authoritative file itself).
pub(crate) async fn notify_roster_reload(socket: &Path) -> Result<()> {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt};

    let exchange = async {
        let stream = tokio::net::UnixStream::connect(socket).await?;
        let mut buffered = tokio::io::BufReader::new(stream);
        buffered.write_all(b"{\"cmd\":\"roster-reload\"}\n").await?;
        buffered.flush().await?;
        // Read the one-line ack so the daemon has processed the request before we
        // return; the content is irrelevant (any failure is non-fatal for the caller).
        let mut line = String::new();
        buffered.read_line(&mut line).await?;
        Ok::<(), Error>(())
    };
    tokio::time::timeout(ROSTER_RELOAD_NOTIFY_TIMEOUT, exchange)
        .await
        .map_err(|_| {
            Error::Io(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "roster-reload notify timed out",
            ))
        })?
}
