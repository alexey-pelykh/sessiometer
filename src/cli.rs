// Copyright (c) 2026 Oleksii PELYKH
// SPDX-License-Identifier: MIT

//! Command-line frontend.
//!
//! Scaffolding scope: subcommand dispatch and the wiring of the foreground
//! `run` loop against the **real** seams. A real argument parser lands in issue
//! #8; the live `status` view lands in #9.

use crate::config::{Account, Config, MAX_ACCOUNTS};
use crate::daemon::{Daemon, RealClock};
use crate::error::{Error, Result};
use crate::keychain::RealCredentialStore;
use crate::observability::EventLog;
use crate::paths;
use crate::usage::{CurlTransport, NoopReStashTrigger, RealUsageSource};

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
                "capture" => {
                    // Optional positional label; the remainder (if any) is ignored,
                    // matching the other subcommands.
                    let label = args.next().map(|s| s.to_string_lossy().into_owned());
                    crate::capture::capture(label).await
                }
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
         capture [<label>]    Stash the active account into the rotation\n    \
         run        Run the foreground daemon (poll + swap)\n    \
         status     Show the roster and the last swap\n    \
         list       List captured accounts\n    \
         --help     Print this help"
    );
}

/// Foreground daemon: poll usage and swap before exhaustion.
///
/// Wires the **real** seams into the generic [`Daemon`] and drives the loop.
/// The subsystems behind the seams are stubbed until their issues land, so in
/// the current scaffold the first poll returns an `Unimplemented` error.
async fn run() -> Result<()> {
    // Load the real config (roster + tunables) and surface any I/O / parse /
    // validation error: a malformed or absent config is fatal, not silently
    // replaced by defaults (issue #3). The roster itself is wired into the swap
    // engine in #6 / #7; here we consume the tunables the loop already needs.
    let config = Config::load()?;

    paths::ensure_private_dir(&paths::config_dir()?)?;
    paths::ensure_private_dir(&paths::logs_dir()?)?;
    let mut log = EventLog::open()?;

    // The usage poller reads the active account's bearer from the canonical
    // keychain item and polls behind its own transport seam; per-account polling
    // across the roster is the swap engine's job (#6 / #7), which constructs
    // stash-backed sources. The re-stash trigger is a no-op here — acting on a
    // rejected token lands in #13 / #6.
    let mut daemon = Daemon::new(
        RealUsageSource::new(
            CurlTransport::new(RealCredentialStore::new()),
            NoopReStashTrigger,
            config.tunables.monitor_401_n,
        ),
        RealCredentialStore::new(),
        RealClock::new(config.poll_interval()),
        config.swap_threshold(),
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

/// List captured accounts — the offline, read-only roster view (issue #17).
///
/// Reads `config.toml` and nothing else: no daemon, no keychain, no network (the
/// static counterpart to `status`, which needs a live `run`). An absent config is
/// the empty state, surfaced as the friendly [`Error::RosterEmpty`]; a malformed
/// config still surfaces as its real parse/validation error. The output is
/// sourced solely from the roster's non-secret fields, so it can never print a
/// token or email (issue #15 redaction).
async fn list() -> Result<()> {
    print!("{}", view(Config::load())?);
    Ok(())
}

/// Resolve a load outcome into the text `list` prints, or the error it exits on.
///
/// Split from [`list`] so the load-outcome → output mapping is unit-testable
/// without touching the filesystem: a present roster renders; an absent config
/// ([`Error::ConfigNotFound`]) becomes the friendly [`Error::RosterEmpty`]; every
/// other load error (malformed / invalid config) surfaces unchanged.
fn view(loaded: Result<Config>) -> Result<String> {
    match loaded {
        Ok(config) => Ok(render_roster(&config.roster)),
        Err(Error::ConfigNotFound { .. }) => Err(Error::RosterEmpty),
        Err(other) => Err(other),
    }
}

/// Render the roster as one `label · uuid · stash` row per account, then a
/// `N of {MAX_ACCOUNTS} slots used` total.
///
/// Sourced solely from each [`Account`]'s non-secret fields (label, short
/// `account_uuid`, stash) — never a token or email (issue #15 redaction).
fn render_roster(roster: &[Account]) -> String {
    let mut out = String::new();
    for account in roster {
        out.push_str(&format!(
            "{} · {} · {}\n",
            account.label,
            short_uuid(&account.account_uuid),
            account.stash,
        ));
    }
    out.push_str(&format!(
        "\n{} of {} slots used\n",
        roster.len(),
        MAX_ACCOUNTS,
    ));
    out
}

/// The display-friendly short form of an `account_uuid`: its first segment (the
/// leading 8 characters of a canonical UUID), or the whole value when shorter.
/// Char-boundary safe — the field is an arbitrary, validated-non-empty string.
fn short_uuid(account_uuid: &str) -> &str {
    match account_uuid.char_indices().nth(8) {
        Some((boundary, _)) => &account_uuid[..boundary],
        None => account_uuid,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Tunables;
    use std::path::PathBuf;

    fn acct(label: &str, uuid: &str, stash: &str) -> Account {
        Account {
            account_uuid: uuid.to_owned(),
            stash: stash.to_owned(),
            label: label.to_owned(),
        }
    }

    /// A `Config` around `roster`, with placeholder tunables `list` never reads.
    fn config_with(roster: Vec<Account>) -> Config {
        Config {
            roster,
            tunables: Tunables {
                poll_secs: 60,
                cooldown_secs: 60,
                session_floor: 80,
                session_trigger: 95,
                monitor_401_n: 3,
            },
        }
    }

    #[test]
    fn renders_each_account_then_the_slot_total() {
        let out = render_roster(&[
            acct(
                "work",
                "11111111-1111-1111-1111-111111111111",
                "Sessiometer/acct-1",
            ),
            acct(
                "personal",
                "22222222-2222-2222-2222-222222222222",
                "Sessiometer/acct-2",
            ),
        ]);
        assert_eq!(
            out,
            "work · 11111111 · Sessiometer/acct-1\n\
personal · 22222222 · Sessiometer/acct-2\n\
\n\
2 of 5 slots used\n"
        );
    }

    #[test]
    fn total_counts_against_max_accounts_not_just_listed_rows() {
        let out = render_roster(&[acct(
            "solo",
            "abcdef00-0000-0000-0000-000000000000",
            "Sessiometer/acct-1",
        )]);
        assert!(out.ends_with("1 of 5 slots used\n"), "got: {out:?}");
    }

    #[test]
    fn short_uuid_takes_the_leading_eight_chars_of_a_canonical_uuid() {
        assert_eq!(
            short_uuid("11111111-2222-3333-4444-555555555555"),
            "11111111"
        );
    }

    #[test]
    fn short_uuid_returns_a_shorter_or_eight_char_value_whole() {
        assert_eq!(short_uuid("u"), "u");
        assert_eq!(short_uuid("12345678"), "12345678");
    }

    #[test]
    fn view_renders_a_present_roster() {
        let config = config_with(vec![acct("work", "11111111-aaaa", "Sessiometer/acct-1")]);
        let out = view(Ok(config)).expect("a present roster is not an error");
        assert_eq!(
            out,
            "work · 11111111 · Sessiometer/acct-1\n\n1 of 5 slots used\n"
        );
    }

    #[test]
    fn view_maps_an_absent_config_to_the_friendly_empty_state() {
        let loaded = Err(Error::ConfigNotFound {
            path: PathBuf::from("/nonexistent/config.toml"),
        });
        assert!(
            matches!(view(loaded), Err(Error::RosterEmpty)),
            "an absent config must become the friendly empty state"
        );
        // The friendly message points at the next step and never leaks the path.
        assert_eq!(
            Error::RosterEmpty.to_string(),
            "no accounts captured yet — run `sessiometer capture`"
        );
    }

    #[test]
    fn view_does_not_conflate_a_malformed_config_with_the_empty_state() {
        let loaded = Err(Error::ConfigParse("expected `=`".into()));
        assert!(
            matches!(view(loaded), Err(Error::ConfigParse(_))),
            "a malformed config must surface as its real error, not the empty state"
        );
    }

    #[test]
    fn output_never_carries_an_email_or_token_sigil() {
        // #15 redaction: the formatter sources only the three non-secret roster
        // fields, so a token / email can never reach the printed surface.
        let out = render_roster(&[acct(
            "work",
            "11111111-1111-1111-1111-111111111111",
            "Sessiometer/acct-1",
        )]);
        assert!(
            !out.contains('@'),
            "list output must not contain an email: {out:?}"
        );
    }
}
