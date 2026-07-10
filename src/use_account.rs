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
use crate::config::{Account, Config};
use crate::daemon::{
    AccountStatusLine, RealRosterPoller, RosterPoller, StatusResponse, SwapAck, SwapRejection,
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
    /// target's handle is absent or duplicated in the reply, or the daemon's last
    /// poll for it failed. `None` is the signal to fall back to a single live poll.
    async fn cached_viability(&self, account: &Account) -> Option<Viability>;
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
    async fn cached_viability(&self, account: &Account) -> Option<Viability> {
        // A live daemon answers instantly; a missing / wedged daemon must never hang
        // `use`, so a timeout — like any other exchange failure — is a cache MISS.
        let response = tokio::time::timeout(CONTROL_SOCKET_TIMEOUT, self.query_status())
            .await
            .ok()? // timed out → MISS
            .ok()?; // no daemon / I/O / malformed reply → MISS
        cached_viability_for(&response, &account.label)
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
/// when the `status` reply carries no usable verdict for it (issue #75). The handle
/// must match EXACTLY ONE line: labels are operator handles and NOT guaranteed
/// unique (see [`resolve_target`]), and the wire reply carries only the handle
/// (issue #15: never the account-uuid), so a zero- or multiple-match is treated as
/// "no usable cached reading" → live fallback rather than guessing.
///
/// A consequence of keying on the handle: in the exotic case where the running
/// daemon is STALE *and* a handle has since been reassigned to a different
/// account-uuid, the cached reading cannot be cross-checked against the uuid, so
/// the verdict returned is the stale account's — a bounded, recoverable mismatch
/// (the swap still operates on the correct target's stash; the daemon's next cycle
/// or a `--force` corrects a wrong refusal). Closing it would mean widening the
/// non-secret wire contract with the account-uuid, out of scope for this gate.
fn cached_viability_for(response: &StatusResponse, label: &str) -> Option<Viability> {
    let mut matches = response.accounts.iter().filter(|line| line.label == label);
    let line = matches.next()?;
    if matches.next().is_some() {
        // A duplicated handle: cannot disambiguate from the wire reply alone.
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
    /// `active_stash` is the active (outgoing) account's stash, `weekly_trigger` is
    /// the fraction at/above which the weekly window counts as exhausted, and
    /// `in_cooldown` is the caller-computed cooldown verdict (kept a parameter so
    /// the gate stays pure and hermetically testable, independent of wall-clock).
    /// The cooldown refusal short-circuits BEFORE any cache query or live poll, so a
    /// cooled-down `use` touches neither the socket nor the network.
    async fn resolve<R: CachedViabilitySource, P: RosterPoller>(
        cache: &R,
        poller: &P,
        account: &Account,
        active_stash: &str,
        weekly_trigger: f64,
        in_cooldown: bool,
    ) -> Result<GateOutcome> {
        if account.stash() == active_stash {
            return Ok(GateOutcome::AlreadyActive);
        }
        if in_cooldown {
            return Ok(GateOutcome::Refused(Refusal::Cooldown));
        }
        match gate_viability(cache, poller, account, weekly_trigger).await? {
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
    weekly_trigger: f64,
) -> Result<Viability> {
    match poller.poll(account, false).await {
        // Only the weekly dimension of the reading drives the viability verdict; the
        // sample-only fields on the `PolledReading` are irrelevant here (this is the
        // one-shot `use`-command probe, not the daemon's sampling poll loop).
        Ok(reading) if reading.usage.weekly >= weekly_trigger => Ok(Viability::WeeklyExhausted),
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
    account: &Account,
    weekly_trigger: f64,
) -> Result<Viability>
where
    R: CachedViabilitySource,
    P: RosterPoller,
{
    if let Some(cached) = cache.cached_viability(account).await {
        return Ok(cached);
    }
    // Cache MISS → a single live poll (today's behaviour). A `429` here is the
    // opaque abort issue #75 fixes: with no daemon to consult, surface a distinct,
    // actionable error rather than the raw rate-limit.
    match poll_viability(poller, account, weekly_trigger).await {
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
    target: &Account,
    weekly_trigger: f64,
) -> Result<()>
where
    R: CachedViabilitySource,
    P: RosterPoller,
{
    let viability = match cache.cached_viability(target).await {
        Some(cached) => Some(cached),
        None => match poll_viability(poller, target, weekly_trigger).await {
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

    let weekly_trigger = f64::from(config.tunables.weekly_trigger) / 100.0;

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
        warn_if_forcing_onto_non_viable(seams.cache, seams.poller, target, weekly_trigger).await?;
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
            warn_if_forcing_onto_non_viable(seams.cache, seams.poller, target, weekly_trigger)
                .await?;
            (SwapTarget::forced(target), SwapReason::Forced)
        } else {
            match SwapTarget::resolve(
                seams.cache,
                seams.poller,
                target,
                &active_stash,
                weekly_trigger,
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

/// `sessiometer use <account> [--force]` — wire the REAL seams into [`run_use`].
///
/// A missing `<account>` is [`Error::UseTargetRequired`] (there is deliberately no
/// "cycle to the next account" fallback — out of scope, #63). Loads the real config
/// (a friendly empty-state if nothing is captured), derives the cooldown verdict
/// from the durable event log, and drives the swap over the live keychain
/// (`apple-tool:` CLI path) and `~/.claude.json`.
pub(crate) async fn use_account(query: Option<String>, force: bool) -> Result<()> {
    let query = query.ok_or(Error::UseTargetRequired)?;
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
    // Both control-socket clients (the cached-reading query #75 and the manual-hold
    // notify #64) speak to the same daemon socket; resolve it once.
    let control_socket = paths::control_socket()?;

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
/// #15), so the operator's own `query` names the target in the message (non-secret operator input).
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
        async fn cached_viability(&self, _account: &Account) -> Option<Viability> {
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
    /// tunables (weekly_trigger 98 ⇒ 0.98, cooldown 60).
    fn config_ab() -> Config {
        Config {
            roster: vec![acct("work", "u-A"), acct("spare", "u-B")],
            tunables: Tunables::default(),
            refresh: crate::config::RefreshConfig::default(),
            login: crate::config::LoginConfig::default(),
            stats: crate::config::StatsConfig::default(),
            migration: crate::config::MigrationConfig::default(),
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
        let config = config_ab();
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
    fn cached_viability_for_requires_a_unique_handle_match() {
        // A unique handle match → its verdict.
        let unique = StatusResponse {
            systemic_refresh_failure: None,
            refresh_enabled: None,
            accounts: vec![
                status_line("work", false, false, Some(20)),
                status_line("spare", false, false, Some(10)),
            ],
            next_swap: None,
        };
        assert_eq!(
            cached_viability_for(&unique, "spare"),
            Some(Viability::Viable)
        );
        // A handle absent from the reply → no cached reading → live fallback.
        assert_eq!(cached_viability_for(&unique, "ghost"), None);
        // A DUPLICATED handle cannot be disambiguated from the wire reply alone
        // (labels are not unique, and the reply carries no account-uuid) → live
        // fallback, never a guess.
        let duped = StatusResponse {
            systemic_refresh_failure: None,
            refresh_enabled: None,
            accounts: vec![
                status_line("dup", false, false, Some(10)),
                status_line("dup", true, false, None),
            ],
            next_swap: None,
        };
        assert_eq!(cached_viability_for(&duped, "dup"), None);
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
        let target = acct("spare", "u-B");
        let (_, verdict) = tokio::join!(server, cache.cached_viability(&target));
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
        let target = acct("spare", "u-B");
        assert_eq!(cache.cached_viability(&target).await, None);
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
        let config = config_ab();
        let log_dir = tempfile::tempdir().unwrap();
        let log_path = log_dir.path().join("sessiometer.log");
        let mut log = EventLog::at(&log_path).unwrap();
        let poller = FakePoller::new(probe);
        let lock_dir = tempfile::tempdir().unwrap();
        let lock_path = lock_dir.path().join("swap.lock");
        let notifier = FakeNotifier::ok();
        let cache = FakeCache::miss();
        let result = run_use(
            &config,
            query,
            force,
            false,
            Seams {
                cache: &cache,
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
        (result, log_text)
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
        ]
        .join("\n");
        meter::assert_clean(&corpus, &secrets);
    }
}
