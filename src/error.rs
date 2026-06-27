// Copyright (c) 2026 Oleksii PELYKH
// SPDX-License-Identifier: MIT

//! Crate-wide error type.
//!
//! Invariant: an [`Error`] value never carries secret material (OAuth blobs,
//! keychain secrets). Its `Display` and `Debug` are therefore safe to print on
//! any channel — the foundation the output-redaction work (issue #15) builds
//! on.

use std::path::PathBuf;

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

    /// No `config.toml` exists yet at the expected path. Carries the path (a
    /// filesystem location, never a secret) so the message can name it.
    #[error("no config file at {path} — run `sessiometer capture` to create one")]
    ConfigNotFound { path: PathBuf },

    /// `sessiometer list` found no roster to show. A friendly, user-facing remap
    /// of [`Error::ConfigNotFound`] for the read-only `list` view: an absent
    /// config is the only real empty state (`capture` cannot persist a 0-account
    /// roster — it fails the `1..=5` load validation), so this reads as "nothing
    /// captured yet" instead of leaking the lower-level "file missing". A
    /// malformed config is deliberately NOT remapped — it keeps surfacing as its
    /// real [`Error::ConfigParse`] / [`Error::ConfigInvalid`]. Secret-free.
    #[error("no accounts captured yet — run `sessiometer capture`")]
    RosterEmpty,

    /// `config.toml` is not valid TOML, or a field has the wrong type. The
    /// wrapped message comes from the TOML parser; it is secret-free because the
    /// config file holds no secrets — only labels, account UUIDs, stash names
    /// and integer tunables (issue #15).
    #[error("malformed config: {0}")]
    ConfigParse(String),

    /// A config value is out of range, or the roster is malformed (wrong size,
    /// duplicate `account_uuid`/`stash`, or an empty field). Carries a precise,
    /// secret-free message naming the offending field.
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

    /// The rotation is already full and the active account is not one of its
    /// members, so it cannot be added. Re-capture an existing member to refresh
    /// it instead. Carries only the limit (an integer, never a secret).
    #[error(
        "rotation is full ({max} accounts): re-capture one already in rotation, \
         or remove one before capturing a new account"
    )]
    RotationFull { max: usize },

    /// A new account was captured without an explicit label. A new account must
    /// be named by the operator (there is deliberately no server-provided
    /// fallback: `displayName` can collide across accounts and `emailAddress` is
    /// redacted, issue #15). Re-capturing an existing account keeps its label, so
    /// this fires only for a brand-new account. Secret-free.
    #[error("a label is required for a new account: pass `sessiometer capture <label>`")]
    LabelRequired,

    /// A per-account stash is missing one or both of its keychain items
    /// (credential / oauthAccount), so the account cannot be restored. Carries
    /// the `service` (the `Sessiometer/acct-N` stash name — a config value, never
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
    /// cycle, never swap on missing data.
    #[error("usage poll did not complete (HTTP status {status}; 0 means no HTTP response)")]
    UsageTransient { status: u16 },

    /// The usage endpoint rate-limited the poll (`HTTP 429`). Back off, log, skip
    /// the cycle — never swap on a throttled (missing) reading.
    #[error("usage poll was rate-limited (HTTP {status})")]
    UsageRateLimited { status: u16 },

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

    /// An underlying I/O failure.
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

impl Error {
    /// The process exit code for this error.
    ///
    /// A held single-instance lock exits `3` ([`Error::AlreadyRunning`], issue
    /// #7) so a second `run` is distinguishable from a generic failure (`1`);
    /// every other error is a generic failure. The mapping lives here so the
    /// `main` exit-code branch (and any future supervisor) stays a thin lookup.
    pub(crate) fn exit_code(&self) -> u8 {
        match self {
            Error::AlreadyRunning => 3,
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
}
