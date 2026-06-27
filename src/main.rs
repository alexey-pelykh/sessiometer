// Copyright (c) 2026 Oleksii PELYKH
// SPDX-License-Identifier: MIT

//! `sessiometer` — manage multiple Claude Code accounts on macOS.
//!
//! A daemon-monolith: a foreground single process that polls each account's
//! usage quota and swaps the active credential out-of-band before exhaustion.
//! This binary wires the runtime and dispatches CLI subcommands; the behavior
//! of each subsystem is filled in by later work items (see the module docs and
//! the `0.1.0` milestone issues).

mod capture;
mod claude_state;
mod cli;
mod config;
mod daemon;
mod error;
mod keychain;
mod observability;
mod paths;
mod stash;
mod swap;
mod usage;

use std::process::ExitCode;

/// Foreground single process on a **current-thread** Tokio runtime.
///
/// The current-thread flavor keeps the async seams free of `Send` bounds (see
/// [`daemon`]); that is what lets the whole poll loop be exercised hermetically
/// in tests against in-memory fakes.
#[tokio::main(flavor = "current_thread")]
async fn main() -> ExitCode {
    match cli::dispatch(std::env::args_os()).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            // `Error` never carries secret material, so this is safe to print.
            eprintln!("sessiometer: {err}");
            // A held single-instance lock exits `3`; every other error exits `1`
            // (issue #7, via `Error::exit_code`).
            ExitCode::from(err.exit_code())
        }
    }
}
