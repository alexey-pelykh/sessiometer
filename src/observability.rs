// Copyright (c) 2026 Oleksii PELYKH
// SPDX-License-Identifier: MIT

//! Local event log.
//!
//! Scaffolding scope: an append-only `0600` log file that records one line per
//! poll — the tick number, the decision ([`crate::daemon::TickAction`]), and each
//! account's non-secret reading (label + percentages, never a token or email;
//! issue #15). Structured events and the `last_swap` shown by `status` land in
//! issue #9.

use std::fs::File;
use std::io::Write;

use crate::daemon::TickOutcome;
use crate::error::Result;
use crate::paths;

/// An append-only event log at `~/Library/Logs/sessiometer/events.log` (`0600`).
pub(crate) struct EventLog {
    file: File,
}

impl EventLog {
    /// Open the event log, creating the log directory (`0700`) and file
    /// (`0600`) if needed.
    pub(crate) fn open() -> Result<Self> {
        let dir = paths::logs_dir()?;
        paths::ensure_private_dir(&dir)?;
        let file = paths::create_private_file(&dir.join("events.log"))?;
        Ok(Self { file })
    }

    /// Append one poll outcome: the tick, the decision, then each account's
    /// non-secret reading. An unavailable reading is logged `n/a` (never a
    /// fabricated `0`); the active account is marked `*`. Sourced solely from
    /// labels + percentages, so the log can never carry a token or email (#15).
    pub(crate) fn record(&mut self, outcome: &TickOutcome) -> Result<()> {
        write!(
            self.file,
            "tick={} at={:?} action={:?}",
            outcome.tick, outcome.at, outcome.action,
        )?;
        for account in &outcome.snapshot.accounts {
            let marker = if account.active { "*" } else { "" };
            match account.usage {
                Some(usage) => write!(
                    self.file,
                    " {}{}=session:{:.4}/weekly:{:.4}",
                    account.label, marker, usage.session, usage.weekly,
                )?,
                None => write!(self.file, " {}{}=n/a", account.label, marker)?,
            }
        }
        writeln!(self.file)?;
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
    use crate::daemon::{AccountReading, StatusSnapshot, TickAction, TickOutcome};
    use crate::usage::Usage;
    use std::time::Instant;

    #[test]
    fn record_writes_a_nonsecret_line_per_tick() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("events.log");
        let mut log = EventLog::at(&path).unwrap();

        log.record(&TickOutcome {
            tick: 1,
            at: Instant::now(),
            action: TickAction::Swapped { from: 0, to: 1 },
            snapshot: StatusSnapshot {
                accounts: vec![
                    AccountReading {
                        label: "work".to_owned(),
                        active: true,
                        usage: Some(Usage {
                            session: 0.97,
                            weekly: 0.40,
                        }),
                    },
                    AccountReading {
                        label: "spare".to_owned(),
                        active: false,
                        usage: None,
                    },
                ],
            },
        })
        .unwrap();

        let logged = std::fs::read_to_string(&path).unwrap();
        assert!(logged.contains("tick=1"), "got: {logged:?}");
        assert!(logged.contains("Swapped"));
        assert!(
            logged.contains("work*"),
            "active account is marked: {logged:?}"
        );
        assert!(
            logged.contains("n/a"),
            "the unavailable account: {logged:?}"
        );
        // #15: the log sources only labels + percentages — no email/token sigil.
        assert!(!logged.contains('@'));
    }
}
