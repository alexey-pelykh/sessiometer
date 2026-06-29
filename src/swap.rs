// Copyright (c) 2026 Oleksii PELYKH
// SPDX-License-Identifier: MIT

//! The swap decision and the out-of-band swap engine.
//!
//! Two concerns live here: [`decide`] turns a usage reading into a
//! [`SwapDecision`] (the poll loop's per-tick verdict), and [`swap`] is the
//! out-of-band swap **engine** that *acts* on a decision — the callable unit that
//! rotates the active credential from one account to another. The poll→decision
//! loop that calls them (#7) and the cooldown / terminal state (#10 / #11) wire
//! this engine in; the engine itself is account-identity-agnostic, moving blobs
//! between stashes and the canonical keychain item addressed only by
//! `Sessiometer/<account_uuid>` stash-service name. (Issue #13's re-auth re-stash
//! is a sibling path: it refreshes a single stash through the same `AccountStash`
//! seam on a detected canonical change, without driving this swap engine.)
//!
//! ## The swap sequence (outgoing A → incoming B, one tick, this order)
//!
//! Replicates what `claude /login` writes — the canonical keychain token (the
//! functional reroute) then `~/.claude.json`'s `oauthAccount` (honest display) —
//! to inherit its proven cross-session propagation (H2; `build/version-compat.md`):
//!   1. read A's CURRENT (silently-refreshed) canonical blob;
//!   2. **re-stash A** to its `Sessiometer/<account_uuid>` stash BEFORE overwriting the
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
//! ## Mid-turn correctness (issue #12)
//!
//! Because the target app re-reads the canonical credential **per request**, a swap
//! that lands mid-turn must present a clean cut: the next request picks up the
//! incoming account, the in-flight request is unaffected, and no reader ever
//! observes a torn / half-written blob. That cut rests entirely on step 3's atomic
//! `-U` canonical write ("old-or-new, never empty / torn"). The
//! `tests::mid_turn_live` oracle demonstrates it against the real `security` CLI:
//! a concurrent reader re-reading the canonical item across a forced swap sees the
//! outgoing account, then the incoming one, and never anything in between. The
//! remaining live-only tail — the in-flight request's at-most-one
//! transparently-retried 401 — is the *target's* own retry, not ours, and stays a
//! deferred manual check (below).
//!
//! ## Deferred live checks (need a live token; cannot run in CI)
//!
//! These oracles need the real login keychain plus a live Claude token, so they are
//! verified manually rather than in CI (re-run on Claude Code auth bumps):
//!   - the end-to-end LIVE oracle — after a swap, an *independent* usage read
//!     reports the new account (the functional reroute actually took effect);
//!   - the mid-turn live tail (#12) — a running session adopts the incoming account
//!     on its next request, and the in-flight request absorbs at most one
//!     transparently-retried 401; `tests::mid_turn_live` proves the
//!     credential-cut half in CI, this is the target-behaviour half;
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
    /// One of the active account's usage dimensions reached its own trigger
    /// (issue #41: session or weekly); the swap engine should rotate to the next
    /// account.
    Swap,
}

/// Decide whether to swap: trigger when EITHER dimension reaches its OWN
/// threshold (issue #41) — the active account's session usage at/above
/// `session_threshold`, OR its weekly usage at/above the separate (typically
/// higher) `weekly_threshold`. The two thresholds are independent: either
/// crossing alone forces a swap-away, and neither subsumes the other.
pub(crate) fn decide(usage: &Usage, session_threshold: f64, weekly_threshold: f64) -> SwapDecision {
    if usage.session >= session_threshold || usage.weekly >= weekly_threshold {
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
/// account to the incoming account. Both are addressed by their `Sessiometer/<account_uuid>`
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
    fn holds_when_both_dimensions_are_below_their_thresholds() {
        let usage = Usage {
            session: 0.5,
            weekly: 0.5,
            weekly_resets_at: None,
        };
        // Session below 0.95 AND weekly below 0.98 → hold.
        assert_eq!(decide(&usage, 0.95, 0.98), SwapDecision::Hold);
    }

    #[test]
    fn swaps_when_session_reaches_its_threshold() {
        // AC #1 (regression preserved): session at its threshold → swap, even
        // with weekly far below its separate (higher) threshold.
        let usage = Usage {
            session: 0.95,
            weekly: 0.1,
            weekly_resets_at: None,
        };
        assert_eq!(decide(&usage, 0.95, 0.98), SwapDecision::Swap);
    }

    #[test]
    fn swaps_when_weekly_reaches_its_threshold_while_session_is_below() {
        // AC #2: weekly at its threshold while session sits below its own → swap.
        let usage = Usage {
            session: 0.50,
            weekly: 0.98,
            weekly_resets_at: None,
        };
        assert_eq!(decide(&usage, 0.95, 0.98), SwapDecision::Swap);
    }

    #[test]
    fn the_two_thresholds_gate_their_dimensions_independently() {
        // AC #3: each dimension is gated by its OWN threshold. A weekly reading of
        // 0.96 — between the two thresholds — does NOT trigger while the weekly
        // threshold is the higher 0.98, but the SAME reading DOES trigger once the
        // weekly threshold is lowered to 0.95. Session is held below its threshold
        // throughout, isolating the weekly axis.
        let usage = Usage {
            session: 0.50,
            weekly: 0.96,
            weekly_resets_at: None,
        };
        assert_eq!(decide(&usage, 0.95, 0.98), SwapDecision::Hold);
        assert_eq!(decide(&usage, 0.95, 0.95), SwapDecision::Swap);
    }

    // --- the swap engine (#6) ---

    const ACCT_A: &str = "Sessiometer/u-A";
    const ACCT_B: &str = "Sessiometer/u-B";

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
            async fn delete(&self, service: &str) -> Result<()> {
                self.log
                    .borrow_mut()
                    .push(format!("delete-stash:{service}"));
                self.items.borrow_mut().remove(service);
                Ok(())
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
            .position(|e| e == "write-stash:Sessiometer/u-A")
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

    /// The mid-turn swap-correctness oracle (issue #12), driven end-to-end against
    /// the real `/usr/bin/security` CLI on a throwaway keychain — never the login
    /// keychain. macOS-only: the property rests on `security -U`'s atomic in-place
    /// update (`build/version-compat.md`), so the real CLI is the system under
    /// test (the same reason [`crate::keychain`]'s round-trip lives behind this cfg).
    ///
    /// Models the scenario the issue names: the target app (Claude Code) re-reads
    /// the canonical credential **per request**, so a swap that lands mid-turn must
    /// present a clean cut — a concurrent reader sees the outgoing account, then the
    /// incoming account, and never a torn / empty / half-written blob in between.
    /// The fully-live tail (the in-flight request's at-most-one transparently-retried
    /// 401, which is the *target's* retry, not ours) needs a live Claude token and
    /// stays a deferred manual oracle — see the module docs.
    #[cfg(target_os = "macos")]
    mod mid_turn_live {
        use super::*;

        use std::process::Command as StdCommand;
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::Arc;
        use std::time::{Duration, Instant};

        use crate::keychain::RealCredentialStore;
        use crate::stash::RealAccountStash;

        /// Claude Code's well-known generic-password service for the canonical
        /// credential (mirrors the private `keychain::SERVICE`; hard-coded here
        /// because the test seeds the item the way `/login` would).
        const CANONICAL_SERVICE: &str = "Claude Code-credentials";
        /// A canonical `acct` deliberately unlike `$USER`, so the store resolves the
        /// STORED acct rather than guessing — the same point `keychain`'s round-trip
        /// test makes.
        const CANONICAL_ACCT: &str = "sessiometer-midturn-acct";

        /// Make + unlock a throwaway keychain; the returned tempdir guard keeps it
        /// alive. Mirrors `keychain::tests::real_cli::fresh_keychain`.
        fn fresh_keychain() -> (tempfile::TempDir, PathBuf) {
            let dir = tempfile::tempdir().unwrap();
            let kc = dir.path().join("test.keychain-db");
            assert!(StdCommand::new("/usr/bin/security")
                .args(["create-keychain", "-p", ""])
                .arg(&kc)
                .status()
                .expect("spawn create-keychain")
                .success());
            assert!(StdCommand::new("/usr/bin/security")
                .args(["unlock-keychain", "-p", ""])
                .arg(&kc)
                .status()
                .expect("spawn unlock-keychain")
                .success());
            (dir, kc)
        }

        /// Seed the canonical `Claude Code-credentials` item, simulating `/login`.
        fn seed_canonical(kc: &Path, secret: &str) {
            assert!(StdCommand::new("/usr/bin/security")
                .args([
                    "add-generic-password",
                    "-U",
                    "-s",
                    CANONICAL_SERVICE,
                    "-a",
                    CANONICAL_ACCT,
                    "-w",
                    secret,
                ])
                .arg(kc)
                .status()
                .expect("spawn add-generic-password")
                .success());
        }

        fn delete_keychain(kc: &Path) {
            let _ = StdCommand::new("/usr/bin/security")
                .arg("delete-keychain")
                .arg(kc)
                .status();
        }

        /// AC (#12): a scripted long-running request + a forced mid-request swap →
        /// the request completes AND the next request reports the new account.
        ///
        /// The "long-running request" is a reader re-reading the canonical item in a
        /// tight loop (the target's per-request read); the "forced mid-request swap"
        /// is the real [`swap`] rotating A → B underneath it. The reader runs as its
        /// own task so its `security` reads genuinely race the swap's `security`
        /// write on the shared keychain.
        #[tokio::test]
        async fn a_long_running_request_completes_and_the_next_request_reports_the_new_account() {
            // Seed the canonical item to A and stash both A and B — the state capture
            // (#4) plus a prior `/login` would leave behind.
            let (_dir, kc) = fresh_keychain();
            seed_canonical(&kc, "A-token");
            let stash = RealAccountStash::for_keychain(kc.clone());
            stash
                .write(ACCT_A, &stashed(b"A-token", "u-A"))
                .await
                .unwrap();
            stash
                .write(ACCT_B, &stashed(b"B-token", "u-B"))
                .await
                .unwrap();
            let (_json_dir, json) = claude_json("u-A", 0o600);

            // `saw_a` gates the swap until the request has actually read the OUTGOING
            // account at least once (so the record spans the cut, not just lands on
            // B); `swap_done` lets the reader stop once it observes the new account
            // after the swap has returned.
            let saw_a = Arc::new(AtomicBool::new(false));
            let swap_done = Arc::new(AtomicBool::new(false));

            let reader = {
                let kc = kc.clone();
                let saw_a = Arc::clone(&saw_a);
                let swap_done = Arc::clone(&swap_done);
                tokio::spawn(async move {
                    let store = RealCredentialStore::for_keychain(kc);
                    let mut seen: Vec<Vec<u8>> = Vec::new();
                    // Reads that found the canonical item ABSENT (errSecItemNotFound,
                    // code 44 → `CredentialNotFound`). The atomic `-U` write keeps the
                    // item present at every instant, so this must stay zero — a
                    // non-zero count is exactly the window a non-atomic delete-then-add
                    // would open, which is asserted against below. Capturing it (rather
                    // than discarding every error) is what makes "never torn / never
                    // absent" falsifiable in CI, not merely observed.
                    let mut absent_reads: u32 = 0;
                    // A wall-clock backstop so a regression that never cuts over
                    // FAILS the assertions below rather than hanging CI.
                    let deadline = Instant::now() + Duration::from_secs(30);
                    while Instant::now() < deadline {
                        match store.read().await {
                            Ok(c) => {
                                let blob = c.expose().to_vec();
                                if blob.as_slice() == b"A-token" {
                                    saw_a.store(true, Ordering::SeqCst);
                                }
                                let is_b = blob.as_slice() == b"B-token";
                                seen.push(blob);
                                if is_b && swap_done.load(Ordering::SeqCst) {
                                    break;
                                }
                            }
                            // The item was absent — the forbidden non-atomic window.
                            Err(Error::CredentialNotFound) => absent_reads += 1,
                            // Any other error is benign contention under the concurrent
                            // write (a locked / busy keychain); the target would
                            // transparently retry, so the loop retries too.
                            Err(_) => {}
                        }
                        tokio::task::yield_now().await;
                    }
                    (seen, absent_reads)
                })
            };

            // Hold the swap until the in-flight request has read A at least once.
            let gate = Instant::now() + Duration::from_secs(10);
            while !saw_a.load(Ordering::SeqCst) {
                assert!(
                    Instant::now() < gate,
                    "the in-flight request never read the pre-swap account A"
                );
                tokio::task::yield_now().await;
            }

            // The forced mid-request swap.
            let swap_store = RealCredentialStore::for_keychain(kc.clone());
            let report = swap(&swap_store, &stash, ACCT_A, ACCT_B, &json)
                .await
                .unwrap();
            swap_done.store(true, Ordering::SeqCst);

            let (seen, absent_reads) = reader.await.expect("the reader task panicked");

            // The canonical item was never absent mid-swap: the atomic `-U` write
            // never opened a delete-then-add gap a per-request reader could fall
            // through (which would surface as item-not-found).
            assert_eq!(
                absent_reads, 0,
                "the canonical item went ABSENT mid-swap ({absent_reads}×) — the write was not atomic"
            );
            // The request completed: every observation is a COMPLETE, valid
            // credential — exactly the outgoing or the incoming token, never empty /
            // half-written / garbage. This is the atomic-`-U` guarantee in action.
            for (i, blob) in seen.iter().enumerate() {
                assert!(
                    blob.as_slice() == b"A-token" || blob.as_slice() == b"B-token",
                    "read #{i} saw a torn credential ({} bytes) — the swap was not atomic",
                    blob.len()
                );
            }
            // It genuinely spanned the swap: it read the outgoing account…
            assert!(
                seen.iter().any(|b| b.as_slice() == b"A-token"),
                "never observed the pre-swap account A"
            );
            // …and the next request reports the new account.
            assert!(
                seen.last().is_some_and(|b| b.as_slice() == b"B-token"),
                "the request did not end on the post-swap account B"
            );
            // The cut is clean and one-way: once B appears, A never returns.
            let first_b = seen
                .iter()
                .position(|b| b.as_slice() == b"B-token")
                .expect("never observed the post-swap account B");
            assert!(
                seen[first_b..].iter().all(|b| b.as_slice() == b"B-token"),
                "the active credential flapped back to A after the cutover"
            );

            // An independent fresh read confirms the canonical reroute landed…
            assert!(swap_store.read().await.unwrap().matches(&cred(b"B-token")));
            assert!(report.canonical_confirmed);
            // …and the OUTGOING account is unaffected: A's credential is preserved,
            // intact and recoverable, in its own stash — the in-flight request that
            // already read A can still complete against it.
            let a = stash.read(ACCT_A).await.unwrap();
            assert_eq!(a.credential.expose(), b"A-token");

            delete_keychain(&kc);
        }
    }
}
