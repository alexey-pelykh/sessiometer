// Copyright (c) 2026 Oleksii PELYKH
// SPDX-License-Identifier: MIT

//! The control socket: the `0600` Unix-domain server the daemon answers `status` /
//! `manual-swapped` / `roster-reload` on, plus the client-side reload / restore notifies
//! (issues #15, #64, #139, #276; the #195 per-concern decomposition).
//!
//! [`UnixControl`] is the production [`Control`] seam the run loop's idle select drives between
//! polls; [`serve_control`] is its core, testable over an in-memory duplex and bounded in space
//! ([`MAX_CONTROL_LINE_BYTES`]) and time ([`CONTROL_EXCHANGE_TIMEOUT`]); [`control_reply`] is the
//! pure request->(reply, signal) mapping. State-affecting commands are gated on the peer being
//! the same local user ([`super::peer_is_same_user`], issue #64). A served exchange hands the run
//! loop any [`ControlSignal`] to apply where `&mut Daemon` is available. Re-exported under
//! `crate::daemon::*`, so relocating them is source-compatible for cli / capture and the
//! in-module test suite (`mod tests`' `use super::*`).

use serde::{Deserialize, Serialize};
use tokio::net::UnixListener;
use tokio::sync::watch;

use super::*;

/// A side effect a served control connection asks the run loop to apply after the
/// reply is sent. `status` produces none (a pure read); each state-affecting command
/// maps to a variant. Returned by [`Control::serve`] so the mutation lands on the
/// daemon's decision state in the run loop, where `&mut Daemon` is available — `serve`
/// itself only borrows the read-only snapshot.
///
/// Deliberately NOT `Copy`: [`Restored`](ControlSignal::Restored) carries an owned
/// `uuid` payload (issue #275), unlike the two payload-less signals. The run loop
/// consumes the signal by value out of the idle `select!`, so a move (not a copy) is
/// all the handling needs.
#[derive(Debug, Clone, PartialEq, Eq)]
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
    /// An authenticated peer (the `login` verb re-logging in a parked account; in
    /// principle a manual operator) asked to un-quarantine the account with this
    /// `uuid` WITHOUT making it active (issue #275). The run loop applies the existing
    /// [`Daemon::apply_refresh_restore`] primitive — the same one the #106 refresh
    /// sweep drives — which flips `quarantined` off, resets `recovery_successes`, and
    /// emits [`Event::CredentialRestored`], with NO canonical write and NO
    /// active-account change. Unlike the two payload-less signals above it CARRIES the
    /// target `uuid` (the reason this enum is not `Copy`). Idempotent at the primitive:
    /// an unknown or already-non-quarantined `uuid` is a logged no-op. Decoupled from
    /// the sweep (which is starved, #260), so this is the RELIABLE on-demand
    /// un-quarantine path for a re-logged-in parked account.
    Restored(String),
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

    /// Publish `snapshot` to any live `watch` subscribers (issue #165). The run loop calls this
    /// once per tick, so a subscriber gets a fresh WHOLE snapshot on every state change. Default:
    /// a no-op — a control seam without a subscriber channel (every hermetic test seam) simply
    /// drops it, so publishing stays invisible to the existing run-loop tests. Only [`UnixControl`]
    /// overrides it, feeding its latest-snapshot channel.
    fn publish(&self, _snapshot: &StatusSnapshot) {}
}

/// Production control: accept one client at a time on the bound socket and answer
/// from the latest snapshot.
pub(crate) struct UnixControl {
    listener: UnixListener,
    /// The latest-snapshot channel (issue #165): the run loop feeds it each cycle through
    /// [`publish`](UnixControl::publish), and every `watch` subscription
    /// ([`serve`](UnixControl::serve)) streams from a [`subscribe`](watch::Sender::subscribe)d
    /// receiver. A `watch` channel (not `broadcast`) precisely because whole snapshots are
    /// idempotent: a subscriber only ever needs the LATEST, so coalescing an intermediate value a
    /// slow client missed is correct, not lossy — the issue's "whole snapshots, not deltas" rule.
    snapshots: watch::Sender<VersionedStatus>,
}

impl UnixControl {
    pub(crate) fn new(listener: UnixListener) -> Self {
        // Seed the channel with an all-defaults snapshot so a subscriber that connects before the
        // first tick still gets a well-formed frame (empty accounts, `generated_at: 0`) it reads
        // as "starting / stale" rather than nothing; the first tick's `publish` replaces it.
        let (snapshots, _rx) =
            watch::channel(versioned_status_response(&StatusSnapshot::default()));
        Self {
            listener,
            snapshots,
        }
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
                match serve_control(stream, snapshot, peer_authenticated).await {
                    // A one-shot command — hand its signal (if any) to the run loop.
                    Ok(ServeOutcome::OneShot(signal)) => signal,
                    // A `watch` subscription (issue #165): hand the connection to a SPAWNED
                    // streaming task so the run loop's idle select is never held for the
                    // subscription's lifetime — an inline stream would stall every tick on the
                    // single-thread runtime (ADR-0001). The task owns only `Send` data (the
                    // stream + a `watch::Receiver` + a timer), never the `!Send` daemon seams, so
                    // `tokio::spawn` is `Send`-clean; it runs cooperatively on the one thread. A
                    // `watch` never mutates daemon state, so it produces no signal.
                    Ok(ServeOutcome::Watch(stream)) => {
                        let receiver = self.snapshots.subscribe();
                        tokio::spawn(async move {
                            // Best-effort: a disconnected subscriber or any I/O error just ends the
                            // stream (the frames carry nothing secret) — never affects the daemon.
                            let _ = serve_watch(stream, receiver, WATCH_HEARTBEAT).await;
                        });
                        None
                    }
                    Err(_) => None,
                }
            }
            Err(_) => None,
        }
    }

    fn publish(&self, snapshot: &StatusSnapshot) {
        // Store the freshest snapshot and wake every subscriber. `send_replace` is infallible (no
        // error with zero subscribers) and always updates the stored value, so a subscriber that
        // connects later still borrows current state as its initial frame. Cheap: one non-secret
        // projection (issue #15) plus a wake.
        self.snapshots
            .send_replace(versioned_status_response(snapshot));
    }
}

/// The `{"cmd": "..."}` control request. `uuid` is present only for the `restored`
/// command (issue #275) — the payload-less commands (`status` / `manual-swapped` /
/// `roster-reload`) omit it, and serde defaults a missing `Option` field to `None`.
#[derive(Deserialize)]
struct ControlRequest {
    cmd: String,
    uuid: Option<String>,
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
        // The reply is the FROZEN versioned envelope (issue #164): the redacted snapshot payload
        // plus the contract `schema_version` + `generated_at`, so a read-only client binds to a
        // stable, versioned struct. Still a non-secret read, answered for any peer.
        Ok(request) if request.cmd == "status" => (
            serde_json::to_string(&versioned_status_response(snapshot))
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
        // `restored` (issue #275) un-quarantines the named account WITHOUT activating it.
        // State-affecting, so — like `manual-swapped` / `roster-reload` — it is honored ONLY
        // for an authenticated same-user peer; an unauthenticated one gets an error and
        // produces NO signal (a stranger can never un-quarantine an account). Auth is checked
        // FIRST, so a stranger learns nothing about the request's well-formedness. A `restored`
        // that parses but carries no `uuid` has no target to restore, so it is malformed-safe
        // like an unparseable line. The idempotent unknown-/already-restored no-op lives in
        // `apply_refresh_restore` (run-loop side, where `&mut Daemon` is available); this pure
        // reply always acks a well-formed authenticated request and lets the primitive decide.
        Ok(request) if request.cmd == "restored" => {
            if peer_authenticated {
                match request.uuid {
                    Some(uuid) => (
                        r#"{"ok":true}"#.to_owned(),
                        Some(ControlSignal::Restored(uuid)),
                    ),
                    None => (r#"{"error":"malformed request"}"#.to_owned(), None),
                }
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

/// What serving one control connection produced (issue #165). Every command except `watch` is a
/// single request→reply [`OneShot`](ServeOutcome::OneShot) exchange carrying an optional
/// [`ControlSignal`] for the run loop; a `watch` subscription instead hands the still-open
/// connection back as [`Watch`](ServeOutcome::Watch) for the caller to stream on — kept OFF the
/// one-shot reply path so the long-lived stream is never served inline in the run loop's idle
/// select (an inline stream would stall every tick on the single-thread runtime, ADR-0001).
pub(crate) enum ServeOutcome<RW> {
    /// A one-shot command was answered with a single reply line; the optional signal is the
    /// run-loop mutation it asks for (`None` for a pure `status` read or a rejected command).
    OneShot(Option<ControlSignal>),
    /// A `watch` subscription was requested; the connection is handed back for the caller to
    /// stream snapshots + heartbeats on (production spawns [`serve_watch`]).
    Watch(RW),
}

/// Serve one control exchange: read one newline-delimited JSON request and either write one
/// newline-delimited JSON reply (returning any [`ControlSignal`] the request produced) or, for a
/// `watch` subscription (issue #165), hand the connection back for the caller to stream on.
/// Generic over the stream so it is testable over an in-memory duplex without binding a real
/// socket; `peer_authenticated` is the caller's peer-credential verdict (issue #64), gating the
/// state-affecting commands. The receive path is BOUNDED in space (the read is capped at
/// [`MAX_CONTROL_LINE_BYTES`]) and in time (the request read + one-shot reply is wrapped in
/// [`CONTROL_EXCHANGE_TIMEOUT`]); the `watch` stream is unbounded by design and runs OUTSIDE that
/// timeout, in the caller's spawned task.
pub(crate) async fn serve_control<RW>(
    stream: RW,
    snapshot: &StatusSnapshot,
    peer_authenticated: bool,
) -> Result<ServeOutcome<RW>>
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
        let trimmed = line.trim_end();
        // A `watch` subscription (issue #165) hands the connection off to the streaming path
        // rather than writing a one-shot reply: the caller spawns `serve_watch` on it. Parsed with
        // the same `ControlRequest` shape as every other command; `into_inner` drops the (empty — a
        // watch client sends only this one line) read buffer to yield a bare stream to stream on.
        if matches!(
            serde_json::from_str::<ControlRequest>(trimmed),
            Ok(request) if request.cmd == "watch"
        ) {
            return Ok(ServeOutcome::Watch(buffered.into_inner()));
        }
        let (reply, signal) = control_reply(trimmed, snapshot, peer_authenticated);
        buffered.write_all(reply.as_bytes()).await?;
        buffered.write_all(b"\n").await?;
        buffered.flush().await?;
        Ok::<_, Error>(ServeOutcome::OneShot(signal))
    };
    // A peer that stalls mid-line must not hold the exchange open: time-box it and
    // drop on elapse. The reply carries nothing secret, so a dropped exchange is
    // harmless — the caller maps both a timeout and an error to "no signal".
    match tokio::time::timeout(CONTROL_EXCHANGE_TIMEOUT, exchange).await {
        Ok(result) => result,
        Err(_elapsed) => Ok(ServeOutcome::OneShot(None)),
    }
}

/// Test helper: the one-shot signal, panicking on a `watch` outcome — for the one-shot command
/// tests, which never expect a subscription.
#[cfg(test)]
impl<RW> ServeOutcome<RW> {
    pub(crate) fn one_shot(self) -> Option<ControlSignal> {
        match self {
            ServeOutcome::OneShot(signal) => signal,
            ServeOutcome::Watch(_) => {
                panic!("expected a one-shot reply, got a watch subscription")
            }
        }
    }
}

/// How often a `watch` subscription (issue #165) emits a heartbeat frame during SILENCE — a
/// liveness beat so a client can tell a live-but-idle daemon (state simply unchanged) from a
/// dropped connection, and show "disconnected / stale" rather than a frozen view. The timer is
/// reset on every snapshot, so a beat fires only after this long with NO state change; on a local
/// Unix socket a dropped peer also delivers EOF/EPIPE promptly, so this bounds detection of the
/// rarer silently-wedged-daemon case. Low-frequency by intent — a monitoring cadence, not a data
/// stream.
const WATCH_HEARTBEAT: Duration = Duration::from_secs(15);

/// Stream one `watch` subscription (issue #165) until the client disconnects: an initial full
/// snapshot on connect, then a full snapshot on every state change, plus a low-frequency heartbeat
/// ([`WATCH_HEARTBEAT`]) during silence. WHOLE snapshots, never deltas — a client that misses an
/// intermediate value still converges on the latest (a missed delta would silently desync it).
/// Newline-delimited JSON, the same framing `serve_control` speaks; each line is a `type`-tagged
/// frame ([`encode_snapshot_frame`] / [`encode_heartbeat_frame`]). Generic over the stream so it is
/// testable over an in-memory duplex, exactly like `serve_control`.
///
/// Ends (returning `Ok`) when the client goes away — detected by the read half hitting EOF (the
/// read-only client closed its end) or by a write failing (broken pipe) — or when the daemon drops
/// the publisher on shutdown (which errors `changed()`). Best-effort: any I/O error just ends the
/// stream; a dropped subscriber never affects the daemon.
pub(crate) async fn serve_watch<RW>(
    stream: RW,
    mut snapshots: watch::Receiver<VersionedStatus>,
    heartbeat: Duration,
) -> Result<()>
where
    RW: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    use tokio::io::AsyncReadExt;

    // Split the connection so the read half (EOF detection) and the write half (pushes) can be
    // driven in one `select!` without aliasing a single `&mut`.
    let (mut read_half, mut write_half) = tokio::io::split(stream);

    // The initial full snapshot, immediately on connect. `borrow_and_update` marks the current
    // value seen, so the first `changed()` below waits for the NEXT state change rather than
    // re-firing on the value just sent.
    let initial = encode_snapshot_frame(&snapshots.borrow_and_update());
    write_line(&mut write_half, &initial).await?;

    // The heartbeat fires only after `heartbeat` of SILENCE: it is reset on every snapshot, so a
    // steadily-updating daemon sends few (or no) beats and an idle one sends one per interval.
    // `Delay` (not the default `Burst`) so a slow task never catches up with a beat storm.
    let mut beat = tokio::time::interval(heartbeat);
    beat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    beat.reset(); // first beat one interval from now, not immediately

    // A tiny sink for the read side: a read-only `watch` client sends nothing, so this only ever
    // observes EOF (`Ok(0)`); any stray bytes are ignored — the stream is push-only.
    let mut discard = [0u8; 64];

    loop {
        tokio::select! {
            biased;
            // The client closed its end (EOF) or the read errored → the subscription is over.
            read = read_half.read(&mut discard) => match read {
                Ok(0) | Err(_) => return Ok(()),
                Ok(_) => {} // ignore any client input; `watch` is push-only
            },
            // A new snapshot was published → stream it, and reset the heartbeat (it fills silence
            // only). A `changed()` error means the daemon dropped the publisher (shutdown) → end.
            changed = snapshots.changed() => {
                if changed.is_err() {
                    return Ok(());
                }
                let frame = encode_snapshot_frame(&snapshots.borrow_and_update());
                write_line(&mut write_half, &frame).await?;
                beat.reset();
            },
            // Silence for a full interval → a liveness beat carrying the last-known freshness.
            _ = beat.tick() => {
                let frame = encode_heartbeat_frame(snapshots.borrow().generated_at);
                write_line(&mut write_half, &frame).await?;
            },
        }
    }
}

/// Write one newline-delimited frame and flush it — the `watch` stream's counterpart of the
/// one-shot reply write in `serve_control`. A write failure (broken pipe: the client went away)
/// propagates so [`serve_watch`] ends the subscription.
async fn write_line<W>(writer: &mut W, line: &str) -> Result<()>
where
    W: tokio::io::AsyncWrite + Unpin,
{
    use tokio::io::AsyncWriteExt;
    writer.write_all(line.as_bytes()).await?;
    writer.write_all(b"\n").await?;
    writer.flush().await?;
    Ok(())
}

/// The `type` tag on every `watch` stream frame (issue #165): a self-describing discriminator so a
/// client routes each newline-delimited line without positional assumptions. Serialized snake_case
/// (`"snapshot"` / `"heartbeat"`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum WatchFrameKind {
    Snapshot,
    Heartbeat,
}

/// A `watch` SNAPSHOT frame (issue #165): the `type` tag plus the FLATTENED frozen #164 envelope,
/// so the wire line is `{"type":"snapshot","schema_version":…,"generated_at":…,"accounts":…,…}` —
/// the exact `status` payload shape a client already knows, only prefixed with the tag. Borrows the
/// envelope (`&VersionedStatus`) so encoding never clones the snapshot. Struct-level `flatten` with
/// a sibling field is the same pattern [`VersionedStatus`] itself uses (its `status` payload), so
/// both serialize and deserialize cleanly — unlike `flatten` inside a tagged enum.
#[derive(Serialize)]
struct SnapshotFrame<'a> {
    #[serde(rename = "type")]
    kind: WatchFrameKind,
    #[serde(flatten)]
    versioned: &'a VersionedStatus,
}

/// A `watch` HEARTBEAT frame (issue #165): the `type` tag plus the freshness envelope
/// (`schema_version` + `generated_at`) and no payload — a self-describing liveness beat a client
/// can version-gate exactly like a snapshot. `generated_at` carries the last-known snapshot instant
/// (the daemon's #164 stamp), so even a beat conveys how fresh the daemon's data is.
#[derive(Serialize, Deserialize)]
struct HeartbeatFrame {
    #[serde(rename = "type")]
    kind: WatchFrameKind,
    generated_at: i64,
    #[serde(default)]
    schema_version: SchemaVersion,
}

/// Encode a `watch` snapshot frame (issue #165) as one JSON line. Non-secret for the same reason
/// `versioned_status_response` is — the frame adds only a `type` tag around the redacted #164
/// envelope (issue #15). Serializing a finite struct cannot realistically fail; a defensive
/// fallback keeps the stream total rather than dropping a subscriber on an impossible encode error.
pub(crate) fn encode_snapshot_frame(versioned: &VersionedStatus) -> String {
    serde_json::to_string(&SnapshotFrame {
        kind: WatchFrameKind::Snapshot,
        versioned,
    })
    .unwrap_or_else(|_| r#"{"type":"snapshot","error":"encode failed"}"#.to_owned())
}

/// Encode a `watch` heartbeat frame (issue #165) as one JSON line, stamping the current contract
/// version ([`STATUS_SCHEMA_VERSION`]) and the last-known `generated_at`.
pub(crate) fn encode_heartbeat_frame(generated_at: i64) -> String {
    serde_json::to_string(&HeartbeatFrame {
        kind: WatchFrameKind::Heartbeat,
        generated_at,
        schema_version: STATUS_SCHEMA_VERSION,
    })
    .unwrap_or_else(|_| r#"{"type":"heartbeat","error":"encode failed"}"#.to_owned())
}

/// A decoded `watch` stream frame (issue #165), the client-side counterpart of the daemon's
/// encoders — the reference decoder a `watch` client (a future menubar, #168) reuses, and the typed
/// surface the stream tests assert against. Test-scoped for now: the daemon PUSHES in production,
/// but no in-tree client CONSUMES yet, so a non-test build would see it unused.
#[cfg(test)]
#[derive(Debug)]
pub(crate) enum WatchFrame {
    /// A full status snapshot (the frozen #164 envelope).
    Snapshot(VersionedStatus),
    /// A liveness beat carrying the last-known freshness.
    Heartbeat {
        generated_at: i64,
        schema_version: SchemaVersion,
    },
    /// A frame whose `type` this build does not understand (or a missing tag) — ignored by a
    /// forward-compatible client (the additive-minor philosophy of the #164 contract), never a
    /// hard error.
    Unknown,
}

/// Classify + decode one `watch` stream line (issue #165). Probes the `type` tag FIRST — the same
/// probe-then-decode shape the `status` client's #164 major gate uses — then decodes the matching
/// frame. A malformed line is an error; an unknown or missing `type` decodes to
/// [`WatchFrame::Unknown`] so a client skips a future frame kind rather than break.
#[cfg(test)]
pub(crate) fn parse_watch_frame(line: &str) -> Result<WatchFrame> {
    #[derive(Deserialize)]
    struct Probe {
        #[serde(rename = "type", default)]
        kind: Option<String>,
    }
    let probe: Probe =
        serde_json::from_str(line).map_err(|err| Error::Io(std::io::Error::other(err)))?;
    match probe.kind.as_deref() {
        // A snapshot line IS the #164 envelope with an extra `type` key serde ignores.
        Some("snapshot") => {
            let versioned: VersionedStatus =
                serde_json::from_str(line).map_err(|err| Error::Io(std::io::Error::other(err)))?;
            Ok(WatchFrame::Snapshot(versioned))
        }
        Some("heartbeat") => {
            let frame: HeartbeatFrame =
                serde_json::from_str(line).map_err(|err| Error::Io(std::io::Error::other(err)))?;
            Ok(WatchFrame::Heartbeat {
                generated_at: frame.generated_at,
                schema_version: frame.schema_version,
            })
        }
        _ => Ok(WatchFrame::Unknown),
    }
}

/// Upper bound on a client-side control notify exchange — the CLI-verb counterpart of the
/// server's [`CONTROL_EXCHANGE_TIMEOUT`], shared by every client notify (`roster-reload`
/// #139, `restored` #276), just as the server bounds every command with the one
/// `CONTROL_EXCHANGE_TIMEOUT`. Mirrors the `use`-side manual-hold notify (#64): a missing /
/// wedged daemon must never hang the `capture` / `login` / `remove` verb, so the whole
/// connect→send→ack exchange is time-boxed and any failure degrades to a logged best-effort
/// skip.
const CLIENT_NOTIFY_TIMEOUT: Duration = Duration::from_secs(2);

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
/// [`CLIENT_NOTIFY_TIMEOUT`] so a missing / wedged daemon can never hang the
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
    tokio::time::timeout(CLIENT_NOTIFY_TIMEOUT, exchange)
        .await
        .map_err(|_| {
            Error::Io(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "roster-reload notify timed out",
            ))
        })?
}

/// Notify a running daemon to un-quarantine a re-logged-in parked account (issue #276)
/// WITHOUT activating it — the CLI-verb counterpart of the daemon's `restored` control
/// handler ([`control_reply`], issue #275). Sends one newline-delimited
/// `{"cmd":"restored","uuid":"<uuid>"}` request and reads the one-line ack so the daemon has
/// RECEIVED it before returning.
///
/// BEST-EFFORT by contract, exactly like [`notify_roster_reload`] (#139) and the `use`-side
/// manual-hold notify (#64): the on-disk stash + roster write is authoritative (the revive
/// already succeeded), so a notify failure — no daemon running (connect refused / socket
/// absent), a timeout, an I/O error — is for the CALLER to log and ignore, never fatal.
/// Bounded by [`CLIENT_NOTIFY_TIMEOUT`] so a missing / wedged daemon can never hang the
/// `login` verb. Carries the account `uuid` but NO credential and NO write target: the daemon
/// un-quarantines from its own roster state (idempotent — an unknown / already-healthy uuid is
/// a no-op, #275), with no canonical write and no active-account change.
///
/// Unlike the payload-less [`notify_roster_reload`], the `uuid` is a dynamic field, so the
/// request is built with `serde_json` (correctly escaped) rather than a raw byte-literal.
pub(crate) async fn notify_restored(socket: &Path, uuid: &str) -> Result<()> {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt};

    // Serializing a finite, string-keyed object cannot fail (mirrors the `json!` seed in
    // `login::onboarding_seed`), so the encode is `.expect`-ed rather than propagated.
    let request = serde_json::to_vec(&serde_json::json!({ "cmd": "restored", "uuid": uuid }))
        .expect("serializing the restored control request");

    let exchange = async {
        let stream = tokio::net::UnixStream::connect(socket).await?;
        let mut buffered = tokio::io::BufReader::new(stream);
        buffered.write_all(&request).await?;
        buffered.write_all(b"\n").await?;
        buffered.flush().await?;
        // Read the one-line ack so the daemon has processed the request before we
        // return; the content is irrelevant (any failure is non-fatal for the caller).
        let mut line = String::new();
        buffered.read_line(&mut line).await?;
        Ok::<(), Error>(())
    };
    tokio::time::timeout(CLIENT_NOTIFY_TIMEOUT, exchange)
        .await
        .map_err(|_| {
            Error::Io(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "restored notify timed out",
            ))
        })?
}
