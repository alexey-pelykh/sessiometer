// Copyright (c) 2026 Oleksii PELYKH
// SPDX-License-Identifier: MIT

//! The daemon run loop (issues #6/#8/#9/#77/#105/#139/#140; the #195 per-concern decomposition
//! after #202/#203 untied the contract cycle and split off the socket / snapshot / peer-auth
//! concerns).
//!
//! Reconcile-on-start, then forever: [`Daemon::tick`], report its events + operator diagnostics,
//! and idle until the next poll — meanwhile serving the control socket, driving the periodic
//! refresh sweep, and watching for an out-of-band login and for shutdown. Shutdown is observed
//! only HERE, between ticks, so a swap inside `tick` always runs to completion (#6 no-half-swap).
//!
//! [`run_loop()`] is the thin orchestrator; each of its ~6 concerns (CODE-F3) is a named
//! single-purpose helper: [`await_startup_delay`] de-bursts the first poll,
//! [`report_tick_outcome`] fans a tick out to its log channels, [`idle_until_next_tick`] runs the
//! shutdown/control/refresh/login-watch idle select, and [`apply_post_idle`] applies the mutations
//! the idle deferred. Drives the [`Daemon`] decision core through its `tick` and idle-path
//! methods; `run_loop` is re-exported under `crate::daemon::*` for `crate::cli` and the
//! in-module test suite.

use super::*;

/// The console line for a swap this cycle, or `None` for any non-swap outcome.
/// Surfaced to the operator watching the foreground `run` (issue #8) — the file
/// event log records every cycle separately. Both swap kinds echo: a normal swap
/// and the #42 emergency swap away from a dead active credential (the latter named
/// distinctly, since it means a credential just died and the daemon force-rotated).
/// Sourced solely from labels, so it can never carry a token or email (issue #15).
pub(crate) fn swap_report(outcome: &TickOutcome) -> Option<String> {
    match outcome.action {
        TickAction::Swapped { from, to } => Some(format!(
            // `off <from> onto <to>` rather than `<from> → <to>` (issue #89): the
            // bare arrow reads ambiguously, but `to` is the account just made
            // active (swapped ONTO) and `from` the one swapped OFF — spell it out
            // so the operator can never misread the direction.
            "swapped off {} onto {}",
            label_at(&outcome.snapshot, from),
            label_at(&outcome.snapshot, to),
        )),
        TickAction::EmergencySwapped { from, to } => Some(format!(
            // Same off/onto phrasing (#89), still named distinctly — the trailing
            // cause tells the operator a credential just died and forced this.
            "emergency-swapped off {} onto {} (dead credential)",
            label_at(&outcome.snapshot, from),
            label_at(&outcome.snapshot, to),
        )),
        _ => None,
    }
}

/// The label of the roster account at `index` in `snapshot`, or `"?"` if out of
/// range. A swap's indices are always valid, but the long-running daemon must
/// never panic on a display path, so this stays total.
fn label_at(snapshot: &StatusSnapshot, index: usize) -> &str {
    snapshot
        .accounts
        .get(index)
        .map_or("?", |account| account.label.as_str())
}

/// Emit one event to the event log, best-effort: a write failure is logged to stderr and
/// swallowed (issue #9). The daemon must never die on a logging failure, and one failed event
/// must not drop the rest — so this never returns an error. The single home for the four
/// tick / idle / post-idle emit sites, so the fail-open path and its message live in one place.
fn emit_best_effort(log: &mut EventLog, event: &Event) {
    if let Err(err) = log.emit(event) {
        eprintln!("sessiometer: event log write failed: {err}");
    }
}

/// How the idle-until-next-tick wait ended. A module-level enum so each idle arm and the
/// post-idle dispatch name the same cases. The wait future (and its `&mut Daemon` borrow) is
/// scoped to [`idle_until_next_tick`] and dropped on its return, before the run loop applies a
/// `ManualSwapped` adoption, which needs its own `&mut Daemon`.
enum Idle {
    /// SIGINT / SIGTERM observed — exit the loop cleanly.
    Shutdown,
    /// The poll interval (or a back-off wait — #13 locked-keychain or #76
    /// rate-limit) elapsed — re-tick.
    Elapsed,
    /// A manual `use` swap notified the daemon (#64) — adopt it, then re-tick.
    ManualSwapped,
    /// A roster write (`capture` / `login` / `remove`) notified the daemon (#139)
    /// — reload + reconcile the in-memory roster, then re-tick.
    RosterReloadRequested,
    /// The external-login watch (#140) saw the canonical credential change out-of-band
    /// during the idle (a manual `claude /login`) — re-tick NOW, off the usage-poll cadence,
    /// so the next `tick`'s `reconcile_canonical_change` re-stashes / re-resolves / surfaces
    /// it within the watch cadence instead of up to a full poll interval later.
    ExternalLoginDetected,
}

/// The four DI seams the idle phase multiplexes: shutdown (#6), the control socket (#64), the
/// periodic refresh ticker (#105), and the external-login watch (#140) — exactly the arms of
/// [`idle_until_next_tick`]'s `select!`. Bundled so that function stays within the 7-argument
/// clippy bound (this repo never `#[allow]`s `too_many_arguments`) — inlining the four seams would
/// push it to eight — and so its signature names one "what it awaits" group rather than four
/// parallel params; [`run_loop()`] threads a single value. Built once by the
/// run loop (after the startup delay, which needs only `shutdown`) and reborrowed each iteration;
/// [`idle_until_next_tick`] reborrows each field back to a plain `&mut` / `&` at entry, so its
/// select body is unchanged from the four-param form. `control` is shared (`&`) — a `status` read
/// mutates nothing; the others are `&mut`.
struct IdleSeams<'a, Sh, Ctl, R, LW> {
    shutdown: &'a mut Sh,
    control: &'a Ctl,
    refresh: &'a mut R,
    login_watch: &'a mut LW,
}

/// De-burst the FIRST poll (issue #76): wait a small jittered delay before the first tick, so
/// repeated restarts of the same config do not synchronize an immediate burst of usage requests.
/// Behind the Clock seam, so tests pass through it instantly. Shutdown-responsive (like the
/// per-cycle idle): a SIGINT / SIGTERM during the delay returns `true` so the run loop exits
/// cleanly rather than deferring the stop for up to `STARTUP_DELAY_CAP`. No control serving here —
/// there is no snapshot to answer from until the first tick.
async fn await_startup_delay<P, C, S, K, Sh>(
    daemon: &mut Daemon<P, C, S, K>,
    shutdown: &mut Sh,
) -> bool
where
    P: RosterPoller,
    C: CredentialStore,
    S: AccountStash,
    K: Clock,
    Sh: Shutdown,
{
    let startup_delay = daemon.startup_delay();
    tokio::select! {
        biased;
        _ = shutdown.requested() => true,
        _ = daemon.clock.tick(startup_delay) => false,
    }
}

/// Fan one tick's outcome out to its channels: emit each structured event to the event log
/// (issue #9) and each operator diagnostic to the verbosity-gated diagnostic channel (issue
/// #77), then echo any swap to the foreground process (issue #8). Best-effort logging — a write
/// failure must not kill the daemon, and one failed event must not drop the rest — so each
/// emission logs and continues, never returns.
fn report_tick_outcome<W: Write>(
    outcome: &TickOutcome,
    log: &mut EventLog,
    diag: &mut DiagnosticLog<W>,
) {
    for event in &outcome.events {
        emit_best_effort(log, event);
    }
    for diagnostic in &outcome.diagnostics {
        diag.emit(diagnostic);
    }
    // The console gets just swaps, sourced solely from labels (issue #15); the file event log
    // (above) records every cycle.
    if let Some(report) = swap_report(outcome) {
        eprintln!("sessiometer: {report}");
    }
}

/// Idle until the next tick is due, meanwhile serving the control socket, driving the periodic
/// refresh sweep (issue #105/#106), and watching for an out-of-band login (issue #140) and for
/// shutdown. A swap (if any) already completed inside `tick`, so a shutdown observed here aborts
/// cleanly before the next tick — no half-swap (#6).
///
/// The wait future borrows `&mut daemon`, so it is scoped to this function and dropped on its
/// return, BEFORE the run loop applies any post-idle mutation. The sweep's RESTORES (issue #106)
/// and credential-clock OBSERVATIONS (issue #119) mutate the health machine (which also needs
/// `&mut daemon`), so they are collected here and returned for the caller to apply once the wait
/// borrow is gone — the same deferral pattern the manual-swap adoption uses.
async fn idle_until_next_tick<P, C, S, K, Sh, Ctl, R, LW>(
    daemon: &mut Daemon<P, C, S, K>,
    log: &mut EventLog,
    seams: &mut IdleSeams<'_, Sh, Ctl, R, LW>,
    snapshot: &StatusSnapshot,
    next_wait: Option<Duration>,
) -> (Idle, Vec<String>, Vec<RefreshObservation>)
where
    P: RosterPoller,
    C: CredentialStore,
    S: AccountStash,
    K: Clock,
    Sh: Shutdown,
    Ctl: Control,
    R: RefreshTicker,
    LW: ExternalLoginWatch,
{
    // Reborrow the bundled seams back into plain per-seam handles up front, so the select below
    // reads exactly as the four-param form did — a split (disjoint-field) borrow, so all four arms
    // can hold their handle at once across the select.
    let shutdown = &mut *seams.shutdown;
    let control = seams.control;
    let refresh = &mut *seams.refresh;
    let login_watch = &mut *seams.login_watch;

    // The accounts the periodic refresh tick (#105) must not touch this idle period — the active
    // account and the imminent swap target — and the quarantined ("needs re-login") accounts it
    // SHOULD attempt for the RESTORE path (issue #106). Both are computed from the POST-tick state
    // HERE, before the idle borrows `&mut daemon`; the tick owns its own roster copy + clock, so
    // the sweep below needs nothing from it.
    let refresh_excluded = daemon.refresh_exclusions();
    let refresh_quarantined = daemon.refresh_quarantined();
    // Accounts the sweep proved still refreshable (issue #106) and the credential-clock
    // observations it read (issue #119): collected inside the idle loop (where `&mut daemon` is
    // held by `wait`) and returned for the caller to apply AFTER it, when `&mut daemon` is free
    // again — the same post-idle pattern as the manual-swap adoption.
    let mut refresh_restored: Vec<String> = Vec::new();
    let mut refresh_observations: Vec<RefreshObservation> = Vec::new();
    // The canonical the daemon last COMMITTED to its watch (issue #140), snapshotted HERE — before
    // the idle borrows `&mut daemon` — so the external-login watch arm can tell an out-of-band
    // write it reads DURING the idle from the daemon's own last state, without needing
    // `&mut daemon` mid-idle. The daemon's own writes (a swap) commit the watch, so this baseline
    // already reflects them and they are never mis-seen as external.
    let canonical_baseline = daemon.canonical_baseline();

    // The wait future borrows `&mut daemon`, so it is scoped to this block and dropped before the
    // returned tuple is built — leaving `&mut daemon` free for the caller's post-idle mutation.
    let idle = {
        let wait = daemon.wait_after_tick(next_wait);
        tokio::pin!(wait);
        loop {
            tokio::select! {
                biased;
                _ = shutdown.requested() => break Idle::Shutdown,
                // A served control connection may carry a signal (#64): a
                // `manual-swapped` breaks the idle to adopt it; a `status` read
                // (None) just continues serving until the wait elapses.
                signal = control.serve(snapshot) => match signal {
                    Some(ControlSignal::ManualSwapped) => break Idle::ManualSwapped,
                    // A `roster-reload` (#139) breaks the idle to reconcile the
                    // in-memory roster to the freshly-written config; a `status`
                    // read (None) just continues serving until the wait elapses.
                    Some(ControlSignal::RosterReloadRequested) => {
                        break Idle::RosterReloadRequested
                    }
                    None => continue,
                },
                // The periodic isolated-refresh tick (issue #105), in the idle path off
                // the poll→usage→swap seam. `until_due` resolves only when a refresh is
                // due — and NEVER when the feature is off (the no-op ticker) — so this arm
                // is inert by default. When it fires, run the sweep under a NESTED select
                // so ONLY a shutdown can interrupt it: a control read must not cancel an
                // in-flight refresh (the swap-lock-holding engine is cancel-safe, but a
                // status query should neither forfeit a token nor be able to starve the
                // sweep). `wait` is pinned OUTSIDE this loop, so a sweep does not reset the
                // poll cadence; after it the loop idles on until the wait elapses.
                () = refresh.until_due() => {
                    tokio::select! {
                        biased;
                        _ = shutdown.requested() => break Idle::Shutdown,
                        sweep = refresh.sweep(&refresh_excluded, &refresh_quarantined) => {
                            // Emit each per-cycle refresh event (issue #106) to the event
                            // log — the SAME best-effort path the tick's events ride; `log`
                            // is not borrowed by `wait`, so it is free to use here. The
                            // RESTORES are deferred: un-quarantining mutates the health
                            // machine (needs `&mut daemon`, held by `wait`), so they are
                            // collected here and applied after the idle block.
                            for event in &sweep.events {
                                emit_best_effort(log, event);
                            }
                            refresh_restored.extend(sweep.restored);
                            // The #119 credential-clock observations, deferred like the
                            // restores: folding them mutates the health machine.
                            refresh_observations.extend(sweep.observations);
                        }
                    }
                }
                // The external-login watch (issue #140): a dedicated SHORT-cadence, LOCAL
                // (no-network) probe of the canonical credential, DECOUPLED from the
                // usage-poll cadence, so a manual `claude /login` on the active account is
                // reflected within the watch cadence, not up to a full poll interval. The
                // probe reads the canonical via the watch's OWN store (the daemon's is
                // borrowed by `wait`) and compares against the pre-idle committed baseline;
                // a difference is an out-of-band write since the last tick → break to
                // re-tick, so `tick`'s `reconcile_canonical_change` does the authoritative
                // re-stash / re-resolve / surface. Fail-safe: an unreadable / locked /
                // absent probe (`None`), or a byte-identical read, or no baseline yet,
                // detects nothing and keeps idling — the loop never stalls. `wait` is pinned
                // OUTSIDE this loop, so a probe does not reset the poll cadence.
                () = login_watch.until_due() => {
                    if let Some(current) = login_watch.read_canonical().await {
                        if canonical_baseline
                            .as_ref()
                            .is_some_and(|base| !base.matches(&current))
                        {
                            break Idle::ExternalLoginDetected;
                        }
                    }
                }
                _ = &mut wait => break Idle::Elapsed,
            }
        }
    };
    (idle, refresh_restored, refresh_observations)
}

/// Apply the mutations the idle period deferred until its `&mut daemon` borrow dropped, then
/// diff the credential-health rollup. In order: un-quarantine each account the sweep RESTORED
/// (issue #106), logging its edge-triggered `credential_restored`; fold the sweep's
/// credential-clock OBSERVATIONS into the health state (issue #119) BEFORE the diff so a
/// transition reflects this cycle's refresh; then emit one edge-triggered `credential_health`
/// per rollup CHANGE (issue #119, AC-3) — run EVERY iteration, not only on a sweep, so a
/// time-driven transition (the access token crossing its expiry) and a quarantine-driven one
/// (the #42 path, even with the refresh feature OFF) are both caught; the first computation per
/// account seeds the baseline silently. Best-effort logging throughout.
fn apply_post_idle<P, C, S, K>(
    daemon: &mut Daemon<P, C, S, K>,
    log: &mut EventLog,
    refresh_restored: &[String],
    refresh_observations: &[RefreshObservation],
) where
    P: RosterPoller,
    C: CredentialStore,
    S: AccountStash,
    K: Clock,
{
    // Restores are applied on every idle exit (shutdown included — the restore genuinely
    // happened, so the log record is honest; the durable effect is the re-stashed fresh token,
    // which persists regardless of the in-memory flip).
    for uuid in refresh_restored {
        if let Some(event) = daemon.apply_refresh_restore(uuid) {
            emit_best_effort(log, &event);
        }
    }
    for observation in refresh_observations {
        daemon.apply_refresh_observation(observation);
    }
    for event in daemon.note_health_transitions(wall_clock_now_secs()) {
        emit_best_effort(log, &event);
    }
}

/// Drive the poll loop until shutdown.
///
/// Reconcile-on-start, then forever: tick, report to the log channels ([`report_tick_outcome`]),
/// and idle until the next poll ([`idle_until_next_tick`]) — meanwhile serving control requests
/// and watching for shutdown — then apply the idle's deferred mutations ([`apply_post_idle`]) and
/// dispatch on how it ended. Shutdown is observed only between ticks, never mid-tick: a swap
/// inside [`Daemon::tick`] always runs to completion, so a shutdown can never tear a swap
/// (complete-or-abort; #6 is no-half-swap). The lifecycle markers (`diag=start` / `diag=stop`)
/// bracket this call in [`crate::cli`], which owns the process lifecycle; this loop emits only
/// the per-tick diagnostics.
pub(crate) async fn run_loop<P, C, S, K, Sh, Ctl, R, LW, W>(
    daemon: &mut Daemon<P, C, S, K>,
    log: &mut EventLog,
    diag: &mut DiagnosticLog<W>,
    shutdown: &mut Sh,
    control: &Ctl,
    refresh: &mut R,
    login_watch: &mut LW,
) -> Result<()>
where
    P: RosterPoller,
    C: CredentialStore,
    S: AccountStash,
    K: Clock,
    Sh: Shutdown,
    Ctl: Control,
    R: RefreshTicker,
    LW: ExternalLoginWatch,
    W: Write,
{
    // Reconcile-on-start is best-effort: a failure is logged and the loop still
    // starts — the next swap re-establishes consistency anyway.
    if let Err(err) = daemon.reconcile_on_start().await {
        eprintln!("sessiometer: reconcile-on-start skipped: {err}");
    }

    // De-burst start-up (issue #76), shutdown-responsive: a SIGINT / SIGTERM during the delay
    // exits cleanly rather than being deferred for up to STARTUP_DELAY_CAP.
    if await_startup_delay(daemon, shutdown).await {
        return Ok(());
    }

    // Bundle the four idle-phase seams once (after the startup delay, which needs only `shutdown`);
    // each loop iteration reborrows `&mut seams` for the idle select.
    let mut seams = IdleSeams {
        shutdown,
        control,
        refresh,
        login_watch,
    };

    loop {
        let outcome = daemon.tick().await;
        report_tick_outcome(&outcome, log, diag);
        // The wait this tick requested — an explicit back-off overrides the normal interval
        // (locked-keychain #13, or rate-limit / transient #76) — captured before the snapshot is
        // moved. The snapshot is what the control socket answers from until the next poll.
        let next_wait = outcome.next_wait;
        let snapshot = outcome.snapshot;

        // Idle until the next tick, collecting the sweep's deferred restores + observations to
        // apply once the idle's `&mut daemon` borrow has dropped.
        let (idle, refresh_restored, refresh_observations) =
            idle_until_next_tick(daemon, log, &mut seams, &snapshot, next_wait).await;

        apply_post_idle(daemon, log, &refresh_restored, &refresh_observations);

        match idle {
            Idle::Shutdown => return Ok(()),
            // Adopt the manual `use` swap (#64) — arm the cooldown so the next tick
            // holds on the operator's choice, and re-resolve active from the
            // canonical — BEFORE looping back to re-tick.
            Idle::ManualSwapped => daemon.adopt_manual_swap().await,
            // Reload + reconcile the in-memory roster to the freshly-written
            // `config.toml` (#139) — the onboarded / relogged-in / removed account is
            // adopted into the live rotation — BEFORE looping back to re-tick.
            Idle::RosterReloadRequested => daemon.adopt_roster_reload().await,
            // The external-login watch (#140) detected an out-of-band canonical change: just
            // re-tick — the next `tick` reads the canonical and its `reconcile_canonical_change`
            // does the authoritative re-stash / re-resolve / surface (no pre-tick adoption
            // needed, unlike a manual swap or a roster reload).
            Idle::ExternalLoginDetected => {}
            Idle::Elapsed => {}
        }
    }
}
