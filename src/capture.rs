// Copyright (c) 2026 Oleksii PELYKH
// SPDX-License-Identifier: MIT

//! The roster write paths: the `capture` command (#4), the `login` command (#135) and the
//! `login` reconcile (#134) it drives.
//!
//! Both land an account into the roster through the SHARED capture-plan
//! ([`plan_capture`]) — `capture` snapshots the account currently logged in to
//! Claude Code, while [`reconcile_login`] lands a credential freshly harvested in
//! isolation by the login engine ([`crate::login`], #132). The user-facing [`login`] verb (#135)
//! wires the two: it drives the capture engine, then reconciles the harvest, then emits ONE
//! redacted [`crate::observability::Event::Login`] audit line for the outcome. They differ only in
//! where the credential comes from and whether the login also becomes active:
//! `capture` reads the already-active canonical credential and does not touch it,
//! whereas the login reconcile re-points the canonical item to the fresh
//! credential (the login takes effect) under the swap lock — see [`reconcile_login`].
//!
//! While an account is the one currently logged in to Claude Code, `capture`:
//!   1. reads that account's `~/.claude.json` `oauthAccount` block
//!      ([`crate::claude_state`]),
//!   2. reads the active `Claude Code-credentials` token ([`crate::keychain`]),
//!   3. stashes both under a per-account `Sessiometer/<account_uuid>` keychain
//!      service ([`crate::stash`]), and
//!   4. writes/refreshes the account's roster entry in `config.toml`
//!      ([`crate::config`]).
//!
//! Accounts are identified by `oauthAccount.accountUuid`: a second `capture` of
//! an already-rostered account is an idempotent *refresh* (same stash, token
//! and identity re-stashed), reported distinctly from a first *capture*. The
//! operator repeats capture-then-`claude /login` once per account (the only
//! interactive step). All output names the account by its **label** only — never
//! the email or token (issue #15 redaction).
//!
//! Capture reads the identity block and the token in two steps, so it assumes the
//! active account does not change underneath it — i.e. the operator does not run
//! `claude /login` *during* a capture. A concurrent re-login could pair one
//! account's token with another's identity; per `build/version-compat.md` that
//! mismatch only mis-displays (auth follows the token), but it would mis-key the
//! roster entry. The capture-then-`/login` loop is sequential, so this does not
//! arise in normal use; #6 should be aware of it when reasoning about staleness.
//!
//! The decision logic ([`plan_capture`]) is a pure function over the roster, and
//! the orchestration ([`run_capture`]) is generic over the stash seam, so both
//! are unit-tested hermetically; [`capture`] only wires the real seams, persists,
//! and prints.

use crate::claude_state::{read_oauth_account, write_oauth_account, OauthAccount};
use crate::config::{Account, Config, LoginConfig, RefreshConfig, Tunables};
use crate::error::{Error, Result};
use crate::keychain::{Credential, CredentialStore, RealCredentialStore};
use crate::login::login_account;
use crate::observability::{Event, EventLog, LoginEventOutcome};
use crate::paths;
use crate::stash::{AccountStash, RealAccountStash, StashedAccount};
use crate::swap::{SwapLock, SWAP_LOCK_MAX_WAIT};
use std::path::Path;
use std::time::Duration;

/// Whether a `capture` added a new account or refreshed an existing one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CaptureOutcome {
    Captured,
    Refreshed,
}

/// The result of planning + stashing a capture: the config to persist plus the
/// facts the confirmation line needs.
struct CaptureReport {
    config: Config,
    outcome: CaptureOutcome,
    label: String,
    count: usize,
}

/// Run the `capture` command: read the active credential + identity, stash them,
/// update the roster, and print the confirmation.
pub(crate) async fn capture(label: Option<String>) -> Result<()> {
    // Identity first (a cheap file read) so "not logged in" fails before we touch
    // the keychain; then the active token.
    let oauth = read_oauth_account()?;
    let credential = RealCredentialStore::new().read().await?;

    let existing = load_existing()?;
    let report = run_capture(
        credential,
        oauth,
        &RealAccountStash::new(),
        existing,
        label.as_deref(),
    )
    .await?;

    report.config.save()?;
    // Tell a running daemon to pick up the new roster now (#139) — best-effort, so no
    // daemon (or a wedged one) never blocks capture; the disk write is authoritative.
    notify_daemon_roster_reload().await;
    println!(
        "{}",
        confirmation(report.outcome, &report.label, report.count)
    );
    Ok(())
}

/// Run the `login` command (issue #135): drive the isolated interactive-login capture engine
/// ([`login_account`], #132), reconcile the fresh harvest into the roster ([`reconcile_login`],
/// #134), and emit ONE redacted [`Event::Login`] audit line for the outcome.
///
/// The optional `label` names a NEW account (an omitted / blank label auto-derives from the
/// account uuid via the shared capture-plan path, #134); a re-login of an already-rostered
/// account keeps its label unless a new, non-empty one is given.
///
/// Terminal behavior (issue #135 AC):
///   - **onboarded / revived** — the harvest landed; print a confirmation and exit `0`.
///   - **cancelled** — the operator did not complete the login (timeout / SIGINT); print
///     "login cancelled, nothing captured" and exit `0` (nothing was written).
///   - **failed** — the engine or the reconcile aborted (e.g. a LOCKED KEYCHAIN, which aborts
///     ONE-SHOT with no wait loop); the error propagates and `main` maps it to the existing
///     taxonomy ([`Error::exit_code`] — a locked keychain, `security` exit 36, is exit `4`).
///
/// The `[login]` config block supplies the capture timeout and an optional `claude` binary
/// override (both defaulted when no `config.toml` exists yet — the first login precedes it).
pub(crate) async fn login(label: Option<String>) -> Result<()> {
    // The `[login]` settings: capture timeout + optional binary override. A MALFORMED config is a
    // hard error surfaced BEFORE the interactive login (never run a multi-minute login only to
    // fail on save); an ABSENT config yields the defaults (the first login precedes any
    // `config.toml`). The override threads through the SAME resolver the refresh path uses (#135
    // AC: no new binary-override mechanism).
    let login_cfg = load_existing()?.map(|c| c.login).unwrap_or_default();

    match login_account(login_cfg.claude_bin.as_deref(), login_cfg.timeout()).await {
        Ok(capture) => {
            // The non-secret account handle the engine surfaces (the account uuid — exactly what
            // `list` prints), or `None` for an incomplete capture. Read via the engine's own
            // accessor BEFORE `into_captured` consumes the outcome; kept as the event handle for a
            // reconcile failure (a success reports the resolved roster label instead).
            let uuid_handle = capture.account_uuid().map(str::to_owned);
            match capture.into_captured() {
                // A completed login: the fresh credential + identity were harvested.
                Some(captured) => match reconcile_login(captured, label).await {
                    Ok((outcome, label, count)) => {
                        let event_outcome = match outcome {
                            LoginOutcome::Onboarded => LoginEventOutcome::Onboarded,
                            LoginOutcome::Revived => LoginEventOutcome::Revived,
                        };
                        emit_login_event(Some(label.clone()), event_outcome);
                        println!("{}", login_confirmation(outcome, &label, count));
                        Ok(())
                    }
                    // Harvested, but landing it in the roster failed (e.g. a contended swap lock,
                    // a save error). We still know WHICH account — report it on the failed event.
                    Err(err) => {
                        emit_login_event(uuid_handle, LoginEventOutcome::Failed);
                        Err(err)
                    }
                },
                // The login did not complete: a timeout or an operator SIGINT. Nothing was
                // harvested — exit 0 with a clear message (issue #135 AC), never a nonzero
                // "failure".
                None => {
                    emit_login_event(None, LoginEventOutcome::Cancelled);
                    println!("login cancelled, nothing captured");
                    Ok(())
                }
            }
        }
        // The capture engine aborted before a harvest (a LOCKED KEYCHAIN — one-shot, no wait loop;
        // a non-tty stdout; a spawn failure; a shared-item mutation). Emit the failed event, then
        // propagate so `main` maps the error to its existing exit code (a locked keychain → 4).
        Err(err) => {
            emit_login_event(None, LoginEventOutcome::Failed);
            Err(err)
        }
    }
}

/// Emit the single redacted [`Event::Login`] audit line (issue #135) — BEST-EFFORT: the login's
/// own outcome (onboarded / revived / cancelled / the propagated error) stands regardless of
/// whether the audit log is writable, so a failure to open or append it is swallowed rather than
/// masking the real result. `account` is a redacted handle (label or uuid), or `None` when no
/// account was ever identified (a cancel, or a failure before harvest).
fn emit_login_event(account: Option<String>, outcome: LoginEventOutcome) {
    if let Ok(mut log) = EventLog::open() {
        let _ = log.emit(&Event::Login { account, outcome });
    }
}

/// Best-effort notify a running daemon that the on-disk roster changed (issue #139):
/// resolve the control socket and send `roster-reload` so the daemon reconciles its
/// in-memory rotation to the freshly-written `config.toml` WITHOUT a restart. Called by
/// every roster-write verb — [`capture`], the [`reconcile_login`] path (`login`), and
/// `remove` — AFTER the `config.toml` save committed, so the daemon re-reads the
/// authoritative new file.
///
/// BEST-EFFORT, exactly like the `use` manual-hold notify (#64): the on-disk write is
/// authoritative (the roster change already succeeded), so a failure — no daemon
/// running (connect refused / socket absent), a timeout, an unresolvable socket path —
/// is logged and ignored, never failing the verb. With no daemon running there is
/// nothing to keep stale: the next `run` loads the fresh roster at startup.
pub(crate) async fn notify_daemon_roster_reload() {
    let socket = match paths::control_socket() {
        Ok(socket) => socket,
        Err(err) => {
            eprintln!(
                "sessiometer: roster-reload notify skipped (cannot resolve control socket): {err}"
            );
            return;
        }
    };
    if let Err(err) = crate::daemon::notify_roster_reload(&socket).await {
        eprintln!("sessiometer: roster-reload notify skipped (is the daemon running?): {err}");
    }
}

/// The operator-facing confirmation for a landed login (issue #135) — the `login` counterpart of
/// [`confirmation`], in the onboarded/revived vocabulary. Names the account by its LABEL only,
/// never the email or token (#15).
fn login_confirmation(outcome: LoginOutcome, label: &str, count: usize) -> String {
    match outcome {
        LoginOutcome::Onboarded => format!("Onboarded \"{label}\" (now {count} in rotation)."),
        LoginOutcome::Revived => format!("Revived \"{label}\" (still {count} in rotation)."),
    }
}

/// Load the existing config so `capture` can add to it.
///
/// An absent file is `None` (the first capture creates `config.toml`). A file that
/// EXISTS — including a well-formed tunables-only one with an *empty* roster — is
/// `Some(config)`, so its tunables are PRESERVED when the first account is added
/// (an empty roster no longer fails to load; the "at least one account" rule is the
/// daemon's [`Config::require_roster`] precondition, not a load-time rejection, #58).
/// A file that exists but is *malformed* stays a hard error — never silently replaced.
fn load_existing() -> Result<Option<Config>> {
    load_existing_from(&paths::config_file()?)
}

/// [`load_existing`] against an explicit path — the injectable seam over
/// [`Config::load_path`], so the three outcomes above (absent → `None`,
/// tunables-only / empty-roster → `Some` with tunables preserved, malformed →
/// `Err`) are testable end-to-end against a controlled on-disk file rather than the
/// real config location. This is the exact `capture` config-load path
/// (`load_existing` → [`Config::load_path`]) that the #58 fix exercised but that
/// prior tests covered only transitively (#59).
fn load_existing_from(path: &Path) -> Result<Option<Config>> {
    match Config::load_path(path) {
        Ok(config) => Ok(Some(config)),
        Err(Error::ConfigNotFound { .. }) => Ok(None),
        Err(err) => Err(err),
    }
}

/// Stash the account and produce the updated config. Generic over the stash seam
/// so it is testable with an in-memory fake; the credential and identity are
/// passed in (already read) so this function performs no keychain/file reads
/// itself.
async fn run_capture(
    credential: Credential,
    oauth: OauthAccount,
    stash: &impl AccountStash,
    existing: Option<Config>,
    label: Option<&str>,
) -> Result<CaptureReport> {
    // Preserve the existing tunables, the periodic-refresh schedule AND the `[login]` settings
    // across a capture (issue #58, extended for #105/#135): adding an account to a config that
    // already carries custom tunables / a `[refresh]` / `[login]` block must not reset any to
    // defaults.
    let (mut roster, tunables, refresh, login) = match existing {
        Some(config) => (config.roster, config.tunables, config.refresh, config.login),
        None => (
            Vec::new(),
            Tunables::default(),
            RefreshConfig::default(),
            LoginConfig::default(),
        ),
    };

    let (stash_name, outcome) = plan_capture(&mut roster, oauth.account_uuid(), label)?;

    let stashed = StashedAccount {
        credential,
        oauth_account: oauth,
    };
    // Stash BEFORE persisting the roster: if this fails, config.toml is never
    // written to reference an unstashed (or half-stashed) stash.
    stash.write(&stash_name, &stashed).await?;

    let count = roster.len();
    // The final label lives on the rostered account (a refresh may have updated it).
    let label = roster
        .iter()
        .find(|a| a.stash() == stash_name)
        .expect("the account just planned is in the roster")
        .label
        .clone();

    Ok(CaptureReport {
        config: Config {
            roster,
            tunables,
            refresh,
            login,
        },
        outcome,
        label,
        count,
    })
}

/// Auto-derive a roster label from the immutable `account_uuid` — the fallback when
/// the operator omits the optional label (issue #134).
///
/// The `account_uuid` is the ONLY non-secret, always-present, unique field the
/// harvested identity exposes: `displayName` is deliberately not surfaced (two
/// distinct accounts can share one — `build/version-compat.md`) and `emailAddress`
/// is redacted (#15). So the derived label IS the uuid — unique (it is the roster
/// key) and safe to print — which the operator can rename later by re-capturing /
/// re-logging-in with an explicit label.
fn derive_label(account_uuid: &str) -> String {
    account_uuid.to_owned()
}

/// Pure roster update. Returns the stash service to write and whether this was a new
/// capture or a refresh. Mutates `roster` in place (appending a new account, or
/// updating an existing one's label).
///
/// The `label` is OPTIONAL (issue #134). For a NEW account, an omitted or blank label
/// is auto-derived via [`derive_label`] rather than rejected — the shared capture-plan
/// path that both `capture` and the [`reconcile_login`] reconcile take, so neither
/// hard-errors nor prompts on a missing label. A re-capture / re-login of an EXISTING
/// account keeps its current label unless a new, non-empty one is given (an
/// auto-derived label never clobbers the operator's chosen name).
fn plan_capture(
    roster: &mut Vec<Account>,
    account_uuid: &str,
    label: Option<&str>,
) -> Result<(String, CaptureOutcome)> {
    let provided = label.map(str::trim).filter(|l| !l.is_empty());

    if let Some(existing) = roster.iter_mut().find(|a| a.account_uuid == account_uuid) {
        // Idempotent refresh: same stash; update the label only if a new, non-empty
        // one was given (otherwise keep what the operator named it before).
        if let Some(l) = provided {
            existing.label = l.to_owned();
        }
        return Ok((existing.stash(), CaptureOutcome::Refreshed));
    }

    // New account: no explicit label → auto-derive one (never reject or prompt,
    // issue #134). There is no roster ceiling (#35) — the operator captures as many
    // accounts as they choose, so a new account is always appended.
    let label = provided.map_or_else(|| derive_label(account_uuid), str::to_owned);
    // Key the stash by the immutable, server-assigned account_uuid — not a
    // positional slot. The keychain service accepts the uuid (hex + hyphens)
    // verbatim, and the stash uses fixed `acct=credential`/`acct=oauthAccount`,
    // so no resolve/uniqueness step is needed (unlike the canonical item). The
    // service name is derived by `Account::stash`, never stored (issue #70).
    let account = Account {
        account_uuid: account_uuid.to_owned(),
        label,
        // A freshly captured account joins the rotation enabled (issue #36).
        enabled: true,
    };
    let stash = account.stash();
    roster.push(account);
    Ok((stash, CaptureOutcome::Captured))
}

/// The confirmation line — label only, never the email or token (issue #15).
fn confirmation(outcome: CaptureOutcome, label: &str, count: usize) -> String {
    match outcome {
        CaptureOutcome::Captured => {
            // No fixed "of N" denominator (#35) — report the running count only.
            format!("Captured \"{label}\" (now {count} in rotation).")
        }
        CaptureOutcome::Refreshed => {
            format!("Refreshed \"{label}\" (still {count} in rotation).")
        }
    }
}

// --- login reconcile (issue #134) ----------------------------------------------

/// Whether a `login` reconcile ONBOARDED a brand-new account or REVIVED one already in
/// the roster (issue #134). The `login` counterpart of [`CaptureOutcome`] — distinct
/// vocabulary because a login is a fresh interactive re-auth (a possibly-quarantined
/// account brought back), not the active-account snapshot `capture` takes. Consumed by
/// the `login` verb (#135) for its redacted `onboarded|revived` event.
#[cfg_attr(not(test), allow(dead_code))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LoginOutcome {
    /// The harvested account was not in the roster — a new entry was appended.
    Onboarded,
    /// The harvested account was already in the roster — its entry was updated IN
    /// PLACE (never duplicated) and its stash + canonical re-pointed to the fresh
    /// credential; the running daemon's #107 path un-quarantines it on that canonical
    /// change (clearing any "needs re-login").
    Revived,
}

impl From<CaptureOutcome> for LoginOutcome {
    fn from(outcome: CaptureOutcome) -> Self {
        // A login ONBOARDS where a capture would "capture" (new account) and REVIVES
        // where a capture would "refresh" (existing account): the SAME roster decision
        // (via the shared [`plan_capture`]), surfaced in login-facing vocabulary.
        match outcome {
            CaptureOutcome::Captured => LoginOutcome::Onboarded,
            CaptureOutcome::Refreshed => LoginOutcome::Revived,
        }
    }
}

/// The result of reconciling a harvested login into the roster: the config to persist
/// plus the facts the `login` verb (#135) needs for its event.
#[cfg_attr(not(test), allow(dead_code))]
struct LoginReport {
    config: Config,
    outcome: LoginOutcome,
    label: String,
    count: usize,
}

/// Reconcile a freshly-harvested login ([`StashedAccount`] from the isolated capture
/// engine, #132) into the roster — the hermetic core (issue #134). Generic over both
/// keychain seams so it is unit-tested with in-memory fakes; performs NO lock, NO config
/// persistence, and NO reads of the real environment (every input is passed in).
///
/// Onboard (new account) or update-IN-PLACE (existing, matched by `account_uuid`) via
/// the SHARED [`plan_capture`]; then, mirroring the swap engine's write ordering (#6):
/// re-stash the fresh credential, re-point the canonical `Claude Code-credentials` item
/// to it — this is the canonical change the running daemon's #107 path un-quarantines a
/// re-logged-in account on (there is no roster-persisted quarantine flag a CLI could
/// clear directly) — then best-effort co-write the identity into `~/.claude.json`.
async fn run_login<C, S>(
    captured: StashedAccount,
    store: &C,
    stash: &S,
    existing: Option<Config>,
    label: Option<&str>,
    claude_json: &Path,
) -> Result<LoginReport>
where
    C: CredentialStore,
    S: AccountStash,
{
    // Preserve the operator's tunables + refresh schedule + `[login]` settings across the
    // reconcile, exactly like `run_capture` (#58/#105/#135): landing a login must never reset
    // any of them to defaults.
    let (mut roster, tunables, refresh, login) = match existing {
        Some(config) => (config.roster, config.tunables, config.refresh, config.login),
        None => (
            Vec::new(),
            Tunables::default(),
            RefreshConfig::default(),
            LoginConfig::default(),
        ),
    };

    let (stash_name, outcome) =
        plan_capture(&mut roster, captured.oauth_account.account_uuid(), label)?;

    // Re-stash the fresh credential BEFORE re-pointing canonical (#6 ordering): a crash
    // between the two leaves a fresh, restorable stash, never a canonical pointing at an
    // unstashed credential.
    stash.write(&stash_name, &captured).await?;

    // Re-point the canonical item to the fresh credential: the freshly-logged-in account
    // becomes the active one AND — being a canonical change — is what the running
    // daemon's #107 reconcile un-quarantines the account on. Atomic (`security -U`),
    // exactly like the swap engine's incoming write.
    store.write(&captured.credential).await?;

    // Best-effort honest-display co-write (the swap engine's step 4): a failure
    // self-heals on the daemon's next reconcile, so it never fails the login.
    let _ = write_oauth_account(claude_json, &captured.oauth_account);

    let count = roster.len();
    // The final label lives on the rostered account (an onboard auto-derived it; a
    // revive kept the prior label unless a new, non-empty one was given).
    let label = roster
        .iter()
        .find(|a| a.stash() == stash_name)
        .expect("the account just planned is in the roster")
        .label
        .clone();

    Ok(LoginReport {
        config: Config {
            roster,
            tunables,
            refresh,
            login,
        },
        outcome: outcome.into(),
        label,
        count,
    })
}

/// [`run_login`] wrapped in the single-writer swap lock (issue #64) when `lock` is
/// `Some((path, max_wait))` — mirrors [`crate::swap::swap_locked`]. The lock is held
/// ONLY around the short keychain write (stash + canonical), NEVER across the
/// interactive login spawn (that ran in the capture engine, #132, before we get here).
/// A `lock` of `None` runs unlocked: the hermetic test path, where there is no
/// concurrent swap to serialize against. A contended acquire fails closed BEFORE any
/// write; when the lock IS taken, the operator's fresh interactive login is the most
/// recent authoritative write, so it wins a race with a concurrent swap (last-writer-wins).
#[cfg_attr(not(test), allow(dead_code))]
async fn run_login_locked<C, S>(
    lock: Option<(&Path, Duration)>,
    captured: StashedAccount,
    store: &C,
    stash: &S,
    existing: Option<Config>,
    label: Option<&str>,
    claude_json: &Path,
) -> Result<LoginReport>
where
    C: CredentialStore,
    S: AccountStash,
{
    // Bind the guard so it outlives the whole write and drops on return (releasing the
    // lock). Acquired BEFORE any write, so a contended refusal is a true no-op.
    let _guard = match lock {
        Some((path, max_wait)) => Some(SwapLock::acquire(path, max_wait).await?),
        None => None,
    };
    run_login(captured, store, stash, existing, label, claude_json).await
}

/// Reconcile a harvested login into the roster over the REAL seams — the production
/// entry point (issue #134) the `login` verb (#135) calls after the capture engine
/// (#132) hands back a [`StashedAccount`]. Wires the real keychain store + stash, holds
/// the swap lock around the short write (serializing against a concurrent daemon swap),
/// then persists the roster. Wired into production by the [`login`] verb (#135).
///
/// The roster (`config.toml`) write is deliberately OUTSIDE the lock: a swap contends
/// only on the keychain + `~/.claude.json`, never on `config.toml`, so no concurrent
/// swap can race it. Stash-before-roster (like [`capture`]): a crash after the locked
/// write but before the save leaves a fresh, restorable stash + canonical, never a
/// roster referencing an unstashed account.
pub(crate) async fn reconcile_login(
    captured: StashedAccount,
    label: Option<String>,
) -> Result<(LoginOutcome, String, usize)> {
    // Ensure the native-local support dir (0700) that houses `swap.lock` exists before
    // acquiring the lock (mirrors `use`, #64).
    paths::ensure_private_dir(&paths::support_dir()?)?;
    let swap_lock = paths::swap_lock()?;
    let claude_json = paths::claude_json()?;
    let existing = load_existing()?;

    let report = run_login_locked(
        Some((&swap_lock, SWAP_LOCK_MAX_WAIT)),
        captured,
        &RealCredentialStore::new(),
        &RealAccountStash::new(),
        existing,
        label.as_deref(),
        &claude_json,
    )
    .await?;

    report.config.save()?;
    // Tell a running daemon to pick up the onboarded / relogged-in account now (#139) —
    // best-effort, the login already committed to disk.
    notify_daemon_roster_reload().await;
    Ok((report.outcome, report.label, report.count))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keychain::FakeCredentialStore;
    use crate::stash::FakeAccountStash;

    fn account(uuid: &str, label: &str) -> Account {
        Account {
            account_uuid: uuid.to_owned(),
            label: label.to_owned(),
            enabled: true,
        }
    }

    fn oauth(uuid: &str) -> OauthAccount {
        let json = format!(r#"{{"accountUuid":"{uuid}","displayName":"ignored"}}"#);
        OauthAccount::from_object_bytes(json.as_bytes()).unwrap()
    }

    /// A freshly-harvested login (the #132 capture engine's hand-off): a fresh
    /// credential bundled with its `oauthAccount` identity.
    fn stashed(uuid: &str, token: &[u8]) -> StashedAccount {
        StashedAccount {
            credential: Credential::new(token.to_vec()),
            oauth_account: oauth(uuid),
        }
    }

    // --- plan_capture (pure) ---

    #[test]
    fn plans_a_new_account_into_an_empty_roster() {
        let mut roster = Vec::new();
        let (stash, outcome) = plan_capture(&mut roster, "u-1", Some("work")).unwrap();
        assert_eq!(stash, "Sessiometer/u-1");
        assert_eq!(outcome, CaptureOutcome::Captured);
        assert_eq!(roster.len(), 1);
        assert_eq!(roster[0], account("u-1", "work"));
    }

    #[test]
    fn mints_the_stash_service_from_the_account_uuid() {
        // AC: a new capture mints the stash as `Sessiometer/<account_uuid>`,
        // keyed by the immutable account_uuid (hyphens accepted verbatim) — no
        // positional `acct-N` slot.
        let mut roster = Vec::new();
        let (stash, _) = plan_capture(
            &mut roster,
            "11111111-1111-1111-1111-111111111111",
            Some("work"),
        )
        .unwrap();
        assert_eq!(stash, "Sessiometer/11111111-1111-1111-1111-111111111111");
        assert_eq!(
            roster[0].stash(),
            "Sessiometer/11111111-1111-1111-1111-111111111111"
        );
    }

    #[test]
    fn a_new_account_without_a_label_auto_derives_from_the_account_uuid() {
        // Issue #134: an omitted label is NOT rejected — it auto-derives from the
        // account_uuid (the only exposed non-secret unique field), so the shared
        // capture-plan path never hard-errors nor prompts on a missing label.
        let mut roster = Vec::new();
        let (stash, outcome) = plan_capture(&mut roster, "u-1", None).unwrap();
        assert_eq!(stash, "Sessiometer/u-1");
        assert_eq!(outcome, CaptureOutcome::Captured);
        assert_eq!(roster.len(), 1);
        assert_eq!(roster[0], account("u-1", "u-1"));
    }

    #[test]
    fn a_blank_label_on_a_new_account_auto_derives_from_the_account_uuid() {
        // A whitespace-only label is treated as absent (trimmed to empty) and the
        // account_uuid is used — the same auto-derive path as an omitted label (#134).
        let mut roster = Vec::new();
        let (_, outcome) = plan_capture(&mut roster, "u-1", Some("   ")).unwrap();
        assert_eq!(outcome, CaptureOutcome::Captured);
        assert_eq!(roster.len(), 1);
        assert_eq!(roster[0].label, "u-1");
    }

    #[test]
    fn label_argument_is_trimmed() {
        let mut roster = Vec::new();
        plan_capture(&mut roster, "u-1", Some("  work  ")).unwrap();
        assert_eq!(roster[0].label, "work");
    }

    #[test]
    fn recapture_is_a_refresh_on_the_same_stash() {
        let mut roster = vec![account("u-1", "work")];
        let (stash, outcome) = plan_capture(&mut roster, "u-1", None).unwrap();
        assert_eq!(stash, "Sessiometer/u-1");
        assert_eq!(outcome, CaptureOutcome::Refreshed);
        // Size unchanged; label kept (no new label given). A refresh does NOT
        // require a label — only a new capture does.
        assert_eq!(roster.len(), 1);
        assert_eq!(roster[0].label, "work");
    }

    #[test]
    fn recapture_updates_the_label_when_a_new_one_is_given() {
        let mut roster = vec![account("u-1", "work")];
        plan_capture(&mut roster, "u-1", Some("personal")).unwrap();
        assert_eq!(roster[0].label, "personal");
        assert_eq!(roster.len(), 1);
    }

    #[test]
    fn a_new_account_is_keyed_by_its_account_uuid() {
        // A second distinct account is keyed by its OWN account_uuid — there is no
        // positional slot allocation; the stash is `Sessiometer/<account_uuid>`.
        let mut roster = vec![account("u-1", "work")];
        let (stash, outcome) = plan_capture(&mut roster, "u-2", Some("personal")).unwrap();
        assert_eq!(stash, "Sessiometer/u-2");
        assert_eq!(outcome, CaptureOutcome::Captured);
        assert_eq!(roster.len(), 2);
    }

    #[test]
    fn capturing_beyond_the_former_cap_succeeds() {
        // #35: there is no roster ceiling — a 6th (and beyond) new account is
        // appended, not rejected.
        let mut roster: Vec<Account> = (1..=5).map(|i| account(&format!("u-{i}"), "l")).collect();
        let (stash, outcome) = plan_capture(&mut roster, "u-6", Some("sixth")).unwrap();
        assert_eq!(stash, "Sessiometer/u-6");
        assert_eq!(outcome, CaptureOutcome::Captured);
        assert_eq!(roster.len(), 6);
        // …and a 7th continues to append.
        plan_capture(&mut roster, "u-7", Some("seventh")).unwrap();
        assert_eq!(roster.len(), 7);
    }

    // --- confirmation (exact AC strings) ---

    #[test]
    fn confirmation_lines_match_the_acceptance_criteria() {
        // #35: no fixed "of N" denominator — the captured line carries the running
        // count only. A count of 6 (past the former cap) is an ordinary capture.
        assert_eq!(
            confirmation(CaptureOutcome::Captured, "work", 6),
            "Captured \"work\" (now 6 in rotation)."
        );
        assert_eq!(
            confirmation(CaptureOutcome::Refreshed, "personal", 2),
            "Refreshed \"personal\" (still 2 in rotation)."
        );
    }

    #[test]
    fn login_confirmation_lines_name_the_account_by_label() {
        // Issue #135: the landed-login confirmation is the onboarded/revived counterpart of the
        // capture confirmation — the account named by its LABEL only (never email/token, #15),
        // with the running count and no fixed denominator (#35).
        assert_eq!(
            login_confirmation(LoginOutcome::Onboarded, "work", 3),
            "Onboarded \"work\" (now 3 in rotation)."
        );
        assert_eq!(
            login_confirmation(LoginOutcome::Revived, "personal", 2),
            "Revived \"personal\" (still 2 in rotation)."
        );
    }

    // --- run_capture (orchestration over the fake stash) ---

    #[tokio::test]
    async fn first_capture_creates_a_one_account_roster_and_stashes_both_halves() {
        let stash = FakeAccountStash::empty();
        let report = run_capture(
            Credential::new(b"token-1".to_vec()),
            oauth("u-1"),
            &stash,
            None,
            Some("work"),
        )
        .await
        .unwrap();

        assert_eq!(report.outcome, CaptureOutcome::Captured);
        assert_eq!(report.count, 1);
        assert_eq!(report.label, "work");
        assert_eq!(report.config.roster.len(), 1);
        assert_eq!(report.config.roster[0].stash(), "Sessiometer/u-1");
        assert_eq!(report.config.roster[0].account_uuid, "u-1");

        // Both halves are in the stash under its service name.
        assert!(stash.contains("Sessiometer/u-1"));
        let stashed = stash.read("Sessiometer/u-1").await.unwrap();
        assert_eq!(stashed.credential.expose(), b"token-1");
        assert_eq!(stashed.oauth_account.account_uuid(), "u-1");
    }

    #[tokio::test]
    async fn bootstraps_the_first_account_into_a_tunables_only_config_preserving_tunables() {
        // Regression (#58): an existing config with custom tunables but an EMPTY
        // roster (a fresh tunables-only file, or one whose last account was just
        // `remove`d) must load and accept the first account WITHOUT resetting the
        // operator's tunables to defaults — the data-loss trap a naive "treat the
        // empty-roster error as None" fix would have introduced.
        let stash = FakeAccountStash::empty();
        let existing = Config {
            roster: vec![],
            tunables: Tunables {
                poll_secs: 120,          // a non-default the operator set
                session_floor: Some(80), // opt-in guard the operator enabled
                ..Tunables::default()
            },
            refresh: RefreshConfig::default(),
            login: LoginConfig::default(),
        };

        let report = run_capture(
            Credential::new(b"token-1".to_vec()),
            oauth("u-1"),
            &stash,
            Some(existing),
            Some("work"),
        )
        .await
        .unwrap();

        assert_eq!(report.outcome, CaptureOutcome::Captured);
        assert_eq!(report.config.roster.len(), 1);
        // The operator's tunables survive the bootstrap (NOT reset to defaults:
        // poll_secs default is 300, session_floor default is None).
        assert_eq!(report.config.tunables.poll_secs, 120);
        assert_eq!(report.config.tunables.session_floor, Some(80));
    }

    #[tokio::test]
    async fn a_capture_preserves_a_custom_login_block() {
        // Issue #135: a `capture` — and, via the IDENTICAL preserve path, the login reconcile —
        // must NOT reset an operator's custom `[login]` settings to defaults when it re-saves the
        // config. The same data-loss trap the tunables/refresh preservation guards against.
        let stash = FakeAccountStash::empty();
        let login = LoginConfig {
            timeout_secs: 420,                                 // a non-default the operator set
            claude_bin: Some("/opt/claude/bin/claude".into()), // an explicit override
        };
        let existing = Config {
            roster: vec![],
            tunables: Tunables::default(),
            refresh: RefreshConfig::default(),
            login: login.clone(),
        };

        let report = run_capture(
            Credential::new(b"token-1".to_vec()),
            oauth("u-1"),
            &stash,
            Some(existing),
            Some("work"),
        )
        .await
        .unwrap();

        // The operator's [login] settings survive the capture (NOT reset: the timeout default is
        // 180, claude_bin default is None).
        assert_eq!(report.config.login, login);
    }

    #[tokio::test]
    async fn recapture_refreshes_the_stash_without_growing_the_roster() {
        let stash = FakeAccountStash::empty();
        let existing = Config {
            roster: vec![account("u-1", "work")],
            tunables: Tunables::default(),
            refresh: RefreshConfig::default(),
            login: LoginConfig::default(),
        };

        let report = run_capture(
            Credential::new(b"rotated".to_vec()),
            oauth("u-1"),
            &stash,
            Some(existing),
            None,
        )
        .await
        .unwrap();

        assert_eq!(report.outcome, CaptureOutcome::Refreshed);
        assert_eq!(report.count, 1);
        assert_eq!(report.label, "work");
        assert_eq!(report.config.roster.len(), 1);
        // The stash was refreshed with the new token.
        assert_eq!(stash.len(), 1);
        let stashed = stash.read("Sessiometer/u-1").await.unwrap();
        assert_eq!(stashed.credential.expose(), b"rotated");
    }

    #[tokio::test]
    async fn a_second_distinct_account_is_appended() {
        let stash = FakeAccountStash::empty();
        let existing = Config {
            roster: vec![account("u-1", "work")],
            tunables: Tunables::default(),
            refresh: RefreshConfig::default(),
            login: LoginConfig::default(),
        };

        let report = run_capture(
            Credential::new(b"token-2".to_vec()),
            oauth("u-2"),
            &stash,
            Some(existing),
            Some("personal"),
        )
        .await
        .unwrap();

        assert_eq!(report.outcome, CaptureOutcome::Captured);
        assert_eq!(report.count, 2);
        assert_eq!(report.config.roster.len(), 2);
        assert_eq!(report.config.roster[1].stash(), "Sessiometer/u-2");
        assert_eq!(stash.len(), 1); // only the new stash was written this call
        assert!(stash.contains("Sessiometer/u-2"));
    }

    // --- load_existing_from (the on-disk load_existing → Config::load_path seam, #59) ---

    #[test]
    fn load_existing_from_reads_a_tunables_only_file_preserving_tunables() {
        // #58 regression, now end-to-end on disk: a REAL tunables-only config.toml
        // (operator tunables, no [[account]] → empty roster) loads as `Some` with the
        // tunables intact and an empty roster. Previously this exact path
        // (load_existing → Config::load_path) was covered only transitively — a
        // validate test plus an in-memory run_capture test — never against a real file.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(
            &path,
            b"[tunables]\npoll_secs = 120\nsession_trigger = 90\nsession_floor = 80\n",
        )
        .unwrap();

        let loaded = load_existing_from(&path).unwrap();
        let config = loaded.expect("a tunables-only file that EXISTS is Some, not None");
        assert!(
            config.roster.is_empty(),
            "a file with no [[account]] loads with an empty roster"
        );
        // The operator's tunables survive the load — NOT reset to defaults (default
        // poll_secs is 300, default session_floor is None).
        assert_eq!(config.tunables.poll_secs, 120);
        assert_eq!(config.tunables.session_floor, Some(80));
    }

    #[test]
    fn load_existing_from_maps_a_missing_file_to_none() {
        // The first-ever capture: no config.toml yet → None, so capture then creates it.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        assert!(load_existing_from(&path).unwrap().is_none());
    }

    #[test]
    fn load_existing_from_surfaces_a_malformed_file_as_an_error() {
        // A file that EXISTS but does not parse stays a hard error — never silently
        // treated as absent (which would clobber the operator's file on the next save).
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, b"][").unwrap();
        assert!(matches!(
            load_existing_from(&path),
            Err(Error::ConfigParse(_))
        ));
    }

    // --- login reconcile (issue #134) ---

    #[test]
    fn login_outcome_maps_from_the_capture_outcome() {
        // The shared plan yields a capture outcome; the login surfaces it as its own
        // vocabulary — a NEW account is an onboard, an EXISTING one a revive.
        assert_eq!(
            LoginOutcome::from(CaptureOutcome::Captured),
            LoginOutcome::Onboarded
        );
        assert_eq!(
            LoginOutcome::from(CaptureOutcome::Refreshed),
            LoginOutcome::Revived
        );
    }

    // A claude.json path that does not exist: the best-effort co-write inside
    // `run_login` fails and is swallowed (`let _ =`), so the reconcile still succeeds —
    // exactly the honest-display self-heal contract, and it keeps the test off the real
    // `~/.claude.json`.
    fn absent_claude_json(dir: &std::path::Path) -> std::path::PathBuf {
        dir.join("claude.json")
    }

    #[tokio::test]
    async fn login_onboards_an_account_absent_from_the_roster() {
        // AC: a login for an account NOT in the roster ADDS a new entry (onboard),
        // stashes the fresh credential, and re-points the canonical item to it. Here the
        // label is omitted → auto-derived from the account_uuid (issue #134).
        let store = FakeCredentialStore::empty();
        let stash = FakeAccountStash::empty();
        let dir = tempfile::tempdir().unwrap();

        let report = run_login(
            stashed("u-new", b"fresh-token"),
            &store,
            &stash,
            None,
            None,
            &absent_claude_json(dir.path()),
        )
        .await
        .unwrap();

        assert_eq!(report.outcome, LoginOutcome::Onboarded);
        assert_eq!(report.count, 1);
        assert_eq!(report.label, "u-new"); // auto-derived from the account_uuid
        assert_eq!(report.config.roster.len(), 1);
        assert_eq!(report.config.roster[0].account_uuid, "u-new");

        // The fresh credential is stashed under the account's service…
        let stashed = stash.read("Sessiometer/u-new").await.unwrap();
        assert_eq!(stashed.credential.expose(), b"fresh-token");
        // …and the canonical item was re-pointed to it (the login took effect).
        assert_eq!(store.read().await.unwrap().expose(), b"fresh-token");
    }

    #[tokio::test]
    async fn login_writes_the_identity_into_an_existing_claude_json() {
        // The best-effort honest-display co-write: when `~/.claude.json` exists, the
        // reconcile writes the harvested identity into it (self-heals if it doesn't —
        // covered by the absent-path tests). Format correctness is claude_state's own
        // tests; here we prove `run_login` WIRES the co-write.
        let store = FakeCredentialStore::empty();
        let stash = FakeAccountStash::empty();
        let dir = tempfile::tempdir().unwrap();
        let claude_json = dir.path().join("claude.json");
        std::fs::write(&claude_json, b"{}").unwrap();

        run_login(
            stashed("u-disp", b"tok"),
            &store,
            &stash,
            None,
            Some("work"),
            &claude_json,
        )
        .await
        .unwrap();

        let written = std::fs::read_to_string(&claude_json).unwrap();
        assert!(written.contains("oauthAccount"));
        assert!(written.contains("u-disp"));
    }

    #[tokio::test]
    async fn login_updates_an_existing_account_in_place_without_duplicating() {
        // AC: a login for an account ALREADY in the roster (matched by account_uuid)
        // updates IN PLACE — never a duplicate — and preserves the operator's tunables.
        let store = FakeCredentialStore::empty();
        let stash = FakeAccountStash::empty();
        let dir = tempfile::tempdir().unwrap();
        let existing = Config {
            roster: vec![account("u-1", "work")],
            tunables: Tunables {
                poll_secs: 120, // a non-default the operator set
                ..Tunables::default()
            },
            refresh: RefreshConfig::default(),
            login: LoginConfig::default(),
        };

        let report = run_login(
            stashed("u-1", b"re-logged-in"),
            &store,
            &stash,
            Some(existing),
            None, // no new label → keep the operator's "work"
            &absent_claude_json(dir.path()),
        )
        .await
        .unwrap();

        assert_eq!(report.outcome, LoginOutcome::Revived);
        assert_eq!(report.count, 1);
        assert_eq!(report.config.roster.len(), 1); // NOT duplicated
        assert_eq!(report.config.roster[0].account_uuid, "u-1");
        assert_eq!(report.label, "work"); // the prior label is kept, not auto-derived
                                          // The operator's tunables survive the reconcile (poll_secs default is 300).
        assert_eq!(report.config.tunables.poll_secs, 120);
        // The stash now holds the fresh credential.
        let stashed = stash.read("Sessiometer/u-1").await.unwrap();
        assert_eq!(stashed.credential.expose(), b"re-logged-in");
    }

    #[tokio::test]
    async fn a_relogin_repoints_canonical_so_the_daemon_unquarantines() {
        // AC: clears any "needs re-login" state by REUSING the un-quarantine-on-re-stash
        // path (#107). Quarantine is DAEMON runtime state, cleared only on a CANONICAL
        // change; #134's contribution is to WRITE the fresh credential to the canonical
        // item, which the running daemon's #107 reconcile then un-quarantines on. Here we
        // assert that re-point: the canonical starts at a STALE credential (the one that
        // got the account quarantined) and ends at the fresh one.
        let store = FakeCredentialStore::empty();
        store
            .write(&Credential::new(b"stale".to_vec()))
            .await
            .unwrap();
        let stash = FakeAccountStash::empty();
        let dir = tempfile::tempdir().unwrap();
        let existing = Config {
            roster: vec![account("u-1", "work")],
            tunables: Tunables::default(),
            refresh: RefreshConfig::default(),
            login: LoginConfig::default(),
        };

        run_login(
            stashed("u-1", b"fresh"),
            &store,
            &stash,
            Some(existing),
            None,
            &absent_claude_json(dir.path()),
        )
        .await
        .unwrap();

        // Canonical re-pointed from the stale credential to the fresh one → the daemon's
        // #107 path sees a canonical change and un-quarantines the account.
        assert_eq!(store.read().await.unwrap().expose(), b"fresh");
    }

    #[tokio::test]
    async fn capture_without_a_label_auto_derives_from_the_account_uuid() {
        // AC: because the optional-label + auto-derive lives in the SHARED capture-plan
        // path, the `capture` verb's label likewise becomes optional — an omitted label
        // auto-derives from the account_uuid rather than erroring (issue #134).
        let stash = FakeAccountStash::empty();
        let report = run_capture(
            Credential::new(b"token".to_vec()),
            oauth("u-cap"),
            &stash,
            None,
            None, // label omitted
        )
        .await
        .unwrap();

        assert_eq!(report.outcome, CaptureOutcome::Captured);
        assert_eq!(report.label, "u-cap"); // auto-derived
        assert_eq!(report.config.roster[0].account_uuid, "u-cap");
    }

    #[tokio::test]
    async fn run_login_locked_writes_through_an_uncontended_lock() {
        // AC: the stash/roster write serializes against a concurrent swap via the
        // EXISTING swap.lock (#64). Here we prove the locked path is WIRED and completes
        // uncontended (the lock's serialization guarantee itself is proven in swap.rs).
        // The lock is held only around this short write — there is no interactive login
        // spawn in scope (that ran in the capture engine, #132, before we got here).
        let store = FakeCredentialStore::empty();
        let stash = FakeAccountStash::empty();
        let dir = tempfile::tempdir().unwrap();
        let lock = dir.path().join("swap.lock");

        let report = run_login_locked(
            Some((&lock, SWAP_LOCK_MAX_WAIT)),
            stashed("u-lock", b"tok"),
            &store,
            &stash,
            None,
            Some("locked"),
            &absent_claude_json(dir.path()),
        )
        .await
        .unwrap();

        assert_eq!(report.outcome, LoginOutcome::Onboarded);
        assert_eq!(report.label, "locked");
        assert_eq!(store.read().await.unwrap().expose(), b"tok");
        assert!(stash.contains("Sessiometer/u-lock"));
    }

    // Keep the production entry (and its production-only callees — the real seam
    // construction, the swap-lock + config-save wiring) reachable from the test target
    // until #135 wires it to the `login` CLI verb; the reference does not run the async
    // body (no real keychain / config / lock is touched). Mirrors how #132 keeps
    // `login_account` alive by referencing it in a test.
    #[test]
    fn the_login_reconcile_entry_stays_reachable() {
        let _entry = reconcile_login;
    }
}
