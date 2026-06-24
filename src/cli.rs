// Copyright (c) 2026 Oleksii PELYKH
// SPDX-License-Identifier: MIT

//! Command-line frontend.
//!
//! Scaffolding scope: subcommand dispatch and the wiring of the foreground
//! `run` loop against the **real** seams. A real argument parser and the full
//! `capture` / `status` / `list` behaviors land in issues #8, #4, #9 and #17.

use crate::config::Config;
use crate::daemon::{Daemon, RealClock};
use crate::error::{Error, Result};
use crate::keychain::RealCredentialStore;
use crate::observability::EventLog;
use crate::paths;
use crate::usage::RealUsageSource;

/// Parse `argv` and run the requested subcommand.
pub(crate) async fn dispatch(args: std::env::ArgsOs) -> Result<()> {
    let mut args = args.skip(1); // skip argv[0]
    match args.next() {
        None => {
            print_usage();
            Ok(())
        }
        Some(cmd) => {
            let name = cmd.to_string_lossy();
            match name.as_ref() {
                "capture" => capture().await,
                "run" => run().await,
                "status" => status().await,
                "list" => list().await,
                "-h" | "--help" => {
                    print_usage();
                    Ok(())
                }
                other => Err(Error::UnknownCommand(other.to_owned())),
            }
        }
    }
}

fn print_usage() {
    println!(
        "sessiometer — manage multiple Claude Code accounts on macOS\n\
         \n\
         USAGE:\n    \
         sessiometer <COMMAND>\n\
         \n\
         COMMANDS:\n    \
         capture    Stash the current account's credential\n    \
         run        Run the foreground daemon (poll + swap)\n    \
         status     Show the roster and the last swap\n    \
         list       List captured accounts\n    \
         --help     Print this help"
    );
}

/// Stash the active account's credential. Lands in issue #4.
async fn capture() -> Result<()> {
    Err(Error::Unimplemented("account capture (#4)"))
}

/// Foreground daemon: poll usage and swap before exhaustion.
///
/// Wires the **real** seams into the generic [`Daemon`] and drives the loop.
/// The subsystems behind the seams are stubbed until their issues land, so in
/// the current scaffold the first poll returns an `Unimplemented` error.
async fn run() -> Result<()> {
    // `load` is a stub today (returns `Unimplemented`), so `default` is the
    // intended fallback. When #3 lands real loading, keep `default` only for
    // the "no config file" case and surface real I/O / parse errors instead of
    // swallowing them here.
    let config = Config::load().unwrap_or_default();

    paths::ensure_private_dir(&paths::config_dir()?)?;
    paths::ensure_private_dir(&paths::logs_dir()?)?;
    let mut log = EventLog::open()?;

    let mut daemon = Daemon::new(
        RealUsageSource,
        RealCredentialStore,
        RealClock::new(config.poll_interval),
        config.swap_threshold,
    );

    loop {
        let outcome = daemon.tick().await?;
        log.record(&outcome)?;
        daemon.wait_for_next_poll().await;
    }
}

/// Show the account roster and the last swap. Lands in issue #9.
async fn status() -> Result<()> {
    Err(Error::Unimplemented("status (#9)"))
}

/// List captured accounts. Lands in issue #17.
async fn list() -> Result<()> {
    Err(Error::Unimplemented("list (#17)"))
}
