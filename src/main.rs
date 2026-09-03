// Copyright (c) 2026 Oleksii PELYKH
// SPDX-License-Identifier: MIT

//! `sessiometer` — manage multiple Claude Code accounts on macOS.
//!
//! A daemon-monolith: a foreground single process that polls each account's
//! usage quota and swaps the active credential out-of-band before exhaustion.
//! This binary wires the runtime and dispatches CLI subcommands; the behavior
//! of each subsystem is filled in by later work items (see the module docs and
//! the `0.1.0` milestone issues).

mod active;
mod canary;
mod capture;
mod cc_version;
mod claude_state;
mod cli;
mod config;
mod contract;
/// The CROSS-SURFACE severity contract (issue #768) — the committed manifest pinning the ADR-0026
/// daemon-payload-fault rank across the `status` CLI and the menubar panel, plus the divergence
/// predicate and the mutation canary both gates run. Test-only: nothing in the shipping binary
/// reads the manifest.
#[cfg(test)]
mod cross_surface;
mod daemon;
mod duration;
mod error;
/// The FRAMING guard's shared vocabulary (issues #160, #542, #918, #1123, #1139) — the central
/// banned-token and banned-phrase lists, the single scanner all five audiences share, and the
/// per-audience exemption sets that let `--help`, the operator advisories and `Error::CliUsage`'s
/// usage hints name this CLI's own verbs. The fifth audience — `Error`'s authored `#[error(...)]`
/// templates (#1139) — has no exemption set and scans the whole list; its carve-outs are
/// per-variant, in `src/error.rs`'s `ERROR_PROSE_LEDGER`. Test-only: nothing in the shipping
/// binary reads the vocabulary.
#[cfg(test)]
mod framing_vocabulary;
mod hex;
mod isolated_spawn;
mod keychain;
mod landing;
mod log;
mod login;
mod migration;
mod observability;
mod paths;
mod percentile;
mod poke;
mod redaction;
mod refresh;
mod refresh_tick;
mod reliability;
/// Shared full-output render-golden machinery (issue #767) — test-only: nothing in the
/// shipping binary reads a golden, so it is not compiled into one.
#[cfg(test)]
mod render_golden;
/// The roster backup ring (issue #1439) — the qualifying-write rule that decides which
/// replaced `config.toml` enters a fixed three-deep private ring, plus the enumeration and
/// restore path behind `config backups` / `config restore`.
mod roster_backup;
mod service;
mod sha256;
mod stash;
mod stats;
mod swap;
mod systemic_refresh;
mod timing;
mod usage;
mod usage_stats;
mod usage_store;
mod use_account;
mod witness;

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
