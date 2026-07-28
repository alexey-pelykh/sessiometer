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
//!     stashed it; never block on "couldn't verify") UNLESS the orphan canonical
//!     does not even parse as a CC credential (issue #730's shape-gate — see the
//!     fail-policy note below): a canonical that is not Claude Code's own shape is
//!     overwhelmingly an UNRELATED secret, so the caller fails CLOSED to keep the
//!     atomic `-U` upsert from clobbering it.
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
//! surface, not this refuse. Narrowing Layer 3 needs an ONLINE liveness signal
//! (`/oauth/usage` currency of the resolved token) — out of scope for the
//! OFFLINE layers above, and since issue #736 available as the opt-in
//! [`probe_liveness`] described below. With that probe disarmed (the default) the
//! INCONCLUSIVE (`Layer-1-only`) verdict on the status wire remains the honest
//! surface of this limit. Non-swap canonical writes (the #467 scrubbed-canonical adopt, `use
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
//! (`src/keychain.rs`). INCONCLUSIVE proceeds (Layer-1-only) — with ONE hardened
//! sub-case (issue #730): a `NoStashMatch` whose orphan canonical does not parse
//! as a CC credential (`canonical_well_formed == false`) is refused at the callers
//! via the SAME fail-closed slot as DRIFT, protecting an unrelated secret from the
//! atomic `-U` clobber, unless the dedicated `canary_nostashmatch_override` tunable
//! (separate from `canary_drift_override`) is set. A well-formed orphan canonical
//! (a benign in-place refresh) still fails OPEN, exactly as before. The IDENTITY
//! answer is inconclusive either way, but the refusal is an operator-visible
//! consequence, so since issue #738 the wire carries its own
//! `refused_unparseable_canonical` verdict (schema 1.10) rather than the quiet
//! `inconclusive` #730 originally reused — collapsing back to `inconclusive`
//! exactly when the override has restored the fail-OPEN and nothing is refused.
//!
//! ## The opt-in ONLINE liveness probe (issue #736)
//!
//! [`probe_liveness`] is the canary's only NETWORKED check and the partial answer
//! to the Layer-3 residual above. It runs on the same pre-`-U` decision path as the
//! offline layers, AFTER the canonical is resolved, and asks the one question an
//! offline check cannot: *does this bearer still authenticate?* It reuses the
//! existing `/oauth/usage` client ([`crate::usage`], reached through the
//! [`RosterPoller`] seam both pre-swap callers already hold) — no new transport, no
//! new auth flow, and the client's own bounded `max-time` means a hung network
//! cannot stall a swap.
//!
//! Two properties bound what it can claim, and both are deliberate:
//!
//! 1. **LIVENESS, never IDENTITY.** The `/oauth/usage` response carries no account
//!    field, so a pass says the token works — never WHOSE session it is. Resolving
//!    identity online is the separate, gated issue #737 (`/api/oauth/profile`);
//!    nothing here may claim it.
//! 2. **It NARROWS Layer 3 rather than closing it.** The residual's shape is a
//!    same-account canonical silently relocated in place. Where the stale copy has
//!    also gone DEAD, the probe sees it; where it is stale-but-VALID — the residual
//!    as originally stated — the probe passes just as the offline layers do. That
//!    sub-case stays open.
//!
//! It is therefore DOUBLE-gated, and off at both gates by default. `canary_online_probe`
//! arms it — while `false`, no request is issued at all, so the default swap path is
//! byte-identical to the pre-#736 one. `canary_online_probe_strict` decides whether a
//! probe that does not confirm liveness REFUSES the write; while `false` (the decided
//! graceful-degrade posture) the verdict is logged and the swap PROCEEDS, so a network
//! outage can never become a swap outage. Strict refuses on INCONCLUSIVE as well as on
//! a positive rejection, because opting into strict IS opting into the network failure
//! mode the non-strict default forbids. Refusing by default would additionally be
//! wrong on the evidence: Claude Code refreshes its `accessToken` in place, so a
//! momentarily expired-but-refreshable token answers `401` while the credential is
//! perfectly healthy.
//!
//! One asymmetry with the offline layers is load-bearing: **an armed probe stands down
//! when the caller has no current reading for the outgoing bearer**
//! ([`ProbeGate::Uninformative`]). Every tick-driven swap arm that fires off a FAILING
//! active does so on exactly that precondition — the emergency swap off a dead active
//! (issue #42), the bounded-blindness preemptive swap off a blind one (#452 / ADR-0017)
//! — so probing there would re-derive the known failure and, under strict, refuse the
//! very swap that exists to escape it, on every tick and forever. The daemon reads its
//! own last poll of the account for this. The daemon-DOWN `use` path has no such state,
//! and neither path can foresee a canonical that dies between the last poll and the
//! swap, so `--force` is the operator's escape at BOTH ([`ProbeGate::Overridden`]); it
//! bypasses THIS layer only — see [`Error::CanaryProbeNotLive`] and the `use` call site
//! for why Layer 3's much milder failure consequence earns an override the
//! unrecoverable-clobber layers do not.
//!
//! Unlike the offline layers the probe yields no STANDING verdict — it runs per swap
//! ATTEMPT, not per tick — so it adds nothing to the `status` wire (no
//! [`CanaryStatus`](crate::daemon::CanaryStatus) variant, no schema bump). Its durable
//! surface is [`Event::CanaryOnlineProbe`](crate::observability::Event::CanaryOnlineProbe),
//! emitted at the refuse sites in the alarm-only idiom its `canary_drift` /
//! `canary_unparseable_canonical` siblings use: a probe that confirms liveness is
//! silent.
//!
//! Every surface derived from these types is secret-free by construction (issue
//! #15): outcomes carry roster INDICES (resolved to operator labels at the event /
//! status boundary), never a token, email, or account-uuid. The probe holds to the
//! same line — its verdict is a three-way class, never a status code, a response
//! body, or a bearer.

use std::path::Path;

use crate::active;
use crate::claude_state;
use crate::config::Account;
use crate::daemon::RosterPoller;
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
    /// The resolved canonical token matches NO account's stash. Two sub-cases the
    /// caller tells apart via `canonical_well_formed` (the #730 shape-check):
    ///   - `true`  — the orphan canonical still parses as a well-formed Claude Code
    ///     credential (`{"claudeAiOauth":{accessToken,refreshToken,expiresAt}}`):
    ///     overwhelmingly the active account's own token refreshed in place since
    ///     it was last stashed (benign) → fail OPEN, EXACTLY #714's behavior (never
    ///     block on a token CC simply hasn't restashed);
    ///   - `false` — the canonical does NOT parse as a CC credential, so it is
    ///     overwhelmingly NOT Claude Code's own item (a future CC storage-format
    ///     change leaving an unrelated secret under the derived service): the caller
    ///     REFUSES the credential write (fail-CLOSED, #730), protecting that secret
    ///     from the atomic `-U` clobber, unless `canary_nostashmatch_override` is set.
    ///
    /// The identity verdict is genuinely INCONCLUSIVE either way (the token matched
    /// nothing, so identity is unverified); the refuse is a daemon-internal POLICY
    /// layered on top. #730 therefore left the wire verdict at `inconclusive` — which
    /// issue #738 corrected: the POLICY is what the operator experiences, so the
    /// refusing sub-case now projects to
    /// [`CanaryStatus::RefusedUnparseableCanonical`](crate::daemon::CanaryStatus::RefusedUnparseableCanonical)
    /// (schema 1.10), while the overridden and well-formed cases stay `inconclusive`.
    NoStashMatch {
        /// Whether the orphan canonical parses as a well-formed Claude Code
        /// credential (issue #730). `false` drives the caller's fail-CLOSED refuse
        /// of the `-U` clobber; `true` preserves #714's fail-OPEN.
        canonical_well_formed: bool,
    },
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
    // No stash matched the resolved canonical. #730: shape-check the canonical in
    // hand — a benign in-place refresh still parses as CC's own credential (fail
    // OPEN, #714), but a canonical that no longer parses as CC's shape is almost
    // certainly an unrelated secret the atomic `-U` upsert must NOT clobber (the
    // caller fails CLOSED). OFFLINE — a local parse of the canonical already read
    // above, no network / no keychain re-read.
    let canonical_well_formed = crate::refresh::is_well_formed_credential(canonical.expose());
    Ok(CanaryOutcome::Inconclusive(
        InconclusiveReason::NoStashMatch {
            canonical_well_formed,
        },
    ))
}

/// The Layer-3 ONLINE liveness verdict (issue #736) — a closed class, never an HTTP
/// status, a response body, or a bearer (issue #15).
///
/// Deliberately NOT an identity verdict: `/oauth/usage` names no account, so
/// [`Live`](Liveness::Live) means "this bearer still authenticates", never "this
/// bearer is account A's".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Liveness {
    /// [`ProbeGate::Disarmed`]: `canary_online_probe` is off, so NO request was issued.
    /// Distinct from [`Inconclusive`](Liveness::Inconclusive), which means the probe RAN
    /// and learned nothing — that one strict mode may refuse on, this one never can
    /// (see [`probe_refuses`]).
    Skipped,
    /// [`ProbeGate::Uninformative`]: armed, but the caller has no CURRENT reading for this
    /// bearer, so NO request was issued. See that variant for why asking anyway would be
    /// both redundant and actively harmful. Never refuses.
    Uninformative,
    /// [`ProbeGate::Overridden`]: armed, but the operator passed `--force`, so NO request
    /// was issued and the swap proceeds. Logged rather than silent — a forced bypass of an
    /// armed strict gate is exactly the kind of thing an operator later needs to find in
    /// the log. Never refuses.
    Overridden,
    /// The endpoint accepted the bearer (`2xx`): the resolved canonical still
    /// authenticates.
    Live,
    /// The endpoint positively REJECTED the bearer (`401`). Suggestive, not proof:
    /// Claude Code refreshes its `accessToken` in place, so a momentarily expired
    /// but perfectly refreshable token answers `401` too — which is why this only
    /// refuses under the opt-in strict mode.
    Rejected,
    /// The probe ran but established nothing either way — no HTTP response at all
    /// (DNS / connection / TLS / timeout), a `429`, a `5xx`, the `403` missing-scope
    /// case (a non-interactive setup token: a statement about the token's SCOPES,
    /// not its validity), or a `2xx` whose body did not parse. A keychain that could
    /// not be read for the bearer lands here too: unreadable is not rejected.
    Inconclusive,
}

impl Liveness {
    /// The stable, secret-free label for the durable event line (issue #15).
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Liveness::Skipped => "skipped",
            Liveness::Uninformative => "uninformative",
            Liveness::Overridden => "overridden",
            Liveness::Live => "live",
            Liveness::Rejected => "rejected",
            Liveness::Inconclusive => "inconclusive",
        }
    }
}

/// Whether — and why — [`probe_liveness`] should issue its request (issue #736).
///
/// The caller supplies FACTS about the swap it is about to perform; the decision to put
/// a request on the wire stays inside `probe_liveness`, so "disarmed means no request at
/// all" is enforced in one place rather than replicated at each call site.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ProbeGate {
    /// `canary_online_probe` is off. → [`Liveness::Skipped`].
    Disarmed,
    /// Armed, and the caller has no independent reason to expect this poll to fail.
    Armed,
    /// Armed, but the caller has NO CURRENT READING for this bearer — its own last poll of
    /// it did not return one (it failed, or it has not polled it yet), or it is inside a
    /// server-directed `Retry-After` / rate-limit hold. → [`Liveness::Uninformative`], no
    /// request.
    ///
    /// This is not an optimization; omitting it is a self-DoS. Every tick-driven swap arm
    /// that fires off a FAILING active does so on exactly this precondition — the emergency
    /// swap off a quarantined one (issue #42) and the bounded-blindness preemptive swap off
    /// a blind one (issue #452 / ADR-0017) both require the active's current reading to be
    /// absent. On those arms a failing probe is the REASON to swap, and a strict gate that
    /// read it as a reason to refuse would block, every tick and forever, the exact swap
    /// that exists to escape the failure.
    ///
    /// The condition is deliberately the DIRECT fact — "no current reading" — and not the
    /// narrower proxies it is tempting to substitute. `quarantined` alone under-covers it:
    /// blindness caused by a `403` missing scope or a non-401 `4xx` neither quarantines the
    /// account nor arms its back-off (it resets the 401 streak and CLEARS `poll_backoff_until`),
    /// yet still blinds it into the preemptive-swap arm. It slightly OVER-covers in one
    /// direction — an account this daemon has not polled YET (a fresh start, a just-`capture`d
    /// account) reads the same — which is the fail-OPEN direction and the honest one: a bearer
    /// the daemon has never reached is one it cannot vouch for either.
    ///
    /// The back-off half is kept as a separate term because it covers the converse case — a
    /// held account can still carry a stale-but-present reading — and because it keeps the
    /// probe from firing an extra request into a hold the server just directed, which the tick
    /// loop's own poll would have skipped (issue #293). That is the whole extent of the claim:
    /// the probe respects an ALREADY-ARMED hold, but its own outcome is never folded back into
    /// [`note_account_backoff`](crate::daemon::Daemon::note_account_backoff), so a probe that
    /// is itself throttled neither advances the account's streak nor arms a window. Under
    /// strict + an intermittently-`429`ing endpoint that leaves one extra request per refused
    /// tick; the refusal loop itself is strict mode's documented cost, not new.
    ///
    /// Nothing is lost by not asking. The probe's question is "has this canonical gone
    /// dead behind our back?" — and when the caller cannot even get a routine reading out
    /// of the bearer, the reply carries no Layer-3 information: a relocated-and-dead
    /// canonical and a plainly unreachable one are indistinguishable to it.
    Uninformative,
    /// Armed, but the operator passed `--force`. → [`Liveness::Overridden`], no request.
    ///
    /// Layer 3's failure consequence is milder than Layers 1–2's — a swap that may not take
    /// effect, versus unrecoverably clobbering an unrelated account's secret — so unlike
    /// those layers it yields to an explicit operator override. That is what makes strict
    /// mode escapable without editing config and restarting: see
    /// [`Error::CanaryProbeNotLive`].
    Overridden,
}

/// Run the Layer-3 online liveness probe (issue #736) against the RESOLVED CANONICAL
/// credential, immediately before a swap's `-U` write.
///
/// `gate` decides whether a request is issued at all, and both no-request cases return
/// without touching `poller` — so the "off means no request" invariant is a property of
/// this function rather than a convention the call sites must each remember.
///
/// `account` is the account Claude Code's own state names active, i.e. the one the
/// canonical is BELIEVED to belong to. It selects nothing on the production path: the
/// `active = true` argument routes [`RosterPoller::poll`] through the canonical
/// credential store, which is precisely the bearer under test. Passing the believed
/// owner keeps the call honest for a fake poller and reads correctly at the call site.
///
/// FRESH by construction. The daemon polls the active account every tick, so a recent
/// reading is usually at hand — but it can be a whole `poll_secs` old, and a relocation
/// can land inside that window. Layer 1 already re-resolves fresh at swap time rather
/// than trusting its boot-pinned cache for the same reason; this holds the same line.
///
/// Never returns `Err`: a probe is evidence-gathering, and its FAILURE modes are
/// verdicts (`Rejected` / `Inconclusive`), not errors. Whether a verdict refuses the
/// swap is [`probe_refuses`]'s decision, kept separate so the policy is unit-testable
/// without a poller.
pub(crate) async fn probe_liveness<P>(poller: &P, account: &Account, gate: ProbeGate) -> Liveness
where
    P: RosterPoller,
{
    match gate {
        ProbeGate::Disarmed => return Liveness::Skipped,
        ProbeGate::Uninformative => return Liveness::Uninformative,
        ProbeGate::Overridden => return Liveness::Overridden,
        ProbeGate::Armed => {}
    }
    match poller.poll(account, true).await {
        Ok(_) => Liveness::Live,
        Err(Error::UsageUnauthorized) => Liveness::Rejected,
        // Everything else is an absence of evidence, NOT evidence of absence: transient
        // transport failures, throttling, a 5xx, a missing scope, an unparseable body, a
        // locked keychain. Collapsed to one verdict on purpose — the swap decision below
        // treats them identically, and a finer taxonomy on the log line would only invite
        // reading proof of relocation into a network blip.
        Err(_) => Liveness::Inconclusive,
    }
}

/// Whether a Layer-3 probe verdict REFUSES the credential write (issue #736).
///
/// `strict` is the `canary_online_probe_strict` tunable. Non-strict — the default —
/// never refuses: the decided graceful-degrade posture, so a network outage cannot
/// become a swap outage. Strict refuses on anything that is not a confirmed-live
/// bearer, [`Liveness::Inconclusive`] included: the issue's constraint is "no MANDATORY
/// network failure mode … unless the operator opts into a strict mode", so opting in IS
/// opting into that failure mode.
///
/// Only a verdict the probe actually WENT AND GOT can refuse. [`Liveness::Skipped`]
/// cannot, which keeps `canary_online_probe_strict` inert while `canary_online_probe`
/// is off — the documented "only meaningful when the probe is armed" contract, enforced
/// rather than merely stated. Neither can [`Liveness::Uninformative`], which is what
/// keeps strict mode from deadlocking the swap arms that fire BECAUSE the outgoing
/// bearer is failing (see [`ProbeGate::Uninformative`]), nor [`Liveness::Overridden`],
/// which is the operator's own `--force` escape out of a strict refusal.
pub(crate) fn probe_refuses(liveness: Liveness, strict: bool) -> bool {
    strict && matches!(liveness, Liveness::Rejected | Liveness::Inconclusive)
}

/// Whether a Layer-3 probe verdict earns a durable
/// [`Event::CanaryOnlineProbe`](crate::observability::Event::CanaryOnlineProbe) line
/// (issue #736) — the ALARM-ONLY rule the two pre-swap sites share.
///
/// Silent on exactly two verdicts, for opposite reasons. [`Liveness::Live`] is the
/// healthy answer, and logging it would make every swap on an armed daemon noisy.
/// [`Liveness::Skipped`] never ran, so a DISARMED probe must leave the log exactly as it
/// was pre-#736. Everything else speaks — including the two stand-downs and the
/// non-strict degrade, where this line is the ONLY trace that the gate did not do what an
/// operator armed it to do.
///
/// A predicate beside [`probe_refuses`] rather than a `matches!` at each call site: the
/// two are this layer's whole policy, they are the pair a new [`Liveness`] variant must be
/// weighed against, and keeping them together is what stops the daemon's copy and the
/// standalone `use` copy from drifting apart.
pub(crate) fn probe_alarms(liveness: Liveness) -> bool {
    !matches!(liveness, Liveness::Skipped | Liveness::Live)
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

    /// A well-formed Claude Code credential blob carrying `access_token` — the exact
    /// `{"claudeAiOauth":{accessToken,refreshToken,expiresAt}}` shape #730 recognizes.
    /// Used where a NoStashMatch canonical must still parse as CC's own credential.
    fn cc_blob(access_token: &str) -> Vec<u8> {
        format!(
            r#"{{"claudeAiOauth":{{"accessToken":"{access_token}","refreshToken":"sk-ant-ort-RT","expiresAt":1700000000000}}}}"#
        )
        .into_bytes()
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
    async fn inconclusive_well_formed_when_a_cc_canonical_matches_no_stash() {
        // The overwhelmingly-common benign state: the active account's token
        // refreshed in place since it was last stashed → no stash matches. The
        // orphan canonical STILL parses as a well-formed CC credential (#730), so it
        // fails OPEN (`canonical_well_formed: true`) — EXACTLY #714's behavior, never
        // a refuse. (The stashes hold raw non-CC bytes, so the canonical matches
        // none of them, yet only the ACTIVE canonical's shape gates — active-scoped.)
        let roster = roster_ab();
        let stash = stash_ab().await;
        let store = store_holding(&cc_blob("sk-ant-oat-REFRESHED")).await;
        let (_dir, json) = claude_json_for("u-A");
        let outcome = run(&store, &stash, &roster, &json).await.unwrap();
        assert_eq!(
            outcome,
            CanaryOutcome::Inconclusive(InconclusiveReason::NoStashMatch {
                canonical_well_formed: true
            })
        );
    }

    #[tokio::test]
    async fn inconclusive_not_well_formed_when_an_unparseable_canonical_matches_no_stash() {
        // The #730 hardened case: under a FUTURE CC storage-format change the
        // resolved canonical matches no stash AND no longer parses as a CC credential
        // — almost certainly an unrelated secret. The canary carries
        // `canonical_well_formed: false` so the caller can fail CLOSED and protect it
        // from the `-U` clobber. The identity verdict is still genuinely INCONCLUSIVE
        // (the refuse is a caller policy on top).
        let roster = roster_ab();
        let stash = stash_ab().await;
        let store = store_holding(b"an-unrelated-keychain-secret").await;
        let (_dir, json) = claude_json_for("u-A");
        let outcome = run(&store, &stash, &roster, &json).await.unwrap();
        assert_eq!(
            outcome,
            CanaryOutcome::Inconclusive(InconclusiveReason::NoStashMatch {
                canonical_well_formed: false
            })
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

    // --- Layer 3: the opt-in online liveness probe (issue #736) ---------------

    /// A [`RosterPoller`] that answers one scripted outcome and COUNTS its calls, so a
    /// test can assert not merely the verdict but whether the network was touched at
    /// all — the load-bearing claim for the disarmed default. No real HTTP: the probe
    /// reaches the network only through this seam.
    struct ScriptedPoller {
        outcome: std::cell::RefCell<Option<Result<()>>>,
        calls: std::cell::Cell<usize>,
    }

    impl ScriptedPoller {
        fn ok() -> Self {
            Self::scripted(Ok(()))
        }

        fn failing(err: Error) -> Self {
            Self::scripted(Err(err))
        }

        fn scripted(outcome: Result<()>) -> Self {
            Self {
                outcome: std::cell::RefCell::new(Some(outcome)),
                calls: std::cell::Cell::new(0),
            }
        }
    }

    impl RosterPoller for ScriptedPoller {
        async fn poll(
            &self,
            _account: &Account,
            active: bool,
        ) -> Result<crate::usage::PolledReading> {
            // The probe must ride the CANONICAL credential, not a stash — the whole point
            // is testing the item the `-U` write is about to overwrite.
            assert!(active, "the liveness probe must poll `active = true`");
            self.calls.set(self.calls.get() + 1);
            match self
                .outcome
                .borrow_mut()
                .take()
                .expect("one poll per probe")
            {
                Ok(()) => Ok(crate::usage::PolledReading {
                    usage: crate::usage::Usage {
                        session: 0.10,
                        weekly: 0.10,
                        weekly_resets_at: None,
                        session_resets_at: None,
                    },
                    severity: None,
                }),
                Err(err) => Err(err),
            }
        }
    }

    #[tokio::test]
    async fn a_disarmed_probe_issues_no_request_at_all() {
        // Issue #736's first hard constraint: `canary_online_probe = false` (the default)
        // must not merely ignore the probe's answer — it must never ASK. Asserted on the
        // poller's call count, since a verdict alone cannot tell "not asked" from "asked
        // and passed".
        let poller = ScriptedPoller::ok();
        let verdict = probe_liveness(&poller, &acct("work", "u-A"), ProbeGate::Disarmed).await;
        assert_eq!(verdict, Liveness::Skipped);
        assert_eq!(poller.calls.get(), 0, "a disarmed probe must not poll");
    }

    #[tokio::test]
    async fn an_uninformative_gate_stands_down_without_asking() {
        // The self-DoS guard. When the caller has no current reading for this bearer, the
        // probe must not ask — both because the answer carries no Layer-3 information and
        // because the swap arms that reach here fire BECAUSE of that failure. Call count
        // pins "did not ask"; the distinct verdict pins that it is told apart from a
        // disarmed probe on the durable line.
        let poller = ScriptedPoller::ok();
        let verdict = probe_liveness(&poller, &acct("work", "u-A"), ProbeGate::Uninformative).await;
        assert_eq!(verdict, Liveness::Uninformative);
        assert_eq!(poller.calls.get(), 0, "an uninformative gate must not poll");
        // …and it can never refuse, whatever strict says — the property that keeps the
        // emergency / blind-preempt escape swaps from deadlocking.
        assert!(!probe_refuses(verdict, true));
    }

    #[tokio::test]
    async fn an_overridden_gate_stands_down_without_asking_but_is_not_silent() {
        // `--force`: the operator's escape out of a strict refusal, at BOTH pre-swap sites.
        // It stands down like the other two no-request gates, and it is deliberately its OWN
        // verdict rather than folded into `Skipped` — `Skipped` is silent by design, and a
        // bypass of a gate the operator ARMED is exactly the event that must survive in the
        // log (the offline layers record their overrides the same way).
        let poller = ScriptedPoller::failing(Error::UsageUnauthorized);
        let verdict = probe_liveness(&poller, &acct("work", "u-A"), ProbeGate::Overridden).await;
        assert_eq!(verdict, Liveness::Overridden);
        assert_eq!(poller.calls.get(), 0, "an overridden gate must not poll");
        assert!(!probe_refuses(verdict, true));
        assert_ne!(verdict.as_str(), Liveness::Skipped.as_str());
    }

    #[tokio::test]
    async fn an_armed_probe_that_authenticates_is_live() {
        let poller = ScriptedPoller::ok();
        let verdict = probe_liveness(&poller, &acct("work", "u-A"), ProbeGate::Armed).await;
        assert_eq!(verdict, Liveness::Live);
        assert_eq!(poller.calls.get(), 1);
    }

    #[tokio::test]
    async fn a_401_is_rejected_not_inconclusive() {
        // The one outcome that is positive evidence the bearer no longer authenticates —
        // kept distinct from the no-evidence class so the durable log line says which.
        let poller = ScriptedPoller::failing(Error::UsageUnauthorized);
        let verdict = probe_liveness(&poller, &acct("work", "u-A"), ProbeGate::Armed).await;
        assert_eq!(verdict, Liveness::Rejected);
    }

    #[tokio::test]
    async fn every_other_failure_is_inconclusive_never_rejected() {
        // Absence of evidence, not evidence of absence: a throttle, a 5xx, an unreachable
        // endpoint, a missing scope, and a locked keychain must NOT masquerade as proof
        // that the canonical went dead.
        for err in [
            Error::UsageRateLimited {
                status: 429,
                retry_after: None,
            },
            Error::UsageTransient {
                status: 503,
                retry_after: None,
            },
            Error::UsageTransient {
                status: 0,
                retry_after: None,
            },
            Error::UsageScopeMissing,
            Error::UsageRejected { status: 418 },
            Error::KeychainLocked { op: "read" },
        ] {
            let label = format!("{err}");
            let poller = ScriptedPoller::failing(err);
            assert_eq!(
                probe_liveness(&poller, &acct("work", "u-A"), ProbeGate::Armed).await,
                Liveness::Inconclusive,
                "{label} must be inconclusive"
            );
        }
    }

    #[test]
    fn non_strict_never_refuses_whatever_the_probe_said() {
        // Issue #736's second hard constraint: probe failure != refuse. A network outage
        // must not become a swap outage under the default posture.
        for verdict in [
            Liveness::Skipped,
            Liveness::Uninformative,
            Liveness::Overridden,
            Liveness::Live,
            Liveness::Rejected,
            Liveness::Inconclusive,
        ] {
            assert!(!probe_refuses(verdict, false), "{verdict:?} refused");
        }
    }

    #[test]
    fn only_a_verdict_the_probe_went_and_got_can_refuse() {
        // The three no-request verdicts are structurally unable to refuse, which is what
        // makes every stand-down safe: `Skipped` keeps strict inert while the probe is
        // disarmed, `Uninformative` keeps it from deadlocking the escape swaps, `Overridden`
        // is the operator's own `--force`. Asserted together so a future variant added to
        // `Liveness` has to face this question.
        assert!(!probe_refuses(Liveness::Skipped, true));
        assert!(!probe_refuses(Liveness::Uninformative, true));
        assert!(!probe_refuses(Liveness::Overridden, true));
    }

    #[test]
    fn strict_refuses_on_inconclusive_as_well_as_rejected() {
        // Strict mode IS the opt-in to the network failure mode the default forbids, so it
        // refuses on "could not confirm", not only on a positive rejection.
        assert!(probe_refuses(Liveness::Rejected, true));
        assert!(probe_refuses(Liveness::Inconclusive, true));
        assert!(!probe_refuses(Liveness::Live, true));
    }

    #[test]
    fn strict_is_inert_while_the_probe_is_disarmed() {
        // `canary_online_probe_strict` is documented as meaningful only with the probe
        // armed. `Skipped` never refusing is what ENFORCES that, rather than leaving the
        // pairing to the call sites: a lone `strict = true` cannot block a swap.
        assert!(!probe_refuses(Liveness::Skipped, true));
    }

    #[test]
    fn only_the_healthy_and_the_never_ran_verdicts_are_silent() {
        // The alarm-only rule, pinned as a set rather than left implicit in two call-site
        // `matches!`es. `Live` is silent so an armed daemon's swaps stay quiet; `Skipped` is
        // silent so a DISARMED probe leaves the log byte-identical to pre-#736. The other
        // four MUST speak — under the non-strict default that line is the only trace the
        // gate failed and the swap went ahead anyway. Enumerated so a future `Liveness`
        // variant has to face this question, exactly as
        // `only_a_verdict_the_probe_went_and_got_can_refuse` does for the refusal policy.
        assert!(!probe_alarms(Liveness::Live));
        assert!(!probe_alarms(Liveness::Skipped));
        for verdict in [
            Liveness::Uninformative,
            Liveness::Overridden,
            Liveness::Rejected,
            Liveness::Inconclusive,
        ] {
            assert!(probe_alarms(verdict), "{verdict:?} must be on the record");
        }
    }

    #[test]
    fn verdict_labels_are_stable_and_secret_free() {
        // These strings reach the durable log line and the typed error's message, so they
        // are contract. Each is a verdict CLASS — never a status code, body, or bearer.
        assert_eq!(Liveness::Skipped.as_str(), "skipped");
        assert_eq!(Liveness::Uninformative.as_str(), "uninformative");
        assert_eq!(Liveness::Overridden.as_str(), "overridden");
        assert_eq!(Liveness::Live.as_str(), "live");
        assert_eq!(Liveness::Rejected.as_str(), "rejected");
        assert_eq!(Liveness::Inconclusive.as_str(), "inconclusive");
    }
}
