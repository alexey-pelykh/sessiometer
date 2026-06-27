// Copyright (c) 2026 Oleksii PELYKH
// SPDX-License-Identifier: MIT

//! Command-line frontend.
//!
//! Scaffolding scope: subcommand dispatch and the wiring of the foreground
//! `run` loop against the **real** seams. A real argument parser lands in issue
//! #8; the live `status` view lands in #9.

use std::path::Path;

use tokio::net::UnixListener;

use crate::config::{Account, Config, MAX_ACCOUNTS};
use crate::daemon::{
    run_loop, Daemon, InstanceLock, RealClock, RealRosterPoller, RealShutdown, UnixControl,
};
use crate::error::{Error, Result};
use crate::keychain::RealCredentialStore;
use crate::observability::EventLog;
use crate::paths;
use crate::stash::RealAccountStash;

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

/// Foreground daemon: poll every account's usage and swap the active credential
/// before exhaustion.
///
/// Wires the **real** seams into the generic [`Daemon`] and drives [`run_loop`]
/// until SIGINT / SIGTERM. Lifecycle order is load-bearing: take the
/// single-instance lock FIRST (a second `run` exits `3` without disturbing the
/// first), then bind the control socket, then run.
async fn run() -> Result<()> {
    // The native-local support dir holds both the lock and the socket; ensure it
    // (0700) before either touches it.
    paths::ensure_private_dir(&paths::support_dir()?)?;

    // Single-instance lock FIRST: held for the process lifetime, released by the
    // kernel on exit (`_lock` drop). A second `run` cannot acquire it and exits
    // `3` (issue #7), without disturbing the running daemon.
    let _lock = InstanceLock::acquire(&paths::daemon_lock()?)?;

    // Load the real config (roster + tunables); a malformed or absent config is
    // fatal, never silently replaced by defaults (issue #3).
    let config = Config::load()?;

    paths::ensure_private_dir(&paths::config_dir()?)?;
    paths::ensure_private_dir(&paths::logs_dir()?)?;
    let mut log = EventLog::open()?;

    // Bind the 0600 control socket (status queries; issue #15: handles +
    // percentages only). The lock above guarantees no live daemon owns a stale
    // socket, so a leftover one is safe to remove and rebind.
    let socket_path = paths::control_socket()?;
    let control = bind_control_socket(&socket_path)?;

    // Build the daemon over the real seams: per-account polling (active via the
    // canonical credential, others via their stash), the canonical store, the
    // account stash, the real clock, and `~/.claude.json` for display reconcile.
    let mut daemon = Daemon::new(
        config.roster.clone(),
        RealRosterPoller::new(config.tunables.monitor_401_n),
        RealCredentialStore::new(),
        RealAccountStash::new(),
        RealClock::new(config.poll_interval()),
        paths::claude_json()?,
        &config.tunables,
    );
    let mut shutdown = RealShutdown::new()?;

    eprintln!(
        "sessiometer: daemon started (polling every {}s); Ctrl-C or SIGTERM to stop",
        config.tunables.poll_secs,
    );
    let result = run_loop(&mut daemon, &mut log, &mut shutdown, &control).await;

    // Best-effort cleanup: remove our socket on the way out (the lock releases
    // when `_lock` drops at the end of this scope).
    let _ = std::fs::remove_file(&socket_path);
    result
}

/// Bind the `0600` Unix-domain control socket at `path`, removing any stale
/// socket left by a previous run first (the single-instance lock guarantees no
/// live daemon owns it). The enclosing support dir is `0700`, so the socket is
/// owner-only-reachable even during the bind→chmod window.
fn bind_control_socket(path: &Path) -> Result<UnixControl> {
    use std::os::unix::fs::PermissionsExt;

    // A leftover socket file makes `bind` fail with EADDRINUSE; the lock we hold
    // means it cannot belong to a running daemon, so remove it. A genuinely
    // absent file is not an error.
    match std::fs::remove_file(path) {
        Ok(()) => {}
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
        Err(err) => return Err(Error::Io(err)),
    }
    let listener = UnixListener::bind(path)?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    Ok(UnixControl::new(listener))
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
