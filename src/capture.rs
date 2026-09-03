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
    account_uuid_violation, Account, Config, CredentialConfig, LoginConfig, MigrationConfig,
    RefreshConfig, StatsConfig, Tunables,
};
use crate::daemon::ReloadIntent;
use crate::error::{Error, Result};
use crate::keychain::{Credential, CredentialStore, RealCredentialStore};
use crate::login::login_account;
use crate::observability::{
    Event, EventLog, LoginEventOutcome, RosterReloadOutcome, RosterReloadReason,
};
use crate::paths;
use crate::stash::{AccountStash, RealAccountStash, StashedAccount};
use crate::swap::{SwapLock, SWAP_LOCK_MAX_WAIT};
use std::io::{BufRead, IsTerminal, Write};
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
/// module docs name (#357). The roster (`config.toml`) save stays OUTSIDE the swap lock — a swap
/// never contends on `config.toml` — preserving stash-before-roster, exactly like
/// [`reconcile_login`]; it takes the dedicated config-write lock instead (issue #1445), which
/// serializes it against the OTHER config writers the swap lock was never about.
pub(crate) async fn capture(label: Option<String>) -> Result<()> {
    let existing = load_existing()?;

    // #1440 (design D-1): an ABSENT `config.toml` means either "never configured" or
    // "configuration disappeared", and this verb can only append — so resolving the
    // ambiguity to "first run" is what wrote a one-account roster over a live six-account
    // one. Refuse when durable local state says the machine was configured before.
    //
    // FIRST, ahead of `ensure_private_dir` as well as the prompt and the credential read,
    // so a refusal leaves the filesystem exactly as it found it. Only the config read
    // precedes it, because its result is this gate's own input.
    crate::witness::admit_append_only(&crate::witness::WitnessSources::real()?, existing.is_some())
        .await?;

    // Ensure the native-local support dir (0700) that houses `swap.lock` exists before
    // acquiring the lock (mirrors `reconcile_login` / `use`, #64).
    paths::ensure_private_dir(&paths::support_dir()?)?;
    let swap_lock = paths::swap_lock()?;
    let claude_json = paths::claude_json()?;

    // #447: when the operator gave no label on the command line, offer the harvested
    // email as the editable, pre-filled default at an interactive prompt. A confirmed
    // value (accepted email or a typed replacement) is operator-authored, so it may be
    // an email under the #444 provenance-scoped waiver. Any outcome that is NOT an
    // operator confirmation — non-tty (piped/scripted), no email to offer, not logged
    // in, or EOF at the prompt — leaves the label unset so the locked path below falls
    // back to the uuid-derived default: no path ever auto-commits an unconfirmed email.
    let label = label.or_else(|| prefill_label_from_identity(&claude_json));

    let report = capture_locked(
        Some((&swap_lock, SWAP_LOCK_MAX_WAIT)),
        &RealCredentialStore::new(),
        &RealAccountStash::new(),
        &claude_json,
        existing,
        label.as_deref(),
    )
    .await?;

    report.config.save().await?;
    // Tell a running daemon to pick up the new roster now (#139) — best-effort, so no
    // daemon (or a wedged one) never blocks capture; the disk write is authoritative.
    // APPEND-ONLY (#1442, R-3): `plan_capture` only updates-in-place or pushes, so the roster
    // this just saved can never be smaller than the one it read.
    notify_daemon_roster_reload(ReloadIntent::AppendOnly).await;
    println!(
        "{}",
        confirmation(report.outcome, &report.label, report.count)
    );
    Ok(())
}

/// Offer the harvested email as the pre-filled label default at an interactive prompt
/// (issue #447), returning the operator-confirmed label — or `None` to fall through to
/// the uuid-derived default ([`derive_label`]).
///
/// The email is read via a cheap, UNLOCKED identity peek used ONLY to seed the prompt.
/// It is deliberately NOT the captured identity: the authoritative identity read stays
/// under the swap lock in [`capture_locked`] (#357), which re-reads `~/.claude.json`
/// after the operator answers. The only value that could go stale between the peek and
/// that locked read is the *cosmetic* label default — never the credential↔identity
/// pairing the lock protects — and only if the operator switches the active account
/// mid-prompt; the label is re-nameable via a re-capture with an explicit label.
///
/// Best-effort by design: any peek failure (not logged in, unreadable file) yields
/// `None` and lets [`capture_locked`]'s own identity read surface the authoritative
/// error, rather than pre-empting it here. The peeked email is held in `Zeroizing`
/// (via [`OauthAccount::email`]) and dropped the instant this returns (#447 AC5).
fn prefill_label_from_identity(claude_json: &Path) -> Option<String> {
    let email = read_oauth_account_from(claude_json).ok()?.email()?;
    // A non-terminal stdout (piped / scripted) must never block on input nor commit an
    // email the operator did not confirm — fall through to the uuid-derived default.
    if !std::io::stdout().is_terminal() {
        return None;
    }
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout();
    prompt_label_default(&email, &mut stdin.lock(), &mut stdout)
        .ok()
        .flatten()
    // `email` (Zeroizing) drops here — the harvested address is wiped once the label
    // is resolved, whatever the operator chose (#447 AC5).
}

/// The pure, testable prompt core: write `Account label [<email>]: `, read one line,
/// and resolve the operator's choice (issue #447).
///
/// - an empty line (bare Enter) **accepts** the pre-filled `email` default;
/// - a non-empty line **replaces** it with the trimmed value;
/// - EOF with no input (Ctrl-D on an empty line) returns `None` — no confirmation,
///   so the caller falls back to the uuid-derived default.
///
/// Every `Some` return is therefore a value the operator actively confirmed at the
/// prompt — an operator-authored label (permitted as an email by the #444 provenance
/// seam). This function NEVER yields the email without the operator pressing Enter on
/// it, satisfying "no path auto-commits the email without the operator confirming it".
fn prompt_label_default(
    email: &str,
    input: &mut impl BufRead,
    output: &mut impl Write,
) -> std::io::Result<Option<String>> {
    write!(output, "Account label [{email}]: ")?;
    output.flush()?;
    let mut line = String::new();
    if input.read_line(&mut line)? == 0 {
        return Ok(None); // EOF (Ctrl-D) with no input: treat as no confirmation
    }
    let trimmed = line.trim();
    Ok(Some(if trimmed.is_empty() {
        email.to_owned() // bare Enter: accept the pre-filled email default
    } else {
        trimmed.to_owned() // operator typed a replacement (e.g. `work` / `EU`)
    }))
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
/// That save carries its own config-write lock (issue #1445) — see [`reconcile_login`] for why
/// outside the SWAP lock does not mean unlocked, and why the two are never nested.
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
///
/// The config is read exactly ONCE, here, and the parsed value is carried the whole way to
/// [`reconcile_login`] (issue #1440, R-5). It used to be read twice — this function kept
/// `c.login` and dropped the roster three lines apart, and the reconcile then rebuilt one
/// from `Vec::new()` in a different function, far below. A second read cannot see what
/// the first one saw, and a re-derivation from nothing is what made the fall-through
/// invisible: the run reported `(now 1 in rotation)` and looked correct.
pub(crate) async fn login(label: Option<String>) -> Result<()> {
    // The ONE config read for the whole verb. A MALFORMED config is a hard error surfaced
    // BEFORE the interactive login (never run a multi-minute login only to fail on save);
    // an ABSENT config is `None` (the first login precedes any `config.toml`).
    let existing = load_existing()?;

    // #1440 (design D-1): resolve the absent-config ambiguity BEFORE the interactive login,
    // for the same reason a malformed config is surfaced here — refusing after a
    // multi-minute login spends the operator's time to reach a verdict already fixed. A
    // refusal is a true no-op: no engine spawned, nothing harvested, nothing written.
    crate::witness::admit_append_only(&crate::witness::WitnessSources::real()?, existing.is_some())
        .await?;

    // The `[login]` settings: capture timeout + optional binary override. Read off the
    // config parsed above — cloned rather than moved, so the roster stays whole for the
    // reconcile. The override threads through the SAME resolver the refresh path uses
    // (#135 AC: no new binary-override mechanism).
    let login_cfg = existing
        .as_ref()
        .map(|c| c.login.clone())
        .unwrap_or_default();

    match login_account(login_cfg.claude_bin.as_deref(), login_cfg.timeout()).await {
        Ok(capture) => {
            // The non-secret account handle the engine surfaces (the account uuid — exactly what
            // `list` prints), or `None` for an incomplete capture. Read via the engine's own
            // accessor BEFORE `into_captured` consumes the outcome; kept as the event handle for a
            // reconcile failure (a success reports the resolved roster label instead).
            let uuid_handle = capture.account_uuid().map(str::to_owned);
            match capture.into_captured() {
                // A completed login: the fresh credential + identity were harvested.
                Some(captured) => match reconcile_login(captured, label, existing).await {
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
///
/// Both failure paths now ALSO leave a durable trace
/// ([`emit_roster_reload_not_notified`], issue #1438): a roster write that the daemon was never
/// told about is the CLI-side twin of the same blind spot the daemon handler had — the file moved
/// and the running rotation did not, with nothing recording the divergence.
///
/// `intent` (issue #1442) declares whether the calling verb can only ADD to the roster
/// ([`ReloadIntent::AppendOnly`] — `capture`, `login`) or may legitimately change its membership
/// ([`ReloadIntent::Mutating`] — `remove`, `enable` / `disable`, `import`, `config restore`). The
/// daemon's never-shrink floor partitions on it, and cannot recover it from the file: a smaller
/// roster on disk is either the operator's `remove` or the 2026-08-27 collapse, and the two are
/// identical from the reading end. Every caller must state it — the wire field is optional only so
/// a pre-#1442 CLI still parses, and an absent one is read as the REFUSING treatment (R-3a).
pub(crate) async fn notify_daemon_roster_reload(intent: ReloadIntent) {
    let socket = match paths::control_socket() {
        Ok(socket) => socket,
        Err(err) => {
            eprintln!(
                "sessiometer: roster-reload notify skipped (cannot resolve control socket): {err}"
            );
            emit_roster_reload_not_notified(RosterReloadReason::SocketUnresolved);
            return;
        }
    };
    if let Err(err) = crate::daemon::notify_roster_reload(&socket, intent).await {
        // A TIMEOUT is not a failure to deliver, and the two must not share one code (#1438). The
        // daemon's jittered start-up delay draws a uniform `[0, STARTUP_DELAY_CAP)` wait — up to
        // 30s (#76) — and its control socket is BOUND before anything accepts on it, so a roster
        // verb landing in that window connects into the listen BACKLOG, waits, and gives up at the
        // 2s `CLIENT_NOTIFY_TIMEOUT`, after which the daemon accepts the queued request and adopts
        // it. The server already anticipates this exact client: its control serve swallows the
        // `EPIPE` from acking a peer that has gone. Reporting that as `notify_failed` would write
        // a durable, FALSE "the daemon was never told" for a reload the daemon actually performs —
        // on every roster verb issued in a daemon's first half-minute.
        //
        // The durable code and the printed message are derived from ONE classification, so they
        // cannot drift apart and tell an operator two different stories: "is the daemon running?"
        // is the right question for a refused connect and the wrong one for a timeout, where the
        // answer is very likely yes and the reload very likely lands anyway.
        let (reason, note) = match &err {
            Error::Io(io) if io.kind() == std::io::ErrorKind::TimedOut => (
                RosterReloadReason::NotifyTimedOut,
                "notify timed out; a starting daemon may still adopt it",
            ),
            _ => (
                RosterReloadReason::NotifyFailed,
                "notify skipped (is the daemon running?)",
            ),
        };
        eprintln!("sessiometer: roster-reload {note}: {err}");
        emit_roster_reload_not_notified(reason);
    }
}

/// Emit the CLI-side [`Event::RosterReload`] for a roster write the running daemon was never told
/// about (issue #1438) — BEST-EFFORT, exactly like [`emit_login_event`]: the write already landed
/// on disk and stands regardless of whether this line can be appended.
///
/// The `eprintln!` beside each call site is KEPT, not replaced. The issue's "gone, not
/// supplemented" is scoped to the DAEMON handler, whose stderr has no reader because it runs in
/// the background; here the operator is standing in front of the verb they just ran, so the
/// message reaches them synchronously and the event is the DURABLE half of the same report rather
/// than a substitute for it.
///
/// Both counts are absent. The CLI cannot see the daemon's in-memory roster, and a count of what
/// it merely WROTE — with no `previous` to pair it against — cannot express the shrink this event
/// exists to make legible, so it is omitted rather than half-reported.
///
/// `reason` carries the whole difference between the three ways this can happen, and the split is
/// load-bearing rather than cosmetic. An unresolvable socket path
/// ([`RosterReloadReason::SocketUnresolved`]) is a broken environment; a failed send
/// ([`RosterReloadReason::NotifyFailed`]) is overwhelmingly just "no daemon is running", which is
/// benign; and a TIMEOUT ([`RosterReloadReason::NotifyTimedOut`]) is not evidence of either — the
/// request may already be queued on a starting daemon and may still be adopted. The outcome is
/// therefore only ever "no ack was seen": a reader chasing divergence between the file and the
/// live rotation has to consult the reason before treating any of these as a divergence at all.
fn emit_roster_reload_not_notified(reason: RosterReloadReason) {
    if let Ok(mut log) = EventLog::open() {
        let _ = log.emit(&Event::RosterReload {
            outcome: RosterReloadOutcome::NotNotified,
            previous: None,
            incoming: None,
            reason: Some(reason),
        });
    }
}

/// Best-effort notify a running daemon to un-quarantine a revived, non-activating parked
/// account (issue #276): resolve the control socket and send `restored <uuid>`, ASKING the
/// daemon to clear the account's `needs re-login` quarantine WITHOUT activating it. Called by
/// [`reconcile_login`] AFTER the roster save + `roster-reload` notify, ONLY for a
/// non-activating revive (see [`should_signal_restored`]).
///
/// The clear is REQUESTED, not promised — since issue #643 the daemon forks on the named
/// account's OWN verdict, in `reconcile_restored` (`src/daemon/refresh_fold.rs`). A non-`Dead`
/// account takes the #275 primitive (`apply_refresh_restore`), which clears the quarantine
/// directly — and is a no-op when there is none to clear. A `Dead`-latched PARKED account is
/// re-probed with an isolated refresh of its own first (`reprobe_dead_parked_credential`), and
/// `fold_recovery_outcome` folds that re-probe three ways: a LIVE outcome un-quarantines, a
/// TRANSIENT `Error` un-quarantines anyway (to `AtRisk`, preserving the #275 guarantee), and a
/// re-probe that comes back definitively `Dead` KEEPS the quarantine — never falsely cleared.
/// That re-probe fork carries a third condition: the isolated engine must be wired. `Daemon::new`
/// leaves it unset and `cli::run` attaches it unconditionally (#426 hoisted it out of the
/// `[refresh].enabled` gate), so a `Dead` PARKED account falls back to the primitive only in a
/// hermetically built daemon.
/// NO canonical write and NO active-account change on either fork.
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
    // `[stats]` settings, the `[migration]` settings AND the `[credential]` settings across a
    // capture (issue #58, extended for #105/#135/#161/#150/#878): adding an account to a config
    // that already carries custom tunables / a `[refresh]` / `[login]` / `[stats]` / `[migration]` /
    // `[credential]` block must not reset any to defaults. Destructured field-by-field with no
    // `..` rest pattern, so a future block is a COMPILE error here rather than a silent reset.
    let Config {
        mut roster,
        tunables,
        refresh,
        login,
        stats,
        migration,
        credential: credential_config,
    } = existing.unwrap_or_else(|| Config {
        roster: Vec::new(),
        tunables: Tunables::default(),
        refresh: RefreshConfig::default(),
        login: LoginConfig::default(),
        stats: StatsConfig::default(),
        migration: MigrationConfig::default(),
        credential: CredentialConfig::default(),
    });

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
            credential: credential_config,
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
    // The uuid is harvested from `~/.claude.json`, which checks only non-emptiness — so
    // this path never crossed `Config::validate` and derived a stash from whatever it
    // found. Gate it here, BEFORE the stash below and before the roster is persisted:
    // the parse path now rejects a malformed uuid (issue #1052), so capturing one would
    // otherwise write a `config.toml` that the very next load refuses, with no in-tool
    // way back. Validating on read but not on write is what makes that brick possible.
    //
    // Reported against `~/.claude.json`, NOT as an invalid config: that is where the value
    // came from, and `config.toml` may be blameless here — or, on a first capture, absent.
    if let Some(rule) = account_uuid_violation(account_uuid) {
        return Err(Error::OauthAccountFieldMalformed {
            field: "accountUuid",
            rule,
        });
    }

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

    // Preserve the operator's tunables + refresh schedule + `[login]` + `[stats]` + `[migration]` +
    // `[credential]` settings across the reconcile, exactly like `run_capture`
    // (#58/#105/#135/#161/#150/#878): landing a login must never reset any of them to defaults.
    // Destructured exactly as `run_capture` does, and for the same compile-time reason.
    //
    // The `None` arm builds a first-run config with an EMPTY roster, and it is the arm that
    // destroyed five accounts (#1440): `reconcile_login` used to re-read `config.toml` itself,
    // so a roster that had been read perfectly well by the verb arrived here as `None` and was
    // rebuilt from `Vec::new()`. Two changes make the arm honest rather than merely rarer —
    // `existing` is now the CALLER's parsed value, so there is no second read to disagree with
    // the first, and the caller has already refused an absent config that a prior-configuration
    // witness contradicts (`crate::witness`). What reaches `None` now is a machine on which
    // no prior configuration could be OBSERVED — which is a genuine first run except for the
    // false negative `PriorConfiguration::Absent` documents (a loss that also took both
    // witnesses), whose second line is the backup ring, not this arm.
    let Config {
        mut roster,
        tunables,
        refresh,
        login,
        stats,
        migration,
        credential: credential_config,
    } = existing.unwrap_or_else(|| Config {
        roster: Vec::new(),
        tunables: Tunables::default(),
        refresh: RefreshConfig::default(),
        login: LoginConfig::default(),
        stats: StatsConfig::default(),
        migration: MigrationConfig::default(),
        credential: CredentialConfig::default(),
    });

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
            credential: credential_config,
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
/// The roster (`config.toml`) write is deliberately OUTSIDE the swap lock, and still is: a
/// swap contends only on the keychain + `~/.claude.json`, never on `config.toml`, so no
/// concurrent swap can race it. Stash-before-roster (like [`capture`]): a crash after the
/// locked write but before the save leaves a fresh, restorable stash + canonical, never a
/// roster referencing an unstashed account.
///
/// OUTSIDE the swap lock is not the same as unlocked, and since issue #1445 it no longer
/// implies it. The sentence above answers only "can a concurrent SWAP race this save", and the
/// answer is still no; the pair it never addressed is two CONFIG writers racing each other —
/// this save against another CLI invocation or the daemon's `config set`. Their PUBLISHES are
/// serialized by a dedicated lock of its own, taken inside [`Config::save`] across the
/// read-modify-write `save_to` itself performs — retain into the ring, write, prune
/// ([`crate::config`]'s `write_lock`, design D-8). It does NOT make this verb's own
/// read-modify-write atomic: `existing` was parsed before an interactive login that can run for
/// minutes, and a concurrent `config set` landing in that window is overwritten here. That
/// residual is issue #1482, and widening this lock to cover the login is exactly what AC-8
/// forbids.
///
/// The two locks are never nested: `run_login_locked` has returned and released the swap lock
/// before the save below is reached — pinned by
/// `the_roster_save_is_reached_only_after_the_swap_lock_has_been_released`. That is a latency
/// property, not the anti-deadlock guarantee; see `write_lock`'s module docs for why the bounded
/// wait is what rules a deadlock out.
///
/// `existing` is the config the CALLER parsed, passed in rather than re-read here (issue
/// #1440, R-5). It used to call [`load_existing`] itself, which meant the roster this
/// function persisted was never the roster the verb's own gate had seen: two reads of one
/// file, a long way apart and separated by one interactive login, with `run_login`'s
/// `unwrap_or_else(Vec::new)` silently supplying a fresh roster if the second read came
/// back `None`. One read, carried, is what removes that gap — there is no second read left
/// to disagree with the first.
pub(crate) async fn reconcile_login(
    captured: StashedAccount,
    label: Option<String>,
    existing: Option<Config>,
) -> Result<(LoginOutcome, String, usize)> {
    // Ensure the native-local support dir (0700) that houses `swap.lock` exists before
    // acquiring the lock (mirrors `use`, #64).
    paths::ensure_private_dir(&paths::support_dir()?)?;
    let swap_lock = paths::swap_lock()?;
    let claude_json = paths::claude_json()?;

    // #274: preserve the currently-active account. Read the current canonical identity — the
    // uuid displayed in `~/.claude.json`, the honest-display pair of the canonical token (the
    // keychain blob carries no uuid, so identity lives only here) — and activate the fresh
    // login ONLY when it IS that account (re-auth in place) or there is no readable active
    // identity (bootstrap). An unreadable/absent `~/.claude.json` (not-found / no
    // `oauthAccount` / malformed) reads as "no active account" via `.ok()` → bootstrap-
    // activate, the safe default for an operator who just ran `login`. Read here, before the
    // swap lock, keeping [`run_login`] pure — the verdict is passed in. (This was "like
    // `load_existing`" until issue #1440 moved the config read to the caller; the ordering
    // it describes is unchanged, the comparison simply no longer has a sibling here.)
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

    report.config.save().await?;
    // Tell a running daemon to pick up the onboarded / relogged-in account now (#139) —
    // best-effort, the login already committed to disk.
    // APPEND-ONLY (#1442, R-3): a login onboards or re-authenticates one account and drops none.
    // This is the verb whose reload collapsed the roster on 2026-08-27, and the intent it now
    // declares is what lets the daemon refuse that reload instead of adopting it.
    notify_daemon_roster_reload(ReloadIntent::AppendOnly).await;
    // #276: a non-activating REVIVE did NOT re-point the canonical item, so the daemon's
    // #107 path won't clear this account's `needs re-login` quarantine — signal it to
    // un-quarantine the revived account NOW (the reliable on-demand path, since the #106
    // sweep is starved, #260). Best-effort like the roster-reload notify above; skipped
    // when `activate` (the re-point already un-quarantines via #107) or on an onboard (a
    // brand-new account was never quarantined). Daemon-side it forks on the named
    // account's OWN verdict, NOT on the quarantine flag (#643): a non-`Dead` account takes
    // the #275 primitive, which IS a no-op when it isn't quarantined — but a `Dead`-latched
    // PARKED account is re-probed with an isolated refresh either way (a `claude -p` spawn
    // and a durable `PollRefresh`), and that re-probe reaches the primitive on every
    // outcome except `Dead`, so a transient error un-quarantines while a confirmed-dead
    // credential KEEPS its quarantine. See [`should_signal_restored`].
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
    fn refuses_to_plan_a_capture_whose_uuid_the_parse_path_would_reject() {
        // Issue #1052. The uuid arrives from `~/.claude.json`, which checks only
        // non-emptiness, and this path does not cross `Config::validate` — so without
        // this gate `capture` would mint a stash and persist a roster that its own next
        // load refuses, bricking the config with no in-tool way back. The roster must be
        // left untouched: nothing planned, nothing to write.
        let mut roster = Vec::new();
        let err = plan_capture(&mut roster, "../x", Some("work")).unwrap_err();
        assert!(roster.is_empty(), "a rejected capture must plan nothing");
        // Reported against `~/.claude.json`, which is where the value came from — NOT as
        // `invalid config:`, which would send the operator to a blameless `config.toml`
        // (on a first capture, one that does not exist yet).
        assert!(
            matches!(
                err,
                Error::OauthAccountFieldMalformed {
                    field: "accountUuid",
                    ..
                }
            ),
            "a `~/.claude.json` fault must not be reported as an invalid config: {err}"
        );
        let rendered = err.to_string();
        assert!(
            !rendered.contains("invalid config"),
            "must not name config.toml: {rendered}"
        );
        assert!(
            rendered.contains("../x"),
            "the operator must learn WHICH value: {rendered}"
        );

        // And the round-trip the gate exists to protect: what capture DOES plan is a
        // roster the parse path accepts. Without the assertion below this test would
        // pass on a gate that merely rejected everything.
        let mut roster = Vec::new();
        plan_capture(
            &mut roster,
            "11111111-1111-1111-1111-111111111111",
            Some("w"),
        )
        .unwrap();
        let toml = format!(
            "[[account]]\naccount_uuid = \"{}\"\nlabel = \"{}\"\n",
            roster[0].account_uuid, roster[0].label
        );
        assert!(
            Config::from_toml_str(&toml).is_ok(),
            "capture must only ever plan a roster the next load can parse"
        );
    }

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

    // --- prompt_label_default (pure prefill core, #447) ---

    /// Drive the prompt with a scripted input line; return (resolved label, prompt
    /// text written). The prompt text proves the email is *offered* as the default.
    fn run_prompt(email: &str, typed: &str) -> (Option<String>, String) {
        let mut input = std::io::Cursor::new(typed.as_bytes().to_vec());
        let mut output: Vec<u8> = Vec::new();
        let resolved = prompt_label_default(email, &mut input, &mut output).unwrap();
        (resolved, String::from_utf8(output).unwrap())
    }

    #[test]
    fn prompt_offers_the_email_as_the_pre_filled_default() {
        // AC: capturing an account offers the email as an editable, pre-filled label.
        let (_resolved, shown) = run_prompt("alice@example.com", "\n");
        assert_eq!(shown, "Account label [alice@example.com]: ");
    }

    #[test]
    fn bare_enter_accepts_the_email_default() {
        // Empty line = accept the offered email → an operator-CONFIRMED (authored)
        // value, never a silent auto-commit (the operator pressed Enter on it).
        let (resolved, _) = run_prompt("alice@example.com", "\n");
        assert_eq!(resolved.as_deref(), Some("alice@example.com"));
    }

    #[test]
    fn a_typed_value_replaces_the_email_default() {
        // AC: the operator shortens it (e.g. to `work` / `EU`).
        let (resolved, _) = run_prompt("alice@example.com", "work\n");
        assert_eq!(resolved.as_deref(), Some("work"));
    }

    #[test]
    fn a_typed_value_is_trimmed() {
        let (resolved, _) = run_prompt("alice@example.com", "  EU  \n");
        assert_eq!(resolved.as_deref(), Some("EU"));
    }

    #[test]
    fn eof_with_no_input_declines_and_falls_back() {
        // Ctrl-D on an empty line = no confirmation → `None`, so the caller uses the
        // uuid-derive default. Never auto-commits the email without a confirmation.
        let (resolved, _) = run_prompt("alice@example.com", "");
        assert!(resolved.is_none());
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
                poll_secs: 120,               // a non-default the operator set
                target_max_session_usage: 70, // a non-default reserve (default 80) the operator set
                ..Tunables::default()
            },
            refresh: RefreshConfig::default(),
            login: LoginConfig::default(),
            stats: StatsConfig::default(),
            migration: MigrationConfig::default(),
            credential: CredentialConfig::default(),
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
        // poll_secs default is 300, target_max_session_usage default is 80 — #398).
        assert_eq!(report.config.tunables.poll_secs, 120);
        assert_eq!(report.config.tunables.target_max_session_usage, 70);
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
            credential: CredentialConfig::default(),
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
            credential: CredentialConfig::default(),
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
            credential: CredentialConfig::default(),
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
            b"[tunables]\npoll_secs = 120\nsession_ceiling = 90\ntarget_max_session_usage = 70\n",
        )
        .unwrap();

        let loaded = load_existing_from(&path).unwrap();
        let config = loaded.expect("a tunables-only file that EXISTS is Some, not None");
        assert!(
            config.roster.is_empty(),
            "a file with no [[account]] loads with an empty roster"
        );
        // The operator's tunables survive the load — NOT reset to defaults (default
        // poll_secs is 300, default target_max_session_usage is 80 — #398).
        assert_eq!(config.tunables.poll_secs, 120);
        assert_eq!(config.tunables.target_max_session_usage, 70);
    }

    #[test]
    fn load_existing_from_reads_the_deprecated_session_floor_key() {
        // #415: an existing on-disk config.toml written with the pre-rename `session_floor`
        // key must still load through the real load_existing → load_path seam, mapping onto
        // `target_max_session_usage` via the serde deprecation alias. Guards the schema migration
        // (ADR-0006) at the actual file boundary, not just an in-memory parse.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(
            &path,
            b"[tunables]\npoll_secs = 120\nsession_ceiling = 90\nsession_floor = 70\n",
        )
        .unwrap();

        let loaded = load_existing_from(&path).unwrap();
        let config = loaded.expect("a tunables-only file that EXISTS is Some, not None");
        // The deprecated key maps onto the new field — the operator's reserve survives.
        assert_eq!(config.tunables.target_max_session_usage, 70);
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
            credential: CredentialConfig::default(),
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
            credential: CredentialConfig::default(),
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
            credential: CredentialConfig::default(),
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

    // --- the prior-configuration witness at the append-only verbs (#1440) --------------

    #[tokio::test]
    async fn a_login_lands_on_the_roster_the_caller_parsed_rather_than_a_re_derived_one() {
        // Issue #1440 R-5 / AC: the roster `login` reads is the roster the reconcile
        // persists. Six accounts in, seven out — the incident's own transition run the right
        // way round. The unguarded shape produced ONE here, because the second read's `None`
        // fell through to `Vec::new()` and `plan_capture`'s only reachable arm on an empty
        // roster is a push.
        let store = FakeCredentialStore::empty();
        let stash = FakeAccountStash::empty();
        let dir = tempfile::tempdir().unwrap();
        let existing = Config {
            roster: (1..=6)
                .map(|n| account(&format!("u-{n}"), "held"))
                .collect(),
            tunables: Tunables::default(),
            refresh: RefreshConfig::default(),
            login: LoginConfig::default(),
            stats: StatsConfig::default(),
            migration: MigrationConfig::default(),
            credential: CredentialConfig::default(),
        };

        let report = run_login(
            HarvestedLogin {
                captured: stashed("u-7", b"seventh"),
                activate: true,
            },
            &store,
            &stash,
            Some(existing),
            None,
            &absent_claude_json(dir.path()),
        )
        .await
        .unwrap();

        assert_eq!(report.outcome, LoginOutcome::Onboarded);
        assert_eq!(report.config.roster.len(), 7);
        assert_eq!(report.count, 7);
        // Every prior account is still there, by uuid — a count alone would pass a roster
        // that replaced six entries with seven different ones.
        for n in 1..=6 {
            let uuid = format!("u-{n}");
            assert!(
                report.config.roster.iter().any(|a| a.account_uuid == uuid),
                "the reconcile dropped {uuid} from the roster it was handed"
            );
        }
    }

    /// AC-4 (issue #1445): a crash between the locked keychain write and the roster save leaves a
    /// fresh, RESTORABLE stash — the stash-before-roster ordering the config-write lock must not
    /// have disturbed.
    ///
    /// The failure is injected through the new lock rather than through a contrived I/O fault,
    /// because that is the failure mode this change ADDS. Be exact about what this observes: a
    /// held lock makes the save NOT PROCEED, and the assertion below is on the stash the
    /// non-proceeding save left behind. It is cancelled at the 200 ms timeout rather than run to
    /// the 5 s budget, so it never reaches the `ConfigWriteLockBusy` return — that return is
    /// observed through `save_to` by
    /// `config::render::tests::a_held_lock_makes_save_to_return_config_write_lock_busy`, and at
    /// the primitive by `config::write_lock::tests::a_contended_acquire_fails_closed_and_recovers_on_release`.
    ///
    /// What "restorable" means concretely: the account's credential is in the stash under its own
    /// key, so a re-run reaches the same roster row rather than a row pointing at nothing. The
    /// inverse — a roster written before the stash — would leave a row referencing an unstashed
    /// account, which is the state the ordering exists to make unreachable.
    #[tokio::test]
    async fn a_failed_roster_save_still_leaves_the_stash_restorable() {
        let store = FakeCredentialStore::empty();
        store
            .write(&Credential::new(b"fresh-token".to_vec()))
            .await
            .unwrap();
        let stash = FakeAccountStash::empty();
        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("config.toml");
        let claude_json = dir.path().join("claude.json");
        std::fs::write(
            &claude_json,
            br#"{"numStartups":1,"oauthAccount":{"accountUuid":"u-new","emailAddress":"n@example.com"}}"#,
        )
        .unwrap();

        // The keychain half runs first and completes — exactly as it does in production, where
        // `capture_locked` returns with the swap lock released and the save is still ahead.
        let report = capture_locked(None, &store, &stash, &claude_json, None, Some("newcomer"))
            .await
            .expect("the locked keychain half completes");

        // Now the crash-equivalent: the save cannot land. Another config writer holds the lock
        // for longer than the save is willing to wait.
        let held = crate::config::acquire_config_write_lock_for_test(
            &config_path,
            std::time::Duration::from_millis(50),
        )
        .await
        .expect("the stand-in config writer takes the lock");
        let saved = tokio::time::timeout(
            std::time::Duration::from_millis(200),
            report.config.save_to(&config_path),
        )
        .await;
        assert!(
            saved.is_err(),
            "the roster save must not have landed — this test's whole premise is that it did not"
        );
        drop(held);

        // The roster never landed…
        assert!(
            !config_path.exists(),
            "no roster was written, so nothing references the account yet"
        );
        // …and the stash did, which is the ordering: an inert ORPHAN stash, never a roster row
        // pointing at an account that was never stashed.
        assert_eq!(
            stash
                .read("Sessiometer/u-new")
                .await
                .expect("the stash landed before the roster and survives the failed save")
                .credential
                .expose(),
            b"fresh-token",
            "the stashed credential is intact, so the capture is restorable by re-running"
        );
    }

    /// AC-5's structural half: the roster save is REACHED only after the swap-locked section has
    /// returned and released, over ALL FOUR verbs that hold a swap lock and then save.
    ///
    /// Not the anti-deadlock guarantee — the BOUNDED wait is, and it would be even if the locks
    /// nested, since `save_to`'s critical section acquires nothing else and only one hold-and-wait
    /// direction is constructible. What non-nesting buys is that a contended config write cannot
    /// extend a swap-lock hold by `CONFIG_WRITE_LOCK_MAX_WAIT`, and that the invariant survives
    /// issue #257 replacing the bounded `flock` poll with a possibly-unbounded `File::try_lock`.
    ///
    /// All four are covered because the claim in `write_lock`'s module docs is universal. The two
    /// outside this file are the ones a reordering would NOT make locally obvious: each holds its
    /// swap lock inside a CALLED function (`capture_locked`, `apply_import`), so the guard is not
    /// visible at the save. This reads the shipped source because the property is about the ORDER
    /// of two calls in a function body, which no runtime assertion can observe.
    #[test]
    fn the_roster_save_is_reached_only_after_the_swap_lock_has_been_released() {
        for (file, verb, locked_call, save_call) in [
            (
                "src/capture.rs",
                "pub(crate) async fn capture",
                "capture_locked(",
                "config.save()",
            ),
            (
                "src/capture.rs",
                "pub(crate) async fn reconcile_login",
                "run_login_locked(",
                "config.save()",
            ),
            (
                "src/daemon/commands.rs",
                "pub(super) async fn perform_socket_capture",
                "capture_locked(",
                "config.save_to(",
            ),
            (
                "src/cli.rs",
                "async fn import",
                "apply_import(",
                "config.save()",
            ),
        ] {
            let source = source_above_the_tests(file);
            let body = fn_body_in(&source, verb, file);
            let locked = body.find(locked_call).unwrap_or_else(|| {
                panic!(
                    "`{verb}` in {file} no longer calls `{locked_call}` — this gate's anchor has \
                     gone stale"
                )
            });
            let save = body.find(save_call).unwrap_or_else(|| {
                panic!(
                    "`{verb}` in {file} no longer calls `{save_call}` — this gate's anchor has \
                     gone stale"
                )
            });
            assert!(
                locked < save,
                "`{verb}` in {file} saves the roster BEFORE `{locked_call}` returns, so the \
                 config-write lock would be taken inside the swap lock — extending a swap-lock \
                 hold by the config-write budget, and a reordering of stash-before-roster besides"
            );
        }
    }

    #[test]
    fn the_login_reconcile_never_re_reads_the_config_it_was_handed() {
        // Issue #1440 AC: the roster parsed by the verb reaches persistence with NO
        // re-derivation. `reconcile_login` used to call `load_existing` itself, which is the
        // re-read this forbids — far below, and one multi-minute interactive login after the
        // verb's own read, with nothing reconciling the two.
        //
        // The parameter makes the RIGHT roster available; only this keeps a second read from
        // being added back beside it, which would restore the divergence while the signature
        // still looked correct.
        let source = capture_source_above_the_tests();
        let body = fn_body(&source, "pub(crate) async fn reconcile_login");
        assert!(
            !body.contains("load_existing"),
            "`reconcile_login` re-reads the config instead of using the one it was handed:\n{body}"
        );
        // Canary: the body was actually extracted, so the absence above is evidence.
        assert!(body.contains("run_login_locked"));
    }

    #[test]
    fn both_append_only_verbs_consult_the_witness_before_they_can_write() {
        /// The gate call, verbatim. Reformatting is free (whitespace is collapsed out of
        /// the tail check and `cargo fmt` owns the line breaks); changing the ARGUMENT or
        /// the propagation is not, which is the point.
        const GATE_CALL: &str = "crate::witness::admit_append_only(&crate::witness::WitnessSources::real()?, existing.is_some())";
        /// What must follow it: awaited, and the verdict propagated.
        const GATE_TAIL: &str = ".await?;";

        // Issue #1440 AC: a refusal writes NOTHING. That is a property of WHERE the gate
        // sits, not of what it returns — a correct verdict reached after the stash has landed
        // (or after a multi-minute interactive login has run) refuses far too late.
        //
        // Each verb's first irreversible or operator-visible step is named below, and the
        // gate has to precede all of them. Moving the gate down is the regression; so is
        // deleting it, which the `expect` on the gate's own position catches.
        let source = capture_source_above_the_tests();
        for (verb, first_effects) in [
            (
                "pub(crate) async fn capture",
                // The support-dir creation (the first filesystem MUTATION, and the reason
                // a refusal leaves the tree untouched), the label prompt (an
                // operator-visible step), the locked identity + token read and stash, the
                // roster save, and the daemon notify.
                &[
                    "ensure_private_dir",
                    "prefill_label_from_identity",
                    "capture_locked(",
                    "report.config.save()",
                    "notify_daemon_roster_reload()",
                ][..],
            ),
            (
                "pub(crate) async fn login",
                // The interactive login engine, and the reconcile that stashes and saves.
                &["login_account(", "reconcile_login("][..],
            ),
        ] {
            let body = fn_body(&source, verb);
            // The whole call, not the bare name. Three regressions ride on this being the
            // anchor: passing a constant instead of `existing.is_some()` (a gate that can
            // never refuse), dropping the `?` so the refusal is computed and discarded, and
            // deleting the call while leaving prose that names it. A bare-token search
            // admits all three.
            let gate = body.find(GATE_CALL).unwrap_or_else(|| {
                panic!("`{verb}` does not consult the prior-configuration witness as `{GATE_CALL}`:\n{body}")
            });
            let tail: String = body[gate + GATE_CALL.len()..]
                .chars()
                .filter(|c| !c.is_whitespace())
                .take(GATE_TAIL.len())
                .collect();
            assert_eq!(
                tail, GATE_TAIL,
                "`{verb}` does not propagate the witness verdict — the refusal is computed and dropped"
            );
            for effect in first_effects {
                let at = body.find(effect).unwrap_or_else(|| {
                    panic!("`{verb}` no longer contains `{effect}` — this gate's anchor set has gone stale and is no longer measuring the order it claims to")
                });
                assert!(
                    gate < at,
                    "`{verb}` reaches `{effect}` before consulting the witness, so a refusal is not a no-op"
                );
            }
        }
    }

    /// This file's source above its test block — the subject of the two structural
    /// assertions above.
    ///
    /// Cut at a column-0 `#[cfg(test)]`, which in this file is the test module and nothing
    /// else. Both callers canary what they extracted, so a boundary that moved up shows as a
    /// failure rather than as a green run over an empty subject.
    fn capture_source_above_the_tests() -> String {
        source_above_the_tests("src/capture.rs")
    }

    /// The production half of `file` — everything above its `#[cfg(test)] mod tests`.
    fn source_above_the_tests(file: &str) -> String {
        let text = std::fs::read_to_string(file).unwrap_or_else(|_| panic!("cannot read {file}"));
        let above = text
            .split("\n#[cfg(test)]\nmod tests")
            .next()
            .expect("split always yields a first element")
            .to_owned();
        assert!(
            above.len() < text.len(),
            "{file} has no `#[cfg(test)] mod tests` at column 0 — this gate would grade the \
             test module as production source"
        );
        above
    }

    /// The CODE of `source` from the line opening `signature` to the next column-0 `}` —
    /// one function body, brace-delimited by the file's own `cargo fmt`-guaranteed layout,
    /// with whole-line comments removed.
    ///
    /// Deliberately not a brace counter: `rustfmt` puts a top-level item's closing brace at
    /// column 0 and nothing else in this file's non-test region has one, so the simple rule
    /// is exact here and fails loudly (an empty or run-on body) rather than subtly if that
    /// ever stops holding — which is what the callers' canaries check.
    ///
    /// The comment strip is what makes both callers measure the thing they claim to. Both
    /// ask about CALLS — does this function re-read the config, does that one reach a write
    /// before the gate — and this file's comments name those very functions, at length and
    /// on purpose. Without the strip, a body's prose about `load_existing` reads as a call
    /// to it, and a paragraph mentioning `capture_locked` shifts a position the ordering
    /// assertion compares.
    ///
    /// Trailing comments are stripped as well as whole-line ones. A trailing comment cannot
    /// precede code on its own line, but it can still sit on an EARLIER line than the code
    /// it names — which is exactly a false first occurrence — so stripping only whole lines
    /// would leave the ordering assertion satisfiable by a comment. A `//` inside a string
    /// literal is left alone (detected by an odd quote count to its left), so the strip
    /// cannot eat code.
    fn fn_body(source: &str, signature: &str) -> String {
        fn_body_in(source, signature, "src/capture.rs")
    }

    /// As [`fn_body`], but for any `origin` file and for a signature at ANY indentation.
    ///
    /// Delimits on a closing brace at the signature's OWN column rather than at column 0, which
    /// `cargo fmt` guarantees and which a method inside an `impl` block needs — a column-0
    /// delimiter would run such a body to the end of the whole `impl`, and an ordering assertion
    /// over that span could be satisfied by two calls in unrelated methods.
    fn fn_body_in(source: &str, signature: &str, origin: &str) -> String {
        let from = source
            .find(signature)
            .unwrap_or_else(|| panic!("`{signature}` is not in {origin}"));
        let line_start = source[..from].rfind('\n').map_or(0, |at| at + 1);
        let closer = format!("\n{}}}\n", &source[line_start..from]);
        let rest = &source[from..];
        let end = rest.find(&closer).unwrap_or_else(|| {
            panic!("`{signature}` in {origin} has no closing brace at its own indentation")
        });
        rest[..end]
            .lines()
            .filter(|line| !line.trim_start().starts_with("//"))
            .map(strip_trailing_comment)
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// `line` up to a `//` that is not inside a string literal.
    ///
    /// "Not inside a string literal" is decided by an even count of unescaped `"` to the
    /// left, which is exact for a single line of Rust that this file's `cargo fmt` layout
    /// produces (no raw strings spanning a `//`, no char literal holding a lone quote).
    fn strip_trailing_comment(line: &str) -> &str {
        let bytes = line.as_bytes();
        let mut in_string = false;
        let mut i = 0usize;
        while i < bytes.len() {
            match bytes[i] {
                b'\\' => i += 1,
                b'"' => in_string = !in_string,
                b'/' if !in_string && bytes.get(i + 1) == Some(&b'/') => return &line[..i],
                _ => {}
            }
            i += 1;
        }
        line
    }
}
