// Copyright (c) 2026 Oleksii PELYKH
// SPDX-License-Identifier: MIT

//! The shared isolated-`CLAUDE_CONFIG_DIR` spawn scaffold (issue #131).
//!
//! One parametrized seam for spawning a `claude` child in an ephemeral, isolated
//! `CLAUDE_CONFIG_DIR`, extracted from the refresh engine ([`crate::refresh`], #102) so a
//! second caller — an interactive-login capture engine (a later issue) — reuses it rather than
//! forking a second copy. A fork would risk silent drift: a dropped [`SPAWN_ENV_REMOVE`] entry
//! or a diverged [`IsolatedSession`] teardown is an isolation / secret-handling regression no
//! single test would catch.
//!
//! The seam owns three of the four scaffold pieces the issue names:
//!   - the child credential / config-override env **scrub** ([`SPAWN_ENV_REMOVE`]), applied in
//!     the single site [`SpawnPlan::build_command`];
//!   - the [`IsolatedSession`] RAII **create/teardown** guard; and
//!   - a reference to the #100 **suffixed-item resolution** ([`crate::keychain::IsolatedKeychainItem`]),
//!     which already lives behind its own single seam in [`crate::keychain`] and is only *used*
//!     here.
//!
//! (The fourth scaffold piece, the startup orphan-reaper #103 — extended to the login isolation
//! root in #133, both halves the crash-path counterpart of [`IsolatedSession`]'s graceful teardown —
//! stays with its daemon-start caller in [`crate::refresh`], sharing one reap core across both.)
//!
//! [`SpawnPlan`] parametrizes the child by the three axes that differ between callers: **argv**
//! (`-p <benign>` vs `/login`), **stdio** ([`Stdio3`] — nulled vs inherit-terminal), and **run
//! bound** ([`RunBound`] — whether an extra shutdown/cancel arm is wired beyond the kill-timeout).
//! The refresh path ([`SpawnPlan::refresh`]) is the only wired caller today; the login
//! parametrization ([`SpawnPlan::login`]) is constructible — the both-parametrizations scrub test
//! builds it — but its production caller lands in a later issue.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use tokio::process::Command;

use crate::error::Result;
use crate::keychain::{Credential, IsolatedKeychain};

/// The benign prompt handed to `claude -p` on the refresh path: it exists only to make CC
/// start up, read the seeded credential, and refresh it — the model output is discarded.
const BENIGN_PROMPT: &str = "say pong";

/// How long to let a timeout-bounded child run before killing it. Comfortably exceeds a
/// cold start + one on-demand token refresh; on timeout the child is killed and the caller
/// classifies whatever state it left (a refresh may already have landed).
const SPAWN_TIMEOUT: Duration = Duration::from_secs(40);

/// Env vars **unset** on every isolated spawn so the child `claude` acts on the ISOLATED item
/// and nothing else — the security-critical scrub (issue #102 step 4). Each entry is a
/// credential source or a config-dir override that, if inherited, would defeat the isolation:
///   - `CLAUDE_CODE_OAUTH_TOKEN` / `ANTHROPIC_API_KEY` — an ambient bearer in the daemon's env:
///     CC would authenticate with it instead of reading the seeded item, so the refresh would
///     silently no-op.
///   - `CLAUDE_SECURESTORAGE_CONFIG_DIR` — takes PRECEDENCE over `CLAUDE_CONFIG_DIR` in CC's
///     keychain service-name derivation (#100, [`crate::keychain`]), so an inherited one would
///     mis-target the read away from the item we seeded.
///
/// Kept as a named list (not inline `env_remove` calls), applied by [`SpawnPlan::build_command`],
/// so the `both_parametrizations_apply_the_full_scrub_set` test can assert the set is applied for
/// EVERY parametrization — a dropped entry is a silent isolation regression on whichever caller
/// drops it.
pub(crate) const SPAWN_ENV_REMOVE: &[&str] = &[
    "CLAUDE_CODE_OAUTH_TOKEN",
    "ANTHROPIC_API_KEY",
    "CLAUDE_SECURESTORAGE_CONFIG_DIR",
];

/// Retries on the isolated item's teardown delete (a transient locked keychain may clear);
/// the final attempt falls back to the synchronous best-effort delete.
const TEARDOWN_DELETE_RETRIES: u32 = 3;
/// Wait between teardown delete retries.
const TEARDOWN_DELETE_RETRY_WAIT: Duration = Duration::from_millis(100);

/// The spawned child's stdio disposition (issue #131 AC1) — how the refresh and login paths
/// differ on the child's streams.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Stdio3 {
    /// All three streams to `/dev/null` — the refresh path is headless, its output discarded.
    Null,
    /// Inherit the parent terminal — the interactive `/login` path needs the user's TTY.
    #[cfg_attr(not(test), allow(dead_code))]
    InheritTerminal,
}

/// How the spawned child's runtime is bounded (issue #131 AC1) — "whether an extra
/// shutdown/cancel arm is wired" beyond the kill-timeout.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RunBound {
    /// Kill the child after a fixed timeout, then return `Ok` (the caller's read-back decides
    /// the outcome). The refresh path's only arm.
    Timeout(Duration),
    /// Bound the runtime AND wire an extra cancel arm — the interactive-login shape, where a
    /// `/login` session runs until the user finishes or a daemon shutdown cancels it. The cancel
    /// signal is wired by the login caller (a later issue); until then this arm is an honest
    /// kill-timeout, identical to [`Timeout`](RunBound::Timeout), so the shape is representable
    /// without pretending a cancel capability that does not yet exist.
    #[cfg_attr(not(test), allow(dead_code))]
    TimeoutOrCancel { timeout: Duration },
}

/// The parametrized isolated-spawn plan (issue #131): the single owner of the (argv, stdio,
/// run-bound) axes and the single site the [`SPAWN_ENV_REMOVE`] scrub is applied. Both the
/// refresh path and the future login path build their `claude` child through this one seam.
pub(crate) struct SpawnPlan {
    /// Extra argv after the binary: `["-p", "<benign>"]` (refresh) vs `["/login"]` (login).
    argv: &'static [&'static str],
    /// The child's stdio disposition.
    stdio: Stdio3,
    /// How the child's runtime is bounded.
    bound: RunBound,
}

impl SpawnPlan {
    /// The refresh parametrization: `claude -p <benign>`, all stdio nulled, killed after
    /// [`SPAWN_TIMEOUT`]. Byte-for-byte equivalent to the pre-#131 inline spawn (guarded by the
    /// `refresh_plan_builds_the_legacy_command` test).
    pub(crate) fn refresh() -> Self {
        Self {
            argv: &["-p", BENIGN_PROMPT],
            stdio: Stdio3::Null,
            bound: RunBound::Timeout(SPAWN_TIMEOUT),
        }
    }

    /// The interactive-login parametrization: `claude /login`, inherit-terminal stdio, and a
    /// `timeout`-bounded run with an extra SIGINT cancel arm ([`RunBound::TimeoutOrCancel`],
    /// wired in [`run`](Self::run)). The `timeout` is the caller's tunable login budget (the
    /// login-capture engine defaults it to 180 s, #132) — comfortably longer than the refresh
    /// path's fixed [`SPAWN_TIMEOUT`] because a `/login` waits on a human completing a browser
    /// OAuth handoff, not a headless token refresh. Wired to its production caller (the
    /// login-capture engine) via [`SpawnClaudeLogin`]; the both-parametrizations scrub test also
    /// builds it to prove the [`SPAWN_ENV_REMOVE`] scrub applies here too.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn login(timeout: Duration) -> Self {
        Self {
            argv: &["/login"],
            stdio: Stdio3::InheritTerminal,
            bound: RunBound::TimeoutOrCancel { timeout },
        }
    }

    /// **The single place the scrub is applied.** Build the `claude` command for `binary`
    /// pointed at the isolated `config_dir`: the parametrized argv, then `CLAUDE_CONFIG_DIR`, the
    /// `DISABLE_*` headless guards (keep a headless run from auto-updating / phoning home
    /// mid-cycle), the parametrized stdio, and finally the [`SPAWN_ENV_REMOVE`] scrub — applied
    /// LAST so no earlier `.env` can resurrect a scrubbed var.
    pub(crate) fn build_command(&self, binary: &Path, config_dir: &OsStr) -> Command {
        let mut command = Command::new(binary);
        command
            .args(self.argv)
            .env("CLAUDE_CONFIG_DIR", config_dir)
            .env("DISABLE_AUTOUPDATER", "1")
            .env("DISABLE_TELEMETRY", "1")
            .env("DISABLE_ERROR_REPORTING", "1")
            .env("DISABLE_BUG_COMMAND", "1");
        match self.stdio {
            Stdio3::Null => {
                command
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null());
            }
            Stdio3::InheritTerminal => {
                command
                    .stdin(Stdio::inherit())
                    .stdout(Stdio::inherit())
                    .stderr(Stdio::inherit());
            }
        }
        // The security-critical scrub, applied LAST (see [`SPAWN_ENV_REMOVE`]).
        for var in SPAWN_ENV_REMOVE {
            command.env_remove(var);
        }
        command
    }

    /// Build, spawn, and enforce this plan's [`RunBound`]. `Ok` whether the child exited cleanly
    /// or was killed on the bound (the caller's read-back classifies the result); `Err` only if
    /// the child could not be spawned at all.
    pub(crate) async fn run(&self, binary: &Path, config_dir: &OsStr) -> Result<()> {
        let mut child = self.build_command(binary, config_dir).spawn()?;
        match self.bound {
            // The refresh arm: bound by a kill-timeout only. On timeout, kill and proceed — the
            // read-back decides the outcome (a refresh may already have landed before the kill).
            RunBound::Timeout(timeout) => {
                if tokio::time::timeout(timeout, child.wait()).await.is_err() {
                    let _ = child.kill().await;
                }
            }
            // The interactive-login arm ([`RunBound::TimeoutOrCancel`], #132): race the child
            // against the timeout AND an operator SIGINT (Ctrl-C). The child is inherit-terminal,
            // so a tty Ctrl-C reaches it directly and it usually exits on its own; the explicit
            // `ctrl_c` arm still (a) overrides the default SIGINT-terminates-*this*-process
            // disposition — so the engine's teardown (isolated item + dir) is never skipped — and
            // (b) guarantees we regain control and kill the child even if it ignores the signal.
            // On either bound the child is killed; `Ok` always — the caller's read-back classifies
            // whether a fresh credential landed (login completed) or not (timeout / cancel).
            RunBound::TimeoutOrCancel { timeout } => {
                tokio::select! {
                    res = child.wait() => {
                        let _ = res;
                    }
                    _ = tokio::time::sleep(timeout) => {
                        let _ = child.kill().await;
                    }
                    _ = tokio::signal::ctrl_c() => {
                        let _ = child.kill().await;
                    }
                }
            }
        }
        Ok(())
    }
}

/// Seam: spawns the `claude` child that performs its work in the isolated config dir (issue
/// #102 step 4 for the refresh path). The real impl drives the `claude` binary through
/// [`SpawnPlan`]; the test impl simulates CC by mutating the (fake) isolated item.
#[allow(dead_code)]
pub(crate) trait ClaudeRefresh {
    /// Run the isolated-refresh spawn (`claude -p <benign>`) with `CLAUDE_CONFIG_DIR=config_dir`,
    /// no token env, all stdio nulled, killed after [`SPAWN_TIMEOUT`]. `Ok` whether it exited
    /// cleanly or was killed (the read-back classifies the result); `Err` only if it could not be
    /// spawned at all.
    async fn run(&self, config_dir: &Path) -> Result<()>;
}

/// Production spawner: the pinned `claude` binary (the engine pins the binary it spawns, per
/// #101 provenance — a wrapper may exec a patched copy). A thin adapter over
/// [`SpawnPlan::refresh`].
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) struct SpawnClaude {
    binary: PathBuf,
}

#[cfg_attr(not(test), allow(dead_code))]
impl SpawnClaude {
    pub(crate) fn new(binary: PathBuf) -> Self {
        Self { binary }
    }
}

impl ClaudeRefresh for SpawnClaude {
    async fn run(&self, config_dir: &Path) -> Result<()> {
        SpawnPlan::refresh()
            .run(&self.binary, config_dir.as_os_str())
            .await
    }
}

/// Seam: spawns the interactive `claude /login` child for the login-capture engine (issue
/// #132) — the login counterpart of [`ClaudeRefresh`]. The real impl drives the `claude` binary
/// through [`SpawnPlan::login`] on the operator's inherited terminal; the test impl simulates a
/// completed login by writing a fresh credential to the (fake) isolated item + an `oauthAccount`
/// into the isolated `.claude.json`, so the engine is exercised hermetically (no real keychain /
/// `claude` / browser).
#[allow(dead_code)]
pub(crate) trait ClaudeLogin {
    /// Run the isolated interactive-login spawn (`claude /login`) with
    /// `CLAUDE_CONFIG_DIR=config_dir`, no token env, the operator's terminal inherited, bounded by
    /// `timeout` plus an operator SIGINT. `Ok` whether the operator completed the login, let it
    /// time out, or cancelled it — the caller's read-back of the isolated item classifies which;
    /// `Err` only if the child could not be spawned at all.
    async fn run(&self, config_dir: &Path, timeout: Duration) -> Result<()>;
}

/// Production login spawner: the pinned `claude` binary (the engine pins the binary it spawns,
/// per #101 provenance — a wrapper may exec a patched copy). A thin adapter over
/// [`SpawnPlan::login`]. Wired to the login-capture engine's production entry (a later issue),
/// hence `allow(dead_code)` off-test.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) struct SpawnClaudeLogin {
    binary: PathBuf,
}

#[cfg_attr(not(test), allow(dead_code))]
impl SpawnClaudeLogin {
    pub(crate) fn new(binary: PathBuf) -> Self {
        Self { binary }
    }
}

impl ClaudeLogin for SpawnClaudeLogin {
    async fn run(&self, config_dir: &Path, timeout: Duration) -> Result<()> {
        SpawnPlan::login(timeout)
            .run(&self.binary, config_dir.as_os_str())
            .await
    }
}

/// RAII guard for one isolated spawn session (issue #102 steps 2 & 8, shared per #131): owns the
/// ephemeral isolated directory and the isolated keychain seam, and tears BOTH down.
///
/// The happy path calls [`teardown`](Self::teardown) (async — the item delete is retried, then
/// the dir removed) which DISARMS the guard; if instead the caller returns early (a hard error, a
/// panic, or a timer-kill before teardown), `Drop` runs a best-effort SYNCHRONOUS teardown.
/// Either way the isolated item and dir never outlive the session.
pub(crate) struct IsolatedSession<K: IsolatedKeychain> {
    keychain: K,
    dir: PathBuf,
    armed: bool,
}

impl<K: IsolatedKeychain> IsolatedSession<K> {
    /// Arm a session over an ALREADY-CREATED isolated `dir` and its keychain seam.
    pub(crate) fn arm(keychain: K, dir: PathBuf) -> Self {
        Self {
            keychain,
            dir,
            armed: true,
        }
    }

    /// Seed the isolated keychain item (delegates to the owned seam).
    pub(crate) async fn seed(&self, blob: &[u8]) -> Result<()> {
        self.keychain.seed(blob).await
    }

    /// Read the (CC-refreshed) blob back (delegates to the owned seam).
    pub(crate) async fn read_back(&self) -> Result<Credential> {
        self.keychain.read_back().await
    }

    /// The isolated directory (the spawned `claude`'s `CLAUDE_CONFIG_DIR`).
    pub(crate) fn dir(&self) -> &Path {
        &self.dir
    }

    /// Explicit async teardown (the happy path): delete the isolated item (retried, falling back
    /// to the synchronous delete), remove the dir, then DISARM so `Drop` is a no-op. Best-effort
    /// — teardown never fails the caller's result.
    ///
    /// The guard stays ARMED until the deletes complete, so if this future is dropped (cancelled
    /// — e.g. a daemon shutdown aborting the task) mid-teardown, `Drop` still runs the synchronous
    /// cleanup rather than leaking the secret-bearing isolated item and its dir. Both deletes are
    /// idempotent (a not-found item / absent dir is success), so a partial async teardown followed
    /// by the sync `Drop` cleanup is fine.
    pub(crate) async fn teardown(mut self) {
        let mut deleted = false;
        for _ in 0..TEARDOWN_DELETE_RETRIES {
            if self.keychain.delete().await.is_ok() {
                deleted = true;
                break;
            }
            tokio::time::sleep(TEARDOWN_DELETE_RETRY_WAIT).await;
        }
        if !deleted {
            self.keychain.delete_blocking();
        }
        let _ = std::fs::remove_dir_all(&self.dir);
        // Disarm LAST: only once cleanup has actually run does `Drop` become a no-op.
        self.armed = false;
    }
}

impl<K: IsolatedKeychain> Drop for IsolatedSession<K> {
    fn drop(&mut self) {
        if self.armed {
            // Synchronous best-effort (Drop cannot await): delete the item, remove the dir.
            // Errors are swallowed — there is no channel to surface them.
            self.keychain.delete_blocking();
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::cell::RefCell;
    use std::collections::{BTreeMap, BTreeSet};
    use std::ffi::OsString;
    use std::rc::Rc;

    use crate::error::Error;

    /// The full env map (`key -> Some(value)` set, `key -> None` removed) of a plan's built
    /// command, order-independent (`std::process::Command` stores env in a `BTreeMap`, so
    /// `get_envs()` yields sorted, not insertion, order — an ordered assertion would be wrong).
    fn env_map(plan: &SpawnPlan) -> BTreeMap<OsString, Option<OsString>> {
        let command = plan.build_command(Path::new("/nonexistent/claude"), OsStr::new("/tmp/iso"));
        command
            .as_std()
            .get_envs()
            .map(|(k, v)| (k.to_os_string(), v.map(|v| v.to_os_string())))
            .collect()
    }

    /// The set of vars a plan's built command REMOVES (`env_remove`'d → value `None`).
    fn scrubbed_vars(plan: &SpawnPlan) -> BTreeSet<OsString> {
        env_map(plan)
            .into_iter()
            .filter_map(|(k, v)| v.is_none().then_some(k))
            .collect()
    }

    /// AC3 (issue #131): the security-critical env scrub is applied to the built command for BOTH
    /// parametrizations — a caller silently dropping a scrub entry (an isolation regression) fails
    /// here regardless of which path drops it.
    #[test]
    fn both_parametrizations_apply_the_full_scrub_set() {
        let expected: BTreeSet<OsString> = SPAWN_ENV_REMOVE
            .iter()
            .map(|s| OsString::from(*s))
            .collect();

        assert_eq!(
            scrubbed_vars(&SpawnPlan::refresh()),
            expected,
            "the refresh spawn must scrub the full credential/config-override set"
        );
        assert_eq!(
            scrubbed_vars(&SpawnPlan::login(SPAWN_TIMEOUT)),
            expected,
            "the login spawn must scrub the full credential/config-override set — a dropped entry \
             is a silent isolation regression on the login path"
        );
    }

    /// AC2 (issue #131): the refresh path's built command is byte-identical to the pre-#131 inline
    /// spawn — same argv, same env set + scrub, all stdio nulled. Guards the behavior-preserving
    /// contract of the extraction.
    #[test]
    fn refresh_plan_builds_the_legacy_command() {
        let plan = SpawnPlan::refresh();
        let command = plan.build_command(Path::new("/nonexistent/claude"), OsStr::new("/tmp/iso"));
        let std = command.as_std();

        // argv: exactly `-p` then the benign prompt, in that order (order-sensitive — `get_args`
        // preserves it).
        let args: Vec<&OsStr> = std.get_args().collect();
        assert_eq!(args, [OsStr::new("-p"), OsStr::new("say pong")]);

        // env: CLAUDE_CONFIG_DIR + the four DISABLE_* set (Some), the three scrub vars removed
        // (None). Order-independent (BTreeMap).
        let expected: BTreeMap<OsString, Option<OsString>> = [
            ("CLAUDE_CONFIG_DIR", Some("/tmp/iso")),
            ("DISABLE_AUTOUPDATER", Some("1")),
            ("DISABLE_TELEMETRY", Some("1")),
            ("DISABLE_ERROR_REPORTING", Some("1")),
            ("DISABLE_BUG_COMMAND", Some("1")),
            ("CLAUDE_CODE_OAUTH_TOKEN", None),
            ("ANTHROPIC_API_KEY", None),
            ("CLAUDE_SECURESTORAGE_CONFIG_DIR", None),
        ]
        .into_iter()
        .map(|(k, v)| (OsString::from(k), v.map(OsString::from)))
        .collect();
        assert_eq!(env_map(&plan), expected);
    }

    /// The login plan carries its distinguishing parametrization — `/login` argv, inherit-terminal
    /// stdio, and the extra-cancel-arm bound — so the shape a later login caller wires is fixed
    /// here (and the both-parametrizations scrub test above exercises the same build path).
    #[test]
    fn login_plan_carries_the_login_parametrization() {
        // A distinct, non-default timeout proves the caller's tunable budget (#132) threads
        // through into the bound rather than being fixed to the refresh path's SPAWN_TIMEOUT.
        let timeout = Duration::from_secs(180);
        let plan = SpawnPlan::login(timeout);
        let command = plan.build_command(Path::new("/nonexistent/claude"), OsStr::new("/tmp/iso"));
        let args: Vec<&OsStr> = command.as_std().get_args().collect();
        assert_eq!(args, [OsStr::new("/login")]);
        assert_eq!(plan.stdio, Stdio3::InheritTerminal);
        assert_eq!(plan.bound, RunBound::TimeoutOrCancel { timeout });
    }

    /// AC2 (issue #132), the security invariant made explicit: the login child is invoked as the
    /// interactive `/login` slash-command and NEVER a token-EMITTING subcommand (`setup-token`,
    /// or a headless `-p` prompt), so no `sk-ant-*` bearer is ever printed to a stream this
    /// process could capture. Paired with the inherit-terminal stdio (asserted here too), which
    /// means the child's OAuth output goes straight to the operator's tty and is never piped into
    /// an internal sink — there is no channel for a token to cross this process's own stdio.
    #[test]
    fn login_never_invokes_a_token_emitting_subcommand() {
        let plan = SpawnPlan::login(Duration::from_secs(180));
        let command = plan.build_command(Path::new("/nonexistent/claude"), OsStr::new("/tmp/iso"));
        let args: Vec<String> = command
            .as_std()
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        // The sole argument is the interactive slash-command…
        assert_eq!(args, ["/login"]);
        // …and specifically none of the token-emitting shapes.
        assert!(!args.iter().any(|a| a == "setup-token"));
        assert!(!args.iter().any(|a| a == "-p" || a == "--print"));
        // Inherit-terminal stdio: the child's streams are the operator's own, never piped — so
        // the engine has no buffer into which a printed token could be teed.
        assert_eq!(plan.stdio, Stdio3::InheritTerminal);
    }

    /// In-memory isolated keychain item — the fake seam for the session teardown test.
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

    #[tokio::test]
    async fn dropping_an_armed_session_synchronously_tears_down_the_item_and_dir() {
        // The RAII backstop in isolation: the path only the guard's `Drop` can cover — a
        // still-ARMED session dropped on a panic / future-cancellation / timer-kill — the sole
        // thing stopping the secret-bearing isolated item + dir from outliving the session.
        let tmp = tempfile::tempdir().unwrap();
        let iso_dir = tmp.path().join("refresh/u-1");
        crate::paths::create_isolated_dir(&iso_dir).unwrap();
        let keychain = FakeIsolatedKeychain::empty();
        keychain.seed(b"secret-bearing-item").await.unwrap();
        {
            // Armed and never torn down explicitly — leaving this scope drops it ARMED,
            // exercising the synchronous `Drop` cleanup.
            let _session = IsolatedSession::arm(keychain.clone(), iso_dir.clone());
        }
        assert!(
            keychain.item.borrow().is_none(),
            "Drop must delete the isolated keychain item"
        );
        assert!(!iso_dir.exists(), "Drop must remove the isolated dir");
    }
}
