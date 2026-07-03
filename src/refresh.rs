// Copyright (c) 2026 Oleksii PELYKH
// SPDX-License-Identifier: MIT

//! The isolated-`CLAUDE_CONFIG_DIR` credential-refresh engine (issue #102).
//!
//! Keeps a *parked* (non-active) managed account's stashed OAuth token from going
//! stale by letting Claude Code refresh it itself, in an isolated config dir, without
//! ever touching the live session's canonical credential. One `refresh_cycle` seeds a
//! copy of the account's stashed credential into an ephemeral isolated keychain item,
//! spawns `claude -p` pointed at that isolated `CLAUDE_CONFIG_DIR` so CC performs its
//! OWN refresh against that item, reads the refreshed token back, and re-stashes it.
//! The shared async engine here is wired in by two thin callers later (the daemon's
//! background refresh and the recovery path).
//!
//! ## The cycle (one parked account, this order)
//!
//!   1. **(short-held `swap.lock`)** read the account's stash — its stored credential
//!      (the CAS snapshot) and its identity. Lock released here, BEFORE the spawn.
//!   2. **`IsolatedSession` RAII guard**: create an ephemeral **0700** dir at
//!      `<support>/refresh/<account-uuid>` (owner-checked, symlink-refused —
//!      [`paths::create_isolated_dir`]) and derive its isolated keychain service by the
//!      #100 function ([`crate::keychain::service_for_config_dir`]). Teardown arms here.
//!   3. seed the isolated keychain item via the **apple-tool:** `security -i` path
//!      ([`IsolatedKeychain::seed`]) with a **back-dated** `expiresAt` copy of
//!      the credential (so CC's 5-minute pre-expiry predicate is unconditionally true,
//!      forcing a refresh — #101 AC-5) + a minimal isolated `.claude.json` (0600).
//!   4. spawn `claude -p "<benign>"` with `CLAUDE_CONFIG_DIR=<dir>`, stdio nulled, no
//!      token env, killed after a timeout — CC reads the seeded item and refreshes it.
//!   5. read the refreshed credential back from the isolated item. The read-back is
//!      **silent** — CC saves via the same `apple-tool:` `security` path, so the
//!      partition list is never re-stamped and NO heal-write is needed (#101 AC-2).
//!   6. classify before/after → **refreshed / no-change / dead / error**. The DEAD
//!      signal is `refreshToken == ""` (CC clears it in place), NOT `expiresAt`-based.
//!   7. **(short-held `swap.lock`)** CAS re-stash: write the fresh token to the
//!      account's stash **only if its stored credential is unchanged since step 1**;
//!      else discard (a concurrent swap / login is authoritative). Identity preserved;
//!      the real stash is the LAST write. A **dead** account is NOT re-stashed —
//!      left as-is and surfaced.
//!   8. `teardown().await`: delete the isolated item + dir (best-effort + retry). The
//!      RAII guard also tears down on error / panic / timer-kill.
//!
//! ## Safety invariants
//!
//!   - **apple-tool: on both writes** — the seed ([`IsolatedKeychain::seed`])
//!     and the re-stash ([`AccountStash::write`]) both ride `/usr/bin/security`
//!     (`security -i`, off-argv — #39), never the Security.framework SDK.
//!   - **secrets zeroized + redaction-safe** — every blob the engine holds is a
//!     [`crate::keychain::Credential`] / `Zeroizing` (wiped on drop); the only value it returns is a
//!     non-secret [`RefreshReport`] (a classification + an integer delta + booleans),
//!     proven leak-free by the redaction-METER test below.
//!   - **lock held only around steps 1 & 7** — never across the spawn (the lock would
//!     otherwise stall the swap engine for the whole `claude -p` runtime).
//!   - **dead account ⇒ no re-stash** — surfaced, the stash left untouched.
//!
//! ## AC-3 telemetry (durable-TTL observation)
//!
//! Whether the refresh delivers durable multi-day value reduces to a server property
//! (does the window slide or hit a cap; does the refresh token rotate) that a single
//! real refresh reveals but that #101 deliberately did NOT run (it would exchange a
//! real refresh token against the operator's live accounts). Instead each cycle's
//! [`RefreshReport`] carries the `expiresAt` delta and the refresh-token-rotation flag,
//! so the engine's OWN first days of operation are the safe multi-day observation
//! (#101 AC-3) — gathered through this CAS-protected flow, never a bespoke probe. The
//! rotation half of that question has since been RESOLVED (the server rotates — see the
//! Caller-contract note below, spike #262); the flag remains the per-cycle new-token
//! signal, and the sliding-window-vs-cap TTL question stays open.
//!
//! ## Caller contract (the two thin callers must honor)
//!
//! The engine is a correct SINGLE cycle, but two hazards are intrinsic to its
//! single-cycle, lock-not-held-across-the-spawn shape and are handled by the callers
//! that wire it in (separate issues), NOT by the engine:
//!
//!   - **Refresh PARKED accounts only.** The CAS guard (step 7) re-stashes only if the
//!     account's stored credential is unchanged since step 1 — but a concurrent swap that
//!     promotes this account to ACTIVE reads its stash WITHOUT rewriting it (the swap
//!     engine, #6), so the "unchanged" check cannot observe the promotion. A caller must
//!     therefore never refresh the active account or an imminent swap target; choose
//!     genuinely-parked accounts.
//!   - **A refresh whose re-stash fails forfeits the fresh token.** If the spawned
//!     `claude` performs the real OAuth exchange (step 4) and the server ROTATES the
//!     refresh token, the old token is invalidated server-side; if step 7's
//!     short-held-lock re-stash then fails transiently (a contended `swap.lock`, a
//!     momentarily-locked keychain) the cycle returns `Err` and teardown (step 8) deletes
//!     the only copy of the fresh token. The next cycle re-seeds the now-stale token and
//!     classifies the account `Dead`. This is bounded and RECOVERABLE — the re-auth /
//!     dead-credential recovery path (#13/#42) surfaces a dead account for an operator
//!     re-login — so a caller treats a refresh `Err` as NON-fatal (log + let recovery
//!     self-heal), never as corruption. **Resolved (spike #262) — the server rotates
//!     (Medium-High):** Anthropic's token endpoint issues a NEW refresh token on each
//!     exchange — exactly what this engine's own `refresh_token_rotated` telemetry (the
//!     AC-3 section above) measures. The new-token half is directly observed; the
//!     invalidation-of-the-old half is INFERRED, not measured here — RFC 9700 §2.2.2
//!     makes rotation (or sender-constraining) a MUST for a public PKCE client, and the
//!     reproduced concurrent-session race (claude-code#24317) plus RE of CC's OAuth flow
//!     corroborate it. OBSERVED, Anthropic-undocumented behavior — the safe posture, not
//!     a contracted guarantee; a live retry of the old token would still settle the
//!     grace-window / reuse-revocation questions #262 leaves open. So the
//!     invalidated-server-side premise above HOLDS and the #253 exclusion stands.
//!
//! ## Deferred live check (needs a live token; cannot run in CI)
//!
//! The full `claude -p` refresh against a real token is not exercised in CI (it would
//! rotate a real refresh token — the zero-impact mandate, #101). The hermetic tests
//! drive the engine's logic with fakes; a real-CLI test ([`crate::keychain`]) covers the
//! isolated keychain item mechanics on a throwaway keychain; the live refresh is the
//! engine's own production telemetry (above).

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::Value;
use zeroize::Zeroizing;

use crate::error::Result;
use crate::isolated_spawn::{ClaudeRefresh, IsolatedSession, SpawnClaude};
use crate::keychain::IsolatedKeychain;
use crate::paths;
use crate::stash::{AccountStash, StashedAccount};
use crate::swap::{SwapLock, SWAP_LOCK_MAX_WAIT};

/// A minimal isolated `.claude.json` (0600). Headless `claude -p` needs no onboarding
/// / theme / trust keys (#101 AC-5) and auto-writes its own minimal file; this empty
/// object is belt-and-suspenders so the isolated dir is never wholly empty.
const MINIMAL_CLAUDE_JSON: &[u8] = b"{}\n";

/// How the engine classified one refresh cycle (issue #102 step 6).
#[cfg_attr(not(test), allow(dead_code))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RefreshOutcome {
    /// CC slid the expiry forward past the back-dated marker — a fresh token, which
    /// is re-stashed (CAS-permitting).
    Refreshed,
    /// CC returned the seeded token unchanged — no refresh happened. Not re-stashed.
    NoChange,
    /// CC cleared the refresh token in place (`refreshToken == ""`) — it is dead and
    /// needs an operator re-login. NOT re-stashed; surfaced (issue #102 step 7).
    Dead,
    /// The cycle ran but produced no usable result (the spawn failed, the read-back
    /// was unreadable / unparseable, or the stored credential was malformed). NOT
    /// re-stashed.
    Error,
}

/// The result of one [`refresh_cycle`] — the breadcrumb the engine surfaces.
///
/// Every field is **non-secret** (a classification, an integer delta in seconds,
/// booleans), so it is safe to hand a caller to log; the redaction-METER test below
/// proves a cycle handling a known secret leaks none of it into this report. The
/// `expires_at_delta_secs` + `refresh_token_rotated` pair is the AC-3 durable-TTL
/// telemetry (the module docs).
#[cfg_attr(not(test), allow(dead_code))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct RefreshReport {
    /// The cycle's classification.
    pub(crate) outcome: RefreshOutcome,
    /// `expiresAt(after) − expiresAt(before-refresh)` in **seconds** on a successful
    /// refresh — the sliding-vs-cap signal (a positive, roughly-constant value across
    /// cycles ⇒ sliding window; shrinking toward zero ⇒ a cap). `None` for any
    /// non-`Refreshed` outcome, or when no usable before/after expiry was read.
    pub(crate) expires_at_delta_secs: Option<i64>,
    /// Whether CC **rotated** the refresh token (the after RT differs from the seeded
    /// RT) — the other half of the AC-3 durability signal. Carries only the boolean,
    /// never either token value.
    pub(crate) refresh_token_rotated: bool,
    /// Whether the CAS re-stash actually wrote (only on `Refreshed`, and only when the
    /// account's stored credential was unchanged since the cycle began).
    pub(crate) re_stashed: bool,
}

impl RefreshReport {
    /// An indeterminate (`Error`) outcome with no telemetry and no re-stash — used when
    /// the cycle ran but produced nothing classifiable.
    fn indeterminate() -> Self {
        Self {
            outcome: RefreshOutcome::Error,
            expires_at_delta_secs: None,
            refresh_token_rotated: false,
            re_stashed: false,
        }
    }
}

/// `claudeAiOauth.expiresAt` as an epoch-millisecond integer (CC stores
/// `Date.now() + expires_in*1000`), or `None` if absent / unparseable. Non-secret —
/// only the expiry timestamp is read, never the token. `pub(crate)` so the daemon's
/// poll path (issue #141) can reuse this one audited extractor on the active account's
/// canonical blob, the way [`stored_expires_at`] serves a per-account stash.
pub(crate) fn expires_at(blob: &[u8]) -> Option<i64> {
    let value: Value = serde_json::from_slice(blob).ok()?;
    value.get("claudeAiOauth")?.get("expiresAt")?.as_i64()
}

/// `claudeAiOauth.refreshToken` as raw bytes wrapped `Zeroizing` (wiped on drop), or
/// `None` if the field is absent / the blob is unparseable. `Some(empty)` is the DEAD
/// signal (CC clears the RT in place — `build/version-compat.md` #101) and is kept
/// distinct from `None` (a missing field). The value is only ever emptiness-checked or
/// byte-compared, never logged or placed in a report.
///
/// Like [`crate::usage`]'s token extraction, the blob is parsed via `serde_json::Value`
/// — a transient in-process structure that briefly holds the token un-zeroized before
/// the wanted field is copied into a `Zeroizing` buffer; this never reaches an output
/// channel (the redaction METER, #15, guards outputs, not in-process heap).
fn refresh_token(blob: &[u8]) -> Option<Zeroizing<Vec<u8>>> {
    let value: Value = serde_json::from_slice(blob).ok()?;
    let rt = value.get("claudeAiOauth")?.get("refreshToken")?.as_str()?;
    Some(Zeroizing::new(rt.as_bytes().to_vec()))
}

/// Re-serialize `blob` with `claudeAiOauth.expiresAt` set to `now_ms` — a back-dated
/// expiry (`<= now`) that makes Claude Code's 5-minute pre-expiry refresh predicate
/// unconditionally true, forcing a deterministic on-demand refresh when the spawned
/// `claude` reads the seeded item (#101 AC-5). Returns the rewritten blob wrapped
/// `Zeroizing` (it carries the bearer token); `None` if `blob` is not the expected
/// `{"claudeAiOauth":{…}}` object (a corrupt stash — surfaced as an indeterminate
/// outcome rather than seeding a malformed item).
///
/// Re-serialization may reorder JSON keys, which is harmless: CC parses the seed
/// order-agnostically, and the refreshed read-back is CC's OWN output (re-stashed
/// verbatim), so this reordering never reaches the canonical stash.
fn backdate(blob: &[u8], now_ms: i64) -> Option<Zeroizing<Vec<u8>>> {
    let mut value: Value = serde_json::from_slice(blob).ok()?;
    let oauth = value.get_mut("claudeAiOauth")?.as_object_mut()?;
    oauth.insert("expiresAt".to_owned(), Value::from(now_ms));
    let out = serde_json::to_vec(&value).ok()?;
    Some(Zeroizing::new(out))
}

/// Classify one cycle (issue #102 step 6) and compute the AC-3 telemetry, reading
/// `original` (the pre-refresh stored blob), `seeded` (the back-dated blob handed to
/// CC) and `after` (the read-back). Returns `(outcome, expires_at_delta_secs,
/// refresh_token_rotated)`.
///
/// The refresh-token VALUES never escape this function — they live only in `Zeroizing`
/// temporaries used for the emptiness check (the DEAD signal) and the rotation compare.
fn classify(original: &[u8], seeded: &[u8], after: &[u8]) -> (RefreshOutcome, Option<i64>, bool) {
    let after_rt = refresh_token(after);
    let rotated = match (refresh_token(seeded), &after_rt) {
        (Some(seeded_rt), Some(after_rt)) => seeded_rt.as_slice() != after_rt.as_slice(),
        _ => false,
    };
    let after_exp = expires_at(after);
    let seeded_exp = expires_at(seeded);

    let outcome = match &after_rt {
        // The read-back had no parseable refresh token → indeterminate.
        None => RefreshOutcome::Error,
        // CC cleared the refresh token in place → the token is DEAD (#101 dead signal).
        Some(rt) if rt.is_empty() => RefreshOutcome::Dead,
        // A non-empty refresh token: did the expiry slide forward past our back-dated
        // marker (CC refreshed) or stay at it (no refresh)?
        Some(_) => match (after_exp, seeded_exp) {
            (Some(after_exp), Some(seeded_exp)) if after_exp > seeded_exp => {
                RefreshOutcome::Refreshed
            }
            (Some(_), Some(_)) => RefreshOutcome::NoChange,
            // The expiry was unreadable on one side — cannot tell refreshed from not.
            _ => RefreshOutcome::Error,
        },
    };

    // The sliding-vs-cap delta (#101 AC-3) is meaningful only on a successful refresh:
    // how far the REAL window moved, measured against the original (pre-back-date)
    // expiry, in seconds.
    let delta = if outcome == RefreshOutcome::Refreshed {
        match (after_exp, expires_at(original)) {
            (Some(after_exp), Some(original_exp)) => Some((after_exp - original_exp) / 1000),
            _ => None,
        }
    } else {
        None
    };

    (outcome, delta, rotated)
}

/// Current wall-clock as epoch milliseconds (the unit CC's `expiresAt` uses). A
/// pre-1970 clock renders as `0` — a clearly-wrong but safe sentinel that simply
/// forces a refresh, never panics.
#[cfg_attr(not(test), allow(dead_code))]
fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Acquire the swap lock when `lock` is `Some` (the production path: serialized against
/// the swap engine on the shared `swap.lock`, #64), or `None` for the hermetic
/// single-process tests where there is no second writer to serialize against.
async fn acquire_swap_lock(lock: Option<(&Path, Duration)>) -> Result<Option<SwapLock>> {
    match lock {
        Some((path, max_wait)) => Ok(Some(SwapLock::acquire(path, max_wait).await?)),
        None => Ok(None),
    }
}

/// Steps 3b–7 of the cycle, run while the [`IsolatedSession`] guard is armed so a
/// failure here still hits the explicit teardown in [`refresh_cycle`]. Hard failures
/// (a locked keychain on seed, an FS error, a contended lock / failed write on the CAS
/// re-stash) return `Err`; soft failures (the spawn or read-back not producing a usable
/// item) return an `Error`-outcome [`RefreshReport`].
async fn run_isolated<S, K, R>(
    session: &IsolatedSession<K>,
    spawner: &R,
    stash: &S,
    stash_service: &str,
    seed: &[u8],
    snapshot: &StashedAccount,
    lock: Option<(&Path, Duration)>,
) -> Result<RefreshReport>
where
    S: AccountStash,
    K: IsolatedKeychain,
    R: ClaudeRefresh,
{
    // STEP 3b: seed the isolated item + a minimal .claude.json. Hard failures (a locked
    // keychain, an FS error) → Err; teardown still runs.
    session.seed(seed).await?;
    paths::write_private_file(&session.dir().join(".claude.json"), MINIMAL_CLAUDE_JSON)?;

    // STEP 4: spawn `claude -p`. A spawn failure (binary missing / un-spawnable) is a
    // soft, non-transient Error outcome — surface, do not retry as transient.
    if spawner.run(session.dir()).await.is_err() {
        return Ok(RefreshReport::indeterminate());
    }

    // STEP 5: read the (CC-refreshed) blob back. Any read-back failure is a soft Error
    // outcome (the spawn ran but produced no usable item); a transient locked keychain
    // is retried by the next periodic cycle.
    let after = match session.read_back().await {
        Ok(after) => after,
        Err(_) => return Ok(RefreshReport::indeterminate()),
    };

    // STEP 6: classify before/after (the secret RT/blob never escape `classify`).
    let (outcome, delta, rotated) = classify(snapshot.credential.expose(), seed, after.expose());

    // STEP 7 (lock held only here): CAS re-stash. ONLY on a fresh token, and ONLY if the
    // account's stored credential is unchanged since step 1 — else a concurrent swap /
    // login re-stashed it and is authoritative, so discard. Identity is preserved from
    // the snapshot; the real stash is the LAST write. (Dead/NoChange/Error ⇒ no write.)
    let re_stashed = if outcome == RefreshOutcome::Refreshed {
        let _lock = acquire_swap_lock(lock).await?;
        let current = stash.read(stash_service).await?;
        if current.credential.matches(&snapshot.credential) {
            stash
                .write(
                    stash_service,
                    &StashedAccount {
                        credential: after,
                        oauth_account: snapshot.oauth_account.clone(),
                    },
                )
                .await?;
            true
        } else {
            false
        }
    } else {
        false
    };

    Ok(RefreshReport {
        outcome,
        expires_at_delta_secs: delta,
        refresh_token_rotated: rotated,
        re_stashed,
    })
}

/// Run one isolated refresh cycle for the parked account stashed at `stash_service`,
/// using the already-constructed isolated `keychain` seam and `spawner`, with the
/// ephemeral isolated config dir at `iso_dir`. The shared async engine — generic over
/// its three seams so it is exercised hermetically with fakes.
///
/// `lock` is `Some((swap.lock, max_wait))` in production (serialized against the swap
/// engine) or `None` for hermetic single-process tests. `now_ms` is the injected clock
/// (epoch ms) used to back-date the seed.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) async fn refresh_cycle<S, K, R>(
    stash: &S,
    stash_service: &str,
    keychain: K,
    spawner: &R,
    iso_dir: PathBuf,
    lock: Option<(&Path, Duration)>,
    now_ms: i64,
) -> Result<RefreshReport>
where
    S: AccountStash,
    K: IsolatedKeychain,
    R: ClaudeRefresh,
{
    // STEP 1 (lock held only here): read the account's stash — its stored credential
    // (the CAS snapshot) and its identity (preserved across any re-stash). The lock is
    // released at the end of this block, BEFORE the spawn.
    let snapshot = {
        let _lock = acquire_swap_lock(lock).await?;
        stash.read(stash_service).await?
    };

    // STEP 3 (prep): back-date the stored credential's expiry to force CC's on-demand
    // refresh. Done BEFORE the STEP 2 dir creation (the out-of-order labels are
    // deliberate) so a malformed stored credential — which cannot be back-dated — bails
    // with an indeterminate outcome and NO filesystem work or spawn (retrying would not
    // fix a corrupt stash; surface it).
    let seed = match backdate(snapshot.credential.expose(), now_ms) {
        Some(seed) => seed,
        None => return Ok(RefreshReport::indeterminate()),
    };

    // STEP 2: create the ephemeral isolated dir (symlink-refused, 0700, owner-checked),
    // then ARM teardown over it + the isolated keychain seam.
    paths::create_isolated_dir(&iso_dir)?;
    let session = IsolatedSession::arm(keychain, iso_dir);

    // STEPS 3b–7, with teardown guaranteed afterwards regardless of outcome.
    let result = run_isolated(
        &session,
        spawner,
        stash,
        stash_service,
        &seed,
        &snapshot,
        lock,
    )
    .await;

    // STEP 8: teardown (delete the isolated item + remove the dir), ALWAYS.
    session.teardown().await;
    result
}

/// Run one isolated refresh for the parked account stashed at `stash_service`, whose
/// identity is `account_uuid` — the production entry point. Derives the isolated dir +
/// service from `account_uuid`, constructs the real isolated keychain + spawner, and
/// runs [`refresh_cycle`] under the swap lock.
///
/// Wired by the one-shot `poke` command (issue #104, the first of the engine's thin
/// callers); the periodic refresh tick (#105) is the second. As a real call root it
/// also keeps its production-only callees ([`now_ms`], [`SpawnClaude::new`],
/// [`crate::keychain::IsolatedKeychainItem::new`]) reachable.
pub(crate) async fn refresh_account<S: AccountStash>(
    stash: &S,
    stash_service: &str,
    account_uuid: &str,
    claude_binary: PathBuf,
) -> Result<RefreshReport> {
    let iso_dir = paths::isolated_refresh_dir(account_uuid)?;
    let keychain = crate::keychain::IsolatedKeychainItem::new(iso_dir.as_os_str())?;
    let spawner = SpawnClaude::new(claude_binary);
    let lock = paths::swap_lock()?;
    refresh_cycle(
        stash,
        stash_service,
        keychain,
        &spawner,
        iso_dir,
        Some((&lock, SWAP_LOCK_MAX_WAIT)),
        now_ms(),
    )
    .await
}

/// Read the stored credential's `expiresAt` (epoch milliseconds) for the account
/// stashed at `service`, or `None` if the stash is unreadable (locked keychain, absent
/// item) or the credential carries no parseable `expiresAt`.
///
/// **Non-secret**: returns only the integer expiry, never the token — so a caller can
/// select *near-expiry* parked accounts (the `poke` all-accounts mode, issue #104)
/// without ever handling raw credential bytes itself. The raw blob is read, parsed for
/// the one timestamp, and dropped here.
pub(crate) async fn stored_expires_at<S: AccountStash>(stash: &S, service: &str) -> Option<i64> {
    let snapshot = stash.read(service).await.ok()?;
    expires_at(snapshot.credential.expose())
}

/// Reap orphaned isolated-refresh artifacts left behind by a crashed cycle (issue
/// #103).
///
/// The engine's RAII guard ([`IsolatedSession`]) deletes the isolated keychain item +
/// dir both on the happy path ([`teardown`](IsolatedSession::teardown)) and on early
/// exit (`Drop` — a hard error, a panic, a timer-kill). What RAII CANNOT cover is a
/// `SIGKILL` / abort / power-loss: the process dies with no chance to run `Drop`,
/// stranding an isolated keychain item that still holds a live credential (and its
/// dir). At daemon `run` start the single-instance lock is held and no refresh cycle is
/// in flight, so any isolated artifact belonging to a roster account is — by
/// construction — such an orphan, safe to delete.
///
/// For each `account_uuid` the reap reconstructs the EXACT `(item, dir)` pair
/// [`refresh_account`] creates — an `IsolatedKeychainItem` over
/// [`paths::isolated_refresh_dir`] — reusing the #100/#102 derivation verbatim rather
/// than re-deriving a (possibly divergent) normalization, so it addresses precisely the
/// engine's own items. Because the only keychain service it ever names is that
/// roster-derived one, it can never touch another `CLAUDE_CONFIG_DIR` profile the user
/// runs (the issue's safety AC).
///
/// Best-effort, like teardown: a per-account failure (a momentarily-locked keychain, an
/// FS error) is logged and the sweep moves on — a reap failure must never block the
/// daemon from starting, and the orphan is retried on the next start.
pub(crate) async fn reap_orphans(account_uuids: &[String]) {
    for account_uuid in account_uuids {
        if let Err(err) = reap_orphan(account_uuid).await {
            eprintln!(
                "sessiometer: isolated-refresh orphan reap skipped for {account_uuid}: {err}"
            );
        }
    }
}

/// Reap one account's isolated-refresh orphan: delete its isolated keychain item and
/// remove its dir, reconstructing both exactly as [`refresh_account`] does so the
/// targets are byte-identical to the artifacts the engine creates.
async fn reap_orphan(account_uuid: &str) -> Result<()> {
    let iso_dir = paths::isolated_refresh_dir(account_uuid)?;
    let item = crate::keychain::IsolatedKeychainItem::new(iso_dir.as_os_str())?;
    reap_isolated(&item, &iso_dir).await
}

/// Delete an isolated keychain `item` and remove its `dir` (issue #103). Generic over
/// the [`IsolatedKeychain`] seam so the hermetic tests drive a fake and the macOS
/// real-CLI test drives a throwaway-keychain item, while production passes the real
/// item + dir from [`reap_orphan`].
///
/// Both deletes are idempotent — an already-absent item ([`delete`](IsolatedKeychain::delete)
/// maps `errSecItemNotFound` to `Ok`) and an already-absent dir are each success, so a
/// non-orphaned account is a clean no-op. Both are ATTEMPTED regardless of the other's
/// outcome — they are independent orphans, so a momentarily-locked keychain must not
/// strand the dir — and the first error (if any) is surfaced for the caller's log.
async fn reap_isolated<K: IsolatedKeychain>(item: &K, dir: &Path) -> Result<()> {
    let item_result = item.delete().await;
    let dir_result = remove_dir_if_present(dir);
    item_result.and(dir_result)
}

/// Remove `dir` and its contents, treating an already-absent dir as success — the FS
/// half of an idempotent isolated-orphan reap.
fn remove_dir_if_present(dir: &Path) -> Result<()> {
    match std::fs::remove_dir_all(dir) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(err.into()),
    }
}

/// Retries on the login-orphan reap's item delete — a momentarily-locked keychain may clear —
/// mirroring [`IsolatedSession`] teardown's retry policy: the reap is the crash-path counterpart of
/// that graceful teardown, so it honors the issue's "best-effort with retry" (and a persistent
/// failure is still retried on the next daemon / `login` start).
const LOGIN_REAP_RETRIES: u32 = 3;
/// Wait between login-orphan reap retries.
const LOGIN_REAP_RETRY_WAIT: Duration = Duration::from_millis(100);

/// Reap a crashed interactive-login's isolated orphan (issue #133) — the login counterpart of
/// [`reap_orphans`], folded in beside it so ALL startup reaping shares the one [`reap_isolated`]
/// mechanism (the issue's "do NOT fork a second reaper" + "folded into the daemon startup reaper").
///
/// The login-capture engine (#132) seeds a suffixed isolated keychain item under a single fixed
/// **0700** login dir ([`paths::isolated_login_dir`], `<support>/login`) and RAII-tears both down on
/// every graceful exit — but a SIGKILL / power-loss leaves that credential-bearing item + dir
/// stranded. This sweeps that one login isolation root: derive its #100-suffixed service EXACTLY as
/// the engine does ([`IsolatedKeychainItem::new`](crate::keychain::IsolatedKeychainItem::new)) and
/// reap the matching item + dir.
///
/// **Scan-based, NOT roster-derived**: a fresh login discovers its account only AFTER it completes,
/// so a crashed login's orphan is keyed by no roster uuid — the fixed login dir is the sole target
/// (unlike [`reap_orphan`]'s per-account family). Because the ONLY keychain service it ever names is
/// `service_for_config_dir(isolated_login_dir())`, it can NEVER touch a sibling `CLAUDE_CONFIG_DIR`
/// profile the operator legitimately runs (the issue's safety AC) — that profile path-hashes to a
/// different suffix. It never enumerates the keychain by a `Claude Code-credentials-*` prefix.
///
/// Best-effort with retry (a per-start failure is logged and retried on the next daemon / `login`
/// start); a genuinely-stranded orphan is logged when reaped, and the common clean-start case is
/// silent. Run at daemon start (in `cli`, beside [`reap_orphans`]) and at `login` start (in
/// [`crate::login`]).
pub(crate) async fn reap_login_orphan() {
    if let Err(err) = reap_login_orphan_inner().await {
        eprintln!("sessiometer: isolated-login orphan reap skipped: {err}");
    }
}

/// The fallible body of [`reap_login_orphan`], separated so the thin wrapper owns the single log
/// site (mirrors [`reap_orphans`] → [`reap_orphan`]). Reconstructs the isolated item under the fixed
/// login dir EXACTLY as the engine does, so the reaped item is byte-identical to the artifact a
/// crashed login left.
async fn reap_login_orphan_inner() -> Result<()> {
    let iso_dir = paths::isolated_login_dir()?;
    // A PRESENT login dir means a crashed login stranded an orphan (a graceful teardown / clean
    // shutdown removed it). Captured PRE-reap and used ONLY to keep the common clean-start case
    // silent — the reap is attempted regardless (hence a presence-probe hiccup degrades to `false`,
    // never skips the delete), so a dir-gone/item-stranded tail (a prior reap that removed the dir
    // but failed the item delete) is still cleaned.
    let stranded = iso_dir.try_exists().unwrap_or(false);
    let item = crate::keychain::IsolatedKeychainItem::new(iso_dir.as_os_str())?;
    reap_login_isolated(&item, &iso_dir).await?;
    if stranded {
        eprintln!(
            "sessiometer: reaped a stranded isolated-login orphan under {}",
            iso_dir.display()
        );
    }
    Ok(())
}

/// The retrying login-orphan reap core — generic over the [`IsolatedKeychain`] seam so the hermetic
/// tests drive fakes (a multi-item keychain for the sibling-untouched safety AC, a flaky one for the
/// retry) with zero real keychain. Retries the whole [`reap_isolated`] — both halves are idempotent,
/// so re-running after a partial success is safe — up to [`LOGIN_REAP_RETRIES`] attempts; the last
/// outcome is surfaced for the caller's log.
async fn reap_login_isolated<K: IsolatedKeychain>(item: &K, dir: &Path) -> Result<()> {
    let mut result = reap_isolated(item, dir).await;
    for _ in 1..LOGIN_REAP_RETRIES {
        if result.is_ok() {
            break;
        }
        tokio::time::sleep(LOGIN_REAP_RETRY_WAIT).await;
        result = reap_isolated(item, dir).await;
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::cell::RefCell;
    use std::rc::Rc;

    use crate::claude_state::OauthAccount;
    use crate::error::Error;
    use crate::keychain::Credential;
    use crate::stash::FakeAccountStash;
    // `IsolatedKeychain`, `StashedAccount`, `AccountStash`, `Value`, `Path`, `Duration`,
    // `SwapLock`, `SWAP_LOCK_MAX_WAIT` all arrive via `super::*` (a child module sees its
    // parent's `use`d names), so they are not re-imported. `Credential` is imported
    // explicitly above — the non-test parent no longer names it (its last user,
    // `IsolatedSession`, moved to `crate::isolated_spawn` in #131).

    const STASH: &str = "Sessiometer/u-1";
    // A fixed injected clock (epoch ms) so the back-dated seed is deterministic.
    const NOW_MS: i64 = 1_700_000_000_000;

    /// A Claude OAuth credential blob with a chosen expiry and refresh token.
    fn blob(expires_at_ms: i64, refresh_token: &str) -> Vec<u8> {
        format!(
            r#"{{"claudeAiOauth":{{"accessToken":"sk-ant-oat-TESTACCESS","refreshToken":"{refresh_token}","expiresAt":{expires_at_ms}}}}}"#
        )
        .into_bytes()
    }

    fn oauth(uuid: &str) -> OauthAccount {
        OauthAccount::from_object_bytes(
            format!(r#"{{"accountUuid":"{uuid}","emailAddress":"{uuid}@example.com"}}"#).as_bytes(),
        )
        .unwrap()
    }

    /// Rewrite a blob's `expiresAt` / `refreshToken` — the test helper the fake spawner
    /// uses to model CC's refresh-on-read (and to seed the stash).
    fn with_fields(
        base: &[u8],
        expires_at_ms: Option<i64>,
        refresh_token: Option<&str>,
    ) -> Vec<u8> {
        let mut v: Value = serde_json::from_slice(base).unwrap();
        let o = v["claudeAiOauth"].as_object_mut().unwrap();
        if let Some(e) = expires_at_ms {
            o.insert("expiresAt".into(), Value::from(e));
        }
        if let Some(rt) = refresh_token {
            o.insert("refreshToken".into(), Value::from(rt));
        }
        serde_json::to_vec(&v).unwrap()
    }

    /// What the fake `claude` does to the seeded isolated item on `run` — models CC.
    #[derive(Clone)]
    enum CcBehavior {
        /// A successful refresh: push `expiresAt` to a far-future value and set the RT
        /// (rotate when it differs from the seeded one).
        Refresh {
            new_expires_at_ms: i64,
            new_refresh_token: String,
        },
        /// A dead token: CC clears `refreshToken` in place.
        Dead,
        /// No refresh happened: the seeded item is left exactly as seeded.
        NoChange,
        /// The read-back is unparseable garbage.
        Garble,
        /// `claude` could not be spawned at all.
        SpawnFails,
    }

    /// In-memory isolated keychain item — the fake seam.
    #[derive(Clone)]
    struct FakeIsolatedKeychain {
        item: Rc<RefCell<Option<Vec<u8>>>>,
    }

    impl FakeIsolatedKeychain {
        fn empty() -> Self {
            Self {
                item: Rc::new(RefCell::new(None)),
            }
        }
    }

    impl IsolatedKeychain for FakeIsolatedKeychain {
        async fn seed(&self, blob: &[u8]) -> Result<()> {
            *self.item.borrow_mut() = Some(blob.to_vec());
            Ok(())
        }
        async fn read_back(&self) -> Result<Credential> {
            self.item
                .borrow()
                .clone()
                .map(Credential::new)
                .ok_or(Error::CredentialNotFound)
        }
        async fn delete(&self) -> Result<()> {
            *self.item.borrow_mut() = None;
            Ok(())
        }
        fn delete_blocking(&self) {
            *self.item.borrow_mut() = None;
        }
    }

    /// Fake spawner: models CC by mutating the shared isolated item per [`CcBehavior`].
    /// Can also record the swap-lock state DURING the spawn (to prove the lock is not held
    /// across it).
    struct FakeSpawn {
        item: Rc<RefCell<Option<Vec<u8>>>>,
        behavior: CcBehavior,
        // When set, the spawn tries a non-blocking acquire of this lock and records
        // whether it succeeded — i.e. whether the engine had released it.
        lock_probe: Option<PathBuf>,
        lock_free_during_spawn: Rc<RefCell<Option<bool>>>,
    }

    impl FakeSpawn {
        fn new(item: Rc<RefCell<Option<Vec<u8>>>>, behavior: CcBehavior) -> Self {
            Self {
                item,
                behavior,
                lock_probe: None,
                lock_free_during_spawn: Rc::new(RefCell::new(None)),
            }
        }
    }

    impl ClaudeRefresh for FakeSpawn {
        async fn run(&self, _config_dir: &Path) -> Result<()> {
            if let Some(lock) = &self.lock_probe {
                // A short bounded acquire: if the engine released the step-1 lock before
                // spawning, this succeeds quickly; if it (wrongly) held it across the
                // spawn, this would time out.
                let free = SwapLock::acquire(lock, Duration::from_millis(200))
                    .await
                    .is_ok();
                *self.lock_free_during_spawn.borrow_mut() = Some(free);
            }
            if let CcBehavior::SpawnFails = self.behavior {
                return Err(Error::Unimplemented("fake claude could not be spawned"));
            }
            let seeded = self.item.borrow().clone();
            if let Some(seeded) = seeded {
                let next = match &self.behavior {
                    CcBehavior::Refresh {
                        new_expires_at_ms,
                        new_refresh_token,
                    } => with_fields(&seeded, Some(*new_expires_at_ms), Some(new_refresh_token)),
                    CcBehavior::Dead => with_fields(&seeded, None, Some("")),
                    CcBehavior::NoChange => seeded.clone(),
                    CcBehavior::Garble => b"not json at all".to_vec(),
                    CcBehavior::SpawnFails => unreachable!(),
                };
                *self.item.borrow_mut() = Some(next);
            }
            Ok(())
        }
    }

    /// Seed a stash with one account's credential + identity.
    async fn seeded_stash(stored_blob: &[u8], uuid: &str) -> FakeAccountStash {
        let stash = FakeAccountStash::empty();
        stash
            .write(
                STASH,
                &StashedAccount {
                    credential: Credential::new(stored_blob.to_vec()),
                    oauth_account: oauth(uuid),
                },
            )
            .await
            .unwrap();
        stash
    }

    /// Run a cycle hermetically (no lock) with a fresh tempdir-based isolated dir.
    async fn run_cycle(
        stash: &FakeAccountStash,
        keychain: FakeIsolatedKeychain,
        spawner: &FakeSpawn,
    ) -> Result<RefreshReport> {
        let tmp = tempfile::tempdir().unwrap();
        let iso_dir = tmp.path().join("refresh/u-1");
        let report = refresh_cycle(
            stash,
            STASH,
            keychain,
            spawner,
            iso_dir.clone(),
            None,
            NOW_MS,
        )
        .await;
        // Teardown removed the dir on the happy path.
        assert!(!iso_dir.exists(), "the isolated dir must be torn down");
        report
    }

    // --- pure helpers ------------------------------------------------------------
    //
    // The spawn env scrub (`SPAWN_ENV_REMOVE`) and the `IsolatedSession` teardown moved to
    // `crate::isolated_spawn` (#131); their guards — the both-parametrizations scrub test and the
    // armed-Drop teardown test — live there now, alongside the shared seam they protect.

    #[test]
    fn backdate_sets_a_forcing_expiry_and_keeps_the_token() {
        // The seed must carry the back-dated expiry (== now), so CC's 5-min predicate
        // fires, while preserving the refresh token CC needs to perform the exchange.
        let original = blob(9_999_999_999_999, "sk-ant-ort-ORIGINAL");
        let seeded = backdate(&original, NOW_MS).unwrap();
        assert_eq!(expires_at(&seeded), Some(NOW_MS));
        assert_eq!(
            refresh_token(&seeded).unwrap().as_slice(),
            b"sk-ant-ort-ORIGINAL"
        );
    }

    #[test]
    fn backdate_rejects_a_malformed_blob() {
        assert!(backdate(b"not json", NOW_MS).is_none());
        assert!(backdate(br#"{"no":"oauth"}"#, NOW_MS).is_none());
    }

    #[test]
    fn refresh_token_distinguishes_empty_from_absent() {
        // The dead signal is Some(empty); a missing field is None — they must not be
        // conflated (one is DEAD, the other is unparseable/Error).
        assert!(refresh_token(&blob(NOW_MS, "")).unwrap().is_empty());
        assert_eq!(
            refresh_token(br#"{"claudeAiOauth":{"accessToken":"x"}}"#),
            None
        );
    }

    #[test]
    fn classify_names_each_outcome() {
        let original = blob(NOW_MS + 300_000, "sk-ant-ort-ORIG");
        let seeded = backdate(&original, NOW_MS).unwrap();
        // Refreshed: a non-empty RT and the expiry slid forward past the seed.
        let refreshed = blob(NOW_MS + 3_600_000, "sk-ant-ort-NEW");
        let (o, delta, rotated) = classify(&original, &seeded, &refreshed);
        assert_eq!(o, RefreshOutcome::Refreshed);
        assert!(delta.is_some());
        assert!(rotated);
        // Dead: the RT was cleared in place.
        let (o, _, _) = classify(&original, &seeded, &blob(NOW_MS, ""));
        assert_eq!(o, RefreshOutcome::Dead);
        // NoChange: a valid RT but the expiry did not move past the seed.
        let (o, delta, _) = classify(&original, &seeded, &seeded);
        assert_eq!(o, RefreshOutcome::NoChange);
        assert_eq!(delta, None, "no delta unless refreshed");
        // Error: the read-back is unparseable.
        let (o, _, _) = classify(&original, &seeded, b"garbage");
        assert_eq!(o, RefreshOutcome::Error);
    }

    #[test]
    fn classify_does_not_flag_rotation_when_the_token_is_unchanged() {
        let original = blob(NOW_MS + 300_000, "sk-ant-ort-SAME");
        let seeded = backdate(&original, NOW_MS).unwrap();
        // Same RT, slid expiry → refreshed but NOT rotated.
        let refreshed = blob(NOW_MS + 3_600_000, "sk-ant-ort-SAME");
        let (o, _, rotated) = classify(&original, &seeded, &refreshed);
        assert_eq!(o, RefreshOutcome::Refreshed);
        assert!(!rotated, "an unchanged refresh token is not a rotation");
    }

    // --- the engine, end to end (hermetic, fakes) --------------------------------

    #[tokio::test]
    async fn a_successful_refresh_re_stashes_the_fresh_token_and_preserves_identity() {
        let original = blob(NOW_MS + 300_000, "sk-ant-ort-ORIG");
        let stash = seeded_stash(&original, "u-1").await;
        let keychain = FakeIsolatedKeychain::empty();
        let spawner = FakeSpawn::new(
            keychain.item.clone(),
            CcBehavior::Refresh {
                new_expires_at_ms: NOW_MS + 7_200_000,
                new_refresh_token: "sk-ant-ort-ROTATED".to_owned(),
            },
        );

        let report = run_cycle(&stash, keychain, &spawner).await.unwrap();

        assert_eq!(report.outcome, RefreshOutcome::Refreshed);
        assert!(report.re_stashed);
        assert!(report.refresh_token_rotated);
        assert!(report.expires_at_delta_secs.unwrap() > 0);
        // The stash now holds the FRESH token, and the identity is preserved.
        let restashed = stash.read(STASH).await.unwrap();
        assert_eq!(
            refresh_token(restashed.credential.expose())
                .unwrap()
                .as_slice(),
            b"sk-ant-ort-ROTATED"
        );
        assert_eq!(restashed.oauth_account.account_uuid(), "u-1");
    }

    #[tokio::test]
    async fn a_dead_token_is_surfaced_and_never_re_stashed() {
        // The dead-account-no-restash invariant: CC cleared the RT; the stash is left
        // exactly as it was (the original, still-non-empty stored token) and surfaced.
        let original = blob(NOW_MS + 300_000, "sk-ant-ort-ORIG");
        let stash = seeded_stash(&original, "u-1").await;
        let keychain = FakeIsolatedKeychain::empty();
        let spawner = FakeSpawn::new(keychain.item.clone(), CcBehavior::Dead);

        let report = run_cycle(&stash, keychain, &spawner).await.unwrap();

        assert_eq!(report.outcome, RefreshOutcome::Dead);
        assert!(!report.re_stashed, "a dead account must NOT be re-stashed");
        // The stored credential is untouched — still the original token.
        let kept = stash.read(STASH).await.unwrap();
        assert_eq!(kept.credential.expose(), original.as_slice());
    }

    #[tokio::test]
    async fn no_change_and_spawn_failure_and_garble_do_not_re_stash() {
        let original = blob(NOW_MS + 300_000, "sk-ant-ort-ORIG");
        for behavior in [
            CcBehavior::NoChange,
            CcBehavior::SpawnFails,
            CcBehavior::Garble,
        ] {
            let stash = seeded_stash(&original, "u-1").await;
            let keychain = FakeIsolatedKeychain::empty();
            let spawner = FakeSpawn::new(keychain.item.clone(), behavior.clone());

            let report = run_cycle(&stash, keychain, &spawner).await.unwrap();

            assert!(!report.re_stashed);
            // The stored credential is untouched in every non-refresh outcome.
            let kept = stash.read(STASH).await.unwrap();
            assert_eq!(kept.credential.expose(), original.as_slice());
            // NoChange classifies as NoChange; the two failure modes classify as Error.
            match behavior {
                CcBehavior::NoChange => assert_eq!(report.outcome, RefreshOutcome::NoChange),
                _ => assert_eq!(report.outcome, RefreshOutcome::Error),
            }
        }
    }

    #[tokio::test]
    async fn a_malformed_stored_credential_is_an_error_with_no_spawn() {
        let stash = seeded_stash(b"not a credential blob", "u-1").await;
        let keychain = FakeIsolatedKeychain::empty();
        let spawner = FakeSpawn::new(
            keychain.item.clone(),
            CcBehavior::Refresh {
                new_expires_at_ms: NOW_MS + 7_200_000,
                new_refresh_token: "sk-ant-ort-NEW".to_owned(),
            },
        );

        let report = run_cycle(&stash, keychain.clone(), &spawner).await.unwrap();

        assert_eq!(report.outcome, RefreshOutcome::Error);
        assert!(!report.re_stashed);
        // The spawn never ran (nothing was seeded into the isolated item).
        assert!(keychain.item.borrow().is_none());
    }

    #[tokio::test]
    async fn a_concurrent_change_to_the_stored_credential_discards_the_refresh() {
        // CAS: the spawner mutates the STASH mid-cycle (modelling a concurrent swap /
        // login re-stashing the account) before the re-stash. The engine re-reads at
        // step 7, sees the stored credential changed since step 1, and DISCARDS its
        // refresh — the concurrent writer is authoritative.
        let original = blob(NOW_MS + 300_000, "sk-ant-ort-ORIG");
        let stash = Rc::new(seeded_stash(&original, "u-1").await);

        struct ConcurrentSwap {
            item: Rc<RefCell<Option<Vec<u8>>>>,
            stash: Rc<FakeAccountStash>,
        }
        impl ClaudeRefresh for ConcurrentSwap {
            async fn run(&self, _config_dir: &Path) -> Result<()> {
                // CC refreshes the isolated item…
                let seeded = self.item.borrow().clone().unwrap();
                *self.item.borrow_mut() = Some(with_fields(
                    &seeded,
                    Some(NOW_MS + 7_200_000),
                    Some("sk-ant-ort-FRESH"),
                ));
                // …but meanwhile a concurrent swap re-stashed the account with a
                // DIFFERENT token, so the CAS snapshot no longer matches.
                self.stash
                    .write(
                        STASH,
                        &StashedAccount {
                            credential: Credential::new(blob(
                                NOW_MS + 1_000_000,
                                "sk-ant-ort-CONCURRENT",
                            )),
                            oauth_account: oauth("u-1"),
                        },
                    )
                    .await
                    .unwrap();
                Ok(())
            }
        }

        let keychain = FakeIsolatedKeychain::empty();
        let spawner = ConcurrentSwap {
            item: keychain.item.clone(),
            stash: stash.clone(),
        };
        let tmp = tempfile::tempdir().unwrap();
        let iso_dir = tmp.path().join("refresh/u-1");
        let report = refresh_cycle(&*stash, STASH, keychain, &spawner, iso_dir, None, NOW_MS)
            .await
            .unwrap();

        // The cycle SAW a refresh, but the CAS guard discarded the re-stash…
        assert_eq!(report.outcome, RefreshOutcome::Refreshed);
        assert!(
            !report.re_stashed,
            "a concurrently-changed stash must win the CAS"
        );
        // …so the stash holds the CONCURRENT writer's token, not the engine's refresh.
        let stored = stash.read(STASH).await.unwrap();
        assert_eq!(
            refresh_token(stored.credential.expose())
                .unwrap()
                .as_slice(),
            b"sk-ant-ort-CONCURRENT"
        );
    }

    #[tokio::test]
    async fn the_swap_lock_is_released_before_the_spawn() {
        // The lock-only-around-steps-1&7 invariant: the engine must NOT hold the swap
        // lock across the spawn. The fake spawner probes the lock mid-spawn; it must be
        // free (the step-1 acquire was released before step 4).
        let original = blob(NOW_MS + 300_000, "sk-ant-ort-ORIG");
        let stash = seeded_stash(&original, "u-1").await;
        let keychain = FakeIsolatedKeychain::empty();
        let lock_dir = tempfile::tempdir().unwrap();
        let lock = lock_dir.path().join("swap.lock");

        let mut spawner = FakeSpawn::new(
            keychain.item.clone(),
            CcBehavior::Refresh {
                new_expires_at_ms: NOW_MS + 7_200_000,
                new_refresh_token: "sk-ant-ort-NEW".to_owned(),
            },
        );
        spawner.lock_probe = Some(lock.clone());
        let observed = spawner.lock_free_during_spawn.clone();

        let tmp = tempfile::tempdir().unwrap();
        let iso_dir = tmp.path().join("refresh/u-1");
        let report = refresh_cycle(
            &stash,
            STASH,
            keychain,
            &spawner,
            iso_dir,
            Some((lock.as_path(), SWAP_LOCK_MAX_WAIT)),
            NOW_MS,
        )
        .await
        .unwrap();

        assert_eq!(report.outcome, RefreshOutcome::Refreshed);
        assert_eq!(
            *observed.borrow(),
            Some(true),
            "the swap lock must be FREE during the spawn (held only around steps 1 & 7)"
        );
    }

    #[tokio::test]
    async fn a_hard_failure_still_tears_down_the_isolated_session() {
        // A seam that fails the seed (a hard error) — the cycle returns Err, and the
        // RAII guard must still have removed the isolated dir (teardown on the error
        // path). A failing-seed keychain also never leaves an item behind.
        struct FailingSeed {
            item: Rc<RefCell<Option<Vec<u8>>>>,
        }
        impl IsolatedKeychain for FailingSeed {
            async fn seed(&self, _blob: &[u8]) -> Result<()> {
                Err(Error::KeychainLocked { op: "seed" })
            }
            async fn read_back(&self) -> Result<Credential> {
                Err(Error::CredentialNotFound)
            }
            async fn delete(&self) -> Result<()> {
                *self.item.borrow_mut() = None;
                Ok(())
            }
            fn delete_blocking(&self) {
                *self.item.borrow_mut() = None;
            }
        }

        let original = blob(NOW_MS + 300_000, "sk-ant-ort-ORIG");
        let stash = seeded_stash(&original, "u-1").await;
        let keychain = FailingSeed {
            item: Rc::new(RefCell::new(None)),
        };
        let spawner = FakeSpawn::new(Rc::new(RefCell::new(None)), CcBehavior::NoChange);

        let tmp = tempfile::tempdir().unwrap();
        let iso_dir = tmp.path().join("refresh/u-1");
        let result = refresh_cycle(
            &stash,
            STASH,
            keychain,
            &spawner,
            iso_dir.clone(),
            None,
            NOW_MS,
        )
        .await;

        assert!(matches!(result, Err(Error::KeychainLocked { .. })));
        assert!(
            !iso_dir.exists(),
            "the isolated dir must be torn down even on a hard failure"
        );
        // The stash is untouched (no re-stash on a failed cycle).
        let kept = stash.read(STASH).await.unwrap();
        assert_eq!(kept.credential.expose(), original.as_slice());
    }

    // The armed-`Drop` teardown test (the RAII backstop for a session dropped on
    // panic / cancellation / timer-kill) moved with `IsolatedSession` to
    // `crate::isolated_spawn` (#131). The cycle's teardown-on-error is still covered here by
    // `a_hard_failure_still_tears_down_the_isolated_session`.

    // --- orphan reap (#103): SIGKILL / power-loss leaves no live isolated item ----
    //
    // Teardown (above) covers graceful exit; the reap covers the gap teardown CANNOT —
    // a crashed cycle whose `Drop` never ran. `reap_isolated` is the generic core,
    // driven by the fake seam here and the real `/usr/bin/security` CLI in `real_cli`.

    #[tokio::test]
    async fn reap_isolated_deletes_a_stranded_item_and_its_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let iso_dir = tmp.path().join("refresh/u-1");
        paths::create_isolated_dir(&iso_dir).unwrap();
        // An orphan: a secret-bearing isolated item + its dir, both present at start.
        let keychain = FakeIsolatedKeychain::empty();
        keychain.seed(b"secret-bearing-orphan").await.unwrap();

        reap_isolated(&keychain, &iso_dir).await.unwrap();

        assert!(
            keychain.item.borrow().is_none(),
            "the reap must delete the stranded isolated item"
        );
        assert!(
            !iso_dir.exists(),
            "the reap must remove the stranded isolated dir"
        );
    }

    #[tokio::test]
    async fn reap_isolated_is_an_idempotent_no_op_when_nothing_is_stranded() {
        // The common case: a clean prior shutdown left no orphan. An absent item
        // (the fake starts empty) and an absent dir (never created) are both success.
        let tmp = tempfile::tempdir().unwrap();
        let iso_dir = tmp.path().join("refresh/u-1");
        let keychain = FakeIsolatedKeychain::empty();

        reap_isolated(&keychain, &iso_dir).await.unwrap();

        assert!(keychain.item.borrow().is_none());
        assert!(!iso_dir.exists());
    }

    #[tokio::test]
    async fn reap_isolated_removes_the_dir_even_when_the_item_delete_fails() {
        // Independent orphans: a momentarily-locked keychain (delete → Err) must not
        // strand the dir. The reap attempts both and surfaces the delete error.
        struct LockedDelete;
        impl IsolatedKeychain for LockedDelete {
            async fn seed(&self, _blob: &[u8]) -> Result<()> {
                Err(Error::KeychainLocked { op: "seed" })
            }
            async fn read_back(&self) -> Result<Credential> {
                Err(Error::CredentialNotFound)
            }
            async fn delete(&self) -> Result<()> {
                Err(Error::KeychainLocked {
                    op: "isolated delete",
                })
            }
            fn delete_blocking(&self) {}
        }

        let tmp = tempfile::tempdir().unwrap();
        let iso_dir = tmp.path().join("refresh/u-1");
        paths::create_isolated_dir(&iso_dir).unwrap();

        let result = reap_isolated(&LockedDelete, &iso_dir).await;

        assert!(
            matches!(result, Err(Error::KeychainLocked { .. })),
            "a failed item delete must be surfaced"
        );
        assert!(
            !iso_dir.exists(),
            "the dir must be removed even when the item delete fails"
        );
    }

    #[test]
    fn the_reap_targets_only_roster_derived_isolated_services() {
        // Safety AC: the reap addresses ONLY the service derived from a roster account's
        // OWN isolated dir, so it can never touch another `CLAUDE_CONFIG_DIR` profile.
        // Each service is `service_for_config_dir(isolated_refresh_dir(uuid))` — the
        // SAME derivation `refresh_account` seeds under (no re-normalization).
        use crate::keychain::service_for_config_dir;

        let svc = |uuid: &str| {
            let dir = paths::isolated_refresh_dir(uuid).unwrap();
            service_for_config_dir(dir.as_os_str()).unwrap()
        };

        let a = svc("11111111-1111-1111-1111-111111111111");
        let b = svc("22222222-2222-2222-2222-222222222222");
        // Distinct accounts → distinct isolated items (no cross-account clobber).
        assert_ne!(a, b);
        // Every derived service is the suffixed isolated name, never the bare canonical.
        assert!(a.starts_with("Claude Code-credentials-"));
        assert_ne!(a, "Claude Code-credentials");
        // A foreign config dir the user might run CC under hashes to a DIFFERENT suffix,
        // so its item is never in the reap's roster-derived target set.
        let foreign =
            service_for_config_dir(std::ffi::OsStr::new("/Users/someone/.claude")).unwrap();
        assert_ne!(a, foreign);
        assert_ne!(b, foreign);
    }

    /// The reap end-to-end against the real `/usr/bin/security` CLI on a throwaway
    /// keychain (issue #103): seed an orphan exactly as the engine would, then prove
    /// `reap_isolated` deletes the real item and removes the dir. macOS-only — the CLI
    /// is the system under test (mirrors `keychain`'s isolated-item round-trip).
    #[cfg(target_os = "macos")]
    #[tokio::test]
    async fn reap_isolated_reaps_a_real_seeded_orphan() {
        use crate::keychain::IsolatedKeychainItem;
        use std::ffi::OsString;
        use std::process::Command as StdCommand;

        const SECURITY: &str = "/usr/bin/security";
        let tmp = tempfile::tempdir().unwrap();
        let kc = tmp.path().join("test.keychain-db");
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

        // The orphan dir + the isolated item the engine would create for it. The dir
        // path doubles as the config dir → the #100-suffixed service, exactly as
        // `IsolatedKeychainItem::new` derives in production.
        let iso_dir = tmp.path().join("refresh/u-1");
        paths::create_isolated_dir(&iso_dir).unwrap();
        let item = IsolatedKeychainItem::for_keychain(
            iso_dir.as_os_str(),
            OsString::from("reap-acct"),
            kc.clone(),
        )
        .unwrap();
        item.seed(br#"{"claudeAiOauth":{"refreshToken":"sk-ant-ort-ORPHAN"}}"#)
            .await
            .expect("seed the orphan item");
        // Sanity: the orphan really is present before the reap.
        item.read_back().await.expect("orphan present pre-reap");

        reap_isolated(&item, &iso_dir)
            .await
            .expect("reap the orphan");

        assert!(
            matches!(item.read_back().await, Err(Error::CredentialNotFound)),
            "the reap must delete the real isolated item"
        );
        assert!(
            !iso_dir.exists(),
            "the reap must remove the real isolated dir"
        );

        let _ = StdCommand::new(SECURITY)
            .arg("delete-keychain")
            .arg(&kc)
            .status();
    }

    // --- login orphan reap (#133): a crashed `claude /login` leaves the login isolation root ---
    //
    // The login counterpart of the roster reap above. The generic reap behavior (delete item +
    // remove dir, idempotency) is already covered by `reap_isolated_*`; these add the two things
    // the login path introduces — the negative SAFETY AC (a sibling `CLAUDE_CONFIG_DIR` profile is
    // NEVER swept) and the "best-effort with retry".

    #[test]
    fn the_login_reap_targets_only_the_login_isolation_root() {
        // Safety AC (#133): the login reap addresses ONLY the service derived from the fixed login
        // isolation dir, so it can never touch another `CLAUDE_CONFIG_DIR` profile the operator
        // runs. It NEVER enumerates the keychain by a `Claude Code-credentials-*` prefix — the sole
        // service it names is `service_for_config_dir(isolated_login_dir())`.
        use crate::keychain::service_for_config_dir;

        let login_svc =
            service_for_config_dir(paths::isolated_login_dir().unwrap().as_os_str()).unwrap();
        // The suffixed isolated name, never the bare canonical stem a prefix sweep keys off.
        assert!(login_svc.starts_with("Claude Code-credentials-"));
        assert_ne!(login_svc, "Claude Code-credentials");
        // A sibling profile the operator runs (a second real `~/.claude`) path-hashes to a
        // DIFFERENT suffix, so its item is never the reap's target — a prefix sweep WOULD match it.
        let sibling =
            service_for_config_dir(std::ffi::OsStr::new("/Users/someone/.claude")).unwrap();
        assert_ne!(login_svc, sibling);
        // Distinct from the refresh engine's isolated items too (no cross-engine clobber).
        let refresh_svc = service_for_config_dir(
            paths::isolated_refresh_dir("11111111-1111-1111-1111-111111111111")
                .unwrap()
                .as_os_str(),
        )
        .unwrap();
        assert_ne!(login_svc, refresh_svc);
    }

    /// A shared keychain modeled as a `service -> blob` map, plus per-service item VIEWS — the fake
    /// for the safety AC, where the keychain holds MORE than the one item being reaped. A view's
    /// `delete` removes ONLY its own service's entry, so reaping one view proves the reap never
    /// sweeps siblings (there is no keychain-wide enumeration seam to sweep them WITH).
    #[derive(Clone, Default)]
    struct FakeKeychainWorld {
        items: Rc<RefCell<std::collections::BTreeMap<String, Vec<u8>>>>,
    }

    impl FakeKeychainWorld {
        fn seed(&self, service: &str, blob: &[u8]) {
            self.items
                .borrow_mut()
                .insert(service.to_string(), blob.to_vec());
        }
        fn has(&self, service: &str) -> bool {
            self.items.borrow().contains_key(service)
        }
        fn view(&self, service: String) -> FakeKeychainView {
            FakeKeychainView {
                service,
                items: self.items.clone(),
            }
        }
    }

    /// A single-service view into a [`FakeKeychainWorld`], implementing the isolated-item seam.
    struct FakeKeychainView {
        service: String,
        items: Rc<RefCell<std::collections::BTreeMap<String, Vec<u8>>>>,
    }

    impl IsolatedKeychain for FakeKeychainView {
        async fn seed(&self, blob: &[u8]) -> Result<()> {
            self.items
                .borrow_mut()
                .insert(self.service.clone(), blob.to_vec());
            Ok(())
        }
        async fn read_back(&self) -> Result<Credential> {
            self.items
                .borrow()
                .get(&self.service)
                .cloned()
                .map(Credential::new)
                .ok_or(Error::CredentialNotFound)
        }
        async fn delete(&self) -> Result<()> {
            self.items.borrow_mut().remove(&self.service);
            Ok(())
        }
        fn delete_blocking(&self) {
            self.items.borrow_mut().remove(&self.service);
        }
    }

    #[tokio::test]
    async fn the_login_reap_leaves_a_sibling_config_dir_profile_untouched() {
        // Safety AC (#133) made behavioral: a keychain holds BOTH a crashed-login orphan AND a
        // sibling `CLAUDE_CONFIG_DIR` profile the operator legitimately runs. The reap — scoped to
        // the login dir's #100 service — deletes ONLY the login item; the sibling survives, because
        // the reap NEVER enumerates the keychain by a `Claude Code-credentials-*` prefix (which
        // WOULD have swept the sibling too).
        use crate::keychain::service_for_config_dir;

        let tmp = tempfile::tempdir().unwrap();
        let login_dir = tmp.path().join("login");
        paths::create_isolated_dir(&login_dir).unwrap();

        let login_svc = service_for_config_dir(login_dir.as_os_str()).unwrap();
        let sibling_svc =
            service_for_config_dir(std::ffi::OsStr::new("/Users/someone/.claude")).unwrap();
        // The two items really are distinct (else the fake wouldn't model the hazard).
        assert_ne!(login_svc, sibling_svc);

        let world = FakeKeychainWorld::default();
        world.seed(&login_svc, b"login-orphan-secret");
        world.seed(&sibling_svc, b"the-operators-own-profile");

        // Reap the LOGIN item's view — exactly the service the production reaper derives.
        reap_login_isolated(&world.view(login_svc.clone()), &login_dir)
            .await
            .unwrap();

        assert!(!world.has(&login_svc), "the login orphan must be reaped");
        assert!(
            world.has(&sibling_svc),
            "the sibling `CLAUDE_CONFIG_DIR` profile must be left untouched"
        );
        assert!(
            !login_dir.exists(),
            "the login isolation dir must be removed"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn the_login_reap_retries_a_transient_keychain_lock_then_succeeds() {
        // "Best-effort with retry" (#133): a momentarily-locked keychain (delete → Err on the first
        // attempt) must not strand the credential-bearing orphan — the reap retries and converges.
        // `start_paused` fast-forwards the inter-retry waits, so the test never sleeps in real time.
        struct FlakyDelete {
            fails_left: RefCell<u32>,
            deleted: RefCell<bool>,
        }
        impl IsolatedKeychain for FlakyDelete {
            async fn seed(&self, _blob: &[u8]) -> Result<()> {
                Ok(())
            }
            async fn read_back(&self) -> Result<Credential> {
                Err(Error::CredentialNotFound)
            }
            async fn delete(&self) -> Result<()> {
                let mut fails_left = self.fails_left.borrow_mut();
                if *fails_left > 0 {
                    *fails_left -= 1;
                    return Err(Error::KeychainLocked {
                        op: "isolated login delete",
                    });
                }
                *self.deleted.borrow_mut() = true;
                Ok(())
            }
            fn delete_blocking(&self) {
                *self.deleted.borrow_mut() = true;
            }
        }

        let tmp = tempfile::tempdir().unwrap();
        let iso_dir = tmp.path().join("login");
        paths::create_isolated_dir(&iso_dir).unwrap();
        let item = FlakyDelete {
            fails_left: RefCell::new(1),
            deleted: RefCell::new(false),
        };

        reap_login_isolated(&item, &iso_dir).await.unwrap();

        assert!(
            *item.deleted.borrow(),
            "the reap must retry past a transient lock and delete the item"
        );
        assert!(!iso_dir.exists(), "the reap must remove the isolated dir");
    }

    // --- redaction METER (#15): a cycle over a real secret leaks nothing ----------

    #[tokio::test]
    async fn a_cycle_over_a_secret_blob_leaks_no_secret_on_any_output_channel() {
        // Seed the stash with the redaction fixture's REAL secret blob (sk-ant- tokens
        // + a distinctive email), run a full refresh cycle, and prove the cycle's OUTPUT
        // channels carry none of it. This is the corpus's one real-secret cycle (the
        // spawned child's stdout/stderr are nulled at the source — see the spawn config
        // above — so they are not a channel); every channel a real cycle feeds is scanned
        // here (issue #106 deliverable 3: an unexercised channel is unmetered).
        let secrets = crate::redaction::meter::Secrets::meter_fixture();
        let stash = seeded_stash(secrets.blob(), "u-1").await;
        let keychain = FakeIsolatedKeychain::empty();
        // CC "refreshes" the fixture blob: slide the expiry and rotate to ANOTHER
        // secret token — both must stay out of every output.
        let spawner = FakeSpawn::new(
            keychain.item.clone(),
            CcBehavior::Refresh {
                new_expires_at_ms: NOW_MS + 7_200_000,
                new_refresh_token: "sk-ant-ort-ANOTHERSECRETROTATED0123456789".to_owned(),
            },
        );

        let report = run_cycle(&stash, keychain, &spawner).await.unwrap();
        assert_eq!(report.outcome, RefreshOutcome::Refreshed);

        // Channel 1 — the engine's in-process output: the RefreshReport's full Debug rendering.
        crate::redaction::meter::assert_clean(&format!("{report:?}"), &secrets);

        // Channel 2 — the durable per-cycle event line (issue #106): the tick builds an
        // `Event::Refresh` from THIS real-secret cycle's report and the daemon emits it to the
        // event log. Scan the EXACT production builder's rendered line — a hand-rolled replica
        // would silently miss a future secret-bearing field added to `refresh_event`. `before`
        // is a non-secret stored timestamp the daemon supplies; its value is immaterial here.
        let line = crate::refresh_tick::refresh_event("work", Some(NOW_MS), &report)
            .to_log_line(UNIX_EPOCH);
        assert!(
            line.contains("event=refresh"),
            "built the event line: {line}"
        );
        crate::redaction::meter::assert_clean(&line, &secrets);
    }
}
