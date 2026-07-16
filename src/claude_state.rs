// Copyright (c) 2026 Oleksii PELYKH
// SPDX-License-Identifier: MIT

//! Claude Code's per-user state file, `~/.claude.json`.
//!
//! Holds the active account's `oauthAccount` identity block — the second half of
//! an account's credential. The first half is the keychain token (see
//! [`crate::keychain`]); issue #16/R1 established that an account is *both* parts,
//! so [`capture`](crate::capture) records this block alongside the token and the
//! swap engine (#6) restores both when rotating an account in.
//!
//! Only the `oauthAccount` object is read; the rest of the (large) file is
//! ignored. The object is preserved as its canonical JSON bytes so it can be
//! written back on restore, and its `accountUuid` (the roster key) is extracted
//! for the roster. The account is identified by `accountUuid` plus the
//! operator-chosen label — never `displayName` (which two distinct accounts can
//! share — `build/version-compat.md`) or `emailAddress`.
//!
//! The account's `emailAddress` is surfaced through exactly one accessor,
//! [`OauthAccount::email`], for exactly one use: the editable, pre-filled default of
//! the interactive capture label prompt (#447). It is returned in a `Zeroizing`
//! buffer and dropped the instant a label is chosen, and the raw address never
//! reaches an output channel on its own — only a label the operator confirms at that
//! prompt does, and an authored email label is a provenance-scoped exception (#444)
//! to the otherwise secret-free surface. Otherwise nothing here is printed: even the
//! JSON-parse error path carries only a line/column, never the surrounding bytes
//! (issue #15 redaction).

use std::io::ErrorKind;
use std::path::Path;

use serde_json::Value;
use zeroize::Zeroizing;

use crate::error::{Error, Result};
use crate::paths;

/// The `oauthAccount` identity block recorded for a captured account.
///
/// `Clone` (not `Copy`) and intentionally no `Debug`: while not a bearer secret
/// like [`crate::keychain::Credential`], it carries the account's email address,
/// so it must not be casually printable.
#[derive(Clone)]
pub(crate) struct OauthAccount {
    /// The object's canonical JSON bytes, re-serialized from the parsed value
    /// (semantically equivalent to the source, not byte-for-byte the original
    /// file bytes), preserved for restore (#6).
    raw: Vec<u8>,
    /// `accountUuid` — the stable per-account identifier and roster key.
    ///
    /// Deliberately the *only* field extracted for the roster: the account is keyed
    /// and displayed by `accountUuid` + the operator's label, never by `displayName`
    /// (two distinct accounts can share a display name — `build/version-compat.md`)
    /// or `emailAddress` (issue #15 redaction).
    account_uuid: String,
}

impl OauthAccount {
    /// The account's stable identifier (`oauthAccount.accountUuid`); the roster
    /// key and the basis for idempotent re-capture.
    pub(crate) fn account_uuid(&self) -> &str {
        &self.account_uuid
    }

    /// The canonical JSON bytes of the `oauthAccount` object, for stashing.
    pub(crate) fn raw_json(&self) -> &[u8] {
        &self.raw
    }

    /// The account's `emailAddress`, if the identity block carries one — surfaced
    /// for the SINGLE sanctioned use of the harvested email: the editable,
    /// pre-filled default of the interactive capture label prompt (issue #447).
    ///
    /// Returned in a [`Zeroizing`] buffer so the extracted address is wiped as soon
    /// as it drops — the capture path holds it only long enough to seed that one
    /// prompt and drops it once the operator commits a label (#447 AC5). It is never
    /// printed, logged, or emitted otherwise; the value the operator confirms at the
    /// prompt is an *operator-authored* label, which the redaction METER permits as a
    /// provenance-scoped exception (#444) — the raw harvested email itself never
    /// reaches a channel.
    ///
    /// `None` when the block has no non-empty `emailAddress` (only `accountUuid` is
    /// required); the prompt then has no email default and capture falls back to the
    /// uuid-derived label ([`crate::capture`]).
    pub(crate) fn email(&self) -> Option<Zeroizing<String>> {
        // `raw` is canonical JSON by construction (re-serialized from a parsed
        // `Value` in `from_object`), so this parse cannot fail in practice; `.ok()?`
        // degrades to "no email default" rather than panicking on the impossible.
        let value: Value = serde_json::from_slice(&self.raw).ok()?;
        value
            .get("emailAddress")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(|s| Zeroizing::new(s.to_owned()))
    }

    /// Build from the bytes of a serialized `oauthAccount` *object* (not the
    /// whole file). Reconstructs a stashed identity on read-back (#6).
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn from_object_bytes(bytes: &[u8]) -> Result<Self> {
        let value: Value = serde_json::from_slice(bytes).map_err(parse_error)?;
        Self::from_object(value)
    }

    /// Build from an already-parsed `oauthAccount` value: require it be an object
    /// with a non-empty `accountUuid`, then re-serialize to canonical bytes.
    fn from_object(value: Value) -> Result<Self> {
        if !value.is_object() {
            return Err(Error::OauthAccountMissing);
        }
        let account_uuid = string_field(&value, "accountUuid")?;
        if account_uuid.trim().is_empty() {
            return Err(Error::OauthAccountFieldMissing {
                field: "accountUuid",
            });
        }
        // Re-serialize the validated object to canonical bytes. `to_vec` on a
        // parsed `Value` (finite numbers, string keys) cannot fail. serde_json's
        // default `Map` is ordered, so this is deterministic across round-trips.
        let raw = serde_json::to_vec(&value).expect("serializing a parsed JSON value");
        Ok(Self { raw, account_uuid })
    }
}

/// Read `~/.claude.json` at an explicit `path` and extract the active account's
/// `oauthAccount` block — the injectable seam, so the not-found / malformed /
/// no-account branches are testable without touching the real `~/.claude.json`.
///
/// Crate-visible because both the `capture` path (under the swap lock, #357) and the
/// daemon (#7) read a configurable `~/.claude.json` path through this — the daemon to
/// identify the active account each cycle and to reconcile-on-start — so their tests can
/// point it at a temp file.
pub(crate) fn read_oauth_account_from(path: &Path) -> Result<OauthAccount> {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(err) if err.kind() == ErrorKind::NotFound => {
            return Err(Error::ClaudeStateNotFound {
                path: path.to_path_buf(),
            });
        }
        Err(err) => return Err(Error::Io(err)),
    };
    let root: Value = serde_json::from_slice(&bytes).map_err(parse_error)?;
    let oauth = root
        .get("oauthAccount")
        .cloned()
        .ok_or(Error::OauthAccountMissing)?;
    OauthAccount::from_object(oauth)
}

/// Co-write `oauth` into `~/.claude.json` as its `oauthAccount`, preserving every
/// other field and the file's existing permission mode.
///
/// The swap engine's honest-display co-write (#6): a field-preserving
/// read-modify-write. The whole file is parsed into a `serde_json::Value`, **only**
/// the `oauthAccount` key is replaced (or inserted, if absent), and the result is
/// written back atomically through [`paths::write_preserving_mode`] (same-directory
/// temp, `fsync`, `rename`, copied mode — never our `0600`, since this file is
/// Claude Code's). Last-writer-wins is acceptable: the keychain token is the
/// authoritative bearer, so a clobbered `oauthAccount` self-heals on the next
/// reconcile.
///
/// Nothing here is printed — the file and `oauth` both carry the account's email;
/// the parse-error path carries only a line/column, never the surrounding bytes
/// (issue #15 redaction). Wired into the swap loop in #7.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn write_oauth_account(path: &Path, oauth: &OauthAccount) -> Result<()> {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(err) if err.kind() == ErrorKind::NotFound => {
            return Err(Error::ClaudeStateNotFound {
                path: path.to_path_buf(),
            });
        }
        Err(err) => return Err(Error::Io(err)),
    };
    let mut root: Value = serde_json::from_slice(&bytes).map_err(parse_error)?;
    // The root must be a JSON object to host `oauthAccount`. A non-object root is a
    // corrupt/unexpected state file — there is no identity slot to write into.
    let obj = root.as_object_mut().ok_or(Error::OauthAccountMissing)?;
    // `oauth.raw_json()` is the canonical bytes of an already-validated object, so
    // this parse cannot fail; `insert` replaces an existing `oauthAccount` or adds
    // it, leaving every sibling key untouched.
    let value: Value =
        serde_json::from_slice(oauth.raw_json()).expect("oauthAccount raw JSON is always valid");
    obj.insert("oauthAccount".to_owned(), value);
    // Re-serialize the whole mutated document; serializing a parsed `Value` cannot
    // fail. Field order may be normalized — semantically irrelevant for JSON, and
    // the AC requires preserving fields, not their order.
    let serialized = serde_json::to_vec(&root).expect("serializing a parsed JSON value");
    paths::write_preserving_mode(path, &serialized)
}

/// Extract a required string field, mapping absence (or a non-string value) to
/// the typed field-missing error — never echoing the value (issue #15).
fn string_field(value: &Value, field: &'static str) -> Result<String> {
    value
        .get(field)
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or(Error::OauthAccountFieldMissing { field })
}

/// Map a `serde_json` error to the secret-free [`Error::ClaudeStateParse`],
/// carrying only its line/column — never the surrounding bytes, which hold the
/// account's identity block (issue #15 redaction).
fn parse_error(err: serde_json::Error) -> Error {
    Error::ClaudeStateParse {
        line: err.line(),
        column: err.column(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A representative `~/.claude.json` fragment: the `oauthAccount` object plus
    /// unrelated top-level keys that must be ignored.
    const CLAUDE_JSON: &str = r#"{
        "numStartups": 42,
        "oauthAccount": {
            "accountUuid": "11111111-1111-1111-1111-111111111111",
            "emailAddress": "person@example.com",
            "displayName": "Work Account",
            "organizationName": "Acme"
        },
        "projects": {}
    }"#;

    fn write_temp(contents: &str) -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".claude.json");
        std::fs::write(&path, contents).unwrap();
        (dir, path)
    }

    #[test]
    fn extracts_uuid_ignoring_other_keys() {
        let (_dir, path) = write_temp(CLAUDE_JSON);
        let oauth = read_oauth_account_from(&path).unwrap();
        assert_eq!(oauth.account_uuid(), "11111111-1111-1111-1111-111111111111");
    }

    #[test]
    fn raw_json_round_trips_through_object_bytes() {
        let (_dir, path) = write_temp(CLAUDE_JSON);
        let oauth = read_oauth_account_from(&path).unwrap();
        // The stash stores raw_json(); read-back reconstructs an equivalent
        // OauthAccount with identical canonical bytes (deterministic ordering).
        let reread = OauthAccount::from_object_bytes(oauth.raw_json()).unwrap();
        assert_eq!(reread.raw_json(), oauth.raw_json());
        assert_eq!(reread.account_uuid(), oauth.account_uuid());
        // The preserved object still carries the (never-printed) email for #6.
        let value: Value = serde_json::from_slice(oauth.raw_json()).unwrap();
        assert_eq!(value["emailAddress"], "person@example.com");
    }

    #[test]
    fn an_account_without_a_display_name_still_extracts() {
        // Only `accountUuid` is required; `displayName` is never used, so its
        // absence must not block capture (guards against re-introducing a
        // displayName dependency — `build/version-compat.md`).
        let json = r#"{"oauthAccount":{"accountUuid":"u-1"}}"#;
        let (_dir, path) = write_temp(json);
        let oauth = read_oauth_account_from(&path).unwrap();
        assert_eq!(oauth.account_uuid(), "u-1");
    }

    #[test]
    fn email_surfaces_the_address_for_the_capture_prompt_default() {
        // #447: the harvested email is offered as the pre-filled label default.
        let (_dir, path) = write_temp(CLAUDE_JSON);
        let oauth = read_oauth_account_from(&path).unwrap();
        assert_eq!(oauth.email().unwrap().as_str(), "person@example.com");
    }

    #[test]
    fn email_is_none_when_the_block_carries_no_address() {
        // `emailAddress` is not required (only `accountUuid` is); no default then,
        // and capture falls back to the uuid-derived label (#447).
        let json = r#"{"oauthAccount":{"accountUuid":"u-1"}}"#;
        let (_dir, path) = write_temp(json);
        let oauth = read_oauth_account_from(&path).unwrap();
        assert!(oauth.email().is_none());
    }

    #[test]
    fn email_is_none_when_the_address_is_blank() {
        // A whitespace-only `emailAddress` is not a usable default — treat as absent.
        let json = r#"{"oauthAccount":{"accountUuid":"u-1","emailAddress":"   "}}"#;
        let (_dir, path) = write_temp(json);
        let oauth = read_oauth_account_from(&path).unwrap();
        assert!(oauth.email().is_none());
    }

    #[test]
    fn missing_file_is_not_found() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".claude.json");
        assert!(matches!(
            read_oauth_account_from(&path),
            Err(Error::ClaudeStateNotFound { .. })
        ));
    }

    #[test]
    fn malformed_json_reports_line_and_column_only() {
        let (_dir, path) = write_temp("{ not json");
        // Note: `OauthAccount` has no `Debug` (it carries the email), so the Ok
        // arm cannot format the value — assert the variant directly instead.
        match read_oauth_account_from(&path) {
            Err(Error::ClaudeStateParse { line, column }) => {
                assert!(line >= 1 && column >= 1);
            }
            Err(other) => panic!("expected ClaudeStateParse, got {other:?}"),
            Ok(_) => panic!("expected ClaudeStateParse, got Ok"),
        }
    }

    #[test]
    fn no_oauth_account_object_is_missing() {
        let (_dir, path) = write_temp(r#"{"numStartups": 1}"#);
        assert!(matches!(
            read_oauth_account_from(&path),
            Err(Error::OauthAccountMissing)
        ));
    }

    #[test]
    fn oauth_account_without_uuid_is_field_missing() {
        let (_dir, path) = write_temp(r#"{"oauthAccount":{"displayName":"x"}}"#);
        assert!(matches!(
            read_oauth_account_from(&path),
            Err(Error::OauthAccountFieldMissing {
                field: "accountUuid"
            })
        ));
    }

    #[test]
    fn empty_uuid_is_field_missing() {
        let (_dir, path) = write_temp(r#"{"oauthAccount":{"accountUuid":"   "}}"#);
        assert!(matches!(
            read_oauth_account_from(&path),
            Err(Error::OauthAccountFieldMissing {
                field: "accountUuid"
            })
        ));
    }

    #[test]
    fn non_object_oauth_account_is_missing() {
        let (_dir, path) = write_temp(r#"{"oauthAccount":"not-an-object"}"#);
        assert!(matches!(
            read_oauth_account_from(&path),
            Err(Error::OauthAccountMissing)
        ));
    }

    // --- write_oauth_account (the swap co-write, #6) ---

    #[test]
    fn write_oauth_account_replaces_only_the_oauth_block_and_preserves_other_fields() {
        let (_dir, path) = write_temp(CLAUDE_JSON);
        let incoming = OauthAccount::from_object_bytes(
            br#"{"accountUuid":"22222222-2222-2222-2222-222222222222","emailAddress":"new@example.com"}"#,
        )
        .unwrap();

        write_oauth_account(&path, &incoming).unwrap();

        let v: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        // The oauthAccount block is now the incoming account's…
        assert_eq!(
            v["oauthAccount"]["accountUuid"],
            "22222222-2222-2222-2222-222222222222"
        );
        assert_eq!(v["oauthAccount"]["emailAddress"], "new@example.com");
        // …the whole block was REPLACED (not merged), so the old account's
        // displayName/organizationName are gone…
        assert!(v["oauthAccount"].get("displayName").is_none());
        assert!(v["oauthAccount"].get("organizationName").is_none());
        // …and every UNRELATED top-level field is preserved verbatim.
        assert_eq!(v["numStartups"], 42);
        assert!(v["projects"].is_object());
    }

    #[test]
    fn write_oauth_account_preserves_the_files_existing_mode() {
        use std::os::unix::fs::PermissionsExt;
        let (_dir, path) = write_temp(CLAUDE_JSON);
        // Claude Code owns this file; give it a non-0600 mode and prove the
        // co-write does NOT force 0600 (unlike `paths::write_private_file`).
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();

        let incoming = OauthAccount::from_object_bytes(br#"{"accountUuid":"u-2"}"#).unwrap();
        write_oauth_account(&path, &incoming).unwrap();

        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(
            mode, 0o644,
            "the co-write must preserve ~/.claude.json's mode, not force 0600"
        );
        let v: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(v["oauthAccount"]["accountUuid"], "u-2");
    }

    #[test]
    fn write_oauth_account_inserts_when_the_block_is_absent() {
        let (_dir, path) = write_temp(r#"{"numStartups":1}"#);
        let incoming = OauthAccount::from_object_bytes(br#"{"accountUuid":"u-new"}"#).unwrap();

        write_oauth_account(&path, &incoming).unwrap();

        let v: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(v["oauthAccount"]["accountUuid"], "u-new");
        assert_eq!(v["numStartups"], 1);
    }

    #[test]
    fn write_oauth_account_reports_not_found_for_a_missing_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".claude.json");
        let incoming = OauthAccount::from_object_bytes(br#"{"accountUuid":"u-2"}"#).unwrap();
        assert!(matches!(
            write_oauth_account(&path, &incoming),
            Err(Error::ClaudeStateNotFound { .. })
        ));
    }

    #[test]
    fn write_oauth_account_rejects_a_non_object_root() {
        let (_dir, path) = write_temp("[1,2,3]");
        let incoming = OauthAccount::from_object_bytes(br#"{"accountUuid":"u"}"#).unwrap();
        assert!(matches!(
            write_oauth_account(&path, &incoming),
            Err(Error::OauthAccountMissing)
        ));
    }

    #[test]
    fn write_oauth_account_reports_parse_error_for_malformed_json() {
        let (_dir, path) = write_temp("{ not json");
        let incoming = OauthAccount::from_object_bytes(br#"{"accountUuid":"u"}"#).unwrap();
        assert!(matches!(
            write_oauth_account(&path, &incoming),
            Err(Error::ClaudeStateParse { .. })
        ));
    }
}
