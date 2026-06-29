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
    run_loop, AccountStatusLine, Daemon, InstanceLock, RealClock, RealRosterPoller, RealShutdown,
    StatusResponse, UnixControl,
};
use crate::error::{Error, Result};
use crate::keychain::RealCredentialStore;
use crate::observability::EventLog;
use crate::paths;
use crate::stash::{AccountStash, RealAccountStash};

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
                // `status [--json]` — `--json` dumps the full response verbatim,
                // the full-data contract regardless of terminal width (issue #72).
                "status" => {
                    let json = args.any(|arg| arg.to_string_lossy() == "--json");
                    status(json).await
                }
                "list" => list().await,
                // `use <account> [--force]` switches the active account on demand
                // (issue #63), reusing the swap engine (#6). The target is the first
                // non-flag positional (resolved by label OR account-uuid, #17); the
                // `--force` flag may appear on either side of it and bypasses the
                // policy gate. There is deliberately no "cycle to the next account"
                // fallback for a missing target (out of scope, #63) — it surfaces as
                // `UseTargetRequired`. Extra positionals are ignored, matching the
                // other subcommands.
                "use" => {
                    let mut target = None;
                    let mut force = false;
                    for arg in args.by_ref() {
                        let arg = arg.to_string_lossy();
                        if arg == "--force" {
                            force = true;
                        } else if target.is_none() {
                            target = Some(arg.into_owned());
                        }
                    }
                    crate::use_account::use_account(target, force).await
                }
                // `disable`/`enable <label>` flip an account's rotation flag and
                // persist (issue #36). Mirror `capture`'s optional-positional parse;
                // a missing label surfaces as `RotationLabelRequired`.
                "disable" => {
                    let label = args.next().map(|s| s.to_string_lossy().into_owned());
                    set_enabled(label, false).await
                }
                "enable" => {
                    let label = args.next().map(|s| s.to_string_lossy().into_owned());
                    set_enabled(label, true).await
                }
                // `remove <label>` drops an account from the roster AND deletes its
                // stash — the destructive sibling of `disable` (issue #13). Same
                // optional-positional parse; a missing label is RotationLabelRequired.
                "remove" => {
                    let label = args.next().map(|s| s.to_string_lossy().into_owned());
                    remove_account(label).await
                }
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
         status [--json]      Show each account's usage + resets-in, and the last swap\n    \
         list       List captured accounts\n    \
         use <account> [--force]  Switch the active account now (--force overrides the pre-swap gate)\n    \
         disable <label>      Park an account: keep it but take it out of the rotation\n    \
         enable <label>       Return a parked account to the rotation\n    \
         remove <label>       Delete an account: drop it from the rotation and erase its stash\n    \
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
    // The daemon needs at least one account to rotate across. This is the daemon's
    // precondition (enforced here, at the consumer), NOT a parse-time rule —
    // `capture` must be able to load a tunables-only config to populate it (#58).
    // Fail fast with the friendly empty-state, before binding the socket or log.
    config.require_roster()?;

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
    // Wire the single-writer swap lock (#64) so the daemon's own swaps serialize
    // against a concurrent manual `use` swap on the same native-local `swap.lock`.
    let mut daemon = Daemon::new(
        config.roster.clone(),
        RealRosterPoller::new(),
        RealCredentialStore::new(),
        RealAccountStash::new(),
        RealClock::new(),
        paths::claude_json()?,
        &config.tunables,
    )
    .with_swap_lock(paths::swap_lock()?);
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
/// handles + percentages + per-account reset instants + a swap age only (issue
/// #15 redaction). `--json` prints that same response verbatim — the full-data
/// contract regardless of terminal width (issue #72).
async fn status(json: bool) -> Result<()> {
    let response = query_status(&paths::control_socket()?).await?;
    if json {
        // The full-data contract, regardless of terminal width (issue #72): the
        // raw response — both per-account reset instants included — pretty-printed,
        // for scripts (`status --json | jq`). Sourced from the same non-secret
        // response as the text view, so it too can never carry a token or email.
        let rendered = serde_json::to_string_pretty(&response)
            .map_err(|err| Error::Io(std::io::Error::other(err)))?;
        println!("{rendered}");
    } else {
        print!("{}", render_status(&response, now_epoch(), terminal_cols()));
    }
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

/// Render a [`StatusResponse`] as the text `status` prints: an aligned column
/// table (issue #72), one record per line, then the `last_swap` footer. Pure (no
/// clock, no I/O) so the response→text mapping is unit-testable — the caller
/// passes `now` (epoch seconds) so each account's "resets in" is deterministic,
/// and `cols` (the terminal width, or `None` when stdout is not a TTY) so the
/// narrow-terminal column degradation is testable.
///
/// Columns, in display order: `ACCOUNT` `SESSION` `WEEKLY` `RESETS` `STATUS`
/// (`STATUS` is omitted when no account carries a tag). When the full table is
/// wider than `cols`, the lowest-priority columns drop in order — `WEEKLY` first,
/// then `STATUS` — never wrapping a row; `ACCOUNT` + `SESSION` + `RESETS` are
/// always kept. A `None` width (piped / redirected) keeps the full table, so
/// `status | grep` and `status > file` stay the complete, greppable surface.
///
/// Sourced solely from the response's non-secret fields — labels, percentages, a
/// swap age, reset instants — so it can never print a token or email (issue #15).
///
/// `pub(crate)` so the issue-#15 redaction METER (driven from [`crate::daemon`])
/// can route this exact `status`-text surface through its scan.
pub(crate) fn render_status(response: &StatusResponse, now: i64, cols: Option<usize>) -> String {
    let rows: Vec<StatusRow> = response
        .accounts
        .iter()
        .map(|account| StatusRow::new(account, now))
        .collect();

    // Display order, each tagged with a drop priority (`None` = always keep; lower
    // number drops first). `STATUS` is included only when some account carries a
    // tag — an all-healthy roster shows no empty `STATUS` column.
    let mut columns: Vec<Column> = vec![
        Column::keep("ACCOUNT", |row| &row.account),
        Column::keep("SESSION", |row| &row.session),
        Column::droppable("WEEKLY", 1, |row| &row.weekly),
        Column::keep("RESETS", |row| &row.resets),
    ];
    if rows.iter().any(|row| !row.status.is_empty()) {
        columns.push(Column::droppable("STATUS", 2, |row| &row.status));
    }

    // Drop the lowest-priority droppable column until the table fits `cols`. A
    // non-TTY width (`None`) never enters the loop — the full table is preserved.
    while let Some(width) = cols {
        if table_width(&columns, &rows) <= width {
            break;
        }
        match columns
            .iter()
            .enumerate()
            .filter_map(|(idx, col)| col.drop_priority.map(|prio| (prio, idx)))
            .min()
        {
            Some((_, idx)) => {
                columns.remove(idx);
            }
            // Only keep-columns remain: never wrap, just let the essential three
            // overflow a very narrow terminal (predictable, one record per line).
            None => break,
        }
    }

    let widths = column_widths(&columns, &rows);
    let mut out = String::new();
    let headers: Vec<&str> = columns.iter().map(|col| col.header).collect();
    out.push_str(&render_cells(&headers, &widths));
    for row in &rows {
        let cells: Vec<&str> = columns.iter().map(|col| (col.get)(row)).collect();
        out.push_str(&render_cells(&cells, &widths));
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

/// Gap between adjacent `status`-table columns (two spaces, matching `list`).
const STATUS_COL_GAP: usize = 2;

/// One account projected to its `status`-table cells (issue #72). Pre-rendered
/// strings so column widths can be measured uniformly across header + rows.
struct StatusRow {
    /// `* label` (active) or `  label` — the marker folds into this column.
    account: String,
    session: String,
    weekly: String,
    /// Compact "resets in", or `n/a` when the governing reset is unknown.
    resets: String,
    /// Inline tags (`disabled`, `needs re-login`), comma-joined; empty when none.
    status: String,
}

impl StatusRow {
    fn new(account: &AccountStatusLine, now: i64) -> Self {
        // `*` marks the active account (as the event log does); a leading space
        // keeps the inactive labels aligned under it.
        let marker = if account.active { '*' } else { ' ' };
        // A parked account is `disabled` (issue #36); a dead-credential one
        // `needs re-login` (issue #42, the durable quarantine status). Both can
        // hold at once, so they comma-join rather than overwrite.
        let mut status = String::new();
        if !account.enabled {
            status.push_str("disabled");
        }
        if account.quarantined {
            if !status.is_empty() {
                status.push_str(", ");
            }
            status.push_str("needs re-login");
        }
        StatusRow {
            account: format!("{marker} {}", account.label),
            session: pct(account.session_pct),
            weekly: pct(account.weekly_pct),
            resets: resets_in(account, now),
            status,
        }
    }
}

/// One `status`-table column: its header, a borrow of the matching [`StatusRow`]
/// cell, and a drop priority (`None` = always keep; `Some(n)` = droppable, lower
/// `n` drops first under a narrow terminal).
struct Column {
    header: &'static str,
    get: fn(&StatusRow) -> &str,
    drop_priority: Option<u8>,
}

impl Column {
    fn keep(header: &'static str, get: fn(&StatusRow) -> &str) -> Self {
        Column {
            header,
            get,
            drop_priority: None,
        }
    }
    fn droppable(header: &'static str, priority: u8, get: fn(&StatusRow) -> &str) -> Self {
        Column {
            header,
            get,
            drop_priority: Some(priority),
        }
    }
}

/// Each included column's render width: the widest of its header and its cells
/// (by char count, matching the `{:<width$}` fill).
fn column_widths(columns: &[Column], rows: &[StatusRow]) -> Vec<usize> {
    columns
        .iter()
        .map(|col| {
            let cells = rows.iter().map(|row| (col.get)(row).chars().count());
            cells.max().unwrap_or(0).max(col.header.chars().count())
        })
        .collect()
}

/// Total rendered width of the table: summed column widths plus the inter-column
/// gaps. Used to decide whether a column must drop to fit the terminal.
fn table_width(columns: &[Column], rows: &[StatusRow]) -> usize {
    let cells: usize = column_widths(columns, rows).iter().sum();
    cells + columns.len().saturating_sub(1) * STATUS_COL_GAP
}

/// Render one table line: each cell left-padded to its column width, joined by the
/// column gap, with trailing whitespace trimmed (so an empty trailing cell — a
/// healthy account's `STATUS` — leaves no dangling spaces and the line stays
/// greppable).
fn render_cells(cells: &[&str], widths: &[usize]) -> String {
    let mut line = String::new();
    for (idx, (cell, width)) in cells.iter().zip(widths).enumerate() {
        if idx > 0 {
            line.push_str(&" ".repeat(STATUS_COL_GAP));
        }
        line.push_str(&format!("{cell:<width$}", width = *width));
    }
    format!("{}\n", line.trim_end())
}

/// One account's compact "resets in" (issue #72): the time until it next regains
/// capacity. When the weekly window is exhausted (`weekly_exhausted` — the daemon's
/// own `weekly >= weekly_trigger` viability verdict, issue #11/#37) the account is
/// blocked until the WEEKLY reset; otherwise the rolling 5-hour SESSION window is
/// what gates it, so the SESSION reset is when it becomes usable again. Keying off
/// the daemon's flag — not a re-derived `weekly_pct == 100` — keeps the display
/// honest for an account at/above the trigger but below a rounded 100%: it is
/// already blocked for the week, and is shown as such. `n/a` when the governing
/// reset is unknown (the poll failed, or the API gave no parseable timestamp) —
/// never a fabricated duration.
fn resets_in(account: &AccountStatusLine, now: i64) -> String {
    let reset_at = if account.weekly_exhausted {
        account.weekly_resets_at
    } else {
        account.session_resets_at
    };
    match reset_at {
        Some(at) => humanize_until(at - now),
        None => "n/a".to_owned(),
    }
}

/// A `0..=100` percent as `N%`, or `n/a` when the last poll for that account
/// failed (never a fabricated `0`).
fn pct(percent: Option<u8>) -> String {
    match percent {
        Some(percent) => format!("{percent}%"),
        None => "n/a".to_owned(),
    }
}

/// A whole-second remaining time as a compact "resets in" string: the two largest
/// non-zero units, e.g. `12m`, `4h`, `3d4h` (a trailing zero unit is dropped). A
/// reset already reached (`<= 0`) renders as `now`, and under a minute as `<1m`.
/// The forward-looking counterpart to [`humanize_secs`] (which renders an elapsed
/// `…ago`).
fn humanize_until(secs: i64) -> String {
    if secs <= 0 {
        return "now".to_owned();
    }
    const MINUTE: i64 = 60;
    const HOUR: i64 = 60 * MINUTE;
    const DAY: i64 = 24 * HOUR;
    let days = secs / DAY;
    let hours = (secs % DAY) / HOUR;
    let mins = (secs % HOUR) / MINUTE;
    if days > 0 {
        if hours > 0 {
            format!("{days}d{hours}h")
        } else {
            format!("{days}d")
        }
    } else if hours > 0 {
        if mins > 0 {
            format!("{hours}h{mins}m")
        } else {
            format!("{hours}h")
        }
    } else if mins > 0 {
        format!("{mins}m")
    } else {
        "<1m".to_owned()
    }
}

/// The controlling terminal's column count for stdout, or `None` when stdout is
/// not a TTY (piped / redirected) or the query fails. Drives `status`'s
/// narrow-terminal column degradation (issue #72); the `None` non-interactive case
/// keeps the full table, so `status | grep` and `status > file` stay complete.
fn terminal_cols() -> Option<usize> {
    // SAFETY: `winsize` is plain-old-data we zero-initialize; the ioctl only writes
    // into it through the pointer we pass and returns `0` on success. The same
    // direct-libc idiom the rest of the crate uses (e.g. `getpeereid`, `flock`).
    let mut ws: libc::winsize = unsafe { std::mem::zeroed() };
    let rc = unsafe { libc::ioctl(libc::STDOUT_FILENO, libc::TIOCGWINSZ, &mut ws) };
    if rc == 0 && ws.ws_col > 0 {
        Some(ws.ws_col as usize)
    } else {
        None
    }
}

/// Current wall-clock time as epoch seconds — the reference `status` measures each
/// account's "resets in" against. A pre-1970 clock degrades to `0` rather than
/// panicking, the same tolerant projection [`crate::observability`] uses.
fn now_epoch() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs() as i64)
        .unwrap_or(0)
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
        // Both empty states read the same: an absent config, OR a well-formed
        // tunables-only file whose roster is empty (now that `capture` can load
        // such a file, #58). Either way `list` shows the friendly "nothing captured
        // yet" rather than a bare "0 accounts".
        Ok(config) if config.roster.is_empty() => Err(Error::RosterEmpty),
        Ok(config) => Ok(render_roster(&config.roster)),
        Err(Error::ConfigNotFound { .. }) => Err(Error::RosterEmpty),
        Err(other) => Err(other),
    }
}

/// Render the roster as two space-aligned columns — each account's `label`, then
/// its full `account_uuid` — one row per account, followed by a bare
/// `N account(s)` total. The label column is padded to the widest label plus a
/// two-space gap so the uuid column lines up. The FULL uuid (not a truncated
/// prefix) is shown so it can be copied straight into `sessiometer use <uuid>`,
/// and the former keychain-name column is dropped — it was just `Sessiometer/` +
/// the uuid, redundant once the full uuid is shown (issue #69). The roster has no
/// fixed size (#35), so the total carries no "of N" denominator — just the count
/// (pluralized for grammar).
///
/// Sourced solely from each [`Account`]'s two non-secret display fields — `label`
/// and `account_uuid` — never a token or email (issue #15 redaction). A label is
/// operator-provided free text: one that happens to contain an `@` is the
/// operator's own value, not a leak.
///
/// `pub(crate)` so the issue-#15 redaction METER (driven from [`crate::daemon`])
/// can route this exact `list`-view surface through its scan.
pub(crate) fn render_roster(roster: &[Account]) -> String {
    // Pad the label column to the widest label (by char count, matching the
    // `{:<width$}` fill) so the uuid column aligns. The offline `list` never
    // renders an empty roster (that maps to the friendly `RosterEmpty`), but
    // `unwrap_or(0)` keeps this total for the METER's direct callers.
    let width = roster
        .iter()
        .map(|account| account.label.chars().count())
        .max()
        .unwrap_or(0);
    let mut out = String::new();
    for account in roster {
        // A parked account is marked inline (issue #36); an enabled one adds
        // nothing.
        let state = if account.enabled { "" } else { " · disabled" };
        out.push_str(&format!(
            "{:<width$}  {}{}\n",
            account.label, account.account_uuid, state,
        ));
    }
    let n = roster.len();
    let noun = if n == 1 { "account" } else { "accounts" };
    out.push_str(&format!("\n{n} {noun}\n"));
    out
}

/// `disable`/`enable <label>` — take an account out of the rotation, or return it
/// (issue #36). A reversible park, distinct from removal (#13): the account keeps
/// its roster entry and its stash; only its `enabled` flag flips. Resolve the
/// account by its non-secret label, set the flag, and persist via [`Config::save`]
/// so the change survives a daemon restart (config-backed). Takes effect at the
/// next daemon start — a running daemon loads the roster once.
///
/// A missing `<label>` is [`Error::RotationLabelRequired`]; a label that matches no
/// account is [`Error::AccountLabelNotFound`]. `enabled` selects the verb so one
/// body serves both subcommands; the `verb` it derives names the usage in errors.
async fn set_enabled(label: Option<String>, enabled: bool) -> Result<()> {
    let verb = if enabled { "enable" } else { "disable" };
    let label = label.ok_or(Error::RotationLabelRequired { verb })?;
    let mut config = Config::load()?;
    let outcome = apply_enabled(&mut config.roster, &label, enabled)?;
    // Only rewrite config.toml when the flag actually changed — re-disabling an
    // already-parked account is a friendly no-op, not a needless disk write.
    if matches!(outcome, FlipOutcome::Changed) {
        config.save()?;
    }
    println!("{}", flip_confirmation(outcome, &label, enabled));
    Ok(())
}

/// Whether an [`apply_enabled`] flip actually changed the stored flag.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FlipOutcome {
    /// The flag was flipped to the requested state.
    Changed,
    /// The account was already in the requested state — nothing to persist.
    Unchanged,
}

/// Resolve `label` in `roster` and set its `enabled` flag, reporting whether the
/// value actually changed. Pure (no I/O) so the resolve-and-flip policy is unit-
/// testable without touching `config.toml`; the caller persists only on
/// [`FlipOutcome::Changed`]. `Err(AccountLabelNotFound)` when no account carries
/// the label. The first match wins (labels are operator handles; uniqueness is not
/// enforced, so a duplicate label resolves to the earliest roster entry).
fn apply_enabled(roster: &mut [Account], label: &str, enabled: bool) -> Result<FlipOutcome> {
    let account = roster
        .iter_mut()
        .find(|account| account.label == label)
        .ok_or_else(|| Error::AccountLabelNotFound {
            label: label.to_owned(),
        })?;
    if account.enabled == enabled {
        Ok(FlipOutcome::Unchanged)
    } else {
        account.enabled = enabled;
        Ok(FlipOutcome::Changed)
    }
}

/// The confirmation line for a `disable`/`enable`. Names the label (non-secret,
/// issue #15) and reflects whether the flag changed or was already in that state.
fn flip_confirmation(outcome: FlipOutcome, label: &str, enabled: bool) -> String {
    let state = if enabled { "enabled" } else { "disabled" };
    match outcome {
        FlipOutcome::Changed => format!("{state} `{label}`"),
        FlipOutcome::Unchanged => format!("`{label}` is already {state}"),
    }
}

/// `remove <label>` — the DESTRUCTIVE sibling of `disable` (issue #13): drop the
/// account from the roster AND delete its keychain stash, so it is gone for good
/// (vs `disable`, which keeps both and only flips the rotation flag). Resolve by
/// label, then persist the roster without the entry FIRST and delete the stash
/// SECOND.
///
/// The ordering is the crash-safe one: a failure (a crash, or a locked keychain at
/// the delete) after the config save leaves only an ORPHANED, unreferenced stash —
/// harmless keychain data nothing reads — rather than a roster entry pointing at a
/// stash that has already been deleted, which the daemon would repeatedly fail to
/// read. The stash delete is idempotent (an already-absent half is success), so a
/// re-run after a partial failure still converges.
///
/// A missing `<label>` is [`Error::RotationLabelRequired`]; a label that matches no
/// account is [`Error::AccountLabelNotFound`]. Takes effect at the next daemon
/// start — a running daemon loads the roster once. Removing the ACTIVE account is
/// allowed and self-heals: this touches only sessiometer's roster entry and stash,
/// never the canonical credential, so the daemon simply polls-only (resolving no
/// active account) until another account is captured or the operator `/login`s.
async fn remove_account(label: Option<String>) -> Result<()> {
    let label = label.ok_or(Error::RotationLabelRequired { verb: "remove" })?;
    let mut config = Config::load()?;
    let removed = apply_remove(&mut config.roster, &label)?;
    // Config FIRST (see the doc): persist the roster without the entry before the
    // destructive stash delete, so any failure past here orphans a harmless stash
    // rather than dangling a roster entry at a deleted one.
    config.save()?;
    // Then delete the now-unreferenced stash — both halves, idempotent. The
    // service name is derived from the removed account's uuid (issue #70).
    RealAccountStash::new().delete(&removed.stash()).await?;
    println!("{}", remove_confirmation(&label));
    Ok(())
}

/// Resolve `label` in `roster` and REMOVE its entry, returning the removed account
/// (whose `stash` name the caller needs to delete the keychain stash). Pure (no
/// I/O) so the resolve-and-remove policy is unit-testable without touching
/// `config.toml`. `Err(AccountLabelNotFound)` when no account carries the label.
/// The first match wins (labels are operator handles; uniqueness is not enforced,
/// so a duplicate label removes the earliest roster entry).
fn apply_remove(roster: &mut Vec<Account>, label: &str) -> Result<Account> {
    let idx = roster
        .iter()
        .position(|account| account.label == label)
        .ok_or_else(|| Error::AccountLabelNotFound {
            label: label.to_owned(),
        })?;
    Ok(roster.remove(idx))
}

/// The confirmation line for a `remove`. Names the label (non-secret, issue #15).
fn remove_confirmation(label: &str) -> String {
    format!("removed `{label}`")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Tunables;
    use crate::daemon::{AccountStatusLine, LastSwapLine};
    use std::path::PathBuf;

    fn acct(label: &str, uuid: &str) -> Account {
        Account {
            account_uuid: uuid.to_owned(),
            label: label.to_owned(),
            enabled: true,
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
            acct("work", "11111111-1111-1111-1111-111111111111"),
            acct("personal", "22222222-2222-2222-2222-222222222222"),
        ]);
        assert_eq!(
            out,
            "work      11111111-1111-1111-1111-111111111111\n\
personal  22222222-2222-2222-2222-222222222222\n\
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
    fn view_renders_a_present_roster() {
        let config = config_with(vec![acct("work", "11111111-aaaa")]);
        let out = view(Ok(config)).expect("a present roster is not an error");
        // A single-account roster reads "1 account" (singular), not "1 accounts".
        assert_eq!(out, "work  11111111-aaaa\n\n1 account\n");
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
    fn view_maps_a_roster_less_config_to_the_friendly_empty_state() {
        // #58: a well-formed tunables-only config (empty roster) reads as the same
        // friendly empty state as an absent file — `capture` can now load such a
        // file, so `list` must not show a bare "0 accounts".
        let config = config_with(vec![]);
        assert!(
            matches!(view(Ok(config)), Err(Error::RosterEmpty)),
            "an empty roster must become the friendly empty state"
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
        // #15 redaction: the formatter sources only the two non-secret roster
        // fields it shows (`label`, `account_uuid`), so it never auto-introduces a
        // token or email. (A label the operator sets to an email is their own
        // value, not a leak — see issue #69.)
        let out = render_roster(&[acct("work", "11111111-1111-1111-1111-111111111111")]);
        assert!(
            !out.contains('@'),
            "list output must not contain an email: {out:?}"
        );
    }

    // --- enable/disable (issue #36) ----------------------------------------

    #[test]
    fn render_roster_marks_a_disabled_account_and_leaves_enabled_ones_unchanged() {
        let mut work = acct("work", "11111111-1111");
        work.enabled = false;
        let spare = acct("spare", "22222222-2222");
        let out = render_roster(&[work, spare]);
        assert_eq!(
            out,
            "work   11111111-1111 · disabled\n\
spare  22222222-2222\n\
\n\
2 accounts\n"
        );
    }

    #[test]
    fn render_status_marks_a_disabled_account_only() {
        let mut spare = status_line("spare", false, Some(10), Some(20));
        spare.enabled = false;
        let response = StatusResponse {
            accounts: vec![status_line("work", true, Some(50), Some(25)), spare],
            last_swap: None,
        };
        let out = render_status(&response, NOW, None);
        // The enabled active account is unmarked; the parked one carries the tag.
        let work = out.lines().find(|l| l.contains("work")).unwrap();
        assert!(work.starts_with("* work") && work.contains("50%") && work.contains("25%"));
        assert!(
            !work.contains("disabled"),
            "active account is unmarked: {work}"
        );
        let spare = out.lines().find(|l| l.contains("spare")).unwrap();
        assert!(
            spare.starts_with("  spare") && spare.contains("10%") && spare.contains("disabled"),
            "the parked account carries the tag: {spare}"
        );
    }

    #[test]
    fn render_status_marks_a_quarantined_account_needs_relogin() {
        // Issue #42: a dead-credential account carries the durable `needs re-login`
        // tag in `status`, while a healthy account's line is unchanged. The tag is a
        // plain string — no token, no email reaches the printed surface (#15).
        let mut spare = status_line("spare", false, None, None);
        spare.quarantined = true;
        let response = StatusResponse {
            accounts: vec![status_line("work", true, Some(50), Some(25)), spare],
            last_swap: None,
        };
        let out = render_status(&response, NOW, None);
        let work = out.lines().find(|l| l.contains("work")).unwrap();
        assert!(
            work.starts_with("* work") && work.contains("50%") && !work.contains("re-login"),
            "the healthy active account is unmarked: {work}"
        );
        let spare = out.lines().find(|l| l.contains("spare")).unwrap();
        assert!(
            spare.contains("n/a") && spare.contains("needs re-login"),
            "the dead account carries the durable re-login tag: {spare}"
        );
        assert!(
            !out.contains('@'),
            "no email on the printed surface: {out:?}"
        );
        assert!(!out.to_lowercase().contains("token"));
    }

    #[test]
    fn apply_enabled_flips_the_resolved_account_and_reports_change() {
        let mut roster = vec![acct("work", "u1"), acct("spare", "u2")];
        // Resolve `spare` by label and disable it; the other account is untouched.
        assert_eq!(
            apply_enabled(&mut roster, "spare", false).unwrap(),
            FlipOutcome::Changed
        );
        assert!(roster[0].enabled, "the unaddressed account is left alone");
        assert!(!roster[1].enabled);
        // Re-enable flips it back.
        assert_eq!(
            apply_enabled(&mut roster, "spare", true).unwrap(),
            FlipOutcome::Changed
        );
        assert!(roster[1].enabled);
    }

    #[test]
    fn apply_enabled_is_idempotent_when_already_in_the_target_state() {
        let mut roster = vec![acct("work", "u1")];
        // Already enabled → Unchanged, so the caller skips the config rewrite.
        assert_eq!(
            apply_enabled(&mut roster, "work", true).unwrap(),
            FlipOutcome::Unchanged
        );
        assert!(roster[0].enabled);
    }

    #[test]
    fn apply_enabled_rejects_an_unknown_label_without_touching_the_roster() {
        let mut roster = vec![acct("work", "u1")];
        let err =
            apply_enabled(&mut roster, "ghost", false).expect_err("an unmatched label is an error");
        assert!(
            matches!(err, Error::AccountLabelNotFound { ref label } if label == "ghost"),
            "got {err:?}"
        );
        assert!(
            roster[0].enabled,
            "a failed resolve leaves the roster intact"
        );
    }

    #[test]
    fn flip_confirmation_reflects_changed_vs_already_in_state() {
        assert_eq!(
            flip_confirmation(FlipOutcome::Changed, "work", false),
            "disabled `work`"
        );
        assert_eq!(
            flip_confirmation(FlipOutcome::Changed, "work", true),
            "enabled `work`"
        );
        assert_eq!(
            flip_confirmation(FlipOutcome::Unchanged, "work", false),
            "`work` is already disabled"
        );
        assert_eq!(
            flip_confirmation(FlipOutcome::Unchanged, "work", true),
            "`work` is already enabled"
        );
    }

    // --- remove (issue #13) ------------------------------------------------

    #[test]
    fn apply_remove_drops_the_resolved_account_and_returns_it() {
        let mut roster = vec![
            acct("work", "u1"),
            acct("spare", "u2"),
            acct("backup", "u3"),
        ];
        // Resolve `spare` by label, remove it, and hand its stash name back so the
        // caller can delete the keychain stash.
        let removed = apply_remove(&mut roster, "spare").expect("a present label removes");
        assert_eq!(removed.label, "spare");
        assert_eq!(removed.stash(), "Sessiometer/u2");
        // The entry is gone and the survivors keep their order.
        assert_eq!(roster.len(), 2);
        assert_eq!(roster[0].label, "work");
        assert_eq!(roster[1].label, "backup");
    }

    #[test]
    fn apply_remove_rejects_an_unknown_label_without_touching_the_roster() {
        let mut roster = vec![acct("work", "u1")];
        let err = apply_remove(&mut roster, "ghost").expect_err("an unmatched label is an error");
        assert!(
            matches!(err, Error::AccountLabelNotFound { ref label } if label == "ghost"),
            "got {err:?}"
        );
        assert_eq!(roster.len(), 1, "a failed resolve leaves the roster intact");
    }

    #[test]
    fn remove_confirmation_names_the_label() {
        assert_eq!(remove_confirmation("work"), "removed `work`");
        // #15: the confirmation carries only the operator label, never a secret.
        assert!(!remove_confirmation("work").contains('@'));
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
            enabled: true,
            quarantined: false,
            session_resets_at: None,
            weekly_resets_at: None,
            weekly_exhausted: false,
        }
    }

    /// A reading with known reset instants and a weekly-exhaustion verdict — the
    /// `resets in` tests (issue #72) script which window each account is waiting on.
    fn status_line_resets(
        label: &str,
        session: Option<u8>,
        weekly: Option<u8>,
        weekly_exhausted: bool,
        session_resets_at: Option<i64>,
        weekly_resets_at: Option<i64>,
    ) -> AccountStatusLine {
        AccountStatusLine {
            label: label.to_owned(),
            active: false,
            session_pct: session,
            weekly_pct: weekly,
            enabled: true,
            quarantined: false,
            session_resets_at,
            weekly_resets_at,
            weekly_exhausted,
        }
    }

    // A fixed `now` for the deterministic `resets in` tests (issue #72): an
    // arbitrary epoch the per-account reset instants below are offset from.
    const NOW: i64 = 1_782_777_600;

    #[test]
    fn render_status_renders_an_aligned_table_with_a_present_last_swap() {
        // Healthy roster (no tags) → no STATUS column. The full table (cols None)
        // keeps every column; values align under their headers, one row each.
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
            "ACCOUNT  SESSION  WEEKLY  RESETS\n",
            "* work   97%      40%     n/a\n",
            "  spare  10%      20%     n/a\n",
            "  third  n/a      n/a     n/a\n",
            "\n",
            "last swap: spare (2m ago)\n",
        );
        assert_eq!(render_status(&response, NOW, None), expected);
    }

    #[test]
    fn render_status_shows_resets_in_for_every_account() {
        // Each account shows when it next regains capacity — the SESSION reset
        // normally, the WEEKLY reset only when the weekly window is exhausted
        // (issue #72). Not only the exhausted one: every row carries a value.
        let response = StatusResponse {
            accounts: vec![
                // healthy → session reset (12 min out)
                status_line_resets(
                    "work",
                    Some(30),
                    Some(40),
                    false,
                    Some(NOW + 12 * 60),
                    Some(NOW + 5 * 86_400),
                ),
                // session-exhausted, weekly fine → session reset (4h out), NOT the
                // far-off weekly reset.
                status_line_resets(
                    "spare",
                    Some(100),
                    Some(60),
                    false,
                    Some(NOW + 4 * 3_600),
                    Some(NOW + 3 * 86_400),
                ),
                // weekly-exhausted → weekly reset (3d4h out), the binding window.
                status_line_resets(
                    "third",
                    Some(100),
                    Some(100),
                    true,
                    Some(NOW + 2 * 3_600),
                    Some(NOW + 3 * 86_400 + 4 * 3_600),
                ),
            ],
            last_swap: None,
        };
        let out = render_status(&response, NOW, None);
        let line = |label: &str| {
            out.lines()
                .find(|l| l.contains(label))
                .unwrap_or_else(|| panic!("no row for {label} in:\n{out}"))
                .to_owned()
        };
        assert!(line("work").contains("12m"), "{}", line("work"));
        assert!(line("spare").contains("4h"), "{}", line("spare"));
        assert!(line("third").contains("3d4h"), "{}", line("third"));
        // Every account row carries a resets value (none blank), and the header
        // names the column.
        assert!(out.contains("RESETS"), "{out}");
    }

    #[test]
    fn render_status_marks_disabled_and_quarantined_in_a_status_column() {
        // A tag on any account adds the STATUS column; both tags can hold at once.
        let mut quarantined = status_line("dead", false, None, None);
        quarantined.enabled = false;
        quarantined.quarantined = true;
        let response = StatusResponse {
            accounts: vec![status_line("work", true, Some(50), Some(25)), quarantined],
            last_swap: None,
        };
        let out = render_status(&response, NOW, None);
        assert!(out.contains("STATUS"), "tagged roster shows STATUS: {out}");
        let dead = out.lines().find(|l| l.contains("dead")).unwrap();
        assert!(
            dead.contains("disabled, needs re-login"),
            "both tags shown: {dead}"
        );
        // A healthy account's row carries no tag text.
        let work = out.lines().find(|l| l.contains("work")).unwrap();
        assert!(!work.contains("disabled") && !work.contains("re-login"));
    }

    #[test]
    fn render_status_drops_columns_in_priority_order_when_narrow() {
        let response = StatusResponse {
            accounts: vec![{
                let mut a = status_line("work", false, Some(50), Some(25));
                a.enabled = false; // a STATUS tag, so the column exists to be dropped
                a
            }],
            last_swap: None,
        };
        // Full table is `ACCOUNT(7) SESSION(7) WEEKLY(6) RESETS(6) STATUS(8)` plus
        // four 2-space gaps = 42; dropping WEEKLY → 34; dropping STATUS too → 24.
        // Full width: every column.
        let full = render_status(&response, NOW, Some(200));
        assert!(full.contains("WEEKLY") && full.contains("STATUS"));
        // Narrow (38 ∈ [34,41]): WEEKLY drops first; STATUS + the three stay.
        let narrow = render_status(&response, NOW, Some(38));
        assert!(!narrow.contains("WEEKLY"), "WEEKLY drops first: {narrow}");
        assert!(
            narrow.contains("STATUS"),
            "STATUS outlives WEEKLY: {narrow}"
        );
        // Narrower (28 ∈ [24,33]): STATUS drops next; the essential three remain.
        let tiny = render_status(&response, NOW, Some(28));
        assert!(
            !tiny.contains("WEEKLY") && !tiny.contains("STATUS"),
            "{tiny}"
        );
        assert!(
            tiny.contains("ACCOUNT") && tiny.contains("SESSION") && tiny.contains("RESETS"),
            "the essential three are always kept: {tiny}"
        );
        // Every degraded form is still one record per line (never wrapped).
        assert_eq!(tiny.lines().filter(|l| l.contains("work")).count(), 1);
        // Even a width too small for the essential three (24 > 10): they are NEVER
        // dropped and the row is NEVER wrapped — it simply overflows, staying one
        // greppable record per line (the terminal soft-wraps it visually).
        let overflow = render_status(&response, NOW, Some(10));
        assert!(
            overflow.contains("ACCOUNT")
                && overflow.contains("SESSION")
                && overflow.contains("RESETS"),
            "the essential three survive any width: {overflow}"
        );
        assert_eq!(overflow.lines().filter(|l| l.contains("work")).count(), 1);
    }

    #[test]
    fn render_status_shows_last_swap_none_before_any_swap() {
        let response = StatusResponse {
            accounts: vec![status_line("work", true, Some(50), Some(25))],
            last_swap: None,
        };
        let out = render_status(&response, NOW, None);
        assert!(out.ends_with("last swap: none\n"), "got: {out:?}");
    }

    #[test]
    fn render_status_never_carries_an_email_or_token_sigil() {
        // #15: the printer sources only labels + percentages + reset instants + a
        // swap age, so a token / email can never reach the printed surface.
        let response = StatusResponse {
            accounts: vec![status_line_resets(
                "work",
                Some(50),
                Some(25),
                false,
                Some(NOW + 600),
                Some(NOW + 86_400),
            )],
            last_swap: Some(LastSwapLine {
                to: "spare".to_owned(),
                secs_ago: 5,
            }),
        };
        let out = render_status(&response, NOW, None);
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

    #[test]
    fn humanize_until_uses_two_largest_compact_units() {
        assert_eq!(humanize_until(0), "now"); // reached
        assert_eq!(humanize_until(-30), "now"); // already past
        assert_eq!(humanize_until(30), "<1m"); // under a minute
        assert_eq!(humanize_until(12 * 60), "12m");
        assert_eq!(humanize_until(60 * 60), "1h");
        assert_eq!(humanize_until(2 * 3_600 + 30 * 60), "2h30m");
        assert_eq!(humanize_until(3 * 86_400 + 4 * 3_600), "3d4h");
        assert_eq!(humanize_until(3 * 86_400), "3d"); // trailing zero unit dropped
    }

    #[test]
    fn resets_in_keys_off_the_binding_window() {
        // weekly NOT exhausted → session reset governs.
        let healthy = status_line_resets(
            "a",
            Some(50),
            Some(50),
            false,
            Some(NOW + 600),
            Some(NOW + 99),
        );
        assert_eq!(resets_in(&healthy, NOW), "10m");
        // weekly exhausted → weekly reset governs, even though the session reset is
        // sooner.
        let weekly_out = status_line_resets(
            "b",
            Some(100),
            Some(100),
            true,
            Some(NOW + 600),
            Some(NOW + 7_200),
        );
        assert_eq!(resets_in(&weekly_out, NOW), "2h");
        // The band the daemon counts as exhausted but a rounded percent does NOT:
        // weekly_pct rounds to 98 (< 100), yet `weekly_exhausted` is true (the raw
        // fraction is at/above the trigger). The OLD `weekly_pct == 100` heuristic
        // would have wrongly shown the 4h session reset; the daemon's flag shows the
        // real 3d weekly block. This is the reviewer-flagged correctness fix.
        let band = status_line_resets(
            "c",
            Some(100),
            Some(98),
            true,
            Some(NOW + 4 * 3_600),
            Some(NOW + 3 * 86_400),
        );
        assert_eq!(resets_in(&band, NOW), "3d");
        // Governing reset unknown → n/a (never a fabricated duration).
        let unknown = status_line_resets("d", Some(50), Some(50), false, None, Some(NOW + 600));
        assert_eq!(resets_in(&unknown, NOW), "n/a");
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
