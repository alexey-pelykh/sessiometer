// Copyright (c) 2026 Oleksii PELYKH
// SPDX-License-Identifier: MIT

//! `sessiometer poke [<account>]` — keep a parked account's stored credential fresh.
//!
//! Runs Claude Code once for a parked (non-active) managed account under a dedicated,
//! ephemeral `CLAUDE_CONFIG_DIR`: `claude -p` is spawned pointed at that isolated dir,
//! where **Claude Code performs its own credential refresh** against an isolated
//! keychain item; the command then reads the refreshed credential back, re-stashes it,
//! and tears the isolated dir + item down. `poke` is the trigger — Claude Code does the
//! refresh. The live `Claude Code-credentials` item the active session reads is never
//! touched.
//!
//! Two modes:
//!   - `poke <account>` — one cycle for the named account (resolved by label OR
//!     account-uuid, the same resolution the offline `list` view keys on, #17).
//!   - `poke` — one cycle for every parked account whose stored token is near expiry.
//!
//! A thin caller over the isolated-refresh engine (issue #102, [`crate::refresh`]); it
//! honors the engine's documented Caller contract:
//!   - **Refresh PARKED accounts only.** The ACTIVE account (resolved TOKEN-FIRST via the
//!     shared [`crate::active`] resolver — canonical keychain token → stash byte-match,
//!     `~/.claude.json` a display-only fallback — so a forced-logout that clears the
//!     display cannot mask a still-active account, issues #207/#250) is excluded —
//!     named-mode REFUSES it ([`Error::PokeTargetActive`]), all-mode SKIPS it. "Mid-swap"
//!     exclusion needs no logic here: the engine holds the swap lock around its stash
//!     read + re-stash, so a
//!     concurrent swap can never interleave (issue #105 note, "the lock enforces the
//!     latter"). A one-shot cannot know the daemon's *imminent* swap target, but that
//!     residual is bounded and self-healing — the engine's CAS re-stash plus the
//!     dead-credential recovery path (#13/#42) absorb it (worst case a wasted refresh);
//!     the in-daemon periodic tick (#105) is the caller that runs with full swap-state
//!     knowledge.
//!   - **A refresh `Err` is NON-fatal.** A re-stash that fails after a real token
//!     rotation forfeits the fresh token, but that is bounded and RECOVERABLE via the
//!     existing dead-credential re-login path (#13/#42) — never corruption. The
//!     all-accounts sweep reports such an error and moves on; named-mode surfaces it
//!     to the operator who targeted that one account.
//!
//! Every output is redacted to non-secret handles (issue #15): a line names only the
//! account's label and the cycle's classification (refreshed / no change / dead /
//! error), never a token. The engine's [`RefreshReport`] is itself secret-free.
//!
//! When the daemon's current verdict (read best-effort over the same control socket `status`
//! uses) marks the poked account dead AND the cycle produced a fresh, re-stashed token, `poke`
//! clears the quarantine AT THE SOURCE (issue #428): it sends the existing `#275 Restored`
//! control signal — the same authenticated, non-activating un-quarantine primitive
//! `login`/reconcile already uses (ADR-0008), reused with NO new socket surface and NO new
//! spawn path — so the daemon un-quarantines and re-polls through the fresh stash, and the
//! reported line states the quarantine was cleared (superseding #163's vacuous `claude /login`
//! / "next refresh sweep" cue, which was false with `[refresh]` off). A cycle that did NOT
//! prove the refresh token — a genuinely `dead` credential, a `no change`, or a refresh a
//! concurrent change kept from re-stashing — is reported as-is (a dead credential still points
//! at `claude /login`). No daemon reachable, or a non-dead verdict, keeps the plain wording.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::active;
use crate::config::{Account, Config};
use crate::daemon::{AccountStatusLine, StatusResponse};
use crate::error::{Error, Result};
use crate::keychain::{CredentialStore, RealCredentialStore};
use crate::observability::CredentialHealth;
use crate::paths;
use crate::refresh::{self, RefreshOutcome, RefreshReport};
use crate::stash::{AccountStash, RealAccountStash};
use crate::use_account::resolve_target;

/// How close to its `expiresAt` a parked account's stored token must be for the
/// all-accounts `poke` to refresh it (already-expired included).
///
/// **Provisional** (issue #104). The refresh-token TTL is deliberately not yet pinned —
/// the #102 engine's own telemetry answers it (its AC-3 durable-TTL observation) — and
/// the periodic-tick issue (#105) owns the CONFIGURABLE threshold. One hour is a
/// conservative default: it refreshes a genuinely-stale parked token without poking a
/// freshly-refreshed one on every run (each refresh may rotate the refresh token). An
/// operator who wants to refresh a specific account regardless names it directly, which
/// bypasses this filter.
const NEAR_EXPIRY_HORIZON: Duration = Duration::from_secs(60 * 60);

/// The per-account isolated-refresh operations [`run_poke`] drives, injected as a seam
/// so the whole selection → refresh → report flow runs hermetically against an
/// in-memory fake in tests — exactly as [`crate::use_account`]'s `run_use` injects its
/// swap seams. The production implementation is [`RealPokeEngine`].
trait PokeEngine {
    /// The stored credential's `expiresAt` (epoch ms) for `account`, or `None` if it is
    /// unreadable — drives the all-accounts near-expiry filter.
    async fn stored_expires_at(&self, account: &Account) -> Option<i64>;
    /// Run one isolated-refresh cycle for `account` (the #102 engine).
    async fn refresh(&self, account: &Account) -> Result<RefreshReport>;
}

/// The production [`PokeEngine`]: the real keychain-backed stash plus the resolved
/// `claude` binary, wired straight into the #102 engine entry points
/// ([`refresh::stored_expires_at`], [`refresh::refresh_account`]).
struct RealPokeEngine {
    stash: RealAccountStash,
    claude_binary: PathBuf,
}

impl PokeEngine for RealPokeEngine {
    async fn stored_expires_at(&self, account: &Account) -> Option<i64> {
        refresh::stored_expires_at(&self.stash, &account.stash()).await
    }

    async fn refresh(&self, account: &Account) -> Result<RefreshReport> {
        refresh::refresh_account(
            &self.stash,
            &account.stash(),
            &account.account_uuid,
            self.claude_binary.clone(),
        )
        .await
    }
}

/// Sends the existing `#275 Restored` control signal to un-quarantine an account by uuid
/// WITHOUT activating it (issue #428) — the send-side of the primitive already wired for
/// `login`/reconcile (ADR-0008), injected as a seam so the hermetic core drives it against
/// an in-memory fake exactly as [`PokeEngine`] injects the refresh engine. Reused with NO
/// new socket surface and NO new spawn path (ADR-0008 explicitly reuses this one primitive).
trait RestoreNotifier {
    /// Best-effort: `Ok(())` once the daemon has RECEIVED the signal (it applies the existing
    /// `apply_refresh_restore` primitive — un-quarantine + re-tick, no canonical write, no
    /// active-account change), `Err` when no daemon is reachable or the exchange fails.
    async fn send_restored(&self, uuid: &str) -> Result<()>;
}

/// The production [`RestoreNotifier`]: resolves the control socket and sends through the
/// existing `#275` client ([`crate::daemon::notify_restored`]) — mirroring `capture.rs`'s
/// `notify_daemon_restored`, but RETURNING the send result (poke reports the honest outcome)
/// rather than logging and swallowing it. An unresolvable socket path degrades to `Err`, so
/// poke reports the un-quarantine as not delivered rather than falsely claiming it cleared.
struct RealRestoreNotifier;

impl RestoreNotifier for RealRestoreNotifier {
    async fn send_restored(&self, uuid: &str) -> Result<()> {
        let socket = paths::control_socket()?;
        crate::daemon::notify_restored(&socket, uuid).await
    }
}

/// `sessiometer poke [<account>]` — the public entry point.
///
/// Loads the roster, resolves which account (if any) is active (to exclude it), and
/// runs [`run_poke`] over the real engine seam. `target` is `Some(query)` for the named
/// mode and `None` for the all-accounts mode.
pub(crate) async fn poke(target: Option<String>) -> Result<()> {
    let config = Config::load()?;
    // Nothing to refresh if the roster is empty — the same friendly empty-state the
    // offline `list` view reports.
    config.require_roster()?;

    // The isolated-refresh dir + the swap lock live under the native-local support dir;
    // ensure it (0700) before the engine touches it (mirrors `use`).
    paths::ensure_private_dir(&paths::support_dir()?)?;

    // Resolve WHICH account is active TOKEN-FIRST (issue #207/#250): read the canonical
    // keychain credential and byte-match it against the stashes, `~/.claude.json`'s
    // clobberable `oauthAccount` a DISPLAY-only fallback. One `RealAccountStash` is reused
    // — borrowed here for resolution, then moved into the engine — so this adds only a
    // `CredentialStore` read over the pre-#250 display-only path.
    let store = RealCredentialStore::new();
    let stash = RealAccountStash::new();
    let active_uuid =
        resolve_active_uuid(&config.roster, &store, &stash, &paths::claude_json()?).await?;
    let engine = RealPokeEngine {
        stash,
        claude_binary: paths::claude_binary().await?,
    };
    // The daemon's current per-account verdict (#163), read best-effort BEFORE the
    // cycle so a refreshed-but-still-quarantined account is reported honestly rather
    // than as a misleading bare `refreshed`. `None` when no daemon is reachable — poke
    // then keeps its plain wording (it never depends on a daemon being up).
    let daemon_status = daemon_status_best_effort().await;
    // The send-side of the existing `#275 Restored` primitive (issue #428): after a
    // re-stashed refresh of a daemon-quarantined account, poke clears the quarantine at the
    // source over the SAME control socket, reusing `login`/reconcile's un-quarantine path.
    let notifier = RealRestoreNotifier;
    run_poke(
        &config.roster,
        target.as_deref(),
        active_uuid.as_deref(),
        now_ms(),
        &engine,
        daemon_status.as_ref(),
        &notifier,
    )
    .await
}

/// The active account's uuid (the one logged in to Claude Code), or `None` when no
/// account is active — resolved TOKEN-FIRST via the shared [`crate::active`] resolver,
/// exactly as the `use` swap and the daemon poll do (issue #207).
///
/// The canonical keychain credential is the authoritative bearer of the live session;
/// `~/.claude.json`'s `oauthAccount` is only the clobberable, last-writer-wins DISPLAY
/// half, which Claude Code clears out-of-band on a forced logout. The pre-#207
/// display-only path let a cleared display mask a still-active account (canonical token
/// unchanged) — defeating poke's "exclude the active account" safety and letting it
/// refresh the live session's shared token (issue #250). Resolution now consults the
/// canonical first: a byte-match against a stash identifies the active account even when
/// the display is cleared or stale.
///
/// The canonical is read ONCE and classified exactly as `use`'s swap does
/// ([`crate::use_account`], issue #212): a LOCKED keychain, or any other PRESENT-but-unreadable
/// failure, REFUSES (propagates the error) — poke must EXCLUDE the active account, so a
/// canonical it merely *could not read* is never degraded to "nothing is active" (the
/// exact blind spot #250 closes). Only a CONFIRMED-absent canonical
/// ([`Error::CredentialNotFound`], the scrubbed item) degrades to the display-only signal
/// — there is no live token to protect. `None` (canonical present but matching no stash
/// AND the display resolving nothing, or both signals absent) means truly logged out ⇒
/// every roster account is parked and safe to poke.
///
/// Returns the resolved account's uuid — converting the resolver's roster INDEX at the
/// boundary — so poke's hermetic core keeps taking a bare `Option<&str>`. The #15
/// redaction invariant holds by construction: the shared resolver yields only an index,
/// never a token or email.
async fn resolve_active_uuid<C: CredentialStore, S: AccountStash>(
    roster: &[Account],
    store: &C,
    stash: &S,
    claude_json: &Path,
) -> Result<Option<String>> {
    let canonical = match store.read().await {
        Ok(canonical) => Some(canonical),
        // Locked ≠ gone (transient — retry when unlocked): refuse rather than risk poking
        // a still-active account behind a locked keychain (issue #250).
        Err(err @ Error::KeychainLocked { .. }) => return Err(err),
        // CONFIRMED absent (`errSecItemNotFound`): the scrubbed canonical — no live token
        // to protect, so degrade to the display-only signal below.
        Err(Error::CredentialNotFound) => None,
        // PRESENT-but-unreadable for any other reason ("could not read" ≠ "gone"): refuse,
        // never degrade to poke-all-on-cleared-display, the exact #250 blind spot.
        Err(err) => return Err(err),
    };
    let active = match &canonical {
        Some(canonical) => active::resolve_account_for(roster, stash, claude_json, canonical).await,
        // No readable canonical → the display is the only remaining signal (itself possibly
        // cleared, leaving the account genuinely logged out ⇒ `None` ⇒ poke all).
        None => active::resolve_via_display(roster, claude_json),
    };
    Ok(active.map(|idx| roster[idx].account_uuid.clone()))
}

/// Run the requested `poke` mode over the injected `engine`: named (one account) or
/// all-accounts (every near-expiry parked account). `active_uuid` is the account to
/// exclude (`None` ⇒ none active); `now_ms` is the clock for the near-expiry filter.
///
/// The hermetic core — generic over its engine seam so tests drive it with an in-memory
/// fake, exactly as `run_use` is driven.
async fn run_poke<E: PokeEngine, N: RestoreNotifier>(
    roster: &[Account],
    target: Option<&str>,
    active_uuid: Option<&str>,
    now_ms: i64,
    engine: &E,
    daemon_status: Option<&StatusResponse>,
    notifier: &N,
) -> Result<()> {
    match target {
        Some(query) => {
            poke_named(roster, query, active_uuid, engine, daemon_status, notifier).await
        }
        None => poke_all(roster, active_uuid, now_ms, engine, daemon_status, notifier).await,
    }
}

/// Whether `account` is the currently-active one — the single safety predicate both
/// modes gate on (named-mode refuses it, all-mode skips it). `active_uuid` is `None`
/// when no account is active, in which case nothing is active.
fn is_active(account: &Account, active_uuid: Option<&str>) -> bool {
    Some(account.account_uuid.as_str()) == active_uuid
}

/// `poke <account>`: resolve the named account, refuse if it is active, run one cycle,
/// and report. A completed cycle (any classification, including `error`) is reported
/// and exits success; a hard `Err` is PROPAGATED — non-fatal for the credential (the
/// recovery path heals a forfeited token), but the operator who named one account
/// should see the typed failure (a locked keychain, a contended swap lock).
async fn poke_named<E: PokeEngine, N: RestoreNotifier>(
    roster: &[Account],
    query: &str,
    active_uuid: Option<&str>,
    engine: &E,
    daemon_status: Option<&StatusResponse>,
    notifier: &N,
) -> Result<()> {
    let account = &roster[resolve_target(roster, query)?];
    if is_active(account, active_uuid) {
        return Err(Error::PokeTargetActive {
            label: account.label.clone(),
        });
    }
    let report = engine.refresh(account).await?;
    let quarantined = daemon_marks_quarantined(daemon_status, roster, &account.label);
    let restore = resolve_restore(account, &report, quarantined, notifier).await;
    println!(
        "{}",
        poke_line(&account.label, &poke_outcome(&report, restore))
    );
    Ok(())
}

/// `poke`: run one cycle for every PARKED (non-active) account whose stored token is
/// near expiry. The active account is excluded here; "mid-swap" exclusion is enforced
/// by the swap lock the engine acquires, not by this selection (issue #105). Per-account
/// errors are non-fatal — reported (the typed message is secret-free) and the sweep
/// continues, so one account's transient failure never aborts the rest.
async fn poke_all<E: PokeEngine, N: RestoreNotifier>(
    roster: &[Account],
    active_uuid: Option<&str>,
    now_ms: i64,
    engine: &E,
    daemon_status: Option<&StatusResponse>,
    notifier: &N,
) -> Result<()> {
    let horizon_ms = NEAR_EXPIRY_HORIZON.as_millis() as i64;
    let mut selected = Vec::new();
    for account in roster {
        if is_active(account, active_uuid) {
            continue; // never refresh the active account (engine Caller contract)
        }
        let expires_at = engine.stored_expires_at(account).await;
        if is_near_expiry(expires_at, now_ms, horizon_ms) {
            selected.push(account);
        }
    }

    if selected.is_empty() {
        println!("no parked accounts are near expiry — nothing to poke");
        return Ok(());
    }

    println!("poking {} near-expiry parked account(s):", selected.len());
    for account in selected {
        let outcome = match engine.refresh(account).await {
            // Each line carries the daemon's own verdict for THIS account (#163); a
            // re-stashed refresh of a quarantined one ALSO clears its quarantine via the
            // #275 signal (#428), so a swept account recovers exactly as a named poke does.
            Ok(report) => {
                let quarantined = daemon_marks_quarantined(daemon_status, roster, &account.label);
                let restore = resolve_restore(account, &report, quarantined, notifier).await;
                poke_outcome(&report, restore)
            }
            // Secret-free: every `Error` Display is redaction-safe (issue #15).
            Err(err) => format!("error ({err})"),
        };
        println!("  {}", poke_line(&account.label, &outcome));
    }
    Ok(())
}

/// Whether a stored token is *near expiry*: its `expiresAt` is within `horizon_ms` of
/// `now_ms` (already-expired included). `None` — the expiry could not be read — is NOT
/// near-expiry: the all-accounts sweep skips a stash it cannot even read (a locked
/// keychain, an absent item); the operator can still name that account explicitly,
/// which surfaces the underlying error.
fn is_near_expiry(expires_at_ms: Option<i64>, now_ms: i64, horizon_ms: i64) -> bool {
    match expires_at_ms {
        Some(expires_at) => expires_at <= now_ms.saturating_add(horizon_ms),
        None => false,
    }
}

/// The non-secret one-line classification for a completed cycle's report (issue #15:
/// the cycle's outcome, never a token).
fn outcome_label(report: &RefreshReport) -> &'static str {
    match report.outcome {
        RefreshOutcome::Refreshed { .. } if report.re_stashed => "refreshed",
        // A refresh the CAS step discarded: a concurrent swap / login changed the stash
        // since the cycle began, so that credential is authoritative (issue #102 step 7)
        // and the fresh token was not stored. Surfaced honestly.
        RefreshOutcome::Refreshed { .. } => {
            "refreshed but not re-stashed (a concurrent change took precedence)"
        }
        RefreshOutcome::NoChange => "no change",
        RefreshOutcome::Dead => "dead — needs re-login",
        // The one-shot `poke` label folds every error sub-cause to "error"; the non-secret
        // `reason=` sub-class (issue #377) is a periodic-sweep EVENT field, not surfaced here.
        RefreshOutcome::Error(_) => "error",
    }
}

/// Whether a completed cycle warrants clearing the daemon quarantine (issue #428): the
/// daemon currently has the account OUT OF ROTATION (`quarantined_per_daemon`, resolved by
/// [`daemon_marks_quarantined`] — a `Degraded` access-token 401-streak (issue #427) or a
/// proven-`Dead` verdict (#261)) AND the cycle produced a fresh, re-stashed token — the exact
/// condition that used to merely PRINT a "still dead" stall (#163), now the trigger to
/// actually un-quarantine via the `#275 Restored` signal so the daemon re-polls through the
/// fresh stash. A `no change` / `dead` / not-re-stashed cycle is NOT a proven refresh (the
/// refresh token was never exercised, was cleared, or its fresh result was discarded by a
/// concurrent change), so it never fires: a `Degraded` account whose refresh SUCCEEDS recovers
/// here (the fix for issue #427 — its refresh token was always valid), while a genuinely dead
/// credential — whose refresh returns `dead`, not `Refreshed` — is filtered out and still
/// points at `claude /login`.
fn should_restore(report: &RefreshReport, quarantined_per_daemon: bool) -> bool {
    quarantined_per_daemon
        && matches!(report.outcome, RefreshOutcome::Refreshed { .. })
        && report.re_stashed
}

/// What the issue-#428 quarantine-clear attempt did for a completed cycle — threaded into
/// [`poke_outcome`] so the reported line reflects what ACTUALLY happened to the quarantine,
/// never a fabricated "cleared" when the signal did not land.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Restore {
    /// No attempt: the cycle did not warrant one ([`should_restore`] false) — not
    /// daemon-quarantined, or no fresh re-stashed token to re-poll through. The line carries
    /// the cycle's own plain classification.
    Skipped,
    /// The `restored` signal was DELIVERED: the daemon un-quarantines and re-polls through
    /// the fresh stash, so the account recovers on the next tick — no re-login needed.
    Cleared,
    /// The signal could NOT be delivered (the daemon went away since the pre-cycle status
    /// read, or the exchange failed): the fresh token IS stashed, but the quarantine persists
    /// until the daemon is signalled again.
    Undelivered,
}

/// Resolve the issue-#428 quarantine-clear for a completed cycle: if the cycle warrants it
/// ([`should_restore`]), send the `#275 Restored` signal through `notifier` and report whether
/// it reached the daemon; otherwise [`Restore::Skipped`]. The single place both poke modes
/// attempt the un-quarantine — keeping the pure send decision out of the I/O and the message
/// layer. Safe + idempotent at the daemon (an unknown / already-healthy uuid is a no-op,
/// ADR-0008), and best-effort: a failed send is reported honestly, never fatal.
async fn resolve_restore<N: RestoreNotifier>(
    account: &Account,
    report: &RefreshReport,
    quarantined_per_daemon: bool,
    notifier: &N,
) -> Restore {
    if !should_restore(report, quarantined_per_daemon) {
        return Restore::Skipped;
    }
    match notifier.send_restored(&account.account_uuid).await {
        Ok(()) => Restore::Cleared,
        Err(_) => Restore::Undelivered,
    }
}

/// The honest per-account outcome text for a completed cycle, given the resolved issue-#428
/// quarantine-clear (`restore`).
///
/// A [`Cleared`](Restore::Cleared) / [`Undelivered`](Restore::Undelivered) restore means the
/// account was daemon-quarantined AND the refresh re-stashed a fresh token: the line reports
/// the un-quarantine RESULT ([`QUARANTINE_CLEARED`] / [`RESTORE_UNDELIVERED`]) rather than a
/// bare `"refreshed"` that would read as "healthy" while hiding the quarantine. Every
/// [`Skipped`](Restore::Skipped) cycle — a healthy or `Unknown ⚪` verdict, no daemon
/// reachable, a genuinely `dead` credential, a `no change`, or a refresh a concurrent change
/// kept from re-stashing — carries its own plain classification unchanged, a true statement of
/// the cycle that ran, never a fabricated liveness claim.
fn poke_outcome(report: &RefreshReport, restore: Restore) -> String {
    match restore {
        Restore::Cleared => QUARANTINE_CLEARED.to_owned(),
        Restore::Undelivered => RESTORE_UNDELIVERED.to_owned(),
        Restore::Skipped => outcome_label(report).to_owned(),
    }
}

/// The issue-#428 confirmation for a poke that refreshed a quarantined account AND cleared the
/// daemon quarantine via the `#275 Restored` signal: the token is fresh and the daemon will
/// re-poll through it — superseding #163's `claude /login` / "next refresh sweep" cue, which
/// promised a sweep that never runs with `[refresh]` off. Non-secret (issue #15): a
/// classification + a recovery statement, no token.
const QUARANTINE_CLEARED: &str = "token refreshed; cleared the daemon quarantine — the account \
     will recover on the next poll (no re-login needed)";

/// The issue-#428 honest fallback when the refresh re-stashed a fresh token but the `#275
/// Restored` signal could not reach the daemon (it went away since the pre-cycle status read,
/// or the exchange failed): the token IS fresh and stashed, but the quarantine persists — so
/// the line neither falsely claims recovery nor points at `claude /login` (the credential is
/// not dead). Non-secret (issue #15): a classification + an honest status, no token.
const RESTORE_UNDELIVERED: &str = "token refreshed and stashed, but the daemon could not be \
     reached to clear the quarantine — it persists until the daemon is signalled again";

/// `<label>: <outcome>` — one per-account report line (the label is the non-secret
/// handle, issue #15).
fn poke_line(label: &str, outcome: &str) -> String {
    format!("{label}: {outcome}")
}

/// Whether the daemon's snapshot currently has `label`'s account OUT OF ROTATION
/// (quarantined) — the verdicts `status` projects to an action cue (mirrors `cli::health_cell`):
/// a current daemon's rollup reading `Degraded` (an access-token 401-streak, issue #427) or
/// `Dead` (a refresh proved the credential unrecoverable, #261), or — a pre-#119 daemon that
/// sent no rollup (`health == None`) — its legacy `quarantined` flag (#42). Both modern states
/// are quarantine states the #428 restore may clear; the `Refreshed && re_stashed` gate in
/// [`should_restore`] then decides which actually recover (a Degraded account's refresh
/// succeeds and clears; a Dead one's refresh returns `dead` and stays put — see there).
///
/// A `None` snapshot (no daemon reachable), a `label` absent from the snapshot, or a `label`
/// that is not a unique key are all NOT quarantined: an indeterminate verdict `poke` reports
/// with its plain wording (#163 AC-3), never a fabricated daemon-state claim.
///
/// Matching is by label — the only account handle the wire [`AccountStatusLine`] carries
/// (issue #15: never the uuid or email) — the same key `status` renders by; a renamed/stale
/// label simply misses and degrades to the plain wording. But labels are operator handles and
/// are NOT required to be unique (see [`resolve_target`]), so the label carries a verdict ONLY
/// while it names EXACTLY ONE account on BOTH sides: once in `roster`, and once in the
/// snapshot. Either count off and the bearers are indistinguishable from a label alone, so
/// poke claims nothing rather than attributing the earliest match's state to whichever account
/// it was handed (issue #1086) — the same degrade-rather-than-guess shape `use`'s
/// `cached_viability_for` already takes for this lookup.
///
/// BOTH counts, because each catches a misattribution the other misses. The snapshot side
/// catches a roster the daemon knows better than this process does (an account removed from
/// `config.toml` since); the roster side catches the reverse — the daemon adopts a new roster
/// only when a reload is SIGNALLED (issue #139), so its snapshot legitimately lags, and a
/// second bearer added on disk leaves the wire with one line that is NOT necessarily the poked
/// account's.
///
/// The cost is stated rather than hidden: on an ambiguous label poke stops clearing
/// quarantines as well as stops fabricating them, so a genuinely quarantined bearer keeps its
/// plain wording and is left to the daemon's #106 refresh sweep WHEN `[refresh]` is enabled, or
/// to a `login`/reconcile. Both halves of that are narrower than they look, and neither should be
/// read as a promise. ADR-0008's other daemon-side revival, #42's `note_poll_outcome`
/// live-recovery, is scoped to the STUCK ACTIVE — and poke excludes the active account by
/// construction — so it cannot apply here at all. The #106 sweep is the only one left, and this
/// same file records it as conditional (`QUARANTINE_CLEARED` above: "a sweep that never runs with
/// `[refresh]` off"). With refresh disabled there is NO daemon-side revival, and the fallback is
/// `login`/reconcile alone.
/// Making the wire disambiguate instead would mean carrying a stable per-account key on a
/// surface #15 restricts to the operator-authored handle. RESIDUAL, unclosable without such a
/// key: a label REASSIGNED to a different account between the snapshot and the current roster
/// matches once on each side and still reads the stale bearer's line — the same residual
/// `cached_viability_for` records for `use`. Bounded: the verdict drives the report wording
/// and a `#275 Restored` signal that is a no-op for an unknown or already-healthy uuid
/// (ADR-0008).
fn daemon_marks_quarantined(
    daemon_status: Option<&StatusResponse>,
    roster: &[Account],
    label: &str,
) -> bool {
    let Some(status) = daemon_status else {
        return false;
    };
    if sole(roster.iter().filter(|account| account.label == label)).is_none() {
        return false;
    }
    sole(status.accounts.iter().filter(|line| line.label == label)).is_some_and(line_is_quarantined)
}

/// The one and only element of `candidates`, or `None` when it holds zero or more than one —
/// the "exactly one match, else no answer" primitive [`daemon_marks_quarantined`] applies to
/// each side of its label lookup. Short-circuits on the second MATCH: it pulls at most two items
/// from `candidates`, but where `candidates` is a filter — as it is at both call sites — the
/// second pull drives that filter until it finds a second match or exhausts the underlying
/// iterator. So on a UNIQUE label, which is the common case and the hot path, it walks the whole
/// roster exactly as a count would. Cost is nil at roster scale; the claim is narrowed here
/// because an earlier wording said "rather than a count of the whole roster", which is false for
/// the case that matters.
fn sole<T>(mut candidates: impl Iterator<Item = T>) -> Option<T> {
    let only = candidates.next()?;
    candidates.next().is_none().then_some(only)
}

/// Resolve ONE status line's quarantine the same way `status` does (`cli::health_cell`): the
/// daemon's rollup when present — `Degraded` (a 401-streak, issue #427) and `Dead` (a proven
/// refresh death, #261) are both out of rotation — else, a pre-#119 daemon that sent no rollup
/// (`health == None`), the legacy `quarantined` flag, so an old daemon reads correctly rather
/// than as a defaulted-healthy line over a quarantined account. `Healthy` / `Unknown` /
/// `Stale` / `AtRisk` are all still IN rotation (poke keeps the plain wording for them).
fn line_is_quarantined(line: &AccountStatusLine) -> bool {
    match line.health {
        Some(health) => matches!(health, CredentialHealth::Degraded | CredentialHealth::Dead),
        None => line.quarantined,
    }
}

/// Best-effort read of the daemon's per-account verdicts over the control socket — the SAME
/// `{"cmd":"status"}` snapshot the `status` command projects (issue #8). Returns `None` on
/// ANY failure (no daemon, an absent/refused socket, a malformed reply): `poke` then keeps
/// its plain wording rather than crash or fabricate a daemon-state claim (#163 AC-3).
///
/// Deliberately fail-QUIET, unlike the `status` command's fail-LOUD `cli::query_status`
/// (which surfaces [`Error::DaemonNotRunning`] because a human explicitly asked for status):
/// for `poke`, a missing daemon is an expected, non-error case, so this reader lives here
/// and swallows every error into `None` rather than reusing the erroring client.
async fn daemon_status_best_effort() -> Option<StatusResponse> {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt};

    let path = paths::control_socket().ok()?;
    let stream = tokio::net::UnixStream::connect(&path).await.ok()?;
    let mut buffered = tokio::io::BufReader::new(stream);
    // The same newline-delimited JSON `serve_control` speaks: one request line, one reply.
    buffered.write_all(b"{\"cmd\":\"status\"}\n").await.ok()?;
    buffered.flush().await.ok()?;
    let mut line = String::new();
    buffered.read_line(&mut line).await.ok()?;
    serde_json::from_str(line.trim_end()).ok()
}

/// Current wall-clock as epoch milliseconds (the unit Claude Code's `expiresAt` uses),
/// matching the engine's clock. `0` on the pre-1970 impossible case.
fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::collections::HashMap;

    use crate::claude_state::OauthAccount;
    use crate::keychain::{Credential, FakeCredentialStore};
    use crate::refresh::RefreshErrorReason;
    use crate::stash::{FakeAccountStash, StashedAccount};

    fn acct(label: &str, uuid: &str) -> Account {
        Account {
            account_uuid: uuid.to_owned(),
            label: label.to_owned(),
            enabled: true,
        }
    }

    fn report(outcome: RefreshOutcome, re_stashed: bool) -> RefreshReport {
        RefreshReport {
            outcome,
            expires_at_delta_secs: None,
            re_stashed,
        }
    }

    /// One daemon `status` line for `label`, carrying only the fields poke's #163 verdict
    /// reads: the 5-state rollup (`health`) and the pre-#119 legacy `quarantined` flag.
    /// Everything else is a benign default — poke never inspects it.
    fn status_line(
        label: &str,
        health: Option<CredentialHealth>,
        quarantined: bool,
    ) -> AccountStatusLine {
        AccountStatusLine {
            label: label.to_owned(),
            active: false,
            enabled: true,
            quarantined,
            recovering: false,
            session_pct: None,
            weekly_pct: None,
            session_resets_at: None,
            weekly_resets_at: None,
            weekly_exhausted: false,
            access_expires_at: None,
            refresh_health: None,
            health,
            // Bounded-blindness (#479) is not read by poke's verdict — inert here.
            blind_active: None,
            // Nor is the #882 refresh-token expiry modifier: poke keys off the auth rollup, and the
            // expiry axis is orthogonal to it (surfaced, never acted upon).
            expiry: None,
        }
    }

    /// A daemon `status` snapshot from a set of account lines — what a running daemon
    /// returns to poke's best-effort control-socket read.
    fn status_snapshot(lines: Vec<AccountStatusLine>) -> StatusResponse {
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
            accounts: lines,
            next_swap: None,
        }
    }

    /// What a faked refresh cycle returns for an account.
    #[derive(Clone, Copy)]
    enum FakeRefresh {
        Report(RefreshReport),
        /// A hard cycle error (the engine's `Err` channel — e.g. a contended lock).
        HardError,
    }

    /// In-memory [`PokeEngine`]: canned per-account expiries + refresh results, plus a
    /// record of which accounts (in order) actually had `refresh` called.
    struct FakePokeEngine {
        expiries: HashMap<String, Option<i64>>,
        results: HashMap<String, FakeRefresh>,
        refreshed: RefCell<Vec<String>>,
    }

    impl FakePokeEngine {
        fn new() -> Self {
            Self {
                expiries: HashMap::new(),
                results: HashMap::new(),
                refreshed: RefCell::new(Vec::new()),
            }
        }

        fn with_expiry(mut self, uuid: &str, expires_at: Option<i64>) -> Self {
            self.expiries.insert(uuid.to_owned(), expires_at);
            self
        }

        fn with_result(mut self, uuid: &str, result: FakeRefresh) -> Self {
            self.results.insert(uuid.to_owned(), result);
            self
        }

        fn refreshed(&self) -> Vec<String> {
            self.refreshed.borrow().clone()
        }
    }

    impl PokeEngine for FakePokeEngine {
        async fn stored_expires_at(&self, account: &Account) -> Option<i64> {
            self.expiries.get(&account.account_uuid).copied().flatten()
        }

        async fn refresh(&self, account: &Account) -> Result<RefreshReport> {
            self.refreshed
                .borrow_mut()
                .push(account.account_uuid.clone());
            match self.results.get(&account.account_uuid) {
                Some(FakeRefresh::Report(r)) => Ok(*r),
                Some(FakeRefresh::HardError) => Err(Error::SwapLockBusy),
                None => Ok(report(RefreshOutcome::NoChange, false)),
            }
        }
    }

    /// In-memory [`RestoreNotifier`]: records every uuid a cycle asked to un-quarantine, and
    /// can be set to FAIL the send (the daemon-went-away race) so the `Undelivered` path is
    /// testable — the restore counterpart of [`FakePokeEngine`]'s refresh recorder.
    struct FakeRestoreNotifier {
        fail: bool,
        sent: RefCell<Vec<String>>,
    }

    impl FakeRestoreNotifier {
        fn new() -> Self {
            Self {
                fail: false,
                sent: RefCell::new(Vec::new()),
            }
        }

        /// A notifier whose send always fails — the daemon vanished between the status read
        /// and the signal, or the exchange errored.
        fn failing() -> Self {
            Self {
                fail: true,
                sent: RefCell::new(Vec::new()),
            }
        }

        fn sent(&self) -> Vec<String> {
            self.sent.borrow().clone()
        }
    }

    impl RestoreNotifier for FakeRestoreNotifier {
        async fn send_restored(&self, uuid: &str) -> Result<()> {
            self.sent.borrow_mut().push(uuid.to_owned());
            if self.fail {
                Err(Error::Io(std::io::Error::other("no daemon reachable")))
            } else {
                Ok(())
            }
        }
    }

    fn write_claude_json(contents: &str) -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".claude.json");
        std::fs::write(&path, contents).unwrap();
        (dir, path)
    }

    // --- is_near_expiry (pure) ----------------------------------------------

    #[test]
    fn near_expiry_includes_within_horizon_and_already_expired() {
        let now = 1_000_000;
        let horizon = 3_600_000; // 1h in ms
                                 // Expires in 30 min — within horizon.
        assert!(is_near_expiry(Some(now + 1_800_000), now, horizon));
        // Already expired.
        assert!(is_near_expiry(Some(now - 1), now, horizon));
        // Exactly at the boundary (<=).
        assert!(is_near_expiry(Some(now + horizon), now, horizon));
    }

    #[test]
    fn near_expiry_excludes_far_future_and_unreadable() {
        let now = 1_000_000;
        let horizon = 3_600_000;
        // Expires in 2h — beyond the horizon.
        assert!(!is_near_expiry(Some(now + 7_200_000), now, horizon));
        // Unreadable expiry — never selected by the sweep.
        assert!(!is_near_expiry(None, now, horizon));
    }

    // --- outcome_label / poke_line (pure) -----------------------------------

    #[test]
    fn outcome_label_maps_every_classification() {
        assert_eq!(
            outcome_label(&report(RefreshOutcome::Refreshed { rotated: true }, true)),
            "refreshed"
        );
        assert_eq!(
            outcome_label(&report(RefreshOutcome::Refreshed { rotated: true }, false)),
            "refreshed but not re-stashed (a concurrent change took precedence)"
        );
        assert_eq!(
            outcome_label(&report(RefreshOutcome::NoChange, false)),
            "no change"
        );
        assert_eq!(
            outcome_label(&report(RefreshOutcome::Dead, false)),
            "dead — needs re-login"
        );
        assert_eq!(
            outcome_label(&report(
                RefreshOutcome::Error(RefreshErrorReason::SpawnFailed),
                false
            )),
            "error"
        );
    }

    #[test]
    fn poke_line_is_label_then_outcome() {
        assert_eq!(poke_line("work", "refreshed"), "work: refreshed");
    }

    // --- #163 daemon-verdict resolution (pure) ------------------------------

    #[test]
    fn line_is_quarantined_reads_the_rollup_then_the_legacy_flag() {
        // A current daemon: the rollup is authoritative — `Degraded` (401-streak, #427) and
        // `Dead` (proven death, #261) are both out of rotation; the rest are in rotation.
        assert!(line_is_quarantined(&status_line(
            "a",
            Some(CredentialHealth::Degraded),
            false
        )));
        assert!(line_is_quarantined(&status_line(
            "a",
            Some(CredentialHealth::Dead),
            false
        )));
        assert!(!line_is_quarantined(&status_line(
            "a",
            Some(CredentialHealth::Healthy),
            false
        )));
        assert!(!line_is_quarantined(&status_line(
            "a",
            Some(CredentialHealth::Unknown),
            false
        )));
        assert!(!line_is_quarantined(&status_line(
            "a",
            Some(CredentialHealth::Stale),
            false
        )));
        assert!(!line_is_quarantined(&status_line(
            "a",
            Some(CredentialHealth::AtRisk),
            false
        )));
        // The rollup WINS when present: a quarantine verdict holds even with the raw
        // `quarantined` flag unset (a refresh-cleared-in-place credential, #119), and
        // `Healthy` is NOT quarantined even if a stale `quarantined` flag lingers — poke
        // reads exactly what `status` renders.
        assert!(line_is_quarantined(&status_line(
            "a",
            Some(CredentialHealth::Degraded),
            false
        )));
        assert!(!line_is_quarantined(&status_line(
            "a",
            Some(CredentialHealth::Healthy),
            true
        )));
        // A pre-#119 daemon (no rollup): fall back to the legacy `quarantined` flag.
        assert!(line_is_quarantined(&status_line("a", None, true)));
        assert!(!line_is_quarantined(&status_line("a", None, false)));
    }

    #[test]
    fn daemon_marks_quarantined_reads_the_named_line_or_degrades_to_not_quarantined() {
        let roster = vec![
            acct("work", "u-A"),
            acct("gone", "u-B"),
            acct("spare", "u-C"),
        ];
        let snap = status_snapshot(vec![
            status_line("work", Some(CredentialHealth::Degraded), false),
            status_line("gone", Some(CredentialHealth::Dead), false),
            status_line("spare", Some(CredentialHealth::Healthy), false),
        ]);
        // Each account's OWN verdict is read, by label — a Degraded 401-streak (#427) and a
        // proven-Dead credential (#261) are both quarantined; a Healthy one is not.
        assert!(daemon_marks_quarantined(Some(&snap), &roster, "work"));
        assert!(daemon_marks_quarantined(Some(&snap), &roster, "gone"));
        assert!(!daemon_marks_quarantined(Some(&snap), &roster, "spare"));
        // No daemon reachable (None) → indeterminate → not quarantined (plain wording, AC-3).
        assert!(!daemon_marks_quarantined(None, &roster, "work"));
        // A label the daemon does not list → indeterminate → not quarantined.
        assert!(!daemon_marks_quarantined(Some(&snap), &roster, "ghost"));
    }

    #[test]
    fn daemon_marks_quarantined_needs_the_label_unique_on_both_sides() {
        // Issue #1086 at the predicate: the label is a valid key only while it names exactly
        // one account in the roster AND one line on the wire. The `Dead` verdict below is
        // deliberately the same in all four cases, so what varies is only the KEY's validity.
        let dead = || status_line("work", Some(CredentialHealth::Dead), false);

        // Unique on both sides → the verdict is readable. The control for the three below.
        let unique = vec![acct("work", "u-A")];
        assert!(daemon_marks_quarantined(
            Some(&status_snapshot(vec![dead()])),
            &unique,
            "work"
        ));
        // Duplicated in the ROSTER, one line on the wire (a snapshot lagging a signalled
        // reload, #139): the single line need not be the caller's account → no claim.
        let dup_roster = vec![acct("work", "u-A"), acct("work", "u-B")];
        assert!(!daemon_marks_quarantined(
            Some(&status_snapshot(vec![dead()])),
            &dup_roster,
            "work"
        ));
        // Duplicated on the WIRE, unique in the roster (the daemon still lists an account
        // dropped from `config.toml`): the earliest line is a guess → no claim.
        assert!(!daemon_marks_quarantined(
            Some(&status_snapshot(vec![dead(), dead()])),
            &unique,
            "work"
        ));
        // Duplicated on both sides — the issue's own scenario.
        assert!(!daemon_marks_quarantined(
            Some(&status_snapshot(vec![dead(), dead()])),
            &dup_roster,
            "work"
        ));
    }

    #[test]
    fn sole_yields_the_single_element_and_nothing_for_zero_or_many() {
        assert_eq!(sole([7].into_iter()), Some(7));
        assert_eq!(sole(std::iter::empty::<u8>()), None);
        assert_eq!(sole([7, 9].into_iter()), None);
        // Short-circuits: a third element is never pulled, so an ambiguity verdict costs two
        // steps of the filtered iterator rather than a walk of the whole roster.
        let mut pulled = 0;
        let counted = [1, 2, 3, 4].into_iter().inspect(|_| pulled += 1);
        assert_eq!(sole(counted), None);
        assert_eq!(pulled, 2);
    }

    #[test]
    fn should_restore_only_on_a_quarantined_verdict_plus_a_restashed_refresh() {
        // The one firing case (issue #428): daemon-quarantined + a fresh, re-stashed token.
        assert!(should_restore(
            &report(RefreshOutcome::Refreshed { rotated: true }, true),
            true
        ));
        // Same cycle, but the daemon does NOT mark it quarantined → nothing to clear.
        assert!(!should_restore(
            &report(RefreshOutcome::Refreshed { rotated: true }, true),
            false
        ));
        // A quarantined verdict, but the cycle did not PROVE the refresh token, so it never fires:
        // - a refresh a concurrent change kept from re-stashing (no fresh stash to re-poll),
        assert!(!should_restore(
            &report(RefreshOutcome::Refreshed { rotated: true }, false),
            true
        ));
        // - no refresh happened (the refresh token was never exercised),
        assert!(!should_restore(
            &report(RefreshOutcome::NoChange, false),
            true
        ));
        // - a genuinely dead credential (still needs re-login),
        assert!(!should_restore(&report(RefreshOutcome::Dead, false), true));
        // - an errored cycle.
        assert!(!should_restore(
            &report(
                RefreshOutcome::Error(RefreshErrorReason::SpawnFailed),
                false
            ),
            true
        ));
    }

    #[test]
    fn poke_outcome_maps_each_restore_state() {
        let refreshed = report(RefreshOutcome::Refreshed { rotated: true }, true);
        // A DELIVERED restore → the cleared-quarantine confirmation, never the bare wording.
        assert_eq!(
            poke_outcome(&refreshed, Restore::Cleared),
            QUARANTINE_CLEARED
        );
        assert_ne!(poke_outcome(&refreshed, Restore::Cleared), "refreshed");
        // An UNDELIVERED restore → the honest fallback (fresh token, quarantine persists).
        assert_eq!(
            poke_outcome(&refreshed, Restore::Undelivered),
            RESTORE_UNDELIVERED
        );
        // A SKIPPED restore → the cycle's own plain classification, unchanged.
        assert_eq!(poke_outcome(&refreshed, Restore::Skipped), "refreshed");
        assert_eq!(
            poke_outcome(
                &report(RefreshOutcome::Refreshed { rotated: true }, false),
                Restore::Skipped
            ),
            "refreshed but not re-stashed (a concurrent change took precedence)"
        );
        assert_eq!(
            poke_outcome(&report(RefreshOutcome::NoChange, false), Restore::Skipped),
            "no change"
        );
        assert_eq!(
            poke_outcome(&report(RefreshOutcome::Dead, false), Restore::Skipped),
            "dead — needs re-login"
        );
        assert_eq!(
            poke_outcome(
                &report(
                    RefreshOutcome::Error(RefreshErrorReason::SpawnFailed),
                    false
                ),
                Restore::Skipped
            ),
            "error"
        );
    }

    // --- #428 resolve_restore: the daemon verdict drives the un-quarantine + message ----

    #[tokio::test]
    async fn dead_account_restashed_refresh_clears_the_quarantine() {
        // AC-1: a parked account the daemon marks Dead, refreshed + re-stashed → poke sends
        // the #275 restored signal for THAT account's uuid and reports the quarantine cleared
        // — never the old vacuous "claude /login" / "next sweep" cue.
        let snap = status_snapshot(vec![status_line(
            "work",
            Some(CredentialHealth::Dead),
            false,
        )]);
        let account = acct("work", "u-A");
        let report = report(RefreshOutcome::Refreshed { rotated: true }, true);
        let notifier = FakeRestoreNotifier::new();
        let restore = resolve_restore(
            &account,
            &report,
            daemon_marks_quarantined(Some(&snap), std::slice::from_ref(&account), "work"),
            &notifier,
        )
        .await;
        assert_eq!(restore, Restore::Cleared);
        assert_eq!(
            notifier.sent(),
            vec!["u-A"],
            "the account's uuid was signalled"
        );
        let line = poke_outcome(&report, restore);
        assert_ne!(line, "refreshed", "must NOT be the misleading bare wording");
        assert!(line.contains("cleared the daemon quarantine"));
        assert!(!line.contains("claude /login"), "no spurious re-login cue");
        assert!(!line.contains("sweep"), "no vacuous refresh-sweep promise");
    }

    #[tokio::test]
    async fn degraded_account_restashed_refresh_clears_the_quarantine() {
        // Issue #427 regression lock: a parked account the daemon marks `Degraded` — its
        // ACCESS token 401-streaked into quarantine, but the REFRESH token is still valid —
        // is refreshed + re-stashed by poke → the #275 restored signal fires and the
        // quarantine clears. This is the exact remedy `status`'s 🟠 "run 'sessiometer poke'"
        // cue advertises. Before #427 folded a bare 401-streak into `Degraded`, this state
        // rendered `Dead` and the restore union already covered it; the rename MUST keep it
        // covered, or poke would re-stash a fresh token yet strand the account quarantined.
        let snap = status_snapshot(vec![status_line(
            "work",
            Some(CredentialHealth::Degraded),
            false,
        )]);
        let account = acct("work", "u-A");
        let report = report(RefreshOutcome::Refreshed { rotated: true }, true);
        let notifier = FakeRestoreNotifier::new();
        let restore = resolve_restore(
            &account,
            &report,
            daemon_marks_quarantined(Some(&snap), std::slice::from_ref(&account), "work"),
            &notifier,
        )
        .await;
        assert_eq!(
            restore,
            Restore::Cleared,
            "a refreshable 401-streak recovers"
        );
        assert_eq!(
            notifier.sent(),
            vec!["u-A"],
            "the account's uuid was signalled"
        );
        let line = poke_outcome(&report, restore);
        assert_ne!(line, "refreshed", "must NOT be the misleading bare wording");
        assert!(line.contains("cleared the daemon quarantine"));
        assert!(!line.contains("claude /login"), "no spurious re-login cue");
    }

    #[tokio::test]
    async fn dead_account_reports_undelivered_when_the_restore_signal_fails() {
        // The daemon vanished between the pre-cycle status read and the signal: the token is
        // stashed, but the quarantine could not be cleared → honest fallback, never a false
        // "cleared" and never a spurious re-login cue (the credential is fresh, not dead).
        let snap = status_snapshot(vec![status_line(
            "work",
            Some(CredentialHealth::Dead),
            false,
        )]);
        let account = acct("work", "u-A");
        let report = report(RefreshOutcome::Refreshed { rotated: true }, true);
        let notifier = FakeRestoreNotifier::failing();
        let restore = resolve_restore(
            &account,
            &report,
            daemon_marks_quarantined(Some(&snap), std::slice::from_ref(&account), "work"),
            &notifier,
        )
        .await;
        assert_eq!(restore, Restore::Undelivered);
        assert_eq!(notifier.sent(), vec!["u-A"], "the send was still attempted");
        let line = poke_outcome(&report, restore);
        assert_eq!(line, RESTORE_UNDELIVERED);
        assert!(!line.contains("claude /login"));
    }

    #[tokio::test]
    async fn healthy_account_is_not_restored_and_keeps_the_plain_wording() {
        // AC-2: a healthy (non-quarantined) parked account → no restore attempt, existing
        // wording unchanged.
        let snap = status_snapshot(vec![status_line(
            "work",
            Some(CredentialHealth::Healthy),
            false,
        )]);
        let account = acct("work", "u-A");
        let report = report(RefreshOutcome::Refreshed { rotated: true }, true);
        let notifier = FakeRestoreNotifier::new();
        let restore = resolve_restore(
            &account,
            &report,
            daemon_marks_quarantined(Some(&snap), std::slice::from_ref(&account), "work"),
            &notifier,
        )
        .await;
        assert_eq!(restore, Restore::Skipped);
        assert!(
            notifier.sent().is_empty(),
            "a healthy account is never signalled"
        );
        assert_eq!(poke_outcome(&report, restore), "refreshed");
    }

    #[tokio::test]
    async fn unknown_or_absent_daemon_never_restores_nor_fabricates_a_verdict() {
        // AC-3: no daemon reachable (None) → no restore, plain wording; no crash, no claim.
        let account = acct("work", "u-A");
        let refreshed = report(RefreshOutcome::Refreshed { rotated: true }, true);
        let notifier = FakeRestoreNotifier::new();
        let restore = resolve_restore(
            &account,
            &refreshed,
            daemon_marks_quarantined(None, std::slice::from_ref(&account), "work"),
            &notifier,
        )
        .await;
        assert_eq!(restore, Restore::Skipped);
        assert!(notifier.sent().is_empty());
        assert_eq!(poke_outcome(&refreshed, restore), "refreshed");
        // A running daemon with an Unknown ⚪ verdict (#137) → likewise no restore, plain
        // wording: the cycle truth, never a fabricated "live" nor a false "dead".
        let unknown = status_snapshot(vec![status_line(
            "work",
            Some(CredentialHealth::Unknown),
            false,
        )]);
        let notifier = FakeRestoreNotifier::new();
        let restore = resolve_restore(
            &account,
            &refreshed,
            daemon_marks_quarantined(Some(&unknown), std::slice::from_ref(&account), "work"),
            &notifier,
        )
        .await;
        assert_eq!(restore, Restore::Skipped);
        assert!(notifier.sent().is_empty());
        assert_eq!(poke_outcome(&refreshed, restore), "refreshed");
    }

    #[tokio::test]
    async fn genuinely_dead_credential_is_not_restored_and_still_points_at_login() {
        // A dead verdict + a `dead` refresh cycle (the refresh token really is cleared) → no
        // restore, and the plain "dead — needs re-login" wording still points at `claude /login`.
        let snap = status_snapshot(vec![status_line(
            "work",
            Some(CredentialHealth::Dead),
            false,
        )]);
        let account = acct("work", "u-A");
        let report = report(RefreshOutcome::Dead, false);
        let notifier = FakeRestoreNotifier::new();
        let restore = resolve_restore(
            &account,
            &report,
            daemon_marks_quarantined(Some(&snap), std::slice::from_ref(&account), "work"),
            &notifier,
        )
        .await;
        assert_eq!(restore, Restore::Skipped);
        assert!(
            notifier.sent().is_empty(),
            "a dead credential is never falsely un-quarantined"
        );
        assert_eq!(poke_outcome(&report, restore), "dead — needs re-login");
    }

    // --- issue #1086: a duplicated label is not a usable key for the daemon's verdict ----
    //
    // `poke` resolves its target by label OR uuid (`resolve_target`), then asks the daemon's
    // snapshot for that account's quarantine verdict keyed on the LABEL — the only account
    // handle the wire carries (issue #15). A duplicated label is a legal, documented roster
    // state, and the lookup took the FIRST bearer's line, so the verdict could belong to a
    // sibling. It drives more than the wording: `resolve_restore` sends the `#275 Restored`
    // signal for the POKED account's uuid on the strength of it, so a wrong verdict is a
    // WRITE, not only a misreport.
    //
    // All four drive the whole hermetic core (`run_poke`), never the predicate directly, so
    // their source is untouched by the fix — the same bytes RED before it and GREEN after.
    // The pair of `refresh` results is the one `should_restore` fires on (a fresh, re-stashed
    // token), which is what makes the verdict observable at all.

    /// The `Refreshed { rotated } + re_stashed` cycle `should_restore` gates on — the only
    /// input that turns a quarantine verdict into an observable signal.
    fn restashed_refresh() -> FakeRefresh {
        FakeRefresh::Report(report(RefreshOutcome::Refreshed { rotated: true }, true))
    }

    #[tokio::test]
    async fn an_ambiguous_labels_verdict_does_not_depend_on_bearer_order() {
        // The issue's core claim, as a property: with two `work` accounts of DIFFERING daemon
        // health, the verdict poke acts on must not flip when the pair is listed the other way
        // round. A first-match `.find()` makes it flip — one order yields the healthy sibling's
        // line, the reverse yields the dead one's — so the same `poke u-healthy` sends a
        // `Restored` signal in one order and not the other. Ordering is not a property of the
        // account being poked, so any order-sensitive answer is a guess.
        async fn sent_for_order(dead_first: bool) -> Vec<String> {
            let (roster, lines) = if dead_first {
                (
                    vec![acct("work", "u-dead"), acct("work", "u-healthy")],
                    vec![
                        status_line("work", Some(CredentialHealth::Dead), false),
                        status_line("work", Some(CredentialHealth::Healthy), false),
                    ],
                )
            } else {
                (
                    vec![acct("work", "u-healthy"), acct("work", "u-dead")],
                    vec![
                        status_line("work", Some(CredentialHealth::Healthy), false),
                        status_line("work", Some(CredentialHealth::Dead), false),
                    ],
                )
            };
            let engine = FakePokeEngine::new().with_result("u-healthy", restashed_refresh());
            let notifier = FakeRestoreNotifier::new();
            run_poke(
                &roster,
                Some("u-healthy"),
                None,
                0,
                &engine,
                Some(&status_snapshot(lines)),
                &notifier,
            )
            .await
            .unwrap();
            notifier.sent()
        }

        let dead_first = sent_for_order(true).await;
        let healthy_first = sent_for_order(false).await;
        assert_eq!(
            dead_first, healthy_first,
            "the verdict poke acts on must not depend on which bearer the roster/wire lists first",
        );
        assert!(
            dead_first.is_empty(),
            "and the order-independent answer must be the indeterminate one — poke cannot tell \
             the bearers apart from a label, so it claims nothing",
        );
    }

    #[tokio::test]
    async fn named_poke_of_a_healthy_duplicate_does_not_inherit_a_dead_siblings_quarantine() {
        // The issue's failure scenario, in the direction where the defect FABRICATES: the uuid
        // resolves `u-healthy` unambiguously, but the label lookup lands on the dead sibling
        // that sorts first — so poke sends a `Restored` for an account the daemon never
        // quarantined and reports the quarantine "cleared". Both are claims about a DIFFERENT
        // account's state.
        let roster = vec![acct("work", "u-dead"), acct("work", "u-healthy")];
        let engine = FakePokeEngine::new().with_result("u-healthy", restashed_refresh());
        let snap = status_snapshot(vec![
            status_line("work", Some(CredentialHealth::Dead), false),
            status_line("work", Some(CredentialHealth::Healthy), false),
        ]);
        let notifier = FakeRestoreNotifier::new();
        run_poke(
            &roster,
            Some("u-healthy"),
            None,
            0,
            &engine,
            Some(&snap),
            &notifier,
        )
        .await
        .unwrap();
        assert_eq!(
            engine.refreshed(),
            vec!["u-healthy"],
            "the uuid still resolves the right account to refresh — only the verdict was wrong",
        );
        assert!(
            notifier.sent().is_empty(),
            "no un-quarantine may be emitted off a sibling's verdict; got {:?}",
            notifier.sent(),
        );
    }

    #[tokio::test]
    async fn sweeping_poke_emits_no_restore_for_either_bearer_of_a_duplicated_label() {
        // The widened path (issue #1086 comment): a bare `sessiometer poke` consults
        // `resolve_target` for nothing, so no uuid is needed to reach the defect. It sweeps
        // both near-expiry bearers and hands each the SAME first-match verdict — the dead
        // one's — so both receive a `Restored` signal, including the healthy one.
        let now = 1_000_000;
        let soon = now + 60_000;
        let roster = vec![acct("work", "u-dead"), acct("work", "u-healthy")];
        let engine = FakePokeEngine::new()
            .with_expiry("u-dead", Some(soon))
            .with_expiry("u-healthy", Some(soon))
            .with_result("u-dead", restashed_refresh())
            .with_result("u-healthy", restashed_refresh());
        let snap = status_snapshot(vec![
            status_line("work", Some(CredentialHealth::Dead), false),
            status_line("work", Some(CredentialHealth::Healthy), false),
        ]);
        let notifier = FakeRestoreNotifier::new();
        run_poke(&roster, None, None, now, &engine, Some(&snap), &notifier)
            .await
            .unwrap();
        assert_eq!(
            engine.refreshed(),
            vec!["u-dead", "u-healthy"],
            "both parked bearers are still swept — the sweep itself is not the defect",
        );
        assert!(
            notifier.sent().is_empty(),
            "neither bearer may be un-quarantined off a verdict the label cannot attribute; \
             got {:?}",
            notifier.sent(),
        );
    }

    #[tokio::test]
    async fn an_ambiguous_roster_label_degrades_even_when_the_daemon_lists_one_line() {
        // The half a wire-side match count alone does NOT catch, and the reason this fix goes
        // past `use_account::cached_viability_for`'s shape. The daemon adopts a new roster only
        // when a reload is SIGNALLED (issue #139), so its snapshot legitimately lags the
        // on-disk roster: two `work` accounts on disk, one `work` line on the wire. Counting
        // wire matches finds exactly one and reads it as unambiguous — but that single line
        // belongs to whichever bearer the daemon knew, and attributing it to the poked account
        // is the same guess by a longer route.
        let roster = vec![acct("work", "u-known"), acct("work", "u-added-since")];
        let engine = FakePokeEngine::new().with_result("u-added-since", restashed_refresh());
        let snap = status_snapshot(vec![status_line(
            "work",
            Some(CredentialHealth::Dead),
            false,
        )]);
        let notifier = FakeRestoreNotifier::new();
        run_poke(
            &roster,
            Some("u-added-since"),
            None,
            0,
            &engine,
            Some(&snap),
            &notifier,
        )
        .await
        .unwrap();
        assert!(
            notifier.sent().is_empty(),
            "a label duplicated in the ROSTER is unusable as a key even when the daemon's \
             lagging snapshot lists it once; got {:?}",
            notifier.sent(),
        );
    }

    #[tokio::test]
    async fn an_ambiguous_wire_label_degrades_even_when_the_roster_is_unique() {
        // The MIRROR of the test above, and the reason it exists: the two conjuncts had asymmetric
        // coverage. Dropping the roster gate reddens that test through the whole `run_poke` path;
        // dropping the WIRE gate reddened only the predicate-direct unit test, so the conjunct
        // this issue was actually filed about had no end-to-end pin. This is it.
        //
        // The lag runs the other way here: one `work` on disk, two `work` lines on the wire — a
        // sibling REMOVED from the roster whose line the daemon has not dropped yet, again per
        // #139's signalled-reload. A roster-side count finds exactly one and reads it as a usable
        // key; the wire still cannot say which line is the poked account's.
        let roster = vec![acct("work", "u-kept")];
        let engine = FakePokeEngine::new().with_result("u-kept", restashed_refresh());
        let snap = status_snapshot(vec![
            status_line("work", Some(CredentialHealth::Dead), false),
            status_line("work", Some(CredentialHealth::Dead), false),
        ]);
        let notifier = FakeRestoreNotifier::new();
        run_poke(
            &roster,
            Some("u-kept"),
            None,
            0,
            &engine,
            Some(&snap),
            &notifier,
        )
        .await
        .unwrap();
        assert!(
            notifier.sent().is_empty(),
            "a label duplicated on the WIRE is unusable as a key even when the on-disk roster \
             carries it exactly once; got {:?}",
            notifier.sent(),
        );
    }

    #[tokio::test]
    async fn named_clears_the_quarantine_end_to_end() {
        // Full named flow: a parked account the daemon marks Dead, refreshed + re-stashed →
        // the engine ran the refresh AND the notifier received that account's uuid.
        let roster = vec![acct("work", "u-A"), acct("spare", "u-B")];
        let engine = FakePokeEngine::new().with_result(
            "u-B",
            FakeRefresh::Report(report(RefreshOutcome::Refreshed { rotated: true }, true)),
        );
        let snap = status_snapshot(vec![status_line(
            "spare",
            Some(CredentialHealth::Dead),
            false,
        )]);
        let notifier = FakeRestoreNotifier::new();
        run_poke(
            &roster,
            Some("spare"),
            Some("u-A"),
            0,
            &engine,
            Some(&snap),
            &notifier,
        )
        .await
        .unwrap();
        assert_eq!(engine.refreshed(), vec!["u-B"]);
        assert_eq!(
            notifier.sent(),
            vec!["u-B"],
            "the quarantined poked account was un-quarantined"
        );
    }

    #[tokio::test]
    async fn all_clears_the_quarantine_for_a_swept_dead_account() {
        let now = 1_000_000;
        let soon = now + 60_000;
        let roster = vec![acct("work", "u-A"), acct("spare", "u-B")];
        let engine = FakePokeEngine::new()
            .with_expiry("u-B", Some(soon))
            .with_result(
                "u-B",
                FakeRefresh::Report(report(RefreshOutcome::Refreshed { rotated: true }, true)),
            );
        let snap = status_snapshot(vec![status_line(
            "spare",
            Some(CredentialHealth::Dead),
            false,
        )]);
        let notifier = FakeRestoreNotifier::new();
        // u-A is active (excluded); u-B is parked, near-expiry, daemon-dead → refreshed + restored.
        run_poke(
            &roster,
            None,
            Some("u-A"),
            now,
            &engine,
            Some(&snap),
            &notifier,
        )
        .await
        .unwrap();
        assert_eq!(engine.refreshed(), vec!["u-B"]);
        assert_eq!(notifier.sent(), vec!["u-B"]);
    }

    // --- resolve_active_uuid (token-first, issue #207/#250) ------------------

    fn cred(blob: &[u8]) -> Credential {
        Credential::new(blob.to_vec())
    }

    /// A `StashedAccount` pairing `token` with `uuid`'s display half — mirrors the
    /// `active` module's own test fixture.
    fn stashed(token: &[u8], uuid: &str) -> StashedAccount {
        StashedAccount {
            credential: cred(token),
            oauth_account: OauthAccount::from_object_bytes(
                format!(r#"{{"accountUuid":"{uuid}","emailAddress":"{uuid}@example.com"}}"#)
                    .as_bytes(),
            )
            .unwrap(),
        }
    }

    /// A `FakeCredentialStore` whose canonical item holds `token`.
    async fn store_holding(token: &[u8]) -> FakeCredentialStore {
        let store = FakeCredentialStore::empty();
        store.write(&cred(token)).await.unwrap();
        store
    }

    /// A stash holding `work` (`u-A` → `A-token`) and `spare` (`u-B` → `B-token`).
    async fn stash_ab() -> FakeAccountStash {
        let stash = FakeAccountStash::empty();
        stash
            .write("Sessiometer/u-A", &stashed(b"A-token", "u-A"))
            .await
            .unwrap();
        stash
            .write("Sessiometer/u-B", &stashed(b"B-token", "u-B"))
            .await
            .unwrap();
        stash
    }

    fn roster_ab() -> Vec<Account> {
        vec![acct("work", "u-A"), acct("spare", "u-B")]
    }

    #[tokio::test]
    async fn active_uuid_resolves_token_first() {
        // AC-1: the canonical token byte-matches u-A's stash → active is u-A, even with a
        // display that agrees.
        let (_dir, json) = write_claude_json(r#"{"oauthAccount":{"accountUuid":"u-A"}}"#);
        let resolved = resolve_active_uuid(
            &roster_ab(),
            &store_holding(b"A-token").await,
            &stash_ab().await,
            &json,
        )
        .await
        .unwrap();
        assert_eq!(resolved.as_deref(), Some("u-A"));
    }

    #[tokio::test]
    async fn active_uuid_token_match_wins_over_a_cleared_display() {
        // AC-2 (the #250 core): Claude Code force-logged-out and cleared `~/.claude.json`'s
        // `oauthAccount`, but the canonical token is unchanged and still byte-matches u-A's
        // stash → poke MUST still resolve u-A as active (and thus exclude it from any
        // refresh), not fall through to "nothing active" and sweep the live token.
        let (_dir, json) = write_claude_json(r#"{"numStartups":1}"#); // cleared display
        let resolved = resolve_active_uuid(
            &roster_ab(),
            &store_holding(b"A-token").await,
            &stash_ab().await,
            &json,
        )
        .await
        .unwrap();
        assert_eq!(
            resolved.as_deref(),
            Some("u-A"),
            "token byte-match resolves the active account despite the cleared display"
        );
    }

    #[tokio::test]
    async fn active_uuid_refuses_on_a_locked_canonical() {
        // AC-3: a locked keychain is a SAFETY abort (locked ≠ gone) — refuse rather than
        // degrade to poke-all-on-cleared-display and risk poking a still-active account.
        let store = FakeCredentialStore::empty();
        store.set_locked(true);
        let (_dir, json) = write_claude_json(r#"{"numStartups":1}"#);
        let err = resolve_active_uuid(&roster_ab(), &store, &stash_ab().await, &json)
            .await
            .unwrap_err();
        assert!(matches!(err, Error::KeychainLocked { .. }));
    }

    #[tokio::test]
    async fn active_uuid_refuses_on_a_present_but_unreadable_canonical() {
        // AC-3: "could not read" ≠ "gone" — a present-but-unreadable canonical (an
        // ACL/auth-deny) likewise refuses, never degraded to "nothing is active".
        let store = FakeCredentialStore::empty();
        store.set_unreadable(true);
        let (_dir, json) = write_claude_json(r#"{"numStartups":1}"#);
        assert!(
            resolve_active_uuid(&roster_ab(), &store, &stash_ab().await, &json)
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn active_uuid_falls_back_to_display_when_the_canonical_is_confirmed_absent() {
        // AC-4: a CONFIRMED-absent canonical (`errSecItemNotFound`, the scrubbed item) is
        // NOT a refuse — there is no live token to protect. Degrade to the display-only
        // signal: a display still naming a roster account marks it (display-)active.
        let store = FakeCredentialStore::empty();
        store.set_not_found(true);
        let (_dir, json) = write_claude_json(r#"{"oauthAccount":{"accountUuid":"u-B"}}"#);
        let resolved = resolve_active_uuid(&roster_ab(), &store, &stash_ab().await, &json)
            .await
            .unwrap();
        assert_eq!(resolved.as_deref(), Some("u-B"));
    }

    #[tokio::test]
    async fn active_uuid_is_none_when_truly_logged_out() {
        // AC-4: canonical present but matching NO stash (an orphan/unmanaged token) AND a
        // display naming nothing in the roster → `None` ⇒ every roster account is parked
        // and safe to poke (today's defensible default, preserved).
        let (_dir, json) = write_claude_json(r#"{"numStartups":1}"#);
        let resolved = resolve_active_uuid(
            &roster_ab(),
            &store_holding(b"ORPHAN-token").await,
            &stash_ab().await,
            &json,
        )
        .await
        .unwrap();
        assert_eq!(resolved, None);
    }

    #[tokio::test]
    async fn active_uuid_is_none_when_canonical_and_display_are_both_absent() {
        // AC-4: fully logged out — the canonical item is gone and the display carries no
        // `oauthAccount` → `None` ⇒ poke all.
        let store = FakeCredentialStore::empty();
        store.set_not_found(true);
        let (_dir, json) = write_claude_json(r#"{"numStartups":1}"#);
        let resolved = resolve_active_uuid(&roster_ab(), &store, &stash_ab().await, &json)
            .await
            .unwrap();
        assert_eq!(resolved, None);
    }

    // --- run_poke: named mode ------------------------------------------------

    #[tokio::test]
    async fn named_refreshes_a_parked_account() {
        let roster = vec![acct("work", "u-A"), acct("spare", "u-B")];
        let engine = FakePokeEngine::new().with_result(
            "u-B",
            FakeRefresh::Report(report(RefreshOutcome::Refreshed { rotated: true }, true)),
        );
        // Active is u-A; we name the parked u-B by its label.
        run_poke(
            &roster,
            Some("spare"),
            Some("u-A"),
            0,
            &engine,
            None,
            &FakeRestoreNotifier::new(),
        )
        .await
        .unwrap();
        assert_eq!(engine.refreshed(), vec!["u-B"]);
    }

    #[tokio::test]
    async fn named_resolves_by_account_uuid_too() {
        let roster = vec![acct("work", "u-A"), acct("spare", "u-B")];
        let engine = FakePokeEngine::new();
        run_poke(
            &roster,
            Some("u-B"),
            Some("u-A"),
            0,
            &engine,
            None,
            &FakeRestoreNotifier::new(),
        )
        .await
        .unwrap();
        assert_eq!(engine.refreshed(), vec!["u-B"]);
    }

    #[tokio::test]
    async fn named_refuses_the_active_account_without_refreshing() {
        let roster = vec![acct("work", "u-A"), acct("spare", "u-B")];
        let engine = FakePokeEngine::new();
        let err = run_poke(
            &roster,
            Some("work"),
            Some("u-A"),
            0,
            &engine,
            None,
            &FakeRestoreNotifier::new(),
        )
        .await
        .unwrap_err();
        assert!(matches!(err, Error::PokeTargetActive { ref label } if label == "work"));
        assert!(engine.refreshed().is_empty());
    }

    #[tokio::test]
    async fn named_reports_an_unresolvable_target() {
        let roster = vec![acct("work", "u-A")];
        let engine = FakePokeEngine::new();
        let err = run_poke(
            &roster,
            Some("ghost"),
            None,
            0,
            &engine,
            None,
            &FakeRestoreNotifier::new(),
        )
        .await
        .unwrap_err();
        assert!(matches!(err, Error::UseTargetNotFound { ref query } if query == "ghost"));
        assert!(engine.refreshed().is_empty());
    }

    // --- issue #1087: a duplicated label refuses, and the refusal is not a report line ------
    //
    // `poke` shares `use_account::resolve_target` with the five other label-resolving sites, so
    // it refuses a duplicated label rather than taking the earliest bearer (#17, OQ-1). The half
    // that had no test is what happens to that `Err` afterwards: `poke` is the one CLI site that
    // COMPOSES per-account report lines, and its all-accounts sweep deliberately turns a
    // per-account `Err` into an outcome string (`format!("error ({err})")`, `poke_all`) and keeps
    // going — a completed, exit-0 report. A named refusal routed into that composition would be
    // indistinguishable to a script and nearly so to a reader: the same words, on the wrong
    // stream, under a success exit.
    //
    // Two tests, because the claim has two halves that fail independently. The first pins the
    // CONTRACT (which error, which exit code, and that the engine never ran); the second pins the
    // OBSERVABLE — the two byte streams an operator actually gets — which no in-process assertion
    // can see, since a swallow that printed a report line and returned `Ok(())` satisfies "the
    // engine never ran" perfectly.

    #[tokio::test]
    async fn named_refuses_a_duplicated_label_and_the_uuid_is_the_way_out() {
        let roster = vec![acct("dup", "u-A"), acct("dup", "u-B"), acct("solo", "u-C")];
        let engine = FakePokeEngine::new();
        let err = run_poke(
            &roster,
            Some("dup"),
            None,
            0,
            &engine,
            None,
            &FakeRestoreNotifier::new(),
        )
        .await
        .expect_err("a duplicated label must refuse, not report a per-account outcome");
        assert!(
            matches!(err, Error::UseTargetAmbiguous { count: 2, ref query } if query == "dup"),
            "got {err:?}",
        );
        // The literal code, not merely "some error": exit 6 is the operator-visible contract a
        // script keys on, and a report line would have exited 0.
        assert_eq!(err.exit_code(), 6, "an ambiguous target exits 6");
        assert!(
            engine.refreshed().is_empty(),
            "an ambiguous target refreshes nothing",
        );

        // The refusal is actionable rather than a dead end: there is no `--account-uuid`
        // disambiguator flag, so the account-uuid the message points at IS the remedy, and it has
        // to work through `poke` itself — the sibling of `cli`'s
        // `the_label_resolving_verbs_accept_an_account_uuid_so_a_refusal_is_actionable`.
        run_poke(
            &roster,
            Some("u-B"),
            None,
            0,
            &engine,
            None,
            &FakeRestoreNotifier::new(),
        )
        .await
        .expect("the account-uuid disambiguates a duplicated label");
        assert_eq!(
            engine.refreshed(),
            vec!["u-B"],
            "and it pokes the bearer the operator named, only",
        );
    }

    /// Environment variable set on the re-exec'd child. Read by BOTH sides: the parent asserts it
    /// is unset (a parent running inside its own child would re-exec forever), the payload asserts
    /// it is set (so an accidental direct `--ignored` run is a loud failure, not a process exit
    /// under someone else's suite).
    const AMBIGUITY_CHILD_VAR: &str = "SESSIOMETER_POKE_AMBIGUITY_CHILD";

    /// The `#[ignore]`d child payload's function name, used to build the `--exact` filter. A drift
    /// between this and the payload's real name selects NOTHING, and libtest exits 0 on an empty
    /// selection — which is why the parent asserts [`AMBIGUITY_CHILD_RAN`] rather than the child's
    /// status alone.
    const AMBIGUITY_CHILD_FN: &str = "poke_ambiguity_child_payload";

    /// Written to fd 2 by the payload before it exits, proving to the parent that the payload
    /// really ran.
    const AMBIGUITY_CHILD_RAN: &str = "poke-ambiguity-child-ran";

    /// The exact sentence `main` prints for this refusal, pinned as a literal rather than
    /// re-derived from `Error::UseTargetAmbiguous` — re-deriving it would make the assertion agree
    /// with whatever the template says, including a rewording that stopped naming the remedy.
    const AMBIGUITY_REFUSAL: &str =
        "`dup` is ambiguous: 2 accounts match — disambiguate with the account-uuid";

    #[test]
    fn a_duplicated_label_refusal_reaches_the_operator_on_stderr_not_as_a_report_line() {
        // AC-1's own words are that the refusal is visible in `poke`'s OUTPUT, so this asserts on
        // the output: both of the child's streams, and its exit status.
        //
        // It needs a child process to do it. `println!` and `eprintln!` route through the
        // thread-local capture libtest installs, so in-process both streams arrive empty — the
        // re-exec with `--nocapture` (the pattern `crate::paths`'s launchd cases use) is what
        // makes the streams observable at all.
        //
        // Which assertions that endangers is worth being exact about, because the imprecise
        // version of this sentence ("every assertion would pass over nothing") invites deleting
        // the CHILD_RAN sentinel as belt-and-braces. The POSITIVE assertions — stderr carries the
        // refusal, the status is 6 — fail loudly against empty streams and need no help. It is the
        // two NEGATIVE ones, requiring that stdout carries neither a `dup:` report line nor the
        // refusal's words, that an empty capture satisfies perfectly. The sentinel is what
        // separates "the child ran and printed nothing there" from "the child never ran": a
        // drifted filter selects zero tests and libtest exits 0, which without it reads as a pass.
        assert!(
            std::env::var_os(AMBIGUITY_CHILD_VAR).is_none(),
            "{AMBIGUITY_CHILD_VAR} is set — this parent is running inside its own child",
        );
        let module = module_path!();
        let module = module.split_once("::").map_or(module, |(_, rest)| rest);
        let output = std::process::Command::new(
            std::env::current_exe().expect("the test binary must know its own path"),
        )
        .arg(format!("{module}::{AMBIGUITY_CHILD_FN}"))
        .arg("--exact")
        .arg("--ignored")
        .arg("--nocapture")
        .arg("--test-threads=1")
        .env(AMBIGUITY_CHILD_VAR, "1")
        .output()
        .expect("the child test binary must run");
        let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
        let stderr = String::from_utf8_lossy(&output.stderr).into_owned();

        assert!(
            stderr.contains(AMBIGUITY_CHILD_RAN),
            "the payload did not run — a drifted name selects nothing and libtest exits 0 on an \
             empty selection.\nstdout: {stdout}\nstderr: {stderr}",
        );
        // 1. The refusal reached the operator, on the ERROR stream, in `main`'s words.
        assert!(
            stderr.contains(&format!("sessiometer: {AMBIGUITY_REFUSAL}")),
            "the refusal must be visible on stderr.\nstdout: {stdout}\nstderr: {stderr}",
        );
        // 2. …carrying exit 6. A per-account report is an exit-0 event, so this alone separates
        //    the two channels for every caller that only reads a status.
        assert_eq!(
            output.status.code(),
            Some(6),
            "the refusal must exit 6.\nstdout: {stdout}\nstderr: {stderr}",
        );
        // 3. …and NOTHING of it reached the report stream. Both shapes a swallow can take are
        //    excluded: a `poke_line` for the query (`<label>: <outcome>`, the form `poke_all`
        //    composes for a per-account error) and the refusal's own words anywhere on fd 1.
        assert!(
            !stdout.lines().any(|line| line.starts_with("dup:")),
            "the refusal must not be reported as a per-account line.\nstdout: {stdout}",
        );
        assert!(
            !stdout.contains(AMBIGUITY_REFUSAL),
            "the refusal must not reach stdout at all.\nstdout: {stdout}",
        );
    }

    /// The child payload of
    /// [`a_duplicated_label_refusal_reaches_the_operator_on_stderr_not_as_a_report_line`]: run
    /// `poke dup` on a duplicate-label roster over in-memory fakes, then reproduce `main`'s tail
    /// verbatim — the refusal on fd 2, the typed exit code — so the parent observes exactly the two
    /// streams an operator does, and nothing is asserted here that the parent could not see.
    ///
    /// `#[ignore]`d because it is not a test in its own right: it asserts nothing about `poke` and
    /// it EXITS the process, so it must never join an ordinary run. The parent selects it by exact
    /// name with `--ignored`.
    #[tokio::test]
    #[ignore = "child payload; re-exec'd by its parent, never run directly"]
    async fn poke_ambiguity_child_payload() {
        use std::io::Write as _;

        assert!(
            std::env::var_os(AMBIGUITY_CHILD_VAR).is_some(),
            "{AMBIGUITY_CHILD_VAR} is unset — this payload exits the process and is only \
             meaningful under its parent",
        );
        let roster = vec![acct("dup", "u-A"), acct("dup", "u-B"), acct("solo", "u-C")];
        let engine = FakePokeEngine::new();
        let result = run_poke(
            &roster,
            Some("dup"),
            None,
            0,
            &engine,
            None,
            &FakeRestoreNotifier::new(),
        )
        .await;
        // Straight to the stderr HANDLE, and BEFORE the exit below: this is the parent's proof that
        // the payload ran at all.
        let _ = writeln!(std::io::stderr(), "{AMBIGUITY_CHILD_RAN}");
        // `main`'s tail, verbatim. An `Ok` exits 0 having said whatever it already printed on
        // fd 1 — which is precisely what a swallowed refusal looks like from outside.
        match result {
            Ok(()) => std::process::exit(0),
            Err(err) => {
                eprintln!("sessiometer: {err}");
                std::process::exit(i32::from(err.exit_code()));
            }
        }
    }

    #[tokio::test]
    async fn named_propagates_a_hard_refresh_error() {
        let roster = vec![acct("work", "u-A"), acct("spare", "u-B")];
        let engine = FakePokeEngine::new().with_result("u-B", FakeRefresh::HardError);
        let err = run_poke(
            &roster,
            Some("spare"),
            Some("u-A"),
            0,
            &engine,
            None,
            &FakeRestoreNotifier::new(),
        )
        .await
        .unwrap_err();
        // The typed error reaches the operator (non-fatal for the credential, but the
        // one-shot exits with its code).
        assert!(matches!(err, Error::SwapLockBusy));
        assert_eq!(engine.refreshed(), vec!["u-B"]);
    }

    // --- run_poke: all-accounts mode ----------------------------------------

    #[tokio::test]
    async fn all_refreshes_only_parked_near_expiry_accounts() {
        let now = 1_000_000;
        let soon = now + 60_000; // within the 1h horizon
        let later = now + 24 * 3_600_000; // a day out — beyond the horizon
        let roster = vec![
            acct("active", "u-A"),
            acct("near", "u-B"),
            acct("fresh", "u-C"),
        ];
        let engine = FakePokeEngine::new()
            // The active account is near expiry too, but must be skipped.
            .with_expiry("u-A", Some(soon))
            .with_expiry("u-B", Some(soon))
            .with_expiry("u-C", Some(later));
        run_poke(
            &roster,
            None,
            Some("u-A"),
            now,
            &engine,
            None,
            &FakeRestoreNotifier::new(),
        )
        .await
        .unwrap();
        // Only the parked, near-expiry account is refreshed.
        assert_eq!(engine.refreshed(), vec!["u-B"]);
    }

    #[tokio::test]
    async fn all_does_nothing_when_no_account_is_near_expiry() {
        let now = 1_000_000;
        let later = now + 24 * 3_600_000;
        let roster = vec![acct("work", "u-A"), acct("spare", "u-B")];
        let engine = FakePokeEngine::new()
            .with_expiry("u-A", Some(later))
            .with_expiry("u-B", Some(later));
        run_poke(
            &roster,
            None,
            None,
            now,
            &engine,
            None,
            &FakeRestoreNotifier::new(),
        )
        .await
        .unwrap();
        assert!(engine.refreshed().is_empty());
    }

    #[tokio::test]
    async fn all_continues_past_a_per_account_error() {
        let now = 1_000_000;
        let soon = now + 60_000;
        let roster = vec![acct("a", "u-A"), acct("b", "u-B")];
        let engine = FakePokeEngine::new()
            .with_expiry("u-A", Some(soon))
            .with_expiry("u-B", Some(soon))
            // The first account errors hard — the sweep must still reach the second.
            .with_result("u-A", FakeRefresh::HardError)
            .with_result(
                "u-B",
                FakeRefresh::Report(report(RefreshOutcome::Refreshed { rotated: true }, true)),
            );
        run_poke(
            &roster,
            None,
            None,
            now,
            &engine,
            None,
            &FakeRestoreNotifier::new(),
        )
        .await
        .unwrap();
        assert_eq!(engine.refreshed(), vec!["u-A", "u-B"]);
    }
}
