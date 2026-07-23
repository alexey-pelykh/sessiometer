// Copyright (c) 2026 Oleksii PELYKH
// SPDX-License-Identifier: MIT

//! Behavioral canary (issue #714) — asserts the reverse-engineered #100 keychain
//! derivation still points at the credential Claude Code is actually using,
//! converting "CC silently changed where/how it stores its credential" from an
//! operational-failure-later into a loud, immediate signal at boot / pre-swap.
//!
//! ## The two layers (and the residual third)
//!
//! **Layer 1 — service resolution (uniqueness).** A FRESH enumeration pass
//! ([`CredentialStore::probe_resolution`]): zero items under the derived service →
//! [`CanaryOutcome::NotFound`] (a service-name derivation change, or a scrubbed
//! keychain), more than one → [`CanaryOutcome::Ambiguous`]. Zero is already
//! fail-closed at swap time by construction (the engine's up-front `store.read()`
//! aborts pre-mutation) — the canary only surfaces it earlier. Late AMBIGUITY is
//! the genuinely new protection: the resolve-once `acct` cache
//! (`src/keychain.rs`) pins the boot-time item, so a second item appearing later
//! (CC re-keying its storage under the same service) would never re-trip the
//! uniqueness rule through cached reads — only this fresh probe sees it.
//!
//! **Layer 2 — the offline stash-token identity cross-check (decided oracle,
//! option C).** Compare the RESOLVED canonical credential to the per-account
//! stashes with the exact-byte [`Credential::matches`] primitive — the same
//! token-first oracle the daemon already runs every tick via
//! [`crate::active::resolve_account_for`] — keyed against `A`, the account Claude
//! Code's own state says is active (`~/.claude.json` `oauthAccount.accountUuid`,
//! the key `stash[A]` is addressed by):
//!   - canonical byte-matches `stash[A]` → [`CanaryOutcome::Ok`] (positive pass);
//!   - canonical byte-matches a DIFFERENT account's `stash[X≠A]` →
//!     [`CanaryOutcome::Drift`] — the caller REFUSES the credential write
//!     (pre-mutation, zero writes);
//!   - canonical matches NO stash → [`CanaryOutcome::Inconclusive`] — fail OPEN
//!     (overwhelmingly CC's own `A`-token refreshed in place since we last
//!     stashed it; never block on "couldn't verify").
//!
//! The `stash[A]`-FIRST order is load-bearing (the #211 short-circuit's shape): a
//! canonical matching `stash[A]` is never refused, even if the same bytes also sit
//! under another account's stash (a shared/duplicated roster token — the issue's
//! empirical falsifier scenario degrades to a safe pass here, not a false refuse).
//! An unresolvable `A` (no `~/.claude.json`, or a displayed account not in the
//! roster) is likewise INCONCLUSIVE: only a POSITIVE `A ≠ B` divergence refuses,
//! and #207's token-first recovery (a cleared display with a healthy canonical)
//! must keep working.
//!
//! **Layer 3 — residual gap (documented, not closed here).** *Same account, CC
//! silently relocated the item, old copy stale-but-valid*: `A == B`, reads stay
//! green, and this offline canary cannot see it — the managed item and CC's real
//! item have gone parallel. The same residual covers the reconcile-masked variant:
//! [`reconcile_display`] (deliberately run BEFORE the cross-check, see below)
//! resolves a display/keychain disagreement in favor of the keychain — on EVERY
//! run, so on a writable `~/.claude.json` even a CC re-assertion of a different
//! active account is healed away before the cross-check reads it. The only
//! Layer-2 DRIFT that actually refuses is a display that CANNOT be brought to
//! agree (an unwritable `~/.claude.json`, or a write racing the check) — the
//! decided fail-closed posture on a positive mismatch the heal could not clear;
//! on a writable display the protection is Layer 1 plus the honest INCONCLUSIVE
//! surface, not this refuse. Closing Layer 3
//! needs an online liveness signal (`/oauth/usage` currency of the resolved
//! token), deliberately out of scope for the offline canary — the INCONCLUSIVE
//! (`Layer-1-only`) verdict on the status wire is the honest surface of this
//! limit. Non-swap canonical writes (the #467 scrubbed-canonical adopt, `use
//! --force` adopt-target, the #282 keep-warm promotion, `capture`) are likewise
//! outside the canary's refuse slot: adopt targets a CONFIRMED-absent/vetted
//! item (nothing coherent to protect), and promotion/capture write the resolved
//! item for the account the daemon just verified against it.
//!
//! ## Reconcile BEFORE the cross-check (false-positive guard)
//!
//! `A`'s source (`~/.claude.json`) is self-co-written by the swap engine
//! (best-effort, `src/swap.rs` step 4), so a swap whose co-write failed leaves the
//! display naming the OUTGOING account while the canonical correctly holds the
//! incoming token — structurally indistinguishable from drift. [`run`] therefore
//! heals the display against the canonical FIRST ([`reconcile_display`], the same
//! core as the boot reconcile, `src/daemon/canonical.rs`) and only then evaluates
//! Layer 2, so a lagging self-co-write can never false-positive a refuse. This
//! ordering is a decided invariant (issue #714's FP-profile), not an optimization.
//!
//! ## Fail-policy (decided via /council, issue #714)
//!
//! Layer-keyed — refuse the WRITE, keep READS live. The canary itself only
//! CLASSIFIES; the refuse lives at the callers (`crate::daemon`'s pre-swap gate
//! and the standalone `use` path), which map [`CanaryOutcome::Drift`] to a refused
//! swap (zero mutations) unless the documented operator override
//! (`canary_drift_override`, `config.toml` tunable) is set — the recovery lever
//! for a false DRIFT on an unattended daemon. Layer-1 failures have no override:
//! zero/ambiguous items give an atomic `-U` upsert no unique, safe target, and a
//! wrongly-addressed write clobbers an unrelated secret unrecoverably
//! (`src/keychain.rs`). INCONCLUSIVE always proceeds (Layer-1-only).
//!
//! Every surface derived from these types is secret-free by construction (issue
//! #15): outcomes carry roster INDICES (resolved to operator labels at the event /
//! status boundary), never a token, email, or account-uuid.

use std::path::Path;

use crate::active;
use crate::claude_state;
use crate::config::Account;
use crate::error::{Error, Result};
use crate::keychain::{Credential, CredentialStore};
use crate::stash::AccountStash;

/// The typed canary verdict (issue #714), spanning Layer 1 (service-resolution
/// uniqueness) and Layer 2 (offline stash-token identity cross-check). Carries
/// roster INDICES — the caller resolves labels for events / status, so no PII
/// can originate here (issue #15).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CanaryOutcome {
    /// Positive Layer-2 pass: the resolved canonical token byte-matches the
    /// displayed active account's OWN stash (`stash[A]`).
    Ok,
    /// Layer 1: zero items under the derived service — a service-name derivation
    /// change, or a scrubbed/empty keychain. Already fail-closed at swap time by
    /// the engine's up-front read; surfaced proactively at boot / `status`.
    NotFound,
    /// Layer 1: more than one item under the derived service — the uniqueness
    /// rule fails, so the derivation no longer addresses a single credential.
    /// Fail-closed at the callers (no override): an atomic in-place write has no
    /// unique, safe target.
    Ambiguous {
        /// How many service-matching items the fresh enumeration found.
        count: usize,
    },
    /// Layer 2 DRIFT: the resolved canonical token byte-matches a DIFFERENT
    /// account's stash than the one Claude Code's own state names active — the
    /// positive `A ≠ B` divergence. The callers refuse the credential write
    /// (pre-mutation, zero writes) unless the operator override is set.
    Drift {
        /// Roster index of `A` — the account `~/.claude.json` names active.
        displayed: usize,
        /// Roster index of `X` — the account whose stashed token the resolved
        /// canonical actually matches.
        matched: usize,
    },
    /// No positive identity evidence either way — fail OPEN (Layer-1-only).
    Inconclusive(InconclusiveReason),
}

/// Why a canary run was [`CanaryOutcome::Inconclusive`] — a closed, secret-free
/// classification (issue #15) so callers and tests can distinguish WHICH evidence
/// was missing (the wire carries only the collapsed `inconclusive` verdict; both
/// reasons fail OPEN identically).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum InconclusiveReason {
    /// `~/.claude.json` was unreadable/absent, or its displayed `accountUuid`
    /// maps to no roster account — there is no `A` to cross-check against. The
    /// #207 recovery posture (a cleared display with a healthy canonical) lands
    /// here and must keep working, so this can never refuse.
    DisplayUnresolved,
    /// The resolved canonical token matches NO account's stash — overwhelmingly
    /// the active account's own token refreshed in place since it was last
    /// stashed (benign); never block on "couldn't verify".
    NoStashMatch,
}

/// Reconcile `~/.claude.json` to the canonical credential — the shared core of
/// the boot reconcile ([`crate::daemon`]'s `reconcile_on_start`) and [`run`]'s
/// pre-cross-check heal (issue #714).
///
/// Finds the roster account whose stash byte-matches `canonical` and, if the
/// displayed `oauthAccount` disagrees, co-writes that account's identity. Heals
/// the post-swap crash / failed-co-write window (the display shows the outgoing
/// account while the keychain already holds the incoming token) so Layer 2 never
/// keys `A` off our OWN stale co-write. When the canonical matches no stash (an
/// in-place token refresh) the display is left untouched — nothing to heal.
/// Best-effort and idempotent; the keychain is authoritative, the display is the
/// clobberable half (issue #207).
pub(crate) async fn reconcile_display<S: AccountStash>(
    roster: &[Account],
    stash: &S,
    claude_json: &Path,
    canonical: &Credential,
) -> Result<()> {
    for account in roster {
        let Ok(stashed) = stash.read(&account.stash()).await else {
            continue;
        };
        if !stashed.credential.matches(canonical) {
            continue;
        }
        // The canonical belongs to this account; ensure the display agrees.
        let displayed = claude_state::read_oauth_account_from(claude_json)
            .ok()
            .map(|o| o.account_uuid().to_owned());
        if displayed.as_deref() != Some(stashed.oauth_account.account_uuid()) {
            claude_state::write_oauth_account(claude_json, &stashed.oauth_account)?;
        }
        return Ok(());
    }
    // No stash matched the canonical token — leave ~/.claude.json untouched.
    Ok(())
}

/// Run one canary pass (issue #714): FRESH Layer-1 resolution probe → canonical
/// read → display reconcile ([`reconcile_display`], the decided false-positive
/// guard) → Layer-2 stash-token cross-check. Read-only but for the reconcile's
/// best-effort display heal; NEVER writes a credential.
///
/// Layer-1 failures return as outcomes (`NotFound` / `Ambiguous`), not errors —
/// they are canary VERDICTS. An `Err` means the canary could not run at all (a
/// LOCKED keychain, a transient `security` failure): the caller keeps its last
/// verdict (no evidence is not a verdict — the same hold discipline as the #464
/// canonical-liveness edge) and, on the pre-swap path, aborts the swap exactly as
/// the engine's own up-front read would.
pub(crate) async fn run<C, S>(
    store: &C,
    stash: &S,
    roster: &[Account],
    claude_json: &Path,
) -> Result<CanaryOutcome>
where
    C: CredentialStore,
    S: AccountStash,
{
    // Layer 1 — the FRESH enumeration probe (never the boot-pinned cache; the
    // OnceLock `acct` cache can go stale, so pre-swap re-resolves every time).
    if let Err(err) = store.probe_resolution().await {
        return match err {
            Error::CredentialNotFound => Ok(CanaryOutcome::NotFound),
            Error::CredentialAmbiguous { count } => Ok(CanaryOutcome::Ambiguous { count }),
            other => Err(other),
        };
    }

    // The resolved item's credential, for the Layer-2 identity cross-check. A
    // probe/read divergence (the probe found a fresh unique item while the pinned
    // addressing reads a now-gone one) honestly classifies NotFound — the loud
    // Layer-1 signal; a daemon restart re-resolves.
    let canonical = match store.read().await {
        Ok(canonical) => canonical,
        Err(Error::CredentialNotFound) => return Ok(CanaryOutcome::NotFound),
        Err(other) => return Err(other),
    };

    // Reconcile BEFORE the cross-check (decided invariant): a lagging self
    // co-write must not false-positive as drift. Best-effort — a failed heal
    // leaves the stale display to be judged as-is (fail-closed on the positive
    // mismatch it then presents, which is exactly the decided posture when the
    // display CANNOT be brought to agree).
    let _ = reconcile_display(roster, stash, claude_json, &canonical).await;

    // Layer 2 — the offline stash-token cross-check (decided oracle, option C).
    let Some(displayed) = active::resolve_via_display(roster, claude_json) else {
        return Ok(CanaryOutcome::Inconclusive(
            InconclusiveReason::DisplayUnresolved,
        ));
    };
    // stash[A] FIRST (the #211 short-circuit's shape): a canonical matching the
    // displayed account's own stash is never refused — even if the same bytes
    // also sit under another stash (a shared/duplicated roster token).
    if let Ok(stashed) = stash.read(&roster[displayed].stash()).await {
        if stashed.credential.matches(&canonical) {
            return Ok(CanaryOutcome::Ok);
        }
    }
    for (matched, account) in roster.iter().enumerate() {
        if matched == displayed {
            continue;
        }
        let Ok(stashed) = stash.read(&account.stash()).await else {
            continue;
        };
        if stashed.credential.matches(&canonical) {
            return Ok(CanaryOutcome::Drift { displayed, matched });
        }
    }
    Ok(CanaryOutcome::Inconclusive(
        InconclusiveReason::NoStashMatch,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::claude_state::OauthAccount;
    use crate::keychain::FakeCredentialStore;
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

    /// A canonical store holding `token`.
    async fn store_holding(token: &[u8]) -> FakeCredentialStore {
        let store = FakeCredentialStore::empty();
        store.write(&cred(token)).await.unwrap();
        store
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

    /// The displayed `accountUuid` of `path`'s `oauthAccount`, if readable.
    fn displayed_uuid(path: &Path) -> Option<String> {
        claude_state::read_oauth_account_from(path)
            .ok()
            .map(|o| o.account_uuid().to_owned())
    }

    #[tokio::test]
    async fn ok_when_the_canonical_matches_the_displayed_accounts_own_stash() {
        // The healthy steady state: display names A, canonical is A's stashed
        // token byte-for-byte → the positive Layer-2 pass.
        let roster = roster_ab();
        let stash = stash_ab().await;
        let store = store_holding(b"A-token").await;
        let (_dir, json) = claude_json_for("u-A");
        let outcome = run(&store, &stash, &roster, &json).await.unwrap();
        assert_eq!(outcome, CanaryOutcome::Ok);
    }

    #[tokio::test]
    async fn drift_when_the_canonical_matches_a_different_accounts_stash() {
        // Identity mismatch (issue #714 AC): CC's own state says A is active,
        // but the RESOLVED item holds B's stashed token byte-for-byte — the
        // positive `A ≠ B` divergence. NOTE the display heal cannot mask this
        // fixture: reconcile would heal display→B, so the persistent-divergence
        // case is modeled with a read-only json (heal fails, display stands).
        let roster = roster_ab();
        let stash = stash_ab().await;
        let store = store_holding(b"B-token").await;
        let (dir, json) = claude_json_for("u-A");
        // Freeze the display: CC keeps asserting A (the heal cannot land). A
        // read-only file makes `write_oauth_account`'s atomic replace fail on the
        // read-only parent below; use a read-only DIRECTORY so the tempfile
        // rename cannot land either.
        let mut perms = std::fs::metadata(dir.path()).unwrap().permissions();
        std::os::unix::fs::PermissionsExt::set_mode(&mut perms, 0o500);
        std::fs::set_permissions(dir.path(), perms.clone()).unwrap();
        let outcome = run(&store, &stash, &roster, &json).await.unwrap();
        // Restore so the tempdir can clean up.
        std::os::unix::fs::PermissionsExt::set_mode(&mut perms, 0o700);
        std::fs::set_permissions(dir.path(), perms).unwrap();
        assert_eq!(
            outcome,
            CanaryOutcome::Drift {
                displayed: 0,
                matched: 1
            }
        );
    }

    #[tokio::test]
    async fn reconcile_heals_a_lagging_self_co_write_instead_of_false_positive() {
        // The decided FP guard (issue #714): a prior swap wrote B's token to the
        // canonical but its display co-write never landed (crash / EPERM), so the
        // display still says A. WITHOUT the reconcile-first ordering this reads
        // as `A ≠ B` drift; WITH it the display heals to B and the canary passes.
        let roster = roster_ab();
        let stash = stash_ab().await;
        let store = store_holding(b"B-token").await;
        let (_dir, json) = claude_json_for("u-A"); // stale display (lagging co-write)
        let outcome = run(&store, &stash, &roster, &json).await.unwrap();
        assert_eq!(outcome, CanaryOutcome::Ok, "healed, not drift");
        assert_eq!(
            displayed_uuid(&json).as_deref(),
            Some("u-B"),
            "the reconcile co-wrote the canonical's owner into the display"
        );
    }

    #[tokio::test]
    async fn inconclusive_when_the_canonical_matches_no_stash() {
        // The overwhelmingly-common benign state: the active account's token
        // refreshed in place since it was last stashed → no stash matches →
        // fail OPEN (Layer-1-only), never a refuse.
        let roster = roster_ab();
        let stash = stash_ab().await;
        let store = store_holding(b"A-refreshed-token").await;
        let (_dir, json) = claude_json_for("u-A");
        let outcome = run(&store, &stash, &roster, &json).await.unwrap();
        assert_eq!(
            outcome,
            CanaryOutcome::Inconclusive(InconclusiveReason::NoStashMatch)
        );
    }

    #[tokio::test]
    async fn inconclusive_when_the_display_is_unresolvable() {
        // No `A` to cross-check against (a cleared / unreadable display — the
        // #207 recovery posture): only a POSITIVE `A ≠ B` refuses, so this is
        // INCONCLUSIVE, not drift — even though the canonical matches a stash.
        // (The reconcile heals the display to the canonical's owner when it CAN
        // write; to model the display staying unresolvable, point at a missing
        // path.)
        let roster = roster_ab();
        let stash = stash_ab().await;
        let store = store_holding(b"B-token").await;
        let missing = std::path::Path::new("/nonexistent/.claude.json");
        let outcome = run(&store, &stash, &roster, missing).await.unwrap();
        assert_eq!(
            outcome,
            CanaryOutcome::Inconclusive(InconclusiveReason::DisplayUnresolved)
        );
    }

    #[tokio::test]
    async fn shared_token_under_both_stashes_passes_via_the_stash_a_first_order() {
        // The empirical-falsifier scenario (issue #714): the SAME token sits
        // under BOTH accounts' stashes. The stash[A]-first order must classify
        // OK (A's own stash matched), never drift off the other stash.
        let roster = roster_ab();
        let stash = FakeAccountStash::empty();
        stash
            .write("Sessiometer/u-A", &stashed(b"SHARED-token", "u-A"))
            .await
            .unwrap();
        stash
            .write("Sessiometer/u-B", &stashed(b"SHARED-token", "u-B"))
            .await
            .unwrap();
        let store = store_holding(b"SHARED-token").await;
        let (_dir, json) = claude_json_for("u-A");
        let outcome = run(&store, &stash, &roster, &json).await.unwrap();
        assert_eq!(outcome, CanaryOutcome::Ok);
    }

    #[tokio::test]
    async fn drift_fires_even_when_the_displayed_accounts_stash_is_absent() {
        // A's stash is absent (captured elsewhere / corrupt) — no positive
        // evidence FOR A, but the canonical DOES byte-match B's stash: the
        // positive `A ≠ B` divergence stands → drift.
        let roster = roster_ab();
        let stash = FakeAccountStash::empty();
        stash
            .write("Sessiometer/u-B", &stashed(b"B-token", "u-B"))
            .await
            .unwrap();
        let store = store_holding(b"B-token").await;
        let (dir, json) = claude_json_for("u-A");
        // Freeze the display as in the drift fixture above (the heal would
        // otherwise co-write B and the verdict would legitimately become OK).
        let mut perms = std::fs::metadata(dir.path()).unwrap().permissions();
        std::os::unix::fs::PermissionsExt::set_mode(&mut perms, 0o500);
        std::fs::set_permissions(dir.path(), perms.clone()).unwrap();
        let outcome = run(&store, &stash, &roster, &json).await.unwrap();
        std::os::unix::fs::PermissionsExt::set_mode(&mut perms, 0o700);
        std::fs::set_permissions(dir.path(), perms).unwrap();
        assert_eq!(
            outcome,
            CanaryOutcome::Drift {
                displayed: 0,
                matched: 1
            }
        );
    }

    #[tokio::test]
    async fn layer1_not_found_when_the_service_resolves_to_zero_items() {
        // Service renamed / scrubbed keychain (issue #714 AC): the fresh probe
        // finds nothing under the derived service.
        let roster = roster_ab();
        let stash = stash_ab().await;
        let store = FakeCredentialStore::empty();
        store.set_not_found(true);
        let (_dir, json) = claude_json_for("u-A");
        let outcome = run(&store, &stash, &roster, &json).await.unwrap();
        assert_eq!(outcome, CanaryOutcome::NotFound);
    }

    #[tokio::test]
    async fn layer1_ambiguous_when_a_second_item_appears_after_boot() {
        // Late ambiguity (issue #714 AC): the boot-pinned cache would keep
        // reading the old item, but the FRESH probe sees two service-matching
        // items — the uniqueness rule fails and the canary says so, even though
        // `read` still succeeds.
        let roster = roster_ab();
        let stash = stash_ab().await;
        let store = store_holding(b"A-token").await;
        store.set_ambiguous(Some(2));
        let (_dir, json) = claude_json_for("u-A");
        let outcome = run(&store, &stash, &roster, &json).await.unwrap();
        assert_eq!(outcome, CanaryOutcome::Ambiguous { count: 2 });
    }

    #[tokio::test]
    async fn a_locked_keychain_is_an_error_not_a_verdict() {
        // A canary that cannot READ has no evidence: `Err`, never a verdict —
        // the caller holds its last state (and a pre-swap caller aborts exactly
        // as the engine's own up-front read would).
        let roster = roster_ab();
        let stash = stash_ab().await;
        let store = store_holding(b"A-token").await;
        store.set_locked(true);
        let (_dir, json) = claude_json_for("u-A");
        let result = run(&store, &stash, &roster, &json).await;
        assert!(matches!(result, Err(Error::KeychainLocked { .. })));
    }

    #[tokio::test]
    async fn reconcile_display_is_a_noop_when_no_stash_matches() {
        // The extracted core keeps `reconcile_on_start`'s contract: an in-place
        // refreshed token (no stash match) leaves the display untouched.
        let roster = roster_ab();
        let stash = stash_ab().await;
        let (_dir, json) = claude_json_for("u-A");
        reconcile_display(&roster, &stash, &json, &cred(b"A-drifted"))
            .await
            .unwrap();
        assert_eq!(displayed_uuid(&json).as_deref(), Some("u-A"));
    }
}
