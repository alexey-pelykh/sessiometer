// Copyright (c) 2026 Oleksii PELYKH
// SPDX-License-Identifier: MIT

//! The socket command handlers of the [`Daemon`] decision core (issue #637 step 4, issue #659,
//! split out of the single `impl Daemon` block).
//!
//! Where [`super::socket`] owns the WIRE (framing, parsing, peer auth, the reply types),
//! this owns what each command DOES to daemon state: apply a `swap` (#167), a `capture`
//! (#359) or a `config-set` (#268), adopt an out-of-band manual `use` swap (#64), and adopt a
//! runtime roster reload (#139). Each returns the ack the socket layer serialises, so a
//! rejected command mutates nothing.

use super::*;

impl<P, C, S, K> super::Daemon<P, C, S, K>
where
    P: RosterPoller,
    C: CredentialStore,
    S: AccountStash,
    K: Clock,
{
    /// Adopt a manual `use` swap signalled over the control socket (issue #64).
    ///
    /// `use` rewrote the canonical credential out-of-band and then notified us; this
    /// records it as the latest swap so the EXISTING post-swap cooldown (#10)
    /// applies — the very next poll therefore HOLDS on the operator's choice instead
    /// of immediately reverting it, and normal policy resumes once the cooldown
    /// window elapses (a cooldown, never a permanent pin). The active account is
    /// re-resolved from the AUTHORITATIVE canonical item, not from the message — the
    /// signal carries no target — so an out-of-order or duplicate notification
    /// cannot corrupt state; at worst it re-arms a cooldown. Mirrors
    /// [`record_swap`](Self::record_swap): update active, arm the cooldown + `status`
    /// display, and prime the canonical watch so this manual write is not later
    /// re-detected as an out-of-band change (#13).
    pub(super) async fn adopt_manual_swap(&mut self) {
        let at = self.clock.now();
        // Re-resolve active from the canonical item and prime the watch. A locked /
        // unreadable keychain leaves active to the next tick's own resolve, but the
        // cooldown is armed regardless below — the load-bearing manual-hold effect.
        if let Ok(canonical) = self.store.read().await {
            let prev_active = self.state.active;
            let next_active = self.resolve_account_for(&canonical).await;
            self.state.active = next_active;
            // If this manual swap moved AWAY from an account that was mid-recovery, drop
            // its now-frozen recovery probe so its dead-spare state is honest (issue
            // #108). This is the load-bearing seam: `adopt_manual_swap` commits the
            // canonical-watch baseline below, so `reconcile_canonical_change` will see
            // this write as `Unchanged` and never re-observe it.
            self.deactivate_recovery_probe(prev_active, next_active);
            // Issue #450: a manual swap to a DIFFERENT account leaves the departing
            // active's `last_good` stale — drop it so the anchor tracks the new active.
            // A duplicate / same-account notification (`next == prev`) keeps it. Mirrors
            // the reset in `record_swap`.
            if next_active != prev_active {
                self.state.last_good = None;
                // Issue #1452: the poll schedule is derived from `active` and `next_poll_index`
                // rebuilds it only at a cycle boundary, so an adoption landing mid-cycle would
                // leave the departed active in every interleave slot and `next_active`
                // unscheduled until the wrap. Under the SAME guard as the reset above, and for
                // the same reason: a duplicate / same-account #64 notification changes no
                // designation, and re-deriving the vector for it would restart the peer sweep for
                // nothing. Inside the `if let Ok(canonical)` block, so a locked keychain — which
                // leaves `active` unresolved and defers to the next tick's own resolve — does not
                // invalidate a schedule that is still correct.
                self.invalidate_poll_schedule();
            }
            self.state.canonical_watch.commit(&canonical);
        }
        // Record it as the latest swap: arms the cooldown (#10). The cooldown arming
        // is what makes a manual choice stick, so it happens even when the active
        // account could not be resolved just now.
        self.state.last_swap = Some(LastSwap { at });
    }

    /// Perform a socket `swap` command (issue #167) against the daemon's live state, returning the
    /// redacted [`SwapAck`] plus any durable [`Event`] to log — WITHOUT touching the socket, so the
    /// re-validation + write are unit-testable apart from the ack I/O (the run loop writes the ack
    /// via [`write_swap_ack`]). Runs where `&mut Daemon` is available (the
    /// run loop's post-idle), because the ack must reflect the REAL outcome (accepted /
    /// rejected-with-reason).
    ///
    /// The daemon re-validates the target's viability from its OWN state — it resolves the handle
    /// against its current roster and reads `quarantined` / weekly-exhaustion / cooldown from its
    /// health + readings — NEVER a client hint (a client-side "greyed out" is UX only). `force` is
    /// POLICY-only ([`swap_command_verdict`]): it bypasses those gates, but it can never manufacture
    /// an outgoing account and never reaches the SAFETY aborts, which live in the swap engine below
    /// — the write goes through the SAME single-writer swap lock (issue #64) the auto-swaps use, so
    /// a contended lock fails closed ([`SwapRejection::SwapLockBusy`]), and the engine's step-1 read
    /// aborts on a LOCKED keychain ([`SwapRejection::KeychainLocked`]) even under `force`. The
    /// single-owner guard is upheld by CONSTRUCTION: the daemon (holding the single-instance lock)
    /// is the SOLE writer, and this command routes the write THROUGH it rather than spawning a
    /// second one.
    ///
    /// On a completed swap it mirrors [`record_swap`](Self::record_swap): caches active, arms the
    /// post-swap cooldown (#10) so the next poll HOLDS on the operator's choice, primes the
    /// canonical watch, drops a now-frozen recovery probe on the account swapped AWAY from (issue
    /// #108, the same transition invariant [`adopt_manual_swap`](Self::adopt_manual_swap) upholds
    /// for the standalone-`use` path), and returns the SAME `Event::Swap` (`Manual` / `Forced`) the
    /// standalone `use` emits — so the cooldown derived from the durable log agrees whichever path
    /// wrote. The returned events also carry any edge-triggered pre-swap canary alarm (#714) the
    /// gated [`locked_swap`](Self::locked_swap) raised — including on a REFUSED swap, where the
    /// wire ack stays a redacted rejection but the durable log names the drift.
    pub(super) async fn perform_socket_swap(
        &mut self,
        command: &SwapCommand,
    ) -> (SwapAck, Vec<Event>) {
        // 1. Resolve the target handle (label OR uuid) against the CURRENT roster — the daemon's
        //    OWN resolution, never a client-provided index; it never guesses (issue #17).
        let target_idx = match crate::use_account::resolve_target(&self.roster, &command.target) {
            Ok(idx) => idx,
            Err(Error::UseTargetAmbiguous { .. }) => {
                return (
                    SwapAck::Rejected {
                        reason: SwapRejection::AmbiguousTarget,
                    },
                    Vec::new(),
                )
            }
            // Not found (or any other resolve failure) → unknown target.
            Err(_) => {
                return (
                    SwapAck::Rejected {
                        reason: SwapRejection::UnknownTarget,
                    },
                    Vec::new(),
                )
            }
        };

        // 2. Re-validate viability from the daemon's LIVE state (health + last readings + the
        //    un-jittered weekly rotation line + the in-memory last-swap), never a client hint. The
        //    `weekly_exhausted` computation is EXACTLY the snapshot's per-account verdict
        //    (`weekly >= weekly_rotation_line()`, i.e. `weekly_ceiling_base − WEEKLY_TAIL_MARGIN`,
        //    issue #607) — same data source AND formula, so the ack agrees with what `status`
        //    shows. This is the daemon's LAST-KNOWN reading (≤ one poll interval old), NOT the
        //    fresh poll the daemon-DOWN `use` runs: a target that crossed the threshold since the
        //    last poll can be accepted here. The divergence is bounded and self-correcting — the
        //    very next tick polls the now-active target and swaps away if it is truly exhausted — and
        //    it deliberately keeps this re-validation off the network so it never blocks the
        //    single-thread run loop (ADR-0001). `force` overrides it either way.
        let now = self.clock.now();
        let quarantined = self.state.accounts[target_idx].health.quarantined;
        // Issue #607: the ROTATION line, not the raw ceiling — otherwise this re-validation accepts
        // a target in the `[ceiling − margin, ceiling)` band that `decide` swaps away from on the
        // next tick, i.e. an operator `use` that silently undoes itself moments later.
        let weekly_exhausted = self.state.accounts[target_idx]
            .last_reading
            .is_some_and(|usage| usage.weekly >= self.weekly_rotation_line());
        let in_cooldown = self
            .state
            .last_swap
            .as_ref()
            .is_some_and(|last| now.saturating_duration_since(last.at) < self.cooldown_base);

        match swap_command_verdict(
            target_idx,
            self.state.active,
            quarantined,
            weekly_exhausted,
            in_cooldown,
            command.force,
        ) {
            SwapVerdict::AlreadyActive => (
                SwapAck::AlreadyActive {
                    to: self.roster[target_idx].label.clone(),
                },
                Vec::new(),
            ),
            SwapVerdict::Reject(reason) => (SwapAck::Rejected { reason }, Vec::new()),
            // The verdict returns `Swap` only when an active account exists (it rejects
            // `NoActiveAccount` otherwise); re-match defensively rather than `expect` on the
            // long-running daemon path.
            SwapVerdict::Swap => match self.state.active {
                None => (
                    SwapAck::Rejected {
                        reason: SwapRejection::NoActiveAccount,
                    },
                    Vec::new(),
                ),
                Some(active_idx) => {
                    let outgoing = self.roster[active_idx].stash();
                    let incoming = self.roster[target_idx].stash();
                    // The SAME lock-wrapped engine (#64) the auto-swaps use: #6 is no-half-swap, so
                    // an error (a contended lock that fails closed, a locked keychain) leaves the
                    // canonical item and both stashes coherent — ZERO writes — and becomes a
                    // redacted rejection. The pre-swap canary (#714) gates it identically to the
                    // auto-swaps — a canary refusal surfaces as the same redacted rejection
                    // (`classify_swap_failure` keeps the wire enum closed for old clients), while
                    // `events` carries the durable edge-triggered canary alarm alongside any
                    // `Event::Swap`. `command.force` rides along so the Layer-3 online probe
                    // (issue #736) honours `use --force` the same way the standalone path does —
                    // otherwise the escape out of a strict refusal would exist only while the
                    // daemon is DOWN, which is the opposite of when an operator needs it. It
                    // bypasses that layer ONLY: Layers 1–2 still fail closed under `--force`.
                    let mut events = Vec::new();
                    match self
                        .locked_swap(&outgoing, &incoming, command.force, &mut events)
                        .await
                    {
                        Ok(_report) => {
                            let prev_active = self.state.active;
                            // Mirror the auto-swap tail: cache active, arm the cooldown, prime the
                            // canonical watch (so this write is not re-detected as out-of-band #13).
                            self.record_swap(target_idx, &incoming, now).await;
                            // An operator-driven swap can move AWAY from a mid-recovery account
                            // (unlike the auto-swap, which HOLDS on one): drop its now-frozen probe
                            // so its dead-spare state is honest (issue #108).
                            self.deactivate_recovery_probe(prev_active, Some(target_idx));
                            let reason = if command.force {
                                SwapReason::Forced
                            } else {
                                SwapReason::Manual
                            };
                            let from = self.roster[active_idx].label.clone();
                            let to = self.roster[target_idx].label.clone();
                            // The SAME durable `Event::Swap` the standalone `use` emits (issue #9),
                            // so the log-derived cooldown agrees whichever path wrote. `session_pct`
                            // = 0: a manual/forced swap is not session-triggered (the reason
                            // distinguishes it). Non-secret handles only (issue #15).
                            events.push(Event::Swap {
                                from: from.clone(),
                                to: to.clone(),
                                reason,
                                session_pct: 0,
                                // Manual / forced swap: operator-driven, not a projection (#634).
                                projection: None,
                            });
                            (SwapAck::Accepted { from, to }, events)
                        }
                        Err(err) => (
                            SwapAck::Rejected {
                                reason: classify_swap_failure(&err),
                            },
                            // A canary-refused swap still carries its edge-triggered alarm
                            // event (#714) — the rejection is redacted on the wire, but the
                            // durable log names the drift/ambiguity once per episode.
                            events,
                        ),
                    }
                }
            },
        }
    }

    /// Perform a socket `capture` command (issue #359) against the daemon's live state, returning
    /// the redacted [`CaptureAck`] plus the durable [`Event::Capture`] to log — WITHOUT touching the
    /// socket, so the read + stash + reconcile are unit-testable apart from the ack I/O (the run
    /// loop writes the ack via [`write_capture_ack`]). Runs where `&mut Daemon` is available (the run
    /// loop's post-idle), because the ack must reflect the REAL outcome (captured / refreshed /
    /// rejected). The daemon-routed sibling of [`perform_socket_swap`](Self::perform_socket_swap),
    /// mirroring it 1:1.
    ///
    /// The daemon does ALL the credential work itself — the client never touches a credential (the
    /// panel-originates-no-seam invariant, REQ-MBR-C-005). It reuses the #357
    /// [`capture_locked`](crate::capture::capture_locked) primitive with its OWN seams (the same
    /// `store` / `stash` / `claude_json` the swaps use), so the identity read → token read → stash
    /// write run under the SAME single-writer swap lock the auto-swaps use: a contended acquire fails
    /// closed ([`CaptureRejection::SwapLockBusy`]) BEFORE any read, and a LOCKED keychain aborts the
    /// token read ([`CaptureRejection::KeychainLocked`]) — the same safety aborts `force` can never
    /// bypass on the swap path. Capture is canonical-READ-ONLY: it never writes the canonical
    /// keychain item or `~/.claude.json`, only a per-account stash + a roster row, so a mid-write
    /// crash cannot corrupt the active account or the live session.
    ///
    /// After the locked stash lands, the new roster is persisted to the wired `config_path` (the
    /// authoritative on-disk `config.toml`, OUTSIDE the lock — a swap never contends on it,
    /// stash-before-roster like the standalone `capture`) and the in-memory rotation is reconciled to
    /// it ([`reconcile_roster`](Self::reconcile_roster), the SAME core the #139 roster-reload
    /// drives): an already-rostered active account is an idempotent REFRESH (its per-account state
    /// preserved, NO duplicate row), and a newly-captured one joins the live rotation without a
    /// restart. The daemon-`None` `config_path` (the hermetic-test default with no reload wired)
    /// fails closed — the capture cannot be persisted, so nothing is stashed-then-lost.
    ///
    /// An ABSENT `config.toml` is gated by the prior-configuration witness (issue #1441, design
    /// D-1) before anything is read or written: this is the second entry point to the append-only
    /// write that collapsed a six-account roster on 2026-08-27, and it carries the SAME rule as the
    /// CLI verbs ([`crate::witness::admit`]) rather than a parallel one. Unlike them it observes
    /// the rule's input TWICE — the durable witness, plus its own in-memory roster, which PRD § 7
    /// row 2 requires and which the CLI has no access to. A refusal is
    /// [`CaptureRejection::PriorConfiguration`] and a true no-op.
    pub(super) async fn perform_socket_capture(
        &mut self,
        command: &CaptureCommand,
    ) -> (CaptureAck, Option<Event>) {
        // The authoritative on-disk roster path — required to persist the capture. Production always
        // wires it (`with_config_path`); with none wired the daemon cannot persist the new roster, so
        // fail closed BEFORE any read rather than land a stash + in-memory row the next restart loses.
        let Some(config_path) = self.config_path.clone() else {
            return self.capture_failure(command, CaptureRejection::Failed);
        };
        // Load the existing roster to plan against (absent → a first capture, malformed → a failure)
        // — read BEFORE the lock, exactly like the standalone `capture`'s `load_existing`.
        let existing = match Config::load_path(&config_path) {
            Ok(config) => Some(config),
            Err(Error::ConfigNotFound { .. }) => None,
            Err(err) => return self.capture_failure(command, classify_capture_failure(&err)),
        };
        // #1441 (design D-1 / D-5, R-2): the arm directly above turns an absent `config.toml` into
        // `None`, and `capture_locked` can then only APPEND — so resolving that ambiguity to "first
        // run" writes a one-account roster over a live six-account one. That is the incident, and
        // this is its SECOND reachable entry point: the menu-bar capture button reaches it over the
        // control socket, so fixing the CLI verbs alone left the collapse reproducible from the GUI.
        //
        // `crate::witness::admit` owns the RULE and is the SAME one the CLI's `capture` / `login`
        // apply — one rule, two entry points, not two rules. What is composed here is its INPUT,
        // because this entry point can observe the one question ("has this machine been configured
        // before?") TWICE, and the CLI can only observe it once.
        //
        // The daemon's OWN in-memory roster is that second observation, and PRD § 7 row 2 turns on
        // it: `config.toml` absent while a RUNNING daemon holds accounts IS the incident, and it
        // must refuse whether or not a durable witness also survived (§ 7a amended row 1 only and
        // says so — "Rows 2–9 are unchanged"). That distinction is reachable rather than academic:
        // the witness's accepted false negative is a loss that took every witness with it, and both
        // usage-store files are SIBLINGS of `config.toml` in one directory, so a single
        // directory-scoped deletion takes all three. On that machine the durable witness answers
        // `Absent` and only the live roster still knows better.
        //
        // Corroboration, never establishment — the direction ADR-0036 permits. The daemon's roster
        // can only ADD refusals: an EMPTY one (§ 7 row 3, a genuine first run with the daemon up)
        // says nothing and leaves the verdict entirely to the durable witness. Reading it costs
        // nothing and consults no socket; the rule's independence from a reachable daemon is what
        // `crate::witness` must preserve, and it still does — this is the caller's observation,
        // made where the fallback is visible rather than hidden inside the rule.
        //
        // Placed HERE, immediately after the config read whose result is this gate's own input and
        // BEFORE `capture_locked` below, so a refusal is a true no-op: no lock acquired, no identity
        // or token read, nothing stashed, no roster saved, `self` unmutated. A verdict reached after
        // the stash has landed would be correct and far too late.
        //
        // `admit`'s `(true, _)` arm is why this whole block is skipped for a PRESENT config: the
        // verdict is fixed there whatever the witness says, so probing would spawn `security` on
        // every capture to learn nothing — the same short-circuit `admit_append_only` makes.
        if existing.is_none() {
            let witness = if self.roster.is_empty() {
                self.witness_sources.observe().await
            } else {
                crate::witness::PriorConfiguration::Present
            };
            if let Err(err) = crate::witness::admit(false, witness) {
                return self.capture_failure(command, classify_capture_failure(&err));
            }
        }
        // Reuse the #357 primitive with the daemon's OWN seams + swap lock. `None` lock is the
        // hermetic-test default (no second in-process writer to serialize against); production threads
        // the real `swap.lock` path, so a concurrent auto-swap cannot interleave with the two reads.
        let lock = self
            .swap_lock_path
            .as_deref()
            .map(|path| (path, SWAP_LOCK_MAX_WAIT));
        match crate::capture::capture_locked(
            lock,
            &self.store,
            &self.stash,
            &self.claude_json,
            existing,
            command.label.as_deref(),
        )
        .await
        {
            Ok(report) => {
                // Persist the new roster OUTSIDE the lock (a swap never contends on `config.toml`),
                // stash-before-roster. A save failure leaves an inert ORPHAN stash, never a partial
                // roster — report it `Failed` (the stash landed, but the roster row did not).
                if let Err(err) = report.config.save_to(&config_path).await {
                    return self.capture_failure(command, classify_capture_failure(&err));
                }
                let crate::capture::CaptureReport {
                    config,
                    outcome,
                    label,
                    count,
                } = report;
                // Reconcile the in-memory rotation to the freshly-written roster (the SAME core the
                // #139 roster-reload drives): an already-rostered active account keeps its per-account
                // state (the idempotent refresh, no duplicate row), a new one joins with default state
                // and becomes a swap target once it has a reading.
                //
                // `capture` is an APPEND-ONLY verb (issue #1442, R-3), so it declares that intent and
                // the shared never-shrink floor applies. It cannot refuse on this capture's OWN
                // account: `plan_capture` has only update-in-place and push arms, so the roster just
                // saved is never smaller than the one read. It CAN refuse when the file that was read
                // had already lost accounts the live daemon still holds — the 2026-08-27 shape
                // reached through this second entry point — and refusing is then exactly right: the
                // stash and the roster row stand on disk, and the live rotation is NOT narrowed onto
                // a file that lost five accounts. The ack reports the CAPTURE, which succeeded, and
                // the single `Event` slot below is already the capture's own line; reporting the
                // disk/memory divergence itself is issue #1443's, on the poll tick. Bound rather
                // than discarded so the `#[must_use]` is answered deliberately.
                let _reconcile = self.reconcile_roster(config.roster, ReloadIntent::AppendOnly);
                // The durable audit line (best-effort logged by the run loop): the resolved roster
                // LABEL handle + the outcome token — non-secret by construction (#15).
                let event = Event::Capture {
                    account: Some(label.clone()),
                    outcome: capture_event_outcome(outcome),
                };
                let ack = match outcome {
                    crate::capture::CaptureOutcome::Captured => {
                        CaptureAck::Captured { label, count }
                    }
                    crate::capture::CaptureOutcome::Refreshed => {
                        CaptureAck::Refreshed { label, count }
                    }
                };
                (ack, Some(event))
            }
            Err(err) => self.capture_failure(command, classify_capture_failure(&err)),
        }
    }

    /// Build the redacted `(CaptureAck, Event)` for a REFUSED capture (issue #359): the bare machine
    /// `reason` on the ack, and the SAME reason folded onto the event's outcome axis. The event's
    /// handle is the operator's label HINT (the only handle a pre-stash failure has — the daemon
    /// never read an identity), or `None` when none was given, so the audit line still names WHY the
    /// capture failed without ever carrying a secret. A pure builder (no `&mut self` mutation), so
    /// the mapping is unit-testable and a refusal is a true no-op on the daemon's state.
    pub(super) fn capture_failure(
        &self,
        command: &CaptureCommand,
        reason: CaptureRejection,
    ) -> (CaptureAck, Option<Event>) {
        let event = Event::Capture {
            account: command.label.clone(),
            outcome: capture_event_outcome_rejected(reason),
        };
        (CaptureAck::Rejected { reason }, Some(event))
    }

    /// Apply an authenticated `config-set` control command (issue #268) where `&mut self` is
    /// available: the tunable + non-credential-label edits the settings UI submitted, applied to
    /// the authoritative on-disk `config.toml` through the tested Rust writer, and NOTHING else —
    /// the safety boundary (no credential, no roster add/remove) is STRUCTURAL, enforced by
    /// [`ConfigSetCommand`]'s [`SetTunables`](crate::config::SetTunables) allow-list + uuid-keyed
    /// label map, so a forbidden edit is unrepresentable here, not merely unhandled.
    ///
    /// Load→overlay→validate→save is the SAME path `capture` uses: read the current file TEXT,
    /// [`apply_settings`](crate::config::Config::apply_settings) overlays the edits onto the raw
    /// layer and re-validates the WHOLE edited config atomically (so a cross-field rule — e.g.
    /// `target_max_session_usage <= session_ceiling` — sees the FINAL state, never a transient
    /// intermediate), then [`save_to`](crate::config::Config::save_to) writes it 0600-atomically
    /// (temp + rename). Every refusal is a TRUE no-op — ZERO writes — leaving the old file intact:
    /// no wired `config_path` → `Unavailable`; absent file → `NoConfig`; unreadable / malformed
    /// baseline → `ConfigUnreadable`; a stale label uuid → `UnknownAccount`; a range / cross-field
    /// violation → `Invalid` (with the non-secret field-named `detail`); an atomic-write failure,
    /// or a config-write lock still held by another writer after its budget (issue #1445 — this
    /// verb is one of the two writers that pair is about) → `SaveFailed`. On success the `effect` tells the UI what the change requires: a LABEL change
    /// is adopted LIVE — the in-memory roster is reconciled to the freshly-written file (the SAME
    /// [`reconcile_roster`](Self::reconcile_roster) core the #139 roster-reload drives) — while a
    /// TUNABLE change is reload-by-restart (the daemon derives its strategy fields once at
    /// construction, with no re-derivation primitive), and a no-op edit writes nothing
    /// (`Unchanged`). A batch that changes both reports `RestartRequired` (the operative action).
    ///
    /// The `config.toml` read is small and inline like `capture`'s `load_existing` (not
    /// `spawn_blocking`): a few KB, well under a tick, and this runs on the run loop's single task
    /// where `&mut self` lives (ADR-0001). Returns only the redacted [`ConfigSetAck`] — a config
    /// edit emits no durable [`Event`] in v1 (the event log records swaps + captures; a
    /// config-change audit line is a clean future addition, not needed for the settings surface).
    pub(super) async fn perform_config_set(&mut self, command: &ConfigSetCommand) -> ConfigSetAck {
        // No wired config path (the hermetic-test default) — config-set is unavailable, exactly as
        // `capture` fails closed with no path to persist to.
        let Some(config_path) = self.config_path.clone() else {
            return ConfigSetAck::Rejected {
                reason: ConfigSetRejection::Unavailable,
                detail: None,
            };
        };
        // Read the current on-disk config TEXT: `apply_settings` re-parses + overlays it, and the
        // save re-renders the canonical commented form (the comments live in the renderer, not the
        // input). Absent → nothing to edit; unreadable → refuse rather than clobber a file we
        // cannot read. Read inline like `capture`'s `load_existing` (small file, ADR-0001).
        let text = match std::fs::read_to_string(&config_path) {
            Ok(text) => text,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                return ConfigSetAck::Rejected {
                    reason: ConfigSetRejection::NoConfig,
                    detail: None,
                };
            }
            Err(_) => {
                return ConfigSetAck::Rejected {
                    reason: ConfigSetRejection::ConfigUnreadable,
                    detail: None,
                };
            }
        };
        // Overlay + atomically re-validate the WHOLE edited config. A rejection writes NOTHING —
        // the old file is intact.
        let (config, change) =
            match Config::apply_settings(&text, &command.tunables, &command.labels) {
                Ok(applied) => applied,
                Err(err) => {
                    let (reason, detail) = classify_config_set_failure(&err);
                    return ConfigSetAck::Rejected { reason, detail };
                }
            };
        // A no-op edit (the submitted values already equal the current ones) writes nothing.
        if !change.tunables_changed && !change.labels_changed {
            return ConfigSetAck::Applied {
                effect: ConfigSetEffect::Unchanged,
            };
        }
        // Persist the validated config atomically (temp + rename, 0600). A write failure leaves the
        // OLD file intact — report `SaveFailed`, adopt nothing.
        if config.save_to(&config_path).await.is_err() {
            return ConfigSetAck::Rejected {
                reason: ConfigSetRejection::SaveFailed,
                detail: None,
            };
        }
        // A LABEL change is adopted LIVE: reconcile the in-memory roster to the freshly-written
        // file (the SAME core the #139 roster-reload drives), so `status` reflects the new label
        // within the poll cadence. A TUNABLE change is reload-by-restart (no hot-reload primitive).
        //
        // APPEND-ONLY (issue #1442), and this is the one call site where the classification is a
        // judgement rather than a transcription, so the reasoning is here rather than cited.
        //
        // R-4 enumerates `config set` among the MUTATING verbs. That enumeration governs a
        // ROSTER-RELOAD's declared intent — which verb wrote the file the daemon is being told to
        // re-read — and this call site is not a reload: no notification is involved, and the CLI
        // `config set` verb sends none. What R-4's mutating class licenses is a shrink the OPERATOR
        // ASKED FOR, and a label edit never asks for one: `apply_settings` overlays the edits onto
        // the roster it parsed out of this very file and can neither add nor drop a row.
        //
        // So the count handed over always equals the count ON DISK — which is exactly the number
        // that can be lower than the LIVE roster, and the two claims are not the same. Declaring
        // `Mutating` here would adopt a diverged file's shrunken roster on an unrelated label edit,
        // reproducing the 2026-08-27 collapse through a verb that changed no membership at all —
        // and this issue's own floor is what makes that state persist, since refusing the reload
        // leaves memory holding more accounts than disk until something reconciles them (#1443).
        // Under `AppendOnly` the same edit is refused instead: the label change still lands on
        // disk, and the live rotation keeps its accounts.
        //
        // Nothing legitimate is given up. A label edit cannot change the count, so the floor is
        // unreachable on every path where the file and the live roster agree.
        if change.labels_changed {
            let _reconcile = self.reconcile_roster(config.roster, ReloadIntent::AppendOnly);
        }
        // A tunable change is the operative action even if a label also changed (both persisted).
        let effect = if change.tunables_changed {
            ConfigSetEffect::RestartRequired
        } else {
            ConfigSetEffect::Live
        };
        ConfigSetAck::Applied { effect }
    }

    /// Adopt a runtime roster-reload signalled over the control socket (issue #139), returning the
    /// one durable [`Event::RosterReload`] record of it (issue #1438).
    ///
    /// A roster write committed a NEW `config.toml` on disk and notified us; re-read that
    /// authoritative file and reconcile the in-memory roster to it via
    /// [`reconcile_roster`](Self::reconcile_roster). Every verb sending it goes through the one
    /// `crate::capture::notify_daemon_roster_reload` and is indistinguishable from here: `capture`
    /// and `login` (in `crate::capture`), and `import`, `enable` / `disable`, `remove` and
    /// `config restore` (in `crate::cli`). Deliberately uncounted — a cardinal here goes stale the
    /// next time a verb is added, which is what issue #1439 did to its predecessor.
    ///
    /// BEST-EFFORT by contract, mirroring [`adopt_manual_swap`](Self::adopt_manual_swap):
    /// the on-disk file is authoritative, so a read failure — a malformed or briefly
    /// absent file — leaves the current in-memory roster INTACT and is never fatal.
    /// A torn/partial read cannot occur: `Config::save` writes a temp file and
    /// `rename`s it over `config.toml` atomically, so this read observes either the
    /// whole old or the whole new file (issue #139 acceptance).
    ///
    /// EVERY return path yields an event, and issue #1438's "no path returns without emitting" is
    /// held by two mechanisms covering two different halves of it. THIS function's own paths are
    /// held by the SIGNATURE: returning a bare `Event` rather than an `Option` means the compiler
    /// refuses an arm that produces none (`E0069`), so a silent return cannot be added. The
    /// CALLER's half is held by `#[must_use]`: without it the run loop could await this and drop
    /// the value, and nothing would object — not the suite, and not
    /// `clippy --all-targets -- -D warnings`, since `Event` is not itself `#[must_use]`. With it,
    /// a run-loop arm that stops emitting fails to BUILD. The behavioural half of the same
    /// guarantee is `run_loop_adopts_a_roster_reload_signal_through_the_idle_select`, which reads
    /// the emitted line back off the log — so an arm that compiles and emits the WRONG thing
    /// fails too.
    ///
    /// Three outcomes, not two. A `None` `config_path` — the hermetic-test default, and any daemon
    /// with no reload wired — is `Refused`: the daemon DECLINED, which is not the same fact as a
    /// read that FAILED, while an absent or unreadable file is the latter. What separates them is
    /// the DECISION, not where it falls relative to the read. That split now carries TWO refusals,
    /// which is what it was shaped for. `ReloadDisabled` declines WITHOUT reading, so its line
    /// carries no `incoming` count; `Shrink` (issue #1442) can only detect a shrink by COMPARING
    /// the two counts, so it must read before it can decline, and it renders BOTH — a second
    /// `Refused` arm distinguished by its REASON, exactly as predicted, and not a fourth outcome.
    ///
    /// `intent` is the originating verb's own declaration, carried on the wire since issue #1442
    /// because the file cannot supply it: a smaller roster is either a legitimate `remove` or the
    /// 2026-08-27 collapse, and only the VERB knows which. It is resolved fail-closed at the socket
    /// boundary ([`ReloadIntent::from_wire`]), so an omitted or unrecognized token reaches this
    /// function as `AppendOnly` and a shrink is refused — an older CLI, and a verb added later
    /// without declaring intent, are both safe by construction (R-3a). This function does not
    /// re-decide any of that; the floor lives in `reconcile_roster` (design D-4) and this arm
    /// reports its verdict.
    ///
    /// It is no longer a SILENT no-op, and the `eprintln!` this replaced is gone rather than
    /// supplemented: the daemon runs in the background, so its stderr had no reader, and the
    /// reload is the one unattended operation that can replace the whole live rotation. That the
    /// print stays gone is asserted, not assumed —
    /// `the_reload_handler_prints_nothing_it_only_returns_the_event` reads this function's own
    /// source back.
    ///
    /// `previous` is read BEFORE the reconcile and `incoming` before the new roster is handed over
    /// by value, because the pair is the only form in which a shrink is legible at all. Either can
    /// legitimately be ZERO — `Config::load_path` accepts a tunables-only file, and this handler
    /// never calls `Config::require_roster` — and a zero is RENDERED, never folded into the
    /// absence that means "this path did not read".
    ///
    /// The error is bound only to CLASSIFY it and is then dropped, never rendered: it carries the
    /// config path, and a TOML parse failure quotes file CONTENT, which the issue's redaction
    /// criterion forbids on a channel with a wider audience than the mode-`0o600` file itself.
    /// Only `ConfigNotFound` is named; the `_` arm deliberately absorbs every other class
    /// `Config::load_path` can return — an I/O failure, a TOML syntax error, and the two
    /// VALIDATION failures (`Error::ConfigInvalid`, `Error::ConfigTargetMaxSessionAboveTrigger`)
    /// — into `Unreadable`, whose own doc records that its wording spans all of them and why one
    /// documented catch-all beats a partition that would look exhaustive while silently
    /// re-absorbing the next class added.
    ///
    /// No lock is taken: the run loop drives `tick`, the control serve, and this
    /// adoption on a SINGLE task, so no daemon swap can interleave with the reconcile;
    /// and `config.toml` is written only by the CLI verbs (never by a daemon swap,
    /// which touches the keychain + `~/.claude.json`), so the re-read races nothing the
    /// daemon itself writes.
    #[must_use = "the roster reload's event is the only durable trace it ran — emit it (#1438)"]
    pub(super) async fn adopt_roster_reload(&mut self, intent: ReloadIntent) -> Event {
        // The live roster size BEFORE anything is reconciled — the `was` half of the pair, and the
        // only thing that makes a shrink readable off the emitted line (issue #1438).
        let previous = Some(self.roster.len());
        let Some(path) = self.config_path.clone() else {
            // Reload disabled (no config path wired): nothing is read, so there is no incoming
            // count to report — and NOT `incoming=0`, which would claim the file presented an
            // empty roster.
            return Event::RosterReload {
                outcome: RosterReloadOutcome::Refused,
                previous,
                incoming: None,
                reason: Some(RosterReloadReason::ReloadDisabled),
            };
        };
        match Config::load_path(&path) {
            Ok(config) => {
                // Read before `reconcile_roster` takes the roster by value.
                let incoming = Some(config.roster.len());
                // The never-shrink floor (issue #1442) lives INSIDE the reconcile, so this arm
                // reports its verdict rather than re-deciding it — the same core, and the same
                // decision, that the other two call sites reach. `intent` came off the wire and was
                // already resolved fail-closed by `ReloadIntent::from_wire`, so an omitted or
                // unrecognized token arrives here as `AppendOnly` and a shrink is refused.
                match self.reconcile_roster(config.roster, intent) {
                    ReconcileOutcome::Adopted => Event::RosterReload {
                        outcome: RosterReloadOutcome::Adopted,
                        previous,
                        incoming,
                        reason: None,
                    },
                    // The post-read refusal the doc above anticipated: it carries BOTH counts,
                    // because a shrink is not legible from either one alone. The in-memory roster
                    // is untouched — the reconcile mutated nothing before returning this.
                    ReconcileOutcome::RefusedShrink => Event::RosterReload {
                        outcome: RosterReloadOutcome::Refused,
                        previous,
                        incoming,
                        reason: Some(RosterReloadReason::Shrink),
                    },
                }
            }
            // Best-effort: keep the current in-memory roster on any read/parse failure
            // (a transient absent file, or a malformed edit) rather than dropping the
            // rotation. The next reload notification re-attempts. The error is CLASSIFIED
            // into a bare machine code and dropped — formatting it onto the event would put
            // the config path, or a quoted fragment of the file, on the event channel.
            Err(err) => Event::RosterReload {
                outcome: RosterReloadOutcome::Failed,
                previous,
                incoming: None,
                reason: Some(match err {
                    Error::ConfigNotFound { .. } => RosterReloadReason::Absent,
                    _ => RosterReloadReason::Unreadable,
                }),
            },
        }
    }

    /// Reconcile the in-memory roster (and its per-account decision state) to
    /// `new_roster` — the pure core of the runtime roster-reload (issue #139),
    /// hermetically testable with no I/O.
    ///
    /// Accounts are matched by the immutable `account_uuid` (never by roster position,
    /// which shifts as accounts are added/removed):
    ///   - an account present in BOTH keeps its carried per-account state — health
    ///     (#42 quarantine/recovery streaks), the last-known usage reading (#80) and
    ///     the `polled_once` warm-up flag — so a `capture`/`login`/`remove` of ANOTHER
    ///     account never resets a healthy account's decision state;
    ///   - an account NEW on disk (an onboard, or a relogin of one never rostered) is
    ///     appended with DEFAULT state (unpolled, no reading, healthy) — it joins the
    ///     rotation and is polled on subsequent ticks, becoming a swap target only once
    ///     it has a reading;
    ///   - an account GONE from disk (a `remove`) is dropped along with its state.
    ///
    /// The active account is re-resolved by `account_uuid`: it keeps its (possibly
    /// shifted) new index when it persists, or becomes `None` when it was removed — the
    /// next [`tick`](Self::tick) then re-resolves active from the canonical credential,
    /// or polls-without-swapping if the active account is no longer rostered. The
    /// staggered poll schedule (#80) is reset (its entries were OLD roster indices);
    /// [`next_poll_index`](Self::next_poll_index) rebuilds it at the next cycle start.
    /// The warm-up latch (#80) is left as-is: once warmed up, a freshly-onboarded
    /// unpolled account is simply not yet a swap target (it has no reading), so it need
    /// not re-gate the whole rotation. State NOT indexed by roster position — the
    /// cooldown (#10), the canonical watch (#13), the tick counter — is deliberately
    /// untouched: a roster change is not a swap and must not re-arm or clear them.
    ///
    /// # The never-shrink floor (issue #1442, design D-4)
    ///
    /// All of the above describes what an ADOPTED reconcile does. Whether to adopt at all is
    /// decided first, here, and NOT at the three call sites — that placement is the decision, not
    /// an implementation detail. The investigation that found this defect enumerated two of the
    /// three callers; a per-caller guard is therefore one missed caller away from reinstating the
    /// original defect, and covers no caller added later. One enforcement point covers all three
    /// and any fourth.
    ///
    /// The rule is **shrink-scoped and intent-partitioned**: an [`ReloadIntent::AppendOnly`]
    /// reconcile presenting FEWER accounts than the live roster is refused whole — nothing is
    /// mutated, and the caller reports it. `<`, never `<=`: an equal-count roster is a relabel or a
    /// re-order, not a shrink, and widening is untouched (a floor that narrowed it would break
    /// issue #139, the reason the roster-reload exists at all — R-18).
    ///
    /// **An empty-roster floor in this position would be inert on the incident and wrong
    /// everywhere else**, which is why the shape is what it is. The 2026-08-27 transition was
    /// 6 → 1, so a "refuse an empty roster" guard never fires on it; and it can never fire on ANY
    /// append-only path, because `plan_capture` has only update-in-place and push arms and so
    /// always leaves at least one account in the file it saves. The one reload that CAN present
    /// zero is a removal of the last account — the single case that should be allowed. That guard
    /// fires only where it is wrong.
    ///
    /// Under [`ReloadIntent::Mutating`] no floor applies at all: `remove`, `disable`, `import` and
    /// `config restore` may legitimately shrink the roster, INCLUDING to zero (R-4).
    ///
    /// Returns the [`ReconcileOutcome`], `#[must_use]` because a caller that drops it reports an
    /// adoption that did not happen — the refusal's only durable trace is the line the caller
    /// writes from this value.
    #[must_use = "a refused reconcile mutated nothing — the caller must report it, not assume it landed (#1442)"]
    pub(super) fn reconcile_roster(
        &mut self,
        new_roster: Vec<Account>,
        intent: ReloadIntent,
    ) -> ReconcileOutcome {
        // The floor, BEFORE any mutation: a refusal is a true no-op, not a partial reshape.
        if matches!(intent, ReloadIntent::AppendOnly) && new_roster.len() < self.roster.len() {
            return ReconcileOutcome::RefusedShrink;
        }

        // Capture the active account's identity from the CURRENT roster before it is
        // replaced, so active can be re-resolved by uuid against the new roster.
        let active_uuid = self
            .state
            .active
            .and_then(|i| self.roster.get(i))
            .map(|account| account.account_uuid.clone());

        // Re-key each account's carried decision state by uuid: preserve it for an
        // account that persists, default it for a newly-onboarded one. (Rosters are a
        // handful of accounts, so the per-account `position` scan is inconsequential.)
        // ONE re-key, covering EVERY per-account signal at once (issue #668): health (#42), the
        // last-known reading (#80) + its timestamp (#449), the session-velocity EMA (#539), the armed
        // landing watch (#613), the session high-water mark (#614), and the pre-blind anchor (#583).
        // A persisting account's whole slot is kept (merely re-indexed) — so an open landing window,
        // a warm velocity EMA, its window's plausibility baseline, and an in-flight blind episode all
        // survive a `capture`/`login`/`remove` of ANOTHER account — and a newly-onboarded one starts
        // at `AccountRuntime::default()`. Because it is ONE vec, no signal can drift out of
        // length/index sync with the roster (which would index the wrong account or panic out of
        // bounds), and a ninth signal is re-keyed here for free.
        let accounts = new_roster
            .iter()
            .map(|account| {
                match self
                    .roster
                    .iter()
                    .position(|old| old.account_uuid == account.account_uuid)
                {
                    Some(old_idx) => self.state.accounts[old_idx].clone(),
                    None => AccountRuntime::default(),
                }
            })
            .collect();

        // Re-resolve active by uuid: kept (its new index) if it persists, else `None`.
        let active = active_uuid.and_then(|uuid| {
            new_roster
                .iter()
                .position(|account| account.account_uuid == uuid)
        });

        // Commit the reconciled roster + its per-account runtime state together, so no tick ever
        // observes a roster/state length mismatch.
        self.roster = new_roster;
        self.state.accounts = accounts;
        self.state.active = active;
        // Issue #450: `last_good` belongs to the active account by identity; reconcile
        // keeps it whole if that account persists (it is merely re-indexed, like
        // the per-account readings above), but drops it if the active account left the roster —
        // a removed account's anchor must not leak into the next active's blindness
        // reasoning (#452).
        if active.is_none() {
            self.state.last_good = None;
        }
        // The schedule held OLD roster indices; clear it so `next_poll_index` rebuilds a
        // fresh one (the active interleaved before each enabled non-quarantined peer,
        // #366) at the next cycle start. This site's reason is INDEX VALIDITY and reaches
        // only a roster change; a change of ACTIVE moves no index, which is why it went
        // uncovered until #1452 — see `invalidate_poll_schedule`, whose contract now names
        // both.
        self.invalidate_poll_schedule();
        ReconcileOutcome::Adopted
    }
}

/// Whether a [`Daemon::reconcile_roster`] call actually replaced the live roster (issue #1442).
///
/// Two variants, and the refusing one names its REASON rather than being a bare `false`: the
/// caller renders it onto the durable `roster-reload` line, where "refused" alone is not an
/// operator-actionable fact. There is exactly one refusal today; a second would be a variant here,
/// so the render arms fail to compile until they account for it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ReconcileOutcome {
    /// The new roster replaced the live one, and every per-account signal was re-keyed onto it.
    Adopted,
    /// Refused: an [`ReloadIntent::AppendOnly`] reconcile presented FEWER accounts than the live
    /// roster (R-3 / R-3a). A TRUE no-op — the roster, the per-account state, the active index and
    /// the poll schedule are all exactly as they were.
    RefusedShrink,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Reconcile and assert the never-shrink floor (issue #1442) ADOPTED it.
    ///
    /// Every reconcile test that predates the floor wants this shape: the reshape it describes is
    /// one the floor must let through, and saying so is the point rather than boilerplate. A
    /// widening, an equal-count relabel, and every mutating shrink all assert it here — so a floor
    /// that over-fires (narrowing issue #139's path, or refusing a legitimate `remove`) fails
    /// across this whole module rather than in one place.
    #[track_caller]
    fn assert_adopted(outcome: ReconcileOutcome) {
        assert_eq!(
            outcome,
            ReconcileOutcome::Adopted,
            "the floor must not refuse this reshape"
        );
    }

    use crate::config::SetTunables;
    use crate::contract::SweepOutcome;
    use crate::daemon::tests::*;
    use crate::keychain::FakeCredentialStore;
    use crate::observability::tests::field_of;
    use crate::observability::{RefreshEventOutcome, Verbosity};

    use std::rc::Rc;

    // --- manual-hold: adopt a manual `use` swap (issue #64) ----------------

    #[tokio::test]
    async fn adopt_manual_swap_arms_the_cooldown_so_the_next_poll_holds() {
        // Issue #64 manual-hold: after a manual `use` swap to B (canonical now B's
        // token), the daemon adopts the notification — which ARMS the post-swap
        // cooldown and re-resolves active — so its very next poll HOLDS on B rather
        // than immediately reverting it, EVEN THOUGH B sits over its swap-away trigger.
        let roster = vec![account("u-A", "work"), account("u-B", "spare")];
        // The manual swap already rewrote the canonical to B's token.
        let store = store_holding(b"B-token").await;
        let stash = stash_with(&[
            ("Sessiometer/u-A", b"A-token", "u-A"),
            ("Sessiometer/u-B", b"B-token", "u-B"),
        ])
        .await;
        let (_dir, json) = claude_json("u-B");
        // B (the manual target) is OVER its session trigger — absent the cooldown the
        // daemon would swap straight back to the wide-open A.
        let poller = FakeRosterPoller::new()
            .ok("u-A", 0.05, 0.05)
            // B over its fire point but BELOW the raw ceiling (0.95): the manual hold is
            // honored. (A manual `use` onto an active AT/over the raw ceiling is instead
            // reverted by the #611 emergency bypass — protection at the not-cross line overrides
            // the hold; covered by
            // `reactive_spike_at_raw_ceiling_bypasses_cooldown_but_a_normal_swap_still_honors_it`.)
            .ok("u-B", 0.92, 0.40);
        let tun = tunables(95, 80, 100); // cooldown 100s

        let mut daemon: FakeDaemon = Daemon::new(
            roster,
            poller,
            store,
            stash,
            FakeClock::new(Duration::ZERO),
            json,
            &tun,
        );
        // The daemon has not yet noticed the out-of-band manual swap: no in-memory
        // last_swap, so without the notification its next poll would revert B.
        assert!(daemon.state.last_swap.is_none());

        daemon.adopt_manual_swap().await;

        // Adoption armed the cooldown (last_swap at "now") and re-resolved active to B.
        assert_eq!(daemon.state.active, Some(1));
        let armed = daemon.state.last_swap.as_ref().expect("cooldown armed");
        assert_eq!(armed.at, daemon.clock.now());

        daemon.clock.advance(Duration::from_secs(10)); // within the 100s cooldown
        let outcome = warmed_tick(&mut daemon).await;

        // The daemon HOLDS on the operator's choice — no immediate revert.
        assert_eq!(outcome.action, TickAction::SkippedCooldown);
        assert!(daemon
            .store
            .read()
            .await
            .unwrap()
            .matches(&cred(b"B-token")));
    }

    #[tokio::test]
    async fn without_the_manual_hold_the_daemon_reverts_an_over_trigger_target() {
        // The contrast that makes the manual-hold load-bearing: the SAME fixture, but
        // the daemon is NOT notified (no adopt). It resolves active to B, finds B over
        // the trigger with NO cooldown armed, and immediately reverts B→A — exactly
        // the revert the #64 notification exists to prevent.
        let roster = vec![account("u-A", "work"), account("u-B", "spare")];
        let store = store_holding(b"B-token").await;
        let stash = stash_with(&[
            ("Sessiometer/u-A", b"A-token", "u-A"),
            ("Sessiometer/u-B", b"B-token", "u-B"),
        ])
        .await;
        let (_dir, json) = claude_json("u-B");
        let poller = FakeRosterPoller::new()
            .ok("u-A", 0.05, 0.05)
            // The SAME below-raw-ceiling reading as the manual-hold test above, so the two
            // stay one fixture: here the difference is purely the missing adopt (no cooldown).
            .ok("u-B", 0.92, 0.40);
        let tun = tunables(95, 80, 100);

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

        // Without the cooldown armed, the daemon reverts the (unannounced) manual swap.
        assert_eq!(outcome.action, TickAction::Swapped { from: 1, to: 0 });
    }

    #[tokio::test]
    async fn adopt_manual_swap_re_resolves_active_from_the_canonical_not_the_message() {
        // The #64 message carries no target; the daemon re-resolves active from the
        // AUTHORITATIVE canonical item. Here the cached active is STALE (A) while the
        // canonical already holds B's token — adoption corrects it to B, so an
        // out-of-order or contentless message cannot corrupt the daemon's state.
        let roster = vec![account("u-A", "work"), account("u-B", "spare")];
        let store = store_holding(b"B-token").await;
        let stash = stash_with(&[
            ("Sessiometer/u-A", b"A-token", "u-A"),
            ("Sessiometer/u-B", b"B-token", "u-B"),
        ])
        .await;
        let (_dir, json) = claude_json("u-B");
        let tun = tunables(95, 80, 100);

        let mut daemon: FakeDaemon = Daemon::new(
            roster,
            FakeRosterPoller::new(),
            store,
            stash,
            FakeClock::new(Duration::ZERO),
            json,
            &tun,
        );
        // A STALE cached active pointing at A, though the canonical is already B.
        daemon.state.active = Some(0);

        daemon.adopt_manual_swap().await;

        // Re-resolved from the canonical (B's token), not left at the stale A.
        assert_eq!(daemon.state.active, Some(1));
    }

    #[tokio::test]
    async fn a_daemon_with_the_swap_lock_wired_still_swaps_normally() {
        // Wiring smoke test (#64): a daemon configured with the single-writer lock
        // acquires + releases it around its own swap, so an UNcontended swap proceeds
        // exactly as before. (The lock's mutual-exclusion property is proven in
        // `swap.rs`; here we only confirm `with_swap_lock` does not deadlock the path.)
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
            .ok("u-B", 0.05, 0.05);
        let tun = tunables(95, 80, 100);
        let lock_dir = tempfile::tempdir().unwrap();

        let mut daemon: FakeDaemon = Daemon::new(
            roster,
            poller,
            store,
            stash,
            FakeClock::new(Duration::ZERO),
            json,
            &tun,
        )
        .with_swap_lock(lock_dir.path().join("swap.lock"));

        let outcome = warmed_tick(&mut daemon).await;

        // The swap landed normally, the lock acquired and released around it: A→B.
        assert_eq!(outcome.action, TickAction::Swapped { from: 0, to: 1 });
        assert!(daemon
            .store
            .read()
            .await
            .unwrap()
            .matches(&cred(b"B-token")));
    }

    // --- socket `swap` command (issue #167) --------------------------------

    #[tokio::test]
    async fn serve_control_hands_back_an_authenticated_swap_command() {
        use tokio::io::AsyncWriteExt;
        // An AUTHENTICATED, well-formed `swap` is NOT answered inline: like `watch`, it hands the
        // OPEN connection back (with the parsed target + force) so the run loop performs the swap
        // against `&mut Daemon` and writes the redacted ack from the REAL outcome — an outcome this
        // pure serve cannot know. No reply is written here.
        let (mut client, server) = tokio::io::duplex(1024);
        client
            .write_all(b"{\"cmd\":\"swap\",\"target\":\"spare\",\"force\":true}\n")
            .await
            .unwrap();
        let (_stream, command) = serve_control(server, &StatusSnapshot::default(), true)
            .await
            .unwrap()
            .swap();
        assert_eq!(
            command,
            SwapCommand {
                target: "spare".to_owned(),
                force: true,
            }
        );
    }

    #[tokio::test]
    async fn serve_control_defaults_an_omitted_swap_force_flag_to_false() {
        use tokio::io::AsyncWriteExt;
        // `force` is `#[serde(default)]`: a `swap` that OMITS it is a NON-force request (the common
        // `use <target>` case), never a parse error — so a plain policy-gated swap routes cleanly.
        let (mut client, server) = tokio::io::duplex(1024);
        client
            .write_all(b"{\"cmd\":\"swap\",\"target\":\"spare\"}\n")
            .await
            .unwrap();
        let (_stream, command) = serve_control(server, &StatusSnapshot::default(), true)
            .await
            .unwrap()
            .swap();
        assert_eq!(command.target, "spare");
        assert!(!command.force, "an omitted force flag defaults to false");
    }

    #[tokio::test]
    async fn serve_control_rejects_an_unauthenticated_swap_command() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        // AC (peer-credential authN): a `swap` is STATE-AFFECTING, so a non-owner peer is rejected
        // BEFORE any handoff — the swap never reaches the run loop (`one_shot()` proves there is NO
        // `Swap` handoff), and the peer gets `unauthorized` and learns nothing past the rejection.
        // This is the socket-layer half of the guard; the real `getpeereid` euid comparison that
        // computes the bool is proven by `serve_control_rejects_a_foreign_uid_peer` / `is_same_user`.
        let (mut client, server) = tokio::net::UnixStream::pair().expect("socketpair");
        client
            .write_all(b"{\"cmd\":\"swap\",\"target\":\"spare\",\"force\":true}\n")
            .await
            .unwrap();
        let outcome = serve_control(server, &StatusSnapshot::default(), false)
            .await
            .unwrap();
        assert!(
            outcome.one_shot().is_none(),
            "an unauthenticated swap must not hand off to the run loop",
        );
        let mut reply = String::new();
        client.read_to_string(&mut reply).await.unwrap();
        assert!(
            reply.contains("unauthorized"),
            "an unauthenticated swap is refused: {reply:?}",
        );
    }

    #[tokio::test]
    async fn serve_control_rejects_an_authenticated_swap_with_no_target() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        // Authenticated but malformed: a `swap` carrying no `target` has nothing to resolve, so it is
        // refused as `malformed request` (bounded / malformed-safe like an unparseable line) with NO
        // handoff. Checked only AFTER auth — the authenticated-but-malformed branch.
        let (mut client, server) = tokio::net::UnixStream::pair().expect("socketpair");
        client
            .write_all(b"{\"cmd\":\"swap\",\"force\":true}\n")
            .await
            .unwrap();
        let outcome = serve_control(server, &StatusSnapshot::default(), true)
            .await
            .unwrap();
        assert!(
            outcome.one_shot().is_none(),
            "a targetless swap must not hand off to the run loop",
        );
        let mut reply = String::new();
        client.read_to_string(&mut reply).await.unwrap();
        assert!(reply.contains("malformed"), "got {reply:?}");
    }

    // --- socket `capture` command (issue #359) ------------------------------

    #[tokio::test]
    async fn serve_control_hands_back_an_authenticated_capture_command() {
        use tokio::io::AsyncWriteExt;
        // An AUTHENTICATED `capture` is NOT answered inline: like `swap`, it hands the OPEN
        // connection back (with the parsed label) so the run loop performs the capture against
        // `&mut Daemon` and writes the redacted ack from the REAL outcome. No reply is written here.
        let (mut client, server) = tokio::io::duplex(1024);
        client
            .write_all(b"{\"cmd\":\"capture\",\"label\":\"work\"}\n")
            .await
            .unwrap();
        let (_stream, command) = serve_control(server, &StatusSnapshot::default(), true)
            .await
            .unwrap()
            .capture();
        assert_eq!(
            command,
            CaptureCommand {
                label: Some("work".to_owned()),
            }
        );
    }

    #[tokio::test]
    async fn serve_control_defaults_an_omitted_capture_label_to_none() {
        use tokio::io::AsyncWriteExt;
        // `label` is `#[serde(default)]` and OPTIONAL: a `capture` that omits it is well-formed (the
        // daemon auto-derives the label from the account uuid, never the email — #15/#134), never a
        // parse error and never a `malformed request` — so, unlike a targetless `swap`, it hands off.
        let (mut client, server) = tokio::io::duplex(1024);
        client.write_all(b"{\"cmd\":\"capture\"}\n").await.unwrap();
        let (_stream, command) = serve_control(server, &StatusSnapshot::default(), true)
            .await
            .unwrap()
            .capture();
        assert_eq!(command, CaptureCommand { label: None });
    }

    #[tokio::test]
    async fn serve_control_rejects_an_unauthenticated_capture_command() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        // AC (peer-credential authN): a `capture` is STATE-AFFECTING, so a non-owner peer is rejected
        // BEFORE any handoff — the capture never reaches the run loop (`one_shot()` proves there is NO
        // `Capture` handoff, so ZERO credential work happens), and the peer gets `unauthorized` and
        // learns nothing past the rejection. The socket-layer half of the guard, exactly like `swap`.
        let (mut client, server) = tokio::net::UnixStream::pair().expect("socketpair");
        client
            .write_all(b"{\"cmd\":\"capture\",\"label\":\"work\"}\n")
            .await
            .unwrap();
        let outcome = serve_control(server, &StatusSnapshot::default(), false)
            .await
            .unwrap();
        assert!(
            outcome.one_shot().is_none(),
            "an unauthenticated capture must not hand off to the run loop",
        );
        let mut reply = String::new();
        client.read_to_string(&mut reply).await.unwrap();
        assert!(
            reply.contains("unauthorized"),
            "an unauthenticated capture is refused: {reply:?}",
        );
    }

    // --- swap_command_verdict (pure re-validation, issue #167) --------------

    #[test]
    fn swap_command_verdict_accepts_a_viable_target_without_force() {
        // The happy path: an active account exists and the target is viable (not quarantined, weekly
        // headroom, no cooldown) → proceed with the swap, no force needed.
        assert!(matches!(
            swap_command_verdict(1, Some(0), false, false, false, false),
            SwapVerdict::Swap
        ));
    }

    #[test]
    fn swap_command_verdict_treats_a_non_force_already_active_target_as_a_noop() {
        // Non-force + target already active → a no-op success (nothing to write), mirroring the
        // standalone `use` no-op (the caller fills the `to` label).
        assert!(matches!(
            swap_command_verdict(0, Some(0), false, false, false, false),
            SwapVerdict::AlreadyActive
        ));
    }

    #[test]
    fn swap_command_verdict_force_onto_the_active_account_proceeds_as_a_self_swap() {
        // A `force` request onto the ALREADY-active account is NOT the no-op — it proceeds as a
        // self-swap (the `use --force <active>` display-repair path), so force is honored end to end.
        assert!(matches!(
            swap_command_verdict(0, Some(0), false, false, false, true),
            SwapVerdict::Swap
        ));
    }

    #[test]
    fn swap_command_verdict_rejects_with_no_active_account_even_under_force() {
        // A normal re-stash swap needs an OUTGOING (active) account. With none, the daemon cannot run
        // it, and `force` cannot MANUFACTURE one (adopt-target #212 is the decoupled standalone path).
        // BOTH the non-force and the force request reject — force is policy-only, never a
        // precondition bypass.
        assert!(matches!(
            swap_command_verdict(1, None, false, false, false, false),
            SwapVerdict::Reject(SwapRejection::NoActiveAccount)
        ));
        assert!(matches!(
            swap_command_verdict(1, None, false, false, false, true),
            SwapVerdict::Reject(SwapRejection::NoActiveAccount)
        ));
    }

    #[test]
    fn swap_command_verdict_rejects_each_non_viable_target_without_force() {
        // AC (daemon re-validates the target itself): a quarantined target, a weekly-exhausted
        // target, and an in-cooldown swap each reject WITHOUT force, with the matching redacted
        // reason. These facts are computed by the caller from the daemon's OWN state — never a
        // client "greyed out" hint.
        assert!(matches!(
            swap_command_verdict(1, Some(0), true, false, false, false),
            SwapVerdict::Reject(SwapRejection::Quarantined)
        ));
        assert!(matches!(
            swap_command_verdict(1, Some(0), false, true, false, false),
            SwapVerdict::Reject(SwapRejection::WeeklyExhausted)
        ));
        assert!(matches!(
            swap_command_verdict(1, Some(0), false, false, true, false),
            SwapVerdict::Reject(SwapRejection::Cooldown)
        ));
    }

    #[test]
    fn swap_command_verdict_force_bypasses_every_policy_gate_at_once() {
        // `force` is POLICY-only: it bypasses ALL THREE viability/cooldown gates together
        // (quarantined AND weekly-exhausted AND in-cooldown) → proceed. It never reaches the SAFETY
        // aborts (the locked keychain / swap lock), which live in the engine BELOW this verdict — so
        // this proves force relaxes POLICY, not that it can bypass a safety abort (that is
        // `classify_swap_failure` + the locked-keychain integration test).
        assert!(matches!(
            swap_command_verdict(1, Some(0), true, true, true, true),
            SwapVerdict::Swap
        ));
    }

    // --- perform_socket_swap (daemon swap-apply, issue #167) ----------------

    #[tokio::test]
    async fn perform_socket_swap_reroutes_the_canonical_and_arms_the_cooldown() {
        // AC (unify `use` onto the daemon; no torn write): a well-formed `swap` runs the daemon's OWN
        // single-writer swap — the SAME engine the auto-swaps use — rerouting the canonical OFF the
        // active account ONTO the target, advancing in-memory active, arming the post-swap cooldown,
        // and emitting the durable `Event::Swap` (reason Manual — operator-driven, not
        // session-triggered). The ack carries the two non-secret labels and NOTHING else.
        let roster = vec![account("u-A", "work"), account("u-B", "spare")];
        let store = store_holding(b"A-token").await;
        let stash = stash_with(&[
            ("Sessiometer/u-A", b"A-token", "u-A"),
            ("Sessiometer/u-B", b"B-token", "u-B"),
        ])
        .await;
        let (_dir, json) = claude_json("u-A");
        // Both under the session trigger, so warm-up HOLDS (no auto-swap) and simply resolves active
        // = work(0) with viable last-known readings — the realistic pre-swap state.
        let poller = FakeRosterPoller::new()
            .ok("u-A", 0.10, 0.10)
            .ok("u-B", 0.10, 0.10);
        let tun = tunables(95, 80, 100);
        let mut daemon: FakeDaemon = Daemon::new(
            roster,
            poller,
            store,
            stash,
            FakeClock::frozen(),
            json.clone(),
            &tun,
        );
        warmed_tick(&mut daemon).await;
        assert_eq!(
            daemon.state.active,
            Some(0),
            "warm-up resolved active = work"
        );

        let (ack, event) = daemon
            .perform_socket_swap(&SwapCommand {
                target: "spare".to_owned(),
                force: false,
            })
            .await;

        // The ack names the two non-secret labels…
        assert_eq!(
            ack,
            SwapAck::Accepted {
                from: "work".to_owned(),
                to: "spare".to_owned(),
            }
        );
        // …and its SERIALIZED bytes leak neither the credential (named `*-token`) nor an email (#15).
        let wire = serde_json::to_string(&ack).unwrap();
        assert!(
            crate::redaction::meter::unauthored_emails(&wire, &[]).is_empty(),
            "the ack leaks no non-authored email (#15/#444): {wire}"
        );
        assert!(
            !wire.to_lowercase().contains("token"),
            "the ack leaks no credential: {wire}",
        );
        // The canonical now holds B's token and the display shows B — a REAL, complete write.
        assert!(daemon
            .store
            .read()
            .await
            .unwrap()
            .matches(&cred(b"B-token")));
        assert_eq!(displayed_uuid(&json).as_deref(), Some("u-B"));
        // In-memory active advanced and the cooldown is armed, so the next auto-tick holds.
        assert_eq!(daemon.state.active, Some(1));
        assert!(
            daemon.state.last_swap.is_some(),
            "a completed swap arms the post-swap cooldown",
        );
        // The durable event is the MANUAL (operator-driven) swap, session_pct 0 (not
        // session-driven) — and nothing else: the pre-swap canary concluded Ok on this
        // healthy fixture, so no alarm event rides along (#714).
        assert_eq!(
            event,
            vec![Event::Swap {
                from: "work".to_owned(),
                to: "spare".to_owned(),
                reason: SwapReason::Manual,
                session_pct: 0,
                projection: None,
            }]
        );
    }

    #[tokio::test]
    async fn perform_socket_swap_revalidates_a_weekly_exhausted_target_then_force_overrides_it() {
        // AC (daemon's own re-validation + force is policy-only): the daemon computes weekly
        // exhaustion from its OWN last-known reading (never a client hint). WITHOUT force the target
        // is refused with ZERO writes; WITH force the SAME target swaps — the operator's explicit
        // policy override, honored end to end (a REAL write lands, reason Forced).
        let roster = vec![account("u-A", "work"), account("u-B", "spare")];
        let store = store_holding(b"A-token").await;
        let stash = stash_with(&[
            ("Sessiometer/u-A", b"A-token", "u-A"),
            ("Sessiometer/u-B", b"B-token", "u-B"),
        ])
        .await;
        let (_dir, json) = claude_json("u-A");
        // spare's weekly (0.99) is at/above the 0.98 base (`tunables` WEEKLY_CEILING=98) → exhausted;
        // work stays active and viable so warm-up holds.
        let poller = FakeRosterPoller::new()
            .ok("u-A", 0.10, 0.10)
            .ok("u-B", 0.10, 0.99);
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
        warmed_tick(&mut daemon).await;
        assert_eq!(daemon.state.active, Some(0));

        // WITHOUT force: the daemon re-validates and refuses — ZERO writes, no event.
        let (rejected, no_event) = daemon
            .perform_socket_swap(&SwapCommand {
                target: "spare".to_owned(),
                force: false,
            })
            .await;
        assert_eq!(
            rejected,
            SwapAck::Rejected {
                reason: SwapRejection::WeeklyExhausted,
            }
        );
        assert!(no_event.is_empty(), "a refused swap emits no event");
        assert!(
            daemon
                .store
                .read()
                .await
                .unwrap()
                .matches(&cred(b"A-token")),
            "the refused swap wrote nothing",
        );
        assert_eq!(daemon.state.active, Some(0));

        // WITH force: the SAME non-viable target now swaps (policy override).
        let (accepted, event) = daemon
            .perform_socket_swap(&SwapCommand {
                target: "spare".to_owned(),
                force: true,
            })
            .await;
        assert_eq!(
            accepted,
            SwapAck::Accepted {
                from: "work".to_owned(),
                to: "spare".to_owned(),
            }
        );
        assert!(daemon
            .store
            .read()
            .await
            .unwrap()
            .matches(&cred(b"B-token")));
        assert_eq!(daemon.state.active, Some(1));
        // A forced swap records the FORCED reason (distinct from Manual), still session_pct 0.
        assert_eq!(
            event,
            vec![Event::Swap {
                from: "work".to_owned(),
                to: "spare".to_owned(),
                reason: SwapReason::Forced,
                session_pct: 0,
                projection: None,
            }]
        );
    }

    #[tokio::test]
    async fn perform_socket_swap_force_cannot_bypass_a_locked_keychain() {
        // AC (force cannot bypass the locked-keychain abort): `force` is POLICY-only. A forced swap
        // onto a VIABLE target is still REFUSED when the keychain is locked (locked ≠ gone — retry
        // when unlocked), with ZERO writes: canonical untouched, active unchanged, no event, no
        // cooldown. The abort lives in the swap ENGINE (its read-everything-before-mutating step-1
        // read), below the force-bypassable policy verdict, so no verdict can reach past it.
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
            json.clone(),
            &tun,
        );
        warmed_tick(&mut daemon).await;
        assert_eq!(daemon.state.active, Some(0));

        // Lock the keychain, THEN force-swap: the engine's step-1 read aborts before any mutation.
        daemon.store.set_locked(true);
        let (ack, event) = daemon
            .perform_socket_swap(&SwapCommand {
                target: "spare".to_owned(),
                force: true,
            })
            .await;

        assert_eq!(
            ack,
            SwapAck::Rejected {
                reason: SwapRejection::KeychainLocked,
            }
        );
        assert!(event.is_empty(), "a refused swap emits no event");
        // ZERO writes: once unlocked the canonical still holds A's token, the display still shows A,
        // in-memory active never advanced, and no cooldown was armed. `force` forged no torn write.
        daemon.store.set_locked(false);
        assert!(daemon
            .store
            .read()
            .await
            .unwrap()
            .matches(&cred(b"A-token")));
        assert_eq!(displayed_uuid(&json).as_deref(), Some("u-A"));
        assert_eq!(daemon.state.active, Some(0));
        assert!(
            daemon.state.last_swap.is_none(),
            "a refused swap arms no cooldown",
        );
    }

    #[tokio::test]
    async fn perform_socket_swap_gates_on_the_online_probe_and_force_carries_past_it() {
        // Issue #736, END TO END on the DAEMON-ROUTED path — the one `use` actually takes
        // whenever a daemon is up (a reachable daemon's ack is authoritative, so `run_use`'s
        // own copy of this gate never executes). Two halves, and both are load-bearing:
        //
        //   1. an armed STRICT probe refuses the operator's swap here too, with zero writes —
        //      the gate is not daemon-down-only;
        //   2. `--force` carries past it — which works ONLY because `command.force` is threaded
        //      into `locked_swap`. Without that thread the escape `Error::CanaryProbeNotLive`
        //      and the README both promise would exist only while the daemon is DOWN, i.e.
        //      never in the case that motivates it. Asserting the gate through `locked_swap`
        //      directly cannot catch that: the flag would simply never arrive.
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
        let tun = Tunables {
            canary_online_probe: true,
            canary_online_probe_strict: true,
            ..tunables(95, 80, 0)
        };
        let mut daemon: FakeDaemon = Daemon::new(
            roster,
            poller,
            store,
            stash,
            FakeClock::frozen(),
            json.clone(),
            &tun,
        );
        warmed_tick(&mut daemon).await;
        assert_eq!(daemon.state.active, Some(0));
        // Both accounts carry a live reading from the warm-up, so `probe_gate` is ARMED — the
        // stand-down keys off a MISSING one, and standing down here would make the test vacuous.
        assert!(daemon.state.accounts[0].last_reading.is_some());

        // The canonical dies AFTER the last poll — precisely the Layer-3 window an offline
        // check cannot see, and the only state in which this gate has anything to say.
        daemon.poller = FakeRosterPoller::new()
            .unauthorized("u-A")
            .ok("u-B", 0.10, 0.10);

        let (ack, events) = daemon
            .perform_socket_swap(&SwapCommand {
                target: "spare".to_owned(),
                force: false,
            })
            .await;

        // Refused — as the opaque `Failed`, because the wire enum stays CLOSED (#167/#714: an
        // old Swift decoder must never meet a new variant). The DETAIL rides the event log.
        assert_eq!(
            ack,
            SwapAck::Rejected {
                reason: SwapRejection::Failed,
            }
        );
        assert!(events.contains(&Event::CanaryOnlineProbe {
            verdict: "rejected",
            refused: true,
        }));
        assert!(
            !events.iter().any(|e| matches!(e, Event::Swap { .. })),
            "a refused swap emits no swap event: {events:?}"
        );
        // ZERO writes: canonical, display, in-memory active and cooldown all untouched.
        assert!(daemon
            .store
            .read()
            .await
            .unwrap()
            .matches(&cred(b"A-token")));
        assert_eq!(displayed_uuid(&json).as_deref(), Some("u-A"));
        assert_eq!(daemon.state.active, Some(0));
        assert!(daemon.state.last_swap.is_none());

        // Same daemon, same dead bearer, same armed strict gate — but forced.
        let (ack, events) = daemon
            .perform_socket_swap(&SwapCommand {
                target: "spare".to_owned(),
                force: true,
            })
            .await;

        assert!(
            matches!(ack, SwapAck::Accepted { .. }),
            "--force must carry the swap past an armed strict probe, got {ack:?}"
        );
        // Not silent: bypassing a gate the operator armed is exactly what they later need to find.
        assert!(events.contains(&Event::CanaryOnlineProbe {
            verdict: "overridden",
            refused: false,
        }));
        assert!(daemon
            .store
            .read()
            .await
            .unwrap()
            .matches(&cred(b"B-token")));
        assert_eq!(daemon.state.active, Some(1));
    }

    #[tokio::test]
    async fn perform_socket_swap_rejects_an_unknown_target_and_writes_nothing() {
        // The daemon resolves the target against its OWN roster and NEVER guesses (#17): a handle
        // matching no account is `UnknownTarget` with ZERO writes and no event — even under force
        // (there is nothing to resolve, so resolution failure is not force-bypassable).
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
        warmed_tick(&mut daemon).await;

        let (ack, event) = daemon
            .perform_socket_swap(&SwapCommand {
                target: "ghost".to_owned(),
                force: true,
            })
            .await;
        assert_eq!(
            ack,
            SwapAck::Rejected {
                reason: SwapRejection::UnknownTarget,
            }
        );
        assert!(event.is_empty());
        assert!(
            daemon
                .store
                .read()
                .await
                .unwrap()
                .matches(&cred(b"A-token")),
            "an unknown target wrote nothing",
        );
        assert_eq!(daemon.state.active, Some(0));
    }

    #[tokio::test]
    async fn perform_socket_swap_refuses_a_duplicated_label_and_serialises_it_onto_the_wire() {
        // Issue #1087 AC-2. The daemon shares `use_account::resolve_target` with the five other
        // label-resolving sites, so it REFUSES a duplicated label rather than taking the earliest
        // bearer (#17, OQ-1). What no test looked at until now is the half after that: this site
        // does not exit a process, it SERIALISES its refusal onto a socket, and a refusal that is
        // correct in-process can still be mis-mapped, malformed or dropped on the way out — a
        // silent failure whose whole surface is the wire.
        //
        // So the assertions come in two halves. First the in-process verdict — the resolver's
        // `UseTargetAmbiguous` becomes `SwapRejection::AmbiguousTarget` and NOT the neighbouring
        // `UnknownTarget` (`perform_socket_swap`'s catch-all arm, immediately below the arm under
        // test, which a swallow of the ambiguous arm would fall through to), with ZERO writes.
        // Then the wire itself: the exact bytes a client reads, and that they decode back.
        //
        // WHICH wire, because this repo has four schema-versioned ones and being on the control
        // socket implies NONE of them: `STATUS_SCHEMA_VERSION` governs the `status` reply and both
        // `watch` frames (src/daemon/snapshot.rs), and `log` / `stats` / `reliability` each carry
        // their own `JSON_SCHEMA_VERSION` in their own module. `SwapAck` is the un-versioned
        // `swap`-command reply: it carries a `result` tag and a kebab-case `reason` and nothing
        // else — no `schema` object — so the literal below is not a versioned fixture and pinning
        // it moves no version.
        let roster = vec![
            account("u-A", "work"),
            account("u-B", "dup"),
            account("u-C", "dup"),
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
            .ok("u-A", 0.10, 0.10)
            .ok("u-B", 0.10, 0.10)
            .ok("u-C", 0.10, 0.10);
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
        warmed_tick(&mut daemon).await;

        // `force: true` deliberately: resolution is not a POLICY gate, so `force` cannot buy past
        // it — the same reason the unknown-target sibling above forces too. Both bearers are
        // otherwise perfectly viable (0.10 weekly, unquarantined, cooldown 0), so the ONLY thing
        // that can refuse this request is the duplicated label.
        let (ack, events) = daemon
            .perform_socket_swap(&SwapCommand {
                target: "dup".to_owned(),
                force: true,
            })
            .await;

        assert_eq!(
            ack,
            SwapAck::Rejected {
                reason: SwapRejection::AmbiguousTarget,
            },
            "a duplicated label is AMBIGUOUS, not unknown and not a swap",
        );
        assert!(events.is_empty(), "a refused swap logs nothing: {events:?}");
        assert!(
            daemon
                .store
                .read()
                .await
                .unwrap()
                .matches(&cred(b"A-token")),
            "an ambiguous target wrote nothing",
        );
        assert_eq!(daemon.state.active, Some(0), "…and moved nothing");

        // The wire half. `write_swap_ack` is what the run loop hands the client, so serialise
        // through IT rather than calling `serde_json` here — the frame's newline, its compact
        // form and its field names are all properties of that function, and a test that
        // re-encoded the ack itself would assert none of them.
        let mut wire = Vec::new();
        write_swap_ack(&mut wire, &ack)
            .await
            .expect("a swap ack must serialise");
        let wire = String::from_utf8(wire).expect("the ack frame is UTF-8");
        assert_eq!(
            wire, "{\"result\":\"rejected\",\"reason\":\"ambiguous-target\"}\n",
            "the ambiguity refusal's frame, byte for byte",
        );

        // …and it is a shape a client can ACT on, not merely one it can read: the frame decodes
        // back to the same closed enum, which is what lets `use`'s daemon-routed path map it to
        // `Error::UseTargetAmbiguous` and exit 6 (pinned there by
        // `swap_rejection_error_maps_each_reason_to_the_standalone_exit_taxonomy`).
        let decoded: SwapAck =
            serde_json::from_str(wire.trim_end()).expect("a client must be able to decode it");
        assert_eq!(decoded, ack, "the frame round-trips");
    }

    #[tokio::test]
    async fn perform_socket_swap_reports_an_already_active_target_as_a_noop() {
        // Non-force swap onto the ALREADY-active account: a no-op SUCCESS (nothing written), the
        // `AlreadyActive` ack — the daemon-routed mirror of the standalone `use` no-op.
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
        warmed_tick(&mut daemon).await;
        assert_eq!(daemon.state.active, Some(0));

        let (ack, event) = daemon
            .perform_socket_swap(&SwapCommand {
                target: "work".to_owned(),
                force: false,
            })
            .await;
        assert_eq!(
            ack,
            SwapAck::AlreadyActive {
                to: "work".to_owned(),
            }
        );
        assert!(event.is_empty());
        assert!(
            daemon
                .store
                .read()
                .await
                .unwrap()
                .matches(&cred(b"A-token")),
            "an already-active no-op writes nothing",
        );
        assert!(daemon.state.last_swap.is_none(), "a no-op arms no cooldown");
    }

    #[tokio::test]
    async fn perform_socket_swap_carries_the_canary_drift_alarm_on_a_refused_swap() {
        // Issue #714 on the daemon-routed path: a drifted canary refuses the socket swap with the
        // EXISTING redacted `Failed` rejection (the wire enum stays closed — an old Swift decoder
        // must keep decoding it), while the returned events STILL carry the edge-triggered
        // `canary_drift` alarm — the durable record of WHY, which the ack deliberately does not
        // spell out. Zero writes. This pins the `Option<Event> → Vec<Event>` plumbing: dropping
        // the events on the `Err` arm would silently lose the drift record while every ack
        // assertion stayed green.
        let roster = vec![account("u-A", "work"), account("u-B", "spare")];
        // The DRIFT fixture: the canonical holds spare's stashed token while the (soon frozen)
        // display still names work — the canary's positive `A ≠ B` divergence.
        let store = store_holding(b"B-token").await;
        let stash = stash_with(&[
            ("Sessiometer/u-A", b"A-token", "u-A"),
            ("Sessiometer/u-B", b"B-token", "u-B"),
        ])
        .await;
        let (dir, json) = claude_json("u-A");
        // Both under the trigger: warm-up HOLDS (no auto-swap fires first) and resolves active
        // token-first to spare(1).
        let poller = FakeRosterPoller::new()
            .ok("u-A", 0.10, 0.10)
            .ok("u-B", 0.10, 0.10);
        let tun = tunables(95, 80, 100);
        let mut daemon: FakeDaemon = Daemon::new(
            roster,
            poller,
            store,
            stash,
            FakeClock::frozen(),
            json.clone(),
            &tun,
        );
        warmed_tick(&mut daemon).await;
        assert_eq!(
            daemon.state.active,
            Some(1),
            "warm-up resolved active token-first = spare"
        );

        // Freeze the display so the canary's best-effort heal cannot land (the persistent-
        // divergence posture every drift fixture models), then drive the operator's swap.
        let mut perms = std::fs::metadata(dir.path()).unwrap().permissions();
        std::os::unix::fs::PermissionsExt::set_mode(&mut perms, 0o500);
        std::fs::set_permissions(dir.path(), perms.clone()).unwrap();
        let (ack, events) = daemon
            .perform_socket_swap(&SwapCommand {
                target: "work".to_owned(),
                force: false,
            })
            .await;
        std::os::unix::fs::PermissionsExt::set_mode(&mut perms, 0o700);
        std::fs::set_permissions(dir.path(), perms).unwrap();

        // The ack is the CLOSED wire enum's opaque `Failed` — never a new variant an old client
        // would fail to decode…
        assert_eq!(
            ack,
            SwapAck::Rejected {
                reason: SwapRejection::Failed,
            }
        );
        // …while the durable alarm rides the returned events (labels only, #15).
        assert_eq!(
            events,
            vec![Event::CanaryDrift {
                displayed: "work".to_owned(),
                matched: "spare".to_owned(),
                overridden: false,
            }]
        );
        // Zero writes: the canonical still holds the drifted token, untouched.
        assert!(daemon
            .store
            .read()
            .await
            .unwrap()
            .matches(&cred(b"B-token")));
        assert!(
            daemon.state.last_swap.is_none(),
            "a refusal arms no cooldown"
        );
    }

    // --- perform_socket_capture (daemon capture-apply, issue #359) -----------

    #[tokio::test]
    async fn perform_socket_capture_captures_the_active_account_and_reconciles_the_roster() {
        // AC (authenticated peer → redacted success ack; capture is the daemon's OWN work): a
        // well-formed `capture` reads the active identity + token through the #357 `capture_locked`
        // primitive, stashes BOTH halves, appends the new roster row, persists `config.toml`, and
        // reconciles the in-memory rotation to it — emitting one redacted `Event::Capture`. The ack
        // carries the operator LABEL + running count and NOTHING secret. Canonical-READ-ONLY: the
        // keychain token and `~/.claude.json` are never rewritten (#359 — capture only writes a
        // per-account stash + a roster row).
        let roster = vec![account("u-B", "spare")];
        let store = store_holding(b"A-token").await;
        let stash = stash_with(&[("Sessiometer/u-B", b"B-token", "u-B")]).await;
        let (_json_dir, json) = claude_json("u-A");
        let cfg_dir = tempfile::tempdir().unwrap();
        let config_path = cfg_dir.path().join("config.toml");
        write_roster_config(&config_path, &[("u-B", "spare")]);
        let tun = tunables(95, 80, 0);
        let mut daemon: FakeDaemon = Daemon::new(
            roster,
            FakeRosterPoller::new(),
            store,
            stash,
            FakeClock::frozen(),
            json.clone(),
            &tun,
        )
        .with_config_path(config_path.clone());

        // Label GIVEN → names the new account; the account is not yet rostered, so this captures.
        let (ack, event) = daemon
            .perform_socket_capture(&CaptureCommand {
                label: Some("work".to_owned()),
            })
            .await;

        // The ack names the operator LABEL + the running count (2 in rotation now)…
        assert_eq!(
            ack,
            CaptureAck::Captured {
                label: "work".to_owned(),
                count: 2,
            }
        );
        // …and its SERIALIZED bytes leak neither the credential (named `*-token`) nor an email (#15).
        let wire = serde_json::to_string(&ack).unwrap();
        assert!(
            crate::redaction::meter::unauthored_emails(&wire, &[]).is_empty(),
            "the ack leaks no non-authored email (#15/#444): {wire}"
        );
        assert!(
            !wire.to_lowercase().contains("token"),
            "the ack leaks no credential: {wire}",
        );
        // The durable audit line: the resolved roster LABEL handle + the `captured` outcome token.
        assert_eq!(
            event,
            Some(Event::Capture {
                account: Some("work".to_owned()),
                outcome: CaptureEventOutcome::Captured,
            })
        );
        // The in-memory rotation reconciled to the freshly-written roster: u-A joined u-B.
        assert_eq!(roster_uuids(&daemon), vec!["u-B", "u-A"]);
        // The on-disk `config.toml` grew the SAME row — persisted, so a restart keeps it.
        let on_disk = Config::load_path(&config_path).unwrap();
        assert_eq!(on_disk.roster.len(), 2);
        assert_eq!(on_disk.roster[1].account_uuid, "u-A");
        assert_eq!(on_disk.roster[1].label, "work");
        // Both credential halves are stashed together under u-A's uuid-derived service.
        let stashed = daemon.stash.read("Sessiometer/u-A").await.unwrap();
        assert_eq!(stashed.credential.expose(), b"A-token");
        assert_eq!(stashed.oauth_account.account_uuid(), "u-A");
        // Canonical-READ-ONLY: the keychain still holds A's token and `~/.claude.json` still shows
        // u-A — capture rewrote NEITHER (it is not a swap).
        assert!(daemon
            .store
            .read()
            .await
            .unwrap()
            .matches(&cred(b"A-token")));
        assert_eq!(displayed_uuid(&json).as_deref(), Some("u-A"));
    }

    #[tokio::test]
    async fn perform_socket_capture_refreshes_an_already_rostered_account_without_a_duplicate_row()
    {
        // AC (already-rostered active account → idempotent refresh, NOT a duplicate row): capturing
        // the active account when it is ALREADY rostered re-points its stash to the current token
        // and updates its row IN PLACE — the count is unchanged and no second row appears. An
        // omitted label keeps the operator's existing name (never clobbered by an auto-derived
        // uuid). The ack is `Refreshed`, the event outcome `refreshed`.
        let roster = vec![account("u-A", "work")];
        let store = store_holding(b"A-token-v2").await; // the canonical rotated since the last stash
        let stash = stash_with(&[("Sessiometer/u-A", b"A-token-v1", "u-A")]).await;
        let (_json_dir, json) = claude_json("u-A");
        let cfg_dir = tempfile::tempdir().unwrap();
        let config_path = cfg_dir.path().join("config.toml");
        write_roster_config(&config_path, &[("u-A", "work")]);
        let tun = tunables(95, 80, 0);
        let mut daemon: FakeDaemon = Daemon::new(
            roster,
            FakeRosterPoller::new(),
            store,
            stash,
            FakeClock::frozen(),
            json,
            &tun,
        )
        .with_config_path(config_path.clone());

        // Label OMITTED → the existing "work" is kept (an auto-derived uuid never clobbers it).
        let (ack, event) = daemon
            .perform_socket_capture(&CaptureCommand { label: None })
            .await;

        assert_eq!(
            ack,
            CaptureAck::Refreshed {
                label: "work".to_owned(),
                count: 1,
            }
        );
        assert_eq!(
            event,
            Some(Event::Capture {
                account: Some("work".to_owned()),
                outcome: CaptureEventOutcome::Refreshed,
            })
        );
        // NO duplicate row — in-memory AND on-disk stay a single u-A account.
        assert_eq!(roster_uuids(&daemon), vec!["u-A"]);
        assert_eq!(Config::load_path(&config_path).unwrap().roster.len(), 1);
        // The stash re-pointed to the CURRENT token under the SAME single service (no new entry).
        assert_eq!(daemon.stash.len(), 1);
        let stashed = daemon.stash.read("Sessiometer/u-A").await.unwrap();
        assert_eq!(stashed.credential.expose(), b"A-token-v2");
    }

    #[tokio::test]
    async fn perform_socket_capture_refuses_when_no_active_account_and_writes_nothing() {
        // AC (no active account → redacted refusal, ZERO writes): with no readable `~/.claude.json`
        // identity there is nothing to capture. The identity read fails FIRST (before the token),
        // so the capture is a true no-op: no stash, no roster row, no `config.toml` change, no
        // in-memory reconcile. The ack is the redacted `NoActiveAccount`; the event still names the
        // operator's label HINT (the only handle a pre-stash failure has) + the `no_active_account`
        // outcome.
        let roster = vec![account("u-B", "spare")];
        let store = store_holding(b"A-token").await; // a token exists, but the identity read aborts first
        let stash = stash_with(&[("Sessiometer/u-B", b"B-token", "u-B")]).await;
        let absent_dir = tempfile::tempdir().unwrap();
        let absent_json = absent_dir.path().join("absent.json"); // never created
        let cfg_dir = tempfile::tempdir().unwrap();
        let config_path = cfg_dir.path().join("config.toml");
        write_roster_config(&config_path, &[("u-B", "spare")]);
        let tun = tunables(95, 80, 0);
        let mut daemon: FakeDaemon = Daemon::new(
            roster,
            FakeRosterPoller::new(),
            store,
            stash,
            FakeClock::frozen(),
            absent_json,
            &tun,
        )
        .with_config_path(config_path.clone());

        let (ack, event) = daemon
            .perform_socket_capture(&CaptureCommand {
                label: Some("work".to_owned()),
            })
            .await;

        assert_eq!(
            ack,
            CaptureAck::Rejected {
                reason: CaptureRejection::NoActiveAccount,
            }
        );
        assert_eq!(
            event,
            Some(Event::Capture {
                account: Some("work".to_owned()),
                outcome: CaptureEventOutcome::NoActiveAccount,
            })
        );
        // ZERO writes: the roster is untouched in memory AND on disk, and nothing new was stashed.
        assert_eq!(roster_uuids(&daemon), vec!["u-B"]);
        assert_eq!(Config::load_path(&config_path).unwrap().roster.len(), 1);
        assert_eq!(daemon.stash.len(), 1);
        assert!(!daemon.stash.contains("Sessiometer/u-A"));
    }

    // --- the prior-configuration witness at the SOCKET entry point (#1441) ---------

    /// Witness sources pinned for a daemon test — never the operator's login keychain.
    ///
    /// `configured` picks which verdict the sources produce, and each arm is arranged the cheap
    /// way `WitnessSources::observe` already documents. A CONFIGURED machine is a non-empty
    /// usage-store file, which `observe` checks FIRST and which therefore never spawns
    /// `security` at all. An unconfigured one is an empty store plus a keychain path that does
    /// not exist — measured (see `crate::witness::resolve_probe`) to exit 0 with empty output,
    /// so the probe answers `Ok(false)` rather than erroring into the fail-closed arm.
    fn pinned_witness(dir: &Path, configured: bool) -> crate::witness::WitnessSources {
        let samples = dir.join("usage-samples.jsonl");
        let usage_store = if configured {
            std::fs::write(&samples, b"{\"ts\":0}\n").unwrap();
            vec![samples]
        } else {
            Vec::new()
        };
        crate::witness::WitnessSources::pinned(dir.join("no-such.keychain-db"), usage_store)
    }

    #[tokio::test]
    async fn perform_socket_capture_refuses_an_absent_config_on_the_durable_witness_alone() {
        // Issue #1441 AC: `config.toml` is gone while durable local state says this machine was
        // configured before. The CLI's rule, applied at the second entry point.
        //
        // The daemon's roster is deliberately EMPTY here, which is what makes this test about the
        // WITNESS. The gate has two observations and refuses on either; giving this one a populated
        // roster would let the corroboration below satisfy it, and a regression that broke the
        // durable half — the only half a CLI-shaped machine has — would stay green.
        let roster: Vec<Account> = Vec::new();
        let store = store_holding(b"A-token").await;
        let stash = stash_with(&[("Sessiometer/u-1", b"one", "u-1")]).await;
        let (_json_dir, json) = claude_json("u-A");
        let cfg_dir = tempfile::tempdir().unwrap();
        // The config path is WIRED but the file is ABSENT — exactly what the operator was left
        // with on 2026-08-27, and what `Config::load_path` turns into `None` above.
        let config_path = cfg_dir.path().join("config.toml");
        let tun = tunables(95, 80, 0);
        let mut daemon: FakeDaemon = Daemon::new(
            roster,
            FakeRosterPoller::new(),
            store,
            stash,
            FakeClock::frozen(),
            json.clone(),
            &tun,
        )
        .with_config_path(config_path.clone())
        .with_witness_sources(pinned_witness(cfg_dir.path(), true));

        let (ack, event) = daemon
            .perform_socket_capture(&CaptureCommand {
                label: Some("work".to_owned()),
            })
            .await;

        assert_eq!(
            ack,
            CaptureAck::Rejected {
                reason: CaptureRejection::PriorConfiguration,
            }
        );
        // The durable audit line carries the same machine reason. The socket path has no stderr,
        // so this is the only DURABLE trace the refusal leaves — the ack itself is read once by the
        // caller and gone.
        assert_eq!(
            event,
            Some(Event::Capture {
                account: Some("work".to_owned()),
                outcome: CaptureEventOutcome::PriorConfiguration,
            })
        );
        // ZERO writes, which is the acceptance criterion and not a nicety — a correct verdict
        // reached after the stash had landed would refuse far too late.
        assert!(
            roster_uuids(&daemon).is_empty(),
            "the refusal grew the roster"
        );
        assert!(
            !config_path.exists(),
            "the refusal created `config.toml`, so it was not a no-op"
        );
        assert_eq!(daemon.stash.len(), 1, "the refusal stashed a credential");
        assert!(!daemon.stash.contains("Sessiometer/u-A"));
        // Canonical-READ-ONLY holds through the refusal too.
        assert!(daemon
            .store
            .read()
            .await
            .unwrap()
            .matches(&cred(b"A-token")));
        assert_eq!(displayed_uuid(&json).as_deref(), Some("u-A"));
    }

    #[tokio::test]
    async fn perform_socket_capture_refuses_an_absent_config_when_the_live_roster_survives() {
        // PRD § 7 row 2, and the incident run at the second entry point with the durable witness
        // GONE: `config.toml` is absent, the daemon is still holding the six accounts the file used
        // to name, and nothing durable survives to say the machine was ever configured.
        //
        // That combination is reachable rather than academic. Both usage-store files are siblings of
        // `config.toml` in one directory, so a single directory-scoped deletion takes all three, and
        // a keychain that cannot be read degrades SILENTLY to "no witness" (measured — see
        // `crate::witness::resolve_probe`). On that machine the durable witness answers `Absent` and
        // only the live roster still knows better. Without the corroboration the gate admits here,
        // `capture_locked` appends to a roster it read as empty, `save_to` writes ONE account over
        // the six, and `reconcile_roster` collapses the live rotation to match — which is the
        // 2026-08-27 incident, reproduced from the menu-bar button.
        //
        // Six accounts in, six accounts out, and a redacted refusal instead of a capture.
        let roster: Vec<Account> = (1..=6)
            .map(|n| account(&format!("u-{n}"), &format!("acct-{n}")))
            .collect();
        let store = store_holding(b"A-token").await;
        let stash = stash_with(&[("Sessiometer/u-1", b"one", "u-1")]).await;
        let (_json_dir, json) = claude_json("u-A");
        let cfg_dir = tempfile::tempdir().unwrap();
        let config_path = cfg_dir.path().join("config.toml");
        let tun = tunables(95, 80, 0);
        let mut daemon: FakeDaemon = Daemon::new(
            roster,
            FakeRosterPoller::new(),
            store,
            stash,
            FakeClock::frozen(),
            json,
            &tun,
        )
        .with_config_path(config_path.clone())
        // NO witness of any kind — the loss took every durable trace with it.
        .with_witness_sources(pinned_witness(cfg_dir.path(), false));

        let (ack, event) = daemon
            .perform_socket_capture(&CaptureCommand { label: None })
            .await;

        assert_eq!(
            ack,
            CaptureAck::Rejected {
                reason: CaptureRejection::PriorConfiguration,
            }
        );
        assert_eq!(
            event,
            Some(Event::Capture {
                account: None,
                outcome: CaptureEventOutcome::PriorConfiguration,
            })
        );
        // The AC in its own words: the daemon's roster is unchanged, and nothing was written.
        assert_eq!(roster_uuids(&daemon).len(), 6, "the daemon's roster shrank");
        assert!(
            !config_path.exists(),
            "the refusal wrote a collapsed roster"
        );
        assert_eq!(daemon.stash.len(), 1, "the refusal stashed a credential");
        assert!(!daemon.stash.contains("Sessiometer/u-A"));
    }

    #[tokio::test]
    async fn perform_socket_capture_still_onboards_a_genuine_first_run() {
        // Issue #1441 AC and PRD § 7 row 3: the GUI first run is a real path and must not be the
        // price of the fix. Same absent `config.toml` as above — the arm the guard sits on — but
        // with NEITHER observation firing: no durable witness, and an EMPTY daemon roster, which is
        // what a machine that has genuinely never been configured looks like. It lands a one-account
        // roster exactly as the unguarded path did.
        //
        // Without this, both refusals above are satisfied by a gate that refuses unconditionally,
        // and the menu-bar capture button would be dead on every fresh install. It is also what
        // keeps the roster corroboration a CORROBORATION: an empty daemon says nothing, so the
        // durable witness alone decides, and row 3 stays distinct from row 2.
        let store = store_holding(b"A-token").await;
        let stash = stash_with(&[]).await;
        let (_json_dir, json) = claude_json("u-A");
        let cfg_dir = tempfile::tempdir().unwrap();
        let config_path = cfg_dir.path().join("config.toml");
        let tun = tunables(95, 80, 0);
        let mut daemon: FakeDaemon = Daemon::new(
            Vec::new(),
            FakeRosterPoller::new(),
            store,
            stash,
            FakeClock::frozen(),
            json.clone(),
            &tun,
        )
        .with_config_path(config_path.clone())
        .with_witness_sources(pinned_witness(cfg_dir.path(), false));

        let (ack, event) = daemon
            .perform_socket_capture(&CaptureCommand {
                label: Some("work".to_owned()),
            })
            .await;

        assert_eq!(
            ack,
            CaptureAck::Captured {
                label: "work".to_owned(),
                count: 1,
            }
        );
        assert_eq!(
            event,
            Some(Event::Capture {
                account: Some("work".to_owned()),
                outcome: CaptureEventOutcome::Captured,
            })
        );
        // The roster was created and the live rotation reconciled to it.
        assert_eq!(roster_uuids(&daemon), vec!["u-A"]);
        let on_disk = Config::load_path(&config_path).unwrap();
        assert_eq!(on_disk.roster.len(), 1);
        assert_eq!(on_disk.roster[0].account_uuid, "u-A");
        assert!(daemon.stash.contains("Sessiometer/u-A"));
    }

    #[tokio::test]
    async fn perform_socket_capture_admits_a_present_config_whatever_the_witness_says() {
        // The rule's `(true, _)` arm at this entry point: a PRESENT `config.toml` is the ordinary
        // append and the witness has nothing to decide, so a configured machine — the arm that
        // refuses above — must still capture normally. Without this the guard could be a blanket
        // "refuse when a witness exists", which would break every capture on every machine that
        // has ever been configured, i.e. all of them.
        let roster = vec![account("u-B", "spare")];
        let store = store_holding(b"A-token").await;
        let stash = stash_with(&[("Sessiometer/u-B", b"B-token", "u-B")]).await;
        let (_json_dir, json) = claude_json("u-A");
        let cfg_dir = tempfile::tempdir().unwrap();
        let config_path = cfg_dir.path().join("config.toml");
        write_roster_config(&config_path, &[("u-B", "spare")]);
        let tun = tunables(95, 80, 0);
        let mut daemon: FakeDaemon = Daemon::new(
            roster,
            FakeRosterPoller::new(),
            store,
            stash,
            FakeClock::frozen(),
            json.clone(),
            &tun,
        )
        .with_config_path(config_path.clone())
        // A CONFIGURED machine — the same sources the refusal above uses.
        .with_witness_sources(pinned_witness(cfg_dir.path(), true));

        let (ack, _event) = daemon
            .perform_socket_capture(&CaptureCommand {
                label: Some("work".to_owned()),
            })
            .await;

        assert_eq!(
            ack,
            CaptureAck::Captured {
                label: "work".to_owned(),
                count: 2,
            }
        );
        assert_eq!(roster_uuids(&daemon), vec!["u-B", "u-A"]);
    }

    #[test]
    fn the_daemon_run_path_wires_the_real_witness_sources() {
        // The gate above is only as good as its seam, and the seam is an OPT-IN: `Daemon::new`
        // leaves it `Unwired` and exactly one line in `run` overrides it. Nothing else notices if
        // that line is dropped — every hermetic test pins its own sources, so the whole suite stays
        // green while the shipped daemon refuses EVERY socket capture into an absent config.
        //
        // Note the direction of that regression: it fails CLOSED, so it costs a false refusal on a
        // genuine GUI first run rather than the collapse. That is the safe way round, and still a
        // real defect — the affordance is dead on a fresh install with no test able to say why.
        // This is the daemon-side counterpart of `admit_append_only`'s own doc comment, which
        // records the same one-call-short gap for the CLI verbs.
        /// The wiring call, verbatim. Reformatting is free (`cargo fmt` owns the line breaks and
        /// the search is over trimmed lines); changing the ARGUMENT is not, which is the point —
        /// `WitnessSources::real()` is what makes the shipped daemon able to observe at all.
        const WIRING_CALL: &str = ".with_witness_sources(crate::witness::WitnessSources::real()?)";

        // A bare `source.contains(...)` is NOT enough here, and the reason is specific: commenting
        // the call out leaves the substring intact, so the one regression this test names would
        // ship under a green suite. The line must therefore be LIVE CODE on the builder chain —
        // a chain link starts at `.` once trimmed, which a `//`-prefixed line never does.
        //
        // The sibling `both_append_only_verbs_consult_the_witness_before_they_can_write`
        // (`src/capture.rs`) is the CLI-side shape of the same idea and goes further, pinning the
        // gate's POSITION against five named first-effects. Position is not the property here:
        // the daemon's builder links are order-independent, so presence on the live chain is the
        // whole claim.
        let cli = std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/cli.rs"),
        )
        .expect("src/cli.rs is readable from the crate root");
        let live_chain_links: Vec<&str> = cli
            .lines()
            .map(str::trim)
            .filter(|line| line.starts_with('.'))
            .collect();
        // Canary the extraction BEFORE the assertion it carries: a truncated read, or a refactor
        // that stops writing the daemon as a builder chain, yields an EMPTY link set — which
        // satisfies no `contains` and would report the regression as absent by having looked at
        // nothing. `with_config_path` is a sibling link on the same chain.
        assert!(
            live_chain_links
                .iter()
                .any(|link| link.starts_with(".with_config_path(")),
            "no live builder chain found in src/cli.rs; this scan has no subject"
        );
        assert!(
            live_chain_links.contains(&WIRING_CALL),
            "`run` no longer wires the real prior-configuration witness as LIVE code, so the \
             shipped daemon carries the `Unwired` default and refuses every socket capture into \
             an absent config — a dead capture button on every fresh install"
        );
    }

    #[tokio::test]
    async fn perform_socket_capture_fails_closed_with_no_witness_sources_wired() {
        // The hermetic-test DEFAULT, asserted rather than left implicit. `Daemon::new` leaves the
        // seam `Unwired`, which cannot observe — and "cannot tell" must never be spelled `Absent`,
        // because only `Absent` permits the write.
        //
        // This is the same fail-closed shape as a `None` `config_path`, and it is what keeps a
        // future daemon test from accidentally re-opening the fall-through: a test that wants a
        // capture into an absent config to SUCCEED has to pin sources that say so, in writing.
        //
        // The daemon roster is EMPTY, so the corroboration cannot be what refuses — this is about
        // the unwired seam and nothing else.
        let store = store_holding(b"A-token").await;
        let stash = stash_with(&[]).await;
        let (_json_dir, json) = claude_json("u-A");
        let cfg_dir = tempfile::tempdir().unwrap();
        let config_path = cfg_dir.path().join("config.toml");
        let tun = tunables(95, 80, 0);
        let mut daemon: FakeDaemon = Daemon::new(
            Vec::new(),
            FakeRosterPoller::new(),
            store,
            stash,
            FakeClock::frozen(),
            json,
            &tun,
        )
        .with_config_path(config_path.clone());

        let (ack, _event) = daemon
            .perform_socket_capture(&CaptureCommand { label: None })
            .await;

        assert_eq!(
            ack,
            CaptureAck::Rejected {
                reason: CaptureRejection::PriorConfiguration,
            }
        );
        assert!(!config_path.exists());
        assert_eq!(daemon.stash.len(), 0);
    }

    #[tokio::test]
    async fn perform_socket_capture_refuses_a_locked_keychain_and_writes_nothing() {
        // AC (locked keychain → redacted SAFETY abort, ZERO writes): the identity reads fine but the
        // active-token read hits a LOCKED keychain (locked ≠ gone — retry when unlocked). The
        // capture aborts with the redacted `KeychainLocked` reason and writes NOTHING — no stash, no
        // roster row, no reconcile. An omitted label leaves the event handle `None` (no identity was
        // ever paired to a label).
        let roster = vec![account("u-B", "spare")];
        let store = store_holding(b"A-token").await;
        let stash = stash_with(&[("Sessiometer/u-B", b"B-token", "u-B")]).await;
        let (_json_dir, json) = claude_json("u-A");
        let cfg_dir = tempfile::tempdir().unwrap();
        let config_path = cfg_dir.path().join("config.toml");
        write_roster_config(&config_path, &[("u-B", "spare")]);
        let tun = tunables(95, 80, 0);
        let mut daemon: FakeDaemon = Daemon::new(
            roster,
            FakeRosterPoller::new(),
            store,
            stash,
            FakeClock::frozen(),
            json.clone(),
            &tun,
        )
        .with_config_path(config_path.clone());

        // Lock the keychain, THEN capture: the token read aborts after the (successful) identity read.
        daemon.store.set_locked(true);
        let (ack, event) = daemon
            .perform_socket_capture(&CaptureCommand { label: None })
            .await;

        assert_eq!(
            ack,
            CaptureAck::Rejected {
                reason: CaptureRejection::KeychainLocked,
            }
        );
        assert_eq!(
            event,
            Some(Event::Capture {
                account: None,
                outcome: CaptureEventOutcome::KeychainLocked,
            })
        );
        // ZERO writes: the roster is untouched in memory AND on disk, and nothing new was stashed.
        assert_eq!(roster_uuids(&daemon), vec!["u-B"]);
        assert_eq!(Config::load_path(&config_path).unwrap().roster.len(), 1);
        assert_eq!(daemon.stash.len(), 1);
        assert!(!daemon.stash.contains("Sessiometer/u-A"));
        // Once unlocked the canonical still holds A's token — the abort forged no write.
        daemon.store.set_locked(false);
        assert!(daemon
            .store
            .read()
            .await
            .unwrap()
            .matches(&cred(b"A-token")));
    }

    // --- socket `config-get` / `config-set` command (issue #268) ------------

    #[tokio::test]
    async fn perform_config_set_is_unavailable_without_a_wired_config_path() {
        // A hermetic daemon with NO `config_path` wired cannot persist an edit — config-set is
        // `Unavailable` (fails closed, exactly like `capture` with no path to persist to), a TRUE
        // no-op with ZERO writes.
        let mut daemon = reconcile_daemon(vec![account("u-A", "work")]);
        let ack = daemon
            .perform_config_set(&ConfigSetCommand {
                tunables: SetTunables {
                    poll_secs: Some(120),
                    ..SetTunables::default()
                },
                labels: BTreeMap::new(),
            })
            .await;
        assert_eq!(
            ack,
            ConfigSetAck::Rejected {
                reason: ConfigSetRejection::Unavailable,
                detail: None,
            }
        );
    }

    #[tokio::test]
    async fn perform_config_set_reports_no_config_for_an_absent_file() {
        // A wired path whose FILE does not exist → `NoConfig` (capture the first account via the CLI
        // first), never a fabricated empty config — and ZERO writes.
        let cfg_dir = tempfile::tempdir().unwrap();
        let config_path = cfg_dir.path().join("config.toml"); // never written
        let mut daemon =
            reconcile_daemon(vec![account("u-A", "work")]).with_config_path(config_path.clone());
        let ack = daemon
            .perform_config_set(&ConfigSetCommand {
                tunables: SetTunables {
                    poll_secs: Some(120),
                    ..SetTunables::default()
                },
                labels: BTreeMap::new(),
            })
            .await;
        assert_eq!(
            ack,
            ConfigSetAck::Rejected {
                reason: ConfigSetRejection::NoConfig,
                detail: None,
            }
        );
        assert!(
            !config_path.exists(),
            "a rejected config-set writes nothing"
        );
    }

    #[tokio::test]
    async fn perform_config_set_applies_a_tunable_and_reports_restart_required() {
        // AC (tunable edit → persisted, reload-by-restart): a tunable change is written to disk and
        // reported `RestartRequired` (the daemon derives its strategy fields once at construction —
        // no hot-reload), leaving the in-memory roster untouched (a tunable is not a live adopt).
        let cfg_dir = tempfile::tempdir().unwrap();
        let config_path = cfg_dir.path().join("config.toml");
        write_roster_config(&config_path, &[("u-A", "work")]); // default poll_secs = 300
        let mut daemon =
            reconcile_daemon(vec![account("u-A", "work")]).with_config_path(config_path.clone());

        let ack = daemon
            .perform_config_set(&ConfigSetCommand {
                tunables: SetTunables {
                    poll_secs: Some(120),
                    ..SetTunables::default()
                },
                labels: BTreeMap::new(),
            })
            .await;

        assert_eq!(
            ack,
            ConfigSetAck::Applied {
                effect: ConfigSetEffect::RestartRequired,
            }
        );
        // The edit landed on disk (a restart picks it up)…
        assert_eq!(
            Config::load_path(&config_path).unwrap().tunables.poll_secs,
            120
        );
        // …but the in-memory roster is untouched — a tunable change is NOT a live reconcile.
        assert_eq!(daemon.roster[0].label, "work");
    }

    #[tokio::test]
    async fn perform_config_set_adopts_a_label_live_and_reconciles_the_roster() {
        // AC (label edit → adopted LIVE): a non-credential label change is persisted AND reconciled
        // into the in-memory roster within the same call (the SAME #139 core), so `status` reflects
        // it without a restart — reported `Live`.
        let cfg_dir = tempfile::tempdir().unwrap();
        let config_path = cfg_dir.path().join("config.toml");
        write_roster_config(&config_path, &[("u-A", "work")]);
        let mut daemon =
            reconcile_daemon(vec![account("u-A", "work")]).with_config_path(config_path.clone());

        let ack = daemon
            .perform_config_set(&ConfigSetCommand {
                tunables: SetTunables::default(),
                labels: BTreeMap::from([("u-A".to_owned(), "day-job".to_owned())]),
            })
            .await;

        assert_eq!(
            ack,
            ConfigSetAck::Applied {
                effect: ConfigSetEffect::Live,
            }
        );
        // Adopted LIVE: the in-memory roster carries the new label (reconciled in-process)…
        assert_eq!(daemon.roster[0].label, "day-job");
        // …and it is persisted (a restart keeps it), keyed by the immutable uuid.
        let on_disk = Config::load_path(&config_path).unwrap();
        assert_eq!(on_disk.roster[0].account_uuid, "u-A");
        assert_eq!(on_disk.roster[0].label, "day-job");
    }

    #[tokio::test]
    async fn a_label_edit_cannot_narrow_the_live_roster_onto_a_diverged_file() {
        // Issue #1442. `perform_config_set` is the one `reconcile_roster` call site whose intent is
        // a judgement rather than a transcription of R-4's verb list, and this is the scenario that
        // decides it — reachable only BECAUSE this issue's floor exists, which is why it needs its
        // own test rather than riding on the reload cases.
        //
        // The sequence: the file diverges to one account while the daemon holds three. A `login`
        // reload is now refused, so the daemon KEEPS its three and the divergence PERSISTS (the
        // daemon never writes `config.toml`; reconciling the two is issue #1443). The operator then
        // does something entirely unrelated — renames a label in the Settings panel. The panel
        // reads the FILE, so the rename targets a surviving account and succeeds.
        //
        // Under `Mutating` that edit would adopt the file's one-account roster and collapse the
        // live rotation 3 -> 1: the 2026-08-27 loss, through a verb that changed no membership,
        // with no roster-reload line and no event of any kind (a config edit emits none in v1). The
        // floor is what prevents it.
        let cfg_dir = tempfile::tempdir().unwrap();
        let config_path = cfg_dir.path().join("config.toml");
        write_roster_config(&config_path, &[("u-A", "work")]);
        let mut daemon: FakeDaemon = reconcile_daemon(vec![
            account("u-A", "work"),
            account("u-B", "spare"),
            account("u-C", "third"),
        ])
        .with_config_path(config_path.clone());
        daemon.state.accounts[2].health.quarantined = true;

        let ack = daemon
            .perform_config_set(&ConfigSetCommand {
                tunables: SetTunables::default(),
                labels: BTreeMap::from([("u-A".to_owned(), "day-job".to_owned())]),
            })
            .await;

        // The edit itself still succeeds and still lands on disk — the floor governs what the
        // daemon ADOPTS, and refusing the operator's label change would be a different bug.
        assert_eq!(
            ack,
            ConfigSetAck::Applied {
                effect: ConfigSetEffect::Live,
            }
        );
        let on_disk = Config::load_path(&config_path).unwrap();
        assert_eq!(on_disk.roster[0].label, "day-job", "the rename persisted");

        // And the live rotation kept all three accounts, with their carried state.
        assert_eq!(
            roster_uuids(&daemon),
            vec!["u-A", "u-B", "u-C"],
            "a label edit must not narrow the live roster onto a file that lost accounts"
        );
        assert!(
            daemon.state.accounts[2].health.quarantined,
            "and no per-account signal was re-keyed by the refused reconcile"
        );
    }

    #[tokio::test]
    async fn perform_config_set_reports_unchanged_for_a_noop() {
        // Submitting the CURRENT values (poll_secs 300 = the default the file carries, and no label
        // edit) changes nothing → `Unchanged`, and nothing is rewritten.
        let cfg_dir = tempfile::tempdir().unwrap();
        let config_path = cfg_dir.path().join("config.toml");
        write_roster_config(&config_path, &[("u-A", "work")]);
        let before = std::fs::read_to_string(&config_path).unwrap();
        let mut daemon =
            reconcile_daemon(vec![account("u-A", "work")]).with_config_path(config_path.clone());

        let ack = daemon
            .perform_config_set(&ConfigSetCommand {
                tunables: SetTunables {
                    poll_secs: Some(300),
                    ..SetTunables::default()
                },
                labels: BTreeMap::from([("u-A".to_owned(), "work".to_owned())]),
            })
            .await;

        assert_eq!(
            ack,
            ConfigSetAck::Applied {
                effect: ConfigSetEffect::Unchanged,
            }
        );
        assert_eq!(
            std::fs::read_to_string(&config_path).unwrap(),
            before,
            "a no-op config-set rewrites nothing",
        );
    }

    #[tokio::test]
    async fn perform_config_set_rejects_an_out_of_range_tunable_and_writes_nothing() {
        // AC (range/cross-field violation → refused, ZERO writes): an out-of-range tunable fails the
        // WHOLE-config revalidation → `Invalid` carrying the non-secret field-named `detail`, and
        // the old file is left byte-for-byte intact.
        let cfg_dir = tempfile::tempdir().unwrap();
        let config_path = cfg_dir.path().join("config.toml");
        write_roster_config(&config_path, &[("u-A", "work")]);
        let before = std::fs::read_to_string(&config_path).unwrap();
        let mut daemon =
            reconcile_daemon(vec![account("u-A", "work")]).with_config_path(config_path.clone());

        let ack = daemon
            .perform_config_set(&ConfigSetCommand {
                tunables: SetTunables {
                    session_ceiling: Some(200), // a usage percent > 100 is out of range
                    ..SetTunables::default()
                },
                labels: BTreeMap::new(),
            })
            .await;

        match ack {
            ConfigSetAck::Rejected {
                reason: ConfigSetRejection::Invalid,
                detail: Some(msg),
            } => assert!(
                !msg.is_empty(),
                "the invalid reason names the offending field"
            ),
            other => panic!("expected an Invalid rejection with detail, got {other:?}"),
        }
        assert_eq!(
            std::fs::read_to_string(&config_path).unwrap(),
            before,
            "a rejected config-set writes nothing",
        );
    }

    #[tokio::test]
    async fn perform_config_set_rejects_an_unknown_account_uuid_and_writes_nothing() {
        // AC (stale label target → refused): a label edit naming a uuid no roster account has (a
        // stale settings client — the account was `remove`d since its `config-get`) → `UnknownAccount`,
        // ZERO writes.
        let cfg_dir = tempfile::tempdir().unwrap();
        let config_path = cfg_dir.path().join("config.toml");
        write_roster_config(&config_path, &[("u-A", "work")]);
        let before = std::fs::read_to_string(&config_path).unwrap();
        let mut daemon =
            reconcile_daemon(vec![account("u-A", "work")]).with_config_path(config_path.clone());

        let ack = daemon
            .perform_config_set(&ConfigSetCommand {
                tunables: SetTunables::default(),
                labels: BTreeMap::from([("u-does-not-exist".to_owned(), "x".to_owned())]),
            })
            .await;

        assert_eq!(
            ack,
            ConfigSetAck::Rejected {
                reason: ConfigSetRejection::UnknownAccount,
                detail: None,
            }
        );
        assert_eq!(
            std::fs::read_to_string(&config_path).unwrap(),
            before,
            "a rejected config-set writes nothing",
        );
    }

    #[tokio::test]
    async fn perform_config_set_refuses_a_malformed_on_disk_config_and_writes_nothing() {
        // AC (refuse rather than clobber — the safety half of the read path): if `config.toml` is
        // hand-broken WHILE the daemon runs, a config-set refuses with `ConfigUnreadable` and leaves
        // the unreadable file byte-for-byte intact — it never overwrites a file it cannot re-render.
        let cfg_dir = tempfile::tempdir().unwrap();
        let config_path = cfg_dir.path().join("config.toml");
        std::fs::write(&config_path, "this is not toml [[[").unwrap();
        let before = std::fs::read_to_string(&config_path).unwrap();
        let mut daemon =
            reconcile_daemon(vec![account("u-A", "work")]).with_config_path(config_path.clone());

        let ack = daemon
            .perform_config_set(&ConfigSetCommand {
                tunables: SetTunables {
                    poll_secs: Some(120),
                    ..SetTunables::default()
                },
                labels: BTreeMap::new(),
            })
            .await;

        // issue #628: the baseline TOML parse error is threaded into `detail` (secret-free — the config
        // holds no secrets, issue #15) instead of discarded, so a stale / version-skewed on-disk config
        // is diagnosable rather than a bare `config-unreadable`.
        match ack {
            ConfigSetAck::Rejected {
                reason: ConfigSetRejection::ConfigUnreadable,
                detail: Some(msg),
            } => {
                assert!(!msg.is_empty(), "the parse error is surfaced as detail");
                assert!(
                    msg.contains("malformed config"),
                    "detail names the parse failure: {msg}"
                );
            }
            other => panic!("expected ConfigUnreadable with a parse detail, got {other:?}"),
        }
        assert_eq!(
            std::fs::read_to_string(&config_path).unwrap(),
            before,
            "a malformed config is refused, never clobbered",
        );
    }

    #[tokio::test]
    async fn serve_control_routes_config_get_to_a_handoff_unauthenticated() {
        use tokio::io::AsyncWriteExt;
        // A `config-get` is a non-secret READ (tunables + redacted labels, like `status` / `stats`),
        // so it is NOT auth-gated: even an unauthenticated peer gets the handoff to the spawned
        // reader (the blocking `config.toml` read runs off the run loop, ADR-0001).
        let (mut client, server) = tokio::io::duplex(1024);
        client
            .write_all(b"{\"cmd\":\"config-get\"}\n")
            .await
            .unwrap();
        let _stream = serve_control(server, &StatusSnapshot::default(), false)
            .await
            .unwrap()
            .config_get();
    }

    #[tokio::test]
    async fn serve_control_rejects_an_unauthenticated_config_set() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        // AC (peer-credential authN): a `config-set` is STATE-AFFECTING (it writes `config.toml`),
        // so a non-owner peer is rejected BEFORE any handoff — the edit never reaches the run loop
        // (`one_shot()` proves there is NO `ConfigSet` handoff) and the peer gets `unauthorized`.
        let (mut client, server) = tokio::net::UnixStream::pair().expect("socketpair");
        client
            .write_all(b"{\"cmd\":\"config-set\",\"tunables\":{\"poll_secs\":120}}\n")
            .await
            .unwrap();
        let outcome = serve_control(server, &StatusSnapshot::default(), false)
            .await
            .unwrap();
        assert!(
            outcome.one_shot().is_none(),
            "an unauthenticated config-set must not hand off to the run loop",
        );
        let mut reply = String::new();
        client.read_to_string(&mut reply).await.unwrap();
        assert!(reply.contains("unauthorized"), "got {reply:?}");
    }

    #[tokio::test]
    async fn serve_control_rejects_an_authenticated_config_set_with_a_forbidden_key() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        // SAFETY (issue #268 structural boundary): even an AUTHENTICATED `config-set` carrying a
        // forbidden top-level key — here a `credential` — is a hard `malformed request` via
        // `ConfigSetRequest`'s `deny_unknown_fields`, with NO handoff. The credential/roster-structure
        // boundary cannot be crossed through config-set: a forbidden key never reaches the run loop,
        // so the daemon never even attempts to interpret it.
        let (mut client, server) = tokio::net::UnixStream::pair().expect("socketpair");
        client
            .write_all(b"{\"cmd\":\"config-set\",\"credential\":\"secret\"}\n")
            .await
            .unwrap();
        let outcome = serve_control(server, &StatusSnapshot::default(), true)
            .await
            .unwrap();
        assert!(
            outcome.one_shot().is_none(),
            "a forbidden-key config-set must not hand off to the run loop",
        );
        let mut reply = String::new();
        client.read_to_string(&mut reply).await.unwrap();
        assert!(reply.contains("malformed"), "got {reply:?}");
        // issue #628: the reply threads serde's message into `detail` — it NAMES the forbidden key so
        // the client can diagnose the rejection...
        assert!(
            reply.contains("credential"),
            "the detail names the forbidden key: {reply:?}"
        );
        // ...but serde's `deny_unknown_fields` names the KEY, never its VALUE — the load-bearing
        // secret-free guarantee for this error surface a client reads (the threaded detail must not
        // leak the `credential` value even when the client sends one).
        assert!(
            !reply.contains("secret"),
            "a forbidden key's VALUE must never leak into the error detail: {reply:?}"
        );
    }

    #[tokio::test]
    async fn serve_control_config_set_detail_distinguishes_a_version_skew_from_a_malformed_line() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        // issue #628: after the #606 rename, a pre-rename menubar sends the now-unknown `session_trigger`
        // tunable in its `config-set`. `SetTunables`' `deny_unknown_fields` rejects it (ZERO writes, so
        // nothing corrupts), but the reply must let the client DISTINGUISH this version-skew edit from a
        // genuinely malformed line: serde's message — which NAMES `session_trigger` (and lists the
        // accepted keys, so the renamed `session_ceiling` is visible) — is threaded into `detail`,
        // instead of the content-free `malformed request` both cases used to share.
        let (mut client, server) = tokio::net::UnixStream::pair().expect("socketpair");
        client
            .write_all(b"{\"cmd\":\"config-set\",\"tunables\":{\"session_trigger\":95}}\n")
            .await
            .unwrap();
        let outcome = serve_control(server, &StatusSnapshot::default(), true)
            .await
            .unwrap();
        assert!(
            outcome.one_shot().is_none(),
            "a version-skewed config-set must not hand off to the run loop",
        );
        let mut skew_reply = String::new();
        client.read_to_string(&mut skew_reply).await.unwrap();
        // The stable machine code is unchanged, but the detail now NAMES the stale tunable.
        assert!(skew_reply.contains("malformed"), "got {skew_reply:?}");
        assert!(
            skew_reply.contains("session_trigger"),
            "the detail names the offending stale tunable: {skew_reply:?}"
        );
        // The VALUE (95) is a non-secret tunable, but serde still names only the KEY — the detail is a
        // key-naming message, matching the secret-free contract of the credential case.

        // A GENERIC malformed line (invalid JSON) is DISTINGUISHABLE: same code, but its decode-specific
        // detail does NOT name `session_trigger`, so a version-skew edit and a garbage line no longer
        // collapse to the same bare reply.
        let (generic_reply, _signal) =
            control_reply("not json at all", &StatusSnapshot::default(), true);
        assert!(generic_reply.contains("malformed"), "got {generic_reply:?}");
        assert!(
            !generic_reply.contains("session_trigger"),
            "a generic malformed line must not name the skew key: {generic_reply:?}"
        );
        // `skew_reply` carries `write_line`'s trailing newline; `control_reply` returns the bare line
        // — trim so the inequality is about CONTENT, not the framing newline (without the trim this
        // assertion would pass trivially on the newline alone, degrading silently).
        assert_ne!(
            skew_reply.trim_end(),
            generic_reply.as_str(),
            "a version-skew edit is distinguishable from a generic malformed line",
        );
    }

    #[tokio::test]
    async fn serve_control_hands_back_an_authenticated_config_set() {
        use tokio::io::AsyncWriteExt;
        // An AUTHENTICATED, well-formed `config-set` is NOT answered inline: like `swap` / `capture`,
        // it hands the OPEN connection back (with the parsed edits) so the run loop applies them
        // against `&mut Daemon` and writes the redacted ack from the REAL outcome.
        let (mut client, server) = tokio::io::duplex(1024);
        client
            .write_all(
                b"{\"cmd\":\"config-set\",\"tunables\":{\"poll_secs\":120},\"labels\":{\"u-A\":\"day-job\"}}\n",
            )
            .await
            .unwrap();
        let (_stream, command) = serve_control(server, &StatusSnapshot::default(), true)
            .await
            .unwrap()
            .config_set();
        assert_eq!(
            command,
            ConfigSetCommand {
                tunables: SetTunables {
                    poll_secs: Some(120),
                    ..SetTunables::default()
                },
                labels: BTreeMap::from([("u-A".to_owned(), "day-job".to_owned())]),
            }
        );
    }

    #[test]
    fn config_get_reply_maps_read_outcomes_to_non_secret_envelopes() {
        // The `config-get` read path: a valid file → a serialized `ConfigView` (tunables + redacted
        // roster), an absent file → `{"error":"no config"}`, a malformed one → `{"error":"config
        // unreadable"}` — never a leak, never a panic.
        let cfg_dir = tempfile::tempdir().unwrap();
        let config_path = cfg_dir.path().join("config.toml");

        // Absent → a `no config` envelope (nothing captured yet).
        assert!(config_get_reply(&config_path).contains("no config"));

        // Valid → a `ConfigView` naming the redacted account handle, decodable by the client.
        write_roster_config(&config_path, &[("u-A", "work")]);
        let reply = config_get_reply(&config_path);
        let view: crate::config::ConfigView = serde_json::from_str(&reply).unwrap();
        assert_eq!(view.accounts.len(), 1);
        assert_eq!(view.accounts[0].account_uuid, "u-A");
        assert_eq!(view.accounts[0].label, "work");

        // Malformed → a `config unreadable` envelope (refuse rather than guess). issue #628: the parse
        // error is threaded into `detail` (secret-free — the config holds no secrets, issue #15) so the
        // client learns WHERE the file is broken, not a content-free envelope.
        std::fs::write(&config_path, "this is not toml [[[").unwrap();
        let unreadable = config_get_reply(&config_path);
        assert!(unreadable.contains("config unreadable"), "got {unreadable}");
        assert!(
            unreadable.contains("\"detail\":") && unreadable.contains("malformed config"),
            "the parse error is surfaced as detail: {unreadable}"
        );
    }

    #[test]
    fn config_set_ack_serializes_to_the_non_secret_wire_shape() {
        // The ack the settings client decodes (issue #268): an internally-tagged `result`, a
        // snake_case `effect` on success, a kebab-case `reason` on refusal, and `detail` OMITTED when
        // absent — non-secret by construction (#15). Round-trips so the client reads back what the
        // daemon wrote.
        let applied = ConfigSetAck::Applied {
            effect: ConfigSetEffect::RestartRequired,
        };
        let wire = serde_json::to_string(&applied).unwrap();
        assert_eq!(wire, r#"{"result":"applied","effect":"restart_required"}"#);
        assert_eq!(
            serde_json::from_str::<ConfigSetAck>(&wire).unwrap(),
            applied
        );

        let invalid = ConfigSetAck::Rejected {
            reason: ConfigSetRejection::Invalid,
            detail: Some("poll_secs must be in 60..=600".to_owned()),
        };
        let wire = serde_json::to_string(&invalid).unwrap();
        assert!(wire.contains(r#""result":"rejected""#), "got {wire}");
        assert!(wire.contains(r#""reason":"invalid""#), "got {wire}");
        assert!(
            wire.contains(r#""detail":"poll_secs must be in 60..=600""#),
            "got {wire}"
        );
        assert_eq!(
            serde_json::from_str::<ConfigSetAck>(&wire).unwrap(),
            invalid
        );

        // A reason WITHOUT detail omits the key entirely (no `"detail":null` noise).
        let no_config = ConfigSetAck::Rejected {
            reason: ConfigSetRejection::NoConfig,
            detail: None,
        };
        let wire = serde_json::to_string(&no_config).unwrap();
        assert_eq!(wire, r#"{"result":"rejected","reason":"no-config"}"#);
        assert_eq!(
            serde_json::from_str::<ConfigSetAck>(&wire).unwrap(),
            no_config
        );
    }

    // --- runtime roster-reload (issue #139) --------------------------------

    #[test]
    fn reconcile_roster_onboards_a_new_account_and_preserves_the_rest() {
        // AC: after an onboard, the daemon's in-memory roster reflects the new account
        // — appended with DEFAULT state — while every persisting account keeps its
        // carried health / reading / warm-up state (a capture of ANOTHER account must
        // not reset a healthy one).
        let mut daemon = reconcile_daemon(vec![account("u-A", "work"), account("u-B", "spare")]);
        daemon.state.active = Some(0);
        daemon.state.accounts[0].health.quarantined = true; // A carries a distinctive health mark
        daemon.state.accounts[1].last_reading = Some(reading(0.30, 0.40)); // B carries a reading
        for account in &mut daemon.state.accounts {
            account.polled_once = true;
        }

        assert_adopted(daemon.reconcile_roster(
            vec![
                account("u-A", "work"),
                account("u-B", "spare"),
                account("u-C", "third"),
            ],
            ReloadIntent::AppendOnly,
        ));

        // The new account is now in the live roster…
        assert_eq!(roster_uuids(&daemon), vec!["u-A", "u-B", "u-C"]);
        // …with the runtime state grown to match (no length skew). Pre-#668 this was three
        // separate length assertions, one per parallel vec; the bundle makes that agreement
        // structural, so one assertion now covers every signal.
        assert_eq!(daemon.state.accounts.len(), 3);
        // Persisting accounts keep their carried state.
        assert!(
            daemon.state.accounts[0].health.quarantined,
            "A's health preserved"
        );
        assert_eq!(
            daemon.state.accounts[1].last_reading,
            Some(reading(0.30, 0.40)),
            "B's reading preserved"
        );
        // The onboarded account joins with DEFAULT state (unpolled, no reading, healthy).
        assert!(!daemon.state.accounts[2].health.quarantined);
        assert_eq!(daemon.state.accounts[2].last_reading, None);
        assert!(!daemon.state.accounts[2].polled_once);
        // Active (A) is unchanged — an append never shifts existing indices.
        assert_eq!(daemon.state.active, Some(0));
    }

    #[test]
    fn reconcile_roster_preserves_state_on_a_relogin_that_updates_the_label() {
        // A relogin of an EXISTING account (same account_uuid) updates the roster
        // CONTENT (e.g. a renamed label) without duplicating the entry, and preserves
        // the account's carried decision state. (Un-quarantine on relogin is the
        // daemon's separate canonical-change path #107, not reconcile's job.)
        let mut daemon = reconcile_daemon(vec![account("u-A", "work"), account("u-B", "spare")]);
        daemon.state.accounts[1].last_reading = Some(reading(0.11, 0.22));
        daemon.state.accounts[1].health.recovery_successes = 2;

        assert_adopted(daemon.reconcile_roster(
            vec![account("u-A", "work"), account("u-B", "spare-renamed")],
            ReloadIntent::AppendOnly,
        ));

        assert_eq!(roster_uuids(&daemon), vec!["u-A", "u-B"], "no duplicate");
        assert_eq!(daemon.roster[1].label, "spare-renamed", "label updated");
        assert_eq!(
            daemon.state.accounts[1].last_reading,
            Some(reading(0.11, 0.22)),
            "carried reading preserved across the relogin"
        );
        assert_eq!(
            daemon.state.accounts[1].health.recovery_successes, 2,
            "health preserved"
        );
    }

    #[test]
    fn reconcile_roster_picks_up_an_enabled_flip() {
        // A `disable` / `enable` (#36) flips an account's `enabled` flag on disk; the
        // reload adopts the new flag (rotation membership) while preserving the
        // account's carried decision state — so the flip takes effect in the live
        // rotation without a restart, not merely at the next daemon start.
        let mut daemon = reconcile_daemon(vec![account("u-A", "work"), account("u-B", "spare")]);
        daemon.state.accounts[1].last_reading = Some(reading(0.10, 0.20));

        // `disable spare` on disk → the reloaded roster carries B parked.
        assert_adopted(daemon.reconcile_roster(
            vec![account("u-A", "work"), disabled_account("u-B", "spare")],
            ReloadIntent::Mutating,
        ));

        assert!(
            !daemon.roster[1].enabled,
            "B is now parked in the live roster"
        );
        assert_eq!(
            daemon.state.accounts[1].last_reading,
            Some(reading(0.10, 0.20)),
            "B's carried reading is preserved across the flip"
        );
    }

    #[test]
    fn reconcile_roster_drops_a_removed_account_and_its_state() {
        // A `remove` on disk drops the account (and its state) from the live rotation;
        // the survivors keep their carried state, re-keyed by uuid across the gap.
        let mut daemon = reconcile_daemon(vec![
            account("u-A", "work"),
            account("u-B", "spare"),
            account("u-C", "third"),
        ]);
        daemon.state.active = Some(0);
        daemon.state.accounts[0].last_reading = Some(reading(0.10, 0.10)); // A reading
        daemon.state.accounts[2].health.recovery_successes = 3; // C health mark

        assert_adopted(daemon.reconcile_roster(
            vec![account("u-A", "work"), account("u-C", "third")],
            ReloadIntent::Mutating,
        ));

        assert_eq!(roster_uuids(&daemon), vec!["u-A", "u-C"], "B dropped");
        assert_eq!(daemon.state.accounts.len(), 2);
        // A (still index 0) keeps its reading; C (now index 1) keeps its health mark.
        assert_eq!(
            daemon.state.accounts[0].last_reading,
            Some(reading(0.10, 0.10))
        );
        assert_eq!(
            daemon.state.accounts[1].health.recovery_successes, 3,
            "C re-keyed by uuid"
        );
        assert_eq!(daemon.state.active, Some(0), "active A preserved");
    }

    #[test]
    fn reconcile_roster_remaps_active_across_an_index_shift() {
        // The active account is re-resolved by uuid, not by stale index: removing an
        // EARLIER account shifts the active account's index, and reconcile follows it.
        let mut daemon = reconcile_daemon(vec![
            account("u-A", "work"),
            account("u-B", "spare"),
            account("u-C", "third"),
        ]);
        daemon.state.active = Some(2); // C is active at index 2

        assert_adopted(daemon.reconcile_roster(
            vec![account("u-B", "spare"), account("u-C", "third")],
            ReloadIntent::Mutating,
        )); // A removed

        // C is now at index 1 — active tracks the uuid, not the old slot.
        assert_eq!(roster_uuids(&daemon), vec!["u-B", "u-C"]);
        assert_eq!(daemon.state.active, Some(1));
    }

    #[test]
    fn reconcile_roster_drops_active_to_none_when_the_active_account_is_removed() {
        // Removing the ACTIVE account leaves active `None` — the next tick re-resolves
        // active from the canonical credential (polls-without-swapping meanwhile).
        let mut daemon = reconcile_daemon(vec![account("u-A", "work"), account("u-B", "spare")]);
        daemon.state.active = Some(0); // A active

        assert_adopted(
            daemon.reconcile_roster(vec![account("u-B", "spare")], ReloadIntent::Mutating),
        ); // A removed

        assert_eq!(roster_uuids(&daemon), vec!["u-B"]);
        assert_eq!(daemon.state.active, None);
    }

    #[test]
    fn reconcile_roster_to_an_empty_roster_clears_active_and_state() {
        // Rewritten for issue #1442 (R-7). This test previously blessed the empty reconcile as a
        // valid runtime state whatever asked for it — the codified form of the assumption that
        // destroyed five accounts on 2026-08-27, and a gate that certifies the defect. An empty
        // reconcile is valid, and it is valid ONLY under mutating intent.
        //
        // Both halves in ONE test on purpose. The legitimate case is `remove` of the LAST account:
        // a real 1 -> 0 the daemon must accept, must not panic on, and must clear active and the
        // per-account state for. The illegitimate case is the SAME reshape arriving from an
        // append-only verb, where zero accounts cannot have come from the verb — `plan_capture`
        // only updates-in-place or pushes — so it is a file that lost its roster, and the daemon
        // keeps what it holds. Either assertion alone is satisfied by a floor that fires on
        // neither, or on both.
        let seed = || {
            let mut daemon = reconcile_daemon(vec![account("u-A", "work")]);
            daemon.state.active = Some(0);
            daemon.state.accounts[0].last_reading = Some(reading(0.10, 0.10));
            daemon
        };

        // Mutating: the `remove` of the final entry. Adopted, and the reshape is clean.
        let mut removing = seed();
        assert_adopted(removing.reconcile_roster(vec![], ReloadIntent::Mutating));
        assert!(removing.roster.is_empty());
        assert!(removing.state.accounts.is_empty());
        assert_eq!(removing.state.active, None);

        // Append-only: the same empty roster, refused. Nothing moves.
        let mut appending = seed();
        assert_eq!(
            appending.reconcile_roster(vec![], ReloadIntent::AppendOnly),
            ReconcileOutcome::RefusedShrink,
            "an empty roster from an append-only verb is a file that lost its accounts"
        );
        assert_eq!(roster_uuids(&appending), vec!["u-A"]);
        assert_eq!(appending.state.active, Some(0));
        assert_eq!(
            appending.state.accounts[0].last_reading,
            Some(reading(0.10, 0.10)),
            "a refusal is a TRUE no-op — the carried reading is untouched"
        );
    }

    #[test]
    fn the_incident_an_append_only_reload_narrowing_six_to_one_is_refused() {
        // Issue #1442, the 2026-08-27 transition itself: a live SIX-account daemon, a `login`
        // (append-only) reload presenting ONE account. Before the floor the daemon adopted it and
        // five accounts left the live rotation. It is 6 -> 1 and not 6 -> 0 on purpose — that is
        // what the incident was, and it is why the invariant is shrink-scoped rather than
        // empty-scoped (see the sibling test directly below).
        let mut daemon = reconcile_daemon(vec![
            account("u-A", "work"),
            account("u-B", "spare"),
            account("u-C", "third"),
            account("u-D", "fourth"),
            account("u-E", "fifth"),
            account("u-F", "sixth"),
        ]);

        assert_eq!(
            daemon.reconcile_roster(vec![account("u-A", "work")], ReloadIntent::AppendOnly),
            ReconcileOutcome::RefusedShrink,
        );

        assert_eq!(
            roster_uuids(&daemon),
            vec!["u-A", "u-B", "u-C", "u-D", "u-E", "u-F"],
            "the daemon still holds all six — the reload was refused whole, not partially applied"
        );
    }

    #[test]
    fn an_empty_roster_floor_would_never_have_fired_on_the_incident() {
        // The control, kept as an argument rather than as the mechanism. A floor that refused only
        // an EMPTY roster passes the incident above by doing nothing — 1 is not 0 — and the daemon
        // still loses five accounts. This test pins the zero case as ALSO refused under append-only
        // intent, which the shrink-scoped rule gives for free, while the test above pins the case
        // an empty-scoped rule would have missed. Reachable only if the file is emptied by
        // something other than an append-only verb: `plan_capture` has update-in-place and push
        // arms only, so an append-only verb cannot itself produce a zero-account file.
        let mut daemon = reconcile_daemon(vec![
            account("u-A", "work"),
            account("u-B", "spare"),
            account("u-C", "third"),
            account("u-D", "fourth"),
            account("u-E", "fifth"),
            account("u-F", "sixth"),
        ]);

        assert_eq!(
            daemon.reconcile_roster(vec![], ReloadIntent::AppendOnly),
            ReconcileOutcome::RefusedShrink,
        );
        assert_eq!(roster_uuids(&daemon).len(), 6, "nothing left the rotation");
    }

    #[test]
    fn a_widening_reload_is_untouched_so_issue_139_still_works() {
        // Issue #139 is the reason `notify_daemon_roster_reload` exists at all: a running daemon
        // picks up an on-disk roster ADDITION without a restart. A never-shrink floor that also
        // narrowed this direction would break the fix it is built on (PRD R-18), and the failure
        // would be silent — a captured account simply never joining the rotation.
        //
        // Asserted under APPEND-ONLY intent specifically: that is the intent `capture` and `login`
        // send, so this is issue #139's own live path and not a variant of it.
        let mut daemon = reconcile_daemon(vec![account("u-A", "work")]);

        assert_adopted(daemon.reconcile_roster(
            vec![account("u-A", "work"), account("u-B", "onboarded")],
            ReloadIntent::AppendOnly,
        ));

        assert_eq!(
            roster_uuids(&daemon),
            vec!["u-A", "u-B"],
            "the onboarded account joined the live rotation"
        );
    }

    #[test]
    fn an_equal_count_reload_is_adopted_because_a_relabel_is_not_a_shrink() {
        // `<`, never `<=`. A relabelled or re-ordered roster of the same size carries no loss, and
        // refusing it would strand a `config set` label edit outside the live rotation while
        // reporting nothing. The distinction is one character and is the whole difference between
        // a floor and a freeze.
        let mut daemon = reconcile_daemon(vec![
            account("u-A", "work"),
            account("u-B", "spare"),
            account("u-C", "third"),
        ]);

        assert_adopted(daemon.reconcile_roster(
            vec![
                account("u-C", "third"),
                account("u-A", "work-renamed"),
                account("u-B", "spare"),
            ],
            ReloadIntent::AppendOnly,
        ));

        assert_eq!(
            roster_uuids(&daemon),
            vec!["u-C", "u-A", "u-B"],
            "the re-ordered, re-labelled roster of the same size was adopted"
        );
        assert_eq!(daemon.roster[1].label, "work-renamed");
    }

    #[test]
    fn a_mutating_reload_may_shrink() {
        // PRD R-4. `remove` is not a defect, and without intent the daemon could only refuse EVERY
        // shrink — blocking this — or none, which is the pre-#1442 behaviour that lost the roster.
        // The partition is what makes both directions correct at once.
        let mut daemon = reconcile_daemon(vec![
            account("u-A", "work"),
            account("u-B", "spare"),
            account("u-C", "third"),
            account("u-D", "fourth"),
            account("u-E", "fifth"),
            account("u-F", "sixth"),
        ]);

        assert_adopted(daemon.reconcile_roster(
            vec![
                account("u-A", "work"),
                account("u-B", "spare"),
                account("u-C", "third"),
                account("u-D", "fourth"),
                account("u-E", "fifth"),
            ],
            ReloadIntent::Mutating,
        ));

        assert_eq!(roster_uuids(&daemon).len(), 5, "the removal was adopted");
    }

    #[test]
    fn a_refused_shrink_leaves_every_carried_signal_exactly_as_it_was() {
        // A refusal must be a TRUE no-op, not an early return part-way through the reshape. The
        // reconcile writes five things — the roster, the per-account runtime vec, the active
        // index, `last_good`, and the poll schedule — and a floor placed after any of them would
        // leave the daemon in a state no reload produced, which is worse than the bug: the roster
        // would survive while the state indexing it did not.
        //
        // Asserted on the WHOLE observable set rather than on the roster alone, because the roster
        // is the one field an incorrectly-placed floor is most likely to get right.
        let mut daemon = reconcile_daemon(vec![
            account("u-A", "work"),
            account("u-B", "spare"),
            account("u-C", "third"),
        ]);
        daemon.state.active = Some(1);
        daemon.state.accounts[0].last_reading = Some(reading(0.11, 0.22));
        daemon.state.accounts[0].health.quarantined = true;
        daemon.state.accounts[2].health.recovery_successes = 2;
        daemon.state.poll_schedule = vec![2, 0, 1];
        daemon.state.poll_pos = 2;
        // Seeded, not left at its `None` default, because the reconcile's `last_good` write only
        // fires when the active account leaves the roster — which is precisely what this shrink
        // would do to `u-B`. Unseeded, that write clears `None` to `None` and the fifth of the
        // five writes named above is asserted by a comparison that cannot fail.
        let anchor = LastGood {
            session: 0.42,
            weekly: 0.17,
            at: Instant::now(),
        };
        daemon.state.last_good = Some(anchor);

        // Compared through `fingerprint`, the module's EXHAUSTIVE per-signal projection: it
        // destructures `AccountRuntime` field by field, so a tenth signal added later is covered
        // here for free rather than silently escaping this assertion.
        let before: Vec<_> = daemon.state.accounts.iter().map(fingerprint).collect();

        assert_eq!(
            daemon.reconcile_roster(vec![account("u-A", "work")], ReloadIntent::AppendOnly),
            ReconcileOutcome::RefusedShrink,
        );

        assert_eq!(roster_uuids(&daemon), vec!["u-A", "u-B", "u-C"]);
        assert_eq!(
            daemon
                .state
                .accounts
                .iter()
                .map(fingerprint)
                .collect::<Vec<_>>(),
            before,
            "no per-account signal moved"
        );
        assert_eq!(daemon.state.active, Some(1), "active was not re-resolved");
        assert_eq!(
            daemon.state.poll_schedule,
            vec![2, 0, 1],
            "the schedule was not invalidated — its indices are still valid, because the roster \
             they index did not change"
        );
        assert_eq!(daemon.state.poll_pos, 2, "and neither was its cursor");
        assert_eq!(
            daemon.state.last_good,
            Some(anchor),
            "the active account's pre-blind anchor was dropped — the reconcile clears it when the \
             active account leaves the roster, and a refusal means it never left"
        );
    }

    /// Every per-account signal on [`AccountRuntime`], projected into one comparable tuple.
    ///
    /// The EXHAUSTIVE destructure is the point (issue #668): a TENTH per-account signal added to
    /// `AccountRuntime` without a matching arm here fails to compile, so the roster-reload property
    /// below can never silently stop covering a signal — which is exactly how the pre-#668 parallel
    /// vecs drifted (a new vec was added, and the sites that had to re-key it were remembered by
    /// hand). The ninth (issue #1453's observation-gap anchor) arrived that way: this destructure
    /// failed to compile, which is the guard working.
    #[expect(
        clippy::type_complexity,
        reason = "the exhaustive per-signal projection IS the compile-time completeness guard"
    )]
    fn fingerprint(
        account: &AccountRuntime,
    ) -> (
        (bool, u32, Option<i64>),
        Option<Usage>,
        Option<Instant>,
        Option<VelocityEma>,
        Option<ParkedLanding>,
        Option<swap::SessionHighWater>,
        Option<BlindAnchor>,
        Option<ObservationGapAnchor>,
        bool,
    ) {
        let AccountRuntime {
            health,
            last_reading,
            last_reading_at,
            session_velocity,
            parked_landing,
            session_high_water,
            blind_anchor,
            observation_gap,
            polled_once,
        } = account;
        (
            (
                health.quarantined,
                health.recovery_successes,
                health.access_expires_at,
            ),
            *last_reading,
            *last_reading_at,
            *session_velocity,
            *parked_landing,
            *session_high_water,
            *blind_anchor,
            *observation_gap,
            *polled_once,
        )
    }

    /// Every ordering of every subset of `items` — the exhaustive reshape space a roster reload can
    /// produce by adding, removing, and reordering accounts in `config.toml`.
    fn subset_permutations<T: Clone>(items: &[T]) -> Vec<Vec<T>> {
        fn perms<T: Clone>(pool: &[T], acc: &mut Vec<T>, out: &mut Vec<Vec<T>>) {
            out.push(acc.clone());
            for (i, item) in pool.iter().enumerate() {
                let mut rest = pool.to_vec();
                rest.remove(i);
                acc.push(item.clone());
                perms(&rest, acc, out);
                acc.pop();
            }
        }
        let mut out = Vec::new();
        perms(items, &mut Vec::new(), &mut out);
        out
    }

    /// PROPERTY (issue #668): a roster reload re-keys EVERY per-account runtime signal by
    /// `account_uuid`, never by positional index — across every ordering of every subset of the
    /// roster, plus an onboard.
    ///
    /// The pre-#668 shape made this eight independent hand-maintained re-keys, so the property held
    /// only as long as every one of them was remembered; bundling makes it one. Seeding each account
    /// with a DISTINCT value in ALL NINE signals is what makes the test discriminating: a re-key
    /// that fell back to positional order (or dropped a signal to its default) mismatches here rather
    /// than coincidentally matching a shared fixture value.
    ///
    /// Three invariants, over all 65 reshapes × 2 onboard variants:
    ///   1. one runtime slot per roster account — always, so no signal can length-skew;
    ///   2. an account present in BOTH keeps its ENTIRE fingerprint, at its NEW index;
    ///   3. an account NEW on disk starts at `AccountRuntime::default()`.
    #[test]
    fn reconcile_roster_rekeys_every_per_account_signal_by_uuid_not_by_index() {
        let base = vec![
            account("u-A", "work"),
            account("u-B", "spare"),
            account("u-C", "backup"),
            account("u-D", "reserve"),
        ];
        let t0 = Instant::now();

        // Seed slot `i` so that EVERY signal differs from every other slot's — the discriminating
        // half: a positional re-key, or a signal silently defaulted, cannot match by coincidence.
        let seed = |account: &mut AccountRuntime, i: usize| {
            let k = i as f64;
            account.health.quarantined = !i.is_multiple_of(2);
            account.health.recovery_successes = 10 + i as u32;
            account.health.access_expires_at = Some(1_000 + i as i64);
            account.last_reading = Some(reading(0.10 + k / 100.0, 0.20 + k / 100.0));
            account.last_reading_at = Some(t0 + Duration::from_secs(i as u64 + 1));
            account.session_velocity = Some(VelocityEma {
                rate: 0.001 * (k + 1.0),
                samples: 3 + i as u32,
            });
            account.parked_landing = Some(ParkedLanding {
                armed_at: t0 + Duration::from_secs(100 + i as u64),
                decision_pct: 90 + i as u8,
            });
            // Through the real fold seam — `SessionHighWater`'s fields are private to `swap`.
            account.session_high_water = swap::SessionHighWater::fold(
                None,
                &Usage {
                    session: 0.30 + k / 100.0,
                    weekly: 0.40,
                    weekly_resets_at: None,
                    session_resets_at: Some(5_000 + i as i64),
                },
            );
            account.blind_anchor = Some(BlindAnchor {
                session: 0.50 + k / 100.0,
                weekly: 0.60 + k / 100.0,
                at: t0 + Duration::from_secs(200 + i as u64),
                was_active: i.is_multiple_of(2),
                near_limit: i.is_multiple_of(3),
            });
            account.observation_gap = Some(ObservationGapAnchor {
                at: t0 + Duration::from_secs(300 + i as u64),
                was_active: i.is_multiple_of(2),
            });
            account.polled_once = i.is_multiple_of(2);
        };

        // The expected fingerprint per uuid, captured once from a freshly-seeded daemon.
        let expected: Vec<_> = {
            let mut daemon = reconcile_daemon(base.clone());
            for (i, account) in daemon.state.accounts.iter_mut().enumerate() {
                seed(account, i);
            }
            daemon.state.accounts.iter().map(fingerprint).collect()
        };
        let uuid_of = |account: &Account| account.account_uuid.clone();
        let onboard = account("u-NEW", "onboarded");

        let mut reshapes = 0;
        for reshape in subset_permutations(&base) {
            for with_onboard in [false, true] {
                let mut new_roster = reshape.clone();
                if with_onboard {
                    new_roster.push(onboard.clone());
                }

                let mut daemon = reconcile_daemon(base.clone());
                for (i, account) in daemon.state.accounts.iter_mut().enumerate() {
                    seed(account, i);
                }
                assert_adopted(daemon.reconcile_roster(new_roster.clone(), ReloadIntent::Mutating));

                let shape: Vec<String> = new_roster.iter().map(uuid_of).collect();

                // 1. One runtime slot per roster account — no signal can length-skew.
                assert_eq!(
                    daemon.state.accounts.len(),
                    new_roster.len(),
                    "reshape {shape:?}: one runtime slot per roster account",
                );

                for (new_idx, account) in new_roster.iter().enumerate() {
                    let got = fingerprint(&daemon.state.accounts[new_idx]);
                    match base.iter().position(|old| uuid_of(old) == uuid_of(account)) {
                        // 2. Carried over by UUID — the WHOLE fingerprint, at the NEW index.
                        Some(old_idx) => assert_eq!(
                            got,
                            expected[old_idx],
                            "reshape {shape:?}: {} moved {old_idx} -> {new_idx} and must carry \
                             every signal with it, not inherit slot {new_idx}'s",
                            uuid_of(account),
                        ),
                        // 3. Onboarded fresh — unpolled, no reading, healthy.
                        None => assert_eq!(
                            got,
                            fingerprint(&AccountRuntime::default()),
                            "reshape {shape:?}: onboarded {} starts at default",
                            uuid_of(account),
                        ),
                    }
                }
                reshapes += 1;
            }
        }
        // Guards the property against a silently-degenerate sweep (a broken generator asserting
        // nothing): every ordering of every subset of 4 accounts is 1+4+12+24+24 = 65, × 2 onboard
        // variants.
        assert_eq!(
            reshapes, 130,
            "the reshape space must be swept exhaustively"
        );
    }

    #[test]
    fn reconcile_roster_resets_the_stale_poll_schedule() {
        // The staggered poll schedule holds OLD roster indices; reconcile clears it so
        // `next_poll_index` rebuilds a fresh one (over the new roster) next cycle,
        // rather than indexing the reshaped roster with a stale cursor.
        let mut daemon = reconcile_daemon(vec![account("u-A", "work"), account("u-B", "spare")]);
        daemon.state.poll_schedule = vec![0, 1];
        daemon.state.poll_pos = 1;

        assert_adopted(daemon.reconcile_roster(
            vec![
                account("u-A", "work"),
                account("u-B", "spare"),
                account("u-C", "third"),
            ],
            ReloadIntent::AppendOnly,
        ));

        assert!(daemon.state.poll_schedule.is_empty(), "schedule reset");
        assert_eq!(daemon.state.poll_pos, 0, "cursor reset");
    }

    #[test]
    fn control_reply_roster_reload_authenticated_signals_a_reload() {
        // Issue #139: an authenticated same-user peer's `roster-reload` acks and yields
        // the `RosterReloadRequested` signal the run loop acts on.
        let snap = StatusSnapshot::default();
        let (reply, signal) = control_reply(r#"{"cmd":"roster-reload"}"#, &snap, true);
        assert_eq!(reply, r#"{"ok":true}"#);
        // Issue #1442: a request carrying NO `intent` is the legacy shape a pre-#1442 CLI sends,
        // and it resolves to the REFUSING treatment — asserted on the signal itself, so a
        // `from_wire` default flipped to `Mutating` fails here rather than silently permitting
        // every shrink an old client triggers.
        assert_eq!(
            signal,
            Some(ControlSignal::RosterReloadRequested(
                ReloadIntent::AppendOnly
            ))
        );
    }

    #[test]
    fn control_reply_roster_reload_unauthenticated_is_refused_with_no_signal() {
        // Issue #139: `roster-reload` is state-affecting, so an UNauthenticated peer is
        // refused and produces NO signal — a stranger can never make the daemon re-read
        // its config (mirrors the `manual-swapped` #64 auth gate).
        let snap = StatusSnapshot::default();
        let (reply, signal) = control_reply(r#"{"cmd":"roster-reload"}"#, &snap, false);
        assert_eq!(reply, r#"{"error":"unauthorized"}"#);
        assert_eq!(signal, None);
    }

    #[test]
    fn control_reply_restored_authenticated_signals_a_restore() {
        // Issue #275: an authenticated same-user peer's `restored` acks and yields the
        // `Restored(uuid)` signal the run loop hands to `reconcile_restored` — carrying
        // the exact uuid from the request line, un-touched.
        let snap = StatusSnapshot::default();
        let (reply, signal) = control_reply(r#"{"cmd":"restored","uuid":"u-B"}"#, &snap, true);
        assert_eq!(reply, r#"{"ok":true}"#);
        assert_eq!(signal, Some(ControlSignal::Restored("u-B".to_owned())));
    }

    #[test]
    fn control_reply_restored_unauthenticated_is_refused_with_no_signal() {
        // Issue #275 (AC-2): `restored` is state-affecting, so an UNauthenticated peer is refused
        // and produces NO signal — a stranger can never un-quarantine an account (parity with the
        // `manual-swapped` #64 / `roster-reload` #139 auth gate). Auth is checked FIRST: even a
        // well-formed request carrying a uuid gets `unauthorized`, never leaking well-formedness.
        let snap = StatusSnapshot::default();
        let (reply, signal) = control_reply(r#"{"cmd":"restored","uuid":"u-B"}"#, &snap, false);
        assert_eq!(reply, r#"{"error":"unauthorized"}"#);
        assert_eq!(signal, None);
    }

    #[test]
    fn control_reply_restored_without_uuid_is_malformed_and_yields_no_signal() {
        // Issue #275: a `restored` that parses but carries no `uuid` has no target to restore, so
        // it is refused as malformed (bounded / malformed-safe like every command) — no signal, no
        // spurious ack. Checked only after auth, so this is the authenticated-but-malformed branch.
        let snap = StatusSnapshot::default();
        let (reply, signal) = control_reply(r#"{"cmd":"restored"}"#, &snap, true);
        assert_eq!(reply, r#"{"error":"malformed request"}"#);
        assert_eq!(signal, None);
    }

    #[test]
    fn control_reply_shutdown_authenticated_signals_a_graceful_stop() {
        // Issue #397: an authenticated same-user peer's `shutdown` — the `daemon stop` control
        // path for an UNMANAGED daemon — acks `{"ok":true}` and yields the `ShutdownRequested`
        // signal the run loop turns into a graceful `Idle::Shutdown` (so an in-flight swap
        // completes before exit). The pure request→(reply, signal) mapping, mirroring the
        // `roster-reload` #139 gate.
        let snap = StatusSnapshot::default();
        let (reply, signal) = control_reply(r#"{"cmd":"shutdown"}"#, &snap, true);
        assert_eq!(reply, r#"{"ok":true}"#);
        assert_eq!(signal, Some(ControlSignal::ShutdownRequested));
    }

    #[test]
    fn control_reply_shutdown_unauthenticated_is_refused_with_no_signal() {
        // Issue #397 (AC): `shutdown` is state-affecting — it ends the process — so an
        // UNauthenticated peer is refused with `{"error":"unauthorized"}` and produces NO signal:
        // a stranger can never stop the daemon (parity with the `manual-swapped` #64 /
        // `roster-reload` #139 / `restored` #275 same-user gate). Fail-closed on the auth verdict.
        let snap = StatusSnapshot::default();
        let (reply, signal) = control_reply(r#"{"cmd":"shutdown"}"#, &snap, false);
        assert_eq!(reply, r#"{"error":"unauthorized"}"#);
        assert_eq!(signal, None);
    }

    #[tokio::test]
    async fn notify_restored_sends_the_uuid_command_and_reads_the_ack() {
        // Issue #276: the client-side `restored` notify writes exactly one newline-delimited
        // `{"cmd":"restored","uuid":"<uuid>"}` request — the uuid embedded and escaped by
        // serde_json (unlike the payload-less `roster-reload`) — and returns Ok once the daemon
        // acks. This is the CLI→daemon wire contract that #275's `control_reply` handler parses
        // back into `Restored("u-B")`, closing the loop `reconcile_login` (#276) drives.
        use tokio::io::{AsyncBufReadExt, AsyncWriteExt};
        use tokio::net::UnixListener;

        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("daemon.sock");
        let listener = UnixListener::bind(&socket).unwrap();

        // Server: accept one connection, assert the exact request line, ack once.
        let server = async move {
            let (stream, _addr) = listener.accept().await.unwrap();
            let mut buffered = tokio::io::BufReader::new(stream);
            let mut request = String::new();
            buffered.read_line(&mut request).await.unwrap();
            assert_eq!(request.trim_end(), r#"{"cmd":"restored","uuid":"u-B"}"#);
            buffered.write_all(br#"{"ok":true}"#).await.unwrap();
            buffered.write_all(b"\n").await.unwrap();
            buffered.flush().await.unwrap();
        };

        let (_, result) = tokio::join!(server, notify_restored(&socket, "u-B"));
        assert!(
            result.is_ok(),
            "a served restored notify returns Ok: {result:?}"
        );
    }

    #[tokio::test]
    async fn notify_restored_errs_when_no_daemon_is_listening() {
        // Issue #276 (AC-2): with no socket bound, the notify surfaces an Err — which the
        // best-effort `notify_daemon_restored` wrapper logs and swallows, so `login` still
        // succeeds (the on-disk stash/roster write is authoritative). A missing / wedged daemon
        // must never fail the verb — the daemon-down counterpart of the roster-reload best-effort
        // contract (#139) and the `use` manual-hold notify (#64).
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("daemon.sock"); // never bound
        assert!(notify_restored(&socket, "u-B").await.is_err());
    }

    #[tokio::test]
    async fn adopt_roster_reload_reads_the_new_roster_from_disk() {
        // AC (end-to-end, no torn read): with a config path wired, the reload re-reads
        // the freshly-written `config.toml` and reconciles the in-memory roster to it —
        // onboarding the new account while preserving a persisting account's state. The
        // on-disk file is written whole, exactly as production's atomic rename leaves it.
        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("config.toml");
        write_roster_config(
            &config_path,
            &[("u-A", "work"), ("u-B", "spare"), ("u-C", "third")],
        );

        let mut daemon: FakeDaemon =
            reconcile_daemon(vec![account("u-A", "work"), account("u-B", "spare")])
                .with_config_path(config_path);
        daemon.state.active = Some(0);
        daemon.state.accounts[0].health.quarantined = true; // A's state must survive the reload

        let event = daemon.adopt_roster_reload(ReloadIntent::AppendOnly).await;

        assert_eq!(roster_uuids(&daemon), vec!["u-A", "u-B", "u-C"]);
        assert!(
            daemon.state.accounts[0].health.quarantined,
            "A's state preserved"
        );
        assert_eq!(daemon.state.active, Some(0));
        // Issue #1438 AC-1: the ADOPT path returns its own durable record, carrying the count
        // PAIR (2 -> 3) that makes a widening — or, reversed, a collapse — readable with no
        // access to the daemon. No `reason`: nothing was refused and nothing failed.
        assert_eq!(
            event,
            Event::RosterReload {
                outcome: RosterReloadOutcome::Adopted,
                previous: Some(2),
                incoming: Some(3),
                reason: None,
            }
        );
    }

    #[tokio::test]
    async fn adopt_roster_reload_keeps_the_current_roster_on_a_malformed_config() {
        // Best-effort: a malformed / mid-edit `config.toml` never drops the live
        // rotation — the current in-memory roster is kept and the reload is skipped.
        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("config.toml");
        std::fs::write(&config_path, b"]not valid toml[").unwrap();

        let mut daemon: FakeDaemon =
            reconcile_daemon(vec![account("u-A", "work"), account("u-B", "spare")])
                .with_config_path(config_path);

        let event = daemon.adopt_roster_reload(ReloadIntent::AppendOnly).await;

        assert_eq!(
            roster_uuids(&daemon),
            vec!["u-A", "u-B"],
            "roster unchanged on a bad read"
        );
        // Issue #1438 AC-1/AC-3: the failure is RECORDED now, not printed to a background
        // process's stderr. The file was THERE and would not parse, so the code is `unreadable`
        // — distinct from the `absent` mid-write race below — and there is no incoming count,
        // because no roster was ever read.
        assert_eq!(
            event,
            Event::RosterReload {
                outcome: RosterReloadOutcome::Failed,
                previous: Some(2),
                incoming: None,
                reason: Some(RosterReloadReason::Unreadable),
            }
        );
    }

    #[tokio::test]
    async fn adopt_roster_reload_refuses_and_records_it_without_a_config_path() {
        // With no config path wired (the hermetic default), a reload signal reads nothing and
        // leaves the roster as-is — but since issue #1438 it is no longer a SILENT no-op: it
        // returns a `refused` record naming why reload is not armed. `previous` is the live
        // roster size; `incoming` is ABSENT rather than `0`, which would claim the file presented
        // an empty roster.
        let mut daemon = reconcile_daemon(vec![account("u-A", "work")]);

        let event = daemon.adopt_roster_reload(ReloadIntent::AppendOnly).await;

        assert_eq!(roster_uuids(&daemon), vec!["u-A"]);
        assert_eq!(
            event,
            Event::RosterReload {
                outcome: RosterReloadOutcome::Refused,
                previous: Some(1),
                incoming: None,
                reason: Some(RosterReloadReason::ReloadDisabled),
            }
        );
    }

    #[tokio::test]
    async fn adopt_roster_reload_reports_an_absent_config_apart_from_an_unreadable_one() {
        // Issue #1438 AC-1, the fourth return path: a config path IS wired but the file is not
        // there — the benign mid-write race the #139 best-effort contract exists for. It reports
        // `absent`, never `unreadable`: a file briefly missing and a file that is there and wrong
        // are different operator situations, and `Error::ConfigNotFound` already separates them
        // for free.
        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("config.toml"); // never written

        let mut daemon: FakeDaemon =
            reconcile_daemon(vec![account("u-A", "work"), account("u-B", "spare")])
                .with_config_path(config_path);

        let event = daemon.adopt_roster_reload(ReloadIntent::AppendOnly).await;

        assert_eq!(
            roster_uuids(&daemon),
            vec!["u-A", "u-B"],
            "roster unchanged when the file is absent"
        );
        assert_eq!(
            event,
            Event::RosterReload {
                outcome: RosterReloadOutcome::Failed,
                previous: Some(2),
                incoming: None,
                reason: Some(RosterReloadReason::Absent),
            }
        );
    }

    #[tokio::test]
    async fn a_roster_reload_that_shrinks_the_fleet_renders_the_whole_pair() {
        // Issue #1438 AC-2, driven end to end through the daemon: the 2026-08-27 shape — a
        // daemon holding SIX accounts reloads a `config.toml` presenting ONE. The assertion is
        // on the RENDERED line, not on the returned value, because the criterion is that an
        // operator holding only the log can tell a collapse from a widening. `adopted 1` says
        // nothing; `adopted 1, was 6` is the whole event.
        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("config.toml");
        write_roster_config(&config_path, &[("u-A", "work")]);

        let mut daemon: FakeDaemon = reconcile_daemon(vec![
            account("u-A", "work"),
            account("u-B", "spare"),
            account("u-C", "third"),
            account("u-D", "fourth"),
            account("u-E", "fifth"),
            account("u-F", "sixth"),
        ])
        .with_config_path(config_path);

        let line = daemon
            .adopt_roster_reload(ReloadIntent::Mutating)
            .await
            .to_log_line(std::time::UNIX_EPOCH);

        assert_eq!(
            roster_uuids(&daemon),
            vec!["u-A"],
            "five accounts left the live rotation"
        );
        // Read TOKEN BY TOKEN rather than pinning the whole rendered tail. This grammar is
        // explicitly extensible — `Event::Swap`'s `late=true` is documented as "a trailing
        // `key=val` existing parsers ignore", and this event is chartered to be extended by the
        // two follow-up items it was defined for — so an `ends_with` over the tail would fail on
        // a purely ADDITIVE change that broke nothing for any reader, which is a test that has to
        // be edited to stay green rather than one that catches a regression. `field_of` is the
        // shipped readers' own tokenization, so this asserts what an operator's tooling sees.
        assert_eq!(
            field_of(&line, "event"),
            Some("roster_reload"),
            "the event kind still names this event: {line}"
        );
        assert_eq!(
            field_of(&line, "outcome"),
            Some("adopted"),
            "the reload adopted: {line}"
        );
        assert_eq!(
            field_of(&line, "previous"),
            Some("6"),
            "the `was` half of the pair, without which the collapse is invisible: {line}"
        );
        assert_eq!(
            field_of(&line, "incoming"),
            Some("1"),
            "and the half it collapsed to: {line}"
        );
    }

    #[tokio::test]
    async fn no_roster_reload_line_carries_a_path_account_or_error_text() {
        // Issue #1438 AC-5, asserted rather than claimed in a comment. Every position that could
        // leak is filled with material nothing else in the tree would produce, and ALL FOUR
        // daemon-side return paths are driven — refused, absent, unreadable and adopted, which is
        // every path `adopt_roster_reload` can take. `Error::ConfigNotFound` carries the config
        // `PathBuf` and a TOML parse error quotes file CONTENT, so this is exactly what the
        // classify-and-drop discipline in the handler exists for: the event log has a WIDER
        // AUDIENCE than the mode-`0o600` file the roster lives in.
        //
        // The refused path needs a SECOND daemon, holding the same secret-laden roster with no
        // `config_path` wired. It is the one return path that never opens the file, so what it
        // could leak is the in-memory roster rather than a discarded error — a different surface,
        // which nothing else here covers.
        const SECRET_SEGMENT: &str = "zz-secret-roster-dir";
        const SECRET_LABEL: &str = "zz-secret-label";
        const SECRET_UUID: &str = "zz-secret-uuid";

        let dir = tempfile::tempdir().unwrap();
        let nested = dir.path().join(SECRET_SEGMENT);
        std::fs::create_dir(&nested).unwrap();
        let config_path = nested.join("config.toml");

        let mut daemon: FakeDaemon = reconcile_daemon(vec![account(SECRET_UUID, SECRET_LABEL)])
            .with_config_path(config_path.clone());
        let render = |event: Event| event.to_log_line(std::time::UNIX_EPOCH);

        // (a) absent — the dropped error carries the whole config path.
        let absent = render(daemon.adopt_roster_reload(ReloadIntent::AppendOnly).await);
        // The sweep below asserts the secrets are NOT on the line, which is evidence only if they
        // were AVAILABLE to leak — so assert positively, here, that the error the handler
        // classifies and discards really does carry them. Without this, a `toml` bump that stopped
        // quoting the source excerpt, or an `Error::ConfigNotFound` narrowed to drop its path,
        // would make the assertions below vacuous on the exact dimension they exist for: silently,
        // and while staying green.
        let absent_err = Config::load_path(&config_path).unwrap_err().to_string();
        assert!(
            absent_err.contains(SECRET_SEGMENT),
            "canary: the absent-file error no longer carries the config path, so the redaction \
             assertion on this path proves nothing: {absent_err}"
        );

        // (b) unreadable — the dropped TOML error quotes the file's own content.
        std::fs::write(&config_path, format!("]not valid toml[ {SECRET_LABEL}")).unwrap();
        let unreadable = render(daemon.adopt_roster_reload(ReloadIntent::AppendOnly).await);
        let unreadable_err = Config::load_path(&config_path).unwrap_err().to_string();
        assert!(
            unreadable_err.contains(SECRET_LABEL),
            "canary: the parse error no longer quotes the file's content, so the redaction \
             assertion on this path proves nothing: {unreadable_err}"
        );

        // (c) adopted — the roster that WAS read is full of distinctive handles.
        write_roster_config(&config_path, &[(SECRET_UUID, SECRET_LABEL)]);
        let adopted = render(daemon.adopt_roster_reload(ReloadIntent::AppendOnly).await);

        // (d) refused — no `config_path` at all, so the file is never opened and the only thing
        // in reach is the live roster, which carries both handles.
        let mut unarmed: FakeDaemon = reconcile_daemon(vec![account(SECRET_UUID, SECRET_LABEL)]);
        let refused = render(unarmed.adopt_roster_reload(ReloadIntent::AppendOnly).await);
        assert!(
            roster_uuids(&unarmed)
                .iter()
                .any(|uuid| uuid == SECRET_UUID),
            "canary: the refused daemon does not hold the secret handle, so its redaction \
             assertion proves nothing"
        );

        let lines = [absent, unreadable, adopted, refused];
        for line in &lines {
            assert!(!line.contains(SECRET_SEGMENT), "no path segment: {line}");
            assert!(!line.contains(SECRET_LABEL), "no account label: {line}");
            assert!(!line.contains(SECRET_UUID), "no account uuid: {line}");
            // No path AT ALL, not merely not this one: the line carries counts and bare machine
            // codes, and none of those can contain a separator.
            assert!(!line.contains('/'), "no filesystem path: {line}");
            assert!(!line.contains('@'), "no email sigil: {line}");
            assert!(!line.to_lowercase().contains("token"), "no token: {line}");
            assert_eq!(line.lines().count(), 1, "exactly one record: {line}");
        }
        // The sweep above is evidence only if it ran on the four distinct paths it names — a loop
        // over an empty or collapsed set would pass having checked nothing.
        assert_eq!(
            lines
                .iter()
                .collect::<std::collections::BTreeSet<_>>()
                .len(),
            4,
            "four DISTINCT paths driven: {lines:?}"
        );
    }

    #[tokio::test]
    async fn a_tunables_only_config_reloads_as_incoming_zero_not_as_an_unread_roster() {
        // Issue #1438's absent-versus-zero rule, driven END TO END rather than only rendered.
        // `Config::load_path` accepts a well-formed file carrying an EMPTY roster — its own doc
        // says the "at least one account" rule is `Config::require_roster`, a daemon precondition
        // and not a parse-time one, and `adopt_roster_reload` never calls it — so a `config.toml`
        // with tunables and no `[[account]]` table loads cleanly and reconciles the live rotation
        // to nothing. That is the most alarming reload there is, and it must not render like the
        // path that never read the file at all.
        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("config.toml");
        write_roster_config(&config_path, &[]); // valid, tunables-only: no `[[account]]`

        let mut daemon: FakeDaemon =
            reconcile_daemon(vec![account("u-A", "work"), account("u-B", "spare")])
                .with_config_path(config_path);

        let line = daemon
            .adopt_roster_reload(ReloadIntent::Mutating)
            .await
            .to_log_line(std::time::UNIX_EPOCH);

        assert!(
            roster_uuids(&daemon).is_empty(),
            "the whole live rotation was replaced by nothing"
        );
        assert_eq!(
            field_of(&line, "outcome"),
            Some("adopted"),
            "an empty roster is READ and adopted, not a read failure: {line}"
        );
        assert_eq!(
            field_of(&line, "previous"),
            Some("2"),
            "the `was` half of the pair: {line}"
        );
        assert_eq!(
            field_of(&line, "incoming"),
            Some("0"),
            "an empty roster read is a RENDERED zero, never an omitted token: {line}"
        );

        // `previous = 0` is reachable, and only from here: the daemon now HOLDS an empty roster,
        // so the next reload reports zero as the `was` half too.
        let again = daemon
            .adopt_roster_reload(ReloadIntent::Mutating)
            .await
            .to_log_line(std::time::UNIX_EPOCH);
        assert_eq!(
            field_of(&again, "previous"),
            Some("0"),
            "a previously-empty rotation is `previous=0`, not an omission: {again}"
        );
        assert_eq!(
            field_of(&again, "incoming"),
            Some("0"),
            "and still: {again}"
        );

        // The contrast belongs in this same test, or either assertion above is satisfied by a
        // renderer that collapses zero into absence: the refusal path reads nothing, and OMITS.
        let mut unarmed: FakeDaemon = reconcile_daemon(vec![account("u-A", "work")]);
        let refused = unarmed
            .adopt_roster_reload(ReloadIntent::AppendOnly)
            .await
            .to_log_line(std::time::UNIX_EPOCH);
        assert_eq!(
            field_of(&refused, "incoming"),
            None,
            "a count the path never read has no token at all: {refused}"
        );
        assert!(
            !refused.contains("incoming="),
            "not the key with an empty value either: {refused}"
        );
    }

    #[tokio::test]
    async fn every_daemon_reload_path_pairs_its_outcome_with_the_right_reason() {
        // Issue #1438 states in three places that `reason` rides the non-adopted outcomes only,
        // and nothing held it: `reason` is a flat `Option` beside `outcome`, so
        // `Adopted { reason: Some(..) }` is representable and would render. Restructuring the
        // variant to make it unrepresentable would change the shape every consumer matches on —
        // far more than the convention is worth — so the convention is CHECKED here instead, on
        // every path that can produce it.
        //
        // "Every emitter" means every DAEMON path: `adopt_roster_reload` is the only emitter that
        // can produce `Adopted` at all. The CLI half (`crate::capture`'s
        // `emit_roster_reload_not_notified`) constructs `RosterReloadOutcome::NotNotified`
        // literally and takes only the reason as a parameter, so it cannot reach this pairing
        // from any input.
        let pairing = |event: &Event| match event {
            Event::RosterReload {
                outcome, reason, ..
            } => (*outcome, *reason),
            other => panic!("not a roster-reload event: {other:?}"),
        };

        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("config.toml");

        // (a) refused — no config path wired.
        let mut unarmed: FakeDaemon = reconcile_daemon(vec![account("u-A", "work")]);
        let refused = unarmed.adopt_roster_reload(ReloadIntent::AppendOnly).await;

        let mut daemon: FakeDaemon =
            reconcile_daemon(vec![account("u-A", "work")]).with_config_path(config_path.clone());
        // (b) failed / absent, (c) failed / unreadable, (d) adopted — in that order, so one
        // daemon walks the file from missing through malformed to valid.
        let absent = daemon.adopt_roster_reload(ReloadIntent::AppendOnly).await;
        std::fs::write(&config_path, "]not valid toml[").unwrap();
        let unreadable = daemon.adopt_roster_reload(ReloadIntent::AppendOnly).await;
        write_roster_config(&config_path, &[("u-A", "work")]);
        let adopted = daemon.adopt_roster_reload(ReloadIntent::AppendOnly).await;
        // (e) refused / shrink — issue #1442's post-read refusal, and the reason this test had to
        // grow: its subject is "every path that can produce a pairing", so a NEW emitting path
        // silently outside the sweep would leave the convention unchecked exactly where it is
        // newest. The daemon holds one account and the file below presents none, under the
        // append-only intent that makes that a refusal rather than a legitimate removal.
        let mut shrinking: FakeDaemon =
            reconcile_daemon(vec![account("u-A", "work")]).with_config_path(config_path.clone());
        write_roster_config(&config_path, &[]);
        let shrunk = shrinking
            .adopt_roster_reload(ReloadIntent::AppendOnly)
            .await;

        let paths = [&refused, &absent, &unreadable, &adopted, &shrunk];
        for event in paths {
            let (outcome, reason) = pairing(event);
            assert_eq!(
                matches!(outcome, RosterReloadOutcome::Adopted),
                reason.is_none(),
                "`reason` rides the NON-adopted outcomes only: {outcome:?} paired with {reason:?}"
            );
        }
        // Cardinality, so the loop is evidence: five paths, and all five distinct — a loop over a
        // collapsed set would satisfy the assertion above having checked one path twice. The two
        // REFUSALS are distinct pairings, which is the property issue #1442 added: `reload_disabled`
        // and `shrink` ride the same outcome and are told apart only by their reason.
        assert_eq!(
            paths
                .iter()
                .map(|event| format!("{:?}", pairing(event)))
                .collect::<std::collections::BTreeSet<_>>()
                .len(),
            paths.len(),
            "every driven path must be a DISTINCT (outcome, reason) pair"
        );
        assert!(
            matches!(pairing(&adopted).0, RosterReloadOutcome::Adopted),
            "canary: the adopted path did not adopt, so the only row that can carry the forbidden \
             pairing was never exercised: {adopted:?}"
        );
    }

    #[test]
    fn the_reload_handler_prints_nothing_it_only_returns_the_event() {
        // Issue #1438 AC: the daemon `eprintln!` is GONE, not supplemented — the daemon runs in
        // the background, so its stderr has no reader, and a reload that printed there was the
        // blind spot the event exists to close. Nothing behavioural can see a print, so this is
        // asserted on the handler's own SOURCE: re-adding the line is the regression, and it
        // would otherwise land silently and leave the whole suite green.
        let source = commands_source_above_the_tests();
        let body = method_body(&source, "async fn adopt_roster_reload");
        assert!(
            !body.contains("eprintln!"),
            "`adopt_roster_reload` prints again; the event is the durable report, and stderr on a \
             background process is not a report at all:\n{body}"
        );
        assert!(
            !body.contains("println!"),
            "`adopt_roster_reload` writes to stdout, which on the daemon is not a channel at \
             all:\n{body}"
        );
        // Canary: the body was actually extracted, so the two absences above are evidence rather
        // than the shape of an empty string. Both anchors are code the handler cannot lose while
        // still doing its job.
        assert!(
            body.contains("RosterReloadOutcome::Adopted"),
            "the extraction returned no handler body, so nothing above was checked:\n{body}"
        );
        assert!(
            body.contains("Config::load_path"),
            "the extraction stopped before the handler's read, so it covered only part of \
             it:\n{body}"
        );
    }

    #[tokio::test]
    async fn a_refused_shrink_records_both_counts_on_the_reload_line() {
        // Issue #1442 AC-1, end to end through the handler: the incident's own shape reaching the
        // daemon over the wire — a live SIX-account daemon, a `config.toml` presenting ONE, and
        // append-only intent. The roster survives and the refusal is LEGIBLE afterwards.
        //
        // Both counts, or the line says nothing. "refused 1" cannot be told from a daemon that
        // refused a widening; `refused 1, was 6` is the whole event, and a shrink is not
        // expressible in either number alone. Asserted on the RENDERED line — that is what an
        // operator holding only the log actually reads — and token by token rather than over the
        // tail, because this grammar is additively extensible by design.
        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("config.toml");
        write_roster_config(&config_path, &[("u-A", "work")]);

        let mut daemon: FakeDaemon = reconcile_daemon(vec![
            account("u-A", "work"),
            account("u-B", "spare"),
            account("u-C", "third"),
            account("u-D", "fourth"),
            account("u-E", "fifth"),
            account("u-F", "sixth"),
        ])
        .with_config_path(config_path);

        let line = daemon
            .adopt_roster_reload(ReloadIntent::AppendOnly)
            .await
            .to_log_line(std::time::UNIX_EPOCH);

        assert_eq!(
            roster_uuids(&daemon),
            vec!["u-A", "u-B", "u-C", "u-D", "u-E", "u-F"],
            "the live rotation is unchanged — five accounts did NOT leave it"
        );
        assert_eq!(field_of(&line, "event"), Some("roster_reload"), "{line}");
        assert_eq!(
            field_of(&line, "outcome"),
            Some("refused"),
            "the daemon DECLINED — it read the file and decided against it: {line}"
        );
        assert_eq!(
            field_of(&line, "reason"),
            Some("shrink"),
            "and the reason separates this from the reload-disabled refusal: {line}"
        );
        assert_eq!(
            field_of(&line, "previous"),
            Some("6"),
            "the `was` half of the pair: {line}"
        );
        assert_eq!(
            field_of(&line, "incoming"),
            Some("1"),
            "and the half a post-read refusal can report, unlike reload_disabled: {line}"
        );
    }

    #[tokio::test]
    async fn a_mutating_reload_shrinks_the_live_roster_through_the_handler() {
        // The other side of the same handler, so the test above is evidence of a PARTITION rather
        // than of a daemon that refuses every shrink. Identical file, identical daemon, identical
        // call — only the intent differs, and the roster legitimately collapses to one.
        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("config.toml");
        write_roster_config(&config_path, &[("u-A", "work")]);

        let mut daemon: FakeDaemon = reconcile_daemon(vec![
            account("u-A", "work"),
            account("u-B", "spare"),
            account("u-C", "third"),
        ])
        .with_config_path(config_path);

        let line = daemon
            .adopt_roster_reload(ReloadIntent::Mutating)
            .await
            .to_log_line(std::time::UNIX_EPOCH);

        assert_eq!(roster_uuids(&daemon), vec!["u-A"]);
        assert_eq!(field_of(&line, "outcome"), Some("adopted"), "{line}");
        assert_eq!(
            field_of(&line, "reason"),
            None,
            "an adoption has no reason: {line}"
        );
    }

    #[test]
    fn an_omitted_or_unrecognized_intent_is_read_as_append_only() {
        // PRD R-3a, the fail-closed default, asserted at the boundary that decides it.
        //
        // THREE inputs, because the requirement's own reasoning covers a rollout in both
        // directions. An omitted token is the legacy shape — `notify_daemon_roster_reload` took no
        // arguments before this issue, so an older CLI against a newer daemon sends exactly that.
        // An unrecognized token is the mirror: a newer CLI against an older daemon, or a third
        // intent added later. Neither can be read as permission to shrink, so both resolve to the
        // refusing treatment, and only the exact `mutating` token opts in.
        assert_eq!(ReloadIntent::from_wire(None), ReloadIntent::AppendOnly);
        assert_eq!(
            ReloadIntent::from_wire(Some("banana")),
            ReloadIntent::AppendOnly,
            "a token this daemon cannot interpret is not permission to lose accounts"
        );
        assert_eq!(
            ReloadIntent::from_wire(Some("")),
            ReloadIntent::AppendOnly,
            "and neither is an empty one"
        );
        // The one opt-in, and the round trip that keeps the two halves of the protocol spelling
        // the same tokens: whatever `as_wire` emits, `from_wire` must read back unchanged.
        for intent in [ReloadIntent::AppendOnly, ReloadIntent::Mutating] {
            assert_eq!(
                ReloadIntent::from_wire(Some(intent.as_wire())),
                intent,
                "`{}` does not round-trip",
                intent.as_wire()
            );
        }
    }

    #[test]
    fn control_reply_reads_the_roster_reload_intent_off_the_request() {
        // The wire half of R-3a: the resolution above has to actually run on the served line.
        // A `roster-reload` request carrying `mutating` yields the permissive signal; one carrying
        // an unknown token, or none at all, yields the refusing one.
        let snap = StatusSnapshot::default();
        for (line, expected) in [
            (
                r#"{"cmd":"roster-reload","intent":"mutating"}"#,
                ReloadIntent::Mutating,
            ),
            (
                r#"{"cmd":"roster-reload","intent":"append-only"}"#,
                ReloadIntent::AppendOnly,
            ),
            (
                r#"{"cmd":"roster-reload","intent":"whatever"}"#,
                ReloadIntent::AppendOnly,
            ),
            (r#"{"cmd":"roster-reload"}"#, ReloadIntent::AppendOnly),
        ] {
            let (reply, signal) = control_reply(line, &snap, true);
            assert_eq!(reply, r#"{"ok":true}"#, "{line}");
            assert_eq!(
                signal,
                Some(ControlSignal::RosterReloadRequested(expected)),
                "{line}"
            );
        }
        // An UNauthenticated peer still produces no signal, whatever intent it declares — a
        // stranger can neither trigger a reload nor choose how it is treated.
        let (reply, signal) = control_reply(
            r#"{"cmd":"roster-reload","intent":"mutating"}"#,
            &snap,
            false,
        );
        assert_eq!(reply, r#"{"error":"unauthorized"}"#);
        assert_eq!(signal, None);
    }

    #[tokio::test]
    async fn the_client_notify_puts_its_intent_on_the_wire() {
        // The seam nothing else covers, and the one that decides whether "intent travels" is TRUE
        // rather than merely typed. `control_reply` is tested above against hand-written JSON, and
        // `notify_daemon_roster_reload` is tested through its callers — so a `notify_roster_reload`
        // that took the parameter and sent the old payload-less line would leave every other test
        // in this file green while the daemon refused, or adopted, on a default nobody chose.
        //
        // Asserted by round-tripping the REAL client against a real socket and feeding what it
        // sent to the REAL server-side parser: no hand-written request line in the middle, so the
        // two halves of this protocol are checked against each other rather than against a
        // literal that could drift with them.
        use tokio::io::AsyncBufReadExt;
        use tokio::net::UnixListener;

        for intent in [ReloadIntent::Mutating, ReloadIntent::AppendOnly] {
            let dir = tempfile::tempdir().unwrap();
            let socket = dir.path().join("daemon.sock");
            let listener = UnixListener::bind(&socket).unwrap();

            let server = async {
                let (stream, _addr) = listener.accept().await.unwrap();
                let mut buffered = tokio::io::BufReader::new(stream);
                let mut request = String::new();
                buffered.read_line(&mut request).await.unwrap();
                // The client waits for an ack before returning; drop the connection instead, which
                // the notify treats as a best-effort failure. What it SENT is already captured.
                request
            };
            let (sent, _) =
                tokio::join!(server, crate::daemon::notify_roster_reload(&socket, intent));

            let (reply, signal) = control_reply(sent.trim_end(), &StatusSnapshot::default(), true);
            assert_eq!(reply, r#"{"ok":true}"#, "the daemon parsed it: {sent}");
            assert_eq!(
                signal,
                Some(ControlSignal::RosterReloadRequested(intent)),
                "the intent the client sent is the intent the daemon read: {sent}"
            );
        }
    }

    #[test]
    fn every_reconcile_roster_call_site_reaches_the_one_shared_floor() {
        // Issue #1442 AC + design D-4, and the distinction is the point: this asserts every call
        // site is covered BY the invariant, NOT that every caller checks. Those are different
        // properties and only one of them survives a fourth caller.
        //
        // The investigation that found this defect enumerated TWO of the three call sites. A
        // per-caller guard written from that enumeration would have left `perform_config_set`
        // reconciling with no floor at all, which is the original defect with extra steps — so the
        // structural claim is what needs asserting, and nothing behavioural can see it.
        let source = commands_source_above_the_tests();

        // (1) The floor is written ONCE. A caller growing its own copy fails here even if that
        // copy is correct, because two copies drift and the second one is invisible.
        const PREDICATE: &str = "new_roster.len() < self.roster.len()";
        assert_eq!(
            source.matches(PREDICATE).count(),
            1,
            "the shrink comparison must exist exactly once, in `reconcile_roster` itself"
        );
        assert!(
            method_body(&source, "pub(super) fn reconcile_roster").contains(PREDICATE),
            "the one comparison is not inside `reconcile_roster`"
        );

        // (2) Every call site, enumerated with the intent it must declare. Extracted from each
        // caller's own body, so a site that moved to a different function fails rather than being
        // matched by a coincidental substring elsewhere in the file.
        const CALL: &str = "self.reconcile_roster(";
        let sites = [
            ("pub(super) async fn perform_socket_capture", "AppendOnly"),
            ("pub(super) async fn perform_config_set", "AppendOnly"),
            ("pub(super) async fn adopt_roster_reload", "intent"),
        ];
        for (signature, intent) in sites {
            let body = method_body(&source, signature);
            let at = body.find(CALL).unwrap_or_else(|| {
                panic!(
                    "`{signature}` no longer reconciles — this enumeration has gone stale:\n{body}"
                )
            });
            let call: String = body[at..].chars().take_while(|c| *c != ';').collect();
            assert!(
                call.contains(intent),
                "`{signature}` must reconcile with `{intent}` intent, and does not: {call}"
            );
        }

        // (3) Cardinality, so (2) is evidence rather than a sample. A FOURTH caller cannot reach
        // the roster without an intent — the parameter is required, so that much the compiler
        // holds — but it can reach it with the WRONG one, silently, and nothing above would look.
        // Failing here forces the new site into the enumeration, where its intent is a decision
        // somebody made rather than one that defaulted.
        //
        // Counted over the WHOLE `daemon` module and by BARE method name, not over this file and
        // not by `self.`. `reconcile_roster` is `pub(super)`, so every file in `crate::daemon` can
        // reach it, and a caller in one of them writes `daemon.reconcile_roster(` — which a
        // this-file, `self.`-anchored count misses on both axes at once. `run_loop.rs` already
        // holds a `&mut Daemon` and calls sibling methods on it, so that fourth site is a plausible
        // edit and not a hypothetical.
        let module_calls: usize = daemon_module_sources()
            .iter()
            .map(|(_, text)| text.matches("reconcile_roster(").count())
            .sum();
        assert_eq!(
            module_calls,
            sites.len() + 1,
            "a `reconcile_roster` call site was added or removed somewhere in `crate::daemon`; add \
             it to `sites` above with the intent its verb implies (the +1 is the definition)"
        );

        // (4) The floor governs the FIELD, not merely this method — which (1) through (3) do not
        // establish. A guard placed perfectly inside `reconcile_roster` is INERT if anything else
        // can assign `self.roster`, and the capture path is one line away from doing exactly that:
        // `perform_socket_capture` already holds the freshly-read config, so
        // `self.roster = config.roster.clone();` written beside its reconcile call reinstates the
        // 2026-08-27 collapse while every assertion above still passes. So the module holds
        // exactly ONE assignment to the field, and it is the one inside `reconcile_roster`.
        const WRITE: &str = ".roster = ";
        let writers: Vec<String> = daemon_module_sources()
            .iter()
            .flat_map(|(file, text)| std::iter::repeat_n(file.clone(), text.matches(WRITE).count()))
            .collect();
        assert_eq!(
            writers.len(),
            1,
            "the daemon's roster field must have exactly ONE writer, so the floor cannot be \
             bypassed by assigning it directly; found {writers:?}"
        );

        // And that one writer is behind the guard, not in front of it. Position, not membership:
        // moving the assignment ABOVE the comparison leaves the count at one and every earlier
        // assertion green, while making a refusal a partial reshape instead of a true no-op.
        let floor = method_body(&source, "pub(super) fn reconcile_roster");
        let compared = floor
            .find(PREDICATE)
            .expect("the comparison is in this body — asserted in (1)");
        let wrote = floor.find(WRITE).unwrap_or_else(|| {
            panic!("the one `{WRITE}` writer is not inside `reconcile_roster`:\n{floor}")
        });
        assert!(
            compared < wrote,
            "`reconcile_roster` must compare the counts BEFORE it assigns the roster — a refusal \
             that has already written is not the no-op the floor promises"
        );
    }

    /// Every file of the `crate::daemon` module, each cut above its own test block and
    /// stripped of comments.
    ///
    /// The scope `reconcile_roster`'s `pub(super)` visibility actually grants — which is what the
    /// cardinality guard has to search, since a caller added in a sibling file is exactly the case
    /// a this-file count cannot see. Enumerated from the directory rather than listed, so a NEW
    /// file in the module is searched the day it lands instead of the day someone remembers it.
    /// `text` above its column-0 test block, with comments removed.
    ///
    /// Both halves matter to what the corpus is evidence OF. The cut drops the tests, because a
    /// test naming a method is not a caller of it. The strip drops the prose, because this module
    /// documents its own methods by name and at length — so a doc comment reading
    /// `reconcile_roster(...)` would otherwise be counted as a fourth call site and fail a guard
    /// for a reason nobody could read.
    fn code_above_the_tests(text: &str) -> String {
        text.split("\n#[cfg(test)]\nmod tests")
            .next()
            .expect("split always yields a first element")
            .lines()
            .filter(|line| !line.trim_start().starts_with("//"))
            .map(strip_trailing_comment)
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn daemon_module_sources() -> Vec<(String, String)> {
        let mut sources: Vec<(String, String)> = std::fs::read_dir("src/daemon")
            .expect("cannot read src/daemon")
            .map(|entry| entry.expect("cannot stat a src/daemon entry").path())
            .filter(|path| path.extension().is_some_and(|ext| ext == "rs"))
            .map(|path| {
                let text = std::fs::read_to_string(&path)
                    .unwrap_or_else(|err| panic!("cannot read {}: {err}", path.display()));
                (path.display().to_string(), code_above_the_tests(&text))
            })
            .collect();
        // `src/daemon.rs` is the module root and sits beside the directory, not inside it.
        let root = std::fs::read_to_string("src/daemon.rs").expect("cannot read src/daemon.rs");
        sources.push(("src/daemon.rs".to_owned(), code_above_the_tests(&root)));
        // Canary: the module is not empty and every file was actually cut, so a count over this
        // corpus is evidence rather than the shape of an empty read.
        assert!(
            sources.len() > 5,
            "only {} daemon module files found — the enumeration is not seeing the module",
            sources.len()
        );
        assert!(
            sources.iter().all(|(_, text)| !text.is_empty()),
            "a daemon module file cut to nothing"
        );
        sources
    }

    /// This file's source above its test block — the subject of
    /// [`the_reload_handler_prints_nothing_it_only_returns_the_event`].
    ///
    /// Cut at a column-0 `#[cfg(test)]`, which in this file is the test module and nothing else.
    /// The caller canaries what it extracted, so a boundary that moved shows up as a failure
    /// rather than as a green run over an empty subject.
    ///
    /// The idiom is `crate::capture`'s `capture_source_above_the_tests` / `fn_body` pair (issue
    /// #1440), re-stated rather than imported: both are private items of that module's own
    /// `mod tests`, and the brace rule differs anyway — see [`method_body`].
    fn commands_source_above_the_tests() -> String {
        let text = std::fs::read_to_string("src/daemon/commands.rs")
            .expect("cannot read src/daemon/commands.rs");
        text.split("\n#[cfg(test)]\nmod tests")
            .next()
            .expect("split always yields a first element")
            .to_owned()
    }

    /// The CODE of `source` from the line opening `signature` to the next column-FOUR `}` — one
    /// `impl` METHOD body, brace-delimited by the file's own `cargo fmt`-guaranteed layout, with
    /// comments removed.
    ///
    /// Column four is the whole difference from `crate::capture`'s `fn_body`, which cuts at column
    /// zero. The subject here is a method, whose closing brace `rustfmt` indents one level; the
    /// column-0 `}` belongs to the enclosing `impl` block and would swallow every sibling method
    /// declared after this one, turning a targeted absence check into a file-wide one.
    ///
    /// The comment strip is what makes the caller measure code rather than prose. It asks whether
    /// the handler PRINTS, and this file's comments name `eprintln!` precisely because the print
    /// was removed — so without the strip, a comment saying so would read as the call itself.
    /// Trailing comments are stripped as well as whole-line ones, since a trailing one can carry
    /// the same token. A `//` inside a string literal is left alone (detected by an even count of
    /// unescaped quotes to its left), so the strip cannot eat code.
    fn method_body(source: &str, signature: &str) -> String {
        let from = source
            .find(signature)
            .unwrap_or_else(|| panic!("`{signature}` is not in src/daemon/commands.rs"));
        let rest = &source[from..];
        let end = rest
            .find("\n    }\n")
            .unwrap_or_else(|| panic!("`{signature}` has no column-4 closing brace"));
        rest[..end]
            .lines()
            .filter(|line| !line.trim_start().starts_with("//"))
            .map(strip_trailing_comment)
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// `line` up to a `//` that is not inside a string literal — the companion strip
    /// [`method_body`] applies, mirroring `crate::capture`'s own.
    ///
    /// "Not inside a string literal" is decided by an even count of unescaped `"` to the left,
    /// which is exact for a single line of Rust in this file's `cargo fmt` layout (no raw string
    /// spanning a `//`, no char literal holding a lone quote).
    fn strip_trailing_comment(line: &str) -> &str {
        let bytes = line.as_bytes();
        let mut in_string = false;
        let mut i = 0usize;
        while i < bytes.len() {
            match bytes[i] {
                b'\\' => i += 1,
                b'"' => in_string = !in_string,
                b'/' if !in_string && bytes.get(i + 1) == Some(&b'/') => return &line[..i],
                _ => {}
            }
            i += 1;
        }
        line
    }

    #[tokio::test]
    async fn run_loop_adopts_a_roster_reload_signal_through_the_idle_select() {
        // Issue #139: the run loop's idle select must route a `RosterReloadRequested`
        // control signal into `adopt_roster_reload` — proving the whole daemon-side
        // chain (signal → idle break → disk re-read → reconcile) end-to-end, the one
        // wiring `NoControl`-based tests leave undriven. A regression turning the
        // `Some(RosterReloadRequested) => break` arm into a `continue` would leave the
        // in-memory roster at its startup two accounts and fail this test.
        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("config.toml");
        write_roster_config(
            &config_path,
            &[("u-A", "work"), ("u-B", "spare"), ("u-C", "third")],
        );

        let roster = vec![account("u-A", "work"), account("u-B", "spare")];
        let store = store_holding(b"A-token").await;
        let stash = stash_with(&[
            ("Sessiometer/u-A", b"A-token", "u-A"),
            ("Sessiometer/u-B", b"B-token", "u-B"),
        ])
        .await;
        let (_json_dir, json) = claude_json("u-A");
        // Holds-only readings so no swap perturbs the idle path.
        let poller = FakeRosterPoller::new()
            .ok("u-A", 0.10, 0.10)
            .ok("u-B", 0.10, 0.10);
        let tun = tunables(95, 80, 100);
        let mut daemon: FakeDaemon = Daemon::new(
            roster,
            poller,
            store,
            stash,
            FakeClock::new(Duration::from_secs(60)),
            json,
            &tun,
        )
        .with_config_path(config_path);
        assert_eq!(
            roster_uuids(&daemon),
            vec!["u-A", "u-B"],
            "startup roster is the two captured accounts"
        );

        let logdir = tempfile::tempdir().unwrap();
        let log_path = logdir.path().join("sessiometer.log");
        let mut log = EventLog::at(&log_path).unwrap();
        // Tick 1 → idle delivers `RosterReloadRequested` (reload) → tick 2 → shutdown.
        // after(3): 1 start-up check (pends) + 2 idle shutdown-checks.
        let mut shutdown = FakeShutdown::after(3);
        let control = OnceRosterReload::new(ReloadIntent::AppendOnly);

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

        // The signal reached `adopt_roster_reload` through the idle select: the daemon
        // re-read `config.toml` and the third account is now in the LIVE rotation — no
        // restart.
        assert_eq!(
            roster_uuids(&daemon),
            vec!["u-A", "u-B", "u-C"],
            "the onboarded account joined the live rotation without a restart"
        );

        // Issue #1438: and the run loop LOGGED it. The handler returning the event by value only
        // guarantees one exists — this is the other half, that the idle arm actually emits it, so
        // a regression dropping the `emit_best_effort` fails here rather than leaving the reload
        // traceless again. Asserted on the appended line, which is what an operator would read.
        let logged = std::fs::read_to_string(&log_path).unwrap();
        let reload_lines: Vec<&str> = logged
            .lines()
            .filter(|line| line.contains(" event=roster_reload "))
            .collect();
        assert_eq!(
            reload_lines.len(),
            1,
            "exactly one reload line for the one reload signal: {logged}"
        );
        // Token-wise, not `ends_with` over the tail: the grammar is additively extensible by
        // design (`Event::Swap`'s `late=true` is the documented precedent), and this event is
        // chartered to grow, so pinning the tail would fail on a change that broke no reader.
        let logged_line = reload_lines[0];
        assert_eq!(
            field_of(logged_line, "outcome"),
            Some("adopted"),
            "the logged reload adopted: {logged_line}"
        );
        assert_eq!(
            field_of(logged_line, "previous"),
            Some("2"),
            "the `was` half of the pair reached the log: {logged_line}"
        );
        assert_eq!(
            field_of(logged_line, "incoming"),
            Some("3"),
            "the widening is legible from the logged line: {logged_line}"
        );
    }

    #[tokio::test]
    async fn run_loop_refuses_a_shrinking_reload_through_the_idle_select() {
        // The refusal's own end-to-end path, and the counterpart of the adoption above. Everything
        // between the control signal and the log line is live: the idle select carries the intent
        // out of `ControlSignal::RosterReloadRequested`, the post-idle arm hands it to
        // `adopt_roster_reload`, the floor declines, and `emit_best_effort` writes the line.
        //
        // Worth its own run because the intent now travels through wiring that previously carried
        // no payload at all. An idle arm that dropped it — or a post-idle arm that passed a
        // constant instead of the signal's own value — would leave the handler tests above green
        // while the daemon adopted every shrink a `login` triggered, which is precisely the
        // 2026-08-27 failure with the guard nominally present.
        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("config.toml");
        // Two live accounts, a file presenting one: the incident's shape, minimised.
        write_roster_config(&config_path, &[("u-A", "work")]);

        let roster = vec![account("u-A", "work"), account("u-B", "spare")];
        let store = store_holding(b"A-token").await;
        let stash = stash_with(&[
            ("Sessiometer/u-A", b"A-token", "u-A"),
            ("Sessiometer/u-B", b"B-token", "u-B"),
        ])
        .await;
        let (_json_dir, json) = claude_json("u-A");
        let poller = FakeRosterPoller::new()
            .ok("u-A", 0.10, 0.10)
            .ok("u-B", 0.10, 0.10);
        let tun = tunables(95, 80, 100);
        let mut daemon: FakeDaemon = Daemon::new(
            roster,
            poller,
            store,
            stash,
            FakeClock::new(Duration::from_secs(60)),
            json,
            &tun,
        )
        .with_config_path(config_path);

        let logdir = tempfile::tempdir().unwrap();
        let log_path = logdir.path().join("sessiometer.log");
        let mut log = EventLog::at(&log_path).unwrap();
        let mut shutdown = FakeShutdown::after(3);
        // The intent is what makes this a refusal rather than the adoption above.
        let control = OnceRosterReload::new(ReloadIntent::AppendOnly);

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
            roster_uuids(&daemon),
            vec!["u-A", "u-B"],
            "the live rotation kept both accounts — the shrinking reload was refused"
        );

        let logged = std::fs::read_to_string(&log_path).unwrap();
        let reload_lines: Vec<&str> = logged
            .lines()
            .filter(|line| line.contains(" event=roster_reload "))
            .collect();
        // TWO lines, and the pair is the point. The refusal is one — silence is what #1438
        // removed. The poll tick that follows it then observes the state the refusal deliberately
        // LEFT — memory holding more accounts than the file — and reports it as a divergence
        // (issue #1443, merged after this test was written). Neither subsumes the other: the
        // refusal says the daemon declined a reload, the divergence says disk and memory disagree,
        // and an operator reading the log needs both to understand why the rotation is what it is.
        assert_eq!(
            reload_lines.len(),
            2,
            "expected the refusal and the divergence the refusal leaves behind: {logged}"
        );

        let refusal = reload_lines
            .iter()
            .find(|line| field_of(line, "outcome") == Some("refused"))
            .unwrap_or_else(|| panic!("no refusal line: {logged}"));
        assert_eq!(field_of(refusal, "reason"), Some("shrink"), "{refusal}");
        assert_eq!(field_of(refusal, "previous"), Some("2"), "{refusal}");
        assert_eq!(field_of(refusal, "incoming"), Some("1"), "{refusal}");

        let divergence = reload_lines
            .iter()
            .find(|line| field_of(line, "outcome") == Some("diverged"))
            .unwrap_or_else(|| panic!("no divergence line: {logged}"));
        assert_eq!(
            field_of(divergence, "reason"),
            Some("count_mismatch"),
            "{divergence}"
        );
        assert_eq!(field_of(divergence, "previous"), Some("2"), "{divergence}");
        assert_eq!(field_of(divergence, "incoming"), Some("1"), "{divergence}");
    }

    #[tokio::test]
    async fn run_loop_restored_control_command_un_quarantines_without_activating() {
        // Issue #275: the run loop's idle select must route a `Restored(uuid)` control signal
        // into `reconcile_restored` — un-quarantining the named PARKED account and logging its
        // edge-triggered `credential_restored` — WITHOUT a canonical write or an active-account
        // change. This is the on-demand un-quarantine path, decoupled from the #106 sweep (which is
        // starved, #260). The control-driven analog of `run_loop_emits_refresh_events_and_applies_restores`:
        // a regression turning the `Some(Restored) => break` arm into a `continue`, or dropping the
        // post-idle reconcile, would leave `spare` quarantined and fail here.
        //
        // Which of `reconcile_restored`'s two forks (issue #643) this exercises is pinned by the
        // setup below, on two independent grounds: `Daemon::new` leaves `poll_refresh` unwired, and
        // the account's `last_refresh_outcome` is never set to `Dead`. Either alone routes it to the
        // plain `apply_refresh_restore`, so this test says nothing about the `Dead` re-probe fork —
        // `run_loop_restored_reprobes_a_dead_parked_account_through_the_idle_select` in
        // `refresh_fold.rs` is what pins that one.
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
            .ok("u-B", 0.10, 0.10); // holds-only — no swap perturbs the idle path
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
        // `spare` (PARKED, non-active) is quarantined ("needs re-login"); `work` is active. The
        // warm-up tick polls only the active `work`, so this flag survives untouched into the idle
        // where the control signal delivers the on-demand restore.
        daemon.state.accounts[1].health.quarantined = true;

        let logdir = tempfile::tempdir().unwrap();
        let log_path = logdir.path().join("sessiometer.log");
        let mut log = EventLog::at(&log_path).unwrap();
        // Tick 1 → idle delivers `Restored(u-B)` → tick 2 → shutdown. after(3): 1 start-up check
        // (pends) + 2 idle shutdown-checks — the same cadence as the roster-reload adoption test.
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

        // The signal reached `apply_refresh_restore` through the idle select's `reconcile_restored`
        // dispatch, on its plain-un-quarantine fork: `spare` is un-quarantined in memory and its
        // edge-triggered `credential_restored` rode the event log exactly once — no sweep involved.
        assert!(
            !daemon.state.accounts[1].health.quarantined,
            "the restored account is un-quarantined"
        );
        let logged = std::fs::read_to_string(&log_path).unwrap();
        assert_eq!(
            logged
                .matches("event=credential_restored account=spare")
                .count(),
            1,
            "exactly one credential_restored logged: {logged:?}"
        );
        // The active account is UNCHANGED — `work` (index 0), never the restored `spare` (index 1).
        // The on-demand restore never re-points canonical or swaps active (#275).
        assert_eq!(
            daemon.state.active,
            Some(0),
            "work stays active; restoring the parked spare never activates it"
        );
    }

    #[tokio::test]
    async fn run_loop_shutdown_control_command_exits_the_loop_gracefully() {
        // Issue #397 (AC): the run loop's idle select must route an authenticated `shutdown`
        // control signal — the `daemon stop` path for an UNMANAGED daemon — into the SAME graceful
        // `Idle::Shutdown` exit a SIGINT / SIGTERM drives. `OnceShutdown` fires `ShutdownRequested`
        // on the first idle, and `FakeShutdown::after(100)` guarantees the SIGINT/SIGTERM seam never
        // fires here — so the ONLY thing that can end this loop is the control signal. A regression
        // turning that arm into a `continue` (or dropping it) would spin the idle forever rather
        // than pass.
        //
        // The AC's "an in-flight swap completes before exit" half is a property of the SHARED
        // `Idle::Shutdown` exit, not of the trigger: a swap always runs to completion inside `tick`
        // (shutdown is observed only BETWEEN ticks), as `run_loop_completes_a_swap_before_a_
        // concurrent_shutdown` proves for the signal path. The socket verb funnels into that
        // identical exit, so it inherits the no-half-swap guarantee by construction.
        let roster = vec![account("u-A", "work"), account("u-B", "spare")];
        let store = store_holding(b"A-token").await;
        let stash = stash_with(&[
            ("Sessiometer/u-A", b"A-token", "u-A"),
            ("Sessiometer/u-B", b"B-token", "u-B"),
        ])
        .await;
        let (_dir, json) = claude_json("u-A");
        // Holds-only readings — no swap perturbs the idle path; the shutdown signal drives the exit.
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

        let logdir = tempfile::tempdir().unwrap();
        let mut log = EventLog::at(&logdir.path().join("sessiometer.log")).unwrap();
        // after(100): the SIGINT/SIGTERM seam never resolves within this test, so a passing run
        // proves the CONTROL signal — not a signal — ended the loop.
        let mut shutdown = FakeShutdown::after(100);
        let control = OnceShutdown::new();

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
        .expect("an authenticated shutdown control command exits the loop cleanly (Ok)");

        // Exactly one warm-up tick ran, then the first idle delivered `ShutdownRequested` and broke
        // to `Idle::Shutdown` — the graceful exit, with the tick already complete.
        assert_eq!(
            daemon.state.ticks, 1,
            "one tick, then the control-driven graceful exit",
        );
    }

    #[tokio::test]
    async fn run_loop_external_login_watch_restashes_off_the_poll_cadence() {
        // Issue #140: the external-login watch's `until_due` resolves inside the idle select (off
        // the usage-poll cadence) and, on reading a canonical that differs from the daemon's
        // last-committed baseline, the run loop breaks the idle to re-tick — so the very next
        // tick's `reconcile_canonical_change` re-stashes the account. The watch and the daemon
        // share ONE canonical store (as in production, one keychain item): the watch simulates a
        // manual `claude /login` by rewriting it mid-idle, and the re-tick it triggers re-stashes
        // A with the fresh token. The run-loop analog of the direct-tick #13 re-stash test —
        // proving the pickup happens WITHOUT waiting a full poll interval.
        let store = Rc::new(FakeCredentialStore::empty());
        store.write(&cred(b"A-token")).await.unwrap();
        let roster = vec![account("u-A", "work"), account("u-B", "spare")];
        let stash = stash_with(&[
            ("Sessiometer/u-A", b"A-token", "u-A"),
            ("Sessiometer/u-B", b"B-token", "u-B"),
        ])
        .await;
        let (_dir, json) = claude_json("u-A");
        let poller = FakeRosterPoller::new()
            .ok("u-A", 0.10, 0.10)
            .ok("u-B", 0.10, 0.10); // holds-only — no swap perturbs the idle path
        let tun = tunables(95, 80, 0);
        let mut daemon = Daemon::new(
            roster,
            poller,
            store.clone(),
            stash,
            FakeClock::frozen(),
            json,
            &tun,
        );

        let logdir = tempfile::tempdir().unwrap();
        let mut log = EventLog::at(&logdir.path().join("sessiometer.log")).unwrap();
        // after(3): #1 start-up check (pends), #2 idle-iter-1 (watch fires → detect → re-tick),
        // #3 idle-iter-2 → shutdown. Exactly ONE watch-driven re-tick, then a clean stop.
        let mut shutdown = FakeShutdown::after(3);
        let control = NoControl;
        let mut login_watch = OnceExternalLogin::new(store.clone(), b"A-reauthed");

        let mut diag = DiagnosticLog::new(std::io::sink(), Verbosity::Quiet);
        run_loop(
            &mut daemon,
            &mut log,
            &mut diag,
            &mut shutdown,
            &control,
            &mut NoopRefreshTicker,
            &mut login_watch,
        )
        .await
        .unwrap();

        // The watch broke the idle and the re-tick re-stashed A with the fresh token — the
        // out-of-band login was picked up off the poll cadence.
        let a = daemon.stash.read("Sessiometer/u-A").await.unwrap();
        assert_eq!(
            a.credential.expose(),
            b"A-reauthed",
            "the re-tick the watch triggered re-stashed A with the freshly-logged-in token"
        );
        assert_eq!(
            a.oauth_account.account_uuid(),
            "u-A",
            "the identity half is preserved through the re-stash"
        );
    }

    #[tokio::test]
    async fn run_loop_external_login_watch_ignores_an_unchanged_canonical() {
        // Issue #140 (healthy no-change path unchanged): the watch fires and reads a canonical
        // BYTE-IDENTICAL to the daemon's baseline — no out-of-band login — so the run loop does
        // NOT break the idle and no re-stash happens. The probe was reached but correctly did
        // nothing.
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

        let logdir = tempfile::tempdir().unwrap();
        let logpath = logdir.path().join("sessiometer.log");
        let mut log = EventLog::at(&logpath).unwrap();
        let mut shutdown = FakeShutdown::after(3);
        let control = NoControl;
        // The watch reports the SAME token the daemon primed on — no change.
        let mut login_watch = ScriptedExternalLogin::returning(Some(cred(b"A-token")));

        let mut diag = DiagnosticLog::new(std::io::sink(), Verbosity::Quiet);
        run_loop(
            &mut daemon,
            &mut log,
            &mut diag,
            &mut shutdown,
            &control,
            &mut NoopRefreshTicker,
            &mut login_watch,
        )
        .await
        .unwrap();

        // The probe ran but the unchanged read produced no re-stash — A's stash is untouched and
        // no restash line was logged.
        assert!(
            login_watch.probed.get(),
            "the watch's read_canonical was reached in the idle"
        );
        let a = daemon.stash.read("Sessiometer/u-A").await.unwrap();
        assert_eq!(
            a.credential.expose(),
            b"A-token",
            "an unchanged canonical triggers no re-stash"
        );
        let logged = std::fs::read_to_string(&logpath).unwrap_or_default();
        assert!(
            !logged.contains("event=restash"),
            "no restash on the no-change path: {logged:?}"
        );
    }

    #[tokio::test]
    async fn run_loop_external_login_watch_tolerates_an_unreadable_probe() {
        // Issue #140 fail-safe (a detection error must not break the loop): the watch fires but
        // its canonical read fails (locked / absent → `None`), so the run loop detects nothing,
        // does NOT break, and idles on normally to a clean shutdown — no crash, no stall, no
        // spurious re-stash. Same fail-open discipline as #156's collector and #162's refresh.
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

        let logdir = tempfile::tempdir().unwrap();
        let mut log = EventLog::at(&logdir.path().join("sessiometer.log")).unwrap();
        let mut shutdown = FakeShutdown::after(3);
        let control = NoControl;
        // The probe cannot read the canonical this cycle (a locked / absent item → None).
        let mut login_watch = ScriptedExternalLogin::returning(None);

        let mut diag = DiagnosticLog::new(std::io::sink(), Verbosity::Quiet);
        run_loop(
            &mut daemon,
            &mut log,
            &mut diag,
            &mut shutdown,
            &control,
            &mut NoopRefreshTicker,
            &mut login_watch,
        )
        .await
        .unwrap();

        // The loop exited cleanly, the probe was reached, and the failed read triggered no
        // re-stash — a detection error neither broke nor perturbed the poll/swap loop.
        assert!(
            login_watch.probed.get(),
            "the watch's read_canonical was reached in the idle"
        );
        let a = daemon.stash.read("Sessiometer/u-A").await.unwrap();
        assert_eq!(
            a.credential.expose(),
            b"A-token",
            "a failed probe triggers no re-stash"
        );
    }

    #[tokio::test]
    async fn external_login_watcher_reads_the_canonical_and_fails_open() {
        // Issue #140: the PRODUCTION watcher's `read_canonical` returns the current canonical
        // over its own store, and FAILS OPEN to `None` on a locked or absent read — so a probe
        // that cannot read simply detects nothing, never an error that could break the loop.
        let readable = FakeCredentialStore::empty();
        readable.write(&cred(b"A-token")).await.unwrap();
        let mut w = ExternalLoginWatcher::new(readable);
        assert_eq!(
            w.read_canonical().await.unwrap().expose(),
            b"A-token",
            "a readable canonical is returned"
        );

        // A LOCKED keychain read is swallowed to `None`, not surfaced as an error.
        let locked = FakeCredentialStore::empty();
        locked.set_locked(true);
        let mut w = ExternalLoginWatcher::new(locked);
        assert!(
            w.read_canonical().await.is_none(),
            "a locked read fails open to None"
        );

        // An ABSENT canonical (no item yet) likewise fails open to `None` — never an error.
        let mut w = ExternalLoginWatcher::new(FakeCredentialStore::empty());
        assert!(
            w.read_canonical().await.is_none(),
            "an absent canonical fails open to None"
        );
    }

    #[tokio::test]
    async fn run_loop_runs_a_refresh_sweep_in_the_idle_path() {
        // Issue #105: an ENABLED ticker's `until_due` resolves inside the idle select, and the
        // run loop then runs its `sweep` — handing it the daemon's live exclusion set (with the
        // active account among the uuids to skip, the "parked only" contract). This is the one
        // run-loop test that drives the live `until_due → sweep` wiring; every other passes the
        // inert `NoopRefreshTicker`, whose `until_due` never resolves.
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
            .ok("u-B", 0.10, 0.10); // holds-only — no swap perturbs the idle path
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

        let logdir = tempfile::tempdir().unwrap();
        let mut log = EventLog::at(&logdir.path().join("sessiometer.log")).unwrap();
        // after(4): #1 start-up check, #2 idle-iter-1 outer (refresh wins → sweep), #3 the
        // sweep's NESTED shutdown arm (pends, so the sweep runs), #4 idle-iter-2 outer →
        // shutdown. So the sweep fires once, then the loop stops cleanly.
        let mut shutdown = FakeShutdown::after(4);
        let control = NoControl;
        let mut ticker = OnceRefreshTicker::new();

        let mut diag = DiagnosticLog::new(std::io::sink(), Verbosity::Quiet);
        run_loop(
            &mut daemon,
            &mut log,
            &mut diag,
            &mut shutdown,
            &control,
            &mut ticker,
            &mut NoopExternalLoginWatch,
        )
        .await
        .unwrap();

        // The sweep ran exactly once, and the daemon handed it the active account (u-A) to
        // skip — the refresh tick reached the idle path with the right exclusions.
        let swept = ticker.swept.borrow();
        assert_eq!(swept.len(), 1, "exactly one sweep ran: {swept:?}");
        assert!(
            swept[0].contains(&"u-A".to_owned()),
            "the active account is excluded from the sweep: {:?}",
            swept[0]
        );
        // No account is quarantined here, so the tick is NEVER handed a recovery prompt — the
        // #280 signal is false whenever there is no restore work (contrast to the quarantined case).
        assert!(
            ticker.due_recovery.borrow().iter().all(|&r| !r),
            "no quarantine means no recovery prompt: {:?}",
            ticker.due_recovery.borrow(),
        );
    }

    #[tokio::test]
    async fn run_loop_lets_shutdown_interrupt_an_in_flight_refresh_sweep() {
        // Issue #105: the refresh arm runs its sweep under a NESTED select whose only other arm
        // is shutdown — so a SIGINT/SIGTERM cuts an in-flight (here deliberately wedged) sweep
        // and the loop returns, rather than deadlocking on a stuck refresh cycle. A control read
        // is NOT in that nested select, so it cannot interrupt a sweep (no token forfeit, no
        // starvation). A regression that awaited `sweep` directly — dropping the nested shutdown
        // arm — would hang here; the `timeout` turns that hang into a clean failure.
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

        let logdir = tempfile::tempdir().unwrap();
        let mut log = EventLog::at(&logdir.path().join("sessiometer.log")).unwrap();
        // after(3): #1 start-up check, #2 idle-iter-1 outer (refresh wins → enter nested), #3
        // the sweep's NESTED shutdown arm fires → break. The wedged sweep is cut by shutdown.
        let mut shutdown = FakeShutdown::after(3);
        let control = NoControl;
        let mut ticker = HangingRefreshTicker::new();

        let mut diag = DiagnosticLog::new(std::io::sink(), Verbosity::Quiet);
        // Reaching the assertion at all is the proof; the timeout guards against the regression
        // (a directly-awaited sweep) deadlocking the suite instead of failing cleanly.
        let result = tokio::time::timeout(
            Duration::from_secs(5),
            run_loop(
                &mut daemon,
                &mut log,
                &mut diag,
                &mut shutdown,
                &control,
                &mut ticker,
                &mut NoopExternalLoginWatch,
            ),
        )
        .await;
        assert!(
            result.is_ok(),
            "shutdown must interrupt the wedged sweep, not deadlock"
        );
        result.unwrap().unwrap();
    }

    #[tokio::test]
    async fn run_loop_emits_refresh_events_and_applies_restores() {
        // Issue #106: the run loop drains a sweep's `SweepOutcome` — it EMITS each per-cycle
        // refresh event to the event log, and APPLIES each reported restore (un-quarantining the
        // recovered account + logging its edge-triggered `credential_restored`). A quarantined
        // PARKED account (`spare`) is never re-polled by the swap path (#42 revival can't fire)
        // and not re-logged-in (#107 can't fire) — the exact gap #106 closes: it would stay stuck
        // forever even though its refresh token still works. Here the sweep reports it recovered
        // and the loop flips it back to eligible.
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
            .ok("u-B", 0.10, 0.10); // holds-only — no swap perturbs the idle path
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
        // `spare` is quarantined ("needs re-login", #42) but its refresh token still works — the
        // parked-and-stuck account #106 rescues. The single warm-up tick polls only the active
        // `work`, so this flag survives untouched into the idle sweep.
        daemon.state.accounts[1].health.quarantined = true;

        let logdir = tempfile::tempdir().unwrap();
        let log_path = logdir.path().join("sessiometer.log");
        let mut log = EventLog::at(&log_path).unwrap();
        // after(4): one sweep fires in idle-iter-1, then idle-iter-2 sees shutdown — the same
        // cadence as `run_loop_runs_a_refresh_sweep_in_the_idle_path`.
        let mut shutdown = FakeShutdown::after(4);
        let control = NoControl;
        // The sweep reports `spare` refreshed: one refresh event to log + one restore to apply.
        let mut ticker = OnceRefreshTicker::returning(SweepOutcome {
            events: vec![Event::Refresh {
                account: "spare".to_owned(),
                outcome: RefreshEventOutcome::Refreshed { rotated: true },
                expires_before: Some(1_000_000),
                expires_after: Some(1_003_600),
                reason: None,
                backoff_secs: None,
            }],
            restored: vec!["u-B".to_owned()],
            observations: Vec::new(),
        });

        let mut diag = DiagnosticLog::new(std::io::sink(), Verbosity::Quiet);
        run_loop(
            &mut daemon,
            &mut log,
            &mut diag,
            &mut shutdown,
            &control,
            &mut ticker,
            &mut NoopExternalLoginWatch,
        )
        .await
        .unwrap();

        // The daemon handed the sweep its quarantined set — the RESTORE candidates — so the tick
        // could attempt them even though they sit far from near-expiry.
        let swept_q = ticker.swept_quarantined.borrow();
        assert_eq!(swept_q.len(), 1, "exactly one sweep ran: {swept_q:?}");
        assert!(
            swept_q[0].contains(&"u-B".to_owned()),
            "the quarantined parked account is offered to the sweep: {:?}",
            swept_q[0]
        );

        // The per-cycle refresh event rode the event log, and the reported restore both
        // un-quarantined `spare` in memory AND logged its edge-triggered `credential_restored`.
        let logged = std::fs::read_to_string(&log_path).unwrap();
        assert!(
            logged.contains("event=refresh account=spare outcome=refreshed"),
            "the refresh event reached the log: {logged:?}"
        );
        assert!(
            logged.contains("event=credential_restored account=spare"),
            "the restore logged its credential_restored: {logged:?}"
        );
        assert!(
            !daemon.state.accounts[1].health.quarantined,
            "the restored account is un-quarantined"
        );
    }

    #[tokio::test]
    async fn run_loop_prompts_the_tick_when_a_quarantined_parked_account_is_present() {
        // Issue #280: the run loop threads the "≥1 quarantined-PARKED account" signal into the
        // tick's DUE computation (`until_due`), not only into `sweep`. With `spare` quarantined and
        // parked, the FIRST idle wait is handed `has_recovery_work = true` — so the restore is
        // prompt (the idle floor) instead of deferred a full refresh cadence. After that period's
        // sweep the prompt is DISARMED (`recovery_prompted`), so every later wait this period sees
        // `false` — the coupling that keeps a still-quarantined account off the sub-poll retry
        // storm ADR-0007 rejected.
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
            .ok("u-B", 0.10, 0.10); // holds-only — no swap perturbs the idle path
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
        // `spare` is quarantined AND parked (u-A is active) — the exact "recovery work" the prompt
        // targets. It survives the warm-up tick, which polls only the active `work`.
        daemon.state.accounts[1].health.quarantined = true;

        let logdir = tempfile::tempdir().unwrap();
        let mut log = EventLog::at(&logdir.path().join("sessiometer.log")).unwrap();
        // after(4): one sweep fires in idle-iter-1, then idle-iter-2 sees shutdown — the same
        // cadence as the sibling refresh run-loop tests.
        let mut shutdown = FakeShutdown::after(4);
        let control = NoControl;
        let mut ticker = OnceRefreshTicker::new();

        let mut diag = DiagnosticLog::new(std::io::sink(), Verbosity::Quiet);
        run_loop(
            &mut daemon,
            &mut log,
            &mut diag,
            &mut shutdown,
            &control,
            &mut ticker,
            &mut NoopExternalLoginWatch,
        )
        .await
        .unwrap();

        // The tick was asked to become due WITH recovery work on the first wait — the prompt
        // reached `until_due`, not just `sweep`.
        let due_recovery = ticker.due_recovery.borrow();
        assert_eq!(
            due_recovery.first(),
            Some(&true),
            "the first idle wait must see the quarantined-parked recovery prompt: {due_recovery:?}",
        );
        // …and every wait AFTER the period's sweep is disarmed (once per period — no sub-poll storm).
        assert!(
            due_recovery.iter().skip(1).all(|&r| !r),
            "the recovery prompt is disarmed after the sweep: {due_recovery:?}",
        );
    }

    #[tokio::test]
    async fn run_loop_completes_a_swap_before_a_concurrent_shutdown() {
        // The warm-up cycle (issue #80) polls A then B across two staggered ticks;
        // the swap fires on the warm-up-completing second tick. Shutdown is then
        // requested. Because a swap runs to completion inside `tick` (shutdown is only
        // observed between ticks), the post-loop state is coherent — no half-swap.
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
            json.clone(),
            &tun,
        );

        let logdir = tempfile::tempdir().unwrap();
        let log_path = logdir.path().join("sessiometer.log");
        let mut log = EventLog::at(&log_path).unwrap();
        // after(3): 1 start-up check (#76 de-burst) + 2 idle shutdown-checks — run
        // both warm-up ticks (poll A, then poll B + swap), then stop.
        let mut shutdown = FakeShutdown::after(3);
        let control = NoControl;

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

        // The warm-up-completing tick's swap landed fully: canonical = B, display = B,
        // active = B.
        assert_eq!(daemon.state.ticks, 2);
        assert!(daemon
            .store
            .read()
            .await
            .unwrap()
            .matches(&cred(b"B-token")));
        assert_eq!(displayed_uuid(&json).as_deref(), Some("u-B"));
        assert_eq!(daemon.state.active, Some(1));

        // End-to-end (issue #9): the swap wrote one structured swap line — handles only
        // (work → spare), never a token or email. The session reading (0.97) is at/over the
        // 95 % trigger, so the line is tagged `reason=session` with the outgoing account's
        // `session_pct`. Since #137, `spare` also logs one honest Unknown→healthy transition
        // as the swap makes it active and its first poll verifies it — an expected companion
        // line, not spurious output (and itself #15-clean: a handle + a bare state token).
        let logged = std::fs::read_to_string(&log_path).unwrap();
        assert_eq!(
            logged.lines().count(),
            2,
            "swap + spare health line: {logged:?}"
        );
        assert!(
            logged.contains("event=swap from=work to=spare reason=session session_pct=97"),
            "got: {logged:?}"
        );
        assert!(
            logged.contains("event=credential_health account=spare state=healthy"),
            "spare verified healthy once polled after the swap (#137): {logged:?}"
        );
        assert!(logged.starts_with("ts="), "stamped: {logged:?}");
        assert!(
            crate::redaction::meter::unauthored_emails(&logged, &[]).is_empty(),
            "no non-authored email (#15/#444): {logged:?}"
        );
    }

    #[tokio::test]
    async fn note_poll_outcome_walks_the_401_streak_and_emits_one_event_per_named_condition() {
        // The daemon-side poll-outcome → event mapping and the per-account 401
        // streak (issue #9) are exercised directly: `note_poll_outcome` turns each
        // poll `Result` into at most one event and maintains the streak. Driving it
        // by hand (rather than through the loop) lets us assert the reset, which a
        // static poller cannot script on a single account across ticks.
        let roster = vec![account("u-A", "work"), account("u-B", "spare")];
        let store = store_holding(b"A-token").await;
        let stash = stash_with(&[
            ("Sessiometer/u-A", b"A-token", "u-A"),
            ("Sessiometer/u-B", b"B-token", "u-B"),
        ])
        .await;
        let (_dir, json) = claude_json("u-A");
        let tun = tunables(95, 80, 0);
        let mut daemon: FakeDaemon = Daemon::new(
            roster,
            FakeRosterPoller::new(),
            store,
            stash,
            FakeClock::frozen(),
            json,
            &tun,
        );

        let mut events = Vec::new();
        // Issue #42: the per-account 401 streak now lives in `accounts[i].health.consec_401`.
        let streak_of = |d: &FakeDaemon| {
            d.state
                .accounts
                .iter()
                .map(|a| a.health.consec_401)
                .collect::<Vec<_>>()
        };

        // A 401 on account 0 starts its streak at 1; a second consecutive 401
        // climbs to 2 — one `monitor_401` per occurrence, account 1 untouched.
        daemon.note_poll_outcome(0, &Err(Error::UsageUnauthorized), &mut events);
        daemon.note_poll_outcome(0, &Err(Error::UsageUnauthorized), &mut events);
        assert_eq!(streak_of(&daemon), vec![2, 0]);
        assert_eq!(
            events,
            vec![
                Event::Monitor401 {
                    account: "work".to_owned(),
                    consecutive: 1,
                },
                Event::Monitor401 {
                    account: "work".to_owned(),
                    consecutive: 2,
                },
            ]
        );

        // A success resets account 0's streak and emits nothing.
        events.clear();
        daemon.note_poll_outcome(
            0,
            &Ok(Usage {
                session: 0.10,
                weekly: 0.10,
                weekly_resets_at: None,
                session_resets_at: None,
            }),
            &mut events,
        );
        assert_eq!(streak_of(&daemon), vec![0, 0]);
        assert!(events.is_empty());

        // After the reset the next 401 restarts the streak at 1 (not 3).
        daemon.note_poll_outcome(0, &Err(Error::UsageUnauthorized), &mut events);
        assert_eq!(streak_of(&daemon), vec![1, 0]);
        assert_eq!(
            events,
            vec![Event::Monitor401 {
                account: "work".to_owned(),
                consecutive: 1,
            }]
        );

        // A locked keychain is detected at top-of-tick now, not per-account (issue
        // #13), so this arm emits NOTHING — it only resets the streak, like any
        // other non-401 outcome. Account 0's streak (1) is left untouched.
        events.clear();
        daemon.note_poll_outcome(1, &Err(Error::KeychainLocked { op: "read" }), &mut events);
        assert_eq!(streak_of(&daemon), vec![1, 0]);
        assert!(events.is_empty());

        // A 403 (missing usage scope) on account 0 emits `usage_scope_fail` and
        // resets its streak — every non-401 outcome clears the streak.
        events.clear();
        daemon.note_poll_outcome(0, &Err(Error::UsageScopeMissing), &mut events);
        assert_eq!(streak_of(&daemon), vec![0, 0]);
        assert_eq!(
            events,
            vec![Event::UsageScopeFail {
                account: "work".to_owned(),
            }]
        );

        // A transient error is silent and also resets (no event, streak cleared).
        events.clear();
        daemon.note_poll_outcome(
            0,
            &Err(Error::UsageTransient {
                status: 0,
                retry_after: None,
            }),
            &mut events,
        );
        assert_eq!(streak_of(&daemon), vec![0, 0]);
        assert!(events.is_empty());
    }

    #[tokio::test]
    async fn run_loop_logs_one_line_per_poll_rejection_each_tick() {
        // Issue #9 acceptance (as amended by #13, #80): each PER-ACCOUNT poll
        // rejection — a 401 and a 403 (missing usage scope) — emits EXACTLY one
        // structured line per occurrence. A per-account keychain-lock is now SILENT
        // here: the lock is process-global and signaled once at top-of-tick (#13),
        // not per poll. The staggered loop (#80) polls ONE account per tick, the active
        // interleaved before each peer (#366 → A, B, A, C), so a full sweep of the
        // 3-account roster takes four ticks; those four ticks poll A twice (ticks 1 and
        // 3) — proving the per-account 401 streak climbs 1 → 2 across its own re-polls —
        // with B's (silent) lock on tick 2 and C's 403 on tick 4, demonstrating
        // `note_poll_outcome` is wired into the loop and serialized.
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
            .unauthorized("u-A")
            .keychain_locked("u-B")
            .scope_missing("u-C");
        let tun = tunables(95, 80, 0);
        let mut daemon: FakeDaemon = Daemon::new(
            roster,
            poller,
            store,
            stash,
            FakeClock::new(Duration::from_secs(60)),
            json,
            &tun,
        );

        let logdir = tempfile::tempdir().unwrap();
        let log_path = logdir.path().join("sessiometer.log");
        let mut log = EventLog::at(&log_path).unwrap();
        // after(5): 4 idle shutdown-checks + 1 start-up check (#76 de-burst) — four
        // staggered ticks; the #366 active-interleave makes them (A, B, A, C).
        let mut shutdown = FakeShutdown::after(5);
        let control = NoControl;

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

        assert_eq!(daemon.state.ticks, 4);

        // Across the four staggered ticks (#80, interleaved #366 → A, B, A, C), A 401s
        // twice (ticks 1, 3) and C 403s once (tick 4) → three per-poll event lines, each
        // stamped, none carrying secret material (handles only — never a token or email).
        // The locked account B contributes nothing per-account (#13).
        //
        // Plus one `observation_gap_enter` for A (issue #1453), which is not a per-poll line
        // at all: A is the ACTIVE account and every poll of it 401s, so it has never been
        // successfully observed. `blind_enter` cannot speak for it — its entry edge needs a
        // prior reading to anchor on — so without this line an active account the daemon has
        // never once seen would be reported only as a credential fault, never as an
        // unobserved one. It fires on tick 2, the first tick a full bound has elapsed since A
        // was designated.
        let logged = std::fs::read_to_string(&log_path).unwrap();
        assert_eq!(logged.lines().count(), 4, "four lines: {logged:?}");
        assert_eq!(
            logged
                .lines()
                .filter(|l| l.contains("event=observation_gap_enter"))
                .count(),
            1,
            "one gap line, on the edge — not one per tick: {logged:?}"
        );
        assert!(
            logged.lines().all(|l| l.starts_with("ts=")),
            "stamped: {logged:?}"
        );
        assert!(
            crate::redaction::meter::unauthored_emails(&logged, &[]).is_empty(),
            "no non-authored email (#15/#444): {logged:?}"
        );

        // The 401 streak is per-occurrence and climbs across ticks.
        assert!(
            logged.contains("event=monitor_401 account=work consecutive=1"),
            "{logged:?}"
        );
        assert!(
            logged.contains("event=monitor_401 account=work consecutive=2"),
            "{logged:?}"
        );
        // The per-account keychain-lock is silent now (#13): NO lock line appears,
        // even though account `spare`'s poll returned a locked error every tick.
        assert!(
            !logged.contains("event=keychain_locked_wait"),
            "a per-account lock must not emit a line: {logged:?}"
        );
        // The 403 line renders once per poll of C (one poll across the four staggered
        // ticks, #80) and carries `status=403`.
        assert_eq!(
            logged
                .lines()
                .filter(|l| l.contains("event=usage_scope_fail account=backup status=403"))
                .count(),
            1,
            "{logged:?}"
        );
        // The active account was unavailable every tick, so no swap line appears;
        // the streak is pure observability. Final state: account 0 saw two 401s.
        assert!(!logged.contains("event=swap"), "{logged:?}");
        let streak_of = |d: &FakeDaemon| {
            d.state
                .accounts
                .iter()
                .map(|a| a.health.consec_401)
                .collect::<Vec<_>>()
        };
        assert_eq!(streak_of(&daemon), vec![2, 0, 0]);
    }

    #[tokio::test]
    async fn run_loop_logs_a_weekly_reason_when_only_the_weekly_dimension_trips() {
        // Issue #9: a swap driven by the WEEKLY dimension (session below its
        // trigger) is logged `reason=weekly`, while `session_pct` still reports the
        // outgoing account's session reading (the schema carries no weekly percent).
        // This guards the reason re-derivation against mislabeling a weekly-only
        // swap as `session`.
        let roster = vec![account("u-A", "work"), account("u-B", "spare")];
        let store = store_holding(b"A-token").await;
        let stash = stash_with(&[
            ("Sessiometer/u-A", b"A-token", "u-A"),
            ("Sessiometer/u-B", b"B-token", "u-B"),
        ])
        .await;
        let (_dir, json) = claude_json("u-A");
        // Session 0.50 is below the 95 % session trigger; weekly 0.99 is over the
        // fixed 98 % weekly trigger → a weekly-only swap. Target B is under the floor.
        // The swap fires on the warm-up-completing second staggered tick (#80).
        let poller = FakeRosterPoller::new()
            .ok("u-A", 0.50, 0.99)
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

        let logdir = tempfile::tempdir().unwrap();
        let log_path = logdir.path().join("sessiometer.log");
        let mut log = EventLog::at(&log_path).unwrap();
        // after(3): 2 idle shutdown-checks + 1 start-up check (#76 de-burst) — two
        // warm-up ticks (poll A, then poll B + swap).
        let mut shutdown = FakeShutdown::after(3);
        let control = NoControl;

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

        // The swap line carries the weekly reason; since #137 `spare` also logs one honest
        // Unknown→healthy transition once the swap makes it active and its poll verifies it.
        let logged = std::fs::read_to_string(&log_path).unwrap();
        assert_eq!(
            logged.lines().count(),
            2,
            "swap + spare health line: {logged:?}"
        );
        assert!(
            logged.contains("event=swap from=work to=spare reason=weekly session_pct=50"),
            "got: {logged:?}"
        );
        assert!(
            logged.contains("event=credential_health account=spare state=healthy"),
            "spare verified healthy once polled after the swap (#137): {logged:?}"
        );
    }
}
