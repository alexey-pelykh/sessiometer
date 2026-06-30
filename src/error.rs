// Copyright (c) 2026 Oleksii PELYKH
// SPDX-License-Identifier: MIT

//! Crate-wide error type.
//!
//! Invariant: an [`Error`] value never carries secret material (OAuth blobs,
//! keychain secrets). Its `Display` and `Debug` are therefore safe to print on
//! any channel — the foundation the output-redaction work (issue #15) builds
//! on.

use std::path::PathBuf;
use std::time::Duration;

/// The result type used throughout the crate.
pub(crate) type Result<T> = std::result::Result<T, Error>;

/// Every fallible operation in the crate surfaces one of these.
#[derive(Debug, thiserror::Error)]
pub(crate) enum Error {
    /// A subsystem exists as a seam but its behavior lands in a later work
    /// item. The payload is a static, secret-free hint (e.g. `"usage polling
    /// (#5)"`).
    #[error("not yet implemented: {0}")]
    Unimplemented(&'static str),

    /// An unrecognized CLI subcommand was given.
    #[error("unknown command: {0}")]
    UnknownCommand(String),

    /// The current user's home directory could not be resolved from the
    /// password database (see [`crate::paths`]).
    #[error("could not resolve the home directory for the current user")]
    HomeUnresolved,

    /// A directory that must be private is owned by a different uid.
    #[error("directory {0} is not owned by the current user")]
    ForeignOwnership(PathBuf),

    /// The current user's login name could not be resolved from the password
    /// database (`getpwuid(getuid())->pw_name`, see [`crate::paths`]). The
    /// isolated-refresh engine (issue #102) seeds its keychain item under this
    /// `acct` (the name Claude Code reads with), so it cannot proceed without it.
    #[error("could not resolve the login name for the current user")]
    UserUnresolved,

    /// The ephemeral isolated-refresh directory (`<support>/refresh/<account-uuid>`,
    /// issue #102) could not be created as a safe private directory: a pre-existing
    /// entry at that path is a symlink, refused rather than followed — a planted
    /// symlink could redirect the seeded `.claude.json` or the spawned `claude`'s
    /// writes outside our `0700` tree. The path is a filesystem location, never a
    /// secret.
    #[error("refusing to use the isolated-refresh directory {path}: it is a symlink, not a private directory")]
    UnsafeIsolatedDir { path: PathBuf },

    /// No `Claude Code-credentials` item is present in the keychain — an account
    /// must be captured before it can be read or swapped.
    #[error("no Claude Code credential found in the keychain (capture an account first)")]
    CredentialNotFound,

    /// More than one `Claude Code-credentials` item is present, so the active
    /// account is ambiguous. The resolve step refuses to guess (issue #2).
    #[error(
        "ambiguous keychain: {count} `Claude Code-credentials` items found (expected exactly one)"
    )]
    CredentialAmbiguous { count: usize },

    /// The keychain is locked: `security` exited 36 (`errSecInteractionNotAllowed`)
    /// during `{op}`. Detection only — wait/backoff handling lives in #13.
    #[error("keychain is locked (security exit 36) during {op}")]
    KeychainLocked { op: &'static str },

    /// A `security` CLI keychain operation failed for another reason. `op` is the
    /// operation (`"resolve"` / `"read"` / `"write"`) and `code` is the exit
    /// status (`-1` if signal-terminated). Deliberately carries neither secret
    /// material nor raw CLI output.
    #[error("keychain {op} via `security` failed (exit status {code})")]
    Keychain { op: &'static str, code: i32 },

    /// `CLAUDE_CONFIG_DIR` (or `CLAUDE_SECURESTORAGE_CONFIG_DIR`) holds a non-ASCII
    /// value, which sessiometer cannot map to Claude Code's keychain service name.
    /// CC hashes the **NFC-normalized** config-dir path; for an ASCII path NFC is
    /// the identity, so the raw bytes hash byte-identically and no Unicode-normalizer
    /// dependency is pulled in (issue #100). A non-ASCII path could differ between
    /// its NFC form and its raw bytes, so rather than compute a suffix that may
    /// silently address the **wrong** keychain item, resolution refuses. The
    /// offending value is a filesystem path and is deliberately NOT echoed.
    #[error(
        "CLAUDE_CONFIG_DIR (or CLAUDE_SECURESTORAGE_CONFIG_DIR) contains non-ASCII characters, \
         which sessiometer cannot map to Claude Code's keychain service name"
    )]
    NonAsciiConfigDir,

    /// No `config.toml` exists yet at the expected path. Carries the path (a
    /// filesystem location, never a secret) so the message can name it.
    #[error("no config file at {path} — run `sessiometer capture` to create one")]
    ConfigNotFound { path: PathBuf },

    /// No accounts in the roster to act on. The friendly, user-facing empty state
    /// for two consumers: the offline `list` view (an absent config, OR a
    /// well-formed tunables-only file whose roster is empty) and the daemon's
    /// [`crate::config::Config::require_roster`] precondition (`run` refuses to
    /// start with nothing to rotate across). Both read as "nothing captured yet"
    /// instead of leaking a lower-level "file missing" or "invalid config". An
    /// empty roster is a legitimate state — `capture` loads it to add the first
    /// account (#58) — so it is NOT a validation error. A *malformed* config is
    /// deliberately not remapped: it keeps surfacing as its real
    /// [`Error::ConfigParse`] / [`Error::ConfigInvalid`]. Secret-free.
    #[error("no accounts captured yet — run `sessiometer capture`")]
    RosterEmpty,

    /// `config.toml` is not valid TOML, or a field has the wrong type. The
    /// wrapped message comes from the TOML parser; it is secret-free because the
    /// config file holds no secrets — only labels, account UUIDs, stash names
    /// and integer tunables (issue #15).
    #[error("malformed config: {0}")]
    ConfigParse(String),

    /// A config value is out of range, or the roster is malformed (duplicate
    /// `account_uuid`/`stash`, or an empty field). An empty roster is NOT in this
    /// set — it is a valid state ([`Error::RosterEmpty`] is the daemon/`list`
    /// empty-state, #58). Carries a precise, secret-free message naming the
    /// offending field.
    #[error("invalid config: {0}")]
    ConfigInvalid(String),

    /// The cross-field rule failed: `session_floor` exceeds `session_trigger`
    /// (no account could ever become a swap target, since the floor a candidate
    /// must sit below is itself above the trigger). A distinct variant from
    /// [`Error::ConfigInvalid`] so this case can be matched specifically
    /// (issue #3).
    #[error("invalid config: session_floor ({floor}) must not exceed session_trigger ({trigger})")]
    ConfigFloorAboveTrigger { floor: i64, trigger: i64 },

    /// Claude Code's state file (`~/.claude.json`) does not exist — Claude Code
    /// has not run / no account is logged in, so there is nothing to capture.
    /// Carries the path (a filesystem location, never a secret).
    #[error("no Claude Code state at {path} — log in with `claude` first")]
    ClaudeStateNotFound { path: PathBuf },

    /// `~/.claude.json` is not valid JSON. Only the parser's `line`/`column` are
    /// carried — never the surrounding bytes, which include the account's
    /// `oauthAccount` identity block (issue #15 redaction).
    #[error(
        "malformed Claude Code state (~/.claude.json): JSON error at line {line} column {column}"
    )]
    ClaudeStateParse { line: usize, column: usize },

    /// `~/.claude.json` has no `oauthAccount` object — Claude Code is installed
    /// but no account is logged in, so there is no identity to record.
    #[error("no account is logged in to Claude Code (~/.claude.json has no oauthAccount)")]
    OauthAccountMissing,

    /// The logged-in account's `oauthAccount` is missing a required field (e.g.
    /// `accountUuid`, the roster key). `field` is a static field name, never a
    /// value (issue #15 redaction).
    #[error("the logged-in account is missing its `{field}` — cannot key the roster")]
    OauthAccountFieldMissing { field: &'static str },

    /// A new account was captured without an explicit label. A new account must
    /// be named by the operator (there is deliberately no server-provided
    /// fallback: `displayName` can collide across accounts and `emailAddress` is
    /// redacted, issue #15). Re-capturing an existing account keeps its label, so
    /// this fires only for a brand-new account. Secret-free.
    #[error("a label is required for a new account: pass `sessiometer capture <label>`")]
    LabelRequired,

    // --- Account enable/disable (issue #36) ----------------------------------
    /// `sessiometer disable`/`enable` was invoked without the required `<label>`.
    /// Carries the subcommand (a static, secret-free string) so the message names
    /// the exact usage.
    #[error("a label is required: `sessiometer {verb} <label>`")]
    RotationLabelRequired { verb: &'static str },

    /// `sessiometer disable`/`enable` was given a `<label>` that matches no roster
    /// account. The label is the operator's non-secret handle (issue #15), safe to
    /// quote; the message points at `list` to show the valid handles.
    #[error("no account labelled `{label}` — run `sessiometer list` to see the roster")]
    AccountLabelNotFound { label: String },

    /// A per-account stash is missing one or both of its keychain items
    /// (credential / oauthAccount), so the account cannot be restored. Carries
    /// the `service` (the `Sessiometer/<account_uuid>` stash name — a config value, never
    /// a secret). Surfaced by the swap engine (#6) reading a target's stash.
    #[error(
        "stash `{service}` is incomplete or absent (re-run `sessiometer capture` for this account)"
    )]
    StashIncomplete { service: String },

    // --- Usage polling (issue #5) ---------------------------------------------
    //
    // The HTTP outcome taxonomy for the read-only usage poll, as typed errors so
    // the poll loop (#7) and the 401 monitor (#13) can route each runtime state.
    // All are secret-free: the usage response carries only percentages / reset
    // timestamps (no token, no email), and these variants deliberately echo none
    // of the body — only a structural hint or the HTTP status code.
    /// The stored credential blob has no usable OAuth access token (no
    /// `claudeAiOauth.accessToken`), so there is no bearer to poll with. A
    /// capture/setup problem, not a transient one. Carries nothing — never the
    /// blob bytes (issue #15 redaction).
    #[error("the stored credential has no usable OAuth access token (re-capture this account)")]
    UsageTokenUnreadable,

    /// The poll did not complete: a `5xx` server error, or — when `status` is
    /// `0` — `curl` returned no HTTP response at all (DNS / connection / TLS /
    /// timeout). Transient by the taxonomy (5xx / network): back off and skip the
    /// cycle, never swap on missing data. `retry_after` carries the server-advised
    /// `Retry-After` (a `503` may send one), parsed from its delta-seconds form;
    /// the daemon honours it as a MINIMUM back-off wait (issue #76).
    #[error("usage poll did not complete (HTTP status {status}; 0 means no HTTP response)")]
    UsageTransient {
        status: u16,
        retry_after: Option<Duration>,
    },

    /// The usage endpoint rate-limited the poll (`HTTP 429`). Back off, log, skip
    /// the cycle — never swap on a throttled (missing) reading. `retry_after`
    /// carries the server-advised `Retry-After` (delta-seconds form) when present;
    /// the daemon honours it as a MINIMUM back-off wait (issue #76).
    #[error("usage poll was rate-limited (HTTP {status})")]
    UsageRateLimited {
        status: u16,
        retry_after: Option<Duration>,
    },

    /// A non-401, non-403 `4xx` other than 429 (e.g. `400` / `404` / `422`). Like
    /// 429 on the monitor path (design G4): back off, log, skip — never swap on a
    /// rejected reading. `status` preserves the actual code for the log.
    #[error("usage poll rejected (HTTP {status})")]
    UsageRejected { status: u16 },

    /// The stored access token was rejected with `HTTP 401` (and the consecutive
    /// count has not yet reached `monitor_401_n`). A transient 401 → back off and
    /// log; the re-stash trigger is a separate seam fired at the Nth consecutive
    /// 401 (issue #13 / #6). The poller never self-refreshes a token.
    #[error("usage poll unauthorized (HTTP 401) — the stored token was rejected")]
    UsageUnauthorized,

    /// The token authenticated but lacks the usage scope (`HTTP 403`) — the
    /// hallmark of a non-interactive setup token. Surfaced **distinctly** from a
    /// 401 (issue #5 acceptance): the fix is a fully-scoped re-capture, not a
    /// re-stash/retry.
    #[error(
        "usage poll forbidden (HTTP 403) — the stored token lacks the usage scope \
         (re-capture this account with an interactive login)"
    )]
    UsageScopeMissing,

    /// The poll returned `200` but the body could not be parsed into both quota
    /// dimensions. The wrapped message is a structural hint (a field/shape name)
    /// — never any response bytes. Treated like missing data: skip, never swap.
    #[error("malformed usage response: {0}")]
    UsageParse(String),

    // --- Daemon lifecycle (issue #7) -----------------------------------------
    /// Another `sessiometer` daemon already holds the single-instance lock, so
    /// this `run` must not start a second poll loop. Maps to process exit code
    /// `3` (see [`Error::exit_code`]) so a supervisor can tell "already running"
    /// apart from a generic failure. Secret-free.
    #[error("another sessiometer daemon is already running (the single-instance lock is held)")]
    AlreadyRunning,

    // --- CLI status client (issue #8) ----------------------------------------
    /// `sessiometer status` could not reach a running daemon: the control socket
    /// is absent, or present but refusing connections (no live `run`). A friendly,
    /// user-facing remap of the raw connect failure — the live counterpart to
    /// [`Error::RosterEmpty`] for the offline `list` (#17) — that points the
    /// operator at the next step instead of leaking a connection error. Secret-free.
    #[error("daemon not running — start it with `sessiometer run`")]
    DaemonNotRunning,

    // --- Manual account selection (`sessiometer use`, issue #63) -------------
    //
    // The one-shot `use <account>` verb's own exit conditions, EXTENDING the
    // existing taxonomy (no parallel scheme): a missing/unresolvable/ambiguous
    // target, a pre-swap gate refusal, and the always-enforced keychain-locked
    // abort (which now carries its own exit code — see [`Error::exit_code`]). All
    // are secret-free: each names only the operator's non-secret query/label
    // (issue #15), never a token or email.
    /// `sessiometer use` was invoked without the required `<account>`. There is
    /// deliberately no "cycle to the next account" fallback (out of scope, #63);
    /// a missing target is an error that names the usage.
    #[error("a target is required: `sessiometer use <account>`")]
    UseTargetRequired,

    /// `use <query>` matched no roster account by label OR account-uuid. The
    /// resolver never guesses (issue #17): an unresolvable target is a hard error
    /// with ZERO writes. `query` is the operator's non-secret input.
    #[error("no account matches `{query}` — run `sessiometer list` to see the roster")]
    UseTargetNotFound { query: String },

    /// `use <query>` matched MORE THAN ONE roster account (a duplicated label).
    /// The resolver refuses to guess (issue #17): disambiguate with the
    /// account-uuid. ZERO writes. `query` is the operator's non-secret input.
    #[error("`{query}` is ambiguous: {count} accounts match — disambiguate with the account-uuid")]
    UseTargetAmbiguous { query: String, count: usize },

    /// `use` could not identify the active account to swap AWAY from: no account
    /// is logged in to Claude Code, or the logged-in `oauthAccount.accountUuid`
    /// matches no roster entry. The swap re-stashes the outgoing account, so its
    /// roster identity must be known — mirrors the daemon's "can't identify active
    /// ⇒ don't swap". ZERO writes. Secret-free.
    #[error(
        "cannot determine the active account to swap away from \
         (no logged-in account matches the roster — run `sessiometer list`)"
    )]
    ActiveAccountUnresolved,

    /// The pre-swap gate REFUSED `use <label>` (without `--force`) because the
    /// target's WEEKLY window is exhausted (issue #11/#37 viability). ZERO writes;
    /// `--force` overrides. `label` is the target's non-secret handle.
    #[error(
        "refusing to swap to `{label}`: its weekly window is exhausted — use `--force` to override"
    )]
    UseTargetWeeklyExhausted { label: String },

    /// The pre-swap gate REFUSED `use` (without `--force`) because a swap COOLDOWN
    /// is currently active (issue #10 anti-oscillation). ZERO writes; `--force`
    /// overrides. Secret-free.
    #[error("refusing to swap: a swap cooldown is active — use `--force` to override")]
    UseCooldownActive,

    /// The pre-swap gate REFUSED `use <label>` (without `--force`) because the
    /// target is QUARANTINED — its stored credential is dead and needs a re-login
    /// (issue #42 viability). ZERO writes; `--force` overrides (warn-and-proceed).
    /// `label` is the target's non-secret handle.
    #[error(
        "refusing to swap to `{label}`: it is quarantined and needs re-login — use `--force` to override"
    )]
    UseTargetQuarantined { label: String },

    /// The pre-swap gate could not VERIFY `use <label>`'s viability (issue #75):
    /// with no daemon running to consult a CACHED reading, the single live fallback
    /// poll was rate-limited (`HTTP 429`). Distinct from the daemon-internal
    /// [`Error::UsageRateLimited`] so the operator gets actionable guidance instead
    /// of an opaque abort — start the daemon so the gate reads its cached verdict,
    /// or `--force` to swap anyway. This is an inability to RUN the gate, not a gate
    /// refusal, so it is NOT in the exit-`7` refusal taxonomy — a generic `1`, the
    /// same transient class the raw rate-limit mapped to before. ZERO writes;
    /// `label` is the target's non-secret handle (issue #15).
    #[error(
        "cannot verify `{label}`: the usage check was rate-limited (HTTP 429) and no \
         daemon is running to consult a cached reading — start it with `sessiometer run`, \
         or use `--force` to swap anyway"
    )]
    UseViabilityUnverifiable { label: String },

    // --- Single-writer swap lock (issue #64) ---------------------------------
    /// The single-writer swap lock (issue #64) could not be acquired within the
    /// bounded wait — another swap (a concurrent `use`, or the daemon's own swap
    /// routine) held it the whole time. The lock is FAIL-CLOSED: rather than write
    /// without it and risk a torn canonical/`~/.claude.json` pair, the swap ABORTS
    /// with ZERO writes. Maps to exit `4`, the same "could not write safely, retry
    /// shortly" class as [`Error::KeychainLocked`] (a locked keychain) — see
    /// [`Error::exit_code`]. Secret-free.
    #[error("another swap is in progress — could not acquire the swap lock; retry shortly")]
    SwapLockBusy,

    // --- One-shot `poke` (issue #104) ----------------------------------------
    /// `poke <account>` named the ACTIVE account. The isolated-refresh engine
    /// refreshes only PARKED (non-active) accounts (`src/refresh.rs` Caller
    /// contract): a concurrent promotion of the refreshed account to active cannot
    /// be observed by the engine's CAS re-stash, so the active account is never a
    /// safe target. REFUSED with ZERO effect; `label` is the target's non-secret
    /// handle (issue #15). The all-accounts mode skips the active account silently
    /// instead — this fires only when an operator names it explicitly.
    #[error("refusing to poke `{label}`: it is the active account — poke only refreshes parked accounts")]
    PokeTargetActive { label: String },

    /// The `claude` binary the isolated refresh spawns (issue #102 step 4) could
    /// not be located: `$CLAUDE_BIN` is unset (or not an existing file) and no
    /// `claude` is on `$PATH`. Secret-free — a missing executable, never a
    /// credential.
    #[error(
        "could not locate the `claude` binary — install Claude Code so `claude` is on \
         your PATH, or set `$CLAUDE_BIN` to its absolute path"
    )]
    ClaudeBinaryNotFound,

    /// An underlying I/O failure.
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

impl Error {
    /// The process exit code for this error.
    ///
    /// A held single-instance lock exits `3` ([`Error::AlreadyRunning`], issue
    /// #7) so a second `run` is distinguishable from a generic failure (`1`). The
    /// one-shot `use` verb (issue #63) EXTENDS this same taxonomy — no parallel
    /// scheme — so a caller (or supervisor) can tell its distinct outcomes apart:
    /// a locked keychain (`4`, the always-enforced abort), an unresolvable (`5`)
    /// or ambiguous (`6`) target, and a pre-swap gate refusal without `--force`
    /// (`7`). Every other error is a generic failure (`1`). The mapping lives here
    /// so the `main` exit-code branch stays a thin lookup.
    pub(crate) fn exit_code(&self) -> u8 {
        match self {
            Error::AlreadyRunning => 3,
            // A locked keychain AND a contended swap lock (issue #64) share exit
            // `4`: both are the "could not write safely right now, retry shortly"
            // class — the swap aborted with ZERO writes rather than tear state.
            Error::KeychainLocked { .. } | Error::SwapLockBusy => 4,
            Error::UseTargetNotFound { .. } => 5,
            Error::UseTargetAmbiguous { .. } => 6,
            // The pre-swap gate refused without `--force` — weekly-exhausted,
            // cooldown, or quarantined all share one "gate-refused" code, each
            // with its own specific message.
            Error::UseTargetWeeklyExhausted { .. }
            | Error::UseCooldownActive
            | Error::UseTargetQuarantined { .. } => 7,
            _ => 1,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn already_running_exits_three_so_a_supervisor_can_tell_it_apart() {
        // The "second `run` exits 3" acceptance (issue #7): a held single-instance
        // lock maps to exit code 3, distinct from a generic failure.
        assert_eq!(Error::AlreadyRunning.exit_code(), 3);
    }

    #[test]
    fn every_other_error_is_a_generic_failure() {
        assert_eq!(Error::CredentialNotFound.exit_code(), 1);
        assert_eq!(Error::Unimplemented("x").exit_code(), 1);
        assert_eq!(Error::Io(std::io::Error::other("boom")).exit_code(), 1);
    }

    #[test]
    fn a_contended_swap_lock_shares_the_locked_keychain_exit_code() {
        // Issue #64: a fail-closed swap-lock abort joins the locked keychain in
        // exit `4` — both are the "could not write safely now, ZERO writes, retry
        // shortly" class, distinct from a generic failure (`1`).
        assert_eq!(Error::SwapLockBusy.exit_code(), 4);
        assert_eq!(
            Error::SwapLockBusy.exit_code(),
            Error::KeychainLocked { op: "write" }.exit_code(),
        );
    }

    #[test]
    fn use_verb_extends_the_exit_code_taxonomy_with_distinct_codes() {
        // Issue #63: the `use` verb's new conditions each get their own code,
        // extending the existing taxonomy (no parallel scheme) so a caller can
        // tell them apart from a generic failure (`1`) and from each other.
        assert_eq!(Error::KeychainLocked { op: "read" }.exit_code(), 4);
        assert_eq!(
            Error::UseTargetNotFound {
                query: "ghost".into()
            }
            .exit_code(),
            5
        );
        assert_eq!(
            Error::UseTargetAmbiguous {
                query: "dup".into(),
                count: 2
            }
            .exit_code(),
            6
        );
        // The three gate-refusal reasons share one "gate-refused-without-force" code.
        assert_eq!(
            Error::UseTargetWeeklyExhausted {
                label: "spare".into()
            }
            .exit_code(),
            7
        );
        assert_eq!(Error::UseCooldownActive.exit_code(), 7);
        assert_eq!(
            Error::UseTargetQuarantined {
                label: "spare".into()
            }
            .exit_code(),
            7
        );
        // A missing argument and an unresolvable active account are precondition
        // errors, not part of the named new taxonomy → generic `1`.
        assert_eq!(Error::UseTargetRequired.exit_code(), 1);
        assert_eq!(Error::ActiveAccountUnresolved.exit_code(), 1);
    }

    #[test]
    fn use_verb_error_messages_carry_no_secret_sigil() {
        // Issue #15: every `use` error names only the operator's non-secret
        // query/label, never a token or email.
        let messages = [
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
            Error::ActiveAccountUnresolved.to_string(),
            Error::UseTargetWeeklyExhausted {
                label: "spare".into(),
            }
            .to_string(),
            Error::UseCooldownActive.to_string(),
            Error::UseTargetQuarantined {
                label: "spare".into(),
            }
            .to_string(),
            Error::UseViabilityUnverifiable {
                label: "spare".into(),
            }
            .to_string(),
        ];
        for message in messages {
            assert!(!message.contains('@'), "no email: {message}");
            assert!(
                !message.to_lowercase().contains("token"),
                "no token: {message}"
            );
        }
    }
}
