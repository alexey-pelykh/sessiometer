// Copyright (c) 2026 Oleksii PELYKH
// SPDX-License-Identifier: MIT

//! The keep-warm / promote cluster of the [`Daemon`] decision core (issue #637 step 4, issue
//! #659, split out of the single `impl Daemon` block).
//!
//! The #282 FOURTH refresh mechanism: rather than let an idle ACTIVE account's canonical
//! token drift toward expiry between polls, spawn `claude` against it to mint a fresh one
//! in place, then promote the mint back into the canonical item. Retry-shaped entry points
//! ([`should_keep_warm_retry`](super::Daemon::should_keep_warm_retry),
//! [`keep_warm_retry`](super::Daemon::keep_warm_retry)) fold a failed poll into the same
//! machinery, so one keep-warm implementation serves both the cadence path and the
//! recovery path.

use super::*;

impl<P, C, S, K> super::Daemon<P, C, S, K>
where
    P: RosterPoller,
    C: CredentialStore,
    S: AccountStash,
    K: Clock,
{
    /// Whether the active poll `i`'s outcome warrants a #282 REACTIVE keep-warm backstop,
    /// evaluated on the PRE-fold state (before [`note_poll_outcome`](Self::note_poll_outcome)
    /// advances the streak) — the ACTIVE-account counterpart of
    /// [`should_refresh_retry`](Self::should_refresh_retry):
    ///
    /// - a keep-warm seam is wired ([`with_keep_warm_engine`](Self::with_keep_warm_engine)),
    /// - the poll was a 401 ([`PollOutcome::Unauthorized`]),
    /// - the account is not already quarantined (a dead account is left to the #42 streak /
    ///   emergency swap — never re-warmed on every re-probe poll),
    /// - this is the FIRST 401 of the current streak episode (`consec_401 == 0`), and
    /// - the account IS the ACTIVE one (`state.active == Some(i)`).
    ///
    /// The last clause is the EXACT complement of `should_refresh_retry`'s active-EXCLUSION
    /// (issue #253): the #162 isolated engine writes the STASH, so it must never touch the active
    /// account; this keep-warm mints and PROMOTES to the canonical item a live session reads, so
    /// it is the ONE path that legitimately targets the active account. The two are therefore
    /// mutually exclusive on `i` and wired as an `if / else if` in [`tick`](Self::tick), so a 401
    /// takes exactly one refresh path. `consec_401 == 0` is the same once-per-episode storm guard:
    /// the first active 401 attempts the in-place revive; the rest of the episode advances the
    /// streak directly toward the #42 emergency swap.
    pub(super) fn should_keep_warm_retry(&self, i: usize, result: &Result<Usage>) -> bool {
        self.keep_warm.is_some()
            && matches!(classify_poll(result), PollOutcome::Unauthorized)
            && !self.state.health[i].quarantined
            && self.state.health[i].consec_401 == 0
            && self.state.active == Some(i)
    }

    /// The REACTIVE keep-warm backstop (issue #282): on the active account's FIRST usage-401,
    /// mint a fresh token in place and PROMOTE it to the canonical item, then re-poll the active
    /// account THROUGH the (now-fresh) canonical — the reading
    /// [`note_poll_outcome`](Self::note_poll_outcome) then folds into the streak. Only called when
    /// [`should_keep_warm_retry`](Self::should_keep_warm_retry) holds, so `keep_warm` is `Some` and
    /// `i` is the ACTIVE account.
    ///
    /// - A successful promote + a re-poll that CLEARS → `Ok(usage)` resets the streak (the
    ///   false-death this fixes: an expired-but-refreshable active token is revived in place before
    ///   it counts toward the #42 quarantine).
    /// - No promote (`NoChange` / a dead-or-absent refresh token / an engine error / a swap that
    ///   raced the mint) → the 401 STANDS (`Err(UsageUnauthorized)`), so the streak advances toward
    ///   quarantine → the #42 emergency swap. This is invariant 4: a truly-dead active credential
    ///   still quarantines and the escape to a live spare is preserved.
    /// - A re-poll that 401s AGAIN even after a fresh token → the 401 stands (a genuine problem the
    ///   fresh token did not fix); the streak advances.
    ///
    /// `canonical` is the blob read once at top-of-tick; `None` (unreadable) → nothing to mint
    /// from, fail-safe to the 401. The mint never crashes the poll loop (a spawn / FS failure is
    /// an `Err` the keep-warm engine swallows into a no-promote).
    pub(super) async fn keep_warm_retry(
        &mut self,
        i: usize,
        canonical: Option<&Credential>,
        events: &mut Vec<Event>,
    ) -> Result<Usage> {
        // No readable canonical blob → cannot mint; fail-safe, let the 401 stand.
        let Some(canonical) = canonical else {
            return Err(Error::UsageUnauthorized);
        };
        if self
            .keep_warm_and_promote(i, canonical, KeepWarmTrigger::Reactive, events)
            .await
            .promoted
        {
            // The canonical now holds a fresh token → re-poll the ACTIVE account through it.
            self.poller
                .poll(&self.roster[i], true)
                .await
                .map(|reading| reading.usage)
        } else {
            // No fresh token promoted → the 401 stands so the #42 streak advances (invariant 4).
            Err(Error::UsageUnauthorized)
        }
    }

    /// The PROACTIVE keep-warm (issue #282): when the active token is within its (staggered)
    /// near-expiry horizon, mint a fresh token in place and PROMOTE it to the canonical item —
    /// BEFORE any 401 — so a live session always reads a warm token and the overnight false-death
    /// cascade never starts. Serialized into [`tick`](Self::tick) just before
    /// [`decide_action`](Self::decide_action). Inert (an immediate return) unless the keep-warm
    /// seam is wired AND the proactive path is opted in.
    ///
    /// Gates, in order (each a cheap check before the expensive `claude -p` mint):
    /// - the PROACTIVE path is opted in ([`proactive_keep_warm`](Self::proactive_keep_warm), issue
    ///   #468 / finding #476 predicate C — OFF by default, so this whole path is inert unless an
    ///   operator sets `[refresh].proactive_keep_warm = true`; the reactive backstop is unaffected);
    /// - the seam is wired, an active account resolved, and its canonical blob is readable;
    /// - the active account is NOT quarantined (a dead account is the streak's job, not re-warmed);
    /// - the token is within `[refresh].cadence_secs + `[`keep_warm_stagger_secs`]` of expiry (the
    ///   per-account stagger de-correlates the roster's mints across the shared ~8h TTL); and
    /// - the proactive per-account throttle has elapsed (`last_keep_warm_attempt`), so a persistently
    ///   no-op mint (CC declines to refresh) cannot spawn `claude -p` every tick in the window.
    ///
    /// `now_ms` is the wall-clock epoch-ms the horizon compares the token's `expiresAt` against,
    /// taken as a parameter (not read inside) so the gate is unit-tested deterministically. Unlike
    /// the reactive path there is NO re-poll: a proactive promote simply leaves a fresh token for
    /// the NEXT tick's poll to read.
    pub(super) async fn keep_active_warm(
        &mut self,
        active: Option<usize>,
        canonical: Option<&Credential>,
        now_ms: i64,
        events: &mut Vec<Event>,
    ) {
        // Issue #468 / finding #476 predicate C: the proactive path is OFF by default. The
        // pre-emptive near-expiry mint rotates the LIVE shared canonical every cadence (~44 % of
        // daemon canonical churn, #476), so an operator opts in explicitly via
        // `[refresh].proactive_keep_warm`; otherwise the active account leans on the REACTIVE
        // backstop (`should_keep_warm_retry`, on a real 401) + the #467 autonomous adopt-target.
        // This gate is proactive-only: `should_keep_warm_retry` does NOT read the flag, so the
        // reactive backstop still fires. DO NOT remove this without the #476 fallback-A analysis —
        // gating proactive off is only safe because #467 recovers the scrub it makes likelier.
        if !self.proactive_keep_warm {
            return;
        }
        if self.keep_warm.is_none() {
            return;
        }
        let (Some(i), Some(canonical)) = (active, canonical) else {
            return;
        };
        // A quarantined active account is a dead credential the #42 streak / emergency swap owns —
        // never re-warmed every tick (invariant 4; mirrors `should_keep_warm_retry`'s guard).
        if self.state.health[i].quarantined {
            return;
        }
        // Near-expiry gate: fire only inside the token's staggered horizon. An unreadable expiry
        // → skip (no basis to decide); a far-from-expiry token → skip (nothing to warm yet).
        let Some(expires_at_ms) = crate::refresh::expires_at(canonical.expose()) else {
            return;
        };
        let stagger = keep_warm_stagger_secs(&self.roster[i].account_uuid, self.keep_warm_cadence);
        let horizon_ms = i64::try_from(self.keep_warm_cadence.as_secs().saturating_add(stagger))
            .unwrap_or(i64::MAX)
            .saturating_mul(1000);
        if expires_at_ms.saturating_sub(now_ms) > horizon_ms {
            return;
        }
        // Proactive throttle: at most one mint per keep-warm cadence (the reactive path ignores
        // this — it is once-per-episode-gated by `consec_401 == 0` — but a reactive mint still
        // stamps `last_keep_warm_attempt`, so it suppresses a redundant proactive mint the same
        // window).
        let now = self.clock.now();
        if let Some(last) = self.state.health[i].last_keep_warm_attempt {
            if now.saturating_duration_since(last) < self.keep_warm_cadence {
                return;
            }
        }
        // The proactive path discards the promote result: it warms the canonical for the NEXT
        // poll to read, it does not re-poll now.
        let _ = self
            .keep_warm_and_promote(i, canonical, KeepWarmTrigger::Proactive, events)
            .await;
    }

    /// Mint a fresh token for the active account `i` from its `canonical` blob and, on a real
    /// refresh, PROMOTE it to the canonical item (issue #282) — the shared core of the proactive and
    /// reactive paths. Returns a [`KeepWarmPromote`]: whether a fresh token was promoted, plus the
    /// cycle's classified outcome (issue #643, folded by the `use`-activate recovery re-probe; the
    /// reactive caller reads only `.promoted`, the proactive one discards the result).
    ///
    /// Steps: (1) short-circuit a dead/absent refresh token — CC has nothing to exchange, so skip
    /// the doomed spawn and report no-promote with a `Dead` outcome (the caller lets the #42 streak
    /// advance: invariant 4; the absence IS the dead signal, issue #643); (2) stamp
    /// `last_keep_warm_attempt` (the proactive throttle + reactive-suppresses-proactive signal)
    /// BEFORE the mint, so even a could-not-run attempt counts; (3) drive the keep-warm engine to
    /// mint; (4) push ONE durable [`Event::KeepWarm`] recording the action (mirrors
    /// [`refresh_retry`](Self::refresh_retry)'s `PollRefresh` event); (5) promote ONLY a real mint
    /// ([`RefreshOutcome::Refreshed`] → `Some(credential)`) via
    /// [`promote_canonical`](Self::promote_canonical); every other outcome
    /// (`NoChange` / `Dead` / `Error` / could-not-run) leaves the canonical item untouched.
    pub(super) async fn keep_warm_and_promote(
        &mut self,
        i: usize,
        canonical: &Credential,
        trigger: KeepWarmTrigger,
        events: &mut Vec<Event>,
    ) -> KeepWarmPromote {
        // A dead (empty) or absent refresh token cannot be revived by ANY mint — skip the doomed
        // `claude -p` spawn and report no-promote so the caller lets the #42 streak advance to
        // quarantine → emergency swap (invariant 4). Only a NON-empty RT is worth minting. Its
        // ABSENCE is itself the dead signal (issue #643): report `Dead` so a recovery re-probe folds
        // an honest 🔴, exactly as a completed cycle that returned `Dead` would.
        if !has_live_refresh_token(canonical) {
            return KeepWarmPromote {
                promoted: false,
                outcome: RefreshEventOutcome::Dead,
                token_rotated: false,
            };
        }
        // Stamp the attempt up front so BOTH the proactive throttle and the
        // reactive-suppresses-a-same-window-proactive signal count even a mint that cannot run.
        self.state.health[i].last_keep_warm_attempt = Some(self.clock.now());
        let minted = match self.keep_warm.as_ref() {
            Some(engine) => engine.keep_warm(&self.roster[i], canonical).await,
            // Unreachable given the callers' `keep_warm.is_some()` gate; treat as no-promote.
            None => {
                return KeepWarmPromote {
                    promoted: false,
                    outcome: RefreshEventOutcome::Error,
                    token_rotated: false,
                }
            }
        };
        // The cycle's non-secret classification, computed once and reused by the event + the return.
        let (outcome, token_rotated) = match &minted {
            Ok((report, _)) => (refresh_event_outcome(report), report.refresh_token_rotated),
            Err(_) => (RefreshEventOutcome::Error, false),
        };
        // Durably record the keep-warm ACTION (issue #282), for EVERY firing — the forensic trail
        // mirroring `refresh_retry`'s `PollRefresh`. A completed cycle maps through the shared
        // `refresh_event_outcome`; a cycle that could not even run (`Err`) is an `Error` outcome.
        events.push(Event::KeepWarm {
            account: self.roster[i].label.clone(),
            trigger,
            outcome,
            refresh_token_rotated: token_rotated,
        });
        // Promote ONLY a real mint; NoChange / Dead / Error / could-not-run leave canonical as-is.
        let promoted = match minted {
            Ok((_, Some(cred))) => self.promote_canonical(i, &cred).await.unwrap_or(false),
            _ => false,
        };
        KeepWarmPromote {
            promoted,
            outcome,
            token_rotated,
        }
    }

    /// Promote a freshly-minted `cred` to the canonical `Claude Code-credentials` item for the
    /// active account `i` (issue #282), serialized against the swap engine (ADR-0003 no-torn-swap).
    /// Returns whether the canonical was actually written (`Ok(false)` = a deliberate abort, not an
    /// error).
    ///
    /// The mint's `claude -p` spawn ran WITHOUT the swap lock (holding it across a multi-second
    /// spawn would stall every swap), so a concurrent `use` / auto swap could have moved the active
    /// account meanwhile. Under the SAME single-writer swap lock the swap engine holds, this
    /// RE-READS the canonical and confirms it still resolves to account `i` BEFORE overwriting:
    /// promoting a now-stale, account-`i`-derived token would CLOBBER that operator swap, so a
    /// changed active identity ABORTS with zero writes (the minted token is simply discarded; the
    /// #13/#42 recovery path reclaims a stranded credential if it ever matters). On the happy path
    /// the write is the keychain's atomic `add-generic-password -U` (a live session's next read
    /// sees the fresh token whole, never torn), then the canonical-watch baseline is committed so
    /// the #140 external-login watch and the next-tick #13 reconcile do NOT misfire on the daemon's
    /// OWN write. A contended lock exhausting its bounded wait fails closed (`Err(SwapLockBusy)`) —
    /// the caller treats it as no-promote, exactly like any other swap-lock refusal.
    pub(super) async fn promote_canonical(&mut self, i: usize, cred: &Credential) -> Result<bool> {
        // Serialize against the swap engine: take the SAME single-writer swap lock `use` / auto
        // swaps hold (when one is configured — hermetic tests run lock-free, no second writer).
        let _guard = match self.swap_lock_path.as_deref() {
            Some(path) => Some(SwapLock::acquire(path, SWAP_LOCK_MAX_WAIT).await?),
            None => None,
        };
        // Re-read UNDER THE LOCK and confirm account `i` is still active before overwriting — the
        // mint ran unlocked, so a swap may have raced it. A changed / unreadable canonical aborts
        // with zero writes rather than clobber a concurrent swap.
        let still_active = match self.store.read().await {
            Ok(current) => self.resolve_account_for(&current).await == Some(i),
            Err(_) => false,
        };
        if !still_active {
            return Ok(false);
        }
        // Atomic canonical write; the live session's next read sees the fresh token whole.
        self.store.write(cred).await?;
        // Baseline-commit so #140 / the #13 reconcile do not misfire on the daemon's own write.
        self.state.canonical_watch.commit(cred);
        // Issue #477: the token just promoted IS the live canonical the active account now
        // resolves to — so reconcile the rollup's staleness input to it. A keep-warm is
        // `refreshed_not_restashed` (it PROMOTES to canonical, never re-stashes), and the active
        // account is excluded from the parked refresh sweep that folds `access_expires_at`, so
        // without this the field stays pinned to the deliberately-stale un-restashed stash expiry
        // and `credential_health` false-fires `Stale` while the canonical is fresh (the 2026-07-11
        // forensic case). MS→s at the boundary, matching `apply_refresh_observation`. The genuine
        // signal is preserved: this only runs on a real promote (a written canonical), so a
        // dead/rejected canonical is never bumped and still reads `Stale` — whether short-circuited
        // BEFORE the mint (an empty refresh token, `has_live_refresh_token`) or AFTER it (a server
        // `Dead` / `NoChange` mint with no fresh token to promote, the `match minted` arm).
        self.state.health[i].access_expires_at =
            crate::refresh::expires_at(cred.expose()).map(millis_to_secs);
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::daemon::tests::*;

    use crate::observability::RefreshEventOutcome;

    // --- #282 in-place ACTIVE-account keep-warm (the FOURTH refresh mechanism) ----
    //
    // The active account's canonical token is kept warm IN PLACE: minted via the isolated
    // spawn on a COPY of the canonical blob, then PROMOTED to the canonical item a live
    // session reads (never the STASH the #253-excluded engine writes). Two firing paths —
    // PROACTIVE (before the token nears expiry) and REACTIVE (a backstop on an active 401,
    // reviving before the 401 counts toward the #42 streak) — plus a per-account STAGGER that
    // de-correlates the roster's mints across the shared ~8h TTL. These tests exercise the
    // seam directly (the near-expiry gate + throttle are pure functions of an injected
    // `now_ms` / [`FakeClock`]) and end-to-end through `tick`.

    #[tokio::test]
    async fn should_keep_warm_retry_is_the_active_only_complement_of_the_162_guard() {
        // The reactive backstop fires on exactly the case #253's `should_refresh_retry` EXCLUDES:
        // the ACTIVE account, first 401 of an episode, seam wired, not quarantined. The two guards
        // partition a 401 by active-ness, so a 401 takes exactly one refresh path.
        let (mut daemon, _outcomes, _calls) = keep_warm_daemon(
            Scripted::Unauthorized,
            RefreshOutcome::Refreshed,
            None,
            Some(cred(b"FRESH-A")),
            warm_canonical(FAR_FUTURE_MS, "rt-live"),
        )
        .await;
        let unauthorized = Err(Error::UsageUnauthorized);

        // Active (idx 0), first 401, seam wired, not quarantined → the keep-warm fires; and the
        // #162 isolated path does NOT (its active exclusion), so they never both fire.
        daemon.state.active = Some(0);
        assert!(daemon.should_keep_warm_retry(0, &unauthorized));
        assert!(!daemon.should_refresh_retry(0, &unauthorized));

        // NON-active (idx 1) is the #162 path's job, never the keep-warm's.
        assert!(!daemon.should_keep_warm_retry(1, &unauthorized));

        // A non-401 outcome never fires it.
        assert!(!daemon.should_keep_warm_retry(0, &Ok(reading(0.1, 0.1))));

        // Past the first 401 of the episode (`consec_401 > 0`) → suppressed (no mint storm; the
        // rest of the episode advances the streak directly toward the #42 emergency swap).
        daemon.state.health[0].consec_401 = 1;
        assert!(!daemon.should_keep_warm_retry(0, &unauthorized));
        daemon.state.health[0].consec_401 = 0;

        // A quarantined active account is the streak's job — never re-warmed every re-probe poll.
        daemon.state.health[0].quarantined = true;
        assert!(!daemon.should_keep_warm_retry(0, &unauthorized));
    }

    #[tokio::test]
    async fn should_keep_warm_retry_is_inert_without_a_wired_seam() {
        // With no keep-warm engine wired (the default / `[refresh]`-off daemon) an active 401 is
        // NEVER a keep-warm — the active account simply lapses exactly as before the fix.
        let mut daemon = lifecycle_daemon().await;
        daemon.state.active = Some(0);
        assert!(daemon.keep_warm.is_none());
        assert!(!daemon.should_keep_warm_retry(0, &Err(Error::UsageUnauthorized)));
    }

    #[tokio::test]
    async fn a_reactive_backstop_revives_an_active_401_and_promotes_the_canonical() {
        // AC-2 (positive): the active account's first 401, with a LIVE refresh token, mints a
        // fresh token in place, PROMOTES it to the canonical item, re-polls through it, and the
        // re-poll clears — so the #42 streak never advances (the false-death this fixes). The
        // canonical now holds the FRESH token a live session reads.
        let (mut daemon, _outcomes, calls) = keep_warm_daemon(
            Scripted::Unauthorized,
            RefreshOutcome::Refreshed,
            Some(reading(0.10, 0.10)), // the mint REVIVES the active token
            Some(cred(b"FRESH-A")),
            warm_canonical(FAR_FUTURE_MS, "rt-live"),
        )
        .await;
        let mut events = Vec::new();
        let result = daemon
            .keep_warm_retry(
                0,
                Some(&warm_canonical(FAR_FUTURE_MS, "rt-live")),
                &mut events,
            )
            .await;

        assert!(
            result.is_ok(),
            "a revived active 401 re-polls to a live reading"
        );
        assert_eq!(calls.get(), 1, "exactly one mint fired");
        assert_eq!(
            daemon.store.read().await.unwrap().expose(),
            b"FRESH-A",
            "the fresh token was promoted to the canonical item a live session reads",
        );
        assert_eq!(
            events,
            vec![Event::KeepWarm {
                account: "work".to_owned(),
                trigger: KeepWarmTrigger::Reactive,
                // A keep-warm promotes, so a real refresh renders `refreshed_not_restashed`.
                outcome: RefreshEventOutcome::RefreshedNotReStashed,
                refresh_token_rotated: true,
            }],
            "one durable keep_warm event records the reactive mint",
        );
    }

    #[tokio::test]
    async fn a_dead_refresh_token_active_401_advances_the_streak_without_minting() {
        // AC-2 / invariant 4 (empty-RT): a dead (empty) refresh token cannot be revived by any
        // mint, so the keep-warm SHORT-CIRCUITS — no `claude -p` spawn — and the 401 stands, so
        // the streak advances toward the #42 emergency swap. A truly-dead active credential still
        // quarantines; the escape to a live spare is preserved.
        let (mut daemon, _outcomes, calls) = keep_warm_daemon(
            Scripted::Unauthorized,
            RefreshOutcome::Refreshed,
            Some(reading(0.10, 0.10)), // would revive IF the mint ever ran — it must not
            Some(cred(b"FRESH-A")),
            warm_canonical(FAR_FUTURE_MS, ""), // EMPTY refresh token → dead
        )
        .await;
        let mut events = Vec::new();
        let result = daemon
            .keep_warm_retry(0, Some(&warm_canonical(FAR_FUTURE_MS, "")), &mut events)
            .await;

        assert!(
            matches!(result, Err(Error::UsageUnauthorized)),
            "a dead-RT active 401 stands so the #42 streak advances (invariant 4)",
        );
        assert_eq!(
            calls.get(),
            0,
            "a dead refresh token skips the doomed mint spawn"
        );
        assert!(
            events.is_empty(),
            "a skipped keep-warm emits no action event"
        );
        assert_ne!(
            daemon.store.read().await.unwrap().expose(),
            b"FRESH-A",
            "the canonical item is left untouched when there is nothing to revive",
        );
    }

    #[tokio::test]
    async fn a_reactive_mint_reporting_dead_advances_the_streak() {
        // AC-2 / invariant 4 (Dead outcome): the RT was non-empty at mint time (so the mint DOES
        // run), but CC cleared it in place and the cycle reports `Dead` — a genuine death. No
        // promote, the 401 stands, the streak advances. The mint fired exactly once.
        let (mut daemon, _outcomes, calls) = keep_warm_daemon(
            Scripted::Unauthorized,
            RefreshOutcome::Dead,
            Some(reading(0.10, 0.10)), // a Dead outcome hands back no credential regardless
            Some(cred(b"FRESH-A")),
            warm_canonical(FAR_FUTURE_MS, "rt-live"),
        )
        .await;
        let mut events = Vec::new();
        let result = daemon
            .keep_warm_retry(
                0,
                Some(&warm_canonical(FAR_FUTURE_MS, "rt-live")),
                &mut events,
            )
            .await;

        assert!(
            matches!(result, Err(Error::UsageUnauthorized)),
            "a Dead mint lets the 401 stand so the streak advances (invariant 4)",
        );
        assert_eq!(
            calls.get(),
            1,
            "the mint ran once (the RT was non-empty) and reported Dead"
        );
        assert_ne!(
            daemon.store.read().await.unwrap().expose(),
            b"FRESH-A",
            "a Dead outcome promotes nothing",
        );
        assert_eq!(
            events,
            vec![Event::KeepWarm {
                account: "work".to_owned(),
                trigger: KeepWarmTrigger::Reactive,
                outcome: RefreshEventOutcome::Dead,
                refresh_token_rotated: false,
            }],
        );
    }

    #[tokio::test]
    async fn a_reactive_backstop_end_to_end_never_quarantines_a_revivable_active_401() {
        // AC-2 end-to-end through `tick`: the active `work` account 401s on its first poll; the
        // reactive keep-warm mints + promotes + re-polls, the re-poll clears, and `work` is never
        // quarantined. The FAR-FUTURE expiry keeps the proactive path inert, isolating the reactive
        // one. The mint fired once (no storm) and the canonical carries the fresh token.
        let (mut daemon, _outcomes, calls) = keep_warm_daemon(
            Scripted::Unauthorized,
            RefreshOutcome::Refreshed,
            Some(reading(0.10, 0.10)),
            Some(cred(b"FRESH-A")),
            warm_canonical(FAR_FUTURE_MS, "rt-live"),
        )
        .await;
        // Tick 1 polls the active `work` first (issue #80 stagger) → its first 401 → the reactive
        // backstop revives it in place.
        let events = daemon.tick().await.events;

        assert!(
            !daemon.state.health[0].quarantined,
            "a revivable active 401 is kept warm in place, never quarantined",
        );
        assert_eq!(
            daemon.state.health[0].consec_401, 0,
            "the cleared re-poll resets the streak",
        );
        assert_eq!(calls.get(), 1, "exactly one mint (no storm)");
        assert_eq!(
            daemon.store.read().await.unwrap().expose(),
            b"FRESH-A",
            "the canonical item now holds the fresh token",
        );
        assert!(
            events.iter().any(|e| matches!(
                e,
                Event::KeepWarm {
                    trigger: KeepWarmTrigger::Reactive,
                    ..
                }
            )),
            "the tick emitted the reactive keep_warm action: {events:?}",
        );
    }

    #[tokio::test]
    async fn the_proactive_path_mints_within_the_near_expiry_horizon_and_promotes() {
        // AC-1: a token INSIDE its (staggered) near-expiry horizon is minted BEFORE any 401 and
        // the fresh token promoted to the canonical item — so a live session always reads a warm
        // token and the overnight false-death cascade never starts.
        let now_ms = 1_800_000_000_000;
        // 60 s to expiry, well inside the 1-hour+stagger horizon.
        let canonical = warm_canonical(now_ms + 60_000, "rt-live");
        let (mut daemon, _outcomes, calls) = keep_warm_daemon(
            Scripted::Ok(reading(0.10, 0.10)),
            RefreshOutcome::Refreshed,
            None,
            Some(cred(b"FRESH-A")),
            canonical.clone(),
        )
        .await;
        let mut events = Vec::new();
        daemon
            .keep_active_warm(Some(0), Some(&canonical), now_ms, &mut events)
            .await;

        assert_eq!(
            calls.get(),
            1,
            "a near-expiry active token is minted proactively"
        );
        assert_eq!(
            daemon.store.read().await.unwrap().expose(),
            b"FRESH-A",
            "the proactive mint promotes the fresh token to the canonical item",
        );
        assert!(
            daemon.state.health[0].last_keep_warm_attempt.is_some(),
            "the attempt is stamped for the proactive throttle",
        );
        assert_eq!(
            events,
            vec![Event::KeepWarm {
                account: "work".to_owned(),
                trigger: KeepWarmTrigger::Proactive,
                outcome: RefreshEventOutcome::RefreshedNotReStashed,
                refresh_token_rotated: true,
            }],
        );
    }

    #[tokio::test]
    async fn a_proactive_promote_reconciles_the_rollup_expiry_to_the_fresh_canonical() {
        // Issue #477 (AC-1 / AC-3): after a proactive keep-warm PROMOTES a fresh token to the
        // canonical, the rollup's staleness input (`access_expires_at`) must reflect that fresh
        // canonical — NOT the un-restashed stash the keep-warm deliberately leaves behind
        // (`refreshed_not_restashed`). The 2026-07-11 forensic case: a fresh canonical (7+ h to
        // expiry) beside an EXPIRED stash false-fired `stale`, because the rollup judged staleness
        // off the stale stash-sourced expiry the parked sweep last folded (the active account is
        // excluded from that sweep, so the field froze there and keep-warm never bumped it).
        let now_ms = 1_800_000_000_000;
        let now_secs = now_ms / 1000;
        // Active canonical 60 s from expiry → inside the near-expiry horizon, so the mint fires.
        let canonical = warm_canonical(now_ms + 60_000, "rt-live");
        // The promoted token is fresh: 7 h to expiry (the observed case).
        let fresh_expiry_ms = now_ms + 7 * 3_600_000;
        let (mut daemon, _outcomes, calls) = keep_warm_daemon(
            Scripted::Ok(reading(0.10, 0.10)),
            RefreshOutcome::Refreshed,
            None,
            Some(warm_canonical(fresh_expiry_ms, "rt-live")),
            canonical.clone(),
        )
        .await;
        // The un-restashed stash the parked sweep last folded is already EXPIRED — the
        // false-staleness source the keep-warm leaves behind.
        daemon.state.health[0].access_expires_at = Some(now_secs - 100);

        let mut events = Vec::new();
        daemon
            .keep_active_warm(Some(0), Some(&canonical), now_ms, &mut events)
            .await;

        assert_eq!(
            calls.get(),
            1,
            "the near-expiry active token is minted proactively"
        );
        // The fresh token reached the canonical the live sessions read…
        assert_eq!(
            crate::refresh::expires_at(daemon.store.read().await.unwrap().expose()),
            Some(fresh_expiry_ms),
            "the fresh token is promoted to the canonical item",
        );
        // …and the rollup's staleness input is reconciled to it (the #477 fix). Before the fix this
        // stayed pinned to the expired stash → a false `stale`.
        assert_eq!(
            daemon.state.health[0].access_expires_at,
            Some(fresh_expiry_ms / 1000),
            "the rollup expiry reflects the promoted canonical, not the stale stash",
        );
        // The verdict is judged off the fresh canonical → NOT `stale`. `has_fresh_reading = false`
        // isolates the expiry-driven verdict: it proves the FIX (not a masking fresh poll) cleared
        // the false stale — the Stale branch precedes the Healthy branch, so a stale expiry would
        // still read `stale` here regardless of a fresh reading.
        let h = &daemon.state.health[0];
        assert_eq!(
            credential_health(
                h.quarantined,
                h.last_refresh_outcome,
                h.consecutive_refresh_failures,
                h.access_expires_at,
                false,
                now_secs,
            ),
            CredentialHealth::Healthy,
            "a fresh promoted canonical reads healthy, not a false stale",
        );
    }

    #[tokio::test]
    async fn a_no_promote_keep_warm_leaves_a_genuinely_stale_canonical_stale() {
        // Issue #477 (AC-2): the #477 reconcile bumps `access_expires_at` ONLY on a real promote. A
        // genuinely dead / server-rejected canonical (an empty refresh token CC cannot exchange)
        // never mints and never promotes, so the stale expiry is preserved and the account still
        // reads `stale` — the real signal #464/#465 depend on is not lost.
        let now_ms = 1_800_000_000_000;
        let now_secs = now_ms / 1000;
        // Near expiry (inside the horizon) but a DEAD (empty) refresh token → `has_live_refresh_token`
        // false → no mint, no promote (the invariant-4 case the keep-warm must not try to revive).
        let canonical = warm_canonical(now_ms + 60_000, "");
        let (mut daemon, _outcomes, calls) = keep_warm_daemon(
            Scripted::Ok(reading(0.10, 0.10)),
            RefreshOutcome::Refreshed,
            None,
            Some(warm_canonical(now_ms + 7 * 3_600_000, "rt-live")),
            canonical.clone(),
        )
        .await;
        // A genuinely expired canonical expiry.
        daemon.state.health[0].access_expires_at = Some(now_secs - 100);

        let mut events = Vec::new();
        daemon
            .keep_active_warm(Some(0), Some(&canonical), now_ms, &mut events)
            .await;

        assert_eq!(calls.get(), 0, "a dead refresh token is never minted");
        // The stale expiry is UNTOUCHED — the reconcile only fires on a real promote.
        assert_eq!(
            daemon.state.health[0].access_expires_at,
            Some(now_secs - 100),
            "no promote → the rollup expiry is not bumped",
        );
        let h = &daemon.state.health[0];
        assert_eq!(
            credential_health(
                h.quarantined,
                h.last_refresh_outcome,
                h.consecutive_refresh_failures,
                h.access_expires_at,
                false,
                now_secs,
            ),
            CredentialHealth::Stale,
            "a genuinely stale / rejected canonical still reads stale",
        );
    }

    #[tokio::test]
    async fn the_proactive_path_skips_far_from_expiry_and_when_quarantined() {
        // AC-1 (negative) / no storm: a token far from expiry is NOT minted (nothing to warm yet),
        // and a quarantined active account is never re-warmed (the #42 streak owns it).
        let now_ms = 1_800_000_000_000;
        let canonical = warm_canonical(now_ms + 60_000, "rt-live"); // near expiry…
        let (mut daemon, _outcomes, calls) = keep_warm_daemon(
            Scripted::Ok(reading(0.10, 0.10)),
            RefreshOutcome::Refreshed,
            None,
            Some(cred(b"FRESH-A")),
            canonical.clone(),
        )
        .await;
        let mut events = Vec::new();

        // Far from expiry (100 days out) → skip.
        let far = warm_canonical(now_ms + 100 * 86_400_000, "rt-live");
        daemon
            .keep_active_warm(Some(0), Some(&far), now_ms, &mut events)
            .await;
        assert_eq!(calls.get(), 0, "a far-from-expiry token is not warmed");

        // …but even a near-expiry token is skipped once the account is quarantined.
        daemon.state.health[0].quarantined = true;
        daemon
            .keep_active_warm(Some(0), Some(&canonical), now_ms, &mut events)
            .await;
        assert_eq!(
            calls.get(),
            0,
            "a quarantined active account is never re-warmed"
        );
        assert!(events.is_empty(), "no mint → no event");
    }

    #[tokio::test]
    async fn proactive_keep_warm_gated_off_is_inert_but_the_reactive_backstop_still_fires() {
        // Issue #468 / finding #476 predicate C. With `proactive_keep_warm = false` (the production
        // DEFAULT) the proactive path is INERT even for a near-expiry active token — the pre-emptive
        // live-canonical mint (~44 % of daemon canonical churn, #476) no longer fires, so the shared
        // token is not rotated pre-emptively. The REACTIVE backstop is a SEPARATE consumer of the
        // same seam and is UNAFFECTED: an active 401 still routes to `should_keep_warm_retry`, so the
        // active token is kept warm exactly when a session needs it (predicate C leans on this + the
        // #467 autonomous adopt-target for the residual scrub window).
        let now_ms = 1_800_000_000_000;
        // 60 s to expiry — INSIDE the near-expiry horizon, so ONLY the #468 gate can suppress the
        // mint (isolating this gate from the far-from-expiry / quarantine / throttle gates).
        let canonical = warm_canonical(now_ms + 60_000, "rt-live");
        let (daemon, _outcomes, calls) = keep_warm_daemon(
            Scripted::Ok(reading(0.10, 0.10)),
            RefreshOutcome::Refreshed,
            None,
            Some(cred(b"FRESH-A")),
            canonical.clone(),
        )
        .await;
        // The seam-test helper opts proactive IN; override back to the production default (OFF).
        let mut daemon = daemon.with_proactive_keep_warm(false);

        let mut events = Vec::new();
        daemon
            .keep_active_warm(Some(0), Some(&canonical), now_ms, &mut events)
            .await;

        assert_eq!(
            calls.get(),
            0,
            "proactive gated off → a near-expiry active token is NOT pre-emptively minted",
        );
        assert!(
            events.is_empty(),
            "no proactive mint → no keep_warm event: {events:?}",
        );
        assert!(
            daemon.state.health[0].last_keep_warm_attempt.is_none(),
            "no mint attempted → the proactive throttle stamp is untouched",
        );
        assert_eq!(
            daemon.store.read().await.unwrap().expose(),
            canonical.expose(),
            "proactive gated off → the shared canonical token is left un-rotated (#468 AC-1)",
        );

        // #468 AC-3: the REACTIVE backstop keys off the seam alone, NOT the proactive flag — an active
        // 401 still triggers keep-warm, so active-expiry mid-use is still prevented.
        daemon.state.active = Some(0);
        assert!(
            daemon.should_keep_warm_retry(0, &Err(Error::UsageUnauthorized)),
            "the reactive backstop is unaffected by the proactive #468 gate",
        );
    }

    #[tokio::test]
    async fn the_proactive_throttle_admits_one_mint_per_cadence() {
        // No storm: while the token sits in the near-expiry window, the proactive path mints at
        // most once per keep-warm cadence — so a persistently no-op mint cannot spawn `claude -p`
        // every tick. The throttle RELEASES once the cadence elapses.
        let now_ms = 1_800_000_000_000;
        let canonical = warm_canonical(now_ms + 60_000, "rt-live");
        let (mut daemon, _outcomes, calls) = keep_warm_daemon(
            Scripted::Ok(reading(0.10, 0.10)),
            RefreshOutcome::Refreshed,
            None,
            Some(cred(b"FRESH-A")),
            canonical.clone(),
        )
        .await;
        let mut events = Vec::new();

        daemon
            .keep_active_warm(Some(0), Some(&canonical), now_ms, &mut events)
            .await;
        // A second attempt at the SAME instant (frozen clock) is throttled.
        daemon
            .keep_active_warm(Some(0), Some(&canonical), now_ms, &mut events)
            .await;
        assert_eq!(
            calls.get(),
            1,
            "a second mint inside the cadence is throttled"
        );

        // Once the cadence elapses, the next attempt mints again.
        daemon.clock.advance(Duration::from_secs(3601));
        daemon
            .keep_active_warm(Some(0), Some(&canonical), now_ms, &mut events)
            .await;
        assert_eq!(calls.get(), 2, "the throttle releases after one cadence");
    }

    #[tokio::test]
    async fn a_promote_aborts_when_a_swap_raced_the_mint() {
        // Invariant 2 (no-torn-swap, ADR-0003): the mint runs WITHOUT the swap lock, so a `use` /
        // auto swap can land meanwhile. Under the lock, `promote_canonical` re-reads the canonical
        // and, finding it no longer resolves to the account it minted for, ABORTS with ZERO writes
        // — never clobbering the concurrent swap.
        let (mut daemon, _outcomes, _calls) = keep_warm_daemon(
            Scripted::Ok(reading(0.10, 0.10)),
            RefreshOutcome::Refreshed,
            None,
            Some(cred(b"FRESH-A")),
            warm_canonical(FAR_FUTURE_MS, "rt-live"),
        )
        .await;
        // Simulate a swap that landed during the mint: the canonical now token-matches `spare`
        // (`u-B` / idx 1), NOT the active `work` (idx 0) the mint targeted.
        daemon.store.write(&cred(b"B-token")).await.unwrap();

        let promoted = daemon
            .promote_canonical(0, &cred(b"FRESH-A"))
            .await
            .unwrap();
        assert!(
            !promoted,
            "a raced swap aborts the promote (Ok(false), a deliberate no-op)"
        );
        assert_eq!(
            daemon.store.read().await.unwrap().expose(),
            b"B-token",
            "the concurrent swap's canonical is left intact — zero writes on abort",
        );
    }

    #[tokio::test]
    async fn a_keep_warm_promote_commits_the_baseline_so_the_140_watch_does_not_misfire() {
        // Invariant 3 (#140 external-login watch): the daemon's OWN in-place canonical write must
        // NOT read back as an operator re-login. `promote_canonical` baseline-commits the fresh
        // credential, so the very next `reconcile_canonical_change` classifies it Unchanged and
        // emits nothing (no ReStash / UncapturedLogin).
        let (mut daemon, _outcomes, _calls) = keep_warm_daemon(
            Scripted::Ok(reading(0.10, 0.10)),
            RefreshOutcome::Refreshed,
            None,
            Some(cred(b"FRESH-A")),
            warm_canonical(FAR_FUTURE_MS, "rt-live"),
        )
        .await;
        daemon.state.active = Some(0);
        // Seed the watch baseline to the current canonical (as top-of-tick does), then promote.
        let seed = daemon.store.read().await.unwrap();
        daemon.state.canonical_watch.commit(&seed);
        let promoted = daemon
            .promote_canonical(0, &cred(b"FRESH-A"))
            .await
            .unwrap();
        assert!(promoted);

        // The next reconcile against the just-promoted canonical sees NO change → no event.
        let mut events = Vec::new();
        let fresh = daemon.store.read().await.unwrap();
        daemon.reconcile_canonical_change(&fresh, &mut events).await;
        assert!(
            events.is_empty(),
            "the daemon's own keep-warm write must not misfire the #140 watch: {events:?}",
        );
    }

    #[test]
    fn the_keep_warm_stagger_is_deterministic_bounded_and_de_correlated() {
        // AC-3: the per-account stagger de-correlates the roster's keep-warm mints across the
        // shared TTL. It is (a) a deterministic pure function of the uuid — STABLE across restarts,
        // (b) bounded to `[0, cadence)` so no account is ever starved past the `cadence` floor, and
        // (c) DISTINCT across accounts (distinct uuids draw distinct offsets), which is the
        // de-correlation.
        let cadence = Duration::from_secs(3600);
        let a = keep_warm_stagger_secs("u-A", cadence);
        let b = keep_warm_stagger_secs("u-B", cadence);
        let c = keep_warm_stagger_secs("u-C", cadence);

        // Deterministic: same uuid → same offset, every call.
        assert_eq!(
            a,
            keep_warm_stagger_secs("u-A", cadence),
            "stagger is stable per uuid"
        );
        // Bounded to the window (never starves an account past the cadence floor).
        for (uuid, offset) in [("u-A", a), ("u-B", b), ("u-C", c)] {
            assert!(
                offset < 3600,
                "{uuid} stagger {offset} escaped [0, cadence)"
            );
        }
        // De-correlated: distinct accounts draw distinct phases (the whole point).
        assert!(
            a != b && b != c && a != c,
            "distinct uuids must de-correlate: {a}, {b}, {c}",
        );
        // A zero cadence degenerates safely to 0 (no window to stagger within).
        assert_eq!(keep_warm_stagger_secs("u-A", Duration::ZERO), 0);
    }

    #[tokio::test]
    async fn a_near_expiry_active_401_mints_once_not_twice_in_a_tick() {
        // AC-1 "no storm" under the real overnight scenario: when the active token is BOTH inside
        // its near-expiry horizon AND returns a 401 in the same tick, the reactive backstop fires
        // and — because it stamps `last_keep_warm_attempt` — the proactive path that runs later the
        // same tick is THROTTLED. Exactly ONE `claude -p` mint, never two. (Every other keep-warm
        // test isolates one path — a healthy poll for proactive, `FAR_FUTURE_MS` for reactive — so
        // this is the only test that intersects them, the crux of the no-double-mint property.)
        let near = wall_clock_now_ms() + 60_000; // 60 s to expiry → inside the horizon
        let (mut daemon, _outcomes, calls) = keep_warm_daemon(
            Scripted::Unauthorized, // the active account 401s this tick
            RefreshOutcome::Refreshed,
            Some(reading(0.10, 0.10)), // the reactive mint revives it
            Some(cred(b"FRESH-A")),
            warm_canonical(near, "rt-live"),
        )
        .await;
        // Tick 1 polls the active `work` first (#80 stagger): 401 → reactive mint → revive →
        // re-poll clears; then the proactive pass runs (near-expiry gate TRUE) but is throttled.
        daemon.tick().await;
        assert_eq!(
            calls.get(),
            1,
            "a near-expiry active 401 mints exactly once — the reactive stamp throttles proactive",
        );
        assert_eq!(
            daemon.state.health[0].consec_401, 0,
            "the revive reset the streak"
        );
        assert!(!daemon.state.health[0].quarantined);
    }

    #[tokio::test]
    async fn a_promote_that_survives_a_still_401ing_token_lets_the_401_stand() {
        // A genuine server-side revocation: the mint reports `Refreshed` and the fresh token IS
        // promoted, but the re-poll through the (now-fresh) canonical STILL 401s — the fresh token
        // did not actually fix the problem. The 401 must stand so the streak advances (never a
        // false "revived" that masks a dead credential). `revive_to = None` keeps the active poll
        // 401ing even after the promote.
        let (mut daemon, _outcomes, calls) = keep_warm_daemon(
            Scripted::Unauthorized,
            RefreshOutcome::Refreshed,
            None, // the mint promotes a fresh token but does NOT revive the poll outcome
            Some(cred(b"FRESH-A")),
            warm_canonical(FAR_FUTURE_MS, "rt-live"),
        )
        .await;
        let mut events = Vec::new();
        let result = daemon
            .keep_warm_retry(
                0,
                Some(&warm_canonical(FAR_FUTURE_MS, "rt-live")),
                &mut events,
            )
            .await;

        assert!(
            matches!(result, Err(Error::UsageUnauthorized)),
            "a fresh token that still 401s lets the 401 stand so the streak advances",
        );
        assert_eq!(calls.get(), 1, "the mint ran once");
        assert_eq!(
            daemon.store.read().await.unwrap().expose(),
            b"FRESH-A",
            "the fresh token WAS promoted — the problem is server-side, not a mint failure",
        );
    }

    #[tokio::test]
    async fn a_promote_under_a_configured_swap_lock_acquires_and_writes() {
        // Invariant 2 (production path): with a swap lock configured, `promote_canonical` acquires
        // it (uncontended here), re-reads the canonical under it, confirms the account is still
        // active, and writes the fresh token atomically. The daemon tests otherwise run lock-free
        // (`swap_lock_path = None`); this exercises the `Some(path)` branch the daemon uses in
        // production. (The contended `SwapLockBusy` fail-closed is covered in `swap.rs`.)
        let lock_dir = tempfile::tempdir().unwrap();
        let (mut daemon, _outcomes, _calls) = keep_warm_daemon(
            Scripted::Ok(reading(0.10, 0.10)),
            RefreshOutcome::Refreshed,
            None,
            Some(cred(b"FRESH-A")),
            warm_canonical(FAR_FUTURE_MS, "rt-live"),
        )
        .await;
        daemon = daemon.with_swap_lock(lock_dir.path().join("swap.lock"));
        daemon.state.active = Some(0);

        let promoted = daemon
            .promote_canonical(0, &cred(b"FRESH-A"))
            .await
            .unwrap();
        assert!(
            promoted,
            "an uncontended locked promote acquires and writes"
        );
        assert_eq!(
            daemon.store.read().await.unwrap().expose(),
            b"FRESH-A",
            "the fresh token was written under the swap lock",
        );
    }

    #[tokio::test]
    async fn a_dead_non_active_account_is_skipped_while_the_rotation_continues() {
        // Quarantine-one (never halt): a dead SPARE is skipped in polling — not a
        // wasted curl, not a swap candidate — while the active account still rotates
        // to a healthy target. The daemon never halts the whole rotation on one dead
        // account.
        let roster = vec![
            account("u-A", "work"),
            account("u-B", "spare"),
            account("u-C", "backup"),
        ];
        let store = store_holding(b"A-token").await;
        let stash = stash_with(&[
            ("Sessiometer/u-A", b"A-token", "u-A"),
            ("Sessiometer/u-B", b"B-token", "u-B"),
            ("Sessiometer/u-C", b"C-token", "u-C"),
        ])
        .await;
        let (_dir, json) = claude_json("u-A");
        let poller = FakeRosterPoller::new()
            .ok("u-A", 0.97, 0.10) // active, over the session trigger → wants a swap
            .unauthorized("u-B") // scripted to 401 — but it is dead, so never polled
            .ok("u-C", 0.10, 0.10); // the only healthy target
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
        // `spare` is already dead from a prior episode.
        daemon.state.health[1].quarantined = true;

        // The staggered schedule (#80) is [work, backup] — the quarantined spare is
        // excluded outright — so the warm-up cycle polls only those two; the swap
        // fires on the warm-up-completing tick.
        let outcome = warmed_tick(&mut daemon).await;

        // The rotation continues: the active account swaps to the healthy `backup`,
        // NOT to the dead `spare` (a quarantined account is never a target).
        assert_eq!(outcome.action, TickAction::Swapped { from: 0, to: 2 });
        // `spare` was skipped, not polled: its 401 script never ran, so its streak
        // stayed 0 and it emitted no `monitor_401`.
        assert_eq!(
            daemon.state.health[1].consec_401, 0,
            "the dead spare was not polled"
        );
        assert!(
            !outcome.events.iter().any(|e| matches!(
                e,
                Event::Monitor401 { account, .. } if account == "spare"
            )),
            "a skipped account emits no poll-outcome event: {:?}",
            outcome.events
        );
    }

    #[tokio::test]
    async fn an_emergency_swap_escapes_a_dead_active_account_bypassing_trigger_and_cooldown() {
        // Emergency-swap: a confirmed-dead ACTIVE account is escaped IMMEDIATELY to
        // the soonest-reset viable target, bypassing BOTH the swap-away trigger (the
        // dead account has no reading to be "over") and the cooldown. A long cooldown
        // plus a just-completed swap would make a NORMAL over-trigger swap
        // `SkippedCooldown`; the emergency path overrides it.
        let mut daemon =
            lifecycle_daemon_with(FakeRosterPoller::new(), tunables(95, 80, 9_999)).await;
        let at = daemon.clock.now();
        daemon.state.active = Some(0);
        daemon.state.health[0].quarantined = true;
        daemon.state.last_swap = Some(LastSwap {
            at, // zero elapsed against a 9_999s cooldown → a normal swap would defer
        });

        // The dead active has no reading (still 401ing); the spare polled live.
        let readings = vec![
            None,
            Some(Usage {
                session: 0.10,
                weekly: 0.10,
                weekly_resets_at: None,
                session_resets_at: None,
            }),
        ];
        let mut events = Vec::new();
        let action = daemon
            .decide_action(at, Some(0), &readings, &mut events)
            .await;

        assert_eq!(action, TickAction::EmergencySwapped { from: 0, to: 1 });
        assert_eq!(
            events,
            vec![Event::EmergencySwap {
                from: "work".to_owned(),
                to: "spare".to_owned(),
            }]
        );
        // The swap took effect: the spare is now active.
        assert_eq!(daemon.state.active, Some(1));
    }

    #[tokio::test]
    async fn emergency_swap_escapes_a_dead_active_ignoring_the_floor() {
        // #398 atomicity: the emergency path drops the target-max-session-usage reserve. A
        // confirmed-dead ACTIVE account must escape to the ONLY live target even when
        // that target sits OVER the default-on floor (0.80) — liveness beats the
        // reserve. Without the floor-drop (emergency passes `None`, not the configured
        // floor), a default-on floor plus an over-floor live spare would strand the
        // daemon on the dead credential (`ActiveDeadNoTarget`) — a self-DoS. This test
        // gates shipping the default-on flip together with the emergency floor-drop.
        let mut daemon = lifecycle_daemon_with(FakeRosterPoller::new(), tunables(95, 80, 0)).await;
        let at = daemon.clock.now();
        daemon.state.active = Some(0);
        daemon.state.health[0].quarantined = true;

        // Dead active has no reading; the spare polled live but is OVER the floor
        // (0.85 ≥ 0.80) — the PROACTIVE path would exclude it, the emergency path must
        // not. It is weekly-viable (0.10) and below the session trigger (0.85 < 0.95),
        // so ONLY the floor could have blocked it.
        let readings = vec![
            None,
            Some(Usage {
                session: 0.85,
                weekly: 0.10,
                weekly_resets_at: None,
                session_resets_at: None,
            }),
        ];
        let mut events = Vec::new();
        let action = daemon
            .decide_action(at, Some(0), &readings, &mut events)
            .await;

        assert_eq!(
            action,
            TickAction::EmergencySwapped { from: 0, to: 1 },
            "the dead active must escape to the over-floor live spare (floor dropped on emergency)"
        );
        assert_eq!(
            events,
            vec![Event::EmergencySwap {
                from: "work".to_owned(),
                to: "spare".to_owned(),
            }]
        );
        assert_eq!(daemon.state.active, Some(1));
    }

    #[tokio::test]
    async fn a_recovering_active_account_is_held_never_swapped_away() {
        // Thrash-safety / protect-recovery: a quarantined ACTIVE account that is
        // polling live again is the operator's re-login recovering it. Hold — never
        // emergency-swap a credential that now works, never swap away mid-recovery.
        let mut daemon = lifecycle_daemon().await;
        let at = daemon.clock.now();
        daemon.state.active = Some(0);
        daemon.state.health[0].quarantined = true;

        // The active account polled live (recovering); the spare is also available.
        let readings = vec![
            Some(Usage {
                session: 0.10,
                weekly: 0.10,
                weekly_resets_at: None,
                session_resets_at: None,
            }),
            Some(Usage {
                session: 0.10,
                weekly: 0.10,
                weekly_resets_at: None,
                session_resets_at: None,
            }),
        ];
        let mut events = Vec::new();
        let action = daemon
            .decide_action(at, Some(0), &readings, &mut events)
            .await;

        assert_eq!(action, TickAction::Held);
        assert!(
            events.is_empty(),
            "a held recovery emits nothing: {events:?}"
        );
        // `decide_action` never recovers — only `note_poll_outcome` does — so the
        // account is still quarantined here.
        assert!(daemon.state.health[0].quarantined);
        assert_eq!(daemon.state.active, Some(0), "no swap away mid-recovery");
    }

    #[tokio::test]
    async fn a_manual_swap_away_mid_recovery_drops_the_phantom_recovery_probe() {
        // Issue #108: `decide_action` HOLDS the daemon's OWN swap away from a recovering
        // active account (`a_recovering_active_account_is_held_never_swapped_away`), but a
        // manual `use` bypasses that hold. Swapping AWAY from an account mid-recovery
        // turns it into a non-active dead spare — never polled (`build_poll_schedule`) —
        // so its recovery probe would FREEZE below M forever, a phantom partial-progress
        // counter that leaves it durably `needs re-login` while LOOKING mid-recovery.
        // Adopting the manual swap drops the probe so the dead-spare state is honest:
        // still quarantined, no in-flight recovery. This is the control-socket door
        // (`adopt_manual_swap`).
        let mut daemon = lifecycle_daemon().await;
        // `work` (active) is mid-recovery: quarantined, but its OWN token started
        // answering again — 1 of `monitor_recovery_m` = 2 live polls accrued.
        daemon.state.active = Some(0);
        daemon.state.health[0].quarantined = true;
        daemon.state.health[0].recovery_successes = 1;

        // The operator runs `use spare`: the canonical now holds spare's token and the
        // control socket signals the daemon to adopt the manual choice.
        daemon.store.write(&cred(b"B-token")).await.unwrap();
        daemon.adopt_manual_swap().await;

        assert_eq!(daemon.state.active, Some(1), "the manual choice is adopted");
        assert!(
            daemon.state.health[0].quarantined,
            "still dead — a swap-away never recovers an account"
        );
        assert_eq!(
            daemon.state.health[0].recovery_successes, 0,
            "the frozen probe is dropped — no phantom partial progress (#108)"
        );
        // The departed account is now indistinguishable from any other dead spare: still
        // quarantined, but with no in-flight recovery on a slot that is never polled.
    }

    #[tokio::test]
    async fn the_reconcile_seam_also_drops_a_mid_recovery_probe_on_a_detected_swap_away() {
        // Issue #108, second door: when the daemon NOTICES the out-of-band canonical
        // change itself (no control-socket signal reached `adopt_manual_swap`) the same
        // reset must fire. `reconcile_canonical_change` re-resolves active to the swap-TO
        // account and drops the departing mid-recovery account's frozen probe, while
        // leaving the swap-TO account untouched.
        let mut daemon = lifecycle_daemon().await;
        daemon.state.active = Some(0);
        daemon.state.health[0].quarantined = true;
        daemon.state.health[0].recovery_successes = 1;

        let mut events = Vec::new();
        // Prime the watch on `work`'s current canonical (A-token): first observation,
        // no change detected, nothing reset.
        daemon
            .reconcile_canonical_change(&cred(b"A-token"), &mut events)
            .await;
        assert_eq!(
            daemon.state.health[0].recovery_successes, 1,
            "priming the watch changes no health"
        );

        // The canonical now holds spare's token — an out-of-band manual swap the daemon
        // detects on its own. Reconcile re-stashes spare and drops work's frozen probe.
        daemon
            .reconcile_canonical_change(&cred(b"B-token"), &mut events)
            .await;

        assert!(
            daemon.state.health[0].quarantined,
            "work is still dead — a detected swap-away never recovers it"
        );
        assert_eq!(
            daemon.state.health[0].recovery_successes, 0,
            "the frozen probe is dropped on the reconcile seam too (#108)"
        );
        assert_eq!(
            daemon.state.active, None,
            "active is dropped for re-resolution against the new canonical"
        );
        // The swap-TO account (`spare`) is healthy and untouched by the probe reset.
        assert!(!daemon.state.health[1].quarantined);
        assert_eq!(daemon.state.health[1].recovery_successes, 0);
    }

    #[tokio::test]
    async fn the_reconcile_seam_drops_the_stale_active_on_an_unresolvable_canonical() {
        // Issue #208, the None-branch counterpart to the swap-away test above: a forced
        // logout / `/login` into an UN-CAPTURED account makes the canonical resolve to NO
        // roster account (`Changed → None`). The stale cached `state.active` must be
        // dropped — mirroring the re-stash (`Changed → Some`) branch — so `status` stops
        // showing a false `*` on the now-inactive account and `decide_action` routes to
        // the safe `SkippedActiveUnknown` path instead of acting on a phantom index.
        // Before the fix the None-branch committed the watch baseline WITHOUT resetting
        // `state.active`, so the stale index survived precisely when the operator trusts
        // `status` during an incident.
        let mut daemon = lifecycle_daemon().await;
        daemon.state.active = Some(0);

        let mut events = Vec::new();
        // Prime the watch on `work`'s current canonical (A-token): first observation, no
        // change detected, the resolved active left in place.
        daemon
            .reconcile_canonical_change(&cred(b"A-token"), &mut events)
            .await;
        assert_eq!(
            daemon.state.active,
            Some(0),
            "priming the watch leaves the resolved active untouched"
        );

        // The canonical now holds a token no stash matches AND the display switches to a
        // uuid not in the roster, so it resolves to no roster account (the None-branch).
        crate::claude_state::write_oauth_account(&daemon.claude_json, &oauth("u-Z")).unwrap();
        daemon
            .reconcile_canonical_change(&cred(b"Z-token"), &mut events)
            .await;

        assert_eq!(
            daemon.state.active, None,
            "the stale active is dropped when the canonical resolves to no roster account (#208)"
        );
        // The un-captured login is still surfaced (never onboarded) — the None-branch's
        // existing behavior is preserved alongside the active reset.
        assert!(
            events
                .iter()
                .any(|e| matches!(e, Event::UncapturedLogin { .. })),
            "the un-captured login is still surfaced: {events:?}"
        );

        // Consequence (AC-3): with the active now unknown, `decide_action` takes the safe
        // poll-only path and fires NO emergency swap on a phantom index.
        let at = daemon.clock.now();
        let active = daemon.state.active;
        let readings = vec![None, None];
        let mut decide_events = Vec::new();
        let action = daemon
            .decide_action(at, active, &readings, &mut decide_events)
            .await;
        assert_eq!(action, TickAction::SkippedActiveUnknown);
        assert!(
            decide_events.is_empty(),
            "no swap fires on an unknown active: {decide_events:?}"
        );
    }

    #[tokio::test]
    async fn a_dead_active_account_with_no_viable_target_signals_the_strand_once() {
        // Emergency-swap with nowhere to go: a dead active account whose only other account is
        // also unavailable holds (`ActiveDeadNoTarget`) without thrashing — and now SURFACES the
        // strand once (issue #405), the strictly-worse sibling of `all_exhausted`, which until now
        // returned SILENTLY (the `credential_dead` transition already fired, but nothing named the
        // fleet-capacity blocker or when it lifts).
        let mut daemon = lifecycle_daemon().await;
        let at = daemon.clock.now();
        daemon.state.active = Some(0);
        daemon.state.health[0].quarantined = true;

        // No other account has a reading → no viable target.
        let readings = vec![None, None];
        let mut events = Vec::new();
        let action = daemon
            .decide_action(at, Some(0), &readings, &mut events)
            .await;

        assert_eq!(action, TickAction::ActiveDeadNoTarget);
        // ONE edge-triggered durable event names the DEAD active held (the re-login target) and WHY
        // relief is blocked (`weekly` — the emergency path bypasses the session gate, so a
        // session-only block cannot arise). No spare reported a weekly reset → `resets_at` absent.
        // (Secret-freeness is covered exhaustively by the observability redaction scanner.)
        assert_eq!(
            events,
            vec![Event::ActiveDeadNoTarget {
                hold: "work".to_owned(),
                cause: SwapReason::Weekly,
                resets_at: None,
            }]
        );
        assert_eq!(daemon.state.active, Some(0), "no swap with no target");

        // Edge-triggered: a SECOND identical tick re-signals NOTHING (the latch is set), so the
        // strand does not spam the log once per emergency tick while every spare stays exhausted.
        let mut again = Vec::new();
        let action = daemon
            .decide_action(at, Some(0), &readings, &mut again)
            .await;
        assert_eq!(action, TickAction::ActiveDeadNoTarget);
        assert!(
            again.is_empty(),
            "the stuck strand re-signals nothing on repeat: {again:?}"
        );
    }

    #[tokio::test]
    async fn leaving_the_active_dead_no_target_strand_emits_the_cleared_marker_once() {
        // The matching LEAVE edge (issue #405), mirroring `all_exhausted_cleared`: once the strand
        // clears (the dead active recovered, or a target became reachable), the daemon emits ONE
        // `active_dead_no_target_cleared` and resets the guard — so a stale strand reading is told
        // from a current one. A healthy active can never itself strand, so ANY tick here leaves it.
        let mut daemon = lifecycle_daemon().await;
        // Simulate having entered the strand on a prior episode.
        daemon.state.signaled_active_dead_no_target = true;

        let first = daemon.tick().await;
        assert_ne!(
            first.action,
            TickAction::ActiveDeadNoTarget,
            "a healthy active never strands"
        );
        assert!(
            first
                .diagnostics
                .contains(&Diagnostic::ActiveDeadNoTargetCleared),
            "leaving the strand emits the cleared marker: {:?}",
            first.diagnostics
        );
        assert!(
            !daemon.state.signaled_active_dead_no_target,
            "the guard is reset on exit"
        );

        // Edge-triggered: a subsequent non-strand tick does NOT re-emit the cleared marker.
        let second = daemon.tick().await;
        assert!(
            !second
                .diagnostics
                .contains(&Diagnostic::ActiveDeadNoTargetCleared),
            "the LEAVE edge fires once, not every non-strand tick: {:?}",
            second.diagnostics
        );
    }

    #[tokio::test]
    async fn m_consecutive_live_polls_recover_a_quarantined_account_and_signal_once() {
        // Spontaneous-revival auto-recovery (no re-login): a dead ACTIVE account whose
        // own token starts answering again un-quarantines after M consecutive live
        // polls, emitting exactly one `credential_restored` on the dead→alive edge. (A
        // re-login takes the immediate #107 path in reconcile_canonical_change instead —
        // see `a_relogin_un_quarantines_a_dead_account_immediately_on_restash`.)
        let mut daemon = lifecycle_daemon().await;
        daemon.state.health[0].quarantined = true;
        let mut events = Vec::new();

        // The first live poll while quarantined is a recovery PROBE — still dead,
        // and silent (below `monitor_recovery_m` = 2).
        daemon.note_poll_outcome(0, &live(0.10, 0.10), &mut events);
        assert!(daemon.state.health[0].quarantined);
        assert_eq!(daemon.state.health[0].recovery_successes, 1);
        assert!(events.is_empty());

        // The 2nd consecutive live reaches the threshold → RESTORED (one event).
        daemon.note_poll_outcome(0, &live(0.10, 0.10), &mut events);
        assert!(!daemon.state.health[0].quarantined);
        assert_eq!(
            daemon.state.health[0].recovery_successes, 0,
            "the probe resets on restore"
        );
        assert_eq!(
            events,
            vec![Event::CredentialRestored {
                account: "work".to_owned(),
            }]
        );

        // A later live on the now-healthy account emits nothing (edge-triggered).
        events.clear();
        daemon.note_poll_outcome(0, &live(0.10, 0.10), &mut events);
        assert!(events.is_empty());
    }

    #[tokio::test]
    async fn a_401_mid_recovery_resets_the_probe_so_recovery_must_restart() {
        // The recovery streak is consecutive: a 401 partway through breaks it, so a
        // single later live is NOT enough — a full M=2 fresh live polls are required.
        let mut daemon = lifecycle_daemon().await;
        daemon.state.health[0].quarantined = true;
        let mut events = Vec::new();

        daemon.note_poll_outcome(0, &live(0.10, 0.10), &mut events); // probe at 1
        assert_eq!(daemon.state.health[0].recovery_successes, 1);
        // A 401 mid-recovery breaks the streak (and is silent — already dead).
        daemon.note_poll_outcome(0, &Err(Error::UsageUnauthorized), &mut events);
        assert_eq!(
            daemon.state.health[0].recovery_successes, 0,
            "the 401 reset the probe"
        );
        assert!(daemon.state.health[0].quarantined);
        assert!(events.is_empty());

        // One live after the reset is not enough; the second crosses the threshold.
        daemon.note_poll_outcome(0, &live(0.10, 0.10), &mut events);
        assert!(
            daemon.state.health[0].quarantined,
            "one live after a reset is not enough"
        );
        daemon.note_poll_outcome(0, &live(0.10, 0.10), &mut events);
        assert!(!daemon.state.health[0].quarantined);
        assert_eq!(
            events,
            vec![Event::CredentialRestored {
                account: "work".to_owned(),
            }]
        );
    }

    #[tokio::test]
    async fn a_quarantined_account_surfaces_a_durable_needs_relogin_status() {
        // Signal — the durable status: a dead account is reported `quarantined` in
        // the `status` snapshot and on the wire, carrying a stable handle but no
        // token and no email (#15).
        let poller = FakeRosterPoller::new().ok("u-A", 0.10, 0.10); // active holds
        let mut daemon = lifecycle_daemon_with(poller, tunables(95, 80, 0)).await;
        daemon.state.health[1].quarantined = true; // `spare` is dead

        let outcome = daemon.tick().await;

        let spare = &outcome.snapshot.accounts[1];
        assert_eq!(spare.label, "spare");
        assert!(
            spare.quarantined,
            "the dead account carries a durable status"
        );
        // The wire projection carries the flag but never a secret. A genuinely dead
        // account (quarantined, NOT mid-recovery) projects `recovering: false` (#109).
        assert!(!spare.recovering, "a dead account is not yet recovering");
        let json = serde_json::to_string(&status_response(&outcome.snapshot)).unwrap();
        assert!(json.contains(r#""quarantined":true"#), "got {json}");
        assert!(json.contains(r#""recovering":false"#), "got {json}");
        assert!(
            crate::redaction::meter::unauthored_emails(&json, &[]).is_empty(),
            "no non-authored email on the wire (#15/#444): {json}"
        );
        assert!(!json.to_lowercase().contains("token"));
    }

    #[tokio::test]
    async fn a_mid_recovery_account_surfaces_recovering_on_the_wire() {
        // Issue #109: a quarantined account whose credential is answering again —
        // `recovery_successes > 0` but below the un-quarantine threshold — is reported
        // `recovering` in the snapshot and on the wire, a refinement of `quarantined`
        // (still true) that lets `status` soften `needs re-login` to `recovering`.
        // Non-secret like every other status field (#15). Built through the real
        // `note_poll_outcome` → `snapshot` derivation, not a hand-set flag.
        let mut daemon = lifecycle_daemon().await;
        daemon.state.active = Some(0);
        daemon.state.health[0].quarantined = true; // `work` is dead…

        // …but its OWN token answers one live probe: still quarantined (below
        // monitor_recovery_m = 2), now mid-recovery.
        let mut events = Vec::new();
        daemon.note_poll_outcome(0, &live(0.10, 0.10), &mut events);
        assert!(daemon.state.health[0].quarantined);
        assert_eq!(daemon.state.health[0].recovery_successes, 1);

        // The snapshot derives `recovering` from that health; the healthy spare does not.
        let readings = vec![
            Some(live(0.10, 0.10).unwrap()),
            Some(live(0.20, 0.20).unwrap()),
        ];
        let snapshot = daemon.snapshot(Some(0), &readings, 0);
        let work = &snapshot.accounts[0];
        assert_eq!(work.label, "work");
        assert!(
            work.quarantined && work.recovering,
            "a healing account is quarantined AND recovering"
        );
        assert!(
            !snapshot.accounts[1].recovering,
            "the healthy spare is not recovering"
        );

        // The wire carries the derived flag but never a secret.
        let json = serde_json::to_string(&status_response(&snapshot)).unwrap();
        assert!(json.contains(r#""recovering":true"#), "got {json}");
        assert!(
            crate::redaction::meter::unauthored_emails(&json, &[]).is_empty(),
            "no non-authored email on the wire (#15/#444): {json}"
        );
        assert!(!json.to_lowercase().contains("token"));
    }

    #[tokio::test]
    async fn a_dead_spare_is_never_polled_so_it_cannot_spuriously_recover() {
        // The recovery precondition, enforced structurally: a quarantined NON-active
        // account is skipped in polling, so it accrues no recovery successes and can
        // never un-quarantine on its own. It can only recover by first becoming active
        // — which happens only via the operator's re-login (the #13 re-stash, covered
        // by the next test). Without that, even an account whose token WOULD poll live
        // stays dead across ticks.
        let poller = FakeRosterPoller::new()
            .ok("u-A", 0.10, 0.10) // active, holds
            .ok("u-B", 0.10, 0.10); // WOULD be live — but the dead spare is never polled
        let mut daemon = lifecycle_daemon_with(poller, tunables(95, 80, 0)).await;
        daemon.state.health[1].quarantined = true; // `spare` died in a prior episode

        for _ in 0..3 {
            let outcome = daemon.tick().await;
            assert!(
                !outcome
                    .events
                    .iter()
                    .any(|e| matches!(e, Event::CredentialRestored { .. })),
                "a never-polled spare must not recover: {:?}",
                outcome.events
            );
        }

        assert!(
            daemon.state.health[1].quarantined,
            "still dead — never polled"
        );
        assert_eq!(daemon.state.health[1].recovery_successes, 0);
        assert_eq!(daemon.state.health[1].consec_401, 0);
    }

    #[tokio::test]
    async fn a_relogin_un_quarantines_a_dead_account_immediately_on_restash() {
        // Issue #107 (AC #1, #2, #4): the full re-login recovery path end-to-end,
        // exercising the #13↔#42 seam. A dead account (quarantined, already
        // emergency-swapped away so the spare is active) is re-logged-in by the
        // operator. The #13 canonical-change re-stash now un-quarantines it ON THE SPOT
        // — `status` stops lying on the NEXT tick, with NO M-poll delay — emitting
        // exactly one `credential_restored` on the dead→alive edge. Distinct from the
        // spontaneous-revival path
        // (`m_consecutive_live_polls_recover_a_quarantined_account_and_signal_once`),
        // which still needs M live polls because no re-login event marks the token fresh.
        let roster = vec![account("u-A", "work"), account("u-B", "spare")];
        let store = store_holding(b"B-token").await; // `spare` is active post-emergency-swap
        let stash = stash_with(&[
            ("Sessiometer/u-A", b"A-token", "u-A"), // the OLD dead token
            ("Sessiometer/u-B", b"B-token", "u-B"),
        ])
        .await;
        let (_dir, json) = claude_json("u-B");
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
            json.clone(),
            &tun,
        );
        // The post-emergency-swap state: `work` is dead and parked off the active slot.
        daemon.state.active = Some(1);
        daemon.state.health[0].quarantined = true;

        // Tick 1 primes the canonical watch on `spare`; the dead `work` is skipped and
        // stays dead — no re-login has happened yet.
        let first = daemon.tick().await;
        assert!(!first
            .events
            .iter()
            .any(|e| matches!(e, Event::ReStash { .. } | Event::CredentialRestored { .. })));
        assert!(daemon.state.health[0].quarantined);

        // The operator `claude /login`s back into `work`: the canonical becomes its
        // fresh token and the display switches to it.
        daemon.store.write(&cred(b"A-reauthed")).await.unwrap();
        crate::claude_state::write_oauth_account(&json, &oauth("u-A")).unwrap();

        // Tick 2 detects the change, re-stashes `work`, re-resolves it active, AND
        // un-quarantines it immediately — no M-poll wait (#107). The same-tick poll that
        // runs after the re-stash sees an already-healthy account, so it does NOT emit a
        // second restore (edge-triggered, exactly once).
        let second = daemon.tick().await;
        assert!(
            second
                .events
                .iter()
                .any(|e| matches!(e, Event::ReStash { account } if account == "work")),
            "the re-login re-stashes work: {:?}",
            second.events
        );
        assert!(
            !daemon.state.health[0].quarantined,
            "the re-login un-quarantines work on the spot — no M-poll delay (#107)"
        );
        assert_eq!(
            daemon.state.health[0].recovery_successes, 0,
            "an immediate restore leaves no recovery probe pending"
        );
        assert_eq!(
            daemon.state.active,
            Some(0),
            "the re-logged-in account is active again"
        );
        assert_eq!(
            second
                .events
                .iter()
                .filter(|e| matches!(e, Event::CredentialRestored { account } if account == "work"))
                .count(),
            1,
            "exactly one credential_restored on the un-quarantine edge: {:?}",
            second.events
        );

        // Tick 3: `work` is healthy and active; no canonical change and no quarantine →
        // no further restore (the edge does not re-fire on an already-alive account).
        let third = daemon.tick().await;
        assert!(!daemon.state.health[0].quarantined);
        assert!(
            !third
                .events
                .iter()
                .any(|e| matches!(e, Event::CredentialRestored { .. })),
            "no repeat restore on an already-healthy account: {:?}",
            third.events
        );
    }

    #[tokio::test]
    async fn the_dead_and_restored_edges_re_arm_across_episodes() {
        // Edge-trigger re-arm (AC #5): a full dead→restored→dead cycle emits
        // credential_dead on EACH death edge and credential_restored on the recovery
        // edge — never stuck, never doubled. Proves the signals are per-transition,
        // not one-shot-per-process.
        let mut daemon = lifecycle_daemon().await;
        let mut events = Vec::new();

        // Episode 1 — death: 3 consecutive 401s.
        for _ in 0..3 {
            daemon.note_poll_outcome(0, &Err(Error::UsageUnauthorized), &mut events);
        }
        // Recovery: 2 consecutive live polls.
        daemon.note_poll_outcome(0, &live(0.10, 0.10), &mut events);
        daemon.note_poll_outcome(0, &live(0.10, 0.10), &mut events);
        // Episode 2 — death again: the streak re-armed, so 3 fresh 401s re-quarantine.
        for _ in 0..3 {
            daemon.note_poll_outcome(0, &Err(Error::UsageUnauthorized), &mut events);
        }

        let deaths = events
            .iter()
            .filter(|e| matches!(e, Event::CredentialDead { .. }))
            .count();
        let restores = events
            .iter()
            .filter(|e| matches!(e, Event::CredentialRestored { .. }))
            .count();
        assert_eq!(deaths, 2, "one credential_dead per death edge: {events:?}");
        assert_eq!(
            restores, 1,
            "one credential_restored per recovery edge: {events:?}"
        );
        assert!(daemon.state.health[0].quarantined, "ends dead in episode 2");
    }
}
