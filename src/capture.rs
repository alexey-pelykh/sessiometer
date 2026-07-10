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
//! whereas the login reconcile re-points the canonical item to the fresh credential
//! (the login takes effect) under the swap lock — but ONLY when the login is the
//! current active account (re-auth in place) or none is active (bootstrap); a login
//! for a DIFFERENT account preserves the active slot (#274). See [`reconcile_login`].
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
//! Capture reads the identity block and the token in two steps, so those reads — and
//! the stash that pairs them — run under the single-writer `swap.lock` (#357, via
//! [`capture_locked`]): the daemon's autonomous timer-swap holds the SAME lock, so it can
//! no longer land between the two reads and pair one account's token with another's
//! identity (which would mis-key the roster entry — per `build/version-compat.md` the
//! mismatch only mis-displays, auth following the token, but the roster row would be
//! wrong). The one writer NOT serialized by that lock is an external `claude /login` (a
//! separate process that never takes sessiometer's `flock`); the operator's
//! capture-then-`/login` loop is sequential, so that does not arise in normal use — #6
//! should be aware of it when reasoning about staleness.
//!
//! The decision logic ([`plan_capture`]) is a pure function over the roster, and
//! the orchestration ([`run_capture`]) is generic over the stash seam, so both
//! are unit-tested hermetically; [`capture`] only wires the real seams, persists,
//! and prints.

use crate::claude_state::{read_oauth_account_from, write_oauth_account, OauthAccount};
use crate::config::{
    Account, Config, LoginConfig, MigrationConfig, RefreshConfig, StatsConfig, Tunables,
};
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
pub(crate) enum CaptureOutcome {
    Captured,
    Refreshed,
}

/// The result of planning + stashing a capture: the config to persist plus the
/// facts the confirmation line needs.
pub(crate) struct CaptureReport {
    pub(crate) config: Config,
    pub(crate) outcome: CaptureOutcome,
    pub(crate) label: String,
    pub(crate) count: usize,
}

/// Run the `capture` command: read the active credential + identity, stash them,
/// update the roster, and print the confirmation.
///
/// The canonical read (identity + token) and the stash write run under the single-writer
/// swap lock via [`capture_locked`], so a concurrent daemon swap cannot land between the two
/// reads and pair one account's identity with another's token — the mis-keyed-roster race the
/// module docs name (#357). The roster (`config.toml`) save stays OUTSIDE the lock — a swap
/// never contends on `config.toml` — preserving stash-before-roster, exactly like
/// [`reconcile_login`].
pub(crate) async fn capture(label: Option<String>) -> Result<()> {
    // Ensure the native-local support dir (0700) that houses `swap.lock` exists before
    // acquiring the lock (mirrors `reconcile_login` / `use`, #64).
    paths::ensure_private_dir(&paths::support_dir()?)?;
    let swap_lock = paths::swap_lock()?;
    let claude_json = paths::claude_json()?;
    let existing = load_existing()?;

    let report = capture_locked(
        Some((&swap_lock, SWAP_LOCK_MAX_WAIT)),
        &RealCredentialStore::new(),
        &RealAccountStash::new(),
        &claude_json,
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

/// The capture core wrapped in the single-writer swap lock (issue #64): acquired BEFORE the
/// identity read and held across the whole canonical critical section —
///
///   read-identity ([`read_oauth_account_from`]) → read-token ([`CredentialStore::read`]) →
///   stash-write ([`run_capture`])
///
/// — so no concurrent daemon swap can interleave BETWEEN the two reads and pair one account's
/// `~/.claude.json` identity with another account's keychain token (mis-keying the roster,
/// #357). Mirrors [`run_login_locked`] / [`crate::swap::swap_locked`]: `lock` is
/// `Some((path, max_wait))` in production and `None` on the hermetic single-process test path
/// (no second writer to serialize against). A contended acquire fails closed
/// ([`Error::SwapLockBusy`]) BEFORE any read, so a refusal is a true no-op (ZERO reads/writes).
///
/// The roster (`config.toml`) write is deliberately the CALLER's job, done AFTER this returns
/// with the lock released: a swap contends only on the keychain + `~/.claude.json`, never on
/// `config.toml`, and stash-before-roster means a crash after the locked stash but before the
/// save leaves an inert orphan stash, never a roster row referencing an unstashed account.
/// Generic over both keychain seams and taking the identity path as an argument, so the
/// daemon-routed `cmd:capture` command (#359) can reuse this exact primitive with its own seams.
pub(crate) async fn capture_locked<C, S>(
    lock: Option<(&Path, Duration)>,
    store: &C,
    stash: &S,
    claude_json: &Path,
    existing: Option<Config>,
    label: Option<&str>,
) -> Result<CaptureReport>
where
    C: CredentialStore,
    S: AccountStash,
{
    // Bind the guard so it outlives the whole critical section and drops on return (releasing
    // the lock). Acquired BEFORE the identity read, so a contended refusal is a true no-op and
    // the two reads are ONE atomic pair with respect to a concurrent swap.
    let _guard = match lock {
        Some((path, max_wait)) => Some(SwapLock::acquire(path, max_wait).await?),
        None => None,
    };
    // Identity first (a cheap file read) so "not logged in" fails before we touch the
    // keychain; then the active token. Both under the lock — no swap can land between them.
    let oauth = read_oauth_account_from(claude_json)?;
    let credential = store.read().await?;
    // Stash under the lock (the roster save is the caller's, after the lock releases).
    run_capture(credential, oauth, stash, existing, label).await
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

/// Best-effort notify a running daemon to un-quarantine a revived, non-activating parked
/// account (issue #276): resolve the control socket and send `restored <uuid>` so the daemon
/// clears the account's `needs re-login` quarantine WITHOUT activating it. Called by
/// [`reconcile_login`] AFTER the roster save + `roster-reload` notify, ONLY for a
/// non-activating revive (see [`should_signal_restored`]).
///
/// BEST-EFFORT, exactly like [`notify_daemon_roster_reload`] (#139) and the `use` manual-hold
/// notify (#64): the on-disk stash + roster write is authoritative (the revive already
/// succeeded), so a failure — no daemon running (connect refused / socket absent), a timeout,
/// an unresolvable socket path — is logged and ignored, never failing the login. With no
/// daemon running there is nothing to un-quarantine: the next `run` loads the fresh roster
/// (with the revived account eligible) at startup.
pub(crate) async fn notify_daemon_restored(uuid: &str) {
    let socket = match paths::control_socket() {
        Ok(socket) => socket,
        Err(err) => {
            eprintln!(
                "sessiometer: restored notify skipped (cannot resolve control socket): {err}"
            );
            return;
        }
    };
    if let Err(err) = crate::daemon::notify_restored(&socket, uuid).await {
        eprintln!("sessiometer: restored notify skipped (is the daemon running?): {err}");
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
    // Preserve the existing tunables, the periodic-refresh schedule, the `[login]` settings, the
    // `[stats]` settings AND the `[migration]` settings across a capture (issue #58, extended for
    // #105/#135/#161/#150): adding an account to a config that already carries custom tunables / a
    // `[refresh]` / `[login]` / `[stats]` / `[migration]` block must not reset any to defaults.
    let (mut roster, tunables, refresh, login, stats, migration) = match existing {
        Some(config) => (
            config.roster,
            config.tunables,
            config.refresh,
            config.login,
            config.stats,
            config.migration,
        ),
        None => (
            Vec::new(),
            Tunables::default(),
            RefreshConfig::default(),
            LoginConfig::default(),
            StatsConfig::default(),
            MigrationConfig::default(),
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
            stats,
            migration,
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
    /// PLACE (never duplicated) and its stash re-pointed to the fresh credential. The
    /// canonical item is re-pointed too — the change the running daemon's #107 path
    /// un-quarantines it on (clearing any "needs re-login") — ONLY when this login
    /// becomes active (#274: it IS the current active account, or none is active);
    /// reviving a NON-active account refreshes its stash + roster and leaves the active
    /// slot untouched, so the immediate un-quarantine is deferred (a separate follow-up).
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

/// A freshly-harvested login the reconcile lands, paired with the caller's verdict on
/// whether it becomes the active account (#274). Bundled into one value — rather than two
/// parallel `captured` + `activate` params — so [`run_login`] / [`run_login_locked`] stay
/// within the 7-argument clippy bound (this repo never `#[allow]`s `too_many_arguments`),
/// mirroring the daemon's `IdleSeams` grouping. The activation verdict travels WITH the
/// harvest it applies to; [`reconcile_login`] decides it (see [`should_activate`]).
struct HarvestedLogin {
    /// The fresh credential + `oauthAccount` identity harvested in isolation (#132).
    captured: StashedAccount,
    /// Re-point the canonical item to `captured` (make it active) — `true` only when it IS
    /// the current active account, or there is no active account (bootstrap).
    activate: bool,
}

/// Reconcile a freshly-harvested login (the [`HarvestedLogin`]'s [`StashedAccount`] from the
/// isolated capture engine, #132) into the roster — the hermetic core (issue #134). Generic
/// over both keychain seams so it is unit-tested with in-memory fakes; performs NO lock, NO
/// config persistence, and NO reads of the real environment (every input is passed in).
///
/// Onboard (new account) or update-IN-PLACE (existing, matched by `account_uuid`) via
/// the SHARED [`plan_capture`]; then, mirroring the swap engine's write ordering (#6):
/// re-stash the fresh credential and — ONLY when `activate` (#274) — re-point the canonical
/// `Claude Code-credentials` item to it, then best-effort co-write the identity into
/// `~/.claude.json`. That re-point is the canonical change the running daemon's #107 path
/// un-quarantines a re-logged-in account on (there is no roster-persisted quarantine flag a
/// CLI could clear directly).
///
/// `activate` is the caller's ([`reconcile_login`]) verdict, keeping this core pure: the
/// freshly-captured account becomes active only when it IS the current active account
/// (re-auth in place) or there is no active account (bootstrap). When a DIFFERENT account is
/// active, `activate` is false and BOTH active-slot writes are skipped — the account is
/// stashed + rostered without stealing the active slot, leaving the canonical item and
/// `~/.claude.json` byte-for-byte unchanged.
async fn run_login<C, S>(
    login: HarvestedLogin,
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
    let HarvestedLogin { captured, activate } = login;

    // Preserve the operator's tunables + refresh schedule + `[login]` + `[stats]` + `[migration]`
    // settings across the reconcile, exactly like `run_capture` (#58/#105/#135/#161/#150): landing
    // a login must never reset any of them to defaults.
    let (mut roster, tunables, refresh, login, stats, migration) = match existing {
        Some(config) => (
            config.roster,
            config.tunables,
            config.refresh,
            config.login,
            config.stats,
            config.migration,
        ),
        None => (
            Vec::new(),
            Tunables::default(),
            RefreshConfig::default(),
            LoginConfig::default(),
            StatsConfig::default(),
            MigrationConfig::default(),
        ),
    };

    let (stash_name, outcome) =
        plan_capture(&mut roster, captured.oauth_account.account_uuid(), label)?;

    // Re-stash the fresh credential BEFORE re-pointing canonical (#6 ordering): a crash
    // between the two leaves a fresh, restorable stash, never a canonical pointing at an
    // unstashed credential.
    stash.write(&stash_name, &captured).await?;

    // Re-point the canonical item to the fresh credential ONLY when this login should
    // become active (#274): the freshly-logged-in account becomes the active one AND —
    // being a canonical change — is what the running daemon's #107 reconcile un-quarantines
    // the account on. When a DIFFERENT account is active (`activate` is false), the active
    // slot is preserved: BOTH the canonical write and the `~/.claude.json` co-write below
    // are skipped, so `login <other>` never steals the active slot.
    if activate {
        // Atomic (`security -U`), exactly like the swap engine's incoming write.
        store.write(&captured.credential).await?;

        // Best-effort honest-display co-write (the swap engine's step 4): a failure
        // self-heals on the daemon's next reconcile, so it never fails the login.
        let _ = write_oauth_account(claude_json, &captured.oauth_account);
    }

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
            stats,
            migration,
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
    login: HarvestedLogin,
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
    run_login(login, store, stash, existing, label, claude_json).await
}

/// Decide whether a freshly-harvested login should become the active account (#274) —
/// i.e. whether [`run_login`] re-points the canonical item. Re-point ONLY when the captured
/// account IS the current active one (`Some(active)` equal to `captured_uuid` — re-auth in
/// place) or there is no active account (`None` — canonical absent / no readable identity →
/// bootstrap). When a DIFFERENT account is active, return `false` so the active slot is
/// preserved and the login is merely stashed + rostered. Pure over the two uuids so the gate
/// is unit-tested hermetically, independent of the real `~/.claude.json`.
fn should_activate(active_uuid: Option<&str>, captured_uuid: &str) -> bool {
    match active_uuid {
        Some(active) => active == captured_uuid,
        None => true,
    }
}

/// Decide whether a landed login must EXPLICITLY signal the daemon to un-quarantine the
/// account (#276) — i.e. whether [`reconcile_login`] sends the `restored` control notify.
/// True only for a NON-ACTIVATING REVIVE: `activate` is false (the canonical item was NOT
/// re-pointed, so the daemon's #107 auto-un-quarantine won't fire for it) AND the account
/// already existed ([`LoginOutcome::Revived`]). An [`LoginOutcome::Onboarded`] account is
/// brand-new and was never quarantined, so the daemon-side `restored` would be a pure no-op;
/// and when `activate` is true the canonical re-point already un-quarantines via #107, so no
/// separate signal is needed. Pure over the two verdicts so the gate is unit-tested
/// hermetically, mirroring [`should_activate`].
fn should_signal_restored(activate: bool, outcome: LoginOutcome) -> bool {
    !activate && outcome == LoginOutcome::Revived
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

    // #274: preserve the currently-active account. Read the current canonical identity — the
    // uuid displayed in `~/.claude.json`, the honest-display pair of the canonical token (the
    // keychain blob carries no uuid, so identity lives only here) — and activate the fresh
    // login ONLY when it IS that account (re-auth in place) or there is no readable active
    // identity (bootstrap). An unreadable/absent `~/.claude.json` (not-found / no
    // `oauthAccount` / malformed) reads as "no active account" via `.ok()` → bootstrap-
    // activate, the safe default for an operator who just ran `login`. Read here (before the
    // swap lock, like `load_existing`), keeping [`run_login`] pure — the verdict is passed in.
    let active_uuid = read_oauth_account_from(&claude_json)
        .ok()
        .map(|o| o.account_uuid().to_owned());
    // Hoist the captured account's uuid before `captured` is moved into [`HarvestedLogin`]:
    // it feeds the #274 activation gate here AND names the account for the #276 restored
    // notify below.
    let captured_uuid = captured.oauth_account.account_uuid().to_owned();
    let activate = should_activate(active_uuid.as_deref(), &captured_uuid);

    let report = run_login_locked(
        Some((&swap_lock, SWAP_LOCK_MAX_WAIT)),
        HarvestedLogin { captured, activate },
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
    // #276: a non-activating REVIVE did NOT re-point the canonical item, so the daemon's
    // #107 path won't clear this account's `needs re-login` quarantine — signal it to
    // un-quarantine the revived account NOW (the reliable on-demand path, since the #106
    // sweep is starved, #260). Best-effort like the roster-reload notify above, and a
    // daemon-side no-op when it isn't quarantined (#275); skipped when `activate` (the
    // re-point already un-quarantines via #107) or on an onboard (a brand-new account was
    // never quarantined). See [`should_signal_restored`].
    if should_signal_restored(activate, report.outcome) {
        notify_daemon_restored(&captured_uuid).await;
    }
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
                poll_secs: 120,       // a non-default the operator set
                target_max_usage: 70, // a non-default reserve (default 80) the operator set
                ..Tunables::default()
            },
            refresh: RefreshConfig::default(),
            login: LoginConfig::default(),
            stats: StatsConfig::default(),
            migration: MigrationConfig::default(),
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
        // poll_secs default is 300, target_max_usage default is 80 — #398).
        assert_eq!(report.config.tunables.poll_secs, 120);
        assert_eq!(report.config.tunables.target_max_usage, 70);
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
            stats: StatsConfig::default(),
            migration: MigrationConfig::default(),
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
            stats: StatsConfig::default(),
            migration: MigrationConfig::default(),
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
            stats: StatsConfig::default(),
            migration: MigrationConfig::default(),
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
            b"[tunables]\npoll_secs = 120\nsession_trigger = 90\ntarget_max_usage = 70\n",
        )
        .unwrap();

        let loaded = load_existing_from(&path).unwrap();
        let config = loaded.expect("a tunables-only file that EXISTS is Some, not None");
        assert!(
            config.roster.is_empty(),
            "a file with no [[account]] loads with an empty roster"
        );
        // The operator's tunables survive the load — NOT reset to defaults (default
        // poll_secs is 300, default target_max_usage is 80 — #398).
        assert_eq!(config.tunables.poll_secs, 120);
        assert_eq!(config.tunables.target_max_usage, 70);
    }

    #[test]
    fn load_existing_from_reads_the_deprecated_session_floor_key() {
        // #415: an existing on-disk config.toml written with the pre-rename `session_floor`
        // key must still load through the real load_existing → load_path seam, mapping onto
        // `target_max_usage` via the serde deprecation alias. Guards the schema migration
        // (ADR-0006) at the actual file boundary, not just an in-memory parse.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(
            &path,
            b"[tunables]\npoll_secs = 120\nsession_trigger = 90\nsession_floor = 70\n",
        )
        .unwrap();

        let loaded = load_existing_from(&path).unwrap();
        let config = loaded.expect("a tunables-only file that EXISTS is Some, not None");
        // The deprecated key maps onto the new field — the operator's reserve survives.
        assert_eq!(config.tunables.target_max_usage, 70);
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
            HarvestedLogin {
                captured: stashed("u-new", b"fresh-token"),
                activate: true, // bootstrap (no active account) → re-point canonical
            },
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
            HarvestedLogin {
                captured: stashed("u-disp", b"tok"),
                activate: true, // proves the co-write WIRES when this login is active
            },
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
            stats: StatsConfig::default(),
            migration: MigrationConfig::default(),
        };

        let report = run_login(
            HarvestedLogin {
                captured: stashed("u-1", b"re-logged-in"),
                activate: true, // re-auth in place (captured == active) → re-point canonical
            },
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
            stats: StatsConfig::default(),
            migration: MigrationConfig::default(),
        };

        run_login(
            HarvestedLogin {
                captured: stashed("u-1", b"fresh"),
                activate: true, // re-auth in place → re-point canonical (the #107 un-quarantine)
            },
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
    async fn login_keeps_a_different_active_account_in_place() {
        // #274 AC: A is active and `login B` captures a uuid ≠ A → the canonical item AND
        // `~/.claude.json` are byte-for-byte unchanged (A stays active); B is still stashed
        // and written to the roster. The activation verdict (false here) is the caller's;
        // this proves `run_login` PRESERVES the active slot when it is false — skipping BOTH
        // the canonical write and the honest-display co-write.
        let store = FakeCredentialStore::empty();
        // A owns the live canonical token…
        store
            .write(&Credential::new(b"A-token".to_vec()))
            .await
            .unwrap();
        let stash = FakeAccountStash::empty();
        let dir = tempfile::tempdir().unwrap();
        // …and `~/.claude.json` displays A's identity — it must survive byte-for-byte.
        let claude_json = dir.path().join("claude.json");
        let a_json: &[u8] =
            br#"{"numStartups":3,"oauthAccount":{"accountUuid":"u-A","emailAddress":"a@example.com"}}"#;
        std::fs::write(&claude_json, a_json).unwrap();
        let existing = Config {
            roster: vec![account("u-A", "work")],
            tunables: Tunables::default(),
            refresh: RefreshConfig::default(),
            login: LoginConfig::default(),
            stats: StatsConfig::default(),
            migration: MigrationConfig::default(),
        };

        let report = run_login(
            HarvestedLogin {
                captured: stashed("u-B", b"B-token"),
                // A is active and we captured B (≠ A) → preserve the active slot
                activate: false,
            },
            &store,
            &stash,
            Some(existing),
            Some("second"),
            &claude_json,
        )
        .await
        .unwrap();

        // B is onboarded into the roster (added alongside A, not replacing it) and stashed…
        assert_eq!(report.outcome, LoginOutcome::Onboarded);
        assert_eq!(report.count, 2);
        assert!(report.config.roster.iter().any(|a| a.account_uuid == "u-B"));
        assert!(report.config.roster.iter().any(|a| a.account_uuid == "u-A"));
        assert_eq!(
            stash
                .read("Sessiometer/u-B")
                .await
                .unwrap()
                .credential
                .expose(),
            b"B-token"
        );
        // …but the active slot is preserved byte-for-byte: canonical still holds A's token…
        assert_eq!(store.read().await.unwrap().expose(), b"A-token");
        // …and `~/.claude.json` is byte-for-byte unchanged (still A, untouched).
        assert_eq!(std::fs::read(&claude_json).unwrap(), a_json);
    }

    #[test]
    fn the_active_identity_gates_the_canonical_repoint() {
        // #274 decision, over the identity seam: read the active uuid from a `~/.claude.json`
        // exactly as `reconcile_login` does, then gate — all three branches.
        let dir = tempfile::tempdir().unwrap();

        // A is the active account…
        let a_json = dir.path().join("a.json");
        std::fs::write(
            &a_json,
            br#"{"oauthAccount":{"accountUuid":"u-A","emailAddress":"a@example.com"}}"#,
        )
        .unwrap();
        let active = read_oauth_account_from(&a_json)
            .ok()
            .map(|o| o.account_uuid().to_owned());
        // …capturing a DIFFERENT account B → do NOT activate (A stays active).
        assert!(!should_activate(active.as_deref(), "u-B"));
        // …capturing A itself → activate (re-auth in place).
        assert!(should_activate(active.as_deref(), "u-A"));

        // No active account (absent `~/.claude.json`) → the read fails, `.ok()` = None →
        // bootstrap-activate.
        let absent = dir.path().join("nope.json");
        let none = read_oauth_account_from(&absent)
            .ok()
            .map(|o| o.account_uuid().to_owned());
        assert_eq!(none, None);
        assert!(should_activate(none.as_deref(), "u-X"));
    }

    #[test]
    fn only_a_non_activating_revive_signals_the_daemon_to_restore() {
        // #276: the `restored` notify fires ONLY for a non-activating revive — the exact case
        // where the canonical item was NOT re-pointed (so the daemon's #107 path won't
        // un-quarantine this account) AND the account already existed (so it may be sitting
        // `needs re-login`).
        assert!(should_signal_restored(false, LoginOutcome::Revived));
        // An ACTIVATING revive (re-auth in place / bootstrap) re-points canonical, which the
        // daemon's #107 path already un-quarantines on — so no separate signal is sent.
        assert!(!should_signal_restored(true, LoginOutcome::Revived));
        // An onboard is a brand-new account that was never quarantined, so `restored` would be
        // a pure daemon-side no-op — never sent, whether or not it activates.
        assert!(!should_signal_restored(false, LoginOutcome::Onboarded));
        assert!(!should_signal_restored(true, LoginOutcome::Onboarded));
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
            HarvestedLogin {
                captured: stashed("u-lock", b"tok"),
                activate: true, // bootstrap → re-point canonical through the locked path
            },
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

    /// Write a `~/.claude.json` whose active `oauthAccount` names `uuid` — the identity seam
    /// the capture path reads first (via [`read_oauth_account_from`]), off the real file.
    fn claude_json_with(dir: &std::path::Path, uuid: &str) -> std::path::PathBuf {
        let path = dir.join("claude.json");
        std::fs::write(
            &path,
            format!(r#"{{"oauthAccount":{{"accountUuid":"{uuid}","displayName":"ignored"}}}}"#),
        )
        .unwrap();
        path
    }

    #[tokio::test]
    async fn capture_locked_reads_both_halves_and_stashes_through_an_uncontended_lock() {
        // AC: `capture()` is refactored into a reusable `capture_locked` primitive that reads
        // identity + token and stashes under the swap lock (#64). Here we prove the locked path
        // is WIRED and, uncontended, behaves EXACTLY like a plain single-threaded capture — NO
        // behavior change (#357 AC). The serialization guarantee itself is proven by the
        // concurrency test below (and, for the swap writers, in swap.rs).
        let store = FakeCredentialStore::empty();
        store
            .write(&Credential::new(b"cap-token".to_vec()))
            .await
            .unwrap();
        let stash = FakeAccountStash::empty();
        let dir = tempfile::tempdir().unwrap();
        let lock = dir.path().join("swap.lock");
        let claude_json = claude_json_with(dir.path(), "u-cap");

        let report = capture_locked(
            Some((&lock, SWAP_LOCK_MAX_WAIT)),
            &store,
            &stash,
            &claude_json,
            None,
            Some("work"),
        )
        .await
        .unwrap();

        // Same outcome a plain capture would produce: a first capture appends one account…
        assert_eq!(report.outcome, CaptureOutcome::Captured);
        assert_eq!(report.label, "work");
        assert_eq!(report.count, 1);
        assert_eq!(report.config.roster[0].account_uuid, "u-cap");
        // …with BOTH halves stashed together under its uuid-derived service.
        let stashed = stash.read("Sessiometer/u-cap").await.unwrap();
        assert_eq!(stashed.credential.expose(), b"cap-token");
        assert_eq!(stashed.oauth_account.account_uuid(), "u-cap");
    }

    #[tokio::test]
    async fn a_swap_between_the_two_capture_reads_cannot_mis_key_the_roster() {
        // #357 regression: capture reads the active identity (`~/.claude.json`) and THEN the
        // active token (keychain) as two steps. If a daemon timer-swap lands BETWEEN them, the
        // token gets stashed under the WRONG account's identity (a mis-keyed roster row). The
        // fix holds the swap lock across BOTH reads, so a concurrent swap serializes and capture
        // always sees a CONSISTENT (identity, token) pair. Mirrors the swap-side
        // `two_real_swap_writers_on_one_item_never_leave_a_split_pair`: a fake YIELDS to widen
        // the exact window a mis-key would open, and the lock closes it (drop the lock and this
        // test mis-keys).
        use std::cell::RefCell;
        use std::rc::Rc;

        // The active account is a COUPLED (identity, token) pair a swap flips atomically:
        // account A is (u-A, A-token); account B is (u-B, B-token). A cross pair
        // (e.g. u-A + B-token) is exactly the mis-key this guards against.
        type Slot = Rc<RefCell<Option<Credential>>>;

        // Capture's token-read seam: yields FIRST, widening the window between capture's
        // (already-done) identity read and this token read — where an unlocked swap would slip
        // in. Reads the shared active-token slot; capture never writes the canonical token.
        struct ProbeStore {
            slot: Slot,
        }
        impl CredentialStore for ProbeStore {
            async fn read(&self) -> Result<Credential> {
                tokio::task::yield_now().await;
                self.slot.borrow().clone().ok_or(Error::CredentialNotFound)
            }
            async fn write(&self, _credential: &Credential) -> Result<()> {
                unreachable!("capture_locked never writes the canonical token")
            }
        }

        let dir = tempfile::tempdir().unwrap();
        let lock = dir.path().join("swap.lock");
        // The active account starts as A: identity u-A in `~/.claude.json`, token A-token.
        let claude_json = claude_json_with(dir.path(), "u-A");
        let slot: Slot = Rc::new(RefCell::new(Some(Credential::new(b"A-token".to_vec()))));

        let store = ProbeStore { slot: slot.clone() };
        let stash = FakeAccountStash::empty();
        let lw = (lock.as_path(), SWAP_LOCK_MAX_WAIT);

        // The concurrent "swap": under the SAME swap lock, atomically flip the active pair from
        // A to B (identity u-A → u-B, token A-token → B-token). Holding the lock ⇒ it runs
        // WHOLLY before or WHOLLY after capture's critical section, never between the two reads.
        let swap_json = claude_json.clone();
        let swap_slot = slot.clone();
        let contend_swap = async move {
            let _guard = SwapLock::acquire(lw.0, lw.1).await.unwrap();
            std::fs::write(
                &swap_json,
                br#"{"oauthAccount":{"accountUuid":"u-B","displayName":"ignored"}}"#,
            )
            .unwrap();
            *swap_slot.borrow_mut() = Some(Credential::new(b"B-token".to_vec()));
        };

        // capture's first `SwapLock::acquire` is synchronous-and-uncontended, so `join!`
        // deterministically lets capture take the lock before the swap is polled — the ONE
        // ordering that opens the between-reads window. The mirror ordering (swap-first) has no
        // mis-key window (capture would then read a consistent post-swap pair), so this single
        // ordering is the discriminating case, not a coverage gap.
        let (cap, ()) = tokio::join!(
            capture_locked(Some(lw), &store, &stash, &claude_json, None, None),
            contend_swap,
        );
        let report = cap.unwrap();

        // Whichever account capture landed on, the stashed token must BELONG to the stashed
        // identity — never a cross pair. A mis-key (u-A stashed with B-token) fails here.
        let uuid = report.config.roster[0].account_uuid.clone();
        let stashed = stash.read(&format!("Sessiometer/{uuid}")).await.unwrap();
        let token = stashed.credential.expose().to_vec();
        let consistent =
            (uuid == "u-A" && token == b"A-token") || (uuid == "u-B" && token == b"B-token");
        assert!(
            consistent,
            "mis-keyed roster: identity {uuid} was stashed with the other account's token \
             — a swap interleaved between capture's identity read and token read",
        );
        assert_eq!(stashed.oauth_account.account_uuid(), uuid);
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
