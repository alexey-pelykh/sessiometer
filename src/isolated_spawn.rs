// Copyright (c) 2026 Oleksii PELYKH
// SPDX-License-Identifier: MIT

//! The shared isolated-`CLAUDE_CONFIG_DIR` spawn scaffold (issue #131).
//!
//! One parametrized seam for spawning a `claude` child in an ephemeral, isolated
//! `CLAUDE_CONFIG_DIR`, extracted from the refresh engine ([`crate::refresh`], #102) so a
//! second caller — the interactive-login capture engine ([`crate::login`]) — reuses it rather
//! than forking a second copy. A fork would risk silent drift: a dropped [`SPAWN_ENV_REMOVE`] entry
//! or a diverged [`IsolatedSession`] teardown is an isolation / secret-handling regression no
//! single test would catch.
//!
//! The seam owns three of the four scaffold pieces the issue names:
//!   - the env **scrub** ([`SPAWN_ENV_REMOVE`]) of variables that decide the child's credential or
//!     the host that receives it, applied at [`SpawnPlan::build_command`] for every `claude` child
//!     and, since issue #783, at [`crate::paths::build_login_shell_env_command`] for the
//!     login-shell PATH harvest;
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
//! The refresh path ([`SpawnPlan::refresh`]) is wired through [`SpawnClaude`]; the login
//! parametrization ([`SpawnPlan::login`]) through [`SpawnClaudeLogin`], which
//! [`crate::login::login_account`] constructs for the `login` verb. The
//! `all_parametrizations_apply_the_full_scrub_set` test builds both.

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

/// Env vars **unset** on every isolated spawn — the security-critical scrub (issue #102 step 4).
/// CC reads each one to decide the credential the child `claude` uses, or the host that receives
/// it. The refresh spawn, the login spawn and the issue #783 login-shell PATH harvest remove them,
/// so the child acts on the ISOLATED item and nothing else.
///
/// **Membership rule.** A variable is eligible for this list when CC reads it from the
/// environment to decide which credential, which credential store, or which OAuth client
/// identity the child authenticates or refreshes with. Selecting a source counts as well as
/// supplying one. Inherited, such a variable would make the child act on something other than
/// the item seeded for it.
/// A variable is also eligible when CC reads it to decide which host receives the
/// credential the child authenticates with. Inherited, such a variable would send that
/// credential to a host the parent environment chose.
/// Eligibility is not membership: the scope ratified for each issue decides which eligible
/// variables join, and a variable the engine itself depends on stays inherited (`USER`, below).
///
/// Where each input sits relative to the claude.ai login was read out of the stock Claude Code
/// 2.1.294 binary. `build/version-compat.md` § "Issue #1535 — environment inputs Claude Code
/// consults ahead of the claude.ai login (CC 2.1.294)" records that precedence with its evidence.
/// It also lists the inputs that read traced and this list leaves out, each with its reason. The
/// list does not hold every eligible variable, and that section is not a census of them.
/// `CLAUDE_CODE_CUSTOM_OAUTH_URL` is eligible and is not on this list. When its value is
/// allow-listed, it selects the keychain service suffix (read at 2.1.227–2.1.233, below). At
/// 2.1.294 such a value also moves the token endpoint CC posts a refresh to. Any other non-empty
/// value makes CC's OAuth config resolver throw. The ledger section records these effects. The
/// scope ratified on issue #1535 adds only `ANTHROPIC_AUTH_TOKEN`,
/// `CLAUDE_CODE_OAUTH_TOKEN_FILE_DESCRIPTOR` and `CLAUDE_CODE_API_KEY_FILE_DESCRIPTOR`. It names
/// this one neither way, so whether it joins the list is left to its own issue. `USER` is eligible
/// too. It sits outside the read of the 2.1.294 binary, and this doc comment records its role. It
/// decides the item's `acct`. It must stay inherited, because the engine seeds the item under the
/// `acct` CC derives from it (`claude_code_acct_from` in `src/keychain.rs`, issue #711). Scrubbing
/// it would point the child at a different item whenever `USER` differs from the passwd name.
///
/// The env credential inputs that follow are ones CC consults ahead of the claude.ai login at
/// 2.1.294. Their roles differ:
///   - `CLAUDE_CODE_OAUTH_TOKEN` and `CLAUDE_CODE_OAUTH_TOKEN_FILE_DESCRIPTOR` SUPPLY an OAuth
///     token. The first holds the token, which outranks the keychain item. The second names a
///     descriptor CC reads one from. That token outranks the item while the descriptor is readable
///     and no access token from the `CLAUDE_BG_AUTH_SNAPSHOT_PATH` snapshot was consumed first.
///     The ledger's builder finding records the mechanism.
///   - `ANTHROPIC_API_KEY` and `ANTHROPIC_AUTH_TOKEN` SUPPLY a credential. Each can also close the
///     gate that decides whether the API clients CC's client builder makes use the claude.ai
///     login. The ledger's gate finding says when each does. The token refresh does not call that
///     gate, though it does read the gate's provider test. The ledger section records when that
///     test skips the refresh. `ANTHROPIC_API_KEY` can close the gate when CC picks it as the API
///     key. CC's start-up marks a `-p` child non-interactive. So outside the VS Code client, CC
///     picks it in the refresh child whenever it is set. The login child inherits sessiometer's
///     terminal stdout, so CC marks it interactive. Outside simple mode (`CLAUDE_CODE_SIMPLE` or
///     `--bare`), the login child takes the key only once the key has been approved. Whether a
///     later write changes the refresh child's non-interactive mark or the login child's
///     interactive mark is open in the ledger section. So is who records that approval. Neither
///     input closes the gate while the exemption the ledger calls `oqe()` holds.
///   - `CLAUDE_CODE_API_KEY_FILE_DESCRIPTOR` both SELECTS and SUPPLIES. Set to a value that is
///     non-empty after trimming, it can close that same gate, whether or not its descriptor can be
///     read. The exemption the ledger calls `vr()` cancels that closure. While the descriptor is
///     readable, CC also reads from it the API key the child sends. It uses the same reader as for
///     `CLAUDE_CODE_OAUTH_TOKEN_FILE_DESCRIPTOR` above. CC's API-key resolver, which the ledger
///     calls the resolver, labels that key `ANTHROPIC_API_KEY`. So that key can close the gate as
///     `ANTHROPIC_API_KEY` does. That closure falls under the `oqe()` exemption, not the `vr()`
///     exemption.
///
/// `oqe()` and `vr()` are minified 2.1.294 names. The ledger's Names row records that such names
/// change between builds, and its gate finding describes both tests. `vr()` holds when
/// `CLAUDE_CODE_REMOTE` is set, or when `CLAUDE_CODE_ENTRYPOINT` equals `claude-desktop`,
/// `claude-desktop-3p` or `local-agent`. `oqe()` holds when `vr()` holds,
/// `CLAUDE_CODE_HOST_AUTH_ENV_VAR` is unset and `CLAUDE_CODE_ENTRYPOINT` is not
/// `claude-desktop-3p`. This list removes none of the variables `oqe()` and `vr()` read.
///
/// The ledger section records what `CLAUDE_CODE_OAUTH_TOKEN`,
/// `CLAUDE_CODE_OAUTH_TOKEN_FILE_DESCRIPTOR`, `ANTHROPIC_API_KEY`, `ANTHROPIC_AUTH_TOKEN` and
/// `CLAUDE_CODE_API_KEY_FILE_DESCRIPTOR` each did to the refresh child in its 2026-11-02T05:46:18Z
/// run, one arm per input. It also records what that run did not measure.
///
/// `ANTHROPIC_BASE_URL` decides where CC's first-party API client sends the child's credential.
/// CC lists two companions for it. `ANTHROPIC_CUSTOM_HEADERS` is one of them, and a supplier in its
/// own right; it is the only one of the two this list removes. Issue #1625 added the base URL and
/// the custom headers. The ledger section records the evidence, read out of 2.1.294:
///   - `ANTHROPIC_BASE_URL` sets the base URL of CC's first-party API client. CC's client builder
///     chooses the credential first. Without a managed provider policy, no host test sits between
///     that choice and the base URL. In that case, whatever credential the child selects goes to
///     that host on that client, the seeded item's token included. The 2026-11-02T05:14:47Z run
///     measured a dummy stored token arriving at an inherited base URL as the bearer. That token
///     was unexpired and had no refresh token.
///     Its store was the plaintext credentials file, so the macOS keychain read was not exercised.
///     For the keychain item the claim rests on the shared builder and client. sessiometer
///     back-dates the seed it gives the refresh child, and the ledger section lists that case as
///     unmeasured.
///   - `ANTHROPIC_CUSTOM_HEADERS` goes into the client's default headers, which the SDK merges
///     after the auth headers it builds from the client's options. A header it names replaces an
///     earlier header of the same name. So on CC's first-party API client, a custom
///     `Authorization` header replaces the bearer the SDK built, and headers of other names ride
///     beside the auth headers. CC can also write its own `Authorization` over a custom one, under
///     conditions the ledger section records. Its other client branches can drop or replace a
///     custom one. The ledger section records the branches its read decoded.
///     Replacing the bearer makes the input eligible as a supplier. That is a static read: no run
///     set this variable. CC also lists it as one of the base URL's two companions. It is removed
///     together with the base URL because, with only the base URL removed, a gateway's headers
///     would go to `api.anthropic.com`.
///
/// Removing the base URL and the custom headers closes the inherited-environment route through them
/// at every place that applies this list: the refresh spawn, the login spawn and the issue #783
/// login-shell PATH harvest. `_CLAUDE_CODE_ASSUME_FIRST_PARTY_BASE_URL`, the base URL's other
/// companion, stays inherited, and issue #1626 holds it. In the host classification the ledger
/// calls `pi()` it acts only on a base URL that is set. `Ikn()`, `jdr()` and `Son()` also read it,
/// whatever the base URL, and those reads were not traced to a credential or a host. Like `oqe()`
/// and `vr()`, the names `pi()`, `Ikn()`, `jdr()` and `Son()` are minified 2.1.294 names, which
/// change between builds. The ledger section lists what they change under what the read did not
/// settle.
/// `CLAUDE_CODE_API_BASE_URL` stays inherited too, and issue #1626 holds it. In what the read
/// covered it feeds only the Files API helpers, as their fallback host. Those helpers send their
/// caller's OAuth token as the bearer, to the caller's own base URL when it passes one. Every
/// caller the read found passes one. The ledger section lists as unsettled whether either child
/// reaches those callers, and whose token each passes.
///
/// `CLAUDE_SECURESTORAGE_CONFIG_DIR` picks the STORE: it takes PRECEDENCE over `CLAUDE_CONFIG_DIR`
/// in CC's keychain service-name derivation (#100, [`crate::keychain`]), so an inherited one would
/// mis-target the read away from the item we seeded.
///
/// The OAuth refresh triple below is issue #1009 / PRD R-13, added BEFORE the login argv moves to
/// the `auth login` form (#1020, ADR-0032 prerequisite 1) rather than alongside it. Every
/// mechanism named below was read out of the shipping CC binary — walked at 2.1.232 and
/// re-checked at 2.1.227, 2.1.228 and 2.1.229; all four agree. The three do not share one
/// justification, so they do not get one sentence:
///   - `CLAUDE_CODE_OAUTH_REFRESH_TOKEN` — the entry point, and a credential in its own right.
///     CC's `auth login` handler SHORT-CIRCUITS on an inherited refresh token: it exchanges the
///     token, writes the credential, prints `Login successful.` and exits 0 — the browser flow
///     sits BELOW that block and is never reached. Under today's `/login` argv the handler is not
///     entered at all, so this entry is inert on arrival — which is the point of landing it first.
///     Once argv moves, an inherited token captures the WRONG account while every existing safety
///     check still passes: nothing is malformed, so nothing objects. That hazard carries a
///     PRECONDITION worth stating, because the sentence travels without it: the capture is SILENT
///     only if `CLAUDE_CODE_OAUTH_SCOPES` is inherited too — see the next entry.
///   - `CLAUDE_CODE_OAUTH_SCOPES` — a REQUIRED CONJUNCT of that same short-circuit, and so the
///     stronger of the two levers rather than the make-weight an earlier draft of this comment
///     called it. The handler reads it immediately after the refresh token and, if it is absent,
///     writes `CLAUDE_CODE_OAUTH_SCOPES is required when using CLAUDE_CODE_OAUTH_REFRESH_TOKEN.`
///     to stderr and exits 1. Scrubbing THIS one alone therefore downgrades the silent
///     wrong-account capture above into a loud, harmless failure. It is NOT confined to that
///     handler, though, and an earlier draft of this comment said it was — claiming this name
///     and the refresh token were "each read at exactly one site, inside the login handler".
///     The INSTRUMENT was the defect: a `process.env.<NAME>` grep cannot see a read through
///     CC's typed accessor over `process.env`, whose getter re-reads `process.env[name]` on
///     each property access. Counting BOTH forms across 2.1.227-2.1.232, canaried each way (a
///     known-present name matches, an invented one matches nothing): this one has 2 read sites
///     — the login handler, plus a scope-list helper OUTSIDE it reached through the accessor —
///     against 1 for `CLAUDE_CODE_OAUTH_REFRESH_TOKEN` and 2 for `CLAUDE_CODE_OAUTH_CLIENT_ID`.
///     Of the triple only the refresh token is confined to the login handler.
///   - `CLAUDE_CODE_OAUTH_CLIENT_ID` — feeds the `client_id` of the OAuth token exchange, so an
///     inherited one performs that exchange under a FOREIGN CLIENT IDENTITY. It reaches the field
///     by two routes: the short-circuit passes it explicitly, and the exchange itself falls back
///     to CC's general OAuth config resolver, which applies the same variable. That general
///     resolver is why it is NOT confined to `auth login` — the same reason, and the same
///     measurement, as the second read site noted for `CLAUDE_CODE_OAUTH_SCOPES` above.
///
///     It does NOT re-target the keychain service name — worth disarming HERE, because the
///     temptation is STRUCTURAL rather than a stale note somewhere: the client id and
///     `OAUTH_FILE_SUFFIX`, the one field that DOES move the service name, are members of the
///     same OAuth config object returned by the same resolver just named, so "an inherited
///     client id reaches that config" reads as "it can reach the suffix". Two independent
///     measurements say it cannot, re-read for this paragraph at 2.1.227, 2.1.228, 2.1.229,
///     2.1.232 and 2.1.233 — all five agree, differing only in minified identifiers. ORDER: the
///     suffix is set by the deployment environment baked into the build (the environment
///     function is a hardcoded `return "prod"`, so a shipped build takes `""`; `-local-oauth` is
///     the local-build literal) or by the allow-listed `CLAUDE_CODE_CUSTOM_OAUTH_URL` branch
///     (`-custom-oauth`), while the client-id read sits AFTER that branch and rebinds
///     `CLIENT_ID` alone. CLOSURE, the stronger of the two: EVERY write to `OAUTH_FILE_SUFFIX`
///     in those binaries is a string LITERAL — `""`, `-local-oauth`, `-custom-oauth`, with no
///     computed write anywhere, and CC's own exported `OAUTH_GLOBAL_FILE_SUFFIXES` is likewise a
///     fixed list of literals — so no env-var value can enter the suffix by any path, ordering
///     aside. This entry is therefore not of the `CLAUDE_SECURESTORAGE_CONFIG_DIR` mis-targeting
///     class above: the empty-suffix form [`crate::keychain`] hardcodes is not something a
///     client id can move.
///
///     That conclusion rests on no citation, deliberately. An earlier draft rested it on a
///     sentence quoted from `build/version-compat.md`, which then recorded the suffix as
///     non-empty "only for a custom OAuth client id". Issue #1268 corrected that record — it now
///     states both directions and names the same two selectors — leaving this paragraph arguing
///     against a quotation the repo no longer contained. The old wording survives there only
///     inside its own retraction note, so a grep that lands on it is finding history, not a live
///     claim.
///
/// Kept as a named list (not inline `env_remove` calls), applied by [`SpawnPlan::build_command`]
/// — and, for the issue #783 login-shell PATH harvest, by
/// [`crate::paths::build_login_shell_env_command`] — so the
/// `all_parametrizations_apply_the_full_scrub_set` test can assert the set is applied for EVERY
/// parametrization — a dropped entry is a silent isolation regression on whichever caller drops
/// it. That test derives its expectation FROM this list, so it cannot notice a name deleted from
/// the list itself. The tests that follow spell names out literally for exactly that reason:
///   - `the_oauth_refresh_triple_is_scrubbed_by_name` pins the issue #1009 triple;
///   - `the_recorded_env_credential_sources_are_scrubbed_by_name` pins `CLAUDE_CODE_OAUTH_TOKEN`,
///     `CLAUDE_CODE_OAUTH_TOKEN_FILE_DESCRIPTOR`, `ANTHROPIC_API_KEY`, `ANTHROPIC_AUTH_TOKEN` and
///     `CLAUDE_CODE_API_KEY_FILE_DESCRIPTOR`;
///   - `the_base_url_and_its_companion_headers_are_scrubbed_by_name` pins `ANTHROPIC_BASE_URL` and
///     `ANTHROPIC_CUSTOM_HEADERS`;
///   - `refresh_plan_builds_the_legacy_command` pins every variable the built refresh command sets
///     or removes.
pub(crate) const SPAWN_ENV_REMOVE: &[&str] = &[
    "CLAUDE_CODE_OAUTH_TOKEN",
    "CLAUDE_CODE_OAUTH_TOKEN_FILE_DESCRIPTOR",
    "ANTHROPIC_API_KEY",
    "ANTHROPIC_AUTH_TOKEN",
    "CLAUDE_CODE_API_KEY_FILE_DESCRIPTOR",
    "ANTHROPIC_BASE_URL",
    "ANTHROPIC_CUSTOM_HEADERS",
    "CLAUDE_SECURESTORAGE_CONFIG_DIR",
    "CLAUDE_CODE_OAUTH_REFRESH_TOKEN",
    "CLAUDE_CODE_OAUTH_SCOPES",
    "CLAUDE_CODE_OAUTH_CLIENT_ID",
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
    /// `/login` session runs until the user finishes, the timeout elapses, or the operator's
    /// SIGINT cancels it. The cancel arm is the `ctrl_c` select arm in [`run`](SpawnPlan::run);
    /// no daemon-shutdown cancel is wired.
    TimeoutOrCancel { timeout: Duration },
}

/// The parametrized isolated-spawn plan (issue #131): the single owner of the (argv, stdio,
/// run-bound) axes and the single site the [`SPAWN_ENV_REMOVE`] scrub is applied to a `claude`
/// child. (The issue #783 login-shell PATH harvest applies the same list, at
/// [`crate::paths::build_login_shell_env_command`].) Both the refresh path and the login path
/// build their `claude` child through this one seam.
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
    /// [`SPAWN_TIMEOUT`]. Equivalent to the pre-#131 inline spawn in argv and stdio; its scrub is
    /// a strict SUPERSET since issues #1009, #1535 and #1625 widened [`SPAWN_ENV_REMOVE`] (guarded
    /// by the `refresh_plan_builds_the_legacy_command` test, whose doc records the divergence).
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
    /// login-capture engine) via [`SpawnClaudeLogin`]; the
    /// `all_parametrizations_apply_the_full_scrub_set` test also builds it to prove the
    /// [`SPAWN_ENV_REMOVE`] scrub applies here too.
    pub(crate) fn login(timeout: Duration) -> Self {
        Self {
            argv: &["/login"],
            stdio: Stdio3::InheritTerminal,
            bound: RunBound::TimeoutOrCancel { timeout },
        }
    }

    /// **The single place the scrub is applied to a `claude` child** (the issue #783 login-shell
    /// PATH harvest applies the same list in [`crate::paths::build_login_shell_env_command`]).
    /// Build the `claude` command for `binary` pointed at the isolated `config_dir`: the
    /// parametrized argv, then `CLAUDE_CONFIG_DIR`, the `DISABLE_*` headless guards (keep a
    /// headless run from auto-updating / phoning home mid-cycle), the parametrized stdio, and
    /// finally the [`SPAWN_ENV_REMOVE`] scrub — applied LAST so no earlier `.env` can resurrect a
    /// scrubbed var.
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
/// [`SpawnPlan::login`], constructed by the login-capture engine's production entry,
/// [`crate::login::login_account`], which the `login` verb reaches through `capture::login`.
pub(crate) struct SpawnClaudeLogin {
    binary: PathBuf,
}

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

    /// The set of vars an arbitrary built command REMOVES — the [`scrubbed_vars`] projection for a
    /// spawn that is not a [`SpawnPlan`] (the issue #783 login-shell harvest).
    fn scrubbed_vars_of(command: &Command) -> BTreeSet<OsString> {
        command
            .as_std()
            .get_envs()
            .filter(|(_, value)| value.is_none())
            .map(|(key, _)| key.to_os_string())
            .collect()
    }

    /// Every scrub seam, each paired with the set of variables the command it builds removes. A
    /// scrub seam is a place that builds a child environment through [`SPAWN_ENV_REMOVE`]: the
    /// refresh spawn, the login spawn and the issue #783 login-shell PATH harvest. In the scrub
    /// tests that iterate this list, "seam" names one such place. Elsewhere in this file the word
    /// also names the shared spawn scaffold, and a point a test substitutes, such as the fake
    /// keychain below.
    ///
    /// The scrub tests below all iterate this one list, so a new scrub seam is added here once
    /// rather than in each test. That avoids the drift the module doc warns about for the scrub
    /// list itself. Adding it here is a convention nothing checks. These tests protect only the
    /// places listed here: the refresh and login plans' [`SpawnPlan::build_command`] and
    /// [`crate::paths::build_login_shell_env_command`].
    ///
    /// The login-shell PATH harvest is the only scrub seam that is not a `claude` spawn. It also
    /// has the sharpest need: it runs the user's login shell, which sources ARBITRARY rc files. A
    /// variable the harvest fails to remove is visible to those files. Where that variable carries
    /// a credential, the credential reaches them.
    fn scrub_seams() -> [(&'static str, BTreeSet<OsString>); 3] {
        [
            ("the refresh spawn", scrubbed_vars(&SpawnPlan::refresh())),
            (
                "the login spawn",
                scrubbed_vars(&SpawnPlan::login(SPAWN_TIMEOUT)),
            ),
            (
                "the login-shell PATH harvest",
                scrubbed_vars_of(&crate::paths::build_login_shell_env_command(Path::new(
                    "/bin/zsh",
                ))),
            ),
        ]
    }

    /// AC3 (issue #131): the security-critical env scrub is applied to the built command for EVERY
    /// parametrization — a caller silently dropping a scrub entry (an isolation regression) fails
    /// here regardless of which path drops it.
    ///
    /// Extended by issue #783 to a THIRD parametrization, the login-shell PATH harvest
    /// ([`crate::paths::build_login_shell_env_command`]); `scrub_seams` records why that seam has
    /// the sharpest need.
    #[test]
    fn all_parametrizations_apply_the_full_scrub_set() {
        let expected: BTreeSet<OsString> = SPAWN_ENV_REMOVE
            .iter()
            .map(|s| OsString::from(*s))
            .collect();

        for (seam, scrubbed) in scrub_seams() {
            assert_eq!(
                scrubbed, expected,
                "the set of variables {seam} removes differs from SPAWN_ENV_REMOVE"
            );
        }
    }

    /// Issue #1009 / PRD R-13, the falsifiable half: the OAuth refresh triple is scrubbed, named
    /// LITERALLY rather than derived from [`SPAWN_ENV_REMOVE`].
    ///
    /// [`all_parametrizations_apply_the_full_scrub_set`] above builds its expectation FROM the
    /// const, so it is green for whatever the const happens to say — it passed before this issue
    /// and would pass again the day a name is deleted from the list. An expectation taken from the
    /// thing under test cannot detect that thing being wrong, so the names are spelled out here
    /// and checked against what each seam actually builds. The pre-#1009 entries are checked
    /// alongside them because an ADDITION is precisely the edit that drops a neighbour
    /// (`docs/specs/spawn-env-scrub.feature.md` Rule 1).
    ///
    /// Asserted on the built command, not on a spawned child: `env_remove` records an explicit
    /// `None`, so the removal is observable whether or not this process happens to carry the var —
    /// the exhaustive check a live child cannot give without mutating process-global environment
    /// (same reasoning as `paths::tests::the_harvest_command_scrubs_the_full_credential_set`).
    #[test]
    fn the_oauth_refresh_triple_is_scrubbed_by_name() {
        // Spelled out, deliberately NOT `SPAWN_ENV_REMOVE`: deleting a name from that list must
        // fail this test, naming the variable it dropped.
        const REQUIRED: &[&str] = &[
            // The issue #1009 / R-13 triple.
            "CLAUDE_CODE_OAUTH_REFRESH_TOKEN",
            "CLAUDE_CODE_OAUTH_SCOPES",
            "CLAUDE_CODE_OAUTH_CLIENT_ID",
            // Their pre-existing neighbours, guarded against the drop an addition invites.
            "CLAUDE_CODE_OAUTH_TOKEN",
            "ANTHROPIC_API_KEY",
            "CLAUDE_SECURESTORAGE_CONFIG_DIR",
        ];

        for (seam, scrubbed) in scrub_seams() {
            for var in REQUIRED {
                assert!(
                    scrubbed.contains(&OsString::from(*var)),
                    "{seam} does not record {var} as removed from the environment it constructs"
                );
            }
        }
    }

    /// The env-sourced credential inputs recorded at CC 2.1.294 are scrubbed, named LITERALLY, at
    /// each seam `scrub_seams` lists. Its subject is the issue #1535 set, which differs from
    /// `the_oauth_refresh_triple_is_scrubbed_by_name`'s: that test guards the issue #1009 triple
    /// plus its own immediate neighbours.
    ///
    /// `build/version-compat.md` § "Issue #1535 — environment inputs Claude Code consults ahead of
    /// the claude.ai login (CC 2.1.294)" records, from a static read of the stock binary, which
    /// environment inputs CC consults ahead of the claude.ai keychain login. The ones pinned here
    /// supply or select the child's credential ahead of that login, AND sit on the list:
    ///   - `ANTHROPIC_API_KEY`, `ANTHROPIC_AUTH_TOKEN` and `CLAUDE_CODE_OAUTH_TOKEN` supply a
    ///     credential directly;
    ///   - `CLAUDE_CODE_OAUTH_TOKEN_FILE_DESCRIPTOR` supplies one through the descriptor it names,
    ///     and that token outranks the keychain item while the descriptor is readable and no
    ///     access token from the `CLAUDE_BG_AUTH_SNAPSHOT_PATH` snapshot was consumed first;
    ///   - `CLAUDE_CODE_API_KEY_FILE_DESCRIPTOR` selects and supplies: set to a value non-empty
    ///     after trimming, it can close the login gate, and while its descriptor is readable CC
    ///     reads the API key from it.
    ///
    /// That section records more inputs than these — others that supply or select a credential,
    /// and the ones the scrub leaves out, each with its reason. Nothing here pins those.
    /// `ANTHROPIC_CUSTOM_HEADERS` can supply a credential as well, and
    /// `the_base_url_and_its_companion_headers_are_scrubbed_by_name` pins it.
    ///
    /// Because the roles differ, the failure message names no consequence. It states only the
    /// detected symptom: the variable is not recorded as removed from the environment the seam
    /// builds. That holds at each seam and for each input.
    /// The PATH harvest runs a login shell, not `claude`. The login spawn is not a refresh. So
    /// "outranks the keychain" or "skips the refresh" would be false on at least one of them. At
    /// the refresh spawn itself, the ledger's 2026-11-02T05:46:18Z run saw a token-host `CONNECT`
    /// still attempted under each of `ANTHROPIC_API_KEY`, `ANTHROPIC_AUTH_TOKEN` and an unopened
    /// `CLAUDE_CODE_API_KEY_FILE_DESCRIPTOR`.
    ///
    /// Spelled out rather than derived from [`SPAWN_ENV_REMOVE`] for the reason
    /// `the_oauth_refresh_triple_is_scrubbed_by_name` already records: an expectation taken from
    /// the thing under test cannot detect that thing being wrong.
    #[test]
    fn the_recorded_env_credential_sources_are_scrubbed_by_name() {
        // Spelled out, deliberately NOT `SPAWN_ENV_REMOVE`: deleting any of these from that list
        // must fail here, naming the seam and the variable.
        const RECORDED_ENV_CREDENTIAL_SOURCES: &[&str] = &[
            "ANTHROPIC_API_KEY",
            "ANTHROPIC_AUTH_TOKEN",
            "CLAUDE_CODE_OAUTH_TOKEN",
            "CLAUDE_CODE_OAUTH_TOKEN_FILE_DESCRIPTOR",
            "CLAUDE_CODE_API_KEY_FILE_DESCRIPTOR",
        ];

        for (seam, scrubbed) in scrub_seams() {
            for var in RECORDED_ENV_CREDENTIAL_SOURCES {
                assert!(
                    scrubbed.contains(&OsString::from(*var)),
                    "{seam} does not record {var} as removed from the environment it builds"
                );
            }
        }
    }

    /// The API base URL and its companion custom headers are scrubbed, named LITERALLY, at each
    /// seam `scrub_seams` lists. Issue #1625 added them to [`SPAWN_ENV_REMOVE`].
    ///
    /// `build/version-compat.md` § "Issue #1535 — environment inputs Claude Code consults ahead of
    /// the claude.ai login (CC 2.1.294)" records why each is on the list, read out of the stock
    /// binary:
    ///   - `ANTHROPIC_BASE_URL` sets the base URL of CC's first-party API client. Without a managed
    ///     provider policy, whatever credential the child selects there goes to that host;
    ///   - `ANTHROPIC_CUSTOM_HEADERS` goes into the API client's default headers, with no host
    ///     test. A header it names replaces an earlier header of the same name. That makes it a
    ///     supplier: on CC's first-party API client, a custom `Authorization` header replaces the
    ///     bearer the SDK builds from the client's options.
    ///
    /// Claude Code lists `ANTHROPIC_CUSTOM_HEADERS` as one of two companions of
    /// `ANTHROPIC_BASE_URL`, and it is the only one of the two on the list. The base URL and the
    /// custom headers are scrubbed together because removing the base URL alone would send a
    /// gateway's headers to `api.anthropic.com`.
    ///
    /// Kept apart from `the_recorded_env_credential_sources_are_scrubbed_by_name`, whose inputs
    /// decide which credential the child sends. The base URL decides where the first-party client
    /// sends it. The custom headers ride beside it, and can replace the credential it carries.
    ///
    /// The failure message states only the detected symptom: the variable is not recorded as
    /// removed from the environment the seam builds. That holds at every seam. The PATH harvest
    /// runs a login shell, not `claude`, so a message naming the host a credential reaches would
    /// be false there.
    ///
    /// Spelled out rather than derived from [`SPAWN_ENV_REMOVE`] for the reason
    /// `the_oauth_refresh_triple_is_scrubbed_by_name` already records: an expectation taken from
    /// the thing under test cannot detect that thing being wrong.
    #[test]
    fn the_base_url_and_its_companion_headers_are_scrubbed_by_name() {
        // Spelled out, deliberately NOT `SPAWN_ENV_REMOVE`: deleting either from that list must
        // fail here, naming the seam and the variable.
        const BASE_URL_AND_COMPANION_HEADERS: &[&str] =
            &["ANTHROPIC_BASE_URL", "ANTHROPIC_CUSTOM_HEADERS"];

        for (seam, scrubbed) in scrub_seams() {
            for var in BASE_URL_AND_COMPANION_HEADERS {
                assert!(
                    scrubbed.contains(&OsString::from(*var)),
                    "{seam} does not record {var} as removed from the environment it builds"
                );
            }
        }
    }

    /// AC2 (issue #131): the refresh path's built command is the pre-#131 inline spawn — same
    /// argv, same env set, all stdio nulled. Guards the behavior-preserving contract of the
    /// extraction.
    ///
    /// The scrub half is no longer byte-identical, and deliberately: issue #1009 widened
    /// [`SPAWN_ENV_REMOVE`] by the OAuth refresh triple, and the list is shared, so the refresh
    /// child now has a strict SUPERSET of the pre-#131 removals. That is the intended blast radius
    /// of a single named list. Only ONE of the three, `CLAUDE_CODE_OAUTH_REFRESH_TOKEN`, is read
    /// solely inside CC's `auth login` handler, which this `-p` child never enters, so scrubbing
    /// THAT one changes what the child inherits and not what it does. The other two are not
    /// claimed as inert here. `CLAUDE_CODE_OAUTH_CLIENT_ID` is read by CC's general OAuth config
    /// resolver, so an inherited one could reach this child's own token exchange; closing that is
    /// a behaviour change, and a wanted one. `CLAUDE_CODE_OAUTH_SCOPES` has a SECOND read site
    /// outside the login handler — a scope-list helper, reached through CC's typed accessor over
    /// `process.env`, which a `process.env.<NAME>` grep does not see and an earlier draft of this
    /// comment therefore missed. Whether that helper is reachable from this `-p` child was NOT
    /// established here; this entry rides the refresh token's ordering argument, not a measured
    /// inertness claim of its own.
    ///
    /// Issue #1535 widened the list again, by `ANTHROPIC_AUTH_TOKEN`,
    /// `CLAUDE_CODE_OAUTH_TOKEN_FILE_DESCRIPTOR` and `CLAUDE_CODE_API_KEY_FILE_DESCRIPTOR`.
    /// Removing these DOES change what this `-p` child does, as intended.
    /// `build/version-compat.md` § "Issue #1535 — environment inputs Claude Code consults ahead of
    /// the claude.ai login (CC 2.1.294)" records what each of these inherited inputs did to this
    /// child, in the section's 2026-11-02T05:46:18Z run.
    ///
    /// Issue #1625 widened it by `ANTHROPIC_BASE_URL` and `ANTHROPIC_CUSTOM_HEADERS`. CC lists two
    /// companions for the base URL, and the custom headers are the only one of them on the list.
    /// Removing the base URL changes where this child's API requests go. It does not move the
    /// token endpoint CC posts the refresh to. In one inherited setup it can change whether that
    /// refresh runs. That setup is `CLAUDE_CODE_USE_GATEWAY` with a gateway token from
    /// `CLAUDE_CODE_GATEWAY_TOKEN_FILE_DESCRIPTOR` or the snapshot, both still inherited. There an
    /// inherited base URL can make CC pick its gateway provider and skip the refresh as not needed.
    /// The ledger section records that chain as a static read, with the parts it did not trace.
    /// Removing the custom headers changes which headers ride its API requests, an
    /// `Authorization` header included. Whether the token-refresh request carries them was not
    /// settled. The ledger section lists it as open.
    ///
    /// The expectation below stays spelled out literally rather than derived from the const: it is
    /// the one place the whole env map is pinned by hand, and a derived one could not catch the
    /// const itself losing an entry.
    #[test]
    fn refresh_plan_builds_the_legacy_command() {
        let plan = SpawnPlan::refresh();
        let command = plan.build_command(Path::new("/nonexistent/claude"), OsStr::new("/tmp/iso"));
        let std = command.as_std();

        // argv: exactly `-p` then the benign prompt, in that order (order-sensitive — `get_args`
        // preserves it).
        let args: Vec<&OsStr> = std.get_args().collect();
        assert_eq!(args, [OsStr::new("-p"), OsStr::new("say pong")]);

        // env: CLAUDE_CONFIG_DIR + the DISABLE_* guards set (Some), every scrub var removed
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
            // The issue #1009 / R-13 refresh triple.
            ("CLAUDE_CODE_OAUTH_REFRESH_TOKEN", None),
            ("CLAUDE_CODE_OAUTH_SCOPES", None),
            ("CLAUDE_CODE_OAUTH_CLIENT_ID", None),
            // The issue #1535 additions, also pinned by name in
            // `the_recorded_env_credential_sources_are_scrubbed_by_name`.
            ("ANTHROPIC_AUTH_TOKEN", None),
            ("CLAUDE_CODE_OAUTH_TOKEN_FILE_DESCRIPTOR", None),
            ("CLAUDE_CODE_API_KEY_FILE_DESCRIPTOR", None),
            // The issue #1625 additions, also pinned by name in
            // `the_base_url_and_its_companion_headers_are_scrubbed_by_name`.
            ("ANTHROPIC_BASE_URL", None),
            ("ANTHROPIC_CUSTOM_HEADERS", None),
        ]
        .into_iter()
        .map(|(k, v)| (OsString::from(k), v.map(OsString::from)))
        .collect();
        assert_eq!(env_map(&plan), expected);
    }

    /// The login plan carries its distinguishing parametrization — `/login` argv, inherit-terminal
    /// stdio, and the extra-cancel-arm bound — so the shape the login caller wires is fixed here
    /// (and [`all_parametrizations_apply_the_full_scrub_set`] above exercises the same build
    /// path).
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
