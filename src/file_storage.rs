// Copyright (c) 2026 Oleksii PELYKH
// SPDX-License-Identifier: MIT

//! File implementations of the existing credential and account-stash contracts.
//!
//! Mutations require the caller's existing SwapLock, except for a genuinely
//! single-writer isolated session. This module never acquires that lock again.
//! Same-user hostile filesystem races are not a claimed security boundary.
#![cfg(unix)]

use std::fs::{self, DirBuilder, FileTimes, Metadata, OpenOptions};
use std::io::{ErrorKind, Read, Write};
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use zeroize::{Zeroize, Zeroizing};

#[cfg(test)]
use crate::claude_state::OauthAccount;
use crate::config::{account_uuid_violation, STASH_PREFIX};
use crate::error::{Error, Result};
use crate::file_policy;
use crate::keychain::{Credential, CredentialStore, IsolatedKeychain};
use crate::stash::{AccountStash, StashedAccount};

const CREDENTIAL_NAME: &str = ".credentials.json";
const CREDENTIAL_LIMIT: u64 = 1024 * 1024;
const IDENTITY_LIMIT: u64 = 1024 * 1024;
const RECORD_LIMIT: u64 = 2 * (CREDENTIAL_LIMIT + IDENTITY_LIMIT) + 4096;
const RECORD_VERSION: u16 = 1;

fn invalid(reason: &'static str) -> Error {
    Error::CredentialFile { reason }
}

fn validate_metadata(info: &Metadata, directory: bool) -> Result<()> {
    let correct_type = if directory {
        info.is_dir()
    } else {
        info.is_file()
    };
    let correct_mode = if directory {
        // Existing Claude profiles need not be 0700. The credential itself is
        // private; other users must not be able to replace its directory entries.
        info.mode() & 0o022 == 0
    } else {
        info.mode() & 0o7777 == file_policy::FILE_MODE
    };
    if !correct_type
        || !correct_mode
        || info.uid() != crate::paths::current_uid()
        || (!directory && info.nlink() != 1)
    {
        return Err(invalid(
            "unsafe ownership, type, link count, or permissions",
        ));
    }
    Ok(())
}

fn check_directory(root: &Path) -> Result<()> {
    // A profile-directory alias is allowed; the final secret file is opened
    // with O_NOFOLLOW below. No chmod of the existing profile takes place.
    validate_metadata(&fs::metadata(root)?, true)
}

/// Create only the leaf directory. Its safe, owned parent must already exist.
/// Existing profile aliases are handled by the canonical writer's check first.
fn create_private_directory(root: &Path) -> Result<()> {
    match fs::symlink_metadata(root) {
        Ok(info) => validate_metadata(&info, true),
        Err(error) if error.kind() == ErrorKind::NotFound => {
            let parent = root
                .parent()
                .ok_or_else(|| invalid("missing parent directory"))?;
            check_directory(parent)?;
            match DirBuilder::new().mode(file_policy::DIR_MODE).create(root) {
                Ok(()) => {}
                Err(error) if error.kind() == ErrorKind::AlreadyExists => {}
                Err(error) => return Err(error.into()),
            }
            check_directory(root)
        }
        Err(error) => Err(error.into()),
    }
}

fn read_private(root: &Path, name: &str, limit: u64) -> Result<Zeroizing<Vec<u8>>> {
    check_directory(root)?;
    let mut file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(root.join(name))?;
    let info = file.metadata()?;
    validate_metadata(&info, false)?;
    if info.len() > limit {
        return Err(invalid("record exceeds the size limit"));
    }
    let mut bytes = Zeroizing::new(Vec::new());
    Read::by_ref(&mut file)
        .take(limit + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > limit {
        return Err(invalid("record grew beyond the size limit"));
    }
    Ok(bytes)
}

/// Read an operator-supplied token without following a final symlink, accepting
/// only an owned, private, regular file in a non-writable-by-others directory.
pub(crate) fn read_token_file(path: &Path) -> Result<Zeroizing<Vec<u8>>> {
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| invalid("token file needs a UTF-8 filename"))?;
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    read_private(parent, name, 16 * 1024)
}

fn milliseconds(time: SystemTime) -> Result<u128> {
    time.duration_since(UNIX_EPOCH)
        .map(|value| value.as_millis())
        .map_err(|_| invalid("modification time is before the Unix epoch"))
}

/// The last required operation is rename: errors returned here precede commit.
/// This matches the repository's existing file-writer durability contract.
/// It does not add a promise of power-loss durability for the directory entry.
fn replace_private(
    root: &Path,
    name: &str,
    bytes: &[u8],
    limit: u64,
    change_mtime: bool,
) -> Result<()> {
    check_directory(root)?;
    if bytes.len() as u64 > limit {
        return Err(invalid("replacement exceeds the size limit"));
    }

    let target = root.join(name);
    let previous = match fs::symlink_metadata(&target) {
        Ok(info) => {
            validate_metadata(&info, false)?;
            Some(info.modified()?)
        }
        Err(error) if error.kind() == ErrorKind::NotFound => None,
        Err(error) => return Err(error.into()),
    };

    // A dedicated name avoids sharing Claude's own temporary-file namespace.
    // Callers serialize all Sessiometer mutations with the existing swap lock.
    let staging = root.join(format!("{name}.sessiometer-tmp"));
    match fs::remove_file(&staging) {
        Ok(()) => {}
        Err(error) if error.kind() == ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }

    let result = (|| -> Result<()> {
        let mut file = file_policy::open_owner_only(
            OpenOptions::new()
                .write(true)
                .create_new(true)
                .custom_flags(libc::O_NOFOLLOW),
            &staging,
        )?;
        file.write_all(bytes)?;
        if change_mtime {
            let stamp = match previous {
                Some(old) => SystemTime::now().max(
                    old.checked_add(Duration::from_millis(2))
                        .ok_or_else(|| invalid("modification time overflow"))?,
                ),
                None => SystemTime::now(),
            };
            file.set_times(FileTimes::new().set_modified(stamp))?;
            if let Some(old) = previous {
                if milliseconds(file.metadata()?.modified()?)? == milliseconds(old)? {
                    return Err(invalid("filesystem did not advance the modification time"));
                }
            }
        }
        validate_metadata(&file.metadata()?, false)?;
        file.sync_all()?;
        drop(file);
        fs::rename(&staging, &target)?;
        Ok(())
    })();

    if result.is_err() {
        let _ = fs::remove_file(&staging);
    }
    result
}

fn remove_private(root: &Path, name: &str) -> Result<()> {
    match fs::metadata(root) {
        Ok(info) => validate_metadata(&info, true)?,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
    }
    let path = root.join(name);
    match fs::symlink_metadata(&path) {
        Ok(info) => validate_metadata(&info, false)?,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
    }
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

fn validate_credential(bytes: &[u8]) -> Result<()> {
    let value: serde_json::Value =
        serde_json::from_slice(bytes).map_err(|_| invalid("invalid credential JSON"))?;
    let token = value
        .get("claudeAiOauth")
        .and_then(|oauth| oauth.get("accessToken"))
        .and_then(serde_json::Value::as_str);
    if token.is_none_or(str::is_empty) {
        return Err(invalid("missing OAuth access token"));
    }
    Ok(())
}

/// An injected metadata-only guard, not another credential source.
pub(crate) trait FileAuthority {
    async fn verify(&self) -> Result<()>;
}

pub(crate) struct NativeFileAuthority {
    #[cfg(target_os = "macos")]
    service: String,
}

impl NativeFileAuthority {
    pub(crate) fn new(service: Option<&str>) -> Result<Self> {
        #[cfg(target_os = "macos")]
        {
            Ok(Self {
                service: service
                    .ok_or_else(|| invalid("missing macOS credential namespace"))?
                    .to_owned(),
            })
        }
        #[cfg(not(target_os = "macos"))]
        {
            let _ = service;
            Ok(Self {})
        }
    }
}

impl FileAuthority for NativeFileAuthority {
    async fn verify(&self) -> Result<()> {
        #[cfg(target_os = "macos")]
        {
            use std::process::Stdio;
            use tokio::process::Command;

            // No -w / -g: never request a password. No keychain pathname:
            // check the default search list used by the inspected Claude client.
            // Any account under the service conflicts, deliberately conservatively.
            let mut command = Command::new("/usr/bin/security");
            command
                .args(["find-generic-password", "-s"])
                .arg(&self.service)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .kill_on_drop(true);
            let status = tokio::time::timeout(Duration::from_secs(5), command.status())
                .await
                .map_err(|_| invalid("credential-source probe timed out"))??;
            match status.code() {
                Some(0) => Err(Error::CredentialSourceConflict),
                Some(44) => Ok(()),
                _ => Err(invalid(
                    "cannot establish that file credentials are authoritative",
                )),
            }
        }
        #[cfg(target_os = "linux")]
        {
            // The supplied Linux Claude source selects plaintext storage
            // directly. No Keychain namespace or subprocess is relevant here.
            Ok(())
        }
        #[cfg(not(any(target_os = "macos", target_os = "linux")))]
        {
            Err(invalid("file storage is not enabled on this platform"))
        }
    }
}

pub(crate) struct FileCredentialStore<P = NativeFileAuthority> {
    profile: PathBuf,
    authority: P,
}

impl<P: FileAuthority> FileCredentialStore<P> {
    pub(crate) fn new(profile: PathBuf, authority: P) -> Self {
        Self { profile, authority }
    }

    /// Import a known account's live Keychain credential. The caller holds the
    /// existing swap lock. Reads/status remain read-only; this is an explicit
    /// reconciliation operation, not a fallback hidden inside CredentialStore::read.
    #[cfg(any(target_os = "macos", test))]
    pub(crate) async fn import_keychain<K, S>(
        &self,
        source: &K,
        stash: &S,
        roster: &[crate::config::Account],
        claude_json: &Path,
    ) -> Result<Option<usize>>
    where
        K: crate::keychain::FileImportSource,
        S: AccountStash,
    {
        // Repeat the preflight under the lock: another Sessiometer process may
        // already have completed the import while this caller was waiting.
        match self.authority.verify().await {
            Ok(()) => return Ok(None),
            Err(Error::CredentialSourceConflict) => {}
            Err(error) => return Err(error),
        }
        let credential = source.read().await?;
        if credential.expose().len() as u64 > CREDENTIAL_LIMIT {
            return Err(invalid("replacement exceeds the size limit"));
        }
        validate_credential(credential.expose())?;
        let idx = crate::active::resolve_account_for(roster, stash, claude_json, &credential)
            .await
            .ok_or(Error::ActiveAccountUnresolved)?;
        let service = roster[idx].stash();
        let previous = stash.read(&service).await?;
        let saved = StashedAccount {
            credential,
            oauth_account: previous.oauth_account,
        };

        // Save the roster entry FIRST. Even if Claude removes the active file
        // during the handoff, Sessiometer retains this credential in its stash.
        // Do not roll back a saved fresh credential on a later failure.
        stash.write(&service, &saved).await?;
        self.write_payload(&saved.credential)?;
        source.delete_if_matches(&saved.credential).await?;

        // Remaining/recreated Keychain items still refuse file operation. Also
        // detect an active file removed or replaced during the transfer.
        if !self.read().await?.matches(&saved.credential) {
            return Err(invalid("credentials changed during Keychain import; retry"));
        }
        Ok(Some(idx))
    }

    /// Validated atomic write, without the source-authority probe. Used by the
    /// checked writer and the copy-before-delete helper/live imports.
    fn write_payload(&self, credential: &Credential) -> Result<()> {
        let bytes = credential.expose();
        if bytes.len() as u64 > CREDENTIAL_LIMIT {
            return Err(invalid("replacement exceeds the size limit"));
        }
        validate_credential(bytes)?;
        // Bootstrap only on a valid write, never on read or construction.
        // Check first so a valid existing directory alias remains supported.
        match check_directory(&self.profile) {
            Err(Error::Io(error)) if error.kind() == ErrorKind::NotFound => {
                create_private_directory(&self.profile)?;
            }
            result => result?,
        }
        replace_private(
            &self.profile,
            CREDENTIAL_NAME,
            bytes,
            CREDENTIAL_LIMIT,
            true,
        )
    }
}

impl<P: FileAuthority> CredentialStore for FileCredentialStore<P> {
    async fn read(&self) -> Result<Credential> {
        self.authority.verify().await?;
        let mut bytes = match read_private(&self.profile, CREDENTIAL_NAME, CREDENTIAL_LIMIT) {
            Ok(bytes) => bytes,
            Err(Error::Io(error)) if error.kind() == ErrorKind::NotFound => {
                return Err(Error::CredentialNotFound);
            }
            Err(error) => return Err(error),
        };
        validate_credential(&bytes)?;
        Ok(Credential::new(std::mem::take(&mut *bytes)))
    }

    async fn write(&self, credential: &Credential) -> Result<()> {
        self.authority.verify().await?;
        self.write_payload(credential)
    }

    async fn probe_resolution(&self) -> Result<()> {
        self.read().await.map(|_| ())
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct StoredAccount {
    version: u16,
    service: String,
    credential_hex: String,
    oauth_account_hex: String,
}

impl Drop for StoredAccount {
    fn drop(&mut self) {
        self.credential_hex.zeroize();
        self.oauth_account_hex.zeroize();
    }
}

#[derive(Serialize)]
struct StoredAccountRef<'a> {
    version: u16,
    service: &'a str,
    credential_hex: &'a str,
    oauth_account_hex: &'a str,
}

fn account_name(service: &str) -> Result<String> {
    let uuid = service
        .strip_prefix(STASH_PREFIX)
        .filter(|uuid| !uuid.is_empty())
        .ok_or_else(|| invalid("invalid account selector"))?;
    if account_uuid_violation(uuid).is_some() {
        return Err(invalid("invalid account selector"));
    }
    // Hash the identifier, NOT the token. This also prevents collisions between
    // case-distinct identifiers on macOS case-insensitive filesystems.
    Ok(format!(
        "{}.json",
        crate::sha256::sha256_hex(service.as_bytes())
    ))
}

pub(crate) struct FileAccountStash {
    root: PathBuf,
}

impl FileAccountStash {
    pub(crate) fn new(root: PathBuf) -> Self {
        Self { root }
    }

    /// The prior-configuration witness must inspect metadata, never token contents.
    /// An unknown regular record is evidence, not permission to overwrite a roster.
    pub(crate) fn any_record_present(&self) -> Result<bool> {
        match fs::symlink_metadata(&self.root) {
            Ok(info) => validate_metadata(&info, true)?,
            Err(error) if error.kind() == ErrorKind::NotFound => return Ok(false),
            Err(error) => return Err(error.into()),
        }
        for entry in fs::read_dir(&self.root)? {
            let entry = entry?;
            if entry
                .path()
                .extension()
                .is_some_and(|extension| extension == "json")
            {
                return Ok(true);
            }
        }
        Ok(false)
    }
}

impl AccountStash for FileAccountStash {
    async fn write(&self, service: &str, account: &StashedAccount) -> Result<()> {
        let name = account_name(service)?;
        crate::stash::validate_identity(service, account.oauth_account.as_ref())
            .map_err(|_| invalid("account identity does not match its selector"))?;
        if account.credential.expose().len() as u64 > CREDENTIAL_LIMIT
            || account.identity_bytes().len() as u64 > IDENTITY_LIMIT
        {
            return Err(invalid("account record exceeds the size limit"));
        }
        create_private_directory(&self.root)?;
        let credential_hex = Zeroizing::new(crate::hex::encode(account.credential.expose()));
        let oauth_account_hex = Zeroizing::new(crate::hex::encode(account.identity_bytes()));
        let record = StoredAccountRef {
            version: if account.oauth_account.is_none() {
                2
            } else {
                RECORD_VERSION
            },
            service,
            credential_hex: &credential_hex,
            oauth_account_hex: &oauth_account_hex,
        };
        let bytes = Zeroizing::new(
            serde_json::to_vec(&record).map_err(|_| invalid("cannot encode account record"))?,
        );
        replace_private(&self.root, &name, &bytes, RECORD_LIMIT, false)
    }

    async fn read(&self, service: &str) -> Result<StashedAccount> {
        let name = account_name(service)?;
        let bytes = match read_private(&self.root, &name, RECORD_LIMIT) {
            Ok(bytes) => bytes,
            Err(Error::Io(error)) if error.kind() == ErrorKind::NotFound => {
                return Err(Error::StashIncomplete {
                    service: service.to_owned(),
                });
            }
            Err(error) => return Err(error),
        };
        let record: StoredAccount =
            serde_json::from_slice(&bytes).map_err(|_| invalid("invalid account record"))?;
        if !matches!(record.version, 1 | 2) || record.service != service {
            return Err(invalid("unsupported or mismatched account record"));
        }
        let mut credential = Zeroizing::new(
            crate::hex::decode(record.credential_hex.as_bytes())
                .ok_or_else(|| invalid("invalid credential encoding"))?,
        );
        let identity = Zeroizing::new(
            crate::hex::decode(record.oauth_account_hex.as_bytes())
                .ok_or_else(|| invalid("invalid identity encoding"))?,
        );
        if credential.len() as u64 > CREDENTIAL_LIMIT || identity.len() as u64 > IDENTITY_LIMIT {
            return Err(invalid("decoded account exceeds the size limit"));
        }
        let oauth_account = crate::stash::decode_identity(&identity)
            .map_err(|_| invalid("invalid stored identity"))?;
        if record.version == 1 && oauth_account.is_none() {
            return Err(invalid("legacy account record requires identity"));
        }
        crate::stash::validate_identity(service, oauth_account.as_ref())
            .map_err(|_| invalid("stored identity does not match its selector"))?;
        Ok(StashedAccount {
            credential: Credential::new(std::mem::take(&mut *credential)),
            oauth_account,
        })
    }

    async fn delete(&self, service: &str) -> Result<()> {
        remove_private(&self.root, &account_name(service)?)
    }
}

/// File-side isolated session. On macOS, normalize the completed helper's
/// Keychain output into its file before the existing readback protocol resumes.
/// The optional item uses the existing trait; Linux passes None.
pub(crate) struct FileIsolatedStore<
    P = NativeFileAuthority,
    K = crate::keychain::IsolatedKeychainItem,
> {
    store: FileCredentialStore<P>,
    keychain_output: Option<K>,
}

impl<P: FileAuthority, K: IsolatedKeychain> FileIsolatedStore<P, K> {
    pub(crate) fn new(profile: PathBuf, authority: P, keychain_output: Option<K>) -> Self {
        Self {
            store: FileCredentialStore::new(profile, authority),
            keychain_output,
        }
    }

    fn delete_files(&self) -> Result<()> {
        let credential = remove_private(&self.store.profile, CREDENTIAL_NAME);
        let staging = remove_private(
            &self.store.profile,
            &format!("{CREDENTIAL_NAME}.sessiometer-tmp"),
        );
        credential.and(staging)
    }
}

impl<P: FileAuthority, K: IsolatedKeychain> IsolatedKeychain for FileIsolatedStore<P, K> {
    async fn seed(&self, blob: &[u8]) -> Result<()> {
        // File selection still seeds only a file, never the temporary Keychain item.
        self.store.write(&Credential::new(blob.to_vec())).await
    }

    async fn read_back(&self) -> Result<Credential> {
        if let Some(item) = &self.keychain_output {
            match item.read_back().await {
                Ok(credential) => {
                    // Claude's primary-store result wins over a possibly stale file.
                    // Publish the full validated blob before deleting the source.
                    self.store.write_payload(&credential)?;
                    item.delete().await?;
                }
                Err(Error::CredentialNotFound) => {}
                Err(error) => return Err(error),
            }
        }
        // Recheck authority after conversion: remaining shadowing is still an error.
        self.store.read().await
    }

    async fn delete(&self) -> Result<()> {
        let keychain = match &self.keychain_output {
            Some(item) => item.delete().await,
            None => Ok(()),
        };
        // Attempt both cleanups even when one fails; the existing guard retries.
        let files = self.delete_files();
        keychain.and(files)
    }

    fn delete_blocking(&self) {
        if let Some(item) = &self.keychain_output {
            item.delete_blocking();
        }
        let _ = self.delete_files();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::{symlink, PermissionsExt};
    use tempfile::TempDir;

    #[derive(Clone, Copy)]
    struct Authority(bool);

    impl FileAuthority for Authority {
        async fn verify(&self) -> Result<()> {
            if self.0 {
                Ok(())
            } else {
                Err(Error::CredentialSourceConflict)
            }
        }
    }

    fn directory() -> TempDir {
        let root = tempfile::tempdir().unwrap();
        file_policy::owner_only_dir(root.path()).unwrap();
        root
    }

    fn credential(token: &str) -> Credential {
        Credential::new(
            format!(
                "{{ \"claudeAiOauth\": {{\"accessToken\":\"{token}\",\
                 \"refreshToken\":\"DUMMY-REFRESH\",\"expiresAt\":1234567890000,\
                 \"scopes\":[\"user:inference\",\"user:profile\"]}},\
                 \"unrelated\":{{\"preserved\":true}} }}\n"
            )
            .into_bytes(),
        )
    }

    fn account(uuid: &str, token: &str) -> StashedAccount {
        StashedAccount {
            credential: credential(token),
            oauth_account: Some(
                OauthAccount::from_object_bytes(
                    format!(
                        "{{\"accountUuid\":\"{uuid}\",\"emailAddress\":\"test@example.invalid\"}}"
                    )
                    .as_bytes(),
                )
                .unwrap(),
            ),
        }
    }

    #[tokio::test]
    async fn token_only_record_roundtrips_null_and_rejects_identity_mismatch() {
        let root = directory();
        let stash = FileAccountStash::new(root.path().join("accounts"));
        let local = "Sessiometer/local-0123456789abcdef0123456789abcdef";
        let token_only = StashedAccount {
            credential: account("B", "DUMMY-C").credential,
            oauth_account: None,
        };
        stash.write(local, &token_only).await.unwrap();
        let saved = stash.read(local).await.unwrap();
        assert!(saved.oauth_account.is_none());
        assert!(saved.credential.matches(&token_only.credential));
        assert!(stash.write("Sessiometer/B", &token_only).await.is_err());
        assert!(stash.write(local, &account("B", "DUMMY-B")).await.is_err());
        assert!(stash
            .read("Sessiometer/local-fedcba9876543210fedcba9876543210")
            .await
            .is_err());
    }

    #[test]
    fn token_file_reader_refuses_public_or_linked_files() {
        let root = directory();
        let path = root.path().join("token");
        fs::write(&path, b"dummy").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        assert!(read_token_file(&path).is_err());
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        assert_eq!(read_token_file(&path).unwrap().as_slice(), b"dummy");
        let link = root.path().join("token-link");
        std::os::unix::fs::symlink(&path, &link).unwrap();
        assert!(read_token_file(&link).is_err());
    }

    #[tokio::test]
    async fn full_credential_is_preserved_byte_for_byte() {
        let root = directory();
        let store = FileCredentialStore::new(root.path().to_path_buf(), Authority(true));
        let expected = credential("DUMMY-A");
        store.write(&expected).await.unwrap();
        assert!(store.read().await.unwrap().matches(&expected));
        assert_eq!(
            fs::read(root.path().join(CREDENTIAL_NAME)).unwrap(),
            expected.expose()
        );
    }

    /// Exercise the unchanged capture and swap engines, not just adapter methods.
    #[tokio::test]
    async fn capture_then_a_b_a_swaps_preserve_full_credentials_and_identity() {
        let root = directory();
        let profile = root.path().join("profile");
        let store = FileCredentialStore::new(profile.clone(), Authority(true));
        let stash = FileAccountStash::new(root.path().join("accounts"));
        let state = root.path().join("state.json");
        let lock = root.path().join("swap.lock");
        let a = account("A", "DUMMY-A");
        let b = account("B", "DUMMY-B");
        store.write(&a.credential).await.unwrap();
        fs::write(&state, b"{}").unwrap();
        crate::claude_state::write_oauth_account(&state, a.require_identity().unwrap()).unwrap();
        let config = crate::config::Config::from_toml_str("[storage]\nbackend = 'file'\n").unwrap();
        let capture = crate::capture::capture_locked(
            Some((&lock, Duration::from_secs(1))),
            &store,
            &stash,
            &state,
            Some(config),
            Some("first"),
        )
        .await
        .unwrap();
        assert_eq!(
            capture.config.storage.backend,
            crate::config::StorageBackend::File
        );
        assert!(stash
            .read("Sessiometer/A")
            .await
            .unwrap()
            .credential
            .matches(&a.credential));
        stash.write("Sessiometer/B", &b).await.unwrap();
        for (outgoing, incoming, expected) in [
            ("Sessiometer/A", "Sessiometer/B", &b),
            ("Sessiometer/B", "Sessiometer/A", &a),
        ] {
            let before = milliseconds(
                fs::metadata(profile.join(CREDENTIAL_NAME))
                    .unwrap()
                    .modified()
                    .unwrap(),
            )
            .unwrap();
            let report = crate::swap::swap_locked(
                Some((&lock, Duration::from_secs(1))),
                &store,
                &stash,
                outgoing,
                incoming,
                &state,
            )
            .await
            .unwrap();
            assert!(report.canonical_confirmed);
            assert!(store.read().await.unwrap().matches(&expected.credential));
            assert_eq!(
                crate::claude_state::read_oauth_account_from(&state)
                    .unwrap()
                    .account_uuid(),
                expected.require_identity().unwrap().account_uuid()
            );
            assert!(
                milliseconds(
                    fs::metadata(profile.join(CREDENTIAL_NAME))
                        .unwrap()
                        .modified()
                        .unwrap()
                )
                .unwrap()
                    > before
            );
        }
        assert!(stash
            .read("Sessiometer/A")
            .await
            .unwrap()
            .credential
            .matches(&a.credential));
        assert!(stash
            .read("Sessiometer/B")
            .await
            .unwrap()
            .credential
            .matches(&b.credential));
        let held = crate::swap::SwapLock::acquire(&lock, Duration::from_secs(1))
            .await
            .unwrap();
        assert!(matches!(
            crate::swap::swap_locked(
                Some((&lock, Duration::ZERO)),
                &store,
                &stash,
                "Sessiometer/A",
                "Sessiometer/B",
                &state,
            )
            .await,
            Err(Error::SwapLockBusy)
        ));
        assert!(store.read().await.unwrap().matches(&a.credential));
        drop(held);
    }

    /// Dummy child drives the real seed/readback/CAS/cleanup lifecycle without
    /// reading a real token, invoking Keychain, or contacting a provider.
    struct RefreshChild {
        concurrent_stash: Option<PathBuf>,
    }

    impl crate::isolated_spawn::ClaudeRefresh for RefreshChild {
        async fn run(&self, profile: &Path) -> Result<()> {
            let store = FileCredentialStore::new(profile.to_path_buf(), Authority(true));
            let seed = store.read().await?;
            assert_eq!(crate::refresh::expires_at(seed.expose()), Some(1_000));
            if let Some(root) = &self.concurrent_stash {
                FileAccountStash::new(root.clone())
                    .write("Sessiometer/A", &account("A", "DUMMY-CONCURRENT"))
                    .await?;
            }
            store.write(&credential("DUMMY-FRESH")).await
        }
    }

    #[tokio::test]
    async fn file_refresh_lifecycle_restashes_fresh_results_but_never_overwrites_a_newer_stash() {
        for concurrent in [false, true] {
            let root = directory();
            let account_root = root.path().join("accounts");
            let stash = FileAccountStash::new(account_root.clone());
            stash
                .write("Sessiometer/A", &account("A", "DUMMY-OLD"))
                .await
                .unwrap();
            let isolated = root.path().join("isolated");
            let lock = root.path().join("swap.lock");
            let report = crate::refresh::refresh_cycle(
                &stash,
                "Sessiometer/A",
                FileIsolatedStore::new(
                    isolated.clone(),
                    Authority(true),
                    None::<crate::keychain::IsolatedKeychainItem>,
                ),
                &RefreshChild {
                    concurrent_stash: concurrent.then_some(account_root),
                },
                isolated.clone(),
                Some((&lock, Duration::from_secs(1))),
                1_000,
            )
            .await
            .unwrap();
            assert!(matches!(
                report.outcome,
                crate::refresh::RefreshOutcome::Refreshed { .. }
            ));
            assert_eq!(report.re_stashed, !concurrent);
            let expected = credential(if concurrent {
                "DUMMY-CONCURRENT"
            } else {
                "DUMMY-FRESH"
            });
            assert!(stash
                .read("Sessiometer/A")
                .await
                .unwrap()
                .credential
                .matches(&expected));
            assert!(!isolated.exists());
        }
    }

    #[tokio::test]
    async fn missing_profile_is_created_only_by_the_first_valid_write() {
        let parent = directory();
        fs::set_permissions(parent.path(), fs::Permissions::from_mode(0o755)).unwrap();
        let profile = parent.path().join("profile");
        let store = FileCredentialStore::new(profile.clone(), Authority(true));
        assert!(!profile.exists());
        assert!(matches!(store.read().await, Err(Error::CredentialNotFound)));
        assert!(matches!(
            store.probe_resolution().await,
            Err(Error::CredentialNotFound)
        ));
        assert!(!profile.exists());

        let expected = credential("DUMMY-A");
        store.write(&expected).await.unwrap();
        assert!(store.read().await.unwrap().matches(&expected));
        assert_eq!(fs::metadata(&profile).unwrap().mode() & 0o777, 0o700);
        assert_eq!(
            fs::metadata(profile.join(CREDENTIAL_NAME)).unwrap().mode() & 0o777,
            0o600
        );
        assert_eq!(fs::metadata(parent.path()).unwrap().mode() & 0o777, 0o755);
    }

    #[tokio::test]
    async fn refused_first_writes_do_not_create_a_profile() {
        let parent = directory();
        let profile = parent.path().join("profile");
        let blocked = FileCredentialStore::new(profile.clone(), Authority(false));
        assert!(matches!(
            blocked.write(&credential("DUMMY-A")).await,
            Err(Error::CredentialSourceConflict)
        ));
        assert!(!profile.exists());

        let store = FileCredentialStore::new(profile.clone(), Authority(true));
        assert!(store
            .write(&Credential::new(b"broken".to_vec()))
            .await
            .is_err());
        assert!(!profile.exists());
        let mut oversized = credential("DUMMY-A").expose().to_vec();
        oversized.resize(CREDENTIAL_LIMIT as usize + 1, b' ');
        assert!(store.write(&Credential::new(oversized)).await.is_err());
        assert!(!profile.exists());
    }

    #[tokio::test]
    async fn first_write_requires_an_existing_safe_parent() {
        let parent = directory();
        let missing_parent = parent.path().join("missing");
        let store = FileCredentialStore::new(missing_parent.join("profile"), Authority(true));
        assert!(store.write(&credential("DUMMY-A")).await.is_err());
        assert!(!missing_parent.exists());

        fs::set_permissions(parent.path(), fs::Permissions::from_mode(0o770)).unwrap();
        let profile = parent.path().join("profile");
        let store = FileCredentialStore::new(profile.clone(), Authority(true));
        assert!(store.write(&credential("DUMMY-A")).await.is_err());
        assert!(!profile.exists());
        assert_eq!(fs::metadata(parent.path()).unwrap().mode() & 0o777, 0o770);
    }

    #[tokio::test]
    async fn dangling_profile_alias_does_not_create_its_target() {
        let parent = directory();
        let target = parent.path().join("missing");
        let alias = parent.path().join("alias");
        symlink(&target, &alias).unwrap();
        let store = FileCredentialStore::new(alias.clone(), Authority(true));
        assert!(store.write(&credential("DUMMY-A")).await.is_err());
        assert!(!target.exists());
        assert!(fs::symlink_metadata(alias)
            .unwrap()
            .file_type()
            .is_symlink());
    }

    #[tokio::test]
    async fn rapid_a_b_a_changes_the_observed_mtime_each_time() {
        let root = directory();
        let store = FileCredentialStore::new(root.path().to_path_buf(), Authority(true));
        let mut previous = None;
        for token in ["DUMMY-A", "DUMMY-B", "DUMMY-A"] {
            let expected = credential(token);
            store.write(&expected).await.unwrap();
            assert!(store.read().await.unwrap().matches(&expected));
            let current = milliseconds(
                fs::metadata(root.path().join(CREDENTIAL_NAME))
                    .unwrap()
                    .modified()
                    .unwrap(),
            )
            .unwrap();
            if let Some(old) = previous {
                assert!(current > old);
            }
            previous = Some(current);
        }
    }

    #[tokio::test]
    async fn source_conflict_is_not_missing_and_does_not_overwrite() {
        let root = directory();
        let allowed = FileCredentialStore::new(root.path().to_path_buf(), Authority(true));
        let blocked = FileCredentialStore::new(root.path().to_path_buf(), Authority(false));
        let original = credential("DUMMY-A");
        allowed.write(&original).await.unwrap();
        assert!(matches!(
            blocked.read().await,
            Err(Error::CredentialSourceConflict)
        ));
        assert!(matches!(
            blocked.write(&credential("DUMMY-B")).await,
            Err(Error::CredentialSourceConflict)
        ));
        assert!(allowed.read().await.unwrap().matches(&original));
    }

    #[tokio::test]
    async fn missing_and_malformed_are_not_the_same_error() {
        let root = directory();
        let store = FileCredentialStore::new(root.path().to_path_buf(), Authority(true));
        assert!(matches!(store.read().await, Err(Error::CredentialNotFound)));
        replace_private(root.path(), CREDENTIAL_NAME, b"not-json", 100, true).unwrap();
        assert!(matches!(
            store.read().await,
            Err(Error::CredentialFile { .. })
        ));
    }

    #[tokio::test]
    async fn invalid_incoming_json_leaves_the_old_credential_intact() {
        let root = directory();
        let store = FileCredentialStore::new(root.path().to_path_buf(), Authority(true));
        let original = credential("DUMMY-A");
        store.write(&original).await.unwrap();
        assert!(store
            .write(&Credential::new(b"broken".to_vec()))
            .await
            .is_err());
        assert!(store.read().await.unwrap().matches(&original));
    }

    #[tokio::test]
    async fn staging_failure_leaves_the_old_complete_credential_and_mtime_intact() {
        let root = directory();
        let store = FileCredentialStore::new(root.path().to_path_buf(), Authority(true));
        let original = credential("DUMMY-A");
        store.write(&original).await.unwrap();
        let path = root.path().join(CREDENTIAL_NAME);
        let before = fs::metadata(&path).unwrap().modified().unwrap();
        // A directory cannot be unlinked as our stale staging file; failure must
        // occur before touching the committed credential, not after truncating it.
        fs::create_dir(
            root.path()
                .join(format!("{CREDENTIAL_NAME}.sessiometer-tmp")),
        )
        .unwrap();
        assert!(store.write(&credential("DUMMY-B")).await.is_err());
        assert!(store.read().await.unwrap().matches(&original));
        assert_eq!(fs::metadata(path).unwrap().modified().unwrap(), before);
    }

    #[tokio::test]
    async fn unsafe_file_permissions_are_rejected_without_tightening_in_place() {
        let root = directory();
        let store = FileCredentialStore::new(root.path().to_path_buf(), Authority(true));
        store.write(&credential("DUMMY-A")).await.unwrap();
        let path = root.path().join(CREDENTIAL_NAME);
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        assert!(store.read().await.is_err());
        assert!(store.write(&credential("DUMMY-B")).await.is_err());
        assert_eq!(fs::metadata(path).unwrap().mode() & 0o777, 0o644);
    }

    #[tokio::test]
    async fn existing_profile_modes_are_accepted_without_chmod() {
        for mode in [0o755, 0o750] {
            let root = directory();
            fs::set_permissions(root.path(), fs::Permissions::from_mode(mode)).unwrap();
            let store = FileCredentialStore::new(root.path().to_path_buf(), Authority(true));
            let expected = credential("DUMMY-A");
            store.write(&expected).await.unwrap();
            assert!(store.read().await.unwrap().matches(&expected));
            assert_eq!(fs::metadata(root.path()).unwrap().mode() & 0o777, mode);
            assert_eq!(
                fs::metadata(root.path().join(CREDENTIAL_NAME))
                    .unwrap()
                    .mode()
                    & 0o777,
                0o600
            );
        }
    }

    #[tokio::test]
    async fn group_or_other_writable_profile_is_refused_without_chmod() {
        for mode in [0o770, 0o707] {
            let root = directory();
            fs::set_permissions(root.path(), fs::Permissions::from_mode(mode)).unwrap();
            let store = FileCredentialStore::new(root.path().to_path_buf(), Authority(true));
            assert!(store.write(&credential("DUMMY-A")).await.is_err());
            assert!(!root.path().join(CREDENTIAL_NAME).exists());
            assert_eq!(fs::metadata(root.path()).unwrap().mode() & 0o777, mode);
        }
    }

    #[tokio::test]
    async fn profile_directory_alias_is_allowed_without_replacing_the_alias() {
        let root = directory();
        let links = directory();
        let alias = links.path().join("profile");
        symlink(root.path(), &alias).unwrap();
        let store = FileCredentialStore::new(alias.clone(), Authority(true));
        let expected = credential("DUMMY-A");
        store.write(&expected).await.unwrap();
        assert!(store.read().await.unwrap().matches(&expected));
        assert!(fs::symlink_metadata(alias)
            .unwrap()
            .file_type()
            .is_symlink());
    }

    #[tokio::test]
    async fn symlinks_and_hard_links_are_rejected() {
        let root = directory();
        let store = FileCredentialStore::new(root.path().to_path_buf(), Authority(true));
        store.write(&credential("DUMMY-A")).await.unwrap();
        let path = root.path().join(CREDENTIAL_NAME);
        let original = root.path().join("original");
        fs::rename(&path, &original).unwrap();
        symlink(&original, &path).unwrap();
        assert!(store.read().await.is_err());
        assert!(store.write(&credential("DUMMY-B")).await.is_err());
        fs::remove_file(&path).unwrap();
        fs::hard_link(&original, &path).unwrap();
        assert!(store.read().await.is_err());
        assert!(store.write(&credential("DUMMY-B")).await.is_err());
    }

    #[tokio::test]
    async fn oversized_file_is_refused_before_an_unbounded_read() {
        let root = directory();
        let path = root.path().join(CREDENTIAL_NAME);
        let file =
            file_policy::open_owner_only(OpenOptions::new().create_new(true).write(true), &path)
                .unwrap();
        file.set_len(CREDENTIAL_LIMIT + 1).unwrap();
        let store = FileCredentialStore::new(root.path().to_path_buf(), Authority(true));
        assert!(matches!(
            store.read().await,
            Err(Error::CredentialFile { .. })
        ));
    }

    #[tokio::test]
    async fn stash_round_trip_preserves_both_halves_and_deletion_is_idempotent() {
        let root = directory();
        let stash = FileAccountStash::new(root.path().join("accounts"));
        let expected = account("u-A", "DUMMY-A");
        stash.write("Sessiometer/u-A", &expected).await.unwrap();
        let actual = stash.read("Sessiometer/u-A").await.unwrap();
        assert!(actual.credential.matches(&expected.credential));
        assert_eq!(actual.identity_bytes(), expected.identity_bytes());
        assert!(stash.any_record_present().unwrap());
        stash.delete("Sessiometer/u-A").await.unwrap();
        stash.delete("Sessiometer/u-A").await.unwrap();
        assert!(!stash.any_record_present().unwrap());
        assert!(matches!(
            stash.read("Sessiometer/u-A").await,
            Err(Error::StashIncomplete { .. })
        ));
    }

    #[test]
    fn account_names_resist_traversal_and_case_insensitive_collisions() {
        assert!(account_name("Sessiometer/../escape").is_err());
        assert!(account_name("Sessiometer/").is_err());
        assert!(account_name("not-an-account").is_err());
        assert_ne!(
            account_name("Sessiometer/u-A").unwrap(),
            account_name("Sessiometer/u-a").unwrap()
        );
    }

    #[tokio::test]
    async fn stash_refuses_mismatched_identity() {
        let root = directory();
        let stash = FileAccountStash::new(root.path().join("accounts"));
        assert!(stash
            .write("Sessiometer/u-A", &account("u-B", "DUMMY-B"))
            .await
            .is_err());
        assert!(!stash.any_record_present().unwrap());
    }

    #[tokio::test]
    async fn corrupt_stash_version_is_not_read_as_a_valid_account() {
        let root = directory();
        let stash_root = root.path().join("accounts");
        let stash = FileAccountStash::new(stash_root.clone());
        stash
            .write("Sessiometer/u-A", &account("u-A", "DUMMY-A"))
            .await
            .unwrap();
        let name = account_name("Sessiometer/u-A").unwrap();
        let bytes = read_private(&stash_root, &name, RECORD_LIMIT).unwrap();
        let mut value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        value["version"] = serde_json::json!(999);
        let altered = Zeroizing::new(serde_json::to_vec(&value).unwrap());
        replace_private(&stash_root, &name, &altered, RECORD_LIMIT, false).unwrap();
        assert!(matches!(
            stash.read("Sessiometer/u-A").await,
            Err(Error::CredentialFile { .. })
        ));
    }

    #[tokio::test]
    async fn isolated_file_operations_work_without_claiming_claude_persistence() {
        let root = directory();
        let isolated = FileIsolatedStore::new(
            root.path().to_path_buf(),
            Authority(true),
            None::<crate::keychain::IsolatedKeychainItem>,
        );
        let expected = credential("DUMMY-A");
        isolated.seed(expected.expose()).await.unwrap();
        assert!(isolated.read_back().await.unwrap().matches(&expected));
        isolated.delete().await.unwrap();
        isolated.delete().await.unwrap();
        isolated.delete_blocking();
    }

    #[derive(Default)]
    struct HandoffState {
        credential: Option<Credential>,
        read_fails: bool,
        delete_fails: bool,
        other_shadow: bool,
        copy_seen_before_delete: bool,
        events: Vec<&'static str>,
    }

    #[derive(Clone)]
    struct HandoffItem {
        state: std::rc::Rc<std::cell::RefCell<HandoffState>>,
        profile: PathBuf,
    }

    impl IsolatedKeychain for HandoffItem {
        async fn seed(&self, _blob: &[u8]) -> Result<()> {
            panic!("file-mode seeding must not write Keychain");
        }

        async fn read_back(&self) -> Result<Credential> {
            let mut state = self.state.borrow_mut();
            state.events.push("keychain-read");
            if state.read_fails {
                return Err(Error::KeychainLocked { op: "read" });
            }
            state
                .credential
                .as_ref()
                .map(|value| Credential::new(value.expose().to_vec()))
                .ok_or(Error::CredentialNotFound)
        }

        async fn delete(&self) -> Result<()> {
            let mut state = self.state.borrow_mut();
            state.events.push("keychain-delete");
            if let Some(value) = &state.credential {
                state.copy_seen_before_delete =
                    read_private(&self.profile, CREDENTIAL_NAME, CREDENTIAL_LIMIT)
                        .is_ok_and(|bytes| bytes.as_slice() == value.expose());
            }
            if state.delete_fails {
                return Err(Error::KeychainLocked { op: "delete" });
            }
            state.credential = None;
            Ok(())
        }

        fn delete_blocking(&self) {
            let mut state = self.state.borrow_mut();
            state.events.push("keychain-delete-blocking");
            if !state.delete_fails {
                state.credential = None;
            }
        }
    }

    struct HandoffAuthority(std::rc::Rc<std::cell::RefCell<HandoffState>>);

    impl FileAuthority for HandoffAuthority {
        async fn verify(&self) -> Result<()> {
            let mut state = self.0.borrow_mut();
            state.events.push("authority");
            if state.credential.is_some() || state.other_shadow {
                Err(Error::CredentialSourceConflict)
            } else {
                Ok(())
            }
        }
    }

    fn handoff_fixture(
        profile: &Path,
    ) -> (
        FileIsolatedStore<HandoffAuthority, HandoffItem>,
        std::rc::Rc<std::cell::RefCell<HandoffState>>,
    ) {
        let state = std::rc::Rc::new(std::cell::RefCell::new(HandoffState::default()));
        let isolated = FileIsolatedStore::new(
            profile.to_path_buf(),
            HandoffAuthority(state.clone()),
            Some(HandoffItem {
                state: state.clone(),
                profile: profile.to_path_buf(),
            }),
        );
        (isolated, state)
    }

    #[tokio::test]
    async fn handoff_prefers_keychain_copies_before_delete_and_preserves_full_bytes() {
        for file_removed_by_claude in [false, true] {
            let root = directory();
            let (isolated, state) = handoff_fixture(root.path());
            isolated
                .seed(credential("DUMMY-OLD").expose())
                .await
                .unwrap();
            let identity = root.path().join(".claude.json");
            fs::write(
                &identity,
                b"identity is untouched by credential normalization",
            )
            .unwrap();
            if file_removed_by_claude {
                remove_private(root.path(), CREDENTIAL_NAME).unwrap();
            }
            state.borrow_mut().credential = Some(credential("DUMMY-FRESH"));
            state.borrow_mut().events.clear();

            let result = isolated.read_back().await.unwrap();
            assert!(result.matches(&credential("DUMMY-FRESH")));
            let state = state.borrow();
            assert!(state.copy_seen_before_delete);
            assert!(state.credential.is_none());
            assert_eq!(
                state.events,
                ["keychain-read", "keychain-delete", "authority"]
            );
            assert_eq!(
                fs::metadata(root.path().join(CREDENTIAL_NAME))
                    .unwrap()
                    .mode()
                    & 0o777,
                0o600
            );
            assert_eq!(
                fs::read(identity).unwrap(),
                b"identity is untouched by credential normalization"
            );
        }
    }

    #[tokio::test]
    async fn handoff_absent_keychain_uses_file_or_preserves_missing_result() {
        let root = directory();
        let (isolated, state) = handoff_fixture(root.path());
        assert!(matches!(
            isolated.read_back().await,
            Err(Error::CredentialNotFound)
        ));
        isolated
            .seed(credential("DUMMY-FILE").expose())
            .await
            .unwrap();
        state.borrow_mut().events.clear();
        assert!(isolated
            .read_back()
            .await
            .unwrap()
            .matches(&credential("DUMMY-FILE")));
        assert_eq!(state.borrow().events, ["keychain-read", "authority"]);
    }

    #[tokio::test]
    async fn handoff_keychain_read_error_never_falls_back_to_stale_file() {
        let root = directory();
        let (isolated, state) = handoff_fixture(root.path());
        isolated
            .seed(credential("DUMMY-OLD").expose())
            .await
            .unwrap();
        state.borrow_mut().read_fails = true;
        state.borrow_mut().events.clear();
        assert!(matches!(
            isolated.read_back().await,
            Err(Error::KeychainLocked { .. })
        ));
        assert_eq!(state.borrow().events, ["keychain-read"]);
        let file = read_private(root.path(), CREDENTIAL_NAME, CREDENTIAL_LIMIT).unwrap();
        assert_eq!(file.as_slice(), credential("DUMMY-OLD").expose());
    }

    #[tokio::test]
    async fn handoff_invalid_or_oversized_output_is_not_published_or_deleted() {
        for bytes in [
            b"not JSON".to_vec(),
            vec![b'x'; CREDENTIAL_LIMIT as usize + 1],
        ] {
            let root = directory();
            let (isolated, state) = handoff_fixture(root.path());
            isolated
                .seed(credential("DUMMY-OLD").expose())
                .await
                .unwrap();
            state.borrow_mut().credential = Some(Credential::new(bytes));
            state.borrow_mut().events.clear();
            assert!(matches!(
                isolated.read_back().await,
                Err(Error::CredentialFile { .. })
            ));
            assert!(state.borrow().credential.is_some());
            assert_eq!(state.borrow().events, ["keychain-read"]);
            let file = read_private(root.path(), CREDENTIAL_NAME, CREDENTIAL_LIMIT).unwrap();
            assert_eq!(file.as_slice(), credential("DUMMY-OLD").expose());
        }
    }

    #[tokio::test]
    async fn handoff_failed_file_publish_does_not_delete_the_keychain_source() {
        let root = directory();
        let (isolated, state) = handoff_fixture(root.path());
        isolated
            .seed(credential("DUMMY-OLD").expose())
            .await
            .unwrap();
        fs::create_dir(
            root.path()
                .join(format!("{CREDENTIAL_NAME}.sessiometer-tmp")),
        )
        .unwrap();
        state.borrow_mut().credential = Some(credential("DUMMY-FRESH"));
        state.borrow_mut().events.clear();
        assert!(isolated.read_back().await.is_err());
        assert!(state.borrow().credential.is_some());
        assert_eq!(state.borrow().events, ["keychain-read"]);
        let file = read_private(root.path(), CREDENTIAL_NAME, CREDENTIAL_LIMIT).unwrap();
        assert_eq!(file.as_slice(), credential("DUMMY-OLD").expose());
    }

    #[tokio::test]
    async fn handoff_failed_keychain_delete_is_an_error_after_file_publication() {
        let root = directory();
        let (isolated, state) = handoff_fixture(root.path());
        state.borrow_mut().credential = Some(credential("DUMMY-FRESH"));
        state.borrow_mut().delete_fails = true;
        assert!(matches!(
            isolated.read_back().await,
            Err(Error::KeychainLocked { .. })
        ));
        assert!(state.borrow().copy_seen_before_delete);
        assert!(state.borrow().credential.is_some());
        assert_eq!(state.borrow().events, ["keychain-read", "keychain-delete"]);
    }

    #[tokio::test]
    async fn handoff_rechecks_authority_instead_of_returning_the_keychain_blob_directly() {
        let root = directory();
        let (isolated, state) = handoff_fixture(root.path());
        state.borrow_mut().credential = Some(credential("DUMMY-FRESH"));
        state.borrow_mut().other_shadow = true;
        assert!(matches!(
            isolated.read_back().await,
            Err(Error::CredentialSourceConflict)
        ));
        assert!(state.borrow().credential.is_none());
        assert_eq!(
            state.borrow().events,
            ["keychain-read", "keychain-delete", "authority"]
        );
    }

    #[tokio::test]
    async fn handoff_cleanup_attempts_files_even_if_keychain_delete_fails() {
        let root = directory();
        let (isolated, state) = handoff_fixture(root.path());
        isolated
            .seed(credential("DUMMY-OLD").expose())
            .await
            .unwrap();
        state.borrow_mut().credential = Some(credential("DUMMY-FRESH"));
        state.borrow_mut().delete_fails = true;
        assert!(isolated.delete().await.is_err());
        assert!(!root.path().join(CREDENTIAL_NAME).exists());
        assert!(state.borrow().credential.is_some());
        state.borrow_mut().delete_fails = false;
        isolated.delete().await.unwrap();
        isolated.delete().await.unwrap();
        assert!(state.borrow().credential.is_none());
    }

    #[tokio::test]
    async fn handoff_drop_cleanup_removes_only_its_owned_workspace() {
        let root = directory();
        let profile = root.path().join("isolated");
        crate::paths::create_isolated_dir(&profile).unwrap();
        let (isolated, state) = handoff_fixture(&profile);
        isolated
            .seed(credential("DUMMY-OLD").expose())
            .await
            .unwrap();
        state.borrow_mut().credential = Some(credential("DUMMY-FRESH"));
        let sibling = root.path().join("untouched");
        fs::write(&sibling, b"not owned by the session").unwrap();
        drop(crate::isolated_spawn::IsolatedSession::arm(
            isolated,
            profile.clone(),
        ));
        assert!(state.borrow().credential.is_none());
        assert!(!profile.exists());
        assert_eq!(fs::read(sibling).unwrap(), b"not owned by the session");
    }

    struct KeychainWritingRefreshChild {
        state: std::rc::Rc<std::cell::RefCell<HandoffState>>,
        concurrent_stash: Option<PathBuf>,
    }

    impl crate::isolated_spawn::ClaudeRefresh for KeychainWritingRefreshChild {
        async fn run(&self, profile: &Path) -> Result<()> {
            let seed = read_private(profile, CREDENTIAL_NAME, CREDENTIAL_LIMIT)?;
            assert_eq!(crate::refresh::expires_at(&seed), Some(1_000));
            if let Some(root) = &self.concurrent_stash {
                FileAccountStash::new(root.clone())
                    .write("Sessiometer/A", &account("A", "DUMMY-CONCURRENT"))
                    .await?;
            }
            self.state.borrow_mut().credential = Some(credential("DUMMY-FRESH"));
            remove_private(profile, CREDENTIAL_NAME)
        }
    }

    #[tokio::test]
    async fn handoff_refresh_uses_existing_classification_cas_and_teardown() {
        for concurrent in [false, true] {
            let root = directory();
            let account_root = root.path().join("accounts");
            let stash = FileAccountStash::new(account_root.clone());
            stash
                .write("Sessiometer/A", &account("A", "DUMMY-OLD"))
                .await
                .unwrap();
            let profile = root.path().join("isolated");
            let (isolated, state) = handoff_fixture(&profile);
            let lock = root.path().join("swap.lock");
            let report = crate::refresh::refresh_cycle(
                &stash,
                "Sessiometer/A",
                isolated,
                &KeychainWritingRefreshChild {
                    state: state.clone(),
                    concurrent_stash: concurrent.then_some(account_root),
                },
                profile.clone(),
                Some((&lock, Duration::from_secs(1))),
                1_000,
            )
            .await
            .unwrap();
            assert!(matches!(
                report.outcome,
                crate::refresh::RefreshOutcome::Refreshed { .. }
            ));
            assert_eq!(report.re_stashed, !concurrent);
            let expected = if concurrent {
                "DUMMY-CONCURRENT"
            } else {
                "DUMMY-FRESH"
            };
            assert!(stash
                .read("Sessiometer/A")
                .await
                .unwrap()
                .credential
                .matches(&credential(expected)));
            assert!(state.borrow().credential.is_none());
            assert!(state.borrow().copy_seen_before_delete);
            assert!(!profile.exists());
        }
    }
    struct LiveImportItem {
        helper: HandoffItem,
        stash_root: PathBuf,
        service: String,
        change_before_delete: std::cell::Cell<bool>,
    }

    impl CredentialStore for LiveImportItem {
        async fn read(&self) -> Result<Credential> {
            self.helper.read_back().await
        }

        async fn write(&self, _: &Credential) -> Result<()> {
            panic!("a live import must never write Keychain");
        }
    }

    impl crate::keychain::FileImportSource for LiveImportItem {
        async fn delete_if_matches(&self, expected: &Credential) -> Result<()> {
            if self.change_before_delete.get() {
                self.helper.state.borrow_mut().credential = Some(credential("DUMMY-NEWER"));
            }
            if !self.read().await?.matches(expected) {
                return Err(Error::CredentialSourceConflict);
            }
            let saved = FileAccountStash::new(self.stash_root.clone())
                .read(&self.service)
                .await?;
            assert!(
                saved.credential.matches(expected),
                "stash must precede delete"
            );
            assert_eq!(
                read_private(&self.helper.profile, CREDENTIAL_NAME, CREDENTIAL_LIMIT)?.as_slice(),
                expected.expose(),
                "active file must precede delete",
            );
            self.helper.delete().await
        }
    }

    struct LiveImportFixture {
        _root: TempDir,
        store: FileCredentialStore<HandoffAuthority>,
        source: LiveImportItem,
        stash: FileAccountStash,
        roster: Vec<crate::config::Account>,
        identity: PathBuf,
    }

    impl LiveImportFixture {
        async fn new() -> Self {
            let root = directory();
            let profile = root.path().join("claude");
            let stash_root = root.path().join("accounts");
            let state = std::rc::Rc::new(std::cell::RefCell::new(HandoffState::default()));
            let store = FileCredentialStore::new(profile.clone(), HandoffAuthority(state.clone()));
            store.write(&credential("DUMMY-OLD")).await.unwrap();
            let stash = FileAccountStash::new(stash_root.clone());
            stash
                .write("Sessiometer/u-A", &account("u-A", "DUMMY-OLD"))
                .await
                .unwrap();
            stash
                .write("Sessiometer/u-B", &account("u-B", "DUMMY-B"))
                .await
                .unwrap();
            let identity = profile.join(".claude.json");
            fs::write(
                &identity,
                br#"{"oauthAccount":{"accountUuid":"u-A"},"unrelated":true}"#,
            )
            .unwrap();
            state.borrow_mut().credential = Some(credential("DUMMY-FRESH"));
            let source = LiveImportItem {
                helper: HandoffItem { state, profile },
                stash_root,
                service: "Sessiometer/u-A".into(),
                change_before_delete: std::cell::Cell::new(false),
            };
            let roster = ["u-A", "u-B"]
                .into_iter()
                .map(|id| crate::config::Account {
                    key: crate::config::AccountKey::ClaudeUuid(id.into()),
                    label: id.into(),
                    enabled: true,
                })
                .collect();
            Self {
                _root: root,
                store,
                source,
                stash,
                roster,
                identity,
            }
        }

        async fn import(&self) -> Result<Option<usize>> {
            self.store
                .import_keychain(&self.source, &self.stash, &self.roster, &self.identity)
                .await
        }

        fn source_matches(&self, token: &str) -> bool {
            self.source
                .helper
                .state
                .borrow()
                .credential
                .as_ref()
                .is_some_and(|current| current.matches(&credential(token)))
        }
    }

    #[tokio::test]
    async fn live_import_preserves_both_copies_before_delete_and_round_trips_a_b_a() {
        for file_removed in [false, true] {
            let f = LiveImportFixture::new().await;
            let identity_before = fs::read(&f.identity).unwrap();
            let saved_identity = f
                .stash
                .read("Sessiometer/u-A")
                .await
                .unwrap()
                .identity_bytes()
                .to_vec();
            if file_removed {
                remove_private(&f.store.profile, CREDENTIAL_NAME).unwrap();
            }
            assert_eq!(f.import().await.unwrap(), Some(0));
            assert!(f.source.helper.state.borrow().credential.is_none());
            assert!(f
                .store
                .read()
                .await
                .unwrap()
                .matches(&credential("DUMMY-FRESH")));
            let a = f.stash.read("Sessiometer/u-A").await.unwrap();
            assert!(a.credential.matches(&credential("DUMMY-FRESH")));
            assert_eq!(a.identity_bytes(), saved_identity);
            assert_eq!(fs::read(&f.identity).unwrap(), identity_before);
            crate::swap::swap(
                &f.store,
                &f.stash,
                "Sessiometer/u-A",
                "Sessiometer/u-B",
                &f.identity,
            )
            .await
            .unwrap();
            crate::swap::swap(
                &f.store,
                &f.stash,
                "Sessiometer/u-B",
                "Sessiometer/u-A",
                &f.identity,
            )
            .await
            .unwrap();
            assert!(f
                .store
                .read()
                .await
                .unwrap()
                .matches(&credential("DUMMY-FRESH")));
            assert_eq!(f.import().await.unwrap(), None);
        }
    }

    #[tokio::test]
    async fn live_import_no_shadow_does_not_read_or_mutate_keychain() {
        let f = LiveImportFixture::new().await;
        f.source.helper.state.borrow_mut().credential = None;
        f.source.helper.state.borrow_mut().events.clear();
        assert_eq!(f.import().await.unwrap(), None);
        assert_eq!(f.source.helper.state.borrow().events, ["authority"]);
        assert!(f
            .stash
            .read("Sessiometer/u-A")
            .await
            .unwrap()
            .credential
            .matches(&credential("DUMMY-OLD")));
    }

    #[tokio::test]
    async fn live_import_invalid_or_unattributed_source_is_never_deleted_or_saved() {
        for invalid in [false, true] {
            let f = LiveImportFixture::new().await;
            if invalid {
                f.source.helper.state.borrow_mut().credential =
                    Some(Credential::new(b"not-json".to_vec()));
            } else {
                fs::write(
                    &f.identity,
                    br#"{"oauthAccount":{"accountUuid":"not-rostered"}}"#,
                )
                .unwrap();
            }
            assert!(f.import().await.is_err());
            assert!(f.source.helper.state.borrow().credential.is_some());
            assert!(!f
                .source
                .helper
                .state
                .borrow()
                .events
                .contains(&"keychain-delete"));
            assert!(f
                .stash
                .read("Sessiometer/u-A")
                .await
                .unwrap()
                .credential
                .matches(&credential("DUMMY-OLD")));
            assert_eq!(
                read_private(&f.store.profile, CREDENTIAL_NAME, CREDENTIAL_LIMIT)
                    .unwrap()
                    .as_slice(),
                credential("DUMMY-OLD").expose()
            );
        }
    }

    #[tokio::test]
    async fn live_import_locked_source_preserves_everything() {
        let f = LiveImportFixture::new().await;
        f.source.helper.state.borrow_mut().read_fails = true;
        assert!(matches!(
            f.import().await,
            Err(Error::KeychainLocked { .. })
        ));
        assert!(f.source_matches("DUMMY-FRESH"));
        assert!(f
            .stash
            .read("Sessiometer/u-A")
            .await
            .unwrap()
            .credential
            .matches(&credential("DUMMY-OLD")));
    }

    #[tokio::test]
    async fn live_import_stash_failure_leaves_source_and_active_file_untouched() {
        let f = LiveImportFixture::new().await;
        // An occupied staging directory makes the account write fail, without
        // making the existing account unreadable.
        let name = account_name("Sessiometer/u-A").unwrap();
        fs::create_dir(f.source.stash_root.join(format!("{name}.sessiometer-tmp"))).unwrap();
        assert!(f.import().await.is_err());
        assert!(f.source_matches("DUMMY-FRESH"));
        assert_eq!(
            read_private(&f.store.profile, CREDENTIAL_NAME, CREDENTIAL_LIMIT)
                .unwrap()
                .as_slice(),
            credential("DUMMY-OLD").expose()
        );
    }

    #[tokio::test]
    async fn live_import_active_file_failure_keeps_source_and_fresh_stash() {
        let f = LiveImportFixture::new().await;
        fs::create_dir(
            f.store
                .profile
                .join(format!("{CREDENTIAL_NAME}.sessiometer-tmp")),
        )
        .unwrap();
        assert!(f.import().await.is_err());
        assert!(f.source_matches("DUMMY-FRESH"));
        assert!(f
            .stash
            .read("Sessiometer/u-A")
            .await
            .unwrap()
            .credential
            .matches(&credential("DUMMY-FRESH")));
    }

    #[tokio::test]
    async fn live_import_delete_failure_is_retryable_without_losing_either_copy() {
        let f = LiveImportFixture::new().await;
        f.source.helper.state.borrow_mut().delete_fails = true;
        assert!(f.import().await.is_err());
        assert!(f.source_matches("DUMMY-FRESH"));
        assert!(f
            .stash
            .read("Sessiometer/u-A")
            .await
            .unwrap()
            .credential
            .matches(&credential("DUMMY-FRESH")));
        assert!(matches!(
            f.store.read().await,
            Err(Error::CredentialSourceConflict)
        ));
        f.source.helper.state.borrow_mut().delete_fails = false;
        assert_eq!(f.import().await.unwrap(), Some(0));
        assert!(f.source.helper.state.borrow().credential.is_none());
    }

    #[tokio::test]
    async fn live_import_changed_source_is_not_deleted() {
        let f = LiveImportFixture::new().await;
        f.source.change_before_delete.set(true);
        assert!(matches!(
            f.import().await,
            Err(Error::CredentialSourceConflict)
        ));
        assert!(f.source_matches("DUMMY-NEWER"));
        assert!(f
            .stash
            .read("Sessiometer/u-A")
            .await
            .unwrap()
            .credential
            .matches(&credential("DUMMY-FRESH")));
        f.source.change_before_delete.set(false);
        assert_eq!(f.import().await.unwrap(), Some(0));
        assert!(f
            .store
            .read()
            .await
            .unwrap()
            .matches(&credential("DUMMY-NEWER")));
    }

    #[tokio::test]
    async fn live_import_remaining_shadow_keeps_file_reads_blocked() {
        let f = LiveImportFixture::new().await;
        f.source.helper.state.borrow_mut().other_shadow = true;
        assert!(matches!(
            f.import().await,
            Err(Error::CredentialSourceConflict)
        ));
        assert!(f
            .stash
            .read("Sessiometer/u-A")
            .await
            .unwrap()
            .credential
            .matches(&credential("DUMMY-FRESH")));
        assert!(matches!(
            f.store.read().await,
            Err(Error::CredentialSourceConflict)
        ));
    }

    #[tokio::test]
    async fn live_import_token_match_wins_over_a_stale_display() {
        let mut f = LiveImportFixture::new().await;
        f.source.helper.state.borrow_mut().credential = Some(credential("DUMMY-B"));
        f.source.service = "Sessiometer/u-B".into();
        assert_eq!(f.import().await.unwrap(), Some(1));
        assert!(f
            .stash
            .read("Sessiometer/u-A")
            .await
            .unwrap()
            .credential
            .matches(&credential("DUMMY-OLD")));
        assert!(f
            .store
            .read()
            .await
            .unwrap()
            .matches(&credential("DUMMY-B")));
    }

    #[tokio::test]
    async fn live_import_token_only_account_keeps_absent_identity() {
        let mut f = LiveImportFixture::new().await;
        let id = "local-0123456789abcdef0123456789abcdef";
        let service = format!("Sessiometer/{id}");
        f.stash
            .write(
                &service,
                &StashedAccount {
                    credential: credential("DUMMY-LONG"),
                    oauth_account: None,
                },
            )
            .await
            .unwrap();
        f.roster.push(crate::config::Account {
            key: crate::config::AccountKey::LocalId(id.into()),
            label: "token-only".into(),
            enabled: true,
        });
        f.source.service = service.clone();
        f.source.helper.state.borrow_mut().credential = Some(credential("DUMMY-LONG"));
        assert_eq!(f.import().await.unwrap(), Some(2));
        assert!(f
            .stash
            .read(&service)
            .await
            .unwrap()
            .oauth_account
            .is_none());
    }
}
