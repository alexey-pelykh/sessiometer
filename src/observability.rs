// Copyright (c) 2026 Oleksii PELYKH
// SPDX-License-Identifier: MIT

//! The structured event log.
//!
//! One line per daemon EVENT, in a flat space-separated `key=val` grammar:
//!
//! ```text
//! ts=<RFC3339> event=<name> <key=val>…
//! ```
//!
//! emitted through the single [`Event::to_log_line`] formatter to
//! `~/Library/Logs/sessiometer/sessiometer.log` (macOS-native, surfaced in
//! Console.app) via the path-resolution module (#1). No logging framework: no
//! levels, rotation, or filtering — plain timestamped lines suffice (issue #9).
//!
//! ## Redaction surface (issue #15)
//!
//! Every [`Event`] field is a HANDLE (the operator label), an enum, a number, or
//! a timestamp — never free-form, secret-bearing text. That type-level constraint
//! is what makes [`Event::to_log_line`] the *sole* place an event becomes a log
//! line, and therefore the one surface the redaction METER (#15) has to check:
//! nothing else interpolates account data onto this channel. Identity is always
//! the stable handle — never an email, never a token.
//!
//! Note for #15: a handle is an operator-chosen label; config validation forbids
//! an *empty* label but not whitespace, so a label containing a space or `=` would
//! split the `key=val` grammar. Enforcing the handle charset is the meter's job
//! (#15); this module localizes the surface but does not police it.
//!
//! ## The diagnostic channel (issue #77)
//!
//! Separate from the event log above, the OPERATOR-FACING diagnostic channel
//! answers "what is `run` doing right now" — per-poll outcomes, the per-tick
//! decision, and lifecycle markers — for an operator debugging the daemon. Where
//! the event log records durable STATE CHANGES (edge-triggered, levelless), the
//! diagnostic channel is per-cycle DETAIL behind a verbosity gate ([`Verbosity`]):
//! default [`Verbosity::Quiet`] emits nothing, `-v`/`--verbose` opts in. It rides
//! its own single redaction surface — [`Diagnostic::to_log_line`], the sibling of
//! [`Event::to_log_line`] — under the SAME field discipline (every field a handle /
//! enum / number / timestamp, never a token or email), so the #15 METER scans
//! rendered diagnostics alongside events and the channel inherits the redaction
//! guarantee without weakening it.

use std::fs::File;
use std::io::Write;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::error::Result;
use crate::paths;

/// Why a swap happened — the `reason=` of an [`Event::Swap`].
///
/// The two AUTONOMOUS reasons are re-derived at swap time from the readings (the
/// binary [`crate::swap::decide`] does not carry which dimension fired); when BOTH
/// dimensions are at/over their triggers, the daemon reports [`SwapReason::Session`]
/// — session-first precedence. The two MANUAL reasons (issue #63) are operator-driven,
/// NOT usage-triggered: [`SwapReason::Manual`] is a `sessiometer use <account>` whose
/// pre-swap gate passed, and [`SwapReason::Forced`] is one whose policy gate was
/// bypassed with `--force`. A manual swap records `session_pct=0` (it was not driven
/// by session usage — this `reason=` is what distinguishes it).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SwapReason {
    /// The session-window trigger fired (or both did — session takes precedence).
    Session,
    /// The weekly-window trigger fired while session was below its own.
    Weekly,
    /// An operator `sessiometer use <account>` whose pre-swap gate PASSED (#63).
    Manual,
    /// An operator `sessiometer use <account> --force` whose policy gate was
    /// BYPASSED (#63). Safety behavior is never bypassed — only policy.
    Forced,
}

impl SwapReason {
    /// The `reason=` token.
    fn as_str(self) -> &'static str {
        match self {
            SwapReason::Session => "session",
            SwapReason::Weekly => "weekly",
            SwapReason::Manual => "manual",
            SwapReason::Forced => "forced",
        }
    }
}

/// One observable daemon state change, rendered as a single `key=val` log line by
/// [`Event::to_log_line`].
///
/// Every field is a handle / enum / number / timestamp — never a token or email
/// (issue #15). That is the type-level guarantee behind the single-surface
/// redaction claim in the module docs.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Event {
    /// The active credential was rotated away from `from` to `to` because `reason`
    /// reached its trigger; `session_pct` is the outgoing account's session usage
    /// (percent) at swap time.
    ///
    /// `from`/`to` are HANDLES (operator labels), NOT roster indices — unlike the
    /// same-named fields of [`crate::daemon::TickAction::Swapped`].
    Swap {
        from: String,
        to: String,
        reason: SwapReason,
        session_pct: u8,
    },
    /// `account`'s canonical credential changed underneath the daemon — the
    /// operator ran `claude /login` and re-authenticated it — so its stash was
    /// refreshed to the new token (issue #13 re-auth re-stash). `account` is the
    /// HANDLE (operator label), resolved from the new canonical's identity.
    ReStash { account: String },
    /// The active account is over a trigger but no other account is a viable swap
    /// target — the all-exhausted terminal state (issue #11). `hold` is the
    /// least-bad account the daemon holds on: the one whose weekly window resets
    /// soonest. `resets_at` is that account's weekly reset as epoch seconds,
    /// rendered to RFC 3339 by [`Event::to_log_line`] and present whenever the API
    /// supplied a parseable timestamp; `None` (the field is omitted) when no
    /// account reported one, keeping the line forward-compatible.
    AllExhausted {
        hold: String,
        resets_at: Option<i64>,
    },
    /// `account`'s stored token was rejected with HTTP 401 `consecutive` times in a
    /// row — the climbing streak toward the dead-credential threshold (issue #42).
    /// Emitted per 401 while the account is still healthy; once it crosses
    /// `monitor_401_n` and is quarantined, the streak stops being logged (the
    /// [`Event::CredentialDead`] transition is signaled instead, and a quarantined
    /// account is no longer polled). Distinct from a re-stash, which is driven by
    /// canonical-change detection ([`Event::ReStash`]).
    Monitor401 { account: String, consecutive: u32 },
    /// `account`'s stored credential is DEAD: its token was rejected `monitor_401_n`
    /// times in a row, so the daemon quarantines it — it stops polling and selecting
    /// it for the rotation until the operator re-logs-in (issue #42). Edge-triggered:
    /// emitted exactly ONCE on the death transition, never per failed poll. The
    /// durable "needs re-login" status is surfaced separately by `status`. `account`
    /// is the HANDLE (operator label) — never a token or email.
    CredentialDead { account: String },
    /// The ACTIVE account's credential died, blocking the live session, so the daemon
    /// emergency-swapped from `from` to `to` — the soonest-reset viable account —
    /// bypassing the normal swap-away trigger and post-swap cooldown (issue #42).
    /// Edge-triggered: exactly ONE per emergency swap. `from`/`to` are HANDLES
    /// (operator labels), never tokens or emails.
    EmergencySwap { from: String, to: String },
    /// A quarantined (dead) `account` recovered: the operator re-logged-in (its
    /// canonical credential changed and was re-stashed, #13) and it then polled
    /// successfully `monitor_recovery_m` times in a row, so the daemon un-quarantined
    /// it and returned it to the rotation (issue #42). Edge-triggered: exactly ONCE
    /// on the recovery transition. `account` is the HANDLE — never a token or email.
    CredentialRestored { account: String },
    /// The keychain was locked when the daemon went to read the canonical
    /// credential, so this tick's work is deferred and the daemon backs off (issue
    /// #13). Edge-triggered: emitted ONCE when the lock is first observed, not every
    /// tick it stays locked. No `account` — a locked keychain is a process-global
    /// condition (every account's stash is unreadable), not tied to one account.
    KeychainLockedWait,
    /// `account`'s token authenticated but lacks the usage scope (HTTP 403) — the
    /// hallmark of a non-interactive setup token (#5). Always `status=403`.
    UsageScopeFail { account: String },
}

impl Event {
    /// Render this event as its single log line (no trailing newline), stamped
    /// with `ts`.
    ///
    /// Pure and the *only* place an event becomes text, so the redaction surface
    /// (#15) is exactly this method. The timestamp is a parameter (not read here)
    /// so the formatting is deterministically unit-testable; [`EventLog::emit`]
    /// supplies `SystemTime::now()` at write time.
    pub(crate) fn to_log_line(&self, ts: SystemTime) -> String {
        let ts = rfc3339(ts);
        match self {
            Event::Swap {
                from,
                to,
                reason,
                session_pct,
            } => {
                let reason = reason.as_str();
                format!(
                    "ts={ts} event=swap from={from} to={to} reason={reason} session_pct={session_pct}"
                )
            }
            Event::ReStash { account } => {
                format!("ts={ts} event=restash account={account}")
            }
            Event::AllExhausted { hold, resets_at } => match resets_at {
                Some(secs) => {
                    let resets_at = rfc3339(system_time_from_epoch(*secs));
                    format!("ts={ts} event=all_exhausted hold={hold} resets_at={resets_at}")
                }
                None => format!("ts={ts} event=all_exhausted hold={hold}"),
            },
            Event::Monitor401 {
                account,
                consecutive,
            } => {
                format!("ts={ts} event=monitor_401 account={account} consecutive={consecutive}")
            }
            Event::CredentialDead { account } => {
                format!("ts={ts} event=credential_dead account={account}")
            }
            Event::EmergencySwap { from, to } => {
                format!("ts={ts} event=emergency_swap from={from} to={to}")
            }
            Event::CredentialRestored { account } => {
                format!("ts={ts} event=credential_restored account={account}")
            }
            Event::KeychainLockedWait => {
                format!("ts={ts} event=keychain_locked_wait")
            }
            Event::UsageScopeFail { account } => {
                format!("ts={ts} event=usage_scope_fail account={account} status=403")
            }
        }
    }
}

/// A [`SystemTime`] from epoch seconds — used to render an `all_exhausted`
/// event's `resets_at` (issue #11) through the same [`rfc3339`] formatter as the
/// line timestamp, so reset times read identically regardless of whether the API
/// gave an epoch or an ISO string. A negative (pre-epoch) input is not expected
/// for a reset time but is handled so this best-effort log path can never panic
/// (it renders via `rfc3339`'s epoch sentinel).
fn system_time_from_epoch(secs: i64) -> SystemTime {
    if secs >= 0 {
        UNIX_EPOCH + Duration::from_secs(secs as u64)
    } else {
        UNIX_EPOCH - Duration::from_secs(secs.unsigned_abs())
    }
}

/// Format a wall-clock instant as whole-second UTC RFC 3339 (`YYYY-MM-DDTHH:MM:SSZ`).
///
/// Dependency-free (there is no date crate in the graph): epoch seconds → civil
/// date via [`civil_from_days`]. Events are second-granular, so no fractional part
/// is emitted. A pre-1970 clock (a `duration_since` error) renders as the epoch — a
/// clearly-wrong but safe sentinel, so a skewed clock can never panic a log write
/// (the daemon's logging is best-effort).
fn rfc3339(ts: SystemTime) -> String {
    let secs = ts
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let days = (secs / 86_400) as i64;
    let rem = secs % 86_400;
    let (hour, minute, second) = (rem / 3600, (rem % 3600) / 60, rem % 60);
    let (year, month, day) = civil_from_days(days);
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}Z")
}

/// Convert a count of days since 1970-01-01 to a `(year, month, day)` proleptic
/// Gregorian civil date — Howard Hinnant's `civil_from_days`. Correct across leap
/// years and the 100/400 century rules (e.g. 2000 is a leap year, 2100 is not).
/// Every intermediate is non-negative for the post-epoch range we format, so the
/// `as u32` narrowings on the final month/day (each well within range) cannot lose
/// information. [`rfc3339`] only ever passes `z >= 0` (a pre-epoch clock renders as
/// the epoch sentinel), so Hinnant's negative-`z` branch is retained verbatim but
/// unreached here.
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = (if z >= 0 { z } else { z - 146_096 }) / 146_097;
    let doe = z - era * 146_097; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365; // [0, 399]
    let year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let day = doy - (153 * mp + 2) / 5 + 1; // [1, 31]
    let month = if mp < 10 { mp + 3 } else { mp - 9 }; // [1, 12]
    (year + i64::from(month <= 2), month as u32, day as u32)
}

/// The path of the structured event log: `sessiometer.log` under the native log
/// directory (`~/Library/Logs/sessiometer/`, #1).
///
/// Factored out as the single source of truth for the filename so its two
/// consumers cannot drift: [`EventLog::open`] (which writes it) and the one-shot
/// `use` verb's cooldown gate (#63), which reads the durable swap record from the
/// same file via [`last_swap_at`].
pub(crate) fn log_path() -> Result<std::path::PathBuf> {
    Ok(paths::logs_dir()?.join("sessiometer.log"))
}

/// The structured event log at `~/Library/Logs/sessiometer/sessiometer.log`
/// (`0600`).
pub(crate) struct EventLog {
    file: File,
}

impl EventLog {
    /// Open the event log, creating the log directory (`0700`) and file (`0600`)
    /// if needed.
    pub(crate) fn open() -> Result<Self> {
        let path = log_path()?;
        // `log_path()` is always `<logs_dir>/sessiometer.log`, so the parent is the
        // native log directory — ensure it (`0700`) before creating the file.
        paths::ensure_private_dir(
            path.parent()
                .expect("log_path() always has a logs-dir parent"),
        )?;
        let file = paths::create_private_file(&path)?;
        Ok(Self { file })
    }

    /// Append `event` as exactly one line, stamped with the current wall-clock
    /// time. The line is built whole and written in a single `write_all`, so a
    /// concurrent reader (Console.app) never observes a torn line.
    pub(crate) fn emit(&mut self, event: &Event) -> Result<()> {
        let mut line = event.to_log_line(SystemTime::now());
        line.push('\n');
        self.file.write_all(line.as_bytes())?;
        Ok(())
    }

    /// Open an event log at an explicit path (tests only), bypassing the native
    /// log directory so the run loop can be exercised hermetically.
    #[cfg(test)]
    pub(crate) fn at(path: &std::path::Path) -> Result<Self> {
        Ok(Self {
            file: paths::create_private_file(path)?,
        })
    }
}

/// The wall-clock instant of the MOST RECENT swap recorded in the event log at
/// `path`, or `None` when the log is absent/unreadable or records no swap.
///
/// The durable, daemon-INDEPENDENT swap record the one-shot `use` verb (#63)
/// consults for its cooldown gate (#10): the daemon's in-memory `last_swap` is not
/// persisted (it is surfaced only over the live control socket), so the structured
/// log — which records every swap through [`Event::to_log_line`] — is the only
/// source a standalone command can read. Both a normal `event=swap` (now including
/// the `use` verb's own `reason=manual|forced`) and an `event=emergency_swap` update
/// the daemon's cooldown floor, so both count here. Best-effort: an unreadable file
/// or an unparseable timestamp yields `None`, so a one-shot manual swap is never
/// blocked by a missing or corrupt log (the cooldown then reads as inactive).
pub(crate) fn last_swap_at(path: &std::path::Path) -> Option<SystemTime> {
    let text = std::fs::read_to_string(path).ok()?;
    // Scan from the END: the log is append-only chronological, so the last swap
    // line is the most recent swap. The surrounding spaces anchor the event key so
    // a label that merely contains the text cannot be mistaken for it.
    let line = text
        .lines()
        .rev()
        .find(|line| line.contains(" event=swap ") || line.contains(" event=emergency_swap "))?;
    let raw_ts = line.strip_prefix("ts=")?.split(' ').next()?;
    let epoch = crate::usage::epoch_from_rfc3339(raw_ts)?;
    // The log only ever writes post-epoch instants; guard the cast so a malformed
    // pre-epoch stamp degrades to `None` rather than wrapping into a wrong instant.
    (epoch >= 0).then(|| UNIX_EPOCH + Duration::from_secs(epoch as u64))
}

/// Operator-facing diagnostic verbosity (issue #77) for the `run` daemon. Default
/// [`Quiet`](Self::Quiet) — no console spam without opt-in; `-v`/`--verbose` selects
/// [`Verbose`](Self::Verbose). The gate is applied by [`DiagnosticLog::emit`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Verbosity {
    /// Default: drop every diagnostic line (the diagnostic channel is silent).
    Quiet,
    /// Emit per-poll, per-tick, and lifecycle diagnostics to the channel sink.
    Verbose,
}

/// The per-poll outcome class on the DIAGNOSTIC channel (issue #77) — the operator
/// taxonomy that SEPARATES a rate-limit (`429`) from a generic transient (`5xx` /
/// network / unreadable), unlike the daemon's poll classification
/// ([`crate::daemon`]'s health-machine `PollOutcome`), which folds both into one
/// transient class. The two views are deliberately different: a rate-limit storm and
/// a flaky network read are the same to the back-off, but an operator staring at the
/// channel needs to tell "I am being throttled" apart from "the endpoint is flaky".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PollClass {
    /// A successful usage reading — the credential is alive.
    Live,
    /// HTTP 401 — the stored token was rejected.
    Unauthorized,
    /// HTTP 403 — authenticated but lacking the usage scope (issue #5).
    Scope,
    /// HTTP 429 — rate-limited; the daemon backs off (issue #76).
    RateLimited,
    /// Any other failure (`5xx` / network / unreadable token / unparseable body) —
    /// a generic transient carrying no liveness signal.
    Transient,
}

impl PollClass {
    /// The `outcome=` token.
    fn as_str(self) -> &'static str {
        match self {
            PollClass::Live => "live",
            PollClass::Unauthorized => "unauthorized",
            PollClass::Scope => "scope",
            PollClass::RateLimited => "rate_limited",
            PollClass::Transient => "transient",
        }
    }
}

/// The per-tick DECISION class on the diagnostic channel (issue #77) — the operator
/// rendering of the daemon's per-cycle verdict, one token per
/// [`crate::daemon::TickAction`]. The swap PARTICIPANTS (the from/to handles) are
/// deliberately NOT carried here: they already ride the event log's `swap` line and
/// the foreground swap echo, so the diagnostic decision line stays a pure label.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DecisionClass {
    /// Active is below the swap-away trigger — stay put.
    Hold,
    /// Swapped the active credential to a viable target.
    Swap,
    /// Emergency-swapped away from a dead active account (issue #42).
    EmergencySwap,
    /// Over the trigger but no viable target — the all-exhausted hold (issue #11).
    AllExhausted,
    /// The active credential is dead and no target is viable — held, unable to
    /// escape (issue #42).
    ActiveDeadNoTarget,
    /// The active account could not be identified — poll-only.
    SkipActiveUnknown,
    /// The active account's reading was unavailable this cycle — never swap on
    /// missing data.
    SkipActiveUnavailable,
    /// Over the trigger but within the post-swap cooldown (issue #10).
    SkipCooldown,
    /// A swap was attempted but the engine returned an error (#6 no-half-swap).
    SwapFailed,
    /// The keychain was locked — the whole tick was deferred (issue #13).
    KeychainLocked,
}

impl DecisionClass {
    /// The `decision=` token.
    fn as_str(self) -> &'static str {
        match self {
            DecisionClass::Hold => "hold",
            DecisionClass::Swap => "swap",
            DecisionClass::EmergencySwap => "emergency_swap",
            DecisionClass::AllExhausted => "all_exhausted",
            DecisionClass::ActiveDeadNoTarget => "active_dead_no_target",
            DecisionClass::SkipActiveUnknown => "skip_active_unknown",
            DecisionClass::SkipActiveUnavailable => "skip_active_unavailable",
            DecisionClass::SkipCooldown => "skip_cooldown",
            DecisionClass::SwapFailed => "swap_failed",
            DecisionClass::KeychainLocked => "keychain_locked",
        }
    }
}

/// One operator-facing diagnostic line (issue #77), rendered by the single
/// [`Diagnostic::to_log_line`] formatter — the diagnostic channel's redaction
/// surface, the sibling of [`Event::to_log_line`].
///
/// Every field is a HANDLE (an operator label), an enum, a number, or a timestamp —
/// never a token or email (issue #15). That type-level constraint is what lets this
/// channel reuse the event log's redaction guarantee without weakening it: the #15
/// METER scans rendered diagnostics alongside events, and there is no field through
/// which a secret could reach the line.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Diagnostic {
    /// The daemon started: the effective config summary, so one run's lines can be
    /// read against the configuration that produced them. `accounts` is the roster
    /// size; the rest are the swap/poll tunables — counts and percentages only, no
    /// handle.
    Start {
        accounts: usize,
        poll_secs: u64,
        session_floor: Option<u8>,
        session_trigger: u8,
        weekly_trigger: u8,
        monitor_401_n: u8,
        monitor_recovery_m: u8,
    },
    /// The daemon is stopping on a clean shutdown (SIGINT / SIGTERM).
    Stop,
    /// One account's poll outcome this tick: its handle and the outcome class.
    Poll { account: String, outcome: PollClass },
    /// The per-tick decision, plus the back-off wait this tick imposed when any —
    /// the locked-keychain back-off (issue #13) or the rate-limit / transient
    /// back-off (issue #76). `None` ⇒ the field is omitted and the next poll uses
    /// the normal jittered interval.
    Tick {
        decision: DecisionClass,
        backoff_secs: Option<u64>,
    },
    /// The daemon LEFT the all-exhausted terminal state (issue #11): a viable swap
    /// target is possible again. The edge-triggered LEAVE marker — the symmetric
    /// partner of the event log's edge-triggered `all_exhausted` ENTER — so a stale
    /// "all exhausted" reading from an earlier episode can be told from a current
    /// one (the very confusion that motivated #77).
    AllExhaustedCleared,
}

impl Diagnostic {
    /// Render this diagnostic as its single line (no trailing newline), stamped with
    /// `ts`. Pure and the *only* place a diagnostic becomes text, so — exactly like
    /// [`Event::to_log_line`] — the redaction surface is this method alone. `ts` is a
    /// parameter (not read here) so the formatting is deterministically unit-testable;
    /// [`DiagnosticLog::emit`] supplies `SystemTime::now()` at write time.
    pub(crate) fn to_log_line(&self, ts: SystemTime) -> String {
        let ts = rfc3339(ts);
        match self {
            Diagnostic::Start {
                accounts,
                poll_secs,
                session_floor,
                session_trigger,
                weekly_trigger,
                monitor_401_n,
                monitor_recovery_m,
            } => {
                // session_floor is opt-in (#10): render the disabled state as an
                // explicit `off` sentinel rather than omitting the key, so the
                // summary always STATES whether the swap-target session guard is on.
                let session_floor = match session_floor {
                    Some(floor) => floor.to_string(),
                    None => "off".to_owned(),
                };
                format!(
                    "ts={ts} diag=start accounts={accounts} poll_secs={poll_secs} \
                     session_floor={session_floor} session_trigger={session_trigger} \
                     weekly_trigger={weekly_trigger} monitor_401_n={monitor_401_n} \
                     monitor_recovery_m={monitor_recovery_m}"
                )
            }
            Diagnostic::Stop => format!("ts={ts} diag=stop"),
            Diagnostic::Poll { account, outcome } => {
                let outcome = outcome.as_str();
                format!("ts={ts} diag=poll account={account} outcome={outcome}")
            }
            Diagnostic::Tick {
                decision,
                backoff_secs,
            } => {
                let decision = decision.as_str();
                // Omit `backoff_secs` when there is none — an empty value after `=`
                // would split the `key=val` grammar (mirrors `all_exhausted`'s
                // optional `resets_at`).
                match backoff_secs {
                    Some(secs) => {
                        format!("ts={ts} diag=tick decision={decision} backoff_secs={secs}")
                    }
                    None => format!("ts={ts} diag=tick decision={decision}"),
                }
            }
            Diagnostic::AllExhaustedCleared => format!("ts={ts} diag=all_exhausted_cleared"),
        }
    }
}

/// The operator-facing diagnostic SINK (issue #77): writes each [`Diagnostic`] as one
/// line when [`Verbosity::Verbose`], and DROPS every line when [`Verbosity::Quiet`]
/// (the default — no console spam without opt-in). Generic over its `Write` sink:
/// production wires `std::io::stderr()` — the foreground daemon's operator channel,
/// where the lifecycle line and swap echo already go — while tests wire a `Vec<u8>`
/// and read the buffer back.
pub(crate) struct DiagnosticLog<W> {
    sink: W,
    verbosity: Verbosity,
}

impl<W: Write> DiagnosticLog<W> {
    /// Wrap `sink`, emitting only when `verbosity` is [`Verbosity::Verbose`].
    pub(crate) fn new(sink: W, verbosity: Verbosity) -> Self {
        Self { sink, verbosity }
    }

    /// Emit `diag` as one stamped line — unless [`Verbosity::Quiet`], when it is
    /// dropped before any work. Best-effort like the event log: a diagnostic write
    /// failure must never kill the daemon, so a write error is ignored (the
    /// diagnostic channel is a debugging aid, not a durable guarantee).
    pub(crate) fn emit(&mut self, diag: &Diagnostic) {
        if self.verbosity == Verbosity::Quiet {
            return;
        }
        let mut line = diag.to_log_line(SystemTime::now());
        line.push('\n');
        let _ = self.sink.write_all(line.as_bytes());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    /// A fixed wall-clock instant `secs` after the epoch, for deterministic `ts=`.
    fn at_epoch(secs: u64) -> SystemTime {
        UNIX_EPOCH + Duration::from_secs(secs)
    }

    // --- rfc3339 / civil_from_days (the dependency-free date math) ----------

    #[test]
    fn rfc3339_renders_the_epoch_and_a_time_of_day() {
        // Epoch, then a within-day split, then the last second of the first day —
        // pins the H:M:S derivation and the zero-padding of single-digit fields.
        assert_eq!(rfc3339(at_epoch(0)), "1970-01-01T00:00:00Z");
        assert_eq!(rfc3339(at_epoch(3_661)), "1970-01-01T01:01:01Z");
        assert_eq!(rfc3339(at_epoch(86_399)), "1970-01-01T23:59:59Z");
    }

    #[test]
    fn rfc3339_handles_leap_years_and_the_century_rules() {
        // %4 leap year (1972-02-29), the 400-rule leap year (2000-02-29 exists),
        // and the 100-not-400 NON-leap year (2100 has no Feb 29: Feb 28 → Mar 1).
        // Ground truth from macOS `date -u`.
        assert_eq!(rfc3339(at_epoch(68_169_600)), "1972-02-29T00:00:00Z");
        assert_eq!(rfc3339(at_epoch(951_782_400)), "2000-02-29T00:00:00Z");
        assert_eq!(rfc3339(at_epoch(951_868_800)), "2000-03-01T00:00:00Z");
        assert_eq!(rfc3339(at_epoch(4_107_456_000)), "2100-02-28T00:00:00Z");
        assert_eq!(rfc3339(at_epoch(4_107_542_400)), "2100-03-01T00:00:00Z");
    }

    #[test]
    fn rfc3339_renders_recent_and_far_future_instants() {
        assert_eq!(rfc3339(at_epoch(1_735_689_600)), "2025-01-01T00:00:00Z");
        assert_eq!(rfc3339(at_epoch(1_750_960_800)), "2025-06-26T18:00:00Z");
        // The largest 4-digit year — proves the `{year:04}` width holds at the top.
        assert_eq!(rfc3339(at_epoch(253_402_300_799)), "9999-12-31T23:59:59Z");
    }

    #[test]
    fn rfc3339_treats_a_pre_epoch_clock_as_the_epoch_sentinel() {
        // A clock set before 1970 yields a `duration_since` error; rather than
        // panic a best-effort log write, it renders the epoch sentinel.
        let before = UNIX_EPOCH - Duration::from_secs(1);
        assert_eq!(rfc3339(before), "1970-01-01T00:00:00Z");
    }

    // --- Event::to_log_line (the single redaction surface) ------------------

    const TS0: &str = "ts=1970-01-01T00:00:00Z";

    #[test]
    fn swap_line_carries_handles_reason_and_session_pct() {
        let line = Event::Swap {
            from: "work".to_owned(),
            to: "spare".to_owned(),
            reason: SwapReason::Session,
            session_pct: 97,
        }
        .to_log_line(at_epoch(0));
        assert_eq!(
            line,
            format!("{TS0} event=swap from=work to=spare reason=session session_pct=97")
        );
    }

    #[test]
    fn swap_line_renders_the_weekly_reason() {
        let line = Event::Swap {
            from: "work".to_owned(),
            to: "spare".to_owned(),
            reason: SwapReason::Weekly,
            session_pct: 40,
        }
        .to_log_line(at_epoch(0));
        assert!(line.contains("reason=weekly"), "got: {line}");
    }

    #[test]
    fn all_exhausted_renders_resets_at_when_known_and_omits_it_otherwise() {
        // No reset reported (#11 fallback) → the field is simply absent and the
        // line stays well-formed.
        let absent = Event::AllExhausted {
            hold: "work".to_owned(),
            resets_at: None,
        }
        .to_log_line(at_epoch(0));
        assert_eq!(absent, format!("{TS0} event=all_exhausted hold=work"));
        assert!(!absent.contains("resets_at"), "got: {absent}");

        // A known reset (epoch seconds, #11) is rendered to RFC 3339 by the same
        // single formatter — 1_782_777_600 is 2026-06-30T00:00:00Z.
        let present = Event::AllExhausted {
            hold: "work".to_owned(),
            resets_at: Some(1_782_777_600),
        }
        .to_log_line(at_epoch(0));
        assert_eq!(
            present,
            format!("{TS0} event=all_exhausted hold=work resets_at=2026-06-30T00:00:00Z")
        );
    }

    #[test]
    fn monitor_401_carries_the_account_and_consecutive_count() {
        let line = Event::Monitor401 {
            account: "work".to_owned(),
            consecutive: 3,
        }
        .to_log_line(at_epoch(0));
        assert_eq!(
            line,
            format!("{TS0} event=monitor_401 account=work consecutive=3")
        );
    }

    #[test]
    fn credential_dead_carries_only_the_account_handle() {
        let line = Event::CredentialDead {
            account: "work".to_owned(),
        }
        .to_log_line(at_epoch(0));
        assert_eq!(line, format!("{TS0} event=credential_dead account=work"));
    }

    #[test]
    fn emergency_swap_carries_the_from_and_to_handles() {
        let line = Event::EmergencySwap {
            from: "work".to_owned(),
            to: "spare".to_owned(),
        }
        .to_log_line(at_epoch(0));
        assert_eq!(
            line,
            format!("{TS0} event=emergency_swap from=work to=spare")
        );
    }

    #[test]
    fn credential_restored_carries_only_the_account_handle() {
        let line = Event::CredentialRestored {
            account: "work".to_owned(),
        }
        .to_log_line(at_epoch(0));
        assert_eq!(
            line,
            format!("{TS0} event=credential_restored account=work")
        );
    }

    #[test]
    fn restash_carries_the_account_handle() {
        let line = Event::ReStash {
            account: "work".to_owned(),
        }
        .to_log_line(at_epoch(0));
        assert_eq!(line, format!("{TS0} event=restash account=work"));
    }

    #[test]
    fn keychain_locked_wait_is_accountless() {
        // A locked keychain is process-global, so the line carries no account —
        // just the event name and timestamp (issue #13).
        let line = Event::KeychainLockedWait.to_log_line(at_epoch(0));
        assert_eq!(line, format!("{TS0} event=keychain_locked_wait"));
    }

    #[test]
    fn usage_scope_fail_carries_the_account_and_constant_403() {
        let line = Event::UsageScopeFail {
            account: "work".to_owned(),
        }
        .to_log_line(at_epoch(0));
        assert_eq!(
            line,
            format!("{TS0} event=usage_scope_fail account=work status=403")
        );
    }

    #[test]
    fn no_event_line_carries_an_email_or_token_sigil() {
        // #15: every field is a handle / enum / number / timestamp, so a token or
        // email can never reach a rendered line. Handles here are plain labels.
        let events = [
            Event::Swap {
                from: "work".to_owned(),
                to: "spare".to_owned(),
                reason: SwapReason::Session,
                session_pct: 97,
            },
            Event::AllExhausted {
                hold: "work".to_owned(),
                resets_at: Some(1_782_777_600),
            },
            Event::ReStash {
                account: "work".to_owned(),
            },
            Event::Monitor401 {
                account: "work".to_owned(),
                consecutive: 2,
            },
            Event::CredentialDead {
                account: "work".to_owned(),
            },
            Event::EmergencySwap {
                from: "work".to_owned(),
                to: "spare".to_owned(),
            },
            Event::CredentialRestored {
                account: "work".to_owned(),
            },
            Event::KeychainLockedWait,
            Event::UsageScopeFail {
                account: "work".to_owned(),
            },
        ];
        for event in &events {
            let line = event.to_log_line(at_epoch(0));
            assert!(!line.contains('@'), "no email sigil: {line}");
            assert!(!line.to_lowercase().contains("token"), "no token: {line}");
            // Exactly one line — no embedded newline could split or forge a record.
            assert_eq!(line.lines().count(), 1, "single line: {line}");
        }
    }

    // --- EventLog (the sink) -----------------------------------------------

    #[test]
    fn emit_appends_one_stamped_line_per_event() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sessiometer.log");
        let mut log = EventLog::at(&path).unwrap();

        log.emit(&Event::Swap {
            from: "work".to_owned(),
            to: "spare".to_owned(),
            reason: SwapReason::Session,
            session_pct: 97,
        })
        .unwrap();
        log.emit(&Event::Monitor401 {
            account: "spare".to_owned(),
            consecutive: 1,
        })
        .unwrap();

        let logged = std::fs::read_to_string(&path).unwrap();
        // One line per event, each carrying its own `ts=` and `event=` keys.
        assert_eq!(logged.lines().count(), 2, "got: {logged:?}");
        assert!(logged.contains("event=swap from=work to=spare"));
        assert!(logged.contains("event=monitor_401 account=spare consecutive=1"));
        for line in logged.lines() {
            assert!(line.starts_with("ts="), "every line is stamped: {line:?}");
        }
        assert!(!logged.contains('@'));
    }

    #[test]
    fn the_log_file_is_created_private() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sessiometer.log");
        let _log = EventLog::at(&path).unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }

    // --- the `use` verb's new swap reasons + cooldown source (issue #63) -----

    #[test]
    fn swap_line_renders_the_manual_and_forced_reasons() {
        // The operator-driven `use` verb emits the STANDARD swap event with the new
        // reason tokens; a manual swap records session_pct=0 (not session-triggered
        // — the reason is what distinguishes it).
        for (reason, token) in [
            (SwapReason::Manual, "manual"),
            (SwapReason::Forced, "forced"),
        ] {
            let line = Event::Swap {
                from: "work".to_owned(),
                to: "spare".to_owned(),
                reason,
                session_pct: 0,
            }
            .to_log_line(at_epoch(0));
            assert_eq!(
                line,
                format!("{TS0} event=swap from=work to=spare reason={token} session_pct=0")
            );
        }
    }

    #[test]
    fn last_swap_at_is_none_for_an_absent_or_swapless_log() {
        let dir = tempfile::tempdir().unwrap();
        // Absent file → None (best-effort: a one-shot swap is never blocked by a
        // missing log; the cooldown then reads as inactive).
        assert_eq!(last_swap_at(&dir.path().join("absent.log")), None);
        // A present log with NO swap line → None.
        let path = dir.path().join("sessiometer.log");
        let mut log = EventLog::at(&path).unwrap();
        log.emit(&Event::Monitor401 {
            account: "work".to_owned(),
            consecutive: 1,
        })
        .unwrap();
        log.emit(&Event::KeychainLockedWait).unwrap();
        assert_eq!(last_swap_at(&path), None);
    }

    #[test]
    fn last_swap_at_returns_the_most_recent_swap_instant() {
        // The log is append-only chronological; `last_swap_at` returns the LAST
        // swap's `ts`, parsed back through the same RFC 3339 the writer rendered.
        // A manual `reason=manual` swap (#63) and an `emergency_swap` both count;
        // a later NON-swap line (monitor_401) is ignored. Hand-written so the
        // instants are deterministic (`emit` stamps with the live clock).
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sessiometer.log");
        let content = "\
ts=1970-01-01T00:00:10Z event=swap from=a to=b reason=session session_pct=97\n\
ts=1970-01-01T00:00:30Z event=swap from=b to=c reason=manual session_pct=0\n\
ts=1970-01-01T00:00:40Z event=monitor_401 account=c consecutive=1\n";
        std::fs::write(&path, content).unwrap();
        // The most recent SWAP line is the manual swap at epoch 30 — not the later
        // monitor_401 line, and not the earlier session swap at epoch 10.
        assert_eq!(last_swap_at(&path), Some(at_epoch(30)));

        // An `emergency_swap` is also a swap for cooldown purposes.
        std::fs::write(
            &path,
            "ts=1970-01-01T00:01:00Z event=emergency_swap from=a to=b\n",
        )
        .unwrap();
        assert_eq!(last_swap_at(&path), Some(at_epoch(60)));
    }

    // --- Diagnostic::to_log_line (the diagnostic channel's redaction surface, #77) ---

    #[test]
    fn start_line_renders_the_effective_config_summary() {
        // session_floor present → its percent; the rest are counts/percentages.
        let on = Diagnostic::Start {
            accounts: 3,
            poll_secs: 30,
            session_floor: Some(70),
            session_trigger: 90,
            weekly_trigger: 98,
            monitor_401_n: 5,
            monitor_recovery_m: 4,
        }
        .to_log_line(at_epoch(0));
        assert_eq!(
            on,
            format!(
                "{TS0} diag=start accounts=3 poll_secs=30 session_floor=70 \
                 session_trigger=90 weekly_trigger=98 monitor_401_n=5 monitor_recovery_m=4"
            )
        );

        // session_floor absent → the explicit `off` sentinel (the guard is disabled,
        // #10), never an empty value that would split the key=val grammar.
        let off = Diagnostic::Start {
            accounts: 1,
            poll_secs: 60,
            session_floor: None,
            session_trigger: 80,
            weekly_trigger: 95,
            monitor_401_n: 3,
            monitor_recovery_m: 2,
        }
        .to_log_line(at_epoch(0));
        assert!(off.contains("session_floor=off"), "got: {off}");
    }

    #[test]
    fn stop_line_is_bare() {
        assert_eq!(
            Diagnostic::Stop.to_log_line(at_epoch(0)),
            format!("{TS0} diag=stop")
        );
    }

    #[test]
    fn poll_line_carries_the_handle_and_each_outcome_class() {
        // The 5-way diagnostic taxonomy — rate_limited is SEPARATE from transient.
        for (outcome, token) in [
            (PollClass::Live, "live"),
            (PollClass::Unauthorized, "unauthorized"),
            (PollClass::Scope, "scope"),
            (PollClass::RateLimited, "rate_limited"),
            (PollClass::Transient, "transient"),
        ] {
            let line = Diagnostic::Poll {
                account: "work".to_owned(),
                outcome,
            }
            .to_log_line(at_epoch(0));
            assert_eq!(
                line,
                format!("{TS0} diag=poll account=work outcome={token}")
            );
        }
    }

    #[test]
    fn tick_line_renders_the_decision_and_omits_backoff_when_absent() {
        // No back-off → the field is simply absent (the line stays well-formed).
        let held = Diagnostic::Tick {
            decision: DecisionClass::Hold,
            backoff_secs: None,
        }
        .to_log_line(at_epoch(0));
        assert_eq!(held, format!("{TS0} diag=tick decision=hold"));
        assert!(!held.contains("backoff_secs"), "got: {held}");

        // A back-off (#13 / #76) → the wait in whole seconds.
        let backed_off = Diagnostic::Tick {
            decision: DecisionClass::KeychainLocked,
            backoff_secs: Some(8),
        }
        .to_log_line(at_epoch(0));
        assert_eq!(
            backed_off,
            format!("{TS0} diag=tick decision=keychain_locked backoff_secs=8")
        );
    }

    #[test]
    fn every_decision_class_renders_its_token() {
        // One token per TickAction (the map is exhaustive, #77).
        for (decision, token) in [
            (DecisionClass::Hold, "hold"),
            (DecisionClass::Swap, "swap"),
            (DecisionClass::EmergencySwap, "emergency_swap"),
            (DecisionClass::AllExhausted, "all_exhausted"),
            (DecisionClass::ActiveDeadNoTarget, "active_dead_no_target"),
            (DecisionClass::SkipActiveUnknown, "skip_active_unknown"),
            (
                DecisionClass::SkipActiveUnavailable,
                "skip_active_unavailable",
            ),
            (DecisionClass::SkipCooldown, "skip_cooldown"),
            (DecisionClass::SwapFailed, "swap_failed"),
            (DecisionClass::KeychainLocked, "keychain_locked"),
        ] {
            let line = Diagnostic::Tick {
                decision,
                backoff_secs: None,
            }
            .to_log_line(at_epoch(0));
            assert_eq!(line, format!("{TS0} diag=tick decision={token}"));
        }
    }

    #[test]
    fn all_exhausted_cleared_line_is_bare() {
        assert_eq!(
            Diagnostic::AllExhaustedCleared.to_log_line(at_epoch(0)),
            format!("{TS0} diag=all_exhausted_cleared")
        );
    }

    #[test]
    fn no_diagnostic_line_carries_an_email_or_token_sigil() {
        // #15: every diagnostic field is a handle / enum / number / timestamp, so a
        // token or email can never reach a rendered line. Mirrors the event-log guard.
        let diags = [
            Diagnostic::Start {
                accounts: 2,
                poll_secs: 30,
                session_floor: Some(70),
                session_trigger: 90,
                weekly_trigger: 98,
                monitor_401_n: 5,
                monitor_recovery_m: 4,
            },
            Diagnostic::Stop,
            Diagnostic::Poll {
                account: "work".to_owned(),
                outcome: PollClass::RateLimited,
            },
            Diagnostic::Tick {
                decision: DecisionClass::Swap,
                backoff_secs: Some(16),
            },
            Diagnostic::AllExhaustedCleared,
        ];
        for diag in &diags {
            let line = diag.to_log_line(at_epoch(0));
            assert!(!line.contains('@'), "no email sigil: {line}");
            assert!(!line.to_lowercase().contains("token"), "no token: {line}");
            assert_eq!(line.lines().count(), 1, "single line: {line}");
        }
    }

    // --- DiagnosticLog (the verbosity-gated sink, #77) ----------------------

    #[test]
    fn diagnostic_log_is_silent_when_quiet() {
        // Default QUIET → nothing reaches the sink (no console spam without opt-in).
        let mut log = DiagnosticLog::new(Vec::<u8>::new(), Verbosity::Quiet);
        log.emit(&Diagnostic::Stop);
        log.emit(&Diagnostic::Poll {
            account: "work".to_owned(),
            outcome: PollClass::Live,
        });
        assert!(
            log.sink.is_empty(),
            "quiet must emit nothing: {:?}",
            log.sink
        );
    }

    #[test]
    fn diagnostic_log_emits_one_line_per_diagnostic_when_verbose() {
        let mut log = DiagnosticLog::new(Vec::<u8>::new(), Verbosity::Verbose);
        log.emit(&Diagnostic::Poll {
            account: "work".to_owned(),
            outcome: PollClass::RateLimited,
        });
        log.emit(&Diagnostic::Tick {
            decision: DecisionClass::Hold,
            backoff_secs: None,
        });
        let out = String::from_utf8(log.sink).unwrap();
        assert_eq!(out.lines().count(), 2, "one line per emit: {out:?}");
        assert!(out.contains("diag=poll account=work outcome=rate_limited"));
        assert!(out.contains("diag=tick decision=hold"));
        // Each line is stamped and newline-terminated.
        assert!(out.ends_with('\n'));
        for line in out.lines() {
            assert!(line.starts_with("ts="), "stamped: {line:?}");
        }
    }
}
