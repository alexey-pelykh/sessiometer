// Copyright (c) 2026 Oleksii PELYKH
// SPDX-License-Identifier: MIT

//! The swap decision and the out-of-band swap engine.
//!
//! Two concerns live here: [`decide`] turns a usage reading into a
//! [`SwapDecision`] (the poll loop's per-tick verdict), and [`swap`] is the
//! out-of-band swap **engine** that *acts* on a decision — the callable unit that
//! rotates the active credential from one account to another. The poll→decision
//! loop that calls them (#7), cooldown / terminal state (#10 / #11) and the
//! Monitor-401 re-stash trigger (#13) wire this engine in; the engine itself is
//! account-identity-agnostic, moving blobs between stash slots and the canonical
//! keychain item addressed only by `Sessiometer/acct-N` stash-service name.
//!
//! ## The swap sequence (outgoing A → incoming B, one tick, this order)
//!
//! Replicates what `claude /login` writes — the canonical keychain token (the
//! functional reroute) then `~/.claude.json`'s `oauthAccount` (honest display) —
//! to inherit its proven cross-session propagation (H2; `build/version-compat.md`):
//!   1. read A's CURRENT (silently-refreshed) canonical blob;
//!   2. **re-stash A** to its `Sessiometer/acct-N` slot BEFORE overwriting the
//!      canonical item — the token-refresh-rotation drift guard (#6 added
//!      acceptance). A's token has drifted (it refreshes in place while active),
//!      so the fresh blob is re-stashed; its `oauthAccount` is display-only and
//!      stable, so it is PRESERVED from A's existing stash, never fabricated;
//!   3. write B's token to the canonical item (atomic `-U`: a reader sees
//!      old-or-new, never empty / torn);
//!   4. co-write B's `oauthAccount` into `~/.claude.json` (best-effort display);
//!   5. re-read the canonical item to confirm B; a third writer (a concurrent
//!      `/login` or a refresh) that changed it leaves the swap unconfirmed, to be
//!      reconciled on the next cycle (re-read each cycle, never cache).
//!
//! Steps 1–3 are the swap proper (they must succeed — a failure aborts before or
//! at the atomic canonical write, leaving A safely re-stashed and the canonical
//! item un-torn); steps 4–5 are best-effort, display / diagnostic — the keychain
//! token is the authoritative bearer, so a clobbered `oauthAccount` self-heals on
//! the next reconcile (last-writer-wins).
//!
//! ## Deferred live checks (need a live token; cannot run in CI)
//!
//! Two oracles need the real login keychain plus a live Claude token, so they are
//! verified manually rather than in CI (re-run on Claude Code auth bumps):
//!   - the end-to-end LIVE oracle — after a swap, an *independent* usage read
//!     reports the new account (the functional reroute actually took effect);
//!   - the `apple-tool:`-ride version check — the CLI write still rides the
//!     `apple-tool:` ACL entry on the current Claude Code version (#2).

use std::path::Path;

use crate::claude_state;
use crate::error::Result;
use crate::keychain::CredentialStore;
use crate::stash::{AccountStash, StashedAccount};
use crate::usage::Usage;

/// What the poll loop decided to do about the active account this tick.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SwapDecision {
    /// Stay on the current account.
    Hold,
    /// The active account crossed the usage threshold; the swap engine should
    /// rotate to the next account.
    Swap,
}

/// Decide whether to swap, based on the worst-case usage dimension.
pub(crate) fn decide(usage: &Usage, threshold: f64) -> SwapDecision {
    if usage.max_ratio() >= threshold {
        SwapDecision::Swap
    } else {
        SwapDecision::Hold
    }
}

/// The result of a completed [`swap`]. The token reroute (the swap proper)
/// succeeded; these two fields report the best-effort, display-only follow-ups so
/// the caller (#7) can log or act without re-reading the keychain itself.
#[cfg_attr(not(test), allow(dead_code))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SwapReport {
    /// Whether the post-swap re-read of the canonical item still matched the token
    /// written for the incoming account. `false` means a third writer (a
    /// concurrent `/login` or a token refresh) changed it between the write and
    /// the re-read; the keychain is authoritative, so the daemon reconciles on the
    /// next cycle. The token reroute itself already succeeded — this is a
    /// display / diagnostic signal, not a swap failure.
    pub(crate) canonical_confirmed: bool,
    /// Whether the `~/.claude.json` `oauthAccount` co-write succeeded. `false` is
    /// tolerated (best-effort display correctness): a missed co-write self-heals
    /// on the next reconcile, since the keychain blob is the authoritative bearer.
    pub(crate) oauth_cowritten: bool,
}

/// Run one out-of-band swap, rotating the active credential from the outgoing
/// account to the incoming account. Both are addressed by their `Sessiometer/acct-N`
/// stash-service name; the engine is account-identity-agnostic (the daemon, #7,
/// maps roster accounts to stash names and picks the pair).
///
/// See the module docs for the five-step sequence and its invariants. Steps 1–3
/// (read outgoing, re-stash outgoing, write incoming) must succeed — a failure
/// there aborts the swap before or at the atomic canonical write, leaving the
/// outgoing account safely re-stashed and the canonical item un-torn. Steps 4–5
/// (co-write, confirm) are best-effort and reported in the returned [`SwapReport`].
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) async fn swap<C, S>(
    store: &C,
    stash: &S,
    outgoing_stash: &str,
    incoming_stash: &str,
    claude_json: &Path,
) -> Result<SwapReport>
where
    C: CredentialStore,
    S: AccountStash,
{
    // Read every input up front, BEFORE any mutation — so a failure to read any of
    // them (a locked keychain, an absent / corrupt stash) aborts the swap as a true
    // no-op, touching neither stash nor the canonical item.
    //
    // 1. The outgoing account's CURRENT (silently-refreshed) canonical blob.
    let outgoing_current = store.read().await?;
    // The outgoing account's existing stash — for its display-only `oauthAccount`
    // half, which is stable and PRESERVED across the re-stash (only the token
    // drifts; a full `StashedAccount` is required to write, so the half is sourced
    // here rather than fabricated).
    let outgoing_prev = stash.read(outgoing_stash).await?;
    // The incoming account's stash — its token (→ canonical) and `oauthAccount`
    // (→ `~/.claude.json`). Read here, before the re-stash write below, so an
    // absent / corrupt incoming stash aborts before the outgoing stash is rewritten.
    let incoming = stash.read(incoming_stash).await?;

    // 2. Re-stash the outgoing account BEFORE overwriting the canonical item — the
    //    token-refresh-rotation drift guard: re-stash its FRESH canonical token
    //    with its PRESERVED `oauthAccount` half (never fabricated).
    stash
        .write(
            outgoing_stash,
            &StashedAccount {
                credential: outgoing_current,
                oauth_account: outgoing_prev.oauth_account,
            },
        )
        .await?;

    // 3. Write the incoming account's token to the canonical item (atomic `-U`).
    store.write(&incoming.credential).await?;

    // 4. Co-write the incoming account's `oauthAccount` into `~/.claude.json`
    //    (best-effort display correctness — a failure is tolerated and self-heals).
    let oauth_cowritten =
        claude_state::write_oauth_account(claude_json, &incoming.oauth_account).is_ok();

    // 5. Post-swap re-read to confirm the canonical item still holds the token we
    //    wrote (re-read each cycle, never cache). A read failure or a third-writer
    //    change leaves it unconfirmed; the token reroute already succeeded.
    let canonical_confirmed = store
        .read()
        .await
        .is_ok_and(|current| current.matches(&incoming.credential));

    Ok(SwapReport {
        canonical_confirmed,
        oauth_cowritten,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::cell::RefCell;
    use std::collections::HashMap;
    use std::path::PathBuf;
    use std::rc::Rc;

    use crate::claude_state::OauthAccount;
    use crate::error::Error;
    use crate::keychain::{Credential, FakeCredentialStore};
    use crate::stash::FakeAccountStash;

    #[test]
    fn holds_below_threshold() {
        let usage = Usage {
            session: 0.5,
            weekly: 0.5,
        };
        assert_eq!(decide(&usage, 0.95), SwapDecision::Hold);
    }

    #[test]
    fn swaps_at_threshold_boundary() {
        let usage = Usage {
            session: 0.95,
            weekly: 0.1,
        };
        assert_eq!(decide(&usage, 0.95), SwapDecision::Swap);
    }

    // --- the swap engine (#6) ---

    const ACCT_A: &str = "Sessiometer/acct-A";
    const ACCT_B: &str = "Sessiometer/acct-B";

    fn cred(blob: &[u8]) -> Credential {
        Credential::new(blob.to_vec())
    }

    fn oauth(uuid: &str) -> OauthAccount {
        OauthAccount::from_object_bytes(
            format!(r#"{{"accountUuid":"{uuid}","emailAddress":"{uuid}@example.com"}}"#).as_bytes(),
        )
        .unwrap()
    }

    fn stashed(token: &[u8], uuid: &str) -> StashedAccount {
        StashedAccount {
            credential: cred(token),
            oauth_account: oauth(uuid),
        }
    }

    /// A minimal `~/.claude.json` displaying `uuid`, at mode `mode`, plus unrelated
    /// fields the co-write must preserve. Returns the tempdir guard and the path.
    fn claude_json(uuid: &str, mode: u32) -> (tempfile::TempDir, PathBuf) {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".claude.json");
        let body = format!(
            r#"{{"numStartups":7,"oauthAccount":{{"accountUuid":"{uuid}","emailAddress":"{uuid}@x.com"}},"projects":{{"/a":1}}}}"#
        );
        std::fs::write(&path, body).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode)).unwrap();
        (dir, path)
    }

    /// The `oauthAccount.accountUuid` currently displayed in a `~/.claude.json`.
    fn displayed_uuid(path: &Path) -> String {
        let v: serde_json::Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
        v["oauthAccount"]["accountUuid"]
            .as_str()
            .unwrap()
            .to_owned()
    }

    /// A `FakeCredentialStore` seeded with `blob` as the active canonical item.
    async fn store_holding(blob: &[u8]) -> FakeCredentialStore {
        let store = FakeCredentialStore::empty();
        store.write(&cred(blob)).await.unwrap();
        store
    }

    /// A `FakeAccountStash` seeded with both accounts' stashes.
    async fn stash_with(a: StashedAccount, b: StashedAccount) -> FakeAccountStash {
        let stash = FakeAccountStash::empty();
        stash.write(ACCT_A, &a).await.unwrap();
        stash.write(ACCT_B, &b).await.unwrap();
        stash
    }

    #[tokio::test]
    async fn reroutes_the_token_and_co_writes_the_identity() {
        let store = store_holding(b"A-token").await;
        let stash = stash_with(stashed(b"A-token", "u-A"), stashed(b"B-token", "u-B")).await;
        let (_dir, json) = claude_json("u-A", 0o600);

        let report = swap(&store, &stash, ACCT_A, ACCT_B, &json).await.unwrap();

        // The canonical item now holds B's token, and the post-swap re-read confirmed it.
        assert!(store.read().await.unwrap().matches(&cred(b"B-token")));
        assert!(report.canonical_confirmed);
        // The display identity now shows B.
        assert!(report.oauth_cowritten);
        assert_eq!(displayed_uuid(&json), "u-B");
    }

    #[tokio::test]
    async fn re_stashes_outgoing_with_its_fresh_token_and_preserved_identity() {
        // A's stash holds an OLD token; the canonical holds A's CURRENT (refreshed)
        // token — the drift the re-stash guards against. A's stashed `oauthAccount`
        // is the stable half that must be preserved (NUANCE 1).
        let store = store_holding(b"A-refreshed").await;
        let stash = stash_with(stashed(b"A-stale", "u-A"), stashed(b"B-token", "u-B")).await;
        let (_dir, json) = claude_json("u-A", 0o600);

        swap(&store, &stash, ACCT_A, ACCT_B, &json).await.unwrap();

        let a = stash.read(ACCT_A).await.unwrap();
        // A was re-stashed with its FRESH canonical token, not the stale stashed one…
        assert_eq!(a.credential.expose(), b"A-refreshed");
        // …and its display-only `oauthAccount` was PRESERVED, not fabricated/changed.
        assert_eq!(a.oauth_account.account_uuid(), "u-A");
        assert_eq!(a.oauth_account.raw_json(), oauth("u-A").raw_json());
        // The incoming write happened (after the re-stash): canonical is B's token.
        assert!(store.read().await.unwrap().matches(&cred(b"B-token")));
    }

    #[tokio::test]
    async fn re_stashes_the_outgoing_account_before_writing_the_incoming() {
        // Recording seams sharing one ordered log, so the re-stash-before-incoming
        // ordering is observed directly across the two seams.
        type Log = Rc<RefCell<Vec<String>>>;

        struct RecStore {
            log: Log,
            slot: RefCell<Option<Credential>>,
        }
        impl CredentialStore for RecStore {
            async fn read(&self) -> Result<Credential> {
                self.log.borrow_mut().push("read-canonical".into());
                self.slot.borrow().clone().ok_or(Error::CredentialNotFound)
            }
            async fn write(&self, credential: &Credential) -> Result<()> {
                self.log.borrow_mut().push("write-canonical".into());
                *self.slot.borrow_mut() = Some(credential.clone());
                Ok(())
            }
        }

        struct RecStash {
            log: Log,
            items: RefCell<HashMap<String, StashedAccount>>,
        }
        impl AccountStash for RecStash {
            async fn write(&self, service: &str, account: &StashedAccount) -> Result<()> {
                self.log.borrow_mut().push(format!("write-stash:{service}"));
                self.items
                    .borrow_mut()
                    .insert(service.to_owned(), account.clone());
                Ok(())
            }
            async fn read(&self, service: &str) -> Result<StashedAccount> {
                self.log.borrow_mut().push(format!("read-stash:{service}"));
                self.items
                    .borrow()
                    .get(service)
                    .cloned()
                    .ok_or(Error::StashIncomplete {
                        service: service.to_owned(),
                    })
            }
        }

        let log: Log = Rc::new(RefCell::new(Vec::new()));
        let store = RecStore {
            log: log.clone(),
            slot: RefCell::new(Some(cred(b"A-token"))),
        };
        let stash = RecStash {
            log: log.clone(),
            items: RefCell::new(HashMap::new()),
        };
        stash
            .write(ACCT_A, &stashed(b"A-token", "u-A"))
            .await
            .unwrap();
        stash
            .write(ACCT_B, &stashed(b"B-token", "u-B"))
            .await
            .unwrap();
        let (_dir, json) = claude_json("u-A", 0o600);
        log.borrow_mut().clear(); // ignore the seeding writes

        swap(&store, &stash, ACCT_A, ACCT_B, &json).await.unwrap();

        let log = log.borrow();
        let restash = log
            .iter()
            .position(|e| e == "write-stash:Sessiometer/acct-A")
            .expect("the outgoing account was re-stashed");
        let write_incoming = log
            .iter()
            .position(|e| e == "write-canonical")
            .expect("the incoming token was written to the canonical item");
        assert!(
            restash < write_incoming,
            "re-stash of the outgoing account must precede the incoming canonical write; log = {log:?}"
        );
    }

    #[tokio::test]
    async fn writes_the_canonical_item_before_co_writing_the_identity() {
        // A store that snapshots the displayed `~/.claude.json` uuid AT THE MOMENT
        // it writes the canonical item — proving canonical-then-oauth ordering: at
        // canonical-write time, the co-write (step 4) has not run, so the file
        // still shows the pre-swap account.
        struct SnapshotStore {
            slot: RefCell<Option<Credential>>,
            claude_json: PathBuf,
            uuid_at_write: RefCell<Option<String>>,
        }
        impl CredentialStore for SnapshotStore {
            async fn read(&self) -> Result<Credential> {
                self.slot.borrow().clone().ok_or(Error::CredentialNotFound)
            }
            async fn write(&self, credential: &Credential) -> Result<()> {
                let snap = std::fs::read(&self.claude_json)
                    .ok()
                    .and_then(|b| serde_json::from_slice::<serde_json::Value>(&b).ok())
                    .and_then(|v| v["oauthAccount"]["accountUuid"].as_str().map(str::to_owned));
                *self.uuid_at_write.borrow_mut() = snap;
                *self.slot.borrow_mut() = Some(credential.clone());
                Ok(())
            }
        }

        let (_dir, json) = claude_json("u-A", 0o600);
        let store = SnapshotStore {
            slot: RefCell::new(Some(cred(b"A-token"))),
            claude_json: json.clone(),
            uuid_at_write: RefCell::new(None),
        };
        let stash = stash_with(stashed(b"A-token", "u-A"), stashed(b"B-token", "u-B")).await;

        swap(&store, &stash, ACCT_A, ACCT_B, &json).await.unwrap();

        // At the instant the canonical item was written, the co-write had NOT run,
        // so the file still showed the pre-swap account…
        assert_eq!(store.uuid_at_write.borrow().as_deref(), Some("u-A"));
        // …and after the swap the co-write has landed: the file now shows B.
        assert_eq!(displayed_uuid(&json), "u-B");
    }

    #[tokio::test]
    async fn b_to_a_to_b_cycle_keeps_canonical_and_identity_consistent() {
        let store = store_holding(b"A-token").await;
        let stash = stash_with(stashed(b"A-token", "u-A"), stashed(b"B-token", "u-B")).await;
        let (_dir, json) = claude_json("u-A", 0o600);

        for (from, to, token, uuid) in [
            (ACCT_A, ACCT_B, b"B-token".as_slice(), "u-B"),
            (ACCT_B, ACCT_A, b"A-token".as_slice(), "u-A"),
            (ACCT_A, ACCT_B, b"B-token".as_slice(), "u-B"),
        ] {
            let report = swap(&store, &stash, from, to, &json).await.unwrap();
            assert!(report.canonical_confirmed, "{from} -> {to} should confirm");
            assert!(
                store.read().await.unwrap().matches(&cred(token)),
                "{from} -> {to}: canonical should hold the incoming token"
            );
            assert_eq!(
                displayed_uuid(&json),
                uuid,
                "{from} -> {to}: identity should match"
            );
        }
    }

    #[tokio::test]
    async fn a_canonical_oauth_mismatch_reconciles_to_the_incoming_account() {
        // A deliberate pre-existing inconsistency: the canonical says A, but the
        // displayed identity is a THIRD account (e.g. a prior best-effort co-write
        // was clobbered). The swap must reconcile BOTH halves to the incoming account.
        let store = store_holding(b"A-token").await;
        let stash = stash_with(stashed(b"A-token", "u-A"), stashed(b"B-token", "u-B")).await;
        let (_dir, json) = claude_json("u-STALE", 0o600);

        let report = swap(&store, &stash, ACCT_A, ACCT_B, &json).await.unwrap();

        assert!(report.canonical_confirmed);
        assert!(report.oauth_cowritten);
        assert!(store.read().await.unwrap().matches(&cred(b"B-token")));
        assert_eq!(displayed_uuid(&json), "u-B");
    }

    #[tokio::test]
    async fn a_third_writer_between_write_and_re_read_leaves_the_swap_unconfirmed() {
        // The confirmation re-read (the 2nd read) returns a different blob than was
        // written — a concurrent `/login` or refresh winning the race between the
        // write and the post-swap re-read.
        struct ThirdWriterStore {
            reads: RefCell<u32>,
            slot: RefCell<Option<Credential>>,
            third_writer: Credential,
        }
        impl CredentialStore for ThirdWriterStore {
            async fn read(&self) -> Result<Credential> {
                let mut n = self.reads.borrow_mut();
                *n += 1;
                if *n == 1 {
                    // Step 1: the outgoing account's current blob.
                    self.slot.borrow().clone().ok_or(Error::CredentialNotFound)
                } else {
                    // Step 5: a concurrent writer has since changed the item.
                    Ok(self.third_writer.clone())
                }
            }
            async fn write(&self, credential: &Credential) -> Result<()> {
                *self.slot.borrow_mut() = Some(credential.clone());
                Ok(())
            }
        }

        let store = ThirdWriterStore {
            reads: RefCell::new(0),
            slot: RefCell::new(Some(cred(b"A-token"))),
            third_writer: cred(b"C-from-a-concurrent-login"),
        };
        let stash = stash_with(stashed(b"A-token", "u-A"), stashed(b"B-token", "u-B")).await;
        let (_dir, json) = claude_json("u-A", 0o600);

        let report = swap(&store, &stash, ACCT_A, ACCT_B, &json).await.unwrap();

        // The token reroute still happened (B was written); the re-read just could
        // not confirm it, because a third writer won the race.
        assert!(!report.canonical_confirmed);
        // The co-write to ~/.claude.json still succeeded (best-effort display).
        assert!(report.oauth_cowritten);
    }

    #[tokio::test]
    async fn an_absent_outgoing_stash_aborts_before_overwriting_the_canonical() {
        // Only B is stashed; A's stash is absent, so the REQUIRED re-stash of the
        // outgoing account cannot run — the swap must abort before the canonical
        // item is touched (no half-swap).
        let store = store_holding(b"A-token").await;
        let stash = FakeAccountStash::empty();
        stash
            .write(ACCT_B, &stashed(b"B-token", "u-B"))
            .await
            .unwrap();
        let (_dir, json) = claude_json("u-A", 0o600);

        let result = swap(&store, &stash, ACCT_A, ACCT_B, &json).await;

        assert!(matches!(result, Err(Error::StashIncomplete { .. })));
        // The canonical item is untouched — still A's token.
        assert!(store.read().await.unwrap().matches(&cred(b"A-token")));
    }

    #[tokio::test]
    async fn an_absent_incoming_stash_aborts_before_re_stashing_the_outgoing() {
        // Only A is stashed; B's stash is absent. Because every input is read before
        // any mutation, the swap aborts as a true no-op: the outgoing account's
        // stash is NOT rewritten and the canonical item is untouched.
        let store = store_holding(b"A-token").await;
        let stash = FakeAccountStash::empty();
        // A's stashed token deliberately DIFFERS from the canonical, so a re-stash
        // (which would copy the canonical token in) is detectable if it wrongly ran.
        stash
            .write(ACCT_A, &stashed(b"A-stash-token", "u-A"))
            .await
            .unwrap();
        let (_dir, json) = claude_json("u-A", 0o600);

        let result = swap(&store, &stash, ACCT_A, ACCT_B, &json).await;

        assert!(matches!(result, Err(Error::StashIncomplete { .. })));
        // A's stash was NOT rewritten (still its original token, not the canonical
        // one) — the abort touched nothing.
        let a = stash.read(ACCT_A).await.unwrap();
        assert_eq!(a.credential.expose(), b"A-stash-token");
        // The canonical item is likewise untouched.
        assert!(store.read().await.unwrap().matches(&cred(b"A-token")));
    }
}
