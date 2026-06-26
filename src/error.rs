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

    /// An underlying I/O failure.
    #[error(transparent)]
    Io(#[from] std::io::Error),
}
