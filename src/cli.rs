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
    run_loop, AccountStatusLine, Daemon, InstanceLock, NextSwap, RealClock, RealRosterPoller,
    RealShutdown, StatusResponse, UnixControl,
};
use crate::error::{Error, Result};
use crate::keychain::RealCredentialStore;
use crate::observability::{Diagnostic, DiagnosticLog, EventLog, Verbosity};
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
                // `run [-v|--verbose]` — verbosity opts into the operator-facing
                // diagnostic channel (issue #77); position-independent, mirroring
                // `status --json`.
                "run" => {
                    let verbose = args.any(|arg| {
                        let arg = arg.to_string_lossy();
                        arg == "-v" || arg == "--verbose"
                    });
                    let verbosity = if verbose {
                        Verbosity::Verbose
                    } else {
                        Verbosity::Quiet
                    };
                    run(verbosity).await
                }
                // `status [--json] [--no-color]` — `--json` dumps the full
                // response verbatim, the full-data contract regardless of terminal
                // width (issue #72); `--no-color` forces the urgency overlay off
                // (issue #73). Both flags may appear in any order; extras ignored.
                "status" => {
                    let mut json = false;
                    let mut no_color = false;
                    for arg in args.by_ref() {
                        match arg.to_string_lossy().as_ref() {
                            "--json" => json = true,
                            "--no-color" => no_color = true,
                            _ => {}
                        }
                    }
                    status(json, no_color).await
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
         run [-v|--verbose]   Run the foreground daemon (poll + swap; -v adds run diagnostics)\n    \
         status [--json] [--no-color]  Show each account's usage + resets-in, and the next swap\n    \
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
///
/// `verbosity` (issue #77) gates the operator-facing diagnostic channel: this
/// function owns the process lifecycle, so it brackets the loop with the
/// `diag=start` / `diag=stop` markers, and the per-tick diagnostics are emitted
/// inside [`run_loop`]. Default [`Verbosity::Quiet`] keeps `run` silent on that
/// channel; `-v`/`--verbose` opts in.
async fn run(verbosity: Verbosity) -> Result<()> {
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

    // The operator-facing diagnostic channel (issue #77): stderr, gated by the
    // verbosity selected from `-v`/`--verbose` (default quiet — no console spam).
    // The lifecycle markers bracket the loop HERE because `cli` owns the process
    // lifecycle: a clean shutdown through EITHER of `run_loop`'s exit paths (the
    // startup-delay or the idle loop) returns `Ok`, so a single `diag=stop` after it
    // covers both. The per-tick diagnostics are emitted inside `run_loop`. The Start
    // summary is the effective config, so one run's lines read against it.
    let mut diag = DiagnosticLog::new(std::io::stderr(), verbosity);
    diag.emit(&Diagnostic::Start {
        accounts: config.roster.len(),
        poll_secs: config.tunables.poll_secs,
        session_floor: config.tunables.session_floor,
        session_trigger: config.tunables.session_trigger,
        weekly_trigger: config.tunables.weekly_trigger,
        monitor_401_n: config.tunables.monitor_401_n,
        monitor_recovery_m: config.tunables.monitor_recovery_m,
    });
    let result = run_loop(&mut daemon, &mut log, &mut diag, &mut shutdown, &control).await;
    // A clean shutdown (`Ok`) → the lifecycle stop marker. An error exit is NOT a
    // clean stop (it surfaces via `main`'s error print), so it emits none.
    if result.is_ok() {
        diag.emit(&Diagnostic::Stop);
    }

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

/// Show the active account, every account's usage, and the next swap candidate (#88).
///
/// The **live** counterpart to the offline `list` (#17): a control-socket CLIENT.
/// Connect to the running daemon's `0600` socket, ask for `status`, and pretty-
/// print the reply. The socket exists only while `run` is live, so a failed
/// connect is the friendly [`Error::DaemonNotRunning`] (exit non-zero), never a
/// raw connection error — the live analog of `list`'s empty-state friendliness.
/// The printer is sourced solely from the [`StatusResponse`], which carries
/// handles + percentages + per-account reset instants + a next-swap candidate
/// label only (issue #15 redaction). `--json` prints that same response verbatim — the full-data
/// contract regardless of terminal width (issue #72).
///
/// The text view marks each account's urgency with a green/yellow/red color
/// overlay (issue #73), but only when the color gate is open — an interactive
/// stdout TTY with none of the opt-outs ([`should_colorize`]). `--json` is never
/// colored (raw data for scripts), and the gate keeps ANSI out of any pipe,
/// redirect, or log, so `status | grep` and `status > file` stay escape-free.
async fn status(json: bool, no_color: bool) -> Result<()> {
    let response = query_status(&paths::control_socket()?).await?;
    if json {
        // The full-data contract, regardless of terminal width (issue #72): the
        // raw response — both per-account reset instants included — pretty-printed,
        // for scripts (`status --json | jq`). Sourced from the same non-secret
        // response as the text view, so it too can never carry a token or email.
        // Never colored — scripts consume the bytes verbatim.
        let rendered = serde_json::to_string_pretty(&response)
            .map_err(|err| Error::Io(std::io::Error::other(err)))?;
        println!("{rendered}");
    } else {
        let color = should_colorize(no_color);
        print!(
            "{}",
            render_status(&response, now_epoch(), terminal_cols(), color)
        );
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
/// table (issue #72), one record per line, then the next-swap footer (#88). Pure (no
/// clock, no I/O) so the response→text mapping is unit-testable — the caller
/// passes `now` (epoch seconds) so each account's "resets in" and urgency are
/// deterministic, `cols` (the terminal width, or `None` when stdout is not a TTY)
/// so the narrow-terminal column degradation is testable, and `color` (whether
/// the color gate is open; [`should_colorize`]) so the ANSI overlay is too.
///
/// Columns, in display order: `ACCOUNT` `SESSION` `WEEKLY` `RESETS` `STATUS`
/// (`STATUS` is omitted when no account carries a tag). When the full table is
/// wider than `cols`, the lowest-priority columns drop in order — `WEEKLY` first,
/// then `STATUS` — never wrapping a row; `ACCOUNT` + `SESSION` + `RESETS` are
/// always kept. A `None` width (piped / redirected) keeps the full table, so
/// `status | grep` and `status > file` stay the complete, greppable surface.
///
/// When `color` is set each CELL is tinted by its OWN health (issue #84), so one
/// glance reads four independent signals per account: `ACCOUNT` by the overall
/// urgency ([`severity`]), `SESSION` / `WEEKLY` by each window's own band
/// ([`util_severity`] / [`weekly_cell_severity`]), and `RESETS` by its relief signal
/// ([`reset_severity`]) — a 95% / 40% account can show a red `SESSION` beside a
/// green `WEEKLY`. (`STATUS` stays untinted: its tags are their own signal.) This
/// supersedes the issue-#73 row-wide tint. The color AUGMENTS — it wraps the
/// already-padded text, so a no-color reader still sees every state and percentage;
/// it is never the only signal. Padding is computed on DISPLAY WIDTH from the raw
/// cell and applied BEFORE the color (pad-before-color), so per-cell colored and
/// multibyte rows stay aligned and the escape bytes never enter the column-width
/// math. The header, the untinted `STATUS` column, and any cell with no reading
/// (nothing to classify — `n/a` is not a false "healthy") stay uncolored.
///
/// Sourced solely from the response's non-secret fields — labels, percentages,
/// reset instants, a next-swap candidate label — so it can never print a token or email (issue #15);
/// the ANSI overlay adds only `\x1b[3Xm`…`\x1b[0m`, never a secret.
///
/// `pub(crate)` so the issue-#15 redaction METER (driven from [`crate::daemon`])
/// can route this exact `status`-text surface through its scan.
pub(crate) fn render_status(
    response: &StatusResponse,
    now: i64,
    cols: Option<usize>,
    color: bool,
) -> String {
    let rows: Vec<StatusRow> = response
        .accounts
        .iter()
        .map(|account| StatusRow::new(account, now))
        .collect();

    // Display order, each tagged with a drop priority (`None` = always keep; lower
    // number drops first). `STATUS` is included only when some account carries a
    // tag — an all-healthy roster shows no empty `STATUS` column.
    let mut columns: Vec<Column> = vec![
        Column::keep("ACCOUNT", |row| &row.account, |row| row.account_severity),
        Column::keep("SESSION", |row| &row.session, |row| row.session_severity),
        Column::droppable("WEEKLY", 1, |row| &row.weekly, |row| row.weekly_severity),
        Column::keep("RESETS", |row| &row.resets, |row| row.resets_severity),
    ];
    if rows.iter().any(|row| !row.status.is_empty()) {
        // STATUS carries its own textual tags (`disabled`, `needs re-login`); it is
        // never tinted (issue #84) — the tags are their own signal, so its severity
        // getter is always `None`.
        columns.push(Column::droppable("STATUS", 2, |row| &row.status, |_| None));
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
    // The header is never tinted — it labels columns, it is not an account.
    let header_colors = vec![None; headers.len()];
    out.push_str(&render_cells(&headers, &widths, &header_colors));
    for row in &rows {
        let cells: Vec<&str> = columns.iter().map(|col| (col.get)(row)).collect();
        // Each cell is tinted by its OWN health when the gate is open (issue #84), so
        // one row can show four independent colors; a cell with no reading, and the
        // whole no-color path, stay uncolored.
        let colors: Vec<Option<&str>> = columns
            .iter()
            .map(|col| {
                color
                    .then(|| (col.severity)(row).map(Severity::sgr))
                    .flatten()
            })
            .collect();
        out.push_str(&render_cells(&cells, &widths, &colors));
    }

    out.push('\n');
    // The forward-looking next-swap candidate (issue #88), computed daemon-side
    // ([`crate::daemon::NextSwap`]); printed plain — the footer carries no color, like
    // the table footer it replaces (per-cell health coloring is #84, orthogonal). A
    // `None` field can only be a pre-#88 daemon that never sends it → bare `none`.
    match &response.next_swap {
        Some(NextSwap::Target { to }) => out.push_str(&format!("next swap: {to}\n")),
        Some(NextSwap::NoViableTarget) => out.push_str("next swap: none (no viable target)\n"),
        Some(NextSwap::AwaitingData) => out.push_str("next swap: none (awaiting usage data)\n"),
        None => out.push_str("next swap: none\n"),
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
    /// Per-cell urgency for the color overlay (issue #84): each cell carries its OWN
    /// health, so one row can show four independent colors (a red `SESSION` beside a
    /// green `WEEKLY`, etc.). Each is `None` when its cell has no reading — that cell
    /// is then printed without color, since absence of color is not a false
    /// "healthy" signal. `account` is the OVERALL (binding-window) [`severity`];
    /// `session` the [`util_severity`] bands on `session_pct`; `weekly` the
    /// [`weekly_cell_severity`] (bands plus the weekly-exhaustion override); `resets`
    /// the [`reset_severity`] relief signal. `STATUS` is never tinted (its tags are
    /// their own signal), so it has no field here.
    account_severity: Option<Severity>,
    session_severity: Option<Severity>,
    weekly_severity: Option<Severity>,
    resets_severity: Option<Severity>,
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
            // Each cell colored by its OWN health (issue #84): ACCOUNT → the overall
            // binding-window severity; SESSION / WEEKLY → each window's own bands
            // (WEEKLY honoring the weekly-exhaustion override); RESETS → the shown
            // reset's relief signal. A cell with no reading stays `None` (uncolored).
            account_severity: severity(account, now),
            session_severity: account.session_pct.map(util_severity),
            weekly_severity: weekly_cell_severity(account),
            resets_severity: reset_severity(account, now),
        }
    }
}

/// One urgency band for the `status` color overlay (issue #73), carried per CELL
/// since issue #84: how much you can rely on what that cell reports at a glance.
///
/// - `Green` — healthy: plenty of quota, usable now.
/// - `Yellow` — getting depleted, OR heavily used but about to reset (recovering).
/// - `Red` — heavily used and not about to reset: the least-available.
///
/// Purely a redundant overlay on the `SESSION`/`WEEKLY` percentages and the
/// `RESETS` time the row already prints — the text stands alone without color
/// (color augments, never the sole signal).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Severity {
    Green,
    Yellow,
    Red,
}

impl Severity {
    /// The ANSI SGR foreground code for this severity (`32`/`33`/`31` =
    /// green/yellow/red). Emitted only when the color gate is open
    /// ([`should_colorize`]); the codes carry no secret (issue #15).
    fn sgr(self) -> &'static str {
        match self {
            Severity::Green => "32",
            Severity::Yellow => "33",
            Severity::Red => "31",
        }
    }
}

/// Utilization at/above which an account is `Red` — heavily depleted, sitting just
/// below the default 95% session swap-away trigger (issue #41), so a red account
/// is genuinely at or near exhaustion.
const RED_UTIL_PCT: u8 = 90;
/// Utilization at/above which an account is at least `Yellow` — getting depleted,
/// worth watching.
const YELLOW_UTIL_PCT: u8 = 75;
/// A binding-window reset within this many seconds counts as "about to recover":
/// it downgrades an otherwise-`Red` account to `Yellow`, telling a heavily-used
/// account that resets imminently apart from one stuck waiting.
const RESET_SOON_SECS: i64 = 30 * 60;

/// Classify one utilization percent into the fixed urgency bands: `>= RED_UTIL_PCT`
/// Red, `>= YELLOW_UTIL_PCT` Yellow, else Green. Extracted (issue #84) so the
/// per-window `SESSION` / `WEEKLY` cells colour off the SAME bands the aggregate
/// [`severity`] applies to its binding window — one definition of "how full is too
/// full", reused everywhere. A pure band lookup: reset proximity and the
/// weekly-exhaustion override live in the callers that need them.
fn util_severity(pct: u8) -> Severity {
    if pct >= RED_UTIL_PCT {
        Severity::Red
    } else if pct >= YELLOW_UTIL_PCT {
        Severity::Yellow
    } else {
        Severity::Green
    }
}

/// Classify one account's OVERALL urgency (issue #73) — the `ACCOUNT` cell's colour
/// under the per-cell overlay (issue #84) — or `None` when there is no reading
/// to classify (both windows `n/a` — the poll failed); such a cell is printed
/// without color, since absence of color is not a false "healthy" signal — the
/// `n/a` text carries the truth.
///
/// Utilization sets the base from the BINDING window. A weekly-EXHAUSTED account
/// (the daemon's blocked-for-the-week verdict, `weekly >= weekly_trigger`, issue
/// #11/#37) is bound by its weekly window whatever the raw percentages say — the
/// SAME window [`resets_in`] shows — and is at least Red: a week-blocked account
/// is never painted "healthy", even when the operator has lowered `weekly_trigger`
/// (configurable down to 50) below the Red utilization cutoff. Otherwise the
/// more-depleted of session / weekly is the constraint, and its percent governs:
/// `>= RED_UTIL_PCT` Red, `>= YELLOW_UTIL_PCT` Yellow, else Green. Reset proximity
/// then refines a depleted account: if the binding window resets within
/// `RESET_SOON_SECS` the account is about to recover, so a Red is downgraded to
/// Yellow. A Green account is never recolored — green is reserved for genuinely
/// low utilization and never lies. Both inputs the issue names — how MUCH is used
/// and how SOON it resets — thus drive the color.
fn severity(account: &AccountStatusLine, now: i64) -> Option<Severity> {
    // The binding window. A weekly-exhausted account is bound by its weekly window
    // regardless of which percent is numerically larger — the daemon has already
    // ruled it blocked for the week (and `weekly_exhausted` implies a present
    // weekly reading, since both derive from the same poll). Otherwise the binding
    // window is whichever of session / weekly is more used; a missing reading
    // counts as "least used" so the other governs, and both missing → None.
    let (util, binding_reset_at) = if account.weekly_exhausted {
        (account.weekly_pct.unwrap_or(100), account.weekly_resets_at)
    } else {
        match (account.session_pct, account.weekly_pct) {
            (None, None) => return None,
            (Some(session), None) => (session, account.session_resets_at),
            (None, Some(weekly)) => (weekly, account.weekly_resets_at),
            (Some(session), Some(weekly)) if session >= weekly => {
                (session, account.session_resets_at)
            }
            (Some(_), Some(weekly)) => (weekly, account.weekly_resets_at),
        }
    };
    // A weekly-exhausted account is Red whatever its percent — it is blocked for
    // the week; otherwise the binding utilization sets the base via the shared
    // [`util_severity`] bands (issue #84).
    let base = if account.weekly_exhausted {
        Severity::Red
    } else {
        util_severity(util)
    };
    // Recovering soon? A Red whose binding window resets within the window (or has
    // already reset — a non-positive delta) is about to free up → downgrade to
    // Yellow. Green / Yellow are unaffected: a soon reset cannot make a depleted
    // account look healthier than Yellow, and never reddens a healthy one.
    if base == Severity::Red && binding_reset_at.is_some_and(|at| at - now <= RESET_SOON_SECS) {
        return Some(Severity::Yellow);
    }
    Some(base)
}

/// The `WEEKLY` cell's own health (issue #84): the fixed [`util_severity`] bands on
/// `weekly_pct`, except a weekly-EXHAUSTED account (the daemon's `weekly >=
/// weekly_trigger` verdict, issue #11/#37) reads Red whatever its rounded percent —
/// a week-blocked account is never painted "healthy", even when the operator has
/// lowered `weekly_trigger` below the Red cutoff (the same guarantee [`severity`]
/// gives the aggregate). `None` when the weekly poll failed: the cell then shows
/// `n/a`, which stays uncolored (absence of color is not a false "healthy"), so the
/// exhaustion override is mapped over a PRESENT reading only.
fn weekly_cell_severity(account: &AccountStatusLine) -> Option<Severity> {
    account.weekly_pct.map(|pct| {
        if account.weekly_exhausted {
            Severity::Red
        } else {
            util_severity(pct)
        }
    })
}

/// The `RESETS` cell's own health (issue #84): proximity-as-relief for the reset the
/// cell SHOWS. Mirrors [`resets_in`] — the WEEKLY reset governs when the account is
/// weekly-exhausted, otherwise the rolling SESSION reset — so the color always
/// describes the duration printed, and keying off the SAME instant `resets_in` uses
/// guarantees a cell shown as `n/a` is never tinted. A depleted account
/// (weekly-exhausted, or the shown window `>= RED_UTIL_PCT`) whose reset lands
/// within `RESET_SOON_SECS` (or has already passed) is about to recover → Yellow
/// ("relief soon"); depleted with a far reset → Red (stuck waiting); a healthy
/// account → Green (the reset is no concern). `None` when that reset instant is
/// unknown — the cell shows `n/a`, which stays uncolored (absence of color must not
/// read as a false "healthy"). This refines, per the reset dimension alone, the
/// Red→Yellow reset-proximity rule [`severity`] applies to the aggregate.
fn reset_severity(account: &AccountStatusLine, now: i64) -> Option<Severity> {
    // The window the RESETS cell displays (see `resets_in`): weekly when exhausted,
    // else session — so the color describes the time actually shown.
    let (pct, reset_at) = if account.weekly_exhausted {
        (account.weekly_pct, account.weekly_resets_at)
    } else {
        (account.session_pct, account.session_resets_at)
    };
    // Unknown governing reset → the cell shows `n/a`; stay uncolored.
    let reset_at = reset_at?;
    let depleted = account.weekly_exhausted || pct.is_some_and(|p| p >= RED_UTIL_PCT);
    if !depleted {
        // Plenty of quota: the reset is no concern.
        return Some(Severity::Green);
    }
    // Depleted: relief is imminent (reset within the window, or already past) →
    // Yellow; a far reset leaves it stuck → Red.
    if reset_at - now <= RESET_SOON_SECS {
        Some(Severity::Yellow)
    } else {
        Some(Severity::Red)
    }
}

/// One `status`-table column: its header, a borrow of the matching [`StatusRow`]
/// cell, the per-cell urgency getter for the color overlay (issue #84), and a drop
/// priority (`None` = always keep; `Some(n)` = droppable, lower `n` drops first
/// under a narrow terminal). `severity` returns this column's own health for a row,
/// or `None` for a column that is never tinted (the `STATUS` tags) or a cell with no
/// reading.
struct Column {
    header: &'static str,
    get: fn(&StatusRow) -> &str,
    severity: fn(&StatusRow) -> Option<Severity>,
    drop_priority: Option<u8>,
}

impl Column {
    fn keep(
        header: &'static str,
        get: fn(&StatusRow) -> &str,
        severity: fn(&StatusRow) -> Option<Severity>,
    ) -> Self {
        Column {
            header,
            get,
            severity,
            drop_priority: None,
        }
    }
    fn droppable(
        header: &'static str,
        priority: u8,
        get: fn(&StatusRow) -> &str,
        severity: fn(&StatusRow) -> Option<Severity>,
    ) -> Self {
        Column {
            header,
            get,
            severity,
            drop_priority: Some(priority),
        }
    }
}

/// Each included column's render width: the widest of its header and its cells,
/// measured in DISPLAY WIDTH ([`display_width`]) — terminal columns, not `char`
/// count — so a wide (CJK) or zero-width glyph in a label sizes the column
/// correctly and the next column still lines up (issue #73).
fn column_widths(columns: &[Column], rows: &[StatusRow]) -> Vec<usize> {
    columns
        .iter()
        .map(|col| {
            let cells = rows.iter().map(|row| display_width((col.get)(row)));
            cells.max().unwrap_or(0).max(display_width(col.header))
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
///
/// Padding is computed on DISPLAY WIDTH ([`display_width`]) — not `char`/byte
/// count, which Rust's `{:<width$}` fill would use — so a wide-glyph cell lands the
/// next column correctly. `colors` carries one entry PER cell (issue #84): when a
/// cell's entry is `Some(sgr)` that cell's text is wrapped in the ANSI color, and
/// the color math is done on the RAW cell width so the escape bytes never enter it —
/// per-cell colors keep the columns aligned exactly as the old row-wide tint did
/// (pad-before-color, issue #73). The trailing pad is appended OUTSIDE the escape so
/// the line's trailing whitespace (an empty `STATUS` cell, a short last cell) still
/// trims away cleanly, leaving no dangling spaces — and stripping every escape
/// recovers the exact plain table (color is purely additive). An entry is `None` for
/// the header, an untinted column, a cell with no reading, and whenever the gate is
/// closed — then that cell emits not one escape byte, keeping a piped / redirected
/// surface clean.
fn render_cells(cells: &[&str], widths: &[usize], colors: &[Option<&str>]) -> String {
    let mut line = String::new();
    for (idx, ((cell, width), color)) in cells.iter().zip(widths).zip(colors).enumerate() {
        if idx > 0 {
            line.push_str(&" ".repeat(STATUS_COL_GAP));
        }
        match color {
            Some(sgr) => line.push_str(&format!("\x1b[{sgr}m{cell}\x1b[0m")),
            None => line.push_str(cell),
        }
        line.push_str(&" ".repeat(width.saturating_sub(display_width(cell))));
    }
    let line = line.trim_end();
    format!("{line}\n")
}

/// The display (terminal-column) width of `s`: how many cells it occupies when
/// printed, which is NOT its `char` count for non-Latin text (issue #73). A
/// pragmatic wcwidth (UAX #11) — wide East Asian glyphs (CJK, Hangul, Kana,
/// fullwidth forms) count two, combining marks and zero-width characters count
/// zero, everything else one. Hand-rolled to keep the dependency graph minimal,
/// matching the crate's other hand-rolled primitives (the SHA-256 in
/// [`crate::redaction`], the civil-date math); it covers the ranges that occur in
/// real operator labels rather than the full Unicode table, and that is enough to
/// keep colored and multibyte `status` rows aligned where `char` count would not.
fn display_width(s: &str) -> usize {
    s.chars().map(char_width).sum()
}

/// The display width of one `char`: 0 (combining / zero-width / NUL), 2 (East
/// Asian wide & fullwidth), or 1 (everything else). The ranges are the well-known
/// UAX #11 wide blocks plus the common zero-width set — see [`display_width`] for
/// the pragmatic-vs-exhaustive scope.
fn char_width(c: char) -> usize {
    let cp = c as u32;
    // Inclusive `(lo, hi)` code-point ranges that occupy ZERO cells: combining
    // marks, the zero-width space/joiner family, variation selectors, and the BOM.
    const ZERO_WIDTH: &[(u32, u32)] = &[
        (0x0300, 0x036F), // combining diacritical marks
        (0x200B, 0x200F), // zero-width space … RLM
        (0xFE00, 0xFE0F), // variation selectors
        (0xFEFF, 0xFEFF), // zero-width no-break space (BOM)
    ];
    // Inclusive ranges that occupy TWO cells: the principal East Asian blocks,
    // fullwidth forms, wide emoji / pictographs, and the supplementary CJK planes.
    const WIDE: &[(u32, u32)] = &[
        (0x1100, 0x115F),   // Hangul Jamo
        (0x2E80, 0x303E),   // CJK radicals … Kangxi … CJK symbols
        (0x3041, 0x33FF),   // Hiragana, Katakana, CJK symbols & punctuation
        (0x3400, 0x4DBF),   // CJK Unified Ext A
        (0x4E00, 0x9FFF),   // CJK Unified Ideographs
        (0xA000, 0xA4CF),   // Yi
        (0xAC00, 0xD7A3),   // Hangul Syllables
        (0xF900, 0xFAFF),   // CJK Compatibility Ideographs
        (0xFE30, 0xFE4F),   // CJK Compatibility Forms
        (0xFF00, 0xFF60),   // Fullwidth Forms
        (0xFFE0, 0xFFE6),   // Fullwidth signs
        (0x1F300, 0x1FAFF), // emoji & pictographs (approximated as uniformly wide)
        (0x20000, 0x3FFFD), // CJK Ext B+ (supplementary planes)
    ];
    let in_any = |ranges: &[(u32, u32)]| ranges.iter().any(|&(lo, hi)| (lo..=hi).contains(&cp));
    // NUL and the zero-width set render nothing; the wide set renders two cells;
    // everything else (the common Latin / ASCII path) renders one.
    if cp == 0 || in_any(ZERO_WIDTH) {
        0
    } else if in_any(WIDE) {
        2
    } else {
        1
    }
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

/// Whether to emit the ANSI urgency overlay on the `status` table (issue #73).
/// Color AUGMENTS the text and must NEVER reach a non-interactive sink (a pipe, a
/// redirect, a log), so the gate is conservative — color is on ONLY on an
/// interactive stdout TTY, and any standard opt-out forces it off. Reads the
/// environment + TTY here; the decision itself is the pure [`color_decision`].
fn should_colorize(no_color: bool) -> bool {
    color_decision(
        no_color,
        std::env::var("NO_COLOR").ok().as_deref(),
        std::env::var("CLICOLOR").ok().as_deref(),
        std::env::var("TERM").ok().as_deref(),
        stdout_is_tty(),
    )
}

/// The pure color decision (issue #73), split from [`should_colorize`] so the
/// gate is unit-testable without touching the process environment or a real TTY.
/// Color is on only when NONE of the opt-outs fire AND stdout is a TTY:
///   - `no_color_flag` — `--no-color` was passed,
///   - `no_color_env` — `NO_COLOR` present and non-empty (<https://no-color.org>),
///   - `clicolor` — `CLICOLOR=0` (the clicolors convention),
///   - `term` — `TERM=dumb` (a terminal that cannot render SGR),
///   - `is_tty` — stdout is interactive (piped / redirected → off).
fn color_decision(
    no_color_flag: bool,
    no_color_env: Option<&str>,
    clicolor: Option<&str>,
    term: Option<&str>,
    is_tty: bool,
) -> bool {
    if no_color_flag {
        return false;
    }
    // `NO_COLOR`: present and non-empty disables; an empty value is treated as
    // unset (the no-color.org wording).
    if no_color_env.is_some_and(|v| !v.is_empty()) {
        return false;
    }
    if clicolor == Some("0") {
        return false;
    }
    if term == Some("dumb") {
        return false;
    }
    is_tty
}

/// Whether stdout is an interactive terminal — the color gate's final condition
/// (issue #73). The `isatty(3)` sibling of [`terminal_cols`]'s `TIOCGWINSZ` probe:
/// a pipe, a redirect, or a closed stdout is not a TTY, so color stays off there.
fn stdout_is_tty() -> bool {
    // SAFETY: `isatty` only inspects the fd and returns 1 (a TTY) or 0; it touches
    // no memory. The same direct-libc idiom the crate uses elsewhere.
    unsafe { libc::isatty(libc::STDOUT_FILENO) == 1 }
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
    use crate::daemon::{AccountStatusLine, NextSwap};
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
            next_swap: None,
        };
        let out = render_status(&response, NOW, None, false);
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
            next_swap: None,
        };
        let out = render_status(&response, NOW, None, false);
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
    fn render_status_renders_an_aligned_table_with_a_next_swap_candidate() {
        // Healthy roster (no tags) → no STATUS column. The full table (cols None)
        // keeps every column; values align under their headers, one row each, then the
        // forward-looking next-swap footer naming the candidate (#88).
        let response = StatusResponse {
            accounts: vec![
                status_line("work", true, Some(97), Some(40)),
                status_line("spare", false, Some(10), Some(20)),
                status_line("third", false, None, None),
            ],
            next_swap: Some(NextSwap::Target {
                to: "spare".to_owned(),
            }),
        };
        let expected = concat!(
            "ACCOUNT  SESSION  WEEKLY  RESETS\n",
            "* work   97%      40%     n/a\n",
            "  spare  10%      20%     n/a\n",
            "  third  n/a      n/a     n/a\n",
            "\n",
            "next swap: spare\n",
        );
        assert_eq!(render_status(&response, NOW, None, false), expected);
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
            next_swap: None,
        };
        let out = render_status(&response, NOW, None, false);
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
            next_swap: None,
        };
        let out = render_status(&response, NOW, None, false);
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
            next_swap: None,
        };
        // Full table is `ACCOUNT(7) SESSION(7) WEEKLY(6) RESETS(6) STATUS(8)` plus
        // four 2-space gaps = 42; dropping WEEKLY → 34; dropping STATUS too → 24.
        // Full width: every column.
        let full = render_status(&response, NOW, Some(200), false);
        assert!(full.contains("WEEKLY") && full.contains("STATUS"));
        // Narrow (38 ∈ [34,41]): WEEKLY drops first; STATUS + the three stay.
        let narrow = render_status(&response, NOW, Some(38), false);
        assert!(!narrow.contains("WEEKLY"), "WEEKLY drops first: {narrow}");
        assert!(
            narrow.contains("STATUS"),
            "STATUS outlives WEEKLY: {narrow}"
        );
        // Narrower (28 ∈ [24,33]): STATUS drops next; the essential three remain.
        let tiny = render_status(&response, NOW, Some(28), false);
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
        let overflow = render_status(&response, NOW, Some(10), false);
        assert!(
            overflow.contains("ACCOUNT")
                && overflow.contains("SESSION")
                && overflow.contains("RESETS"),
            "the essential three survive any width: {overflow}"
        );
        assert_eq!(overflow.lines().filter(|l| l.contains("work")).count(), 1);
    }

    #[test]
    fn render_status_shows_each_next_swap_footer_state() {
        // Every footer variant the candidate (#88) can take. The roster body is the
        // same single active account each time — only `next_swap` drives the footer.
        let footer = |next_swap| {
            let response = StatusResponse {
                accounts: vec![status_line("work", true, Some(50), Some(25))],
                next_swap,
            };
            render_status(&response, NOW, None, false)
                .lines()
                .last()
                .unwrap()
                .to_owned()
        };
        assert_eq!(
            footer(Some(NextSwap::Target {
                to: "spare".to_owned()
            })),
            "next swap: spare"
        );
        assert_eq!(
            footer(Some(NextSwap::NoViableTarget)),
            "next swap: none (no viable target)"
        );
        assert_eq!(
            footer(Some(NextSwap::AwaitingData)),
            "next swap: none (awaiting usage data)"
        );
        // `None` is only a pre-#88 daemon that omits the field → a bare `none`.
        assert_eq!(footer(None), "next swap: none");
    }

    #[test]
    fn render_status_footer_is_plain_even_under_color() {
        // The candidate footer (#88) carries no SGR even when the color gate is open —
        // per-cell health coloring is #84, orthogonal; the footer stays uncolored.
        let response = StatusResponse {
            accounts: vec![status_line("work", true, Some(99), Some(40))],
            next_swap: Some(NextSwap::Target {
                to: "spare".to_owned(),
            }),
        };
        let colored = render_status(&response, NOW, None, true);
        let footer = colored.lines().last().unwrap();
        assert_eq!(footer, "next swap: spare");
        assert!(
            !footer.contains('\x1b'),
            "the next-swap footer is never tinted: {colored:?}"
        );
    }

    #[test]
    fn render_status_never_carries_an_email_or_token_sigil() {
        // #15: the printer sources only labels + percentages + reset instants + a
        // next-swap candidate label, so a token / email can never reach the printed surface.
        let response = StatusResponse {
            accounts: vec![status_line_resets(
                "work",
                Some(50),
                Some(25),
                false,
                Some(NOW + 600),
                Some(NOW + 86_400),
            )],
            next_swap: Some(NextSwap::Target {
                to: "spare".to_owned(),
            }),
        };
        let out = render_status(&response, NOW, None, false);
        assert!(
            !out.contains('@'),
            "status output must not contain an email: {out:?}"
        );
        assert!(!out.to_lowercase().contains("token"));
    }

    // --- status: urgency color + display width (issue #73) -----------------

    /// Strip ANSI SGR sequences (`\x1b[…m`) from `s` — the test-side inverse of
    /// the color overlay, to prove the overlay is purely ADDITIVE: stripping it
    /// must recover the exact plain table.
    fn strip_ansi(s: &str) -> String {
        let mut out = String::new();
        let mut chars = s.chars();
        while let Some(c) = chars.next() {
            if c == '\x1b' {
                // Skip the CSI body up to and including its final `m`.
                for d in chars.by_ref() {
                    if d == 'm' {
                        break;
                    }
                }
            } else {
                out.push(c);
            }
        }
        out
    }

    #[test]
    fn severity_classifies_by_utilization_then_reset_proximity() {
        // Low utilization → green, whatever the reset timing.
        let healthy = status_line_resets(
            "a",
            Some(50),
            Some(40),
            false,
            Some(NOW + 600),
            Some(NOW + 5 * 86_400),
        );
        assert_eq!(severity(&healthy, NOW), Some(Severity::Green));
        // Moderately used (>= 75) → yellow.
        let warm = status_line_resets(
            "b",
            Some(80),
            Some(40),
            false,
            Some(NOW + 4 * 3_600),
            Some(NOW + 5 * 86_400),
        );
        assert_eq!(severity(&warm, NOW), Some(Severity::Yellow));
        // Heavily used (>= 90) with a FAR binding (session) reset → red (stuck).
        let hot = status_line_resets(
            "c",
            Some(96),
            Some(40),
            false,
            Some(NOW + 4 * 3_600),
            Some(NOW + 5 * 86_400),
        );
        assert_eq!(severity(&hot, NOW), Some(Severity::Red));
        // Heavily used but the binding window resets within RESET_SOON_SECS →
        // downgraded to yellow (recovering, not stuck).
        let recovering = status_line_resets(
            "d",
            Some(96),
            Some(40),
            false,
            Some(NOW + 10 * 60),
            Some(NOW + 5 * 86_400),
        );
        assert_eq!(severity(&recovering, NOW), Some(Severity::Yellow));
        // The binding window is the MORE-used one: weekly 96 dominates session 10,
        // and ITS far reset governs → red, NOT downgraded by the soon session reset.
        let weekly_bound = status_line_resets(
            "e",
            Some(10),
            Some(96),
            true,
            Some(NOW + 60),
            Some(NOW + 3 * 86_400),
        );
        assert_eq!(severity(&weekly_bound, NOW), Some(Severity::Red));
        // No reading at all → unclassifiable (printed without color).
        let dark = status_line_resets("f", None, None, false, None, None);
        assert_eq!(severity(&dark, NOW), None);
    }

    #[test]
    fn severity_sits_at_the_documented_thresholds() {
        // `status_line` carries no reset instants, so no soon-reset downgrade fires.
        let at_yellow = status_line("a", false, Some(YELLOW_UTIL_PCT), Some(0));
        assert_eq!(severity(&at_yellow, NOW), Some(Severity::Yellow));
        let below_yellow = status_line("b", false, Some(YELLOW_UTIL_PCT - 1), Some(0));
        assert_eq!(severity(&below_yellow, NOW), Some(Severity::Green));
        let at_red = status_line("c", false, Some(RED_UTIL_PCT), Some(0));
        assert_eq!(severity(&at_red, NOW), Some(Severity::Red));
    }

    #[test]
    fn severity_treats_a_weekly_exhausted_account_as_blocked_not_healthy() {
        // The daemon's blocked-for-the-week verdict (`weekly_exhausted`) must win
        // over raw utilization: with a lowered `weekly_trigger` an account can be
        // exhausted at a weekly percent well BELOW the Red cutoff, yet it is
        // blocked for days — it must read Red, never the "healthy" Green its 65%
        // utilization would otherwise give. Mirrors what `resets_in` shows (the
        // far weekly reset).
        let blocked = status_line_resets(
            "blocked",
            Some(30),               // session is fine…
            Some(65),               // …weekly below RED_UTIL_PCT, but…
            true,                   // …exhausted (e.g. weekly_trigger lowered to 60)
            Some(NOW + 600),        // a soon SESSION reset must NOT rescue it
            Some(NOW + 3 * 86_400), // the binding WEEKLY reset is 3 days out
        );
        assert_eq!(
            severity(&blocked, NOW),
            Some(Severity::Red),
            "a week-blocked account is Red, not Green, and the soon session reset \
             does not downgrade it (the weekly reset governs)"
        );
        // …unless the WEEKLY reset itself is imminent → recovering → Yellow.
        let recovering = status_line_resets(
            "soon",
            Some(30),
            Some(65),
            true,
            Some(NOW + 4 * 3_600),
            Some(NOW + 5 * 60), // weekly reset in 5 min
        );
        assert_eq!(severity(&recovering, NOW), Some(Severity::Yellow));
    }

    #[test]
    fn severity_reset_proximity_handles_the_boundary_past_and_unknown_cases() {
        let red = |session_reset| {
            severity(
                &status_line_resets("r", Some(99), Some(40), false, session_reset, None),
                NOW,
            )
        };
        // Exactly at the soon boundary (`<=`) downgrades.
        assert_eq!(red(Some(NOW + RESET_SOON_SECS)), Some(Severity::Yellow));
        // One second past the boundary does not.
        assert_eq!(red(Some(NOW + RESET_SOON_SECS + 1)), Some(Severity::Red));
        // An already-past reset (negative delta) downgrades — it has recovered.
        assert_eq!(red(Some(NOW - 100)), Some(Severity::Yellow));
        // An unknown binding reset leaves the Red base intact (no fabricated
        // recovery) — the downgrade rests on the pairing being present.
        assert_eq!(red(None), Some(Severity::Red));
    }

    #[test]
    fn util_severity_classifies_at_the_documented_thresholds() {
        // The per-window (SESSION / WEEKLY) band core (issue #84): the same
        // thresholds the aggregate uses, with no reset-proximity or exhaustion logic.
        assert_eq!(util_severity(0), Severity::Green);
        assert_eq!(util_severity(YELLOW_UTIL_PCT - 1), Severity::Green);
        assert_eq!(util_severity(YELLOW_UTIL_PCT), Severity::Yellow);
        assert_eq!(util_severity(RED_UTIL_PCT - 1), Severity::Yellow);
        assert_eq!(util_severity(RED_UTIL_PCT), Severity::Red);
        assert_eq!(util_severity(100), Severity::Red);
    }

    #[test]
    fn weekly_cell_severity_applies_bands_and_the_exhaustion_override() {
        // Not exhausted → the plain util bands on weekly_pct.
        let mut acct = status_line("w", false, Some(50), Some(50));
        assert_eq!(weekly_cell_severity(&acct), Some(Severity::Green));
        acct.weekly_pct = Some(80);
        assert_eq!(weekly_cell_severity(&acct), Some(Severity::Yellow));
        acct.weekly_pct = Some(95);
        assert_eq!(weekly_cell_severity(&acct), Some(Severity::Red));
        // Exhausted (the daemon's weekly_trigger verdict) → Red even at a percent
        // well below the Red cutoff: a week-blocked cell never reads "healthy",
        // honoring a lowered weekly_trigger (issue #11/#37).
        let blocked = status_line_resets("b", Some(20), Some(65), true, None, Some(NOW + 86_400));
        assert_eq!(weekly_cell_severity(&blocked), Some(Severity::Red));
        // No weekly reading → None: the cell shows `n/a`, which stays uncolored.
        let dark = status_line("d", false, Some(50), None);
        assert_eq!(weekly_cell_severity(&dark), None);
    }

    #[test]
    fn reset_severity_is_proximity_as_relief() {
        // Healthy (shown window below the Red cutoff) → green: the reset is no
        // concern, however far off.
        let healthy =
            status_line_resets("h", Some(50), Some(40), false, Some(NOW + 5 * 86_400), None);
        assert_eq!(reset_severity(&healthy, NOW), Some(Severity::Green));
        // Depleted (session >= RED) with a FAR session reset → red (stuck waiting).
        let stuck = status_line_resets("s", Some(99), Some(40), false, Some(NOW + 4 * 3_600), None);
        assert_eq!(reset_severity(&stuck, NOW), Some(Severity::Red));
        // Depleted with a SOON session reset → yellow (relief soon); an already-past
        // reset counts as relief too.
        let soon = status_line_resets("r", Some(99), Some(40), false, Some(NOW + 10 * 60), None);
        assert_eq!(reset_severity(&soon, NOW), Some(Severity::Yellow));
        let past = status_line_resets("p", Some(99), Some(40), false, Some(NOW - 100), None);
        assert_eq!(reset_severity(&past, NOW), Some(Severity::Yellow));
        // Weekly-exhausted → the WEEKLY reset governs (mirrors `resets_in`): a far
        // weekly reset is red despite a soon session reset; a soon one is yellow.
        let weekly_far = status_line_resets(
            "wf",
            Some(20),
            Some(65),
            true,
            Some(NOW + 60),
            Some(NOW + 3 * 86_400),
        );
        assert_eq!(reset_severity(&weekly_far, NOW), Some(Severity::Red));
        let weekly_soon = status_line_resets(
            "ws",
            Some(20),
            Some(65),
            true,
            Some(NOW + 4 * 3_600),
            Some(NOW + 5 * 60),
        );
        assert_eq!(reset_severity(&weekly_soon, NOW), Some(Severity::Yellow));
        // The SHOWN reset unknown → the cell shows `n/a`; stay uncolored (None) even
        // though a util reading exists — absence of color is not a false "healthy".
        let na = status_line_resets("n", Some(99), Some(40), false, None, Some(NOW + 600));
        assert_eq!(reset_severity(&na, NOW), None);
        // No reading at all → None.
        let dark = status_line_resets("d", None, None, false, None, None);
        assert_eq!(reset_severity(&dark, NOW), None);
    }

    #[test]
    fn display_width_counts_terminal_cells_not_chars() {
        assert_eq!(display_width("ascii"), 5);
        assert_eq!(display_width("* work"), 6);
        // Wide CJK: each glyph is two cells (three chars → six cells).
        assert_eq!(display_width("日本語"), 6);
        assert_eq!("日本語".chars().count(), 3); // the count it must NOT use
                                                 // A combining mark adds no width: "e" + U+0301 (combining acute) → one cell.
        assert_eq!(display_width("e\u{0301}"), 1);
        // Zero-width joiner and the BOM contribute nothing.
        assert_eq!(display_width("a\u{200d}b"), 2);
        assert_eq!(display_width("\u{feff}hi"), 2);
    }

    #[test]
    fn color_decision_requires_a_tty_and_honors_every_opt_out() {
        // Happy path: a TTY, no opt-out → color on.
        assert!(color_decision(false, None, None, None, true));
        // Not a TTY (piped / redirected) → off, even with no opt-out.
        assert!(!color_decision(false, None, None, None, false));
        // `--no-color` forces off on a TTY.
        assert!(!color_decision(true, None, None, None, true));
        // NO_COLOR present and non-empty → off; an empty value is treated as unset.
        assert!(!color_decision(false, Some("1"), None, None, true));
        assert!(color_decision(false, Some(""), None, None, true));
        // CLICOLOR=0 → off; CLICOLOR=1 does not force color onto a non-TTY.
        assert!(!color_decision(false, None, Some("0"), None, true));
        assert!(!color_decision(false, None, Some("1"), None, false));
        // TERM=dumb → off; a normal TERM is fine.
        assert!(!color_decision(false, None, None, Some("dumb"), true));
        assert!(color_decision(
            false,
            None,
            None,
            Some("xterm-256color"),
            true
        ));
    }

    #[test]
    fn color_off_emits_not_one_escape_byte() {
        // Even with a red-urgency account present, color=false yields no ANSI — so
        // a pipe / redirect / log never carries an escape (the gate's promise).
        let response = StatusResponse {
            accounts: vec![status_line_resets(
                "hot",
                Some(99),
                Some(40),
                false,
                Some(NOW + 4 * 3_600),
                Some(NOW + 5 * 86_400),
            )],
            next_swap: None,
        };
        let out = render_status(&response, NOW, None, false);
        assert!(
            !out.contains('\x1b'),
            "no escape byte when color is off: {out:?}"
        );
    }

    #[test]
    fn color_on_tints_each_row_and_strips_back_to_the_exact_plain_table() {
        let response = StatusResponse {
            accounts: vec![
                // green: low utilization.
                status_line_resets(
                    "calm",
                    Some(20),
                    Some(15),
                    false,
                    Some(NOW + 3_600),
                    Some(NOW + 5 * 86_400),
                ),
                // red: heavily used, far reset.
                status_line_resets(
                    "hot",
                    Some(99),
                    Some(40),
                    false,
                    Some(NOW + 4 * 3_600),
                    Some(NOW + 5 * 86_400),
                ),
            ],
            next_swap: Some(NextSwap::Target {
                to: "calm".to_owned(),
            }),
        };
        let plain = render_status(&response, NOW, None, false);
        let colored = render_status(&response, NOW, None, true);
        // The overlay emits escapes and tints by severity (green=32, red=31).
        assert!(
            colored.contains("\x1b[32m"),
            "green row tinted: {colored:?}"
        );
        assert!(colored.contains("\x1b[31m"), "red row tinted: {colored:?}");
        // …and is purely ADDITIVE: stripping the ANSI recovers the EXACT plain
        // table — proving color augments (every state + percentage still present)
        // and that padding was computed BEFORE coloring (alignment survives strip).
        assert_eq!(strip_ansi(&colored), plain);
        // The header line is never tinted (it labels columns, it is not an account).
        let header = colored.lines().next().unwrap();
        assert!(header.starts_with("ACCOUNT") && !header.contains('\x1b'));
    }

    #[test]
    fn color_paints_each_cell_by_its_own_health() {
        // One account, four independent signals (issue #84): SESSION heavily used
        // (red) sits beside a comfortable WEEKLY (green) on the SAME row — proving
        // per-cell color, not one row-wide tint.
        let response = StatusResponse {
            accounts: vec![status_line_resets(
                "mix",
                Some(99), // SESSION: red band
                Some(40), // WEEKLY: green band
                false,
                Some(NOW + 4 * 3_600), // far session reset → depleted + far
                Some(NOW + 5 * 86_400),
            )],
            next_swap: None,
        };
        let colored = render_status(&response, NOW, None, true);
        let plain = render_status(&response, NOW, None, false);
        let row = colored
            .lines()
            .find(|l| l.contains("mix"))
            .expect("a row for mix");
        // The SESSION cell is red AND the WEEKLY cell is green, on one line.
        assert!(row.contains("\x1b[31m99%"), "session cell red: {row:?}");
        assert!(row.contains("\x1b[32m40%"), "weekly cell green: {row:?}");
        // Each colored cell is independently wrapped + reset (not one row-wide span).
        assert!(
            row.matches("\x1b[0m").count() >= 2,
            "multiple independently-tinted cells: {row:?}"
        );
        // Still purely additive: stripping the ANSI recovers the exact plain table.
        assert_eq!(strip_ansi(&colored), plain);
    }

    #[test]
    fn color_leaves_an_n_a_cell_uncolored() {
        // SESSION has a reading (red); WEEKLY does not (`n/a`). The n/a cell must
        // stay uncolored — absence of color is not a false "healthy" (issue #84) —
        // while its colored siblings prove the overlay is active.
        let response = StatusResponse {
            accounts: vec![status_line_resets(
                "half",
                Some(99), // session present → red
                None,     // weekly n/a → uncolored
                false,
                Some(NOW + 4 * 3_600),
                None,
            )],
            next_swap: None,
        };
        let colored = render_status(&response, NOW, None, true);
        let plain = render_status(&response, NOW, None, false);
        // No `n/a` is ever wrapped in an SGR color (the only n/a here is WEEKLY).
        for sgr in ["31", "32", "33"] {
            assert!(
                !colored.contains(&format!("\x1b[{sgr}mn/a")),
                "the n/a weekly cell stays uncolored: {colored:?}"
            );
        }
        // …yet the overlay is active on the cells that DO have a reading.
        assert!(
            colored.contains("\x1b[31m"),
            "session cell tinted: {colored:?}"
        );
        assert_eq!(strip_ansi(&colored), plain);
    }

    #[test]
    fn multibyte_label_rows_stay_aligned_on_display_width() {
        // A wide (CJK) label is two display cells per glyph; padding on display
        // width keeps the SESSION column under its header where `.chars().count()`
        // would misalign it.
        let response = StatusResponse {
            accounts: vec![
                status_line("ascii", true, Some(50), Some(60)),
                status_line("日本語", false, Some(10), Some(20)),
            ],
            next_swap: None,
        };
        let out = render_status(&response, NOW, None, false);
        // Each row's SESSION value begins at the same DISPLAY column.
        let session_col = |needle: &str| {
            let line = out.lines().find(|l| l.contains(needle)).unwrap();
            let idx = line.find(needle).unwrap();
            display_width(&line[..idx])
        };
        assert_eq!(
            session_col("50%"),
            session_col("10%"),
            "wide-label and ascii rows align the SESSION column on display width:\n{out}"
        );
    }

    #[test]
    fn colored_output_never_carries_an_email_or_token_sigil() {
        // #15 holds with the #73 overlay: the ANSI codes add only `\x1b[3Xm`…,
        // never an `@`-email or a token sigil.
        let response = StatusResponse {
            accounts: vec![status_line_resets(
                "work",
                Some(99),
                Some(40),
                false,
                Some(NOW + 4 * 3_600),
                Some(NOW + 5 * 86_400),
            )],
            next_swap: Some(NextSwap::Target {
                to: "spare".to_owned(),
            }),
        };
        let out = render_status(&response, NOW, None, true);
        assert!(out.contains('\x1b'), "the overlay is active: {out:?}");
        assert!(
            !out.contains('@'),
            "no email on the colored surface: {out:?}"
        );
        assert!(!out.to_lowercase().contains("token"));
        assert!(!out.contains("sk-ant-"));
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
            next_swap: Some(NextSwap::Target {
                to: "spare".to_owned(),
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
        // The next-swap candidate round-trips intact (#88).
        assert_eq!(
            parsed.next_swap,
            Some(NextSwap::Target {
                to: "spare".to_owned()
            })
        );
    }
}
