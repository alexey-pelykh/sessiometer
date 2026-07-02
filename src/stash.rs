// Copyright (c) 2026 Oleksii PELYKH
// SPDX-License-Identifier: MIT

//! Per-account credential stash in the login keychain.
//!
//! Each captured account's restorable state lives under the keychain service
//! named by its roster `stash` field (`Sessiometer/<account_uuid>`), as two
//! generic-password items distinguished by their `acct` attribute:
//!   - `acct = "credential"` — the raw `Claude Code-credentials` blob, stored
//!     byte-identical (it mirrors the canonical item, which issue #16/H1 verified
//!     a namespaced stash survives a subsequent `claude /login` byte-for-byte).
//!     The blob is printable UTF-8 JSON, so it round-trips through the CLI as
//!     text — the same property [`crate::keychain`] relies on for the canonical
//!     item.
//!   - `acct = "oauthAccount"` — the account's `~/.claude.json` identity block,
//!     **hex-encoded** before storage (see [`crate::claude_state`] for the source
//!     bytes). Encoding is required, not cosmetic: `find-generic-password -w`
//!     renders a secret containing any byte ≥ `0x80` as hex rather than text, and
//!     this block can hold non-ASCII names/organizations — hex-encoding keeps the
//!     stored value pure-ASCII so the read-back is byte-exact regardless of
//!     content and independent of the CLI's text-vs-hex heuristic.
//!
//! Both halves are needed to restore an account, so [`capture`](crate::capture)
//! writes both and [`AccountStash::read`] requires both. This is the clean,
//! reusable primitive the out-of-band swap (#6) drives — it reads the target's
//! stash and re-stashes the outgoing account through this same `write` — which is
//! why it is a standalone module rather than inline in the `capture` command.
//!
//! Like [`crate::keychain`], all access is through the `/usr/bin/security` CLI at
//! its absolute path — never the Security.framework SDK (a CI guard,
//! `scripts/check-no-security-framework.sh`, enforces this). Unlike the canonical
//! item, the `acct` here is chosen by us, so there is no resolve/uniqueness step;
//! and because these items are read only by sessiometer (never by Claude Code),
//! their ACL identity is irrelevant — the `apple-tool:` preservation that matters
//! for the canonical item does not apply here.

use std::ffi::OsString;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::process::Stdio;

#[cfg(test)]
use std::cell::RefCell;
#[cfg(test)]
use std::collections::HashMap;

use tokio::process::Command;
use zeroize::{Zeroize, Zeroizing};

use crate::claude_state::OauthAccount;
use crate::error::{Error, Result};
use crate::keychain::Credential;
use crate::paths;

/// Absolute path to the system `security` tool — absolute (not `$PATH`-resolved)
/// so a hijacked `PATH` cannot substitute a different binary for this
/// security-sensitive call. Mirrors [`crate::keychain`]'s constant; kept local to
/// avoid coupling the two keychain modules.
const SECURITY: &str = "/usr/bin/security";

/// The `acct` attribute under which the raw credential blob is stored.
const ACCT_CREDENTIAL: &str = "credential";
/// The `acct` attribute under which the `oauthAccount` JSON is stored.
const ACCT_OAUTH: &str = "oauthAccount";

/// Both halves of a captured account, as stashed under one `Sessiometer/<account_uuid>`
/// service. `Clone` so the in-memory test fake can hand back copies.
#[derive(Clone)]
pub(crate) struct StashedAccount {
    /// The raw `Claude Code-credentials` blob (the bearer token).
    pub(crate) credential: Credential,
    /// The account's `~/.claude.json` `oauthAccount` identity block.
    pub(crate) oauth_account: OauthAccount,
}

/// Reads/writes a per-account stash. The real impl drives the macOS `security`
/// CLI; the test impl is an in-memory map.
///
/// `read` is consumed by the swap engine (#6) and the round-trip test, not yet by
/// any live `capture`-only path — hence `dead_code` is allowed at the trait
/// level, mirroring [`crate::keychain::CredentialStore`].
#[allow(dead_code)]
pub(crate) trait AccountStash {
    /// Stash both halves of `account` under `service` (idempotent: `-U` updates
    /// an existing item in place, so re-capture refreshes the stash).
    async fn write(&self, service: &str, account: &StashedAccount) -> Result<()>;
    /// Read both halves back, or [`Error::StashIncomplete`] if either is absent.
    async fn read(&self, service: &str) -> Result<StashedAccount>;
    /// Delete both halves of the stash under `service` — account removal (issue
    /// #13). Idempotent: an already-absent half is treated as success (the
    /// post-condition "the stash is gone" holds), so a partially-written stash and
    /// a re-run both delete cleanly. Surfaces [`Error::KeychainLocked`] if the
    /// keychain is locked.
    async fn delete(&self, service: &str) -> Result<()>;
}

/// Real keychain-backed stash, driving `/usr/bin/security`.
pub(crate) struct RealAccountStash {
    /// Keychain to operate on. `None` is production (the login keychain via
    /// [`paths::login_keychain`]); `Some` pins a specific keychain file for the
    /// round-trip test, which drives the real CLI against a throwaway keychain.
    keychain: Option<PathBuf>,
}

impl RealAccountStash {
    /// Production stash, operating on the login keychain.
    pub(crate) fn new() -> Self {
        Self { keychain: None }
    }

    /// Stash pinned to a specific keychain file (round-trip test only).
    #[cfg(all(test, target_os = "macos"))]
    pub(crate) fn for_keychain(path: PathBuf) -> Self {
        Self {
            keychain: Some(path),
        }
    }

    /// The keychain path to pin on every call.
    fn keychain_path(&self) -> Result<PathBuf> {
        match &self.keychain {
            Some(kc) => Ok(kc.clone()),
            None => paths::login_keychain(),
        }
    }

    /// `add-generic-password -U` one item, storing `payload` as the secret —
    /// driven through `security -i` (the command on stdin) so `payload` never
    /// reaches this process's argv (issue #39). This matters most for the
    /// `oauthAccount` half, whose hex carries the account's email: the previously
    /// accepted argv exposure (wider than #2's opaque-token case) is now closed.
    async fn add_item(
        &self,
        service: &str,
        acct: &str,
        keychain: &Path,
        payload: &[u8],
    ) -> Result<()> {
        // `line` (the command, payload included) is the only heap copy of the
        // escaped secret and is `Zeroizing`; only `-i` ever reaches argv.
        let line = write_item_command_line(service, acct, keychain, payload);
        let output = run_interactive_write(&line).await?;
        if output.status.success() {
            Ok(())
        } else {
            Err(stash_error(
                "stash write",
                output.status.code().unwrap_or(-1),
            ))
        }
    }

    /// `find-generic-password -w` one item, returning its stored value bytes (the
    /// single trailing newline `-w` appends stripped). Callers that hex-encoded on
    /// write must hex-decode this; the credential half is stored raw and used
    /// as-is. On failure the (possibly secret-bearing) stdout is zeroized before a
    /// typed error is returned, mirroring [`crate::keychain`]'s read hygiene.
    async fn find_item(&self, service: &str, acct: &str, keychain: &Path) -> Result<Vec<u8>> {
        let output = Command::new(SECURITY)
            .args(read_item_args(service, acct, keychain))
            .stdin(Stdio::null())
            .output()
            .await?;
        if output.status.success() {
            Ok(strip_one_trailing_newline(output.stdout))
        } else {
            let mut stdout = output.stdout;
            stdout.zeroize();
            let code = output.status.code().unwrap_or(-1);
            // 44 == errSecItemNotFound: this half of the stash is absent, so the
            // stash is incomplete (or was never written).
            if code == 44 {
                Err(Error::StashIncomplete {
                    service: service.to_owned(),
                })
            } else {
                Err(stash_error("stash read", code))
            }
        }
    }

    /// `delete-generic-password` one item, tolerating an absent one. Unlike
    /// `add_item` there is no secret payload (service + acct are non-secret config
    /// values), so this is a plain argv call — no `security -i` / #39 concern. A
    /// `44` (`errSecItemNotFound`) is mapped to `Ok`: the post-condition "the item
    /// is gone" already holds, so deletes are idempotent. `36` maps to
    /// [`Error::KeychainLocked`] via [`stash_error`].
    async fn delete_item(&self, service: &str, acct: &str, keychain: &Path) -> Result<()> {
        let output = Command::new(SECURITY)
            .args(delete_item_args(service, acct, keychain))
            .stdin(Stdio::null())
            .output()
            .await?;
        if output.status.success() {
            return Ok(());
        }
        let code = output.status.code().unwrap_or(-1);
        if code == 44 {
            Ok(()) // already absent → idempotent success
        } else {
            Err(stash_error("stash delete", code))
        }
    }
}

impl AccountStash for RealAccountStash {
    async fn write(&self, service: &str, account: &StashedAccount) -> Result<()> {
        let keychain = self.keychain_path()?;
        // Credential first, then identity. The roster entry is written by
        // `capture` only after this returns, so a crash mid-write never leaves a
        // roster pointing at a half-stash for a *new* account; a refresh is
        // simply re-runnable.
        //
        // The credential blob is printable ASCII (Claude OAuth JSON) so it is
        // stored raw — byte-identical to the canonical item. The oauthAccount can
        // be non-ASCII, so it is hex-encoded to keep the stored secret pure-ASCII
        // (see the module doc on `security -w`'s text-vs-hex rendering).
        self.add_item(
            service,
            ACCT_CREDENTIAL,
            &keychain,
            account.credential.expose(),
        )
        .await?;
        self.add_item(
            service,
            ACCT_OAUTH,
            &keychain,
            crate::hex::encode(account.oauth_account.raw_json()).as_bytes(),
        )
        .await?;
        Ok(())
    }

    async fn read(&self, service: &str) -> Result<StashedAccount> {
        let keychain = self.keychain_path()?;
        let credential =
            Credential::new(self.find_item(service, ACCT_CREDENTIAL, &keychain).await?);
        let oauth_hex = self.find_item(service, ACCT_OAUTH, &keychain).await?;
        // We always write valid hex, so a decode failure means the stored item was
        // truncated or tampered with — treat that as an unusable stash.
        let oauth_bytes = crate::hex::decode(&oauth_hex).ok_or_else(|| Error::StashIncomplete {
            service: service.to_owned(),
        })?;
        let oauth_account = OauthAccount::from_object_bytes(&oauth_bytes)?;
        Ok(StashedAccount {
            credential,
            oauth_account,
        })
    }

    async fn delete(&self, service: &str) -> Result<()> {
        let keychain = self.keychain_path()?;
        // Delete both halves. A missing half is tolerated (`delete_item` maps
        // not-found to Ok), so a partially-written stash still deletes cleanly and
        // the operation is safe to re-run.
        self.delete_item(service, ACCT_CREDENTIAL, &keychain)
            .await?;
        self.delete_item(service, ACCT_OAUTH, &keychain).await?;
        Ok(())
    }
}

/// Append `token` to `out` double-quoted and backslash-escaped for the
/// `security -i` interactive tokenizer (escape `\` → `\\` and `"` → `\"`, then
/// wrap in `"…"`). The tokenizer is **not** a shell — whitespace, `$`, backticks,
/// `;`, `|` are literal inside the quotes — so this carries an arbitrary
/// single-line byte string as one argument (issue #39). Mirrors
/// [`crate::keychain`]'s helper, kept local to avoid coupling the two modules.
fn push_quoted(out: &mut Vec<u8>, token: &[u8]) {
    out.push(b'"');
    for &b in token {
        if b == b'\\' || b == b'"' {
            out.push(b'\\');
        }
        out.push(b);
    }
    out.push(b'"');
}

/// The `security -i` stdin line for one stash write: an in-place (`-U`)
/// `add-generic-password` of `(service, acct)`, pinning the keychain, every field
/// double-quoted (incl. the payload). Fed on stdin so the payload stays off argv
/// (issue #39). The returned buffer holds the secret, so it is `Zeroizing`.
fn write_item_command_line(
    service: &str,
    acct: &str,
    keychain: &Path,
    payload: &[u8],
) -> Zeroizing<Vec<u8>> {
    // Line-based reader: a newline in `payload` would truncate the command. The
    // stored halves never contain one (the credential is single-line OAuth JSON;
    // the oauthAccount half is pure-ASCII hex) — and if one ever did, `security`
    // exits non-zero and the caller surfaces it, never a silent partial write.
    debug_assert!(
        !payload.contains(&b'\n'),
        "interactive command line is newline-delimited"
    );
    let mut line = Vec::new();
    line.extend_from_slice(b"add-generic-password -U -s ");
    push_quoted(&mut line, service.as_bytes());
    line.extend_from_slice(b" -a ");
    push_quoted(&mut line, acct.as_bytes());
    line.extend_from_slice(b" -w ");
    push_quoted(&mut line, payload);
    line.push(b' ');
    push_quoted(&mut line, keychain.as_os_str().as_bytes());
    line.push(b'\n');
    Zeroizing::new(line)
}

/// Run one off-argv write: spawn `security -i` (argv is only `-i` — the payload
/// rides stdin, never the process command line, issue #39), feed `line`, close
/// stdin so the CLI hits EOF and exits, and collect the result. Mirrors
/// [`crate::keychain`]'s helper, kept local to avoid coupling the two modules.
async fn run_interactive_write(line: &[u8]) -> Result<std::process::Output> {
    use tokio::io::AsyncWriteExt;
    let mut child = Command::new(SECURITY)
        .arg("-i")
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()?;
    // One small (< pipe-buffer) write — no stdin/stderr deadlock risk; dropping
    // the handle closes the pipe → EOF.
    child
        .stdin
        .take()
        .expect("stdin was piped")
        .write_all(line)
        .await?;
    Ok(child.wait_with_output().await?)
}

/// `find-generic-password` arguments (after the program name): read the secret of
/// `(service, acct)`, pinning the keychain path.
fn read_item_args(service: &str, acct: &str, keychain: &Path) -> Vec<OsString> {
    vec![
        "find-generic-password".into(),
        "-w".into(),
        "-s".into(),
        service.into(),
        "-a".into(),
        acct.into(),
        keychain.as_os_str().to_owned(),
    ]
}

/// `delete-generic-password` arguments (after the program name): delete the item
/// `(service, acct)`, pinning the keychain path. No `-w` / payload — delete needs
/// only the non-secret identifiers (issue #13 account removal).
fn delete_item_args(service: &str, acct: &str, keychain: &Path) -> Vec<OsString> {
    vec![
        "delete-generic-password".into(),
        "-s".into(),
        service.into(),
        "-a".into(),
        acct.into(),
        keychain.as_os_str().to_owned(),
    ]
}

/// Strip the single trailing newline `find-generic-password -w` appends, so a
/// write→read round-trip returns the stored bytes exactly. Only one is removed:
/// a newline that is genuinely part of the stored payload is preserved.
fn strip_one_trailing_newline(mut bytes: Vec<u8>) -> Vec<u8> {
    if bytes.last() == Some(&b'\n') {
        bytes.pop();
    }
    bytes
}

/// Map a non-zero `security` exit `code` to a typed error. `36` is
/// `errSecInteractionNotAllowed` (locked keychain); other codes are surfaced
/// generically. (`44`/not-found is handled at the call site, where the stash
/// service name is in scope for [`Error::StashIncomplete`].)
fn stash_error(op: &'static str, code: i32) -> Error {
    match code {
        36 => Error::KeychainLocked { op },
        _ => Error::Keychain { op, code },
    }
}

/// In-memory stash for hermetic tests of `capture`'s wiring.
#[cfg(test)]
pub(crate) struct FakeAccountStash {
    items: RefCell<HashMap<String, StashedAccount>>,
}

#[cfg(test)]
impl FakeAccountStash {
    pub(crate) fn empty() -> Self {
        Self {
            items: RefCell::new(HashMap::new()),
        }
    }

    /// How many distinct stash services have been written.
    pub(crate) fn len(&self) -> usize {
        self.items.borrow().len()
    }

    /// Whether `service` has been stashed.
    pub(crate) fn contains(&self, service: &str) -> bool {
        self.items.borrow().contains_key(service)
    }
}

#[cfg(test)]
impl AccountStash for FakeAccountStash {
    async fn write(&self, service: &str, account: &StashedAccount) -> Result<()> {
        self.items
            .borrow_mut()
            .insert(service.to_owned(), account.clone());
        Ok(())
    }

    async fn read(&self, service: &str) -> Result<StashedAccount> {
        self.items
            .borrow()
            .get(service)
            .cloned()
            .ok_or(Error::StashIncomplete {
                service: service.to_owned(),
            })
    }

    async fn delete(&self, service: &str) -> Result<()> {
        // Idempotent: removing an absent service is a no-op success.
        self.items.borrow_mut().remove(service);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn push_quoted_wraps_and_escapes_only_backslash_and_quote() {
        let mut out = Vec::new();
        push_quoted(&mut out, b"plain");
        assert_eq!(out, b"\"plain\"");

        // `"` → `\"` and `\` → `\\`, wrapped in quotes.
        let mut out = Vec::new();
        push_quoted(&mut out, br#"a"b\c"#);
        assert_eq!(out, br#""a\"b\\c""#);

        // Whitespace and shell metacharacters are literal (not a shell).
        let mut out = Vec::new();
        push_quoted(&mut out, b"a b$c`d;e|f&g");
        assert_eq!(out, b"\"a b$c`d;e|f&g\"");
    }

    #[test]
    fn write_item_command_line_quotes_every_field_and_keeps_payload_off_argv() {
        let kc = Path::new("/tmp/login.keychain-db");
        let line = write_item_command_line(
            "Sessiometer/11111111-1111-1111-1111-111111111111",
            ACCT_CREDENTIAL,
            kc,
            br#"blob "x" \y"#,
        );
        let expected = format!(
            "add-generic-password -U -s \"Sessiometer/11111111-1111-1111-1111-111111111111\" -a \"credential\" -w \"blob \\\"x\\\" \\\\y\" \"{}\"\n",
            kc.display()
        );
        assert_eq!(&line[..], expected.as_bytes());
    }

    #[test]
    fn read_args_pin_service_acct_and_keychain() {
        let kc = Path::new("/tmp/login.keychain-db");
        assert_eq!(
            read_item_args(
                "Sessiometer/22222222-2222-2222-2222-222222222222",
                ACCT_OAUTH,
                kc
            ),
            vec![
                OsString::from("find-generic-password"),
                OsString::from("-w"),
                OsString::from("-s"),
                OsString::from("Sessiometer/22222222-2222-2222-2222-222222222222"),
                OsString::from("-a"),
                OsString::from("oauthAccount"),
                kc.as_os_str().to_owned(),
            ]
        );
    }

    #[test]
    fn delete_args_pin_service_acct_and_keychain_without_payload() {
        let kc = Path::new("/tmp/login.keychain-db");
        // No `-w` and no payload — delete needs only the non-secret identifiers,
        // so (unlike write) there is nothing to keep off argv.
        assert_eq!(
            delete_item_args(
                "Sessiometer/33333333-3333-3333-3333-333333333333",
                ACCT_CREDENTIAL,
                kc
            ),
            vec![
                OsString::from("delete-generic-password"),
                OsString::from("-s"),
                OsString::from("Sessiometer/33333333-3333-3333-3333-333333333333"),
                OsString::from("-a"),
                OsString::from("credential"),
                kc.as_os_str().to_owned(),
            ]
        );
    }

    #[test]
    fn strip_one_trailing_newline_removes_exactly_one() {
        assert_eq!(strip_one_trailing_newline(b"abc\n".to_vec()), b"abc");
        assert_eq!(strip_one_trailing_newline(b"abc".to_vec()), b"abc");
        assert_eq!(strip_one_trailing_newline(b"abc\n\n".to_vec()), b"abc\n");
    }

    #[test]
    fn stash_error_classifies_lock_and_other() {
        assert!(matches!(
            stash_error("stash write", 36),
            Error::KeychainLocked { op: "stash write" }
        ));
        assert!(matches!(
            stash_error("stash read", 1),
            Error::Keychain {
                op: "stash read",
                code: 1
            }
        ));
    }

    #[tokio::test]
    async fn fake_stash_round_trips_and_reports_absent() {
        let stash = FakeAccountStash::empty();
        let account = StashedAccount {
            credential: Credential::new(b"raw-token".to_vec()),
            oauth_account: OauthAccount::from_object_bytes(br#"{"accountUuid":"u-1"}"#).unwrap(),
        };
        stash
            .write("Sessiometer/11111111-1111-1111-1111-111111111111", &account)
            .await
            .unwrap();
        assert!(stash.contains("Sessiometer/11111111-1111-1111-1111-111111111111"));

        let got = stash
            .read("Sessiometer/11111111-1111-1111-1111-111111111111")
            .await
            .unwrap();
        assert_eq!(got.credential.expose(), b"raw-token");
        assert_eq!(got.oauth_account.account_uuid(), "u-1");

        assert!(matches!(
            stash
                .read("Sessiometer/99999999-9999-9999-9999-999999999999")
                .await,
            Err(Error::StashIncomplete { .. })
        ));
    }

    #[tokio::test]
    async fn fake_delete_removes_and_is_idempotent() {
        let stash = FakeAccountStash::empty();
        let service = "Sessiometer/11111111-1111-1111-1111-111111111111";
        let account = StashedAccount {
            credential: Credential::new(b"raw-token".to_vec()),
            oauth_account: OauthAccount::from_object_bytes(br#"{"accountUuid":"u-1"}"#).unwrap(),
        };
        stash.write(service, &account).await.unwrap();
        assert!(stash.contains(service));

        // Delete removes the stash, and reads now report it absent.
        stash.delete(service).await.unwrap();
        assert!(!stash.contains(service));
        assert!(matches!(
            stash.read(service).await,
            Err(Error::StashIncomplete { .. })
        ));

        // Deleting an already-absent service is a no-op success (idempotent).
        stash.delete(service).await.unwrap();
    }

    /// Drives the real `security` CLI end-to-end against a throwaway keychain
    /// (created, used, and deleted here) — never the login keychain. macOS-only:
    /// `/usr/bin/security` is the system under test.
    #[cfg(target_os = "macos")]
    mod real_cli {
        use super::*;
        use std::process::Command as StdCommand;

        fn fresh_keychain() -> (tempfile::TempDir, PathBuf) {
            let dir = tempfile::tempdir().unwrap();
            let kc = dir.path().join("test.keychain-db");
            assert!(StdCommand::new(SECURITY)
                .args(["create-keychain", "-p", ""])
                .arg(&kc)
                .status()
                .expect("spawn create-keychain")
                .success());
            assert!(StdCommand::new(SECURITY)
                .args(["unlock-keychain", "-p", ""])
                .arg(&kc)
                .status()
                .expect("spawn unlock-keychain")
                .success());
            (dir, kc)
        }

        fn delete(kc: &Path) {
            let _ = StdCommand::new(SECURITY)
                .arg("delete-keychain")
                .arg(kc)
                .status();
        }

        #[tokio::test]
        async fn write_then_read_round_trips_both_halves_and_refresh_is_idempotent() {
            let (_dir, kc) = fresh_keychain();
            let stash = RealAccountStash::for_keychain(kc.clone());
            let service = "Sessiometer/11111111-1111-1111-1111-111111111111";

            // Credential: printable JSON, stored raw (like the real canonical
            // blob #2 reads). oauthAccount: deliberately NON-ASCII (accented +
            // Cyrillic displayName) — the case where `find-generic-password -w`
            // renders a raw secret as hex and breaks a naive round-trip; the
            // hex-encoding of the oauth half must make it byte-exact regardless.
            let blob = br#"{"claudeAiOauth":{"accessToken":"sk-ant-oat-EXAMPLE","refreshToken":"sk-ant-ort-EXAMPLE"}}"#;
            let oauth_json = "{\"accountUuid\":\"11111111-1111-1111-1111-111111111111\",\
                 \"displayName\":\"Cafe\u{301} \u{41e}\u{43b}\u{435}\u{43a}\u{441}i\u{439}\"}";
            let account = StashedAccount {
                credential: Credential::new(blob.to_vec()),
                oauth_account: OauthAccount::from_object_bytes(oauth_json.as_bytes()).unwrap(),
            };
            let oauth_raw = account.oauth_account.raw_json().to_vec();
            stash.write(service, &account).await.expect("write stash");

            let got = stash.read(service).await.expect("read stash");
            assert_eq!(got.credential.expose(), blob);
            assert_eq!(
                got.oauth_account.account_uuid(),
                "11111111-1111-1111-1111-111111111111"
            );
            // The non-ASCII identity block survives byte-exact (what #6 restores).
            assert_eq!(got.oauth_account.raw_json(), oauth_raw.as_slice());

            // Re-stash a fresh credential under the same service: `-U` updates in
            // place (idempotent refresh), and a single read still resolves it —
            // proving no duplicate item was created.
            let refreshed = StashedAccount {
                credential: Credential::new(b"rotated-token".to_vec()),
                oauth_account: account.oauth_account.clone(),
            };
            stash
                .write(service, &refreshed)
                .await
                .expect("refresh stash");
            let reread = stash.read(service).await.expect("re-read stash");
            assert_eq!(reread.credential.expose(), b"rotated-token");

            delete(&kc);
        }

        #[tokio::test]
        async fn read_reports_incomplete_when_absent() {
            let (_dir, kc) = fresh_keychain();
            let stash = RealAccountStash::for_keychain(kc.clone());
            assert!(matches!(
                stash
                    .read("Sessiometer/00000000-0000-0000-0000-000000000000")
                    .await,
                Err(Error::StashIncomplete { .. })
            ));
            delete(&kc);
        }

        #[tokio::test]
        async fn delete_removes_both_halves_and_is_idempotent() {
            let (_dir, kc) = fresh_keychain();
            let stash = RealAccountStash::for_keychain(kc.clone());
            let service = "Sessiometer/33333333-3333-3333-3333-333333333333";
            let account = StashedAccount {
                credential: Credential::new(b"raw-token".to_vec()),
                oauth_account: OauthAccount::from_object_bytes(
                    br#"{"accountUuid":"33333333-3333-3333-3333-333333333333"}"#,
                )
                .unwrap(),
            };
            stash.write(service, &account).await.expect("write stash");
            stash.read(service).await.expect("stash present");

            // Delete drops both halves: a subsequent read reports the stash gone.
            stash.delete(service).await.expect("delete stash");
            assert!(matches!(
                stash.read(service).await,
                Err(Error::StashIncomplete { .. })
            ));

            // Re-deleting an already-absent stash succeeds (each half's not-found
            // maps to Ok) — removal is safe to re-run.
            stash.delete(service).await.expect("idempotent re-delete");
            delete(&kc);
        }

        #[tokio::test]
        async fn write_round_trips_a_credential_with_shell_metacharacters() {
            // The credential half goes raw through the off-argv `security -i`
            // path; prove the escaping carries spaces, double quotes, backslashes,
            // and `$`/backticks/`;`/`|`/`&` byte-exact (issue #39). The oauthAccount
            // half is pure-ASCII hex, trivially tokenizer-safe.
            let (_dir, kc) = fresh_keychain();
            let stash = RealAccountStash::for_keychain(kc.clone());
            let service = "Sessiometer/22222222-2222-2222-2222-222222222222";
            let blob = br#"{"accessToken":"a b \" c \\ d","x":"$y `z` ;|&"}"#;
            let account = StashedAccount {
                credential: Credential::new(blob.to_vec()),
                oauth_account: OauthAccount::from_object_bytes(
                    br#"{"accountUuid":"22222222-2222-2222-2222-222222222222"}"#,
                )
                .unwrap(),
            };
            stash.write(service, &account).await.expect("write stash");
            let got = stash.read(service).await.expect("read stash");
            assert_eq!(got.credential.expose(), blob);
            delete(&kc);
        }
    }
}
