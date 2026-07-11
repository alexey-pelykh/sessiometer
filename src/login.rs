// Copyright (c) 2026 Oleksii PELYKH
// SPDX-License-Identifier: MIT

//! The isolated interactive-login **capture** engine (issue #132).
//!
//! Captures a **fresh** Claude Code login in isolation: run `claude /login` interactively inside
//! an ephemeral, isolated `CLAUDE_CONFIG_DIR`, then harvest the resulting credential into a
//! [`StashedAccount`] (credential + `oauthAccount` identity) — the hand-off to the stash/roster
//! write path (#134) — **without ever touching the shared `Claude Code-credentials` item** that a
//! live Claude Code session reads per-request.
//!
//! The login counterpart of the isolated-refresh engine ([`crate::refresh`]): both spawn a
//! `claude` child in an isolated dir through the shared #131 seam ([`crate::isolated_spawn`]) and
//! tear the isolation down with the same [`IsolatedSession`] RAII guard. The essential
//! differences from refresh:
//!   - **No credential seed.** Refresh seeds a back-dated copy of a stored token so CC refreshes
//!     it; a fresh *login* seeds NO token — the whole point is to capture the credential CC writes
//!     itself during the browser OAuth handoff. Seeding auth would defeat the capture.
//!   - **Onboarding seed, not `{}`.** The isolated `.claude.json` is seeded with
//!     `hasCompletedOnboarding:true` (+ `theme`) so the ONLY login is our explicit `/login` — a
//!     single operator OAuth. A fresh profile otherwise runs CC's first-start onboarding, whose
//!     own auto-login step makes the operator log in TWICE (`build/version-compat.md` #130). This
//!     differs from refresh's `{}` (`MINIMAL_CLAUDE_JSON`, #101 AC-5): headless `claude -p` skips
//!     onboarding, interactive `/login` does not.
//!   - **Inherit-terminal, never a mediated pty.** The child inherits the operator's stdin/stdout/
//!     stderr so the OAuth URL + prompts render directly to them; the engine ABORTS if stdout is
//!     not a TTY rather than allocate a pty the operator could not drive.
//!   - **Read the identity from the ISOLATED `.claude.json`.** The fresh account's uuid comes from
//!     the isolated dir's own `oauthAccount` (CC writes it there on login), not the operator's
//!     `~/.claude.json`.
//!
//! ## The cycle (one login, this order)
//!
//!   1. **TTY gate** (AC1): require a real terminal on stdout, else abort with
//!      [`Error::LoginRequiresTty`] before any filesystem / keychain work.
//!   2. **Shared-item baseline** (AC5): hash the shared `Claude Code-credentials` blob BEFORE the
//!      spawn (via the injected [`CredentialStore`] seam). `None` when there is no active shared
//!      session — nothing to protect.
//!   3. **`IsolatedSession` guard**: create an ephemeral **0700** dir at
//!      [`paths::isolated_login_dir`] (symlink-refused, owner-checked) and arm teardown over it +
//!      the isolated keychain seam.
//!   4. seed the isolated `.claude.json` (0600) with the onboarding keys — **no** token.
//!   5. spawn `claude /login` with `CLAUDE_CONFIG_DIR=<dir>`, the operator's terminal inherited,
//!      no token env, bounded by a tunable timeout (default [`DEFAULT_LOGIN_TIMEOUT`]) + an
//!      operator SIGINT ([`crate::isolated_spawn::SpawnPlan::login`]).
//!   6. **Shared-item re-check** (AC5): re-hash the shared item; a mismatch is a SAFETY ALARM
//!      ([`Error::SharedCredentialMutated`]) — the isolation premise was violated, so refuse to
//!      harvest.
//!   7. read the FRESH credential back from the **suffixed** isolated item (#100) via the metered
//!      read-back path (a `Zeroizing` [`crate::keychain::Credential`], AC3/AC4). An absent item
//!      means the operator
//!      did not finish the login (timeout / cancel) → [`LoginCapture::Incomplete`].
//!   8. read the FRESH identity from the isolated `.claude.json` `oauthAccount`; the account uuid
//!      is derived from it (AC3).
//!   9. **teardown** — delete the isolated item + remove the dir — on EVERY exit (success,
//!      incomplete, hard error, timeout, SIGINT). The RAII guard also tears down on a dropped
//!      future (cancellation).
//!
//! ## Safety invariants
//!
//!   - **shared item byte-for-byte unchanged** — its hash before == after, else the engine aborts
//!     loudly rather than hand back a harvest taken while the live session's credential moved.
//!   - **secrets never leak** — the fresh blob is a `Zeroizing` [`crate::keychain::Credential`]
//!     (wiped on drop) and the identity a `Debug`-less [`crate::claude_state::OauthAccount`]; the
//!     whole [`LoginCapture`] is un-formattable,
//!     so the only value a caller can log is the non-secret account uuid. The shown interactive
//!     channel is INHERITED (never piped), so no internal sink is ever fed the child's stdio
//!     (proven leak-free by the redaction METER test below).
//!   - **never a token-emitting subcommand** — the child is invoked as interactive `/login`, never
//!     `setup-token` or `-p` (guarded in [`crate::isolated_spawn`]), so no `sk-ant-*` token ever
//!     crosses this process's own stdio.
//!
//! ## Hermetic testability (AC7)
//!
//! The engine core ([`login_capture`]) is generic over its three seams — the shared
//! [`CredentialStore`], the isolated [`IsolatedKeychain`], and the [`ClaudeLogin`] spawner — so
//! the whole cycle is exercised with in-memory fakes (a fake `/login` that simulates "CC wrote a
//! fresh blob to the isolated item + an `oauthAccount` to the isolated `.claude.json`"), touching
//! zero real keychain / `claude` / browser. The real interactive browser flow is the #130 manual
//! gate, not a CI test.

use std::io::IsTerminal;
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::claude_state::read_oauth_account_from;
use crate::error::{Error, Result};
use crate::isolated_spawn::{ClaudeLogin, IsolatedSession, SpawnClaudeLogin};
use crate::keychain::{CredentialStore, IsolatedKeychain, RealCredentialStore};
use crate::paths;
use crate::sha256::sha256_hex;
use crate::stash::StashedAccount;

/// The default bound on one whole login capture: **180 s** (issue #132 AC6). Comfortably longer
/// than the refresh path's ~40 s `claude -p` budget because a `/login` waits on a human completing
/// a browser OAuth handoff. Tunable per call (the production entry threads it through to
/// [`crate::isolated_spawn::SpawnPlan::login`]); on expiry the child is killed and teardown runs.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) const DEFAULT_LOGIN_TIMEOUT: Duration = Duration::from_secs(180);

/// The outcome of one login-capture cycle (issue #132).
///
/// Deliberately **no `Debug`**: the `Captured` arm holds a bearer [`crate::keychain::Credential`]
/// and the email-bearing [`crate::claude_state::OauthAccount`] (both un-printable, issue #15), so
/// the whole enum is un-formattable and cannot be accidentally logged. A caller reads the
/// non-secret [`account_uuid`](Self::account_uuid) for a log line and [`into_captured`](Self::into_captured)
/// for the harvest.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) enum LoginCapture {
    /// The operator completed the login: a fresh credential + identity were harvested, ready for
    /// the stash/roster write path (#134).
    Captured(StashedAccount),
    /// The login did not complete within the timeout, or the operator cancelled it (SIGINT):
    /// nothing was harvested. The isolated item + dir were still torn down.
    Incomplete,
}

#[cfg_attr(not(test), allow(dead_code))]
impl LoginCapture {
    /// The captured account's uuid — the roster key, non-secret (exactly what `list` prints) — or
    /// `None` for an incomplete capture. The ONLY account-identifying value the engine surfaces to
    /// a log; the email + token never leave the harvested [`StashedAccount`].
    pub(crate) fn account_uuid(&self) -> Option<&str> {
        match self {
            LoginCapture::Captured(stashed) => Some(stashed.oauth_account.account_uuid()),
            LoginCapture::Incomplete => None,
        }
    }

    /// Consume the outcome into the harvested account (`Captured` only), for handing to the
    /// stash/roster write path (#134).
    pub(crate) fn into_captured(self) -> Option<StashedAccount> {
        match self {
            LoginCapture::Captured(stashed) => Some(stashed),
            LoginCapture::Incomplete => None,
        }
    }
}

/// AC1: the login capture requires a real terminal on stdout so the OAuth URL + prompts render to
/// the operator. Pure over the `is_tty` observation so the abort path is testable without a real
/// tty (the production entry passes `std::io::stdout().is_terminal()`). A non-terminal stdout — a
/// pipe, a file, a CI runner — is [`Error::LoginRequiresTty`]; the engine never allocates a
/// mediated pty the operator could not interact with.
#[cfg_attr(not(test), allow(dead_code))]
fn require_tty(is_tty: bool) -> Result<()> {
    if is_tty {
        Ok(())
    } else {
        Err(Error::LoginRequiresTty)
    }
}

/// The isolated dir's `.claude.json` — the file the onboarding seed is written to and the
/// `oauthAccount` identity is read back from after the login.
#[cfg_attr(not(test), allow(dead_code))]
fn claude_json_path(dir: &Path) -> PathBuf {
    dir.join(".claude.json")
}

/// Build the onboarding seed for the isolated `.claude.json` (issue #130, launched from `cwd`).
///
/// Carries the keys that reduce the interactive login to a SINGLE operator OAuth:
///   - `hasCompletedOnboarding:true` skips CC's first-start onboarding — whose own auto-login step
///     is the *extra* login a fresh profile otherwise forces (the double-login #130 observed) — so
///     the only login is our explicit `/login`.
///   - `theme` suppresses the first-start theme prompt.
///   - a per-cwd `projects.<cwd>.hasTrustDialogAccepted` entry suppresses the trust-folder dialog,
///     which #130 found is **CWD-scoped** (a top-level `hasTrustDialogAccepted` did not cover it);
///     the top-level flag is kept too, belt-and-suspenders. This is polish (one keystroke), not a
///     login-count fix.
///
/// **No token / `oauthAccount`** is seeded — that fresh credential is exactly what the capture
/// exists to obtain. This differs from the refresh path's `{}` (`MINIMAL_CLAUDE_JSON`, #101 AC-5),
/// which suffices because headless `claude -p` skips onboarding.
#[cfg_attr(not(test), allow(dead_code))]
fn onboarding_seed(cwd: &Path) -> Vec<u8> {
    let seed = serde_json::json!({
        "hasCompletedOnboarding": true,
        "theme": "dark",
        "hasTrustDialogAccepted": true,
        "projects": { cwd.to_string_lossy(): { "hasTrustDialogAccepted": true } },
    });
    // Serializing a value built from the `json!` macro (finite, string-keyed) cannot fail.
    let mut bytes = serde_json::to_vec(&seed).expect("serializing the onboarding seed");
    bytes.push(b'\n');
    bytes
}

/// Hash the shared `Claude Code-credentials` blob for the AC5 before/after comparison, or `None`
/// when the item is absent (no active shared session — nothing to protect). The blob is read
/// (exposing its bytes only to hash them, then dropped) and reduced to a non-secret sha256 hex
/// that is only ever compared in-process, never emitted. Any read error OTHER than "absent" is
/// propagated: if the shared item cannot be read to baseline it, the capture must not proceed (it
/// could not prove the item stays untouched).
#[cfg_attr(not(test), allow(dead_code))]
async fn shared_item_hash<C: CredentialStore>(shared_store: &C) -> Result<Option<String>> {
    match shared_store.read().await {
        Ok(credential) => Ok(Some(sha256_hex(credential.expose()))),
        Err(Error::CredentialNotFound) => Ok(None),
        Err(err) => Err(err),
    }
}

/// Steps 4–8 of the cycle, run while the [`IsolatedSession`] guard is armed so any failure here
/// still hits the explicit teardown in [`login_capture`]. Hard failures (an FS error seeding the
/// `.claude.json`, an un-spawnable binary, an unreadable shared item, a shared-item mutation)
/// return `Err`; a login the operator did not complete returns [`LoginCapture::Incomplete`].
#[cfg_attr(not(test), allow(dead_code))]
async fn run_login<C, K, L>(
    session: &IsolatedSession<K>,
    shared_store: &C,
    login: &L,
    seed_json: &[u8],
    baseline: Option<String>,
    timeout: Duration,
) -> Result<LoginCapture>
where
    C: CredentialStore,
    K: IsolatedKeychain,
    L: ClaudeLogin,
{
    // STEP 4: seed the isolated .claude.json with the onboarding keys — NOT the keychain item (the
    // spawned `claude /login` writes the fresh credential itself). A hard FS error → Err; teardown
    // still runs.
    paths::write_private_file(&claude_json_path(session.dir()), seed_json)?;

    // STEP 5: spawn `claude /login` (inherit-terminal, timeout + SIGINT). A spawn failure
    // (un-spawnable binary) is a hard Err; otherwise Ok whether the operator completed, timed out,
    // or cancelled — the read-back below classifies which.
    login.run(session.dir(), timeout).await?;

    // STEP 6 (AC5): the shared item must be byte-for-byte unchanged across the login. A mismatch is
    // a SAFETY ALARM — the isolation premise (`/login` writes ONLY the suffixed item, #130) was
    // violated, so refuse to harvest.
    if shared_item_hash(shared_store).await? != baseline {
        return Err(Error::SharedCredentialMutated);
    }

    // STEP 7 (AC3/AC4): read the FRESH credential back from the SUFFIXED isolated item (#100) via
    // the metered read-back path (a `Zeroizing` Credential). An absent item means the operator did
    // not complete the login (timeout / cancel) → Incomplete, not an error.
    let credential = match session.read_back().await {
        Ok(credential) => credential,
        Err(Error::CredentialNotFound) => return Ok(LoginCapture::Incomplete),
        Err(err) => return Err(err),
    };

    // STEP 8 (AC3): read the FRESH identity from the isolated .claude.json `oauthAccount`; the
    // account uuid is derived from it. A credential written but no identity yet (a partially-landed
    // login) is likewise an incomplete capture, not a hard error.
    let oauth_account = match read_oauth_account_from(&claude_json_path(session.dir())) {
        Ok(oauth_account) => oauth_account,
        Err(Error::OauthAccountMissing)
        | Err(Error::ClaudeStateNotFound { .. })
        | Err(Error::OauthAccountFieldMissing { .. }) => return Ok(LoginCapture::Incomplete),
        Err(err) => return Err(err),
    };

    // The harvest — handed to the stash/roster write path (#134). Never printed here.
    Ok(LoginCapture::Captured(StashedAccount {
        credential,
        oauth_account,
    }))
}

/// Capture one fresh interactive login in isolation — the shared async engine, generic over its
/// three seams (shared [`CredentialStore`], isolated [`IsolatedKeychain`], [`ClaudeLogin`] spawner)
/// so it runs hermetically with fakes.
///
/// `iso_dir` is the ephemeral isolated `CLAUDE_CONFIG_DIR`; `seed_json` the onboarding seed;
/// `timeout` the login budget; `is_tty` the injected terminal observation (AC1). Teardown of the
/// isolated item + dir runs on EVERY exit (AC6).
#[cfg_attr(not(test), allow(dead_code))]
async fn login_capture<C, K, L>(
    shared_store: &C,
    keychain: K,
    login: &L,
    iso_dir: PathBuf,
    seed_json: &[u8],
    timeout: Duration,
    is_tty: bool,
) -> Result<LoginCapture>
where
    C: CredentialStore,
    K: IsolatedKeychain,
    L: ClaudeLogin,
{
    // STEP 1 (AC1): require a real terminal BEFORE any filesystem / keychain work — abort cleanly
    // otherwise (no dir created, no spawn, no pty).
    require_tty(is_tty)?;

    // STEP 2 (AC5 baseline): hash the shared item BEFORE the spawn. `None` = no active shared
    // session; any other read error means we cannot establish the baseline, so we do not proceed.
    let baseline = shared_item_hash(shared_store).await?;

    // STEP 3: create the ephemeral isolated dir (symlink-refused, 0700, owner-checked), then ARM
    // teardown over it + the isolated keychain seam.
    paths::create_isolated_dir(&iso_dir)?;
    let session = IsolatedSession::arm(keychain, iso_dir);

    // STEPS 4–8, with teardown guaranteed afterwards regardless of outcome.
    let result = run_login(&session, shared_store, login, seed_json, baseline, timeout).await;

    // STEP 9: teardown (delete the isolated item + remove the dir), ALWAYS.
    session.teardown().await;
    result
}

/// Capture a fresh interactive login in isolation — the production entry point (issue #132).
///
/// Resolves the `claude` binary (config override → `$CLAUDE_BIN` → `$PATH`), derives the isolated
/// login dir + its suffixed keychain item (#100), builds the onboarding seed for the current
/// working dir (#130), and runs [`login_capture`] over the REAL seams: the shared
/// `Claude Code-credentials` store (baseline-hashed for AC5), the suffixed isolated item, and the
/// interactive `claude /login` spawner. `config_bin` is the operator's `[…].claude_bin` override
/// (`None` defers to `$CLAUDE_BIN`/`$PATH`); `timeout` the tunable login budget
/// ([`DEFAULT_LOGIN_TIMEOUT`] by default).
///
/// Wired into a CLI verb by the stash/roster write path (#134); until then reachable only from
/// tests (which keep its production-only callees live), hence `allow(dead_code)` off-test. The
/// real browser flow it drives is the #130 manual gate, not a CI test (AC7).
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) async fn login_account(
    config_bin: Option<&Path>,
    timeout: Duration,
) -> Result<LoginCapture> {
    // Fail fast on a non-terminal stdout (AC1) BEFORE resolving the binary or touching the
    // keychain, so the clear TTY error is never preempted by an unrelated one. The engine core
    // re-checks the same gate — that is the hermetically-tested contract.
    require_tty(std::io::stdout().is_terminal())?;

    // Reap a prior crashed login's stranded isolated item + dir (issue #133) BEFORE starting a fresh
    // capture — the login-start half of the reaper folded into the daemon startup reaper (#103).
    // `create_isolated_dir` below already clears a stale DIR, but not the orphaned keychain ITEM;
    // this sweeps both, so a fresh login never runs beside a leftover credential-bearing orphan.
    // Best-effort — never blocks the login.
    crate::refresh::reap_login_orphan().await;

    let iso_dir = paths::isolated_login_dir()?;
    let binary = paths::claude_binary_with_override(config_bin)?;
    let shared_store = RealCredentialStore::new();
    let keychain = crate::keychain::IsolatedKeychainItem::new(iso_dir.as_os_str())?;
    let login = SpawnClaudeLogin::new(binary);
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let seed = onboarding_seed(&cwd);
    let is_tty = std::io::stdout().is_terminal();

    login_capture(
        &shared_store,
        keychain,
        &login,
        iso_dir,
        &seed,
        timeout,
        is_tty,
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::cell::RefCell;
    use std::rc::Rc;

    use crate::keychain::{Credential, IsolatedKeychain};

    // A fresh Claude OAuth credential blob CC would write to the isolated item on a completed
    // login, with a chosen access token so a test can assert the harvest is byte-exact.
    fn fresh_blob(access_token: &str) -> Vec<u8> {
        format!(
            r#"{{"claudeAiOauth":{{"accessToken":"{access_token}","refreshToken":"sk-ant-ort-FRESH","expiresAt":1782777600}}}}"#
        )
        .into_bytes()
    }

    // The `oauthAccount` OBJECT (the value CC writes under `.claude.json`'s `oauthAccount` key) for
    // a given uuid + email — what the login harvests the identity from.
    fn oauth_object(uuid: &str, email: &str) -> String {
        format!(r#"{{"accountUuid":"{uuid}","emailAddress":"{email}","displayName":"ignored"}}"#)
    }

    // --- fakes: shared store, isolated item, `/login` spawner ---------------------

    /// The shared `Claude Code-credentials` store — the canonical item the capture must leave
    /// untouched. Returns [`Error::CredentialNotFound`] when absent (unlike
    /// `keychain::FakeCredentialStore`, whose empty read is `Unimplemented`), so the engine's
    /// absent-baseline branch is exercised faithfully.
    #[derive(Clone)]
    struct FakeSharedStore {
        slot: Rc<RefCell<Option<Vec<u8>>>>,
    }

    impl FakeSharedStore {
        fn empty() -> Self {
            Self {
                slot: Rc::new(RefCell::new(None)),
            }
        }
        fn seeded(blob: &[u8]) -> Self {
            Self {
                slot: Rc::new(RefCell::new(Some(blob.to_vec()))),
            }
        }
        fn snapshot(&self) -> Option<Vec<u8>> {
            self.slot.borrow().clone()
        }
    }

    impl CredentialStore for FakeSharedStore {
        async fn read(&self) -> Result<Credential> {
            self.slot
                .borrow()
                .clone()
                .map(Credential::new)
                .ok_or(Error::CredentialNotFound)
        }
        async fn write(&self, credential: &Credential) -> Result<()> {
            *self.slot.borrow_mut() = Some(credential.expose().to_vec());
            Ok(())
        }
    }

    /// The suffixed isolated keychain item CC writes the fresh login credential to. Its cell is
    /// shared with [`FakeLogin`] so the fake `/login` can populate it.
    #[derive(Clone)]
    struct FakeIsolatedKeychain {
        item: Rc<RefCell<Option<Vec<u8>>>>,
    }

    impl FakeIsolatedKeychain {
        fn empty() -> Self {
            Self {
                item: Rc::new(RefCell::new(None)),
            }
        }
    }

    impl IsolatedKeychain for FakeIsolatedKeychain {
        async fn seed(&self, blob: &[u8]) -> Result<()> {
            *self.item.borrow_mut() = Some(blob.to_vec());
            Ok(())
        }
        async fn read_back(&self) -> Result<Credential> {
            self.item
                .borrow()
                .clone()
                .map(Credential::new)
                .ok_or(Error::CredentialNotFound)
        }
        async fn delete(&self) -> Result<()> {
            *self.item.borrow_mut() = None;
            Ok(())
        }
        fn delete_blocking(&self) {
            *self.item.borrow_mut() = None;
        }
    }

    /// What the fake `claude /login` does when spawned — models CC (and a rogue CC).
    enum LoginBehavior {
        /// The operator completes the login: CC writes `fresh_blob` to the isolated item and
        /// `oauth_object` (an `oauthAccount` object) into `<dir>/.claude.json`.
        Complete {
            fresh_blob: Vec<u8>,
            oauth_object: String,
        },
        /// The login did not complete (timeout / cancel): nothing written.
        Incomplete,
        /// A completed login that ALSO (wrongly) rewrote the SHARED item — the isolation breach.
        BreachSharedItem {
            fresh_blob: Vec<u8>,
            oauth_object: String,
        },
        /// The binary could not be spawned at all.
        SpawnFails,
    }

    /// Fake `/login` spawner: writes the isolated item + isolated `.claude.json` per its
    /// [`LoginBehavior`], optionally mutating the shared store to model an isolation breach.
    struct FakeLogin {
        isolated_item: Rc<RefCell<Option<Vec<u8>>>>,
        shared: FakeSharedStore,
        behavior: LoginBehavior,
    }

    impl ClaudeLogin for FakeLogin {
        async fn run(&self, config_dir: &Path, _timeout: Duration) -> Result<()> {
            let (fresh_blob, oauth_object, breach) = match &self.behavior {
                LoginBehavior::SpawnFails => {
                    return Err(Error::Unimplemented(
                        "fake claude /login could not be spawned",
                    ))
                }
                // An incomplete login leaves the isolated item empty and writes no identity.
                LoginBehavior::Incomplete => return Ok(()),
                LoginBehavior::Complete {
                    fresh_blob,
                    oauth_object,
                } => (fresh_blob, oauth_object, false),
                LoginBehavior::BreachSharedItem {
                    fresh_blob,
                    oauth_object,
                } => (fresh_blob, oauth_object, true),
            };
            // CC writes the fresh credential to the suffixed isolated item…
            *self.isolated_item.borrow_mut() = Some(fresh_blob.clone());
            // …and records the account's identity in the isolated .claude.json (its own state
            // write on login). A real file so `read_oauth_account_from` reads it back.
            std::fs::write(
                config_dir.join(".claude.json"),
                format!(r#"{{"oauthAccount":{oauth_object}}}"#),
            )
            .unwrap();
            if breach {
                // The breach: CC wrongly rewrote the SHARED item too — the AC5 alarm.
                self.shared
                    .write(&Credential::new(b"MUTATED-BY-A-ROGUE-LOGIN".to_vec()))
                    .await
                    .unwrap();
            }
            Ok(())
        }
    }

    /// Run one capture cycle hermetically over a fresh tempdir-based isolated dir, asserting the
    /// isolated dir was torn down afterwards (AC6 teardown-always).
    async fn run_capture(
        shared: &FakeSharedStore,
        keychain: FakeIsolatedKeychain,
        login: &FakeLogin,
        is_tty: bool,
    ) -> Result<LoginCapture> {
        let tmp = tempfile::tempdir().unwrap();
        let iso_dir = tmp.path().join("login");
        let seed = onboarding_seed(Path::new("/tmp/launch-cwd"));
        let result = login_capture(
            shared,
            keychain,
            login,
            iso_dir.clone(),
            &seed,
            DEFAULT_LOGIN_TIMEOUT,
            is_tty,
        )
        .await;
        assert!(
            !iso_dir.exists(),
            "the isolated login dir must be torn down on every outcome (AC6)"
        );
        result
    }

    // --- pure helpers -------------------------------------------------------------

    #[test]
    fn require_tty_rejects_a_non_terminal() {
        assert!(require_tty(true).is_ok());
        assert!(matches!(require_tty(false), Err(Error::LoginRequiresTty)));
    }

    #[test]
    fn default_login_timeout_is_180s() {
        // AC6: the tunable timeout defaults to 180 s — comfortably over the browser-OAuth wait.
        assert_eq!(DEFAULT_LOGIN_TIMEOUT, Duration::from_secs(180));
    }

    #[test]
    fn the_onboarding_seed_carries_the_single_login_keys_and_no_auth() {
        // The seed differs from refresh's `{}`: it must carry `hasCompletedOnboarding:true` (the
        // key that removes the onboarding auto-login → a SINGLE `/login`, #130) + `theme`, plus a
        // per-cwd trust entry — and must NOT pre-seed any auth (that would defeat the capture).
        let seed = onboarding_seed(Path::new("/Users/op/work"));
        let value: serde_json::Value = serde_json::from_slice(&seed).unwrap();
        assert_eq!(value["hasCompletedOnboarding"], true);
        assert_eq!(value["theme"], "dark");
        // The CWD-scoped trust entry (#130 polish) is seeded for the launch cwd.
        assert_eq!(
            value["projects"]["/Users/op/work"]["hasTrustDialogAccepted"],
            true
        );
        // No auth / oauthAccount is seeded — the fresh credential is exactly what we capture.
        assert!(value.get("oauthAccount").is_none());
        assert!(value.get("claudeAiOauth").is_none());
        // Distinct from the refresh path's minimal `{}` seed (which omits the onboarding keys).
        assert_ne!(seed.as_slice(), b"{}\n");
    }

    // --- the engine, end to end (hermetic, fakes) ---------------------------------

    #[tokio::test]
    async fn a_completed_login_harvests_the_fresh_credential_and_identity() {
        // AC3/AC7 happy path: CC writes a fresh blob to the isolated item + an identity to the
        // isolated .claude.json; the engine harvests both, the uuid coming from the ISOLATED
        // .claude.json's oauthAccount.
        let blob = fresh_blob("sk-ant-oat-CAPTURED");
        let shared = FakeSharedStore::seeded(b"live-session-credential");
        let keychain = FakeIsolatedKeychain::empty();
        let login = FakeLogin {
            isolated_item: keychain.item.clone(),
            shared: shared.clone(),
            behavior: LoginBehavior::Complete {
                fresh_blob: blob.clone(),
                oauth_object: oauth_object("u-fresh", "op@example.com"),
            },
        };

        let capture = run_capture(&shared, keychain, &login, true).await.unwrap();

        assert_eq!(capture.account_uuid(), Some("u-fresh"));
        let stashed = capture
            .into_captured()
            .expect("a completed login is Captured");
        // The harvested credential is the fresh isolated blob, byte-exact…
        assert_eq!(stashed.credential.expose(), blob.as_slice());
        // …and the identity's uuid is derived from the isolated .claude.json's oauthAccount.
        assert_eq!(stashed.oauth_account.account_uuid(), "u-fresh");
        // AC5: the shared item is byte-for-byte unchanged across the capture.
        assert_eq!(
            shared.snapshot().as_deref(),
            Some(b"live-session-credential".as_slice())
        );
    }

    #[tokio::test]
    async fn a_capture_with_no_active_shared_session_still_succeeds() {
        // AC5 absent-baseline branch: with no shared item present (operator fully logged out), the
        // baseline is None; the login writes only the isolated item, so after==before (both
        // absent) and the harvest proceeds.
        let blob = fresh_blob("sk-ant-oat-CAPTURED");
        let shared = FakeSharedStore::empty();
        let keychain = FakeIsolatedKeychain::empty();
        let login = FakeLogin {
            isolated_item: keychain.item.clone(),
            shared: shared.clone(),
            behavior: LoginBehavior::Complete {
                fresh_blob: blob.clone(),
                oauth_object: oauth_object("u-fresh", "op@example.com"),
            },
        };

        let capture = run_capture(&shared, keychain, &login, true).await.unwrap();

        assert_eq!(capture.account_uuid(), Some("u-fresh"));
        // The shared item is still absent afterwards — the capture never created one.
        assert_eq!(shared.snapshot(), None);
    }

    #[tokio::test]
    async fn a_non_tty_stdout_aborts_before_any_isolation_work() {
        // AC1: a non-terminal stdout aborts with the clear TTY error — and BEFORE the isolated dir
        // is created or the spawner is invoked (no pty, no side effects).
        let shared = FakeSharedStore::seeded(b"live-session-credential");
        let keychain = FakeIsolatedKeychain::empty();
        let login = FakeLogin {
            isolated_item: keychain.item.clone(),
            shared: shared.clone(),
            behavior: LoginBehavior::Complete {
                fresh_blob: fresh_blob("sk-ant-oat-X"),
                oauth_object: oauth_object("u-fresh", "op@example.com"),
            },
        };

        let result = run_capture(&shared, keychain.clone(), &login, false).await;

        assert!(matches!(result, Err(Error::LoginRequiresTty)));
        // The spawner never ran: the isolated item was never written.
        assert!(keychain.item.borrow().is_none());
        // The shared item is untouched.
        assert_eq!(
            shared.snapshot().as_deref(),
            Some(b"live-session-credential".as_slice())
        );
    }

    #[tokio::test]
    async fn a_shared_item_mutation_during_login_is_a_hard_error() {
        // AC5 alarm: a login that mutates the shared `Claude Code-credentials` item breaks the
        // isolation premise → the engine refuses to harvest and surfaces the breach loudly.
        let shared = FakeSharedStore::seeded(b"live-session-credential");
        let keychain = FakeIsolatedKeychain::empty();
        let login = FakeLogin {
            isolated_item: keychain.item.clone(),
            shared: shared.clone(),
            behavior: LoginBehavior::BreachSharedItem {
                fresh_blob: fresh_blob("sk-ant-oat-CAPTURED"),
                oauth_object: oauth_object("u-fresh", "op@example.com"),
            },
        };

        let result = run_capture(&shared, keychain, &login, true).await;

        assert!(matches!(result, Err(Error::SharedCredentialMutated)));
    }

    #[tokio::test]
    async fn an_incomplete_login_yields_incomplete_and_tears_down() {
        // AC6: a login the operator did not complete (timeout / cancel — modelled as CC writing
        // nothing) yields Incomplete, not an error, and still tears down (asserted in run_capture).
        let shared = FakeSharedStore::seeded(b"live-session-credential");
        let keychain = FakeIsolatedKeychain::empty();
        let login = FakeLogin {
            isolated_item: keychain.item.clone(),
            shared: shared.clone(),
            behavior: LoginBehavior::Incomplete,
        };

        let capture = run_capture(&shared, keychain, &login, true).await.unwrap();

        assert!(matches!(capture, LoginCapture::Incomplete));
        assert_eq!(capture.account_uuid(), None);
        assert!(capture.into_captured().is_none());
    }

    #[tokio::test]
    async fn a_spawn_failure_is_a_hard_error_and_tears_down() {
        // A `claude` that could not be spawned is a hard Err; the isolated dir is still torn down
        // (asserted in run_capture).
        let shared = FakeSharedStore::seeded(b"live-session-credential");
        let keychain = FakeIsolatedKeychain::empty();
        let login = FakeLogin {
            isolated_item: keychain.item.clone(),
            shared: shared.clone(),
            behavior: LoginBehavior::SpawnFails,
        };

        let result = run_capture(&shared, keychain, &login, true).await;

        assert!(matches!(result, Err(Error::Unimplemented(_))));
    }

    // --- redaction METER (#15/AC4): a capture over a real secret leaks nothing ----

    #[tokio::test]
    async fn a_capture_over_a_secret_blob_leaks_no_secret_on_any_output_channel() {
        // Drive a full capture whose fresh credential IS the redaction fixture's real secret blob
        // (sk-ant- tokens + a distinctive email), and prove the engine's non-secret OUTPUT surface
        // — the only value a caller may log — carries none of it. The harvested StashedAccount
        // legitimately holds the secret (it is handed to #134's keychain write, never printed) and
        // is un-formattable by construction (no Debug on Credential / OauthAccount); the shown
        // interactive channel is inherited, never teed into a sink.
        let secrets = crate::redaction::meter::Secrets::meter_fixture();
        let shared = FakeSharedStore::empty();
        let keychain = FakeIsolatedKeychain::empty();
        let login = FakeLogin {
            isolated_item: keychain.item.clone(),
            shared: shared.clone(),
            behavior: LoginBehavior::Complete {
                fresh_blob: secrets.blob().to_vec(),
                // The isolated identity carries the fixture's distinctive email — which must stay
                // out of every output channel just like the tokens.
                oauth_object: oauth_object("u-meter", secrets.email()),
            },
        };

        let capture = run_capture(&shared, keychain, &login, true).await.unwrap();

        // The secret WAS captured correctly (positive: the harvest holds the fixture blob).
        let uuid = capture.account_uuid().expect("captured").to_owned();
        let stashed = capture.into_captured().unwrap();
        assert_eq!(stashed.credential.expose(), secrets.blob());

        // Channel — the engine's non-secret surface a caller logs: the account uuid + outcome
        // label. Neither may carry the fixture's tokens, blob, or email.
        crate::redaction::meter::assert_clean(&uuid, &secrets, &[]);
        crate::redaction::meter::assert_clean(&format!("captured account {uuid}"), &secrets, &[]);
    }

    // Keep the production entry (and its production-only callees — `SpawnClaudeLogin`,
    // `paths::isolated_login_dir`, the real seam construction) reachable from the test target until
    // #134 wires it to a CLI verb; the reference does not run the async body (no real keychain /
    // `claude` / browser is touched). Mirrors how #131 keeps `SpawnPlan::login` alive by building
    // it in a test.
    #[test]
    fn the_production_entry_stays_reachable() {
        let _entry = login_account;
    }
}
