// Copyright (c) 2026 Oleksii PELYKH
// SPDX-License-Identifier: MIT

//! Per-account credential stash in the login keychain.
//!
//! Each captured account's restorable state lives under the keychain service
//! named by its roster `stash` field (`Sessiometer/acct-N`), as two
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

use std::ffi::{OsStr, OsString};
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::process::Stdio;

#[cfg(test)]
use std::cell::RefCell;
#[cfg(test)]
use std::collections::HashMap;

use tokio::process::Command;
use zeroize::Zeroize;

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

/// Both halves of a captured account, as stashed under one `Sessiometer/acct-N`
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

    /// `add-generic-password -U` one item, storing `payload` as the secret.
    ///
    /// NOTE: `payload` is briefly visible in this process's argv (the same
    /// accepted 0.1.0 residual risk as [`crate::keychain`]'s write — there is no
    /// stdin path for `-w`; never log the argv). For the `oauthAccount` half this
    /// argv carries the account's email (hex-encoded, but trivially recoverable),
    /// so the exposure is wider than #2's opaque-token case — a conscious 0.1.0
    /// trade-off, not an inherited one.
    async fn add_item(
        &self,
        service: &str,
        acct: &str,
        keychain: &Path,
        payload: &[u8],
    ) -> Result<()> {
        let output = Command::new(SECURITY)
            .args(write_item_args(service, acct, keychain, payload))
            .stdin(Stdio::null())
            .output()
            .await?;
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
            hex_encode(account.oauth_account.raw_json()).as_bytes(),
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
        let oauth_bytes = hex_decode(&oauth_hex).ok_or_else(|| Error::StashIncomplete {
            service: service.to_owned(),
        })?;
        let oauth_account = OauthAccount::from_object_bytes(&oauth_bytes)?;
        Ok(StashedAccount {
            credential,
            oauth_account,
        })
    }
}

/// `add-generic-password` arguments (after the program name): an in-place (`-U`)
/// update of `(service, acct)`, pinning the keychain path. The payload is passed
/// as a single verbatim argument.
fn write_item_args(service: &str, acct: &str, keychain: &Path, payload: &[u8]) -> Vec<OsString> {
    vec![
        "add-generic-password".into(),
        "-U".into(),
        "-s".into(),
        service.into(),
        "-a".into(),
        acct.into(),
        "-w".into(),
        OsStr::from_bytes(payload).to_owned(),
        keychain.as_os_str().to_owned(),
    ]
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

/// Strip the single trailing newline `find-generic-password -w` appends, so a
/// write→read round-trip returns the stored bytes exactly. Only one is removed:
/// a newline that is genuinely part of the stored payload is preserved.
fn strip_one_trailing_newline(mut bytes: Vec<u8>) -> Vec<u8> {
    if bytes.last() == Some(&b'\n') {
        bytes.pop();
    }
    bytes
}

/// Encode bytes as lowercase, two-digits-per-byte hex. Used to keep a stored
/// secret pure-ASCII so `find-generic-password -w` always renders it as text
/// (see the module doc); the inverse is [`hex_decode`].
fn hex_encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        // `from_digit(0..16, 16)` is infallible and yields `0-9a-f`.
        out.push(char::from_digit((b >> 4) as u32, 16).expect("high nibble < 16"));
        out.push(char::from_digit((b & 0x0f) as u32, 16).expect("low nibble < 16"));
    }
    out
}

/// Decode the lowercase hex produced by [`hex_encode`] (accepting either case).
/// Returns `None` for odd length or a non-hex byte — i.e. a corrupted item.
fn hex_decode(hex: &[u8]) -> Option<Vec<u8>> {
    if !hex.len().is_multiple_of(2) {
        return None;
    }
    let mut out = Vec::with_capacity(hex.len() / 2);
    for pair in hex.chunks_exact(2) {
        let hi = (pair[0] as char).to_digit(16)?;
        let lo = (pair[1] as char).to_digit(16)?;
        out.push((hi * 16 + lo) as u8);
    }
    Some(out)
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
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_args_pin_service_acct_payload_and_keychain() {
        let kc = Path::new("/tmp/login.keychain-db");
        assert_eq!(
            write_item_args("Sessiometer/acct-1", ACCT_CREDENTIAL, kc, b"blob"),
            vec![
                OsString::from("add-generic-password"),
                OsString::from("-U"),
                OsString::from("-s"),
                OsString::from("Sessiometer/acct-1"),
                OsString::from("-a"),
                OsString::from("credential"),
                OsString::from("-w"),
                OsString::from("blob"),
                kc.as_os_str().to_owned(),
            ]
        );
    }

    #[test]
    fn read_args_pin_service_acct_and_keychain() {
        let kc = Path::new("/tmp/login.keychain-db");
        assert_eq!(
            read_item_args("Sessiometer/acct-2", ACCT_OAUTH, kc),
            vec![
                OsString::from("find-generic-password"),
                OsString::from("-w"),
                OsString::from("-s"),
                OsString::from("Sessiometer/acct-2"),
                OsString::from("-a"),
                OsString::from("oauthAccount"),
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

    #[test]
    fn hex_round_trips_arbitrary_bytes_including_non_ascii() {
        // Every byte value, plus a non-ASCII UTF-8 JSON string like a real
        // displayName/organizationName — the case that broke a raw round-trip.
        let all_bytes: Vec<u8> = (0u8..=255).collect();
        for sample in [
            b"".as_slice(),
            b"{\"a\":1}",
            "{\"displayName\":\"Cafe\u{301} \u{41e}\u{43b}\u{435}\u{43a}\u{441}i\u{439}\"}"
                .as_bytes(),
            &all_bytes,
        ] {
            let encoded = hex_encode(sample);
            assert!(encoded.is_ascii(), "hex output must be pure ASCII");
            assert_eq!(hex_decode(encoded.as_bytes()).as_deref(), Some(sample));
        }
    }

    #[test]
    fn hex_decode_rejects_odd_length_and_non_hex() {
        assert_eq!(hex_decode(b"abc"), None); // odd length
        assert_eq!(hex_decode(b"zz"), None); // non-hex digit
        assert_eq!(hex_decode(b"6X"), None); // one bad digit
                                             // Uppercase is accepted (decode is case-insensitive).
        assert_eq!(hex_decode(b"4A").as_deref(), Some(b"\x4a".as_slice()));
    }

    #[tokio::test]
    async fn fake_stash_round_trips_and_reports_absent() {
        let stash = FakeAccountStash::empty();
        let account = StashedAccount {
            credential: Credential::new(b"raw-token".to_vec()),
            oauth_account: OauthAccount::from_object_bytes(br#"{"accountUuid":"u-1"}"#).unwrap(),
        };
        stash.write("Sessiometer/acct-1", &account).await.unwrap();
        assert!(stash.contains("Sessiometer/acct-1"));

        let got = stash.read("Sessiometer/acct-1").await.unwrap();
        assert_eq!(got.credential.expose(), b"raw-token");
        assert_eq!(got.oauth_account.account_uuid(), "u-1");

        assert!(matches!(
            stash.read("Sessiometer/acct-9").await,
            Err(Error::StashIncomplete { .. })
        ));
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
            let service = "Sessiometer/acct-1";

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
                stash.read("Sessiometer/acct-absent").await,
                Err(Error::StashIncomplete { .. })
            ));
            delete(&kc);
        }
    }
}
