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
//! A successful refresh does NOT imply the account is healthy: `poke` cannot un-quarantine
//! a daemon-dead account (#42), so when the daemon's current verdict (read best-effort over
//! the same control socket `status` uses) marks the poked account dead, the bare `refreshed`
//! line is replaced with the honest truth — the token was refreshed, but the daemon still
//! marks it dead — pointing to `claude /login` (issue #163). No daemon reachable, or a
//! non-dead verdict, keeps the plain wording.

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
        claude_binary: paths::claude_binary()?,
    };
    // The daemon's current per-account verdict (#163), read best-effort BEFORE the
    // cycle so a refreshed-but-still-quarantined account is reported honestly rather
    // than as a misleading bare `refreshed`. `None` when no daemon is reachable — poke
    // then keeps its plain wording (it never depends on a daemon being up).
    let daemon_status = daemon_status_best_effort().await;
    run_poke(
        &config.roster,
        target.as_deref(),
        active_uuid.as_deref(),
        now_ms(),
        &engine,
        daemon_status.as_ref(),
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
async fn run_poke<E: PokeEngine>(
    roster: &[Account],
    target: Option<&str>,
    active_uuid: Option<&str>,
    now_ms: i64,
    engine: &E,
    daemon_status: Option<&StatusResponse>,
) -> Result<()> {
    match target {
        Some(query) => poke_named(roster, query, active_uuid, engine, daemon_status).await,
        None => poke_all(roster, active_uuid, now_ms, engine, daemon_status).await,
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
async fn poke_named<E: PokeEngine>(
    roster: &[Account],
    query: &str,
    active_uuid: Option<&str>,
    engine: &E,
    daemon_status: Option<&StatusResponse>,
) -> Result<()> {
    let account = &roster[resolve_target(roster, query)?];
    if is_active(account, active_uuid) {
        return Err(Error::PokeTargetActive {
            label: account.label.clone(),
        });
    }
    let report = engine.refresh(account).await?;
    let dead = daemon_marks_dead(daemon_status, &account.label);
    println!(
        "{}",
        poke_line(&account.label, &poke_outcome(&report, dead))
    );
    Ok(())
}

/// `poke`: run one cycle for every PARKED (non-active) account whose stored token is
/// near expiry. The active account is excluded here; "mid-swap" exclusion is enforced
/// by the swap lock the engine acquires, not by this selection (issue #105). Per-account
/// errors are non-fatal — reported (the typed message is secret-free) and the sweep
/// continues, so one account's transient failure never aborts the rest.
async fn poke_all<E: PokeEngine>(
    roster: &[Account],
    active_uuid: Option<&str>,
    now_ms: i64,
    engine: &E,
    daemon_status: Option<&StatusResponse>,
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
            // Each line carries the daemon's own verdict for THIS account (#163), so a
            // refreshed-but-still-quarantined one in the sweep is as honest as a named poke.
            Ok(report) => poke_outcome(&report, daemon_marks_dead(daemon_status, &account.label)),
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
        RefreshOutcome::Refreshed if report.re_stashed => "refreshed",
        // A refresh the CAS step discarded: a concurrent swap / login changed the stash
        // since the cycle began, so that credential is authoritative (issue #102 step 7)
        // and the fresh token was not stored. Surfaced honestly.
        RefreshOutcome::Refreshed => {
            "refreshed but not re-stashed (a concurrent change took precedence)"
        }
        RefreshOutcome::NoChange => "no change",
        RefreshOutcome::Dead => "dead — needs re-login",
        RefreshOutcome::Error => "error",
    }
}

/// The honest per-account outcome text for a completed cycle, given the daemon's current
/// verdict for the account (`dead_per_daemon`, resolved by [`daemon_marks_dead`]).
///
/// The ONLY classification a daemon-dead verdict rewrites is the bare `"refreshed"` (a
/// `Refreshed` cycle that re-stashed): that line reads as "fixed", but `poke` — a separate
/// CLI process with no daemon IPC — cannot un-quarantine a daemon-dead account (#163), and
/// the fresh token it just stashed is not even polled while the account stays quarantined
/// (#42). So it states BOTH truths: the token WAS refreshed, AND the daemon still marks the
/// account dead, with the recovery cue ([`DAEMON_STILL_DEAD`]).
///
/// Every other classification is already honest and is returned unchanged: `dead` /
/// `no change` / `refreshed but not re-stashed` / `error` each carry their own truth, and
/// the bare `"refreshed"` itself stands whenever the daemon does NOT mark the account dead
/// — healthy, `Unknown` ⚪, or no daemon reachable — a true statement of the cycle that just
/// ran, never a fabricated liveness claim.
fn poke_outcome(report: &RefreshReport, dead_per_daemon: bool) -> String {
    if dead_per_daemon && matches!(report.outcome, RefreshOutcome::Refreshed) && report.re_stashed {
        return DAEMON_STILL_DEAD.to_owned();
    }
    outcome_label(report).to_owned()
}

/// The #163 honest replacement for a bare `"refreshed"` when the daemon still marks the
/// poked account dead: the token WAS refreshed, but `poke` cannot clear the #42 quarantine
/// — only an operator `claude /login`, or the daemon's own next refresh sweep, can. Kept a
/// single message for any dead verdict (a mid-recovery account #109 is covered by the
/// passive-sweep clause). Non-secret (issue #15): a classification + a recovery cue, no token.
const DAEMON_STILL_DEAD: &str = "token refreshed, but the daemon still marks this account \
     dead — run `claude /login` (or it will re-evaluate on its next refresh sweep)";

/// `<label>: <outcome>` — one per-account report line (the label is the non-secret
/// handle, issue #15).
fn poke_line(label: &str, outcome: &str) -> String {
    format!("{label}: {outcome}")
}

/// Whether the daemon's snapshot currently marks `label`'s account DEAD — the exact
/// verdict `status` projects to 🔴 (mirrors `cli::health_cell`): a current daemon's 4-state
/// rollup (`health == Some(Dead)`, issue #119/#137), or — a pre-#119 daemon that sent no
/// rollup (`health == None`) — its legacy `quarantined` flag (#42).
///
/// A `None` snapshot (no daemon reachable) or a `label` absent from the snapshot is NOT
/// dead: an indeterminate verdict `poke` reports with its plain wording (#163 AC-3), never a
/// fabricated daemon-state claim. Matching is by label — the only account handle the wire
/// [`AccountStatusLine`] carries (issue #15: never the uuid or email) — the same key `status`
/// renders by; a renamed/stale label simply misses and degrades to the plain wording.
fn daemon_marks_dead(daemon_status: Option<&StatusResponse>, label: &str) -> bool {
    let Some(status) = daemon_status else {
        return false;
    };
    status
        .accounts
        .iter()
        .find(|line| line.label == label)
        .is_some_and(line_is_dead)
}

/// Resolve ONE status line's dead-ness the same way `status` does (`cli::health_cell`): the
/// daemon's 4-state rollup when present (`health == Some(Dead)`), else — a pre-#119 daemon
/// that sent no rollup (`health == None`) — the legacy `quarantined` flag, so an old daemon
/// reads correctly rather than as a defaulted-healthy line over a dead account. `Healthy` /
/// `Unknown` / `Stale` / `AtRisk` are all NOT dead (poke keeps the plain wording for them).
fn line_is_dead(line: &AccountStatusLine) -> bool {
    match line.health {
        Some(health) => health == CredentialHealth::Dead,
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
            refresh_token_rotated: false,
            re_stashed,
        }
    }

    /// One daemon `status` line for `label`, carrying only the fields poke's #163 verdict
    /// reads: the 4-state rollup (`health`) and the pre-#119 legacy `quarantined` flag.
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
        }
    }

    /// A daemon `status` snapshot from a set of account lines — what a running daemon
    /// returns to poke's best-effort control-socket read.
    fn status_snapshot(lines: Vec<AccountStatusLine>) -> StatusResponse {
        StatusResponse {
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
            outcome_label(&report(RefreshOutcome::Refreshed, true)),
            "refreshed"
        );
        assert_eq!(
            outcome_label(&report(RefreshOutcome::Refreshed, false)),
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
            outcome_label(&report(RefreshOutcome::Error, false)),
            "error"
        );
    }

    #[test]
    fn poke_line_is_label_then_outcome() {
        assert_eq!(poke_line("work", "refreshed"), "work: refreshed");
    }

    // --- #163 daemon-verdict resolution (pure) ------------------------------

    #[test]
    fn line_is_dead_reads_the_rollup_then_the_legacy_flag() {
        // A current daemon: the 4-state rollup is authoritative — only `Dead` is dead.
        assert!(line_is_dead(&status_line(
            "a",
            Some(CredentialHealth::Dead),
            false
        )));
        assert!(!line_is_dead(&status_line(
            "a",
            Some(CredentialHealth::Healthy),
            false
        )));
        assert!(!line_is_dead(&status_line(
            "a",
            Some(CredentialHealth::Unknown),
            false
        )));
        assert!(!line_is_dead(&status_line(
            "a",
            Some(CredentialHealth::Stale),
            false
        )));
        assert!(!line_is_dead(&status_line(
            "a",
            Some(CredentialHealth::AtRisk),
            false
        )));
        // The rollup WINS when present: `Dead` is dead even with the raw `quarantined`
        // flag unset (a refresh-cleared-in-place credential, #119), and `Healthy` is NOT
        // dead even if a stale `quarantined` flag lingers — poke reads exactly what
        // `status` renders.
        assert!(line_is_dead(&status_line(
            "a",
            Some(CredentialHealth::Dead),
            false
        )));
        assert!(!line_is_dead(&status_line(
            "a",
            Some(CredentialHealth::Healthy),
            true
        )));
        // A pre-#119 daemon (no rollup): fall back to the legacy `quarantined` flag.
        assert!(line_is_dead(&status_line("a", None, true)));
        assert!(!line_is_dead(&status_line("a", None, false)));
    }

    #[test]
    fn daemon_marks_dead_reads_the_named_line_or_degrades_to_not_dead() {
        let snap = status_snapshot(vec![
            status_line("work", Some(CredentialHealth::Dead), false),
            status_line("spare", Some(CredentialHealth::Healthy), false),
        ]);
        // Each account's OWN verdict is read, by label.
        assert!(daemon_marks_dead(Some(&snap), "work"));
        assert!(!daemon_marks_dead(Some(&snap), "spare"));
        // No daemon reachable (None) → indeterminate → not dead (plain wording, AC-3).
        assert!(!daemon_marks_dead(None, "work"));
        // A label the daemon does not list → indeterminate → not dead.
        assert!(!daemon_marks_dead(Some(&snap), "ghost"));
    }

    #[test]
    fn poke_outcome_augments_only_the_bare_refreshed_and_only_when_dead() {
        let refreshed = report(RefreshOutcome::Refreshed, true);
        // The one misleading line: refreshed + re-stashed + daemon-dead → honest truth.
        assert_eq!(poke_outcome(&refreshed, true), DAEMON_STILL_DEAD);
        // Same cycle, NOT daemon-dead → the plain wording is unchanged.
        assert_eq!(poke_outcome(&refreshed, false), "refreshed");
        // A refresh that did NOT re-stash is already honest (its own distinct wording) —
        // never rewritten, even under a dead verdict.
        let not_restashed = report(RefreshOutcome::Refreshed, false);
        assert_eq!(
            poke_outcome(&not_restashed, true),
            "refreshed but not re-stashed (a concurrent change took precedence)"
        );
        // Every other classification already carries its own truth → unchanged under a
        // dead verdict (no double-reporting "dead", no masking "error"/"no change").
        assert_eq!(
            poke_outcome(&report(RefreshOutcome::Dead, false), true),
            "dead — needs re-login"
        );
        assert_eq!(
            poke_outcome(&report(RefreshOutcome::NoChange, false), true),
            "no change"
        );
        assert_eq!(
            poke_outcome(&report(RefreshOutcome::Error, false), true),
            "error"
        );
    }

    // --- #163 the three reported states, keyed off the daemon verdict -------

    #[test]
    fn dead_account_poke_reports_the_quarantine_truth_not_bare_refreshed() {
        // AC-1: a parked account the daemon marks Dead, refreshed successfully → the
        // output states the token refreshed AND that the daemon still marks it dead,
        // pointing to `claude /login`.
        let snap = status_snapshot(vec![status_line(
            "work",
            Some(CredentialHealth::Dead),
            false,
        )]);
        let outcome = poke_outcome(
            &report(RefreshOutcome::Refreshed, true),
            daemon_marks_dead(Some(&snap), "work"),
        );
        assert_ne!(
            outcome, "refreshed",
            "must NOT be the misleading bare wording"
        );
        assert!(outcome.contains("still marks this account dead"));
        assert!(outcome.contains("claude /login"));
    }

    #[test]
    fn healthy_account_poke_keeps_the_plain_refreshed_wording() {
        // AC-2: a healthy (non-quarantined) parked account → existing wording unchanged.
        let snap = status_snapshot(vec![status_line(
            "work",
            Some(CredentialHealth::Healthy),
            false,
        )]);
        assert_eq!(
            poke_outcome(
                &report(RefreshOutcome::Refreshed, true),
                daemon_marks_dead(Some(&snap), "work")
            ),
            "refreshed"
        );
    }

    #[test]
    fn unknown_or_absent_daemon_is_honest_never_a_fabricated_verdict() {
        let refreshed = report(RefreshOutcome::Refreshed, true);
        // AC-3: no daemon reachable (None) → degrade to plain wording; no crash, no
        // daemon-state claim.
        assert_eq!(
            poke_outcome(&refreshed, daemon_marks_dead(None, "work")),
            "refreshed"
        );
        // A running daemon with an Unknown ⚪ verdict (#137) → honest plain wording: the
        // cycle truth, never a fabricated "live" nor a false "dead".
        let unknown = status_snapshot(vec![status_line(
            "work",
            Some(CredentialHealth::Unknown),
            false,
        )]);
        assert_eq!(
            poke_outcome(&refreshed, daemon_marks_dead(Some(&unknown), "work")),
            "refreshed"
        );
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
            FakeRefresh::Report(report(RefreshOutcome::Refreshed, true)),
        );
        // Active is u-A; we name the parked u-B by its label.
        run_poke(&roster, Some("spare"), Some("u-A"), 0, &engine, None)
            .await
            .unwrap();
        assert_eq!(engine.refreshed(), vec!["u-B"]);
    }

    #[tokio::test]
    async fn named_resolves_by_account_uuid_too() {
        let roster = vec![acct("work", "u-A"), acct("spare", "u-B")];
        let engine = FakePokeEngine::new();
        run_poke(&roster, Some("u-B"), Some("u-A"), 0, &engine, None)
            .await
            .unwrap();
        assert_eq!(engine.refreshed(), vec!["u-B"]);
    }

    #[tokio::test]
    async fn named_refuses_the_active_account_without_refreshing() {
        let roster = vec![acct("work", "u-A"), acct("spare", "u-B")];
        let engine = FakePokeEngine::new();
        let err = run_poke(&roster, Some("work"), Some("u-A"), 0, &engine, None)
            .await
            .unwrap_err();
        assert!(matches!(err, Error::PokeTargetActive { ref label } if label == "work"));
        assert!(engine.refreshed().is_empty());
    }

    #[tokio::test]
    async fn named_reports_an_unresolvable_target() {
        let roster = vec![acct("work", "u-A")];
        let engine = FakePokeEngine::new();
        let err = run_poke(&roster, Some("ghost"), None, 0, &engine, None)
            .await
            .unwrap_err();
        assert!(matches!(err, Error::UseTargetNotFound { ref query } if query == "ghost"));
        assert!(engine.refreshed().is_empty());
    }

    #[tokio::test]
    async fn named_propagates_a_hard_refresh_error() {
        let roster = vec![acct("work", "u-A"), acct("spare", "u-B")];
        let engine = FakePokeEngine::new().with_result("u-B", FakeRefresh::HardError);
        let err = run_poke(&roster, Some("spare"), Some("u-A"), 0, &engine, None)
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
        run_poke(&roster, None, Some("u-A"), now, &engine, None)
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
        run_poke(&roster, None, None, now, &engine, None)
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
                FakeRefresh::Report(report(RefreshOutcome::Refreshed, true)),
            );
        run_poke(&roster, None, None, now, &engine, None)
            .await
            .unwrap();
        assert_eq!(engine.refreshed(), vec!["u-A", "u-B"]);
    }
}
