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

use std::fs::File;
use std::io::Write;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::error::Result;
use crate::paths;

/// Which usage dimension tripped its swap-away trigger this cycle — the `reason=`
/// of an [`Event::Swap`].
///
/// Re-derived at swap time from the readings (the binary [`crate::swap::decide`]
/// does not carry which dimension fired); when BOTH dimensions are at/over their
/// triggers, the daemon reports [`SwapReason::Session`] — session-first precedence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SwapReason {
    /// The session-window trigger fired (or both did — session takes precedence).
    Session,
    /// The weekly-window trigger fired while session was below its own.
    Weekly,
}

impl SwapReason {
    /// The `reason=` token.
    fn as_str(self) -> &'static str {
        match self {
            SwapReason::Session => "session",
            SwapReason::Weekly => "weekly",
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
    /// row. Observability only — a persistent 401 means a dead credential, whose
    /// quarantine + emergency swap is issue #42 (NOT a re-stash; re-stash is driven
    /// by canonical-change detection, [`Event::ReStash`]).
    Monitor401 { account: String, consecutive: u32 },
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

/// The structured event log at `~/Library/Logs/sessiometer/sessiometer.log`
/// (`0600`).
pub(crate) struct EventLog {
    file: File,
}

impl EventLog {
    /// Open the event log, creating the log directory (`0700`) and file (`0600`)
    /// if needed.
    pub(crate) fn open() -> Result<Self> {
        let dir = paths::logs_dir()?;
        paths::ensure_private_dir(&dir)?;
        let file = paths::create_private_file(&dir.join("sessiometer.log"))?;
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
}
