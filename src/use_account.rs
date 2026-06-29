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

use crate::claude_state;
use crate::config::{Account, Config};
use crate::daemon::{RealRosterPoller, RosterPoller};
use crate::error::{Error, Result};
use crate::keychain::{CredentialStore, RealCredentialStore};
use crate::observability::{self, Event, EventLog, SwapReason};
use crate::paths;
use crate::stash::{AccountStash, RealAccountStash};
use crate::swap;

/// How long the best-effort manual-hold notify ([`ControlSocketNotifier`]) waits
/// on the control socket before giving up (issue #64). Short: a live daemon, idle
/// between polls, answers instantly; a missing or wedged daemon must never hang
/// `use`, so the notify times out and is logged-and-ignored (the swap already
/// succeeded — the keychain write is authoritative).
const MANUAL_SWAP_NOTIFY_TIMEOUT: Duration = Duration::from_secs(2);

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
/// [`MANUAL_SWAP_NOTIFY_TIMEOUT`] so a missing / wedged daemon never hangs `use`;
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
        tokio::time::timeout(MANUAL_SWAP_NOTIFY_TIMEOUT, exchange)
            .await
            .map_err(|_| {
                Error::Io(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    "manual-hold notify timed out",
                ))
            })?
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

/// The target's viability, as proven by a poll of its stashed token.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Viability {
    /// A live poll below the weekly trigger — a sound destination.
    Viable,
    /// The stored credential is dead (`401`/`403`) — quarantined / needs re-login (#42).
    Quarantined,
    /// A live poll at/above the weekly trigger — the weekly window is exhausted (#11/#37).
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
    /// The target is quarantined (dead credential).
    Quarantined,
}

impl SwapTarget {
    /// The gated constructor — the ONLY path to a non-forced target. Runs the
    /// pre-swap gate for `account` (the resolved target): already-active → no-op;
    /// in-cooldown → refused; otherwise poll viability and mint a target ONLY when
    /// it is viable. A locked keychain or a transient poll failure propagates as
    /// `Err` (still before any write) for the caller to surface.
    ///
    /// `active_stash` is the active (outgoing) account's stash, `weekly_trigger` is
    /// the fraction at/above which the weekly window counts as exhausted, and
    /// `in_cooldown` is the caller-computed cooldown verdict (kept a parameter so
    /// the gate stays pure and hermetically testable, independent of wall-clock).
    async fn resolve<P: RosterPoller>(
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
        match poll_viability(poller, account, weekly_trigger).await? {
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
        Ok(usage) if usage.weekly >= weekly_trigger => Ok(Viability::WeeklyExhausted),
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
/// resolution the offline `list` view keys on (#17). The resolver NEVER guesses:
/// zero matches is [`Error::UseTargetNotFound`], more than one (a duplicated label)
/// is [`Error::UseTargetAmbiguous`]. Each account is counted once even if both its
/// fields equal `query`.
fn resolve_target(roster: &[Account], query: &str) -> Result<usize> {
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

/// The `--force` warning for forcing onto a quarantined (dead-credential) target.
/// Names only the non-secret handle.
fn warn_quarantined(label: &str) -> String {
    format!("warning: forcing onto `{label}`, which is quarantined and needs re-login")
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

/// The injectable seams [`run_use`] drives — the viability/credential/stash/state
/// surfaces — so the whole gate→swap flow runs hermetically against in-memory fakes
/// in tests, exactly as [`crate::daemon::Daemon`] injects its seams.
struct Seams<'a, P, C, S, N> {
    /// Polls the TARGET's stashed token for viability (#37/#42).
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
async fn run_use<P, C, S, N>(
    config: &Config,
    query: &str,
    force: bool,
    in_cooldown: bool,
    seams: Seams<'_, P, C, S, N>,
    log: &mut EventLog,
) -> Result<()>
where
    P: RosterPoller,
    C: CredentialStore,
    S: AccountStash,
    N: ManualSwapNotifier,
{
    // 1. Resolve the target by label OR uuid (the resolver never guesses, #17).
    let target = &config.roster[resolve_target(&config.roster, query)?];
    let target_label = target.label.clone();

    // 2. Identify the active (outgoing) account: the swap re-stashes it, so its
    //    roster identity MUST be known (mirrors the daemon's "can't identify active
    //    ⇒ don't swap"). Its displayed identity is Claude Code's `oauthAccount`.
    let active_uuid = claude_state::read_oauth_account_from(seams.claude_json)
        .ok()
        .map(|oauth| oauth.account_uuid().to_owned());
    let active = active_uuid
        .as_deref()
        .and_then(|uuid| {
            config
                .roster
                .iter()
                .find(|account| account.account_uuid == uuid)
        })
        .ok_or(Error::ActiveAccountUnresolved)?;
    let active_stash = active.stash();
    let active_label = active.label.clone();

    let weekly_trigger = f64::from(config.tunables.weekly_trigger) / 100.0;

    // 3. Gate (default) or `--force`-bypass — yielding the vetted target + reason.
    let (swap_target, reason) = if force {
        // `--force` bypasses the POLICY gates (cooldown, weekly-exhausted,
        // already-active), but still WARNS when forcing onto a non-viable target.
        // SAFETY is never bypassed: a locked keychain aborts (ZERO writes); a
        // transient poll failure only affects the informational warning, so the
        // forced swap proceeds best-effort without one.
        match poll_viability(seams.poller, target, weekly_trigger).await {
            // A known viability: emit the matching warn-and-proceed warning (none
            // for a viable target). The DECISION is the pure `force_warning`; only
            // the emission lives here.
            Ok(viability) => {
                if let Some(warning) = force_warning(viability, &target_label) {
                    eprintln!("{warning}");
                }
            }
            // SAFETY is never bypassed: a locked keychain aborts even with `--force`
            // (ZERO writes — the swap never runs).
            Err(err @ Error::KeychainLocked { .. }) => return Err(err),
            // A transient poll failure only affects the (informational) warning, so
            // the forced swap proceeds best-effort without one (decision D1).
            Err(_) => {}
        }
        (SwapTarget::forced(target), SwapReason::Forced)
    } else {
        match SwapTarget::resolve(
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
                // No-op success: already active, nothing to write.
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

    // 4. Reuse the swap engine UNCHANGED, now wrapped in the single-writer swap
    //    lock (#64): the lock is acquired (blocking, bounded) BEFORE the swap reads
    //    anything and held across the whole two-step write, so a concurrent daemon
    //    swap cannot interleave into a split state. FAIL-CLOSED — a contended lock
    //    that never frees within the bounded wait aborts with `SwapLockBusy` (exit
    //    `4`, ZERO writes), never a torn write. Inside, the engine's own discipline
    //    still holds: canonical write FIRST (a locked keychain aborts here with ZERO
    //    writes — the always-enforced safety, even with `--force`), then the atomic,
    //    field-preserving `~/.claude.json` co-write.
    swap::swap_locked(
        Some((seams.lock_path, swap::SWAP_LOCK_MAX_WAIT)),
        seams.store,
        seams.stash,
        &active_stash,
        swap_target.incoming_stash(),
        seams.claude_json,
    )
    .await?;

    // 5. Emit the standard structured event (#9) — the durable record that also
    //    updates `last_swap` — with the new manual/forced reason, and print the
    //    one-line confirmation. `session_pct=0`: a manual swap is not session-
    //    triggered (the reason distinguishes it). Both are sourced from non-secret
    //    handles only (issue #15).
    log.emit(&Event::Swap {
        from: active_label.clone(),
        to: target_label.clone(),
        reason,
        session_pct: 0,
    })?;

    // 6. Manual-hold (#64): the swap has COMMITTED and `swap_locked` has released
    //    the lock on return, so — and ONLY now, never before — best-effort notify a
    //    running daemon to arm its cooldown, so its next poll does not immediately
    //    revert this choice. A failure (no daemon, a timeout) is logged and ignored:
    //    the keychain write is authoritative, so the manual swap already succeeded.
    if let Err(err) = seams.notifier.notify().await {
        eprintln!("sessiometer: manual-hold notify skipped (is the daemon running?): {err}");
    }

    println!("{}", swap_confirmation(&active_label, &target_label));
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
    let notifier = ControlSocketNotifier {
        socket: paths::control_socket()?,
    };
    let mut log = EventLog::open()?;
    run_use(
        &config,
        &query,
        force,
        in_cooldown,
        Seams {
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

#[cfg(test)]
mod tests {
    use super::*;

    use std::cell::Cell;
    use std::path::PathBuf;

    use crate::claude_state::OauthAccount;
    use crate::config::Tunables;
    use crate::keychain::{Credential, FakeCredentialStore};
    use crate::stash::{FakeAccountStash, StashedAccount};
    use crate::usage::Usage;

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
        async fn poll(&self, _account: &Account, _active: bool) -> Result<Usage> {
            self.calls.set(self.calls.get() + 1);
            match self.probe {
                Probe::Live { weekly } => Ok(Usage {
                    session: 0.10,
                    weekly,
                    weekly_resets_at: None,
                    session_resets_at: None,
                }),
                Probe::Dead => Err(Error::UsageUnauthorized),
                Probe::ScopeMissing => Err(Error::UsageScopeMissing),
                Probe::Transient => Err(Error::UsageTransient {
                    status: 503,
                    retry_after: None,
                }),
                Probe::Locked => Err(Error::KeychainLocked { op: "read" }),
            }
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

    /// Run `use spare` (uuid `u-B`) against a fresh fixture: active = `work` (`u-A`).
    /// Returns the result, the store, the stash, the poll-call count, and the log's
    /// text — everything a test needs to assert the swap (or its absence).
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
        // claude.json shows an account NOT in the roster → the outgoing account is
        // unknown, so the swap (which re-stashes it) cannot run. ZERO writes.
        let config = config_ab();
        let (store, stash) = seeded_store_and_stash().await;
        let (_json_dir, json) = claude_json_for("u-UNKNOWN");
        let log_dir = tempfile::tempdir().unwrap();
        let mut log = EventLog::at(&log_dir.path().join("sessiometer.log")).unwrap();
        let poller = FakePoller::new(Probe::Live { weekly: 0.10 });
        let lock_dir = tempfile::tempdir().unwrap();
        let lock_path = lock_dir.path().join("swap.lock");
        let notifier = FakeNotifier::ok();
        let result = run_use(
            &config,
            "spare",
            false,
            false,
            Seams {
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
        // The swap never ran, so the daemon was never notified (no manual hold to
        // signal). ZERO writes AND zero notifications.
        assert_eq!(notifier.calls.get(), 0, "an aborted swap must not notify");
        assert!(
            matches!(result, Err(Error::ActiveAccountUnresolved)),
            "got {result:?}"
        );
        assert_eq!(canonical(&store).await, b"A-token", "ZERO writes");
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
        let poller = FakePoller::new(Probe::Live { weekly: 0.10 });
        let lock_dir = tempfile::tempdir().unwrap();
        let lock_path = lock_dir.path().join("swap.lock");
        let result = run_use(
            &config,
            "spare",
            false,
            false,
            Seams {
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
            Error::ActiveAccountUnresolved.to_string(),
            Error::KeychainLocked { op: "read" }.to_string(),
        ]
        .join("\n");
        meter::assert_clean(&corpus, &secrets);
    }
}
