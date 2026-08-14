// Copyright (c) 2026 Oleksii PELYKH
// SPDX-License-Identifier: MIT

//! `sessiometer use <account>` — operator-driven manual account selection.
//!
//! Switches the active account to a NAMED one on demand, reusing the existing
//! out-of-band swap engine (#6) unchanged. `<account>` resolves by label OR
//! account-uuid (the same roster resolution the offline `list` view uses, #17);
//! the resolver never guesses — an unresolvable target exits "not found", an
//! ambiguous one exits "ambiguous", and neither writes anything.
//!
//! ## The pre-swap gate (default, without `--force`)
//!
//! Before swapping, a gate refuses (non-zero exit, specific reason, ZERO writes)
//! when the target is not a sound destination:
//!   - its WEEKLY window is exhausted (#11/#37 viability), or it is QUARANTINED /
//!     needs re-login (#42) — both surfaced by polling the target's STASHED token
//!     through the existing [`RosterPoller`] seam (a `401`/`403` is the one-shot,
//!     daemon-independent signal for a dead credential);
//!   - a swap COOLDOWN is currently active (#10), derived from the durable event
//!     log's most-recent swap (the daemon's in-memory `last_swap` is socket-only).
//!
//! If the target is ALREADY ACTIVE it is a no-op success (no write).
//!
//! ## `--force`
//!
//! `--force` bypasses the POLICY gates above (weekly-exhausted, cooldown,
//! already-active — a re-write is then allowed) and still WARNS (warn-and-proceed,
//! no prompt) when forcing onto a weekly-exhausted or quarantined target. It NEVER
//! bypasses any SAFETY behavior: a locked keychain still aborts with the locked
//! exit code and ZERO writes (the swap engine reads the canonical item first); the
//! swap stays on the `apple-tool:` CLI path; write-ordering and the atomic,
//! field-preserving `~/.claude.json` co-write are preserved; and output redaction
//! (#15) holds on every channel (all output is sourced from non-secret handles).
//!
//! ## Adopt-target recovery (issue #212)
//!
//! `--force` ALSO recovers the session when the active credential itself is GONE or
//! ROTATED — a forced Claude logout that scrubbed / rotated the canonical keychain
//! token (issue #209), leaving no sound outgoing account to swap AWAY from (token-first
//! resolution, #207, finds no stash when the token itself is gone). When the canonical
//! is confirmed-absent (scrubbed) OR the outgoing is otherwise unresolvable (e.g. a
//! readable but rotated token that matches no stash, with the display cleared too),
//! [`run_use`] routes to the swap engine's [`swap::adopt_target`] variant instead of the
//! normal re-stash swap: it installs ONLY the target (canonical write + `~/.claude.json`
//! co-write — the sequence's steps 3–5), skipping the outgoing read + re-stash (steps
//! 1–2). The departing (dead / absent) token is not required, and because nothing is
//! re-stashed, no credential can be stapled under a wrong identity (#211 is moot).
//! SAFETY is unchanged: a LOCKED keychain still aborts (locked ≠ gone — transient, retry
//! when unlocked) — as does a canonical that merely CANNOT BE READ for any other reason
//! (an ACL / auth-deny: "could not read" ≠ "gone"; only a confirmed-absent or readable
//! canonical is adopted). Recovery is `--force`-gated (WITHOUT it, an unresolvable
//! outgoing stays the fail-closed [`Error::ActiveAccountUnresolved`]).
//!
//! ## The forced target is a NAMED escape hatch (issue #63)
//!
//! [`SwapTarget`] wraps the incoming stash name the swap engine consumes; its field
//! is PRIVATE and its only two constructors live here. [`SwapTarget::resolve`] (the
//! gated path) mints one ONLY on the proven-viable branch, so a non-`--force` swap
//! structurally cannot name a quarantined/exhausted account. [`SwapTarget::forced`]
//! is the single, explicitly-named way to target a non-viable account, used ONLY by
//! `--force`. The autonomous daemon never constructs a [`SwapTarget`] at all — it
//! selects a target by index through [`crate::daemon`]'s `pick_target`, whose
//! quarantine exclusion (a quarantined account is never polled, so it has no reading
//! to select) is an unchanged, separately-tested data-flow invariant. This command's
//! forced constructor therefore does not — and cannot — widen the autonomous path.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use crate::active;
use crate::canary::{self, CanaryOutcome, InconclusiveReason, ProbeGate};
use crate::config::{Account, Config};
#[cfg(test)]
use crate::daemon::NoTargetCause;
use crate::daemon::{
    AccountStatusLine, NextSwap, NextSwapReason, RealRosterPoller, RosterPoller, StatusResponse,
    SwapAck, SwapRejection,
};
use crate::error::{Error, Result};
use crate::keychain::{CredentialStore, RealCredentialStore};
use crate::observability::{self, Event, EventLog, SwapReason};
use crate::paths;
use crate::stash::{AccountStash, RealAccountStash};
use crate::swap;

/// How long either control-socket exchange `use` makes — the cached-reading query
/// ([`ControlSocketCache`], issue #75) before the gate, and the best-effort
/// manual-hold notify ([`ControlSocketNotifier`], issue #64) after the swap —
/// waits before giving up. Short: a live daemon, idle between polls, answers
/// instantly; a missing or wedged daemon must NEVER hang `use`, so each exchange
/// times out and degrades gracefully — the query falls back to a single live poll,
/// and the notify is logged-and-ignored (the swap already succeeded — the keychain
/// write is authoritative).
const CONTROL_SOCKET_TIMEOUT: Duration = Duration::from_secs(2);

/// Notifies a running daemon that a manual swap just committed (issue #64), so it
/// arms its cooldown (#10) and re-resolves active — the "manual-hold" that stops
/// the daemon immediately reverting the operator's choice on its next poll.
///
/// BEST-EFFORT by contract: the keychain write is authoritative, so the manual
/// swap has already SUCCEEDED by the time this runs; a notify failure (no daemon,
/// a timeout) is logged and ignored, never fatal. Injected as a seam so both the
/// success and failure paths are hermetically testable.
trait ManualSwapNotifier {
    async fn notify(&self) -> Result<()>;
}

/// The real [`ManualSwapNotifier`]: connect to the daemon's control socket and
/// send one newline-delimited `manual-swapped` request (issue #64), reading the
/// one-line ack so the daemon has received it before returning. Bounded by
/// [`CONTROL_SOCKET_TIMEOUT`] so a missing / wedged daemon never hangs `use`;
/// the "no daemon" case (connect refused / not found) and a timeout both surface
/// as `Err` for the caller to log-and-ignore. The request carries NO credential
/// and NO write target — it is a pure cooldown-only signal.
struct ControlSocketNotifier {
    socket: PathBuf,
}

impl ManualSwapNotifier for ControlSocketNotifier {
    async fn notify(&self) -> Result<()> {
        use tokio::io::{AsyncBufReadExt, AsyncWriteExt};

        let exchange = async {
            let stream = tokio::net::UnixStream::connect(&self.socket).await?;
            let mut buffered = tokio::io::BufReader::new(stream);
            buffered
                .write_all(b"{\"cmd\":\"manual-swapped\"}\n")
                .await?;
            buffered.flush().await?;
            // Read the one-line ack so the daemon has processed the request before
            // we return; the content is irrelevant (any failure is non-fatal above).
            let mut line = String::new();
            buffered.read_line(&mut line).await?;
            Ok::<(), Error>(())
        };
        tokio::time::timeout(CONTROL_SOCKET_TIMEOUT, exchange)
            .await
            .map_err(|_| {
                Error::Io(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    "manual-hold notify timed out",
                ))
            })?
    }
}

/// Consults the daemon's CACHED per-account reading for a target's viability
/// (issue #75), so the pre-swap gate need not issue its OWN live usage poll when a
/// daemon is already polling on its cadence. Injected as a seam so both the
/// cache-HIT path (daemon up, usable reading present) and the cache-MISS path (no
/// daemon / no usable reading → the caller's single live fallback) run hermetically
/// against in-memory fakes in tests.
trait CachedViabilitySource {
    /// The daemon's cached viability verdict for `account`, or `None` when there is
    /// no usable cached reading — no daemon running, the exchange failed, the
    /// target's handle is absent or non-unique on either side of the `roster`/reply
    /// pair, or the daemon's last poll for it failed. `None` is the signal to fall
    /// back to a single live poll.
    ///
    /// `roster` is the LOCAL roster the target was resolved against. It is carried
    /// because the reply keys on the label alone, which is not a unique handle — see
    /// [`cached_viability_for`], which owns that count and the reason for it.
    async fn cached_viability(&self, roster: &[Account], account: &Account) -> Option<Viability>;
}

/// The real [`CachedViabilitySource`]: ask the daemon's control socket for `status`
/// and read the target's CACHED viability from the reply (issue #75) — the SAME
/// non-secret [`StatusResponse`] the `status` command renders, carrying per-account
/// `quarantined` (#42) + `weekly_exhausted` (#11/#37, the daemon's own viability
/// verdict). Issues ZERO usage-endpoint requests of its own. Bounded by
/// [`CONTROL_SOCKET_TIMEOUT`] so a missing / wedged daemon never hangs `use`; ANY
/// failure (no daemon, a timeout, a malformed reply, an absent/duplicated handle)
/// is a cache MISS (`None`) → the caller's single live fallback poll.
struct ControlSocketCache {
    socket: PathBuf,
}

impl CachedViabilitySource for ControlSocketCache {
    async fn cached_viability(&self, roster: &[Account], account: &Account) -> Option<Viability> {
        // A live daemon answers instantly; a missing / wedged daemon must never hang
        // `use`, so a timeout — like any other exchange failure — is a cache MISS.
        let response = tokio::time::timeout(CONTROL_SOCKET_TIMEOUT, self.query_status())
            .await
            .ok()? // timed out → MISS
            .ok()?; // no daemon / I/O / malformed reply → MISS
        cached_viability_for(&response, roster, &account.label)
    }
}

impl ControlSocketCache {
    /// One `status` request/reply over the control socket, parsed into the shared
    /// [`StatusResponse`]. The SAME newline-delimited JSON the daemon's
    /// `serve_control` speaks and the `status` command's own client uses; the shared
    /// wire type keeps the two clients in lockstep. The "no daemon" case (connect
    /// refused / not found) needs no special remap — the caller maps EVERY error
    /// identically to a cache MISS (fall back to a live poll).
    async fn query_status(&self) -> Result<StatusResponse> {
        use tokio::io::{AsyncBufReadExt, AsyncWriteExt};

        let stream = tokio::net::UnixStream::connect(&self.socket).await?;
        let mut buffered = tokio::io::BufReader::new(stream);
        buffered.write_all(b"{\"cmd\":\"status\"}\n").await?;
        buffered.flush().await?;
        let mut line = String::new();
        buffered.read_line(&mut line).await?;
        serde_json::from_str(line.trim_end()).map_err(|err| Error::Io(std::io::Error::other(err)))
    }
}

/// The daemon's cached viability for the account with handle `label`, or `None`
/// when no usable verdict can be ATTRIBUTED to it (issue #75). The handle carries a
/// verdict only while it names EXACTLY ONE account on BOTH sides — once in `roster`,
/// once in the wire reply — because labels are operator handles and NOT guaranteed
/// unique (see [`resolve_target`]) while the reply carries only the handle (issue
/// #15: never the account-uuid). Either count off and the bearers are
/// indistinguishable from a label alone, so this returns `None` — "no usable cached
/// reading", the caller's live fallback — rather than guessing. The same both-counts
/// shape [`crate::poke`]'s `daemon_verdict` takes, and for the same reason (issue
/// #1086).
///
/// BOTH counts, because each catches a misattribution the other misses. The WIRE
/// count catches a roster the daemon knows better than this process does. The ROSTER
/// count catches the reverse, and it is the one issue #1201 added: the daemon adopts
/// a new roster only when a reload is SIGNALLED (issue #139), so its snapshot
/// legitimately lags, and a second bearer added on disk leaves the wire with ONE line
/// that is not necessarily the named account's. `resolve_target` shuts the label door
/// on that roster — a duplicated label is refused outright — but it resolves a UUID
/// query without complaint, and the wire lookup past it keys on the label, so the uuid
/// is the door the wire count alone leaves open.
///
/// Degrading costs `use` almost nothing, which is why it degrades on a count `poke`
/// had to weigh: the fallback is a single live poll of the target's OWN stashed token
/// ([`poll_viability`]), keyed by account-uuid through the stash name, so it cannot
/// mistake the bearer. The cost is one usage request against issue #75's "zero
/// requests when a daemon is up" — and only in a roster that duplicates a label,
/// which is already the shape `resolve_target` refuses to resolve by name.
///
/// RESIDUAL, unclosable without a stable per-account key on the wire: a label
/// REASSIGNED to a different account-uuid between the snapshot and the current roster
/// names one account on each side, passes both counts, and still reads the stale
/// bearer's line. Bounded — the verdict drives a refusal or a permit, never which
/// stash is written, so the swap still operates on the correct target and the daemon's
/// next cycle or a `--force` corrects a wrong refusal. This is the same residual
/// `poke`'s `daemon_verdict` records, and closing it would mean carrying a stable
/// per-account key on a surface issue #15 restricts to the operator-authored handle —
/// the alternative #1086 weighed and rejected for `poke`, over the SAME wire type this
/// side reads.
fn cached_viability_for(
    response: &StatusResponse,
    roster: &[Account],
    label: &str,
) -> Option<Viability> {
    // ROSTER side (issue #1201). Zero bearers is unreachable from either production
    // call site — both pass a roster account's OWN label — and degrades identically
    // to a duplicated one, which is the safe answer for a label naming nothing here.
    let mut bearers = roster.iter().filter(|account| account.label == label);
    bearers.next()?;
    if bearers.next().is_some() {
        return None;
    }
    // WIRE side (issue #75). A duplicated handle cannot be disambiguated from the
    // reply alone, which carries no account-uuid to break the tie.
    let mut matches = response.accounts.iter().filter(|line| line.label == label);
    let line = matches.next()?;
    if matches.next().is_some() {
        return None;
    }
    cached_viability_of(line)
}

/// Map one daemon `status` line to a cached viability verdict, or `None` when the
/// line carries no usable reading (issue #75). The daemon's own flags ARE the
/// verdict: `quarantined` (#42, checked first — a dead credential is the harder
/// block and the daemon stops polling it, so it carries no usage payload) and
/// `weekly_exhausted` (#11/#37, computed off the SAME un-jittered base the gate
/// treats as exhausted) need no usage reading. Otherwise a line is viable ONLY when
/// the daemon actually holds a fresh reading for it (`weekly_pct.is_some()`); a
/// non-quarantined line with no reading means the daemon's last poll for it failed
/// (or it is parked / unpolled) — NOT a viability verdict → `None`, so the caller
/// falls back to a live poll.
fn cached_viability_of(line: &AccountStatusLine) -> Option<Viability> {
    if line.quarantined {
        Some(Viability::Quarantined)
    } else if line.weekly_exhausted {
        Some(Viability::WeeklyExhausted)
    } else if line.weekly_pct.is_some() {
        Some(Viability::Viable)
    } else {
        None
    }
}

/// A vetted incoming swap target — the `incoming_stash` name [`swap::swap`] needs,
/// plus a TYPE-LEVEL certificate of HOW it was vetted. The field is private and the
/// only two constructors are [`SwapTarget::resolve`] (gated: mints solely on the
/// proven-viable branch) and [`SwapTarget::forced`] (the named `--force` escape
/// hatch), so no other code path — the daemon included — can produce one except
/// through those two auditable doors (issue #63).
struct SwapTarget {
    incoming_stash: String,
}

/// The target's viability — sourced from the daemon's CACHED reading when one is
/// available (issue #75), else proven by a live poll of its stashed token.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Viability {
    /// Below the weekly trigger — a sound destination.
    Viable,
    /// The stored ACCESS token was rejected (`401`/`403`) — quarantined / out of rotation
    /// (#42). NOT proven dead: a 401 never sees the refresh token, so the remedy is a
    /// refresh (`sessiometer poke`), not a re-login (issue #427).
    Quarantined,
    /// At/above the weekly trigger — the weekly window is exhausted (#11/#37).
    WeeklyExhausted,
}

/// The pre-swap gate's verdict for a non-`--force` `use` (issue #63). Only
/// [`GateOutcome::Proceed`] carries a [`SwapTarget`]; every refusal carries none,
/// so "refused ⇒ ZERO writes" is structural — the caller has nothing to swap with
/// on any non-proceed branch.
enum GateOutcome {
    /// The gate passed: swap to this vetted target.
    Proceed(SwapTarget),
    /// The target is already the active account — a no-op success (no write).
    AlreadyActive,
    /// The gate refused before any write, for this reason.
    Refused(Refusal),
}

/// Why the pre-swap gate refused (without `--force`). Each maps to a distinct,
/// secret-free [`Error`] message sharing the one "gate-refused" exit code (`7`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Refusal {
    /// The target's weekly window is exhausted.
    WeeklyExhausted,
    /// A swap cooldown is currently active.
    Cooldown,
    /// The target is quarantined (its access token was rejected — out of rotation, but
    /// not proven dead; issue #427).
    Quarantined,
}

impl SwapTarget {
    /// The gated constructor — the ONLY path to a non-forced target. Runs the
    /// pre-swap gate for `account` (the resolved target): already-active → no-op;
    /// in-cooldown → refused; otherwise classify viability via [`gate_viability`]
    /// (the daemon's CACHED reading first, a single live poll only on a miss —
    /// issue #75) and mint a target ONLY when it is viable. A locked keychain or a
    /// transient poll failure on the live fallback propagates as `Err` (still
    /// before any write) for the caller to surface.
    ///
    /// `active_stash` is the active (outgoing) account's stash, `weekly_ceiling` is
    /// the fraction at/above which the weekly window counts as exhausted, and
    /// `in_cooldown` is the caller-computed cooldown verdict (kept a parameter so
    /// the gate stays pure and hermetically testable, independent of wall-clock).
    /// The cooldown refusal short-circuits BEFORE any cache query or live poll, so a
    /// cooled-down `use` touches neither the socket nor the network.
    async fn resolve<R: CachedViabilitySource, P: RosterPoller>(
        cache: &R,
        poller: &P,
        roster: &[Account],
        account: &Account,
        active_stash: &str,
        weekly_ceiling: f64,
        in_cooldown: bool,
    ) -> Result<GateOutcome> {
        if account.stash() == active_stash {
            return Ok(GateOutcome::AlreadyActive);
        }
        if in_cooldown {
            return Ok(GateOutcome::Refused(Refusal::Cooldown));
        }
        match gate_viability(cache, poller, roster, account, weekly_ceiling).await? {
            Viability::Viable => Ok(GateOutcome::Proceed(SwapTarget {
                incoming_stash: account.stash(),
            })),
            Viability::WeeklyExhausted => Ok(GateOutcome::Refused(Refusal::WeeklyExhausted)),
            Viability::Quarantined => Ok(GateOutcome::Refused(Refusal::Quarantined)),
        }
    }

    /// The escape hatch — the single, explicitly-named constructor for a target that
    /// has NOT been proven viable. Used ONLY by `use --force`. It bypasses the
    /// POLICY gate above; it does NOT bypass SAFETY, which lives downstream in
    /// [`swap::swap`] (canonical-first read ⇒ a locked keychain still aborts).
    fn forced(account: &Account) -> Self {
        SwapTarget {
            incoming_stash: account.stash(),
        }
    }

    /// The `incoming_stash` name to hand to [`swap::swap`].
    fn incoming_stash(&self) -> &str {
        &self.incoming_stash
    }
}

/// Classify the target's viability by polling its STASHED token (`active=false`),
/// exactly as the daemon polls a non-active account. A dead credential (`401`/`403`)
/// is the one-shot, daemon-independent signal for "quarantined / needs re-login"
/// (#42); a weekly reading at/above the trigger is "weekly-exhausted" (#11/#37);
/// anything else (including a poll that could not classify) is viable. A locked
/// keychain or a transient failure PROPAGATES — the caller decides what to do with
/// it (the gated path aborts; `--force` treats it best-effort).
async fn poll_viability<P: RosterPoller>(
    poller: &P,
    account: &Account,
    weekly_ceiling: f64,
) -> Result<Viability> {
    match poller.poll(account, false).await {
        // Only the weekly dimension of the reading drives the viability verdict; the
        // sample-only fields on the `PolledReading` are irrelevant here (this is the
        // one-shot `use`-command probe, not the daemon's sampling poll loop).
        Ok(reading) if reading.usage.weekly >= weekly_ceiling => Ok(Viability::WeeklyExhausted),
        Ok(_) => Ok(Viability::Viable),
        // A dead stored token: the daemon-independent "quarantined / needs re-login"
        // signal (#42). 401 (rejected) and 403 (missing usage scope) both mean the
        // stored credential cannot authenticate.
        Err(Error::UsageUnauthorized | Error::UsageScopeMissing) => Ok(Viability::Quarantined),
        // A locked keychain (SAFETY) or a transient poll failure: not a viability
        // verdict — propagate for the caller to surface or tolerate.
        Err(other) => Err(other),
    }
}

/// The pre-swap gate's viability check (issue #75): consult the daemon's CACHED
/// reading FIRST — zero usage-endpoint requests when a daemon is up — and only on a
/// cache MISS (no daemon running, or no usable cached reading for the target) fall
/// back to a single live [`poll_viability`] poll. The fallback is today's
/// behaviour, preserving the "`use` needs no daemon" property.
///
/// On that live fallback a `429` ([`Error::UsageRateLimited`]) is remapped to the
/// distinct [`Error::UseViabilityUnverifiable`] — a clear, actionable abort naming
/// the target instead of the opaque raw rate-limit error (issue #75 acceptance). A
/// locked keychain and every other transient poll failure propagate unchanged: the
/// gated caller aborts with ZERO writes. (`--force` does NOT route through here —
/// it tolerates a miss best-effort; see [`run_use`].)
async fn gate_viability<R, P>(
    cache: &R,
    poller: &P,
    roster: &[Account],
    account: &Account,
    weekly_ceiling: f64,
) -> Result<Viability>
where
    R: CachedViabilitySource,
    P: RosterPoller,
{
    if let Some(cached) = cache.cached_viability(roster, account).await {
        return Ok(cached);
    }
    // Cache MISS → a single live poll (today's behaviour). A `429` here is the
    // opaque abort issue #75 fixes: with no daemon to consult, surface a distinct,
    // actionable error rather than the raw rate-limit.
    match poll_viability(poller, account, weekly_ceiling).await {
        Err(Error::UsageRateLimited { .. }) => Err(Error::UseViabilityUnverifiable {
            label: account.label.clone(),
        }),
        other => other,
    }
}

/// Whether a swap cooldown is currently active: `last_swap_at` known AND less than
/// `cooldown` has elapsed since it as of `now`. Pure, so the gate is hermetically
/// testable without a real clock or log. No prior swap (`None`) ⇒ not in cooldown;
/// a `cooldown` of zero ⇒ never in cooldown; a `last_swap_at` in the future (clock
/// skew) ⇒ not in cooldown (a one-shot manual swap is not blocked by a weird clock).
fn cooldown_active(last_swap_at: Option<SystemTime>, now: SystemTime, cooldown: Duration) -> bool {
    match last_swap_at {
        Some(last) => now
            .duration_since(last)
            .map(|elapsed| elapsed < cooldown)
            .unwrap_or(false),
        None => false,
    }
}

/// Resolve `query` to a single roster INDEX by label OR account-uuid — the same
/// resolution the offline `list` view keys on (#17), shared with the one-shot `poke`
/// command (issue #104). The resolver NEVER guesses: zero matches is
/// [`Error::UseTargetNotFound`], more than one (a duplicated label) is
/// [`Error::UseTargetAmbiguous`]. Each account is counted once even if both its
/// fields equal `query`.
pub(crate) fn resolve_target(roster: &[Account], query: &str) -> Result<usize> {
    let matches: Vec<usize> = roster
        .iter()
        .enumerate()
        .filter(|(_, account)| account.label == query || account.account_uuid == query)
        .map(|(i, _)| i)
        .collect();
    match matches.as_slice() {
        [] => Err(Error::UseTargetNotFound {
            query: query.to_owned(),
        }),
        [i] => Ok(*i),
        many => Err(Error::UseTargetAmbiguous {
            query: query.to_owned(),
            count: many.len(),
        }),
    }
}

/// The one-line confirmation a completed swap prints: `from → to`, both non-secret
/// handles (issue #15 — never a token or email).
fn swap_confirmation(from: &str, to: &str) -> String {
    format!("{from} → {to}")
}

/// The confirmation an already-active no-op prints (no swap performed). Names only
/// the non-secret handle.
fn already_active_confirmation(label: &str) -> String {
    format!("`{label}` is already active")
}

/// The `--force` warning for forcing onto a weekly-exhausted target. Names only the
/// non-secret handle.
fn warn_weekly_exhausted(label: &str) -> String {
    format!("warning: forcing onto `{label}`, whose weekly window is exhausted")
}

/// The `--force` warning for forcing onto a quarantined target — its ACCESS token was
/// rejected, so it is out of rotation, but a refresh (`sessiometer poke`) may revive it;
/// it is NOT proven dead (issue #427). Names only the non-secret handle.
fn warn_quarantined(label: &str) -> String {
    format!(
        "warning: forcing onto `{label}`, which is quarantined (out of rotation — a `sessiometer poke` may refresh it)"
    )
}

/// The `from` handle logged / printed for an adopt-target recovery (issue #212) when
/// the outgoing account is genuinely unknown — the canonical was scrubbed / rotated AND
/// `~/.claude.json` was cleared, so no roster account resolves. A non-secret sentinel
/// (issue #15 — never a token, email, or account-uuid).
const ADOPT_UNKNOWN_FROM: &str = "(unknown)";

/// The adopt-target recovery note (issue #212): the canonical credential was gone or
/// rotated, so the target was installed DIRECTLY — the previous account was NOT
/// re-stashed (there was no sound outgoing token to re-stash). Tells the operator what
/// the recovery did; names only the non-secret handle (issue #15).
fn note_adopt_target(label: &str) -> String {
    format!(
        "note: the previous credential was gone or rotated — adopted `{label}` directly \
         (the previous account was not re-stashed)"
    )
}

/// The `--force` warn-and-proceed warning for forcing onto a target of this
/// `viability`, or `None` when it is viable (nothing to warn about). The pure
/// DECISION of WHICH warning a forced swap emits — split from the `eprintln!` in
/// [`run_use`] so the viability→warning mapping is unit-tested directly (this
/// crate's "pure producer + thin I/O wrapper" idiom), rather than only inferred
/// from the swap outcome.
fn force_warning(viability: Viability, label: &str) -> Option<String> {
    match viability {
        Viability::WeeklyExhausted => Some(warn_weekly_exhausted(label)),
        Viability::Quarantined => Some(warn_quarantined(label)),
        Viability::Viable => None,
    }
}

/// The `--force` warn-and-proceed viability probe, shared by the normal forced swap and
/// the adopt-target recovery (issue #212 / #63): consult the daemon's CACHED verdict
/// first (issue #75 — ZERO usage-endpoint requests when a daemon is up), else a single
/// live poll. SAFETY is never bypassed — a LOCKED keychain on the live fallback ABORTS
/// (ZERO writes, `Err` propagates); any other poll failure (transient / `429`) only
/// costs the informational warning, so the forced swap proceeds without one (decision
/// D1). Emits the specific warning — none for a viable target, none when viability is
/// unknown (cache miss + failed live poll) — via stderr. Extracted so the forced and
/// adopt paths cannot drift on this safety-bearing locked-abort.
async fn warn_if_forcing_onto_non_viable<R, P>(
    cache: &R,
    poller: &P,
    roster: &[Account],
    target: &Account,
    weekly_ceiling: f64,
) -> Result<()>
where
    R: CachedViabilitySource,
    P: RosterPoller,
{
    let viability = match cache.cached_viability(roster, target).await {
        Some(cached) => Some(cached),
        None => match poll_viability(poller, target, weekly_ceiling).await {
            Ok(viability) => Some(viability),
            // SAFETY is never bypassed: a locked keychain aborts even with `--force`
            // (ZERO writes — the swap never runs).
            Err(err @ Error::KeychainLocked { .. }) => return Err(err),
            // A transient / rate-limited poll only affects the (informational) warning,
            // so the forced swap proceeds without one (decision D1).
            Err(_) => None,
        },
    };
    if let Some(warning) = viability.and_then(|v| force_warning(v, &target.label)) {
        eprintln!("{warning}");
    }
    Ok(())
}

/// The injectable seams [`run_use`] drives — the viability/credential/stash/state
/// surfaces — so the whole gate→swap flow runs hermetically against in-memory fakes
/// in tests, exactly as [`crate::daemon::Daemon`] injects its seams.
struct Seams<'a, R, P, C, S, N> {
    /// Consults the daemon's CACHED per-account viability over the control socket
    /// (issue #75) — so the gate need not issue its own live poll when a daemon is
    /// already polling. A miss falls back to `poller`.
    cache: &'a R,
    /// Polls the TARGET's stashed token for viability (#37/#42) — the live FALLBACK
    /// when the daemon holds no cached reading (issue #75).
    poller: &'a P,
    /// The canonical credential the swap reroutes (#6).
    store: &'a C,
    /// The per-account stash the swap reads / re-stashes (#6).
    stash: &'a S,
    /// Claude Code's `~/.claude.json`: the active-account source (read) and the
    /// swap's best-effort display co-write target.
    claude_json: &'a Path,
    /// The single-writer swap lock file (#64): the swap acquires it (blocking,
    /// bounded, fail-closed) so a concurrent daemon swap cannot interleave. A real
    /// path in production; a throwaway file in tests (uncontended → instant).
    lock_path: &'a Path,
    /// Best-effort daemon notifier (#64): pinged AFTER the swap commits and the
    /// lock is released so a running daemon arms its cooldown (manual-hold).
    notifier: &'a N,
}

/// Run the `use <account>` flow over injected seams: resolve the target, identify
/// the active (outgoing) account, gate (or `--force`-bypass), then reuse the swap
/// engine UNCHANGED, emit the standard event (#9), and print the confirmation.
///
/// The hermetic core of the command — generic over its seams so tests drive it with
/// in-memory fakes. Returns `Ok(())` on a completed swap or an already-active no-op;
/// every refusal / abort is a typed [`Error`] whose `exit_code` extends the taxonomy
/// (issue #63), and on every error path the swap has not run, so there are ZERO
/// writes.
async fn run_use<R, P, C, S, N>(
    config: &Config,
    query: &str,
    force: bool,
    in_cooldown: bool,
    seams: Seams<'_, R, P, C, S, N>,
    log: &mut EventLog,
) -> Result<()>
where
    R: CachedViabilitySource,
    P: RosterPoller,
    C: CredentialStore,
    S: AccountStash,
    N: ManualSwapNotifier,
{
    // 1. Resolve the target by label OR uuid (the resolver never guesses, #17).
    let target = &config.roster[resolve_target(&config.roster, query)?];
    let target_label = target.label.clone();

    // 2. Identify the active (outgoing) account TOKEN-FIRST, mirroring the daemon
    //    (issue #207). The swap re-stashes the outgoing account, so its roster
    //    identity MUST be known — but the CANONICAL keychain token is the
    //    authoritative bearer, whereas `~/.claude.json`'s `oauthAccount` is only the
    //    clobberable display half Claude Code clears out-of-band on a forced logout.
    //    Resolving from the display alone made `use` (the recovery verb) hard-fail
    //    `ActiveAccountUnresolved` exactly when an operator needed to swap AWAY from a
    //    logged-out account. Issue #212 extends this: when a forced logout ALSO scrubs /
    //    rotates the canonical keychain token, token-first resolution finds no stash, so
    //    `--force` recovers via adopt-target (step 3 below) instead of hard-failing.
    //    Read the canonical ONCE and classify it. The credential is treated as GONE
    //    (the #212 recovery signal) ONLY on positive evidence — a CONFIRMED-absent item
    //    (`CredentialNotFound`, the scrubbed token). A LOCKED keychain is a SAFETY abort
    //    (locked ≠ gone — transient, retry when unlocked), and EVERY other read failure
    //    (an ACL / auth-deny or other `security` error, ambiguity, I/O) is likewise an
    //    abort: a canonical we merely *could not read* is NOT proven *gone* — treating it
    //    as gone would let `--force` adopt-clobber a present token without re-stashing it
    //    (the #211 loss). Resolve token→stash, then the display fallback (the clobberable
    //    half above), via the shared resolver both this verb and the daemon use.
    let canonical = match seams.store.read().await {
        Ok(canonical) => Some(canonical),
        // SAFETY: a LOCKED keychain aborts here with the locked exit code and ZERO
        // writes (the swap never runs) — never swallowed to `ActiveAccountUnresolved`
        // nor to the adopt-target recovery path (locked ≠ gone).
        Err(err @ Error::KeychainLocked { .. }) => return Err(err),
        // CONFIRMED absent (errSecItemNotFound): the scrubbed canonical. `None` degrades
        // active resolution to the display-only signal AND is the adopt-target recovery
        // signal below (issue #212).
        Err(Error::CredentialNotFound) => None,
        // PRESENT-but-unreadable for another reason (ACL / auth-deny or other `security`
        // error, ambiguity, I/O): "could not read" is NOT "gone". Abort with ZERO writes
        // rather than misclassify as gone and adopt-clobber it — mirroring the engine
        // probe and the normal swap's step-1 read (issue #212).
        Err(err) => return Err(err),
    };
    let active = match &canonical {
        Some(canonical) => {
            active::resolve_account_for(&config.roster, seams.stash, seams.claude_json, canonical)
                .await
        }
        // No readable canonical → the display is the only remaining signal (it may be
        // cleared too, leaving the outgoing genuinely unknown — adopt-target's case).
        None => active::resolve_via_display(&config.roster, seams.claude_json),
    }
    .map(|idx| &config.roster[idx]);

    // Issue #607: the weekly ROTATION line — the configured ceiling less the tail margin — not the
    // raw ceiling. `weekly_ceiling` is a CEILING since #607 and the daemon releases at
    // `ceiling − WEEKLY_TAIL_MARGIN`, so gating `use` on the raw value would ACCEPT a target in the
    // `[ceiling − margin, ceiling)` band that the daemon then swaps away from on its next tick — an
    // operator command that silently undoes itself. Gating on the same line the daemon rotates
    // against keeps the standalone `use` verdict and the daemon's verdict identical (the daemon-
    // attached path gets the same value from `Daemon::weekly_rotation_line`).
    let weekly_ceiling =
        crate::swap::weekly_effective_ceiling(f64::from(config.tunables.weekly_ceiling) / 100.0);

    // 3. Decide the swap MODE and perform the keychain rotation, yielding
    //    `(from_label, reason, adopted)` for the shared event / notify / print tail:
    //    - ADOPT-TARGET RECOVERY (#212): with `--force`, when the canonical is GONE
    //      (`canonical` is `None`) OR the outgoing account is unresolvable, the normal
    //      re-stash swap cannot run (its steps 1–2 read + re-stash the outgoing
    //      canonical, which is absent). Skip those and install the target (steps 3–5)
    //      via `adopt_target_locked`. `--force`-gated; a locked keychain already
    //      aborted above.
    //    - FORCED swap (#63): `--force` with a sound outgoing — bypass the policy gates.
    //    - GATED swap (#63): the default pre-swap gate.
    let adopt = force && (canonical.is_none() || active.is_none());
    let (from_label, reason, adopted) = if adopt {
        // Warn-and-proceed if forcing onto a non-viable target, exactly as a normal
        // forced swap does; a locked keychain on the viability poll still aborts (ZERO
        // writes) — the always-enforced safety.
        warn_if_forcing_onto_non_viable(
            seams.cache,
            seams.poller,
            &config.roster,
            target,
            weekly_ceiling,
        )
        .await?;
        // Adopt: skip the outgoing re-stash, install the target (steps 3–5), lock-wrapped
        // (#64) so a concurrent daemon swap cannot interleave. SAFETY still holds inside
        // the engine: the canonical is probed for a LOCK before any write (ZERO writes on
        // lock — locked ≠ gone), and the incoming stash is read before any mutation. The
        // departing (dead / absent) token is NOT required, and nothing is re-stashed, so
        // no credential can be stapled under a wrong identity (#211 is moot here).
        swap::adopt_target_locked(
            Some((seams.lock_path, swap::SWAP_LOCK_MAX_WAIT)),
            seams.store,
            seams.stash,
            &target.stash(),
            seams.claude_json,
        )
        .await?;
        // The outgoing account is gone / unknown; name it if the display still resolved
        // one, else a non-secret sentinel (issue #15 — never a token or email).
        let from = active
            .map(|a| a.label.clone())
            .unwrap_or_else(|| ADOPT_UNKNOWN_FROM.to_owned());
        (from, SwapReason::Forced, true)
    } else {
        // The normal paths re-stash the outgoing account, so it MUST be known. (Without
        // `--force`, a gone canonical / unresolvable outgoing stays the fail-closed
        // `ActiveAccountUnresolved` — recovery requires `--force`.)
        let active = active.ok_or(Error::ActiveAccountUnresolved)?;
        let active_stash = active.stash();
        let active_label = active.label.clone();

        // Gate (default) or `--force`-bypass — yielding the vetted target + reason.
        let (swap_target, reason) = if force {
            // `--force` bypasses the POLICY gates (cooldown, weekly-exhausted,
            // already-active), but still WARNS when forcing onto a non-viable target.
            warn_if_forcing_onto_non_viable(
                seams.cache,
                seams.poller,
                &config.roster,
                target,
                weekly_ceiling,
            )
            .await?;
            (SwapTarget::forced(target), SwapReason::Forced)
        } else {
            match SwapTarget::resolve(
                seams.cache,
                seams.poller,
                &config.roster,
                target,
                &active_stash,
                weekly_ceiling,
                in_cooldown,
            )
            .await?
            {
                GateOutcome::Proceed(swap_target) => (swap_target, SwapReason::Manual),
                GateOutcome::AlreadyActive => {
                    // No-op success: already active, nothing to write. If token-first
                    // resolution (issue #207) reached here past a CLEARED `~/.claude.json`
                    // (target == the token-resolved active), the stale display is left
                    // unhealed on purpose — this no-op writes nothing; the daemon's
                    // next reconcile, or an explicit `use --force`, repairs the display.
                    println!("{}", already_active_confirmation(&target_label));
                    return Ok(());
                }
                GateOutcome::Refused(Refusal::WeeklyExhausted) => {
                    return Err(Error::UseTargetWeeklyExhausted {
                        label: target_label,
                    })
                }
                GateOutcome::Refused(Refusal::Cooldown) => return Err(Error::UseCooldownActive),
                GateOutcome::Refused(Refusal::Quarantined) => {
                    return Err(Error::UseTargetQuarantined {
                        label: target_label,
                    })
                }
            }
        };

        // Pre-swap behavioral canary (issue #714): the SAME fresh Layer-1 resolution
        // probe → display reconcile → Layer-2 stash-token cross-check the daemon runs
        // in `locked_swap`, so the daemon-DOWN write path is gated identically to the
        // daemon-routed one. SAFETY, not policy — `--force` does NOT bypass it (it sits
        // with the locked-keychain abort, not the cooldown/viability gates); only the
        // documented `canary_drift_override` tunable lets a diagnosed-false DRIFT
        // proceed, and an overridden run is still logged. Layer-1 refusals (gone /
        // ambiguous resolution) have no override — an atomic in-place write has no
        // unique, safe target. INCONCLUSIVE proceeds (never block on "couldn't
        // verify"). Refusal events are best-effort like the daemon's: the typed error
        // must surface even if the log write fails. The adopt branch above is exempt by
        // construction: it runs only when the canonical is CONFIRMED-gone / the
        // outgoing unresolvable, so there is no resolved credential to cross-check.
        match canary::run(seams.store, seams.stash, &config.roster, seams.claude_json).await? {
            CanaryOutcome::NotFound => return Err(Error::CredentialNotFound),
            CanaryOutcome::Ambiguous { count } => {
                let _ = log.emit(&Event::CanaryAmbiguous { count });
                return Err(Error::CredentialAmbiguous { count });
            }
            CanaryOutcome::Drift { displayed, matched } => {
                let displayed = config.roster[displayed].label.clone();
                let matched = config.roster[matched].label.clone();
                let overridden = config.tunables.canary_drift_override;
                let _ = log.emit(&Event::CanaryDrift {
                    displayed: displayed.clone(),
                    matched: matched.clone(),
                    overridden,
                });
                if !overridden {
                    return Err(Error::CanaryDrift { displayed, matched });
                }
            }
            CanaryOutcome::Inconclusive(InconclusiveReason::NoStashMatch {
                canonical_well_formed: false,
            }) => {
                // #730: the resolved canonical matches no stash AND does not parse as a
                // Claude Code credential — an unrelated secret the atomic `-U` upsert must
                // NOT clobber. Fail CLOSED unless the dedicated `canary_nostashmatch_override`
                // is set. Redaction-safe like the drift refusal (the `use` path has no carried
                // state to edge off, so it emits one line per invocation).
                let overridden = config.tunables.canary_nostashmatch_override;
                let _ = log.emit(&Event::CanaryUnparseableCanonical { overridden });
                if !overridden {
                    return Err(Error::CanaryUnparseableCanonical);
                }
            }
            CanaryOutcome::Ok | CanaryOutcome::Inconclusive(_) => {}
        }

        // Layer 3 (issue #736): the opt-in ONLINE liveness probe, the standalone mirror of
        // the daemon's — same slot (offline layers cleared, still pre-mutation), same double
        // opt-in, same graceful-degrade default. Disarmed by default, in which case
        // `probe_liveness` issues no request at all and this is a no-op; armed, it asks only
        // whether the resolved canonical's bearer still authenticates, never WHOSE session it
        // is (`/oauth/usage` carries no identity field — issue #737 is the separate, gated
        // identity fetch). `active` is the account the display names active, polled with
        // `active = true` so the request rides the CANONICAL credential rather than a stash.
        //
        // Like its offline siblings on THIS path the emission is per invocation — the `use`
        // path carries no state to edge off, and each line here IS one operator action.
        // Alarm-only: a confirmed-live probe stays silent, and a failed one logs whether or
        // not it refused, so the graceful-degrade ride is never invisible.
        //
        // `--force` DOES bypass this layer, unlike the offline ones above — the one place
        // the layers are treated differently, and deliberately. Layers 1 and 2 refuse
        // because the write would clobber an unrelated secret UNRECOVERABLY, which no
        // operator intent can make safe. Layer 3 refuses on much weaker evidence and a far
        // milder consequence: a dead canonical does not endanger another secret, it only
        // means the swap may not take effect. So `--force` stays what it is — the operator's
        // explicit "I have looked, do it anyway" — and remains the manual escape hatch when
        // a strict probe is wrongly blocking a swap the operator needs. The daemon-routed
        // path honours it identically (`Daemon::probe_gate`), so the escape does not depend
        // on whether the daemon happens to be up.
        //
        // A forced bypass of an ARMED probe is `Overridden`, not `Disarmed`: it LOGS. That
        // matches how the offline layers record their overrides (`overridden=true`), and it
        // is the only durable trace that a swap skipped a gate the operator had armed.
        let gate = match (config.tunables.canary_online_probe, force) {
            (true, false) => ProbeGate::Armed,
            (true, true) => ProbeGate::Overridden,
            (false, _) => ProbeGate::Disarmed,
        };
        let liveness = canary::probe_liveness(seams.poller, active, gate).await;
        let refused = canary::probe_refuses(liveness, config.tunables.canary_online_probe_strict);
        if canary::probe_alarms(liveness) {
            let _ = log.emit(&Event::CanaryOnlineProbe {
                verdict: liveness.as_str(),
                refused,
            });
        }
        if refused {
            return Err(Error::CanaryProbeNotLive {
                verdict: liveness.as_str(),
            });
        }

        // Reuse the swap engine UNCHANGED, wrapped in the single-writer swap lock
        // (#64): acquired (blocking, bounded) BEFORE the swap reads anything and held
        // across the whole two-step write, so a concurrent daemon swap cannot interleave
        // into a split state. FAIL-CLOSED — a contended lock that never frees within the
        // bounded wait aborts with `SwapLockBusy` (exit `4`, ZERO writes), never a torn
        // write. Inside, the engine's own discipline still holds: canonical write FIRST
        // (a locked keychain aborts here with ZERO writes — the always-enforced safety,
        // even with `--force`), then the atomic, field-preserving `~/.claude.json`
        // co-write.
        swap::swap_locked(
            Some((seams.lock_path, swap::SWAP_LOCK_MAX_WAIT)),
            seams.store,
            seams.stash,
            &active_stash,
            swap_target.incoming_stash(),
            seams.claude_json,
        )
        .await?;
        (active_label, reason, false)
    };

    // 4. Emit the standard structured event (#9) — the durable record that also updates
    //    `last_swap` — with the manual / forced reason. `session_pct=0`: a manual swap is
    //    not session-triggered (the reason distinguishes it). Sourced from non-secret
    //    handles only (issue #15); for an adopt recovery with an unknown outgoing, `from`
    //    is the non-secret `(unknown)` sentinel.
    log.emit(&Event::Swap {
        from: from_label.clone(),
        to: target_label.clone(),
        reason,
        session_pct: 0,
        // Manual / forced (or adopt-recovery) swap: not a projection-driven decision (issue #634).
        projection: None,
    })?;

    // 5. For an adopt-target recovery, tell the operator what the recovery did: the
    //    previous credential was gone / rotated, so the target was adopted directly and
    //    the previous account was NOT re-stashed. Non-secret handle only (issue #15).
    if adopted {
        eprintln!("{}", note_adopt_target(&target_label));
    }

    // 6. Manual-hold (#64): the swap has COMMITTED and the lock is released on return,
    //    so — and ONLY now, never before — best-effort notify a running daemon to arm
    //    its cooldown, so its next poll does not immediately revert this choice. A
    //    failure (no daemon, a timeout) is logged and ignored: the keychain write is
    //    authoritative, so the manual swap already succeeded.
    if let Err(err) = seams.notifier.notify().await {
        eprintln!("sessiometer: manual-hold notify skipped (is the daemon running?): {err}");
    }

    println!("{}", swap_confirmation(&from_label, &target_label));
    Ok(())
}

/// `sessiometer use <account> [--force]` / `use --next [--force]` — wire the REAL seams
/// into [`run_use`].
///
/// A bare `use` — neither `<account>` nor `--next` — is [`Error::UseTargetRequired`]:
/// there is deliberately no IMPLICIT "cycle to the next account" fallback (out of scope,
/// #63). Issue #960 makes that advance OPT-IN as `--next`, which fills exactly this slot
/// by READING the target from the daemon's published `next_swap` ([`resolve_next_target`])
/// rather than re-deriving selection here. Loads the real config (a friendly empty-state if
/// nothing is captured), derives the cooldown verdict from the durable event log, and drives
/// the swap over the live keychain (`apple-tool:` CLI path) and `~/.claude.json`.
///
/// `--next` composes with `--force` exactly as a named target does, and adds NO gate of its
/// own: it only supplies the handle, after which the path is byte-identical to a named
/// swap — so `--force` keeps overriding precisely the POLICY verdicts it always did
/// (quarantined / weekly-exhausted / cooldown) and keeps NOT overriding the SAFETY aborts
/// (locked keychain, contended swap lock) it never could.
pub(crate) async fn use_account(query: Option<String>, force: bool, next: bool) -> Result<()> {
    // Raised BEFORE any config load, exactly where it always was, so a bare `use`'s
    // ordering against the empty-roster empty-state is unchanged (issue #960 adds a way to
    // SUPPLY the target, and changes nothing about its absence).
    if query.is_none() && !next {
        return Err(Error::UseTargetRequired);
    }
    let config = Config::load()?;
    // Nothing to swap to if the roster is empty — the same friendly empty-state the
    // offline `list` view reports.
    config.require_roster()?;

    // Cooldown (#10): derived from the durable event log's most-recent swap — a
    // daemon-INDEPENDENT swap record, so `use` gates correctly with NO daemon
    // running. (The daemon's own in-memory `last_swap` is the live-socket view;
    // this manual path also NOTIFIES the daemon to arm that cooldown after a swap,
    // below — #64.) Bypassed by `--force`.
    let in_cooldown = if force {
        false
    } else {
        let last_swap_at = observability::last_swap_at(&observability::log_path()?);
        cooldown_active(
            last_swap_at,
            SystemTime::now(),
            Duration::from_secs(config.tunables.cooldown_secs),
        )
    };

    // The swap lock and the control socket live under the native-local support dir;
    // ensure it (0700) exists before the swap acquires the lock (#64). `use` needs
    // NO daemon — these are just files; the notify below is the only daemon-dependent
    // step, and it is best-effort.
    paths::ensure_private_dir(&paths::support_dir()?)?;

    let claude_json = paths::claude_json()?;
    let lock_path = paths::swap_lock()?;
    // All three control-socket clients (the cached-reading query #75, the manual-hold
    // notify #64, and the `--next` resolution #960) speak to the same daemon socket;
    // resolve it once.
    let control_socket = paths::control_socket()?;

    // `--next` (issue #960) is exactly the slot that supplies the target the operator did
    // not name — and it is READ from the daemon, never re-derived here. `NextSwap::Target`'s
    // own doc comment is why: the choice is DAEMON-AUTHORITATIVE, because the session
    // trigger and floor that `pick_target` consumes are daemon-only and never on the wire,
    // so a client-side re-implementation would be structurally unable to match the daemon's
    // pick and would silently diverge from the candidate `status` shows.
    //
    // Resolved HERE — after the roster check, so an empty roster still gets its friendly
    // empty-state rather than a daemon complaint, and before the swap, because the daemon's
    // answer IS the target the rest of this function operates on. `query` is `None` only
    // when `next` is set: the top-of-function guard returned for neither, and the parser
    // rejects both together.
    let query = match query {
        Some(target) => target,
        None => resolve_next_target(&control_socket).await?,
    };

    // Route THROUGH the daemon when one is up (issue #167): a SINGLE writer and a single place for
    // the lock, write-ordering, and redaction. `request_swap` carries only the target handle + the
    // POLICY force flag; the daemon re-validates the target's viability ITSELF and returns a
    // redacted ack. A reachable daemon's verdict is authoritative — `use` does NOT also write
    // standalone (that is exactly the torn / double write the unification removes). A reached-but-
    // failed exchange (`Err`) is surfaced here (the `?`), never retried standalone: the daemon may
    // already have written, so a standalone retry could double-write.
    match crate::daemon::request_swap(&control_socket, &query, force).await? {
        // EXACTLY the daemon's `no active account` rejection falls THROUGH to the standalone adopt
        // path below (issue #212 recovery) — see [`ack_falls_back_to_standalone`] for why this one
        // ack is a guaranteed zero-write and why the fallback is lock-safe.
        Some(ack) if ack_falls_back_to_standalone(&ack) => {}
        // Every OTHER reachable-daemon ack is authoritative: a completed / already-active swap, or a
        // policy/safety rejection (unknown / ambiguous / quarantined / weekly-exhausted / cooldown /
        // keychain-locked / swap-lock-busy / failed) the daemon already resolved. Report it and do
        // NOT also write standalone (that is the torn / double write the unification removes).
        Some(ack) => return report_swap_ack(ack, &query),
        // No daemon reachable (`Ok(None)`) — fall through to the standalone write path (daemon-down).
        None => {}
    }

    let cache = ControlSocketCache {
        socket: control_socket.clone(),
    };
    let notifier = ControlSocketNotifier {
        socket: control_socket,
    };
    let mut log = EventLog::open()?;
    run_use(
        &config,
        &query,
        force,
        in_cooldown,
        Seams {
            cache: &cache,
            poller: &RealRosterPoller::new(),
            store: &RealCredentialStore::new(),
            stash: &RealAccountStash::new(),
            claude_json: &claude_json,
            lock_path: &lock_path,
            notifier: &notifier,
        },
        &mut log,
    )
    .await
}

/// The note `use --next` prints once it knows WHICH account it is advancing to (issue #960),
/// carrying the daemon's own selection rationale ([`crate::daemon::NextSwapReason`], issue #393)
/// when it sent one. The operator did NOT name this target, so an outcome line that simply
/// mentions it — "switched: primary → spare", or worse "refusing to swap to `spare`" — reads as a
/// non-sequitur; this line is what makes both readable. Deliberately the SAME rationale wording
/// `cli::render_next_swap` puts in the `status` footer, so the account `status` predicts and the
/// account `--next` takes are visibly the same decision. Names only the non-secret handle (issue
/// #15); a pre-#393 daemon sends no reason and gets the bare label, the honest fallback.
fn note_next_target(label: &str, reason: Option<NextSwapReason>) -> String {
    let why = match reason {
        Some(NextSwapReason::SoonestReset { .. }) => " (weekly resets soonest)",
        Some(NextSwapReason::OnlyCandidate) => " (only viable target)",
        Some(NextSwapReason::RosterOrder) => " (first eligible; no reset times known)",
        None => "",
    };
    format!("note: --next advancing to `{label}`{why}")
}

/// Resolve `use --next`'s target (issue #960) from the daemon's PUBLISHED `next_swap`, and tell
/// the operator which account that turned out to be ([`note_next_target`]) before the swap runs.
///
/// This READS the daemon's choice; it never re-derives one. `NextSwap::Target`'s own doc comment
/// is the reason: the pick is DAEMON-AUTHORITATIVE, because the session trigger and floor
/// `pick_target` consumes are daemon-only and never on the wire — so a client-side
/// re-implementation could not match it and would silently diverge from the candidate `status`
/// shows. Every non-`Target` outcome fails CLOSED with ZERO writes: `--next` never guesses.
async fn resolve_next_target(socket: &Path) -> Result<String> {
    let response = query_next_swap(socket).await?;
    let (label, reason) = next_swap_target(response.next_swap.as_ref(), now_epoch())?;
    eprintln!("{}", note_next_target(&label, reason));
    Ok(label)
}

/// One `status` request/reply over the control socket for [`resolve_next_target`] (issue #960),
/// bounded by [`CONTROL_SOCKET_TIMEOUT`] so a wedged daemon can never hang `use`.
///
/// Unlike its sibling [`ControlSocketCache::query_status`] — whose caller degrades EVERY failure
/// to a cache MISS and falls back to a live poll — `--next` has no fallback to degrade to, so the
/// failures are split by what the operator can actually do about them. A refused / absent socket
/// is "no daemon" ([`Error::UseNextRequiresDaemon`]: start one, or name a target). Anything else —
/// a mid-exchange I/O error, a timeout after connecting, or a reply this build cannot decode
/// (including a daemon whose contract MAJOR has moved on, issue #164) — is a reached-but-unusable
/// daemon ([`Error::UseNextUnresolved`]: retry, or name a target). Both fail closed.
async fn query_next_swap(socket: &Path) -> Result<StatusResponse> {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt};

    let exchange = async {
        let stream = tokio::net::UnixStream::connect(socket)
            .await
            .map_err(|err| {
                match err.kind() {
                    // No socket file, or a stale one with no listener → no live daemon.
                    std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused => {
                        Error::UseNextRequiresDaemon
                    }
                    _ => Error::UseNextUnresolved {
                        detail: "the daemon's control socket could not be reached".to_owned(),
                    },
                }
            })?;
        let mut buffered = tokio::io::BufReader::new(stream);
        buffered.write_all(b"{\"cmd\":\"status\"}\n").await?;
        buffered.flush().await?;
        let mut line = String::new();
        buffered.read_line(&mut line).await?;
        serde_json::from_str::<StatusResponse>(line.trim_end()).map_err(|_| {
            Error::UseNextUnresolved {
                detail: "the daemon's status reply could not be read".to_owned(),
            }
        })
    };
    match tokio::time::timeout(CONTROL_SOCKET_TIMEOUT, exchange).await {
        Ok(result) => result,
        Err(_) => Err(Error::UseNextUnresolved {
            detail: "the daemon did not answer in time".to_owned(),
        }),
    }
}

/// Current wall-clock time as epoch seconds, for humanizing the #405 relief instant. A pre-1970
/// clock degrades to `0` rather than panicking — the same tolerant projection
/// [`crate::observability`] and the `status` client's own `now_epoch` use.
fn now_epoch() -> i64 {
    SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs() as i64)
        .unwrap_or(0)
}

/// The target `use --next` should swap to, plus the daemon's rationale for it — or the typed
/// [`Error`] explaining why there isn't one (issue #960). Pure over the wire value and `now`, so
/// every branch is unit-tested without a socket or a clock, the same shape as its siblings
/// [`ack_falls_back_to_standalone`] and [`swap_rejection_error`].
///
/// The four wire cases split across exactly two error classes, and the split is the exit code:
///   - [`NextSwap::Target`] — the daemon picked one. The rationale rides along for the note.
///   - [`NextSwap::NoViableTarget`] — the daemon RAN its selection and refused: `pick_target`
///     excluded every account. A genuine gate refusal ⇒ exit `7`, carrying the #405 relief hint
///     verbatim (WHY the fleet is blocked and WHEN capacity returns) rather than throwing it away.
///     A pre-#405 daemon sends no `cause` and degrades to the bare "no viable target" that the
///     `status` footer also falls back to — say only what the wire substantiates. The hint itself
///     is composed by [`crate::cli::out_of_capacity_phrase`], shared with that footer.
///   - [`NextSwap::AwaitingData`] — the staggered poll loop (#80) has not read the rotation yet.
///   - `None` — no candidate published at all: a current daemon with no active account to anchor a
///     swap FROM, or (via `#[serde(default)]`) a pre-#88 daemon that omits the field.
///
/// The last two are an inability to RUN the selection, NOT a refusal by it, so they take the
/// generic exit `1` — the same distinction [`Error::UseViabilityUnverifiable`] draws for the
/// un-runnable viability gate, and the reason they are not folded into the exit-`7` class.
fn next_swap_target(
    next_swap: Option<&NextSwap>,
    now: i64,
) -> Result<(String, Option<NextSwapReason>)> {
    match next_swap {
        Some(NextSwap::Target { to, reason }) => Ok((to.clone(), *reason)),
        Some(NextSwap::NoViableTarget { cause, resets_at }) => Err(Error::UseNextNoViableTarget {
            detail: match cause {
                None => "no viable target".to_owned(),
                Some(_) => crate::cli::out_of_capacity_phrase(*resets_at, now),
            },
        }),
        Some(NextSwap::AwaitingData) => Err(Error::UseNextUnresolved {
            detail: "the daemon has not polled the rotation yet".to_owned(),
        }),
        None => Err(Error::UseNextUnresolved {
            detail: "the daemon published no next-swap candidate".to_owned(),
        }),
    }
}

/// Whether a reachable daemon's `swap` ack should FALL THROUGH to the standalone write path
/// (issue #167 / #212 recovery) rather than be reported as the final outcome. TRUE for EXACTLY one
/// ack — the [`SwapRejection::NoActiveAccount`] rejection — and FALSE for every other ack (a
/// completed / already-active swap, or ANY of the other six rejections). Pure, so the discriminator
/// is unit-testable apart from the socket I/O and seam wiring.
///
/// Why ONLY this one: `NoActiveAccount` is a VERDICT-time reject — the daemon has no active account
/// to swap away from (its canonical was scrubbed, e.g. a forced logout), so it rejects BEFORE the
/// swap engine runs. It is therefore a GUARANTEED zero-write: the daemon performed nothing, so
/// falling back to the standalone adopt-target recovery (#212) — the operator-directed
/// adopt-a-named-spare path the daemon does not run over this channel — can never double-write.
/// The fallback is safe against the daemon's own autonomous reconcile because the standalone adopt
/// acquires the SAME cross-process swap lock (`paths::swap_lock`) that EVERY daemon canonical write
/// holds (the auto / emergency / socket swaps via `swap_locked`, and the #282 `promote_canonical`):
/// the two serialize, and a contended acquire fails closed ([`Error::SwapLockBusy`], zero writes) —
/// never a torn or double write. A reached-but-FAILED exchange never reaches this predicate: it
/// surfaced as `Err` from `request_swap` (propagated by `?` before the match), never a silent
/// standalone retry — the daemon may already have written, so retrying could double-write.
fn ack_falls_back_to_standalone(ack: &SwapAck) -> bool {
    matches!(
        ack,
        SwapAck::Rejected {
            reason: SwapRejection::NoActiveAccount,
        }
    )
}

/// Report the daemon's redacted `swap` ack (issue #167) to the operator: print the standard
/// confirmation for a completed / already-active swap (the SAME lines the standalone path prints,
/// from non-secret labels), or map a rejection to the typed [`Error`] whose `exit_code` the
/// standalone path would have produced — so routing THROUGH the daemon leaves `use`'s stdout and
/// exit codes unchanged. Pure but for the confirmation print, so the ack→outcome mapping is
/// unit-testable. NOTE: the `NoActiveAccount` rejection never reaches here on the wired path — the
/// caller ([`use_account`]) intercepts it via [`ack_falls_back_to_standalone`] and falls through to
/// the standalone adopt recovery — but the arm is retained (and directly tested) for completeness.
fn report_swap_ack(ack: SwapAck, query: &str) -> Result<()> {
    match ack {
        SwapAck::Accepted { from, to } => {
            println!("{}", swap_confirmation(&from, &to));
            Ok(())
        }
        SwapAck::AlreadyActive { to } => {
            println!("{}", already_active_confirmation(&to));
            Ok(())
        }
        SwapAck::Rejected { reason } => Err(swap_rejection_error(reason, query)),
    }
}

/// Map a redacted [`SwapRejection`] (issue #167) to the typed [`Error`] the standalone `use` path
/// raises for the same condition, so the daemon-routed path shares `use`'s exit-code taxonomy: a
/// dead / exhausted / cooled-down target is the gate-refused exit `7`, a locked keychain / contended
/// swap lock the retry-shortly exit `4`, an unknown / ambiguous handle exits `5` / `6`, and the
/// no-active + generic-failure cases the generic exit `1`. The rejection carries no label (redaction
/// #15), so `query` names the target in the message instead. `query` is non-secret on BOTH paths
/// that reach here: the operator's own argv for a named target, and — since issue #960's `--next` —
/// the roster label the daemon itself published on `next_swap`, which [`crate::daemon::NextSwap`]
/// is non-secret by construction (a label or a bare reason, never a token or email).
fn swap_rejection_error(reason: SwapRejection, query: &str) -> Error {
    match reason {
        SwapRejection::UnknownTarget => Error::UseTargetNotFound {
            query: query.to_owned(),
        },
        // The daemon does not surface the duplicate count over the wire; `2` is the minimum a
        // "duplicated label" ambiguity implies — enough for the exit-`6` message.
        SwapRejection::AmbiguousTarget => Error::UseTargetAmbiguous {
            query: query.to_owned(),
            count: 2,
        },
        SwapRejection::Quarantined => Error::UseTargetQuarantined {
            label: query.to_owned(),
        },
        SwapRejection::WeeklyExhausted => Error::UseTargetWeeklyExhausted {
            label: query.to_owned(),
        },
        SwapRejection::Cooldown => Error::UseCooldownActive,
        SwapRejection::NoActiveAccount => Error::ActiveAccountUnresolved,
        // `op: "read"` (NOT "write") to match the standalone path byte-for-byte: the locked-keychain
        // abort is the swap engine's step-1 READ (`keychain.rs` read → `Error::KeychainLocked { op:
        // "read" }`), and it aborts BEFORE any write — so "…during read" is both accurate (nothing
        // was written) and identical to the message the daemon-down path prints for this condition.
        SwapRejection::KeychainLocked => Error::KeychainLocked { op: "read" },
        SwapRejection::SwapLockBusy => Error::SwapLockBusy,
        SwapRejection::Failed => Error::DaemonSwapFailed,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::cell::Cell;
    use std::path::PathBuf;

    use crate::claude_state::OauthAccount;
    use crate::config::Tunables;
    use crate::keychain::{Credential, FakeCredentialStore};
    use crate::stash::{FakeAccountStash, StashedAccount};
    use crate::usage::{PolledReading, Usage};

    // --- fakes + fixtures ---------------------------------------------------

    /// One scripted target-poll outcome — reconstructed per call (so the fake needs
    /// no `Clone` of the un-`Clone`-able `Error`), and a call counter so a test can
    /// assert a one-shot command never busy-spins.
    #[derive(Clone, Copy)]
    enum Probe {
        /// A live poll whose weekly fraction is the payload (below trigger ⇒ viable,
        /// at/above ⇒ weekly-exhausted).
        Live { weekly: f64 },
        /// A dead credential — `401` (rejected) ⇒ quarantined / needs re-login.
        Dead,
        /// A dead credential — `403` (missing usage scope) ⇒ also quarantined.
        ScopeMissing,
        /// A transient failure (server / network) — not a viability verdict.
        Transient,
        /// A `429` rate-limit. On the gated live fallback issue #75 remaps it to the
        /// distinct [`Error::UseViabilityUnverifiable`]; `--force` tolerates it
        /// best-effort.
        RateLimited,
        /// The keychain is locked (the always-enforced safety abort).
        Locked,
    }

    struct FakePoller {
        probe: Probe,
        calls: Cell<u32>,
    }

    impl FakePoller {
        fn new(probe: Probe) -> Self {
            Self {
                probe,
                calls: Cell::new(0),
            }
        }
    }

    impl RosterPoller for FakePoller {
        async fn poll(&self, _account: &Account, _active: bool) -> Result<PolledReading> {
            self.calls.set(self.calls.get() + 1);
            match self.probe {
                Probe::Live { weekly } => Ok(PolledReading {
                    usage: Usage {
                        session: 0.10,
                        weekly,
                        weekly_resets_at: None,
                        session_resets_at: None,
                    },
                    severity: None,
                }),
                Probe::Dead => Err(Error::UsageUnauthorized),
                Probe::ScopeMissing => Err(Error::UsageScopeMissing),
                Probe::Transient => Err(Error::UsageTransient {
                    status: 503,
                    retry_after: None,
                }),
                Probe::RateLimited => Err(Error::UsageRateLimited {
                    status: 429,
                    retry_after: None,
                }),
                Probe::Locked => Err(Error::KeychainLocked { op: "read" }),
            }
        }
    }

    /// A scripted [`CachedViabilitySource`] for the cache-vs-live tests (issue #75):
    /// a cache HIT returns a fixed verdict; a cache MISS returns `None` (→ the
    /// caller's single live fallback poll). Counts calls so a test can assert the
    /// gate consulted the cache.
    struct FakeCache {
        verdict: Option<Viability>,
        calls: Cell<u32>,
    }

    impl FakeCache {
        /// A cache MISS — the daemon-down / no-cached-reading case → live fallback.
        /// The default for the gate tests that predate the cache.
        fn miss() -> Self {
            Self {
                verdict: None,
                calls: Cell::new(0),
            }
        }
        /// A cache HIT carrying `verdict` — a running daemon with a usable reading.
        fn hit(verdict: Viability) -> Self {
            Self {
                verdict: Some(verdict),
                calls: Cell::new(0),
            }
        }
    }

    impl CachedViabilitySource for FakeCache {
        async fn cached_viability(
            &self,
            roster: &[Account],
            account: &Account,
        ) -> Option<Viability> {
            // The VERDICT is scripted; the ROSTER is not ignored (issue #1246). Every
            // production call site threads `&config.roster` beside a target taken FROM
            // that roster, so the account is always a bearer — the same by-construction
            // guarantee [`cached_viability_for`]'s roster count leans on ("Zero bearers
            // is unreachable from either production call site"), checked here by uuid
            // identity rather than by that comment's label bearership. A seam that
            // discarded this parameter is what let the roster go unpinned at two of the
            // three sites that thread it.
            //
            // NECESSARY, not sufficient: this fires on an EMPTY or target-omitting roster
            // at ANY site, in EVERY test that installs this fake, and that is its whole
            // value — it is a standing floor under new call sites. It cannot see a roster
            // that contains the target and is still the wrong one; only the
            // `ControlSocketCache`-over-a-real-socket tests pin the lookup's semantics,
            // per site.
            assert!(
                roster
                    .iter()
                    .any(|bearer| bearer.account_uuid == account.account_uuid),
                "the cache was handed a roster with no bearer for `{}` ({}): the caller \
                 threaded a roster the target is not in",
                account.label,
                account.account_uuid,
            );
            self.calls.set(self.calls.get() + 1);
            self.verdict
        }
    }

    /// A recording [`ManualSwapNotifier`] for the manual-hold tests (#64): counts
    /// `notify` calls and can be made to FAIL, proving the best-effort contract —
    /// a failed notify is non-fatal, so `use` still exits success.
    struct FakeNotifier {
        calls: Cell<u32>,
        fail: bool,
    }

    impl FakeNotifier {
        fn ok() -> Self {
            Self {
                calls: Cell::new(0),
                fail: false,
            }
        }
        fn failing() -> Self {
            Self {
                calls: Cell::new(0),
                fail: true,
            }
        }
    }

    impl ManualSwapNotifier for FakeNotifier {
        async fn notify(&self) -> Result<()> {
            self.calls.set(self.calls.get() + 1);
            if self.fail {
                // The "no daemon listening" case — best-effort delivery's expected
                // failure, which `run_use` logs and ignores.
                Err(Error::DaemonNotRunning)
            } else {
                Ok(())
            }
        }
    }

    fn acct(label: &str, uuid: &str) -> Account {
        Account {
            account_uuid: uuid.to_owned(),
            label: label.to_owned(),
            enabled: true,
        }
    }

    fn oauth(uuid: &str) -> OauthAccount {
        OauthAccount::from_object_bytes(
            format!(r#"{{"accountUuid":"{uuid}","emailAddress":"{uuid}@example.com"}}"#).as_bytes(),
        )
        .unwrap()
    }

    fn cred(blob: &[u8]) -> Credential {
        Credential::new(blob.to_vec())
    }

    fn stashed(token: &[u8], uuid: &str) -> StashedAccount {
        StashedAccount {
            credential: cred(token),
            oauth_account: oauth(uuid),
        }
    }

    /// A two-account config: `work` (uuid `u-A`) and `spare` (uuid `u-B`), default
    /// tunables (weekly_ceiling 98 ⇒ 0.98, cooldown 60).
    fn config_ab() -> Config {
        Config {
            roster: vec![acct("work", "u-A"), acct("spare", "u-B")],
            tunables: Tunables::default(),
            refresh: crate::config::RefreshConfig::default(),
            login: crate::config::LoginConfig::default(),
            stats: crate::config::StatsConfig::default(),
            migration: crate::config::MigrationConfig::default(),
            credential: crate::config::CredentialConfig::default(),
        }
    }

    /// A `~/.claude.json` displaying `active_uuid`, returned with its tempdir guard.
    fn claude_json_for(active_uuid: &str) -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".claude.json");
        std::fs::write(
            &path,
            format!(
                r#"{{"numStartups":1,"oauthAccount":{{"accountUuid":"{active_uuid}","emailAddress":"{active_uuid}@x.com"}}}}"#
            ),
        )
        .unwrap();
        (dir, path)
    }

    /// The canonical store seeded with the active account's token, plus a stash
    /// holding BOTH accounts (so the swap can re-stash A and read B).
    async fn seeded_store_and_stash() -> (FakeCredentialStore, FakeAccountStash) {
        let store = FakeCredentialStore::empty();
        store.write(&cred(b"A-token")).await.unwrap();
        let stash = FakeAccountStash::empty();
        stash
            .write("Sessiometer/u-A", &stashed(b"A-token", "u-A"))
            .await
            .unwrap();
        stash
            .write("Sessiometer/u-B", &stashed(b"B-token", "u-B"))
            .await
            .unwrap();
        (store, stash)
    }

    /// Run `use spare` (uuid `u-B`) against a fresh fixture: active = `work` (`u-A`),
    /// with a caller-supplied `cache` seam. Returns the result, the store, the stash,
    /// the LIVE-poll call count, and the log's text — everything a test needs to
    /// assert the swap (or its absence) AND whether the gate fell back to a live poll.
    async fn run_with(
        cache: &FakeCache,
        query: &str,
        force: bool,
        in_cooldown: bool,
        probe: Probe,
    ) -> (
        Result<()>,
        FakeCredentialStore,
        FakeAccountStash,
        u32,
        String,
    ) {
        run_on(config_ab(), cache, query, force, in_cooldown, probe).await
    }

    /// [`run_with`] over a caller-supplied `config` rather than the shared [`config_ab`]
    /// fixture — for the cases whose subject IS the roster's shape (a duplicated label,
    /// issue #1087), which the fixture cannot express.
    ///
    /// Generic over the cache seam rather than fixed to [`FakeCache`], so a test can drive
    /// this whole path with the PRODUCTION [`ControlSocketCache`] over a real socket — which
    /// is what issue #1201 needed and could not have: a scripted verdict proves what the gate
    /// does with an answer, never which answer the real lookup returns for which account.
    async fn run_on<R: CachedViabilitySource>(
        config: Config,
        cache: &R,
        query: &str,
        force: bool,
        in_cooldown: bool,
        probe: Probe,
    ) -> (
        Result<()>,
        FakeCredentialStore,
        FakeAccountStash,
        u32,
        String,
    ) {
        let (store, stash) = seeded_store_and_stash().await;
        let (_json_dir, json) = claude_json_for("u-A");
        let log_dir = tempfile::tempdir().unwrap();
        let log_path = log_dir.path().join("sessiometer.log");
        let mut log = EventLog::at(&log_path).unwrap();
        let poller = FakePoller::new(probe);
        // A throwaway, uncontended swap lock (#64): acquires instantly, so the
        // helper exercises the same locked path as production without contention.
        let lock_dir = tempfile::tempdir().unwrap();
        let lock_path = lock_dir.path().join("swap.lock");
        let notifier = FakeNotifier::ok();

        let result = run_use(
            &config,
            query,
            force,
            in_cooldown,
            Seams {
                cache,
                poller: &poller,
                store: &store,
                stash: &stash,
                claude_json: &json,
                lock_path: &lock_path,
                notifier: &notifier,
            },
            &mut log,
        )
        .await;

        let log_text = std::fs::read_to_string(&log_path).unwrap_or_default();
        (result, store, stash, poller.calls.get(), log_text)
    }

    /// `use` with NO usable cached reading — the daemon-down path that falls back to
    /// a single live poll (today's behaviour). The default for the gate tests that
    /// predate the cache (issue #75): they assert the LIVE-poll path unchanged.
    async fn run(
        query: &str,
        force: bool,
        in_cooldown: bool,
        probe: Probe,
    ) -> (
        Result<()>,
        FakeCredentialStore,
        FakeAccountStash,
        u32,
        String,
    ) {
        run_with(&FakeCache::miss(), query, force, in_cooldown, probe).await
    }

    /// `use` with a daemon-CACHED `cached` verdict (issue #75). The `probe` is the
    /// poller the gate must NOT consult on a cache hit — these tests pass the poison
    /// [`Probe::Locked`] (which would abort if wrongly polled) and assert the
    /// live-poll count is `0`, proving the swap used the cached reading alone.
    async fn run_with_cache(
        cached: Viability,
        query: &str,
        force: bool,
        in_cooldown: bool,
        probe: Probe,
    ) -> (
        Result<()>,
        FakeCredentialStore,
        FakeAccountStash,
        u32,
        String,
    ) {
        run_with(&FakeCache::hit(cached), query, force, in_cooldown, probe).await
    }

    /// The canonical credential's current blob (the active reroute target).
    async fn canonical(store: &FakeCredentialStore) -> Vec<u8> {
        store.read().await.unwrap().expose().to_vec()
    }

    // --- resolve_target (pure): label OR uuid, never guesses (#17) -----------

    #[test]
    fn resolve_target_matches_by_label_or_account_uuid() {
        let roster = [acct("work", "u-A"), acct("spare", "u-B")];
        assert_eq!(resolve_target(&roster, "spare").unwrap(), 1);
        assert_eq!(resolve_target(&roster, "u-A").unwrap(), 0);
    }

    #[test]
    fn resolve_target_reports_not_found_for_an_unmatched_query() {
        let roster = [acct("work", "u-A")];
        let err = resolve_target(&roster, "ghost").unwrap_err();
        assert!(
            matches!(err, Error::UseTargetNotFound { ref query } if query == "ghost"),
            "got {err:?}"
        );
    }

    #[test]
    fn resolve_target_reports_ambiguous_for_a_duplicated_label_and_never_guesses() {
        // Labels are operator handles; uniqueness is not enforced. A query that
        // matches two accounts is ambiguous — the resolver refuses to guess (#17).
        let roster = [
            acct("dup", "u-A"),
            acct("dup", "u-B"),
            acct("unique", "u-C"),
        ];
        let err = resolve_target(&roster, "dup").unwrap_err();
        assert!(
            matches!(err, Error::UseTargetAmbiguous { count: 2, ref query } if query == "dup"),
            "got {err:?}"
        );
    }

    #[test]
    fn resolve_target_counts_an_account_once_when_both_fields_match() {
        // An account whose label AND account-uuid both equal the query is ONE match,
        // not a spurious "ambiguous: 2" — each account is counted once.
        let roster = [acct("dup", "dup"), acct("other", "u-O")];
        assert_eq!(resolve_target(&roster, "dup").unwrap(), 0);
    }

    // --- the label-resolution completeness tripwire (issue #1186) ------------

    /// Every `.rs` file under `src/`, read from disk at test time rather than embedded with
    /// `include_str!`.
    ///
    /// `include_str!` is what [`crate::cli`]'s `INLINE_PROSE_REGISTER` uses, and it is the wrong
    /// tool one scope up: that gate scans ONE file it can name, while this one's whole claim is
    /// that it scanned every file there IS. A literal path list is a list that drifts — a new
    /// module would simply not be scanned, and the gate would stay green while its subject grew.
    /// Reading the tree closes that by construction. It also keeps ~6.5 MB of source out of the
    /// test binary.
    ///
    /// The cost is a dependency on the working directory, which `cargo test` sets to the crate
    /// root. That is not assumed — [`every_handle_read_is_dispositioned`] pins the file count and
    /// the read total, so a walk that finds nothing (or half a tree) FAILS rather than reporting
    /// an empty population as a clean run.
    fn crate_sources() -> Vec<(String, String)> {
        fn walk(dir: &std::path::Path, out: &mut Vec<(String, String)>) {
            let mut entries: Vec<_> = std::fs::read_dir(dir)
                .unwrap_or_else(|e| panic!("cannot read {}: {e}", dir.display()))
                .map(|e| e.expect("a readable dir entry").path())
                .collect();
            entries.sort();
            for path in entries {
                if path.is_dir() {
                    walk(&path, out);
                } else if path.extension().is_some_and(|e| e == "rs") {
                    let text = std::fs::read_to_string(&path)
                        .unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()));
                    out.push((path.display().to_string(), text));
                }
            }
        }
        let mut out = Vec::new();
        walk(std::path::Path::new("src"), &mut out);
        out
    }

    /// The non-test region of `source` — everything above the file's `#[cfg(test)] mod … {` line.
    ///
    /// The boundary is `#[cfg(test)]` IMMEDIATELY followed by a column-0 `mod` that OPENS A
    /// BLOCK rather than declaring one, and the second half is load-bearing rather than
    /// defensive. `INLINE_PROSE_REGISTER`'s lexer cuts at the
    /// first line *starting with* `#[cfg(test)]`, which is correct for `src/cli.rs` and wrong
    /// here: this very file carries a column-0 `#[cfg(test)]` on a test-only `use` up in its
    /// import block, far above [`resolve_target`]. Under that rule this gate would stop at that
    /// import, never reach the resolver it exists to protect, and report green forever.
    /// [`the_non_test_boundary_survives_a_cfg_test_import`] pins it against exactly that.
    ///
    /// The `mod` half must sit at column 0, which is a deliberate BIAS rather than an oversight.
    /// Accepting an INDENTED `mod` would cut at the first nested test block in any future file,
    /// discarding the production code below it. Between over-scanning and under-scanning, this
    /// gate takes over-scanning every time: a stray test helper costs ONE register line and says
    /// so loudly, while a truncated scan is a green run over a subject that was never read.
    ///
    /// **Seven of the fifty-nine files under `src/` therefore carry no boundary at all and are
    /// scanned whole** — measured, not assumed, and worth stating because the bias's cost is
    /// exactly what those files' TEST-ONLY regions contribute, read as production. Today that is
    /// nothing: `config/test_support.rs` (entirely test-only), `redaction.rs`'s `mod meter`,
    /// `main.rs`'s declared test modules and `socket.rs`'s `#[cfg(test)]` helpers hold no
    /// identity-field read between them. Their PRODUCTION reads are a separate matter and are not
    /// part of the over-scan — three of the seven carry one (`daemon/run_loop.rs:69`,
    /// `daemon/snapshot.rs:991`, `daemon/socket.rs:835`, that last being why `serve_control` sits
    /// in [`HANDLE_READ_REGISTER`]) and each is dispositioned like any other. Re-check with
    /// `git grep -nE '\.(label|account_uuid)\b' -- <the seven>`; through PR #1198 this paragraph
    /// asserted the stronger and false "none of the seven contributes a read", which the register
    /// two screens below already contradicted (issue #1203). Three
    /// classes: four files have no `#[cfg(test)]` whatsoever, their suites living in a sibling
    /// (`config/test_support.rs`, `daemon/peer_auth.rs`, `daemon/run_loop.rs`,
    /// `daemon/snapshot.rs` — the last two are covered from `daemon.rs` and `snapshot_build.rs`);
    /// `daemon/socket.rs` carries `#[cfg(test)]` helpers but no test module; and two are cut by
    /// a spelling rather than by a nesting — `main.rs` DECLARES its test modules
    /// (`mod cross_surface;`, per the trailing-`{` rule below), and `redaction.rs` opens its
    /// test-only block as `pub(crate) mod meter {`, which does not start with `mod `.
    ///
    /// The trailing-`{` requirement is that same bias applied to the shape that actually shipped
    /// broken (issue #1197). A test module can be DECLARED rather than opened — `#[cfg(test)]` /
    /// `mod test_support;`, pointing at a whole file of helpers elsewhere — and that declaration
    /// satisfies "next line starts with `mod `" exactly as `mod tests {` does while enclosing
    /// nothing at all. `src/config.rs` carries one at its line 57, so the unqualified rule cut
    /// that file to its first 56 lines and threw away `struct Account` and `impl Account` below
    /// them: the type this gate is named for, reporting green over 3% of it. `src/main.rs` was
    /// cut the same way, inertly — 23 of its 88 lines, no reads either side.
    ///
    /// The predicate demands an OPENING BRACE rather than merely rejecting a `;`, and the
    /// difference is not pedantic: `mod test_support; // helpers live elsewhere` ends with neither,
    /// so a rule phrased as "does not end with `;`" restores the whole defect through a COMMENT —
    /// and `src/config.rs:53` already carries one two lines above that very block. Demanding the
    /// brace fails toward over-scan on every unrecognized shape, which is the bias above.
    /// [`the_non_test_boundary_survives_a_cfg_test_import`] pins both arms, against synthetic
    /// fixtures and against `src/config.rs` itself.
    fn non_test_region(source: &str) -> String {
        let lines: Vec<&str> = source.lines().collect();
        let end = (0..lines.len())
            .find(|&i| {
                lines[i].starts_with("#[cfg(test)]")
                    && lines
                        .get(i + 1)
                        .is_some_and(|n| n.starts_with("mod ") && n.trim_end().ends_with('{'))
            })
            .unwrap_or(lines.len());
        lines[..end].join("\n")
    }

    /// Every read of an [`Account`] identity field in `source`'s non-test code, as
    /// `(enclosing function, field)`.
    ///
    /// The subject is the field READ, and that is the irreducible one: `label` and `account_uuid`
    /// are plain `pub(crate)` fields with no accessors, so code that resolves an operator's handle
    /// MUST read one.
    ///
    /// **The SPELLING of that read is not irreducible, and this paragraph claimed it was** until
    /// issue #1202. Rust reads a field without ever writing a `.`, by binding it in a pattern, and
    /// [`identity_field_at`] matches the `.`-access token alone. Measured against the real
    /// instrument, `let crate::config::Account { label, .. } = account;` inside a first-match-wins
    /// `position` closure passed the gate — a seventh label-resolving site landing silent, which
    /// is the whole of what issue #1186 opened this gate to prevent. The inference "must read one,
    /// therefore matching the read is enough" was sound; it just was not a statement about the
    /// token being matched. [`braced_binding_at`] is the second spelling, and the two arms
    /// together are what the sentence above now describes.
    ///
    /// Two narrower subjects were measured against the tree and both are refuted:
    ///
    /// - **Only reads next to a comparison** (`==`, `.eq`) misses a `let` hoist, and such hoists
    ///   already ship — including inside `cli::apply_enabled` (`let label = account.label.clone()`),
    ///   which is one of the six sites.
    ///   This is the same refutation `INLINE_PROSE_REGISTER` records for narrowing by emitting
    ///   position, and it fails here for the same reason: a one-line hoist defeats it anywhere.
    /// - **Only functions naming the roster** (`Account` in the signature, or `roster` in the
    ///   body) drops a resolver that reaches its accounts through a differently-named field.
    ///
    /// A read reached through a METHOD (`.account_uuid()`) is deliberately NOT a match: that
    /// spelling belongs to the OAuth capture types, not to a roster account, and admitting it
    /// would fill the register with entries no reader can act on. `Account` grows an accessor
    /// only by someone editing `src/config.rs`, where
    /// [`the_identity_fields_stay_plain_fields`] is waiting.
    ///
    /// **The braced-pattern family, enumerated rather than left implicit** (issue #1202's last
    /// acceptance criterion). Every member below is one rule — a field name bound as SHORTHAND
    /// directly inside a braced group — so they are covered together rather than one at a time,
    /// and each is a case in
    /// [`the_handle_read_tripwire_bites_on_a_braced_pattern_binding`]:
    ///
    /// | Spelling | Seen | Why |
    /// |---|---|---|
    /// | `let Account { label, .. } = a` | yes | the issue's probe, and the `let Type { … }` destructures already established in the tree |
    /// | `if let` / `while let` | yes | the same braced group; the leading keyword is not read |
    /// | a `match` arm's struct pattern | yes | `cli::execute` ships one, and it is a register row as of this change |
    /// | a closure parameter | yes | a braced group inside the call's parens |
    /// | a destructuring `fn` parameter | yes | attributed to the function being declared — it binds before the body brace opens, so `handle_reads` reaches for `pending` rather than an open scope |
    /// | `ref` / `mut` prefixes | yes | skipped before the enclosing token is read; `src/capture.rs:432` spells `mut roster` |
    /// | a RENAMED binding (`Account { label: h, .. }`) | **no** | textually identical to a struct DEFINITION's `label: String` and a struct LITERAL's `label: expr`, so admitting it admits every declared field in the crate |
    /// | a tuple-struct pattern (`Account(label, ..)`) | n/a | positional patterns cannot bind a NAMED field, and `Account`'s identity fields are named — [`the_identity_fields_stay_plain_fields`] is what keeps that true |
    ///
    /// The destructures the tree already ships were run through the widened scan rather than
    /// read: every one of them binds other fields (`mut roster`, `tunables`, `size`, `outcome`,
    /// `major` / `minor`, …) and not an identity field, so none of them owes a register row. That
    /// is a measurement with a short shelf life, and it is not what holds — what holds is that a
    /// destructure which starts binding one now REDS [`every_handle_read_is_dispositioned`] like
    /// any other read. The gate is the record; this sentence only says how it was checked.
    ///
    /// The rename is the one open residual, and it is a cheap bypass — cheaper than the `.`-access
    /// one this change closes, since it is a single extra token. It is not closed here because
    /// closing it needs the scanner to tell a pattern from a type ascription, which means lexing
    /// Rust rather than matching a shape: `src/cli.rs`'s `inline_literals` pays that price because
    /// its subject is every literal in a file, and the subject here is one field name. It is
    /// pinned OPEN by the canary above, so a future widening that closes it reds a test and brings
    /// its reader back to this table rather than silently making it false.
    ///
    /// What rides along, deliberately: a struct LITERAL's field-init shorthand is the same tokens
    /// as a pattern (`Self { raw, account_uuid }`), so a WRITE is filed as a read. That is the
    /// over-scan bias [`non_test_region`] states, and it is cheap here — a literal shorthand needs
    /// a local named exactly `label`, which is a handle that came from somewhere.
    ///
    /// One pass's findings. Both populations come out of ONE lexer deliberately: they are
    /// compared against each other (a `ViaSharedResolver` entry must appear in
    /// [`Scan::resolver_callers`]), and two lexers that could disagree about where a function
    /// body starts would let that comparison pass for the wrong reason.
    struct Scan {
        /// `(enclosing function, field)` per identity-field read.
        reads: Vec<(String, String)>,
        /// `(function, definition site)` per DISTINCT reading function — the site being the
        /// index of its opening brace, which is unique per definition even when the name is not.
        ///
        /// A read bound by a destructuring PARAMETER contributes to [`Scan::reads`] but NOT here:
        /// it is matched before its function's body brace exists, and the pattern's own brace is
        /// not a definition site — filing it as one would report a function whose parameter and
        /// body each read an identity field as a two-definition collision. See `handle_reads`.
        ///
        /// [`HANDLE_READ_REGISTER`] keys on the name alone, so two same-named functions that
        /// both read an identity field collapse to one register row and inherit one disposition
        /// — silently, since the name-set comparison still balances. This is what makes that
        /// collision visible; [`every_handle_read_is_dispositioned`] asserts one site per name.
        read_sites: Vec<(String, usize)>,
        /// Every function whose body calls [`resolve_target`].
        resolver_callers: Vec<String>,
        /// Every function holding at least one read that is COMPARED rather than formatted or
        /// forwarded, as `(function, char index of the read)`.
        ///
        /// [`HANDLE_READ_REGISTER`] keys on the function NAME and asks only whether that name
        /// reads an identity field AT ALL, so a name already in it absorbs any further read for
        /// free — including a first-match-wins lookup grown inside a function registered for
        /// forwarding a uuid (issue #1199). This is the second question, asked per read:
        /// [`COMPARING_READERS`] is the set that must declare every name appearing here.
        compared_reads: Vec<(String, usize)>,
    }

    fn handle_reads(source: &str) -> Scan {
        let src: Vec<char> = source.chars().collect();
        let mut out = Vec::new();
        let mut callers: Vec<String> = Vec::new();
        let mut sites: Vec<(String, usize)> = Vec::new();
        let mut compared: Vec<(String, usize)> = Vec::new();
        // Each open function body, with the brace depth it opened at and the index of that brace.
        let mut scopes: Vec<(String, usize, usize)> = Vec::new();
        // Every delimiter currently open, innermost last. `depth` counts braces only and cannot
        // answer the question [`braced_binding_at`] asks — whether a bare `label` sits directly
        // in a BRACED group (`Account { label, .. }`) or in a parenthesised one
        // (`run_login(login, store, stash, existing, label, claude_json)`, `src/capture.rs:762`).
        // Both spell the same three tokens; only the enclosing delimiter tells them apart.
        //
        // That is the INNERMOST delimiter, and it is the answer to that question alone. The two
        // other questions asked of this stack below — which brace opens a pending `fn`'s body,
        // and which `;` ends its declaration — are questions about its DEPTH, and answering
        // either one innermost-first is a bypass: issues #1223 and #1225 respectively.
        let mut delims: Vec<char> = Vec::new();
        let mut pending: Option<String> = None;
        // The delimiter depth `pending` was captured AT, and the answer to both questions the
        // signature poses: its body brace is the one that opens back at that same depth, and the
        // `;` that ENDS its declaration is the one that sits at it. Every brace and every `;`
        // deeper than this belongs to the signature. Captured at the `fn` token, so any
        // delimiter already enclosing the declaration is counted in it.
        let mut pending_depth = 0usize;
        let mut depth = 0usize;
        let mut i = 0usize;

        while i < src.len() {
            // Comments FIRST — this file's doc comments discuss `account.label` in prose, and
            // lexing one as code is the likeliest way to lose the place.
            if src[i] == '/' && src.get(i + 1) == Some(&'/') {
                while i < src.len() && src[i] != '\n' {
                    i += 1;
                }
                continue;
            }
            if src[i] == '/' && src.get(i + 1) == Some(&'*') {
                let mut nesting = 1usize;
                i += 2;
                while i < src.len() && nesting > 0 {
                    if src[i] == '/' && src.get(i + 1) == Some(&'*') {
                        nesting += 1;
                        i += 2;
                    } else if src[i] == '*' && src.get(i + 1) == Some(&'/') {
                        nesting -= 1;
                        i += 2;
                    } else {
                        i += 1;
                    }
                }
                continue;
            }
            if let Some(next) = raw_string_end(&src, i) {
                i = next;
                continue;
            }
            if let Some(next) = quoted_string_end(&src, i) {
                i = next;
                continue;
            }
            if src[i] == '\'' {
                i += char_literal_len(&src, i).unwrap_or(1);
                continue;
            }
            // `fn NAME`, remembered until its opening brace — so a signature broken across lines,
            // or carrying a `where` clause, still binds the body that follows it.
            if src[i] == 'f'
                && src.get(i + 1) == Some(&'n')
                && (i == 0 || !(src[i - 1].is_ascii_alphanumeric() || src[i - 1] == '_'))
                && src.get(i + 2).is_some_and(|c| c.is_whitespace())
            {
                let mut j = i + 2;
                while src.get(j).is_some_and(|c| c.is_whitespace()) {
                    j += 1;
                }
                let start = j;
                while src
                    .get(j)
                    .is_some_and(|c| c.is_ascii_alphanumeric() || *c == '_')
                {
                    j += 1;
                }
                if j > start {
                    pending = Some(src[start..j].iter().collect());
                    pending_depth = delims.len();
                    i = j;
                    continue;
                }
            }
            // A bodiless signature (`fn probe(&self);` in a trait) must not capture the next
            // unrelated block — and only THAT `;` ends a declaration. The one terminating a
            // declaration sits at the delimiter depth the `fn` token was read at; a `;` nested
            // inside the signature (`[u8; N]`, in a parameter or a return type) sits deeper, and
            // clearing on it left the body brace with no `pending` to bind, dropping every read
            // in that body from this gate (issue #1225). Same depth question as the body-brace
            // arm below, asked one token earlier.
            if src[i] == ';' {
                if delims.len() == pending_depth {
                    pending = None;
                }
                i += 1;
                continue;
            }
            if matches!(src[i], '(' | '[') {
                delims.push(src[i]);
                i += 1;
                continue;
            }
            if matches!(src[i], ')' | ']') {
                delims.pop();
                i += 1;
                continue;
            }
            if src[i] == '{' {
                depth += 1;
                // A pending `fn` binds to the brace that opens its BODY, and a brace opened
                // anywhere INSIDE the signature is not that brace — it is a destructuring
                // parameter (`fn park(Account { label, .. }: &Account) { … }`). Binding the
                // scope there opened it at the pattern and closed it at the pattern's `}`,
                // leaving the whole body with no enclosing scope: every read in it was
                // attributed to nothing and dropped. That is a total bypass of this gate, in the
                // same family as the one issue #1202 is about.
                //
                // What tells the two braces apart is DEPTH, not the innermost delimiter: the
                // body brace is the one that opens back at the depth the `fn` token was read at.
                // Asking only what the innermost delimiter is (`delims.last() != Some(&'(')`)
                // answers the question for a FLAT pattern and re-opens the bypass for anything
                // nested inside it, because at the inner `{` of `Wrapper { inner: Account { … } }`
                // the innermost delimiter is `{` rather than `(` (issue #1223). No signature in
                // the tree opens a brace at all today — measured over the non-test regions with
                // an instrument canaried against both shapes, so it is the tree that is clean and
                // not the scan that is broken — which makes both the flat and the nested case
                // latent, and [`the_handle_read_tripwire_bites_on_a_braced_pattern_binding`] is
                // what keeps the arm honest rather than decorative.
                //
                // The third root of the same total bypass — a `;` NESTED in a signature
                // (`[u8; N]`), which cleared `pending` above before this arm was ever consulted
                // — is closed by asking the SAME depth question one token earlier (issue #1225).
                // It was the one of the three the tree actually spelled.
                //
                // What that leaves unreached is not another token. `pending` is written in
                // exactly three places in this function — set at the `fn` above, cleared by that
                // `;` arm, taken here — so there is no sibling clear to widen either guard to.
                // Re-derive with `git grep -nE 'pending = (None|Some)|pending\.take' --
                // src/use_account.rs`, reading past the one prose line that quotes those
                // spellings — the mutation table in the pinning test below — since the grep
                // cannot tell code from a comment about it.
                //
                // Both guards instead share ONE assumption: that a signature leaves `delims`
                // balanced. Within the `fn` grammar it does — a `;` there can only sit inside an
                // array type or a block, and both `[` and `{` are pushed — but `<` and `>` are
                // not tracked at all, so a construct that opens or closes a tracked delimiter
                // where this lexer does not see it puts BOTH depth tests off by the same amount
                // at once, and neither is then a question about `;` or about braces.
                if delims.len() == pending_depth {
                    if let Some(name) = pending.take() {
                        scopes.push((name, depth, i));
                    }
                }
                delims.push('{');
                i += 1;
                continue;
            }
            if src[i] == '}' {
                if scopes.last().is_some_and(|(_, at, _)| *at == depth) {
                    scopes.pop();
                }
                depth = depth.saturating_sub(1);
                delims.pop();
                i += 1;
                continue;
            }
            if let Some(next) = resolver_call_at(&src, i) {
                if let Some((name, _, _)) = scopes.last() {
                    callers.push(name.clone());
                }
                i = next;
                continue;
            }
            let matched = identity_field_at(&src, i).or_else(|| {
                (delims.last() == Some(&'{'))
                    .then(|| braced_binding_at(&src, i))
                    .flatten()
            });
            if let Some((field, next)) = matched {
                // A destructuring PARAMETER binds before its function's body brace opens, so
                // there is no open scope to attribute it to and `pending` — the function being
                // declared — is the honest owner. No SITE is recorded for it: the site is the
                // body brace, which has not been reached, and inventing the pattern brace
                // instead would file a function whose parameter AND body each read one as a
                // two-definition collision that does not exist. What that costs is exactly this:
                // [`MULTI_SITE_READERS`] cannot see a name colliding through parameter bindings
                // ALONE. The name set above still catches such a function, and nothing in the
                // tree spells the shape at all — measured.
                //
                // Deeper than the depth the `fn` was read at, with that `fn` still pending:
                // the match is inside the signature. Depth again rather than the second-innermost
                // delimiter, and for the same reason — a nested pattern puts `{` there, so
                // asking whether `delims[len - 2]` is `(` sees the outer pattern brace and not
                // the parameter list (issue #1223).
                let in_param_pattern = pending.is_some() && delims.len() > pending_depth;
                if let (true, Some(name)) = (in_param_pattern, &pending) {
                    out.push((name.clone(), field));
                } else if let Some((name, _, site)) = scopes.last() {
                    if read_is_compared(&src, i, next) {
                        compared.push((name.clone(), i));
                    }
                    out.push((name.clone(), field));
                    sites.push((name.clone(), *site));
                }
                i = next;
                continue;
            }
            i += 1;
        }
        callers.sort_unstable();
        callers.dedup();
        sites.sort_unstable();
        sites.dedup();
        compared.sort_unstable();
        compared.dedup();
        Scan {
            reads: out,
            read_sites: sites,
            resolver_callers: callers,
            compared_reads: compared,
        }
    }

    /// Whether the identity-field read spanning `at..end` is COMPARED — an operand of `==` or
    /// `!=` — rather than formatted, forwarded or assigned.
    ///
    /// This is the read SHAPE, and it is a second question about a read the name-keyed register
    /// has already absorbed. `refresh` forwards `&account.account_uuid` into the refresher and is
    /// registered for exactly that; the probe in issue #1199 adds
    /// `.position(|a| a.label == "probe")` to that same body, and nothing about the FIRST question
    /// moves — the name set is unchanged, the definition site is unchanged. The shape moves.
    ///
    /// `==` / `!=` rather than a list of search combinators, deliberately. Every `.position` /
    /// `.find` / `.any` / `.filter` over an identity field in this crate's production code
    /// reaches it through a closure that compares it with `==` — measured, not assumed, and
    /// re-checkable with `git grep -nE '\.(position|find|any|filter)\(' -- src` — so the
    /// combinator's name was never what made one visible. The operator is both the narrower
    /// subject and the wider net: it also catches the hand-rolled `for` loop that no list of
    /// combinator names contains.
    ///
    /// What it does NOT see is stated at [`COMPARING_READERS`], with the canary that pins each
    /// residual open rather than leaving it to be discovered.
    fn read_is_compared(src: &[char], at: usize, end: usize) -> bool {
        comparison_follows(src, end) || comparison_precedes(src, at)
    }

    /// `==` / `!=` at `i`, and not `<=` / `>=` (whose second char is the same `=`).
    fn equality_operator_at(src: &[char], i: usize) -> bool {
        matches!(src.get(i), Some('=' | '!')) && src.get(i + 1) == Some(&'=')
    }

    /// `a.label == query`, looking THROUGH the two wrappers that already ship between a read and
    /// its operator: an argument-less postfix conversion (`a.label.as_str() == query`) and a
    /// call the read sits inside (`Some(a.account_uuid.as_str()) == active_uuid`, which is
    /// `poke::is_active` verbatim). A scan that stopped at the first `.clone()` or the first `)`
    /// would file both of those as formatting reads — and `is_active` is registered, in prose, as
    /// a comparison, so the register would have contradicted the measurement on day one.
    fn comparison_follows(src: &[char], end: usize) -> bool {
        let mut i = end;
        loop {
            while src.get(i).is_some_and(|c| c.is_whitespace()) {
                i += 1;
            }
            // Out of a call the read is an argument of. Only a paren opened BEFORE the read can
            // close here: one opened after it is part of the postfix chain below, which is
            // consumed whole.
            if src.get(i) == Some(&')') {
                i += 1;
                continue;
            }
            if src.get(i) != Some(&'.') {
                break;
            }
            let mut j = i + 1;
            while src
                .get(j)
                .is_some_and(|c| c.is_ascii_alphanumeric() || *c == '_')
            {
                j += 1;
            }
            let method: String = src[i + 1..j].iter().collect();
            // The operator written as a method. Nothing in the tree spells it this way today, so
            // this arm carries no register row — it is here because issue #1199 named `.eq` as a
            // comparison shape, and a scan that saw `==` but not `.eq(` would be one token from
            // blind.
            if EQUALITY_METHODS.contains(&method.as_str()) && src.get(j) == Some(&'(') {
                return true;
            }
            if j > i + 1 && src.get(j) == Some(&'(') && src.get(j + 1) == Some(&')') {
                i = j + 2;
                continue;
            }
            break;
        }
        equality_operator_at(src, i)
    }

    /// `==` and `!=` spelled as a method call, which is the same comparison.
    const EQUALITY_METHODS: &[&str] = &["eq", "ne", "eq_ignore_ascii_case"];

    /// `query == &account.label` — the same comparison written the other way round. Walks back
    /// over the receiver path (`self.account`, `a`), an optional borrow, and any call the read is
    /// an argument of (`query == Some(&a.label)`), then looks for the operator.
    fn comparison_precedes(src: &[char], at: usize) -> bool {
        let mut i = at;
        loop {
            while i > 0 && {
                let c = src[i - 1];
                c.is_ascii_alphanumeric() || c == '_' || c == '.' || c == '&'
            } {
                i -= 1;
            }
            if i > 0 && src[i - 1] == '(' {
                i -= 1;
                continue;
            }
            break;
        }
        while i > 0 && src[i - 1].is_whitespace() {
            i -= 1;
        }
        i >= 2 && equality_operator_at(src, i - 2)
    }

    /// A CALL to [`resolve_target`] at `at`, and the index just past its name. The `fn` arm above
    /// consumes the declaration's own name before this runs, so the definition never counts as a
    /// call to itself.
    fn resolver_call_at(src: &[char], at: usize) -> Option<usize> {
        const NAME: &str = "resolve_target";
        if at > 0 && (src[at - 1].is_ascii_alphanumeric() || src[at - 1] == '_') {
            return None;
        }
        let end = at + NAME.chars().count();
        if src.get(at..end)?.iter().collect::<String>() != NAME {
            return None;
        }
        let mut k = end;
        while src.get(k).is_some_and(|c| c.is_whitespace()) {
            k += 1;
        }
        (src.get(k) == Some(&'(')).then_some(end)
    }

    /// `.label` / `.account_uuid` read as a FIELD at `at`, and the index just past it. `None`
    /// when `at` does not begin one, or when the name is followed by `(` — a method call on one
    /// of the OAuth capture types rather than a roster account's field.
    fn identity_field_at(src: &[char], at: usize) -> Option<(String, usize)> {
        if src.get(at) != Some(&'.') {
            return None;
        }
        let mut j = at + 1;
        let start = j;
        while src
            .get(j)
            .is_some_and(|c| c.is_ascii_alphanumeric() || *c == '_')
        {
            j += 1;
        }
        let name: String = src[start..j].iter().collect();
        if name != "label" && name != "account_uuid" {
            return None;
        }
        let mut k = j;
        while src.get(k).is_some_and(|c| c.is_whitespace()) {
            k += 1;
        }
        if src.get(k) == Some(&'(') {
            return None;
        }
        Some((name, j))
    }

    /// `label` / `account_uuid` bound by NAME inside a braced pattern at `at`, and the index just
    /// past it — the spelling that reads the field without ever writing a `.` (issue #1202).
    ///
    /// The caller supplies the half this function cannot see: it runs only when the INNERMOST
    /// open delimiter is `{`. That is the whole discriminator, and it is what makes the rule
    /// below safe to state so loosely. `Account { label, .. }` and `run_login(…, label, …)` spell
    /// the same three tokens; the first sits in a braced group and the second in a parenthesised
    /// one, and `src/capture.rs:237` and `:762` already ship the second in production code this
    /// gate scans — `:762` measured, by removing the test and reading what the register then
    /// refused. `:237` is not reached by that measurement: `label` is last in that call, so it is
    /// followed by `)`, and the CLOSE test below admits only `,` or `}`.
    ///
    /// SHORTHAND only — the name must both OPEN its entry (preceded by `{` or `,`, through an
    /// optional `ref` / `mut`, which `src/capture.rs:432` already spells as `mut roster`) and
    /// CLOSE it (followed by `,` or `}`). That is not a narrowing for its own sake: it is what
    /// keeps a struct DEFINITION out, since a declared field is always `label: Type` and can
    /// never be shorthand. The cost is the renamed binding, recorded as a residual at
    /// [`HANDLE_READ_REGISTER`] and pinned open by
    /// [`the_handle_read_tripwire_bites_on_a_braced_pattern_binding`].
    ///
    /// One rule covers the whole family — `let`, `if let`, `while let`, a `match` arm, a
    /// destructuring `fn` parameter and a closure parameter are all a braced group whose entries
    /// are field names, and none of them is spelled with a `.`. What rides along is the struct
    /// LITERAL's field-init shorthand (`Self { raw, account_uuid }`, `src/claude_state.rs:122`),
    /// which is textually identical to the pattern and stays. Admitting it is the same over-scan
    /// bias [`non_test_region`] takes: a literal shorthand needs a local named exactly `label`,
    /// which is itself a handle that came from somewhere, so the register row it costs is a row
    /// about a real handle rather than noise.
    fn braced_binding_at(src: &[char], at: usize) -> Option<(String, usize)> {
        // A left word boundary, and neither a field access nor a path segment: `.label` is
        // [`identity_field_at`]'s, and `Foo::label` is a path rather than a binding.
        if at > 0 && {
            let prev = src[at - 1];
            prev.is_ascii_alphanumeric() || prev == '_' || prev == '.' || prev == ':'
        } {
            return None;
        }
        let mut j = at;
        while src
            .get(j)
            .is_some_and(|c| c.is_ascii_alphanumeric() || *c == '_')
        {
            j += 1;
        }
        let name: String = src[at..j].iter().collect();
        if name != "label" && name != "account_uuid" {
            return None;
        }
        // It CLOSES its entry — so `label: String` in a struct declaration is not a binding, and
        // neither is a bare `label` used as an expression (`return label;`).
        let mut k = j;
        while src.get(k).is_some_and(|c| c.is_whitespace()) {
            k += 1;
        }
        if !matches!(src.get(k), Some(',' | '}')) {
            return None;
        }
        // …and it OPENS one, through an optional `ref` / `mut`. Without this a block's TAIL
        // EXPRESSION reads as a field binding: in `{ let _ = 1; label }` the name is followed by
        // `}` exactly as a shorthand entry is, and only what PRECEDES it tells the two apart.
        // (A tail expression sitting directly after the `{` is not separated by this test and is
        // admitted — the over-scan the doc comment above accepts.)
        let mut b = at;
        loop {
            while b > 0 && src[b - 1].is_whitespace() {
                b -= 1;
            }
            let mut w = b;
            while w > 0 && (src[w - 1].is_ascii_alphanumeric() || src[w - 1] == '_') {
                w -= 1;
            }
            let word: String = src[w..b].iter().collect();
            if word != "ref" && word != "mut" {
                break;
            }
            b = w;
        }
        if !matches!(src.get(b.checked_sub(1)?), Some('{' | ',')) {
            return None;
        }
        Some((name, j))
    }

    /// The index just past a raw string literal beginning at `at` — every spelling Rust accepts
    /// (the optional `b` / `c` prefix, any hash count).
    fn raw_string_end(src: &[char], at: usize) -> Option<usize> {
        let mut i = at;
        if matches!(src.get(i), Some('b' | 'c')) {
            i += 1;
        }
        if src.get(i) != Some(&'r') {
            return None;
        }
        i += 1;
        let hashes = {
            let start = i;
            while src.get(i) == Some(&'#') {
                i += 1;
            }
            i - start
        };
        if src.get(i) != Some(&'"') {
            return None;
        }
        i += 1;
        while i < src.len() {
            if src[i] == '"' && (1..=hashes).all(|h| src.get(i + h) == Some(&'#')) {
                return Some(i + 1 + hashes);
            }
            i += 1;
        }
        Some(src.len())
    }

    /// The index just past an ordinary quoted string beginning at `at` (the optional `b` / `c`
    /// prefix included), honouring backslash escapes.
    fn quoted_string_end(src: &[char], at: usize) -> Option<usize> {
        let mut i = at;
        if matches!(src.get(i), Some('b' | 'c')) {
            i += 1;
        }
        if src.get(i) != Some(&'"') {
            return None;
        }
        i += 1;
        while i < src.len() {
            match src[i] {
                '\\' => i += 2,
                '"' => return Some(i + 1),
                _ => i += 1,
            }
        }
        Some(src.len())
    }

    /// The length of a char literal at `at`, or `None` when the quote opens a lifetime
    /// (`'a`) instead.
    fn char_literal_len(src: &[char], at: usize) -> Option<usize> {
        if src.get(at) != Some(&'\'') {
            return None;
        }
        if src.get(at + 1) == Some(&'\\') {
            let mut i = at + 2;
            while i < src.len() && src[i] != '\'' {
                i += 1;
            }
            return Some(i + 1 - at);
        }
        if src.get(at + 2) == Some(&'\'') {
            return Some(3);
        }
        None
    }

    /// What a function's reads of an [`Account`] identity field ARE.
    ///
    /// Every arm is a statement about WHERE THE MATCHED STRING CAME FROM, never about editorial
    /// quality — deliberately, and for the reason [`crate::cli`]'s `INLINE_PROSE_REGISTER` gives
    /// for the same choice one scope over. "Is this resolution?" invites an opinion; "did an
    /// OPERATOR supply this string?" is a question about the code, and it is the only question
    /// R-6a actually asks. The daemon matching a poll observation's own account-uuid back to a
    /// roster index is not a weaker form of `use <label>`; it is a different act, and refusing on
    /// ambiguity there would be a bug.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum HandleRead {
        /// It IS the shared resolver.
        SharedResolver,
        /// It resolves an operator-supplied handle, and does so BY CALLING [`resolve_target`].
        /// Mechanically verified — [`every_handle_read_is_dispositioned`] rejects this arm for a
        /// function whose body holds no `resolve_target` call, so the claim cannot be merely
        /// asserted.
        ViaSharedResolver,
        /// It reads an identity field for something that is not operator-handle resolution:
        /// matching an identifier the SYSTEM supplied, capturing a handle off an
        /// already-resolved index for an event or a snapshot, rendering, or a field of a
        /// different type that happens to share the name.
        NotHandleResolution,
    }

    use HandleRead::{NotHandleResolution, SharedResolver, ViaSharedResolver};

    /// Every function in the crate's non-test code that reads an [`Account`] identity field, with
    /// what that read IS and why.
    ///
    /// This is the set-level counterpart to the six per-site refusal tests (issues #1005, #1087),
    /// and it exists because those six cannot cover the property their names imply. A per-site
    /// test proves that THAT site refuses; no number of them can detect a SEVENTH site that grows
    /// its own resolver, because the seventh site does not exist yet to have a test written about
    /// it. That gap is what issue #1186 is about, and it is the whole of what this register
    /// closes.
    ///
    /// **What this gate does NOT do.** It does not prove a `NotHandleResolution` entry is
    /// correctly classified — that is a human reading, recorded with its reason so the next
    /// reader can check it rather than re-derive it. What it mechanically guarantees is narrower
    /// and is the part that matters: a function that reads an identity field and is NOT in this
    /// list reddens the run. A seventh site therefore cannot land silently; its author must
    /// either route it through [`resolve_target`] or write down, here, why the string it matches
    /// is not an operator handle.
    ///
    /// It also does not — and cannot — see a seventh site added INSIDE a function this list
    /// already names. "Does this function read an identity field?" is a question a name answers
    /// once, so every further read a registered name grows is absorbed for free: issue #1199
    /// measured a first-match-wins lookup dropped into `RealPokeEngine::refresh`, and nothing
    /// here moved. [`COMPARING_READERS`] is the second question, asked per read rather than per
    /// name, and it is where the residual that survives BOTH is written down.
    ///
    /// Keyed on the function NAME, so two same-named functions in different modules share one
    /// entry. [`MULTI_SITE_READERS`] is what closes that: every name whose reads come from more
    /// than one definition must be listed there with the reason one disposition covers both, and
    /// [`every_handle_read_is_dispositioned`] compares that list against the measured sites in
    /// each direction. A crate-wide read TOTAL stood in for this until issue #1197 — it moved
    /// when a second same-named function gained a read, but it also moved on every unrelated
    /// commit anywhere in `src/`, so it reported the collision it was for and a hundred things
    /// it was not.
    const HANDLE_READ_REGISTER: &[(&str, HandleRead, &str)] = &[
        // --- the shared resolver, and the sites that read a handle after routing through it ----
        ("resolve_target", SharedResolver, "the one resolver: label OR account-uuid, refusing on zero and on many (#17, OQ-1)"),
        ("apply_enabled", ViaSharedResolver, "`enable` / `disable` — reads the resolved account's label for the confirmation"),
        // `cli::apply_remove` is the sixth site and is deliberately ABSENT: it resolves and then
        // removes, never reading an identity field, so the field scan cannot see it. That is not
        // a hole — it is why this register alone is not the whole gate, and why
        // [`resolve_target_has_exactly_the_five_known_call_sites`] pins the compliant set from
        // the other side.
        ("perform_socket_swap", ViaSharedResolver, "the daemon's control-socket swap — reads the resolved from/to labels for the event"),
        ("poke_named", ViaSharedResolver, "`poke <label>` — reads the resolved account's label for its report line"),
        ("run_use", ViaSharedResolver, "`use <label>` — reads the resolved target's label for the swap confirmation"),
        // --- matched by an identifier the SYSTEM supplied, never an operator handle -------------
        //
        // These DO compare an identity field against something, and every one of them is correct
        // to take the first match: the string is an account-uuid the daemon, the OAuth capture or
        // the config itself produced, and it is unique by construction (`config::validate`
        // rejects a duplicated uuid — only LABELS are un-unique, which is why R-6a is about
        // labels). Refusing on ambiguity here would refuse on an impossible condition.
        ("apply_refresh_observation", NotHandleResolution, "matches a poll observation's own account-uuid back to its roster index"),
        ("apply_refresh_restore", NotHandleResolution, "matches a restore outcome's account-uuid back to its roster index"),
        ("is_active", NotHandleResolution, "compares an account's uuid to the ACTIVE uuid `active.rs` read from the credential"),
        ("overlay_labels", NotHandleResolution, "applies the settings overlay to the account bearing that uuid, and ASSIGNS a label rather than matching one"),
        ("plan_capture", NotHandleResolution, "matches the freshly captured OAuth account's uuid; the label is the name being ASSIGNED to it"),
        ("reconcile_restored", NotHandleResolution, "matches a restored credential's uuid back to its roster index"),
        ("reconcile_roster", NotHandleResolution, "re-indexes the roster across a config reload, by uuid"),
        ("recovery_pending", NotHandleResolution, "membership of an account's uuid in the quarantined / excluded uuid sets"),
        ("resolve_via_display", NotHandleResolution, "matches the DISPLAYED credential's uuid to a roster index — the credential is the input, not the operator"),
        ("restash_account", NotHandleResolution, "compares a roster account's uuid to the displayed credential's before restashing"),
        ("stash", NotHandleResolution, "`Account::stash` DERIVES the keychain key from the uuid — a formatting read with no comparison, so there is nothing here to resolve first-match-wins"),
        ("run_sweep", NotHandleResolution, "the refresh sweep's per-account uuid membership checks, plus the handles its events carry"),
        // --- a MEMBERSHIP test, which takes no match at all ------------------------------------
        //
        // Held out of the SYSTEM-supplied section above, whose reasoning is false here on all
        // three of its clauses (issue #1243): the string an operator typed into
        // `[refresh].accounts` is a handle THEY supplied, not one the daemon, the OAuth capture
        // or the config produced; it may be a `list` LABEL as readily as an account-uuid —
        // `config::RefreshConfig` documents an entry as either — so uniqueness by construction
        // does not hold; and there is no first match to be correct about, because nothing is
        // selected. That last clause is the whole reason this is not resolution: the read folds
        // to a bool rather than an index, so a duplicated label legitimately admits BOTH bearers
        // instead of one of them silently winning. `COMPARING_READERS` asks the second question
        // of the same function and lands in the same place, naming the shape "test membership
        // and select nothing at all"; a reader auditing THIS register alone should not come away
        // with a different answer, which is what filing it above did.
        ("account_listed_in", NotHandleResolution, "the `[refresh].accounts` allowlist membership rule — a config-supplied set, and a duplicated label there legitimately admits BOTH bearers"),
        // --- a handle read off an ALREADY-RESOLVED account or index ----------------------------
        //
        // The index came from the daemon's own scheduler, from an enumeration of the whole
        // roster, or from a resolver call that already happened. Nothing here matches an operator
        // string against anything; the read turns a chosen account into a handle for an event, a
        // snapshot field, or a rendered cell.
        ("active_blind_projection", NotHandleResolution, "keys the blind-projection map by label for `status`"),
        ("apply_import", NotHandleResolution, "the import merge — matches an INCOMING artifact account's uuid, and reports per-entry labels"),
        ("blind_swap", NotHandleResolution, "the blind-swap episode events' account / from / to handles"),
        ("cached_viability", NotHandleResolution, "passes an already-chosen account's label to the wire lookup below"),
        ("canary_status_of", NotHandleResolution, "the canary status' displayed / matched handles"),
        ("decide_action", NotHandleResolution, "the swap-decision events' hold / from / to handles"),
        ("emergency_swap", NotHandleResolution, "the emergency-swap events' hold / from / to handles"),
        ("fold_expiry_observation", NotHandleResolution, "the expiry observation event's account uuid"),
        ("fold_recovery_outcome", NotHandleResolution, "the recovery outcome's account uuid"),
        ("gate_viability", NotHandleResolution, "names the already-chosen target in the viability error"),
        ("gather_auth_subset", NotHandleResolution, "keys the per-account refresh-outcome map by label while rendering `status -v`"),
        ("gather_payload", NotHandleResolution, "collects roster uuids into the export payload"),
        ("keep_active_warm", NotHandleResolution, "seeds the keep-warm stagger from the active account's uuid"),
        ("keep_warm", NotHandleResolution, "passes an already-chosen account's uuid to the keep-warm seam"),
        ("keep_warm_and_promote", NotHandleResolution, "the promotion event's account handle"),
        ("label_at", NotHandleResolution, "renders the handle at a given roster index, or `?`"),
        ("label_bearers", NotHandleResolution, "counts bearers PER label for the import's duplicate notice — the count is the point"),
        ("locked_swap", NotHandleResolution, "the locked-keychain event's displayed / matched handles"),
        ("maintain_stats_store", NotHandleResolution, "keys the per-account stats store by label"),
        ("next_swap", NotHandleResolution, "the next-swap projection's target handle"),
        ("note_account_backoff", NotHandleResolution, "the backoff event's account uuid"),
        ("note_blind_episode", NotHandleResolution, "the blind-episode events' account uuid"),
        ("note_blind_gate_eligibility", NotHandleResolution, "the blind-gate eligibility event's account uuid"),
        ("note_canonical_liveness", NotHandleResolution, "the canonical-liveness event's active handle"),
        ("note_exhausted_poll", NotHandleResolution, "the exhausted-poll event's account uuid"),
        ("note_expiry_horizon_edge", NotHandleResolution, "the expiry-horizon event's account uuid"),
        ("note_health_transitions", NotHandleResolution, "the health-transition events' account handles"),
        ("note_landing_overshoot", NotHandleResolution, "the landing-overshoot event's from handle"),
        ("note_poll_outcome", NotHandleResolution, "the four poll-outcome events' account handles"),
        ("note_refresh_outcome", NotHandleResolution, "the refresh-outcome event's account handle"),
        ("poke_all", NotHandleResolution, "`poke` with no target — sweeps the WHOLE roster, so it resolves nothing"),
        ("reconcile_canonical_change", NotHandleResolution, "the canonical-change events' account handles"),
        ("recover_scrubbed_canonical", NotHandleResolution, "the scrubbed-canonical recovery events' account handles"),
        ("refresh", NotHandleResolution, "passes an already-chosen account's uuid to the refresher (`poke` and the tick each have one)"),
        ("refresh_exclusions", NotHandleResolution, "collects the excluded accounts' uuids"),
        ("refresh_quarantined", NotHandleResolution, "collects the quarantined accounts' uuids"),
        ("refresh_retry", NotHandleResolution, "the refresh-retry event's account handle"),
        ("remove_account", NotHandleResolution, "prints the REMOVED account's label; `apply_remove` did the resolving"),
        ("render", NotHandleResolution, "emits `account_uuid` / `label` back into `config.toml`"),
        ("render_access_token_expiry", NotHandleResolution, "widths and cells for the `-v` access-token table"),
        ("render_roster", NotHandleResolution, "`list`'s label column widths, cells and uuid column"),
        ("reprobe_dead_parked_credential", NotHandleResolution, "the dead-parked reprobe event's account handle"),
        ("resolve_active_uuid", NotHandleResolution, "reads the uuid at the ACTIVE index — the index is the input"),
        ("resolve_active_uuid_for_import", NotHandleResolution, "reads the uuid at the ACTIVE index for the import's adoption check"),
        ("resolve_restore", NotHandleResolution, "hands an already-chosen account's uuid to the restore notifier"),
        ("roster_handles", NotHandleResolution, "collects every label for `stats`"),
        ("run", NotHandleResolution, "collects roster uuids for the migration pre-flight"),
        ("run_capture", NotHandleResolution, "reads the just-captured account's label for its report; found by STASH name"),
        ("run_login", NotHandleResolution, "reads the just-logged-in account's label for its report; found by STASH name"),
        ("snapshot", NotHandleResolution, "the wire snapshot's per-account `label` field and its active handle"),
        ("status_response", NotHandleResolution, "the `status` reply's per-account `label` field"),
        ("tick", NotHandleResolution, "the tick's own events' account handles"),
        ("validate", NotHandleResolution, "config validation — enforces uuid uniqueness and label non-emptiness, and is where a duplicated LABEL is warned about (R-6) rather than resolved"),
        ("velocity_swap", NotHandleResolution, "the velocity-swap event's from / to handles"),
        ("view", NotHandleResolution, "projects an account into the settings `AccountView`"),
        ("warn_if_forcing_onto_non_viable", NotHandleResolution, "names the already-chosen target in the `--force` warning"),
        // --- a `label` / `account_uuid` field on a DIFFERENT type ------------------------------
        //
        // The lexer is textual and cannot resolve types, so these ride along. They are cheap to
        // carry and the alternative — teaching the scanner about types — is what would make it
        // evadable.
        //
        // Four of them ride along on the SPELLING as well as on the type, and arrived together
        // when [`braced_binding_at`] taught the scan to see a field bound by name (issue #1202):
        // `execute` and `to_log_line` bind one in a `match` arm, `parse_subcommand` and
        // `from_object` write one as a struct literal's field-init shorthand. Every one of them
        // is an enum-variant or record field on `Command`, `Event` or the OAuth state record
        // rather than on an `Account`, so the section they are in is the right one — but the
        // reason they were INVISIBLE until #1202 is the spelling, not the type.
        ("account_uuid", NotHandleResolution, "`&self.account_uuid` on the OAuth state record and on the migration artifact, not on a roster account"),
        ("cached_viability_for", NotHandleResolution, "matches BOTH the daemon's wire line (`AccountStatusLine.label`) and the roster (`Account.label`) since issue #1201 — but it COUNTS bearers on each side and refuses on either being non-unique, exactly as `poke`'s `daemon_verdict` does. Counting is not resolving"),
        ("capture", NotHandleResolution, "`CaptureReport.label`"),
        ("capture_failure", NotHandleResolution, "`CaptureCommand.label` — the name being ASSIGNED to a new account"),
        ("execute", NotHandleResolution, "`Command::Capture { label }` / `Command::Login { label }` — a MATCH ARM binding the operator's CLI positional out of the parsed command and handing it to `capture` / `login`, which ASSIGN it to a new account. Nothing is matched against the roster here"),
        ("from_object", NotHandleResolution, "`Self { raw, account_uuid }` — the OAuth state record building ITSELF from `~/.claude.json`'s `accountUuid`, written as a struct literal's field-init shorthand. Not a roster account, and a write rather than a read"),
        ("daemon_verdict", NotHandleResolution, "matches BOTH the daemon's wire line (`AccountStatusLine.label`) and the roster (`Account.label`) since issue #1086 — but it COUNTS bearers on each side and refuses on either being non-unique, exactly as the registered `label_bearers` does. Counting is not resolving. Issue #1200 renamed it from `daemon_marks_quarantined` and widened its ANSWER from a bool to a three-state verdict, so a non-unique label is now told apart from an absent one; both are still refusals, and neither selects a bearer"),
        ("import_report", NotHandleResolution, "`AccountImport.label` — per-entry import outcomes"),
        ("new", NotHandleResolution, "`AccountStatusLine.label` — `StatusRow`'s rendered account cell — and, since issue #1202 taught the scan to see a field bound by name, `ManagedAccount { account_uuid, .. }`, the migration artifact naming its own key. Neither type is a roster `Account`; see MULTI_SITE_READERS"),
        ("parse_subcommand", NotHandleResolution, "`|label| Command::Capture { label }` — packs the operator's CLI positional INTO the parsed command, the same string `execute` unpacks and the same direction: a name being carried to an ASSIGNMENT, never compared with a roster handle"),
        ("perform_socket_capture", NotHandleResolution, "`CaptureCommand.label` — the name being ASSIGNED to a new account"),
        ("reconcile_login", NotHandleResolution, "`CaptureReport.label`"),
        ("serve_control", NotHandleResolution, "the control request's own `label` field, forwarded into a `CaptureCommand`"),
        ("to_log_line", NotHandleResolution, "`Event::UncapturedLogin { account_uuid }` — a MATCH ARM binding the event's OWN uuid payload to render it into the `acct=` field of a log line. Only the `Event` definition binds one; the `Diagnostic` one at `src/observability.rs:3252` does not, so this stays a single-site row"),
    ];

    /// Names that read an identity field from MORE THAN ONE definition, and why ONE
    /// [`HANDLE_READ_REGISTER`] row honestly covers every site.
    ///
    /// The register keys on the bare function name, so two same-named readers collapse into one
    /// row and the second inherits the first's disposition for free — while the name-set
    /// comparison in [`every_handle_read_is_dispositioned`] still balances, because a set cannot
    /// count. This list is what makes each such collision a written claim: a NEW one fails that
    /// test, and so does an entry here whose collision has since gone away.
    const MULTI_SITE_READERS: &[(&str, &str)] = &[
        ("account_uuid", "the accessor on the OAuth state record (`src/claude_state.rs`) and on the migration artifact (`src/migration.rs`) — both return `&self.account_uuid` on a type that is not a roster account, so the single `NotHandleResolution` row states the same fact about each"),
        ("new", "`StatusRow::new` (`src/cli.rs`) reads an `AccountStatusLine.label` for a rendered cell, and `ManagedAccount::new` (`src/migration.rs`) writes the migration artifact's own `account_uuid` from its parameter of that name. Different types, different directions, and NEITHER is a roster `Account` — which is the one fact the shared `NotHandleResolution` row states, so it is exact for both. The migration site was invisible until issue #1202: it is a struct literal's field-init shorthand, which carries no `.`"),
        ("refresh", "`impl PokeEngine for RealPokeEngine` (`src/poke.rs`) and `impl RefreshEngine for RealRefreshEngine` (`src/refresh_tick.rs`) — two unrelated traits, not one abstraction, but both forward an ALREADY-CHOSEN account's `account_uuid` into `refresh::refresh_account`, so neither resolves a handle and the shared row is exact for both. A THIRD same-named reader would not be covered by that reasoning; it reds this check and needs its own"),
    ];

    /// Every function that COMPARES an identity field — an operand of `==` or `!=` — and why
    /// that comparison is not first-match-wins resolution of an operator's handle.
    ///
    /// [`HANDLE_READ_REGISTER`] asks whether a function reads an identity field AT ALL, which is
    /// a question a name can only answer once. A name already in it therefore absorbs every
    /// further read for free, and issue #1199 measured the consequence: a first-match-wins lookup
    /// dropped into `RealPokeEngine::refresh` — registered for FORWARDING a uuid into the
    /// refresher — moved nothing that register can see. Not the name set, not the definition
    /// site, not [`MULTI_SITE_READERS`]. This list is the second question, asked per read rather
    /// than per name, and it is what that probe moves.
    ///
    /// Compared in both directions, so a function that GAINS a comparison reds, and so does an
    /// entry here whose comparison has gone away.
    ///
    /// **What the shape scan does not see**, stated here rather than left to be discovered, and
    /// each pinned by [`the_shape_gate_bites_on_a_comparison_grown_inside_a_registered_reader`]
    /// so that closing one reds a test and brings a reader back to this paragraph:
    ///
    /// - **A comparison hoisted through a binding** — `let handle = a.label.clone();` and a
    ///   `handle == query` some lines below. The read is followed by `;`, so its shape is
    ///   `Plain` while its use is a comparison. The PRESENCE question above still sees the read,
    ///   and this is the same `let` hoist
    ///   [`the_handle_read_tripwire_bites_on_a_seventh_site`] already ships as its second shape —
    ///   there the hoist is caught, because a new function is a new name. Since issue #1202 a
    ///   braced-pattern binding reaches this same residual BY CONSTRUCTION rather than by
    ///   accident: the bound name is followed by `,` or `}`, never by an operator, so the
    ///   presence question sees it and the shape question cannot. That is not a new hole — it is
    ///   this one, reached by a second spelling — and
    ///   [`the_handle_read_tripwire_bites_on_a_braced_pattern_binding`] asserts it so that a
    ///   widening of either scan cannot quietly contradict this bullet.
    /// - **Equality reached through a hash** — `HashSet::insert`, `HashMap::entry` / `get`. Those
    ///   compare by hash rather than by operator, and one ships in `config::validate`
    ///   (`uuids.insert(account.account_uuid.clone())`, the uuid-uniqueness rule). A roster
    ///   pre-indexed into a label-keyed map and looked up by an operator string would resolve
    ///   without ever writing `==`.
    /// - **A non-equality predicate applied to the read** — `a.label.starts_with(q)`,
    ///   `.ends_with`, `.contains`, `.cmp(&q) == Ordering::Equal`. Unlike the two above this IS
    ///   reachable in one line, and a prefix-match resolver (`sessiometer use wo` → `work`) is the
    ///   plausible one; measured, `[account].iter().position(|a| a.label.starts_with("probe"))`
    ///   injected into a registered reader leaves this gate GREEN. Nothing in the tree spells it
    ///   today — a `git grep -nE` over `src` for any of those four methods applied to a `label` or
    ///   `account_uuid` read returns nothing — which is why the equality operator was chosen over
    ///   a combinator list, rather than because the shape is unreachable.
    ///
    /// So the hole is narrowed rather than closed, and this is where the edge is written down as
    /// far as it has been measured. The first two residuals are not reachable in one line — both
    /// need a second statement or a data structure — which is the reason for stopping here rather
    /// than teaching the scan to follow a binding across a body.
    const COMPARING_READERS: &[(&str, &str)] = &[
        // --- the resolver ----------------------------------------------------------------------
        ("resolve_target", "IS the resolver — the one place an operator-supplied string is compared against a roster handle and a match SELECTED, and it counts every match rather than taking the first (#17, OQ-1)"),
        // --- compared against an account-UUID ----------------------------------------------------
        //
        // Unique by construction: `config::validate` rejects a duplicated uuid, so there is no
        // ambiguity here to refuse on. Only LABELS are un-unique, which is what R-6a is about.
        ("apply_import", "matches an INCOMING artifact account's uuid to a local roster index, and compares that same uuid against the ACTIVE one for the adoption check"),
        ("apply_refresh_observation", "matches a poll observation's own account-uuid back to its roster index"),
        ("apply_refresh_restore", "matches a restore outcome's account-uuid back to its roster index"),
        ("is_active", "compares an account's uuid to the ACTIVE uuid `active.rs` read from the credential"),
        ("overlay_labels", "finds the account bearing a settings-overlay uuid and ASSIGNS it a label — the uuid is the config's, and the label is the output rather than the query"),
        ("plan_capture", "matches the freshly captured OAuth account's uuid; the label is the name being ASSIGNED to it"),
        ("reconcile_restored", "matches a restored credential's uuid back to its roster index"),
        ("reconcile_roster", "re-indexes the roster across a config reload, by uuid"),
        ("recovery_pending", "membership of an account's uuid in the quarantined / excluded uuid sets"),
        ("resolve_via_display", "matches the DISPLAYED credential's uuid to a roster index — the credential is the input, not the operator"),
        ("restash_account", "`!=` between a roster account's uuid and the displayed credential's, before restashing"),
        ("run_sweep", "the sweep's per-account membership checks against the excluded and quarantined uuid lists"),
        // --- compared against a LABEL ------------------------------------------------------------
        //
        // The un-unique field, so each of these owes an account of what it does INSTEAD of taking
        // the first match: refuse on non-unique, or test membership and select nothing at all.
        ("account_listed_in", "the `[refresh].accounts` allowlist — a MEMBERSHIP test over a config-supplied set, returning a bool rather than an index, and a duplicated label there legitimately admits BOTH bearers"),
        ("cached_viability_for", "both sides since issue #1201 — the daemon WIRE line and the roster: it pulls a second match on each and returns None when one exists, rather than taking the first"),
        ("daemon_verdict", "both sides — the wire line and the roster — filtered through `lookup()`, which since issue #1200 separates zero from more-than-one where the `sole()` it replaced returned None for both (issue #1086). Refusing is not resolving, and separating two kinds of refusal is not either"),
    ];

    /// The names [`every_handle_read_is_dispositioned`] compares against the register, extracted
    /// so the canaries can drive the IDENTICAL predicate over a deliberately broken subject
    /// rather than over a paraphrase of it (ADR-0031 § 4 CONSTRAINT-A).
    fn functions_reading_a_handle(source: &str) -> Vec<String> {
        let mut names: Vec<String> = handle_reads(&non_test_region(source))
            .reads
            .into_iter()
            .map(|(name, _)| name)
            .collect();
        names.sort_unstable();
        names.dedup();
        names
    }

    /// The names compared against [`COMPARING_READERS`], extracted for the same reason and used
    /// by the same canary.
    fn functions_comparing_a_handle(source: &str) -> Vec<String> {
        let mut names: Vec<String> = handle_reads(&non_test_region(source))
            .compared_reads
            .into_iter()
            .map(|(name, _)| name)
            .collect();
        names.sort_unstable();
        names.dedup();
        names
    }

    /// The completeness tripwire for label resolution (issue #1186).
    ///
    /// `every_label_resolving_site_shares_one_resolver` (`src/cli.rs`) was named for a property
    /// over every label-resolving site and drove two of them, and the name is what a later reader
    /// trusts: issue #1087 was filed believing it gave `poke` and the daemon transitive cover. It
    /// did not. That test now says what it does; this one owns the set-level claim its old name
    /// made.
    #[test]
    fn every_handle_read_is_dispositioned() {
        let sources = crate_sources();
        let mut reads = 0usize;
        let mut spelled: Vec<String> = Vec::new();
        let mut callers: Vec<String> = Vec::new();
        let mut sites: Vec<(String, String, usize)> = Vec::new();
        let mut compared: Vec<(String, String, usize)> = Vec::new();
        for (path, text) in &sources {
            let region = non_test_region(text);
            let scan = handle_reads(&region);
            reads += scan.reads.len();
            spelled.extend(scan.reads.into_iter().map(|(name, _)| name));
            sites.extend(
                scan.read_sites
                    .into_iter()
                    .map(|(name, at)| (name, path.clone(), at)),
            );
            compared.extend(scan.compared_reads.into_iter().map(|(name, at)| {
                // Reported as `path:line`, because this message's whole job is to take its reader
                // to the comparison it is asking about. `at` is a CHAR index, and the region is a
                // line-prefix of the file, so the count is the file's own line number.
                let line = region.chars().take(at).filter(|c| *c == '\n').count() + 1;
                (name, path.clone(), line)
            }));
            callers.extend(scan.resolver_callers);
        }
        spelled.sort_unstable();
        spelled.dedup();
        let mut registered: Vec<String> = HANDLE_READ_REGISTER
            .iter()
            .map(|(name, _, _)| (*name).to_owned())
            .collect();
        registered.sort_unstable();

        assert_eq!(
            spelled, registered,
            "every function in the crate's non-test code that reads an Account identity field \
             must be dispositioned in HANDLE_READ_REGISTER — an undispositioned one may be a \
             SEVENTH label-resolving site that grew its own resolver, which is the gap issue \
             #1186 was opened about"
        );

        // Cardinality as FLOORS, never as crate-wide exact totals. The names above would agree at
        // zero if the walk found no files or the lexer matched nothing — a degenerate subject
        // reporting as a clean run, which is the failure mode a source lint dies of. A floor
        // catches that collapse and nothing else, which is the whole intent: an exact crate-wide
        // total reds on any commit that adds or removes an identity read ANYWHERE, so it taxes
        // PRs that never touch this gate and teaches its reader to re-bless a number rather than
        // check a disposition. These are collapse detectors set far below the live figures, not
        // targets — raise one only if the tree ever shrinks past it, and never to track growth.
        assert!(
            sources.len() >= 40,
            "only {} `.rs` files found under src/ — the walk collapsed, so every assertion above \
             passed over a subject that was never read",
            sources.len()
        );
        assert!(
            reads >= 100,
            "only {reads} identity-field reads found across the crate — the lexer collapsed, so \
             every assertion above passed over a subject that was never read"
        );

        // A name whose reads come from more than one definition must be an ACKNOWLEDGED collision.
        // HANDLE_READ_REGISTER keys on the name alone, so a second same-named reader inherits the
        // first one's disposition while the name-set comparison above still balances — a set
        // cannot count. Compared in both directions, so a NEW collision fails and so does a
        // MULTI_SITE_READERS entry whose collision has gone away. The crate-wide read total stood
        // in for this until issue #1197; it does so directly now, and names the sites.
        sites.sort();
        let mut collided: Vec<String> = sites
            .windows(2)
            .filter(|pair| pair[0].0 == pair[1].0)
            .map(|pair| pair[0].0.clone())
            .collect();
        collided.dedup();
        let mut acknowledged: Vec<String> = MULTI_SITE_READERS
            .iter()
            .map(|(name, _)| (*name).to_owned())
            .collect();
        acknowledged.sort_unstable();
        let measured: Vec<String> = collided
            .iter()
            .map(|name| {
                let at: Vec<String> = sites
                    .iter()
                    .filter(|(spelled, _, _)| spelled == name)
                    .map(|(_, path, at)| format!("{path} char {at}"))
                    .collect();
                format!("{name} ({})", at.join(", "))
            })
            .collect();
        assert_eq!(
            collided,
            acknowledged,
            "the names reading an identity field from more than one definition must be exactly \
             those listed in MULTI_SITE_READERS, each with the reason ONE register disposition \
             covers every site — measured: [{}]",
            measured.join("; ")
        );

        // The read SHAPE, which is the question the two set comparisons above cannot ask (issue
        // #1199). Both key on a function NAME, so a name already registered absorbs any further
        // read it grows — including a first-match-wins lookup dropped into a body registered for
        // forwarding a uuid. Measured: that probe leaves the name set, the site set and every
        // per-disposition count exactly where they were. It moves this.
        //
        // A SET, not a count, so it is silent on every commit that does not change what a
        // function compares — the property issue #1197 removed the crate-wide read total to get.
        compared.sort();
        // Two comparisons can share a line (`entry == &a.label || entry == &a.account_uuid` is
        // one, at `refresh_tick.rs:420`). One line is one place to look.
        compared.dedup();
        let mut comparing: Vec<String> = compared.iter().map(|(name, _, _)| name.clone()).collect();
        comparing.dedup();
        let mut declared: Vec<String> = COMPARING_READERS
            .iter()
            .map(|(name, _)| (*name).to_owned())
            .collect();
        declared.sort_unstable();
        let locate = |name: &String| {
            let at: Vec<String> = compared
                .iter()
                .filter(|(spelled, _, _)| spelled == name)
                .map(|(_, path, line)| format!("{path}:{line}"))
                .collect();
            format!("{name} ({})", at.join(", "))
        };
        assert_eq!(
            comparing,
            declared,
            "the functions COMPARING an identity field (`==` / `!=`) must be exactly those listed \
             in COMPARING_READERS, each with the reason its comparison is not first-match-wins \
             resolution of an operator handle. A name that gained one is a resolver grown inside \
             a body already registered for reading — measured: [{}]",
            comparing.iter().map(locate).collect::<Vec<_>>().join("; ")
        );

        // Per-disposition counts stay exact: the register they mirror is fifty lines up in THIS
        // file, so the reader who moves one is looking straight at the number that must move with
        // it. That is what the crate-wide totals above could never be.
        for (arm, expected) in [
            (SharedResolver, 1),
            (ViaSharedResolver, 4),
            (NotHandleResolution, 83),
        ] {
            let actual = HANDLE_READ_REGISTER
                .iter()
                .filter(|(_, disposition, _)| *disposition == arm)
                .count();
            assert_eq!(
                actual, expected,
                "expected {expected} {arm:?} entries in HANDLE_READ_REGISTER, found {actual}"
            );
        }

        // `ViaSharedResolver` is the one arm that is a CLAIM about behaviour rather than a
        // reading, so it is the one arm that is checked rather than believed: the function must
        // actually contain a `resolve_target` call. Without this the arm would be a comment.
        callers.sort_unstable();
        callers.dedup();
        for (name, disposition, _) in HANDLE_READ_REGISTER {
            if *disposition == ViaSharedResolver {
                assert!(
                    callers.iter().any(|c| c == name),
                    "{name:?} is dispositioned ViaSharedResolver but its body holds no \
                     `resolve_target` call — it either resolves some other way, or the \
                     disposition is stale"
                );
            }
        }

        // An entry without a reason is indistinguishable from an oversight, and the reason is
        // what the next reader copies the shape of.
        for (name, disposition, reason) in HANDLE_READ_REGISTER {
            assert!(
                !reason.trim().is_empty(),
                "{name:?} is dispositioned {disposition:?} with no reason recorded"
            );
        }
    }

    /// CONSTRAINT-A for the braced-pattern arm (ADR-0031 § 4) — issue #1202's probe, committed
    /// as the canary that issue asks for rather than left in its prose.
    ///
    /// The defect: [`identity_field_at`] matches a `.`-access, and Rust reads a field without a
    /// `.` through pattern binding. The probe below was measured against the real instrument and
    /// passed it — a seventh first-match-wins label-resolving site landing silent, which is the
    /// gap issue #1186 was opened about, reached by spelling rather than by structure.
    ///
    /// Driven through [`functions_reading_a_handle`], the predicate the real assertion compares
    /// with, so this canary cannot pass over a paraphrase of the gate.
    ///
    /// Every member of the family is here rather than the probe alone, because ONE rule covers
    /// them and a rule is only demonstrated by the set it spans: the `let` the issue names, the
    /// `if let` and `match` arm it asks about, the closure parameter and the destructuring `fn`
    /// parameter that share the shape, and the `ref` / `mut` prefixes — of which the tree spells
    /// only `mut`, at `src/capture.rs:432`, `ref` being covered by the same two lines and pinned
    /// here rather than claimed. Each carries the same positive control — the fixture's own `#[cfg(test)]`
    /// helper reads an identity field on EVERY run, so an assertion of `["apply_park"]` is
    /// simultaneously evidence the scan reached the production code and evidence it stopped at
    /// the test boundary. A scan that matched nothing would fail both halves rather than pass
    /// quietly.
    #[test]
    fn the_handle_read_tripwire_bites_on_a_braced_pattern_binding() {
        // Built by joining lines rather than with `\`-continuations, for the reason
        // [`the_non_test_boundary_survives_a_cfg_test_import`] records: that escape eats the
        // next line's leading whitespace, and these fixtures' `#[cfg(test)]` must stay at column 0.
        let injected = |signature: &str, body: &[&str]| {
            let mut lines = vec![signature];
            lines.extend_from_slice(body);
            lines.extend_from_slice(&[
                "}",
                "#[cfg(test)]",
                "mod tests {",
                "fn a_test_helper(a: &Account) -> String { a.label.clone() }",
                "}",
            ]);
            lines.join("\n")
        };
        const SIG: &str = "fn apply_park(roster: &[Account], query: &str) -> Option<usize> {";

        for (spelling, signature, body) in [
            // The issue's probe, verbatim: a `let` destructure of a fully-qualified path, inside
            // the closure of a first-match-wins `position`.
            (
                "a `let` destructure (issue #1202's probe)",
                SIG,
                &[
                    "    roster.iter().position(|account| {",
                    "        let crate::config::Account { label, .. } = account;",
                    "        label == query",
                    "    })",
                ][..],
            ),
            (
                "an `if let`",
                SIG,
                &[
                    "    if let Account { label, .. } = &roster[0] {",
                    "        return (label == query).then_some(0);",
                    "    }",
                    "    None",
                ][..],
            ),
            (
                "a `match` arm's struct pattern",
                SIG,
                &[
                    "    match &roster[0] {",
                    "        Account { account_uuid, .. } => (account_uuid == query).then_some(0),",
                    "    }",
                ][..],
            ),
            (
                "a closure parameter",
                SIG,
                &["    roster.iter().position(|Account { label, .. }| label == query)"][..],
            ),
            (
                "a `ref` binding",
                SIG,
                &[
                    "    let Account { ref label, .. } = &roster[0];",
                    "    (label == query).then_some(0)",
                ][..],
            ),
            (
                "a `mut` binding",
                SIG,
                &[
                    "    let Account { mut label, .. } = roster[0].clone();",
                    "    (label == query).then_some(0)",
                ][..],
            ),
            // A destructuring PARAMETER, which binds before the body brace opens. Attributed to
            // the function being declared rather than dropped — see `handle_reads`.
            (
                "a destructuring `fn` parameter",
                "fn apply_park(Account { label, .. }: &Account, query: &str) -> Option<usize> {",
                &["    (label == query).then_some(0)"][..],
            ),
            // The case above cannot see the parameter-list brace guard: its one read comes from
            // the PARAMETER under both guard states — via `in_param_pattern` when the guard is
            // present, via the wrongly-pushed scope when it is absent. So the parameter here
            // binds NO identity field and the BODY reads one. Without the guard the scope binds
            // to the pattern brace and closes at the pattern's `}`, leaving the body's read with
            // no enclosing scope to attribute it to, and it is dropped.
            (
                "a body read beneath a destructuring `fn` parameter",
                "fn apply_park(Account { .. }: &Account, roster: &[Account], query: &str) -> Option<usize> {",
                &["    roster.iter().position(|a| a.label == query)"][..],
            ),
            // …and the same shape one level deeper (issue #1223). These two are a PAIR, and each
            // half reds under a mutation the other survives — which is the whole reason both are
            // here. Deleting the guard outright reds the FLAT case above. Narrowing it to the
            // INNERMOST delimiter (`delims.last() != Some(&'(')`) leaves that flat case green —
            // at its pattern brace the innermost open delimiter really is `(` — and reds only
            // this one, because at the inner `{` of a nested pattern the innermost delimiter is
            // `{`. A guard that asks about depth instead answers both. Measured by mutation per
            // ADR-0031 § 4 CONSTRAINT-A, not by reading the arm.
            (
                "a body read beneath a NESTED destructuring `fn` parameter",
                "fn apply_park(Wrapper { inner: Account { .. }, .. }: &Wrapper, roster: &[Account], query: &str) -> Option<usize> {",
                &["    roster.iter().position(|a| a.label == query)"][..],
            ),
            // …and the mirror-image half, which is what pins `in_param_pattern`'s own depth test.
            // Here the NESTED pattern binds the identity field and the body reads nothing, so the
            // read can only arrive through the parameter arm. Under the second-innermost-delimiter
            // form (`delims[len - 2] == '('`) that arm sees the outer PATTERN brace instead of the
            // parameter list, the read is filed nowhere at all, and this reds with `[]` — the same
            // total bypass one question over. Measured by mutation per ADR-0031 § 4 CONSTRAINT-A.
            (
                "an identity field bound INSIDE a nested `fn` parameter pattern",
                "fn apply_park(Wrapper { inner: Account { label, .. }, .. }: &Wrapper, query: &str) -> Option<usize> {",
                &["    (label == query).then_some(0)"][..],
            ),
        ] {
            let source = injected(signature, body);
            assert_eq!(
                functions_reading_a_handle(&source),
                ["apply_park"],
                "the scan must see a seventh site whose handle read is spelled as {spelling}, \
                 and must see ONLY that: the fixture's own test-module read is not production \
                 resolution, and its presence is what proves the scan ran at all"
            );
            // …and that IS a red run, because the register cannot hold a name it has never seen.
            // Stated as the real membership rather than left as an inference about it.
            assert!(
                !HANDLE_READ_REGISTER
                    .iter()
                    .any(|(name, _, _)| *name == "apply_park"),
                "the canary's function must be absent from the register, or it proves nothing"
            );
        }

        // A `fn` is not always declared at the depth the file opens at, and the depth its body
        // brace is recognised by has to be captured where the `fn` token is READ — with every
        // enclosing delimiter already on the stack. `bar` below sits inside a closure inside a
        // call, so it opens at depth 2 and its body brace closes back to depth 2; an
        // implementation comparing against a fixed 0, or capturing the depth at the `(` after
        // the name, never binds it and the read inside it lands on the ENCLOSING function
        // instead. That is the mutation this reds under — not the innermost-delimiter form,
        // which resolves this case correctly. So it pins a property the fix must not break
        // rather than the defect the fix closes, and it is the one case here whose subject is
        // which function a read is attributed to rather than whether it is seen at all.
        let nested_fn = injected(
            SIG,
            &[
                "    foo(|| { fn bar(r: &[Account], q: &str) -> Option<usize> {",
                "        r.iter().position(|a| a.label == q)",
                "    } });",
                "    None",
            ],
        );
        assert_eq!(
            functions_reading_a_handle(&nested_fn),
            ["bar"],
            "a read inside a `fn` nested in a closure belongs to that `fn`, not to the function \
             enclosing it: `apply_park` reads nothing here, and naming it would mean the nested \
             body brace was never recognised as one"
        );
        // …and the NAME alone cannot say that, which the mutation above was what established.
        // A `pending` that is never taken is never cleared either, so `in_param_pattern` stays
        // true for a signature that in effect never ended, and the body's read is filed under
        // the same name `bar` by the parameter arm instead — identical here, and the assertion
        // above passes on it. The SITE is what separates the two, because a parameter read
        // deliberately records none (see [`Scan::read_sites`]): under the correct binding there
        // is exactly one, and under that mutation there are none.
        let sites: Vec<String> = handle_reads(&non_test_region(&nested_fn))
            .read_sites
            .into_iter()
            .map(|(name, _)| name)
            .collect();
        assert_eq!(
            sites,
            ["bar"],
            "the nested read must be attributed through an open BODY scope, which records a \
             definition site — a name filed by the parameter arm instead records none, and \
             would leave this empty while the assertion above still passed"
        );

        // The negative control, and the reason `handle_reads` tracks every delimiter rather
        // than only braces. Asserted as a PAIR differing in one character, because either half
        // alone passes for the wrong reason: `is_empty()` is satisfied by a scan that matched
        // nothing anywhere, and the positive half is satisfied by a scan that matches everything.
        // Together they are the discrimination. Without the delimiter test the scan files
        // ordinary argument lists as handle reads — measured on this tree by removing it:
        // `run_login_locked` starts reporting `run_login(…, label, …)` (`src/capture.rs:762`) and
        // `login` starts reporting the tuple pattern `Ok((outcome, label, count))` (`:276`), and
        // every future one would too. `capture_locked`'s `run_capture(…, label)` (`:237`) does
        // NOT — `label` is last there, and the CLOSE test admits `,` or `}` rather than `)`.
        let delimited = |open: &str, close: &str| {
            injected(
                "fn report(actual: &str, expected: &str) {",
                &[&format!(
                    "    check{open} actual, label, expected {close});"
                )],
            )
        };
        assert!(
            functions_reading_a_handle(&delimited("(", "")).is_empty(),
            "a bare `label` in a PARENTHESISED list is an argument, not a field binding"
        );
        assert_eq!(
            functions_reading_a_handle(&delimited("( Thing {", "}")),
            ["report"],
            "…and the SAME tokens inside a braced group ARE a field binding — the pair is what \
             proves the delimiter is doing the work rather than the identifier"
        );

        // The residual the shorthand rule leaves open, pinned rather than left in prose: a
        // RENAMED binding. `Account { label: handle, .. }` reads the field and calls it something
        // else, and that spelling is textually identical to a struct DEFINITION's `label: String`
        // and to a struct LITERAL's `label: expr` — so admitting it would admit every declared
        // field in the crate. Paired with its shorthand twin for the reason above: alone, the
        // `is_empty()` half would be satisfied by a fixture that stopped producing its subject.
        // If a future widening starts seeing the rename, this reds and sends its reader to the
        // note in HANDLE_READ_REGISTER.
        let bound_as = |binding: &str| {
            injected(
                SIG,
                &[
                    &format!("    let Account {{ {binding}, .. }} = &roster[0];"),
                    "    (handle == query).then_some(0)",
                ],
            )
        };
        assert!(
            functions_reading_a_handle(&bound_as("label: handle")).is_empty(),
            "the rename residual: a field bound under a DIFFERENT name is NOT seen, and \
             HANDLE_READ_REGISTER says so"
        );
        assert_eq!(
            functions_reading_a_handle(&bound_as("label")),
            ["apply_park"],
            "…and the same fixture in shorthand IS seen — so the `is_empty()` above is the \
             rename being invisible, not the fixture having stopped producing a subject"
        );

        // The backward half of the shorthand rule: the name must OPEN its entry, not merely
        // close one. Without it a block's TAIL EXPRESSION reads as a field binding — `label`
        // alone on the last line is followed by `}` exactly as a shorthand entry is, and only
        // what precedes it tells the two apart. Paired, for the reason the two pairs above are.
        assert!(
            functions_reading_a_handle(&injected(
                "fn handle(label: String) -> String {",
                &["    let _ = 1;", "    label"],
            ))
            .is_empty(),
            "a block's tail expression is a USE of a binding, not a field bound by one: it \
             closes an entry it never opened"
        );
        assert_eq!(
            functions_reading_a_handle(&injected(
                "fn handle(a: &Account) -> String {",
                &["    let Account { label, .. } = a;", "    label"],
            )),
            ["handle"],
            "…and the identical tail expression IS seen once a braced pattern above it opened \
             the entry — the pair is what proves the backward test discriminates"
        );

        // The shape question is deliberately unmoved by any of this. A pattern binding is
        // followed by `,` or `}`, never by an operator, so it lands in the hoist residual
        // COMPARING_READERS already documents — one binding away from its comparison, exactly
        // like the `let handle = a.label.clone()` that residual is written about. Asserted so
        // that a future widening of the SHAPE scan cannot quietly contradict that paragraph.
        let probe = injected(
            SIG,
            &[
                "    roster.iter().position(|account| {",
                "        let crate::config::Account { label, .. } = account;",
                "        label == query",
                "    })",
            ],
        );
        assert!(
            functions_comparing_a_handle(&probe).is_empty(),
            "a pattern binding is a READ, not a comparison operand — it reaches its `==` through \
             a binding, which is the hoist residual COMPARING_READERS records"
        );
    }

    /// A `;` NESTED in a `fn` signature does not end the declaration (issue #1225), and a `;`
    /// TERMINATING a bodiless one still does.
    ///
    /// One canary for both, because they are one arm's two directions and neither assertion
    /// means anything alone: each half is GREEN under the mutation that reds the other, which is
    /// what makes the pair a discrimination rather than two agreeable facts (ADR-0031 § 4
    /// CONSTRAINT-A). Four mutations were RUN, and each red was read for its value rather than
    /// for being red:
    ///
    /// | mutation of the `;` arm | what reds |
    /// |---|---|
    /// | restore the unconditional `pending = None` | every case of half one, each to `[]` — bar the attribution one, which reds to `["apply_park"]` — and the live-site case to `[]`. The bodiless half stays GREEN |
    /// | delete the clear outright | ONLY the bodiless half, to `["apply_park", "probe"]`. Half one stays GREEN |
    /// | `delims.last() != Some(&'[')` — the innermost-delimiter form | ONLY half one's last row, to `[]` |
    /// | `delims.is_empty()` — a fixed depth 0 rather than the captured one | ONLY the bodiless half, to `["apply_park", "probe"]` |
    ///
    /// The first two are the pair. The third is #1223's lesson asked one token over, and the
    /// fourth is why `pending_depth` is captured at the `fn` token rather than assumed.
    ///
    /// What the defect cost was a whole BODY rather than a read: `pending` cleared mid-signature
    /// leaves the body brace with no function to bind, so every read in that body is attributed
    /// to nothing and dropped, and the gate goes on reporting green over a function it stopped
    /// looking at. That is the same total bypass the destructuring-parameter family in
    /// [`the_handle_read_tripwire_bites_on_a_braced_pattern_binding`] pins, reached through a
    /// different token — and it was the one of the three the tree actually SPELLED, which the
    /// last case here measures on the tree rather than on a fixture.
    ///
    /// Driven through [`functions_reading_a_handle`] for the reason that test records: so this
    /// cannot pass over a paraphrase of the gate.
    #[test]
    fn a_semicolon_nested_in_a_signature_does_not_end_the_declaration() {
        // Whole items rather than a signature plus a body, because several of these fixtures
        // declare more than one. Joined rather than `\`-continued for the reason
        // [`the_non_test_boundary_survives_a_cfg_test_import`] records: that escape eats the
        // next line's leading whitespace, and the `#[cfg(test)]` below must stay at column 0.
        let injected = |items: &[&str]| {
            let mut lines: Vec<&str> = items.to_vec();
            lines.extend_from_slice(&[
                "#[cfg(test)]",
                "mod tests {",
                "fn a_test_helper(a: &Account) -> String { a.label.clone() }",
                "}",
            ]);
            lines.join("\n")
        };

        // Half one: the `;` the fix stops honouring. Each fixture's read sits in the BODY, so a
        // name here means the body brace bound its function; `[]` means it bound nothing. The
        // `#[cfg(test)]` helper reads an identity field on every run, so asserting exactly one
        // name is simultaneously evidence the scan reached the production code and evidence it
        // stopped at the test boundary — a scan that matched nothing fails both halves.
        for (spelling, reader, items) in [
            // The shape that ISOLATES this root from issue #1223's: no brace anywhere in the
            // signature, so the parameter-list brace guard cannot be what drops it.
            (
                "a parameter's array type, with no brace anywhere",
                "apply_park",
                &[
                    "fn apply_park(buf: [u8; 4], roster: &[Account], query: &str) -> Option<usize> {",
                    "    roster.iter().position(|a| a.label == query)",
                    "}",
                ][..],
            ),
            // The RETURN-type position, which is where the crate's live instance spells it.
            (
                "a RETURN type's array",
                "apply_park",
                &[
                    "fn apply_park(roster: &[Account], query: &str) -> Option<[u8; 4]> {",
                    "    roster.iter().position(|a| a.label == query).map(|_| [0u8; 4])",
                    "}",
                ][..],
            ),
            // The shape issue #1223 was FILED against and fixed the wrong half of: with that
            // depth-guarded brace arm in place this still came back `[]`, which is what
            // separated the two roots rather than folding this into that fix.
            (
                "an array length spelled as a braced const expression",
                "apply_park",
                &[
                    "fn apply_park(buf: [u8; { SIZE }], roster: &[Account], query: &str) -> Option<usize> {",
                    "    roster.iter().position(|a| a.label == query)",
                    "}",
                ][..],
            ),
            // …and that block carrying a `;` of its OWN, one declaration-depth down. This is the
            // row that pins DEPTH rather than the innermost delimiter, exactly as #1223's pair
            // does one question over: a guard spelled `delims.last() != Some(&'[')` satisfies
            // every other case here — at their `;` the innermost open delimiter really is `[` —
            // and reds only this one, where it is `{`. Measured. Its `fn` is also the only one
            // here declared below depth 0, but that is exercise rather than discrimination: a
            // guard comparing against a fixed 0 is caught by the bodiless half below, and this
            // row stays green under it. Measured too, rather than assumed from the shape.
            (
                "a signature block carrying its own `;`, one delimiter deeper than the file",
                "described",
                &[
                    "trait Probe {",
                    "    fn described(&self, buf: [u8; { let n = 4; n }], r: &[Account], q: &str) -> Option<usize> {",
                    "        r.iter().position(|a| a.label == q)",
                    "    }",
                    "}",
                ][..],
            ),
        ] {
            assert_eq!(
                functions_reading_a_handle(&injected(items)),
                [reader],
                "a `;` inside a signature must not end the declaration, and this one is spelled \
                 as {spelling}: `[]` here is the body brace binding no function at all, which \
                 drops every read in that body and leaves this gate green over it"
            );
        }

        // …and the same root with a DIFFERENT observable, which is why the four above are not
        // the whole half. When the dropped function is nested inside another, its body read does
        // not vanish — the enclosing scope is still open, so the read is filed under the WRONG
        // name and the population stays the same size. A gate asserting only that something was
        // seen passes on that; only the name separates them.
        let nested = injected(&[
            "fn apply_park(roster: &[Account], query: &str) -> Option<usize> {",
            "    fn inner(buf: [u8; 4], r: &[Account], q: &str) -> Option<usize> {",
            "        r.iter().position(|a| a.label == q)",
            "    }",
            "    inner([0u8; 4], roster, query)",
            "}",
        ]);
        assert_eq!(
            functions_reading_a_handle(&nested),
            ["inner"],
            "a read in a nested `fn` whose signature carries a `;` belongs to that `fn`: naming \
             `apply_park` instead means `inner` never bound a scope and its body landed on the \
             function enclosing it — the same defect, silently rather than emptily"
        );

        // Half two: the `;` the fix must go on honouring, and the reason the unconditional clear
        // was written. A bodiless declaration ends at its `;`; if it does not, `pending` survives
        // to bind the next brace at its depth — here the associated const's initializer block —
        // and `probe` is filed as a reader of a body it does not have. That is what the arm's own
        // comment claims, asserted rather than trusted.
        let bodiless = injected(&[
            "trait Probe {",
            "    fn probe(&self) -> Option<usize>;",
            "    const SEEN: usize = { DEFAULTS.label.len() };",
            "}",
            "fn apply_park(roster: &[Account], query: &str) -> Option<usize> {",
            "    roster.iter().position(|a| a.label == query)",
            "}",
        ]);
        assert_eq!(
            functions_reading_a_handle(&bodiless),
            ["apply_park"],
            "a bodiless declaration must not capture the block that follows it: `probe` \
             appearing here is that block bound as its body, which is the bug the clear exists \
             against and the one a `;` guard widened too far reintroduces"
        );

        // The live instance, measured on the real tree rather than retyped (issue #1225's third
        // acceptance criterion). The signature below is READ from `src/migration.rs` at test
        // time, so this cannot pass over a paraphrase that lost the property it is about.
        //
        // The body is synthetic because it has to be: `derive_key` reads no identity field
        // today, and the fix is therefore invisible in what this gate reports over the tree —
        // the whole-tree scan (reads, sites, resolver callers and compared reads) comes out
        // IDENTICAL with the guard and without it, measured by diffing the two. What moved is
        // the site's reachability, and only a read placed in that body can observe it.
        let migration = crate_sources()
            .into_iter()
            .find(|(path, _)| path.ends_with("migration.rs"))
            .map(|(_, text)| non_test_region(&text))
            .expect("`src/migration.rs` is one of the files this gate scans");
        let live = migration
            .lines()
            .find(|line| line.trim_start().starts_with("fn derive_key("))
            .expect(
                "`src/migration.rs` declares `derive_key`, the live `;`-in-signature site issue \
                 #1225 measured; if it has moved or been renamed, re-derive the site rather than \
                 dropping this case",
            )
            .to_string();
        assert!(
            live.contains(';'),
            "…and it is the subject only because that signature CARRIES a nested `;`, which \
             this one no longer does: {live}"
        );
        assert_eq!(
            functions_reading_a_handle(&injected(&[
                &live,
                "    if passphrase.label == \"probe\" {",
                "        return Err(Error::MigrationCryptoParams(\"probe\"));",
                "    }",
                "    unreachable!()",
                "}",
            ])),
            ["derive_key"],
            "an identity-field read added to the REAL `derive_key` body must be seen: `[]` here \
             is the live site still dropping its whole body, with \
             `every_handle_read_is_dispositioned` green while it does"
        );
    }

    /// The compliant set, pinned from the other side (issue #1186).
    ///
    /// [`every_handle_read_is_dispositioned`] catches a site that grows its OWN resolver. It
    /// cannot catch the inverse — a site quietly dropping [`resolve_target`] — because
    /// `cli::apply_remove` proves the two populations differ: it resolves and removes without
    /// ever reading an identity field, so no field scan can see it. Six operator-facing verbs
    /// (`use`, `poke`, the daemon's control-socket swap, `enable`, `disable`, `remove`) reach the
    /// resolver through FIVE call sites, because `enable` and `disable` share `apply_enabled`.
    #[test]
    fn resolve_target_has_exactly_the_five_known_call_sites() {
        let mut callers: Vec<String> = crate_sources()
            .iter()
            .flat_map(|(_, text)| handle_reads(&non_test_region(text)).resolver_callers)
            .collect();
        callers.sort_unstable();
        callers.dedup();
        assert_eq!(
            callers,
            [
                "apply_enabled",
                "apply_remove",
                "perform_socket_swap",
                "poke_named",
                "run_use",
            ],
            "the five label-resolving call sites; a new one is a new verb that must also be \
             dispositioned in HANDLE_READ_REGISTER, and a MISSING one is a site that stopped \
             sharing the resolver"
        );
    }

    /// CONSTRAINT-A for the tripwire above (ADR-0031 § 4): the gate is observed to REDDEN on a
    /// subject carrying the defect, not merely read and believed. Both cases are driven through
    /// [`functions_reading_a_handle`], the predicate the real assertion compares with.
    ///
    /// The payload is the defect issue #1186 describes — a seventh verb that grows its own
    /// first-match-wins lookup instead of calling [`resolve_target`].
    ///
    /// The second case is the one that settles the SUBJECT rather than the gate. Hoisting the
    /// field through a `let` before comparing it is an ordinary refactor, and it is what a
    /// scanner keyed on the COMPARISON would lose — which is not hypothetical: such hoists
    /// already ship, including inside `cli::apply_enabled`, a site this property is about. Both
    /// shapes must land, or the tripwire is one `let` away from blind.
    #[test]
    fn the_handle_read_tripwire_bites_on_a_seventh_site() {
        for (shape, body) in [
            (
                "compared in place",
                "    let idx = roster.iter().position(|a| a.label == query)?;",
            ),
            (
                "hoisted through a `let`",
                "    let handle = &roster[0].label;\n    let idx = (handle == query).then_some(0)?;",
            ),
        ] {
            let injected = format!(
                "fn apply_park(roster: &mut Vec<Account>, query: &str) -> Option<usize> {{\n\
                 {body}\n\
                 }}\n\
                 #[cfg(test)]\n\
                 mod tests {{\n\
                 fn a_test_helper(a: &Account) -> String {{ a.label.clone() }}\n\
                 }}\n"
            );
            let spelled = functions_reading_a_handle(&injected);
            assert_eq!(
                spelled,
                ["apply_park"],
                "the scan must see a seventh site's handle read {shape}, and must see ONLY that: \
                 the test module's own reads are not production resolution"
            );
            // …and that IS a red run, because the register cannot hold a name it has never seen.
            // Stated as the real comparison rather than left as an inference about it.
            assert!(
                !HANDLE_READ_REGISTER
                    .iter()
                    .any(|(name, _, _)| *name == "apply_park"),
                "the canary's function must be absent from the register, or it proves nothing"
            );
        }
    }

    /// CONSTRAINT-A for the SHAPE half (ADR-0031 § 4) — the probe from issue #1199, committed as
    /// the canary that issue asks for rather than left in its prose.
    ///
    /// The subject is `RealPokeEngine::refresh` reduced to its identity read: a body forwarding
    /// an already-chosen account's uuid into the refresher, under a name
    /// [`HANDLE_READ_REGISTER`] already carries. The mutation is the issue's probe — a
    /// first-match-wins label lookup dropped into that same body.
    ///
    /// Both halves are asserted, because only the pair is evidence. The probe must move the
    /// shape question, AND it must leave the two older questions exactly where they were. That
    /// second half is not decoration: it is the measured reason this gate had to exist at all,
    /// and it is what stops the assertion below from one day passing for a reason it does not
    /// claim — a future widening that made the NAME question move on this probe would leave
    /// every assertion here green while the sentence above it went false.
    #[test]
    fn the_shape_gate_bites_on_a_comparison_grown_inside_a_registered_reader() {
        // Built by joining lines rather than with `\`-continuations, for the reason
        // [`the_non_test_boundary_survives_a_cfg_test_import`] records: that escape eats the
        // next line's leading whitespace, and this fixture's `#[cfg(test)]` must stay at column 0.
        let body = |extra: &str| {
            [
                "impl PokeEngine for RealPokeEngine {",
                "    async fn refresh(&self, account: &Account) -> Result<RefreshReport> {",
                extra,
                "        refresh::refresh_account(&self.stash, &account.account_uuid).await",
                "    }",
                "}",
                "#[cfg(test)]",
                "mod tests {",
                "    fn a_test_helper(roster: &[Account], q: &str) -> bool {",
                "        roster.iter().any(|a| a.label == q)",
                "    }",
                "}",
            ]
            .join("\n")
        };
        let forwarding = body("");
        let probed = body(
            "        let _seventh: Option<usize> = [account].iter().position(|a| a.label == \"probe\");",
        );

        // Neither older question can tell these two apart. Issue #1199's measurement, re-run
        // here against the real predicates rather than quoted from the issue.
        assert_eq!(
            functions_reading_a_handle(&forwarding),
            functions_reading_a_handle(&probed),
            "the probe must leave the READ question unmoved — `refresh` reads an identity field \
             either way, which is precisely why HANDLE_READ_REGISTER cannot see this"
        );
        assert_eq!(
            handle_reads(&non_test_region(&forwarding)).read_sites,
            handle_reads(&non_test_region(&probed)).read_sites,
            "…and the SITE question with it: the probe adds no definition, so the collision \
             check MULTI_SITE_READERS drives is unmoved too"
        );

        // The shape question moves, and moves only on the probe.
        assert!(
            functions_comparing_a_handle(&forwarding).is_empty(),
            "a forwarded uuid is not a comparison — and the fixture's own test module compares \
             one on every run, so a scan that reached past the boundary would report a resolver \
             in every file that tests one"
        );
        assert_eq!(
            functions_comparing_a_handle(&probed),
            ["refresh"],
            "the probe must be seen as a COMPARISON inside `refresh`, the name it hides behind"
        );

        // …and that IS a red run. Stated as the two real memberships rather than left as an
        // inference about them: `refresh` is registered as a READER, deliberately not as a
        // comparer, so the set comparison in `every_handle_read_is_dispositioned` cannot balance.
        assert!(
            HANDLE_READ_REGISTER
                .iter()
                .any(|(name, _, _)| *name == "refresh"),
            "the canary's function must be REGISTERED, or it re-demonstrates the #1186 hole \
             instead of the #1199 one"
        );
        assert!(
            !COMPARING_READERS.iter().any(|(name, _)| *name == "refresh"),
            "…and absent from COMPARING_READERS, or the probe's name would balance and the \
             mutation would prove nothing"
        );

        // The same probe with the operator spelled as a method. Nothing in the tree writes it
        // that way, so that arm of the scan has no register row holding it up and this assertion
        // is the only thing between it and decoration.
        let via_method = body(
            "        let _seventh: Option<usize> = [account].iter().position(|a| a.label.eq(\"probe\"));",
        );
        assert_eq!(
            functions_comparing_a_handle(&via_method),
            ["refresh"],
            "`.eq(…)` is the same comparison as `==`, and issue #1199 named it as one"
        );

        // The two residuals [`COMPARING_READERS`] documents, pinned OPEN rather than left in
        // prose — an unasserted limit is one that quietly closes or quietly widens, and either
        // way the paragraph promising it stops being true without anything saying so. If a
        // future widening starts seeing one of these, this reds and sends its reader there.
        let hoisted = body(
            "        let handle = account.label.clone();\n        let _ = handle == \"probe\";",
        );
        assert!(
            functions_comparing_a_handle(&hoisted).is_empty(),
            "the hoist residual: a comparison one binding away from its read is NOT seen, and \
             COMPARING_READERS says so"
        );
        let hashed = body("        let _ = uuids.insert(account.account_uuid.clone());");
        assert!(
            functions_comparing_a_handle(&hashed).is_empty(),
            "the hash residual: equality reached through a set or a map is NOT seen either — \
             this is `config::validate`'s uuid-uniqueness rule, and COMPARING_READERS says so"
        );
    }

    /// The boundary rule, pinned against the shape that would silently blind this whole gate.
    ///
    /// `INLINE_PROSE_REGISTER`'s lexer cuts at the first line STARTING with `#[cfg(test)]`. That
    /// is correct for `src/cli.rs` and wrong for this file, which carries one in its import block
    /// on a test-only `use`, far above [`resolve_target`]. Under that rule the gate would stop at
    /// that import and never see the resolver it protects — a green run over a truncated subject.
    /// Every arm of the rule is asserted here: a `#[cfg(test)]` on an import must NOT cut, one on
    /// an INDENTED `mod` must NOT cut, one on a module DECLARATION (`mod test_support;`) must NOT
    /// cut, and one on a column-0 inline `mod tests {` MUST.
    #[test]
    fn the_non_test_boundary_survives_a_cfg_test_import() {
        let source = "use crate::a;\n\
                      #[cfg(test)]\n\
                      use crate::b;\n\
                      fn production(a: &Account) -> String { a.label.clone() }\n\
                      #[cfg(test)]\n\
                      mod tests {\n\
                      fn helper(a: &Account) -> String { a.account_uuid.clone() }\n\
                      }\n";
        assert_eq!(
            functions_reading_a_handle(source),
            ["production"],
            "a `#[cfg(test)]` on an IMPORT must not truncate the scan, and the test module must"
        );

        // The other half of the bias, pinned so it is a decision rather than an accident: an
        // INDENTED `mod` does not cut. `src/redaction.rs` nests its `mod tests` inside
        // `mod meter`, and honouring that spelling would mean cutting at the first nested test
        // block in ANY file — discarding whatever production code sits below it.
        // Built by joining lines rather than with `\`-continuations: that escape eats the
        // following line's leading whitespace, which would silently flatten this fixture back
        // into the column-0 case it exists to be different from.
        let nested = [
            "fn production(a: &Account) -> String { a.label.clone() }",
            "mod meter {",
            "    #[cfg(test)]",
            "    mod tests {}",
            "}",
            "fn below(a: &Account) -> String { a.account_uuid.clone() }",
        ]
        .join("\n");
        assert_eq!(
            functions_reading_a_handle(&nested),
            ["below", "production"],
            "an indented test module must NOT cut the scan — the code below it is production"
        );

        // The third shape, and the one that actually shipped broken (issue #1197): a `#[cfg(test)]`
        // on a module DECLARATION — `mod test_support;`, a whole file's worth of test helpers
        // living elsewhere. It satisfies "next line starts with `mod `" exactly as an inline
        // `mod tests {` does, so the unqualified rule cut `src/config.rs` at its line 57 and
        // discarded `struct Account` and `impl Account` below it: the very type this gate is
        // named for, scanned at 3% of its length, reporting green.
        let declaration = [
            "mod render;",
            "#[cfg(test)]",
            "mod test_support;",
            "mod validate;",
            "fn production(a: &Account) -> String { a.label.clone() }",
        ]
        .join("\n");
        assert_eq!(
            functions_reading_a_handle(&declaration),
            ["production"],
            "a `#[cfg(test)]` on a module DECLARATION must not cut the scan — the declaration \
             names a file, and the production code below it is still production"
        );

        // …and the same declaration wearing a trailing COMMENT, which is why the rule demands an
        // opening brace rather than merely rejecting a `;`. Phrased the weaker way, this shape
        // restores the entire defect through a comment — and `src/config.rs:53` already carries
        // one two lines above the block this fixture is modelled on.
        let commented = [
            "mod render;",
            "#[cfg(test)]",
            "mod test_support; // helpers live in config/test_support.rs",
            "fn production(a: &Account) -> String { a.label.clone() }",
        ]
        .join("\n");
        assert_eq!(
            functions_reading_a_handle(&commented),
            ["production"],
            "a commented module declaration must not cut the scan either — a comment cannot be \
             what decides whether this gate reads the file that defines `Account`"
        );

        // …and the same rule, over the two real files that hold the trap: the resolver's own,
        // and `Account`'s.
        let this_file = std::fs::read_to_string("src/use_account.rs").expect("readable");
        assert!(
            non_test_region(&this_file).contains("pub(crate) fn resolve_target"),
            "the non-test region of src/use_account.rs must still contain the resolver — if it \
             does not, every assertion above is running on a truncated subject"
        );
        let config = std::fs::read_to_string("src/config.rs").expect("readable");
        assert!(
            non_test_region(&config).contains("impl Account {"),
            "the non-test region of src/config.rs must still contain `impl Account` — it sits \
             BELOW that file's `#[cfg(test)] mod test_support;`, so this is the assertion that \
             fails if the declaration clause is ever dropped"
        );
    }

    /// The lexer is the one textual link in this completeness chain, so its skip-classes are
    /// tested directly rather than only through their caller. A scanner that reads an identity
    /// field out of a doc comment or a string literal does not merely over-report: it makes the
    /// register's counts unmaintainable, and an unmaintainable gate gets bumped past.
    #[test]
    fn the_lexer_reads_code_and_not_prose() {
        let source = "/// Doc prose about account.label and account.account_uuid.\n\
                      // A line comment mentioning a.label too.\n\
                      fn render() -> String {\n\
                      let _ = \"a string holding .label\";\n\
                      let _ = r#\"a raw string holding .account_uuid\"#;\n\
                      let _ = '\\'';\n\
                      account.label.clone()\n\
                      }\n";
        let scan = handle_reads(&non_test_region(source));
        assert_eq!(
            scan.reads,
            [("render".to_owned(), "label".to_owned())],
            "only the CODE read counts — comments, strings and raw strings are prose about it"
        );
    }

    /// The method-call exclusion in [`identity_field_at`] is safe only while [`Account`]'s
    /// identity fields stay plain fields. `.account_uuid()` is deliberately not a match because
    /// that spelling belongs to the OAuth capture types — but if `Account` ever grew an accessor,
    /// a resolver could read the handle through it and pass this gate untouched.
    #[test]
    fn the_identity_fields_stay_plain_fields() {
        let config = std::fs::read_to_string("src/config.rs").expect("readable");
        let production = non_test_region(&config);
        for accessor in ["fn label(", "fn account_uuid("] {
            assert!(
                !production.contains(accessor),
                "`Account` grew a {accessor}…) accessor. The handle-read scan matches FIELDS and \
                 skips method calls, so a resolver could now read a handle invisibly — widen \
                 `identity_field_at` before adding one"
            );
        }
    }

    // --- cooldown_active (pure) ---------------------------------------------

    #[test]
    fn cooldown_active_reflects_elapsed_vs_window() {
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1000);
        let cooldown = Duration::from_secs(60);
        // 30s after the last swap → still within the 60s window.
        let recent = SystemTime::UNIX_EPOCH + Duration::from_secs(970);
        assert!(cooldown_active(Some(recent), now, cooldown));
        // 90s after → window elapsed.
        let old = SystemTime::UNIX_EPOCH + Duration::from_secs(910);
        assert!(!cooldown_active(Some(old), now, cooldown));
        // No prior swap, and a zero window, are both never-in-cooldown.
        assert!(!cooldown_active(None, now, cooldown));
        assert!(!cooldown_active(Some(recent), now, Duration::ZERO));
    }

    // --- poll_viability classification --------------------------------------

    #[tokio::test]
    async fn poll_viability_classifies_each_poll_outcome() {
        let account = acct("spare", "u-B");
        let trigger = 0.98;
        // Each poll temporary lives for its full `.await` statement, and `&account`
        // is a fresh shared borrow per call — so the cases stay independent.
        let viable = poll_viability(
            &FakePoller::new(Probe::Live { weekly: 0.10 }),
            &account,
            trigger,
        )
        .await;
        assert_eq!(viable.unwrap(), Viability::Viable);
        let exhausted = poll_viability(
            &FakePoller::new(Probe::Live { weekly: 0.99 }),
            &account,
            trigger,
        )
        .await;
        assert_eq!(exhausted.unwrap(), Viability::WeeklyExhausted);
        // Both dead-credential statuses (401 rejected, 403 missing scope) → quarantined.
        let dead = poll_viability(&FakePoller::new(Probe::Dead), &account, trigger).await;
        assert_eq!(dead.unwrap(), Viability::Quarantined);
        let scope = poll_viability(&FakePoller::new(Probe::ScopeMissing), &account, trigger).await;
        assert_eq!(scope.unwrap(), Viability::Quarantined);
        // A locked keychain and a transient failure are NOT viability verdicts — they
        // propagate for the caller to abort on (gated) or tolerate (`--force`).
        let locked = poll_viability(&FakePoller::new(Probe::Locked), &account, trigger).await;
        assert!(matches!(locked, Err(Error::KeychainLocked { .. })));
        let transient = poll_viability(&FakePoller::new(Probe::Transient), &account, trigger).await;
        assert!(matches!(transient, Err(Error::UsageTransient { .. })));
    }

    // --- force_warning: the viability→warning DECISION (pure) ----------------

    #[test]
    fn force_warning_maps_each_viability_to_its_warning() {
        // The warn-and-proceed DECISION a forced swap emits (the `eprintln!` in
        // run_use is the thin wrapper). A viable target warns nothing; each non-
        // viable state carries its own specific warning — so AC#4/#5's "warns when
        // forcing onto an exhausted/quarantined target" is asserted, not just
        // inferred from the swap outcome.
        assert_eq!(force_warning(Viability::Viable, "spare"), None);
        assert_eq!(
            force_warning(Viability::WeeklyExhausted, "spare"),
            Some(warn_weekly_exhausted("spare"))
        );
        assert_eq!(
            force_warning(Viability::Quarantined, "spare"),
            Some(warn_quarantined("spare"))
        );
    }

    // --- cached viability classification (pure, issue #75) -------------------

    /// Build a daemon `status` line for the cached-viability classifier tests: only
    /// the fields the gate reads vary — handle, `quarantined`, `weekly_exhausted`,
    /// and whether a usage reading is present (a failed poll leaves both pct fields
    /// `None`, exactly as the daemon projects it).
    fn status_line(
        label: &str,
        quarantined: bool,
        weekly_exhausted: bool,
        weekly_pct: Option<u8>,
    ) -> AccountStatusLine {
        AccountStatusLine {
            label: label.to_owned(),
            active: false,
            enabled: true,
            quarantined,
            // The viability gate keys off `quarantined` (a recovering account is still
            // dead, still refused) — `recovering` (#109) is display-only, so a fixed
            // `false` here keeps these gate tests focused on the verdict they assert.
            recovering: false,
            session_pct: weekly_pct,
            weekly_pct,
            session_resets_at: None,
            weekly_resets_at: None,
            weekly_exhausted,
            // The #119 credential-health rollup is DISPLAY-only — `cached_viability_of`
            // keys off `quarantined` / `weekly_exhausted` / reading-presence, never the
            // rollup — so these are inert here.
            access_expires_at: None,
            refresh_health: None,
            health: None,
            // Bounded-blindness (#479) is a daemon `status`-snapshot concern, not read by the
            // cached-viability gate — inert here.
            blind_active: None,
            // Likewise the #882 refresh-token expiry modifier: surfaced, never acted upon, so the
            // viability gate does not read it.
            expiry: None,
        }
    }

    #[test]
    fn cached_viability_of_maps_each_line() {
        // A quarantined line is a usable verdict even with NO usage reading — the
        // daemon stops polling a dead account, so it carries no percentages.
        assert_eq!(
            cached_viability_of(&status_line("a", true, false, None)),
            Some(Viability::Quarantined)
        );
        // A weekly-exhausted line → WeeklyExhausted (the daemon's own verdict).
        assert_eq!(
            cached_viability_of(&status_line("a", false, true, Some(99))),
            Some(Viability::WeeklyExhausted)
        );
        // A healthy line WITH a fresh reading → Viable.
        assert_eq!(
            cached_viability_of(&status_line("a", false, false, Some(10))),
            Some(Viability::Viable)
        );
        // A healthy line with NO reading (the daemon's last poll failed, or it is
        // parked / unpolled) is NOT a verdict → `None` → the caller's live fallback.
        assert_eq!(
            cached_viability_of(&status_line("a", false, false, None)),
            None
        );
        // Quarantined takes priority over any (stale) exhausted flag.
        assert_eq!(
            cached_viability_of(&status_line("a", true, true, None)),
            Some(Viability::Quarantined)
        );
    }

    #[test]
    fn cached_viability_for_requires_a_unique_handle_match_on_both_sides() {
        // A unique handle match → its verdict.
        let unique = StatusResponse {
            systemic_refresh_failure: None,
            systemic_refresh_source: None,
            canonical_scrub: None,
            keychain_locked: false,
            canary: None,
            expiry_cohort: None,
            recent_blind_preempt_swap: None,
            recent_landing_overshoot: None,
            refresh_enabled: None,
            accounts: vec![
                status_line("work", false, false, Some(20)),
                status_line("spare", false, false, Some(10)),
            ],
            next_swap: None,
        };
        let roster = [
            acct("work", "u-A"),
            acct("spare", "u-B"),
            acct("ghost", "u-G"),
        ];
        assert_eq!(
            cached_viability_for(&unique, &roster, "spare"),
            Some(Viability::Viable)
        );
        // A handle absent from the reply → no cached reading → live fallback.
        assert_eq!(cached_viability_for(&unique, &roster, "ghost"), None);
        // A DUPLICATED handle cannot be disambiguated from the wire reply alone
        // (labels are not unique, and the reply carries no account-uuid) → live
        // fallback, never a guess.
        let duped = StatusResponse {
            systemic_refresh_failure: None,
            systemic_refresh_source: None,
            canonical_scrub: None,
            keychain_locked: false,
            canary: None,
            expiry_cohort: None,
            recent_blind_preempt_swap: None,
            recent_landing_overshoot: None,
            refresh_enabled: None,
            accounts: vec![
                status_line("dup", false, false, Some(10)),
                status_line("dup", true, false, None),
            ],
            next_swap: None,
        };
        assert_eq!(
            cached_viability_for(&duped, &[acct("dup", "u-D")], "dup"),
            None
        );
        // And the ROSTER side (issue #1201), which the wire count cannot see: ONE `dup` line
        // on the wire, TWO bearers on disk. The reply is perfectly unambiguous and still
        // cannot be attributed — the daemon adopts a new roster only on a signalled reload
        // (#139), so its single line need not be the named account's.
        let sole = StatusResponse {
            accounts: vec![status_line("dup", false, true, Some(99))],
            ..status_reply(Vec::new())
        };
        assert_eq!(
            cached_viability_for(&sole, &[acct("dup", "u-D"), acct("dup", "u-E")], "dup"),
            None,
            "two local bearers make the sole wire line un-attributable",
        );
        assert_eq!(
            cached_viability_for(&sole, &[acct("dup", "u-D")], "dup"),
            Some(Viability::WeeklyExhausted),
            "one bearer on each side is the only shape that carries a verdict",
        );
        // ZERO local bearers is unreachable from either production call site — both pass a
        // roster account's OWN label — and is answered rather than left to inference: a label
        // naming no local account degrades exactly like a duplicated one.
        assert_eq!(cached_viability_for(&sole, &[], "dup"), None);
    }

    // --- acceptance: viable use (#63) ---------------------------------------

    #[tokio::test]
    async fn viable_use_swaps_and_logs_reason_manual() {
        // `use spare` (viable) → the canonical item is rerouted to B's token, the
        // event logs reason=manual, and the confirmation is printed. (The
        // canonical-THEN-oauth write ORDERING is the swap engine's own, separately-
        // tested guarantee — reused unchanged.)
        let (result, store, stash, calls, log) =
            run("spare", false, false, Probe::Live { weekly: 0.10 }).await;
        assert!(result.is_ok(), "viable use should swap: {result:?}");
        assert_eq!(
            canonical(&store).await,
            b"B-token",
            "canonical rerouted to B"
        );
        // The outgoing account A was re-stashed with its fresh canonical token.
        assert_eq!(
            stash
                .read("Sessiometer/u-A")
                .await
                .unwrap()
                .credential
                .expose(),
            b"A-token"
        );
        assert!(
            log.contains("event=swap from=work to=spare reason=manual"),
            "log: {log}"
        );
        assert_eq!(calls, 1, "a one-shot command polls the target exactly once");
    }

    // --- acceptance: gate refusals without --force (#63) --------------------

    #[tokio::test]
    async fn weekly_exhausted_without_force_refuses_with_zero_writes() {
        let (result, store, stash, _calls, log) =
            run("spare", false, false, Probe::Live { weekly: 0.99 }).await;
        assert!(
            matches!(result, Err(Error::UseTargetWeeklyExhausted { ref label }) if label == "spare"),
            "got {result:?}"
        );
        // ZERO writes: the canonical item is untouched and A was not re-stashed.
        assert_eq!(canonical(&store).await, b"A-token");
        assert!(!log.contains("event=swap"), "no swap logged: {log}");
        let _ = stash;
    }

    #[tokio::test]
    async fn cooldown_active_without_force_refuses_with_zero_writes() {
        // in_cooldown=true → refuse before any poll or write.
        let (result, store, _stash, calls, log) =
            run("spare", false, true, Probe::Live { weekly: 0.10 }).await;
        assert!(
            matches!(result, Err(Error::UseCooldownActive)),
            "got {result:?}"
        );
        assert_eq!(canonical(&store).await, b"A-token", "ZERO writes");
        assert_eq!(calls, 0, "cooldown refuses before the viability poll");
        assert!(!log.contains("event=swap"), "no swap logged: {log}");
    }

    #[tokio::test]
    async fn quarantined_without_force_refuses_with_zero_writes() {
        let (result, store, _stash, _calls, log) = run("spare", false, false, Probe::Dead).await;
        assert!(
            matches!(result, Err(Error::UseTargetQuarantined { ref label }) if label == "spare"),
            "got {result:?}"
        );
        assert_eq!(canonical(&store).await, b"A-token", "ZERO writes");
        assert!(!log.contains("event=swap"), "no swap logged: {log}");
    }

    // --- acceptance: --force warns-and-proceeds (#63) -----------------------

    #[tokio::test]
    async fn force_onto_weekly_exhausted_warns_and_swaps_with_reason_forced() {
        let (result, store, _stash, _calls, log) =
            run("spare", true, false, Probe::Live { weekly: 0.99 }).await;
        assert!(
            result.is_ok(),
            "--force overrides weekly-exhausted: {result:?}"
        );
        assert_eq!(
            canonical(&store).await,
            b"B-token",
            "the forced swap rerouted to B"
        );
        assert!(log.contains("reason=forced"), "log: {log}");
    }

    #[tokio::test]
    async fn force_onto_quarantined_warns_and_swaps_with_reason_forced() {
        // The forced escape hatch CAN target a quarantined account (the autonomous
        // path, which selects via pick_target, structurally cannot — that invariant
        // is unchanged and separately tested in the daemon).
        let (result, store, _stash, _calls, log) = run("spare", true, false, Probe::Dead).await;
        assert!(result.is_ok(), "--force overrides quarantine: {result:?}");
        assert_eq!(canonical(&store).await, b"B-token");
        assert!(log.contains("reason=forced"), "log: {log}");
    }

    #[tokio::test]
    async fn force_with_a_transient_poll_proceeds_best_effort() {
        // D1: a transient poll failure only affects the (informational) warning, so
        // a forced swap proceeds without one rather than aborting.
        let (result, store, _stash, _calls, log) =
            run("spare", true, false, Probe::Transient).await;
        assert!(
            result.is_ok(),
            "a transient poll must not block a forced swap: {result:?}"
        );
        assert_eq!(canonical(&store).await, b"B-token");
        assert!(log.contains("reason=forced"), "log: {log}");
    }

    #[tokio::test]
    async fn transient_poll_without_force_aborts_with_zero_writes() {
        // D1: without --force, an unverifiable target (transient poll) aborts rather
        // than swapping blind — the gate only proceeds on a PROVEN-viable target.
        let (result, store, _stash, _calls, log) =
            run("spare", false, false, Probe::Transient).await;
        assert!(
            matches!(result, Err(Error::UsageTransient { .. })),
            "got {result:?}"
        );
        assert_eq!(canonical(&store).await, b"A-token", "ZERO writes");
        assert!(!log.contains("event=swap"), "log: {log}");
    }

    // --- acceptance: daemon-cached viability + live fallback (#75) -----------

    #[tokio::test]
    async fn cached_viable_target_swaps_without_a_live_poll() {
        // AC#1: with a running daemon holding a viable cached reading, `use` swaps on
        // that reading and issues ZERO usage-endpoint requests of its own. The poison
        // `Probe::Locked` would abort if the gate wrongly fell back to a live poll —
        // it does not, and the live-poll count is 0.
        let (result, store, stash, calls, log) =
            run_with_cache(Viability::Viable, "spare", false, false, Probe::Locked).await;
        assert!(result.is_ok(), "a cached-viable target swaps: {result:?}");
        assert_eq!(
            canonical(&store).await,
            b"B-token",
            "canonical rerouted to B"
        );
        // The outgoing account A was re-stashed with its fresh canonical token.
        assert_eq!(
            stash
                .read("Sessiometer/u-A")
                .await
                .unwrap()
                .credential
                .expose(),
            b"A-token"
        );
        assert!(
            log.contains("event=swap from=work to=spare reason=manual"),
            "log: {log}"
        );
        assert_eq!(
            calls, 0,
            "ZERO live polls — the gate used the cached reading"
        );
    }

    #[tokio::test]
    async fn cached_weekly_exhausted_refuses_without_a_live_poll() {
        // AC#2: a cached weekly-exhausted reading produces the SAME refusal as a live
        // one (UseTargetWeeklyExhausted), with ZERO live polls and ZERO writes.
        let (result, store, _stash, calls, log) = run_with_cache(
            Viability::WeeklyExhausted,
            "spare",
            false,
            false,
            Probe::Locked,
        )
        .await;
        assert!(
            matches!(result, Err(Error::UseTargetWeeklyExhausted { ref label }) if label == "spare"),
            "got {result:?}"
        );
        assert_eq!(canonical(&store).await, b"A-token", "ZERO writes");
        assert_eq!(calls, 0, "ZERO live polls — refused on the cached reading");
        assert!(!log.contains("event=swap"), "log: {log}");
    }

    #[tokio::test]
    async fn cached_quarantined_refuses_without_a_live_poll() {
        // AC#2: a cached quarantined reading → UseTargetQuarantined, ZERO live polls,
        // ZERO writes — the same refusal a live dead-credential poll produces.
        let (result, store, _stash, calls, log) =
            run_with_cache(Viability::Quarantined, "spare", false, false, Probe::Locked).await;
        assert!(
            matches!(result, Err(Error::UseTargetQuarantined { ref label }) if label == "spare"),
            "got {result:?}"
        );
        assert_eq!(canonical(&store).await, b"A-token", "ZERO writes");
        assert_eq!(calls, 0, "ZERO live polls — refused on the cached reading");
        assert!(!log.contains("event=swap"), "log: {log}");
    }

    #[tokio::test]
    async fn force_overrides_a_cached_exhausted_reading_without_a_live_poll() {
        // AC#2: --force still overrides a cached refusal (warn-and-proceed), deciding
        // the warning from the cached reading alone — ZERO live polls.
        let (result, store, _stash, calls, log) = run_with_cache(
            Viability::WeeklyExhausted,
            "spare",
            true,
            false,
            Probe::Locked,
        )
        .await;
        assert!(
            result.is_ok(),
            "--force overrides a cached weekly-exhausted: {result:?}"
        );
        assert_eq!(
            canonical(&store).await,
            b"B-token",
            "the forced swap rerouted to B"
        );
        assert!(log.contains("reason=forced"), "log: {log}");
        assert_eq!(
            calls, 0,
            "ZERO live polls — the warning used the cached reading"
        );
    }

    #[tokio::test]
    async fn no_daemon_falls_back_to_a_single_live_poll() {
        // AC#3: with no daemon (a cache MISS), `use` falls back to a single live poll
        // — today's behaviour. A viable live poll swaps, polling exactly once.
        let (result, store, _stash, calls, log) =
            run("spare", false, false, Probe::Live { weekly: 0.10 }).await;
        assert!(
            result.is_ok(),
            "the live fallback swaps a viable target: {result:?}"
        );
        assert_eq!(
            canonical(&store).await,
            b"B-token",
            "canonical rerouted to B"
        );
        assert!(
            log.contains("event=swap from=work to=spare reason=manual"),
            "log: {log}"
        );
        assert_eq!(calls, 1, "exactly one live fallback poll");
    }

    #[tokio::test]
    async fn rate_limited_live_fallback_surfaces_a_distinct_error() {
        // AC#3: a 429 on the live fallback is surfaced as the distinct, actionable
        // UseViabilityUnverifiable — NOT the opaque raw UsageRateLimited — with ZERO
        // writes. (Before #75 the raw 429 propagated and aborted `use` opaquely even
        // for a plainly-viable target.)
        let (result, store, _stash, calls, log) =
            run("spare", false, false, Probe::RateLimited).await;
        assert!(
            matches!(result, Err(Error::UseViabilityUnverifiable { ref label }) if label == "spare"),
            "got {result:?}"
        );
        assert_eq!(canonical(&store).await, b"A-token", "ZERO writes");
        assert_eq!(
            calls, 1,
            "one live fallback poll, then a clean abort (no busy-spin)"
        );
        assert!(!log.contains("event=swap"), "log: {log}");
    }

    #[tokio::test]
    async fn force_with_a_rate_limited_live_fallback_proceeds_best_effort() {
        // AC#3 + D1: under --force a 429 on the live fallback only costs the warning,
        // so the forced swap proceeds — it never surfaces UseViabilityUnverifiable.
        let (result, store, _stash, _calls, log) =
            run("spare", true, false, Probe::RateLimited).await;
        assert!(
            result.is_ok(),
            "a rate-limited poll must not block a forced swap: {result:?}"
        );
        assert_eq!(canonical(&store).await, b"B-token");
        assert!(log.contains("reason=forced"), "log: {log}");
    }

    // --- ControlSocketCache: the REAL client over a real socket (#75) --------

    #[tokio::test]
    async fn control_socket_cache_reads_a_cached_verdict_over_a_real_socket() {
        // The production [`ControlSocketCache`] round-trips the SAME newline-JSON
        // `status` exchange the daemon serves and the `status` command speaks: bind a
        // socket, serve one reply, and assert the client reads the target's cached
        // verdict — proving the real socket path (not just the pure classifiers) maps
        // a live reply to a viability without any usage-endpoint request.
        use tokio::io::{AsyncBufReadExt, AsyncWriteExt};
        use tokio::net::UnixListener;

        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("daemon.sock");
        let listener = UnixListener::bind(&socket).unwrap();

        let response = StatusResponse {
            systemic_refresh_failure: None,
            systemic_refresh_source: None,
            canonical_scrub: None,
            keychain_locked: false,
            canary: None,
            expiry_cohort: None,
            recent_blind_preempt_swap: None,
            recent_landing_overshoot: None,
            refresh_enabled: None,
            accounts: vec![
                status_line("work", false, false, Some(20)),
                status_line("spare", false, false, Some(10)),
            ],
            next_swap: None,
        };
        let wire = serde_json::to_string(&response).unwrap();
        // Server: accept one connection, expect the status request, reply once.
        let server = async move {
            let (stream, _addr) = listener.accept().await.unwrap();
            let mut buffered = tokio::io::BufReader::new(stream);
            let mut request = String::new();
            buffered.read_line(&mut request).await.unwrap();
            assert_eq!(request.trim_end(), r#"{"cmd":"status"}"#);
            buffered.write_all(wire.as_bytes()).await.unwrap();
            buffered.write_all(b"\n").await.unwrap();
            buffered.flush().await.unwrap();
        };

        let cache = ControlSocketCache { socket };
        let roster = [acct("work", "u-A"), acct("spare", "u-B")];
        let target = acct("spare", "u-B");
        let (_, verdict) = tokio::join!(server, cache.cached_viability(&roster, &target));
        assert_eq!(
            verdict,
            Some(Viability::Viable),
            "the real client reads the cached viable verdict over the socket"
        );
    }

    #[tokio::test]
    async fn control_socket_cache_misses_when_no_daemon_is_listening() {
        // No socket bound → the cache MISSES (`None`) so the gate falls back to a live
        // poll — a missing daemon must never block `use` (issue #75), the daemon-down
        // counterpart of the manual-hold notify's best-effort contract (#64).
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("daemon.sock"); // never bound
        let cache = ControlSocketCache { socket };
        let roster = [acct("work", "u-A"), acct("spare", "u-B")];
        let target = acct("spare", "u-B");
        assert_eq!(cache.cached_viability(&roster, &target).await, None);
    }

    /// The lagging-daemon half of the issue-#1201 reproduction: serve exactly one `status`
    /// reply on `listener`, then return. Split out so the test body below reads as the
    /// scenario rather than as socket plumbing.
    async fn serve_one_status(listener: tokio::net::UnixListener, response: &StatusResponse) {
        use tokio::io::{AsyncBufReadExt, AsyncWriteExt};

        let wire = serde_json::to_string(response).unwrap();
        let (stream, _addr) = listener.accept().await.unwrap();
        let mut buffered = tokio::io::BufReader::new(stream);
        let mut request = String::new();
        buffered.read_line(&mut request).await.unwrap();
        assert_eq!(request.trim_end(), r#"{"cmd":"status"}"#);
        buffered.write_all(wire.as_bytes()).await.unwrap();
        buffered.write_all(b"\n").await.unwrap();
        buffered.flush().await.unwrap();
    }

    /// A `status` reply carrying `accounts` and nothing else set — the shape the cache
    /// lookup reads (every other field is inert to [`cached_viability_of`]).
    fn status_reply(accounts: Vec<AccountStatusLine>) -> StatusResponse {
        StatusResponse {
            systemic_refresh_failure: None,
            systemic_refresh_source: None,
            canonical_scrub: None,
            keychain_locked: false,
            canary: None,
            expiry_cohort: None,
            recent_blind_preempt_swap: None,
            recent_landing_overshoot: None,
            refresh_enabled: None,
            accounts,
            next_swap: None,
        }
    }

    /// The roster on DISK for the pair below: two `work` bearers with distinct uuids, which
    /// `config_ab`'s fixture cannot express. Rebuilt per run because `run_on` consumes it.
    fn dup_work_config() -> Config {
        let mut config = config_ab();
        config.roster = vec![acct("work", "u-A"), acct("work", "u-B")];
        config
    }

    /// A snapshot from a daemon that has NOT reloaded the roster (issue #139): ONE `work`
    /// line, weekly-exhausted, and it belongs to `u-A`. Built from the daemon's own
    /// pre-reload roster rather than hand-listed, so "the single line is u-A's" is a property
    /// of the fixture and not of a comment.
    fn lagging_status() -> StatusResponse {
        let daemon_roster = [acct("work", "u-A")];
        let reply = status_reply(
            daemon_roster
                .iter()
                .map(|account| status_line(&account.label, false, true, Some(99)))
                .collect(),
        );
        assert_eq!(reply.accounts.len(), 1, "one line, and it is u-A's");
        reply
    }

    #[tokio::test]
    async fn a_uuid_named_target_is_not_answered_by_a_same_labelled_siblings_reading() {
        // Issue #1201, driven through the real `use` path rather than the predicate: two
        // accounts labelled `work`, the operator names the SECOND by uuid, and the daemon's
        // snapshot predates it. The roster count degrades, the gate falls through to `u-B`'s
        // OWN stashed token, and the swap proceeds.
        //
        // Why the uuid is the door. `resolve_target` refuses an ambiguous LABEL — asserted
        // below rather than assumed — but resolves a UUID query without complaint, and every
        // step past it keys on the label alone. Before this issue the wire count was the only
        // one, so the sole `work` line was read as `u-B`'s verdict; the same fixture measured
        // `Err(UseTargetWeeklyExhausted)` with ZERO live polls, i.e. a viable account refused
        // on its sibling's reading with the one lookup that could not have mistaken the
        // bearer never run.
        //
        // Real here: the socket, the production `ControlSocketCache`, `resolve_target`, and
        // the gate. Faked: only the seams `use` already injects (store, stash, poller,
        // notifier) — so no scripted verdict stands in for the lookup under test.
        use tokio::net::UnixListener;

        let disk = dup_work_config();
        assert!(
            matches!(
                resolve_target(&disk.roster, "work"),
                Err(Error::UseTargetAmbiguous { count: 2, .. })
            ),
            "the LABEL is ambiguous and refused: {:?}",
            resolve_target(&disk.roster, "work"),
        );
        assert_eq!(
            resolve_target(&disk.roster, "u-B").unwrap(),
            1,
            "the UUID resolves to the second bearer without complaint",
        );

        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("daemon.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let lagging = lagging_status();
        let cache = ControlSocketCache { socket };
        let (_, (result, store, _stash, calls, log)) = tokio::join!(
            serve_one_status(listener, &lagging),
            run_on(
                dup_work_config(),
                &cache,
                "u-B",
                false,
                false,
                Probe::Live { weekly: 0.10 },
            ),
        );
        assert!(
            result.is_ok(),
            "`u-B` is viable on its own token and must not inherit `u-A`'s reading: {result:?}",
        );
        assert_eq!(
            calls, 1,
            "the cache degraded, so exactly one live poll — of `u-B`'s own stashed token, \
             keyed by uuid — classified the named account",
        );
        assert_eq!(canonical(&store).await, b"B-token", "the swap reached u-B");
        assert!(log.contains("event=swap"), "log: {log}");
    }

    #[tokio::test]
    async fn a_lagging_daemons_reading_still_answers_for_a_labels_only_local_bearer() {
        // The other half, and the one that keeps the fix above from being "stop using the
        // cache": an identically-SHAPED lagging reply — one line, exhausted, built here
        // rather than reused from `lagging_status()` because `config_ab`'s bearer is
        // `spare` — read against a roster where the label has exactly ONE local bearer.
        // The cached verdict is honoured, the gate refuses, and issue #75's
        // "zero usage-endpoint requests when a daemon is up" holds — so the roster count
        // narrows the lookup to the case it cannot attribute, rather than switching it off.
        use tokio::net::UnixListener;

        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("daemon.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        // `spare` (u-B) is the only bearer of its label on BOTH sides; the reply's one line
        // carries its exhausted reading.
        let reply = status_reply(vec![status_line("spare", false, true, Some(99))]);
        let cache = ControlSocketCache { socket };
        let (_, (result, store, _stash, calls, log)) = tokio::join!(
            serve_one_status(listener, &reply),
            run_on(config_ab(), &cache, "u-B", false, false, Probe::Locked,),
        );
        assert!(
            matches!(result, Err(Error::UseTargetWeeklyExhausted { ref label }) if label == "spare"),
            "the cached verdict still gates a uniquely-labelled target: {result:?}",
        );
        assert_eq!(
            calls, 0,
            "a HIT issues no usage request (the poller here would abort if consulted)",
        );
        assert_eq!(
            canonical(&store).await,
            b"A-token",
            "the refusal writes nothing"
        );
        assert!(!log.contains("event=swap"), "log: {log}");
    }

    #[tokio::test]
    async fn an_ambiguous_wire_label_degrades_even_when_the_roster_is_unique() {
        // The mirror of the pair above, and it exists because mutating the two counts
        // SEPARATELY is what showed the coverage was asymmetric: dropping the roster count
        // reddens a unit test AND a full `run_use` path, while dropping the wire count reddened
        // the unit test alone — so the conjunct that predates this issue had no end-to-end pin
        // at all. Here `spare` names exactly one account on disk and TWO lines on a wire this
        // process cannot reconcile, so the cached verdict is un-attributable from the other
        // direction and the same live fallback runs.
        use tokio::net::UnixListener;

        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("daemon.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let reply = status_reply(vec![
            status_line("spare", false, true, Some(99)),
            status_line("spare", false, false, Some(10)),
        ]);
        let cache = ControlSocketCache { socket };
        let (_, (result, store, _stash, calls, log)) = tokio::join!(
            serve_one_status(listener, &reply),
            run_on(
                config_ab(),
                &cache,
                "u-B",
                false,
                false,
                Probe::Live { weekly: 0.10 },
            ),
        );
        assert!(
            result.is_ok(),
            "two `spare` lines carry no attributable verdict, so the gate polls: {result:?}",
        );
        assert_eq!(
            calls, 1,
            "exactly one live poll classified the named account"
        );
        assert_eq!(canonical(&store).await, b"B-token", "the swap reached u-B");
        assert!(log.contains("event=swap"), "log: {log}");
    }

    // --- the roster at the two `--force` sites (issue #1246) -----------------
    //
    // `run_use` threads `&config.roster` into the cache lookup from THREE places: the
    // gated path (`SwapTarget::resolve`, pinned by the trio above), and the two that
    // reach it through `warn_if_forcing_onto_non_viable` — adopt-target recovery and
    // the plain `--force` bypass. The two below pin those. Each drives the PRODUCTION
    // `ControlSocketCache` over a real socket, because the roster only changes an
    // answer inside `cached_viability_for`; the older force-path tests run on
    // `FakeCache`, whose verdict is scripted, so no roster it is handed can change it.
    //
    // The shape both use: a target whose label bears exactly once on BOTH sides is a
    // cache HIT, so the poller is never consulted — and the poller is armed with
    // `Probe::Locked`, which ABORTS if it ever is. Substituting `&[]` for the roster
    // at the site under test counts zero bearers, discards the verdict, and issues
    // that poll, so `calls == 0` is the assertion that carries the pin.

    #[tokio::test]
    async fn a_forced_swap_reads_a_uniquely_labelled_targets_cached_verdict() {
        // The `--force` site. `--force` bypasses the POLICY gate but still consults the
        // daemon's cached verdict to decide whether to WARN, and that lookup is the one
        // issue #75 prices: zero usage-endpoint requests while a daemon is up. An empty
        // roster there would discard a perfectly good verdict on every forced swap and
        // poll live instead — the exact cost the doc comment weighs as the price of
        // degrading, paid unconditionally.
        use tokio::net::UnixListener;

        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("daemon.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        // `spare` (u-B) bears its label exactly once in `config_ab` and once on the wire.
        let reply = status_reply(vec![status_line("spare", false, true, Some(99))]);
        let cache = ControlSocketCache { socket };
        let (_, (result, store, _stash, calls, log)) = tokio::join!(
            serve_one_status(listener, &reply),
            run_on(config_ab(), &cache, "u-B", true, false, Probe::Locked),
        );
        assert_eq!(
            calls, 0,
            "the forced path must read the cached verdict: a roster counting zero bearers \
             for `spare` degrades to a live poll, and this poller aborts if consulted",
        );
        // …and the path was actually WALKED — `calls == 0` alone is also what a forced
        // swap that never reached the lookup would report.
        assert!(
            result.is_ok(),
            "`--force` warns on the exhausted target and proceeds: {result:?}",
        );
        assert_eq!(canonical(&store).await, b"B-token", "the swap reached u-B");
        assert!(
            log.contains("event=swap from=work to=spare reason=forced"),
            "log: {log}",
        );
    }

    #[tokio::test]
    async fn an_adopting_forced_swap_reads_a_uniquely_labelled_targets_cached_verdict() {
        // The adopt-target site (#212): the canonical is scrubbed and the display cleared,
        // so the normal re-stash swap cannot run and `--force` installs the target directly.
        // It reaches the SAME `warn_if_forcing_onto_non_viable`, threading its own copy of
        // the roster, and nothing pinned that copy — the recovery path is the one an
        // operator hits when a session is already broken, so degrading it to a live poll
        // costs a usage request exactly when the credential state is least dependable.
        use tokio::net::UnixListener;

        let (store, stash) = seeded_store_and_stash().await;
        store.set_not_found(true); // the scrubbed canonical → the adopt branch
        let (_json_dir, json) = claude_json_for("u-UNKNOWN"); // display cleared too

        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("daemon.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let reply = status_reply(vec![status_line("spare", false, true, Some(99))]);
        let cache = ControlSocketCache { socket };
        let (_, (result, calls, log)) = tokio::join!(
            serve_one_status(listener, &reply),
            run_use_over_cache(&cache, &store, &stash, &json, "spare", true, Probe::Locked),
        );
        assert_eq!(
            calls, 0,
            "adopt-target must read the cached verdict too: a roster counting zero bearers \
             for `spare` degrades to a live poll, and this poller aborts if consulted",
        );
        // …and this is the ADOPT branch, not the ordinary forced swap: the sentinel `from=`
        // is what distinguishes them, and it is only reachable with the canonical gone.
        assert!(
            result.is_ok(),
            "adopt warns on the exhausted target and recovers: {result:?}",
        );
        assert_eq!(
            canonical(&store).await,
            b"B-token",
            "adopted spare into the absent canonical",
        );
        assert!(
            log.contains("event=swap from=(unknown) to=spare reason=forced"),
            "log: {log}",
        );
    }

    // --- daemon-routed swap (issue #167): request_swap + ack mapping ---------

    #[tokio::test]
    async fn request_swap_returns_none_when_no_daemon_is_reachable() {
        // Daemon-DOWN fallback: an absent / refused socket is the "no daemon" signal → `Ok(None)`, so
        // `use` falls back to the standalone write path below. The decision is the CONNECT alone, so
        // NOTHING was sent — `use` can then write standalone with no risk of a double write. This is
        // the daemon-down half of the unify AC (the daemon-up half is `request_swap_reads_a_daemon_
        // ack_over_a_real_socket`), the counterpart of the cache's own best-effort miss (#75).
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("absent.sock"); // never bound
        let ack = crate::daemon::request_swap(&missing, "spare", false)
            .await
            .unwrap();
        assert!(
            ack.is_none(),
            "no reachable daemon ⇒ fall back to the standalone write path",
        );
    }

    #[tokio::test]
    async fn request_swap_reads_a_daemon_ack_over_a_real_socket() {
        // Daemon-UP unify: with a daemon listening, `request_swap` sends the redacted command and
        // reads back the ack — the load-bearing route-THROUGH case (one writer, one place for the
        // lock / write-ordering / redaction). A minimal fake daemon reads the request line, asserts it
        // carries only the target + force (never a credential), and replies with a canned redacted
        // ack; `request_swap` decodes it into the shared wire type.
        use tokio::io::{AsyncBufReadExt, AsyncWriteExt};
        use tokio::net::UnixListener;

        let dir = tempfile::tempdir().unwrap();
        let sock = dir.path().join("daemon.sock");
        let listener = UnixListener::bind(&sock).unwrap();

        let server = async {
            let (stream, _addr) = listener.accept().await.unwrap();
            let mut buffered = tokio::io::BufReader::new(stream);
            let mut line = String::new();
            buffered.read_line(&mut line).await.unwrap();
            // The request carries the operator's target + force — and NOTHING secret.
            let request: serde_json::Value = serde_json::from_str(line.trim_end()).unwrap();
            assert_eq!(request["cmd"], "swap");
            assert_eq!(request["target"], "spare");
            assert_eq!(request["force"], true);
            // Reply with a redacted completed-swap ack (two labels, no secret).
            let ack = serde_json::to_string(&SwapAck::Accepted {
                from: "work".to_owned(),
                to: "spare".to_owned(),
            })
            .unwrap();
            buffered.write_all(ack.as_bytes()).await.unwrap();
            buffered.write_all(b"\n").await.unwrap();
            buffered.flush().await.unwrap();
        };

        let client = crate::daemon::request_swap(&sock, "spare", true);
        let (_, ack) = tokio::join!(server, client);
        assert_eq!(
            ack.unwrap(),
            Some(SwapAck::Accepted {
                from: "work".to_owned(),
                to: "spare".to_owned(),
            }),
            "the client reads the daemon's redacted ack over the socket",
        );
    }

    #[test]
    fn report_swap_ack_returns_ok_for_a_completed_or_already_active_swap() {
        // A completed / already-active ack is a SUCCESS — the confirmation is printed and the write
        // already happened daemon-side, so `use` exits 0 (the same outcome the standalone path gives).
        assert!(report_swap_ack(
            SwapAck::Accepted {
                from: "work".to_owned(),
                to: "spare".to_owned(),
            },
            "spare",
        )
        .is_ok());
        assert!(report_swap_ack(
            SwapAck::AlreadyActive {
                to: "spare".to_owned(),
            },
            "spare",
        )
        .is_ok());
    }

    #[test]
    fn report_swap_ack_surfaces_a_rejection_as_the_typed_error() {
        // A rejection becomes the typed error (hence the standalone exit code): here the gate-refused
        // exit 7 for a quarantined target.
        let err = report_swap_ack(
            SwapAck::Rejected {
                reason: SwapRejection::Quarantined,
            },
            "spare",
        )
        .unwrap_err();
        assert_eq!(err.exit_code(), 7);
    }

    #[test]
    fn swap_rejection_error_maps_each_reason_to_the_standalone_exit_taxonomy() {
        // AC (unify leaves `use`'s exit codes UNCHANGED): routing a swap THROUGH the daemon must map
        // each redacted rejection to the SAME typed error — hence exit code — the standalone path
        // raises. Pin the FULL table so a reason ⇄ exit drift can't ship green.
        let q = "spare";
        assert_eq!(
            swap_rejection_error(SwapRejection::UnknownTarget, q).exit_code(),
            5
        );
        assert_eq!(
            swap_rejection_error(SwapRejection::AmbiguousTarget, q).exit_code(),
            6
        );
        assert_eq!(
            swap_rejection_error(SwapRejection::Quarantined, q).exit_code(),
            7
        );
        assert_eq!(
            swap_rejection_error(SwapRejection::WeeklyExhausted, q).exit_code(),
            7
        );
        assert_eq!(
            swap_rejection_error(SwapRejection::Cooldown, q).exit_code(),
            7
        );
        assert_eq!(
            swap_rejection_error(SwapRejection::NoActiveAccount, q).exit_code(),
            1
        );
        assert_eq!(
            swap_rejection_error(SwapRejection::KeychainLocked, q).exit_code(),
            4
        );
        assert_eq!(
            swap_rejection_error(SwapRejection::SwapLockBusy, q).exit_code(),
            4
        );
        assert_eq!(
            swap_rejection_error(SwapRejection::Failed, q).exit_code(),
            1
        );
    }

    #[test]
    fn swap_rejection_error_names_the_operator_query_not_a_daemon_label() {
        // The rejection carries NO label (redaction #15), so the operator's own `query` names the
        // target in the surfaced message — non-secret operator input, never a daemon-side echo.
        let err = swap_rejection_error(SwapRejection::UnknownTarget, "my-target");
        assert!(err.to_string().contains("my-target"), "got {err}");
    }

    #[test]
    fn next_swap_target_takes_the_daemons_published_pick_and_carries_its_reason() {
        // Issue #960's load-bearing claim: `--next` READS the daemon's `next_swap`; it never
        // re-derives selection. So the resolved label is whatever the wire said — verbatim — and
        // the daemon's own rationale rides along for the note, including the pre-#393 `None`.
        const NOW: i64 = 1_800_000_000;
        for reason in [
            Some(NextSwapReason::SoonestReset {
                resets_at: NOW + 3_600,
            }),
            Some(NextSwapReason::OnlyCandidate),
            Some(NextSwapReason::RosterOrder),
            None,
        ] {
            let picked = NextSwap::Target {
                to: "spare".to_owned(),
                reason,
            };
            assert_eq!(
                next_swap_target(Some(&picked), NOW).unwrap(),
                ("spare".to_owned(), reason),
                "the wire's own pick, carried through unchanged: {reason:?}",
            );
        }
    }

    #[test]
    fn next_swap_target_reports_the_relief_hint_rather_than_a_bare_no_target() {
        // Issue #960 AC: when every candidate is excluded, do NOT throw away the #405 hint —
        // report WHY the fleet is blocked and WHEN capacity returns. A genuine gate refusal, so
        // it takes the exit-`7` gate-refused code its per-target siblings already own.
        const NOW: i64 = 1_800_000_000;
        let blocked = NextSwap::NoViableTarget {
            cause: Some(NoTargetCause::Weekly),
            resets_at: Some(NOW + 2 * 86_400),
        };
        let err = next_swap_target(Some(&blocked), NOW).unwrap_err();
        assert_eq!(err.exit_code(), 7, "a fleet-wide gate refusal is exit 7");
        let message = err.to_string();
        assert!(
            message.contains("out of capacity") && message.contains("resets in 2d"),
            "carries the relief hint, not a content-free 'no target': {message}",
        );
        assert!(
            message.contains("add an account"),
            "a multi-day wait is a STRUCTURAL shortage, so the nudge fires: {message}",
        );
        // A pre-#405 daemon sends no `cause` — say only what the wire substantiates, exactly as
        // the `status` footer's bare fallback does. Still a refusal, so still exit `7`.
        let bare = NextSwap::NoViableTarget {
            cause: None,
            resets_at: None,
        };
        let err = next_swap_target(Some(&bare), NOW).unwrap_err();
        assert_eq!(err.exit_code(), 7);
        assert!(
            err.to_string().contains("no viable target"),
            "bare fallback: {err}",
        );
    }

    #[test]
    fn next_swap_target_separates_an_unrunnable_selection_from_a_refused_one() {
        // Issue #960: the daemon being UNABLE to answer is not the daemon REFUSING. Both fail
        // closed with zero writes, but only the refusal is exit `7` — an inability takes the
        // generic `1`, the same distinction `UseViabilityUnverifiable` draws for the un-runnable
        // viability gate. Collapsing them would tell a supervisor "no capacity, back off" when
        // the daemon had merely not polled yet.
        const NOW: i64 = 1_800_000_000;
        let awaiting = NextSwap::AwaitingData;
        let err = next_swap_target(Some(&awaiting), NOW).unwrap_err();
        assert_eq!(
            err.exit_code(),
            1,
            "not-yet-polled is an inability, not a refusal"
        );
        assert!(
            err.to_string().contains("has not polled the rotation yet"),
            "names WHICH inability: {err}",
        );
        // No candidate published at all: a current daemon with no active account to anchor a swap
        // FROM, or a pre-#88 daemon that omits the field. Distinct message, same generic `1`.
        let err = next_swap_target(None, NOW).unwrap_err();
        assert_eq!(err.exit_code(), 1);
        assert!(
            err.to_string().contains("published no next-swap candidate"),
            "names WHICH inability: {err}",
        );
    }

    #[test]
    fn the_next_target_note_names_the_account_and_the_daemons_reason() {
        // Issue #960 AC: the operator did NOT name this target, so an ack that does not say which
        // account it moved to is unreadable. The note carries the label plus the daemon's own
        // rationale — the SAME wording the `status` footer uses, so `status`' prediction and
        // `--next`'s action are visibly one decision.
        assert_eq!(
            note_next_target(
                "spare",
                Some(NextSwapReason::SoonestReset {
                    resets_at: 1_800_003_600
                })
            ),
            "note: --next advancing to `spare` (weekly resets soonest)",
        );
        assert_eq!(
            note_next_target("spare", Some(NextSwapReason::OnlyCandidate)),
            "note: --next advancing to `spare` (only viable target)",
        );
        assert_eq!(
            note_next_target("spare", Some(NextSwapReason::RosterOrder)),
            "note: --next advancing to `spare` (first eligible; no reset times known)",
        );
        // A pre-#393 daemon sends no reason → the bare label, the honest fallback.
        assert_eq!(
            note_next_target("spare", None),
            "note: --next advancing to `spare`",
        );
    }

    #[test]
    fn ack_falls_back_to_standalone_only_for_no_active_account() {
        // The fallback discriminator (issue #167 / #212): EXACTLY the `NoActiveAccount` rejection
        // falls back to the standalone adopt path — the one guaranteed-zero-write verdict-time reject
        // where the daemon performed nothing and `use <spare> --force` must still be able to adopt a
        // named spare while the daemon runs. NO other ack falls back: a completed / already-active
        // swap is authoritative, and each of the OTHER SIX rejections is a policy/safety verdict the
        // daemon already resolved (falling back on those would double-act or wrongly override). A
        // reached-but-failed `Err` never reaches this predicate — `request_swap` surfaces it and the
        // `?` propagates it BEFORE the match — so it too can never fall back (the daemon may already
        // have written; a standalone retry could double-write).
        assert!(
            ack_falls_back_to_standalone(&SwapAck::Rejected {
                reason: SwapRejection::NoActiveAccount,
            }),
            "NoActiveAccount MUST fall back to standalone adopt recovery",
        );
        // Completed / already-active acks are authoritative — never fall back.
        for ack in [
            SwapAck::Accepted {
                from: "work".to_owned(),
                to: "spare".to_owned(),
            },
            SwapAck::AlreadyActive {
                to: "spare".to_owned(),
            },
        ] {
            assert!(
                !ack_falls_back_to_standalone(&ack),
                "a completed/already-active ack must NOT fall back: {ack:?}",
            );
        }
        // Every OTHER rejection is authoritative — never fall back. Exhaustive over the non-
        // NoActiveAccount rejections so a future rejection variant forces a deliberate decision here.
        for reason in [
            SwapRejection::UnknownTarget,
            SwapRejection::AmbiguousTarget,
            SwapRejection::Quarantined,
            SwapRejection::WeeklyExhausted,
            SwapRejection::Cooldown,
            SwapRejection::KeychainLocked,
            SwapRejection::SwapLockBusy,
            SwapRejection::Failed,
        ] {
            assert!(
                !ack_falls_back_to_standalone(&SwapAck::Rejected { reason }),
                "the {reason:?} rejection is authoritative and must NOT fall back",
            );
        }
    }

    // --- acceptance: not-found / ambiguous through run_use (#63) -------------

    #[tokio::test]
    async fn unresolvable_target_aborts_with_zero_writes() {
        let (result, store, _stash, calls, log) =
            run("ghost", false, false, Probe::Live { weekly: 0.10 }).await;
        assert!(
            matches!(result, Err(Error::UseTargetNotFound { .. })),
            "got {result:?}"
        );
        assert_eq!(canonical(&store).await, b"A-token", "ZERO writes");
        assert_eq!(
            calls, 0,
            "an unresolvable target is rejected before any poll"
        );
        assert!(!log.contains("event=swap"), "log: {log}");
    }

    #[tokio::test]
    async fn ambiguous_target_aborts_with_zero_writes() {
        // The `ambiguous` half this section's heading has always named and never had (issue
        // #1087). Its sibling above covers not-found; nothing covered a DUPLICATED label through
        // `run_use`, and the two are not the same claim — `UseTargetNotFound` resolves to nothing
        // so there is nothing a swallow could pick, whereas an ambiguous query has two perfectly
        // usable bearers sitting right there. A `resolve_target(…).unwrap_or(0)` here would swap
        // the operator onto the earliest bearer of a label they did not disambiguate, which is
        // exactly the first-match-wins harm OQ-1 removed from `enable`/`disable`/`remove`, and
        // `resolve_target`'s own test cannot see it: the resolver would still be refusing.
        let mut config = config_ab();
        config.roster.push(acct("spare", "u-C"));
        let (result, store, _stash, calls, log) = run_on(
            config,
            &FakeCache::miss(),
            "spare",
            false,
            false,
            Probe::Live { weekly: 0.10 },
        )
        .await;
        assert!(
            matches!(result, Err(Error::UseTargetAmbiguous { count: 2, ref query }) if query == "spare"),
            "a duplicated label must refuse, not resolve to one of its bearers: got {result:?}"
        );
        assert_eq!(
            result.unwrap_err().exit_code(),
            6,
            "an ambiguous target exits 6, where not-found exits 5"
        );
        assert_eq!(canonical(&store).await, b"A-token", "ZERO writes");
        assert_eq!(calls, 0, "an ambiguous target is rejected before any poll");
        assert!(!log.contains("event=swap"), "log: {log}");
    }

    // --- acceptance: already-active (#63) -----------------------------------

    #[tokio::test]
    async fn already_active_without_force_is_a_noop_success_with_zero_writes() {
        // `use work` when work (u-A) is already active → no-op success, no write.
        let (result, store, _stash, calls, log) =
            run("work", false, false, Probe::Live { weekly: 0.10 }).await;
        assert!(
            result.is_ok(),
            "already-active is a no-op success: {result:?}"
        );
        assert_eq!(canonical(&store).await, b"A-token", "ZERO writes");
        assert_eq!(calls, 0, "already-active short-circuits before the poll");
        assert!(!log.contains("event=swap"), "no swap logged: {log}");
    }

    #[tokio::test]
    async fn already_active_with_force_allows_a_rewrite() {
        // `use work --force` when work is already active → a re-write is allowed (a
        // self-swap re-stashes + rewrites the same token, harmless).
        let (result, store, _stash, _calls, log) =
            run("work", true, false, Probe::Live { weekly: 0.10 }).await;
        assert!(
            result.is_ok(),
            "--force allows a re-write of the active account: {result:?}"
        );
        // The canonical item ends up holding A's own (re-written) token.
        assert_eq!(canonical(&store).await, b"A-token");
        assert!(
            log.contains("event=swap from=work to=work reason=forced"),
            "log: {log}"
        );
    }

    /// Issue #1001 / PRD AC-2a — `use --force <active-label>` really does replace the
    /// canonical bytes when the account's stash has DIVERGED from them, which is precisely
    /// the state a `migration import` leaves behind: the imported credential staged into
    /// `Sessiometer/u-A` while the canonical still holds the pre-import token.
    ///
    /// The sibling above cannot show this. It runs on the shared fixture where the stash and
    /// the canonical BOTH hold `A-token`, so its `canonical == b"A-token"` is satisfied by a
    /// no-op just as well as by a real adoption — it distinguishes `--force` from the
    /// unqualified form only by the logged event, never by the bytes. Import's report tells
    /// the operator to run this command; without this test, that instruction's *effect* is
    /// documented and unasserted.
    ///
    /// The blobs are WELL-FORMED Claude Code credentials, and that is a fidelity
    /// requirement rather than decoration. Staging over A's stash leaves the canonical
    /// matching NO stash, which routes the #730 canary to `NoStashMatch` — and there the
    /// shape-check decides: a well-formed orphan fails OPEN (a benign not-yet-restashed CC
    /// token) and the swap proceeds, while an unparseable one is refused with
    /// `CanaryUnparseableCanonical` to protect an unrelated secret from the `-U` clobber.
    /// A production canonical is real CC JSON, so it is the well-formed branch this must
    /// exercise; the shared fixture's bare `b"A-token"` takes the refusal branch instead and
    /// would make this test assert the opposite of the shipped behaviour.
    ///
    /// There is deliberately NO stash assertion here — read design § 4.1's implementer note
    /// before adding one. With `outgoing_stash == incoming_stash`, step 2 re-stashes the
    /// outgoing canonical blob back over the freshly-imported stash, so the stash ends up
    /// holding the STALE token. Adoption still succeeds — `incoming` is read BEFORE that
    /// write — which is why the canonical is the only thing worth asserting, and why a stash
    /// assertion would read as a failure while the behaviour is correct.
    #[tokio::test]
    async fn force_adopts_a_freshly_imported_credential_for_the_already_active_account() {
        /// A well-formed CC credential carrying `access` as its bearer — the
        /// `{"claudeAiOauth":{accessToken,refreshToken,expiresAt}}` shape #730 recognizes.
        fn cc_credential(access: &str) -> Vec<u8> {
            format!(
                r#"{{"claudeAiOauth":{{"accessToken":"{access}","refreshToken":"sk-ant-ort-RT","expiresAt":1700000000000}}}}"#
            )
            .into_bytes()
        }
        let pre_import = cc_credential("sk-ant-oat-STALE");
        let imported = cc_credential("sk-ant-oat-IMPORTED");

        let config = config_ab();
        let store = FakeCredentialStore::empty();
        store.write(&cred(&pre_import)).await.unwrap();
        let stash = FakeAccountStash::empty();
        // The state `import` leaves: A's own stash now holds the artifact's credential,
        // while the canonical — the only item Claude Code reads — still holds the old one.
        stash
            .write("Sessiometer/u-A", &stashed(&imported, "u-A"))
            .await
            .unwrap();
        stash
            .write("Sessiometer/u-B", &stashed(b"B-token", "u-B"))
            .await
            .unwrap();
        assert_eq!(
            canonical(&store).await,
            pre_import,
            "precondition: the canonical is STALE relative to the staged stash"
        );

        let (_json_dir, json) = claude_json_for("u-A");
        let log_dir = tempfile::tempdir().unwrap();
        let log_path = log_dir.path().join("sessiometer.log");
        let mut log = EventLog::at(&log_path).unwrap();
        let poller = FakePoller::new(Probe::Live { weekly: 0.10 });
        let lock_dir = tempfile::tempdir().unwrap();
        let lock_path = lock_dir.path().join("swap.lock");
        let notifier = FakeNotifier::ok();

        let result = run_use(
            &config,
            "work",
            true, // --force — the token import's report names, and the reason it works
            false,
            Seams {
                cache: &FakeCache::miss(),
                poller: &poller,
                store: &store,
                stash: &stash,
                claude_json: &json,
                lock_path: &lock_path,
                notifier: &notifier,
            },
            &mut log,
        )
        .await;

        assert!(result.is_ok(), "the named command must succeed: {result:?}");
        assert_eq!(
            canonical(&store).await,
            imported,
            "AC-2a: the command the import report names must actually replace the canonical \
             bytes — this is the assertion the unqualified `use work` fails, since it \
             short-circuits on service-name equality and writes nothing"
        );
    }

    // --- acceptance: keychain locked, always (even with --force) (#63) -------

    #[tokio::test]
    async fn keychain_locked_aborts_with_the_locked_exit_code_and_zero_writes() {
        // SAFETY (always enforced, even with --force): a locked keychain aborts with
        // the locked exit code (4) and ZERO writes, and does NOT busy-spin (the
        // target is polled at most once — a one-shot command, never the daemon loop).
        for force in [false, true] {
            let (result, store, _stash, calls, log) =
                run("spare", force, false, Probe::Locked).await;
            let err = result.expect_err("a locked keychain must abort");
            assert!(
                matches!(err, Error::KeychainLocked { .. }),
                "force={force}: {err:?}"
            );
            assert_eq!(err.exit_code(), 4, "the locked exit code");
            assert_eq!(
                canonical(&store).await,
                b"A-token",
                "force={force}: ZERO writes"
            );
            assert!(
                calls <= 1,
                "force={force}: no busy-spin (polled at most once)"
            );
            assert!(!log.contains("event=swap"), "force={force}: no swap logged");
        }
    }

    // --- acceptance: active account unresolvable -----------------------------

    #[tokio::test]
    async fn unresolvable_active_account_aborts_before_swapping() {
        // Fail-closed (issue #207, token-first): the canonical token matches NO stash
        // AND ~/.claude.json names an account not in the roster → the outgoing account
        // is genuinely unknown, so the swap (which re-stashes it) cannot run. ZERO
        // writes.
        let config = config_ab();
        let (store, stash) = seeded_store_and_stash().await;
        // Overwrite the canonical with an ORPHAN token no stash holds, so neither the
        // token match nor the (unresolvable u-UNKNOWN) display resolves the active.
        store.write(&cred(b"ORPHAN-token")).await.unwrap();
        let (_json_dir, json) = claude_json_for("u-UNKNOWN");
        let log_dir = tempfile::tempdir().unwrap();
        let mut log = EventLog::at(&log_dir.path().join("sessiometer.log")).unwrap();
        let poller = FakePoller::new(Probe::Live { weekly: 0.10 });
        let lock_dir = tempfile::tempdir().unwrap();
        let lock_path = lock_dir.path().join("swap.lock");
        let notifier = FakeNotifier::ok();
        // The active account is unresolvable, so the gate (and its cache query) is
        // never reached — a miss cache that, like the poller, must go untouched.
        let cache = FakeCache::miss();
        let result = run_use(
            &config,
            "spare",
            false,
            false,
            Seams {
                cache: &cache,
                poller: &poller,
                store: &store,
                stash: &stash,
                claude_json: &json,
                lock_path: &lock_path,
                notifier: &notifier,
            },
            &mut log,
        )
        .await;
        assert_eq!(
            cache.calls.get(),
            0,
            "the gate (and its cache query) is reached only after active resolution"
        );
        // The swap never ran, so the daemon was never notified (no manual hold to
        // signal). ZERO writes AND zero notifications.
        assert_eq!(notifier.calls.get(), 0, "an aborted swap must not notify");
        assert!(
            matches!(result, Err(Error::ActiveAccountUnresolved)),
            "got {result:?}"
        );
        assert_eq!(canonical(&store).await, b"ORPHAN-token", "ZERO writes");
    }

    // --- acceptance: token-first active resolution (issue #207) --------------

    /// Run `run_use` over a caller-built `store` + `json` path — the #207 tests must
    /// DIVERGE the canonical token from the display (or lock the store), which the
    /// `u-A`-pinned `run` helper cannot express. Returns the result and the log text.
    async fn run_use_over(
        store: &FakeCredentialStore,
        stash: &FakeAccountStash,
        json: &Path,
        query: &str,
        force: bool,
        probe: Probe,
    ) -> (Result<()>, String) {
        let (result, _calls, log) =
            run_use_over_cache(&FakeCache::miss(), store, stash, json, query, force, probe).await;
        (result, log)
    }

    /// [`run_use_over`] over a caller-supplied cache seam rather than the
    /// [`FakeCache::miss`] default, also reporting the live-poll count — what the
    /// ADOPT-TARGET recovery path needs to be driven against the PRODUCTION
    /// [`ControlSocketCache`] (issue #1246). Generic over the seam for exactly the
    /// reason [`run_on`] is: a scripted verdict proves what the path does with an
    /// answer, never which answer the real lookup returns for which roster — and the
    /// roster is the subject here. `run_on` cannot stand in: it seeds a PRESENT
    /// canonical that resolves, so `adopt` is never true and this site never runs.
    async fn run_use_over_cache<R: CachedViabilitySource>(
        cache: &R,
        store: &FakeCredentialStore,
        stash: &FakeAccountStash,
        json: &Path,
        query: &str,
        force: bool,
        probe: Probe,
    ) -> (Result<()>, u32, String) {
        let config = config_ab();
        let log_dir = tempfile::tempdir().unwrap();
        let log_path = log_dir.path().join("sessiometer.log");
        let mut log = EventLog::at(&log_path).unwrap();
        let poller = FakePoller::new(probe);
        let lock_dir = tempfile::tempdir().unwrap();
        let lock_path = lock_dir.path().join("swap.lock");
        let notifier = FakeNotifier::ok();
        let result = run_use(
            &config,
            query,
            force,
            false,
            Seams {
                cache,
                poller: &poller,
                store,
                stash,
                claude_json: json,
                lock_path: &lock_path,
                notifier: &notifier,
            },
            &mut log,
        )
        .await;
        let log_text = std::fs::read_to_string(&log_path).unwrap_or_default();
        (result, poller.calls.get(), log_text)
    }

    #[tokio::test]
    async fn token_match_recovers_a_swap_when_the_display_is_cleared() {
        // The #207 fix: ~/.claude.json's oauthAccount is STALE (names an account not
        // in the roster — the out-of-band "forced logout" clobber), but the canonical
        // token still byte-matches work's (u-A) stash. `use spare` must resolve the
        // outgoing account TOKEN-FIRST and swap — where the old display-only
        // resolution hard-failed `ActiveAccountUnresolved` ("can't recover").
        let (store, stash) = seeded_store_and_stash().await; // canonical = A-token
        let (_json_dir, json) = claude_json_for("u-UNKNOWN"); // display cleared/stale
        let (result, log) = run_use_over(
            &store,
            &stash,
            &json,
            "spare",
            false,
            Probe::Live { weekly: 0.10 },
        )
        .await;
        assert!(
            result.is_ok(),
            "token-first resolution recovers the swap: {result:?}"
        );
        assert_eq!(
            canonical(&store).await,
            b"B-token",
            "the swap rerouted the canonical to spare (u-B)"
        );
        assert!(
            log.contains("event=swap from=work to=spare reason=manual"),
            "outgoing resolved token-first to work: {log}"
        );
        // Self-heal: the swap's co-write repaired the cleared display to the incoming
        // account (u-B), so the display and canonical agree again.
        let healed = crate::claude_state::read_oauth_account_from(&json).unwrap();
        assert_eq!(
            healed.account_uuid(),
            "u-B",
            "the cleared display was healed to the incoming account"
        );
    }

    #[tokio::test]
    async fn keychain_locked_on_active_resolution_aborts_with_zero_writes() {
        // SAFETY (issue #207): token-first resolution reads the canonical, so a LOCKED
        // keychain must abort with the locked exit code (4) and ZERO writes — never
        // swallowed to `ActiveAccountUnresolved`, and never a swap.
        let (store, stash) = seeded_store_and_stash().await;
        store.set_locked(true);
        let (_json_dir, json) = claude_json_for("u-A");
        let (result, log) = run_use_over(
            &store,
            &stash,
            &json,
            "spare",
            false,
            Probe::Live { weekly: 0.10 },
        )
        .await;
        let err = result.expect_err("a locked keychain must abort");
        assert!(matches!(err, Error::KeychainLocked { .. }), "got {err:?}");
        assert_eq!(err.exit_code(), 4, "the locked exit code");
        assert!(!log.contains("event=swap"), "no swap logged: {log}");
        // Unlock and confirm ZERO writes: the canonical still holds work's token.
        store.set_locked(false);
        assert_eq!(canonical(&store).await, b"A-token", "ZERO writes");
    }

    // --- acceptance: behavioral canary on the standalone path (issue #714) ---

    /// Freeze / thaw the `~/.claude.json` directory (0o500 ⇄ 0o700) so the
    /// canary's best-effort display heal — a temp-file + rename in that same
    /// directory — cannot land, pinning the stale display the drift fixtures need.
    fn set_dir_mode(dir: &Path, mode: u32) {
        let mut perms = std::fs::metadata(dir).unwrap().permissions();
        std::os::unix::fs::PermissionsExt::set_mode(&mut perms, mode);
        std::fs::set_permissions(dir, perms).unwrap();
    }

    #[tokio::test]
    async fn use_refuses_a_drifted_canary_even_with_force() {
        // Issue #714 AC on the daemon-DOWN path: the canonical byte-matches
        // spare's stash while the (heal-frozen) display names work — positive
        // identity drift. The standalone `use` refuses the credential write with
        // ZERO writes, and `--force` does NOT bypass it: the canary is SAFETY
        // (like the locked-keychain abort), not policy.
        let store = FakeCredentialStore::empty();
        store.write(&cred(b"B-token")).await.unwrap();
        let stash = FakeAccountStash::empty();
        stash
            .write("Sessiometer/u-A", &stashed(b"A-token", "u-A"))
            .await
            .unwrap();
        stash
            .write("Sessiometer/u-B", &stashed(b"B-token", "u-B"))
            .await
            .unwrap();
        let (json_dir, json) = claude_json_for("u-A");

        set_dir_mode(json_dir.path(), 0o500);
        let (result, log) = run_use_over(
            &store,
            &stash,
            &json,
            "work",
            true,
            Probe::Live { weekly: 0.10 },
        )
        .await;
        set_dir_mode(json_dir.path(), 0o700);

        assert!(
            matches!(
                &result,
                Err(Error::CanaryDrift { displayed, matched })
                    if displayed == "work" && matched == "spare"
            ),
            "got {result:?}"
        );
        assert_eq!(canonical(&store).await, b"B-token", "ZERO writes");
        // The refusal is on the durable record (labels only), with no swap line.
        assert!(
            log.contains("event=canary_drift displayed=work matched=spare"),
            "the refused drift is logged: {log}"
        );
        assert!(!log.contains("overridden=true"), "no override rode: {log}");
        assert!(!log.contains("event=swap"), "no swap was written: {log}");
    }

    #[tokio::test]
    async fn use_with_the_override_swaps_through_a_drift_and_logs_it() {
        // Issue #714 AC: `canary_drift_override = true` lets the standalone `use`
        // proceed despite a standing Layer-2 drift — and the ride is on the
        // durable record (`overridden=true`) alongside the normal swap event.
        let store = FakeCredentialStore::empty();
        store.write(&cred(b"B-token")).await.unwrap();
        let stash = FakeAccountStash::empty();
        stash
            .write("Sessiometer/u-A", &stashed(b"A-token", "u-A"))
            .await
            .unwrap();
        stash
            .write("Sessiometer/u-B", &stashed(b"B-token", "u-B"))
            .await
            .unwrap();
        let (json_dir, json) = claude_json_for("u-A");
        let mut config = config_ab();
        config.tunables.canary_drift_override = true;

        let log_dir = tempfile::tempdir().unwrap();
        let log_path = log_dir.path().join("sessiometer.log");
        let mut log = EventLog::at(&log_path).unwrap();
        let poller = FakePoller::new(Probe::Live { weekly: 0.10 });
        let lock_dir = tempfile::tempdir().unwrap();
        let lock_path = lock_dir.path().join("swap.lock");
        let notifier = FakeNotifier::ok();
        let cache = FakeCache::miss();

        set_dir_mode(json_dir.path(), 0o500);
        let result = run_use(
            &config,
            "work",
            false,
            false,
            Seams {
                cache: &cache,
                poller: &poller,
                store: &store,
                stash: &stash,
                claude_json: &json,
                lock_path: &lock_path,
                notifier: &notifier,
            },
            &mut log,
        )
        .await;
        set_dir_mode(json_dir.path(), 0o700);

        assert!(result.is_ok(), "the override lets the swap run: {result:?}");
        assert_eq!(
            canonical(&store).await,
            b"A-token",
            "the canonical rerouted to work (u-A) despite the standing drift"
        );
        let log_text = std::fs::read_to_string(&log_path).unwrap_or_default();
        assert!(
            log_text.contains("event=canary_drift displayed=work matched=spare overridden=true"),
            "the overridden ride is on the record: {log_text}"
        );
        assert!(
            log_text.contains("event=swap from=spare to=work reason=manual"),
            "the swap itself logs normally: {log_text}"
        );
    }

    #[tokio::test]
    async fn use_proceeds_when_a_well_formed_canonical_matches_no_stash() {
        // Issue #730 / #714 fail-OPEN on the daemon-DOWN path: the resolved canonical
        // matches NO stash but STILL parses as a Claude Code credential — overwhelmingly
        // the active account's own token refreshed in place since it was last stashed
        // (benign). The shape-gate leaves it fail-OPEN: the standalone `use` swaps, and
        // NO unparseable-canonical refusal is logged. This pins the branch the refuse arm
        // depends on — a later widening of the `canonical_well_formed: false` guard would
        // wrongly refuse a benign refresh here and silently break #714's guarantee.
        let store = FakeCredentialStore::empty();
        store
            .write(&cred(
                br#"{"claudeAiOauth":{"accessToken":"sk-ant-oat-REFRESHED","refreshToken":"sk-ant-ort-RT","expiresAt":1700000000000}}"#,
            ))
            .await
            .unwrap();
        let stash = FakeAccountStash::empty();
        stash
            .write("Sessiometer/u-A", &stashed(b"A-token", "u-A"))
            .await
            .unwrap();
        stash
            .write("Sessiometer/u-B", &stashed(b"B-token", "u-B"))
            .await
            .unwrap();
        let (_json_dir, json) = claude_json_for("u-A");

        let (result, log) = run_use_over(
            &store,
            &stash,
            &json,
            "spare",
            false,
            Probe::Live { weekly: 0.10 },
        )
        .await;

        assert!(
            result.is_ok(),
            "a well-formed unmatched canonical fails OPEN: {result:?}"
        );
        assert_eq!(
            canonical(&store).await,
            b"B-token",
            "the swap rerouted to spare (u-B)"
        );
        assert!(
            !log.contains("event=canary_unparseable_canonical"),
            "a well-formed canonical is never refused: {log}"
        );
        assert!(
            log.contains("event=swap from=work to=spare reason=manual"),
            "the swap itself proceeds and logs normally: {log}"
        );
    }

    #[tokio::test]
    async fn use_refuses_an_unparseable_canonical_that_matches_no_stash() {
        // Issue #730 on the daemon-DOWN path: the resolved canonical matches NO stash
        // AND does not parse as a Claude Code credential — an unrelated secret. The
        // standalone `use` refuses the credential write with ZERO writes (the same
        // fail-closed slot as DRIFT), and `--force` does NOT bypass it (SAFETY, not
        // policy). No display freeze needed: an unmatched canonical leaves the reconcile
        // a no-op, so the display stands on its own.
        let store = FakeCredentialStore::empty();
        store
            .write(&cred(b"an-unrelated-keychain-secret"))
            .await
            .unwrap();
        let stash = FakeAccountStash::empty();
        stash
            .write("Sessiometer/u-A", &stashed(b"A-token", "u-A"))
            .await
            .unwrap();
        stash
            .write("Sessiometer/u-B", &stashed(b"B-token", "u-B"))
            .await
            .unwrap();
        let (_json_dir, json) = claude_json_for("u-A");

        let (result, log) = run_use_over(
            &store,
            &stash,
            &json,
            "spare",
            true, // --force must NOT bypass the shape-gate
            Probe::Live { weekly: 0.10 },
        )
        .await;

        assert!(
            matches!(&result, Err(Error::CanaryUnparseableCanonical)),
            "got {result:?}"
        );
        assert_eq!(
            canonical(&store).await,
            b"an-unrelated-keychain-secret",
            "ZERO writes"
        );
        // The refusal is on the durable record, redaction-safe (no token bytes), no swap.
        assert!(
            log.contains("event=canary_unparseable_canonical"),
            "the refused shape-gate is logged: {log}"
        );
        assert!(!log.contains("overridden=true"), "no override rode: {log}");
        assert!(!log.contains("event=swap"), "no swap was written: {log}");
    }

    #[tokio::test]
    async fn use_with_the_nostashmatch_override_swaps_through_an_unparseable_canonical() {
        // Issue #730 AC: `canary_nostashmatch_override = true` lets the standalone `use`
        // proceed despite an unparseable orphan canonical — the ride is on the durable
        // record (`overridden=true`) alongside the normal swap event. A DEDICATED switch:
        // `canary_drift_override` stays default and does not gate this case.
        let store = FakeCredentialStore::empty();
        store.write(&cred(b"a-vetted-new-cc-format")).await.unwrap();
        let stash = FakeAccountStash::empty();
        stash
            .write("Sessiometer/u-A", &stashed(b"A-token", "u-A"))
            .await
            .unwrap();
        stash
            .write("Sessiometer/u-B", &stashed(b"B-token", "u-B"))
            .await
            .unwrap();
        let (_json_dir, json) = claude_json_for("u-A");
        let mut config = config_ab();
        config.tunables.canary_nostashmatch_override = true;

        let log_dir = tempfile::tempdir().unwrap();
        let log_path = log_dir.path().join("sessiometer.log");
        let mut log = EventLog::at(&log_path).unwrap();
        let poller = FakePoller::new(Probe::Live { weekly: 0.10 });
        let lock_dir = tempfile::tempdir().unwrap();
        let lock_path = lock_dir.path().join("swap.lock");
        let notifier = FakeNotifier::ok();
        let cache = FakeCache::miss();

        let result = run_use(
            &config,
            "spare",
            false,
            false,
            Seams {
                cache: &cache,
                poller: &poller,
                store: &store,
                stash: &stash,
                claude_json: &json,
                lock_path: &lock_path,
                notifier: &notifier,
            },
            &mut log,
        )
        .await;

        assert!(result.is_ok(), "the override lets the swap run: {result:?}");
        assert_eq!(
            canonical(&store).await,
            b"B-token",
            "the canonical rerouted to spare (u-B) despite the unparseable canonical"
        );
        let log_text = std::fs::read_to_string(&log_path).unwrap_or_default();
        assert!(
            log_text.contains("event=canary_unparseable_canonical overridden=true"),
            "the overridden ride is on the record: {log_text}"
        );
        assert!(
            log_text.contains("event=swap from=work to=spare reason=manual"),
            "the swap itself logs normally: {log_text}"
        );
    }

    // --- acceptance: adopt-target recovery (issue #212) ----------------------

    #[tokio::test]
    async fn force_adopts_the_target_when_the_canonical_is_absent_and_display_is_cleared() {
        // AC #1: a forced logout scrubbed the canonical (read → CredentialNotFound) AND
        // cleared ~/.claude.json (u-UNKNOWN). `use --force spare` RECOVERS by adopting the
        // healthy target directly — where before it hard-failed ActiveAccountUnresolved
        // (token-first resolution, #207, finds no stash when the token itself is gone).
        let (store, stash) = seeded_store_and_stash().await;
        store.set_not_found(true); // the scrubbed / absent canonical
        let (_json_dir, json) = claude_json_for("u-UNKNOWN"); // display cleared
        let (result, log) = run_use_over(
            &store,
            &stash,
            &json,
            "spare",
            true,
            Probe::Live { weekly: 0.10 },
        )
        .await;
        assert!(
            result.is_ok(),
            "adopt-target recovers the session: {result:?}"
        );
        // The canonical now holds spare's (u-B) token — the write created the absent item.
        assert_eq!(
            canonical(&store).await,
            b"B-token",
            "adopted spare into the canonical"
        );
        // The display was co-written to the incoming account (self-healed).
        let healed = crate::claude_state::read_oauth_account_from(&json).unwrap();
        assert_eq!(healed.account_uuid(), "u-B");
        // The outgoing account is unknown (display cleared) → the non-secret sentinel.
        assert!(
            log.contains("event=swap from=(unknown) to=spare reason=forced"),
            "log: {log}"
        );
        // AC #3: NOTHING was re-stashed — work's stash is untouched (no wrong-identity
        // staple; the departing token was never required).
        let a = stash.read("Sessiometer/u-A").await.unwrap();
        assert_eq!(
            a.credential.expose(),
            b"A-token",
            "work's stash must be untouched"
        );
        assert_eq!(a.oauth_account.account_uuid(), "u-A");
    }

    #[tokio::test]
    async fn force_adopts_the_target_when_the_canonical_is_rotated_and_display_is_cleared() {
        // AC #1, rotated variant: the canonical holds a ROTATED orphan token (matches no
        // stash) and the display is cleared → the outgoing is unresolvable → `--force`
        // adopts the target, overwriting the orphan without stashing it anywhere.
        let (store, stash) = seeded_store_and_stash().await;
        store.write(&cred(b"ORPHAN-rotated")).await.unwrap(); // canonical rotated in place
        let (_json_dir, json) = claude_json_for("u-UNKNOWN");
        let (result, log) = run_use_over(
            &store,
            &stash,
            &json,
            "spare",
            true,
            Probe::Live { weekly: 0.10 },
        )
        .await;
        assert!(
            result.is_ok(),
            "adopt-target recovers a rotated canonical: {result:?}"
        );
        assert_eq!(canonical(&store).await, b"B-token");
        assert!(
            log.contains("event=swap from=(unknown) to=spare reason=forced"),
            "log: {log}"
        );
    }

    #[tokio::test]
    async fn force_adopt_names_the_outgoing_when_the_display_still_resolves() {
        // AC #1: the canonical is gone but ~/.claude.json still names a roster account
        // (u-A = work). Adopt still recovers (the normal swap would fault reading the
        // absent canonical at its step 1), and the event NAMES the resolved outgoing
        // rather than the sentinel — a more useful record when the display survived.
        let (store, stash) = seeded_store_and_stash().await;
        store.set_not_found(true);
        let (_json_dir, json) = claude_json_for("u-A"); // display still resolves work
        let (result, log) = run_use_over(
            &store,
            &stash,
            &json,
            "spare",
            true,
            Probe::Live { weekly: 0.10 },
        )
        .await;
        assert!(
            result.is_ok(),
            "adopt recovers with a resolvable display: {result:?}"
        );
        assert_eq!(canonical(&store).await, b"B-token");
        assert!(
            log.contains("event=swap from=work to=spare reason=forced"),
            "the resolved outgoing is named, not the sentinel: {log}"
        );
    }

    #[tokio::test]
    async fn a_locked_keychain_aborts_adopt_recovery_with_zero_writes() {
        // AC #2 ("locked ≠ gone"): even in the adopt SCENARIO — the canonical would be
        // gone AND the display cleared, so `--force` would adopt — a LOCKED keychain
        // aborts with the locked exit code (4) and ZERO writes. A lock is transient
        // (retry when unlocked), never a scrubbed credential to clobber over.
        let (store, stash) = seeded_store_and_stash().await;
        store.set_not_found(true); // would-be-gone…
        store.set_locked(true); // …but the keychain is LOCKED (locked takes precedence)
        let (_json_dir, json) = claude_json_for("u-UNKNOWN");
        let (result, log) = run_use_over(
            &store,
            &stash,
            &json,
            "spare",
            true,
            Probe::Live { weekly: 0.10 },
        )
        .await;
        let err = result.expect_err("a locked keychain must abort even in the adopt path");
        assert!(matches!(err, Error::KeychainLocked { .. }), "got {err:?}");
        assert_eq!(err.exit_code(), 4, "the locked exit code");
        assert!(!log.contains("event=swap"), "no swap logged: {log}");
        // ZERO writes: unlock and confirm the canonical is STILL absent (never adopted).
        store.set_locked(false);
        assert!(
            matches!(store.read().await, Err(Error::CredentialNotFound)),
            "the canonical must still be absent (ZERO writes)"
        );
    }

    #[tokio::test]
    async fn a_present_but_unreadable_canonical_aborts_adopt_recovery_with_zero_writes() {
        // AC #2 generalized ("could not read ≠ gone"): the canonical is PRESENT (holds
        // work's live token) but its secret cannot be READ — a non-lock, non-not-found
        // `security` error (an ACL / auth-deny in a UI session), NOT a scrubbed
        // credential. The display is cleared, so a naive "any read failure is gone"
        // classification would let `--force` adopt-CLOBBER work's present token WITHOUT
        // re-stashing it — losing it. The fix aborts here (as it does on a lock): only a
        // CONFIRMED-absent or readable canonical is adopt-eligible.
        let (store, stash) = seeded_store_and_stash().await; // canonical = A-token (present)
        store.set_unreadable(true); // …but its secret is unreadable (not a lock, not absent)
        let (_json_dir, json) = claude_json_for("u-UNKNOWN"); // display cleared
        let (result, log) = run_use_over(
            &store,
            &stash,
            &json,
            "spare",
            true,
            Probe::Live { weekly: 0.10 },
        )
        .await;
        let err = result.expect_err("a present-but-unreadable canonical must abort the adopt");
        assert!(matches!(err, Error::Keychain { .. }), "got {err:?}");
        assert_eq!(err.exit_code(), 1, "a generic keychain read failure");
        assert!(!log.contains("event=swap"), "no swap logged: {log}");
        // ZERO writes: clear the read fault and confirm work's live token is STILL the
        // canonical — it was NOT clobbered, and (AC #3) nothing was re-stashed.
        store.set_unreadable(false);
        assert_eq!(
            canonical(&store).await,
            b"A-token",
            "the present token must be untouched (ZERO writes — not adopt-clobbered)"
        );
        let a = stash.read("Sessiometer/u-A").await.unwrap();
        assert_eq!(a.credential.expose(), b"A-token", "work's stash untouched");
    }

    #[tokio::test]
    async fn no_force_leaves_a_gone_canonical_unresolved_with_zero_writes() {
        // Recovery requires `--force`: WITHOUT it, a scrubbed canonical + cleared display
        // stays the fail-closed ActiveAccountUnresolved (adopt never triggers), ZERO
        // writes — the #207 behaviour is unchanged for the non-forced path.
        let (store, stash) = seeded_store_and_stash().await;
        store.set_not_found(true);
        let (_json_dir, json) = claude_json_for("u-UNKNOWN");
        let (result, log) = run_use_over(
            &store,
            &stash,
            &json,
            "spare",
            false,
            Probe::Live { weekly: 0.10 },
        )
        .await;
        assert!(
            matches!(result, Err(Error::ActiveAccountUnresolved)),
            "without --force, a gone canonical stays unresolved: {result:?}"
        );
        assert!(
            matches!(store.read().await, Err(Error::CredentialNotFound)),
            "ZERO writes — the canonical is still absent"
        );
        assert!(!log.contains("event=swap"), "no swap logged: {log}");
    }

    // --- acceptance: manual-hold daemon notification (#64) -------------------

    /// Drive a gated `use spare` over a viable target with a caller-supplied
    /// notifier, returning the result + the notifier so a test can assert the
    /// notify happened. Separate from `run` (which hides its notifier) precisely so
    /// the manual-hold tests can inspect it.
    async fn run_with_notifier(notifier: &FakeNotifier) -> (Result<()>, FakeCredentialStore) {
        let config = config_ab();
        let (store, stash) = seeded_store_and_stash().await;
        let (_json_dir, json) = claude_json_for("u-A");
        let log_dir = tempfile::tempdir().unwrap();
        let mut log = EventLog::at(&log_dir.path().join("sessiometer.log")).unwrap();
        // No daemon cached reading → the gate falls back to the live (viable) poll.
        let cache = FakeCache::miss();
        let poller = FakePoller::new(Probe::Live { weekly: 0.10 });
        let lock_dir = tempfile::tempdir().unwrap();
        let lock_path = lock_dir.path().join("swap.lock");
        let result = run_use(
            &config,
            "spare",
            false,
            false,
            Seams {
                cache: &cache,
                poller: &poller,
                store: &store,
                stash: &stash,
                claude_json: &json,
                lock_path: &lock_path,
                notifier,
            },
            &mut log,
        )
        .await;
        (result, store)
    }

    #[tokio::test]
    async fn a_committed_manual_swap_notifies_the_daemon_exactly_once() {
        // Manual-hold (#64): a successful manual swap notifies the daemon so it arms
        // its cooldown. The swap committed (canonical now holds B's token), and the
        // notify fired exactly once — never a busy-loop.
        let notifier = FakeNotifier::ok();
        let (result, store) = run_with_notifier(&notifier).await;

        assert!(result.is_ok(), "the swap succeeds: {result:?}");
        assert_eq!(canonical(&store).await, b"B-token", "the swap committed");
        assert_eq!(
            notifier.calls.get(),
            1,
            "exactly one manual-hold notification after a committed swap"
        );
    }

    #[tokio::test]
    async fn a_failed_notify_is_non_fatal_and_use_still_succeeds() {
        // Best-effort (#64): the notify FAILS (no daemon listening), yet `use` still
        // exits SUCCESS and the swap stays committed — the keychain write is
        // authoritative, so the manual swap already succeeded; the failure is logged,
        // not propagated.
        let notifier = FakeNotifier::failing();
        let (result, store) = run_with_notifier(&notifier).await;

        assert!(
            result.is_ok(),
            "a failed manual-hold notify must NOT fail the swap: {result:?}"
        );
        assert_eq!(
            canonical(&store).await,
            b"B-token",
            "the swap is committed regardless of the notify outcome"
        );
        assert_eq!(notifier.calls.get(), 1, "the notify was attempted once");
    }

    // --- acceptance: redaction over ALL command output (#15) -----------------

    #[test]
    fn command_output_is_redaction_clean() {
        // Every output surface the command can emit — the confirmation, the
        // already-active note, both --force warnings, and every new error message —
        // is sourced solely from non-secret handles/labels, so the #15 meter finds
        // no token, blob fingerprint, or email. The corpus is built from the SAME
        // recognizable secrets the meter scans for, so a leak would surface.
        use crate::redaction::meter;
        let secrets = meter::Secrets::meter_fixture();
        let corpus = [
            swap_confirmation("work", "spare"),
            // The adopt-target recovery surfaces (#212): its note, and a confirmation
            // whose outgoing is the non-secret `(unknown)` sentinel.
            note_adopt_target("spare"),
            swap_confirmation(ADOPT_UNKNOWN_FROM, "spare"),
            already_active_confirmation("spare"),
            warn_weekly_exhausted("spare"),
            warn_quarantined("spare"),
            Error::UseTargetRequired.to_string(),
            Error::UseTargetNotFound {
                query: "ghost".into(),
            }
            .to_string(),
            Error::UseTargetAmbiguous {
                query: "dup".into(),
                count: 2,
            }
            .to_string(),
            Error::UseTargetWeeklyExhausted {
                label: "spare".into(),
            }
            .to_string(),
            Error::UseCooldownActive.to_string(),
            Error::UseTargetQuarantined {
                label: "spare".into(),
            }
            .to_string(),
            Error::UseViabilityUnverifiable {
                label: "spare".into(),
            }
            .to_string(),
            Error::ActiveAccountUnresolved.to_string(),
            Error::KeychainLocked { op: "read" }.to_string(),
            // Issue #736: the Layer-3 probe's strict refusal carries a verdict CLASS only.
            Error::CanaryProbeNotLive {
                verdict: "rejected",
            }
            .to_string(),
            Error::CanaryProbeNotLive {
                verdict: "inconclusive",
            }
            .to_string(),
        ]
        .join("\n");
        meter::assert_clean(&corpus, &secrets, &[]);
    }

    // --- Layer 3: the opt-in online liveness probe (issue #736) --------------

    /// Run `use spare` with the two #736 switches set and a scripted probe answer.
    ///
    /// The target-viability gate is satisfied from a cache HIT so the ONLY poll the run
    /// makes is the Layer-3 probe itself — which both keeps `probe` unambiguous (the
    /// shared [`FakePoller`] answers every account identically) and pins that the probe
    /// is a genuinely NEW call rather than a reuse of the gate's viability poll.
    /// Returns the result, the canonical's bytes afterwards, the poll count, and the log.
    async fn run_use_with_probe(
        probe: Probe,
        online_probe: bool,
        strict: bool,
    ) -> (Result<()>, Vec<u8>, u32, String) {
        run_use_with_probe_forced(probe, online_probe, strict, false).await
    }

    /// [`run_use_with_probe`] with `--force` under the caller's control, for the one test
    /// that pins Layer 3's deliberate `--force` bypass.
    async fn run_use_with_probe_forced(
        probe: Probe,
        online_probe: bool,
        strict: bool,
        force: bool,
    ) -> (Result<()>, Vec<u8>, u32, String) {
        let mut config = config_ab();
        config.tunables.canary_online_probe = online_probe;
        config.tunables.canary_online_probe_strict = strict;
        let (store, stash) = seeded_store_and_stash().await;
        let (_json_dir, json) = claude_json_for("u-A");
        let log_dir = tempfile::tempdir().unwrap();
        let log_path = log_dir.path().join("sessiometer.log");
        let mut log = EventLog::at(&log_path).unwrap();
        let poller = FakePoller::new(probe);
        let lock_dir = tempfile::tempdir().unwrap();
        let lock_path = lock_dir.path().join("swap.lock");
        let notifier = FakeNotifier::ok();
        let cache = FakeCache::hit(Viability::Viable);

        let result = run_use(
            &config,
            "spare",
            force,
            false,
            Seams {
                cache: &cache,
                poller: &poller,
                store: &store,
                stash: &stash,
                claude_json: &json,
                lock_path: &lock_path,
                notifier: &notifier,
            },
            &mut log,
        )
        .await;

        let log_text = std::fs::read_to_string(&log_path).unwrap_or_default();
        (
            result,
            canonical(&store).await,
            poller.calls.get(),
            log_text,
        )
    }

    #[tokio::test]
    async fn use_with_a_disarmed_probe_issues_no_poll_and_swaps() {
        // Issue #736's first hard constraint on the standalone path: `canary_online_probe
        // = false` (the default) must not merely ignore the probe — it must never ASK.
        // With the viability gate served from cache, a poll count of ZERO is direct
        // evidence no request was issued. Scripted `Dead` so the probe WOULD have refused
        // had it run: this passes because the probe is disarmed, not because it was happy.
        let (result, canonical, polls, log) = run_use_with_probe(Probe::Dead, false, true).await;

        assert!(result.is_ok(), "got {result:?}");
        assert_eq!(polls, 0, "a disarmed probe must issue no request: {log}");
        assert_eq!(canonical, b"B-token", "the swap wrote the incoming token");
        assert!(!log.contains("event=canary_online_probe"), "logged: {log}");
    }

    #[tokio::test]
    async fn use_with_an_armed_live_probe_polls_once_and_stays_silent() {
        // The healthy armed case. The poll count pins that the probe is its OWN call —
        // the viability gate was served from cache and made none — while the empty log
        // pins the alarm-only idiom: arming the probe must not make every swap noisy.
        let (result, canonical, polls, log) =
            run_use_with_probe(Probe::Live { weekly: 0.10 }, true, true).await;

        assert!(result.is_ok(), "got {result:?}");
        assert_eq!(polls, 1, "the probe is a call of its own");
        assert_eq!(canonical, b"B-token");
        assert!(!log.contains("event=canary_online_probe"), "logged: {log}");
    }

    #[tokio::test]
    async fn use_with_an_armed_non_strict_probe_logs_the_failure_and_swaps_anyway() {
        // Issue #736's second hard constraint on the standalone path: probe failure !=
        // refuse. The swap completes, and the durable line is the only trace that the
        // probe failed — which is exactly why it is emitted with nothing refused.
        let (result, canonical, _polls, log) =
            run_use_with_probe(Probe::Transient, true, false).await;

        assert!(result.is_ok(), "got {result:?}");
        assert_eq!(canonical, b"B-token", "the swap still wrote");
        assert!(
            log.contains("event=canary_online_probe verdict=inconclusive"),
            "the degraded probe is on the record: {log}"
        );
        assert!(!log.contains("refused=true"), "nothing was refused: {log}");
        assert!(log.contains("event=swap"), "the swap is logged: {log}");
    }

    #[tokio::test]
    async fn use_with_an_armed_strict_probe_refuses_a_dead_bearer_with_zero_writes() {
        // Both switches armed — the one configuration where the probe can cost a swap.
        // Refused pre-mutation, like the offline layers' refusals: the canonical still
        // holds the OUTGOING token and no swap event was written.
        let (result, canonical, _polls, log) = run_use_with_probe(Probe::Dead, true, true).await;

        assert!(
            matches!(
                &result,
                Err(Error::CanaryProbeNotLive {
                    verdict: "rejected"
                })
            ),
            "got {result:?}"
        );
        assert_eq!(canonical, b"A-token", "ZERO writes");
        assert!(
            log.contains("event=canary_online_probe verdict=rejected refused=true"),
            "the refusal is on the record: {log}"
        );
        assert!(!log.contains("event=swap"), "no swap was written: {log}");
    }

    #[tokio::test]
    async fn use_force_bypasses_the_online_probe_so_recovery_is_always_possible() {
        // Layer 3 is the ONE layer `--force` bypasses, and the asymmetry is the point: the
        // offline layers refuse because the write would clobber an unrelated secret
        // UNRECOVERABLY, which no operator intent can make safe, while a failed probe means
        // only that the swap may not take effect. Without this, a strict operator whose
        // active credential has genuinely died would have no way to swap off it — and the
        // daemon-routed path honours `--force` identically (`Daemon::probe_gate`), so the
        // escape does not blink out whenever the daemon happens to be up.
        // The poll count pins that `--force` does not merely ignore the verdict: it never asks.
        // The log line pins that it is not SILENT either — bypassing a gate the operator armed
        // is exactly what an operator later needs to find, so it is `overridden`, not skipped.
        //
        // Contrast `use_refuses_a_drifted_canary_even_with_force` above, which pins that
        // `--force` does NOT bypass Layer 2 — the two tests together fix the boundary.
        let (result, canonical, polls, log) =
            run_use_with_probe_forced(Probe::Dead, true, true, true).await;

        assert!(
            result.is_ok(),
            "--force must clear the online probe: {result:?}"
        );
        assert_eq!(polls, 0, "--force must not even ask: {log}");
        assert_eq!(canonical, b"B-token", "the forced swap wrote");
        assert!(
            log.contains("event=canary_online_probe verdict=overridden"),
            "the bypass must leave a durable trace: {log}"
        );
        assert!(!log.contains("refused=true"), "not a refusal: {log}");
    }

    #[tokio::test]
    async fn use_with_an_armed_strict_probe_refuses_an_unreachable_endpoint_too() {
        // Strict IS the opt-in to the network failure mode the default forbids, so it
        // refuses on "could not confirm" as well — with the verdict naming WHICH, so an
        // operator can tell a dead bearer from an unreachable endpoint without guessing.
        let (result, canonical, _polls, log) =
            run_use_with_probe(Probe::RateLimited, true, true).await;

        assert!(
            matches!(
                &result,
                Err(Error::CanaryProbeNotLive {
                    verdict: "inconclusive"
                })
            ),
            "got {result:?}"
        );
        assert_eq!(canonical, b"A-token", "ZERO writes");
        assert!(
            log.contains("event=canary_online_probe verdict=inconclusive refused=true"),
            "the refusal is on the record: {log}"
        );
    }
}
