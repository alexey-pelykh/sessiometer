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
//!   - **Refresh PARKED accounts only.** The ACTIVE account (resolved from
//!     `~/.claude.json`) is excluded — named-mode REFUSES it
//!     ([`Error::PokeTargetActive`]), all-mode SKIPS it. "Mid-swap" exclusion needs no
//!     logic here: the engine holds the swap lock around its stash read + re-stash, so a
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

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::claude_state;
use crate::config::{Account, Config};
use crate::error::{Error, Result};
use crate::paths;
use crate::refresh::{self, RefreshOutcome, RefreshReport};
use crate::stash::RealAccountStash;
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

    let active_uuid = resolve_active_uuid(&paths::claude_json()?)?;
    let engine = RealPokeEngine {
        stash: RealAccountStash::new(),
        claude_binary: paths::claude_binary()?,
    };
    run_poke(
        &config.roster,
        target.as_deref(),
        active_uuid.as_deref(),
        now_ms(),
        &engine,
    )
    .await
}

/// The active account's uuid (the one logged in to Claude Code), or `None` when no
/// account is active.
///
/// A MISSING `~/.claude.json`, or one carrying no `oauthAccount`, means no account is
/// logged in ⇒ `None` ⇒ every roster account is parked and safe to poke. A
/// PRESENT-but-unreadable state file (a parse error, a malformed `oauthAccount`)
/// PROPAGATES as an error: `poke` must EXCLUDE the active account, so when one exists
/// but cannot be identified it refuses rather than risk poking it.
fn resolve_active_uuid(claude_json: &Path) -> Result<Option<String>> {
    match claude_state::read_oauth_account_from(claude_json) {
        Ok(oauth) => Ok(Some(oauth.account_uuid().to_owned())),
        Err(Error::ClaudeStateNotFound { .. }) | Err(Error::OauthAccountMissing) => Ok(None),
        Err(other) => Err(other),
    }
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
) -> Result<()> {
    match target {
        Some(query) => poke_named(roster, query, active_uuid, engine).await,
        None => poke_all(roster, active_uuid, now_ms, engine).await,
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
) -> Result<()> {
    let account = &roster[resolve_target(roster, query)?];
    if is_active(account, active_uuid) {
        return Err(Error::PokeTargetActive {
            label: account.label.clone(),
        });
    }
    let report = engine.refresh(account).await?;
    println!("{}", poke_line(&account.label, outcome_label(&report)));
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
            Ok(report) => outcome_label(&report).to_owned(),
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

/// `<label>: <outcome>` — one per-account report line (the label is the non-secret
/// handle, issue #15).
fn poke_line(label: &str, outcome: &str) -> String {
    format!("{label}: {outcome}")
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

    // --- resolve_active_uuid -------------------------------------------------

    #[test]
    fn active_uuid_is_some_when_logged_in() {
        let (_dir, path) = write_claude_json(r#"{"oauthAccount":{"accountUuid":"u-A"}}"#);
        assert_eq!(resolve_active_uuid(&path).unwrap().as_deref(), Some("u-A"));
    }

    #[test]
    fn active_uuid_is_none_when_no_state_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("absent.json");
        assert_eq!(resolve_active_uuid(&path).unwrap(), None);
    }

    #[test]
    fn active_uuid_is_none_when_not_logged_in() {
        // Valid JSON, but no `oauthAccount` — no account is active.
        let (_dir, path) = write_claude_json(r#"{"numStartups":1}"#);
        assert_eq!(resolve_active_uuid(&path).unwrap(), None);
    }

    #[test]
    fn active_uuid_refuses_on_an_unreadable_state_file() {
        // Present but malformed — an active account may exist yet cannot be named, so
        // poke refuses rather than risk poking it.
        let (_dir, path) = write_claude_json("{ not json");
        assert!(resolve_active_uuid(&path).is_err());
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
        run_poke(&roster, Some("spare"), Some("u-A"), 0, &engine)
            .await
            .unwrap();
        assert_eq!(engine.refreshed(), vec!["u-B"]);
    }

    #[tokio::test]
    async fn named_resolves_by_account_uuid_too() {
        let roster = vec![acct("work", "u-A"), acct("spare", "u-B")];
        let engine = FakePokeEngine::new();
        run_poke(&roster, Some("u-B"), Some("u-A"), 0, &engine)
            .await
            .unwrap();
        assert_eq!(engine.refreshed(), vec!["u-B"]);
    }

    #[tokio::test]
    async fn named_refuses_the_active_account_without_refreshing() {
        let roster = vec![acct("work", "u-A"), acct("spare", "u-B")];
        let engine = FakePokeEngine::new();
        let err = run_poke(&roster, Some("work"), Some("u-A"), 0, &engine)
            .await
            .unwrap_err();
        assert!(matches!(err, Error::PokeTargetActive { ref label } if label == "work"));
        assert!(engine.refreshed().is_empty());
    }

    #[tokio::test]
    async fn named_reports_an_unresolvable_target() {
        let roster = vec![acct("work", "u-A")];
        let engine = FakePokeEngine::new();
        let err = run_poke(&roster, Some("ghost"), None, 0, &engine)
            .await
            .unwrap_err();
        assert!(matches!(err, Error::UseTargetNotFound { ref query } if query == "ghost"));
        assert!(engine.refreshed().is_empty());
    }

    #[tokio::test]
    async fn named_propagates_a_hard_refresh_error() {
        let roster = vec![acct("work", "u-A"), acct("spare", "u-B")];
        let engine = FakePokeEngine::new().with_result("u-B", FakeRefresh::HardError);
        let err = run_poke(&roster, Some("spare"), Some("u-A"), 0, &engine)
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
        run_poke(&roster, None, Some("u-A"), now, &engine)
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
        run_poke(&roster, None, None, now, &engine).await.unwrap();
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
        run_poke(&roster, None, None, now, &engine).await.unwrap();
        assert_eq!(engine.refreshed(), vec!["u-A", "u-B"]);
    }
}
