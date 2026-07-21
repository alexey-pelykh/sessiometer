// Copyright (c) 2026 Oleksii PELYKH
// SPDX-License-Identifier: MIT

//! Status/snapshot assembly for the [`Daemon`] decision core (issue #637 step 4, issue #659,
//! split out of the single `impl Daemon` block).
//!
//! Builds the [`StatusSnapshot`] every operator surface reads — the `status` one-shot, the
//! #165 `watch` subscription, and the foreground run's diagnostics — by rolling per-account
//! health, credential clocks and the forward-looking next-swap candidate into one
//! versioned value. Handles carry labels and percentages only, never a token (issue #15).

use super::*;

impl<P, C, S, K> super::Daemon<P, C, S, K>
where
    P: RosterPoller,
    C: CredentialStore,
    S: AccountStash,
    K: Clock,
{
    /// Read the just-polled account's stored access-token expiry (epoch SECONDS, issue
    /// #141) — the DISPLAY clock the poll path feeds into [`AccountHealth::poll_expires_at`],
    /// so `status --json` surfaces an expiry even with `[refresh]` off. Reads the SAME
    /// credential the usage poll used: the CANONICAL item for the active account (its token
    /// refreshes in place there, the freshest expiry), the per-account STASH otherwise —
    /// mirroring [`RealRosterPoller::poll`]. Reuses the non-secret
    /// [`crate::refresh::expires_at`] / [`crate::refresh::stored_expires_at`] extractors
    /// (only the `i64` is pulled, never the token) and converts MS→s at this boundary. A
    /// best-effort clock, never a gate: `None` when the credential is unreadable (a locked
    /// keychain, an absent stash), which just leaves the wire field null this cycle.
    pub(super) async fn read_poll_expires_at(
        &self,
        account: &Account,
        active: bool,
    ) -> Option<i64> {
        let expires_at_ms = if active {
            self.store
                .read()
                .await
                .ok()
                .and_then(|credential| crate::refresh::expires_at(credential.expose()))
        } else {
            crate::refresh::stored_expires_at(&self.stash, &account.stash()).await
        };
        expires_at_ms.map(millis_to_secs)
    }

    /// Recompute every account's 5-state credential-health rollup (issue #119) against
    /// `now_secs` and emit one [`Event::CredentialHealth`] per account whose verdict CHANGED
    /// since the last call — the edge-triggered health timeline the issue's AC-3 requires
    /// ("exactly one redacted event per transition"). The very first computation per account
    /// SEEDS [`AccountHealth::last_health`] WITHOUT emitting (no prior state to transition
    /// from), so a fresh daemon never logs a startup storm.
    ///
    /// Driven from the run loop AFTER folding the sweep's restores + observations, so a
    /// transition reflects both the quarantine machinery (#42, updated in `tick`) and the
    /// refresh clocks (#119, updated post-idle). Independent of — and complementary to — the
    /// #42 [`Event::CredentialDead`] / [`Event::CredentialRestored`] edges: those signal the
    /// quarantine sub-state and drive recovery, while this is the operator-facing rollup edge
    /// (it also captures the Healthy↔Stale↔AtRisk transitions #42 never sees, and a
    /// refresh-detected death the 401 path never quarantines).
    pub(super) fn note_health_transitions(&mut self, now_secs: i64) -> Vec<Event> {
        let mut events = Vec::new();
        // The same masked, in-rotation readings the display snapshot uses (keyed on the
        // current active), so the edge-triggered event verdict matches what `status` shows:
        // a `Some` entry is this account's positive-liveness signal (a successful poll),
        // `None` a failed poll / out-of-rotation account → the #137 `Unknown` input.
        let readings = self.decision_readings(self.state.active);
        for (i, reading) in readings.iter().enumerate() {
            let health = &self.state.accounts[i].health;
            let verdict = credential_health(
                health.quarantined,
                health.last_refresh_outcome,
                health.consecutive_refresh_failures,
                health.access_expires_at,
                reading.is_some(),
                now_secs,
            );
            // Emit only on a CHANGE from a SEEDED baseline; the first observation (None)
            // seeds silently.
            if let Some(prev) = self.state.accounts[i].health.last_health {
                if prev != verdict {
                    events.push(Event::CredentialHealth {
                        account: self.roster[i].label.clone(),
                        state: verdict,
                    });
                }
            }
            self.state.accounts[i].health.last_health = Some(verdict);
        }
        events
    }

    /// Build the non-secret per-account snapshot for the event log and the socket.
    pub(super) fn snapshot(
        &self,
        active: Option<usize>,
        readings: &[Option<Usage>],
        now_secs: i64,
    ) -> StatusSnapshot {
        // One monotonic read for the bounded-blindness projection (issue #479): `blind_elapsed` is
        // measured against the SAME clock the retained anchor's `at` was stamped on (`last_good`,
        // #450) — the monotonic `clock` seam, DISTINCT from the wall-clock `now_secs` the freshness
        // stamp + health rollup read.
        let blind_at = self.clock.now();
        StatusSnapshot {
            accounts: self
                .roster
                .iter()
                .enumerate()
                .map(|(i, account)| {
                    let health = &self.state.accounts[i].health;
                    AccountReading {
                        label: account.label.clone(),
                        active: active == Some(i),
                        enabled: account.enabled,
                        quarantined: health.quarantined,
                        // Mid-recovery iff dead AND its credential is currently answering
                        // again (issue #109) — a refinement of `quarantined`, so `status`
                        // can soften `needs re-login` to `recovering` for a healing account.
                        recovering: health.quarantined && health.recovery_successes > 0,
                        // The daemon's own viability verdict, deterministic (the un-jittered
                        // rotation line, not a per-cycle draw) so the displayed "resets in"
                        // matches when `use` would accept the account again (issue #72).
                        // Issue #607: this is the ROTATION line (ceiling − tail margin), the same
                        // line the swap + `use` gates use — so "weekly exhausted" means exactly
                        // "the daemon will not rotate onto this", with no band in which the UI
                        // reports an account usable while the daemon refuses it.
                        weekly_exhausted: readings[i]
                            .is_some_and(|usage| usage.weekly >= self.weekly_rotation_line()),
                        usage: readings[i],
                        // The credential clocks + the daemon-computed 5-state rollup (issue
                        // #119), projected from this account's carried health state. The
                        // rollup is computed HERE (daemon-side) against `now_secs`; the thin
                        // client just renders the verdict's glyph + the raw clocks. The wire
                        // clock prefers the refresh-sourced expiry and falls back to the
                        // poll-sourced one (issue #141) so it is populated with `[refresh]`
                        // off; the rollup below still reads ONLY the refresh-sourced field,
                        // so a lapsed idle poll clock never fires a false-🟠 Stale (see #137).
                        access_expires_at: health.access_expires_at.or(health.poll_expires_at),
                        refresh_health: refresh_health_view(health),
                        // #137: a `Some` reading is this account's positive-liveness signal (a
                        // successful poll); without one (and no refresh telemetry / expiry) the
                        // rollup is `Unknown`, never a false 🟢. The poll-sourced clock above is
                        // display-only and deliberately NOT fed here (set even on a failed poll).
                        health: credential_health(
                            health.quarantined,
                            health.last_refresh_outcome,
                            health.consecutive_refresh_failures,
                            health.access_expires_at,
                            readings[i].is_some(),
                            now_secs,
                        ),
                        // The bounded-blindness projection (issue #479): ONLY the active account can
                        // be in bounded blindness — it is the only one that self-exhausts while
                        // active and the only one the `last_good` anchor belongs to. Keyed off
                        // `accounts[active].last_reading.is_none()` (the true blind predicate the anchor +
                        // `note_blind_gate_eligibility` logic use, NOT the masked `readings` arg) and
                        // the retained anchor; `None` for every other account (and omitted from the
                        // wire there via `skip_serializing_if`).
                        blind_active: if active == Some(i) {
                            blind_active_view(
                                // Issue #619: the anchor plus the active's frozen per-window high-water
                                // mark, so the anchor arm degrades on the PLAUSIBLE pre-blind session (a
                                // stale-low reading no longer shows false-"OK" while the corrected gate
                                // is armed).
                                AnchorArmInputs {
                                    last_good: self.state.last_good,
                                    high_water: self.state.accounts[i].session_high_water,
                                },
                                self.state.accounts[i].last_reading.is_none(),
                                health.quarantined,
                                // Issue #582: a server `Retry-After` still holding the blind active
                                // off its poll degrades auto-protection too — read from the SAME
                                // shared predicate `blind_swap` decides on, so `status` never claims
                                // "auto-protection OK" during a #582 hold. `health` is this account's.
                                server_retry_after_holding(health, blind_at).is_some(),
                                // Issue #584: the active account's retained #539 velocity EMA + the BASE
                                // (un-jittered) session trigger feed the velocity-projection arm, so
                                // `status` also degrades a below-band anchor whose measured climb could
                                // reach the trigger inside the blind window (a burn the anchor arm, frozen
                                // below the band, cannot see). Report-only — no swap keys off this.
                                self.state.accounts[i].session_velocity,
                                self.session_ceiling_base,
                                blind_at,
                            )
                        } else {
                            None
                        },
                    }
                })
                .collect(),
            // The forward-looking next-swap candidate (issue #88), computed from the
            // same raw readings; sourced from a label only, so no token/email can
            // reach it (issue #15).
            next_swap: self.next_swap(active, readings),
            // The config `[refresh].enabled` (#105), carried to the client for the #138
            // advisory — the CONFIG value, so the advisory keys off what the operator set
            // (AC-2: "suppressed when [refresh] is enabled").
            refresh_enabled: self.refresh_enabled,
            // The snapshot's freshness stamp for the frozen wire contract (issue #164): the SAME
            // `now_secs` the #119 health rollup reads above, so one wall-clock read backs the
            // whole cycle and the client's live-vs-stale check agrees with the rollup's clock.
            generated_at: now_secs,
            // The daemon-level systemic refresh-health indicator (issue #378): `Some(n)` while a
            // systemic-failure episode is active (n consecutive all-account error sweeps), `None`
            // when healthy — surfaced by `status` so the mechanism-down state is visible without
            // waiting for an account to die. A COUNT only (#15).
            systemic_refresh: self.state.systemic_refresh.status(),
            // The daemon-level canonical-scrub rollup (issue #516): project the two edge-latched scrub
            // signals into the wire discriminant so `status` / the menubar (#469) can surface the
            // fleet-wide scrubbed lockout. GATE on `signaled_canonical_scrubbed` FIRST (the master "is
            // the canonical currently empty" signal), THEN refine by exhaustion — because the restore
            // path clears `signaled_canonical_scrubbed` (a live re-read) but NOT
            // `signaled_scrub_adopt_exhausted` (cleared only on churn-window age-out, inside the
            // scrubbed-gated `recover_scrubbed_canonical`), so `(scrubbed=false, exhausted=true)` is
            // reachable after a `claude /login` recovery. Checking exhaustion first would then FALSELY
            // report un-recoverable over a HEALED canonical. Recovery-EXHAUSTED (#467, the residual
            // #469 renders with the `claude /login` remedy) outranks merely-scrubbed-but-RECOVERING
            // (#464); both clear to `None` (healthy) once the canonical is observed live again.
            canonical_scrub: if self.state.signaled_canonical_scrubbed {
                if self.state.signaled_scrub_adopt_exhausted {
                    Some(CanonicalScrub::Exhausted)
                } else {
                    Some(CanonicalScrub::Recovering)
                }
            } else {
                None
            },
            // The daemon-level keychain-locked flag (issue #498): project the edge-latched
            // `signaled_keychain_locked` signal straight onto the wire so `status` / the menubar (#498)
            // can surface the fleet-wide unreadable-credential lockout (the login keychain is LOCKED, so
            // the shared item can't be READ at all — distinct from a readable-but-scrubbed canonical).
            // Read directly, mirroring `canonical_scrub` above: the latch is `true` for exactly the
            // duration of a lock episode — set in `locked_tick` before this snapshot is built, cleared
            // on the first readable cycle — so a direct read is a faithful "currently locked" indicator.
            keychain_locked: self.state.signaled_keychain_locked,
            // The daemon-level narrated preemptive-swap notice (issue #479): project the retained
            // `last_blind_preempt_swap` record onto the wire, but only while STILL-CURRENT (the swap's
            // target is still the active account) AND RECENT (within `BLIND_PREEMPT_NOTICE_SECS`) — both
            // decided here (`recent_blind_preempt_swap_view`), on the same monotonic `blind_at` the
            // #479 blind projection uses, so `render_status` stays a pure function of the wire (#169).
            recent_blind_preempt_swap: recent_blind_preempt_swap_view(
                self.state.last_blind_preempt_swap.as_ref(),
                active.map(|i| self.roster[i].label.as_str()),
                blind_at,
            ),
            // The daemon-level runtime landing-overshoot notice (issue #613): project the retained
            // `last_landing_overshoot` record onto the wire while still within the notice window,
            // decided here (`recent_landing_overshoot_view`) on the SAME monotonic `blind_at` the #479
            // blind projection uses, so `render_status` stays a pure function of the wire (#169).
            recent_landing_overshoot: recent_landing_overshoot_view(
                self.state.last_landing_overshoot.as_ref(),
                blind_at,
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::daemon::tests::*;

    use crate::observability::RefreshEventOutcome;

    // --- status snapshot + control protocol --------------------------------

    #[test]
    fn status_response_carries_handles_and_percentages_and_never_a_secret() {
        let snapshot = StatusSnapshot {
            systemic_refresh: None,
            canonical_scrub: None,
            keychain_locked: false,
            recent_blind_preempt_swap: None,
            // Issue #613: a runtime landing overshoot rides the wire as one operator handle + two
            // small percents — populated here so the secret-free assertion below covers its bytes.
            recent_landing_overshoot: Some(LandingOvershoot {
                from_label: "spare".to_owned(),
                decision_pct: 95,
                landing_pct: 99,
            }),
            generated_at: 0,
            refresh_enabled: false,
            accounts: vec![
                AccountReading {
                    label: "work".to_owned(),
                    active: true,
                    enabled: true,
                    quarantined: false,
                    recovering: false,
                    weekly_exhausted: false,
                    usage: Some(Usage {
                        session: 0.97,
                        weekly: 0.40,
                        weekly_resets_at: None,
                        session_resets_at: None,
                    }),
                    ..Default::default()
                },
                AccountReading {
                    label: "spare".to_owned(),
                    active: false,
                    enabled: true,
                    quarantined: false,
                    recovering: false,
                    weekly_exhausted: false,
                    usage: None,
                    ..Default::default()
                },
            ],
            // A viable candidate rides the wire as a label + the daemon's #393 selection reason (#88).
            next_swap: Some(NextSwap::Target {
                to: "spare".to_owned(),
                reason: Some(NextSwapReason::OnlyCandidate),
            }),
        };
        let json = serde_json::to_string(&status_response(&snapshot)).unwrap();
        assert!(json.contains("\"label\":\"work\""));
        assert!(json.contains("\"active\":true"));
        // Issue #36: the rotation flag is carried so `status` can mark a parked account.
        assert!(json.contains("\"enabled\":true"));
        assert!(json.contains("\"session_pct\":97"));
        assert!(json.contains("\"weekly_pct\":40"));
        // The unavailable account reports null, not a fabricated 0.
        assert!(json.contains("\"session_pct\":null"));
        // The next-swap candidate is projected as a label (#88); `last_swap` is gone.
        assert!(json.contains("\"next_swap\":"));
        assert!(json.contains("\"state\":\"target\""));
        assert!(json.contains("\"to\":\"spare\""));
        assert!(!json.contains("last_swap"));
        // Issue #613: the runtime landing-overshoot notice projects a handle + the two percents.
        assert!(json.contains("\"recent_landing_overshoot\":{"));
        assert!(json.contains("\"from_label\":\"spare\""));
        assert!(json.contains("\"decision_pct\":95"));
        assert!(json.contains("\"landing_pct\":99"));
        // Issue #15: the projection sources only labels + percentages, so neither an
        // email nor a token can ever reach the wire — the new candidate and overshoot included.
        assert!(crate::redaction::meter::unauthored_emails(&json, &[]).is_empty());
        assert!(!json.to_lowercase().contains("token"));
    }

    // --- credential-health rollup (issue #119) -----------------------------

    #[test]
    fn credential_health_rolls_up_the_states_by_severity() {
        use RefreshEventOutcome::{Error, NoChange, Refreshed};
        const NOW: i64 = 1_782_777_600;

        // Healthy — not quarantined, no refresh failure, token not yet expired; the refresh
        // telemetry + a future expiry are the positive-liveness signal (no reading needed).
        assert_eq!(
            credential_health(false, Some(Refreshed), 0, Some(NOW + 60), false, NOW),
            CredentialHealth::Healthy
        );
        // A fresh successful usage reading is ALSO a positive-liveness signal (#137): even
        // with `[refresh]` off (no telemetry, no clock), a live-API poll ⇒ Healthy. The
        // no-reading counterpart is `Unknown`, covered in the sibling test.
        assert_eq!(
            credential_health(false, None, 0, None, true, NOW),
            CredentialHealth::Healthy
        );

        // Stale — the access token has expired (`<= now`) but the refresh net is still
        // alive: a transient window the next refresh recovers. The boundary is inclusive.
        assert_eq!(
            credential_health(false, Some(NoChange), 0, Some(NOW), false, NOW),
            CredentialHealth::Stale
        );
        assert_eq!(
            credential_health(false, Some(Refreshed), 0, Some(NOW - 1), false, NOW),
            CredentialHealth::Stale
        );

        // AtRisk — the refresh safety-net is failing (a streak of errors), even while the
        // access token itself has not yet expired.
        assert_eq!(
            credential_health(false, Some(Error), 1, Some(NOW + 60), false, NOW),
            CredentialHealth::AtRisk
        );

        // Degraded — a bare quarantine (the #42 access-token 401-streak) is NON-TERMINAL
        // (issue #427): the refresh token is unproven, so `poke` / a restart revive it. It is
        // 🟠, NOT the terminal 🔴 `Dead` — the honesty fix that stops the false "claude /login".
        assert_eq!(
            credential_health(true, None, 0, None, false, NOW),
            CredentialHealth::Degraded
        );
        // Dead — reserved for PROVEN refresh-token death: a sweep-refresh actually returned
        // `Dead` (#261). Surfaced as 🔴 rather than hidden — this is a DISPLAY rollup, it never
        // quarantines. This is the ONLY 🔴 case (issue #427).
        assert_eq!(
            credential_health(
                false,
                Some(RefreshEventOutcome::Dead),
                0,
                Some(NOW + 60),
                false,
                NOW
            ),
            CredentialHealth::Dead
        );
        // Proven death WINS over a co-occurring quarantine: a quarantined account whose refresh
        // ALSO returned `Dead` is genuinely dead (needs re-login), so it reads 🔴 `Dead`, not
        // 🟠 `Degraded` — `Dead` is checked before `quarantined` (issue #427).
        assert_eq!(
            credential_health(true, Some(RefreshEventOutcome::Dead), 0, None, false, NOW),
            CredentialHealth::Dead
        );

        // Severity ladder (Dead > Degraded > AtRisk > Stale > Healthy): a quarantined account
        // whose token is ALSO expired and whose refresh is ALSO merely FAILING (an `Error`
        // streak, not a proven `Dead`) reads 🟠 `Degraded` — the quarantine outranks the
        // at-risk streak, and without a proven refresh death it is NOT terminal (issue #427).
        // An at-risk account whose token is ALSO expired reads AtRisk, not Stale. A fresh
        // reading NEVER masks a negative signal — even `has_fresh_reading = true` holds here.
        assert_eq!(
            credential_health(true, Some(Error), 3, Some(NOW - 10), true, NOW),
            CredentialHealth::Degraded
        );
        assert_eq!(
            credential_health(false, Some(Error), 2, Some(NOW - 10), true, NOW),
            CredentialHealth::AtRisk
        );
    }

    #[test]
    fn credential_health_reports_unknown_without_a_positive_liveness_signal() {
        use RefreshEventOutcome::NoChange;
        const NOW: i64 = 1_782_777_600;

        // #137: absence of a NEGATIVE signal is not health. A non-active account never
        // successfully polled, `[refresh]` off (no telemetry, no refresh-sourced expiry, no
        // fresh reading) ⇒ Unknown — NOT a false 🟢 that would jump straight to 🔴 the moment
        // the 401-streak quarantines it. This is the exact case that fell through to Healthy
        // before the fix.
        assert_eq!(
            credential_health(false, None, 0, None, false, NOW),
            CredentialHealth::Unknown
        );

        // Any ONE positive-liveness signal lifts it to Healthy:
        //  (a) a fresh successful usage reading (the strongest proof — a live-API poll),
        assert_eq!(
            credential_health(false, None, 0, None, true, NOW),
            CredentialHealth::Healthy
        );
        //  (b) refresh telemetry (the refresh path observed the account alive),
        assert_eq!(
            credential_health(false, Some(NoChange), 0, None, false, NOW),
            CredentialHealth::Healthy
        );
        //  (c) a FUTURE refresh-sourced expiry (the refresh engine read a valid token).
        assert_eq!(
            credential_health(false, None, 0, Some(NOW + 60), false, NOW),
            CredentialHealth::Healthy
        );

        // AC: a LAPSED refresh-sourced expiry (no telemetry, no reading) is a KNOWN stale
        // window the refresh net recovers — Stale wins over the no-evidence check, never
        // Unknown and never a false Healthy.
        assert_eq!(
            credential_health(false, None, 0, Some(NOW - 1), false, NOW),
            CredentialHealth::Stale
        );

        // A negative signal always overrides missing evidence — a bare quarantine ⇒ Degraded
        // (issue #427: NON-TERMINAL, needs a refresh not a re-login), never Unknown, even with
        // no other input.
        assert_eq!(
            credential_health(true, None, 0, None, false, NOW),
            CredentialHealth::Degraded
        );
    }

    #[test]
    fn credential_health_reserves_dead_for_proven_refresh_death_not_a_bare_quarantine() {
        // Issue #427 regression: locks the honesty trajectory 🟢 → 🟠 degraded → 🔴-only-on-proof
        // so a parked account that merely 401-streaked into quarantine can never again render the
        // terminal 🔴 / "claude /login" while its refresh token is still good.
        const NOW: i64 = 1_782_777_600;

        // Healthy — a positive-liveness signal (a fresh reading), refresh path untouched.
        assert_eq!(
            credential_health(false, None, 0, None, true, NOW),
            CredentialHealth::Healthy
        );

        // Degraded (NOT Dead) — the access token 401-streaked into quarantine, but no refresh has
        // returned `Dead`, so the refresh token is unproven and `poke` / a restart revive it. This
        // is the exact false-🔴 the issue fixes: a bare quarantine is 🟠 needs-refresh, never
        // 🔴 needs-re-login — regardless of whether the refresh net is merely failing (`Error`),
        // idle (`None`), or last succeeded (`NoChange`), and regardless of a stale/fresh clock.
        for refresh in [
            None,
            Some(RefreshEventOutcome::Error),
            Some(RefreshEventOutcome::NoChange),
            Some(RefreshEventOutcome::Refreshed),
        ] {
            assert_eq!(
                credential_health(true, refresh, 0, None, false, NOW),
                CredentialHealth::Degraded,
                "a bare quarantine (refresh={refresh:?}) is degraded, never dead"
            );
        }

        // Dead — ONLY once a sweep-refresh actually returns `Dead` (#261 / `CredentialUnrecoverable`):
        // the refresh token itself was rejected, so a re-login is genuinely required. Holds whether
        // or not the account is also quarantined — proven death is checked first and wins.
        assert_eq!(
            credential_health(false, Some(RefreshEventOutcome::Dead), 0, None, false, NOW),
            CredentialHealth::Dead
        );
        assert_eq!(
            credential_health(true, Some(RefreshEventOutcome::Dead), 0, None, false, NOW),
            CredentialHealth::Dead
        );
    }

    #[test]
    fn millis_to_secs_folds_a_known_expiry_at_the_ms_boundary() {
        // The blob's `expiresAt` is epoch MILLISECONDS; the wire and rollup are epoch SECONDS
        // (issue #141 must-carry — a missed fold misfires the operator clock by 1000×). A
        // known instant folds exactly; a sub-second remainder truncates (immaterial for a
        // token-lifetime clock) and matches the refresh fold's `ms / 1000`.
        assert_eq!(millis_to_secs(1_782_777_600_000), 1_782_777_600);
        assert_eq!(millis_to_secs(1_782_777_600_999), 1_782_777_600);
        assert_eq!(millis_to_secs(0), 0);
    }

    #[tokio::test]
    async fn poll_populates_the_display_expiry_clock_without_the_refresh_tick() {
        // Issue #141: with `[refresh]` OFF (no `RefreshObservation` ever folded — the refresh
        // engine, the field's only OTHER writer, is off by default), the poll path alone must
        // surface each polled account's access-token expiry on `status --json`, WITHOUT feeding
        // the naive `access_expires_at <= now → Stale` rollup branch — that would false-🟠 every
        // idle account whose stashed token has lapsed (the rollup's positive-liveness
        // consumption of the poll clock lands under #137).

        // A realistic CC credential: the SECRET token beside the non-secret `expiresAt` (ms).
        // The active account's CANONICAL item and the per-account STASH carry DIFFERENT
        // expiries, so the assertions prove the clock is sourced from the SAME credential the
        // poll used — canonical for the active account, the stash for any other.
        const TOKEN: &str = "sk-ant-oat-SECRET-must-not-leak";
        const CANON_MS: i64 = 1_782_777_600_000;
        const CANON_S: i64 = 1_782_777_600;
        const STASH_MS: i64 = 1_782_784_800_000;
        const STASH_S: i64 = 1_782_784_800;
        let blob = |expires_at_ms: i64| -> Vec<u8> {
            format!(
                r#"{{"claudeAiOauth":{{"accessToken":"{TOKEN}","expiresAt":{expires_at_ms}}}}}"#
            )
            .into_bytes()
        };

        let canon_blob = blob(CANON_MS);
        let stash_blob = blob(STASH_MS);
        let roster = vec![
            account("u-A", "work"),
            account("u-B", "spare"),
            account("u-C", "backup"),
        ];
        let store = store_holding(&canon_blob).await; // canonical = the active account's bearer
        let stash = stash_with(&[
            ("Sessiometer/u-A", &stash_blob, "u-A"),
            ("Sessiometer/u-B", &stash_blob, "u-B"),
            ("Sessiometer/u-C", &stash_blob, "u-C"),
        ])
        .await;
        let (dir, json) = claude_json("u-A");
        std::mem::forget(dir);
        let tun = tunables(95, 80, 0);
        let mut daemon = Daemon::new(
            roster,
            FakeRosterPoller::new()
                .ok("u-A", 0.11, 0.10)
                .ok("u-B", 0.22, 0.10)
                .ok("u-C", 0.33, 0.10),
            store,
            stash,
            FakeClock::frozen(),
            json,
            &tun,
        );

        // Tick 1 polls the ACTIVE account (u-A) → its expiry is read from the CANONICAL item…
        daemon.tick().await;
        assert_eq!(
            daemon.state.accounts[0].health.poll_expires_at,
            Some(CANON_S)
        );
        // …while the refresh-sourced field the rollup actually reads stays untouched: with
        // `[refresh]` off it is still `None`, so no lapsed poll clock can reach the Stale branch.
        assert_eq!(daemon.state.accounts[0].health.access_expires_at, None);

        // Tick 2 polls a NON-active account (u-B) → its expiry is read from that account's STASH.
        daemon.tick().await;
        assert_eq!(
            daemon.state.accounts[1].health.poll_expires_at,
            Some(STASH_S)
        );
        assert_eq!(daemon.state.accounts[1].health.access_expires_at, None);

        // Project the wire the control socket returns, with `now` set a day AFTER the polled
        // expiry — the exact lapsed-idle case. The clock IS populated (AC: non-null with
        // `[refresh]` off) yet the ACTIVE account stays Healthy, NOT a false-🟠 Stale: the
        // poll clock never reaches the Stale branch, and its own successful poll is the
        // positive-liveness signal keeping it Healthy rather than Unknown (#137).
        let readings = daemon.state.readings();
        let snapshot = daemon.snapshot(daemon.state.active, &readings, CANON_S + 86_400);
        assert_eq!(snapshot.accounts[0].access_expires_at, Some(CANON_S));
        assert_eq!(snapshot.accounts[0].health, CredentialHealth::Healthy);
        // The third account (u-C) was never polled this run — no reading, no telemetry, no
        // refresh clock — so #137 reports it ⚪ Unknown, NOT a false 🟢, even as #141's
        // display clock keeps working for the accounts that were polled.
        assert_eq!(snapshot.accounts[2].health, CredentialHealth::Unknown);

        // The clock reached the wire (non-vacuous), and the surrounding token never rode
        // alongside it into any output channel (issue #15 / #141 secret-handling).
        let corpus = serde_json::to_string(&status_response(&snapshot)).unwrap();
        assert!(corpus.contains(r#""access_expires_at":1782777600"#));
        assert!(!corpus.contains(TOKEN));
        // #137 AC: the raw Unknown state rides the `--json` wire as a scriptable token,
        // so a consumer can tell "unverified" apart from a genuine "healthy". The wire key
        // is `auth` (issue #143 renamed the field `health` → `auth`).
        assert!(corpus.contains(r#""auth":"unknown""#));
    }

    #[tokio::test]
    async fn note_health_transitions_seeds_silently_then_emits_one_event_per_change() {
        let mut daemon = three_account_daemon(FakeRosterPoller::new()).await;
        const NOW: i64 = 1_782_777_600;

        // The FIRST computation per account SEEDS the baseline WITHOUT emitting — a fresh
        // daemon logs no startup storm. All three start UNKNOWN (#137): not quarantined, no
        // clocks, and no successful poll yet (this daemon was never ticked) — no positive
        // liveness signal, so honestly unverified rather than a false 🟢.
        assert!(daemon.note_health_transitions(NOW).is_empty());
        assert_eq!(
            daemon.state.accounts[0].health.last_health,
            Some(CredentialHealth::Unknown)
        );

        // A genuine change emits EXACTLY ONE redacted event (AC-3) — the handle and the new
        // state — and only for the account that changed. A bare quarantine (an access-token
        // 401-streak) transitions to Degraded, NOT Dead (issue #427): the event log carries the
        // honest non-terminal verdict too, so a `grep` never cries a false death.
        daemon.state.accounts[0].health.quarantined = true; // → Degraded
        assert_eq!(
            daemon.note_health_transitions(NOW),
            vec![Event::CredentialHealth {
                account: "work".to_owned(),
                state: CredentialHealth::Degraded,
            }]
        );

        // No change ⇒ no event (edge-triggered, not level-triggered).
        assert!(daemon.note_health_transitions(NOW).is_empty());

        // Un-quarantine WITHOUT any new evidence ⇒ back to Unknown, NOT a false Healthy
        // (#137): clearing the quarantine flag does not prove the credential is alive.
        daemon.state.accounts[0].health.quarantined = false; // → Unknown (still no liveness signal)
        assert_eq!(
            daemon.note_health_transitions(NOW),
            vec![Event::CredentialHealth {
                account: "work".to_owned(),
                state: CredentialHealth::Unknown,
            }]
        );

        // Evidence ARRIVES — a successful poll for `work` (enabled, non-quarantined, so it
        // surfaces through `decision_readings`) ⇒ Unknown transitions to a real Healthy state.
        daemon.state.accounts[0].last_reading = Some(Usage {
            session: 0.10,
            weekly: 0.10,
            weekly_resets_at: None,
            session_resets_at: None,
        });
        assert_eq!(
            daemon.note_health_transitions(NOW),
            vec![Event::CredentialHealth {
                account: "work".to_owned(),
                state: CredentialHealth::Healthy,
            }]
        );
    }

    #[test]
    fn redaction_meter_covers_the_new_credential_clock_fields() {
        use crate::redaction::meter::{assert_clean, Secrets};
        // The full-loop meter test runs no sweep, so its corpus never carries a populated
        // `refresh_health` / `access_expires_at`. Exercise those new wire fields here with
        // non-default values — the expiry is the SAME instant embedded in the fixture blob's
        // `expiresAt`, so a path that leaked the surrounding token alongside the expiry would
        // surface it — and prove the value-based meter (#15) still reads clean.
        let secrets = Secrets::meter_fixture();
        let snapshot = StatusSnapshot {
            systemic_refresh: None,
            canonical_scrub: None,
            keychain_locked: false,
            recent_blind_preempt_swap: None,
            recent_landing_overshoot: None,
            generated_at: 0,
            refresh_enabled: false,
            accounts: vec![AccountReading {
                label: "work".to_owned(),
                active: true,
                enabled: true,
                access_expires_at: Some(1_782_777_600),
                refresh_health: Some(RefreshHealth {
                    last_ok: true,
                    rotated: true,
                    consecutive_failures: 0,
                }),
                health: CredentialHealth::Stale,
                ..Default::default()
            }],
            next_swap: None,
        };
        let response = status_response(&snapshot);
        let mut corpus = serde_json::to_string(&response).unwrap();
        corpus.push('\n');
        // The text surface too (it carries the 🟡 glyph for this Stale account).
        corpus.push_str(&crate::cli::render_status(
            &response,
            1_782_700_000,
            None,
            false,
        ));
        // …and the `status --verbose` access-token expiry block (issue #143), a third
        // operator-facing surface that reprojects the same `access_expires_at` clock — so a
        // path leaking the surrounding token alongside the expiry surfaces here too.
        corpus.push_str(&crate::cli::render_access_token_expiry(
            &response,
            1_782_700_000,
        ));

        // Cardinality (#15 non-vacuous gate): the new fields actually reached the scanned
        // corpus before the clean verdict below is trusted.
        assert!(corpus.contains(r#""access_expires_at":1782777600"#));
        assert!(corpus.contains(r#""refresh_health":{"#));
        // The rollup rides the wire under the `auth` key (issue #143 renamed `health` → `auth`).
        assert!(corpus.contains(r#""auth":"stale""#));
        assert_clean(&corpus, &secrets, &[]);
    }

    #[test]
    fn status_response_carries_the_refresh_enabled_flag_onto_the_wire() {
        // Issue #138: the daemon's live `[refresh].enabled` is wrapped `Some(..)` on the wire
        // (mirroring `health`) so the thin `status` client can gate its advisory off the daemon's
        // ACTUAL refresh state. A current daemon always sends a definite `Some(true/false)`; only
        // a pre-#138 daemon omits the field (→ the client decodes `None` and suppresses).
        for enabled in [true, false] {
            let snapshot = StatusSnapshot {
                refresh_enabled: enabled,
                ..Default::default()
            };
            assert_eq!(status_response(&snapshot).refresh_enabled, Some(enabled));
        }
    }

    #[tokio::test]
    async fn daemon_snapshot_reflects_with_refresh_enabled() {
        // Issue #138 daemon plumbing: `with_refresh_enabled` (fed `config.refresh.enabled` in the
        // run path) flows onto the display snapshot, so the client's advisory gate sees the
        // daemon's LIVE refresh state. Default (no builder) is the opt-in `false`; the builder
        // flips it. `snapshot` reads only the flag here, so all-`None` readings keep it minimal.
        let default_daemon = lifecycle_daemon().await;
        let readings = vec![None; default_daemon.roster.len()];
        let off = default_daemon.snapshot(Some(0), &readings, 0);
        assert!(!off.refresh_enabled, "the opt-in default carries tick-off");

        let on = lifecycle_daemon()
            .await
            .with_refresh_enabled(true)
            .snapshot(Some(0), &readings, 0);
        assert!(
            on.refresh_enabled,
            "with_refresh_enabled(true) flows to the display snapshot"
        );
    }

    #[tokio::test]
    async fn serve_control_answers_status_with_exactly_one_line() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let snapshot = StatusSnapshot {
            systemic_refresh: None,
            canonical_scrub: None,
            keychain_locked: false,
            recent_blind_preempt_swap: None,
            recent_landing_overshoot: None,
            generated_at: 0,
            refresh_enabled: false,
            accounts: vec![AccountReading {
                label: "work".to_owned(),
                active: true,
                enabled: true,
                quarantined: false,
                recovering: false,
                weekly_exhausted: false,
                usage: Some(Usage {
                    session: 0.50,
                    weekly: 0.25,
                    weekly_resets_at: None,
                    session_resets_at: None,
                }),
                ..Default::default()
            }],
            next_swap: None,
        };
        let (mut client, server) = tokio::io::duplex(1024);
        client.write_all(b"{\"cmd\":\"status\"}\n").await.unwrap();
        // `status` is a non-secret read — answered for any peer, and producing no
        // control signal (it never mutates daemon state).
        let signal = serve_control(server, &snapshot, false)
            .await
            .unwrap()
            .one_shot();
        assert!(signal.is_none(), "status must not produce a control signal");

        let mut reply = String::new();
        client.read_to_string(&mut reply).await.unwrap();
        assert_eq!(
            reply.lines().count(),
            1,
            "exactly one status line: {reply:?}"
        );
        assert!(reply.contains("\"label\":\"work\""));
        assert!(reply.contains("\"session_pct\":50"));
        assert!(crate::redaction::meter::unauthored_emails(&reply, &[]).is_empty());
    }

    #[tokio::test]
    async fn serve_control_rejects_an_unknown_command() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let (mut client, server) = tokio::io::duplex(1024);
        client.write_all(b"{\"cmd\":\"nope\"}\n").await.unwrap();
        let signal = serve_control(server, &StatusSnapshot::default(), true)
            .await
            .unwrap()
            .one_shot();
        assert!(signal.is_none(), "an unknown command produces no signal");

        let mut reply = String::new();
        client.read_to_string(&mut reply).await.unwrap();
        assert!(reply.contains("unknown command"), "got {reply:?}");
    }

    #[tokio::test]
    async fn serve_control_writes_the_ok_ack_and_yields_the_shutdown_signal() {
        // Issue #397: an authenticated same-user `shutdown` — the `daemon stop` control path for an
        // unmanaged daemon — is answered with `{"ok":true}` over the stream AND yields the
        // `ShutdownRequested` signal the run loop turns into a graceful `Idle::Shutdown`. The ack is
        // flushed HERE, before the signal ever reaches the run loop, so the client learns the stop
        // was accepted before the daemon goes away.
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let (mut client, server) = tokio::io::duplex(1024);
        client.write_all(b"{\"cmd\":\"shutdown\"}\n").await.unwrap();
        let signal = serve_control(server, &StatusSnapshot::default(), true)
            .await
            .unwrap()
            .one_shot();
        assert_eq!(
            signal,
            Some(ControlSignal::ShutdownRequested),
            "an authenticated shutdown yields the graceful-stop signal",
        );

        let mut reply = String::new();
        client.read_to_string(&mut reply).await.unwrap();
        assert_eq!(
            reply.trim_end(),
            r#"{"ok":true}"#,
            "the ok ack is written before the daemon exits: {reply:?}",
        );
    }

    #[tokio::test]
    async fn serve_control_honours_a_shutdown_whose_peer_hung_up_before_the_ack() {
        // Issue #397: the daemon accepts control connections only BETWEEN ticks, so a `daemon stop`
        // against a busy daemon can time out and close before the daemon ever reads the request.
        // When the daemon then answers, the ack write fails with `EPIPE`. Delivering the ack is
        // best-effort — the request was already read and authenticated, so the shutdown MUST still
        // take effect. Propagating the write error instead would discard the signal at
        // `UnixControl::serve`'s `Err(_) => Signal(None)` arm: the operator's `daemon stop` would
        // exit 1 AND the daemon would keep running.
        use tokio::io::AsyncWriteExt;

        let (mut client, server) = tokio::io::duplex(1024);
        client.write_all(b"{\"cmd\":\"shutdown\"}\n").await.unwrap();
        drop(client); // the client gave up waiting and hung up: the ack write will now fail

        let signal = serve_control(server, &StatusSnapshot::default(), true)
            .await
            .expect("a failed ack must not fail the exchange")
            .one_shot();
        assert_eq!(
            signal,
            Some(ControlSignal::ShutdownRequested),
            "an authenticated shutdown survives an undeliverable ack",
        );
    }

    #[tokio::test]
    async fn serve_control_refuses_a_shutdown_from_an_unauthenticated_peer() {
        // Issue #397: `shutdown` is state-affecting (it ends the process), so an UNauthenticated
        // peer is fail-closed with `{"error":"unauthorized"}` and produces NO signal — a stranger
        // can never stop the daemon (the same same-user gate `manual-swapped` #64 / `roster-reload`
        // #139 / `restored` #275 sit behind). Auth is the ONLY gate on this verb, so this is the
        // whole guard: the socket-layer half here, the real `getpeereid` euid comparison that
        // computes the bool in `serve_control_rejects_a_foreign_uid_peer` / `is_same_user`.
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let (mut client, server) = tokio::io::duplex(1024);
        client.write_all(b"{\"cmd\":\"shutdown\"}\n").await.unwrap();
        let signal = serve_control(server, &StatusSnapshot::default(), false)
            .await
            .unwrap()
            .one_shot();
        assert!(
            signal.is_none(),
            "an unauthorized shutdown produces no signal — the daemon keeps running",
        );

        let mut reply = String::new();
        client.read_to_string(&mut reply).await.unwrap();
        assert!(reply.contains("unauthorized"), "fail-closed: {reply:?}");
    }

    #[tokio::test]
    async fn serve_control_bounds_an_oversized_request_line() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        // Issue #64: the receive path must be BOUNDED. A peer that streams a line
        // longer than the cap — with NO newline and the connection held OPEN — would
        // hang an unbounded `read_line`; only the byte cap can end this read (EOF at
        // the limit), after which the over-long request is rejected as malformed.
        // The client never closes, so it is the cap (not an EOF) that ends the read;
        // a regressed cap is caught by the exchange timeout firing with no reply.
        let oversized = vec![b'{'; MAX_CONTROL_LINE_BYTES as usize + 1];
        let (mut client, server) = tokio::io::duplex(oversized.len() + 64);
        client.write_all(&oversized).await.unwrap();
        let signal = serve_control(server, &StatusSnapshot::default(), true)
            .await
            .unwrap()
            .one_shot();
        assert!(signal.is_none(), "an oversized request produces no signal");

        let mut reply = String::new();
        client.read_to_string(&mut reply).await.unwrap();
        assert!(
            reply.contains("malformed"),
            "an over-long request is bounded and rejected: {reply:?}"
        );
    }

    // ---- `watch` subscription (issue #165) ------------------------------------------------

    /// A one-account status snapshot for the `watch` tests, stamped with a chosen `generated_at`
    /// and session fraction so a test can tell one pushed snapshot from the next.
    fn watch_snapshot(label: &str, generated_at: i64, session: f64) -> StatusSnapshot {
        StatusSnapshot {
            systemic_refresh: None,
            canonical_scrub: None,
            keychain_locked: false,
            recent_blind_preempt_swap: None,
            recent_landing_overshoot: None,
            generated_at,
            refresh_enabled: false,
            next_swap: None,
            accounts: vec![AccountReading {
                label: label.to_owned(),
                active: true,
                enabled: true,
                usage: Some(Usage {
                    session,
                    weekly: 0.10,
                    weekly_resets_at: None,
                    session_resets_at: None,
                }),
                ..Default::default()
            }],
        }
    }

    /// Read exactly one newline-delimited frame line from a `watch` stream, asserting the framing.
    async fn read_frame<R: tokio::io::AsyncBufRead + Unpin>(reader: &mut R) -> String {
        use tokio::io::AsyncBufReadExt;
        let mut line = String::new();
        reader
            .read_line(&mut line)
            .await
            .expect("read a frame line");
        assert!(
            line.ends_with('\n'),
            "watch frames are newline-delimited: {line:?}"
        );
        line.trim_end().to_owned()
    }

    #[tokio::test]
    async fn serve_watch_streams_the_initial_snapshot_then_one_per_update() {
        // The daemon side of the latest-snapshot channel, seeded with the first snapshot.
        let (tx, rx) = tokio::sync::watch::channel(versioned_status_response(&watch_snapshot(
            "work", 100, 0.20,
        )));
        let (client, server) = tokio::io::duplex(4096);
        // A far-off heartbeat keeps liveness beats out of this update-focused test.
        let watcher = tokio::spawn(serve_watch(server, rx, Duration::from_secs(3600)));

        let mut reader = tokio::io::BufReader::new(client);
        // 1) The initial full snapshot arrives immediately on connect.
        let initial = read_frame(&mut reader).await;
        match parse_watch_frame(&initial).unwrap() {
            WatchFrame::Snapshot(v) => {
                assert_eq!(v.generated_at, 100);
                assert_eq!(v.status.accounts[0].label, "work");
                assert_eq!(v.status.accounts[0].session_pct, Some(20));
                assert!(
                    crate::redaction::meter::unauthored_emails(&initial, &[]).is_empty(),
                    "no non-authored email can travel (#15/#444)"
                );
            }
            other => panic!("expected an initial snapshot, got {other:?}"),
        }
        // 2) A published state change streams the WHOLE new snapshot (never a delta).
        tx.send_replace(versioned_status_response(&watch_snapshot(
            "work", 200, 0.55,
        )));
        let update = read_frame(&mut reader).await;
        match parse_watch_frame(&update).unwrap() {
            WatchFrame::Snapshot(v) => {
                assert_eq!(v.generated_at, 200);
                assert_eq!(v.status.accounts[0].session_pct, Some(55));
            }
            other => panic!("expected an update snapshot, got {other:?}"),
        }
        drop(reader); // the client goes away → the stream ends cleanly
        watcher.await.unwrap().unwrap();
    }

    #[tokio::test(start_paused = true)]
    async fn serve_watch_beats_during_silence() {
        // No state change ever occurs, so the ONLY frames after the initial snapshot are beats.
        let (_tx, rx) = tokio::sync::watch::channel(versioned_status_response(&watch_snapshot(
            "work", 500, 0.10,
        )));
        let (client, server) = tokio::io::duplex(4096);
        let heartbeat = Duration::from_secs(15);
        let watcher = tokio::spawn(serve_watch(server, rx, heartbeat));

        let mut reader = tokio::io::BufReader::new(client);
        let initial = read_frame(&mut reader).await;
        assert!(matches!(
            parse_watch_frame(&initial).unwrap(),
            WatchFrame::Snapshot(_)
        ));
        // After one interval of SILENCE, a heartbeat fires (bounding a client's stale detection).
        tokio::time::advance(heartbeat + Duration::from_millis(1)).await;
        let beat = read_frame(&mut reader).await;
        match parse_watch_frame(&beat).unwrap() {
            WatchFrame::Heartbeat {
                generated_at,
                schema_version,
            } => {
                assert_eq!(
                    generated_at, 500,
                    "the beat carries the last-known freshness"
                );
                assert_eq!(schema_version, STATUS_SCHEMA_VERSION);
            }
            other => panic!("expected a heartbeat during silence, got {other:?}"),
        }
        drop(reader);
        watcher.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn serve_watch_ends_when_the_client_disconnects() {
        // Issue #165 AC-2 (server side): a dropped subscriber must not leak the streaming task.
        let (_tx, rx) =
            tokio::sync::watch::channel(versioned_status_response(&watch_snapshot("work", 1, 0.0)));
        let (client, server) = tokio::io::duplex(4096);
        let watcher = tokio::spawn(serve_watch(server, rx, Duration::from_secs(3600)));

        let mut reader = tokio::io::BufReader::new(client);
        // The stream is live: the initial snapshot arrived.
        let initial = read_frame(&mut reader).await;
        assert!(matches!(
            parse_watch_frame(&initial).unwrap(),
            WatchFrame::Snapshot(_)
        ));
        // The subscriber goes away → the daemon detects it (read EOF) and ends the stream. Ending
        // via EOF returns `Ok`; a race that ends via a broken write returns `Err` — both are a
        // clean end (the property under test), never a hang, so the timeout is the real assertion.
        drop(reader);
        let ended = tokio::time::timeout(Duration::from_secs(5), watcher).await;
        let joined = ended.expect("serve_watch must end promptly when the client disconnects");
        joined.expect("the watch task must not panic").ok();
    }

    #[tokio::test]
    async fn a_watch_client_detects_a_dropped_daemon_via_socket_close() {
        use tokio::io::AsyncReadExt;
        // Issue #165 AC-2 (client side): a client can tell "disconnected" from a frozen view.
        let (tx, rx) = tokio::sync::watch::channel(versioned_status_response(&watch_snapshot(
            "work", 7, 0.30,
        )));
        let (client, server) = tokio::io::duplex(4096);
        let watcher = tokio::spawn(serve_watch(server, rx, Duration::from_secs(3600)));

        let mut reader = tokio::io::BufReader::new(client);
        // The client reads its initial snapshot — the stream is live.
        let initial = read_frame(&mut reader).await;
        assert!(matches!(
            parse_watch_frame(&initial).unwrap(),
            WatchFrame::Snapshot(_)
        ));
        // The daemon goes away: dropping the publisher ends `serve_watch`, which closes its end of
        // the socket when the task finishes.
        drop(tx);
        let _ = watcher.await.unwrap();
        // Client-side: the next read returns EOF (0 bytes) — a detectable "disconnected / stale"
        // signal rather than a frozen view.
        let mut rest = Vec::new();
        let n = reader.read_to_end(&mut rest).await.unwrap();
        assert_eq!(
            n, 0,
            "a dropped daemon is detectable client-side as socket EOF"
        );
    }

    #[tokio::test]
    async fn serve_control_routes_a_watch_command_to_a_stream() {
        use tokio::io::AsyncWriteExt;
        // A `watch` command is NOT answered with a one-shot reply — it hands the connection back
        // for the caller to stream on, keeping the long-lived stream off the idle select.
        let (mut client, server) = tokio::io::duplex(1024);
        client.write_all(b"{\"cmd\":\"watch\"}\n").await.unwrap();
        match serve_control(server, &StatusSnapshot::default(), false)
            .await
            .unwrap()
        {
            ServeOutcome::Watch(_stream) => {}
            ServeOutcome::OneShot(_) => {
                panic!("a watch command must route to a stream, not a one-shot reply")
            }
            ServeOutcome::Swap(..) => {
                panic!("a watch command must route to a watch stream, not a swap handoff")
            }
            ServeOutcome::Capture(..) => {
                panic!("a watch command must route to a watch stream, not a capture handoff")
            }
            ServeOutcome::Stats(..) => {
                panic!("a watch command must route to a watch stream, not a stats handoff")
            }
            ServeOutcome::ConfigGet(_) => {
                panic!("a watch command must route to a watch stream, not a config-get handoff")
            }
            ServeOutcome::ConfigSet(..) => {
                panic!("a watch command must route to a watch stream, not a config-set handoff")
            }
        }
    }

    #[tokio::test]
    async fn serve_control_routes_a_stats_command_to_a_handoff_unauthenticated() {
        use tokio::io::AsyncWriteExt;
        // A `stats` command is a non-secret READ (issue #356): like `watch`, it is UN-auth-gated
        // (peer `false`) and NOT answered inline — it hands the connection back so the caller
        // computes the series in a SPAWNED task, off the run loop. The `period` rides the handoff to
        // the task verbatim.
        let (mut client, server) = tokio::io::duplex(1024);
        client
            .write_all(b"{\"cmd\":\"stats\",\"period\":\"week\"}\n")
            .await
            .unwrap();
        let (_stream, request) = serve_control(server, &StatusSnapshot::default(), false)
            .await
            .unwrap()
            .stats();
        assert_eq!(
            request.period.as_deref(),
            Some("week"),
            "the stats period rides the handoff to the spawned task verbatim"
        );
    }

    #[tokio::test]
    async fn serve_control_routes_a_periodless_stats_command() {
        use tokio::io::AsyncWriteExt;
        // A `stats` command with no `period` is well-formed — the task defaults it to `week` (the
        // 7-day daily-bucket window), so the handoff carries `None` and there is no inline rejection.
        let (mut client, server) = tokio::io::duplex(1024);
        client.write_all(b"{\"cmd\":\"stats\"}\n").await.unwrap();
        let (_stream, request) = serve_control(server, &StatusSnapshot::default(), true)
            .await
            .unwrap()
            .stats();
        assert!(
            request.period.is_none(),
            "a periodless stats request hands off None (defaulted to week in the task)"
        );
    }

    #[tokio::test]
    async fn serve_stats_writes_exactly_one_json_line() {
        use tokio::io::AsyncBufReadExt;
        // End-to-end of the spawned answer path (issue #356): `serve_stats` computes off the runtime
        // thread (`spawn_blocking`) and writes ONE newline-delimited JSON reply, then closes. The
        // store state is environment-dependent (a real store, or none → an `{"error":…}` envelope),
        // so assert the request/response FRAMING (AC3), not the payload: a single line that parses as
        // one JSON object, then EOF. Exercises the `spawn_blocking` + `write_line` + timeout glue.
        let (client, server) = tokio::io::duplex(64 * 1024);
        serve_stats(
            server,
            StatsRequest {
                period: Some("week".to_owned()),
            },
        )
        .await
        .unwrap();
        let mut reader = tokio::io::BufReader::new(client);
        let mut line = String::new();
        reader.read_line(&mut line).await.unwrap();
        assert!(line.ends_with('\n'), "the stats reply is newline-delimited");
        let value: serde_json::Value =
            serde_json::from_str(line.trim_end()).expect("the stats reply is one JSON object");
        assert!(
            value.is_object(),
            "the reply is a JSON object — a StatsWire or an {{\"error\":…}} envelope"
        );
        let mut extra = String::new();
        let n = reader.read_line(&mut extra).await.unwrap();
        assert_eq!(n, 0, "stats is a one-shot reply, not a stream");
    }

    #[tokio::test]
    async fn unix_control_streams_a_watch_subscription_over_a_real_socket() {
        use tokio::io::AsyncWriteExt;
        // The production path end-to-end: a real `0600` socket, `UnixControl::serve` accepting a
        // `watch` request and SPAWNING the streaming task, and `publish` fanning a state change to
        // it — the wiring the duplex-level `serve_watch` tests above cannot reach.
        let dir = tempfile::tempdir().expect("tempdir");
        let sock = dir.path().join("daemon.sock");
        let listener = tokio::net::UnixListener::bind(&sock).expect("bind");
        let control = UnixControl::new(listener);

        // A client opens the dedicated read-only connection and subscribes.
        let mut client = tokio::net::UnixStream::connect(&sock)
            .await
            .expect("connect");
        client.write_all(b"{\"cmd\":\"watch\"}\n").await.unwrap();
        client.flush().await.unwrap();

        // Publish the current snapshot, THEN accept: the spawned task subscribes at this value and
        // sends it as the initial frame — the same publish-then-serve order the run loop uses.
        control.publish(&watch_snapshot("work", 100, 0.20));
        let yielded = control.serve(&watch_snapshot("work", 100, 0.20)).await;
        assert!(
            matches!(yielded, ControlYield::Signal(None)),
            "a watch subscription produces no control signal"
        );

        let mut reader = tokio::io::BufReader::new(client);
        let initial = read_frame(&mut reader).await;
        match parse_watch_frame(&initial).unwrap() {
            WatchFrame::Snapshot(v) => assert_eq!(v.generated_at, 100),
            other => panic!("expected an initial snapshot, got {other:?}"),
        }
        // A subsequent state change is pushed to the live subscription.
        control.publish(&watch_snapshot("work", 200, 0.55));
        let update = read_frame(&mut reader).await;
        match parse_watch_frame(&update).unwrap() {
            WatchFrame::Snapshot(v) => assert_eq!(v.generated_at, 200),
            other => panic!("expected an update snapshot, got {other:?}"),
        }
    }

    #[test]
    fn parse_watch_frame_classifies_each_frame_kind() {
        // A snapshot line round-trips to the frozen #164 envelope (the `type` tag is ignored by the
        // payload decode, so a snapshot frame carries the full contract a client already knows).
        let snap = encode_snapshot_frame(&versioned_status_response(&watch_snapshot(
            "work", 42, 0.60,
        )));
        match parse_watch_frame(&snap).unwrap() {
            WatchFrame::Snapshot(v) => {
                assert_eq!(v.generated_at, 42);
                assert_eq!(v.schema_version, STATUS_SCHEMA_VERSION);
                assert_eq!(v.status.accounts[0].session_pct, Some(60));
            }
            other => panic!("expected Snapshot, got {other:?}"),
        }
        // A heartbeat line round-trips to its freshness envelope.
        match parse_watch_frame(&encode_heartbeat_frame(42)).unwrap() {
            WatchFrame::Heartbeat {
                generated_at,
                schema_version,
            } => {
                assert_eq!(generated_at, 42);
                assert_eq!(schema_version, STATUS_SCHEMA_VERSION);
            }
            other => panic!("expected Heartbeat, got {other:?}"),
        }
        // An unknown (future) frame kind — or a line with no `type` tag — is IGNORED, not an error:
        // a forward-compatible client skips what it does not understand (the #164 additive ethos).
        assert!(matches!(
            parse_watch_frame(r#"{"type":"future","x":1}"#).unwrap(),
            WatchFrame::Unknown
        ));
        assert!(matches!(
            parse_watch_frame(r#"{"nope":1}"#).unwrap(),
            WatchFrame::Unknown
        ));
        // A malformed line is a hard error.
        assert!(parse_watch_frame("not json").is_err());
    }

    // --- Cross-language wire golden fixtures (issue #340) ----------------------------------
    //
    // The byte-frozen goldens the Swift menubar wire fixtures (`apps/menubar/Tests/Fixtures.swift`)
    // are pinned against. #322 hand-mirrored the daemon's frozen #164 wire contract into Swift
    // `Codable` types + byte-exact fixtures, but nothing caught a FUTURE daemon wire change silently
    // diverging from that hand-written mirror — ADR-0010 keeps Rust out of the Swift build, so the
    // Swift-only suite validates against its OWN now-stale fixtures and stays green. These goldens
    // close that gap: the daemon serializes its own wire encoders here (the single source of truth),
    // the byte-equality pin test below asserts the committed bytes still match (so a wire change
    // can't land without regenerating them), and CI asserts the Swift fixtures are byte-identical to
    // the same bytes (`apps/menubar/Tests/WireGoldenTests.swift`) — forcing the Swift mirror to move
    // in lockstep with any daemon wire change.
    //
    // Unlike the `src/migration.rs` golden (non-deterministic AEAD salt/nonce → a one-time
    // `#[ignore]` emitter, read-only thereafter), wire serialization is DETERMINISTIC, so the pin
    // test re-emits in-process and asserts byte-equality directly — a stronger gate than a frozen
    // read-only capture.

    /// The canonical snapshot frame the golden freezes: `encode_snapshot_frame` for
    /// `watch_snapshot("work", 42, 0.60)` — the SAME input
    /// [`parse_watch_frame_classifies_each_frame_kind`] decodes, so the golden and that test can
    /// never disagree on the representative healthy frame. Mirrored by Swift `Fixtures.snapshotBasic`.
    fn wire_golden_snapshot_frame() -> String {
        encode_snapshot_frame(&versioned_status_response(&watch_snapshot(
            "work", 42, 0.60,
        )))
    }

    /// The canonical heartbeat frame the golden freezes: `encode_heartbeat_frame(42)` — mirrored by
    /// Swift `Fixtures.heartbeatBasic`.
    fn wire_golden_heartbeat_frame() -> String {
        encode_heartbeat_frame(42)
    }

    /// A snapshot frame whose `next_swap` carries the #393 structured reason — the basic golden's
    /// `next_swap` is `null`, so the [`NextSwap::Target`] `reason` field (the whole point of #393)
    /// had NO byte-drift coverage. This freezes the `{"state":"target","to":…,"reason":{"kind":
    /// "soonest_reset","resets_at":…}}` shape, so the cross-language guard now fails if the Rust
    /// reason encoder and the Swift mirror ever diverge. Built as the basic frame with an overridden
    /// `next_swap`, so it differs from `wire_golden_snapshot_frame` in exactly that one field.
    /// Mirrored by Swift `Fixtures.snapshotNextSwap`.
    fn wire_golden_snapshot_next_swap_frame() -> String {
        let mut snapshot = watch_snapshot("work", 42, 0.60);
        snapshot.next_swap = Some(NextSwap::Target {
            to: "spare".to_owned(),
            reason: Some(NextSwapReason::SoonestReset {
                resets_at: 1_893_800_000,
            }),
        });
        encode_snapshot_frame(&versioned_status_response(&snapshot))
    }

    /// A snapshot frame whose `canonical_scrub` carries the #516 daemon-level rollup — the basic
    /// golden OMITS the field entirely (a healthy snapshot, `skip_serializing_if`), so the
    /// [`CanonicalScrub`] discriminant (the whole point of #516) has NO byte-drift coverage without
    /// this. Freezes the `{…,"canonical_scrub":{"state":"exhausted"}}` shape (the residual
    /// un-recoverable state #469 renders with the `claude /login` remedy), so the cross-language guard
    /// fails if the Rust rollup encoder and the Swift mirror ever diverge. Built as the basic frame
    /// with the scrub field set, so it differs from `wire_golden_snapshot_frame` in exactly that one
    /// ADDED key. Mirrored by Swift `Fixtures.snapshotCanonicalScrubExhausted`.
    fn wire_golden_snapshot_canonical_scrub_frame() -> String {
        let mut snapshot = watch_snapshot("work", 42, 0.60);
        snapshot.canonical_scrub = Some(CanonicalScrub::Exhausted);
        encode_snapshot_frame(&versioned_status_response(&snapshot))
    }

    /// One-time emitter for the committed wire goldens. `#[ignore]` — NOT part of the suite; it
    /// WRITES the bytes the pin test and the Swift fixtures consume. Run it ONLY alongside a
    /// deliberate wire-contract change:
    ///   `cargo test -- --ignored emit_wire_golden_fixtures`
    /// then update the Swift mirror (`apps/menubar/Sources/WireModel.swift`) and fixtures
    /// (`apps/menubar/Tests/Fixtures.swift`) so the cross-language byte-equality holds again.
    #[test]
    #[ignore = "one-time wire-golden emitter — run ONLY alongside a deliberate wire-contract change"]
    fn emit_wire_golden_fixtures() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("build/fixtures");
        std::fs::create_dir_all(&dir).expect("create build/fixtures");
        std::fs::write(
            dir.join("wire-snapshot-basic.json"),
            wire_golden_snapshot_frame(),
        )
        .expect("write wire-snapshot golden");
        std::fs::write(
            dir.join("wire-heartbeat-basic.json"),
            wire_golden_heartbeat_frame(),
        )
        .expect("write wire-heartbeat golden");
        std::fs::write(
            dir.join("wire-snapshot-next-swap.json"),
            wire_golden_snapshot_next_swap_frame(),
        )
        .expect("write wire-snapshot-next-swap golden");
        std::fs::write(
            dir.join("wire-snapshot-canonical-scrub.json"),
            wire_golden_snapshot_canonical_scrub_frame(),
        )
        .expect("write wire-snapshot-canonical-scrub golden");
    }

    /// The committed snapshot-frame golden — the exact bytes Swift `Fixtures.snapshotBasic` is
    /// pinned to. `include_str!` makes the file a compile-time input, so it must exist before this
    /// module compiles (emit once via [`emit_wire_golden_fixtures`]).
    const WIRE_SNAPSHOT_GOLDEN: &str = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/build/fixtures/wire-snapshot-basic.json"
    ));

    /// The committed heartbeat-frame golden — the exact bytes Swift `Fixtures.heartbeatBasic` is
    /// pinned to.
    const WIRE_HEARTBEAT_GOLDEN: &str = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/build/fixtures/wire-heartbeat-basic.json"
    ));

    /// The committed next-swap-reason snapshot golden (issue #393) — the exact bytes Swift
    /// `Fixtures.snapshotNextSwap` is pinned to.
    const WIRE_SNAPSHOT_NEXT_SWAP_GOLDEN: &str = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/build/fixtures/wire-snapshot-next-swap.json"
    ));

    /// The committed canonical-scrub snapshot golden (issue #516) — the exact bytes Swift
    /// `Fixtures.snapshotCanonicalScrubExhausted` is pinned to.
    const WIRE_SNAPSHOT_CANONICAL_SCRUB_GOLDEN: &str = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/build/fixtures/wire-snapshot-canonical-scrub.json"
    ));

    #[test]
    fn the_committed_wire_goldens_still_match_the_daemon_encoders() {
        // The load-bearing gate. Wire serialization is deterministic, so re-emitting in-process and
        // comparing to the COMMITTED bytes catches any daemon wire-type change — a renamed / added /
        // reordered / re-typed field, a changed enum tag, a `STATUS_SCHEMA_VERSION` bump — that
        // shifts the bytes: the committed golden goes stale and this fails, forcing a regenerate
        // (`emit_wire_golden_fixtures`) that in turn breaks the Swift byte-equality check until the
        // hand-written Swift mirror is updated too. That is the cross-language lockstep #340 exists
        // to enforce — no same-language test can witness a divergence between the Rust wire types
        // and the independently-maintained Swift mirror.
        assert_eq!(
            wire_golden_snapshot_frame(),
            WIRE_SNAPSHOT_GOLDEN,
            "the committed wire-snapshot golden drifted from encode_snapshot_frame — re-run \
             `cargo test -- --ignored emit_wire_golden_fixtures`, then update the Swift mirror \
             (apps/menubar) so its fixtures stay byte-identical"
        );
        assert_eq!(
            wire_golden_heartbeat_frame(),
            WIRE_HEARTBEAT_GOLDEN,
            "the committed wire-heartbeat golden drifted from encode_heartbeat_frame — re-run \
             `cargo test -- --ignored emit_wire_golden_fixtures`, then update the Swift mirror \
             (apps/menubar) so its fixtures stay byte-identical"
        );
        assert_eq!(
            wire_golden_snapshot_next_swap_frame(),
            WIRE_SNAPSHOT_NEXT_SWAP_GOLDEN,
            "the committed wire-snapshot-next-swap golden drifted from the next_swap reason encoder \
             (issue #393) — re-run `cargo test -- --ignored emit_wire_golden_fixtures`, then update \
             the Swift mirror (apps/menubar) so its fixtures stay byte-identical"
        );
        assert_eq!(
            wire_golden_snapshot_canonical_scrub_frame(),
            WIRE_SNAPSHOT_CANONICAL_SCRUB_GOLDEN,
            "the committed wire-snapshot-canonical-scrub golden drifted from the canonical_scrub \
             rollup encoder (issue #516) — re-run `cargo test -- --ignored emit_wire_golden_fixtures`, \
             then update the Swift mirror (apps/menubar) so its fixtures stay byte-identical"
        );
    }

    #[test]
    fn snapshot_frame_encodes_recent_blind_preempt_swap_only_when_present() {
        // Issue #479 (surface 2): the ADDITIVE `recent_blind_preempt_swap` wire field (schema 1.7).
        // The 4 committed goldens all carry it as `None` (it is
        // `#[serde(default, skip_serializing_if = "Option::is_none")]`), so THEY prove the omit-when-
        // absent half — an older client and every unaffected frame stay byte-identical — but NONE of
        // them exercise the POPULATED shape. This locks the wire bytes the field emits when a recent
        // preemptive swap IS present, so a later `#[serde(rename)]` / reorder / retype of
        // `BlindPreemptSwap`'s fields (which the CLI render test would NOT catch — it constructs the
        // struct in Rust, never round-trips the JSON keys) drifts this test. The menubar mirror is out
        // of scope for #479 (#169/#485 own it), so this stays a Rust-side byte-lock rather than a fifth
        // cross-language golden; the Swift decoder's forward-compatible tolerance of the new unknown
        // top-level key is already covered by the `future_top` fixture (apps/menubar Fixtures.swift).
        let mut snapshot = watch_snapshot("work", 42, 0.60);
        snapshot.recent_blind_preempt_swap = Some(BlindPreemptSwap {
            from_label: "work".to_owned(),
            to_label: "spare".to_owned(),
            last_known_session_pct: 68,
        });
        let frame = encode_snapshot_frame(&versioned_status_response(&snapshot));
        assert!(
            frame.contains(
                r#""recent_blind_preempt_swap":{"from_label":"work","to_label":"spare","last_known_session_pct":68}"#
            ),
            "the populated preemptive-swap narration serializes its exact wire shape: {frame}"
        );

        // Absent → the key is omitted entirely (the additive-minor discipline the goldens rely on).
        let healthy = watch_snapshot("work", 42, 0.60);
        let frame = encode_snapshot_frame(&versioned_status_response(&healthy));
        assert!(
            !frame.contains("recent_blind_preempt_swap"),
            "the field is omitted when there is no recent preemptive swap: {frame}"
        );
    }

    #[test]
    fn control_reply_rejects_malformed_json() {
        let (reply, signal) = control_reply("not json", &StatusSnapshot::default(), true);
        assert!(reply.contains("malformed"));
        assert!(signal.is_none());
        // issue #628: serde's decode message is threaded into `detail` rather than discarded, so a
        // client learns WHY the line was rejected instead of reading a content-free bare envelope.
        assert!(
            reply.contains("\"detail\":"),
            "a decode failure carries serde's message as detail: {reply:?}"
        );
    }

    #[test]
    fn manual_swapped_is_honored_only_for_an_authenticated_peer() {
        // Issue #64: `manual-swapped` is state-affecting, so an UNauthenticated peer
        // gets an error and produces NO signal — a stranger can never arm the
        // daemon's cooldown. The same-user peer gets an ack and the adopt signal.
        let snap = StatusSnapshot::default();
        let (denied, no_signal) = control_reply(r#"{"cmd":"manual-swapped"}"#, &snap, false);
        assert!(denied.contains("unauthorized"), "got {denied:?}");
        assert!(
            no_signal.is_none(),
            "an unauthenticated peer must not arm cooldown"
        );

        let (ack, signal) = control_reply(r#"{"cmd":"manual-swapped"}"#, &snap, true);
        assert!(ack.contains("\"ok\":true"), "got {ack:?}");
        assert_eq!(signal, Some(ControlSignal::ManualSwapped));
    }

    #[tokio::test]
    async fn peer_is_same_user_authenticates_a_same_process_peer() {
        // Issue #64: the manual-hold receive path authenticates the peer's uid via
        // `getpeereid(2)` before honoring a state-affecting command. A socket pair
        // made in THIS process has its peer on our own uid, so the real (unsafe) FFI
        // path must report it authenticated — exercising the `getpeereid`/`getuid`
        // computation that the boolean-gated `control_reply` tests take as a given.
        let (ours, _peer) = tokio::net::UnixStream::pair().expect("socketpair");
        assert!(
            peer_is_same_user(&ours),
            "a same-process socket peer is the same local user"
        );
    }

    #[test]
    fn is_same_user_denies_foreign_and_unreadable_credentials() {
        // Issue #196: the pure peer-auth decision, exercised on all three branches so a
        // silent auth-inverting refactor cannot ship green. Fixed uids (no syscall) —
        // the real `getpeereid` path is covered by the socket tests around this one.
        let owner: libc::uid_t = 1_000;
        // Same user → authenticated.
        assert!(
            is_same_user(Some(owner), owner),
            "the socket owner is the same local user"
        );
        // A foreign (non-owner) uid → rejected. Inverting `==`→`!=` would ALLOW this.
        assert!(
            !is_same_user(Some(owner + 1), owner),
            "a foreign uid is not the same local user"
        );
        // Unreadable credential (a `getpeereid` error) → fail closed. Both a fail-OPEN
        // regression (treating `None` as allow) and inverting the comparison ALLOW this.
        assert!(
            !is_same_user(None, owner),
            "an unreadable peer credential must fail closed"
        );
    }

    #[test]
    fn peer_euid_fails_closed_when_getpeereid_errors() {
        use std::os::unix::io::AsRawFd;
        // Issue #196: the fail-closed ERROR branch, driven for real. `getpeereid` on a
        // non-socket fd returns `ENOTSOCK` — the syscall itself errors — so `peer_euid`
        // must yield `None` and the decision must then DENY. A fail-open regression that
        // surfaced a default uid on error (e.g. the pre-`Option` `euid` left at 0) would
        // return `Some(_)` here. A regular file's fd is a real, valid fd that is simply
        // not a socket, so this exercises the `rc != 0` branch portably.
        let file = tempfile::tempfile().expect("tempfile");
        let euid = peer_euid(file.as_raw_fd());
        assert_eq!(
            euid, None,
            "getpeereid on a non-socket fd must fail (no credential)"
        );
        // SAFETY: `getuid` cannot fail and has no preconditions.
        assert!(
            !is_same_user(euid, unsafe { libc::getuid() }),
            "a getpeereid error must deny — fail closed"
        );
    }

    #[tokio::test]
    async fn serve_control_rejects_a_foreign_uid_peer() {
        use std::os::unix::io::AsRawFd;
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        // Issue #196: drive a REAL socket peer through the serve path and assert the
        // state-affecting `manual-swapped` is REJECTED when the peer is not the socket
        // owner. A genuinely foreign-uid peer cannot be spawned without root, so "foreign"
        // is realized faithfully: the peer's uid is read for real via `getpeereid`, then
        // compared against an owner uid deliberately NOT it. The real credential read and
        // the real serve exchange are exercised; only the owner identity is synthesized.
        let (mut client, server) = tokio::net::UnixStream::pair().expect("socketpair");

        // Real getpeereid read of the connected peer — its true euid (our own).
        let peer_uid =
            peer_euid(server.as_raw_fd()).expect("a connected peer has a readable credential");
        // A socket owner that is NOT the peer → the peer is foreign to this socket.
        let authenticated = is_same_user(Some(peer_uid), peer_uid.wrapping_add(1));
        assert!(
            !authenticated,
            "a foreign uid must not authenticate (guards against an inverted decision)"
        );

        client
            .write_all(b"{\"cmd\":\"manual-swapped\"}\n")
            .await
            .expect("write request");
        let signal = serve_control(server, &StatusSnapshot::default(), authenticated)
            .await
            .expect("serve")
            .one_shot();
        assert!(
            signal.is_none(),
            "a foreign-uid peer must NOT arm the daemon (no control signal)"
        );

        let mut reply = String::new();
        client.read_to_string(&mut reply).await.expect("read reply");
        assert!(
            reply.contains("unauthorized"),
            "a foreign-uid manual-swapped must be rejected: {reply:?}"
        );
    }

    // --- the frozen versioned wire contract (issue #164) -----------------------

    #[test]
    fn versioned_status_response_stamps_the_current_version_and_generated_at() {
        let snapshot = StatusSnapshot {
            generated_at: 1_782_777_600,
            ..Default::default()
        };
        let versioned = versioned_status_response(&snapshot);
        assert_eq!(versioned.schema_version, STATUS_SCHEMA_VERSION);
        assert_eq!(versioned.generated_at, 1_782_777_600);
    }

    #[test]
    fn a_blind_active_account_serializes_its_projection_and_a_normal_one_omits_it() {
        // Issue #479: `blind_active` rides the wire as an additive OPTIONAL field — present (with its
        // three sub-fields) for a blind active account, OMITTED (`skip_serializing_if`) for a
        // non-blind one, so a non-blind frame's per-line bytes stay unchanged across the 1.3 → 1.4
        // minor bump. Round-trips back to the same projection.
        let snapshot = StatusSnapshot {
            generated_at: 42,
            accounts: vec![
                AccountReading {
                    label: "work".to_owned(),
                    active: true,
                    blind_active: Some(BlindActive {
                        blind_secs: 480,
                        last_known_session_pct: 87,
                        auto_protection_degraded: true,
                    }),
                    ..Default::default()
                },
                AccountReading {
                    label: "spare".to_owned(),
                    ..Default::default()
                },
            ],
            ..Default::default()
        };
        let json = serde_json::to_string(&status_response(&snapshot)).unwrap();
        assert!(
            json.contains(
                r#""blind_active":{"blind_secs":480,"last_known_session_pct":87,"auto_protection_degraded":true}"#
            ),
            "the blind active account carries its projection on the wire: {json}",
        );
        // The non-blind peer omits the field entirely (`skip_serializing_if`).
        let spare_tail = json.split(r#""label":"spare""#).nth(1).unwrap();
        assert!(
            !spare_tail.contains("blind_active"),
            "a non-blind account omits `blind_active` from the wire: {json}",
        );
        // Round-trip: the projection decodes back unchanged (a current client reads it).
        let parsed: StatusResponse = serde_json::from_str(&json).unwrap();
        let work = parsed.accounts.iter().find(|a| a.active).unwrap();
        assert_eq!(
            work.blind_active,
            Some(BlindActive {
                blind_secs: 480,
                last_known_session_pct: 87,
                auto_protection_degraded: true,
            }),
        );
        assert!(
            parsed
                .accounts
                .iter()
                .find(|a| !a.active)
                .unwrap()
                .blind_active
                .is_none(),
            "the omitted field decodes back to None (serde default)",
        );
    }

    #[test]
    fn canonical_scrub_rides_the_wire_as_an_additive_rollup_and_a_healthy_snapshot_omits_it() {
        // Issue #516 / schema 1.4 → 1.5: the daemon-level `canonical_scrub` rollup rides the wire as
        // an additive OPTIONAL field — present as a `{"state":…}` discriminant while the shared
        // canonical is scrubbed, OMITTED (`skip_serializing_if`) when healthy, so a non-scrub frame's
        // bytes stay unchanged (mirroring `blind_active`). Round-trips back to the same rollup, and
        // carries a bare STATE discriminant only — never a token or email (#15).
        for (scrub, wire) in [
            (
                CanonicalScrub::Recovering,
                r#""canonical_scrub":{"state":"recovering"}"#,
            ),
            (
                CanonicalScrub::Exhausted,
                r#""canonical_scrub":{"state":"exhausted"}"#,
            ),
        ] {
            let snapshot = StatusSnapshot {
                canonical_scrub: Some(scrub),
                accounts: vec![AccountReading {
                    label: "work".to_owned(),
                    active: true,
                    ..Default::default()
                }],
                ..Default::default()
            };
            let json = serde_json::to_string(&status_response(&snapshot)).unwrap();
            assert!(
                json.contains(wire),
                "the scrub rollup rides the wire: {json}"
            );
            // #15: a bare state discriminant — never an email or token sigil. Non-vacuous: the label
            // "work" reaches the scanned corpus (a real handle rode the wire), so the clean verdict
            // is not vacuously true on an empty payload.
            assert!(json.contains("\"label\":\"work\""), "got {json}");
            assert!(crate::redaction::meter::unauthored_emails(&json, &[]).is_empty());
            assert!(!json.to_lowercase().contains("token"));
            // Round-trip: the rollup decodes back unchanged (a current client reads it).
            let parsed: StatusResponse = serde_json::from_str(&json).unwrap();
            assert_eq!(parsed.canonical_scrub, Some(scrub));
        }

        // A HEALTHY snapshot omits the field ENTIRELY (`skip_serializing_if`), so its bytes are
        // byte-for-byte unchanged across the additive minor bump — the property the golden pins.
        let healthy = StatusSnapshot {
            accounts: vec![AccountReading {
                label: "work".to_owned(),
                active: true,
                ..Default::default()
            }],
            ..Default::default()
        };
        let json = serde_json::to_string(&status_response(&healthy)).unwrap();
        assert!(
            !json.contains("canonical_scrub"),
            "a healthy snapshot omits the field: {json}"
        );
        // …and the omitted field decodes back to None (serde default) — a current client reads a
        // pre-#516 / healthy frame as "no scrub".
        let parsed: StatusResponse = serde_json::from_str(&json).unwrap();
        assert!(parsed.canonical_scrub.is_none());
    }

    #[test]
    fn keychain_locked_rides_the_wire_as_an_additive_flag_and_an_unlocked_snapshot_omits_it() {
        // Issue #498 / schema 1.5 → 1.6: the daemon-level `keychain_locked` flag rides the wire as an
        // additive OPTIONAL field — present as `"keychain_locked":true` while the login keychain is
        // LOCKED (the shared credential is UNREADABLE), OMITTED (`skip_serializing_if`) when unlocked,
        // so a non-locked frame's bytes stay unchanged (mirroring `canonical_scrub`). Round-trips back
        // to the same flag, and carries a bare BINARY state discriminant only — never a token or email
        // (#15). Distinct from `canonical_scrub`: an UNREADABLE item (keychain locked) vs a readable-
        // but-scrubbed one.
        let snapshot = StatusSnapshot {
            keychain_locked: true,
            accounts: vec![AccountReading {
                label: "work".to_owned(),
                active: true,
                ..Default::default()
            }],
            ..Default::default()
        };
        let json = serde_json::to_string(&status_response(&snapshot)).unwrap();
        assert!(
            json.contains(r#""keychain_locked":true"#),
            "the keychain-locked flag rides the wire: {json}"
        );
        // #15: a bare boolean discriminant — never an email or token sigil. Non-vacuous: the label
        // "work" reaches the scanned corpus (a real handle rode the wire), so the clean verdict is
        // not vacuously true on an empty payload.
        assert!(json.contains("\"label\":\"work\""), "got {json}");
        assert!(crate::redaction::meter::unauthored_emails(&json, &[]).is_empty());
        assert!(!json.to_lowercase().contains("token"));
        // Round-trip: the flag decodes back unchanged (a current client reads it).
        let parsed: StatusResponse = serde_json::from_str(&json).unwrap();
        assert!(parsed.keychain_locked);

        // An UNLOCKED snapshot omits the field ENTIRELY (`skip_serializing_if`), so its bytes are
        // byte-for-byte unchanged across the additive minor bump — the property the golden pins.
        let unlocked = StatusSnapshot {
            accounts: vec![AccountReading {
                label: "work".to_owned(),
                active: true,
                ..Default::default()
            }],
            ..Default::default()
        };
        let json = serde_json::to_string(&status_response(&unlocked)).unwrap();
        assert!(
            !json.contains("keychain_locked"),
            "an unlocked snapshot omits the field: {json}"
        );
        // …and the omitted field decodes back to `false` (serde default) — a current client reads an
        // unlocked frame as "not locked".
        let parsed: StatusResponse = serde_json::from_str(&json).unwrap();
        assert!(!parsed.keychain_locked);

        // Explicit forward/back-compat: a pre-#498 (schema 1.5) frame that never carried the key
        // decodes to `false` — the `#[serde(default)]` tolerate-by-ignoring contract a current client
        // relies on (mirrors the Swift `decodeIfPresent(Bool.self) ?? false` path).
        let pre_498 = r#"{"accounts":[],"next_swap":null,"refresh_enabled":true,"systemic_refresh_failure":null}"#;
        let parsed: StatusResponse = serde_json::from_str(pre_498).unwrap();
        assert!(!parsed.keychain_locked);
    }

    #[test]
    fn the_status_wire_is_flat_and_carries_the_frozen_meta() {
        // AC-1: the snapshot carries `schema_version` + `generated_at`, and the payload stays
        // FLAT at the top level (the settled #137–#143 shape, only prefixed with the two meta
        // fields — so existing internal readers that decode a bare `StatusResponse` still work).
        let snapshot = StatusSnapshot {
            generated_at: 1_782_777_600,
            accounts: vec![AccountReading {
                label: "work".to_owned(),
                ..Default::default()
            }],
            ..Default::default()
        };
        let json = serde_json::to_string(&versioned_status_response(&snapshot)).unwrap();
        assert!(
            json.contains(r#""schema_version":{"major":1,"minor":8}"#),
            "got {json}"
        );
        assert!(json.contains(r#""generated_at":1782777600"#), "got {json}");
        // Flat: the payload's `accounts` sits at the top level, not nested under a wrapper key.
        assert!(json.contains(r#""accounts":[{"#), "got {json}");
    }

    #[test]
    fn the_control_status_reply_is_the_versioned_envelope() {
        // The end-to-end wire: a `status` control request replies with the frozen envelope a
        // read-only client decodes (issue #164) — version + freshness stamp + payload, no signal.
        let snapshot = StatusSnapshot {
            generated_at: 42,
            accounts: vec![AccountReading {
                label: "work".to_owned(),
                ..Default::default()
            }],
            ..Default::default()
        };
        let (reply, signal) = control_reply(r#"{"cmd":"status"}"#, &snapshot, false);
        assert!(signal.is_none(), "status is a pure read");
        let parsed: VersionedStatus = serde_json::from_str(reply.trim_end()).unwrap();
        assert_eq!(parsed.schema_version, STATUS_SCHEMA_VERSION);
        assert_eq!(parsed.generated_at, 42);
        assert_eq!(parsed.status.accounts[0].label, "work");
    }

    #[test]
    fn a_bare_status_response_decodes_from_the_versioned_wire() {
        // The flatten envelope keeps the wire FLAT (issue #164), so the internal readers that
        // decode a BARE `StatusResponse` (`poke::daemon_status_best_effort`,
        // `use_account::query_status`) are UNAFFECTED by the two meta fields — serde ignores the
        // extra top-level `schema_version` / `generated_at` keys they do not name. This is the
        // backward-compat guarantee the flatten design rests on.
        let snapshot = StatusSnapshot {
            systemic_refresh: None,
            canonical_scrub: None,
            keychain_locked: false,
            recent_blind_preempt_swap: None,
            recent_landing_overshoot: None,
            generated_at: 1_782_777_600,
            accounts: vec![AccountReading {
                label: "work".to_owned(),
                active: true,
                ..Default::default()
            }],
            next_swap: Some(NextSwap::NoViableTarget {
                cause: None,
                resets_at: None,
            }),
            refresh_enabled: false,
        };
        let wire = serde_json::to_string(&versioned_status_response(&snapshot)).unwrap();
        let bare: StatusResponse = serde_json::from_str(&wire).unwrap();
        assert_eq!(bare.accounts.len(), 1);
        assert_eq!(bare.accounts[0].label, "work");
        assert!(bare.accounts[0].active);
        assert_eq!(
            bare.next_swap,
            Some(NextSwap::NoViableTarget {
                cause: None,
                resets_at: None
            })
        );
    }

    #[test]
    fn a_pre_393_target_without_a_reason_decodes_as_none() {
        // Forward-compat (#393): `reason` is an ADDITIVE field (schema 1.2), so a pre-#393 daemon
        // emits a bare `target` with no `reason` key. `#[serde(default)]` must read that as `None`
        // — never a deserialize error — which is the contract the Swift client's `decodeIfPresent`
        // path mirrors (`WireDecoderTests.testPreReasonTargetDecodesWithNilReason`). An explicit
        // `null` decodes the same way, and a `None` round-trips back out as `"reason":null`
        // (this wire carries no `skip_serializing_if`, per the codebase convention).
        let expected = NextSwap::Target {
            to: "spare".to_owned(),
            reason: None,
        };

        let absent: NextSwap = serde_json::from_str(r#"{"state":"target","to":"spare"}"#).unwrap();
        assert_eq!(absent, expected, "an absent `reason` key decodes as None");

        let explicit_null: NextSwap =
            serde_json::from_str(r#"{"state":"target","to":"spare","reason":null}"#).unwrap();
        assert_eq!(explicit_null, expected, "an explicit null decodes as None");

        assert_eq!(
            serde_json::to_string(&expected).unwrap(),
            r#"{"state":"target","to":"spare","reason":null}"#
        );
    }

    #[test]
    fn every_next_swap_reason_variant_round_trips_its_wire_tag() {
        // The `kind` tags are a CROSS-LANGUAGE contract: `WireModel.swift` matches these exact
        // strings and treats an unknown tag as a hard decode error, so a tag rename here silently
        // breaks the panel. Pin all three shapes (only `soonest_reset` carries a payload).
        for (reason, wire) in [
            (
                NextSwapReason::SoonestReset {
                    resets_at: 1_782_800_000,
                },
                r#"{"kind":"soonest_reset","resets_at":1782800000}"#,
            ),
            (
                NextSwapReason::OnlyCandidate,
                r#"{"kind":"only_candidate"}"#,
            ),
            (NextSwapReason::RosterOrder, r#"{"kind":"roster_order"}"#),
        ] {
            assert_eq!(serde_json::to_string(&reason).unwrap(), wire);
            assert_eq!(
                serde_json::from_str::<NextSwapReason>(wire).unwrap(),
                reason
            );
        }
    }

    #[test]
    fn the_versioned_status_wire_carries_no_secret() {
        // AC-3 (redaction unchanged): the envelope adds only a version object + a timestamp, so
        // the wire still carries no email / token / fingerprint (issue #15).
        let snapshot = StatusSnapshot {
            generated_at: 1_782_777_600,
            accounts: vec![AccountReading {
                label: "work".to_owned(),
                ..Default::default()
            }],
            ..Default::default()
        };
        let json = serde_json::to_string(&versioned_status_response(&snapshot)).unwrap();
        assert!(
            crate::redaction::meter::unauthored_emails(&json, &[]).is_empty(),
            "got {json}"
        );
        assert!(!json.to_lowercase().contains("token"), "got {json}");
    }

    #[tokio::test]
    async fn refresh_exclusions_name_the_active_and_imminent_target_not_dead_accounts() {
        // Issues #105 + #106: the periodic refresh tick touches PARKED accounts only, so the
        // daemon hands it the uuids to skip — computed from the authoritative swap state the
        // tick has no view of:
        //   - the ACTIVE account (the live session's credential — never refresh it), and
        //   - the IMMINENT swap target (the same account `next_swap` surfaces; a swap promotes
        //     it by reading its stash WITHOUT rewriting it (#6), so the engine's CAS re-stash
        //     (#102) could not observe the promotion — exclude it ahead of the window).
        // A QUARANTINED (dead, #42) account is NO LONGER excluded (#106 reverses #105): it is
        // a RESTORE candidate, reported separately by `refresh_quarantined`. A HEALTHY parked
        // account that is NOT the imminent target is left out of BOTH sets — it is exactly
        // what the tick exists to keep fresh on the routine near-expiry path.
        let roster = vec![
            account("u-A", "work"),    // active
            account("u-B", "spare"),   // viable, soonest reset -> imminent swap target
            account("u-C", "backup"),  // quarantined (dead) -> restore candidate, NOT excluded
            account("u-D", "reserve"), // healthy parked, later reset -> in neither set
        ];
        let store = store_holding(b"A-token").await;
        let stash = stash_with(&[
            ("Sessiometer/u-A", b"A-token", "u-A"),
            ("Sessiometer/u-B", b"B-token", "u-B"),
            ("Sessiometer/u-C", b"C-token", "u-C"),
            ("Sessiometer/u-D", b"D-token", "u-D"),
        ])
        .await;
        let (_dir, json) = claude_json("u-A");
        let tun = tunables(95, 80, 0); // target-max-session-usage 0.80
        let mut daemon: FakeDaemon = Daemon::new(
            roster,
            FakeRosterPoller::new(),
            store,
            stash,
            FakeClock::frozen(),
            json,
            &tun,
        );

        // Seed the post-tick state the exclusion set reads from: active = `work`, and carried
        // readings that make `spare` (reset 200) the soonest-reset viable target ahead of
        // `reserve` (reset 500). `backup` is dead — its masked-away reading is irrelevant.
        daemon.state.active = Some(0);
        daemon.state.seed_readings([
            Some(Usage {
                session: 0.97,
                weekly: 0.10,
                weekly_resets_at: Some(100), // soonest overall — but it is the active account
                session_resets_at: None,
            }),
            Some(Usage {
                session: 0.50, // below the 0.80 floor -> viable
                weekly: 0.10,
                weekly_resets_at: Some(200), // soonest among the viable -> the target
                session_resets_at: None,
            }),
            Some(Usage {
                session: 0.10,
                weekly: 0.10,
                weekly_resets_at: Some(300),
                session_resets_at: None,
            }),
            Some(Usage {
                session: 0.10, // also viable…
                weekly: 0.10,
                weekly_resets_at: Some(500), // …but a later reset, so never the target
                session_resets_at: None,
            }),
        ]);
        daemon.state.accounts[2].health.quarantined = true; // `backup` is dead

        let excluded = daemon.refresh_exclusions();
        let quarantined = daemon.refresh_quarantined();

        // Excluded = active (u-A) + the imminent target (u-B) ONLY — NOT the dead `backup`
        // (u-C, now a restore candidate) and NOT the healthy parked `reserve` (u-D).
        assert_eq!(excluded, vec!["u-A".to_owned(), "u-B".to_owned()]);
        assert!(
            !excluded.contains(&"u-C".to_owned()),
            "dead account is no longer excluded"
        );
        assert!(!excluded.contains(&"u-D".to_owned()));
        // The dead account is reported for the RESTORE path instead (#106).
        assert_eq!(quarantined, vec!["u-C".to_owned()]);
    }

    /// Issue #612 arming: the enhanced selection is OFF until a `tiebreak_seed` is set, and setting
    /// it is what turns it on. That is the invariant the whole gating design rests on — `Daemon::new`
    /// leaves it `None`, so every hermetic daemon test keeps the deterministic pre-#612 order, and
    /// production opts in explicitly (`cli.rs`, one entropy draw per process). Asserted THROUGH
    /// `refresh_exclusions`, a real consumer of the selection, so it pins the field being threaded
    /// and consumed — not merely stored.
    ///
    /// The `cli.rs` arming line itself sits inside the runtime `run` entry (real keychain, poller
    /// and socket) and is no more unit-reachable than its sibling `with_refresh_enabled` /
    /// `with_systemic_failure_n` calls in the same builder chain; this pins the switch those flip.
    #[tokio::test]
    async fn tiebreak_seed_is_unarmed_by_default_and_arms_the_enhanced_selection() {
        /// A retained velocity EMA, `samples` at or above [`MIN_VELOCITY_SAMPLES`] so the rate is
        /// TRUSTED and the velocity axis acts on it. A local copy of `selection`'s same-named test
        /// helper: test helpers are private to their own `mod tests`, so the split leaves both
        /// modules self-contained rather than coupling one test module to the other.
        fn ema(rate: f64, samples: u32) -> Option<VelocityEma> {
            Some(VelocityEma { rate, samples })
        }
        // `spare` and `backup` TIE on their weekly reset (200), so #37's dominant axis cannot
        // separate them and the tie-break decides. `backup` is the calmer climber, so the enhanced
        // selection lands there while the legacy path keeps roster order (`spare`).
        async fn tied_pair_daemon() -> (tempfile::TempDir, FakeDaemon) {
            let roster = vec![
                account("u-A", "work"),   // active
                account("u-B", "spare"),  // tied reset, steep climber
                account("u-C", "backup"), // tied reset, gentle climber
            ];
            let store = store_holding(b"A-token").await;
            let stash = stash_with(&[
                ("Sessiometer/u-A", b"A-token", "u-A"),
                ("Sessiometer/u-B", b"B-token", "u-B"),
                ("Sessiometer/u-C", b"C-token", "u-C"),
            ])
            .await;
            let (dir, json) = claude_json("u-A");
            let tun = tunables(95, 80, 0); // target-max-session-usage 0.80
            let mut daemon: FakeDaemon = Daemon::new(
                roster,
                FakeRosterPoller::new(),
                store,
                stash,
                FakeClock::frozen(),
                json,
                &tun,
            );
            daemon.state.active = Some(0);
            daemon.state.seed_readings([
                Some(Usage {
                    session: 0.97,
                    weekly: 0.10,
                    weekly_resets_at: Some(100), // soonest overall — but it is the active account
                    session_resets_at: None,
                }),
                Some(Usage {
                    session: 0.50, // below the 0.80 floor -> viable
                    weekly: 0.10,
                    weekly_resets_at: Some(200), // tied…
                    session_resets_at: None,
                }),
                Some(Usage {
                    session: 0.50, // also viable…
                    weekly: 0.10,
                    weekly_resets_at: Some(200), // …tied, so the tie-break decides
                    session_resets_at: None,
                }),
            ]);
            for (account, velocity) in
                daemon
                    .state
                    .accounts
                    .iter_mut()
                    .zip([None, ema(0.010, 3), ema(0.001, 3)])
            {
                account.session_velocity = velocity;
            }
            (dir, daemon)
        }

        // Unarmed — `Daemon::new`'s default. The reset tie falls through to roster order exactly as
        // it did pre-#612: `spare` wins despite climbing ten times faster than `backup`.
        let (_dir, unarmed) = tied_pair_daemon().await;
        assert_eq!(
            unarmed.tiebreak_seed, None,
            "new() must leave the enhanced selection OFF — every hermetic daemon test depends on it"
        );
        assert_eq!(
            unarmed.refresh_exclusions(),
            vec!["u-A".to_owned(), "u-B".to_owned()]
        );

        // Armed — what production does. The velocity axis engages and prefers the calmer `backup`,
        // so the seed demonstrably reaches the selection rather than sitting unread on the daemon.
        let (_dir, armed) = tied_pair_daemon().await;
        let armed = armed.with_tiebreak_seed(0x5EED);
        assert_eq!(armed.tiebreak_seed, Some(0x5EED));
        assert_eq!(
            armed.refresh_exclusions(),
            vec!["u-A".to_owned(), "u-C".to_owned()],
            "an armed daemon must take the enhanced path"
        );
    }

    #[test]
    fn swap_report_renders_only_for_a_swap_outcome() {
        let snapshot = StatusSnapshot {
            systemic_refresh: None,
            canonical_scrub: None,
            keychain_locked: false,
            recent_blind_preempt_swap: None,
            recent_landing_overshoot: None,
            generated_at: 0,
            refresh_enabled: false,
            accounts: vec![
                AccountReading {
                    label: "work".to_owned(),
                    active: false,
                    enabled: true,
                    quarantined: false,
                    recovering: false,
                    weekly_exhausted: false,
                    usage: None,
                    ..Default::default()
                },
                AccountReading {
                    label: "spare".to_owned(),
                    active: true,
                    enabled: true,
                    quarantined: false,
                    recovering: false,
                    weekly_exhausted: false,
                    usage: None,
                    ..Default::default()
                },
            ],
            next_swap: None,
        };
        let outcome = |action| TickOutcome {
            action,
            events: Vec::new(),
            diagnostics: Vec::new(),
            snapshot: snapshot.clone(),
            next_wait: None,
        };
        assert_eq!(
            swap_report(&outcome(TickAction::Swapped { from: 0, to: 1 })).as_deref(),
            Some("swapped off work onto spare"),
        );
        // #42: an emergency swap echoes too, named distinctly so the operator sees a
        // dead credential forced the rotation.
        assert_eq!(
            swap_report(&outcome(TickAction::EmergencySwapped { from: 0, to: 1 })).as_deref(),
            Some("emergency-swapped off work onto spare (dead credential)"),
        );
        // #467: the autonomous scrubbed-canonical recovery echoes too, named distinctly so the
        // operator sees the daemon self-healed the shared item back onto a live account.
        assert_eq!(
            swap_report(&outcome(TickAction::CanonicalAdopted { to: 1 })).as_deref(),
            Some("recovered scrubbed canonical onto spare (was Not-logged-in)"),
        );
        assert_eq!(swap_report(&outcome(TickAction::Held)), None);
        assert_eq!(swap_report(&outcome(TickAction::SkippedCooldown)), None);
        assert_eq!(swap_report(&outcome(TickAction::NoViableTarget)), None);
        // A dead active account with no viable target holds — no console echo.
        assert_eq!(swap_report(&outcome(TickAction::ActiveDeadNoTarget)), None);
    }

    #[test]
    fn unrecoverable_report_names_the_handle_and_the_relogin_action() {
        // Issue #261 AC1/AC3: the operator message names the account HANDLE and the fix
        // (`claude /login`), and is sourced from the LABEL alone — no token or email (#15). Both
        // operator channels (console + macOS) carry this exact string, so testing it covers both.
        let line = unrecoverable_report("work");
        assert!(line.contains("work"), "must name the handle: {line}");
        assert!(
            line.contains("claude /login"),
            "must name the fix action: {line}"
        );
        // The whole message is the handle interpolated into a fixed non-secret template — a label
        // is the ONLY dynamic input, mirroring `Event::CredentialUnrecoverable`'s redaction.
        assert_eq!(
            line,
            "account work needs re-login — its refresh token is dead; run: claude /login"
        );
    }

    #[tokio::test]
    async fn swap_log_lines_name_to_as_the_now_active_account_from_as_swapped_away() {
        // DECIDER (issue #89): the from→to direction on BOTH operator surfaces — the
        // foreground console echo (`swap_report`) AND the durable `event=swap` log line
        // — must match the PHYSICAL outcome of a real swap: `to` is the account the
        // daemon just made active (swapped ONTO), `from` the one it swapped OFF. Drive
        // a genuine swap (`work` active and over the session trigger → the viable target
        // `spare`) and tie both rendered lines back to `state.active`, so a future
        // inversion of either surface — or of the `Event::Swap` / `TickAction` source —
        // fails HERE instead of silently misleading the operator. (#15: labels only.)
        let roster = vec![account("u-A", "work"), account("u-B", "spare")];
        let store = store_holding(b"A-token").await;
        let stash = stash_with(&[
            ("Sessiometer/u-A", b"A-token", "u-A"),
            ("Sessiometer/u-B", b"B-token", "u-B"),
        ])
        .await;
        let (_dir, json) = claude_json("u-A");
        let poller = FakeRosterPoller::new()
            .ok("u-A", 0.97, 0.40) // active, over the session trigger
            .ok("u-B", 0.05, 0.05); // the only viable target
        let tun = tunables(95, 80, 0);

        let mut daemon: FakeDaemon = Daemon::new(
            roster,
            poller,
            store,
            stash,
            FakeClock::new(Duration::ZERO),
            json,
            &tun,
        );

        let outcome = warmed_tick(&mut daemon).await;

        // The physical outcome: `work` (index 0) was swapped OFF; `spare` (index 1) is
        // now active. `to` must name the now-active account on every surface.
        assert_eq!(outcome.action, TickAction::Swapped { from: 0, to: 1 });
        assert_eq!(
            daemon.state.active,
            Some(1),
            "physical outcome: `spare` (index 1) is now the active account"
        );

        // Surface 1 — the foreground console echo: off=<swapped-away>, onto=<now-active>.
        assert_eq!(
            swap_report(&outcome).as_deref(),
            Some("swapped off work onto spare"),
            "console echo must name the swapped-away account, then the now-active one",
        );

        // Surface 2 — the durable event log agrees: from=<swapped-away> to=<now-active>.
        let swap_event = outcome
            .events
            .iter()
            .find(|e| matches!(e, Event::Swap { .. }))
            .expect("a swap surfaces a structured Event::Swap (#9)");
        let log_line = swap_event.to_log_line(std::time::SystemTime::UNIX_EPOCH);
        assert!(
            log_line.contains("event=swap from=work to=spare"),
            "event log must name from=<swapped-away> to=<now-active>; got `{log_line}`",
        );
    }

    #[tokio::test]
    async fn next_swap_classifies_the_candidate_from_the_readings() {
        // The daemon-side candidate (#88) IS `pick_target` mapped to a label, plus the
        // two no-candidate verdicts the wire must distinguish. Reuses the 3-account
        // harness (work=0, spare=1, backup=2; target_max_session_usage 0.80, weekly_ceiling_base
        // 0.98). This pins the projection/classification wrapper — `pick_target`'s own
        // selection logic is covered by its dedicated suite above.
        let daemon = three_account_daemon(FakeRosterPoller::new()).await;
        let usage = |session: f64, weekly: f64| {
            Some(Usage {
                session,
                weekly,
                weekly_resets_at: None,
                session_resets_at: None,
            })
        };

        // Viable target → the choice mapped to a label plus its #393 reason. spare and backup are
        // both under the floor and weekly-viable; with no known reset the tie falls to the earliest
        // roster index (spare). Because TWO targets were viable and no reset-time comparison could
        // discriminate them, the reason is `RosterOrder` — neither a fabricated `SoonestReset` with
        // no epoch to carry, nor `OnlyCandidate`, which would tell the operator "only viable
        // target" while backup was equally viable.
        assert_eq!(
            daemon.next_swap(
                Some(0),
                &[usage(0.97, 0.40), usage(0.10, 0.10), usage(0.20, 0.10)]
            ),
            Some(NextSwap::Target {
                to: "spare".to_owned(),
                reason: Some(NextSwapReason::RosterOrder),
            }),
        );

        // Readings in hand but none viable (both over the 0.80 target-max-session-usage) → a
        // genuine no-viable-target verdict, NOT awaiting-data. Both spares are weekly-VIABLE
        // (0.10 < 0.98) yet over the session ceiling (`min(0.95, 0.80)` = 0.80), so the fleet
        // is blocked only by SESSION — the footer relief carries `Session` (issue #405). No
        // parseable session reset in these readings → `resets_at` is `None`.
        assert_eq!(
            daemon.next_swap(
                Some(0),
                &[usage(0.97, 0.40), usage(0.95, 0.10), usage(0.90, 0.10)]
            ),
            Some(NextSwap::NoViableTarget {
                cause: Some(NoTargetCause::Session),
                resets_at: None
            }),
        );

        // Every other account weekly-exhausted (>= 0.98 base) → no viable target; the block is
        // WEEKLY-wide, so the relief carries `Weekly` (issue #405). No parseable weekly reset here.
        assert_eq!(
            daemon.next_swap(
                Some(0),
                &[usage(0.97, 0.40), usage(0.10, 0.99), usage(0.10, 0.99)]
            ),
            Some(NextSwap::NoViableTarget {
                cause: Some(NoTargetCause::Weekly),
                resets_at: None
            }),
        );

        // No reading for any other account yet — the post-restart moment #88 exists to
        // surface distinctly.
        assert_eq!(
            daemon.next_swap(Some(0), &[usage(0.97, 0.40), None, None]),
            Some(NextSwap::AwaitingData),
        );

        // MIXED warm-up: one live other already polled-and-disqualified (spare over the
        // 0.80 floor), another still unpolled (backup). This is the ONLY input that
        // separates the `all_unpolled` rule from a naive any-unpolled one — `all_unpolled`
        // is false (spare has a reading), so the verdict is `no viable target`, NOT
        // `awaiting usage data`, even though a live account is still awaiting its first
        // poll. Pins the deliberate all-vs-any choice (an `&=`→`=` mutation flips this).
        assert_eq!(
            daemon.next_swap(Some(0), &[usage(0.97, 0.40), usage(0.95, 0.10), None]),
            Some(NextSwap::NoViableTarget {
                cause: Some(NoTargetCause::Session),
                resets_at: None
            }),
        );

        // No active anchor to swap from → no candidate at all (renders a bare `none`).
        assert_eq!(
            daemon.next_swap(None, &[usage(0.97, 0.40), None, None]),
            None
        );
    }

    #[tokio::test]
    async fn next_swap_reads_all_quarantined_others_as_no_viable_target() {
        // A `None` reading for another account has two causes the #88 substates must NOT
        // conflate: a not-yet-polled cold start (genuine `awaiting usage data`) vs a
        // QUARANTINED account (#42) whose reading `decision_readings` masks to `None`.
        // When every OTHER enabled account is quarantined there is no live target, so the
        // footer must say `no viable target` — promising "usage data" that needs a
        // re-login, not a poll, would mislead. Reuses the 3-account harness (work=0
        // active, spare=1, backup=2).
        let mut daemon = three_account_daemon(FakeRosterPoller::new()).await;
        let usage = |session: f64, weekly: f64| {
            Some(Usage {
                session,
                weekly,
                weekly_resets_at: None,
                session_resets_at: None,
            })
        };

        // Both other accounts dead (their readings masked to `None`, as the snapshot
        // would pass them) → no viable target, NOT awaiting-data.
        daemon.state.accounts[1].health.quarantined = true;
        daemon.state.accounts[2].health.quarantined = true;
        // Every other reading masked to `None` → relief falls to the WEEKLY-wide default with no
        // parseable reset (the per-account 🔴 health names the re-login remedy on each row; #405).
        assert_eq!(
            daemon.next_swap(Some(0), &[usage(0.97, 0.40), None, None]),
            Some(NextSwap::NoViableTarget {
                cause: Some(NoTargetCause::Weekly),
                resets_at: None
            }),
        );

        // Revive one: a live, not-yet-polled other account restores the genuine
        // cold-start `awaiting usage data` verdict (the substate is unchanged for it).
        daemon.state.accounts[1].health.quarantined = false;
        assert_eq!(
            daemon.next_swap(Some(0), &[usage(0.97, 0.40), None, None]),
            Some(NextSwap::AwaitingData),
        );
    }
}
