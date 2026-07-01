// Copyright (c) 2026 Oleksii PELYKH
// SPDX-License-Identifier: MIT

//! Active-account resolution — WHICH roster account the machine is currently
//! logged into, resolved TOKEN-FIRST.
//!
//! The canonical keychain credential is the authoritative bearer of the active
//! session; `~/.claude.json`'s `oauthAccount` is only the clobberable,
//! last-writer-wins DISPLAY half — Claude Code rewrites or clears it out-of-band on
//! a forced logout (see [`crate::claude_state`]). So resolution consults the two
//! signals in that order of trust:
//!   1. the canonical token byte-matches an account's stashed credential → that
//!      account (exact right after a swap or a re-stash), then
//!   2. the displayed `accountUuid` maps to a roster account — the only signal when
//!      the token changed in place and no stash matches it yet (a fresh `/login` or
//!      a silent in-place refresh).
//!
//! Both the daemon's poll loop ([`crate::daemon`]) and the manual `use` swap
//! ([`crate::use_account`]) resolve the outgoing account through these SAME two
//! functions, so the recovery verb `use` no longer hard-fails on a cleared display
//! while a healthy token still sits in the keychain (issue #207). The resolvers
//! return a roster INDEX only — never a token, email, or `accountUuid` — so the
//! output-redaction invariant (issue #15) holds by construction.

use std::path::Path;

use crate::claude_state;
use crate::config::Account;
use crate::keychain::Credential;
use crate::stash::AccountStash;

/// Identify which roster account the given `canonical` credential belongs to, using
/// two signals in order: (1) the canonical token byte-matches an account's stash —
/// exact right after a swap or a re-stash; (2) the displayed `~/.claude.json`
/// `accountUuid` maps to a roster account — the signal when the token has changed in
/// place and no stash matches it yet. `None` if neither resolves.
///
/// Token-first because the keychain credential is the authoritative bearer and
/// `~/.claude.json` is the clobberable display half (issue #207): a cleared or stale
/// display still resolves as long as the canonical token matches a stash. Shared by
/// the daemon's active-resolution and re-auth re-stash paths and the manual `use`
/// swap's outgoing-account resolution.
pub(crate) async fn resolve_account_for<S: AccountStash>(
    roster: &[Account],
    stash: &S,
    claude_json: &Path,
    canonical: &Credential,
) -> Option<usize> {
    for (i, account) in roster.iter().enumerate() {
        if let Ok(stashed) = stash.read(&account.stash()).await {
            if stashed.credential.matches(canonical) {
                return Some(i);
            }
        }
    }
    resolve_via_display(roster, claude_json)
}

/// The `~/.claude.json` DISPLAY-only fallback: the roster account whose
/// `account_uuid` matches the displayed `oauthAccount.accountUuid`, or `None` when
/// the file is unreadable / absent or names an account not in the roster.
///
/// The weaker of the two signals ([`resolve_account_for`]'s step 2), factored out
/// because it is ALSO the only signal available when the canonical token cannot be
/// read at all — the daemon's locked-keychain poll degrades to display-only rather
/// than swap blindly (`use`, by contrast, treats a locked keychain as a safety
/// abort, so it never reaches this fallback under a lock).
pub(crate) fn resolve_via_display(roster: &[Account], claude_json: &Path) -> Option<usize> {
    let oauth = claude_state::read_oauth_account_from(claude_json).ok()?;
    roster
        .iter()
        .position(|account| account.account_uuid == oauth.account_uuid())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::claude_state::OauthAccount;
    use crate::stash::{FakeAccountStash, StashedAccount};

    fn acct(label: &str, uuid: &str) -> Account {
        Account {
            account_uuid: uuid.to_owned(),
            label: label.to_owned(),
            enabled: true,
        }
    }

    fn cred(blob: &[u8]) -> Credential {
        Credential::new(blob.to_vec())
    }

    fn stashed(token: &[u8], uuid: &str) -> StashedAccount {
        StashedAccount {
            credential: cred(token),
            oauth_account: OauthAccount::from_object_bytes(
                format!(r#"{{"accountUuid":"{uuid}","emailAddress":"{uuid}@example.com"}}"#)
                    .as_bytes(),
            )
            .unwrap(),
        }
    }

    /// A two-account roster: `work` (`u-A`) and `spare` (`u-B`).
    fn roster_ab() -> Vec<Account> {
        vec![acct("work", "u-A"), acct("spare", "u-B")]
    }

    /// A stash holding both accounts' tokens (`A-token` / `B-token`).
    async fn stash_ab() -> FakeAccountStash {
        let stash = FakeAccountStash::empty();
        stash
            .write("Sessiometer/u-A", &stashed(b"A-token", "u-A"))
            .await
            .unwrap();
        stash
            .write("Sessiometer/u-B", &stashed(b"B-token", "u-B"))
            .await
            .unwrap();
        stash
    }

    /// A `~/.claude.json` displaying `active_uuid`, returned with its tempdir guard.
    fn claude_json_for(active_uuid: &str) -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".claude.json");
        std::fs::write(
            &path,
            format!(
                r#"{{"numStartups":1,"oauthAccount":{{"accountUuid":"{active_uuid}","emailAddress":"{active_uuid}@x.com"}}}}"#
            ),
        )
        .unwrap();
        (dir, path)
    }

    #[tokio::test]
    async fn token_match_resolves_even_when_the_display_is_stale() {
        // The #207 core: ~/.claude.json points at an account NOT in the roster
        // (Claude Code cleared/rewrote the display), yet the canonical token
        // byte-matches u-A's stash → resolve token-first to u-A regardless.
        let roster = roster_ab();
        let stash = stash_ab().await;
        let (_dir, json) = claude_json_for("u-UNKNOWN");
        let resolved = resolve_account_for(&roster, &stash, &json, &cred(b"A-token")).await;
        assert_eq!(
            resolved,
            Some(0),
            "token byte-match wins over the stale display"
        );
    }

    #[tokio::test]
    async fn display_fallback_when_the_token_matches_no_stash() {
        // A fresh /login (or in-place refresh) minted a token no stash holds yet;
        // the display still names a roster account → resolve via the display half.
        let roster = roster_ab();
        let stash = stash_ab().await;
        let (_dir, json) = claude_json_for("u-B");
        let resolved = resolve_account_for(&roster, &stash, &json, &cred(b"ORPHAN-token")).await;
        assert_eq!(
            resolved,
            Some(1),
            "no stash matches → display maps u-B to index 1"
        );
    }

    #[tokio::test]
    async fn unresolved_when_token_matches_no_stash_and_display_is_unresolvable() {
        // Fail-closed: neither signal resolves — an orphan token AND a display
        // naming an account not in the roster → None (the caller fails cleanly).
        let roster = roster_ab();
        let stash = stash_ab().await;
        let (_dir, json) = claude_json_for("u-UNKNOWN");
        let resolved = resolve_account_for(&roster, &stash, &json, &cred(b"ORPHAN-token")).await;
        assert_eq!(
            resolved, None,
            "no token match and no display match → unresolved"
        );
    }

    #[test]
    fn resolve_via_display_maps_a_displayed_uuid_to_its_roster_index() {
        let roster = roster_ab();
        let (_dir, json) = claude_json_for("u-B");
        assert_eq!(resolve_via_display(&roster, &json), Some(1));
    }

    #[test]
    fn resolve_via_display_is_none_when_the_file_is_absent() {
        let roster = roster_ab();
        let missing = std::path::Path::new("/nonexistent/.claude.json");
        assert_eq!(resolve_via_display(&roster, missing), None);
    }
}
