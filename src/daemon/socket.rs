// Copyright (c) 2026 Oleksii PELYKH
// SPDX-License-Identifier: MIT

//! The control socket: the `0600` Unix-domain server the daemon answers the local control protocol
//! on, plus the client-side reload / restore notifies (issues #15, #64, #139, #276, #359; the #195
//! per-concern decomposition). The protocol is a newline-delimited `{"cmd":"…"}` request → one
//! newline-delimited JSON reply. The commands:
//!   - `status` (#9/#164) — a non-secret READ, answered for ANY peer (the frozen versioned snapshot).
//!   - `watch` (#165) — a non-secret snapshot STREAM, also un-auth-gated (ADR-0011).
//!   - `stats` (#356) — a non-secret READ answering a bounded per-account daily usage series
//!     (`{"cmd":"stats","period":"<day|week|month|lifetime>"}`; `period` optional → `week`, the
//!     7-day daily-bucket window). Un-auth-gated like `watch`, and answered OFF the run loop in a
//!     SPAWNED task because the store read is blocking `std::fs` (ADR-0001); the reply is byte-parity
//!     with `sessiometer stats --period <p> --json` (the same `StatsWire`). A request/response verb —
//!     OFF the always-on `watch` stream, whose frozen contract is unchanged.
//!   - `manual-swapped` (#64) / `roster-reload` (#139) / `restored` (#275) — state-affecting
//!     fire-and-forget signals, honored ONLY for an authenticated same-user peer (an unauthenticated
//!     one gets `{"error":"unauthorized"}` and produces NO signal).
//!   - `shutdown` (#397) — a state-affecting STOP for an UNMANAGED daemon (the `daemon stop`
//!     control path, for a foreground / detached `sessiometer run`). Like the signals above it is
//!     honored ONLY for an authenticated same-user peer; it drives the daemon's existing graceful
//!     [`Idle::Shutdown`](mod@super::run_loop) exit (an in-flight swap completes first — shutdown is
//!     observed only BETWEEN ticks). A MANAGED launchd agent is stopped with `launchctl bootout`
//!     instead, so its `KeepAlive` does not respawn it.
//!   - `swap` (#167) — a state-affecting command the daemon performs ITSELF (needs `&mut Daemon`),
//!     re-validating the target against its own roster and returning a REDACTED ack from the real
//!     outcome (`{"cmd":"swap","target":"<label|uuid>","force":<bool>}`).
//!   - `capture` (#359) — the daemon-routed capture, mirroring `swap` 1:1
//!     (`{"cmd":"capture","label":"<label>"}`; `label` optional → the daemon derives it from the
//!     account uuid, NEVER the email, #15/#134). The daemon reads the active credential + identity
//!     and stashes them under the swap lock (the #357 `capture_locked` primitive), reconciles the
//!     in-memory roster, and returns a REDACTED ack (label + count, or a bare machine reason). The
//!     client never touches a credential (the panel-originates-no-seam invariant, REQ-MBR-C-005).
//!   - `config-get` (#268) — a non-secret READ projecting the effective config for the settings UI:
//!     the scalar tunables + each roster account's `account_uuid` / `label` / `enabled`, never a
//!     credential (issue #15). Un-auth-gated like `stats`, and answered OFF the run loop in a SPAWNED
//!     task because the `config.toml` read is blocking `std::fs` (ADR-0001).
//!   - `config-set` (#268) — the daemon-routed settings write, mirroring `swap` / `capture`: a
//!     state-affecting command the daemon performs ITSELF (load→validate→save via the tested
//!     `Config::apply_settings`), returning a REDACTED ack whose `effect` tells the UI whether the
//!     change is live (a label, adopted via a roster reconcile) or needs a restart (a tunable). The
//!     editable surface is tunables + existing-account LABELS only — never a credential or a roster
//!     add/remove (structurally: `{"cmd":"config-set","tunables":{…},"labels":{"<uuid>":"<label>"}}`
//!     re-parses under `deny_unknown_fields`, so a forbidden key is a hard `malformed request`).
//!
//! [`UnixControl`] is the production [`Control`] seam the run loop's idle select drives between
//! polls; [`serve_control`] is its core, testable over an in-memory duplex and bounded in space
//! ([`MAX_CONTROL_LINE_BYTES`]) and time ([`CONTROL_EXCHANGE_TIMEOUT`]); [`control_reply`] is the
//! pure request->(reply, signal) mapping. State-affecting commands are gated on the peer being
//! the same local user ([`super::peer_is_same_user`], issue #64). A served exchange hands the run
//! loop any [`ControlSignal`] to apply where `&mut Daemon` is available. Re-exported under
//! `crate::daemon::*`, so relocating them is source-compatible for cli / capture and the
//! in-module test suite (`mod tests`' `use super::*`).

use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::watch;

use super::*;

use crate::config::SetTunables;

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
    /// An authenticated same-user peer asked the daemon to STOP (issue #397) — the `daemon
    /// stop` control path for an UNMANAGED daemon (a foreground / detached `sessiometer run`;
    /// a MANAGED launchd agent is stopped with `launchctl bootout` instead, so its `KeepAlive`
    /// does not respawn it). The run loop breaks its idle to the SAME graceful
    /// [`Idle::Shutdown`](mod@super::run_loop) exit a SIGINT / SIGTERM drives, so an in-flight swap
    /// completes before exit (shutdown is observed only BETWEEN ticks, never mid-swap). Like the
    /// other state-affecting signals it is payload-less and honored ONLY for an authenticated
    /// same-user peer; the `{"ok":true}` ack is flushed to the client BEFORE the loop exits.
    ShutdownRequested,
}

/// What serving one control connection yielded to the run loop. Most commands are fire-and-forget
/// [`Signal`](ControlYield::Signal)s applied AFTER the reply is already sent (`None` for a pure
/// `status` read or a rejected command); a `swap` command (issue #167) instead hands the
/// still-open connection back as [`Swap`](ControlYield::Swap) so the run loop performs the swap
/// where `&mut Daemon` is available and writes the REDACTED ack from the real outcome — an outcome
/// the read-only serve seam cannot know (accepted / rejected-with-reason).
///
/// The stream is the concrete production [`UnixStream`] (the sole real [`Control`] impl,
/// [`UnixControl`], accepts one); the hermetic test seams never yield a [`Swap`](ControlYield::Swap)
/// (they only fire signals), so the concrete type costs the trait no generality it uses.
pub(crate) enum ControlYield {
    /// A fire-and-forget signal (or none) for the run loop to apply — the existing
    /// `manual-swapped` / `roster-reload` / `restored` commands and every non-signal read.
    Signal(Option<ControlSignal>),
    /// A `swap` command (issue #167): the open connection + the parsed request, handed to the run
    /// loop to perform the swap (needs `&mut Daemon`) and write the redacted ack.
    Swap(UnixStream, SwapCommand),
    /// A `capture` command (issue #359): the open connection + the parsed request, handed to the
    /// run loop to perform the capture (needs `&mut Daemon`) and write the redacted ack — the
    /// daemon-routed sibling of `swap`, mirroring it 1:1.
    Capture(UnixStream, CaptureCommand),
    /// A `config-set` command (issue #268): the open connection + the parsed edits, handed to the
    /// run loop to apply them (needs `&mut Daemon` to reconcile a live label change) and write the
    /// redacted ack — the config-editing sibling of `swap` / `capture`. (`config-get` is a spawned
    /// read, like `stats`, so it never yields here — it produces a bare [`Signal(None)`](ControlYield::Signal).)
    ConfigSet(UnixStream, Box<ConfigSetCommand>),
}

/// Control seam: serve control-socket connections. The production impl
/// ([`UnixControl`]) accepts on a `UnixListener`; the run loop's idle select
/// drives it between polls. The test no-op never resolves, so it never wins the
/// select. A served connection yields a [`ControlYield`] for the run loop — a
/// [`Signal`](ControlYield::Signal) to apply (`None` for a pure `status` read) or a
/// [`Swap`](ControlYield::Swap) handoff the run loop performs itself (issue #167).
pub(crate) trait Control {
    /// Serve at most one control connection from `snapshot`, then resolve to the
    /// [`ControlYield`] the exchange produced.
    async fn serve(&self, snapshot: &StatusSnapshot) -> ControlYield;

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
    async fn serve(&self, snapshot: &StatusSnapshot) -> ControlYield {
        match self.listener.accept().await {
            Ok((stream, _addr)) => {
                // Authenticate the peer as the SAME local user (issue #64): a
                // state-affecting command (`manual-swapped`, `swap` #167) is honored
                // only from our own uid. The socket is already `0600` in a `0700`
                // dir, so this is defense-in-depth — but the state-affecting receive
                // path must be authenticated, never trust-by-reachability. Peer creds
                // are read from the real fd here; `serve_control` takes the verdict as
                // a plain bool so it stays testable over an in-memory duplex.
                let peer_authenticated = peer_is_same_user(&stream);
                // Best-effort: a malformed or disconnected client must never crash
                // the daemon — drop the exchange (the reply carries nothing secret).
                match serve_control(stream, snapshot, peer_authenticated).await {
                    // A one-shot command — hand its signal (if any) to the run loop.
                    Ok(ServeOutcome::OneShot(signal)) => ControlYield::Signal(signal),
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
                        ControlYield::Signal(None)
                    }
                    // A `swap` command (issue #167): an authenticated, well-formed request whose
                    // ack must reflect the REAL swap outcome. `serve_control` already answered a
                    // stranger / malformed request inline (a `OneShot(None)` above); here it hands
                    // the OPEN connection back so the run loop performs the swap where
                    // `&mut Daemon` is available and writes the redacted ack itself.
                    Ok(ServeOutcome::Swap(stream, command)) => ControlYield::Swap(stream, command),
                    // A `capture` command (issue #359): an authenticated, well-formed request whose
                    // ack must reflect the REAL capture outcome (captured / refreshed / rejected).
                    // Like `swap`, a stranger was already answered inline (a `OneShot(None)` above);
                    // here the OPEN connection is handed back so the run loop performs the capture
                    // where `&mut Daemon` is available and writes the redacted ack itself.
                    Ok(ServeOutcome::Capture(stream, command)) => {
                        ControlYield::Capture(stream, command)
                    }
                    // A `stats` command (issue #356): like `watch`, a non-secret READ answered in a
                    // SPAWNED task so the blocking store read never stalls the run loop's idle select
                    // (ADR-0001). The task owns only `Send` data (the stream + the parsed period),
                    // never the `!Send` daemon seams, so `tokio::spawn` is `Send`-clean; it reads the
                    // store, computes the series, writes ONE reply line, and closes. Un-auth-gated,
                    // so no peer check — and it never mutates daemon state, so it produces no signal.
                    Ok(ServeOutcome::Stats(stream, request)) => {
                        tokio::spawn(async move {
                            // Best-effort: a disconnected subscriber or any I/O error just ends the
                            // exchange (the reply carries nothing secret) — never affects the daemon.
                            let _ = serve_stats(stream, request).await;
                        });
                        ControlYield::Signal(None)
                    }
                    // A `config-get` command (issue #268): like `stats`, a non-secret READ answered
                    // in a SPAWNED task so the blocking `config.toml` read never stalls the run loop's
                    // idle select (ADR-0001). Un-auth-gated; it never mutates daemon state, so it
                    // produces no signal.
                    Ok(ServeOutcome::ConfigGet(stream)) => {
                        tokio::spawn(async move {
                            // Best-effort: a disconnected client or any I/O error just ends the
                            // exchange (the reply carries nothing secret) — never affects the daemon.
                            let _ = serve_config_get(stream).await;
                        });
                        ControlYield::Signal(None)
                    }
                    // A `config-set` command (issue #268): an authenticated, well-formed request whose
                    // ack must reflect the REAL apply outcome (applied / rejected). Like `swap` /
                    // `capture`, a stranger / malformed request was already answered inline; here the
                    // OPEN connection is handed back so the run loop applies the edits where
                    // `&mut Daemon` is available and writes the redacted ack itself.
                    Ok(ServeOutcome::ConfigSet(stream, command)) => {
                        ControlYield::ConfigSet(stream, command)
                    }
                    Err(_) => ControlYield::Signal(None),
                }
            }
            Err(_) => ControlYield::Signal(None),
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
/// command (issue #275); `target` / `force` only for the `swap` command (issue #167);
/// `label` only for the `capture` command (issue #359) — the payload-less commands
/// (`status` / `manual-swapped` / `roster-reload`) omit them all, and serde defaults a
/// missing field (`Option` → `None`, `bool` → `false`).
#[derive(Deserialize)]
struct ControlRequest {
    cmd: String,
    uuid: Option<String>,
    /// The `swap` target handle (label OR account-uuid), present only for the `swap`
    /// command (issue #167) — an operator-supplied handle, NEVER a credential and
    /// NEVER a usage decision. `#[serde(default)]` so every other command omits it.
    #[serde(default)]
    target: Option<String>,
    /// The `swap` command's POLICY-only force flag (issue #167): bypasses the policy
    /// gates (weekly-exhausted / cooldown / quarantined), NEVER a safety invariant
    /// (the locked-keychain abort, the single-writer swap lock). `#[serde(default)]`
    /// so every other command omits it (and a `swap` without it defaults to `false`).
    #[serde(default)]
    force: bool,
    /// The `capture` command's OPTIONAL operator label (issue #359): names the account
    /// being captured. An OPERATOR-supplied handle, NEVER a credential and never the
    /// email — omitted (`None`) means the daemon auto-derives the label from the account
    /// uuid, NEVER the email (#15/#134). `#[serde(default)]` so every other command omits it.
    #[serde(default)]
    label: Option<String>,
    /// The `stats` command's OPTIONAL period selector (issue #356): the CLI `--period` grammar
    /// (`day|week|month|lifetime`). An omitted period defaults to `week` (the 7-day daily-bucket
    /// window the panel Stats tab reads). `#[serde(default)]` so every other command omits it, and
    /// the value is validated (and mapped to the series) in the spawned task, never here.
    #[serde(default)]
    period: Option<String>,
}

/// A parsed `swap` control request (issue #167): an operator-supplied target handle plus the
/// POLICY-only force flag, and NOTHING else — never a credential, never a viability hint the daemon
/// would trust. Handed from [`serve_control`] to the run loop (via [`ServeOutcome::Swap`] →
/// [`ControlYield::Swap`]) so the swap is performed where `&mut Daemon` is available and the daemon
/// re-validates the target's viability ITSELF (a client-side "greyed out" is UX only, not trusted).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SwapCommand {
    /// The target account's label or account-uuid, resolved by the daemon against its own roster.
    pub(crate) target: String,
    /// Whether to bypass the POLICY gates (weekly-exhausted / cooldown / quarantined). NEVER
    /// bypasses a safety invariant — the daemon's swap engine still aborts on a locked keychain
    /// and still serializes behind the single-writer swap lock.
    pub(crate) force: bool,
}

/// The redacted acknowledgement the daemon returns for a `swap` control command (issue #167) —
/// the ONLY thing a `swap` client learns about the outcome. Non-secret by construction (issue
/// #15): a machine `result` tag plus, for a completed swap, the two non-secret roster LABELS
/// (`from` / `to`) — a swap ack NEVER carries a credential or an email. Internally tagged on
/// `result`, so the three cases stay one self-describing, forward-compatible field a client routes
/// on (mirroring [`NextSwap`]'s `state` tag). Derives `Serialize` (the daemon writes it) and
/// `Deserialize` (the `use` client reads it back).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "result")]
pub(crate) enum SwapAck {
    /// The swap COMMITTED: the active credential was rerouted OFF `from` ONTO `to` (both
    /// non-secret labels). The daemon's own single-writer swap already did the write.
    Accepted { from: String, to: String },
    /// A no-op success: `to` was ALREADY the active account, so nothing was written (the
    /// non-`force` already-active case, mirroring the standalone `use` no-op). Label only.
    AlreadyActive { to: String },
    /// The daemon REFUSED with a redacted machine reason — ZERO writes happened.
    Rejected { reason: SwapRejection },
}

/// Why the daemon refused a `swap` command (issue #167) — a redacted, stable machine code the
/// `use` client maps back to its exit-code taxonomy (never a secret, never free-form). Splits the
/// daemon's own POLICY re-validation verdicts (viability + cooldown, all `force`-bypassable) from
/// the SAFETY / write-time aborts `force` can NEVER bypass (the locked-keychain abort, the
/// single-writer swap lock), plus the target-resolution failures. Serialized kebab-case.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum SwapRejection {
    /// The target handle matched no roster account.
    UnknownTarget,
    /// The target handle matched more than one account (a duplicated label) — the daemon never
    /// guesses (issue #17).
    AmbiguousTarget,
    /// The target is quarantined (a dead credential, issue #42) — refused WITHOUT `force`.
    Quarantined,
    /// The target's weekly window is exhausted (issue #11/#37) — refused WITHOUT `force`.
    WeeklyExhausted,
    /// A post-swap cooldown is active (issue #10) — refused WITHOUT `force`.
    Cooldown,
    /// No active account to swap AWAY from (or its canonical credential is gone): the daemon
    /// cannot run a normal re-stash swap. Recovery (adopt-target, issue #212) is the standalone
    /// `use --force` path, decoupled from this channel per the issue.
    NoActiveAccount,
    /// The keychain is LOCKED — a SAFETY abort the swap engine makes even under `force` (locked ≠
    /// gone; retry when unlocked). ZERO writes.
    KeychainLocked,
    /// The single-writer swap lock (issue #64) stayed held the whole bounded wait — fail-closed,
    /// ZERO writes. `force` never bypasses the lock.
    SwapLockBusy,
    /// The swap engine aborted for another reason (a wrong-identity re-stash guard #211, an I/O
    /// error). ZERO writes.
    Failed,
}

/// A parsed `capture` control request (issue #359): the operator's OPTIONAL label, and NOTHING
/// else — never a credential. Handed from [`serve_control`] to the run loop (via
/// [`ServeOutcome::Capture`] → [`ControlYield::Capture`]) so the daemon performs the WHOLE capture
/// itself — reading the active credential + identity and stashing them under the single-writer swap
/// lock (the #357 [`capture_locked`](crate::capture::capture_locked) primitive) — where `&mut Daemon`
/// is available. The client never touches a credential (the panel-originates-no-seam invariant,
/// REQ-MBR-C-005): it names only the account, and even that is optional — an omitted label
/// auto-derives from the account uuid, NEVER the email (#15/#134).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CaptureCommand {
    /// The operator's label for the account, or `None` to auto-derive it from the account uuid
    /// (never the email — #15/#134).
    pub(crate) label: Option<String>,
}

/// A parsed `config-set` control request (issue #268): the tunable + label edits the settings UI
/// submits, and NOTHING else — never a credential, never a roster add/remove. Handed from
/// [`serve_control`] to the run loop (via [`ServeOutcome::ConfigSet`] → [`ControlYield::ConfigSet`])
/// so the daemon applies them where `&mut Daemon` is available: load→validate→save through the
/// tested [`Config::apply_settings`](crate::config::Config::apply_settings), then adopt a label
/// change live (a tunable change is reload-by-restart). The safety boundary is STRUCTURAL —
/// [`SetTunables`] and the uuid-keyed label map cannot express a credential or a roster structure change.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ConfigSetCommand {
    /// The scalar `[tunables]` edits (each `None` = unedited); the allow-list is the type itself.
    pub(crate) tunables: SetTunables,
    /// `account_uuid` → new label, applied only to EXISTING accounts (an unmatched uuid rejects).
    pub(crate) labels: BTreeMap<String, String>,
}

/// The STRICT wire schema for a `config-set` request (issue #268): re-parsed from the raw line AFTER
/// the generic [`ControlRequest`] identified the command, with `#[serde(deny_unknown_fields)]` so a
/// forbidden top-level key (a credential, a roster field, a mistyped section) is a hard parse error
/// → `{"error":"malformed request"}`, never a silent no-op. The `tunables` sub-object is itself a
/// deny-unknown allow-list ([`SetTunables`]); `labels` maps `account_uuid` → new label. Both default
/// to empty, so a request may edit only tunables, only labels, or both.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ConfigSetRequest {
    /// Present and equal to `"config-set"` (already matched on the generic parse); declared so
    /// `deny_unknown_fields` accepts the line. Not read again after the re-parse.
    #[allow(dead_code)]
    cmd: String,
    #[serde(default)]
    tunables: SetTunables,
    #[serde(default)]
    labels: BTreeMap<String, String>,
}

/// The redacted acknowledgement the daemon returns for a `config-set` control command (issue #268) —
/// the ONLY thing a settings client learns about the outcome. Non-secret by construction (issue #15):
/// a machine `result` tag plus, on success, an `effect` telling the UI whether the change is live (a
/// label, adopted immediately), needs a restart (a tunable — the daemon derives its strategy fields
/// once at construction), or was a no-op. Internally tagged on `result`, mirroring [`SwapAck`].
/// Derives `Serialize` (the daemon writes it) + `Deserialize` (the client / tests read it back).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "result")]
pub(crate) enum ConfigSetAck {
    /// The edits VALIDATED and were persisted (or were a no-op). `effect` says what the operator
    /// must do for them to take effect.
    Applied { effect: ConfigSetEffect },
    /// The daemon REFUSED — ZERO writes. `detail` carries the non-secret message for the `invalid`
    /// reason (the field-named range / cross-field error) and for `config-unreadable` (the baseline
    /// TOML parse error, issue #628); absent for the reasons that carry no such message.
    Rejected {
        reason: ConfigSetRejection,
        #[serde(skip_serializing_if = "Option::is_none", default)]
        detail: Option<String>,
    },
}

/// What a successful `config-set` (issue #268) requires for its edits to take effect — the `effect`
/// the UI renders (a "restart the daemon" hint vs nothing). Serialized snake_case. A batch that
/// changes BOTH a tunable and a label reports `restart_required` (the operative action), even though
/// the label is also adopted live.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ConfigSetEffect {
    /// Only label(s) changed — adopted LIVE (the daemon reconciled its roster in-process); no restart.
    Live,
    /// A tunable changed — persisted, effective on the NEXT daemon start (no hot-reload). Restart hint.
    RestartRequired,
    /// The submitted values equalled the current ones — nothing was written.
    Unchanged,
}

/// Why the daemon refused a `config-set` (issue #268) — a redacted, stable machine code (never a
/// secret, never free-form). Serialized kebab-case. Mirrors [`SwapRejection`]'s stable-code contract.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum ConfigSetRejection {
    /// A tunable was out of range, or a cross-field rule failed (e.g. `exhausted_poll_secs < poll_secs`,
    /// `target_max_session_usage > session_ceiling`). The ack's `detail` names the offending field.
    Invalid,
    /// A label edit named an `account_uuid` matching no roster account (a stale client — the account
    /// was `remove`d since its `config-get`).
    UnknownAccount,
    /// `config.toml` does not exist — nothing to edit (capture the first account via the CLI first).
    NoConfig,
    /// `config.toml` exists but could not be read / parsed — refused rather than overwrite a file the
    /// daemon cannot understand.
    ConfigUnreadable,
    /// The validated config could not be persisted (an atomic-write failure) — the OLD file is intact.
    SaveFailed,
    /// The daemon has no wired config path (a hermetic default) — config-set is unavailable.
    Unavailable,
}

/// A parsed `stats` control request (issue #356): the operator's OPTIONAL period selector, and
/// NOTHING else — never a credential, never a write. Handed from [`serve_control`] to a SPAWNED
/// task (via [`ServeOutcome::Stats`] → [`serve_stats`]) so the bounded per-account daily series is
/// computed OFF the run loop — the store read is blocking `std::fs`, and ADR-0001's single runtime
/// thread must never block a tick (the same reason `watch` spawns, taken one step further). A
/// non-secret READ (usage fractions + already-redacted roster labels, issue #15), so — unlike
/// `swap` / `capture` — it is UN-auth-gated, exactly like `status` / `watch`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct StatsRequest {
    /// The CLI `--period` grammar (`day|week|month|lifetime`), or `None` to default to `week`
    /// (the 7-day daily-bucket window). Validated — and mapped to the series — by
    /// [`crate::stats::socket_stats_reply`] in the spawned task, never here; an unknown value
    /// yields a redacted `{"error":"invalid period"}` reply.
    pub(crate) period: Option<String>,
}

/// The redacted acknowledgement the daemon returns for a `capture` control command (issue #359) —
/// the ONLY thing a `capture` client learns about the outcome. Non-secret by construction (issue
/// #15): a machine `result` tag plus, for a landed capture, the non-secret roster LABEL and the
/// running account COUNT — a capture ack NEVER carries a credential, an email, or an oauth blob.
/// Internally tagged on `result`, mirroring [`SwapAck`], so the three cases stay one
/// self-describing, forward-compatible field a client routes on. Derives `Serialize` (the daemon
/// writes it) and `Deserialize` (a client reads it back).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "result")]
pub(crate) enum CaptureAck {
    /// A NEW account was captured into the roster: its LABEL and the running account COUNT (issue
    /// #35 — no fixed "of N" denominator). The daemon stashed both credential halves under the lock.
    Captured { label: String, count: usize },
    /// An already-rostered active account was REFRESHED in place (the issue-#359 idempotent-refresh
    /// AC): its stash re-pointed to the current token and its roster row updated — NEVER a duplicate
    /// row. Label + running count, exactly like [`Captured`](CaptureAck::Captured).
    Refreshed { label: String, count: usize },
    /// The daemon REFUSED with a redacted machine reason — ZERO roster writes.
    Rejected { reason: CaptureRejection },
}

/// Why the daemon refused a `capture` command (issue #359) — a redacted, stable machine code (never
/// a secret, never free-form), the capture counterpart of [`SwapRejection`]. Serialized kebab-case.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum CaptureRejection {
    /// No active account to capture: not logged in to Claude Code (absent / unreadable
    /// `~/.claude.json` identity), or the canonical credential is gone. Nothing was stashed.
    NoActiveAccount,
    /// The keychain is LOCKED — a SAFETY abort the capture makes when reading the active credential
    /// (locked ≠ gone; retry when unlocked). ZERO writes.
    KeychainLocked,
    /// The single-writer swap lock (issue #64) stayed held the whole bounded wait — fail-closed,
    /// ZERO work (no read, no stash, no roster write). The #357 primitive acquires the lock BEFORE
    /// the identity read, so a contended refusal is a true no-op.
    SwapLockBusy,
    /// The capture aborted for another reason (an I/O error, or the post-stash roster save failed).
    /// Stash-before-roster ordering (#6) means a save failure leaves an inert ORPHAN stash, never a
    /// partial or duplicate roster row.
    Failed,
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
        // `shutdown` (issue #397) STOPS an unmanaged daemon — the `daemon stop` control path for a
        // foreground / detached `sessiometer run`. State-affecting (it ends the process), so — like
        // `manual-swapped` / `roster-reload` / `restored` — it is honored ONLY for an authenticated
        // same-user peer; an unauthenticated one gets an error and produces NO signal (a stranger
        // can never stop the daemon). Fail-closed on the auth verdict, identical to the sibling
        // signals. The `{"ok":true}` ack is written by `serve_control` BEFORE this signal reaches
        // the run loop, so the client learns the stop was accepted before the daemon exits.
        Ok(request) if request.cmd == "shutdown" => {
            if peer_authenticated {
                (
                    r#"{"ok":true}"#.to_owned(),
                    Some(ControlSignal::ShutdownRequested),
                )
            } else {
                (r#"{"error":"unauthorized"}"#.to_owned(), None)
            }
        }
        Ok(_) => (r#"{"error":"unknown command"}"#.to_owned(), None),
        // A line that is not a well-formed `ControlRequest` (issue #628): thread serde's own message
        // into `detail` (secret-free — see `error_with_detail`) rather than discarding it, so a client
        // can tell WHY the line was rejected instead of reading a bare "malformed request".
        Err(err) => (
            error_with_detail("malformed request", &err.to_string()),
            None,
        ),
    }
}

/// Build a redacted `{"error":<reason>,"detail":<detail>}` control-reply envelope for a DECODE-time
/// failure (issue #628), threading serde's own message into the `detail` a client reads instead of
/// discarding it. The `detail` is SECRET-FREE — not because serde never echoes a value (a serde_json
/// type-mismatch DOES reflect the offending scalar, and a TOML error echoes the offending source
/// line) but because no SECRET can reach an echoed position on this socket: credentials live in the
/// OS keychain and are structurally unrepresentable here (issue #15) — a control line carries only
/// non-secret command params, a `config-set` payload only non-secret tunables + labels, and
/// `config.toml` holds no secrets — so an echoed value is at worst the client's own non-secret input
/// reflected back over its own same-uid connection. Built with `serde_json` (not `format!`) so a
/// multi-line TOML `detail` is escaped INTO the JSON string — the reply stays one newline-delimited
/// frame — and a `detail` carrying quotes stays well-formed. The added `detail` is purely additive —
/// a client probing the `error` key is unaffected — and `error` is emitted FIRST (declaration order),
/// so the reply's leading bytes are unchanged for a prefix / substring matcher too. Falls back to the
/// bare `{"error":<reason>}` if the (two-string, in-practice-infallible) encode ever fails.
fn error_with_detail(reason: &'static str, detail: &str) -> String {
    #[derive(Serialize)]
    struct ErrorWithDetail<'a> {
        error: &'a str,
        detail: &'a str,
    }
    serde_json::to_string(&ErrorWithDetail {
        error: reason,
        detail,
    })
    .unwrap_or_else(|_| format!(r#"{{"error":"{reason}"}}"#))
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
    /// run-loop mutation it asks for (`None` for a pure `status` read or a rejected command —
    /// including a `swap` already answered inline as unauthorized / malformed, issue #167).
    OneShot(Option<ControlSignal>),
    /// A `watch` subscription was requested; the connection is handed back for the caller to
    /// stream snapshots + heartbeats on (production spawns [`serve_watch`]).
    Watch(RW),
    /// An authenticated, well-formed `swap` command was requested (issue #167); the connection is
    /// handed back — NO reply written yet — for the caller (the run loop) to perform the swap and
    /// write the redacted ack from the real outcome.
    Swap(RW, SwapCommand),
    /// An authenticated `capture` command was requested (issue #359); the connection is handed back
    /// — NO reply written yet — for the caller (the run loop) to perform the capture (via the #357
    /// `capture_locked` primitive) and write the redacted ack from the real outcome. Unlike `swap`,
    /// a `capture` needs no `target`, so an authenticated request is ALWAYS well-formed (the label
    /// is optional); the only inline rejection is the unauthorized peer.
    Capture(RW, CaptureCommand),
    /// A `stats` command was requested (issue #356); the connection is handed back — NO reply
    /// written yet — for the caller to answer in a SPAWNED task ([`serve_stats`]), like `watch`
    /// (issue #165), so the blocking store read runs OFF the run loop (ADR-0001). UN-auth-gated: a
    /// non-secret read, so an authenticated peer is NOT required, and the request is ALWAYS
    /// well-formed (the period is optional and validated in the task).
    Stats(RW, StatsRequest),
    /// A `config-get` command (issue #268); the connection is handed back — NO reply written yet —
    /// for the caller to answer in a SPAWNED task ([`serve_config_get`]), like `stats`, so the
    /// blocking `config.toml` read runs OFF the run loop (ADR-0001). UN-auth-gated (a non-secret
    /// read) and param-less, so ALWAYS well-formed.
    ConfigGet(RW),
    /// An authenticated, well-formed `config-set` command (issue #268); the connection is handed
    /// back — NO reply written yet — for the caller (the run loop) to apply the edits (needs
    /// `&mut Daemon` to reconcile a live label change) via [`Config::apply_settings`](crate::config::Config::apply_settings)
    /// and write the redacted ack from the real outcome. An unauthenticated peer or an unparseable /
    /// forbidden-key request is answered inline (a `OneShot(None)`).
    ConfigSet(RW, Box<ConfigSetCommand>),
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
        // `watch` (issue #165), `swap` (issue #167), and `capture` (issue #359) hand the connection
        // OFF rather than write a one-shot reply here; every other command falls through to the pure
        // `control_reply`. Parse once and branch. `into_inner` drops the (empty — these clients send
        // only this one line) read buffer to yield a bare stream to hand back.
        match serde_json::from_str::<ControlRequest>(trimmed) {
            // A `watch` subscription (issue #165): a non-secret read stream (like `status`), so —
            // unlike `swap` below — it is NOT auth-gated; hand the connection to the streaming path.
            Ok(request) if request.cmd == "watch" => {
                return Ok(ServeOutcome::Watch(buffered.into_inner()));
            }
            // A `stats` request (issue #356): a non-secret READ (a bounded per-account usage series
            // + redacted roster labels, like `status` / `watch`), so — unlike `swap` / `capture`
            // below — it is NOT auth-gated. Hand the connection to the spawned `serve_stats` task so
            // the blocking store read + aggregation runs OFF the run loop (ADR-0001), exactly like
            // `watch`. The period is optional (defaults to `week` in the task) and validated there,
            // so an authenticated peer is never required and there is no malformed-inline case.
            Ok(request) if request.cmd == "stats" => {
                return Ok(ServeOutcome::Stats(
                    buffered.into_inner(),
                    StatsRequest {
                        period: request.period,
                    },
                ));
            }
            // A `swap` command (issue #167): STATE-AFFECTING, so authenticate FIRST (like
            // `manual-swapped`). An unauthenticated peer gets `{"error":"unauthorized"}` and NO
            // stream handoff — the swap never reaches the run loop, and a stranger learns nothing
            // past the rejection. A `swap` with no `target` is malformed-safe, like an unparseable
            // line. An authenticated, well-formed request hands the OPEN connection back so the run
            // loop performs the swap (needs `&mut Daemon`) and writes the redacted ack from the
            // REAL outcome — an outcome this pure serve cannot know.
            Ok(request) if request.cmd == "swap" => {
                if !peer_authenticated {
                    write_line(&mut buffered, r#"{"error":"unauthorized"}"#).await?;
                    return Ok(ServeOutcome::OneShot(None));
                }
                return match request.target {
                    Some(target) => Ok(ServeOutcome::Swap(
                        buffered.into_inner(),
                        SwapCommand {
                            target,
                            force: request.force,
                        },
                    )),
                    None => {
                        write_line(&mut buffered, r#"{"error":"malformed request"}"#).await?;
                        Ok(ServeOutcome::OneShot(None))
                    }
                };
            }
            // A `capture` command (issue #359): STATE-AFFECTING, so authenticate FIRST (exactly like
            // `swap` / `manual-swapped`). An unauthenticated peer gets `{"error":"unauthorized"}` and
            // NO stream handoff — the capture never reaches the run loop, and a stranger learns
            // nothing past the rejection AND causes ZERO work (no credential is ever read). Unlike
            // `swap`, a `capture` needs no `target`: the label is OPTIONAL (an omitted one
            // auto-derives from the account uuid, never the email — #15/#134), so an authenticated
            // request is ALWAYS well-formed and hands the OPEN connection back so the run loop
            // performs the capture (needs `&mut Daemon`) and writes the redacted ack from the REAL
            // outcome — an outcome this pure serve cannot know.
            Ok(request) if request.cmd == "capture" => {
                if !peer_authenticated {
                    write_line(&mut buffered, r#"{"error":"unauthorized"}"#).await?;
                    return Ok(ServeOutcome::OneShot(None));
                }
                return Ok(ServeOutcome::Capture(
                    buffered.into_inner(),
                    CaptureCommand {
                        label: request.label,
                    },
                ));
            }
            // A `config-get` command (issue #268): a non-secret READ of the effective config
            // (tunables + redacted roster labels, like `status` / `stats`), so — unlike `config-set`
            // below — it is NOT auth-gated. Hand the connection to the spawned `serve_config_get`
            // task so the blocking `config.toml` read runs OFF the run loop (ADR-0001), exactly like
            // `stats`. It carries no params, so an authenticated peer is never required and there is
            // no malformed-inline case.
            Ok(request) if request.cmd == "config-get" => {
                return Ok(ServeOutcome::ConfigGet(buffered.into_inner()));
            }
            // A `config-set` command (issue #268): STATE-AFFECTING (it writes `config.toml`), so
            // authenticate FIRST (like `swap` / `capture`). An unauthenticated peer gets
            // `{"error":"unauthorized"}` and NO handoff — the edit never reaches the run loop, and a
            // stranger learns nothing past the rejection. Then RE-PARSE the line STRICTLY
            // (`deny_unknown_fields`) so a forbidden top-level key (a credential, a roster field) is a
            // loud `malformed request`, never a silent no-op; the `tunables` sub-object is itself a
            // deny-unknown allow-list. A well-formed authenticated request hands the OPEN connection
            // back so the run loop applies the edits (needs `&mut Daemon`) and writes the redacted ack
            // from the REAL outcome — an outcome this pure serve cannot know.
            Ok(request) if request.cmd == "config-set" => {
                if !peer_authenticated {
                    write_line(&mut buffered, r#"{"error":"unauthorized"}"#).await?;
                    return Ok(ServeOutcome::OneShot(None));
                }
                return match serde_json::from_str::<ConfigSetRequest>(trimmed) {
                    Ok(set_request) => Ok(ServeOutcome::ConfigSet(
                        buffered.into_inner(),
                        Box::new(ConfigSetCommand {
                            tunables: set_request.tunables,
                            labels: set_request.labels,
                        }),
                    )),
                    // A forbidden top-level key, or a stale/renamed tunable from a version-skewed
                    // client (issue #628 — e.g. a pre-#606 menubar sending `session_trigger`, which
                    // `SetTunables`' `deny_unknown_fields` now rejects): thread serde's key-naming
                    // message into `detail` so the client can distinguish a version-skew edit from a
                    // genuinely malformed line, instead of a content-free `malformed request`. Still
                    // ZERO writes — the edit never reaches the run loop.
                    Err(err) => {
                        write_line(
                            &mut buffered,
                            &error_with_detail("malformed request", &err.to_string()),
                        )
                        .await?;
                        Ok(ServeOutcome::OneShot(None))
                    }
                };
            }
            _ => {}
        }
        let (reply, signal) = control_reply(trimmed, snapshot, peer_authenticated);
        // Delivering the ack is BEST-EFFORT: the request is already read, authenticated, and its
        // effect decided. A peer that hung up first (a client that gave up waiting, or was
        // interrupted) makes this write fail with `EPIPE` — and propagating that would discard
        // `signal` at [`UnixControl::serve`]'s `Err(_) => Signal(None)` arm, silently cancelling an
        // action the operator authenticated. A `shutdown` the daemon accepted must still happen.
        let ack = async {
            buffered.write_all(reply.as_bytes()).await?;
            buffered.write_all(b"\n").await?;
            buffered.flush().await
        }
        .await;
        let _ = ack;
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
            ServeOutcome::Swap(..) => {
                panic!("expected a one-shot reply, got a swap handoff")
            }
            ServeOutcome::Capture(..) => {
                panic!("expected a one-shot reply, got a capture handoff")
            }
            ServeOutcome::Stats(..) => {
                panic!("expected a one-shot reply, got a stats handoff")
            }
            ServeOutcome::ConfigGet(_) => {
                panic!("expected a one-shot reply, got a config-get handoff")
            }
            ServeOutcome::ConfigSet(..) => {
                panic!("expected a one-shot reply, got a config-set handoff")
            }
        }
    }

    /// The handed-back stream + parsed command of a [`Swap`](ServeOutcome::Swap) outcome, panicking
    /// on any other — for the issue-#167 swap-handoff tests, which always expect a swap.
    pub(crate) fn swap(self) -> (RW, SwapCommand) {
        match self {
            ServeOutcome::Swap(stream, command) => (stream, command),
            ServeOutcome::OneShot(_) => panic!("expected a swap handoff, got a one-shot reply"),
            ServeOutcome::Watch(_) => panic!("expected a swap handoff, got a watch subscription"),
            ServeOutcome::Capture(..) => panic!("expected a swap handoff, got a capture handoff"),
            ServeOutcome::Stats(..) => panic!("expected a swap handoff, got a stats handoff"),
            ServeOutcome::ConfigGet(_) => {
                panic!("expected a swap handoff, got a config-get handoff")
            }
            ServeOutcome::ConfigSet(..) => {
                panic!("expected a swap handoff, got a config-set handoff")
            }
        }
    }

    /// The handed-back stream + parsed command of a [`Capture`](ServeOutcome::Capture) outcome,
    /// panicking on any other — for the issue-#359 capture-handoff tests, which always expect a
    /// capture.
    pub(crate) fn capture(self) -> (RW, CaptureCommand) {
        match self {
            ServeOutcome::Capture(stream, command) => (stream, command),
            ServeOutcome::OneShot(_) => panic!("expected a capture handoff, got a one-shot reply"),
            ServeOutcome::Watch(_) => {
                panic!("expected a capture handoff, got a watch subscription")
            }
            ServeOutcome::Swap(..) => panic!("expected a capture handoff, got a swap handoff"),
            ServeOutcome::Stats(..) => panic!("expected a capture handoff, got a stats handoff"),
            ServeOutcome::ConfigGet(_) => {
                panic!("expected a capture handoff, got a config-get handoff")
            }
            ServeOutcome::ConfigSet(..) => {
                panic!("expected a capture handoff, got a config-set handoff")
            }
        }
    }

    /// The handed-back stream + parsed request of a [`Stats`](ServeOutcome::Stats) outcome, panicking
    /// on any other — for the issue-#356 stats-handoff tests, which always expect a stats handoff.
    pub(crate) fn stats(self) -> (RW, StatsRequest) {
        match self {
            ServeOutcome::Stats(stream, request) => (stream, request),
            ServeOutcome::OneShot(_) => panic!("expected a stats handoff, got a one-shot reply"),
            ServeOutcome::Watch(_) => panic!("expected a stats handoff, got a watch subscription"),
            ServeOutcome::Swap(..) => panic!("expected a stats handoff, got a swap handoff"),
            ServeOutcome::Capture(..) => panic!("expected a stats handoff, got a capture handoff"),
            ServeOutcome::ConfigGet(_) => {
                panic!("expected a stats handoff, got a config-get handoff")
            }
            ServeOutcome::ConfigSet(..) => {
                panic!("expected a stats handoff, got a config-set handoff")
            }
        }
    }

    /// The handed-back stream of a [`ConfigGet`](ServeOutcome::ConfigGet) outcome, panicking on any
    /// other — for the issue-#268 config-get-handoff tests, which always expect a config-get.
    pub(crate) fn config_get(self) -> RW {
        match self {
            ServeOutcome::ConfigGet(stream) => stream,
            ServeOutcome::OneShot(_) => {
                panic!("expected a config-get handoff, got a one-shot reply")
            }
            ServeOutcome::Watch(_) => {
                panic!("expected a config-get handoff, got a watch subscription")
            }
            ServeOutcome::Swap(..) => panic!("expected a config-get handoff, got a swap handoff"),
            ServeOutcome::Capture(..) => {
                panic!("expected a config-get handoff, got a capture handoff")
            }
            ServeOutcome::Stats(..) => panic!("expected a config-get handoff, got a stats handoff"),
            ServeOutcome::ConfigSet(..) => {
                panic!("expected a config-get handoff, got a config-set handoff")
            }
        }
    }

    /// The handed-back stream + parsed command of a [`ConfigSet`](ServeOutcome::ConfigSet) outcome,
    /// panicking on any other — for the issue-#268 config-set-handoff tests, which always expect a
    /// config-set.
    pub(crate) fn config_set(self) -> (RW, ConfigSetCommand) {
        match self {
            ServeOutcome::ConfigSet(stream, command) => (stream, *command),
            ServeOutcome::OneShot(_) => {
                panic!("expected a config-set handoff, got a one-shot reply")
            }
            ServeOutcome::Watch(_) => {
                panic!("expected a config-set handoff, got a watch subscription")
            }
            ServeOutcome::Swap(..) => panic!("expected a config-set handoff, got a swap handoff"),
            ServeOutcome::Capture(..) => {
                panic!("expected a config-set handoff, got a capture handoff")
            }
            ServeOutcome::Stats(..) => panic!("expected a config-set handoff, got a stats handoff"),
            ServeOutcome::ConfigGet(_) => {
                panic!("expected a config-set handoff, got a config-get handoff")
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

/// Answer one `stats` control request (issue #356) in a SPAWNED task: compute the bounded
/// per-account daily series and write it as one newline-delimited JSON reply, then close. Spawned
/// (like `watch`, [`UnixControl::serve`]) precisely so the store read never stalls the run loop's
/// idle select — and the read itself is blocking `std::fs` inside
/// [`crate::stats::socket_stats_reply`], so it runs in [`tokio::task::spawn_blocking`], OFF the
/// single runtime thread (ADR-0001, one step past `watch`, which only awaits a channel). The reply
/// is byte-parity with `sessiometer stats --period <period> --json` — the SAME `StatsWire` over the
/// SAME on-disk store (issue #356) — serialized COMPACT for the newline-delimited frame. Non-secret
/// by construction: usage fractions + already-redacted roster labels (issue #15), never a
/// credential. Best-effort + time-boxed ([`SWAP_ACK_WRITE_TIMEOUT`]): a disconnected / wedged client
/// just drops the reply. Generic over the stream so it is testable over an in-memory duplex, exactly
/// like [`serve_watch`].
pub(crate) async fn serve_stats<RW>(mut stream: RW, request: StatsRequest) -> Result<()>
where
    RW: tokio::io::AsyncWrite + Unpin,
{
    // The store read + aggregation is blocking I/O + CPU; move it OFF the runtime thread so a large
    // samples file never blocks a tick (ADR-0001 — the reason `watch` spawns, here taken one step
    // further because this arm does synchronous file I/O, not just an await). `spawn_blocking` runs
    // on the blocking pool even on the current-thread runtime; a join error (the blocking closure
    // panicked) degrades to the same non-secret envelope any other failure uses.
    let reply = tokio::task::spawn_blocking(move || {
        crate::stats::socket_stats_reply(request.period.as_deref())
    })
    .await
    .unwrap_or_else(|_| r#"{"error":"stats unavailable"}"#.to_owned());
    // The reply carries nothing secret, so a disconnected client just drops it; time-box the write
    // so a wedged client can never park the spawned task indefinitely (mirrors the swap/capture ack
    // bound, even though — being spawned — this write is already off the run loop's critical path).
    let _ = tokio::time::timeout(SWAP_ACK_WRITE_TIMEOUT, write_line(&mut stream, &reply)).await;
    Ok(())
}

/// Upper bound on writing one one-shot control reply — shared by the `swap` ack (issue #167), the
/// `capture` ack (issue #359), and the `stats` series reply (issue #356). Each reply carries nothing
/// secret, so a disconnected / wedged client just drops it; time-box the write so a stalled client
/// can never park its writer. The swap / capture acks are written INLINE in the run loop's post-idle,
/// so this bound is what stops a wedged client from stalling the loop; the `stats` reply is written
/// from a SPAWNED task (already off the run loop's critical path), where the same bound instead just
/// keeps that task from parking indefinitely. Mirrors [`CONTROL_EXCHANGE_TIMEOUT`], the serve-side
/// one-shot bound.
pub(crate) const SWAP_ACK_WRITE_TIMEOUT: Duration = Duration::from_secs(2);

/// Write a redacted [`SwapAck`] as one newline-delimited JSON line to a `swap` client (issue
/// #167) — the run loop's counterpart of `serve_control`'s one-shot reply, written AFTER the swap
/// is performed (the ack must reflect the real outcome). Non-secret by construction: the ack
/// carries only a `result` tag and roster labels (issue #15). Takes the stream BY VALUE so it is
/// closed on return; a write failure (the client went away) propagates so the caller drops it
/// best-effort. Serializing a finite enum cannot realistically fail; a defensive fallback keeps
/// the write total rather than dropping a completed swap's ack on an impossible encode error.
pub(crate) async fn write_swap_ack<W>(mut stream: W, ack: &SwapAck) -> Result<()>
where
    W: tokio::io::AsyncWrite + Unpin,
{
    let line = serde_json::to_string(ack)
        .unwrap_or_else(|_| r#"{"result":"rejected","reason":"failed"}"#.to_owned());
    write_line(&mut stream, &line).await
}

/// Write a redacted [`CaptureAck`] as one newline-delimited JSON line to a `capture` client (issue
/// #359) — the run loop's counterpart of `serve_control`'s one-shot reply, written AFTER the
/// capture is performed (the ack must reflect the real outcome). The exact sibling of
/// [`write_swap_ack`], sharing its [`SWAP_ACK_WRITE_TIMEOUT`] bound in the run loop. Non-secret by
/// construction: the ack carries only a `result` tag plus a roster label + count (issue #15). Takes
/// the stream BY VALUE so it is closed on return; a write failure (the client went away) propagates
/// so the caller drops it best-effort. Serializing a finite enum cannot realistically fail; a
/// defensive fallback keeps the write total rather than dropping a completed capture's ack on an
/// impossible encode error.
pub(crate) async fn write_capture_ack<W>(mut stream: W, ack: &CaptureAck) -> Result<()>
where
    W: tokio::io::AsyncWrite + Unpin,
{
    let line = serde_json::to_string(ack)
        .unwrap_or_else(|_| r#"{"result":"rejected","reason":"failed"}"#.to_owned());
    write_line(&mut stream, &line).await
}

/// Answer one `config-get` control request (issue #268) in a SPAWNED task: project the effective
/// `config.toml` to the non-secret [`ConfigView`](crate::config::ConfigView) and write it as one
/// newline-delimited JSON reply, then close. Spawned (like `stats`, [`UnixControl::serve`]) so the
/// blocking `config.toml` read never stalls the run loop's idle select — the read is blocking
/// `std::fs` inside [`config_get_reply`], so it runs in [`tokio::task::spawn_blocking`], OFF the
/// single runtime thread (ADR-0001). Non-secret by construction: the view carries only tunables +
/// already-redacted roster labels (issue #15), never a credential. Best-effort + time-boxed
/// ([`SWAP_ACK_WRITE_TIMEOUT`]): a disconnected / wedged client just drops the reply. Generic over the
/// stream so it is testable over an in-memory duplex, exactly like [`serve_stats`].
pub(crate) async fn serve_config_get<RW>(mut stream: RW) -> Result<()>
where
    RW: tokio::io::AsyncWrite + Unpin,
{
    // The config read is blocking `std::fs`; move it OFF the runtime thread (ADR-0001), exactly like
    // `serve_stats`. A join error (the closure panicked) degrades to the same non-secret envelope.
    let reply = tokio::task::spawn_blocking(|| match crate::paths::config_file() {
        Ok(path) => config_get_reply(&path),
        Err(_) => r#"{"error":"config unreadable"}"#.to_owned(),
    })
    .await
    .unwrap_or_else(|_| r#"{"error":"config unreadable"}"#.to_owned());
    // The reply carries nothing secret, so a disconnected client just drops it; time-box the write so
    // a wedged client can never park the spawned task indefinitely (mirrors `serve_stats`).
    let _ = tokio::time::timeout(SWAP_ACK_WRITE_TIMEOUT, write_line(&mut stream, &reply)).await;
    Ok(())
}

/// Build the `config-get` reply (issue #268): read `path`, project the effective config to the
/// non-secret [`ConfigView`](crate::config::ConfigView), and serialize it COMPACT for the newline
/// frame. A missing file → `{"error":"no config"}` (capture the first account via the CLI first); an
/// unreadable / invalid one → `{"error":"config unreadable"}`. Blocking `std::fs`, so the caller runs
/// it in `spawn_blocking`. Non-secret: the view carries only tunables + redacted labels (issue #15).
pub(crate) fn config_get_reply(path: &Path) -> String {
    match std::fs::read_to_string(path) {
        Ok(text) => match crate::config::Config::view_from_text(&text) {
            Ok(view) => serde_json::to_string(&view)
                .unwrap_or_else(|_| r#"{"error":"encode failed"}"#.to_owned()),
            // The file exists but does not parse (issue #628): thread the parse error — a secret-free
            // TOML message echoing the offending source line, and naming the key when the failure has
            // one (the config holds no secrets, issue #15) — into `detail` rather than discarding it,
            // so the client learns WHERE the file is broken, not a bare envelope.
            Err(err) => error_with_detail("config unreadable", &err.to_string()),
        },
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            r#"{"error":"no config"}"#.to_owned()
        }
        Err(_) => r#"{"error":"config unreadable"}"#.to_owned(),
    }
}

/// Write a redacted [`ConfigSetAck`] as one newline-delimited JSON line to a `config-set` client
/// (issue #268) — the run loop's counterpart of `serve_control`'s one-shot reply, written AFTER the
/// edits are applied (the ack must reflect the real outcome). The exact sibling of
/// [`write_swap_ack`] / [`write_capture_ack`], sharing [`SWAP_ACK_WRITE_TIMEOUT`] in the run loop.
/// Non-secret: a `result` tag + effect / reason (issue #15). Takes the stream BY VALUE so it closes
/// on return; a write failure propagates so the caller drops it best-effort.
pub(crate) async fn write_config_set_ack<W>(mut stream: W, ack: &ConfigSetAck) -> Result<()>
where
    W: tokio::io::AsyncWrite + Unpin,
{
    let line = serde_json::to_string(ack)
        .unwrap_or_else(|_| r#"{"result":"rejected","reason":"save-failed"}"#.to_owned());
    write_line(&mut stream, &line).await
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

/// Extra head-room the `swap` command exchange allows ON TOP of the swap lock's own bounded wait
/// (issue #167): enough for the daemon's post-lock `security` subprocesses plus a tick in flight.
const SWAP_COMMAND_SLACK: Duration = Duration::from_secs(5);

/// Upper bound on the whole `swap` command exchange from the `use` client (issue #167). Longer than
/// the fire-and-forget [`CLIENT_NOTIFY_TIMEOUT`] because the ack arrives only AFTER the daemon
/// performs the swap — which may wait the full single-writer swap-lock budget
/// ([`crate::swap::SWAP_LOCK_MAX_WAIT`]) and then run several `security` subprocesses. DERIVED from
/// that budget + [`SWAP_COMMAND_SLACK`] (rather than a bare literal) so the two can never silently
/// drift apart if the lock's max-wait is retuned. A wedged daemon can therefore never hang `use`
/// past this bound; a timeout AFTER connecting surfaces as an error rather than a silent standalone
/// fallback (never a double write, see [`request_swap`]).
const SWAP_COMMAND_TIMEOUT: Duration =
    crate::swap::SWAP_LOCK_MAX_WAIT.saturating_add(SWAP_COMMAND_SLACK);

/// Send a `swap` control command to a running daemon (issue #167) and read its redacted ack — the
/// CLI-verb counterpart of the daemon's `swap` handler ([`ServeOutcome::Swap`] → the run loop's
/// swap-apply). `use` calls this to route a swap THROUGH the daemon when one is up, so there is a
/// SINGLE writer and a single place for the lock, write-ordering, and redaction. Carries ONLY the
/// operator's target handle + the POLICY force flag — never a credential, never a viability
/// decision (the daemon re-validates the target itself).
///
/// Three outcomes, distinguished so `use` routes correctly:
///   - `Ok(Some(ack))` — a daemon answered; `use` reports its redacted verdict (the daemon already
///     did any write). This is the load-bearing UNIFY case.
///   - `Ok(None)` — NO daemon is reachable (connect refused / socket absent), so `use` falls back
///     to the standalone write path (the daemon-down fallback). Reachability is decided by the
///     CONNECT alone, so a fallback happens ONLY when nothing was sent — never after a write the
///     daemon may already have performed.
///   - `Err(..)` — a daemon was reached but the exchange failed (a mid-exchange I/O error, a
///     malformed ack, or a timeout after connecting). Surfaced, NOT retried standalone: the daemon
///     may have performed the write, so a standalone retry could DOUBLE-write. Bounded by
///     [`SWAP_COMMAND_TIMEOUT`] so a wedged daemon can never hang `use`.
pub(crate) async fn request_swap(
    socket: &Path,
    target: &str,
    force: bool,
) -> Result<Option<SwapAck>> {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt};

    // Serializing a finite, string-keyed object cannot fail (mirrors `notify_restored`), so the
    // encode is `.expect`-ed rather than propagated.
    let request = serde_json::to_vec(&serde_json::json!({
        "cmd": "swap",
        "target": target,
        "force": force,
    }))
    .expect("serializing the swap control request");

    let exchange = async {
        // Connect FIRST: a refused connect / absent socket is the "no daemon" signal (→ `Ok(None)`,
        // fall back to standalone), distinct from a mid-exchange failure (→ `Err`, do NOT fall
        // back — the daemon may already have written). This split is what keeps `use` from ever
        // double-writing when the daemon is up.
        let stream = match tokio::net::UnixStream::connect(socket).await {
            Ok(stream) => stream,
            Err(_) => return Ok::<Option<SwapAck>, Error>(None),
        };
        let mut buffered = tokio::io::BufReader::new(stream);
        buffered.write_all(&request).await?;
        buffered.write_all(b"\n").await?;
        buffered.flush().await?;
        // Read the one-line redacted ack, then decode it into the shared wire type.
        let mut line = String::new();
        buffered.read_line(&mut line).await?;
        let ack: SwapAck = serde_json::from_str(line.trim_end())
            .map_err(|err| Error::Io(std::io::Error::other(err)))?;
        Ok(Some(ack))
    };
    tokio::time::timeout(SWAP_COMMAND_TIMEOUT, exchange)
        .await
        .map_err(|_| {
            Error::Io(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "swap command timed out",
            ))
        })?
}
