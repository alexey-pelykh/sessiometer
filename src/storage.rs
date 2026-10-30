// Copyright (c) 2026 Oleksii PELYKH
// SPDX-License-Identifier: MIT

//! Backend selection. Existing engine traits remain the integration contracts.

use std::ffi::{OsStr, OsString};
use std::path::{Component, Path, PathBuf};

use unicode_normalization::UnicodeNormalization;

use crate::config::{StorageBackend, StorageConfig};
use crate::error::{Error, Result};
#[cfg(unix)]
use crate::file_storage::{
    FileAccountStash, FileCredentialStore, FileIsolatedStore, NativeFileAuthority,
};
use crate::keychain::{
    Credential, CredentialStore, IsolatedKeychain, IsolatedKeychainItem, RealCredentialStore,
};
use crate::stash::{AccountStash, RealAccountStash, StashedAccount};

/// One resolved profile, not a profile-management subsystem. Keep launch spelling
/// separate from filesystem paths because the Keychain hash uses the former.
#[derive(Clone)]
struct ClaudeProfile {
    directory: PathBuf,
    state_file: PathBuf,
    config_dir: Option<OsString>,
}

impl ClaudeProfile {
    fn resolve(
        configured: Option<&Path>,
        inherited: Option<&OsStr>,
        home: &Path,
        cwd: &Path,
    ) -> Result<Self> {
        let config_dir = configured
            .map(Path::as_os_str)
            .or(inherited)
            .map(OsStr::to_owned);
        let default_directory = home.join(".claude");
        let value = config_dir
            .as_deref()
            .unwrap_or(default_directory.as_os_str());
        let text = value.to_str().ok_or_else(|| {
            Error::ConfigInvalid("Claude profile path must be valid UTF-8".into())
        })?;
        let normalized: String = text.nfc().collect();
        let directory = absolute_lexical(Path::new(&normalized), cwd);

        // Claude's ordinary state-file fallback uses the original non-empty
        // override, not the NFC credential-directory spelling. Empty means home.
        let state_root = config_dir
            .as_deref()
            .filter(|value| !value.is_empty())
            .map(Path::new)
            .unwrap_or(home);
        let legacy = directory.join(".config.json");
        let state_file = match std::fs::symlink_metadata(&legacy) {
            Ok(info) if info.is_file() => legacy,
            Ok(_) => {
                return Err(Error::ConfigInvalid(
                    "unsafe legacy Claude state file".into(),
                ))
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                absolute_lexical(state_root, cwd).join(".claude.json")
            }
            Err(error) => return Err(error.into()),
        };
        Ok(Self {
            directory,
            state_file,
            config_dir,
        })
    }
}

/// Match lexical path joining without filesystem canonicalization. The supported
/// runtime targets are Unix; Windows runtime/path enablement is separate work.
fn absolute_lexical(path: &Path, cwd: &Path) -> PathBuf {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        cwd.join(path)
    };
    let mut result = PathBuf::new();
    for component in absolute.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                result.pop();
            }
            other => result.push(other.as_os_str()),
        }
    }
    result
}

pub(crate) enum CanonicalStore {
    Keychain(RealCredentialStore),
    #[cfg(unix)]
    File(FileCredentialStore),
}

impl CredentialStore for CanonicalStore {
    async fn read(&self) -> Result<Credential> {
        match self {
            Self::Keychain(store) => store.read().await,
            #[cfg(unix)]
            Self::File(store) => store.read().await,
        }
    }

    async fn write(&self, credential: &Credential) -> Result<()> {
        match self {
            Self::Keychain(store) => store.write(credential).await,
            #[cfg(unix)]
            Self::File(store) => store.write(credential).await,
        }
    }

    async fn probe_resolution(&self) -> Result<()> {
        match self {
            Self::Keychain(store) => store.probe_resolution().await,
            #[cfg(unix)]
            Self::File(store) => store.probe_resolution().await,
        }
    }
}

pub(crate) enum AccountStore {
    Keychain(RealAccountStash),
    #[cfg(unix)]
    File(FileAccountStash),
}

impl AccountStash for AccountStore {
    async fn write(&self, service: &str, account: &StashedAccount) -> Result<()> {
        match self {
            Self::Keychain(stash) => stash.write(service, account).await,
            #[cfg(unix)]
            Self::File(stash) => stash.write(service, account).await,
        }
    }

    async fn read(&self, service: &str) -> Result<StashedAccount> {
        match self {
            Self::Keychain(stash) => stash.read(service).await,
            #[cfg(unix)]
            Self::File(stash) => stash.read(service).await,
        }
    }

    async fn delete(&self, service: &str) -> Result<()> {
        match self {
            Self::Keychain(stash) => stash.delete(service).await,
            #[cfg(unix)]
            Self::File(stash) => stash.delete(service).await,
        }
    }
}

pub(crate) enum IsolatedStore {
    Keychain(IsolatedKeychainItem),
    #[cfg(unix)]
    File(FileIsolatedStore),
}

impl IsolatedKeychain for IsolatedStore {
    async fn seed(&self, blob: &[u8]) -> Result<()> {
        match self {
            Self::Keychain(store) => store.seed(blob).await,
            #[cfg(unix)]
            Self::File(store) => store.seed(blob).await,
        }
    }

    async fn read_back(&self) -> Result<Credential> {
        match self {
            Self::Keychain(store) => store.read_back().await,
            #[cfg(unix)]
            Self::File(store) => store.read_back().await,
        }
    }

    async fn delete(&self) -> Result<()> {
        match self {
            Self::Keychain(store) => store.delete().await,
            #[cfg(unix)]
            Self::File(store) => store.delete().await,
        }
    }

    fn delete_blocking(&self) {
        match self {
            Self::Keychain(store) => store.delete_blocking(),
            #[cfg(unix)]
            Self::File(store) => store.delete_blocking(),
        }
    }
}

/// Immutable per-process selection; deliberately contains no credentials.
#[derive(Clone)]
pub(crate) struct Storage {
    settings: StorageConfig,
    profile: ClaudeProfile,
    keychain_service: Option<String>,
    account_root: PathBuf,
}

impl Storage {
    pub(crate) fn from_config(settings: &StorageConfig) -> Result<Self> {
        Self::from_inputs(
            settings,
            std::env::var_os("CLAUDE_CONFIG_DIR").as_deref(),
            std::env::var_os("CLAUDE_SECURESTORAGE_CONFIG_DIR").as_deref(),
            &crate::paths::claude_home_dir()?,
            &std::env::current_dir()?,
        )
    }

    /// Injectable non-secret inputs: tests never mutate the process environment.
    fn from_inputs(
        settings: &StorageConfig,
        inherited: Option<&OsStr>,
        securestorage: Option<&OsStr>,
        home: &Path,
        cwd: &Path,
    ) -> Result<Self> {
        settings.validate()?;
        let profile =
            ClaudeProfile::resolve(settings.claude_config_dir.as_deref(), inherited, home, cwd)?;
        let keychain_service = {
            #[cfg(target_os = "macos")]
            {
                if settings.backend == StorageBackend::File && securestorage.is_some() {
                    return Err(Error::ConfigInvalid(
                        "unset CLAUDE_SECURESTORAGE_CONFIG_DIR when using file storage".into(),
                    ));
                }
                Some(crate::keychain::service_for_selection(
                    securestorage,
                    profile.config_dir.as_deref(),
                )?)
            }
            #[cfg(not(target_os = "macos"))]
            {
                let _ = (securestorage, &profile.config_dir);
                None
            }
        };
        Ok(Self {
            settings: settings.clone(),
            profile,
            keychain_service,
            account_root: crate::paths::support_dir()?.join("file-accounts"),
        })
    }

    pub(crate) fn settings(&self) -> &StorageConfig {
        &self.settings
    }

    pub(crate) fn state_file(&self) -> &Path {
        &self.profile.state_file
    }

    pub(crate) fn account_root(&self) -> &Path {
        &self.account_root
    }

    /// Check a daemon-owned config without changing this process's pinned selection.
    pub(crate) fn require_config_path(&self, path: &Path) -> Result<()> {
        self.require_same_settings(&crate::config::Config::load_path(path)?.storage)
    }

    pub(crate) fn require_same_settings(&self, configured: &StorageConfig) -> Result<()> {
        if self.settings != *configured {
            return Err(Error::StorageRestartRequired);
        }
        Ok(())
    }

    /// Recover a live-profile Keychain write only for macOS file storage.
    /// Keychain mode and non-macOS platforms do not probe, read, or delete it.
    pub(crate) async fn recover_file_login(
        &self,
        roster: &[crate::config::Account],
        config_path: &Path,
        lock_path: &Path,
    ) -> Result<Option<usize>> {
        #[cfg(target_os = "macos")]
        {
            use crate::file_storage::FileAuthority;

            if self.settings.backend != StorageBackend::File {
                return Ok(None);
            }
            let authority = NativeFileAuthority::new(self.keychain_service.as_deref())?;
            // No contention on the ordinary no-shadow path. The locked importer
            // checks again before reading or mutating anything.
            match authority.verify().await {
                Ok(()) => return Ok(None),
                Err(Error::CredentialSourceConflict) => {}
                Err(error) => return Err(error),
            }
            let _guard =
                crate::swap::SwapLock::acquire(lock_path, crate::swap::SWAP_LOCK_MAX_WAIT).await?;
            self.require_config_path(config_path)?;
            let service = self
                .keychain_service
                .as_ref()
                .ok_or(Error::CredentialFile {
                    reason: "missing profile Keychain namespace",
                })?;
            // A new handle per import: resolve the currently stored account
            // attribute rather than inheriting a boot-time cached resolution.
            let source = RealCredentialStore::for_service(service.clone());
            let store = FileCredentialStore::new(self.profile.directory.clone(), authority);
            store
                .import_keychain(&source, &self.stash()?, roster, self.state_file())
                .await
        }
        #[cfg(not(target_os = "macos"))]
        {
            let _ = (roster, config_path, lock_path);
            Ok(None)
        }
    }

    pub(crate) fn canonical(&self) -> Result<CanonicalStore> {
        match self.settings.backend {
            StorageBackend::Keychain => {
                let service = self.keychain_service.as_ref().ok_or_else(|| {
                    Error::ConfigInvalid("Keychain storage is unavailable on this platform".into())
                })?;
                Ok(CanonicalStore::Keychain(RealCredentialStore::for_service(
                    service.clone(),
                )))
            }
            StorageBackend::File => {
                #[cfg(unix)]
                {
                    Ok(CanonicalStore::File(FileCredentialStore::new(
                        self.profile.directory.clone(),
                        NativeFileAuthority::new(self.keychain_service.as_deref())?,
                    )))
                }
                #[cfg(not(unix))]
                {
                    Err(Error::CredentialFile {
                        reason: "file backend is unavailable here",
                    })
                }
            }
        }
    }

    pub(crate) fn stash(&self) -> Result<AccountStore> {
        match self.settings.backend {
            StorageBackend::Keychain => Ok(AccountStore::Keychain(RealAccountStash::new())),
            StorageBackend::File => {
                #[cfg(unix)]
                {
                    Ok(AccountStore::File(FileAccountStash::new(
                        self.account_root().to_path_buf(),
                    )))
                }
                #[cfg(not(unix))]
                {
                    Err(Error::CredentialFile {
                        reason: "file backend is unavailable here",
                    })
                }
            }
        }
    }

    /// Select temporary storage for an owned login/refresh helper. File mode
    /// remains a file adapter; only macOS adds post-exit Keychain normalization.
    pub(crate) fn isolated(&self, profile: &Path) -> Result<IsolatedStore> {
        match self.settings.backend {
            StorageBackend::Keychain => Ok(IsolatedStore::Keychain(IsolatedKeychainItem::new(
                profile.as_os_str(),
            )?)),
            StorageBackend::File => {
                #[cfg(unix)]
                {
                    #[cfg(target_os = "macos")]
                    let (authority, keychain_output) = {
                        let service = crate::keychain::service_for_config_dir(profile.as_os_str())?;
                        (
                            NativeFileAuthority::new(Some(&service))?,
                            Some(IsolatedKeychainItem::new(profile.as_os_str())?),
                        )
                    };
                    #[cfg(not(target_os = "macos"))]
                    let (authority, keychain_output) = (NativeFileAuthority::new(None)?, None);

                    Ok(IsolatedStore::File(FileIsolatedStore::new(
                        profile.to_path_buf(),
                        authority,
                        keychain_output,
                    )))
                }
                #[cfg(not(unix))]
                {
                    Err(Error::CredentialFile {
                        reason: "file backend is unavailable here",
                    })
                }
            }
        }
    }
}

#[cfg(test)]
mod platform_tests {
    use super::*;
    use tempfile::TempDir;

    fn roots(temp: &TempDir) -> (PathBuf, PathBuf) {
        let home = temp.path().join("home");
        let cwd = temp.path().join("work");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(&cwd).unwrap();
        (home, cwd)
    }

    #[test]
    fn default_credentials_and_identity_have_different_parent_directories() {
        let temp = tempfile::tempdir().unwrap();
        let (home, cwd) = roots(&temp);
        let profile = ClaudeProfile::resolve(None, None, &home, &cwd).unwrap();
        assert_eq!(profile.directory, home.join(".claude"));
        assert_eq!(profile.state_file, home.join(".claude.json"));
        assert!(profile.config_dir.is_none());
    }

    #[test]
    fn explicit_override_wins_over_environment_and_supports_keychain_too() {
        let temp = tempfile::tempdir().unwrap();
        let (home, cwd) = roots(&temp);
        let explicit = temp.path().join("configured");
        let inherited = temp.path().join("inherited");
        let profile =
            ClaudeProfile::resolve(Some(&explicit), Some(inherited.as_os_str()), &home, &cwd)
                .unwrap();
        assert_eq!(profile.directory, explicit);
        assert_eq!(profile.state_file, explicit.join(".claude.json"));
        assert_eq!(profile.config_dir.as_deref(), Some(explicit.as_os_str()));
    }

    #[test]
    fn inherited_override_is_used_when_no_explicit_profile_exists() {
        let temp = tempfile::tempdir().unwrap();
        let (home, cwd) = roots(&temp);
        let inherited = temp.path().join("inherited");
        let profile =
            ClaudeProfile::resolve(None, Some(inherited.as_os_str()), &home, &cwd).unwrap();
        assert_eq!(profile.directory, inherited);
        assert_eq!(profile.state_file, inherited.join(".claude.json"));
    }

    #[test]
    fn empty_override_is_distinct_from_absent_and_wins_over_inherited_value() {
        let temp = tempfile::tempdir().unwrap();
        let (home, cwd) = roots(&temp);
        let profile = ClaudeProfile::resolve(
            Some(Path::new("")),
            Some(OsStr::new("ignored")),
            &home,
            &cwd,
        )
        .unwrap();
        assert_eq!(profile.directory, cwd);
        assert_eq!(profile.state_file, home.join(".claude.json"));
        assert_eq!(profile.config_dir.as_deref(), Some(OsStr::new("")));
    }

    #[test]
    fn relative_dot_segments_are_resolved_but_namespace_spelling_is_preserved() {
        let temp = tempfile::tempdir().unwrap();
        let (home, cwd) = roots(&temp);
        let raw = Path::new("../profile/./");
        let profile = ClaudeProfile::resolve(Some(raw), None, &home, &cwd).unwrap();
        assert_eq!(profile.directory, temp.path().join("profile"));
        assert_eq!(profile.state_file, temp.path().join("profile/.claude.json"));
        assert_eq!(profile.config_dir.as_deref(), Some(raw.as_os_str()));
        assert_eq!(
            absolute_lexical(Path::new("/../../profile"), &cwd),
            PathBuf::from("/profile")
        );
    }

    #[test]
    fn nfc_applies_to_credentials_without_rewriting_raw_identity_or_namespace_input() {
        let temp = tempfile::tempdir().unwrap();
        let (home, cwd) = roots(&temp);
        let raw = temp.path().join("cafe\u{301}");
        let profile = ClaudeProfile::resolve(Some(&raw), None, &home, &cwd).unwrap();
        assert_eq!(profile.directory, temp.path().join("café"));
        assert_eq!(profile.state_file, raw.join(".claude.json"));
        assert_eq!(profile.config_dir.as_deref(), Some(raw.as_os_str()));
    }

    #[test]
    fn legacy_state_wins_and_unsafe_legacy_state_does_not_fall_back() {
        let temp = tempfile::tempdir().unwrap();
        let (home, cwd) = roots(&temp);
        let dir = home.join(".claude");
        std::fs::create_dir(&dir).unwrap();
        let legacy = dir.join(".config.json");
        std::fs::write(&legacy, b"{}").unwrap();
        assert_eq!(
            ClaudeProfile::resolve(None, None, &home, &cwd)
                .unwrap()
                .state_file,
            legacy
        );
        std::fs::remove_file(&legacy).unwrap();
        std::fs::create_dir(&legacy).unwrap();
        assert!(ClaudeProfile::resolve(None, None, &home, &cwd).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn directory_aliases_are_not_canonicalized_and_non_utf8_is_not_lossily_decoded() {
        use std::os::unix::ffi::OsStringExt;
        use std::os::unix::fs::symlink;
        let temp = tempfile::tempdir().unwrap();
        let (home, cwd) = roots(&temp);
        let alias = temp.path().join("alias");
        symlink(&home, &alias).unwrap();
        let profile = ClaudeProfile::resolve(Some(&alias), None, &home, &cwd).unwrap();
        assert_eq!(profile.directory, alias);
        let invalid = OsString::from_vec(vec![0xff]);
        assert!(ClaudeProfile::resolve(None, Some(&invalid), &home, &cwd).is_err());
    }

    #[test]
    fn file_factory_supports_default_profile_without_creating_or_reading_credentials() {
        let temp = tempfile::tempdir().unwrap();
        let (home, cwd) = roots(&temp);
        let settings = StorageConfig {
            backend: StorageBackend::File,
            claude_config_dir: None,
        };
        let storage = Storage::from_inputs(&settings, None, None, &home, &cwd).unwrap();
        assert!(matches!(
            storage.canonical().unwrap(),
            CanonicalStore::File(_)
        ));
        assert!(matches!(storage.stash().unwrap(), AccountStore::File(_)));
        assert_eq!(storage.state_file(), home.join(".claude.json"));
        assert!(!home.join(".claude").exists());
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_default_and_explicit_default_directory_have_different_namespaces() {
        let temp = tempfile::tempdir().unwrap();
        let (home, cwd) = roots(&temp);
        let implicit =
            Storage::from_inputs(&StorageConfig::default(), None, None, &home, &cwd).unwrap();
        let explicit = Storage::from_inputs(
            &StorageConfig {
                backend: StorageBackend::Keychain,
                claude_config_dir: Some(home.join(".claude")),
            },
            None,
            None,
            &home,
            &cwd,
        )
        .unwrap();
        assert_eq!(
            implicit.keychain_service.as_deref(),
            Some("Claude Code-credentials")
        );
        assert_ne!(implicit.keychain_service, explicit.keychain_service);
        assert!(matches!(
            explicit.canonical().unwrap(),
            CanonicalStore::Keychain(_)
        ));
        assert_eq!(explicit.state_file(), home.join(".claude/.claude.json"));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_file_factory_constructs_without_io_and_still_refuses_legacy_override() {
        let temp = tempfile::tempdir().unwrap();
        let (home, cwd) = roots(&temp);
        let settings = StorageConfig {
            backend: StorageBackend::File,
            claude_config_dir: None,
        };
        let storage = Storage::from_inputs(&settings, None, None, &home, &cwd).unwrap();
        assert!(matches!(
            storage.isolated(&temp.path().join("isolated")).unwrap(),
            IsolatedStore::File(_)
        ));
        assert!(Storage::from_inputs(&settings, None, Some(OsStr::new("")), &home, &cwd).is_err());
        assert!(!temp.path().join("isolated").exists());
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn linux_default_file_factory_needs_no_keychain_namespace_or_command() {
        use crate::file_storage::FileAuthority;
        use std::os::unix::ffi::OsStringExt;
        let temp = tempfile::tempdir().unwrap();
        let (home, cwd) = roots(&temp);
        let invalid_legacy_namespace = OsString::from_vec(vec![0xff]);
        let storage = Storage::from_inputs(
            &StorageConfig::default(),
            None,
            Some(&invalid_legacy_namespace),
            &home,
            &cwd,
        )
        .unwrap();
        assert!(storage.keychain_service.is_none());
        assert!(matches!(
            storage.canonical().unwrap(),
            CanonicalStore::File(_)
        ));
        assert!(matches!(
            storage.isolated(&temp.path().join("isolated")).unwrap(),
            IsolatedStore::File(_)
        ));
        NativeFileAuthority::new(None)
            .unwrap()
            .verify()
            .await
            .unwrap();
        assert!(!home.join(".claude").exists());
    }
    #[cfg(target_os = "macos")]
    #[tokio::test]
    async fn live_import_is_inert_in_keychain_mode_before_any_path_access() {
        let temp = tempfile::tempdir().unwrap();
        let (home, cwd) = roots(&temp);
        let storage =
            Storage::from_inputs(&StorageConfig::default(), None, None, &home, &cwd).unwrap();
        assert_eq!(storage.settings.backend, StorageBackend::Keychain);
        let missing = temp.path().join("does-not-exist");
        assert_eq!(
            storage
                .recover_file_login(&[], &missing, &missing)
                .await
                .unwrap(),
            None
        );
        assert!(!missing.exists());
        assert!(!home.join(".claude").exists());
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn live_import_is_inert_on_linux_before_any_path_access() {
        let temp = tempfile::tempdir().unwrap();
        let (home, cwd) = roots(&temp);
        let storage =
            Storage::from_inputs(&StorageConfig::default(), None, None, &home, &cwd).unwrap();
        assert_eq!(storage.settings.backend, StorageBackend::File);
        let missing = temp.path().join("does-not-exist");
        assert_eq!(
            storage
                .recover_file_login(&[], &missing, &missing)
                .await
                .unwrap(),
            None
        );
        assert!(!missing.exists());
        assert!(!home.join(".claude").exists());
    }
}
