// Copyright (c) 2026 Oleksii PELYKH
// SPDX-License-Identifier: MIT

//! The `capture` command: stash the active account and add it to the roster.
//!
//! While an account is the one currently logged in to Claude Code, `capture`:
//!   1. reads that account's `~/.claude.json` `oauthAccount` block
//!      ([`crate::claude_state`]),
//!   2. reads the active `Claude Code-credentials` token ([`crate::keychain`]),
//!   3. stashes both under a per-account `Sessiometer/<account_uuid>` keychain
//!      service ([`crate::stash`]), and
//!   4. writes/refreshes the account's roster entry in `config.toml`
//!      ([`crate::config`]).
//!
//! Accounts are identified by `oauthAccount.accountUuid`: a second `capture` of
//! an already-rostered account is an idempotent *refresh* (same stash, token
//! and identity re-stashed), reported distinctly from a first *capture*. The
//! operator repeats capture-then-`claude /login` once per account (the only
//! interactive step). All output names the account by its **label** only — never
//! the email or token (issue #15 redaction).
//!
//! Capture reads the identity block and the token in two steps, so it assumes the
//! active account does not change underneath it — i.e. the operator does not run
//! `claude /login` *during* a capture. A concurrent re-login could pair one
//! account's token with another's identity; per `build/version-compat.md` that
//! mismatch only mis-displays (auth follows the token), but it would mis-key the
//! roster entry. The capture-then-`/login` loop is sequential, so this does not
//! arise in normal use; #6 should be aware of it when reasoning about staleness.
//!
//! The decision logic ([`plan_capture`]) is a pure function over the roster, and
//! the orchestration ([`run_capture`]) is generic over the stash seam, so both
//! are unit-tested hermetically; [`capture`] only wires the real seams, persists,
//! and prints.

use crate::claude_state::{read_oauth_account, OauthAccount};
use crate::config::{Account, Config, Tunables, MAX_ACCOUNTS};
use crate::error::{Error, Result};
use crate::keychain::{Credential, CredentialStore, RealCredentialStore};
use crate::stash::{AccountStash, RealAccountStash, StashedAccount};

/// The keychain-service namespace prefix; the account's immutable `account_uuid`
/// is appended to form the per-account stash service `Sessiometer/<account_uuid>`.
const STASH_PREFIX: &str = "Sessiometer/";

/// Whether a `capture` added a new account or refreshed an existing one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CaptureOutcome {
    Captured,
    Refreshed,
}

/// The result of planning + stashing a capture: the config to persist plus the
/// facts the confirmation line needs.
struct CaptureReport {
    config: Config,
    outcome: CaptureOutcome,
    label: String,
    count: usize,
}

/// Run the `capture` command: read the active credential + identity, stash them,
/// update the roster, and print the confirmation.
pub(crate) async fn capture(label: Option<String>) -> Result<()> {
    // Identity first (a cheap file read) so "not logged in" fails before we touch
    // the keychain; then the active token.
    let oauth = read_oauth_account()?;
    let credential = RealCredentialStore::new().read().await?;

    let existing = load_existing()?;
    let report = run_capture(
        credential,
        oauth,
        &RealAccountStash::new(),
        existing,
        label.as_deref(),
    )
    .await?;

    report.config.save()?;
    println!(
        "{}",
        confirmation(report.outcome, &report.label, report.count)
    );
    Ok(())
}

/// Load the existing config, treating an absent file as an empty roster (the
/// first capture creates `config.toml`). A file that exists but is malformed is a
/// hard error — never silently replaced.
fn load_existing() -> Result<Option<Config>> {
    match Config::load() {
        Ok(config) => Ok(Some(config)),
        Err(Error::ConfigNotFound { .. }) => Ok(None),
        Err(err) => Err(err),
    }
}

/// Stash the account and produce the updated config. Generic over the stash seam
/// so it is testable with an in-memory fake; the credential and identity are
/// passed in (already read) so this function performs no keychain/file reads
/// itself.
async fn run_capture(
    credential: Credential,
    oauth: OauthAccount,
    stash: &impl AccountStash,
    existing: Option<Config>,
    label: Option<&str>,
) -> Result<CaptureReport> {
    let (mut roster, tunables) = match existing {
        Some(config) => (config.roster, config.tunables),
        None => (Vec::new(), Tunables::default()),
    };

    let (stash_name, outcome) = plan_capture(&mut roster, oauth.account_uuid(), label)?;

    let stashed = StashedAccount {
        credential,
        oauth_account: oauth,
    };
    // Stash BEFORE persisting the roster: if this fails, config.toml is never
    // written to reference an unstashed (or half-stashed) stash.
    stash.write(&stash_name, &stashed).await?;

    let count = roster.len();
    // The final label lives on the rostered account (a refresh may have updated it).
    let label = roster
        .iter()
        .find(|a| a.stash == stash_name)
        .expect("the account just planned is in the roster")
        .label
        .clone();

    Ok(CaptureReport {
        config: Config { roster, tunables },
        outcome,
        label,
        count,
    })
}

/// Pure roster update. Returns the stash service to write and whether this was a new
/// capture or a refresh. Mutates `roster` in place (appending a new account, or
/// updating an existing one's label).
///
/// A new account requires an explicit, operator-chosen `label`: the account must
/// be identifiable by something the operator controls, never a server-provided
/// field. `displayName` is unsuitable (two distinct accounts can share one —
/// `build/version-compat.md`) and `emailAddress` is redacted (#15), so there is
/// no field to default to — hence [`Error::LabelRequired`] rather than a fallback.
fn plan_capture(
    roster: &mut Vec<Account>,
    account_uuid: &str,
    label: Option<&str>,
) -> Result<(String, CaptureOutcome)> {
    let provided = label.map(str::trim).filter(|l| !l.is_empty());

    if let Some(existing) = roster.iter_mut().find(|a| a.account_uuid == account_uuid) {
        // Idempotent refresh: same stash; update the label only if a new, non-empty
        // one was given (otherwise keep what the operator named it before).
        if let Some(l) = provided {
            existing.label = l.to_owned();
        }
        return Ok((existing.stash.clone(), CaptureOutcome::Refreshed));
    }

    // New account: the rotation must have room, and an explicit label is required.
    if roster.len() >= MAX_ACCOUNTS {
        return Err(Error::RotationFull { max: MAX_ACCOUNTS });
    }
    let label = provided.ok_or(Error::LabelRequired)?.to_owned();
    // Key the stash by the immutable, server-assigned account_uuid — not a
    // positional slot. The keychain service accepts the uuid (hex + hyphens)
    // verbatim, and the stash uses fixed `acct=credential`/`acct=oauthAccount`,
    // so no resolve/uniqueness step is needed (unlike the canonical item).
    let stash = format!("{STASH_PREFIX}{account_uuid}");
    roster.push(Account {
        account_uuid: account_uuid.to_owned(),
        stash: stash.clone(),
        label,
    });
    Ok((stash, CaptureOutcome::Captured))
}

/// The confirmation line — label only, never the email or token (issue #15).
fn confirmation(outcome: CaptureOutcome, label: &str, count: usize) -> String {
    match outcome {
        CaptureOutcome::Captured => {
            format!("Captured \"{label}\" (account {count} of {MAX_ACCOUNTS} in rotation).")
        }
        CaptureOutcome::Refreshed => {
            format!("Refreshed \"{label}\" (still {count} in rotation).")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::stash::FakeAccountStash;

    fn account(uuid: &str, stash: &str, label: &str) -> Account {
        Account {
            account_uuid: uuid.to_owned(),
            stash: stash.to_owned(),
            label: label.to_owned(),
        }
    }

    fn oauth(uuid: &str) -> OauthAccount {
        let json = format!(r#"{{"accountUuid":"{uuid}","displayName":"ignored"}}"#);
        OauthAccount::from_object_bytes(json.as_bytes()).unwrap()
    }

    // --- plan_capture (pure) ---

    #[test]
    fn plans_a_new_account_into_an_empty_roster() {
        let mut roster = Vec::new();
        let (stash, outcome) = plan_capture(&mut roster, "u-1", Some("work")).unwrap();
        assert_eq!(stash, "Sessiometer/u-1");
        assert_eq!(outcome, CaptureOutcome::Captured);
        assert_eq!(roster.len(), 1);
        assert_eq!(roster[0], account("u-1", "Sessiometer/u-1", "work"));
    }

    #[test]
    fn mints_the_stash_service_from_the_account_uuid() {
        // AC: a new capture mints the stash as `Sessiometer/<account_uuid>`,
        // keyed by the immutable account_uuid (hyphens accepted verbatim) — no
        // positional `acct-N` slot.
        let mut roster = Vec::new();
        let (stash, _) = plan_capture(
            &mut roster,
            "11111111-1111-1111-1111-111111111111",
            Some("work"),
        )
        .unwrap();
        assert_eq!(stash, "Sessiometer/11111111-1111-1111-1111-111111111111");
        assert_eq!(
            roster[0].stash,
            "Sessiometer/11111111-1111-1111-1111-111111111111"
        );
    }

    #[test]
    fn a_new_account_without_a_label_is_rejected() {
        // No server-provided fallback: an explicit operator label is required.
        let mut roster = Vec::new();
        assert!(matches!(
            plan_capture(&mut roster, "u-1", None),
            Err(Error::LabelRequired)
        ));
        // Nothing was appended on the error path.
        assert!(roster.is_empty());
    }

    #[test]
    fn a_blank_label_on_a_new_account_is_rejected() {
        let mut roster = Vec::new();
        assert!(matches!(
            plan_capture(&mut roster, "u-1", Some("   ")),
            Err(Error::LabelRequired)
        ));
        assert!(roster.is_empty());
    }

    #[test]
    fn label_argument_is_trimmed() {
        let mut roster = Vec::new();
        plan_capture(&mut roster, "u-1", Some("  work  ")).unwrap();
        assert_eq!(roster[0].label, "work");
    }

    #[test]
    fn recapture_is_a_refresh_on_the_same_stash() {
        let mut roster = vec![account("u-1", "Sessiometer/u-1", "work")];
        let (stash, outcome) = plan_capture(&mut roster, "u-1", None).unwrap();
        assert_eq!(stash, "Sessiometer/u-1");
        assert_eq!(outcome, CaptureOutcome::Refreshed);
        // Size unchanged; label kept (no new label given). A refresh does NOT
        // require a label — only a new capture does.
        assert_eq!(roster.len(), 1);
        assert_eq!(roster[0].label, "work");
    }

    #[test]
    fn recapture_updates_the_label_when_a_new_one_is_given() {
        let mut roster = vec![account("u-1", "Sessiometer/u-1", "work")];
        plan_capture(&mut roster, "u-1", Some("personal")).unwrap();
        assert_eq!(roster[0].label, "personal");
        assert_eq!(roster.len(), 1);
    }

    #[test]
    fn a_new_account_is_keyed_by_its_account_uuid() {
        // A second distinct account is keyed by its OWN account_uuid — there is no
        // positional slot allocation; the stash is `Sessiometer/<account_uuid>`.
        let mut roster = vec![account("u-1", "Sessiometer/u-1", "work")];
        let (stash, outcome) = plan_capture(&mut roster, "u-2", Some("personal")).unwrap();
        assert_eq!(stash, "Sessiometer/u-2");
        assert_eq!(outcome, CaptureOutcome::Captured);
        assert_eq!(roster.len(), 2);
    }

    #[test]
    fn a_full_rotation_rejects_a_new_account() {
        let mut roster: Vec<Account> = (1..=MAX_ACCOUNTS)
            .map(|i| account(&format!("u-{i}"), &format!("Sessiometer/u-{i}"), "l"))
            .collect();
        assert!(matches!(
            plan_capture(&mut roster, "u-new", Some("x")),
            Err(Error::RotationFull { max: 5 })
        ));
        // …but a full rotation still allows refreshing a member.
        assert!(plan_capture(&mut roster, "u-1", None).is_ok());
    }

    // --- confirmation (exact AC strings) ---

    #[test]
    fn confirmation_lines_match_the_acceptance_criteria() {
        assert_eq!(
            confirmation(CaptureOutcome::Captured, "work", 3),
            "Captured \"work\" (account 3 of 5 in rotation)."
        );
        assert_eq!(
            confirmation(CaptureOutcome::Refreshed, "personal", 2),
            "Refreshed \"personal\" (still 2 in rotation)."
        );
    }

    // --- run_capture (orchestration over the fake stash) ---

    #[tokio::test]
    async fn first_capture_creates_a_one_account_roster_and_stashes_both_halves() {
        let stash = FakeAccountStash::empty();
        let report = run_capture(
            Credential::new(b"token-1".to_vec()),
            oauth("u-1"),
            &stash,
            None,
            Some("work"),
        )
        .await
        .unwrap();

        assert_eq!(report.outcome, CaptureOutcome::Captured);
        assert_eq!(report.count, 1);
        assert_eq!(report.label, "work");
        assert_eq!(report.config.roster.len(), 1);
        assert_eq!(report.config.roster[0].stash, "Sessiometer/u-1");
        assert_eq!(report.config.roster[0].account_uuid, "u-1");

        // Both halves are in the stash under its service name.
        assert!(stash.contains("Sessiometer/u-1"));
        let stashed = stash.read("Sessiometer/u-1").await.unwrap();
        assert_eq!(stashed.credential.expose(), b"token-1");
        assert_eq!(stashed.oauth_account.account_uuid(), "u-1");
    }

    #[tokio::test]
    async fn recapture_refreshes_the_stash_without_growing_the_roster() {
        let stash = FakeAccountStash::empty();
        let existing = Config {
            roster: vec![account("u-1", "Sessiometer/u-1", "work")],
            tunables: Tunables::default(),
        };

        let report = run_capture(
            Credential::new(b"rotated".to_vec()),
            oauth("u-1"),
            &stash,
            Some(existing),
            None,
        )
        .await
        .unwrap();

        assert_eq!(report.outcome, CaptureOutcome::Refreshed);
        assert_eq!(report.count, 1);
        assert_eq!(report.label, "work");
        assert_eq!(report.config.roster.len(), 1);
        // The stash was refreshed with the new token.
        assert_eq!(stash.len(), 1);
        let stashed = stash.read("Sessiometer/u-1").await.unwrap();
        assert_eq!(stashed.credential.expose(), b"rotated");
    }

    #[tokio::test]
    async fn a_second_distinct_account_is_appended() {
        let stash = FakeAccountStash::empty();
        let existing = Config {
            roster: vec![account("u-1", "Sessiometer/u-1", "work")],
            tunables: Tunables::default(),
        };

        let report = run_capture(
            Credential::new(b"token-2".to_vec()),
            oauth("u-2"),
            &stash,
            Some(existing),
            Some("personal"),
        )
        .await
        .unwrap();

        assert_eq!(report.outcome, CaptureOutcome::Captured);
        assert_eq!(report.count, 2);
        assert_eq!(report.config.roster.len(), 2);
        assert_eq!(report.config.roster[1].stash, "Sessiometer/u-2");
        assert_eq!(stash.len(), 1); // only the new stash was written this call
        assert!(stash.contains("Sessiometer/u-2"));
    }
}
