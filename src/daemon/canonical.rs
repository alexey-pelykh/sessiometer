// Copyright (c) 2026 Oleksii PELYKH
// SPDX-License-Identifier: MIT

//! Canonical-credential handling for the [`Daemon`] decision core (issue #637 step 4, issue
//! #659, split out of the single `impl Daemon` block).
//!
//! Everything that reads, resolves, heals or re-stashes the SHARED canonical keychain item:
//! reconcile-on-start's crash / third-writer heal, resolving which roster account a
//! canonical token belongs to, re-stashing a re-authenticated account, the #467 autonomous
//! recovery of a scrubbed canonical, and the edge-triggered canonical-liveness rollup that
//! tells the operator the shared item was scrubbed or yanked out from under the daemon.

use super::*;

use crate::canary::{CanaryOutcome, InconclusiveReason};

/// Close a canary ALARM episode's durable bracket exactly once (issue #714, generalized by #738):
/// push [`Event::CanaryCleared`] iff the verdict being REPLACED was an alarm this module opened —
/// a drift (an OVERRIDDEN one included: its alarm was real even though the write proceeded) or an
/// ambiguous resolution.
///
/// Called by every NON-OPENING transition, so they all close identically. The alarm arms
/// (`Drift`, `Ambiguous`) deliberately do NOT call it: an alarm → different alarm transition
/// RE-LABELS the episode rather than ending it, firing only the new alarm's event. So the rule has
/// two halves — a verdict that opens its own alarm re-labels; a verdict that opens nothing closes
/// whatever it replaces.
///
/// Extracted because inlining the check at each arm is what let issue #738's new verdict silently
/// skip it: that arm fires no event of its own, so the missing close looked like "nothing to do"
/// rather than a dropped bracket, and a drift alarm raised before an unrelated secret appeared
/// would have stayed open in the log forever.
///
/// Deliberately does NOT list [`CanaryStatus::RefusedUnparseableCanonical`] among the closable
/// alarms: `refresh_canary` never OPENS a bracket for it (its durable line is per-attempt and owned
/// by the refuse sites), and closing a bracket that was never opened would emit an unpaired
/// `canary_cleared`.
fn close_canary_alarm_bracket(replacing: Option<&CanaryStatus>, events: &mut Vec<Event>) {
    if matches!(
        replacing,
        Some(CanaryStatus::Drift { .. } | CanaryStatus::Ambiguous { .. })
    ) {
        events.push(Event::CanaryCleared);
    }
}

impl<P, C, S, K> super::Daemon<P, C, S, K>
where
    P: RosterPoller,
    C: CredentialStore,
    S: AccountStash,
    K: Clock,
{
    /// Reconcile `~/.claude.json` to the canonical credential on startup.
    ///
    /// Heals the post-swap crash window: a swap writes the incoming token to the
    /// canonical item (the commit) and then co-writes `~/.claude.json` (best
    /// effort); a crash in between leaves the canonical holding the incoming token
    /// while the display still shows the outgoing account. Here we find the roster
    /// account whose stash matches the canonical token and, if the display
    /// disagrees, co-write that account's `oauthAccount`. When the canonical
    /// matches no stash — the active account's token has merely drifted (refreshed
    /// in place) on a normal restart, or it belongs to an un-captured account —
    /// `~/.claude.json` is left untouched (there is nothing to heal). Best-effort
    /// and idempotent.
    ///
    /// This IS the issue #13 process-death-mid-swap recovery: the swap commits the
    /// canonical token before co-writing the display, so a crash in that window
    /// leaves the keychain authoritative and the display stale — exactly the
    /// mismatch healed here on the next start. No separate mechanism is needed; the
    /// keychain-first ordering plus this reconcile make a torn swap self-healing.
    ///
    /// The heal core is [`crate::canary::reconcile_display`] (extracted for issue
    /// #714): the behavioral canary runs the SAME reconcile before its pre-swap
    /// identity cross-check, so a lagging self-co-write can never false-positive
    /// as drift — one core, two wirings.
    pub(crate) async fn reconcile_on_start(&self) -> Result<()> {
        let canonical = self.store.read().await?;
        crate::canary::reconcile_display(&self.roster, &self.stash, &self.claude_json, &canonical)
            .await
    }

    /// Run one behavioral-canary pass (issue #714) and fold its verdict into
    /// daemon state: retain it for the `status` wire ([`Daemon::snapshot`] copies
    /// `state.canary`) and emit the durable edge-triggered event pairs.
    ///
    /// Wired at BOOT (right after [`reconcile_on_start`](Self::reconcile_on_start),
    /// so the operator sees a drifted derivation without waiting for a swap) and
    /// PRE-SWAP inside [`locked_swap`](Self::locked_swap) (the refuse slot — a
    /// fresh verdict every swap, never the boot-pinned one, because the `OnceLock`
    /// resolution cache and CC's own state both move under a running daemon).
    ///
    /// Edge semantics, mirroring the [`Event::CanonicalScrubbed`] /
    /// [`Event::CanonicalRestored`] durable-pair idiom: an event fires only when
    /// the verdict CHANGES. A new / re-shaped alarm (drift with different labels,
    /// ambiguity with a different count) re-fires its event; an alarm → non-alarm
    /// transition fires [`Event::CanaryCleared`] exactly once; alarm → different
    /// alarm fires only the new alarm's event (the episode continues, re-labeled).
    /// A run that could not conclude (`Err` — locked keychain, transient
    /// `security` failure) HOLDS the last verdict untouched: no evidence is not a
    /// verdict (the #464 no-evidence discipline).
    ///
    /// [`CanaryStatus::RefusedUnparseableCanonical`] (issue #738) is the one alarm
    /// this method does not OPEN: its durable line is per-ATTEMPT and belongs to the
    /// refuse sites, so edge-firing it here would double-log. It still CLOSES a prior
    /// bracket exactly as the quiet verdicts do — it opens nothing, so it has no
    /// episode of its own to continue (an alarm → alarm transition re-labels instead,
    /// per the paragraph above). The asymmetry is the point: a bracket that is never
    /// closed is an alarm the operator can never see end.
    pub(crate) async fn refresh_canary(
        &mut self,
        events: &mut Vec<Event>,
    ) -> Result<CanaryOutcome> {
        let outcome =
            crate::canary::run(&self.store, &self.stash, &self.roster, &self.claude_json).await?;
        let status = self.canary_status_of(outcome);
        if self.state.canary.as_ref() != Some(&status) {
            match &status {
                CanaryStatus::Drift {
                    displayed,
                    matched,
                    overridden,
                } => events.push(Event::CanaryDrift {
                    displayed: displayed.clone(),
                    matched: matched.clone(),
                    overridden: *overridden,
                }),
                CanaryStatus::Ambiguous { count } => {
                    events.push(Event::CanaryAmbiguous { count: *count });
                }
                CanaryStatus::RefusedUnparseableCanonical => {
                    // Issue #738: no event OPENS here. The durable `canary_unparseable_canonical`
                    // line is owned by the REFUSE sites (`locked_swap` here, `use_account`'s `use`
                    // path), which log one line per refused/overridden ATTEMPT; firing it again on
                    // the verdict edge would double-log the same episode. The wire verdict is this
                    // transition's own surface — strictly MORE than before, since a boot-time run
                    // now shows the refusal without waiting for a swap to be attempted.
                    //
                    // But a prior alarm's bracket MUST still be closed. This verdict replaced the
                    // `Inconclusive` that #730 mapped the case to, and that verdict DID close the
                    // bracket — so omitting it here would leave a `canary_drift` open forever once
                    // a drifted canonical is replaced by an unrelated secret. The drift/ambiguity
                    // genuinely ENDED (the canary no longer sees it), which is exactly what
                    // `CanaryCleared` asserts, so the close is honest and not merely bookkeeping.
                    close_canary_alarm_bracket(self.state.canary.as_ref(), events);
                }
                CanaryStatus::Ok | CanaryStatus::Inconclusive | CanaryStatus::NotFound => {
                    close_canary_alarm_bracket(self.state.canary.as_ref(), events);
                }
            }
            self.state.canary = Some(status);
        }
        Ok(outcome)
    }

    /// Project a typed [`CanaryOutcome`] (roster indices) onto the wire-shaped
    /// [`CanaryStatus`] (operator labels — the #15-safe handles — plus the
    /// [`canary_drift_override`](crate::config::Tunables::canary_drift_override)
    /// stamp, so `status` shows WHETHER a standing drift is currently refusing
    /// writes or riding the override).
    ///
    /// Both override tunables are read HERE rather than left to the render sites,
    /// because each decides which VERDICT the wire carries, not merely how it is
    /// drawn: a drift carries its `overridden` stamp, and the #730 unparseable-
    /// canonical case collapses back to the quiet
    /// [`Inconclusive`](CanaryStatus::Inconclusive) when
    /// [`canary_nostashmatch_override`](crate::config::Tunables::canary_nostashmatch_override)
    /// has restored the pre-#730 fail-OPEN. Severity is therefore a property of
    /// the (fault, VARIANT) pair the wire already names (#575), never something a
    /// surface re-derives.
    fn canary_status_of(&self, outcome: CanaryOutcome) -> CanaryStatus {
        match outcome {
            CanaryOutcome::Ok => CanaryStatus::Ok,
            CanaryOutcome::NotFound => CanaryStatus::NotFound,
            CanaryOutcome::Ambiguous { count } => CanaryStatus::Ambiguous { count },
            CanaryOutcome::Drift { displayed, matched } => CanaryStatus::Drift {
                displayed: self.roster[displayed].label.clone(),
                matched: self.roster[matched].label.clone(),
                overridden: self.canary_drift_override,
            },
            // Issue #738: the #730 fail-CLOSED sub-case earns its OWN verdict — the identity
            // answer is inconclusive, but the operator-visible consequence is a REFUSAL, and
            // rendering that as the quiet `inconclusive` is what #738 fixes. Gated on the
            // override precisely so the verdict never outlives the refusal it names: with the
            // override set the write proceeds, so there is nothing to refuse and the honest
            // verdict is `Inconclusive` again (the pre-#730 fail-OPEN `canary_nostashmatch_override`
            // is documented to restore). A WELL-FORMED unmatched canonical is untouched — it
            // fails OPEN regardless, exactly as in #714.
            CanaryOutcome::Inconclusive(InconclusiveReason::NoStashMatch {
                canonical_well_formed: false,
            }) if !self.canary_nostashmatch_override => CanaryStatus::RefusedUnparseableCanonical,
            CanaryOutcome::Inconclusive(_) => CanaryStatus::Inconclusive,
        }
    }

    /// Identify the active account: the roster index whose credential the
    /// canonical keychain item currently holds.
    ///
    /// Delegates to [`resolve_account_for`](Self::resolve_account_for) when the
    /// canonical is readable (token-match, then the `~/.claude.json` display
    /// fallback); when the canonical is unreadable (locked / not-found) it uses the
    /// displayed identity alone — the same json signal, the only one available
    /// without a token to match. `None` if neither resolves; the caller then polls
    /// but never swaps.
    pub(super) async fn resolve_active(&self) -> Option<usize> {
        match self.store.read().await {
            Ok(canonical) => self.resolve_account_for(&canonical).await,
            // Canonical unreadable (locked / not-found): the displayed identity is the
            // only signal left — the same display-only fallback the shared resolver's
            // step 2 uses. The daemon degrades to it here rather than swap blindly.
            Err(_) => crate::active::resolve_via_display(&self.roster, &self.claude_json),
        }
    }

    /// Detect and heal an OUT-OF-BAND canonical change (issue #13 re-auth re-stash):
    /// the operator ran `claude /login` (or the active token silently refreshed in
    /// place), rewriting the canonical credential underneath the daemon. Classify
    /// the freshly-read `canonical` against the watch; on a `Changed` verdict, find
    /// the account it now belongs to and refresh that account's stash to the new
    /// token — so a later swap AWAY and back restores the re-authenticated
    /// credential, not the stale stashed one.
    ///
    /// The watch's two-step protocol (classify, then commit) makes this re-fire
    /// safe: the baseline advances only once the re-stash SUCCEEDS, so a failure
    /// (e.g. the keychain locks mid-write) leaves the change to be re-detected and
    /// retried next cycle. After a successful re-stash the cached active index is
    /// dropped so it is re-resolved against the new canonical (a `/login` may have
    /// switched to a different account).
    ///
    /// If the re-stashed account was QUARANTINED (issue #107), the re-login also
    /// un-quarantines it immediately and emits [`Event::CredentialRestored`] — a
    /// just-re-authenticated credential is live, so it must not linger in
    /// `needs re-login` for `monitor_recovery_m` more polls. The slower
    /// M-consecutive-live-poll recovery in [`note_poll_outcome`](Self::note_poll_outcome)
    /// stays for the spontaneous-revival path (no re-login).
    pub(super) async fn reconcile_canonical_change(
        &mut self,
        canonical: &Credential,
        events: &mut Vec<Event>,
    ) {
        match self.state.canonical_watch.classify(canonical) {
            // First observation this run: prime the baseline, detect nothing.
            CanonicalChange::Primed => self.state.canonical_watch.commit(canonical),
            // No out-of-band write since we last looked.
            CanonicalChange::Unchanged => {}
            CanonicalChange::Changed => match self.resolve_account_for(canonical).await {
                Some(idx) => {
                    if self.restash_account(idx, canonical).await {
                        events.push(Event::ReStash {
                            account: self.roster[idx].label.clone(),
                        });
                        // A re-login of a quarantined account un-quarantines it ON THE
                        // SPOT (issue #107): a just-re-authenticated canonical IS a live
                        // credential, so stranding it in `needs re-login` for
                        // `monitor_recovery_m` more polls would make the durable status
                        // lie for ~a poll interval. Edge-triggered: clear the flag, drop
                        // any in-flight recovery probe, and emit `CredentialRestored`
                        // exactly once on the dead→alive transition. If the new token is
                        // somehow dead after all, the normal `monitor_401_n` path
                        // re-quarantines it. The M-consecutive-live-poll recovery in
                        // `note_poll_outcome` remains for the spontaneous-revival path (a
                        // dead ACTIVE account whose own token answers again WITHOUT a
                        // re-login).
                        if self.state.accounts[idx].health.quarantined {
                            self.state.accounts[idx].health.quarantined = false;
                            self.state.accounts[idx].health.recovery_successes = 0;
                            events.push(Event::CredentialRestored {
                                account: self.roster[idx].label.clone(),
                            });
                        }
                        // If this out-of-band change swapped AWAY from a DIFFERENT
                        // account that was mid-recovery, drop its now-frozen recovery
                        // probe (issue #108) — the daemon-notices-it-itself fallback to
                        // the `adopt_manual_swap` seam. `idx` is the swap-TO account (the
                        // new active, re-resolved below); `deactivate_recovery_probe`
                        // skips it as `next` and acts only on a departing `prev`.
                        let prev_active = self.state.active;
                        self.deactivate_recovery_probe(prev_active, Some(idx));
                        // Handled: advance the baseline so this write is not
                        // re-detected, and drop the cached active so it is
                        // re-resolved against the new canonical below.
                        self.state.canonical_watch.commit(canonical);
                        self.state.active = None;
                        // Issue #450: the departed active's `last_good` is now stale —
                        // drop it (mirrors the swap-away reset in `record_swap`).
                        self.state.last_good = None;
                    }
                    // else: the re-stash failed (e.g. a locked keychain) — do NOT
                    // commit; leave the change to re-fire and catch up next cycle.
                }
                None => {
                    // The new canonical maps to no roster account: an UN-CAPTURED login
                    // (issue #140 scope decision). SURFACE it, do NOT auto-onboard — the
                    // daemon cannot isolate this shared-item token or attribute its identity;
                    // that is the managed `sessiometer login` (#132/#134/#135) path's job. The
                    // event prompts the operator to run it. Edge-triggered by the commit below:
                    // the next `classify` sees this same blob as `Unchanged`, so it fires ONCE
                    // per distinct un-captured login, not every watch cycle. Best-effort
                    // identity: the displayed `accountUuid` when readable (a redacted, non-PII
                    // handle, like #135's post-harvest `Login` account), else omitted.
                    let account_uuid = claude_state::read_oauth_account_from(&self.claude_json)
                        .ok()
                        .map(|oauth| oauth.account_uuid().to_owned());
                    events.push(Event::UncapturedLogin { account_uuid });
                    // Committed so we do not re-surface it every cycle; nothing to re-stash.
                    self.state.canonical_watch.commit(canonical);
                    // Drop the cached active too (issue #208), mirroring the Some-branch
                    // above: the canonical now resolves to NO roster account, so a
                    // surviving stale index would make `status` show a false `*` on the
                    // now-inactive account and let `decide_action` act on a phantom
                    // active. Cleared here, the top-of-tick re-resolution finds no stash
                    // or display match and re-resolves to `None`, so `decide_action`
                    // routes to the safe `SkippedActiveUnknown` path.
                    self.state.active = None;
                    // Issue #450: the departed active's `last_good` is now stale — drop
                    // it (mirrors the swap-away reset in `record_swap`).
                    self.state.last_good = None;
                }
            },
        }
    }

    /// The canonical credential the daemon last COMMITTED to its [`CanonicalWatch`] — the
    /// baseline the external-login watch (issue #140) compares a fresh idle-time read against
    /// to detect an out-of-band `claude /login`. Snapshotted before the idle block (like the
    /// refresh exclusions) so the watch arm can distinguish an external write from the daemon's
    /// own last-committed state WITHOUT borrowing `&mut self` mid-idle. `None` until the first
    /// tick primes the watch.
    pub(crate) fn canonical_baseline(&self) -> Option<Credential> {
        self.state.canonical_watch.baseline()
    }

    /// Identify which roster account the given `canonical` credential belongs to — a
    /// thin `&self` adapter over the shared token-first resolver
    /// [`crate::active::resolve_account_for`] (canonical token byte-match, then the
    /// `~/.claude.json` display fallback). Extracted so the manual `use` swap resolves
    /// the active account the SAME way (issue #207); called here by
    /// [`resolve_active`](Self::resolve_active) and the re-auth re-stash path (#13).
    pub(super) async fn resolve_account_for(&self, canonical: &Credential) -> Option<usize> {
        crate::active::resolve_account_for(&self.roster, &self.stash, &self.claude_json, canonical)
            .await
    }

    /// Refresh account `idx`'s stash to the new `canonical` token (issue #13 re-auth
    /// re-stash), PRESERVING its `oauthAccount` identity half. The identity is taken
    /// from the existing stash if present; otherwise from `~/.claude.json` — but
    /// only when the displayed identity actually belongs to account `idx` (its
    /// `accountUuid` matches the roster entry), so a wrong identity is never stapled
    /// onto the refreshed token. Returns `false` (re-stash not performed) when no
    /// usable identity is available or the stash write fails — the caller then
    /// leaves the change to re-fire rather than committing the baseline.
    pub(super) async fn restash_account(&self, idx: usize, canonical: &Credential) -> bool {
        let account = &self.roster[idx];
        // Prefer the identity already stashed for this account: it is authoritative
        // and does not depend on the best-effort display file.
        let oauth_account = if let Ok(existing) = self.stash.read(&account.stash()).await {
            existing.oauth_account
        } else if let Ok(displayed) = claude_state::read_oauth_account_from(&self.claude_json) {
            // No existing stash: fall back to the displayed identity, but only if it
            // is THIS account's — never staple a different account's identity on.
            if account.account_uuid != displayed.account_uuid() {
                return false;
            }
            displayed
        } else {
            return false;
        };
        let refreshed = StashedAccount {
            credential: canonical.clone(),
            oauth_account,
        };
        self.stash.write(&account.stash(), &refreshed).await.is_ok()
    }

    /// Autonomously recover a SCRUBBED / empty shared canonical (issue #467) — the ADR-0018
    /// decision-1 mitigation. When Claude Code empties the shared `Claude Code-credentials` item on
    /// its first `invalid_grant` (the fleet-wide "Not logged in" lockout, ADR-0018), the daemon
    /// installs a VIABLE roster account's token back into the canonical via [`swap::adopt_target`],
    /// so every live `claude` session re-reads a usable credential on its next request — no operator
    /// `claude /login`.
    ///
    /// The narrow carve-out from ADR-0007 decision 4: recovery for a scrubbed canonical is
    /// otherwise `use --force`-gated and the autonomous daemon never adopts; this relaxes the gate
    /// ONLY for the scrubbed-**with-a-live-target** case. A genuinely-all-dead roster (no viable
    /// target) is NOT this case — it returns `None` and falls through to the existing
    /// `active_dead_no_target` / surfaced scrub signal, which still needs a manual `/login`
    /// (ADR-0007 decision 4 / ADR-0016), never a silent adopt churn.
    ///
    /// Target selection mirrors [`emergency_swap`](Self::emergency_swap): [`pick_target`] with the
    /// weekly-viability filter but the session gate and reserve bypassed (`f64::INFINITY` / `None`) —
    /// liveness beats session headroom when the whole fleet is locked out. The active account is
    /// EXCLUDED (`pick_target`'s always-on `i != active`): a scrubbed active is polled through the
    /// now-empty canonical, so its reading is unreliable, whereas a spare is polled through its OWN
    /// stash and is therefore a KNOWN-live token to adopt. An UNRESOLVED active
    /// (`usize::MAX` sentinel — no roster index equals it) excludes nothing, so every account is a
    /// candidate.
    ///
    /// BOUNDED against a re-auth thrash loop: at most [`SCRUB_ADOPT_MAX`] LANDED adopts per
    /// [`SCRUB_ADOPT_WINDOW`]. On the bound the daemon backs off — emits one edge-triggered
    /// [`Event::CanonicalRecoveryExhausted`] and holds — leaving the `canonical_scrubbed` signal up
    /// for the operator (status / menubar, #469) rather than churning. The window ages out on its
    /// own clock, so an isolated scrub an hour later opens a fresh episode and heals at once.
    ///
    /// Returns `Some(TickAction::CanonicalAdopted { to })` on a landed adopt (this cycle's decision
    /// IS the recovery), else `None` to fall through to [`decide_action`](Self::decide_action).
    pub(super) async fn recover_scrubbed_canonical(
        &mut self,
        active: Option<usize>,
        readings: &[Option<Usage>],
        at: Instant,
        events: &mut Vec<Event>,
    ) -> Option<TickAction> {
        // Age out the churn window: once the FIRST adopt of an episode is older than the window, open
        // a fresh episode (counter + back-off latch reset), so an isolated scrub later heals at once.
        // Elapsing is the ONLY reset — deliberately NOT an observed recovery (a top-of-tick canonical
        // Present): under a SLOW re-scrub churn (each adopt survives a poll or two before CC re-scrubs)
        // a reset-on-Present would clear the counter every episode and defeat the bound — the exact
        // re-auth thrash AC4/#467 exists to cap.
        if let Some(start) = self.state.scrub_adopt_window_start {
            if at.saturating_duration_since(start) >= SCRUB_ADOPT_WINDOW {
                self.state.scrub_adopt_count = 0;
                self.state.scrub_adopt_window_start = None;
                self.state.signaled_scrub_adopt_exhausted = false;
            }
        }

        // Bound reached: BACK OFF rather than thrash the re-auth loop. Emit the durable back-off
        // signal ONCE per episode (edge-triggered) and fall through — `canonical_scrubbed` already
        // surfaces the stuck state for the operator (#469).
        if self.state.scrub_adopt_count >= SCRUB_ADOPT_MAX {
            if !self.state.signaled_scrub_adopt_exhausted {
                events.push(Event::CanonicalRecoveryExhausted {
                    account: active.map(|i| self.roster[i].label.clone()),
                });
                self.state.signaled_scrub_adopt_exhausted = true;
            }
            return None;
        }

        // Pick a VIABLE target with the emergency-path filter (mirroring `emergency_swap`): the
        // weekly-exhaustion + enabled + not-active filter, but the session gate and reserve bypassed
        // (`f64::INFINITY` / `None`) — the whole fleet is locked out, so liveness beats headroom. No
        // viable target → `None`, falling through to the surfaced-signal path (never a churn).
        //
        // Issue #607 EXEMPTION (same rationale as `emergency_swap`, which this mirrors): RAW ceiling
        // and SYMMETRIC `draw`, both widening the admissible target set. A scrubbed canonical locks
        // out the whole fleet, so adopting a live token that must rotate again shortly strictly
        // beats leaving every session unauthenticated.
        let weekly_ceiling = self.weekly_ceiling_strategy.draw(
            &mut self.rng,
            WEEKLY_CEILING_PCT_LO,
            WEEKLY_CEILING_PCT_HI,
        ) / 100.0;
        let target_idx = pick_target_ranked(
            active.unwrap_or(usize::MAX),
            readings,
            &self.enabled_mask(),
            None,
            f64::INFINITY,
            weekly_ceiling,
            // Enhanced selection (issue #612): disperse the fleet-locked scrub-recovery target and
            // prefer a calmer peer.
            self.selection_tiebreak(),
        )?;

        // Install the target into the scrubbed canonical, lock-wrapped (#64). SAFETY holds inside the
        // engine (ADR-0003): a LOCKED / unreadable keychain aborts with ZERO writes ("locked ≠
        // gone"), the incoming stash is read before any mutation, and the canonical write is the
        // atomic `-U` upsert (a concurrent reader sees the empty item then the adopted credential,
        // never a torn blob). A concurrent WRITER — a `claude /login` landing a live token in the
        // sub-tick window — is overwritten here by the known-live target: accepted last-writer-wins
        // (ADR-0003 reconcile; ADR-0018 is reactive, not preventive), harmless as the fleet stays live
        // and the window is a single tick's synchronous ms. #6 no-half-swap: a lock-busy / write error
        // leaves the canonical un-torn and is retried next cycle — do NOT count it toward the bound (no
        // adopt landed) and fall through to the normal decision this tick.
        let incoming = self.roster[target_idx].stash();
        match self.locked_adopt(&incoming).await {
            Ok(_report) => {
                // Adopt the swapped-in account exactly as a swap does: set it active, arm the
                // post-swap cooldown, drop the departed pre-blind anchor, and COMMIT the write to the
                // canonical_watch so the daemon's OWN adopt is not re-detected as an out-of-band
                // `/login` (issue #13).
                self.record_swap(target_idx, &incoming, at).await;
                if self.state.scrub_adopt_window_start.is_none() {
                    self.state.scrub_adopt_window_start = Some(at);
                }
                self.state.scrub_adopt_count += 1;
                events.push(Event::CanonicalRecovered {
                    account: self.roster[target_idx].label.clone(),
                });
                Some(TickAction::CanonicalAdopted { to: target_idx })
            }
            Err(_) => None,
        }
    }

    /// Record the canonical `Claude Code-credentials` item's OWN per-poll liveness (issue #464)
    /// and edge-trigger its durable scrub / recovery events — the shared-credential observability
    /// umbrella #463 needs to make the fleet-wide "Not logged in" scrub visible and measurable.
    ///
    /// `canonical` is the blob read ONCE at top-of-tick (`None` when unreadable); `absent`
    /// distinguishes a CONFIRMED gone item ([`Error::CredentialNotFound`]) from a transient read
    /// failure, so a flaky read classifies [`CanonicalLiveness::Unknown`] (no event, hold the
    /// signal) rather than a false scrub. `active` supplies the handle — on a scrub the last-known
    /// active account is the one Claude Code emptied for everyone.
    ///
    /// Two outputs: (1) a `diag=canonical` LEVEL line every poll — the fingerprint series +
    /// present/scrubbed reading #465/#467 consume; (2) on a present↔scrubbed transition, one
    /// durable [`Event::CanonicalScrubbed`] / [`Event::CanonicalRestored`]. Non-secret by
    /// construction: a liveness discriminant, a hash-prefix fingerprint, a handle, and a timestamp
    /// — never a token or email (issue #15). Present/empty and the fingerprint both key off the
    /// single audited [`crate::refresh::refresh_token`] extractor — the same discipline
    /// [`has_live_refresh_token`] follows — so the emptiness rule lives in one place.
    ///
    /// RETURNS the classified [`CanonicalLiveness`] so the tick can react to a `Scrubbed` reading —
    /// the autonomous adopt-target recovery (issue #467) heals a scrubbed canonical when a viable
    /// target exists, off the same single audited emptiness rule this uses for the edge trigger.
    pub(super) fn note_canonical_liveness(
        &mut self,
        canonical: Option<&Credential>,
        absent: bool,
        active: Option<usize>,
        events: &mut Vec<Event>,
        diagnostics: &mut Vec<Diagnostic>,
    ) -> CanonicalLiveness {
        let handle = active.map(|i| self.roster[i].label.clone());
        let (state, fingerprint, expires_at) = match canonical {
            Some(cred) => {
                let blob = cred.expose();
                match crate::refresh::refresh_token(blob) {
                    // A live (non-empty) refresh token — a usable shared credential.
                    Some(rt) if !rt.is_empty() => (
                        CanonicalLiveness::Present,
                        Some(canonical_fingerprint(&rt)),
                        crate::refresh::expires_at(blob).map(millis_to_secs),
                    ),
                    // A present-but-EMPTY refresh token (`Some("")`): the tokens were cleared in
                    // place — the dead signal per `refresh::refresh_token`. Claude Code's observed
                    // scrub empties the whole ITEM (→ `CredentialNotFound` below, ADR-0018); this
                    // arm defensively catches an in-place clear too. No live token to fingerprint.
                    Some(_) => (CanonicalLiveness::Scrubbed, None, None),
                    // An unparseable / non-`claudeAiOauth` blob (`refresh_token` → `None`): the
                    // item is present but its liveness cannot be determined — honestly UNKNOWN, not
                    // a confirmed scrub (a corrupt read must never fabricate a scrub edge).
                    None => (CanonicalLiveness::Unknown, None, None),
                }
            }
            // The item is GONE (`CredentialNotFound`, exit 44) — the confirmed scrub Claude Code's
            // `invalid_grant` empties the item into (ADR-0018), and the exact signal #467 adopts on.
            None if absent => (CanonicalLiveness::Scrubbed, None, None),
            // Transient / unreadable for a non-lock, non-not-found reason — no evidence this poll.
            None => (CanonicalLiveness::Unknown, None, None),
        };

        // Rotation-YANK detection (issue #475): a Present→Present canonical fingerprint change means
        // the shared item ROTATED under any mid-flight sessions — the RECOVERABLE "Not logged in"
        // mode (they re-read the still-live item on `continue`, no `/login`), distinct from the
        // UNRECOVERABLE scrub below. Derived purely from the observed present/valid state + the
        // fingerprint delta (AC1: "not guessed"). The anchor is advanced ONLY here — never by the
        // daemon's own swap / keep-warm canonical writes (UNLIKE `canonical_watch`) — so a
        // self-authored rotation is still marked, keeping the yank series the full canonical-rotation
        // denominator #465 measures.
        let rotated_from = match (state, &fingerprint) {
            (CanonicalLiveness::Present, Some(fp)) => {
                // Advance the anchor; if a DIFFERENT fingerprint was anchored, mark the yank carrying
                // the PRIOR fingerprint. The first observation (anchor `None`) seeds silently.
                match self.state.prev_canonical_fingerprint.replace(fp.clone()) {
                    Some(prev) if prev != *fp => Some(prev),
                    _ => None,
                }
            }
            // A scrub CLEARS the anchor: a rotation spanning a scrub is a scrub + recovery, not a
            // yank — the `canonical_restored` edge marks the recovery, re-seeding on the next Present.
            (CanonicalLiveness::Scrubbed, _) => {
                self.state.prev_canonical_fingerprint = None;
                None
            }
            // Unknown (or a present blob with no parseable fingerprint): no evidence — HOLD the anchor
            // and mark nothing, the same "a flaky read carries no signal" hold the scrub edge uses.
            _ => None,
        };

        // (1) The per-poll LEVEL record on the diagnostic channel (issue #464): every poll, so the
        // fingerprint series + present/scrubbed reading are measurable from the log alone. On a
        // rotation, the additive `mode=yank prev=…` marker (issue #475) rides this same line.
        diagnostics.push(Diagnostic::Canonical {
            state,
            fingerprint,
            account: handle.clone(),
            expires_at,
            rotated_from,
        });

        // (2) The durable, EDGE-triggered transition events (issue #464). A transient UNKNOWN
        // carries no evidence — hold the current signal rather than fabricate a scrub or recovery.
        match state {
            CanonicalLiveness::Scrubbed => {
                if !self.state.signaled_canonical_scrubbed {
                    events.push(Event::CanonicalScrubbed { account: handle });
                    self.state.signaled_canonical_scrubbed = true;
                }
            }
            CanonicalLiveness::Present => {
                if self.state.signaled_canonical_scrubbed {
                    events.push(Event::CanonicalRestored { account: handle });
                    self.state.signaled_canonical_scrubbed = false;
                }
            }
            CanonicalLiveness::Unknown => {}
        }

        state
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::daemon::tests::*;

    // --- locked keychain & re-auth re-stash (issue #13) --------------------

    #[tokio::test]
    async fn a_locked_keychain_defers_the_whole_tick_and_signals_once() {
        // #13: a locked keychain defers the ENTIRE cycle — no resolve, no poll, no
        // swap — emits ONE edge-triggered keychain_locked_wait, and returns a
        // back-off as the next wait. The daemon never auto-unlocks or prompts; the
        // back-off is the whole response. A is set over the session trigger so that,
        // absent the lock, this cycle WOULD swap — proving the lock truly defers it.
        let roster = vec![account("u-A", "work"), account("u-B", "spare")];
        let store = store_holding(b"A-token").await;
        let stash = stash_with(&[
            ("Sessiometer/u-A", b"A-token", "u-A"),
            ("Sessiometer/u-B", b"B-token", "u-B"),
        ])
        .await;
        let (_dir, json) = claude_json("u-A");
        let poller = FakeRosterPoller::new()
            .ok("u-A", 0.97, 0.40)
            .ok("u-B", 0.10, 0.10);
        let tun = tunables(95, 80, 0);
        let mut daemon: FakeDaemon = Daemon::new(
            roster,
            poller,
            store,
            stash,
            FakeClock::frozen(),
            json,
            &tun,
        );

        daemon.store.set_locked(true);

        let first = daemon.tick().await;
        assert_eq!(first.action, TickAction::KeychainLocked);
        // One lock-wait event on the FIRST locked cycle (edge-triggered)…
        assert_eq!(first.events, vec![Event::KeychainLockedWait]);
        // …with the back-off starting at the base.
        assert_eq!(first.next_wait, Some(LOCK_BACKOFF_BASE));
        // The cycle deferred before resolving: no active account, no swap.
        assert_eq!(daemon.state.active, None);
        // status still answers — the roster is listed, every reading absent.
        assert_eq!(first.snapshot.accounts.len(), 2);
        assert!(first.snapshot.accounts.iter().all(|a| a.usage.is_none()));
        // Diagnostic channel (#77): a locked tick polls NOTHING (it short-circuits
        // before the poll loop), so there are NO per-poll lines — just the decision
        // line naming the deferral and the back-off wait it imposed.
        assert_eq!(
            first.diagnostics,
            vec![Diagnostic::Tick {
                decision: DecisionClass::KeychainLocked,
                backoff_secs: Some(LOCK_BACKOFF_BASE.as_secs()),
                retry_after_secs: None,
            }],
        );

        // A second locked cycle is SILENT (edge-triggered) and the back-off grows.
        let second = daemon.tick().await;
        assert_eq!(second.action, TickAction::KeychainLocked);
        assert!(
            second.events.is_empty(),
            "the lock signal is edge-triggered"
        );
        assert_eq!(second.next_wait, Some(LOCK_BACKOFF_BASE * 2));

        // The canonical was never written (no auto-unlock, no swap): once the lock
        // clears, it still holds A's original token.
        daemon.store.set_locked(false);
        assert!(daemon
            .store
            .read()
            .await
            .unwrap()
            .matches(&cred(b"A-token")));
    }

    #[tokio::test]
    async fn the_locked_keychain_back_off_doubles_then_caps() {
        // #13: the deferred-cycle back-off grows exponentially from the base and
        // saturates at the cap, so a long lock settles at one retry per cap-interval
        // rather than spinning or growing without bound.
        let roster = vec![account("u-A", "work")];
        let store = store_holding(b"A-token").await;
        let stash = stash_with(&[("Sessiometer/u-A", b"A-token", "u-A")]).await;
        let (_dir, json) = claude_json("u-A");
        let poller = FakeRosterPoller::new().ok("u-A", 0.10, 0.10);
        let tun = tunables(95, 80, 0);
        let mut daemon: FakeDaemon = Daemon::new(
            roster,
            poller,
            store,
            stash,
            FakeClock::frozen(),
            json,
            &tun,
        );

        daemon.store.set_locked(true);
        let mut waits = Vec::new();
        for _ in 0..8 {
            waits.push(daemon.tick().await.next_wait.unwrap());
        }
        // Doubling from the 1 s base, capped at the 60 s ceiling:
        // 1, 2, 4, 8, 16, 32, then 64→capped 60, then 60.
        assert_eq!(
            waits,
            vec![
                LOCK_BACKOFF_BASE,
                LOCK_BACKOFF_BASE * 2,
                LOCK_BACKOFF_BASE * 4,
                LOCK_BACKOFF_BASE * 8,
                LOCK_BACKOFF_BASE * 16,
                LOCK_BACKOFF_BASE * 32,
                LOCK_BACKOFF_CAP, // 64 s would exceed the cap → clamped
                LOCK_BACKOFF_CAP,
            ]
        );
    }

    #[tokio::test]
    async fn unlocking_the_keychain_resumes_normal_ticks_and_rearms_the_signal() {
        // #13: after a lock episode, the first readable cycle clears the back-off
        // (next_wait None → normal interval) and re-arms the edge-trigger, so a
        // LATER lock episode signals afresh and restarts the back-off at the base.
        let roster = vec![account("u-A", "work"), account("u-B", "spare")];
        let store = store_holding(b"A-token").await;
        let stash = stash_with(&[
            ("Sessiometer/u-A", b"A-token", "u-A"),
            ("Sessiometer/u-B", b"B-token", "u-B"),
        ])
        .await;
        let (_dir, json) = claude_json("u-A");
        let poller = FakeRosterPoller::new()
            .ok("u-A", 0.10, 0.10)
            .ok("u-B", 0.10, 0.10);
        let tun = tunables(95, 80, 0);
        let mut daemon: FakeDaemon = Daemon::new(
            roster,
            poller,
            store,
            stash,
            FakeClock::frozen(),
            json,
            &tun,
        );

        daemon.store.set_locked(true);
        let locked = daemon.tick().await;
        assert_eq!(locked.action, TickAction::KeychainLocked);
        assert_eq!(locked.events, vec![Event::KeychainLockedWait]);

        // Unlock: the next cycle reads normally, resolves the active account, holds,
        // and restores the normal interval.
        daemon.store.set_locked(false);
        let resumed = daemon.tick().await;
        assert_eq!(resumed.action, TickAction::Held);
        assert_eq!(resumed.next_wait, None);
        assert_eq!(daemon.state.active, Some(0));

        // A second lock episode signals again (the readable cycle re-armed the edge)
        // and the back-off restarts at the base, not where the first episode left off.
        daemon.store.set_locked(true);
        let relocked = daemon.tick().await;
        assert_eq!(relocked.events, vec![Event::KeychainLockedWait]);
        assert_eq!(relocked.next_wait, Some(LOCK_BACKOFF_BASE));
    }

    // --- reconcile-on-start ------------------------------------------------

    #[tokio::test]
    async fn reconcile_co_writes_the_matched_account_when_the_display_is_stale() {
        // Post-swap crash: canonical holds B's token, but the display still shows
        // A (the co-write never landed). Reconcile heals the display to B.
        let roster = vec![account("u-A", "work"), account("u-B", "spare")];
        let store = store_holding(b"B-token").await;
        let stash = stash_with(&[
            ("Sessiometer/u-A", b"A-token", "u-A"),
            ("Sessiometer/u-B", b"B-token", "u-B"),
        ])
        .await;
        let (_dir, json) = claude_json("u-A"); // stale display
        let tun = tunables(95, 80, 0);
        let daemon: FakeDaemon = Daemon::new(
            roster,
            FakeRosterPoller::new(),
            store,
            stash,
            FakeClock::frozen(),
            json.clone(),
            &tun,
        );

        daemon.reconcile_on_start().await.unwrap();
        assert_eq!(displayed_uuid(&json).as_deref(), Some("u-B"));
    }

    #[tokio::test]
    async fn reconcile_leaves_the_display_untouched_when_no_stash_matches() {
        // Normal restart: the active account's token has drifted (refreshed in
        // place), matching no stash. The display is already correct → untouched.
        let roster = vec![account("u-A", "work")];
        let store = store_holding(b"A-drifted-token").await;
        let stash = stash_with(&[("Sessiometer/u-A", b"A-old-token", "u-A")]).await;
        let (_dir, json) = claude_json("u-A");
        let tun = tunables(95, 80, 0);
        let daemon: FakeDaemon = Daemon::new(
            roster,
            FakeRosterPoller::new(),
            store,
            stash,
            FakeClock::frozen(),
            json.clone(),
            &tun,
        );

        daemon.reconcile_on_start().await.unwrap();
        assert_eq!(displayed_uuid(&json).as_deref(), Some("u-A"));
    }

    #[tokio::test]
    async fn reconcile_is_a_noop_when_the_display_already_matches() {
        let roster = vec![account("u-A", "work")];
        let store = store_holding(b"A-token").await;
        let stash = stash_with(&[("Sessiometer/u-A", b"A-token", "u-A")]).await;
        let (_dir, json) = claude_json("u-A");
        let tun = tunables(95, 80, 0);
        let daemon: FakeDaemon = Daemon::new(
            roster,
            FakeRosterPoller::new(),
            store,
            stash,
            FakeClock::frozen(),
            json.clone(),
            &tun,
        );

        daemon.reconcile_on_start().await.unwrap();
        assert_eq!(displayed_uuid(&json).as_deref(), Some("u-A"));
    }

    #[test]
    fn canonical_fingerprint_is_the_16_hex_prefix_of_the_token_sha256() {
        // Issue #464: a stable, redaction-safe identity — the first 16 hex of the token's
        // SHA-256, deterministic and distinct per token, never the token itself. 16 chars keeps
        // it under the redaction meter's 20-char high-entropy backstop.
        let fp = canonical_fingerprint(b"live-rt");
        assert_eq!(fp.len(), CANONICAL_FINGERPRINT_HEX);
        assert_eq!(fp, crate::sha256::sha256_hex(b"live-rt")[..16]);
        assert!(fp.chars().all(|c| c.is_ascii_hexdigit()));
        // A different token yields a different fingerprint (identity, not a constant) — the
        // rotation signal #465 reads.
        assert_ne!(fp, canonical_fingerprint(b"other-rt"));
    }

    #[tokio::test]
    async fn note_canonical_liveness_edge_triggers_the_scrub_once_then_the_restore() {
        // Issue #464: the shared canonical's present↔scrubbed transitions each emit EXACTLY ONE
        // durable event — the scrub fires once (not per poll while it stays empty), a transient
        // unreadable poll HOLDS the signal, and only a confirmed live read fires the clearing
        // restore. The core edge-trigger AC.
        let mut daemon = three_account_daemon(FakeRosterPoller::new()).await;
        let live = cred(
            br#"{"claudeAiOauth":{"accessToken":"a","refreshToken":"live-rt","expiresAt":1782777600000}}"#,
        );

        // Present while nothing is signalled: no event, one `diag=canonical` present line.
        let mut events = Vec::new();
        let mut diags = Vec::new();
        daemon.note_canonical_liveness(Some(&live), false, Some(0), &mut events, &mut diags);
        assert!(
            events.is_empty(),
            "a present item with no prior scrub emits no event: {events:?}"
        );
        // The Present arm's field population through the METHOD: the fingerprint derived from the
        // live refresh token and the `expiresAt` ms→s fold (1782777600000 ms → 1782777600 s). This
        // is the FIRST Present observation, so it SEEDS the yank anchor silently (`rotated_from:
        // None` — no rotation to mark).
        assert_eq!(
            diags,
            vec![Diagnostic::Canonical {
                state: CanonicalLiveness::Present,
                fingerprint: Some(canonical_fingerprint(b"live-rt")),
                account: Some("work".to_owned()),
                expires_at: Some(1_782_777_600),
                rotated_from: None,
            }]
        );
        assert!(!daemon.state.signaled_canonical_scrubbed);
        assert_eq!(
            daemon.state.prev_canonical_fingerprint,
            Some(canonical_fingerprint(b"live-rt")),
            "the first Present observation seeds the yank anchor"
        );

        // The item is scrubbed (gone) → exactly one `canonical_scrubbed` carrying the handle.
        let mut events = Vec::new();
        daemon.note_canonical_liveness(None, true, Some(0), &mut events, &mut Vec::new());
        assert_eq!(
            events,
            vec![Event::CanonicalScrubbed {
                account: Some("work".to_owned())
            }]
        );
        assert!(daemon.state.signaled_canonical_scrubbed);

        // Still scrubbed next poll → no repeat (edge-triggered, not level-triggered).
        let mut events = Vec::new();
        daemon.note_canonical_liveness(None, true, Some(0), &mut events, &mut Vec::new());
        assert!(
            events.is_empty(),
            "a persisting scrub re-signals nothing: {events:?}"
        );
        assert!(daemon.state.signaled_canonical_scrubbed);

        // A transient unreadable poll (absent=false) carries no evidence → no event, signal HELD:
        // a flaky read must never fabricate a recovery.
        let mut events = Vec::new();
        let mut diags = Vec::new();
        daemon.note_canonical_liveness(None, false, Some(0), &mut events, &mut diags);
        assert!(
            events.is_empty(),
            "a flaky read fabricates no recovery: {events:?}"
        );
        assert!(
            daemon.state.signaled_canonical_scrubbed,
            "the scrub signal survives a transient read"
        );
        assert_eq!(
            diags.len(),
            1,
            "the unknown level reading is still recorded"
        );

        // A confirmed live read → exactly one `canonical_restored`, signal cleared.
        let mut events = Vec::new();
        daemon.note_canonical_liveness(Some(&live), false, Some(0), &mut events, &mut Vec::new());
        assert_eq!(
            events,
            vec![Event::CanonicalRestored {
                account: Some("work".to_owned())
            }]
        );
        assert!(!daemon.state.signaled_canonical_scrubbed);
    }

    #[tokio::test]
    async fn note_canonical_liveness_treats_an_emptied_refresh_token_as_scrubbed() {
        // Issue #464: Claude Code's in-place scrub clears the tokens rather than deleting the
        // item — a readable blob with an EMPTY refresh token is the DEAD signal (refresh.rs), so
        // it must classify scrubbed and edge-trigger the event just like a gone item, and the
        // level line records the scrubbed state with no fingerprint / expiry.
        let mut daemon = three_account_daemon(FakeRosterPoller::new()).await;
        let emptied =
            cred(br#"{"claudeAiOauth":{"accessToken":"","refreshToken":"","expiresAt":0}}"#);
        let mut events = Vec::new();
        let mut diags = Vec::new();
        daemon.note_canonical_liveness(Some(&emptied), false, Some(0), &mut events, &mut diags);
        assert_eq!(
            events,
            vec![Event::CanonicalScrubbed {
                account: Some("work".to_owned())
            }]
        );
        assert_eq!(
            diags,
            vec![Diagnostic::Canonical {
                state: CanonicalLiveness::Scrubbed,
                fingerprint: None,
                account: Some("work".to_owned()),
                expires_at: None,
                rotated_from: None,
            }]
        );
    }

    #[tokio::test]
    async fn tick_adopts_a_viable_target_into_a_scrubbed_canonical() {
        // Issue #467 AC1: an emptied canonical with a viable roster account → the daemon installs
        // that account's token and emits a recovery event, with NO operator action — so a live
        // session recovers on its next request. The narrow ADR-0007 d4 carve-out (ADR-0018 d1): a
        // scrubbed canonical WITH a live target is not `active_dead_no_target`.
        let poller = FakeRosterPoller::new()
            .ok("u-A", 0.50, 0.40) // active, below trigger → warm-up holds, no swap
            .ok("u-B", 0.10, 0.20) // viable spare — earliest index, so the pick
            .ok("u-C", 0.15, 0.25); // viable spare
        let mut daemon = three_account_daemon(poller).await;
        // Warm-up runs on the opaque canonical (`refresh_token` can't parse `b"A-token"` → liveness
        // UNKNOWN, never Scrubbed), so the recovery branch is NOT taken and nothing is adopted.
        let warm = warmed_tick(&mut daemon).await;
        assert!(
            !matches!(warm.action, TickAction::CanonicalAdopted { .. }),
            "an UNKNOWN-liveness (non-scrubbed) canonical never triggers an adopt: {:?}",
            warm.action
        );
        assert_eq!(daemon.state.active, Some(0));

        // Claude Code scrubs the shared canonical to empty on its first `invalid_grant` (ADR-0018).
        daemon.store.set_not_found(true);
        let outcome = daemon.tick().await;

        assert_eq!(outcome.action, TickAction::CanonicalAdopted { to: 1 });
        assert!(
            outcome.events.contains(&Event::CanonicalRecovered {
                account: "spare".to_owned()
            }),
            "the autonomous recovery emits a durable event naming the adopted account: {:?}",
            outcome.events
        );
        // The scrub itself is still recorded (the fleet-wide lockout event), even though brief.
        assert!(outcome.events.contains(&Event::CanonicalScrubbed {
            account: Some("work".to_owned())
        }));
        // The canonical now holds the adopted spare's token, so every session re-reads a usable
        // credential on its next request — no `claude /login`.
        assert!(daemon
            .store
            .read()
            .await
            .unwrap()
            .matches(&cred(b"B-token")));
        assert_eq!(
            daemon.state.active,
            Some(1),
            "the adopted account is now active"
        );
    }

    #[tokio::test]
    async fn tick_does_not_adopt_a_scrubbed_canonical_when_no_target_is_viable() {
        // Issue #467 AC2: no viable target → fall THROUGH to the existing decision path (the surfaced
        // signal), never a silent adopt churn. Here every spare is weekly-exhausted, so `pick_target`
        // finds nothing and the recovery yields to the normal `decide_action` (Held) — the canonical
        // stays scrubbed (zero adopt writes) and the durable `canonical_scrubbed` signal stands.
        let poller = FakeRosterPoller::new()
            .ok("u-A", 0.50, 0.40) // active, viable, below trigger
            .ok("u-B", 0.10, 0.99) // weekly-EXHAUSTED (> 0.98 trigger) → not a viable target
            .ok("u-C", 0.15, 0.99); // weekly-EXHAUSTED → not a viable target
        let mut daemon = three_account_daemon(poller).await;
        warmed_tick(&mut daemon).await;
        assert_eq!(daemon.state.active, Some(0));

        daemon.store.set_not_found(true);
        let outcome = daemon.tick().await;

        assert!(
            !matches!(outcome.action, TickAction::CanonicalAdopted { .. }),
            "no viable target → no adopt: {:?}",
            outcome.action
        );
        assert!(
            !outcome
                .events
                .iter()
                .any(|e| matches!(e, Event::CanonicalRecovered { .. })),
            "no recovery event when nothing was adopted: {:?}",
            outcome.events
        );
        // The scrub IS surfaced (the durable signal the operator acts on), not swallowed.
        assert!(outcome.events.contains(&Event::CanonicalScrubbed {
            account: Some("work".to_owned())
        }));
        // Zero adopt writes: the canonical stays scrubbed (no thrash) until a viable target appears
        // or the operator re-logs-in (ADR-0007 d4 / ADR-0016 remedy for the all-dead case).
        assert!(
            matches!(daemon.store.read().await, Err(Error::CredentialNotFound)),
            "the canonical is left scrubbed — no adopt write"
        );
        assert_eq!(
            daemon.state.active,
            Some(0),
            "no state change without an adopt"
        );
    }

    #[tokio::test]
    async fn recover_scrubbed_canonical_aborts_with_zero_writes_when_unreadable() {
        // Issue #467 AC3 (no ADR-0003 regression): a LOCKED / unreadable keychain is "could not
        // read", NOT "gone" — the adopt MUST abort with ZERO writes rather than clobber a canonical
        // it could not read. Driven at the daemon layer by calling the recovery directly against an
        // unreadable store; the engine-level matrix (locked / unreadable / absent-stash) is proven
        // in `swap::tests::adopt_target_aborts_with_zero_writes_*`.
        let poller = FakeRosterPoller::new()
            .ok("u-A", 0.50, 0.40)
            .ok("u-B", 0.10, 0.20) // a viable target exists — so only the unreadable canonical aborts
            .ok("u-C", 0.15, 0.25);
        let mut daemon = three_account_daemon(poller).await;
        warmed_tick(&mut daemon).await;
        let readings = daemon.decision_readings(Some(0));
        let at = daemon.clock.now();

        daemon.store.set_unreadable(true);
        let mut events = Vec::new();
        let outcome = daemon
            .recover_scrubbed_canonical(Some(0), &readings, at, &mut events)
            .await;

        assert_eq!(outcome, None, "an unreadable canonical aborts the adopt");
        assert!(
            events.is_empty(),
            "no false recovery event on an aborted adopt: {events:?}"
        );
        assert_eq!(
            daemon.state.scrub_adopt_count, 0,
            "an adopt that never landed is not counted toward the churn bound"
        );
        assert_eq!(
            daemon.state.active,
            Some(0),
            "no state change on an aborted adopt"
        );
        // Clearing the unreadable flag shows the canonical still holds the PRE-adopt token — the
        // abort wrote nothing (ADR-0003 / #212 "locked ≠ gone").
        daemon.store.set_unreadable(false);
        assert!(
            daemon
                .store
                .read()
                .await
                .unwrap()
                .matches(&cred(b"A-token")),
            "zero writes: the canonical is untouched"
        );
    }

    #[tokio::test]
    async fn scrubbed_canonical_recovery_is_bounded_then_resumes_after_the_window() {
        // Issue #467 AC4: the recovery is BOUNDED against a re-auth churn loop. When the canonical
        // keeps getting re-scrubbed right after each adopt, the daemon heals at most SCRUB_ADOPT_MAX
        // times per window, then BACKS OFF (one durable `canonical_recovery_exhausted`, no more
        // adopts) — leaving the scrub signal up for the operator — and RESUMES once the window
        // elapses. A frozen clock holds every tick inside one window until we advance it.
        let poller = FakeRosterPoller::new()
            .ok("u-A", 0.20, 0.30)
            .ok("u-B", 0.10, 0.20)
            .ok("u-C", 0.15, 0.25); // all viable, all below trigger → a target always exists
        let mut daemon = three_account_daemon(poller).await;
        warmed_tick(&mut daemon).await;

        // Up to the bound: each re-scrub heals (a different viable account each time as the active
        // rotates — `pick_target` excludes the current active).
        let mut adopts = 0;
        for _ in 0..SCRUB_ADOPT_MAX {
            daemon.store.set_not_found(true);
            let outcome = daemon.tick().await;
            if matches!(outcome.action, TickAction::CanonicalAdopted { .. }) {
                adopts += 1;
                assert!(
                    outcome
                        .events
                        .iter()
                        .any(|e| matches!(e, Event::CanonicalRecovered { .. })),
                    "each landed adopt emits a recovery event"
                );
            }
        }
        assert_eq!(
            adopts, SCRUB_ADOPT_MAX,
            "every scrub within the bound is healed"
        );
        assert_eq!(daemon.state.scrub_adopt_count, SCRUB_ADOPT_MAX);

        // The (MAX+1)th re-scrub in the same window BACKS OFF: no adopt, one back-off signal.
        daemon.store.set_not_found(true);
        let backoff = daemon.tick().await;
        assert!(
            !matches!(backoff.action, TickAction::CanonicalAdopted { .. }),
            "the churn bound stops the adopt: {:?}",
            backoff.action
        );
        assert!(
            backoff
                .events
                .iter()
                .any(|e| matches!(e, Event::CanonicalRecoveryExhausted { .. })),
            "the back-off is surfaced durably: {:?}",
            backoff.events
        );

        // A further re-scrub in the same window stays backed off AND does not re-emit (edge-triggered).
        daemon.store.set_not_found(true);
        let still = daemon.tick().await;
        assert!(!matches!(still.action, TickAction::CanonicalAdopted { .. }));
        assert!(
            !still
                .events
                .iter()
                .any(|e| matches!(e, Event::CanonicalRecoveryExhausted { .. })),
            "the back-off signal is edge-triggered, not repeated per held tick"
        );

        // Once the churn window elapses, recovery RESUMES — an isolated later scrub heals at once.
        daemon
            .clock
            .advance(SCRUB_ADOPT_WINDOW + Duration::from_secs(1));
        daemon.store.set_not_found(true);
        let resumed = daemon.tick().await;
        assert!(
            matches!(resumed.action, TickAction::CanonicalAdopted { .. }),
            "recovery resumes after the window resets: {:?}",
            resumed.action
        );
    }

    /// Extract the #475 yank marker (`rotated_from`) from a `diag=canonical` diagnostic, panicking
    /// on any other variant — a focused reader for the yank-detection assertions below.
    fn rotated_from_of(d: &Diagnostic) -> Option<String> {
        match d {
            Diagnostic::Canonical { rotated_from, .. } => rotated_from.clone(),
            other => panic!("expected a diag=canonical, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn note_canonical_liveness_marks_a_present_to_present_rotation_as_a_yank() {
        // Issue #475: a Present→Present canonical fingerprint CHANGE is a rotation-YANK — the
        // frequent, RECOVERABLE "Not logged in" mode. The FIRST Present seeds the anchor silently; a
        // later Present with a DIFFERENT refresh token carries `rotated_from = Some(prior-fingerprint)`
        // (rendered `mode=yank prev=…`); an UNCHANGED Present carries none; a scrub CLEARS the anchor
        // so the recovery Present re-seeds WITHOUT a false yank across the restore edge. Derived
        // purely from the observed present/valid state + fingerprint delta (AC1: "not guessed").
        let mut daemon = three_account_daemon(FakeRosterPoller::new()).await;
        let rt1 = cred(
            br#"{"claudeAiOauth":{"accessToken":"a","refreshToken":"rt-1","expiresAt":1782777600000}}"#,
        );
        let rt2 = cred(
            br#"{"claudeAiOauth":{"accessToken":"a","refreshToken":"rt-2","expiresAt":1782777600000}}"#,
        );

        // (1) First Present: seed the anchor, no yank marker.
        let mut diags = Vec::new();
        daemon.note_canonical_liveness(Some(&rt1), false, Some(0), &mut Vec::new(), &mut diags);
        assert_eq!(
            rotated_from_of(&diags[0]),
            None,
            "the first Present observation seeds the anchor without a yank"
        );

        // (2) Present with a DIFFERENT token: a rotation → yank carrying rt-1's fingerprint.
        let mut diags = Vec::new();
        daemon.note_canonical_liveness(Some(&rt2), false, Some(0), &mut Vec::new(), &mut diags);
        assert_eq!(
            rotated_from_of(&diags[0]),
            Some(canonical_fingerprint(b"rt-1")),
            "a Present→Present token change marks a yank carrying the PRIOR fingerprint"
        );

        // (3) Present with the SAME token: no rotation, no marker.
        let mut diags = Vec::new();
        daemon.note_canonical_liveness(Some(&rt2), false, Some(0), &mut Vec::new(), &mut diags);
        assert_eq!(
            rotated_from_of(&diags[0]),
            None,
            "an unchanged Present marks no yank"
        );

        // (4) A scrub CLEARS the anchor.
        daemon.note_canonical_liveness(None, true, Some(0), &mut Vec::new(), &mut Vec::new());
        assert_eq!(
            daemon.state.prev_canonical_fingerprint, None,
            "a scrub clears the yank anchor"
        );

        // (5) Recovery Present: re-seeds silently — a restore is NOT a yank.
        let mut diags = Vec::new();
        daemon.note_canonical_liveness(Some(&rt1), false, Some(0), &mut Vec::new(), &mut diags);
        assert_eq!(
            rotated_from_of(&diags[0]),
            None,
            "the Present that recovers a scrub re-seeds without a false yank"
        );
    }

    #[tokio::test]
    async fn note_canonical_liveness_omits_the_handle_when_no_active_is_resolved() {
        // Issue #464: a daemon that first reads an already-scrubbed item has no active to name —
        // the scrub still fires (the state is real), with the handle absent rather than fabricated.
        let mut daemon = three_account_daemon(FakeRosterPoller::new()).await;
        let mut events = Vec::new();
        daemon.note_canonical_liveness(None, true, None, &mut events, &mut Vec::new());
        assert_eq!(events, vec![Event::CanonicalScrubbed { account: None }]);
    }

    #[tokio::test]
    async fn a_tick_observing_a_scrubbed_canonical_emits_the_edge_triggered_scrub_event() {
        // Issue #464 AC-1 END-TO-END through the real poll path: when a tick's canonical read
        // returns `CredentialNotFound` (Claude Code's `invalid_grant` scrub empties the item —
        // ADR-0018), the tick emits exactly one durable `canonical_scrubbed` carrying the
        // last-known active handle — even though no `credential_dead` fires (the observability
        // gap the umbrella closes). Exercises the `Err(CredentialNotFound)` → `canonical_absent`
        // → `Event::CanonicalScrubbed` wiring the direct-call tests reach only in halves.
        let mut daemon = three_account_daemon(
            FakeRosterPoller::new()
                .ok("u-A", 0.10, 0.10)
                .ok("u-B", 0.10, 0.10)
                .ok("u-C", 0.10, 0.10),
        )
        .await;

        // First tick: the canonical is readable (opaque `A-token`), active resolves to `work`,
        // and no scrub is signalled.
        let before = daemon.tick().await;
        assert!(
            !before
                .events
                .iter()
                .any(|e| matches!(e, Event::CanonicalScrubbed { .. })),
            "a readable canonical emits no scrub: {:?}",
            before.events
        );
        assert_eq!(daemon.state.active, Some(0));
        assert!(!daemon.state.signaled_canonical_scrubbed);

        // Claude Code scrubs the shared item to empty → the next read is `CredentialNotFound`.
        daemon.store.set_not_found(true);
        let scrubbed = daemon.tick().await;
        assert_eq!(
            scrubbed
                .events
                .iter()
                .filter(|e| matches!(e, Event::CanonicalScrubbed { .. }))
                .collect::<Vec<_>>(),
            vec![&Event::CanonicalScrubbed {
                account: Some("work".to_owned())
            }],
            "the poll that observes the emptied canonical emits exactly one scrub event: {:?}",
            scrubbed.events
        );
        assert!(daemon.state.signaled_canonical_scrubbed);

        // A second scrubbed tick re-signals nothing (edge-triggered, not level-triggered).
        let still = daemon.tick().await;
        assert!(
            !still
                .events
                .iter()
                .any(|e| matches!(e, Event::CanonicalScrubbed { .. })),
            "a persisting scrub re-signals nothing: {:?}",
            still.events
        );
    }

    #[test]
    fn redaction_meter_covers_the_canonical_snapshot_fields() {
        use crate::redaction::meter::{assert_clean, Secrets};
        // Issue #464 / #475 / #15: the per-poll canonical snapshot + its scrub/restore events must
        // leak no secret. Build the log lines with a fingerprint derived from the fixture's REAL
        // refresh token — so a path that emitted the token (or its raw/hashed blob) rather than
        // the truncated per-token hash would surface here — and prove the value-based meter reads
        // clean. The Present line ALSO carries the #475 `mode=yank prev=<fingerprint>` marker with a
        // real-token-derived prior fingerprint, so a bug rendering the raw prior token in the `prev=`
        // slot (rather than its hash prefix) would surface here too.
        let secrets = Secrets::meter_fixture();
        let blob = secrets.blob();
        let rt = crate::refresh::refresh_token(blob).expect("fixture blob carries a refresh token");
        let fingerprint = canonical_fingerprint(&rt);
        let expires_at = crate::refresh::expires_at(blob).map(millis_to_secs);

        let mut corpus = String::new();
        corpus.push_str(
            &Diagnostic::Canonical {
                state: CanonicalLiveness::Present,
                fingerprint: Some(fingerprint.clone()),
                account: Some("work".to_owned()),
                expires_at,
                rotated_from: Some(fingerprint.clone()),
            }
            .to_log_line(std::time::SystemTime::UNIX_EPOCH),
        );
        corpus.push('\n');
        corpus.push_str(
            &Event::CanonicalScrubbed {
                account: Some("work".to_owned()),
            }
            .to_log_line(std::time::SystemTime::UNIX_EPOCH),
        );
        corpus.push('\n');
        corpus.push_str(
            &Event::CanonicalRestored {
                account: Some("work".to_owned()),
            }
            .to_log_line(std::time::SystemTime::UNIX_EPOCH),
        );
        corpus.push('\n');

        // Cardinality (#15 non-vacuous gate): the fingerprint derived from the REAL fixture token
        // actually reached the scanned corpus, and it is the 16-hex prefix — so the clean verdict
        // below is not vacuously true on an empty/degraded corpus.
        assert_eq!(fingerprint.len(), 16);
        assert!(corpus.contains(&format!("fingerprint={fingerprint}")));
        // …and the raw refresh token never rode alongside it.
        assert!(!corpus.contains(std::str::from_utf8(&rt).unwrap()));
        assert_clean(&corpus, &secrets, &[]);
    }

    // --- behavioral canary (issue #714) --------------------------------------

    /// Freeze `dir` read-only (0o500) so `reconcile_display`'s best-effort heal —
    /// a temp-file + rename in the SAME directory — cannot land, pinning a stale
    /// display for the drift fixtures. Restore with [`thaw_dir`] before the
    /// tempdir drops.
    fn freeze_dir(dir: &std::path::Path) {
        let mut perms = std::fs::metadata(dir).unwrap().permissions();
        std::os::unix::fs::PermissionsExt::set_mode(&mut perms, 0o500);
        std::fs::set_permissions(dir, perms).unwrap();
    }

    fn thaw_dir(dir: &std::path::Path) {
        let mut perms = std::fs::metadata(dir).unwrap().permissions();
        std::os::unix::fs::PermissionsExt::set_mode(&mut perms, 0o700);
        std::fs::set_permissions(dir, perms).unwrap();
    }

    /// The daemon-level DRIFT fixture: the canonical holds `spare`'s stashed token
    /// while the FROZEN `~/.claude.json` still names `work` — the display heal
    /// cannot land, so the canary's Layer-2 cross-check sees the positive `A ≠ B`
    /// divergence. `spare` (the token-resolved active) is over the session trigger
    /// so every tick WANTS to swap; whether the write happens is then exactly the
    /// canary's call.
    async fn drift_daemon(tun: &Tunables) -> (tempfile::TempDir, FakeDaemon) {
        let roster = vec![account("u-A", "work"), account("u-B", "spare")];
        let store = store_holding(b"B-token").await;
        let stash = stash_with(&[
            ("Sessiometer/u-A", b"A-token", "u-A"),
            ("Sessiometer/u-B", b"B-token", "u-B"),
        ])
        .await;
        let (dir, json) = claude_json("u-A");
        let poller = FakeRosterPoller::new()
            .ok("u-A", 0.10, 0.10)
            .ok("u-B", 0.97, 0.40);
        let daemon: FakeDaemon =
            Daemon::new(roster, poller, store, stash, FakeClock::frozen(), json, tun);
        (dir, daemon)
    }

    #[tokio::test]
    async fn a_drifted_canary_refuses_the_swap_with_zero_writes_and_one_edge_event() {
        // Issue #714 AC: identity mismatch (the resolved canonical byte-matches
        // `stash[X≠A]`) → the credential WRITE is refused pre-mutation — ZERO
        // writes to the canonical and both stashes — while reads/poll/status stay
        // live. The durable `canary_drift` event fires exactly ONCE (edge), and
        // the `status` wire carries the verdict with labels.
        let tun = tunables(95, 80, 0);
        let (dir, mut daemon) = drift_daemon(&tun).await;

        freeze_dir(dir.path());
        let first = warmed_tick(&mut daemon).await;
        let second = daemon.tick().await;
        thaw_dir(dir.path());

        // The swap was decided (spare is over the trigger) but the write refused.
        assert_eq!(first.action, TickAction::SwapFailed);
        assert_eq!(
            first.events,
            vec![Event::CanaryDrift {
                displayed: "work".to_owned(),
                matched: "spare".to_owned(),
                overridden: false,
            }]
        );
        // Reads stayed live: the refusing tick still polled the whole roster.
        assert!(first.snapshot.accounts.iter().all(|a| a.usage.is_some()));
        // The second refusing tick is SILENT (edge-triggered), still refusing.
        assert_eq!(second.action, TickAction::SwapFailed);
        assert!(
            second.events.is_empty(),
            "the drift alarm is edge-triggered: {:?}",
            second.events
        );
        // The verdict rides the status wire, labels only.
        assert_eq!(
            second.snapshot.canary,
            Some(CanaryStatus::Drift {
                displayed: "work".to_owned(),
                matched: "spare".to_owned(),
                overridden: false,
            })
        );
        // ZERO writes: the canonical and BOTH stashes hold their original bytes.
        assert!(daemon
            .store
            .read()
            .await
            .unwrap()
            .matches(&cred(b"B-token")));
        assert!(daemon
            .stash
            .read("Sessiometer/u-A")
            .await
            .unwrap()
            .credential
            .matches(&cred(b"A-token")));
        assert!(daemon
            .stash
            .read("Sessiometer/u-B")
            .await
            .unwrap()
            .credential
            .matches(&cred(b"B-token")));
    }

    #[tokio::test]
    async fn an_overridden_drift_swaps_anyway_and_logs_overridden_true() {
        // Issue #714 AC: the documented operator override (`canary_drift_override`)
        // clears a diagnosed FALSE drift — the swap proceeds despite the standing
        // Layer-2 alarm, and the durable record marks the ride with
        // `overridden=true` (status mirrors it).
        let mut tun = tunables(95, 80, 0);
        tun.canary_drift_override = true;
        let (dir, mut daemon) = drift_daemon(&tun).await;

        freeze_dir(dir.path());
        let outcome = warmed_tick(&mut daemon).await;
        thaw_dir(dir.path());

        // The swap RAN: spare (token-resolved active, index 1) → work (index 0).
        assert_eq!(outcome.action, TickAction::Swapped { from: 1, to: 0 });
        // The drift alarm still fired — overridden, never silenced…
        assert_eq!(
            outcome.events.first(),
            Some(&Event::CanaryDrift {
                displayed: "work".to_owned(),
                matched: "spare".to_owned(),
                overridden: true,
            })
        );
        // …alongside the normal swap event.
        assert!(
            outcome.events.iter().any(
                |e| matches!(e, Event::Swap { from, to, .. } if from == "spare" && to == "work")
            ),
            "the overridden swap still logs: {:?}",
            outcome.events
        );
        // The canonical was rerouted to the incoming account's token.
        assert!(daemon
            .store
            .read()
            .await
            .unwrap()
            .matches(&cred(b"A-token")));
        assert_eq!(
            outcome.snapshot.canary,
            Some(CanaryStatus::Drift {
                displayed: "work".to_owned(),
                matched: "spare".to_owned(),
                overridden: true,
            })
        );
    }

    #[tokio::test]
    async fn an_ambiguous_resolution_refuses_the_swap_then_clears_on_the_edge() {
        // Issue #714 AC: >1 item under the derived service → the uniqueness rule
        // fails, the write refuses (NO override — an atomic in-place write has no
        // unique, safe target), edge-triggered `canary_ambiguous` once; when the
        // duplicate clears, `canary_cleared` closes the episode and the swap runs.
        let roster = vec![account("u-A", "work"), account("u-B", "spare")];
        let store = store_holding(b"A-token").await;
        let stash = stash_with(&[
            ("Sessiometer/u-A", b"A-token", "u-A"),
            ("Sessiometer/u-B", b"B-token", "u-B"),
        ])
        .await;
        let (_dir, json) = claude_json("u-A");
        let poller = FakeRosterPoller::new()
            .ok("u-A", 0.97, 0.40)
            .ok("u-B", 0.10, 0.10);
        let tun = tunables(95, 80, 0);
        let mut daemon: FakeDaemon = Daemon::new(
            roster,
            poller,
            store,
            stash,
            FakeClock::frozen(),
            json,
            &tun,
        );
        daemon.store.set_ambiguous(Some(2));

        let first = warmed_tick(&mut daemon).await;
        assert_eq!(first.action, TickAction::SwapFailed);
        assert_eq!(first.events, vec![Event::CanaryAmbiguous { count: 2 }]);
        assert_eq!(
            first.snapshot.canary,
            Some(CanaryStatus::Ambiguous { count: 2 })
        );
        // Zero writes while ambiguous.
        assert!(daemon
            .store
            .read()
            .await
            .unwrap()
            .matches(&cred(b"A-token")));

        let second = daemon.tick().await;
        assert_eq!(second.action, TickAction::SwapFailed);
        assert!(
            second.events.is_empty(),
            "the ambiguity alarm is edge-triggered: {:?}",
            second.events
        );

        // The duplicate item is removed → the very next swap attempt re-probes
        // FRESH (never the boot-pinned verdict), sees a unique item, closes the
        // episode with `canary_cleared`, and the held swap finally lands.
        daemon.store.set_ambiguous(None);
        let third = daemon.tick().await;
        assert_eq!(third.action, TickAction::Swapped { from: 0, to: 1 });
        assert_eq!(
            third.events.first(),
            Some(&Event::CanaryCleared),
            "the alarm closes exactly once: {:?}",
            third.events
        );
        assert_eq!(third.snapshot.canary, Some(CanaryStatus::Ok));
    }

    #[tokio::test]
    async fn an_unmatched_canonical_is_inconclusive_and_the_swap_proceeds() {
        // Issue #714 AC: the resolved canonical matches NO stash — overwhelmingly
        // an in-place token refresh — so the canary is INCONCLUSIVE and fails
        // OPEN (never block on "couldn't verify"): the swap runs, and the engine's
        // own re-stash captures the refreshed token (the drift guard #6 exists
        // for), composing exactly with the #211 allow-neither rule. Issue #730: the
        // refreshed canonical is a WELL-FORMED CC credential (`canonical_well_formed:
        // true`), so the shape-gate leaves this fail-OPEN untouched — and the spares'
        // raw stashes never gate it (the gate is scoped to the ACTIVE canonical).
        let roster = vec![account("u-A", "work"), account("u-B", "spare")];
        let refreshed = cc_blob("sk-ant-oat-REFRESHED");
        let store = store_holding(&refreshed).await;
        let stash = stash_with(&[
            ("Sessiometer/u-A", b"A-token", "u-A"),
            ("Sessiometer/u-B", b"B-token", "u-B"),
        ])
        .await;
        let (_dir, json) = claude_json("u-A");
        let poller = FakeRosterPoller::new()
            .ok("u-A", 0.97, 0.40)
            .ok("u-B", 0.10, 0.10);
        let tun = tunables(95, 80, 0);
        let mut daemon: FakeDaemon = Daemon::new(
            roster,
            poller,
            store,
            stash,
            FakeClock::frozen(),
            json,
            &tun,
        );

        let outcome = warmed_tick(&mut daemon).await;
        assert_eq!(outcome.action, TickAction::Swapped { from: 0, to: 1 });
        assert!(
            !outcome
                .events
                .iter()
                .any(|e| matches!(e, Event::CanaryDrift { .. } | Event::CanaryAmbiguous { .. })),
            "inconclusive raises no alarm: {:?}",
            outcome.events
        );
        assert_eq!(outcome.snapshot.canary, Some(CanaryStatus::Inconclusive));
        // The engine captured the refreshed token into the outgoing stash (#6)…
        assert!(daemon
            .stash
            .read("Sessiometer/u-A")
            .await
            .unwrap()
            .credential
            .matches(&cred(&refreshed)));
        // …and rerouted the canonical to the incoming account.
        assert!(daemon
            .store
            .read()
            .await
            .unwrap()
            .matches(&cred(b"B-token")));
    }

    #[tokio::test]
    async fn an_unparseable_canonical_with_no_stash_match_refuses_the_swap() {
        // Issue #730: a swap is decided (the display-resolved active is over its
        // trigger), but the resolved canonical matches NO stash AND does not parse as
        // a Claude Code credential — overwhelmingly an unrelated secret. The pre-swap
        // gate refuses the atomic `-U` clobber (ZERO writes) and logs the redaction-
        // safe refusal. Issue #738: the wire now SAYS SO — the identity answer is still
        // inconclusive, but the refusal is the operator-visible fact, so it gets its own
        // verdict instead of the quiet `inconclusive` #730 reused.
        let roster = vec![account("u-A", "work"), account("u-B", "spare")];
        // A non-CC canonical no stash holds → the active resolves via the display (u-A).
        let store = store_holding(b"an-unrelated-keychain-secret").await;
        let stash = stash_with(&[
            ("Sessiometer/u-A", b"A-token", "u-A"),
            ("Sessiometer/u-B", b"B-token", "u-B"),
        ])
        .await;
        let (_dir, json) = claude_json("u-A");
        let poller = FakeRosterPoller::new()
            .ok("u-A", 0.97, 0.40)
            .ok("u-B", 0.10, 0.10);
        let tun = tunables(95, 80, 0);
        let mut daemon: FakeDaemon = Daemon::new(
            roster,
            poller,
            store,
            stash,
            FakeClock::frozen(),
            json,
            &tun,
        );

        let first = warmed_tick(&mut daemon).await;

        assert_eq!(first.action, TickAction::SwapFailed);
        assert_eq!(
            first.events,
            vec![Event::CanaryUnparseableCanonical { overridden: false }]
        );
        // Reads stayed live — the refusing tick still polled the roster.
        assert!(first.snapshot.accounts.iter().all(|a| a.usage.is_some()));
        // Issue #738 — the whole point: the wire carries the REFUSAL, so the CLI's
        // `render_canary` and the menubar banner can both voice it. The durable event still
        // fires exactly once from the refuse site (asserted above), NOT a second time from
        // the verdict edge.
        assert_eq!(
            first.snapshot.canary,
            Some(CanaryStatus::RefusedUnparseableCanonical)
        );
        // ZERO writes: the canonical still holds the unrelated secret, stashes intact.
        assert!(daemon
            .store
            .read()
            .await
            .unwrap()
            .matches(&cred(b"an-unrelated-keychain-secret")));
        assert!(daemon
            .stash
            .read("Sessiometer/u-A")
            .await
            .unwrap()
            .credential
            .matches(&cred(b"A-token")));
        assert!(daemon
            .stash
            .read("Sessiometer/u-B")
            .await
            .unwrap()
            .credential
            .matches(&cred(b"B-token")));
    }

    #[tokio::test]
    async fn refresh_canary_closes_the_drift_bracket_when_entering_the_unparseable_refusal() {
        // Issue #738 regression guard. The `canary_drift` / `canary_cleared` pair is a durable
        // BRACKET (`src/observability.rs`): every opened episode must close exactly once, or the
        // operator's log shows an identity alarm raised and never resolved.
        //
        // #738 introduced a verdict that opens no bracket of its own (its durable line is
        // per-ATTEMPT, owned by the refuse sites). The trap is that "opens nothing" reads as
        // "nothing to do" on the INCOMING edge too — but this verdict REPLACED the `Inconclusive`
        // that #730 mapped the case to, and `Inconclusive` closed the bracket. So a drift followed
        // by an unrelated secret appearing under the derived service would have left
        // `canary_drift` open forever.
        //
        // Driven through `refresh_canary` directly: the edge contract is the unit under test, and
        // routing it through a full tick would additionally depend on swap-decision plumbing that
        // has nothing to do with bracketing.
        let roster = vec![account("u-A", "work"), account("u-B", "spare")];
        // Canonical holds `spare`'s token while `~/.claude.json` names `work` active → DRIFT.
        let store = store_holding(b"B-token").await;
        let stash = stash_with(&[
            ("Sessiometer/u-A", b"A-token", "u-A"),
            ("Sessiometer/u-B", b"B-token", "u-B"),
        ])
        .await;
        let (dir, json) = claude_json("u-A");
        let poller = FakeRosterPoller::new()
            .ok("u-A", 0.10, 0.10)
            .ok("u-B", 0.10, 0.10);
        let tun = tunables(95, 80, 0);
        let mut daemon: FakeDaemon = Daemon::new(
            roster,
            poller,
            store,
            stash,
            FakeClock::frozen(),
            json,
            &tun,
        );
        // The canary reconciles the display before cross-checking, which would HEAL this drift
        // into agreement (the #714 anti-false-positive step). Freezing the directory makes
        // `~/.claude.json` unwritable so the divergence survives — the same device the sibling
        // drift tests use.
        freeze_dir(dir.path());

        // Edge 1 — the drift alarm OPENS its bracket.
        let mut opened = Vec::new();
        daemon.refresh_canary(&mut opened).await.unwrap();
        assert_eq!(
            opened,
            vec![Event::CanaryDrift {
                displayed: "work".to_owned(),
                matched: "spare".to_owned(),
                overridden: false,
            }],
            "the drift episode opens exactly once"
        );
        assert_eq!(
            daemon.state.canary,
            Some(CanaryStatus::Drift {
                displayed: "work".to_owned(),
                matched: "spare".to_owned(),
                overridden: false,
            })
        );

        // An unrelated secret replaces the canonical — no stash matches it, and it does not parse
        // as a Claude Code credential, so the #730 shape-gate refuses.
        daemon
            .store
            .write(&cred(b"an-unrelated-keychain-secret"))
            .await
            .unwrap();

        // Edge 2 — the verdict moves to the refusal, and the drift bracket CLOSES.
        let mut closed = Vec::new();
        daemon.refresh_canary(&mut closed).await.unwrap();
        assert_eq!(
            daemon.state.canary,
            Some(CanaryStatus::RefusedUnparseableCanonical)
        );
        assert_eq!(
            closed,
            vec![Event::CanaryCleared],
            "the drift bracket closes on the way INTO the refusal — and the refusal itself opens \
             no bracket here (its durable line is per-attempt, from the refuse sites)"
        );

        // Edge 3 — leaving the refusal for a quiet verdict emits NOTHING: `refresh_canary` closes
        // only brackets it opened, and it opened none for the refusal. An unpaired `canary_cleared`
        // would be as dishonest as the unclosed bracket this test exists to prevent.
        daemon.store.write(&cred(b"A-token")).await.unwrap();
        let mut quiet = Vec::new();
        daemon.refresh_canary(&mut quiet).await.unwrap();
        assert_eq!(daemon.state.canary, Some(CanaryStatus::Ok));
        assert!(
            quiet.is_empty(),
            "no unpaired close on the way out of the refusal: {quiet:?}"
        );

        thaw_dir(dir.path());
    }

    #[tokio::test]
    async fn the_nostashmatch_override_swaps_through_an_unparseable_canonical() {
        // Issue #730 override: `canary_nostashmatch_override = true` restores fail-OPEN
        // for the unparseable case — the operator has vetted the canonical (e.g. a
        // legitimate NEW CC credential format they will re-stash). The swap proceeds;
        // the ride is logged with `overridden=true`, never silenced. DEDICATED switch:
        // `canary_drift_override` is left at its default and does not gate this case.
        let roster = vec![account("u-A", "work"), account("u-B", "spare")];
        let store = store_holding(b"a-vetted-new-cc-format").await;
        let stash = stash_with(&[
            ("Sessiometer/u-A", b"A-token", "u-A"),
            ("Sessiometer/u-B", b"B-token", "u-B"),
        ])
        .await;
        let (_dir, json) = claude_json("u-A");
        let poller = FakeRosterPoller::new()
            .ok("u-A", 0.97, 0.40)
            .ok("u-B", 0.10, 0.10);
        let mut tun = tunables(95, 80, 0);
        tun.canary_nostashmatch_override = true;
        let mut daemon: FakeDaemon = Daemon::new(
            roster,
            poller,
            store,
            stash,
            FakeClock::frozen(),
            json,
            &tun,
        );

        let outcome = warmed_tick(&mut daemon).await;

        // The swap RAN: work (display-resolved active, 0) → spare (1).
        assert_eq!(outcome.action, TickAction::Swapped { from: 0, to: 1 });
        // The refusal was logged as overridden, never silenced…
        assert!(
            outcome
                .events
                .contains(&Event::CanaryUnparseableCanonical { overridden: true }),
            "the overridden refusal is logged: {:?}",
            outcome.events
        );
        // …alongside the normal swap event.
        assert!(
            outcome.events.iter().any(
                |e| matches!(e, Event::Swap { from, to, .. } if from == "work" && to == "spare")
            ),
            "the overridden swap still logs: {:?}",
            outcome.events
        );
        // Issue #738, the complement of the refusing case: with the override set NOTHING is
        // refused, so the honest verdict is the quiet `Inconclusive` (the pre-#730 fail-OPEN
        // this tunable is documented to restore) — NOT `RefusedUnparseableCanonical`, whose
        // name would then describe a refusal that did not happen and would raise an act-now
        // `.error` banner over a swap that succeeded.
        assert_eq!(outcome.snapshot.canary, Some(CanaryStatus::Inconclusive));
        // The canonical was rerouted to the incoming account's token.
        assert!(daemon
            .store
            .read()
            .await
            .unwrap()
            .matches(&cred(b"B-token")));
    }

    #[tokio::test]
    async fn a_lagging_display_cowrite_is_healed_not_reported_as_drift() {
        // Issue #714 decided invariant: reconcile-BEFORE-cross-check. The fixture
        // is a lagging self-co-write (a prior swap put spare's token in the
        // canonical; the display still says work) with a WRITABLE `~/.claude.json`
        // — the canary's embedded reconcile heals the display first, so the swap
        // proceeds with NO false drift alarm.
        let tun = tunables(95, 80, 0);
        let (_dir, mut daemon) = drift_daemon(&tun).await;

        // Same divergent fixture as the drift tests — but the display is writable.
        let outcome = warmed_tick(&mut daemon).await;

        assert_eq!(outcome.action, TickAction::Swapped { from: 1, to: 0 });
        assert!(
            !outcome
                .events
                .iter()
                .any(|e| matches!(e, Event::CanaryDrift { .. })),
            "a healable display is never drift: {:?}",
            outcome.events
        );
        assert_eq!(outcome.snapshot.canary, Some(CanaryStatus::Ok));
    }

    #[tokio::test]
    async fn refresh_canary_holds_state_and_edges_through_drift_and_clear() {
        // The boot-path entry (issue #714): `refresh_canary` concludes a verdict,
        // retains it for the wire, and edge-triggers the durable pair — silent on
        // a repeat verdict, `canary_cleared` exactly once when the drift resolves.
        // A gone canonical concludes NOT_FOUND with NO canary event of its own
        // (the scrub machinery voices that state).
        let tun = tunables(95, 80, 0);
        let (dir, mut daemon) = drift_daemon(&tun).await;

        freeze_dir(dir.path());
        let mut events = Vec::new();
        let first = daemon.refresh_canary(&mut events).await.unwrap();
        assert_eq!(
            first,
            crate::canary::CanaryOutcome::Drift {
                displayed: 0,
                matched: 1
            }
        );
        assert_eq!(
            events,
            vec![Event::CanaryDrift {
                displayed: "work".to_owned(),
                matched: "spare".to_owned(),
                overridden: false,
            }]
        );

        // Re-running under the SAME verdict is silent (edge-triggered).
        let mut repeat = Vec::new();
        daemon.refresh_canary(&mut repeat).await.unwrap();
        assert!(
            repeat.is_empty(),
            "no re-fire on a held verdict: {repeat:?}"
        );
        thaw_dir(dir.path());

        // The drift resolves (the canonical returns to the DISPLAYED account's
        // token) → one `canary_cleared` closes the episode…
        daemon.store.write(&cred(b"A-token")).await.unwrap();
        let mut cleared = Vec::new();
        let verdict = daemon.refresh_canary(&mut cleared).await.unwrap();
        assert_eq!(verdict, crate::canary::CanaryOutcome::Ok);
        assert_eq!(cleared, vec![Event::CanaryCleared]);

        // …and NOT_FOUND is a verdict with no canary event (scrub owns the voice).
        daemon.store.set_not_found(true);
        let mut gone = Vec::new();
        let verdict = daemon.refresh_canary(&mut gone).await.unwrap();
        assert_eq!(verdict, crate::canary::CanaryOutcome::NotFound);
        assert!(gone.is_empty(), "not-found emits no canary event: {gone:?}");
    }

    #[test]
    fn redaction_meter_covers_the_canary_lines_and_status() {
        use crate::redaction::meter::{assert_clean, Secrets};
        // Issue #714 / #15: EVERY canary surface — each durable event line and the
        // wire-serialized `CanaryStatus` — must carry operator LABELS and COUNTS
        // only, never a token, blob, email, or account-uuid. Build the
        // corpus from the meter fixture's REAL secret material context (the labels
        // are authored, the secrets exist in the fixture) and prove the
        // value-based meter reads clean.
        //
        // The enumeration below is the falsifier for the module's "every surface derived
        // from these types is secret-free by construction" claim (`src/canary.rs`), so a
        // canary event added without a row here silently un-falsifies it. #730's
        // `CanaryUnparseableCanonical` and #736's `CanaryOnlineProbe` are therefore
        // enumerated alongside #714's originals.
        let secrets = Secrets::meter_fixture();
        let mut corpus = String::new();
        for event in [
            Event::CanaryDrift {
                displayed: "work".to_owned(),
                matched: "spare".to_owned(),
                overridden: false,
            },
            Event::CanaryDrift {
                displayed: "work".to_owned(),
                matched: "spare".to_owned(),
                overridden: true,
            },
            Event::CanaryAmbiguous { count: 3 },
            Event::CanaryCleared,
            Event::CanaryUnparseableCanonical { overridden: false },
            Event::CanaryUnparseableCanonical { overridden: true },
            // Issue #736 — every `Liveness` label that can reach a line, at both
            // `refused` settings, so a future verdict carrying a status code or a
            // response fragment fails HERE rather than shipping.
            Event::CanaryOnlineProbe {
                verdict: crate::canary::Liveness::Uninformative.as_str(),
                refused: false,
            },
            Event::CanaryOnlineProbe {
                verdict: crate::canary::Liveness::Overridden.as_str(),
                refused: false,
            },
            Event::CanaryOnlineProbe {
                verdict: crate::canary::Liveness::Inconclusive.as_str(),
                refused: false,
            },
            Event::CanaryOnlineProbe {
                verdict: crate::canary::Liveness::Rejected.as_str(),
                refused: true,
            },
        ] {
            corpus.push_str(&event.to_log_line(std::time::SystemTime::UNIX_EPOCH));
            corpus.push('\n');
        }
        for status in [
            CanaryStatus::Ok,
            CanaryStatus::Inconclusive,
            CanaryStatus::NotFound,
            CanaryStatus::Ambiguous { count: 3 },
            CanaryStatus::Drift {
                displayed: "work".to_owned(),
                matched: "spare".to_owned(),
                overridden: true,
            },
            CanaryStatus::RefusedUnparseableCanonical,
        ] {
            corpus.push_str(&serde_json::to_string(&status).unwrap());
            corpus.push('\n');
        }
        // The corpus is substantive (no vacuous pass on a degraded build)…
        assert!(corpus.contains("event=canary_drift"));
        assert!(corpus.contains("overridden=true"));
        assert!(corpus.contains(r#""verdict":"drift""#));
        assert!(corpus.contains("event=canary_unparseable_canonical"));
        assert!(corpus.contains("event=canary_online_probe verdict=rejected refused=true"));
        // …and carries nothing the meter knows as secret.
        assert_clean(&corpus, &secrets, &[]);
    }

    // --- Layer 3: the opt-in online liveness probe (issue #736) --------------

    /// A daemon whose OFFLINE canary layers all pass — canonical holds `work`'s token,
    /// `work`'s stash matches it, and the display names `work` active — so a
    /// `locked_swap` reaches the Layer-3 probe. `poller` and `tun` script the probe's
    /// answer and the two `[tunables]` switches.
    ///
    /// Both accounts are seeded with a CURRENT reading, which is what leaves `probe_gate`
    /// ARMED: the stand-down keys off a missing one. The blind cases seed `None` explicitly
    /// so the state under test is visible at the test rather than inherited from here.
    async fn probe_daemon(
        poller: FakeRosterPoller,
        tun: Tunables,
    ) -> (tempfile::TempDir, FakeDaemon) {
        let roster = vec![account("u-A", "work"), account("u-B", "spare")];
        let store = store_holding(b"A-token").await;
        let stash = stash_with(&[
            ("Sessiometer/u-A", b"A-token", "u-A"),
            ("Sessiometer/u-B", b"B-token", "u-B"),
        ])
        .await;
        let (dir, json) = claude_json("u-A");
        let mut daemon = Daemon::new(
            roster,
            poller,
            store,
            stash,
            FakeClock::frozen(),
            json,
            &tun,
        );
        daemon
            .state
            .seed_readings([Some(reading(0.10, 0.10)), Some(reading(0.10, 0.10))]);
        (dir, daemon)
    }

    /// `tunables(95, 80, 0)` with the two #736 switches set — the only thing these tests
    /// vary.
    fn probe_tunables(probe: bool, strict: bool) -> Tunables {
        Tunables {
            canary_online_probe: probe,
            canary_online_probe_strict: strict,
            ..tunables(95, 80, 0)
        }
    }

    #[tokio::test]
    async fn the_disarmed_probe_neither_refuses_nor_logs_even_on_a_dead_bearer() {
        // Issue #736's first hard constraint at the integration level: with
        // `canary_online_probe = false` (the shipped default) the swap path is the
        // pre-#736 one. Scripted so the probe WOULD come back rejected if it ran — a
        // `401` on the very account it would poll — so a pass here is the disarm
        // working, not a vacuously healthy fixture.
        let poller = FakeRosterPoller::new().unauthorized("u-A");
        let (_dir, mut daemon) = probe_daemon(poller, probe_tunables(false, false)).await;
        let mut events = Vec::new();

        // Driven through `locked_swap` directly rather than a full tick: the probe gate is
        // the unit under test, and a tick's own poll of the outgoing account would have to
        // succeed for a swap to be decided at all — which is precisely the answer these
        // tests need to script as a FAILURE. (Same reason the #738 bracket test above
        // drives `refresh_canary` directly.)
        let report = daemon
            .locked_swap("Sessiometer/u-A", "Sessiometer/u-B", false, &mut events)
            .await
            .expect("a disarmed probe must not block the swap");

        assert!(report.canonical_confirmed);
        assert!(
            !events
                .iter()
                .any(|e| matches!(e, Event::CanaryOnlineProbe { .. })),
            "a disarmed probe must emit nothing: {events:?}"
        );
        // The swap really happened — the canonical now holds the incoming token.
        assert!(daemon
            .store
            .read()
            .await
            .unwrap()
            .matches(&cred(b"B-token")));
    }

    #[tokio::test]
    async fn an_armed_probe_that_confirms_liveness_is_silent_and_swaps() {
        // The healthy armed case: alarm-only emission means a confirmed-live probe leaves
        // no line at all, so arming the probe does not turn every swap into log noise.
        let poller = FakeRosterPoller::new().ok("u-A", 0.10, 0.10);
        let (_dir, mut daemon) = probe_daemon(poller, probe_tunables(true, true)).await;
        let mut events = Vec::new();

        daemon
            .locked_swap("Sessiometer/u-A", "Sessiometer/u-B", false, &mut events)
            .await
            .expect("a live probe must not block the swap");

        assert!(
            !events
                .iter()
                .any(|e| matches!(e, Event::CanaryOnlineProbe { .. })),
            "a live probe must be silent: {events:?}"
        );
        assert!(daemon
            .store
            .read()
            .await
            .unwrap()
            .matches(&cred(b"B-token")));
    }

    #[tokio::test]
    async fn an_armed_non_strict_probe_that_fails_logs_and_still_swaps() {
        // Issue #736's second hard constraint: probe failure != refuse. An unreachable
        // endpoint must not become a swap outage. The durable line is the ONLY trace that
        // the probe failed and the swap went ahead regardless, which is why it is emitted
        // even though nothing was refused — and why `refused` must NOT trail it.
        let poller = FakeRosterPoller::new().failing("u-A");
        let (_dir, mut daemon) = probe_daemon(poller, probe_tunables(true, false)).await;
        let mut events = Vec::new();

        daemon
            .locked_swap("Sessiometer/u-A", "Sessiometer/u-B", false, &mut events)
            .await
            .expect("a non-strict probe failure must not block the swap");

        assert!(events.contains(&Event::CanaryOnlineProbe {
            verdict: "inconclusive",
            refused: false,
        }));
        assert!(daemon
            .store
            .read()
            .await
            .unwrap()
            .matches(&cred(b"B-token")));
    }

    #[tokio::test]
    async fn an_armed_strict_probe_that_is_rejected_refuses_with_zero_writes() {
        // Both switches armed — the one configuration in which the probe can cost a swap.
        // A `401` says the resolved canonical no longer authenticates, so the write is
        // refused BEFORE any mutation, exactly like the offline layers' refusals.
        let poller = FakeRosterPoller::new().unauthorized("u-A");
        let (_dir, mut daemon) = probe_daemon(poller, probe_tunables(true, true)).await;
        let mut events = Vec::new();

        let result = daemon
            .locked_swap("Sessiometer/u-A", "Sessiometer/u-B", false, &mut events)
            .await;

        assert!(
            matches!(
                result,
                Err(Error::CanaryProbeNotLive {
                    verdict: "rejected"
                })
            ),
            "expected a rejected-probe refusal, got {result:?}"
        );
        assert!(events.contains(&Event::CanaryOnlineProbe {
            verdict: "rejected",
            refused: true,
        }));
        // ZERO writes: the canonical still holds the OUTGOING token and both stashes are
        // untouched — the refusal is pre-mutation, not a rollback.
        assert!(daemon
            .store
            .read()
            .await
            .unwrap()
            .matches(&cred(b"A-token")));
        assert!(daemon
            .stash
            .read("Sessiometer/u-A")
            .await
            .unwrap()
            .credential
            .matches(&cred(b"A-token")));
        assert!(daemon
            .stash
            .read("Sessiometer/u-B")
            .await
            .unwrap()
            .credential
            .matches(&cred(b"B-token")));
    }

    #[tokio::test]
    async fn a_strict_probe_never_blocks_the_emergency_swap_off_a_dead_active() {
        // THE self-DoS guard, and the reason `ProbeGate::Uninformative` exists.
        //
        // `emergency_swap` fires exactly when the active account is QUARANTINED on a 401
        // streak and still failing to poll (`src/daemon.rs`) — the escape hatch off a dead
        // credential. Probing that same bearer necessarily returns `Rejected`, because it
        // IS dead; that is the reason to swap, not a reason to refuse. Without the stand-down
        // a strict operator would deadlock here on EVERY tick, forever, with no recovery but
        // a config edit + restart — reintroducing precisely the `ActiveDeadNoTarget` self-DoS
        // the surrounding code drops the target reserve to avoid.
        let poller = FakeRosterPoller::new().unauthorized("u-A");
        let (_dir, mut daemon) = probe_daemon(poller, probe_tunables(true, true)).await;
        // The state `emergency_swap` runs in, BOTH halves of it: quarantined AND with this
        // cycle's reading absent. The second half is what `probe_gate` actually reads — the
        // quarantine flag is set here so the fixture is the real precondition rather than a
        // convenient subset of it.
        daemon.state.accounts[0].health.quarantined = true;
        daemon
            .state
            .seed_readings([None, Some(reading(0.10, 0.10))]);
        let mut events = Vec::new();

        daemon
            .locked_swap("Sessiometer/u-A", "Sessiometer/u-B", false, &mut events)
            .await
            .expect("the escape swap off a dead active must not be blocked by the probe");

        // The swap landed — the fleet is off the dead credential.
        assert!(daemon
            .store
            .read()
            .await
            .unwrap()
            .matches(&cred(b"B-token")));
        // The stand-down is on the record: an operator who armed strict is entitled to see
        // that the gate did not run, and why — but it must NOT read as a refusal.
        assert!(events.contains(&Event::CanaryOnlineProbe {
            verdict: "uninformative",
            refused: false,
        }));
    }

    #[tokio::test]
    async fn a_strict_probe_never_blocks_the_preemptive_swap_off_a_blind_active() {
        // The arm the `quarantined` proxy MISSES, and the reason `probe_gate` reads the
        // reading directly instead.
        //
        // `blind_swap` (#452 / ADR-0017) runs in the NON-quarantined branch: its precondition
        // is only that the active's current reading is absent. Blindness caused by a `403`
        // missing scope or any other non-401 `4xx` gets there while BOTH proxies read healthy
        // — such an outcome RESETS the 401 streak (so never quarantines) and takes the
        // `backoff_signal → None` branch that CLEARS `poll_backoff_until`. A gate keyed on
        // `quarantined || backing_off` would arm here, probe the same unreachable endpoint,
        // score `Inconclusive`, and under strict refuse the preemptive escape on every tick
        // forever — the identical deadlock the emergency arm above is guarded against.
        let poller = FakeRosterPoller::new().scope_missing("u-A");
        let (_dir, mut daemon) = probe_daemon(poller, probe_tunables(true, true)).await;
        // Blind, but by neither proxy's definition: not quarantined, not backing off.
        daemon
            .state
            .seed_readings([None, Some(reading(0.10, 0.10))]);
        assert!(!daemon.state.accounts[0].health.quarantined);
        assert!(daemon.state.accounts[0].health.poll_backoff_until.is_none());
        let mut events = Vec::new();

        daemon
            .locked_swap("Sessiometer/u-A", "Sessiometer/u-B", false, &mut events)
            .await
            .expect("the preemptive swap off a blind active must not be blocked by the probe");

        assert!(daemon
            .store
            .read()
            .await
            .unwrap()
            .matches(&cred(b"B-token")));
        assert!(events.contains(&Event::CanaryOnlineProbe {
            verdict: "uninformative",
            refused: false,
        }));
    }

    #[tokio::test]
    async fn a_quarantined_account_that_polls_again_is_still_worth_probing() {
        // The converse of the two stand-downs, and the reason the condition is the reading
        // rather than `quarantined || …`: a quarantined account MID-RECOVERY (issue #42 —
        // its own token started answering again) has a live reading, so its bearer
        // demonstrably works and a probe of it is genuinely informative. Standing down there
        // would silently disarm the gate on an account an operator can still `use` away from.
        let poller = FakeRosterPoller::new().unauthorized("u-A");
        let (_dir, mut daemon) = probe_daemon(poller, probe_tunables(true, true)).await;
        daemon.state.accounts[0].health.quarantined = true;
        // …but with THIS cycle's poll live: the recovery probe is landing.
        daemon
            .state
            .seed_readings([Some(reading(0.10, 0.10)), Some(reading(0.10, 0.10))]);
        let mut events = Vec::new();

        let result = daemon
            .locked_swap("Sessiometer/u-A", "Sessiometer/u-B", false, &mut events)
            .await;

        assert!(
            matches!(
                result,
                Err(Error::CanaryProbeNotLive {
                    verdict: "rejected"
                })
            ),
            "the gate must stay ARMED on a recovering account, got {result:?}"
        );
    }

    #[tokio::test]
    async fn a_forced_swap_bypasses_an_armed_strict_probe_and_says_so() {
        // The escape `Error::CanaryProbeNotLive` and the README both promise, on the
        // DAEMON-ROUTED `use` path (issue #167) — where `run_use`'s own gate never executes,
        // because a reachable daemon's ack is authoritative. Without `force` threaded through
        // `locked_swap` the documented remedy would work only while the daemon is DOWN, i.e.
        // never in the case that motivates it.
        //
        // It bypasses LAYER 3 ONLY, and it is not silent: `verdict=overridden` is the durable
        // trace that a swap skipped a gate the operator had armed, mirroring how the offline
        // layers record `overridden=true`.
        let poller = FakeRosterPoller::new().unauthorized("u-A");
        let (_dir, mut daemon) = probe_daemon(poller, probe_tunables(true, true)).await;
        let mut events = Vec::new();

        daemon
            .locked_swap("Sessiometer/u-A", "Sessiometer/u-B", true, &mut events)
            .await
            .expect("--force must carry the swap past an armed strict probe");

        assert!(daemon
            .store
            .read()
            .await
            .unwrap()
            .matches(&cred(b"B-token")));
        assert!(events.contains(&Event::CanaryOnlineProbe {
            verdict: "overridden",
            refused: false,
        }));
    }

    #[tokio::test]
    async fn a_strict_probe_never_blocks_a_swap_off_a_backing_off_active() {
        // The OTHER term of the stand-down, isolated: this account still carries a reading
        // (seeded by `probe_daemon`), so only `account_backing_off` can stand the gate down
        // here. That term exists for the #293 per-account back-off discipline — the tick loop
        // skips a held account's poll, and a probe that ignored the hold would fire an extra
        // request into a window the server just directed, on top of failing for that reason
        // and refusing the swap under strict.
        let poller = FakeRosterPoller::new().rate_limited("u-A", None);
        let (_dir, mut daemon) = probe_daemon(poller, probe_tunables(true, true)).await;
        // Inside a rate-limit back-off window on the monotonic clock.
        daemon.state.accounts[0].health.poll_backoff_until =
            Some(daemon.clock.now() + Duration::from_secs(60));
        assert!(daemon.state.accounts[0].last_reading.is_some());
        let mut events = Vec::new();

        daemon
            .locked_swap("Sessiometer/u-A", "Sessiometer/u-B", false, &mut events)
            .await
            .expect("a swap off a throttled active must not be blocked by the probe");

        assert!(daemon
            .store
            .read()
            .await
            .unwrap()
            .matches(&cred(b"B-token")));
        assert!(events.contains(&Event::CanaryOnlineProbe {
            verdict: "uninformative",
            refused: false,
        }));
    }

    #[tokio::test]
    async fn an_armed_probe_that_cannot_name_the_outgoing_account_refuses_rather_than_falls_open() {
        // The one arm with non-obvious semantics. `outgoing` naming no roster account is
        // unreachable in production (every caller derives it FROM the roster), but if it
        // ever became reachable an ARMED gate must not fall silently open — "could not
        // probe" is INCONCLUSIVE, which strict refuses on, not a pass.
        let poller = FakeRosterPoller::new().ok("u-A", 0.10, 0.10);
        let (_dir, mut daemon) = probe_daemon(poller, probe_tunables(true, true)).await;
        let mut events = Vec::new();

        let result = daemon
            .locked_swap("Sessiometer/absent", "Sessiometer/u-B", false, &mut events)
            .await;

        assert!(
            matches!(
                result,
                Err(Error::CanaryProbeNotLive {
                    verdict: "inconclusive"
                })
            ),
            "an armed gate must not fall open, got {result:?}"
        );
        assert!(daemon
            .store
            .read()
            .await
            .unwrap()
            .matches(&cred(b"A-token")));
    }

    #[tokio::test]
    async fn a_disarmed_probe_stays_a_no_op_even_when_the_outgoing_is_unnameable() {
        // The complement of the test above: on the same unreachable arm, a DISARMED gate
        // must leave a default daemon's behaviour byte-identical to the pre-#736 one —
        // `Skipped`, no refusal, no log line. A gate nobody armed must never be able to
        // block a swap.
        //
        // The swap still fails, but on the ENGINE's own terms — there is no outgoing stash
        // to re-stash into (`StashIncomplete`), which is the pre-existing behaviour for a
        // bogus outgoing and further evidence this arm is unreachable in production. What
        // this test pins is the DISCRIMINATOR: the failure is not `CanaryProbeNotLive`.
        let poller = FakeRosterPoller::new().ok("u-A", 0.10, 0.10);
        let (_dir, mut daemon) = probe_daemon(poller, probe_tunables(false, true)).await;
        let mut events = Vec::new();

        let result = daemon
            .locked_swap("Sessiometer/absent", "Sessiometer/u-B", false, &mut events)
            .await;

        assert!(
            !matches!(result, Err(Error::CanaryProbeNotLive { .. })),
            "a disarmed gate must never refuse, got {result:?}"
        );
        assert!(
            !events
                .iter()
                .any(|e| matches!(e, Event::CanaryOnlineProbe { .. })),
            "a disarmed probe must emit nothing: {events:?}"
        );
    }

    #[tokio::test]
    async fn an_armed_strict_probe_refuses_on_inconclusive_too() {
        // Strict mode IS the opt-in to the network failure mode the default forbids, so
        // "could not confirm" refuses just as a positive rejection does — and the verdict
        // on the wire says WHICH, so the operator can tell a dead bearer (investigate the
        // credential) from an unreachable endpoint (investigate the network).
        let poller = FakeRosterPoller::new().rate_limited("u-A", None);
        let (_dir, mut daemon) = probe_daemon(poller, probe_tunables(true, true)).await;
        let mut events = Vec::new();

        let result = daemon
            .locked_swap("Sessiometer/u-A", "Sessiometer/u-B", false, &mut events)
            .await;

        assert!(
            matches!(
                result,
                Err(Error::CanaryProbeNotLive {
                    verdict: "inconclusive"
                })
            ),
            "expected an inconclusive-probe refusal, got {result:?}"
        );
        assert!(daemon
            .store
            .read()
            .await
            .unwrap()
            .matches(&cred(b"A-token")));
    }
}
