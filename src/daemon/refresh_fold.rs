// Copyright (c) 2026 Oleksii PELYKH
// SPDX-License-Identifier: MIT

//! Poll-refresh and recovery-outcome folding for the [`Daemon`] decision core (issue #637
//! step 4, issue #659, split out of the single `impl Daemon` block).
//!
//! The fold from a raw outcome to carried per-account health: a poll result (#9), a
//! same-tick refresh retry (#162), a periodic sweep observation (#102), the systemic-failure
//! detector, and the #42 dead-credential lifecycle with its quarantine / restore /
//! re-probe recovery arms (including the #643 revived-account stale-`Dead`-latch re-probe).

use super::*;

impl<P, C, S, K> super::Daemon<P, C, S, K>
where
    P: RosterPoller,
    C: CredentialStore,
    S: AccountStash,
    K: Clock,
{
    /// Fold account `i`'s poll `result` into its per-account health (issue #42) and
    /// push any poll-outcome event. Classifies the result into a [`PollOutcome`]:
    ///
    /// - **Unauthorized** (401): increment the consecutive-401 streak and reset any
    ///   recovery probe. While the account is still healthy, emit `monitor_401` with
    ///   the climbing count; the Nth consecutive (`monitor_401_n`) QUARANTINES it (a
    ///   dead credential) and emits [`Event::CredentialDead`] — edge-triggered, ONCE
    ///   on the transition. Once quarantined, further 401s are silent (no spam): the
    ///   dead state is a durable status, not a repeated log line.
    /// - **Live** (success): reset the streak. If the account is quarantined, this is
    ///   a recovery probe — count consecutive successes and, at `monitor_recovery_m`,
    ///   un-quarantine it and emit [`Event::CredentialRestored`] (edge-triggered,
    ///   ONCE). This M-poll path is now the SPONTANEOUS-REVIVAL case only: a re-login
    ///   un-quarantines immediately in
    ///   [`reconcile_canonical_change`](Self::reconcile_canonical_change) (issue #107),
    ///   so the account reaching here is a dead ACTIVE one with no viable swap target
    ///   (it stays active and is re-probed) whose OWN token starts answering again
    ///   WITHOUT a re-login. That is intended — a token returning success M times in a
    ///   row is a working credential, and leaving such an account stranded in
    ///   `needs re-login` would make the durable status lie.
    /// - **ScopeMissing** (403): reset the streak — a 403 token authenticates, so it
    ///   is NOT dead — and emit `usage_scope_fail` (#5). Resets any recovery probe.
    /// - **Transient** (5xx / network / 429 / other 4xx / locked / unreadable): reset
    ///   the streak silently — no liveness signal either way — and reset any recovery
    ///   probe (only a `Live` poll advances recovery). A locked keychain is
    ///   process-global and signaled once at top-of-tick (#13), never here.
    pub(super) fn note_poll_outcome(
        &mut self,
        i: usize,
        result: &Result<Usage>,
        events: &mut Vec<Event>,
    ) {
        match classify_poll(result) {
            PollOutcome::Unauthorized => {
                let consecutive = self.state.accounts[i].health.consec_401.saturating_add(1);
                self.state.accounts[i].health.consec_401 = consecutive;
                // A 401 breaks any in-progress recovery probe.
                self.state.accounts[i].health.recovery_successes = 0;
                // Already dead → stay silent: the durable status carries the dead
                // state; CredentialDead already fired on the transition (no spam).
                if self.state.accounts[i].health.quarantined {
                    return;
                }
                events.push(Event::Monitor401 {
                    account: self.roster[i].label.clone(),
                    consecutive,
                });
                // The Nth consecutive non-scope 401 declares the credential DEAD.
                if consecutive >= u32::from(self.monitor_401_n) {
                    self.state.accounts[i].health.quarantined = true;
                    // Open a fresh unrecoverable-death episode (issue #261): this account
                    // may later be confirmed unrecoverable by a dead sweep-refresh, and the
                    // #261 latch must be armed for THIS quarantine, having been left set by
                    // any prior episode. Reset here, the single quarantine-SET site.
                    self.state.accounts[i].health.unrecoverable_signaled = false;
                    events.push(Event::CredentialDead {
                        account: self.roster[i].label.clone(),
                    });
                }
            }
            PollOutcome::Live => {
                self.state.accounts[i].health.consec_401 = 0;
                if self.state.accounts[i].health.quarantined {
                    let m = self.state.accounts[i]
                        .health
                        .recovery_successes
                        .saturating_add(1);
                    self.state.accounts[i].health.recovery_successes = m;
                    if m >= u32::from(self.monitor_recovery_m) {
                        self.state.accounts[i].health.quarantined = false;
                        self.state.accounts[i].health.recovery_successes = 0;
                        events.push(Event::CredentialRestored {
                            account: self.roster[i].label.clone(),
                        });
                    }
                }
            }
            PollOutcome::ScopeMissing => {
                self.state.accounts[i].health.consec_401 = 0;
                self.state.accounts[i].health.recovery_successes = 0;
                events.push(Event::UsageScopeFail {
                    account: self.roster[i].label.clone(),
                });
            }
            PollOutcome::Transient => {
                self.state.accounts[i].health.consec_401 = 0;
                self.state.accounts[i].health.recovery_successes = 0;
            }
        }
    }

    /// Whether poll `i`'s outcome warrants a #162 refresh-then-retry, evaluated on the
    /// PRE-fold state (before [`note_poll_outcome`](Self::note_poll_outcome) advances the
    /// streak):
    ///
    /// - a refresh seam is wired ([`with_refresh_engine`](Self::with_refresh_engine)),
    /// - the poll was a 401 ([`PollOutcome::Unauthorized`]),
    /// - the account is not already quarantined (a dead account is left to the #106 sweep /
    ///   an operator re-login — never re-refreshed on every re-probe poll),
    /// - this is the FIRST 401 of the current streak episode (`consec_401 == 0`), and
    /// - the account is NOT the ACTIVE one (`state.active != Some(i)`, issue #253).
    ///
    /// The `consec_401 == 0` condition is the once-per-episode guard (AC-4, no refresh storm): a
    /// refresh spawns `claude -p` under the swap lock (seconds), so a persistently-401 account
    /// must refresh at most once per streak — the first 401 attempts the revive; the rest of the
    /// episode advances the streak directly.
    ///
    /// The active-account exclusion (issue #253) upholds the #102 engine's Caller contract
    /// ("refresh PARKED accounts only", `refresh.rs`): the isolated refresh performs a real OAuth
    /// exchange that ROTATES the refresh token server-side, but CAS-writes the fresh token only to
    /// the account's STASH — the canonical keychain item every live session reads keeps the old,
    /// now-invalidated token. Refreshing the active account would therefore break concurrent live
    /// sessions AND mask the account healthy (its stash re-poll succeeds), stranding the fresh
    /// token where no recovery path promotes it. `state.active` is resolved token-first at
    /// top-of-tick (#207) — the same authoritative signal [`refresh_exclusions`](Self::refresh_exclusions)
    /// (#105) and #250/`poke` exclude on. A still-active account's 401 instead advances the #42
    /// streak toward an operator re-login, exactly how a dead active account is already handled.
    pub(super) fn should_refresh_retry(&self, i: usize, result: &Result<Usage>) -> bool {
        self.poll_refresh.is_some()
            && matches!(classify_poll(result), PollOutcome::Unauthorized)
            && !self.state.accounts[i].health.quarantined
            && self.state.accounts[i].health.consec_401 == 0
            && self.state.active != Some(i)
    }

    /// Attempt one isolated refresh of account `i` (the #102 engine) and a single re-poll,
    /// returning the outcome [`note_poll_outcome`](Self::note_poll_outcome) then folds into
    /// the streak (issue #162). Only called when [`should_refresh_retry`](Self::should_refresh_retry)
    /// holds, so `poll_refresh` is `Some` and — per that guard's active-account exclusion
    /// (issue #253) — `i` is always a PARKED account, never the active one.
    ///
    /// - Refresh reports **`Dead`** (the refresh token was cleared in place, `refresh.rs`) →
    ///   a genuine death: skip the re-poll and let the 401 stand so the streak advances.
    /// - Refresh ran otherwise (refreshed / no-change / even an engine error report) → the
    ///   account's STASH may now bear a fresh token, so re-poll THROUGH THE STASH
    ///   (`active = false`). The re-poll is a liveness probe against the parked account's stash
    ///   that never touches the live canonical credential. (This path deliberately does NOT run
    ///   for the active account: `should_refresh_retry` excludes it, because the harm is the
    ///   `engine.refresh` server-side token rotation one step EARLIER — which the re-poll cannot
    ///   undo — not the re-poll itself, issue #253.)
    /// - The refresh itself **errors** → "could not revive"; fail-safe by letting the 401
    ///   stand. A refresh failure never crashes the poll loop.
    ///
    /// Every firing pushes ONE durable [`Event::PollRefresh`] onto `events` (issue #255): the
    /// isolated-refresh ACTION the durable log previously lacked — until now only the DOWNSTREAM
    /// poll outcome reached it (via [`note_poll_outcome`](Self::note_poll_outcome)), so the log
    /// showed a `CredentialDead` edge but not the poll-refresh that preceded it.
    pub(super) async fn refresh_retry(&self, i: usize, events: &mut Vec<Event>) -> Result<Usage> {
        let refreshed = match self.poll_refresh.as_ref() {
            Some(engine) => engine.refresh(&self.roster[i]).await,
            // Unreachable given the `should_refresh_retry` guard; treat as could-not-revive.
            None => return Err(Error::UsageUnauthorized),
        };
        // Durably record the isolated poll-refresh ACTION (issue #255): the fact it fired, its
        // target PARKED account (redacted handle), and the classified outcome — the forensic
        // trail the transient `diag=` line alone did not leave. Emitted for EVERY firing, BEFORE
        // the Dead / re-poll split below, so a genuine-death `Dead` is evented as surely as a
        // revive. A completed cycle maps through the shared `refresh_event_outcome` (the same
        // vocabulary the periodic #106 `event=refresh` uses); an engine that could not even run
        // (`Err`) is an `Error` outcome — mirroring `refresh_tick`'s `error_refresh_event`.
        events.push(Event::PollRefresh {
            account: self.roster[i].label.clone(),
            outcome: match &refreshed {
                Ok(report) => refresh_event_outcome(report),
                Err(_) => RefreshEventOutcome::Error,
            },
            // The AC-3 rotation flag on the poll path (issue #279): the completed cycle's
            // own signal; an engine that could not even run (`Err`) renders `false`.
            refresh_token_rotated: match &refreshed {
                Ok(report) => report.refresh_token_rotated,
                Err(_) => false,
            },
        });
        match refreshed {
            // The refresh token was cleared in place → genuinely dead: let the 401 stand.
            Ok(report) if report.outcome == RefreshOutcome::Dead => Err(Error::UsageUnauthorized),
            // A fresh token may now be stashed → probe liveness through the stash.
            Ok(_) => self
                .poller
                .poll(&self.roster[i], false)
                .await
                .map(|reading| reading.usage),
            // Could not revive (spawn / read-back failure) → fail-safe: the 401 stands.
            Err(_) => Err(Error::UsageUnauthorized),
        }
    }

    /// The roster `account_uuid`s the periodic refresh tick (issue #105) must NOT refresh —
    /// the inputs to the engine's "parked accounts only" Caller contract, computed daemon-side
    /// from the authoritative swap state (the tick has none of its own):
    ///
    ///   - the **active** account (the live session's credential — never touch it), and
    ///   - the **imminent swap target** (the live selection's current choice, the same account
    ///     `next_swap` shows): a swap that promotes it reads its stash WITHOUT rewriting it
    ///     (#6), so the engine's CAS re-stash cannot observe the promotion (#102) — exclude it
    ///     ahead of time. The mid-swap window itself is covered by the swap lock the engine
    ///     holds; this excludes only the *predictable* targets.
    ///
    /// Quarantined (dead) accounts are NO LONGER excluded (issue #106 reverses #105's
    /// "futile to refresh" exclusion): a dead credential may still be REFRESHABLE — its
    /// refresh token can work even after its access token began failing — so refreshing it is
    /// exactly the RESTORE path. They are supplied separately by [`refresh_quarantined`](Self::refresh_quarantined),
    /// which the tick uses to bypass the near-expiry filter and to report a recovered account
    /// for un-quarantine. The bounded cost — one wasted isolated-spawn per cadence for a
    /// TRULY-dead account, until an operator re-login — is accepted to close the gap where a
    /// less-recently-used parked account silently stays unusable.
    ///
    /// Returns owned uuids so the run loop can hand them to the tick without borrowing the
    /// daemon across the idle wait.
    pub(super) fn refresh_exclusions(&self) -> Vec<String> {
        let mut excluded = Vec::new();
        if let Some(active) = self.state.active {
            excluded.push(self.roster[active].account_uuid.clone());
            // The imminent swap target from the latest carried readings — the same selection
            // `next_swap` surfaces (enhanced #612 axes, same per-daemon seed). A BEST-EFFORT
            // prediction, never a guarantee: this runs on POST-tick state, so any input that moves
            // before the next tick's `decide` — a changed reading or viable set, and since #612 a
            // reset-tied pair whose velocity EMAs cross — can promote a peer this did not exclude.
            // That residual is what the swap lock covers (#64/#102, per this fn's docs: "the mid-swap
            // window itself is covered by the swap lock"); excluding the predictable target only
            // narrows the exposure. `pick_target_ranked` already excludes the active account.
            let readings = self.decision_readings(Some(active));
            let enabled = self.enabled_mask();
            if let Some(target) = pick_target_ranked(
                active,
                &readings,
                &enabled,
                Some(self.target_max_session_usage),
                self.session_ceiling_base,
                // Issue #607: the rotation line, matching the swap paths this exclusion shadows.
                self.weekly_rotation_line(),
                self.selection_tiebreak(),
            ) {
                excluded.push(self.roster[target].account_uuid.clone());
            }
        }
        excluded
    }

    /// The roster `account_uuid`s the daemon currently holds QUARANTINED ("needs re-login",
    /// issue #42) — handed to the refresh tick so it can attempt the RESTORE path (#106):
    /// refresh them even when not near expiry (a server-revoked token may sit far from its
    /// stored timestamp expiry) and report a successful one for un-quarantine. An account
    /// here that is ALSO in [`refresh_exclusions`](Self::refresh_exclusions) (a dead ACTIVE
    /// account) is still skipped — the tick checks exclusion first (the engine Caller contract
    /// wins), so the active credential is never touched.
    ///
    /// Owned uuids, like [`refresh_exclusions`](Self::refresh_exclusions), so the run loop
    /// need not borrow the daemon across the idle wait.
    pub(super) fn refresh_quarantined(&self) -> Vec<String> {
        self.roster
            .iter()
            .enumerate()
            .filter(|(i, _)| self.state.accounts[*i].health.quarantined)
            .map(|(_, account)| account.account_uuid.clone())
            .collect()
    }

    /// Apply one RESTORE the refresh tick reported (issue #106): un-quarantine the account
    /// with `uuid` whose isolated refresh succeeded, returning the edge-triggered
    /// [`Event::CredentialRestored`] for the run loop to log — or `None` if the account is
    /// no longer quarantined (a concurrent re-login already restored it, #107) or the uuid is
    /// unknown. Pairs the health flip with its event in the daemon, exactly as the #42 poll
    /// and #107 re-login recovery paths do; the tick only signals which accounts recovered.
    pub(super) fn apply_refresh_restore(&mut self, uuid: &str) -> Option<Event> {
        let idx = self.roster.iter().position(|a| a.account_uuid == uuid)?;
        if !self.state.accounts[idx].health.quarantined {
            return None;
        }
        self.state.accounts[idx].health.quarantined = false;
        self.state.accounts[idx].health.recovery_successes = 0;
        Some(Event::CredentialRestored {
            account: self.roster[idx].label.clone(),
        })
    }

    /// Handle a `restored` control notify (issue #276) — the on-demand recovery of a revived
    /// account — with the issue #643 credential re-probe. When the revived account's health
    /// verdict is the terminal 🔴 `Dead` (its [`last_refresh_outcome`](AccountHealth::last_refresh_outcome)
    /// latched `Dead`), drive an IMMEDIATE isolated refresh (the always-wired #162/#426
    /// `poll_refresh` engine) and fold a genuinely SUCCESSFUL refresh into the verdict, so a fixed
    /// credential returns to 🟢 within a cycle instead of latching 🔴 for a full ~8h access-token
    /// lifetime until the next natural near-expiry sweep. Recovery from `Dead` stays gated on a real
    /// successful refresh, NEVER a usage-poll 200 (the false-recovery guard issue #427 established: a
    /// usage 200 exercises the ACCESS token and says nothing about the REFRESH token, which is
    /// exactly what `Dead` asserts) — a still-dead re-stash whose fresh credential ALSO fails to
    /// refresh keeps the honest 🔴.
    ///
    /// When the account is NOT `Dead` (a bare `Degraded` quarantine, whose access-token 401-streak a
    /// re-login clears WITHOUT needing a refresh), the isolated engine is unwired (a hermetic
    /// daemon), or the account is unexpectedly the ACTIVE one (the isolated engine must never touch
    /// it, issue #253 — the active path re-probes via keep-warm instead), it falls back to the plain
    /// #275 on-demand un-quarantine ([`apply_refresh_restore`](Self::apply_refresh_restore)). Returns
    /// the events to log; the edge-triggered `CredentialHealth` transition rides the run loop's
    /// immediate re-tick.
    pub(super) async fn reconcile_restored(&mut self, uuid: &str) -> Vec<Event> {
        let Some(idx) = self.roster.iter().position(|a| a.account_uuid == uuid) else {
            // Unknown uuid: an idempotent no-op (the daemon no longer holds it), mirroring
            // `apply_refresh_restore`.
            return Vec::new();
        };
        // `credential_health` reserves the terminal 🔴 `Dead` for `last_refresh_outcome == Dead`
        // (issue #427) — the ONLY verdict a fresh refresh is needed to clear; a bare `Degraded`
        // quarantine clears on the un-quarantine alone. The active-account exclusion mirrors
        // `should_refresh_retry` (issue #253): the isolated engine rotates the server-side refresh
        // token but writes only the STASH, so it is safe for PARKED accounts only.
        let is_dead =
            self.state.accounts[idx].health.last_refresh_outcome == Some(RefreshEventOutcome::Dead);
        if is_dead && self.poll_refresh.is_some() && self.state.active != Some(idx) {
            self.reprobe_dead_parked_credential(idx).await
        } else {
            // #275: the bare on-demand un-quarantine — a `Degraded` revive, or an unwired engine.
            self.apply_refresh_restore(uuid).into_iter().collect()
        }
    }

    /// Re-probe a revived PARKED account's credential with ONE isolated refresh and fold a
    /// genuinely SUCCESSFUL result into its health (issue #643). Only called from
    /// [`reconcile_restored`](Self::reconcile_restored) when the account is `Dead`, `poll_refresh`
    /// is wired, AND it is not the active one — so the engine `as_ref()` is always `Some`. The
    /// isolated #102 engine rotates the server-side refresh token but CAS-writes only the account's
    /// STASH, never the live canonical — parked-account-safe (issue #253), the SAME engine the
    /// reactive #162 poll path drives. Emits one durable [`Event::PollRefresh`] for the ACTION
    /// (mirroring [`refresh_retry`](Self::refresh_retry)), then folds a live outcome via
    /// [`fold_recovery_outcome`](Self::fold_recovery_outcome); a `Dead` re-stash (still dead) or a
    /// transient engine error leaves the honest 🔴 standing.
    pub(super) async fn reprobe_dead_parked_credential(&mut self, idx: usize) -> Vec<Event> {
        let refreshed = match self.poll_refresh.as_ref() {
            Some(engine) => engine.refresh(&self.roster[idx]).await,
            // Unreachable given the caller's `poll_refresh.is_some()` gate; treat as could-not-run.
            None => return Vec::new(),
        };
        let (outcome, rotated) = match &refreshed {
            Ok(report) => (refresh_event_outcome(report), report.refresh_token_rotated),
            Err(_) => (RefreshEventOutcome::Error, false),
        };
        // Durably record the isolated-refresh ACTION (issue #255 vocabulary), for EVERY firing —
        // the forensic trail the transient `diag=` line alone did not leave, exactly as the #162
        // poll-path refresh does.
        let mut events = vec![Event::PollRefresh {
            account: self.roster[idx].label.clone(),
            outcome,
            refresh_token_rotated: rotated,
        }];
        // Read the freshly re-stashed access-token expiry (the sweep reads it the same way) so the
        // rollup's staleness clock tracks the revived credential. `fold_recovery_outcome` applies it
        // whenever the stash yielded one (`Some`); on a non-live outcome the verdict is `Dead`/`AtRisk`
        // regardless of the expiry, so the update is inert there.
        let expires_at_ms =
            crate::refresh::stored_expires_at(&self.stash, &self.roster[idx].stash()).await;
        events.extend(self.fold_recovery_outcome(idx, outcome, rotated, expires_at_ms));
        events
    }

    /// Fold a MANUAL-RECOVERY refresh `outcome` for account `idx` into its health (issue #643) — the
    /// shared core of the parked ([`reconcile_restored`](Self::reconcile_restored)) and active
    /// (`use`-activate) recovery re-probes. Folds the refresh-health half via
    /// [`note_refresh_outcome`](Self::note_refresh_outcome) (the SAME primitive the #119 sweep uses,
    /// so `last_refresh_outcome` / the #261 latch never drift), then decides quarantine membership by
    /// the outcome's DEFINITIVENESS — recovery from the terminal 🔴 `Dead` stays gated on a genuinely
    /// successful refresh (the issue #427 false-recovery guard: a usage-poll 200 exercises only the
    /// ACCESS token and never reaches this refresh path):
    ///
    /// - a LIVE outcome (`Refreshed` / `RefreshedNotReStashed` / `NoChange` — the refresh token
    ///   actually answered) → un-quarantine → 🟢 (the fix: a fixed credential clears within a cycle);
    /// - a definitive `Dead` (CC rejected the refresh token itself) → KEEP the quarantine, so a
    ///   confirmed-dead credential stays out of rotation and honestly 🔴 (the AC-3 regression guard);
    /// - a transient `Error` (a spawn / read-back / lock hiccup — INCONCLUSIVE, not a dead verdict)
    ///   → un-quarantine (preserving the #275 guarantee that a re-login clears the quarantine) → 🟡
    ///   `AtRisk`, which self-heals on the next sweep rather than stranding a genuinely-fixed account.
    ///
    /// `expires_at_ms` refreshes the staleness clock ONLY when `Some` (the parked path passes the
    /// fresh stash expiry; the active path passes `None`, since
    /// [`promote_canonical`](Self::promote_canonical) already reconciled `access_expires_at` to the
    /// promoted canonical — issue #477 — and clobbering it to `None` would false-fire `Stale`). The
    /// `CredentialHealth` rollup transition rides the caller's next tick.
    pub(super) fn fold_recovery_outcome(
        &mut self,
        idx: usize,
        outcome: RefreshEventOutcome,
        rotated: bool,
        expires_at_ms: Option<i64>,
    ) -> Vec<Event> {
        let mut events = Vec::new();
        // The refresh-health fold (`last_refresh_outcome` + rotated + the at-risk streak + the #261
        // latch), shared with the sweep. Any `CredentialUnrecoverable` edge it returns is LOGGED
        // (via the caller's `emit_best_effort`) but never NOTIFIED — a login-triggered re-probe must
        // not spawn a "run claude /login" macOS toast the instant the operator just DID.
        if let Some(event) = self.note_refresh_outcome(idx, outcome, rotated) {
            events.push(event);
        }
        // Staleness clock: parked passes the fresh stash expiry; active passes `None` (promote owns
        // it). ms → s at the boundary, matching `apply_refresh_observation`.
        if let Some(ms) = expires_at_ms {
            self.state.accounts[idx].health.access_expires_at = Some(ms / 1000);
        }
        // Quarantine membership by definitiveness (see the doc ladder): a confirmed-`Dead` credential
        // STAYS quarantined (out of rotation, honest 🔴); every other outcome un-quarantines — a live
        // one to 🟢, an inconclusive `Error` to 🟡 (the #275 un-quarantine guarantee).
        if outcome != RefreshEventOutcome::Dead {
            let uuid = self.roster[idx].account_uuid.clone();
            if let Some(event) = self.apply_refresh_restore(&uuid) {
                events.push(event);
            }
        }
        events
    }

    /// Re-probe the ACTIVE account's credential with ONE forced keep-warm mint when a `use`-activation
    /// (a manual `use` swap or a menubar swap-on-click) lands on an account carrying the terminal 🔴
    /// `Dead` verdict (issue #643) — the active-account counterpart of
    /// [`reprobe_dead_parked_credential`](Self::reprobe_dead_parked_credential). Driven from the run
    /// loop right after a swap adopts the new active ([`adopt_manual_swap`](Self::adopt_manual_swap) /
    /// [`perform_socket_swap`](Self::perform_socket_swap) have already set `state.active`), so the
    /// newly-active account is re-evaluated WITHIN the cycle rather than latching a stale `Dead` for a
    /// full ~8h access-token lifetime — the sweep that would otherwise clear it EXCLUDES the active
    /// account (issue #253), so nothing else re-probes it.
    ///
    /// Gated to the exact fix precondition: the active account's verdict is `Dead`
    /// (`last_refresh_outcome == Some(Dead)`) AND the active-safe keep-warm engine is wired — the ONE
    /// refresh path that may touch the LIVE canonical (issue #253; the isolated poll-refresh engine
    /// must never write it, which is why the parked path uses `poll_refresh` and this one uses
    /// `keep_warm`). A non-`Dead` active account (nothing to recover) or an unwired engine returns
    /// empty. The mint reads the POST-swap canonical (`self.store.read()` — the now-active account's
    /// blob the swap just wrote); an unreadable / locked keychain fails safe to empty (the next tick
    /// re-resolves active anyway — locked ≠ dead).
    ///
    /// Recovery stays gated on a genuinely SUCCESSFUL refresh (the issue #427 false-recovery guard):
    /// [`keep_warm_and_promote`](Self::keep_warm_and_promote) mints + promotes, surfacing the cycle's
    /// classified [`RefreshEventOutcome`], which [`fold_recovery_outcome`](Self::fold_recovery_outcome)
    /// folds — a live outcome clears the `Dead` latch to 🟢; a `Dead` re-mint (CC rejected the refresh
    /// token, or it is absent) keeps the honest 🔴. `expires_at_ms` is `None`: a real mint's
    /// [`promote_canonical`](Self::promote_canonical) already reconciled `access_expires_at` (issue
    /// #477), so the active path must not clobber it to `None` and false-fire `Stale`.
    pub(super) async fn reprobe_active_if_dead(&mut self) -> Vec<Event> {
        let Some(idx) = self.state.active else {
            return Vec::new();
        };
        // Only the terminal 🔴 `Dead` verdict needs a forced refresh; a healthy / degraded active
        // account is left to the normal tick. `keep_warm` is the ONLY active-safe refresh (issue
        // #253): unwired → nothing to do.
        let is_dead =
            self.state.accounts[idx].health.last_refresh_outcome == Some(RefreshEventOutcome::Dead);
        if !is_dead || self.keep_warm.is_none() {
            return Vec::new();
        }
        // Mint from the POST-swap canonical — the now-active account's credential the swap just
        // wrote. A locked / unreadable keychain fails safe (the next tick re-resolves active anyway).
        let Ok(canonical) = self.store.read().await else {
            return Vec::new();
        };
        let mut events = Vec::new();
        // `.promoted` is deliberately unused here (the `..`): a promote either landed a fresh token
        // for the fold's live outcome to clear, or it didn't and the fold keeps the honest 🔴 — the
        // recovery verdict keys on `outcome`, not on whether the canonical was rewritten.
        let KeepWarmPromote {
            outcome,
            token_rotated,
            ..
        } = self
            .keep_warm_and_promote(idx, &canonical, KeepWarmTrigger::Recovery, &mut events)
            .await;
        // `None` expiry: a real mint's `promote_canonical` already reconciled `access_expires_at`
        // (issue #477); the active path must not clobber it to `None` and false-fire `Stale`.
        events.extend(self.fold_recovery_outcome(idx, outcome, token_rotated, None));
        events
    }

    /// Fold one [`RefreshObservation`] the refresh sweep reported (issue #119) into the
    /// owning account's carried health state — the credential clocks the `status` rollup
    /// projects. The engine's `expiresAt` is MS (CC's native unit); it is converted to the
    /// epoch SECONDS the rollup and wire use HERE, at the fold boundary. A `None` uuid (an
    /// account the daemon no longer holds) is ignored, mirroring [`Self::apply_refresh_restore`].
    ///
    /// The expiry clock updates on EVERY observation (refreshed or read-only). The
    /// refresh-health fields update only when the sweep actually refreshed the account
    /// (`observation.refresh` is `Some`): a `Dead` / `Error` outcome advances the
    /// consecutive-failure streak; any alive outcome resets it — so the streak the rollup's
    /// `AtRisk` keys off counts only CONSECUTIVE failures.
    ///
    /// Returns [`Event::CredentialUnrecoverable`] on the ONE observation that first confirms a
    /// QUARANTINED account's refresh token is dead — the sweep's isolated refresh came back
    /// `Dead`, so no automated path revives it and only an operator `claude /login` can (issue
    /// #261). Gated by the sticky per-account [`AccountHealth::unrecoverable_signaled`] latch so
    /// the caller emits the operator signal exactly once per quarantine episode, never per sweep
    /// re-probe. Every other observation returns `None` (mirroring [`Self::apply_refresh_restore`]'s
    /// `Option<Event>` shape, so the caller emits uniformly).
    pub(super) fn apply_refresh_observation(
        &mut self,
        observation: &RefreshObservation,
    ) -> Option<Event> {
        let idx = self
            .roster
            .iter()
            .position(|a| a.account_uuid == observation.account_uuid)?;
        // ms → s at the boundary; the rollup/wire are uniform epoch seconds. The expiry clock
        // updates on EVERY observation (refreshed or read-only).
        self.state.accounts[idx].health.access_expires_at =
            observation.expires_at_ms.map(|ms| ms / 1000);
        // The refresh-health fields update only when the sweep actually refreshed the account.
        let delta = observation.refresh?;
        self.note_refresh_outcome(idx, delta.outcome, delta.token_rotated)
    }

    /// Fold ONE refresh cycle's non-secret `outcome` (+ whether the refresh token rotated) into
    /// account `idx`'s carried refresh-health — the shared core of the #119 sweep observation
    /// ([`apply_refresh_observation`](Self::apply_refresh_observation)) and the issue #643 manual-
    /// recovery re-probe ([`fold_recovery_outcome`](Self::fold_recovery_outcome)), so
    /// `last_refresh_outcome`, the at-risk streak, and the #261 latch stay single-homed and cannot
    /// drift across the two callers. Deliberately does NOT touch `access_expires_at` — that clock is
    /// owned by each caller (the sweep reads it from the stash; the active recovery leaves
    /// [`promote_canonical`](Self::promote_canonical)'s #477 reconciliation intact).
    ///
    /// A `Dead` / `Error` outcome advances the consecutive-failure streak; any alive outcome resets
    /// it — so the streak the rollup's `AtRisk` keys off counts only CONSECUTIVE failures. Returns
    /// [`Event::CredentialUnrecoverable`] on the ONE fold that first confirms a QUARANTINED account's
    /// refresh token is dead (issue #261): CC rejected the refresh token, so no automated path
    /// revives it and only an operator `claude /login` can. Gated by the sticky per-account
    /// [`AccountHealth::unrecoverable_signaled`] latch so the caller emits the operator signal
    /// exactly once per quarantine episode, never per re-probe. Every other fold returns `None`.
    pub(super) fn note_refresh_outcome(
        &mut self,
        idx: usize,
        outcome: RefreshEventOutcome,
        token_rotated: bool,
    ) -> Option<Event> {
        let health = &mut self.state.accounts[idx].health;
        health.last_refresh_outcome = Some(outcome);
        health.refresh_token_rotated = Some(token_rotated);
        match outcome {
            RefreshEventOutcome::Dead | RefreshEventOutcome::Error => {
                health.consecutive_refresh_failures =
                    health.consecutive_refresh_failures.saturating_add(1);
            }
            RefreshEventOutcome::Refreshed
            | RefreshEventOutcome::RefreshedNotReStashed
            | RefreshEventOutcome::NoChange => {
                health.consecutive_refresh_failures = 0;
            }
        }
        // Issue #261: a QUARANTINED account whose isolated refresh returns `Dead` is confirmed
        // unrecoverable. Fire the operator signal once per quarantine episode — the latch (reset on
        // re-quarantine in `note_poll_outcome`) suppresses the re-probe repeats and the `Dead`↔`Error`
        // flap. Keyed on the latch, deliberately NOT on the prior `last_refresh_outcome`, which is
        // orthogonal to the quarantine lifecycle.
        let signal = outcome == RefreshEventOutcome::Dead
            && health.quarantined
            && !health.unrecoverable_signaled;
        if signal {
            health.unrecoverable_signaled = true;
        }
        // The `&mut health` borrow ends at its last use above (NLL); read the label off `roster`.
        signal.then(|| Event::CredentialUnrecoverable {
            account: self.roster[idx].label.clone(),
        })
    }

    /// Fold one sweep's [`SweepHealth`] classification into the daemon-level systemic-refresh
    /// detector (issue #378), returning the edge-triggered [`Event`] to emit at an episode
    /// boundary — [`Event::RefreshSystemicFailure`] on the streak crossing
    /// [`systemic_failure_n`](Self::systemic_failure_n), [`Event::RefreshSystemicRecovered`] on
    /// recovery — or `None` on a neutral / mid-episode sweep.
    ///
    /// Driven from the run loop AFTER the idle borrow drops (like the #106 restores + #119
    /// observations), once PER SWEEP: the classification is captured per sweep in
    /// `idle_until_next_tick` so multiple sweeps in one idle period (a low cadence under a long
    /// poll interval) each advance the streak individually rather than merging into one. The
    /// per-account observation fold ([`apply_refresh_observation`](Self::apply_refresh_observation))
    /// updates the `at_risk` rollup independently — this is the orthogonal MECHANISM-level signal.
    pub(super) fn note_systemic_refresh(&mut self, health: SweepHealth) -> Option<Event> {
        self.state
            .systemic_refresh
            .note(health, self.systemic_failure_n)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::config::Tunables;

    use crate::daemon::tests::*;
    use crate::keychain::FakeCredentialStore;
    use crate::observability::{RefreshEventOutcome, Verbosity};
    use crate::stash::FakeAccountStash;

    use std::cell::{Cell, RefCell};
    use std::collections::HashMap;
    use std::rc::Rc;

    #[test]
    fn refresh_health_view_is_none_until_observed_then_reduces_the_outcome() {
        // No refresh observed yet (`[refresh]` off, or not yet swept) → None, so the wire
        // omits the field rather than fabricating a verdict.
        assert_eq!(refresh_health_view(&AccountHealth::default()), None);

        // An alive outcome reduces to `last_ok: true`, carrying the rotation flag (the AC-3
        // durability signal) and the failure streak.
        let alive = AccountHealth {
            last_refresh_outcome: Some(RefreshEventOutcome::NoChange),
            refresh_token_rotated: Some(true),
            consecutive_refresh_failures: 0,
            ..Default::default()
        };
        assert_eq!(
            refresh_health_view(&alive),
            Some(RefreshHealth {
                last_ok: true,
                rotated: true,
                consecutive_failures: 0,
            })
        );

        // A dead/error outcome reduces to `last_ok: false`, surfacing the failure streak the
        // rollup's at-risk input keys off.
        let failing = AccountHealth {
            last_refresh_outcome: Some(RefreshEventOutcome::Error),
            refresh_token_rotated: Some(false),
            consecutive_refresh_failures: 3,
            ..Default::default()
        };
        assert_eq!(
            refresh_health_view(&failing),
            Some(RefreshHealth {
                last_ok: false,
                rotated: false,
                consecutive_failures: 3,
            })
        );
    }

    #[tokio::test]
    async fn apply_refresh_observation_folds_ms_expiry_and_tracks_consecutive_failures() {
        let mut daemon = three_account_daemon(FakeRosterPoller::new()).await;
        // `RefreshDelta` lives in `contract` and is not imported at module scope (only
        // the daemon's fold consumes it); name it in full here.
        use crate::contract::RefreshDelta;
        let observe = |outcome, rotated, ms| RefreshObservation {
            account_uuid: "u-A".to_owned(),
            expires_at_ms: Some(ms),
            refresh: Some(RefreshDelta {
                outcome,
                token_rotated: rotated,
            }),
        };

        // A read-only observation (`refresh: None`) updates ONLY the expiry — folded from
        // the engine's milliseconds to the rollup's epoch seconds at this boundary.
        daemon.apply_refresh_observation(&RefreshObservation {
            account_uuid: "u-A".to_owned(),
            expires_at_ms: Some(1_782_777_600_000),
            refresh: None,
        });
        assert_eq!(
            daemon.state.accounts[0].health.access_expires_at,
            Some(1_782_777_600)
        );
        assert_eq!(daemon.state.accounts[0].health.last_refresh_outcome, None);
        assert_eq!(
            daemon.state.accounts[0].health.consecutive_refresh_failures,
            0
        );

        // Failing refreshes advance the consecutive-failure streak and record the outcome.
        daemon.apply_refresh_observation(&observe(
            RefreshEventOutcome::Error,
            false,
            1_782_777_600_000,
        ));
        assert_eq!(
            daemon.state.accounts[0].health.consecutive_refresh_failures,
            1
        );
        daemon.apply_refresh_observation(&observe(
            RefreshEventOutcome::Dead,
            false,
            1_782_777_600_000,
        ));
        assert_eq!(
            daemon.state.accounts[0].health.consecutive_refresh_failures,
            2
        );
        assert_eq!(
            daemon.state.accounts[0].health.last_refresh_outcome,
            Some(RefreshEventOutcome::Dead)
        );

        // Any alive refresh resets the streak to zero — so the at-risk input counts only
        // CONSECUTIVE failures — and slides the expiry forward.
        daemon.apply_refresh_observation(&observe(
            RefreshEventOutcome::Refreshed,
            true,
            1_782_784_800_000,
        ));
        assert_eq!(
            daemon.state.accounts[0].health.consecutive_refresh_failures,
            0
        );
        assert_eq!(
            daemon.state.accounts[0].health.refresh_token_rotated,
            Some(true)
        );
        assert_eq!(
            daemon.state.accounts[0].health.access_expires_at,
            Some(1_782_784_800)
        );

        // An observation for a uuid the daemon no longer holds is ignored (no panic, no
        // spurious mutation) — mirroring `apply_refresh_restore`; the siblings stay pristine.
        daemon.apply_refresh_observation(&RefreshObservation {
            account_uuid: "u-GONE".to_owned(),
            expires_at_ms: Some(0),
            refresh: None,
        });
        assert_eq!(daemon.state.accounts[1].health.access_expires_at, None);
    }

    #[tokio::test]
    async fn apply_refresh_restore_un_quarantines_once_and_is_a_noop_otherwise() {
        // Issue #275 (AC-1 + AC-3): the primitive the new `restored` control command drives.
        // A quarantined account is un-quarantined EXACTLY once — the first call flips
        // `quarantined` off, resets `recovery_successes`, and returns its edge-triggered
        // `CredentialRestored`; a second call is a silent `None` (already restored). An unknown
        // uuid and an already-non-quarantined account are both idempotent `None` no-ops. Throughout,
        // the ACTIVE account is untouched — the restore never re-points canonical or swaps active,
        // the guarantee that lets `login <B>` clear B's quarantine WITHOUT activating B.
        let mut daemon = three_account_daemon(FakeRosterPoller::new()).await;
        let active_before = daemon.state.active;

        // An already-non-quarantined account: no-op, no event.
        assert_eq!(daemon.apply_refresh_restore("u-B"), None);
        // An unknown uuid: no-op, no event (the daemon no longer holds it).
        assert_eq!(daemon.apply_refresh_restore("u-NOPE"), None);

        // Quarantine the PARKED, non-active `spare` (index 1) — the #106 parked-and-stuck case —
        // and seed a recovery streak the restore must clear.
        daemon.state.accounts[1].health.quarantined = true;
        daemon.state.accounts[1].health.recovery_successes = 2;

        // First restore: un-quarantines and emits exactly the edge event, named by handle only.
        assert_eq!(
            daemon.apply_refresh_restore("u-B"),
            Some(Event::CredentialRestored {
                account: "spare".to_owned(),
            })
        );
        assert!(
            !daemon.state.accounts[1].health.quarantined,
            "spare un-quarantined"
        );
        assert_eq!(
            daemon.state.accounts[1].health.recovery_successes, 0,
            "the recovery streak is reset on restore"
        );

        // Second restore of the now-eligible account: idempotent silent no-op.
        assert_eq!(daemon.apply_refresh_restore("u-B"), None);

        // The active account was never touched by any of the above (AC-1: active unchanged).
        assert_eq!(
            daemon.state.active, active_before,
            "an on-demand restore never changes the active account"
        );
    }

    #[tokio::test]
    async fn unrecoverable_signal_fires_once_and_only_when_quarantined() {
        // Issue #261: a QUARANTINED account whose isolated sweep-refresh returns `Dead` is
        // confirmed unrecoverable — `apply_refresh_observation` yields `credential_unrecoverable`
        // ONCE per quarantine episode, never per re-probe (AC2), and never for a non-quarantined
        // account (the AC's scope gate). The handle is the operator label only (AC3/#15).
        use crate::contract::RefreshDelta;
        let mut daemon = three_account_daemon(FakeRosterPoller::new()).await;
        let obs = |uuid: &str, outcome| RefreshObservation {
            account_uuid: uuid.to_owned(),
            expires_at_ms: Some(1_782_777_600_000),
            refresh: Some(RefreshDelta {
                outcome,
                token_rotated: false,
            }),
        };

        // A non-quarantined account's dead sweep-refresh does NOT notify — that is the #119
        // refresh-detected death the rollup surfaces, deliberately outside #261's console/macOS
        // operator-signal scope.
        assert_eq!(
            daemon.apply_refresh_observation(&obs("u-A", RefreshEventOutcome::Dead)),
            None
        );
        assert!(!daemon.state.accounts[0].health.unrecoverable_signaled);

        // Quarantine account 0 (the #42 verdict); the next dead sweep-refresh CONFIRMS it
        // unrecoverable → exactly one event, named by handle only.
        daemon.state.accounts[0].health.quarantined = true;
        assert_eq!(
            daemon.apply_refresh_observation(&obs("u-A", RefreshEventOutcome::Dead)),
            Some(Event::CredentialUnrecoverable {
                account: "work".to_owned(),
            })
        );
        assert!(daemon.state.accounts[0].health.unrecoverable_signaled);

        // Every subsequent re-probe of the still-dead token is SILENT — INCLUDING a
        // `Dead`→`Error`→`Dead` flap, which a naive last-outcome guard would double-fire on (a
        // transient sweep `Error` between dead probes must not re-arm the signal).
        assert_eq!(
            daemon.apply_refresh_observation(&obs("u-A", RefreshEventOutcome::Dead)),
            None
        );
        assert_eq!(
            daemon.apply_refresh_observation(&obs("u-A", RefreshEventOutcome::Error)),
            None
        );
        assert_eq!(
            daemon.apply_refresh_observation(&obs("u-A", RefreshEventOutcome::Dead)),
            None
        );
    }

    #[tokio::test]
    async fn unrecoverable_latch_resets_on_requarantine_so_the_signal_can_refire() {
        // Issue #261: the latch is reset at the single quarantine-SET site, so each NEW quarantine
        // episode re-arms the signal. This covers two regressions a `last_refresh_outcome`-based
        // guard fails: (b) a sweep that saw `Dead` BEFORE the account quarantined must STILL fire
        // once it does, and (a) a recover→re-die must re-fire.
        use crate::contract::RefreshDelta;
        let mut daemon = three_account_daemon(FakeRosterPoller::new()).await;
        let dead = |uuid: &str| RefreshObservation {
            account_uuid: uuid.to_owned(),
            expires_at_ms: Some(1_782_777_600_000),
            refresh: Some(RefreshDelta {
                outcome: RefreshEventOutcome::Dead,
                token_rotated: false,
            }),
        };
        let mut events = Vec::new();
        // Drive account `i` into quarantine through the real poll path (monitor_401_n = 3): the
        // Nth consecutive 401 sets `quarantined` AND resets the #261 latch.
        let quarantine = |d: &mut FakeDaemon, i: usize, sink: &mut Vec<Event>| {
            for _ in 0..3 {
                d.note_poll_outcome(i, &Err(Error::UsageUnauthorized), sink);
            }
        };

        // (b) The sweep sees `Dead` while account 0 is NOT yet quarantined: no signal, but
        // `last_refresh_outcome` is now `Some(Dead)` — the state that would poison a naive guard.
        assert_eq!(daemon.apply_refresh_observation(&dead("u-A")), None);
        assert_eq!(
            daemon.state.accounts[0].health.last_refresh_outcome,
            Some(RefreshEventOutcome::Dead)
        );

        // The access token then 401-streaks account 0 into quarantine; the SET clears the latch.
        quarantine(&mut daemon, 0, &mut events);
        assert!(daemon.state.accounts[0].health.quarantined);
        assert!(!daemon.state.accounts[0].health.unrecoverable_signaled);
        // Despite `last_refresh_outcome` ALREADY being `Dead`, the next dead sweep FIRES — the
        // latch, not the outcome history, gates the edge.
        assert_eq!(
            daemon.apply_refresh_observation(&dead("u-A")),
            Some(Event::CredentialUnrecoverable {
                account: "work".to_owned(),
            })
        );

        // (a) Recover (an operator re-login un-quarantines) then re-die: the fresh episode re-fires.
        daemon.state.accounts[0].health.quarantined = false;
        daemon.state.accounts[0].health.consec_401 = 0;
        quarantine(&mut daemon, 0, &mut events);
        assert!(!daemon.state.accounts[0].health.unrecoverable_signaled);
        assert_eq!(
            daemon.apply_refresh_observation(&dead("u-A")),
            Some(Event::CredentialUnrecoverable {
                account: "work".to_owned(),
            })
        );
    }

    /// The #378 daemon-side wiring the pure `SystemicRefreshHealth` unit tests can't reach:
    /// `note_systemic_refresh` folds each sweep through the daemon's OWN configured threshold — here
    /// the default 3, since `three_account_daemon` leaves `systemic_failure_n` at the config default
    /// (the "drives at defaults" integration case the field's doc-comment anticipates) — and an
    /// active episode surfaces in `snapshot`'s `systemic_refresh` projection, the `status`-visible
    /// indicator that shows the mechanism is down without waiting for an account to die.
    #[tokio::test]
    async fn note_systemic_refresh_threads_the_configured_threshold_and_projects_into_snapshot() {
        let mut daemon = three_account_daemon(FakeRosterPoller::new()).await;
        const NOW: i64 = 1_782_777_600;
        let no_readings: [Option<Usage>; 3] = [None, None, None];
        let indicator = |d: &FakeDaemon| d.snapshot(None, &no_readings, NOW).systemic_refresh;

        // Below the default threshold the mechanism-down streak climbs but stays silent, and the
        // indicator reads healthy — proving the daemon threads its own `systemic_failure_n`, not a
        // hardcoded N (a broken default or field-plumbing would fire early here).
        assert_eq!(daemon.note_systemic_refresh(SweepHealth::AllError), None);
        assert_eq!(daemon.note_systemic_refresh(SweepHealth::AllError), None);
        assert_eq!(indicator(&daemon), None, "healthy below the threshold");

        // The 3rd consecutive all-error sweep crosses the threshold → exactly one edge-triggered
        // failure carrying the count, and the snapshot now surfaces the mechanism-down count.
        assert_eq!(
            daemon.note_systemic_refresh(SweepHealth::AllError),
            Some(Event::RefreshSystemicFailure { consecutive: 3 })
        );
        assert_eq!(
            indicator(&daemon),
            Some(3),
            "active episode is status-visible"
        );

        // A further all-error sweep keeps climbing but does NOT re-emit (edge-, not level-triggered).
        assert_eq!(daemon.note_systemic_refresh(SweepHealth::AllError), None);
        assert_eq!(indicator(&daemon), Some(4));

        // A single working sweep is the recovery edge: one recovery event, and the indicator clears.
        assert_eq!(
            daemon.note_systemic_refresh(SweepHealth::Working),
            Some(Event::RefreshSystemicRecovered)
        );
        assert_eq!(
            indicator(&daemon),
            None,
            "recovery clears the status indicator"
        );
    }

    // --- #42 dead-credential lifecycle -------------------------------------
    //
    // The persistent-401 lifecycle: detect (N consecutive 401s → DEAD), quarantine
    // (skip the dead account, never halt the rotation), emergency-swap (escape a dead
    // ACTIVE account immediately, bypassing trigger + cooldown), auto-recover (M
    // consecutive live polls un-quarantine a re-logged-in account), and signal (one
    // edge-triggered event per transition + a durable "needs re-login" status). The
    // pure `classify_poll` mapping and the per-account health that carries the streak
    // ACROSS ticks (the issue's CODE PREREQUISITE) are exercised directly.

    #[tokio::test]
    async fn nth_consecutive_401_quarantines_the_account_and_signals_once() {
        // Detection + edge-trigger + anti-spam, driven directly (a static poller
        // cannot script a streak that crosses the threshold). Driving `spare`
        // (non-active) isolates detection from the emergency-swap path.
        let mut daemon = lifecycle_daemon().await;
        let mut events = Vec::new();

        // Two 401s climb the streak; below the threshold (3) the account stays alive.
        daemon.note_poll_outcome(1, &Err(Error::UsageUnauthorized), &mut events);
        daemon.note_poll_outcome(1, &Err(Error::UsageUnauthorized), &mut events);
        assert!(!daemon.state.accounts[1].health.quarantined);
        assert_eq!(daemon.state.accounts[1].health.consec_401, 2);

        // The 3rd consecutive 401 declares the credential DEAD: the climbing
        // `monitor_401` AND exactly one `credential_dead`, on the false→true edge.
        events.clear();
        daemon.note_poll_outcome(1, &Err(Error::UsageUnauthorized), &mut events);
        assert!(daemon.state.accounts[1].health.quarantined);
        assert_eq!(
            events,
            vec![
                Event::Monitor401 {
                    account: "spare".to_owned(),
                    consecutive: 3,
                },
                Event::CredentialDead {
                    account: "spare".to_owned(),
                },
            ]
        );

        // A 4th 401 on the already-dead account is SILENT — the dead state is a
        // durable status, not a repeated log line (no spam).
        events.clear();
        daemon.note_poll_outcome(1, &Err(Error::UsageUnauthorized), &mut events);
        assert!(daemon.state.accounts[1].health.quarantined);
        assert!(
            events.is_empty(),
            "an already-dead 401 re-emits nothing: {events:?}"
        );
    }

    // --- Issue #162: poll↔refresh seam ------------------------------------

    /// A [`PollRefresh`] fake for the #162 seam tests: it COUNTS refresh calls (the
    /// once-per-episode guard, AC-4), returns a scripted [`RefreshOutcome`], and — when
    /// `revive_to` is set — REVIVES the account by flipping its shared [`SeamOutcomes`]
    /// entry to a live reading (the false-death the fix rescues). `hard_error` makes the
    /// refresh itself fail (the fail-safe path).
    struct SeamRefresh {
        outcomes: SeamOutcomes,
        outcome: RefreshOutcome,
        revive_to: Option<Usage>,
        hard_error: bool,
        calls: Rc<Cell<u32>>,
    }

    impl PollRefresh for SeamRefresh {
        fn refresh<'a>(
            &'a self,
            account: &'a Account,
        ) -> Pin<Box<dyn Future<Output = Result<RefreshReport>> + 'a>> {
            Box::pin(async move {
                self.calls.set(self.calls.get() + 1);
                if self.hard_error {
                    // A refresh that cannot even run (spawn / lock failure) → could-not-revive.
                    return Err(Error::SwapLockBusy);
                }
                if let Some(usage) = self.revive_to {
                    self.outcomes
                        .borrow_mut()
                        .insert(account.account_uuid.clone(), Scripted::Ok(usage));
                }
                Ok(RefreshReport {
                    outcome: self.outcome,
                    expires_at_delta_secs: None,
                    // A rotation only happens when CC actually performed the exchange (a real
                    // `Refreshed`); NoChange / Dead / Error never rotate. Lets the #279
                    // poll-refresh event test observe a `true` threaded from the report.
                    refresh_token_rotated: matches!(self.outcome, RefreshOutcome::Refreshed),
                    re_stashed: matches!(self.outcome, RefreshOutcome::Refreshed),
                })
            })
        }
    }

    /// A two-account seam daemon (issue #162): `work` (`u-A`) polls healthy and stays the
    /// active account; `spare` (`u-B`) is the non-active account under test (isolating the
    /// refresh-retry from the emergency-swap path, exactly as
    /// [`nth_consecutive_401_quarantines_the_account_and_signals_once`] isolates detection).
    /// The round-robin schedule (#80) polls `work` then `spare`, so `spare` is polled on
    /// every SECOND tick. Returns the daemon plus the shared outcome cell (to re-script
    /// mid-run) and the refresh call-counter (to assert no storm).
    async fn seam_daemon(
        spare_outcome: Scripted,
        refresh_outcome: RefreshOutcome,
        revive_to: Option<Usage>,
        hard_error: bool,
        monitor_401_n: u8,
    ) -> (
        Daemon<SeamPoller, FakeCredentialStore, FakeAccountStash, FakeClock>,
        SeamOutcomes,
        Rc<Cell<u32>>,
    ) {
        let roster = vec![account("u-A", "work"), account("u-B", "spare")];
        let store = store_holding(b"A-token").await;
        let stash = stash_with(&[
            ("Sessiometer/u-A", b"A-token", "u-A"),
            ("Sessiometer/u-B", b"B-token", "u-B"),
        ])
        .await;
        let (dir, json) = claude_json("u-A");
        // Keep the temp `~/.claude.json` alive for the daemon's life (as `three_account_daemon`).
        std::mem::forget(dir);
        let tun = Tunables {
            monitor_401_n,
            ..tunables(95, 80, 0)
        };
        let outcomes: SeamOutcomes = Rc::new(RefCell::new(HashMap::from([
            ("u-A".to_owned(), Scripted::Ok(reading(0.10, 0.10))),
            ("u-B".to_owned(), spare_outcome),
        ])));
        let calls = Rc::new(Cell::new(0u32));
        let daemon = Daemon::new(
            roster,
            SeamPoller {
                outcomes: outcomes.clone(),
            },
            store,
            stash,
            FakeClock::frozen(),
            json,
            &tun,
        )
        .with_refresh_engine(Box::new(SeamRefresh {
            outcomes: outcomes.clone(),
            outcome: refresh_outcome,
            revive_to,
            hard_error,
            calls: calls.clone(),
        }));
        (daemon, outcomes, calls)
    }

    #[tokio::test]
    async fn a_usage_401_that_clears_after_refresh_does_not_quarantine() {
        // AC-1: a parked account whose access token merely EXPIRED (401) but whose refresh
        // token is valid → the daemon refreshes + re-polls, the re-poll CLEARS, and the #42
        // death streak never advances. This is the false death the fix eliminates.
        let (mut daemon, _outcomes, calls) = seam_daemon(
            Scripted::Unauthorized,
            RefreshOutcome::Refreshed,
            Some(reading(0.10, 0.10)), // the refresh REVIVES the spare's token
            false,
            3,
        )
        .await;
        // Drive three spare polls (round-robin idx 1 → ticks 2, 4, 6).
        for _ in 0..6 {
            daemon.tick().await;
        }
        assert!(
            !daemon.state.accounts[1].health.quarantined,
            "a 401 that clears after one refresh must NOT quarantine the account",
        );
        assert_eq!(
            daemon.state.accounts[1].health.consec_401, 0,
            "the successful re-poll resets the streak",
        );
        assert_eq!(
            calls.get(),
            1,
            "exactly one refresh — the revive, not a per-poll storm",
        );
    }

    #[tokio::test]
    async fn a_usage_401_that_survives_a_fresh_token_still_quarantines_after_n() {
        // AC-2 (+ AC-4): a 401 that PERSISTS after a fresh token is the genuine dead signal —
        // it still quarantines after `monitor_401_n` such survivals, and the refresh fires at
        // most ONCE per episode, not on every poll.
        let (mut daemon, _outcomes, calls) = seam_daemon(
            Scripted::Unauthorized,
            RefreshOutcome::Refreshed, // the refresh "succeeds" but does NOT revive (no flip)
            None,
            false,
            3,
        )
        .await;
        // spare polled on ticks 2, 4, 6 → three surviving 401s → quarantine at N = 3.
        for _ in 0..6 {
            daemon.tick().await;
        }
        assert!(
            daemon.state.accounts[1].health.quarantined,
            "a 401 that survives the fresh token must still quarantine after N",
        );
        assert_eq!(
            calls.get(),
            1,
            "AC-4: at most ONE refresh per streak episode — no per-poll refresh storm",
        );
    }

    #[tokio::test]
    async fn a_refresh_reporting_dead_is_treated_as_a_genuine_death() {
        // AC-3: the refresh clears the refresh token in place (Dead) — a genuine death. The
        // re-poll is skipped, the 401 stands, and the account quarantines through the streak.
        let (mut daemon, _outcomes, calls) =
            seam_daemon(Scripted::Unauthorized, RefreshOutcome::Dead, None, false, 3).await;
        for _ in 0..6 {
            daemon.tick().await;
        }
        assert!(
            daemon.state.accounts[1].health.quarantined,
            "a refresh that reports the token Dead must quarantine the account",
        );
        assert_eq!(
            calls.get(),
            1,
            "one refresh confirmed the death; the rest of the streak advances directly",
        );
    }

    #[tokio::test]
    async fn a_refresh_that_fails_is_fail_safe_and_still_quarantines() {
        // Fail-safe AC: a refresh that itself ERRORS (spawn / lock failure) is handled — it
        // never crashes the poll loop, and "could not revive" lets the 401 stand so a truly
        // dead account still quarantines after N.
        let (mut daemon, _outcomes, calls) = seam_daemon(
            Scripted::Unauthorized,
            // unused: `hard_error` short-circuits before the report; any error sub-reason stands in.
            RefreshOutcome::Error(crate::refresh::RefreshErrorReason::SpawnFailed),
            None,
            true,
            3,
        )
        .await;
        for _ in 0..6 {
            daemon.tick().await;
        }
        assert!(
            daemon.state.accounts[1].health.quarantined,
            "a refresh failure is treated as could-not-revive → the account still quarantines",
        );
        assert_eq!(
            calls.get(),
            1,
            "the failed refresh is still bounded to one attempt per episode",
        );
    }

    #[tokio::test]
    async fn a_new_streak_episode_may_refresh_again_after_a_recovery() {
        // AC-4 boundary: the once-per-episode guard is per-STREAK, not per-lifetime. A 401
        // refreshes (persists → streak = 1); the streak then RESETS on a live poll, closing
        // the episode; a LATER 401 opens a fresh episode allowed one more refresh.
        let (mut daemon, outcomes, calls) = seam_daemon(
            Scripted::Unauthorized,
            RefreshOutcome::Refreshed, // succeeds but does not auto-revive
            None,
            false,
            3,
        )
        .await;
        daemon.tick().await; // tick 1: work (healthy)
        daemon.tick().await; // tick 2: spare 401 → refresh (calls = 1), streak = 1
        assert_eq!(calls.get(), 1);
        assert_eq!(daemon.state.accounts[1].health.consec_401, 1);
        // Heal the spare: its next poll is Live → the streak resets, closing the episode.
        outcomes
            .borrow_mut()
            .insert("u-B".to_owned(), Scripted::Ok(reading(0.10, 0.10)));
        daemon.tick().await; // tick 3: work
        daemon.tick().await; // tick 4: spare Live → streak resets to 0
        assert_eq!(daemon.state.accounts[1].health.consec_401, 0);
        assert_eq!(calls.get(), 1, "a live poll needs no refresh");
        // Break the spare again → the next spare 401 is a NEW episode → one more refresh.
        outcomes
            .borrow_mut()
            .insert("u-B".to_owned(), Scripted::Unauthorized);
        daemon.tick().await; // tick 5: work
        daemon.tick().await; // tick 6: spare 401 (consec 0) → refresh AGAIN (calls = 2)
        assert_eq!(
            calls.get(),
            2,
            "a fresh streak episode is allowed one more refresh",
        );
    }

    #[tokio::test]
    async fn a_healthy_poll_path_never_refreshes_or_quarantines() {
        // The seam is inert on the happy path — a never-401 account triggers no refresh (no
        // `claude -p` spawn) and never quarantines, so the fix costs the common case nothing.
        let (mut daemon, _outcomes, calls) = seam_daemon(
            Scripted::Ok(reading(0.20, 0.10)), // spare polls healthy from the start
            RefreshOutcome::Refreshed,
            None,
            false,
            3,
        )
        .await;
        for _ in 0..6 {
            daemon.tick().await;
        }
        assert!(!daemon.state.accounts[1].health.quarantined);
        assert_eq!(
            calls.get(),
            0,
            "a healthy poll path never invokes the refresh seam",
        );
    }

    #[tokio::test]
    async fn the_active_account_is_never_isolated_refreshed_on_a_401() {
        // Issue #253: the #162 refresh-then-retry must NEVER isolated-refresh the ACTIVE account.
        // The #102 engine performs a real OAuth exchange that ROTATES the refresh token
        // server-side (`refresh.rs` Caller contract), invalidating the canonical credential every
        // live Claude Code session reads — the exact hazard #105/`refresh_exclusions` and
        // #250/`poke` already guard. The existing seam tests only drive the non-active `spare`;
        // this covers the active account. Two rotation-INDEPENDENT defects must hold
        // deterministically:
        //   1. caller-contract: the active account is never handed to `engine.refresh` at all
        //      (`calls == 0`), and
        //   2. no masking: its surviving 401 ADVANCES the #42 streak (toward operator re-login),
        //      never a stash re-poll that resets the streak and marks it healthy.
        // `revive_to` is set so that WITHOUT the fix the masking is sharp (a refresh would revive
        // `work` and reset its streak); WITH the fix the refresh never fires, so it is inert.
        let (mut daemon, outcomes, calls) = seam_daemon(
            Scripted::Ok(reading(0.10, 0.10)), // spare stays healthy — isolate the active path
            RefreshOutcome::Refreshed,
            Some(reading(0.10, 0.10)), // would (wrongly) revive `work` IF it were refreshed
            false,
            3,
        )
        .await;
        // Re-script the ACTIVE account (`work` / u-A, idx 0) to 401.
        outcomes
            .borrow_mut()
            .insert("u-A".to_owned(), Scripted::Unauthorized);
        // `work` is polled on ticks 1 and 3 (staggered schedule #80) → two surviving 401s.
        for _ in 0..4 {
            daemon.tick().await;
        }
        assert_eq!(
            calls.get(),
            0,
            "#253: the active account must NEVER be isolated-refreshed — its live session reads \
             the canonical credential the #102 refresh would rotate server-side",
        );
        assert_eq!(
            daemon.state.accounts[0].health.consec_401, 2,
            "#253: a still-active account's 401 advances the #42 streak toward operator re-login, \
             never a stash-only refresh + re-poll that resets the streak and masks it healthy",
        );
    }

    #[tokio::test]
    async fn a_reactive_refresh_of_a_swap_target_cannot_race_the_promotion() {
        // Issue #426 council falsifier. The #162 reactive engine is now ALWAYS wired (hoisted out
        // of `[refresh].enabled`), so the swap-race must be proven safe: a reactive refresh must
        // never leave an account that is promoted to active THIS TICK holding a torn canonical.
        // The adversarial tick does BOTH at once — reactively refresh a PARKED account on its 401
        // AND promote that same account to active. It is safe by construction:
        //   - the refresh fires while the target is still PARKED (`state.active != Some(i)`,
        //     token-first #207): no live session reads its token, so the isolated engine writing
        //     only its STASH (#253) harms nothing; and
        //   - the swap runs STRICTLY AFTER the refresh in the single-threaded tick (`refresh_retry`
        //     at the poll seam, THEN `decide_action`) and promotes FROM THAT SAME STASH
        //     (`incoming = target.stash()`, read back in `record_swap`) — so the canonical a live
        //     session reads post-swap is exactly the token the refresh left in the stash, never a
        //     torn / stale one. There is no in-tick ordering that promotes the account FIRST and
        //     then reactively refreshes its now-live canonical.
        let (mut daemon, outcomes, calls) = seam_daemon(
            Scripted::Unauthorized,    // spare (u-B): parked, its access token 401s…
            RefreshOutcome::Refreshed, // …the isolated refresh succeeds…
            Some(reading(0.10, 0.20)), // …reviving it to a VIABLE, below-floor swap target.
            false,
            3,
        )
        .await;
        // work (u-A) is the ACTIVE account, carried OVER its session trigger (0.97 > 0.95) — so the
        // very tick that revives the spare also decides to swap AWAY from work, TO the spare.
        outcomes
            .borrow_mut()
            .insert("u-A".to_owned(), Scripted::Ok(reading(0.97, 0.40)));

        // Tick 1 polls the active work (over trigger, but not yet warmed up → HELD). Tick 2 polls
        // the parked spare: its 401 fires the reactive refresh (revive), and the now-warmed
        // decision swaps work → spare in the SAME tick.
        let _ = daemon.tick().await; // work polled → HELD (pre-warm-up)
        let swap_tick = daemon.tick().await; // spare 401 → reactive refresh → swapped-to

        assert_eq!(
            swap_tick.action,
            TickAction::Swapped { from: 0, to: 1 },
            "the tick that reactively refreshes the parked spare also promotes it — the exact \
             swap-race the falsifier stresses",
        );
        assert_eq!(
            calls.get(),
            1,
            "exactly ONE reactive refresh fired — the PARKED spare's; the active work account was \
             never isolated-refreshed (the #253 / token-first #207 exclusion held throughout)",
        );
        assert_eq!(
            daemon.state.active,
            Some(1),
            "the spare was promoted to the active account",
        );
        // The falsifier's core: the promoted canonical is the token the swap read from the spare's
        // STASH — the very stash the reactive refresh owns (and, with the real engine, CAS-wrote the
        // fresh post-rotation token to one step earlier). A torn race would leave the canonical
        // holding work's OLD token (no promotion) or a stale value; it holds the spare's stash.
        assert!(
            daemon.store.read().await.unwrap().matches(&cred(b"B-token")),
            "the canonical a live session reads holds the spare's stash token, promoted coherently \
             AFTER the reactive refresh — never a torn write to a live credential",
        );
        // Cross-tick ordering: now that the spare is ACTIVE, a later 401 on it can NEVER be
        // reactively refreshed (token-first #207 excludes the active account), so the feared
        // swap-then-refresh-the-now-active ordering cannot arise on a subsequent tick either.
        assert!(
            !daemon.should_refresh_retry(1, &Err(Error::UsageUnauthorized)),
            "the newly-promoted active account is excluded from reactive refresh going forward",
        );
    }

    /// Drive the #162 seam to exactly ONE poll-refresh firing (issue #255) and return the
    /// durable events the REFRESHING tick emitted: tick 1 polls `work` (healthy, seam inert),
    /// tick 2 is `spare`'s first 401 → [`refresh_retry`](Daemon::refresh_retry) → the
    /// `Event::PollRefresh` under test. `refresh_outcome` is what the fake engine reports;
    /// `hard_error` makes the refresh itself fail (the fail-safe `Error` path).
    async fn poll_refresh_tick_events(
        refresh_outcome: RefreshOutcome,
        hard_error: bool,
    ) -> Vec<Event> {
        let (mut daemon, _outcomes, _calls) =
            seam_daemon(Scripted::Unauthorized, refresh_outcome, None, hard_error, 3).await;
        daemon.tick().await; // tick 1: work (healthy) — the seam stays inert
        daemon.tick().await.events // tick 2: spare's first 401 → the poll-refresh fires
    }

    #[tokio::test]
    async fn a_poll_refresh_emits_one_durable_event_per_outcome_branch() {
        // AC (issue #255): every #162 poll-refresh firing emits ONE durable `Event::PollRefresh`
        // carrying the target PARKED account (redacted handle) and the classified refresh outcome
        // — the isolated-refresh ACTION the durable log lacked (only the DOWNSTREAM poll outcome
        // was evented, via `note_poll_outcome`). One firing per outcome branch, asserted like the
        // `Monitor401` / `ReStash` event tests. The event also carries the cycle's rotation flag
        // (issue #279): a real `Refreshed` threads `rotated=true` from the report, while an engine
        // that could not run (`hard_error`) forces `false` via the `Err(_) => false` branch.
        let cases = [
            // (fake engine report outcome, hard engine error?, expected evented outcome)
            (
                RefreshOutcome::Refreshed,
                false,
                RefreshEventOutcome::Refreshed,
            ),
            (
                RefreshOutcome::NoChange,
                false,
                RefreshEventOutcome::NoChange,
            ),
            (RefreshOutcome::Dead, false, RefreshEventOutcome::Dead),
            (
                RefreshOutcome::Error(crate::refresh::RefreshErrorReason::SpawnFailed),
                false,
                RefreshEventOutcome::Error,
            ),
            // The engine could not even RUN (spawn / lock failure): the fail-safe `Error`
            // outcome, mirroring `refresh_tick`'s `error_refresh_event`. The report `outcome`
            // is unused on this path, so any value stands in.
            (RefreshOutcome::Refreshed, true, RefreshEventOutcome::Error),
        ];
        for (report_outcome, hard_error, expected) in cases {
            let events = poll_refresh_tick_events(report_outcome, hard_error).await;
            let poll_refreshes = events
                .iter()
                .filter(|e| matches!(e, Event::PollRefresh { .. }))
                .cloned()
                .collect::<Vec<_>>();
            // The rotation flag threads from the cycle's report on the Ok path (a real
            // `Refreshed` rotates in the seam fake), and is forced `false` when the engine
            // could not even run (`hard_error` → the `Err(_) => false` branch of #279).
            let expected_rotated =
                matches!(report_outcome, RefreshOutcome::Refreshed) && !hard_error;
            assert_eq!(
                poll_refreshes,
                vec![Event::PollRefresh {
                    account: "spare".to_owned(),
                    outcome: expected,
                    refresh_token_rotated: expected_rotated,
                }],
                "report {report_outcome:?} (hard_error={hard_error}) must emit exactly one \
                 poll_refresh event with the redacted handle + mapped outcome + rotation flag",
            );
        }
    }

    // --- issue #643: revived-account stale-Dead-latch re-probe --------------
    //
    // A non-active account whose REFRESH token died latches 🔴 `Dead` (`last_refresh_outcome ==
    // Some(Dead)`), which clears ONLY on a genuinely successful refresh. Manual recovery
    // (`sessiometer login` on a parked account whose ACCESS token is still valid, or `sessiometer
    // use` activating a dead spare) never drove such a refresh — usage polls kept returning 200, so
    // the account read stale 🔴 for a full ~8h access-token lifetime. The fix re-probes on the spot:
    // the parked path (`reconcile_restored`) drives the isolated `poll_refresh` engine; the active
    // path (`reprobe_active_if_dead`) drives the active-safe `keep_warm` engine. Recovery stays
    // gated on a SUCCESSFUL refresh (never a usage 200 — the #427 false-recovery guard).

    #[tokio::test]
    async fn reconcile_restored_reprobes_a_dead_parked_account_and_clears_to_healthy_on_a_live_refresh(
    ) {
        // AC-1 (positive): a PARKED account latched 🔴 `Dead` whose operator just ran `claude /login`
        // (the `restored` notify) is re-probed with ONE isolated refresh. The revived credential
        // answers (`Refreshed`), so the `Dead` latch clears, the quarantine lifts, and the account
        // reads 🟢 WITHIN the cycle — not stale 🔴 until the next natural near-expiry sweep.
        const NOW: i64 = 1_782_777_600;
        let (mut daemon, _outcomes, calls) = seam_daemon(
            Scripted::Ok(reading(0.10, 0.10)),
            RefreshOutcome::Refreshed,
            None,
            false,
            3,
        )
        .await;
        // `work` (idx 0) stays active; `spare` (u-B, idx 1) is the parked account latched
        // Dead+quarantined, with its ACCESS token still valid (the #643 precondition — usage polls
        // keep returning 200, so nothing else triggers a refresh). Already signaled unrecoverable by
        // the sweep that first confirmed the death.
        daemon.state.active = Some(0);
        daemon.state.accounts[1].health.last_refresh_outcome = Some(RefreshEventOutcome::Dead);
        daemon.state.accounts[1].health.quarantined = true;
        daemon.state.accounts[1].health.unrecoverable_signaled = true;
        daemon.state.accounts[1].health.access_expires_at = Some(NOW + 3600);

        let events = daemon.reconcile_restored("u-B").await;

        assert_eq!(
            calls.get(),
            1,
            "exactly one isolated refresh re-probed the revived credential"
        );
        assert!(
            !daemon.state.accounts[1].health.quarantined,
            "a genuinely successful refresh lifts the quarantine"
        );
        let h = &daemon.state.accounts[1].health;
        assert_eq!(
            credential_health(
                h.quarantined,
                h.last_refresh_outcome,
                h.consecutive_refresh_failures,
                h.access_expires_at,
                false,
                NOW,
            ),
            CredentialHealth::Healthy,
            "the revived credential reads 🟢, not the stale 🔴 Dead latch",
        );
        assert_eq!(
            events,
            vec![
                Event::PollRefresh {
                    account: "spare".to_owned(),
                    outcome: RefreshEventOutcome::Refreshed,
                    refresh_token_rotated: true,
                },
                Event::CredentialRestored {
                    account: "spare".to_owned(),
                },
            ],
            "the re-probe logs its isolated-refresh ACTION, then the un-quarantine edge",
        );
    }

    #[tokio::test]
    async fn reconcile_restored_clears_a_dead_not_quarantined_parked_account_via_the_fold() {
        // AC-1 (Case B): a refresh-detected death the 401 path never quarantined (`last_refresh_outcome
        // == Dead` but NOT quarantined). `apply_refresh_restore` is a no-op here (nothing to
        // un-quarantine), so it is the FOLD of the fresh live outcome — not the un-quarantine — that
        // clears the verdict. Proves the fix does not rely on the quarantine flag being set.
        const NOW: i64 = 1_782_777_600;
        let (mut daemon, _outcomes, calls) = seam_daemon(
            Scripted::Ok(reading(0.10, 0.10)),
            RefreshOutcome::Refreshed,
            None,
            false,
            3,
        )
        .await;
        daemon.state.active = Some(0);
        daemon.state.accounts[1].health.last_refresh_outcome = Some(RefreshEventOutcome::Dead);
        daemon.state.accounts[1].health.quarantined = false; // Case B: dead verdict, never quarantined
        daemon.state.accounts[1].health.access_expires_at = Some(NOW + 3600);

        let events = daemon.reconcile_restored("u-B").await;

        assert_eq!(calls.get(), 1, "the revived credential is re-probed once");
        let h = &daemon.state.accounts[1].health;
        assert_eq!(
            credential_health(
                h.quarantined,
                h.last_refresh_outcome,
                h.consecutive_refresh_failures,
                h.access_expires_at,
                false,
                NOW,
            ),
            CredentialHealth::Healthy,
            "the fold of a live outcome clears the Dead verdict even without a quarantine to lift",
        );
        assert_eq!(
            events,
            vec![Event::PollRefresh {
                account: "spare".to_owned(),
                outcome: RefreshEventOutcome::Refreshed,
                refresh_token_rotated: true,
            }],
            "no CredentialRestored — there was no quarantine to lift (Case B)",
        );
    }

    #[tokio::test]
    async fn reconcile_restored_keeps_an_honest_dead_when_the_revived_parked_credential_still_fails(
    ) {
        // AC-3 (the false-recovery regression guard): a login-revive whose FRESH credential ALSO
        // fails to refresh (the isolated re-probe returns `Dead`) must NOT be falsely cleared.
        // Recovery from the terminal 🔴 stays gated on a genuinely successful refresh (issue #427) —
        // a usage-poll 200 exercises only the ACCESS token and never reaches this refresh path — so
        // the account stays quarantined and reads an honest 🔴. The operator signal does NOT re-fire
        // (the #261 latch was already set).
        const NOW: i64 = 1_782_777_600;
        let (mut daemon, _outcomes, calls) = seam_daemon(
            Scripted::Ok(reading(0.10, 0.10)),
            RefreshOutcome::Dead,
            None,
            false,
            3,
        )
        .await;
        daemon.state.active = Some(0);
        daemon.state.accounts[1].health.last_refresh_outcome = Some(RefreshEventOutcome::Dead);
        daemon.state.accounts[1].health.quarantined = true;
        daemon.state.accounts[1].health.unrecoverable_signaled = true;

        let events = daemon.reconcile_restored("u-B").await;

        assert_eq!(calls.get(), 1, "the revived credential is re-probed once");
        assert!(
            daemon.state.accounts[1].health.quarantined,
            "a still-dead re-probe keeps the quarantine — never falsely cleared",
        );
        let h = &daemon.state.accounts[1].health;
        assert_eq!(
            credential_health(
                h.quarantined,
                h.last_refresh_outcome,
                h.consecutive_refresh_failures,
                h.access_expires_at,
                false,
                NOW,
            ),
            CredentialHealth::Dead,
            "the confirmed-dead credential stays an honest 🔴, not a false 🟢",
        );
        assert_eq!(
            events,
            vec![Event::PollRefresh {
                account: "spare".to_owned(),
                outcome: RefreshEventOutcome::Dead,
                refresh_token_rotated: false,
            }],
            "the action is logged; the #261 operator signal does NOT re-fire (already latched)",
        );
    }

    #[tokio::test]
    async fn reconcile_restored_un_quarantines_a_transient_error_re_probe_to_at_risk_not_dead() {
        // A TRANSIENT re-probe error (a spawn / lock / read-back hiccup — INCONCLUSIVE, not a proven
        // death) must NOT strand a genuinely-fixed account. The three-way fold un-quarantines on any
        // non-`Dead` outcome, so an `Error` clears the terminal 🔴 to 🟡 `AtRisk` (which self-heals on
        // the next sweep) rather than leaving it stuck at 🔴 `Dead`/🟠 `Degraded`. This preserves the
        // #275 un-quarantine guarantee: a re-login always at least lifts the quarantine.
        const NOW: i64 = 1_782_777_600;
        let (mut daemon, _outcomes, calls) = seam_daemon(
            Scripted::Ok(reading(0.10, 0.10)),
            RefreshOutcome::Refreshed, // ignored: `hard_error` returns Err before reading it
            None,
            true, // hard_error → the isolated refresh cannot even run
            3,
        )
        .await;
        daemon.state.active = Some(0);
        daemon.state.accounts[1].health.last_refresh_outcome = Some(RefreshEventOutcome::Dead);
        daemon.state.accounts[1].health.quarantined = true;
        daemon.state.accounts[1].health.unrecoverable_signaled = true;

        let events = daemon.reconcile_restored("u-B").await;

        assert_eq!(calls.get(), 1, "the revived credential is re-probed once");
        assert!(
            !daemon.state.accounts[1].health.quarantined,
            "a transient error still lifts the quarantine (never strands a fixed account at 🔴)",
        );
        let h = &daemon.state.accounts[1].health;
        assert_eq!(
            credential_health(
                h.quarantined,
                h.last_refresh_outcome,
                h.consecutive_refresh_failures,
                h.access_expires_at,
                false,
                NOW,
            ),
            CredentialHealth::AtRisk,
            "an inconclusive error reads 🟡 AtRisk (self-heals next sweep), not a false 🔴/🟠",
        );
        assert_eq!(
            events,
            vec![
                Event::PollRefresh {
                    account: "spare".to_owned(),
                    outcome: RefreshEventOutcome::Error,
                    refresh_token_rotated: false,
                },
                Event::CredentialRestored {
                    account: "spare".to_owned(),
                },
            ],
        );
    }

    #[tokio::test]
    async fn reconcile_restored_bare_un_quarantines_a_degraded_account_without_a_refresh() {
        // A quarantined-but-NOT-`Dead` account (a bare `Degraded` 401-streak quarantine, whose
        // access-token streak a re-login clears WITHOUT needing a refresh) must take the plain #275
        // on-demand un-quarantine — NOT drive an isolated refresh. Proves the fix does not
        // over-refresh: only the terminal 🔴 `Dead` verdict warrants the re-probe.
        let (mut daemon, _outcomes, calls) = seam_daemon(
            Scripted::Ok(reading(0.10, 0.10)),
            RefreshOutcome::Refreshed,
            None,
            false,
            3,
        )
        .await;
        daemon.state.active = Some(0);
        daemon.state.accounts[1].health.last_refresh_outcome = None; // NOT Dead
        daemon.state.accounts[1].health.quarantined = true;

        let events = daemon.reconcile_restored("u-B").await;

        assert_eq!(
            calls.get(),
            0,
            "a non-Dead account is never isolated-refreshed — the plain #275 un-quarantine only",
        );
        assert!(
            !daemon.state.accounts[1].health.quarantined,
            "the bare on-demand un-quarantine still lifts the quarantine (#275 preserved)",
        );
        assert_eq!(
            events,
            vec![Event::CredentialRestored {
                account: "spare".to_owned(),
            }],
        );
    }

    #[tokio::test]
    async fn reconcile_restored_never_isolated_refreshes_the_active_account() {
        // #253 safety guard: the isolated `poll_refresh` engine rotates the server-side refresh token
        // but writes only the STASH — safe for PARKED accounts only. If a `restored` notify ever names
        // the ACTIVE account, `reconcile_restored` must FALL BACK to the plain un-quarantine and NEVER
        // drive the isolated engine against the live canonical (the active path re-probes via keep-warm
        // instead).
        let (mut daemon, _outcomes, calls) = seam_daemon(
            Scripted::Ok(reading(0.10, 0.10)),
            RefreshOutcome::Refreshed,
            None,
            false,
            3,
        )
        .await;
        // u-B (idx 1) is BOTH the active account AND latched Dead — the excluded case.
        daemon.state.active = Some(1);
        daemon.state.accounts[1].health.last_refresh_outcome = Some(RefreshEventOutcome::Dead);
        daemon.state.accounts[1].health.quarantined = true;

        let events = daemon.reconcile_restored("u-B").await;

        assert_eq!(
            calls.get(),
            0,
            "the isolated engine is NEVER driven against the active account (#253)",
        );
        // The other half of the guard: it still FALLS BACK to the plain #275 un-quarantine (a return
        // that did nothing would also leave `calls == 0`, so assert the fallback actually fired).
        assert!(
            !daemon.state.accounts[1].health.quarantined,
            "the active-account case falls back to the bare un-quarantine, not a silent no-op",
        );
        assert_eq!(
            events,
            vec![Event::CredentialRestored {
                account: "spare".to_owned(),
            }],
        );
    }

    #[tokio::test]
    async fn reprobe_active_if_dead_clears_a_use_activated_dead_account_on_a_live_mint() {
        // AC-2 (positive): `sessiometer use <dead-account>` activates an account latched 🔴 `Dead`. The
        // active-safe keep-warm mint answers (`Refreshed`), promotes the fresh token to the canonical,
        // and the fold clears the latch — so the account reads 🟢 within the cycle. `access_expires_at`
        // is left to `promote_canonical`'s #477 reconciliation (the fold passes `None`), NOT clobbered.
        const NOW: i64 = 1_782_777_600;
        let fresh_expiry_ms = 1_800_000_000_000 + 7 * 3_600_000;
        let fresh = warm_canonical(fresh_expiry_ms, "rt-live2");
        let (mut daemon, _outcomes, calls) = keep_warm_daemon(
            Scripted::Ok(reading(0.10, 0.10)),
            RefreshOutcome::Refreshed,
            None,
            Some(fresh.clone()),
            warm_canonical(FAR_FUTURE_MS, "rt-live"),
        )
        .await;
        daemon.state.active = Some(0);
        daemon.state.accounts[0].health.last_refresh_outcome = Some(RefreshEventOutcome::Dead);
        daemon.state.accounts[0].health.quarantined = true;
        daemon.state.accounts[0].health.unrecoverable_signaled = true;

        let events = daemon.reprobe_active_if_dead().await;

        assert_eq!(
            calls.get(),
            1,
            "exactly one active-safe mint re-probed the credential"
        );
        assert!(
            !daemon.state.accounts[0].health.quarantined,
            "a live mint lifts the quarantine"
        );
        assert_eq!(
            daemon.store.read().await.unwrap().expose(),
            fresh.expose(),
            "the fresh token is promoted to the canonical item a live session reads",
        );
        assert_eq!(
            daemon.state.accounts[0].health.access_expires_at,
            Some(fresh_expiry_ms / 1000),
            "the fold passes None so promote_canonical's #477 expiry is preserved, not clobbered",
        );
        let h = &daemon.state.accounts[0].health;
        assert_eq!(
            credential_health(
                h.quarantined,
                h.last_refresh_outcome,
                h.consecutive_refresh_failures,
                h.access_expires_at,
                false,
                NOW,
            ),
            CredentialHealth::Healthy,
            "the use-activated account reads 🟢, not the stale 🔴 Dead latch",
        );
        assert_eq!(
            events,
            vec![
                Event::KeepWarm {
                    account: "work".to_owned(),
                    trigger: KeepWarmTrigger::Recovery,
                    // A keep-warm PROMOTES rather than re-stashes → refreshed_not_restashed.
                    outcome: RefreshEventOutcome::RefreshedNotReStashed,
                    refresh_token_rotated: true,
                },
                Event::CredentialRestored {
                    account: "work".to_owned(),
                },
            ],
            "the re-probe logs a `recovery`-trigger keep_warm ACTION, then the un-quarantine edge",
        );
    }

    #[tokio::test]
    async fn reprobe_active_if_dead_keeps_an_honest_dead_when_the_active_mint_reports_dead() {
        // AC-2 / AC-3 (regression guard, active path): the RT was non-empty (so the mint DOES run) but
        // CC rejected it and the cycle reports `Dead` — a genuine death. No promote, the latch stays,
        // the account stays quarantined and reads an honest 🔴. Recovery is never falsely granted.
        const NOW: i64 = 1_782_777_600;
        let (mut daemon, _outcomes, calls) = keep_warm_daemon(
            Scripted::Ok(reading(0.10, 0.10)),
            RefreshOutcome::Dead,
            None,
            None,
            warm_canonical(FAR_FUTURE_MS, "rt-live"),
        )
        .await;
        daemon.state.active = Some(0);
        daemon.state.accounts[0].health.last_refresh_outcome = Some(RefreshEventOutcome::Dead);
        daemon.state.accounts[0].health.quarantined = true;
        daemon.state.accounts[0].health.unrecoverable_signaled = true;

        let events = daemon.reprobe_active_if_dead().await;

        assert_eq!(
            calls.get(),
            1,
            "the mint ran once (the RT was non-empty) and reported Dead"
        );
        assert!(
            daemon.state.accounts[0].health.quarantined,
            "a Dead mint keeps the quarantine — never falsely cleared",
        );
        assert_eq!(
            daemon.store.read().await.unwrap().expose(),
            warm_canonical(FAR_FUTURE_MS, "rt-live").expose(),
            "a Dead outcome promotes nothing — the canonical is untouched",
        );
        let h = &daemon.state.accounts[0].health;
        assert_eq!(
            credential_health(
                h.quarantined,
                h.last_refresh_outcome,
                h.consecutive_refresh_failures,
                h.access_expires_at,
                false,
                NOW,
            ),
            CredentialHealth::Dead,
            "the confirmed-dead active credential stays an honest 🔴",
        );
        assert_eq!(
            events,
            vec![Event::KeepWarm {
                account: "work".to_owned(),
                trigger: KeepWarmTrigger::Recovery,
                outcome: RefreshEventOutcome::Dead,
                refresh_token_rotated: false,
            }],
        );
    }

    #[tokio::test]
    async fn reprobe_active_if_dead_keeps_an_honest_dead_for_an_empty_refresh_token() {
        // AC-2 / AC-3 / invariant 4 (empty-RT): a dead (empty) refresh token cannot be revived by any
        // mint, so the active re-probe SHORT-CIRCUITS — no `claude -p` spawn, no event — and the honest
        // 🔴 stays. The absence of the refresh token IS the dead signal.
        const NOW: i64 = 1_782_777_600;
        let (mut daemon, _outcomes, calls) = keep_warm_daemon(
            Scripted::Ok(reading(0.10, 0.10)),
            RefreshOutcome::Refreshed,
            None,
            Some(cred(b"FRESH-A")),
            warm_canonical(FAR_FUTURE_MS, ""), // EMPTY refresh token → dead
        )
        .await;
        daemon.state.active = Some(0);
        daemon.state.accounts[0].health.last_refresh_outcome = Some(RefreshEventOutcome::Dead);
        daemon.state.accounts[0].health.quarantined = true;
        // A `Dead`-latched account was already signaled unrecoverable by the sweep that set it Dead,
        // so the fold does not re-fire the #261 operator signal (isolating the "no MINT action" claim).
        daemon.state.accounts[0].health.unrecoverable_signaled = true;

        let events = daemon.reprobe_active_if_dead().await;

        assert_eq!(
            calls.get(),
            0,
            "a dead (empty) refresh token skips the doomed mint spawn"
        );
        assert!(
            daemon.state.accounts[0].health.quarantined,
            "the honest 🔴 quarantine stays"
        );
        let h = &daemon.state.accounts[0].health;
        assert_eq!(
            credential_health(
                h.quarantined,
                h.last_refresh_outcome,
                h.consecutive_refresh_failures,
                h.access_expires_at,
                false,
                NOW,
            ),
            CredentialHealth::Dead,
            "an absent refresh token stays an honest 🔴",
        );
        assert!(
            events.is_empty(),
            "a skipped re-probe emits no action event: {events:?}"
        );
    }

    #[tokio::test]
    async fn reprobe_active_if_dead_is_a_noop_for_a_non_dead_active_account() {
        // Guard: the re-probe fires ONLY on the terminal 🔴 `Dead` verdict. A healthy active account
        // (a normal `use` swap onto a live spare) is left entirely to the normal tick — no mint.
        let (mut daemon, _outcomes, calls) = keep_warm_daemon(
            Scripted::Ok(reading(0.10, 0.10)),
            RefreshOutcome::Refreshed,
            None,
            Some(cred(b"FRESH-A")),
            warm_canonical(FAR_FUTURE_MS, "rt-live"),
        )
        .await;
        daemon.state.active = Some(0);
        daemon.state.accounts[0].health.last_refresh_outcome = Some(RefreshEventOutcome::Refreshed);

        let events = daemon.reprobe_active_if_dead().await;

        assert_eq!(
            calls.get(),
            0,
            "a non-Dead active account is never re-probed"
        );
        assert!(events.is_empty(), "no mint → no event: {events:?}");
    }

    #[tokio::test]
    async fn reprobe_active_if_dead_is_inert_without_a_wired_keep_warm_engine() {
        // Guard: `keep_warm` is the ONLY active-safe refresh (#253). With no keep-warm engine wired
        // (a `[refresh]`-off daemon — where `Dead` is in any case unreachable, the sweep that sets it
        // being on the same switch) the active re-probe is an inert no-op, never touching the isolated
        // `poll_refresh` engine against the live canonical.
        let (mut daemon, _outcomes, calls) = seam_daemon(
            Scripted::Ok(reading(0.10, 0.10)),
            RefreshOutcome::Refreshed,
            None,
            false,
            3,
        )
        .await;
        assert!(
            daemon.keep_warm.is_none(),
            "the seam daemon wires no keep-warm engine"
        );
        daemon.state.active = Some(0);
        daemon.state.accounts[0].health.last_refresh_outcome = Some(RefreshEventOutcome::Dead);

        let events = daemon.reprobe_active_if_dead().await;

        assert_eq!(
            calls.get(),
            0,
            "the active path never drives the isolated poll_refresh engine (#253)",
        );
        assert!(
            events.is_empty(),
            "an unwired keep-warm re-probe is a silent no-op"
        );
    }

    #[tokio::test]
    async fn run_loop_restored_reprobes_a_dead_parked_account_through_the_idle_select() {
        // AC-1 (wiring): the run loop's idle select must route a `Restored(uuid)` control signal into
        // `reconcile_restored`, which — for a `Dead` parked account with `poll_refresh` wired — drives
        // the isolated re-probe end-to-end. A regression unwiring the arm (or reverting it to the bare
        // `apply_refresh_restore`) would leave `spare` latched 🔴 and fail this test.
        let (mut daemon, _outcomes, calls) = seam_daemon(
            Scripted::Ok(reading(0.10, 0.10)),
            RefreshOutcome::Refreshed,
            None,
            false,
            3,
        )
        .await;
        // `spare` (u-B, idx 1) is the parked account latched Dead+quarantined; `work` is active. The
        // warm-up tick polls only the active `work`, so this state survives into the idle where the
        // control signal delivers the re-probe. The `NoopRefreshTicker` keeps the periodic sweep from
        // firing, so the ONLY refresh is the one `reconcile_restored` drives.
        daemon.state.active = Some(0);
        daemon.state.accounts[1].health.last_refresh_outcome = Some(RefreshEventOutcome::Dead);
        daemon.state.accounts[1].health.quarantined = true;
        daemon.state.accounts[1].health.unrecoverable_signaled = true;

        let logdir = tempfile::tempdir().unwrap();
        let log_path = logdir.path().join("sessiometer.log");
        let mut log = EventLog::at(&log_path).unwrap();
        // Tick 1 → idle delivers `Restored(u-B)` → tick 2 → shutdown. after(3): 1 start-up check
        // (pends) + 2 idle shutdown-checks.
        let mut shutdown = FakeShutdown::after(3);
        let control = OnceRestored::new("u-B");

        let mut diag = DiagnosticLog::new(std::io::sink(), Verbosity::Quiet);
        run_loop(
            &mut daemon,
            &mut log,
            &mut diag,
            &mut shutdown,
            &control,
            &mut NoopRefreshTicker,
            &mut NoopExternalLoginWatch,
        )
        .await
        .unwrap();

        assert_eq!(
            calls.get(),
            1,
            "the Restored signal reached reconcile_restored's isolated re-probe through the select",
        );
        assert!(
            !daemon.state.accounts[1].health.quarantined,
            "the revived parked account is un-quarantined within the cycle",
        );
        assert_ne!(
            daemon.state.accounts[1].health.last_refresh_outcome,
            Some(RefreshEventOutcome::Dead),
            "the stale Dead latch is cleared — no multi-hour 🔴",
        );
        let logged = std::fs::read_to_string(&log_path).unwrap();
        assert!(
            logged.contains("event=credential_restored account=spare"),
            "the un-quarantine edge rode the log: {logged:?}",
        );
        // The active account is UNCHANGED — restoring the parked spare never activates it.
        assert_eq!(daemon.state.active, Some(0), "work stays active");
    }

    #[tokio::test]
    async fn run_loop_manual_swap_reprobes_a_dead_active_account_through_the_idle_select() {
        // AC-2 (wiring): the run loop's idle select must route a `ManualSwapped` control signal into
        // `adopt_manual_swap` THEN `reprobe_active_if_dead` — so `sessiometer use <dead-account>`
        // forces the active-safe mint end-to-end. A regression dropping the post-adopt re-probe would
        // leave the newly-active account latched 🔴 and fail this test.
        let fresh = warm_canonical(FAR_FUTURE_MS, "rt-live2");
        let (mut daemon, _outcomes, calls) = keep_warm_daemon(
            Scripted::Ok(reading(0.10, 0.10)),
            RefreshOutcome::Refreshed,
            None,
            Some(fresh.clone()),
            // FAR-future canonical → the proactive near-expiry path stays inert, isolating the
            // recovery re-probe as the ONLY mint.
            warm_canonical(FAR_FUTURE_MS, "rt-live"),
        )
        .await;
        // The active `work` (idx 0) is latched Dead — the dead spare a forced `use` just activated.
        daemon.state.active = Some(0);
        daemon.state.accounts[0].health.last_refresh_outcome = Some(RefreshEventOutcome::Dead);
        daemon.state.accounts[0].health.quarantined = true;
        daemon.state.accounts[0].health.unrecoverable_signaled = true;

        let logdir = tempfile::tempdir().unwrap();
        let mut log = EventLog::at(&logdir.path().join("sessiometer.log")).unwrap();
        // Tick 1 → idle delivers `ManualSwapped` (adopt + re-probe) → tick 2 → shutdown. after(3): 1
        // start-up check (pends) + 2 idle shutdown-checks.
        let mut shutdown = FakeShutdown::after(3);
        let control = OnceManualSwap::new();

        let mut diag = DiagnosticLog::new(std::io::sink(), Verbosity::Quiet);
        run_loop(
            &mut daemon,
            &mut log,
            &mut diag,
            &mut shutdown,
            &control,
            &mut NoopRefreshTicker,
            &mut NoopExternalLoginWatch,
        )
        .await
        .unwrap();

        assert_eq!(
            calls.get(),
            1,
            "the ManualSwapped signal reached reprobe_active_if_dead's mint through the select",
        );
        assert_ne!(
            daemon.state.accounts[0].health.last_refresh_outcome,
            Some(RefreshEventOutcome::Dead),
            "the use-activated account's stale Dead latch is cleared within the cycle",
        );
        assert!(
            !daemon.state.accounts[0].health.quarantined,
            "the revived active account is un-quarantined",
        );
        assert_eq!(
            daemon.store.read().await.unwrap().expose(),
            fresh.expose(),
            "the fresh token was promoted to the canonical item",
        );
    }

    #[tokio::test]
    async fn run_loop_swap_requested_reprobes_a_dead_active_account_through_the_idle_select() {
        // AC-2 (wiring, menubar swap-on-click): the run loop's `Idle::SwapRequested` arm must — AFTER
        // writing the client ack — drive `reprobe_active_if_dead`, so a socket `swap` onto an account
        // latched 🔴 `Dead` forces the active-safe mint end-to-end (the menubar counterpart of the
        // `sessiometer use` → `ManualSwapped` path). A regression dropping ONLY this arm's post-ack
        // re-probe would pass every other test; this is its dedicated guard.
        let roster = vec![account("u-A", "work"), account("u-B", "spare")];
        let store = store_holding(b"A-token").await;
        // `spare`'s stash holds a LIVE-RT credential, so the forced swap installs a canonical the
        // active-safe mint can actually exchange — a dead-RT canonical would short-circuit, masking
        // whether the re-probe ran at all.
        let spare_cred = warm_canonical(FAR_FUTURE_MS, "rt-live");
        let stash = stash_with(&[
            ("Sessiometer/u-A", b"A-token", "u-A"),
            ("Sessiometer/u-B", spare_cred.expose(), "u-B"),
        ])
        .await;
        let (_dir, json) = claude_json("u-A");
        let poller = FakeRosterPoller::new()
            .ok("u-A", 0.10, 0.10)
            .ok("u-B", 0.10, 0.10);
        let tun = tunables(95, 80, 0);
        let fresh = warm_canonical(FAR_FUTURE_MS, "rt-live2");
        let calls = Rc::new(Cell::new(0u32));
        let mut daemon: FakeDaemon = Daemon::new(
            roster,
            poller,
            store,
            stash,
            FakeClock::new(Duration::from_secs(60)),
            json,
            &tun,
        )
        .with_keep_warm_engine(
            Box::new(SeamKeepWarm {
                outcomes: Rc::new(RefCell::new(HashMap::new())),
                outcome: RefreshOutcome::Refreshed,
                revive_to: None,
                fresh: Some(fresh.clone()),
                calls: calls.clone(),
            }),
            Duration::from_secs(3600),
        );
        // `spare` (u-B, idx 1) is the dead spare a forced swap-on-click activates. Proactive keep-warm
        // stays OFF (the default), so the ONLY mint is the recovery re-probe.
        daemon.state.accounts[1].health.last_refresh_outcome = Some(RefreshEventOutcome::Dead);
        daemon.state.accounts[1].health.quarantined = true;
        daemon.state.accounts[1].health.unrecoverable_signaled = true;

        // A real socket pair so the arm's `write_swap_ack` has a client end; the re-probe runs AFTER it.
        let (mut client, server) = tokio::net::UnixStream::pair().expect("socketpair");
        let control = OnceSwap {
            command: SwapCommand {
                target: "spare".to_owned(),
                force: true, // bypass the quarantine POLICY gate so the swap onto the dead spare lands
            },
            stream: RefCell::new(Some(server)),
            fired: Cell::new(false),
        };

        let logdir = tempfile::tempdir().unwrap();
        let mut log = EventLog::at(&logdir.path().join("sessiometer.log")).unwrap();
        let mut shutdown = FakeShutdown::after(4);
        let mut diag = DiagnosticLog::new(std::io::sink(), Verbosity::Quiet);
        run_loop(
            &mut daemon,
            &mut log,
            &mut diag,
            &mut shutdown,
            &control,
            &mut NoopRefreshTicker,
            &mut NoopExternalLoginWatch,
        )
        .await
        .unwrap();

        // The client received the swap ack — proving the arm reached its post-ack re-probe, not
        // aborted before it. (EOF arrives when the loop drops the server end after writing.)
        use tokio::io::AsyncReadExt;
        let mut reply = String::new();
        client.read_to_string(&mut reply).await.unwrap();
        assert!(
            !reply.trim().is_empty(),
            "the client received a swap ack: {reply:?}"
        );

        // The forced swap activated the dead spare, THEN the re-probe minted + cleared the latch.
        assert_eq!(
            daemon.state.active,
            Some(1),
            "the forced swap activated spare"
        );
        assert_eq!(
            calls.get(),
            1,
            "the SwapRequested arm drove the active-safe re-probe after the ack",
        );
        assert_ne!(
            daemon.state.accounts[1].health.last_refresh_outcome,
            Some(RefreshEventOutcome::Dead),
            "the swap-on-click's stale Dead latch is cleared within the cycle",
        );
        assert!(
            !daemon.state.accounts[1].health.quarantined,
            "the revived active account is un-quarantined",
        );
    }
}
