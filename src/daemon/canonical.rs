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
    pub(crate) async fn reconcile_on_start(&self) -> Result<()> {
        let canonical = self.store.read().await?;
        for account in &self.roster {
            let Ok(stashed) = self.stash.read(&account.stash()).await else {
                continue;
            };
            if !stashed.credential.matches(&canonical) {
                continue;
            }
            // The canonical belongs to this account; ensure the display agrees.
            let displayed = claude_state::read_oauth_account_from(&self.claude_json)
                .ok()
                .map(|o| o.account_uuid().to_owned());
            if displayed.as_deref() != Some(stashed.oauth_account.account_uuid()) {
                claude_state::write_oauth_account(&self.claude_json, &stashed.oauth_account)?;
            }
            return Ok(());
        }
        // No stash matched the canonical token — leave ~/.claude.json untouched.
        Ok(())
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
}
