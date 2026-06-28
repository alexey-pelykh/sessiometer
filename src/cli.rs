// Copyright (c) 2026 Oleksii PELYKH
// SPDX-License-Identifier: MIT

//! Command-line frontend.
//!
//! A hand-rolled subcommand dispatch (the handful of flag-less subcommands needs
//! no parser dependency) over the **real** seams: `capture` (#4), the foreground
//! `run` loop (#7), the live `status` control-socket client (#8), and the offline
//! `list` roster view (#17).

use std::path::Path;

use tokio::net::{UnixListener, UnixStream};

use crate::config::{Account, Config};
use crate::daemon::{
    run_loop, Daemon, InstanceLock, RealClock, RealRosterPoller, RealShutdown, StatusResponse,
    UnixControl,
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
        RealClock::new(),
        paths::claude_json()?,
        &config.tunables,
    );
    let mut shutdown = RealShutdown::new()?;

    eprintln!(
        "sessiometer: daemon started (polling about every {}s, jittered); \
         Ctrl-C or SIGTERM to stop",
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

/// Show the active account, every account's usage, and the last swap (issue #8).
///
/// The **live** counterpart to the offline `list` (#17): a control-socket CLIENT.
/// Connect to the running daemon's `0600` socket, ask for `status`, and pretty-
/// print the reply. The socket exists only while `run` is live, so a failed
/// connect is the friendly [`Error::DaemonNotRunning`] (exit non-zero), never a
/// raw connection error — the live analog of `list`'s empty-state friendliness.
/// The printer is sourced solely from the [`StatusResponse`], which carries
/// handles + percentages + a swap age only (issue #15 redaction).
async fn status() -> Result<()> {
    let response = query_status(&paths::control_socket()?).await?;
    print!("{}", render_status(&response));
    Ok(())
}

/// Connect to the daemon's control socket at `path`, request `status`, and parse
/// the one-line reply. A connect failure that means "no daemon" — the socket is
/// absent, or present but refusing — maps to the friendly [`Error::DaemonNotRunning`];
/// any other connect error surfaces as itself.
async fn query_status(path: &Path) -> Result<StatusResponse> {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt};

    let stream = match UnixStream::connect(path).await {
        Ok(stream) => stream,
        // No socket file, or a stale one with no listener → no live daemon.
        Err(err)
            if matches!(
                err.kind(),
                std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused
            ) =>
        {
            return Err(Error::DaemonNotRunning);
        }
        Err(err) => return Err(Error::Io(err)),
    };

    // The same newline-delimited JSON the daemon's `serve_control` speaks: write
    // one request line, read one reply line.
    let mut buffered = tokio::io::BufReader::new(stream);
    buffered.write_all(b"{\"cmd\":\"status\"}\n").await?;
    buffered.flush().await?;
    let mut line = String::new();
    buffered.read_line(&mut line).await?;
    serde_json::from_str(line.trim_end()).map_err(|err| Error::Io(std::io::Error::other(err)))
}

/// Render a [`StatusResponse`] as the text `status` prints. Pure (no clock, no
/// I/O) so the response→text mapping is unit-testable. Sourced solely from the
/// response's non-secret fields, so it can never print a token or email (issue #15).
fn render_status(response: &StatusResponse) -> String {
    let mut out = String::new();
    for account in &response.accounts {
        // `*` marks the active account (as the event log does); a leading space
        // keeps the other labels aligned under it.
        let marker = if account.active { "*" } else { " " };
        out.push_str(&format!(
            "{} {} · session {} · weekly {}\n",
            marker,
            account.label,
            pct(account.session_pct),
            pct(account.weekly_pct),
        ));
    }
    out.push('\n');
    match &response.last_swap {
        Some(swap) => out.push_str(&format!(
            "last swap: {} ({})\n",
            swap.to,
            humanize_secs(swap.secs_ago),
        )),
        None => out.push_str("last swap: none\n"),
    }
    out
}

/// A `0..=100` percent as `N%`, or `n/a` when the last poll for that account
/// failed (never a fabricated `0`).
fn pct(percent: Option<u8>) -> String {
    match percent {
        Some(percent) => format!("{percent}%"),
        None => "n/a".to_owned(),
    }
}

/// A whole-second age as a compact relative string, e.g. `90` → `1m ago`. Coarse
/// by design — the minimal `last_swap` presentation for #8.
fn humanize_secs(secs: u64) -> String {
    const MINUTE: u64 = 60;
    const HOUR: u64 = 60 * MINUTE;
    const DAY: u64 = 24 * HOUR;
    if secs < MINUTE {
        format!("{secs}s ago")
    } else if secs < HOUR {
        format!("{}m ago", secs / MINUTE)
    } else if secs < DAY {
        format!("{}h ago", secs / HOUR)
    } else {
        format!("{}d ago", secs / DAY)
    }
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

/// Render the roster as one `label · uuid · stash` row per account, then a bare
/// `N account(s)` total. The roster has no fixed size (#35), so the total carries
/// no "of N" denominator — just the count (pluralized for grammar).
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
    let n = roster.len();
    let noun = if n == 1 { "account" } else { "accounts" };
    out.push_str(&format!("\n{n} {noun}\n"));
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
    use crate::daemon::{AccountStatusLine, LastSwapLine};
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
                session_floor: None,
                session_trigger: 95,
                monitor_401_n: 3,
                // `list` reads no timing strategies; default jitter is a fine
                // placeholder (issue #38).
                ..Tunables::default()
            },
        }
    }

    #[test]
    fn renders_each_account_then_the_count_total() {
        let out = render_roster(&[
            acct(
                "work",
                "11111111-1111-1111-1111-111111111111",
                "Sessiometer/11111111-1111-1111-1111-111111111111",
            ),
            acct(
                "personal",
                "22222222-2222-2222-2222-222222222222",
                "Sessiometer/22222222-2222-2222-2222-222222222222",
            ),
        ]);
        assert_eq!(
            out,
            "work · 11111111 · Sessiometer/11111111-1111-1111-1111-111111111111\n\
personal · 22222222 · Sessiometer/22222222-2222-2222-2222-222222222222\n\
\n\
2 accounts\n"
        );
    }

    #[test]
    fn total_is_a_bare_count_with_no_denominator_and_no_cap() {
        // #35: the total is the row count alone — no "of N" denominator, and the
        // roster can hold more than the former 5-account cap.
        let roster: Vec<Account> = (0..6)
            .map(|i| {
                acct(
                    &format!("l{i}"),
                    &format!("0000000{i}-0000-0000-0000-000000000000"),
                    &format!("Sessiometer/0000000{i}"),
                )
            })
            .collect();
        let out = render_roster(&roster);
        assert!(out.ends_with("\n6 accounts\n"), "got: {out:?}");
        assert!(
            !out.contains("slots"),
            "no 'slots used' denominator: {out:?}"
        );
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
        let config = config_with(vec![acct(
            "work",
            "11111111-aaaa",
            "Sessiometer/11111111-aaaa",
        )]);
        let out = view(Ok(config)).expect("a present roster is not an error");
        // A single-account roster reads "1 account" (singular), not "1 accounts".
        assert_eq!(
            out,
            "work · 11111111 · Sessiometer/11111111-aaaa\n\n1 account\n"
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
            "Sessiometer/11111111-1111-1111-1111-111111111111",
        )]);
        assert!(
            !out.contains('@'),
            "list output must not contain an email: {out:?}"
        );
    }

    // --- status: response → text (issue #8) --------------------------------

    fn status_line(
        label: &str,
        active: bool,
        session: Option<u8>,
        weekly: Option<u8>,
    ) -> AccountStatusLine {
        AccountStatusLine {
            label: label.to_owned(),
            active,
            session_pct: session,
            weekly_pct: weekly,
        }
    }

    #[test]
    fn render_status_shows_marker_quotas_and_a_present_last_swap() {
        let response = StatusResponse {
            accounts: vec![
                status_line("work", true, Some(97), Some(40)),
                status_line("spare", false, Some(10), Some(20)),
                status_line("third", false, None, None),
            ],
            last_swap: Some(LastSwapLine {
                to: "spare".to_owned(),
                secs_ago: 125,
            }),
        };
        let expected = concat!(
            "* work · session 97% · weekly 40%\n",
            "  spare · session 10% · weekly 20%\n",
            "  third · session n/a · weekly n/a\n",
            "\n",
            "last swap: spare (2m ago)\n",
        );
        assert_eq!(render_status(&response), expected);
    }

    #[test]
    fn render_status_shows_last_swap_none_before_any_swap() {
        let response = StatusResponse {
            accounts: vec![status_line("work", true, Some(50), Some(25))],
            last_swap: None,
        };
        let out = render_status(&response);
        assert!(out.ends_with("last swap: none\n"), "got: {out:?}");
    }

    #[test]
    fn render_status_never_carries_an_email_or_token_sigil() {
        // #15: the printer sources only labels + percentages + a swap age, so a
        // token / email can never reach the printed surface.
        let response = StatusResponse {
            accounts: vec![status_line("work", true, Some(50), Some(25))],
            last_swap: Some(LastSwapLine {
                to: "spare".to_owned(),
                secs_ago: 5,
            }),
        };
        let out = render_status(&response);
        assert!(
            !out.contains('@'),
            "status output must not contain an email: {out:?}"
        );
        assert!(!out.to_lowercase().contains("token"));
    }

    #[test]
    fn humanize_secs_uses_compact_units() {
        assert_eq!(humanize_secs(0), "0s ago");
        assert_eq!(humanize_secs(59), "59s ago");
        assert_eq!(humanize_secs(60), "1m ago");
        assert_eq!(humanize_secs(3599), "59m ago");
        assert_eq!(humanize_secs(3600), "1h ago");
        assert_eq!(humanize_secs(86_399), "23h ago");
        assert_eq!(humanize_secs(86_400), "1d ago");
    }

    #[tokio::test]
    async fn query_status_is_friendly_when_no_daemon_is_listening() {
        // The socket exists only while `run` is live; an absent one is the
        // friendly empty state, not a raw connection error (the live analog of
        // `list`'s RosterEmpty, issue #17).
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("daemon.sock"); // never bound
        let err = query_status(&socket).await.expect_err("no daemon → error");
        assert!(matches!(err, Error::DaemonNotRunning), "got {err:?}");
        assert_eq!(
            err.to_string(),
            "daemon not running — start it with `sessiometer run`"
        );
    }

    #[tokio::test]
    async fn query_status_round_trips_over_a_real_socket() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("daemon.sock");
        let listener = UnixListener::bind(&path).unwrap();

        let response = StatusResponse {
            accounts: vec![status_line("work", true, Some(50), Some(25))],
            last_swap: Some(LastSwapLine {
                to: "spare".to_owned(),
                secs_ago: 120,
            }),
        };
        let wire = serde_json::to_string(&response).unwrap();

        // Server side: accept one connection, expect the status request, reply once.
        let server = async {
            use tokio::io::{AsyncBufReadExt, AsyncWriteExt};
            let (stream, _addr) = listener.accept().await.unwrap();
            let mut buffered = tokio::io::BufReader::new(stream);
            let mut request = String::new();
            buffered.read_line(&mut request).await.unwrap();
            assert_eq!(request.trim_end(), r#"{"cmd":"status"}"#);
            buffered.write_all(wire.as_bytes()).await.unwrap();
            buffered.write_all(b"\n").await.unwrap();
            buffered.flush().await.unwrap();
        };

        let (_, parsed) = tokio::join!(server, query_status(&path));
        let parsed = parsed.expect("a live socket round-trips");
        assert_eq!(parsed.accounts.len(), 1);
        assert_eq!(parsed.accounts[0].label, "work");
        assert_eq!(parsed.accounts[0].session_pct, Some(50));
        let swap = parsed.last_swap.expect("last_swap present");
        assert_eq!(swap.to, "spare");
        assert_eq!(swap.secs_ago, 120);
    }
}
