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

use std::fs::OpenOptions;
use std::path::Path;
use std::time::{Duration, Instant};

use crate::claude_state;
use crate::error::{Error, Result};
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

    // Identity-consistency guard (issue #211): the step-2 re-stash below writes the
    // LIVE canonical token under the OUTGOING account's stash key + its PRESERVED
    // identity. Refuse — BEFORE any write (ZERO writes) — when the engine has POSITIVE
    // evidence the caller mis-resolved the outgoing account: the live canonical does
    // NOT match the outgoing account's own stashed token, yet DOES match the incoming
    // account's. Re-stashing then would staple the incoming account's credential onto
    // the outgoing stash — silent corruption (a stale `~/.claude.json` naming an
    // account that is no longer active: the failure mode #207 fixes at the caller;
    // this is the engine-level safety net UNDERNEATH it, also covering `--force` and
    // adopt-target callers). Mirrors the daemon's token-first ownership check +
    // "never staple a different account's identity" (`restash_account`, `src/daemon.rs`).
    //
    // Both halves are load-bearing:
    //   - The canonical MATCHING the outgoing stash is the safe no-drift case (and the
    //     self-swap / shared-token case) — never refuse it.
    //   - The canonical matching NEITHER stash is the legitimate in-place token-refresh
    //     DRIFT the re-stash exists to capture (the outgoing account's OWN freshly-
    //     refreshed token, stashed nowhere yet) — allow it, so a normal swap is
    //     unaffected. Only a canonical that is DEMONSTRABLY the incoming account's token
    //     (matches its stash) while NOT the outgoing's is a wrong-identity staple.
    if !outgoing_current.matches(&outgoing_prev.credential)
        && outgoing_current.matches(&incoming.credential)
    {
        return Err(Error::SwapWrongIdentityRestash);
    }

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

/// Run one out-of-band **adopt-target** recovery (issue #212): install `incoming`
/// as the active account by writing ONLY its credential to the canonical item and
/// co-writing its `oauthAccount` into `~/.claude.json` — the swap sequence's steps
/// 3–5, SKIPPING the outgoing read + re-stash (steps 1–2).
///
/// This is the recovery path for when the canonical credential is **gone or rotated**
/// — a forced Claude logout that scrubbed / rotated the keychain token (issue #209) —
/// so the normal [`swap`] cannot run: its step 1 reads the outgoing canonical and step
/// 2 re-stashes it, but there is no sound outgoing token to re-stash. Adopt-target needs
/// NEITHER: the departing (dead / absent) token is not required, and because it
/// re-stashes nothing, no credential can be stapled under a wrong identity — the #211
/// failure mode is structurally impossible here (nothing is written to any stash).
///
/// SAFETY is preserved, matching [`swap`]'s discipline:
///   - **read-everything-before-mutate** — the incoming account's stash (its token
///     → canonical, its `oauthAccount` → `~/.claude.json`) is read FIRST; an absent /
///     corrupt incoming stash aborts with ZERO writes;
///   - **could not read ≠ gone** — the canonical item is PROBED before any write, and
///     the write proceeds ONLY on positive evidence it is safe: a CONFIRMED-absent
///     canonical ([`Error::CredentialNotFound`], the scrubbed token) or a PRESENT-but-
///     readable one (`Ok`, a rotated / orphan token the caller vetted as un-attributable)
///     — the two states adopt-target recovers from. EVERY other read outcome aborts with
///     ZERO writes: a LOCKED keychain (transient — "locked ≠ gone", retry when unlocked),
///     but equally an ACL / auth-deny or other [`Error::Keychain`], ambiguity, or I/O
///     error, because a canonical we merely *could not read* is not proven *gone* —
///     clobbering it would lose a present token without re-stashing it. This mirrors the
///     normal [`swap`]'s step-1 read, which aborts on any error (`?`) identically.
///
/// Gated by `--force` at the sole caller ([`crate::use_account`]); the autonomous daemon
/// never adopts (it only rotates between known, re-stashed accounts). Steps 4–5 are
/// best-effort and reported in the returned [`SwapReport`], exactly as in [`swap`].
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) async fn adopt_target<C, S>(
    store: &C,
    stash: &S,
    incoming_stash: &str,
    claude_json: &Path,
) -> Result<SwapReport>
where
    C: CredentialStore,
    S: AccountStash,
{
    // Read the essential input FIRST — the incoming account's stash. An absent /
    // corrupt stash aborts before any mutation (ZERO writes), exactly as [`swap`]
    // reads all its inputs up front.
    let incoming = stash.read(incoming_stash).await?;

    // Probe the canonical before any write. Proceed ONLY on positive evidence it is safe
    // to overwrite: a CONFIRMED-absent canonical (the scrubbed token) or a PRESENT-but-
    // readable one (a rotated / orphan token the caller vetted as un-attributable) — the
    // two states adopt-target recovers from. EVERY other read outcome aborts with ZERO
    // writes, because a canonical we merely *could not read* is NOT proven *gone*:
    // clobbering it would lose a present token without re-stashing it (the #211 loss the
    // normal [`swap`] avoids by aborting its step-1 read on any error).
    match store.read().await {
        // Present + readable: the caller routes here only when this token matches no
        // known account, so overwriting the orphan is the intended rotation recovery.
        Ok(_) => {}
        // Confirmed absent (errSecItemNotFound): nothing to clobber — the recovery case.
        Err(Error::CredentialNotFound) => {}
        // LOCKED (transient — "locked ≠ gone"), or an ACL / auth-deny or other
        // `security` error, ambiguity, or I/O: "could not read" is not "gone". Abort.
        Err(err) => return Err(err),
    }

    // 3. Write the incoming account's token to the canonical item (atomic `-U`: a
    //    reader sees old-or-new, never empty / torn).
    store.write(&incoming.credential).await?;

    // 4. Co-write the incoming account's `oauthAccount` into `~/.claude.json`
    //    (best-effort display correctness — a failure is tolerated and self-heals).
    let oauth_cowritten =
        claude_state::write_oauth_account(claude_json, &incoming.oauth_account).is_ok();

    // 5. Post-write re-read to confirm the canonical still holds the token we wrote
    //    (re-read each cycle, never cache). A read failure or a third-writer change
    //    leaves it unconfirmed; the token reroute already succeeded.
    let canonical_confirmed = store
        .read()
        .await
        .is_ok_and(|current| current.matches(&incoming.credential));

    Ok(SwapReport {
        canonical_confirmed,
        oauth_cowritten,
    })
}

/// How long a contended swap-lock acquire ([`SwapLock::acquire`]) waits before
/// failing closed (issue #64). Comfortably exceeds one swap's keychain work (a
/// handful of `security` subprocesses, sub-second to ~2 s), so the ordinary
/// contention — the OTHER writer simply mid-swap — resolves with margin; only a
/// genuinely wedged holder reaches the ceiling, where failing closed (ZERO writes)
/// beats blocking forever.
pub(crate) const SWAP_LOCK_MAX_WAIT: Duration = Duration::from_secs(10);

/// Poll interval while waiting on a contended swap lock (issue #64). Short enough
/// that the wait ends within ~one interval of the holder releasing, small enough
/// that the few polls during a typical sub-second swap are negligible.
const SWAP_LOCK_RETRY: Duration = Duration::from_millis(50);

/// A held single-WRITER swap lock: a kernel advisory `flock(LOCK_EX)` on the
/// native-local `swap.lock`, held only for the DURATION of one swap (issue #64).
/// The file is held open for the critical section; the kernel releases the lock on
/// drop (or process death), so there is no stale-lock reaping.
///
/// DISTINCT from the daemon's single-INSTANCE lock ([`crate::daemon::InstanceLock`]),
/// which is held NON-blocking for the whole process lifetime to reject a second
/// `run`. This lock is BLOCKING (bounded) and per-swap: both the manual `use` swap
/// and the daemon's own swap routine acquire it, collapsing their two-step swaps
/// (canonical keychain write → `~/.claude.json` co-write) into mutually-exclusive
/// critical sections so the two writers can never interleave into a split state
/// (canonical = one account while `~/.claude.json` = another).
#[derive(Debug)]
pub(crate) struct SwapLock {
    // Held open purely to keep the lock; dropping it (or the process dying)
    // releases it.
    _file: std::fs::File,
}

impl SwapLock {
    /// Acquire the swap lock at `path` (creating the file `0600` if needed),
    /// bounded-blocking up to `max_wait`.
    ///
    /// FAIL-CLOSED: if the lock cannot be taken within `max_wait` — another swap
    /// held it the whole time — returns [`Error::SwapLockBusy`] so the caller
    /// aborts with ZERO writes, rather than writing without it and reopening the
    /// torn-write race. Polls `flock(LOCK_EX|LOCK_NB)` and yields the runtime
    /// between tries (an async sleep, never a busy-spin or a blocked OS thread), so
    /// the current-thread runtime keeps cooperating while it waits — the daemon
    /// stays responsive, and `use` stays interruptible.
    pub(crate) async fn acquire(path: &Path, max_wait: Duration) -> Result<Self> {
        use std::os::unix::fs::OpenOptionsExt;
        use std::os::unix::io::AsRawFd;

        let file = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .mode(0o600)
            .open(path)?;
        let deadline = Instant::now() + max_wait;
        loop {
            // Raw `flock` FFI, kept un-wrapped by ADR-0004: kept raw rather than
            // adding a `rustix` production dependency; the std wheel
            // (`File::try_lock`, stable 1.89) is the planned replacement once MSRV
            // reaches 1.89 (see #257).
            // SAFETY: `flock` takes a valid open fd (owned by `file`, which outlives
            // the call) and the two flag constants; it has no other preconditions.
            let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
            if rc == 0 {
                return Ok(Self { _file: file });
            }
            let err = std::io::Error::last_os_error();
            // EWOULDBLOCK (== EAGAIN) means another swap holds the lock; anything
            // else is a genuine I/O failure (a broken fd / filesystem), surfaced
            // as itself rather than masqueraded as contention.
            if err.raw_os_error() != Some(libc::EWOULDBLOCK) {
                return Err(Error::Io(err));
            }
            // Out of patience: fail closed (the caller aborts with ZERO writes).
            if Instant::now() >= deadline {
                return Err(Error::SwapLockBusy);
            }
            tokio::time::sleep(SWAP_LOCK_RETRY).await;
        }
    }
}

/// Run one out-of-band [`swap`], wrapped in the single-writer swap lock (issue
/// #64) when `lock` is `Some((path, max_wait))`.
///
/// The lock is acquired BEFORE the swap reads any input and held across the WHOLE
/// two-step sequence, so the manual `use` writer and the daemon's swap routine —
/// the two real swap writers — are serialized on one keychain item and can never
/// interleave into a split canonical/`~/.claude.json` pair. Whoever waits proceeds
/// on FRESH state once the holder releases. A `lock` of `None` runs the swap
/// unlocked: the hermetic single-process test path, where there is no second
/// writer to serialize against.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) async fn swap_locked<C, S>(
    lock: Option<(&Path, Duration)>,
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
    // Bind the guard here so it outlives the entire swap and drops only on return.
    // A contended acquire fails closed (`Err`) BEFORE any swap input is read, so a
    // refusal is a true no-op (ZERO writes), exactly like the swap engine's own
    // read-everything-before-mutating discipline.
    let _guard = match lock {
        Some((path, max_wait)) => Some(SwapLock::acquire(path, max_wait).await?),
        None => None,
    };
    swap(store, stash, outgoing_stash, incoming_stash, claude_json).await
}

/// Run one out-of-band [`adopt_target`] recovery (issue #212), wrapped in the
/// single-writer swap lock (issue #64) when `lock` is `Some((path, max_wait))` —
/// the locked counterpart of [`swap_locked`], for the adopt path.
///
/// The lock is acquired BEFORE the adopt reads any input and held across the whole
/// write, so the manual `use` recovery cannot interleave with a concurrent daemon
/// swap into a split canonical / `~/.claude.json` pair. A contended acquire fails
/// closed (`Err`) BEFORE any input is read, so a refusal is a true no-op (ZERO
/// writes), matching [`swap_locked`]. A `lock` of `None` runs unlocked — the
/// hermetic single-process test path.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) async fn adopt_target_locked<C, S>(
    lock: Option<(&Path, Duration)>,
    store: &C,
    stash: &S,
    incoming_stash: &str,
    claude_json: &Path,
) -> Result<SwapReport>
where
    C: CredentialStore,
    S: AccountStash,
{
    // Bind the guard here so it outlives the entire adopt and drops only on return.
    // A contended acquire fails closed (`Err`) BEFORE any input is read, so a refusal
    // is a true no-op (ZERO writes), exactly like [`swap_locked`].
    let _guard = match lock {
        Some((path, max_wait)) => Some(SwapLock::acquire(path, max_wait).await?),
        None => None,
    };
    adopt_target(store, stash, incoming_stash, claude_json).await
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::cell::{Cell, RefCell};
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
            session_resets_at: None,
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
            session_resets_at: None,
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
            session_resets_at: None,
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
            session_resets_at: None,
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

    #[tokio::test]
    async fn a_stale_claude_json_cannot_corrupt_another_accounts_stash() {
        // Issue #211 (AC2): the wrong-identity re-stash guard. `~/.claude.json` is
        // stale (shows A) while the LIVE canonical is actually B's token, so a caller
        // (`use`) mis-resolves outgoing = A and asks to swap A → B. The live canonical
        // (B's token) belongs to the INCOMING account, so re-stashing it under A's key
        // would corrupt A's stash. The engine must REFUSE with ZERO writes.
        let store = store_holding(b"B-token").await; // canonical is really B's
        let stash = stash_with(stashed(b"A-token", "u-A"), stashed(b"B-token", "u-B")).await;
        let (_dir, json) = claude_json("u-A", 0o600); // display lies: shows A

        let result = swap(&store, &stash, ACCT_A, ACCT_B, &json).await;

        // AC1: refused on the token↔key mismatch — no wrong-identity staple.
        assert!(
            matches!(result, Err(Error::SwapWrongIdentityRestash)),
            "the swap must refuse a wrong-identity re-stash"
        );
        // AC2: A's stash is UNCORRUPTED — still A's own token + identity, never B's.
        let a = stash.read(ACCT_A).await.unwrap();
        assert_eq!(
            a.credential.expose(),
            b"A-token",
            "A's stash must be untouched"
        );
        assert_eq!(a.oauth_account.account_uuid(), "u-A");
        // AC3: ZERO writes — the canonical item and B's stash are both untouched.
        assert!(
            store.read().await.unwrap().matches(&cred(b"B-token")),
            "the canonical item must be untouched (ZERO writes)"
        );
        let b = stash.read(ACCT_B).await.unwrap();
        assert_eq!(
            b.credential.expose(),
            b"B-token",
            "B's stash must be untouched"
        );
    }

    // --- adopt-target recovery (#212) --------------------------------------

    #[tokio::test]
    async fn adopt_target_installs_the_target_when_the_canonical_is_absent() {
        // AC #1 (engine half): the canonical is GONE (scrubbed) — `store.read()` is
        // `CredentialNotFound`. Adopt-target installs B's token → canonical and
        // co-writes B's identity, WITHOUT reading or re-stashing any outgoing account.
        let store = FakeCredentialStore::empty();
        store.set_not_found(true); // the scrubbed / absent canonical
        let stash = stash_with(stashed(b"A-token", "u-A"), stashed(b"B-token", "u-B")).await;
        let (_dir, json) = claude_json("u-STALE", 0o600); // display is stale/cleared

        let report = adopt_target(&store, &stash, ACCT_B, &json).await.unwrap();

        // The canonical now holds B's token (the write created the absent item), and
        // the post-write re-read confirmed it.
        assert!(store.read().await.unwrap().matches(&cred(b"B-token")));
        assert!(report.canonical_confirmed);
        // The display identity was co-written to B.
        assert!(report.oauth_cowritten);
        assert_eq!(displayed_uuid(&json), "u-B");
        // AC #3: NOTHING was re-stashed — A's stash is byte-for-byte untouched, so no
        // credential could be stapled under a wrong identity (the departing token was
        // never required).
        let a = stash.read(ACCT_A).await.unwrap();
        assert_eq!(
            a.credential.expose(),
            b"A-token",
            "A's stash must be untouched"
        );
        assert_eq!(a.oauth_account.account_uuid(), "u-A");
    }

    #[tokio::test]
    async fn adopt_target_installs_the_target_when_the_canonical_is_rotated() {
        // AC #1 (engine half), rotated variant: the canonical holds a ROTATED token that
        // matches no stash (a forced logout replaced it). Adopt-target overwrites it with
        // B's token regardless — it does not re-stash the orphan under any identity.
        let store = store_holding(b"ORPHAN-rotated-token").await;
        let stash = stash_with(stashed(b"A-token", "u-A"), stashed(b"B-token", "u-B")).await;
        let (_dir, json) = claude_json("u-STALE", 0o600);

        let report = adopt_target(&store, &stash, ACCT_B, &json).await.unwrap();

        assert!(store.read().await.unwrap().matches(&cred(b"B-token")));
        assert!(report.canonical_confirmed);
        assert!(report.oauth_cowritten);
        assert_eq!(displayed_uuid(&json), "u-B");
        // The rotated orphan was NOT stashed anywhere — A's stash is untouched.
        let a = stash.read(ACCT_A).await.unwrap();
        assert_eq!(
            a.credential.expose(),
            b"A-token",
            "A's stash must be untouched"
        );
    }

    #[tokio::test]
    async fn adopt_target_aborts_with_zero_writes_when_the_keychain_is_locked() {
        // AC #2: "locked ≠ gone." A LOCKED keychain is transient (retry when unlocked),
        // NOT a scrubbed credential — so adopt-target must ABORT with ZERO writes rather
        // than clobber a canonical it cannot even read. The probe catches the lock before
        // any write.
        let store = store_holding(b"A-token").await;
        store.set_locked(true);
        let stash = stash_with(stashed(b"A-token", "u-A"), stashed(b"B-token", "u-B")).await;
        let (_dir, json) = claude_json("u-A", 0o600);

        let result = adopt_target(&store, &stash, ACCT_B, &json).await;

        assert!(
            matches!(result, Err(Error::KeychainLocked { .. })),
            "a locked keychain must abort the adopt (locked ≠ gone)"
        );
        // ZERO writes: unlock and confirm the canonical is untouched, the display is
        // untouched, and NOTHING was stashed.
        store.set_locked(false);
        assert!(
            store.read().await.unwrap().matches(&cred(b"A-token")),
            "the canonical must be untouched (ZERO writes)"
        );
        assert_eq!(
            displayed_uuid(&json),
            "u-A",
            "the display must be untouched"
        );
        let a = stash.read(ACCT_A).await.unwrap();
        assert_eq!(a.credential.expose(), b"A-token");
    }

    #[tokio::test]
    async fn adopt_target_aborts_with_zero_writes_when_the_canonical_is_present_but_unreadable() {
        // "Could not read ≠ gone." A canonical that is PRESENT but whose secret cannot be
        // read (a non-lock, non-not-found `security` error — an ACL / auth-deny) is NOT a
        // scrubbed credential: adopt-target must ABORT with ZERO writes rather than clobber
        // a present token without re-stashing it. The probe treats this exactly as a lock
        // (only a CONFIRMED-absent or readable canonical proceeds), matching the normal
        // swap's step-1 read, which `?`-aborts on any error.
        let store = store_holding(b"A-token").await;
        store.set_unreadable(true);
        let stash = stash_with(stashed(b"A-token", "u-A"), stashed(b"B-token", "u-B")).await;
        let (_dir, json) = claude_json("u-A", 0o600);

        let result = adopt_target(&store, &stash, ACCT_B, &json).await;

        assert!(
            matches!(result, Err(Error::Keychain { .. })),
            "a present-but-unreadable canonical must abort the adopt (could not read ≠ gone), got {result:?}"
        );
        // ZERO writes: clear the read fault and confirm the canonical is untouched, the
        // display is untouched, and NOTHING was stashed.
        store.set_unreadable(false);
        assert!(
            store.read().await.unwrap().matches(&cred(b"A-token")),
            "the canonical must be untouched (ZERO writes)"
        );
        assert_eq!(
            displayed_uuid(&json),
            "u-A",
            "the display must be untouched"
        );
        let a = stash.read(ACCT_A).await.unwrap();
        assert_eq!(a.credential.expose(), b"A-token");
    }

    #[tokio::test]
    async fn adopt_target_aborts_before_any_write_when_the_incoming_stash_is_absent() {
        // Read-everything-before-mutate: the incoming stash is the essential input; an
        // absent one aborts as a true no-op (ZERO writes) — the canonical stays absent.
        let store = FakeCredentialStore::empty();
        store.set_not_found(true);
        let stash = FakeAccountStash::empty(); // B is NOT stashed
        stash
            .write(ACCT_A, &stashed(b"A-token", "u-A"))
            .await
            .unwrap();
        let (_dir, json) = claude_json("u-A", 0o600);

        let result = adopt_target(&store, &stash, ACCT_B, &json).await;

        assert!(matches!(result, Err(Error::StashIncomplete { .. })));
        // ZERO writes: the canonical is still absent (never created).
        assert!(matches!(store.read().await, Err(Error::CredentialNotFound)));
        // The display is untouched.
        assert_eq!(displayed_uuid(&json), "u-A");
    }

    #[tokio::test]
    async fn adopt_target_locked_installs_the_target_through_the_lock() {
        // The lock-wrapped adopt (the production path): an uncontended lock acquires
        // instantly and the adopt runs, proving `adopt_target_locked` drives the same
        // recovery as the bare engine through the single-writer lock (#64).
        let dir = tempfile::tempdir().unwrap();
        let lock = dir.path().join("swap.lock");
        let store = FakeCredentialStore::empty();
        store.set_not_found(true);
        let stash = stash_with(stashed(b"A-token", "u-A"), stashed(b"B-token", "u-B")).await;
        let (_jdir, json) = claude_json("u-STALE", 0o600);

        let report = adopt_target_locked(
            Some((lock.as_path(), SWAP_LOCK_MAX_WAIT)),
            &store,
            &stash,
            ACCT_B,
            &json,
        )
        .await
        .unwrap();

        assert!(store.read().await.unwrap().matches(&cred(b"B-token")));
        assert!(report.canonical_confirmed);
        assert_eq!(displayed_uuid(&json), "u-B");
    }

    // --- the single-writer swap lock (#64) ---------------------------------

    #[tokio::test]
    async fn the_swap_lock_serializes_two_writers_with_no_overlap() {
        // The lock's core property (issue #64 acceptance): two writers contending on
        // one lock never occupy the critical section at once — the second BLOCKS
        // until the first releases. Each worker, while holding the lock, marks the
        // section occupied and yields TWICE, so the other worker is polled and WOULD
        // observe an overlap if the lock did not serialize them.
        let dir = tempfile::tempdir().unwrap();
        let lock = dir.path().join("swap.lock");
        let occupancy = Rc::new(Cell::new(0u32));
        let max_seen = Rc::new(Cell::new(0u32));

        let worker = |occupancy: Rc<Cell<u32>>, max_seen: Rc<Cell<u32>>, lock: PathBuf| async move {
            let _guard = SwapLock::acquire(&lock, SWAP_LOCK_MAX_WAIT).await.unwrap();
            let now = occupancy.get() + 1;
            occupancy.set(now);
            max_seen.set(max_seen.get().max(now));
            tokio::task::yield_now().await;
            tokio::task::yield_now().await;
            occupancy.set(occupancy.get() - 1);
        };

        tokio::join!(
            worker(occupancy.clone(), max_seen.clone(), lock.clone()),
            worker(occupancy.clone(), max_seen.clone(), lock.clone()),
        );

        assert_eq!(
            max_seen.get(),
            1,
            "the swap lock must serialize writers — the second blocks until the first releases"
        );
    }

    #[tokio::test]
    async fn the_swap_lock_fails_closed_while_held_then_recovers_on_release() {
        // FAIL-CLOSED (the boundary-conformance refinement): a contended acquire that
        // exhausts its bounded wait returns `SwapLockBusy` (the caller then aborts
        // with ZERO writes) rather than proceeding without the lock. Once the holder
        // releases, a fresh acquire succeeds — the lock is per-swap, not sticky.
        let dir = tempfile::tempdir().unwrap();
        let lock = dir.path().join("swap.lock");

        let held = SwapLock::acquire(&lock, SWAP_LOCK_MAX_WAIT).await.unwrap();
        // A second, SEPARATE open of the same file contends even within one process
        // (flock locks the open file description) — so the bounded wait elapses.
        let busy = SwapLock::acquire(&lock, Duration::from_millis(120))
            .await
            .unwrap_err();
        assert!(
            matches!(busy, Error::SwapLockBusy),
            "a held lock must fail closed, got {busy:?}"
        );
        assert_eq!(
            busy.exit_code(),
            4,
            "fail-closed shares the locked-keychain code"
        );

        drop(held);
        // Released → the next swap acquires it (no stale-lock reaping needed).
        let _recovered = SwapLock::acquire(&lock, Duration::from_millis(500))
            .await
            .expect("the lock is free once the holder drops");
    }

    #[tokio::test]
    async fn two_real_swap_writers_on_one_item_never_leave_a_split_pair() {
        // The acceptance integration: two REAL swap engines (steps 1–5) contend on
        // one keychain item + one `~/.claude.json`, serialized only by the lock. The
        // shared store YIELDS inside its canonical write, widening the exact window a
        // split would open (canonical written by one writer, json co-written by the
        // other). With the lock, the writers serialize, so the final pair is
        // CONSISTENT — canonical token and displayed identity name the SAME account —
        // and reflects the writer that ran last (fresh state), never a torn mix.
        type Slot = Rc<RefCell<Option<Credential>>>;

        struct YieldingStore {
            slot: Slot,
        }
        impl CredentialStore for YieldingStore {
            async fn read(&self) -> Result<Credential> {
                self.slot.borrow().clone().ok_or(Error::CredentialNotFound)
            }
            async fn write(&self, credential: &Credential) -> Result<()> {
                // Yield mid-write: without the lock the OTHER swap would interleave
                // here, between this canonical write and its own json co-write.
                tokio::task::yield_now().await;
                *self.slot.borrow_mut() = Some(credential.clone());
                tokio::task::yield_now().await;
                Ok(())
            }
        }

        let dir = tempfile::tempdir().unwrap();
        let lock = dir.path().join("swap.lock");
        let (_jdir, json) = claude_json("u-O", 0o600);

        // One shared canonical item, seeded with the origin account O.
        let slot: Slot = Rc::new(RefCell::new(Some(cred(b"O-token"))));
        let store_x = YieldingStore { slot: slot.clone() };
        let store_y = YieldingStore { slot: slot.clone() };
        // Two stashes per writer: the shared origin (outgoing) and that writer's
        // distinct incoming target. Distinct stash instances stand in for the one
        // keychain — both writers re-stash O and write the canonical, the contended
        // surface the lock protects.
        let stash_x = stash_with(stashed(b"O-token", "u-O"), stashed(b"X-token", "u-X")).await;
        let stash_y = stash_with(stashed(b"O-token", "u-O"), stashed(b"Y-token", "u-Y")).await;

        let lw = (lock.as_path(), SWAP_LOCK_MAX_WAIT);
        let (rx, ry) = tokio::join!(
            swap_locked(Some(lw), &store_x, &stash_x, ACCT_A, ACCT_B, &json),
            swap_locked(Some(lw), &store_y, &stash_y, ACCT_A, ACCT_B, &json),
        );
        rx.unwrap();
        ry.unwrap();

        // The final pair is CONSISTENT — not a split. The canonical token and the
        // displayed identity name the SAME account (both X or both Y), proving no
        // interleave left canonical from one writer beside json from the other.
        let canonical = slot.borrow().clone().unwrap();
        let displayed = displayed_uuid(&json);
        let consistent = (canonical.matches(&cred(b"X-token")) && displayed == "u-X")
            || (canonical.matches(&cred(b"Y-token")) && displayed == "u-Y");
        assert!(
            consistent,
            "split write: canonical and ~/.claude.json disagree (displayed={displayed})"
        );
    }

    #[tokio::test]
    async fn the_fallback_adopt_fails_closed_while_a_daemon_write_holds_the_lock() {
        // Issue #167 / #212 fallback safety: when the daemon is UP, `use <spare> --force` adopt-
        // recovery falls back to the STANDALONE adopt (`adopt_target_locked`), which takes the SAME
        // cross-process swap lock (`paths::swap_lock`) that EVERY daemon canonical write holds — the
        // auto / emergency / socket swaps via `swap_locked`, and the #282 `promote_canonical`. So
        // while a daemon canonical write is IN FLIGHT (lock held), the fallback adopt CANNOT also
        // write: a bounded acquire fails CLOSED (`SwapLockBusy`) with ZERO writes — exactly one
        // writer ever touches the canonical, never a double / torn write. Once the daemon releases,
        // the fallback acquires and installs the target in one clean write. (The daemon holds the
        // full `SWAP_LOCK_MAX_WAIT`; the fallback is given a short bounded wait so the contention
        // resolves fast — the same technique the sibling fail-closed test uses.)
        let dir = tempfile::tempdir().unwrap();
        let lock = dir.path().join("swap.lock");
        let (_jdir, json) = claude_json("u-O", 0o600);
        let store = store_holding(b"O-token").await;
        let stash = stash_with(stashed(b"O-token", "u-O"), stashed(b"R-token", "u-R")).await;

        // A daemon canonical write is in flight: it holds the single-writer swap lock.
        let held = SwapLock::acquire(&lock, SWAP_LOCK_MAX_WAIT).await.unwrap();

        // The fallback adopt, given a bounded wait, fails CLOSED — no second writer, ZERO writes.
        let busy = adopt_target_locked(
            Some((lock.as_path(), Duration::from_millis(120))),
            &store,
            &stash,
            ACCT_B,
            &json,
        )
        .await
        .unwrap_err();
        assert!(
            matches!(busy, Error::SwapLockBusy),
            "the fallback adopt must fail closed while the daemon holds the lock, got {busy:?}",
        );
        // ZERO writes: the canonical still holds O and the display still shows O — no double write.
        assert!(store.read().await.unwrap().matches(&cred(b"O-token")));
        assert_eq!(displayed_uuid(&json), "u-O");

        // Once the daemon releases, the fallback adopt acquires and installs R — one clean write.
        drop(held);
        adopt_target_locked(
            Some((lock.as_path(), Duration::from_millis(500))),
            &store,
            &stash,
            ACCT_B,
            &json,
        )
        .await
        .expect("the fallback adopt acquires once the daemon releases the lock");
        assert!(store.read().await.unwrap().matches(&cred(b"R-token")));
        assert_eq!(displayed_uuid(&json), "u-R");
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

        /// How many independent swap scenarios to run when discriminating a real
        /// non-atomic window from the #457 flake. A genuine delete-then-add opens an
        /// absent window on EVERY swap (the write path itself is broken), so it
        /// reproduces across essentially every scenario; the #457 flake is instead a
        /// rare securityd cross-process `errSecItemNotFound` under a concurrent
        /// `security` modify — a Heisenberg that appears in a small MINORITY of runs
        /// (20/20 clean locally; 1× in CI). Declaring the item "reproducibly absent"
        /// only on a MAJORITY of scenarios sits in the wide valley between the two
        /// rates: a deterministic regression clears the majority every time, a lone
        /// transient never does. (Immediate-re-read "persistence" does NOT work — a
        /// realistic bare delete-then-add window is ~one `add` process, which self-heals
        /// before a few back-to-back re-reads finish, so the window must be caught by
        /// RECURRENCE across scenarios, not persistence within one — issue #457.)
        const ABSENCE_REPRO_ATTEMPTS: u32 = 5;

        /// AC (#12): a scripted long-running request + a forced mid-request swap →
        /// the request completes AND the next request reports the new account.
        ///
        /// The "long-running request" is a reader re-reading the canonical item in a
        /// tight loop (the target's per-request read); the "forced mid-request swap"
        /// is the real [`swap`] rotating A → B underneath it. The reader runs as its
        /// own task so its `security` reads genuinely race the swap's `security`
        /// write on the shared keychain.
        ///
        /// Runs [`run_mid_turn_swap_scenario`] up to `ABSENCE_REPRO_ATTEMPTS` times and
        /// decides by MAJORITY: a real delete-then-add is absent in (essentially) every
        /// scenario → a majority-absent verdict fails; the rare cross-process transient
        /// is absent in a minority → a majority-clean verdict passes. Short-circuits as
        /// soon as a majority lands either way, so a healthy run pays for a bare
        /// majority of clean scenarios.
        #[tokio::test]
        async fn a_long_running_request_completes_and_the_next_request_reports_the_new_account() {
            let majority = ABSENCE_REPRO_ATTEMPTS / 2 + 1;
            let mut absent_scenarios: u32 = 0;
            let mut clean_scenarios: u32 = 0;
            let mut last_absent_reads: u32 = 0;
            for _ in 0..ABSENCE_REPRO_ATTEMPTS {
                let absent_reads = run_mid_turn_swap_scenario().await;
                if absent_reads == 0 {
                    clean_scenarios += 1;
                } else {
                    absent_scenarios += 1;
                    last_absent_reads = absent_reads;
                }
                // Decide as soon as a majority lands: a real (reproducible) window
                // reaches the `absent` majority; a healthy write reaches the `clean` one.
                if absent_scenarios >= majority || clean_scenarios >= majority {
                    break;
                }
            }
            // A MAJORITY of independent scenarios found the canonical item absent mid-
            // swap: the absence is REPRODUCIBLE, which the atomic `-U` write can never
            // be — a genuine delete-then-add gap a per-request reader falls through. A
            // lone transient (minority) never reaches here (issue #457).
            assert!(
                clean_scenarios >= majority,
                "the canonical item was absent mid-swap in a majority of scenarios \
                 ({absent_scenarios}/{ABSENCE_REPRO_ATTEMPTS}, last {last_absent_reads}× reads) \
                 — the write was not atomic (a reproducible delete-then-add gap)"
            );
        }

        /// One forced-mid-turn-swap scenario against a throwaway keychain; returns the
        /// count of reads that found the canonical item ABSENT while the swap raced
        /// underneath — the only flaky signal, handed to the caller for majority
        /// (reproducibility) adjudication. Every OTHER guarantee (never-torn,
        /// spans-the-swap, ends-on-B, one-way cut, reroute landed, outgoing preserved)
        /// is DETERMINISTIC and asserted here directly: those never flake, so a
        /// violation fails the scenario on the spot rather than being retried.
        async fn run_mid_turn_swap_scenario() -> u32 {
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
                    // item present at every instant, so a genuine delete-then-add would
                    // surface here on EVERY swap — but a lone hit can also be the rare
                    // securityd cross-process transient, so the caller adjudicates by
                    // MAJORITY across scenarios (§ ABSENCE_REPRO_ATTEMPTS) rather than
                    // failing on a single scenario's count. Capturing it (rather than
                    // discarding every error) is what keeps "never torn / never absent"
                    // falsifiable in CI, not merely observed.
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
                            // The item was absent — a CANDIDATE non-atomic window,
                            // confirmed real by the caller only if it REPRODUCES across
                            // a majority of scenarios (a lone hit is the transient).
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
            absent_reads
        }
    }
}
