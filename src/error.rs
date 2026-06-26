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

    /// An underlying I/O failure.
    #[error(transparent)]
    Io(#[from] std::io::Error),
}
