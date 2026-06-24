// Copyright (c) 2026 Oleksii PELYKH
// SPDX-License-Identifier: MIT

//! Local event log.
//!
//! Scaffolding scope: an append-only `0600` log file that records each poll
//! outcome. Structured events and the `last_swap` shown by `status` land in
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

    /// Append one poll outcome to the log.
    pub(crate) fn record(&mut self, outcome: &TickOutcome) -> Result<()> {
        writeln!(
            self.file,
            "tick={} at={:?} session={:.4} weekly={:.4} decision={:?}",
            outcome.tick, outcome.at, outcome.usage.session, outcome.usage.weekly, outcome.decision,
        )?;
        Ok(())
    }
}
