// Copyright (c) 2026 Oleksii PELYKH
// SPDX-License-Identifier: MIT

//! The active Claude Code credential in the macOS login keychain.
//!
//! Scaffolding scope: the opaque [`Credential`] carrier and the
//! [`CredentialStore`] seam. The real impl drives the `/usr/bin/security` CLI
//! (never the Security.framework SDK — a CI guard enforces this) and lands in
//! issue #2.

#[cfg(test)]
use std::cell::RefCell;

use crate::error::{Error, Result};

/// An opaque credential blob (the active account's OAuth tokens).
///
/// Deliberately does **not** derive `Debug`: issue #2 wraps this in a
/// zeroize-on-drop carrier, and no secret-bearing type may be printable.
#[derive(Clone, PartialEq)]
// Real construction lands with the keychain read (#2); for now only the
// in-memory test fake builds one, so the bin target sees it as unconstructed.
#[allow(dead_code)]
pub(crate) struct Credential(Vec<u8>);

#[allow(dead_code)]
impl Credential {
    /// Wrap a raw credential blob.
    pub(crate) fn new(blob: Vec<u8>) -> Self {
        Self(blob)
    }
}

/// Seam: reads/writes the active credential. The real impl drives the macOS
/// `security` CLI (#2); the test impl is an in-memory cell.
///
/// The daemon holds this seam but does not yet call it; the out-of-band swap
/// engine (#6/#7) reads and rewrites the credential through it.
#[allow(dead_code)]
pub(crate) trait CredentialStore {
    async fn read(&self) -> Result<Credential>;
    async fn write(&self, credential: &Credential) -> Result<()>;
}

/// Real keychain-backed store. Behavior lands in issue #2.
pub(crate) struct RealCredentialStore;

impl CredentialStore for RealCredentialStore {
    async fn read(&self) -> Result<Credential> {
        Err(Error::Unimplemented("keychain read (#2)"))
    }

    async fn write(&self, _credential: &Credential) -> Result<()> {
        Err(Error::Unimplemented("keychain write (#2)"))
    }
}

#[cfg(test)]
pub(crate) struct FakeCredentialStore {
    slot: RefCell<Option<Credential>>,
}

#[cfg(test)]
impl FakeCredentialStore {
    pub(crate) fn empty() -> Self {
        Self {
            slot: RefCell::new(None),
        }
    }
}

#[cfg(test)]
impl CredentialStore for FakeCredentialStore {
    async fn read(&self) -> Result<Credential> {
        self.slot
            .borrow()
            .clone()
            .ok_or(Error::Unimplemented("no credential stashed in the fake"))
    }

    async fn write(&self, credential: &Credential) -> Result<()> {
        *self.slot.borrow_mut() = Some(credential.clone());
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn fake_store_round_trips() {
        let store = FakeCredentialStore::empty();
        let cred = Credential::new(b"oauth-blob".to_vec());
        store.write(&cred).await.unwrap();
        // `Credential` has no `Debug`, so compare with `==` rather than `assert_eq!`.
        assert!(store.read().await.unwrap() == cred);
    }

    #[tokio::test]
    async fn real_store_reports_unimplemented() {
        let result = RealCredentialStore.read().await;
        assert!(matches!(result, Err(Error::Unimplemented(_))));
    }
}
