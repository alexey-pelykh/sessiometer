// Copyright (c) 2026 Oleksii PELYKH
// SPDX-License-Identifier: MIT

//! Command-line frontend.
//!
//! A hand-rolled subcommand dispatch (the handful of flag-less subcommands needs
//! no parser dependency) over the **real** seams: `capture` (#4), the foreground
//! `run` loop (#7), the live `status` control-socket client (#8), and the offline
//! `list` roster view (#17).

use std::ffi::{OsStr, OsString};
use std::io::IsTerminal;
use std::path::{Path, PathBuf};
use std::time::Duration;

use lexopt::Arg::{Long, Short, Value};

use tokio::net::{UnixListener, UnixStream};

use unicode_width::UnicodeWidthStr;

use crate::claude_state::OauthAccount;
use crate::config::{Account, Config, ConflictPolicy};
use crate::daemon::{
    run_loop, AccountStatusLine, Daemon, ExternalLoginWatcher, InstanceLock, NextSwap, RealClock,
    RealRosterPoller, RealShutdown, StatusResponse, UnixControl,
};
use crate::error::{Error, Result};
use crate::keychain::{Credential, RealCredentialStore};
use crate::migration::{ManagedAccount, MigrationArtifact, Passphrase, Payload, PLAINTEXT_WARNING};
use crate::observability::{
    CredentialHealth, Diagnostic, DiagnosticLog, Event, EventLog, ExportMode, RefreshEventOutcome,
    Verbosity,
};
use crate::paths;
use crate::refresh;
use crate::refresh_tick::{RealRefreshEngine, RefreshTick};
use crate::sha256::sha256_hex;
use crate::stash::{AccountStash, RealAccountStash, StashedAccount};
use crate::swap::{SwapLock, SWAP_LOCK_MAX_WAIT};

/// Parse `argv`, then run the requested subcommand.
///
/// A thin seam over the two halves the strict argv layer (issue #175) is built from:
/// `parse` maps the argument vector to a [`Command`] — rejecting unknown flags and
/// honouring `-h`/`--help`/`--version` as it goes — and `execute` runs it. Keeping
/// `parse` a pure, I/O-free mapping is what lets the mis-parse cases be pinned by unit
/// tests without a keychain, roster, or daemon: a typo'd `--force` never reaches the swap
/// engine because it fails at `parse`, and `capture --help` resolves to help rather than
/// re-labelling the roster (owner's #175 note).
pub(crate) async fn dispatch(args: std::env::ArgsOs) -> Result<()> {
    execute(parse(args.skip(1))?).await
}

/// A lexopt parse failure folds into the crate error taxonomy as a [`Error::CliUsage`].
/// Only `Parser::next` propagates here (an unconsumed `--flag=value` on a boolean flag);
/// the common unknown-flag and missing-value cases are turned into our own wording by
/// `unexpected` / `required_value` before this ever fires, so this carries the generic
/// root hint. lexopt's messages are secret-free — argv never holds a token or passphrase
/// (the passphrase is read off-argv, cf. #39).
impl From<lexopt::Error> for Error {
    fn from(err: lexopt::Error) -> Self {
        Error::CliUsage {
            message: err.to_string(),
            usage_hint: "sessiometer --help",
        }
    }
}

/// One fully-parsed CLI invocation: a subcommand plus its validated options, or one of
/// the two argv-level meta actions (`--version`, `--help`). Produced by `parse`, run by
/// `execute`. A plain, I/O-free data enum on purpose (issue #175) — the seam that makes
/// the parse layer unit-testable, so a typo'd flag or a `<verb> --help` can be asserted
/// without touching the keychain, roster, or daemon.
#[derive(Debug, PartialEq)]
enum Command {
    /// `capture [<label>]` — stash the active account into the rotation.
    Capture { label: Option<String> },
    /// `login [<label>]` — `claude /login` in isolation, then land it in the rotation.
    Login { label: Option<String> },
    /// `run [-v|--verbose]` — the foreground poll+swap daemon.
    Run { verbose: bool },
    /// `status [--json] [--no-color] [-v|--verbose]` — the live status client.
    Status {
        json: bool,
        no_color: bool,
        verbose: bool,
    },
    /// `list` — the offline roster view.
    List,
    /// `use <account> [--force]` — switch the active account now.
    Use { target: Option<String>, force: bool },
    /// `disable`/`enable <label>` — flip an account's rotation flag (`enabled`).
    SetEnabled {
        label: Option<String>,
        enabled: bool,
    },
    /// `remove <label>` — drop an account and erase its stash.
    Remove { label: Option<String> },
    /// `poke [<account>]` — refresh a parked account's credential once.
    Poke { target: Option<String> },
    /// `stats [<account>...] [--period …] [--since …] [--json] [--no-color] [--ascii]`.
    Stats(crate::stats::StatsArgs),
    /// `export [PATH] …`. The raw flags are carried and resolved to an `Encryption` in
    /// `execute`, so this variant stays a plain comparable value for the parser tests.
    Export {
        path: Option<PathBuf>,
        no_secrets: bool,
        plaintext: bool,
        passphrase_file: Option<PathBuf>,
        passphrase_stdin: bool,
    },
    /// `import <PATH> …`. Like `Export`, carries raw flags resolved to a `PassphraseSource`
    /// in `execute`; the required `PATH` is enforced at parse time.
    Import {
        path: PathBuf,
        overwrite: bool,
        passphrase_file: Option<PathBuf>,
        passphrase_stdin: bool,
    },
    /// `--version` / `-V` — print the crate version.
    Version,
    /// `-h` / `--help`, top-level or after a subcommand — print the matching help.
    Help(HelpTopic),
}

/// Which help text a [`Command::Help`] prints (issue #175): the root overview, or one
/// subcommand's own usage. Doubles as the subcommand identity in a [`Error::CliUsage`]
/// hint, so a rejected flag points at the exact `--help` to run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HelpTopic {
    Root,
    Capture,
    Login,
    Run,
    Status,
    List,
    Use,
    Disable,
    Enable,
    Remove,
    Poke,
    Stats,
    Export,
    Import,
}

impl HelpTopic {
    /// The `sessiometer … --help` invocation an error points the operator at.
    fn hint(self) -> &'static str {
        match self {
            HelpTopic::Root => "sessiometer --help",
            HelpTopic::Capture => "sessiometer capture --help",
            HelpTopic::Login => "sessiometer login --help",
            HelpTopic::Run => "sessiometer run --help",
            HelpTopic::Status => "sessiometer status --help",
            HelpTopic::List => "sessiometer list --help",
            HelpTopic::Use => "sessiometer use --help",
            HelpTopic::Disable => "sessiometer disable --help",
            HelpTopic::Enable => "sessiometer enable --help",
            HelpTopic::Remove => "sessiometer remove --help",
            HelpTopic::Poke => "sessiometer poke --help",
            HelpTopic::Stats => "sessiometer stats --help",
            HelpTopic::Export => "sessiometer export --help",
            HelpTopic::Import => "sessiometer import --help",
        }
    }

    /// The full help text this topic prints (trailing newline included). The root topic is
    /// the top-level overview; every subcommand has its own focused usage block, so
    /// `sessiometer <verb> --help` is command-specific (issue #175).
    fn help(self) -> &'static str {
        match self {
            HelpTopic::Root => ROOT_USAGE,
            HelpTopic::Capture => CAPTURE_USAGE,
            HelpTopic::Login => LOGIN_USAGE,
            HelpTopic::Run => RUN_USAGE,
            HelpTopic::Status => STATUS_USAGE,
            HelpTopic::List => LIST_USAGE,
            HelpTopic::Use => USE_USAGE,
            HelpTopic::Disable => DISABLE_USAGE,
            HelpTopic::Enable => ENABLE_USAGE,
            HelpTopic::Remove => REMOVE_USAGE,
            HelpTopic::Poke => POKE_USAGE,
            HelpTopic::Stats => STATS_USAGE,
            HelpTopic::Export => EXPORT_USAGE,
            HelpTopic::Import => IMPORT_USAGE,
        }
    }
}

/// Map an unrecognized argument to the strict-usage error (issue #175): a `-x` / `--foo`
/// flag the subcommand does not accept, or a stray positional where none belongs. `topic`
/// selects the `--help` the message points at. Secret-free — argv holds no token.
fn unexpected(arg: lexopt::Arg<'_>, topic: HelpTopic) -> Error {
    let message = match arg {
        Short(c) => format!("unknown flag `-{c}`"),
        Long(name) => format!("unknown flag `--{name}`"),
        Value(value) => format!("unexpected argument `{}`", value.to_string_lossy()),
    };
    Error::CliUsage {
        message,
        usage_hint: topic.hint(),
    }
}

/// Take the value a value-bearing flag requires, or map lexopt's `MissingValue` to a clear
/// strict-usage error (issue #175) — the `--period`/`--since`/`--passphrase-file` case
/// where the flag is the last token. Returns the raw `OsString` (a path may be non-UTF-8);
/// the caller lossily stringifies where a `String` is wanted.
fn required_value(parser: &mut lexopt::Parser, flag: &str, topic: HelpTopic) -> Result<OsString> {
    parser.value().map_err(|_| Error::CliUsage {
        message: format!("`--{flag}` needs a value"),
        usage_hint: topic.hint(),
    })
}

/// Parse a subcommand that takes an optional single positional (capture, login, disable,
/// enable, remove, poke): the first non-flag token is it, extras are ignored (matching the
/// prior behavior), `-h`/`--help` in any position short-circuits to help, and any unknown
/// flag is rejected. `build` turns the collected positional into the right [`Command`].
fn parse_positional(
    parser: &mut lexopt::Parser,
    topic: HelpTopic,
    build: impl FnOnce(Option<String>) -> Command,
) -> Result<Command> {
    let mut positional = None;
    while let Some(arg) = parser.next()? {
        match arg {
            Short('h') | Long("help") => return Ok(Command::Help(topic)),
            Value(value) if positional.is_none() => {
                positional = Some(value.to_string_lossy().into_owned());
            }
            Value(_) => {} // extra positional ignored, matching the prior behavior
            other => return Err(unexpected(other, topic)),
        }
    }
    Ok(build(positional))
}

/// Parse `list` — no positional, no flags but `-h`/`--help`. A stray positional is ignored
/// (prior behavior); an unknown flag is rejected (issue #175).
fn parse_list(parser: &mut lexopt::Parser) -> Result<Command> {
    while let Some(arg) = parser.next()? {
        match arg {
            Short('h') | Long("help") => return Ok(Command::Help(HelpTopic::List)),
            Value(_) => {}
            other => return Err(unexpected(other, HelpTopic::List)),
        }
    }
    Ok(Command::List)
}

/// Parse `run [-v|--verbose]` (issue #77) — the verbosity flag, position-independent.
fn parse_run(parser: &mut lexopt::Parser) -> Result<Command> {
    let mut verbose = false;
    while let Some(arg) = parser.next()? {
        match arg {
            Short('h') | Long("help") => return Ok(Command::Help(HelpTopic::Run)),
            Short('v') | Long("verbose") => verbose = true,
            Value(_) => {}
            other => return Err(unexpected(other, HelpTopic::Run)),
        }
    }
    Ok(Command::Run { verbose })
}

/// Parse `status [--json] [--no-color] [-v|--verbose]` (issues #72/#73/#143) — all flags
/// order-independent.
fn parse_status(parser: &mut lexopt::Parser) -> Result<Command> {
    let mut json = false;
    let mut no_color = false;
    let mut verbose = false;
    while let Some(arg) = parser.next()? {
        match arg {
            Short('h') | Long("help") => return Ok(Command::Help(HelpTopic::Status)),
            Long("json") => json = true,
            Long("no-color") => no_color = true,
            Short('v') | Long("verbose") => verbose = true,
            Value(_) => {}
            other => return Err(unexpected(other, HelpTopic::Status)),
        }
    }
    Ok(Command::Status {
        json,
        no_color,
        verbose,
    })
}

/// Parse `use <account> [--force]` (issue #63) — `--force` order-independent, the first
/// non-flag token is the target, extras ignored. A missing target is left to
/// `use_account` (surfaced as `UseTargetRequired`), preserving the prior split.
fn parse_use(parser: &mut lexopt::Parser) -> Result<Command> {
    let mut target = None;
    let mut force = false;
    while let Some(arg) = parser.next()? {
        match arg {
            Short('h') | Long("help") => return Ok(Command::Help(HelpTopic::Use)),
            Long("force") => force = true,
            Value(value) if target.is_none() => {
                target = Some(value.to_string_lossy().into_owned());
            }
            Value(_) => {}
            other => return Err(unexpected(other, HelpTopic::Use)),
        }
    }
    Ok(Command::Use { target, force })
}

/// Parse `stats [<account>...] [--period …] [--since …] [--json] [--no-color] [--ascii]`
/// (issues #158/#159). Positionals are the account filter; `--period`/`--since` take a
/// value (space- or `=`-separated, handled by lexopt). Validation lives in `stats::run`.
fn parse_stats(parser: &mut lexopt::Parser) -> Result<Command> {
    let mut accounts = Vec::new();
    let mut period = None;
    let mut since = None;
    let mut json = false;
    let mut no_color = false;
    let mut ascii = false;
    while let Some(arg) = parser.next()? {
        match arg {
            Short('h') | Long("help") => return Ok(Command::Help(HelpTopic::Stats)),
            Long("json") => json = true,
            Long("no-color") => no_color = true,
            Long("ascii") => ascii = true,
            Long("period") => {
                period = Some(
                    required_value(parser, "period", HelpTopic::Stats)?
                        .to_string_lossy()
                        .into_owned(),
                );
            }
            Long("since") => {
                since = Some(
                    required_value(parser, "since", HelpTopic::Stats)?
                        .to_string_lossy()
                        .into_owned(),
                );
            }
            Value(value) => accounts.push(value.to_string_lossy().into_owned()),
            other => return Err(unexpected(other, HelpTopic::Stats)),
        }
    }
    Ok(Command::Stats(crate::stats::StatsArgs {
        accounts,
        period,
        since,
        json,
        no_color,
        ascii,
    }))
}

/// Parse `export [PATH] [--plaintext] [--no-secrets] [--passphrase-file <path> |
/// --passphrase-stdin]` (issue #148) — the first non-flag token is the PATH, extras
/// ignored. The passphrase source is NEVER an argv value (#39): `--passphrase-file` takes
/// a path, `--passphrase-stdin` a flag; both resolve to an `Encryption` in `execute`.
fn parse_export(parser: &mut lexopt::Parser) -> Result<Command> {
    let mut path = None;
    let mut no_secrets = false;
    let mut plaintext = false;
    let mut passphrase_file = None;
    let mut passphrase_stdin = false;
    while let Some(arg) = parser.next()? {
        match arg {
            Short('h') | Long("help") => return Ok(Command::Help(HelpTopic::Export)),
            Long("plaintext") => plaintext = true,
            Long("no-secrets") => no_secrets = true,
            Long("passphrase-stdin") => passphrase_stdin = true,
            Long("passphrase-file") => {
                passphrase_file = Some(PathBuf::from(required_value(
                    parser,
                    "passphrase-file",
                    HelpTopic::Export,
                )?));
            }
            Value(value) if path.is_none() => path = Some(PathBuf::from(value)),
            Value(_) => {}
            other => return Err(unexpected(other, HelpTopic::Export)),
        }
    }
    Ok(Command::Export {
        path,
        no_secrets,
        plaintext,
        passphrase_file,
        passphrase_stdin,
    })
}

/// Parse `import <PATH> [--overwrite] [--passphrase-file <path> | --passphrase-stdin]`
/// (issue #149) — the first non-flag token is the required PATH (a missing one is
/// `MigrationImportPathRequired`, preserved from the prior dispatch), extras ignored. The
/// passphrase source is NEVER an argv value (#39), resolved to a `PassphraseSource` in
/// `execute`.
fn parse_import(parser: &mut lexopt::Parser) -> Result<Command> {
    let mut path = None;
    let mut overwrite = false;
    let mut passphrase_file = None;
    let mut passphrase_stdin = false;
    while let Some(arg) = parser.next()? {
        match arg {
            Short('h') | Long("help") => return Ok(Command::Help(HelpTopic::Import)),
            Long("overwrite") => overwrite = true,
            Long("passphrase-stdin") => passphrase_stdin = true,
            Long("passphrase-file") => {
                passphrase_file = Some(PathBuf::from(required_value(
                    parser,
                    "passphrase-file",
                    HelpTopic::Import,
                )?));
            }
            Value(value) if path.is_none() => path = Some(PathBuf::from(value)),
            Value(_) => {}
            other => return Err(unexpected(other, HelpTopic::Import)),
        }
    }
    let path = path.ok_or(Error::MigrationImportPathRequired)?;
    Ok(Command::Import {
        path,
        overwrite,
        passphrase_file,
        passphrase_stdin,
    })
}

/// Map `argv` (already past `argv[0]`) to a [`Command`], or a strict-usage error. The
/// argv-level meta options come first: no args or `-h`/`--help` is the root overview,
/// `-V`/`--version` is the version, an unknown leading flag is rejected. Otherwise the
/// first positional is the subcommand and its parser takes over. Pure — no I/O — so the
/// whole surface is unit-testable (issue #175).
fn parse<I>(args: I) -> Result<Command>
where
    I: IntoIterator,
    I::Item: Into<OsString>,
{
    let mut parser = lexopt::Parser::from_args(args);
    match parser.next()? {
        None => Ok(Command::Help(HelpTopic::Root)),
        Some(Short('h') | Long("help")) => Ok(Command::Help(HelpTopic::Root)),
        Some(Short('V') | Long("version")) => Ok(Command::Version),
        Some(Value(name)) => parse_subcommand(&name, &mut parser),
        Some(other) => Err(unexpected(other, HelpTopic::Root)),
    }
}

/// Route a subcommand name to its parser (the remainder of `argv` is consumed there). An
/// unrecognized name is `UnknownCommand` — unchanged from the prior dispatch.
fn parse_subcommand(name: &OsStr, parser: &mut lexopt::Parser) -> Result<Command> {
    match name.to_string_lossy().as_ref() {
        "capture" => parse_positional(parser, HelpTopic::Capture, |label| Command::Capture {
            label,
        }),
        "login" => parse_positional(parser, HelpTopic::Login, |label| Command::Login { label }),
        "run" => parse_run(parser),
        "status" => parse_status(parser),
        "list" => parse_list(parser),
        "use" => parse_use(parser),
        "disable" => parse_positional(parser, HelpTopic::Disable, |label| Command::SetEnabled {
            label,
            enabled: false,
        }),
        "enable" => parse_positional(parser, HelpTopic::Enable, |label| Command::SetEnabled {
            label,
            enabled: true,
        }),
        "remove" => parse_positional(parser, HelpTopic::Remove, |label| Command::Remove { label }),
        "poke" => parse_positional(parser, HelpTopic::Poke, |target| Command::Poke { target }),
        "stats" => parse_stats(parser),
        "export" => parse_export(parser),
        "import" => parse_import(parser),
        other => Err(Error::UnknownCommand(other.to_owned())),
    }
}

/// The `--version` line (issue #175): the crate name plus `CARGO_PKG_VERSION`, the sole
/// version source (`Cargo.toml`). Extracted so the parser test can assert its content
/// without capturing stdout.
fn version_line() -> &'static str {
    concat!("sessiometer ", env!("CARGO_PKG_VERSION"))
}

/// Run a parsed [`Command`]. The inverse of `parse`: this half owns the I/O (keychain,
/// roster, daemon socket), so `parse` can stay pure and testable.
async fn execute(command: Command) -> Result<()> {
    match command {
        Command::Capture { label } => crate::capture::capture(label).await,
        Command::Login { label } => crate::capture::login(label).await,
        Command::Run { verbose } => {
            let verbosity = if verbose {
                Verbosity::Verbose
            } else {
                Verbosity::Quiet
            };
            run(verbosity).await
        }
        Command::Status {
            json,
            no_color,
            verbose,
        } => status(json, no_color, verbose).await,
        Command::List => list().await,
        Command::Use { target, force } => crate::use_account::use_account(target, force).await,
        Command::SetEnabled { label, enabled } => set_enabled(label, enabled).await,
        Command::Remove { label } => remove_account(label).await,
        Command::Poke { target } => crate::poke::poke(target).await,
        Command::Stats(args) => crate::stats::run(args).await,
        Command::Export {
            path,
            no_secrets,
            plaintext,
            passphrase_file,
            passphrase_stdin,
        } => {
            export(
                path,
                no_secrets,
                export_encryption(plaintext, passphrase_file, passphrase_stdin),
            )
            .await
        }
        Command::Import {
            path,
            overwrite,
            passphrase_file,
            passphrase_stdin,
        } => {
            import(
                path,
                overwrite,
                import_passphrase(passphrase_file, passphrase_stdin),
            )
            .await
        }
        Command::Version => {
            println!("{}", version_line());
            Ok(())
        }
        Command::Help(topic) => {
            print!("{}", topic.help());
            Ok(())
        }
    }
}

/// The top-level overview: the command list plus the two argv-level meta options
/// (`--version`, `--help`). Printed for `sessiometer`, `sessiometer -h`/`--help`, and no
/// args at all. Issue #175 added the `OPTIONS` block (`-V`/`--version` and the
/// per-command-help note); the `COMMANDS` list is unchanged.
const ROOT_USAGE: &str = "sessiometer — manage multiple Claude Code accounts on macOS

USAGE:
    sessiometer <COMMAND> [OPTIONS]

COMMANDS:
    capture [<label>]    Stash the active account into the rotation
    login [<label>]      Log in to an account (claude /login) in isolation and add it to the rotation
    run [-v|--verbose]   Run the foreground daemon (poll + swap; -v adds run diagnostics)
    status [--json] [--no-color] [-v|--verbose]  Show each account's usage + resets-in, and the next swap (-v adds each access token's expiry)
    list       List captured accounts
    use <account> [--force]  Switch the active account now (--force overrides the pre-swap gate)
    disable <label>      Park an account: keep it but take it out of the rotation
    enable <label>       Return a parked account to the rotation
    remove <label>       Delete an account: drop it from the rotation and erase its stash
    poke [<account>]     Run Claude Code once in an isolated config dir so it refreshes a parked account's credential (all near-expiry if omitted)
    stats [<account>...] [--period day|week|month|lifetime] [--since <when>] [--json]  Show usage over a period, offline (reads the sample store directly)
    export [PATH] [--plaintext] [--no-secrets] [--passphrase-stdin]  Serialize state to an (encrypted by default) migration artifact — a file (0600) or stdout
    import <PATH> [--overwrite] [--passphrase-stdin]  Rehydrate accounts from a migration artifact — skips accounts already present unless --overwrite

OPTIONS:
    -h, --help     Print help (append it to a command for that command's usage)
    -V, --version  Print version

Run `sessiometer <command> --help` for command-specific usage.
";

/// Per-subcommand help (issue #175): a one-line summary, a usage line, then the accepted
/// arguments and flags. Each is what `sessiometer <verb> --help` prints and matches the
/// flags the corresponding `parse_*` accepts, so help and parser stay in lockstep.
const CAPTURE_USAGE: &str = "sessiometer capture — stash the active account into the rotation

USAGE:
    sessiometer capture [<label>]

    <label>     a name for the captured account (auto-derived from its account-uuid if omitted)
    -h, --help  print this help
";

const LOGIN_USAGE: &str = "sessiometer login — log in to an account (claude /login) in isolation and add it to the rotation

USAGE:
    sessiometer login [<label>]

    <label>     a name for the new account (auto-derived from its account-uuid if omitted)
    -h, --help  print this help
";

const RUN_USAGE: &str = "sessiometer run — run the foreground daemon (poll every account's usage and swap before exhaustion)

USAGE:
    sessiometer run [-v|--verbose]

    -v, --verbose  emit per-tick run diagnostics on stderr
    -h, --help     print this help
";

const STATUS_USAGE: &str = "sessiometer status — show each account's usage + resets-in and the next swap (needs a running daemon)

USAGE:
    sessiometer status [--json] [--no-color] [-v|--verbose]

    --json         print the raw status response, uncoloured (for scripts)
    --no-color     force the urgency colour overlay off
    -v, --verbose  add each account's access-token expiry under the table
    -h, --help     print this help
";

const LIST_USAGE: &str =
    "sessiometer list — list captured accounts (offline; reads the roster directly)

USAGE:
    sessiometer list

    -h, --help  print this help
";

const USE_USAGE: &str = "sessiometer use — switch the active account now

USAGE:
    sessiometer use <account> [--force]

    <account>   the target account (its label or account-uuid)
    --force     override the pre-swap gate
    -h, --help  print this help
";

const DISABLE_USAGE: &str =
    "sessiometer disable — park an account: keep it but take it out of the rotation

USAGE:
    sessiometer disable <label>

    <label>     the account to park (its label)
    -h, --help  print this help
";

const ENABLE_USAGE: &str = "sessiometer enable — return a parked account to the rotation

USAGE:
    sessiometer enable <label>

    <label>     the parked account to re-enable (its label)
    -h, --help  print this help
";

const REMOVE_USAGE: &str =
    "sessiometer remove — delete an account: drop it from the rotation and erase its stash

USAGE:
    sessiometer remove <label>

    <label>     the account to delete (its label)
    -h, --help  print this help
";

const POKE_USAGE: &str = "sessiometer poke — run Claude Code once in an isolated config dir to refresh a parked account's credential

USAGE:
    sessiometer poke [<account>]

    <account>   the parked account to refresh (all near-expiry parked accounts if omitted)
    -h, --help  print this help
";

const STATS_USAGE: &str = "sessiometer stats — show usage over a period, offline (reads the sample store directly)

USAGE:
    sessiometer stats [<account>...] [--period day|week|month|lifetime] [--since <when>] [--json] [--no-color] [--ascii]

    <account>...    filter to these accounts (all if omitted)
    --period <p>    look-back window: day, week (default), month, or lifetime
    --since <when>  look back to a time (e.g. 7d, 24h, or YYYY-MM-DD); exclusive with --period
    --json          print the raw stats, uncoloured (for scripts)
    --no-color      force the chart colour overlay off
    --ascii         force the ASCII glyph ramp
    -h, --help      print this help
";

const EXPORT_USAGE: &str = "sessiometer export — serialize state to an (encrypted by default) migration artifact

USAGE:
    sessiometer export [PATH] [--plaintext] [--no-secrets] [--passphrase-file <path> | --passphrase-stdin]

    PATH                   write the artifact here (0600); stdout if omitted
    --plaintext            do not encrypt (warned when it carries secrets)
    --no-secrets           drop every credential blob (config-only artifact)
    --passphrase-file <p>  read the passphrase from a file (never from argv)
    --passphrase-stdin     read the passphrase from standard input
    -h, --help             print this help
";

const IMPORT_USAGE: &str = "sessiometer import — rehydrate accounts from a migration artifact

USAGE:
    sessiometer import <PATH> [--overwrite] [--passphrase-file <path> | --passphrase-stdin]

    PATH                   the artifact to import (required)
    --overwrite            replace accounts already present (skip them otherwise)
    --passphrase-file <p>  read the passphrase from a file (never from argv)
    --passphrase-stdin     read the passphrase from standard input
    -h, --help             print this help
";

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
    .with_swap_lock(paths::swap_lock()?)
    // Re-read this on a runtime roster-reload (#139): a `capture` / `login` / `remove`
    // notifies the daemon over the control socket, which then reconciles the in-memory
    // rotation to the freshly-written `config.toml` without a restart.
    .with_config_path(paths::config_file()?)
    // Maintain the usage-stats store (#161): compact + roll aged samples under the operator's
    // `[stats]` retention horizons, emitting redacted `usage_rollup` / `usage_gap` events. The
    // poll cadence is the daily-coverage denominator, so it is threaded in from `[tunables]`.
    .with_stats(
        config
            .stats
            .retention_policy(config.tunables.poll_secs as i64),
    );
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
    // Reap any isolated-refresh artifacts (issue #103) a crashed cycle (SIGKILL /
    // power-loss — no RAII teardown) may have stranded: the single-instance lock above
    // guarantees no cycle is in flight, so a present isolated item/dir for a roster
    // account is an orphan still holding a live credential. Best-effort and
    // roster-scoped — a sibling of `run_loop`'s reconcile-on-start, kept HERE rather
    // than inside `run_loop` so the hermetic loop tests never spawn `/usr/bin/security`.
    let roster_uuids: Vec<String> = config
        .roster
        .iter()
        .map(|account| account.account_uuid.clone())
        .collect();
    refresh::reap_orphans(&roster_uuids).await;
    // …and the login isolation root (issue #133): a crashed `claude /login` (SIGKILL / power-loss —
    // no RAII teardown) can strand a credential-bearing isolated item + dir under `<support>/login`.
    // Folded in beside the roster reap under the same single-instance lock (no login is in flight),
    // scan-based (the fixed login dir is not roster-keyed). Best-effort — never blocks daemon start.
    refresh::reap_login_orphan().await;

    // The periodic isolated-refresh tick (issue #105): opt-in, driven from `run_loop`'s idle
    // path off the poll→usage→swap seam. Resolve the spawn binary ONLY when enabled — a
    // resolution failure DISABLES the tick (logged) rather than failing the daemon, whose
    // core job is polling/swapping. When disabled the tick is wholly inert.
    let refresh_enabled = config.refresh.enabled;
    let claude_binary = if refresh_enabled {
        match paths::claude_binary_with_override(config.refresh.claude_bin.as_deref()) {
            Ok(bin) => Some(bin),
            Err(err) => {
                eprintln!(
                    "sessiometer: periodic refresh disabled — cannot resolve the claude binary: {err}"
                );
                None
            }
        }
    } else {
        None
    };
    // Issue #162: the poll path's refresh-then-retry reuses the SAME #102 engine, so a
    // usage 401 (usually a merely-expired access token) attempts one isolated refresh +
    // re-poll BEFORE it counts toward the #42 dead-credential streak — closing the
    // false-death window the ~10×-slower periodic sweep (#105) structurally cannot. Gated
    // on the SAME effective switch as the periodic tick (a resolvable `claude` binary);
    // with refresh off the seam stays unset and the poll path behaves exactly as before.
    // `as_ref` + `clone` so `claude_binary` is still owned for the tick's engine below.
    if let Some(bin) = claude_binary.as_ref() {
        daemon = daemon.with_refresh_engine(Box::new(RealRefreshEngine::new(
            RealAccountStash::new(),
            bin.clone(),
        )));
    }
    let mut refresh_tick = RefreshTick::new(
        config.roster.clone(),
        config.refresh.clone(),
        refresh_enabled && claude_binary.is_some(),
        RealRefreshEngine::new(
            RealAccountStash::new(),
            // Unused while the tick is disabled (the effective-enabled flag above gates every
            // spawn); a placeholder keeps the engine total.
            claude_binary.unwrap_or_else(|| PathBuf::from("claude")),
        ),
        RealClock::new(),
    );
    // The external-login watch (issue #140): a short-cadence LOCAL probe of the canonical item
    // over its OWN `RealCredentialStore`, driven from `run_loop`'s idle path, so a manual
    // `claude /login` on the active account is reflected within `EXTERNAL_LOGIN_WATCH_SECS`
    // instead of up to a full `poll_secs`. Always-on (no feature gate — a cheap local read); its
    // own store leaves the daemon's untouched by the idle borrow.
    let mut login_watch = ExternalLoginWatcher::new(RealCredentialStore::new());

    let result = run_loop(
        &mut daemon,
        &mut log,
        &mut diag,
        &mut shutdown,
        &control,
        &mut refresh_tick,
        &mut login_watch,
    )
    .await;
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
///
/// `verbose` (`-v`/`--verbose`, issue #143) appends the per-account access-token
/// expiry block under the table — the raw "expires in" clock, labelled so it is not
/// misread as a re-login deadline. It affects only the text view: `--json` already
/// carries the raw `access_expires_at` for every account (the full-data contract), so
/// verbose adds nothing there.
async fn status(json: bool, no_color: bool, verbose: bool) -> Result<()> {
    let response = query_status(&paths::control_socket()?).await?;
    if json {
        // The full-data contract, regardless of terminal width (issue #72): the
        // raw response — both per-account reset instants AND the raw access-token
        // expiry included — pretty-printed, for scripts (`status --json | jq`).
        // Sourced from the same non-secret response as the text view, so it too can
        // never carry a token or email. Never colored — scripts consume the bytes
        // verbatim; `--verbose` is inert here (the raw clock is already present).
        let rendered = serde_json::to_string_pretty(&response)
            .map_err(|err| Error::Io(std::io::Error::other(err)))?;
        println!("{rendered}");
    } else {
        let color = should_colorize(no_color);
        // One `now` for both the table's "resets in" and the verbose expiry block, so
        // the two never read against different clocks within a single render.
        let now = now_epoch();
        print!("{}", render_status(&response, now, terminal_cols(), color));
        // The verbose access-token expiry block (issue #143) trails the table — content,
        // not color, so it shows through a pipe like the rest of the table (the
        // color gate governs only the ANSI overlay).
        if verbose {
            print!("{}", render_access_token_expiry(&response, now));
        }
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

/// Render a [`StatusResponse`] as the text `status` prints: an aligned table with a
/// labelled header row (issue #99), one record per line, then the next-swap footer
/// (#88). Pure (no clock, no I/O) so the response→text mapping is unit-testable —
/// the caller passes `now` (epoch seconds) so each account's "resets in" and
/// urgency are deterministic, `cols` (the terminal width, or `None` when stdout is
/// not a TTY) so the narrow-terminal column degradation is testable, and `color`
/// (whether the color gate is open; [`should_colorize`]) so the ANSI overlay is too.
///
/// Columns, in display order: `account` then the SESSION pair (`session% `
/// `session-reset`), then the WEEKLY pair (`weekly% ` `weekly-reset`), then the
/// health-text tags (issue #94). A labelled header row (issue #99) tops the table —
/// `ACCOUNT`, the grouped `SESSION%` + `RESET`, the grouped `WEEKLY%` + `RESET`, then
/// `AUTH` — measured into the SAME column widths as the data so the labels line up;
/// the pairing is also read by adjacency (each `%` sits immediately before its OWN
/// reset), so the two reset columns share the `RESET` label. A reset's lead gap is a
/// single space (tying it to its `%`); independent columns are two spaces apart. When
/// the full table is wider than `cols`, the lowest-priority columns drop — the WEEKLY
/// pair (`weekly%` + `weekly-reset`) FIRST and ATOMICALLY (never a `%` stranded
/// without its reset), then the health-text column, each taking its own header label
/// with it — never wrapping a row; `account` + the SESSION pair (the soonest, most
/// actionable reset) and their labels are always kept. A `None` width (piped /
/// redirected) keeps the full table, so `status | grep` and `status > file` stay the
/// complete, greppable surface.
///
/// When `color` is set each CELL is tinted by its OWN health (issue #84), so one
/// glance reads several independent signals per account: `account` by the overall
/// urgency ([`severity`]), each `%` by its window's own utilization band
/// ([`util_severity`] / [`weekly_cell_severity`]), and each reset by its OWN
/// PROXIMITY ([`proximity_severity`], issue #94) — an imminent session reset reads
/// green (relief arriving) while a far weekly reset on the same row is dimmed
/// (issue #90). (The health-text tags
/// stay untinted: they are their own signal.) The color AUGMENTS — it wraps the
/// already-padded text, so a no-color reader still sees every state, percentage, and
/// reset; it is never the only signal. Padding is computed on DISPLAY WIDTH from the
/// raw cell and applied BEFORE the color (pad-before-color), so per-cell colored and
/// multibyte rows stay aligned and the escape bytes never enter the column-width
/// math. The untinted health-text column, and any cell with no reading (nothing to
/// classify — `n/a` is not a false "healthy") stay uncolored.
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

    // Display order (issue #94): `account`, then the SESSION pair (% + its reset),
    // then the WEEKLY pair, then the health-text tags. Each column carries a lead
    // gap (the spaces BEFORE it): `0` for the first column, `1` to tie a reset
    // tightly to the `%` it pairs with, `2` between independent columns — so each
    // `%` reads immediately followed by its own reset, the pairing the header row
    // (issue #99) also labels. A drop priority of `None` always keeps the column; the
    // WEEKLY pair shares priority 1 so both leave atomically (never a `%` without its
    // reset); the health-text column is priority 2 (drops next). The health-text
    // column is included only when some account carries a tag — an all-healthy roster
    // shows none.
    let mut columns: Vec<Column> = vec![
        Column::keep("ACCOUNT", |row| &row.account, |row| row.account_severity, 0),
        Column::keep(
            "SESSION%",
            |row| &row.session,
            |row| row.session_severity,
            STATUS_COL_GAP,
        ),
        Column::keep(
            "RESET",
            |row| &row.session_reset,
            |row| row.session_reset_severity,
            STATUS_PAIR_GAP,
        ),
        Column::droppable(
            "WEEKLY%",
            1,
            |row| &row.weekly,
            |row| row.weekly_severity,
            STATUS_COL_GAP,
        ),
        Column::droppable(
            "RESET",
            1,
            |row| &row.weekly_reset,
            |row| row.weekly_reset_severity,
            STATUS_PAIR_GAP,
        ),
    ];
    if rows.iter().any(|row| !row.status.is_empty()) {
        // The AUTH column carries the credential-auth state — the 4-state+Unknown glyph
        // (issue #119/#137) plus its cues (`claude /login` on 🔴, `recovering`, `disabled`);
        // it is never tinted (issue #84) — the glyph is self-coloring and the tags are their
        // own signal, so its severity getter is always `None`. Its header is `AUTH` (issue
        // #143, renamed from the over-general `HEALTH` of issue #99 — this column reports
        // the credential-AUTH standing, while rate-limit health lives in `SESSION%`/`WEEKLY%`).
        columns.push(Column::droppable(
            "AUTH",
            2,
            |row| &row.status,
            |_| None,
            STATUS_COL_GAP,
        ));
    }

    // Drop the lowest-priority droppable columns until the table fits `cols`. ALL
    // columns sharing the lowest present priority drop together, so the WEEKLY pair
    // (both priority 1) leaves atomically — never a weekly `%` stranded without its
    // reset. A non-TTY width (`None`) never enters the loop — the full table is kept.
    while let Some(width) = cols {
        if table_width(&columns, &rows) <= width {
            break;
        }
        match columns.iter().filter_map(|col| col.drop_priority).min() {
            Some(min_priority) => columns.retain(|col| col.drop_priority != Some(min_priority)),
            // Only keep-columns remain: never wrap, just let the essential columns
            // overflow a very narrow terminal (predictable, one record per line).
            None => break,
        }
    }

    let widths = column_widths(&columns, &rows);
    let lead_gaps: Vec<usize> = columns.iter().map(|col| col.lead_gap).collect();
    let mut out = String::new();
    // Header row (issue #99): a plain, uncolored label per column, padded to the SAME
    // measured widths as the data so labels and values line up. Printed in the text
    // view regardless of the colour gate or TTY (it is never in `--json`, the separate
    // full-data contract). Skipped only for an empty roster — a lone header labelling
    // no data would mislead. Whichever columns survive the narrow-terminal drop above
    // carry their labels with them, so a dropped WEEKLY pair takes its `WEEKLY%`/`RESET`
    // labels too while `ACCOUNT` + the always-kept SESSION pair keep theirs.
    if !rows.is_empty() {
        let headers: Vec<&str> = columns.iter().map(|col| col.header).collect();
        let uncolored: Vec<Option<&str>> = vec![None; columns.len()];
        out.push_str(&render_cells(&headers, &widths, &uncolored, &lead_gaps));
    }
    for row in &rows {
        let cells: Vec<&str> = columns.iter().map(|col| (col.get)(row)).collect();
        // Each cell is tinted by its OWN health when the gate is open (issue #84), so
        // one row can show several independent colors; a cell with no reading, and the
        // whole no-color path, stay uncolored.
        let colors: Vec<Option<&str>> = columns
            .iter()
            .map(|col| {
                color
                    .then(|| (col.severity)(row).map(Severity::sgr))
                    .flatten()
            })
            .collect();
        out.push_str(&render_cells(&cells, &widths, &colors, &lead_gaps));
    }

    out.push('\n');
    // The forward-looking next-swap candidate (issue #88), computed daemon-side
    // ([`crate::daemon::NextSwap`]); printed plain — the footer carries no color, like
    // the table footer it replaces (per-cell health coloring is #84, orthogonal). A
    // `None` field means the daemon sent no candidate — either a current daemon with no
    // active account to anchor a swap from, or (via `#[serde(default)]`) a pre-#88 daemon
    // that omits the field — and renders a bare `none` either way.
    match &response.next_swap {
        Some(NextSwap::Target { to }) => out.push_str(&format!("next swap: {to}\n")),
        Some(NextSwap::NoViableTarget) => out.push_str("next swap: none (no viable target)\n"),
        Some(NextSwap::AwaitingData) => out.push_str("next swap: none (awaiting usage data)\n"),
        None => out.push_str("next swap: none\n"),
    }
    out
}

/// Gap between adjacent independent `status`-table columns (two spaces, matching
/// `list`).
const STATUS_COL_GAP: usize = 2;
/// Tighter gap that ties a reset to the `%` it pairs with (issue #94): one space, so
/// `session% session-reset` reads as one pair, disambiguated by adjacency and labelled
/// by the header row (issue #99 — each window's reset under its own `RESET` label).
const STATUS_PAIR_GAP: usize = 1;

/// One account projected to its `status`-table cells (issue #72). Pre-rendered
/// strings so column widths can be measured uniformly across header + rows.
struct StatusRow {
    /// `* label` (active) or `  label` — the marker folds into this column.
    account: String,
    /// SESSION usage percent, or `n/a` when the last poll failed.
    session: String,
    /// Compact time until the SESSION window resets, or `n/a` when that instant is
    /// unknown (issue #94).
    session_reset: String,
    /// WEEKLY usage percent, or `n/a`.
    weekly: String,
    /// Compact time until the WEEKLY window resets, or `n/a` (issue #94).
    weekly_reset: String,
    /// The AUTH cell (issue #119): the daemon's 4-state credential rollup as ONE glyph
    /// (🟢 healthy · 🟡 stale · 🟠 at-risk · 🔴 dead), with the `claude /login` cue appended
    /// for a dead account — softened to `recovering` for a healing quarantined one (#109) —
    /// and a trailing `disabled` for a parked account (#36, orthogonal to credential health).
    /// Falls back to the legacy comma-joined tags (`disabled`, `needs re-login` / `recovering`)
    /// when the daemon sent no rollup (a pre-#119 daemon, `health == None`). Empty only for a
    /// pre-#119 daemon with no tags.
    status: String,
    /// Per-cell urgency for the color overlay (issue #84): each cell carries its OWN
    /// health, so one row can show several independent colors (a red `session` reset
    /// beside a green `weekly` reset, etc.). Each is `None` when its cell has no
    /// reading — that cell is then printed without color, since absence of color is
    /// not a false "healthy" signal. `account` is the OVERALL (binding-window)
    /// [`severity`]; `session` / `weekly` the [`util_severity`] /
    /// [`weekly_cell_severity`] utilization bands on each `%`; each reset its OWN
    /// [`proximity_severity`] (issue #94) — how soon that window flips, independent
    /// of utilization. The health-text column is never tinted (its tags are their own
    /// signal), so it has no field here.
    account_severity: Option<Severity>,
    session_severity: Option<Severity>,
    session_reset_severity: Option<Severity>,
    weekly_severity: Option<Severity>,
    weekly_reset_severity: Option<Severity>,
}

impl StatusRow {
    fn new(account: &AccountStatusLine, now: i64) -> Self {
        // `*` marks the active account (as the event log does); a leading space
        // keeps the inactive labels aligned under it.
        let marker = if account.active { '*' } else { ' ' };
        StatusRow {
            account: format!("{marker} {}", account.label),
            session: pct(account.session_pct),
            session_reset: reset_cell(account.session_resets_at, now),
            weekly: pct(account.weekly_pct),
            weekly_reset: reset_cell(account.weekly_resets_at, now),
            status: health_cell(account),
            // Each cell colored by its OWN health (issue #84): `account` → the overall
            // binding-window severity; `session` / `weekly` `%` → each window's own
            // utilization bands (weekly honoring the exhaustion override); each reset →
            // its OWN proximity (issue #94), how soon that window flips. A cell with no
            // reading stays `None` (uncolored).
            account_severity: severity(account, now),
            session_severity: account.session_pct.map(util_severity),
            session_reset_severity: proximity_severity(account.session_resets_at, now),
            weekly_severity: weekly_cell_severity(account),
            weekly_reset_severity: proximity_severity(account.weekly_resets_at, now),
        }
    }
}

/// The `status` AUTH cell for one account (issue #119): the daemon's 4-state credential
/// rollup as ONE glyph plus the minimal cue an operator needs to act, with the `disabled`
/// rotation tag (#36) — orthogonal to credential health — appended.
///
/// `health == Some(verdict)` (a current daemon) renders the glyph; a DEAD account carries
/// the actionable `claude /login` cue (AC-1), softened to `recovering` for a healing
/// quarantined account so the operator neither re-logs-in needlessly nor swaps away from a
/// recovering — often healthier — account (#109). `health == None` (a pre-#119 daemon that
/// sent no rollup) falls back to the legacy comma-joined tags, so an old daemon's `status`
/// is unchanged rather than mis-reading a defaulted glyph over a dead account.
fn health_cell(account: &AccountStatusLine) -> String {
    let Some(health) = account.health else {
        return legacy_health_tags(account);
    };
    let mut cell = health_glyph(health).to_owned();
    if health == CredentialHealth::Dead {
        // A healing account (#109) reads `recovering`, not the `claude /login` command, so
        // the operator holds rather than re-authing or swapping away; a genuinely dead one
        // gets the exact command to run.
        cell.push(' ');
        cell.push_str(if account.recovering {
            "recovering"
        } else {
            "claude /login"
        });
    }
    // `disabled` (rotation #36) is independent of credential health — a parked account can
    // be perfectly healthy — so it trails the glyph rather than replacing it.
    if !account.enabled {
        cell.push_str(" disabled");
    }
    cell
}

/// The emoji glyph for a 4-state rollup verdict (issue #119). Self-coloring (the glyph is
/// content, not an ANSI overlay), so it conveys state even under `--no-color` and through a
/// pipe; `display_width` already measures each as two terminal cells (emoji-presentation
/// glyphs, per `unicode-width`), so the table stays aligned.
fn health_glyph(health: CredentialHealth) -> &'static str {
    match health {
        CredentialHealth::Healthy => "🟢",
        // #137: no positive-liveness evidence — a neutral ⚪, not a false 🟢. `display_width`
        // measures U+26AA as two cells (emoji-presentation, per `unicode-width`) so the column stays aligned.
        CredentialHealth::Unknown => "⚪",
        CredentialHealth::Stale => "🟡",
        CredentialHealth::AtRisk => "🟠",
        CredentialHealth::Dead => "🔴",
    }
}

/// The pre-#119 AUTH-column text for an account whose daemon sent no rollup (`health == None`):
/// the comma-joined `disabled` (#36) + `needs re-login` / `recovering` (#42/#109) tags the
/// column carried before the glyph rollup. Kept so a `status` client talking to an older
/// daemon degrades gracefully rather than showing a defaulted-healthy glyph over a dead
/// account.
fn legacy_health_tags(account: &AccountStatusLine) -> String {
    let mut status = String::new();
    if !account.enabled {
        status.push_str("disabled");
    }
    if account.quarantined {
        if !status.is_empty() {
            status.push_str(", ");
        }
        status.push_str(if account.recovering {
            "recovering"
        } else {
            "needs re-login"
        });
    }
    status
}

/// One urgency band for the `status` color overlay (issue #73), carried per CELL
/// since issue #84: how much you can rely on what that cell reports at a glance.
///
/// - `Green` — healthy: plenty of quota, usable now (util cells); OR a reset that is
///   imminent, i.e. fresh quota is arriving (reset cells, issue #90).
/// - `Yellow` — getting depleted, OR heavily used but about to reset (recovering);
///   OR a reset that is approaching (reset cells).
/// - `Red` — heavily used and not about to reset: the least-available (util cells).
/// - `Dim` — de-emphasis, NOT an urgency: a reset that is far off — the window just
///   reset, so there is nothing to act on. Used only by the reset cells
///   ([`proximity_severity`]); it renders faint rather than alarming, because a
///   just-reset account is the *healthiest* state, not an emergency (issue #90).
///
/// Purely a redundant overlay on the `SESSION`/`WEEKLY` percentages and the
/// `RESETS` time the row already prints — the text stands alone without color
/// (color augments, never the sole signal).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Severity {
    Green,
    Yellow,
    Red,
    Dim,
}

impl Severity {
    /// The ANSI SGR code for this severity (`32`/`33`/`31` = green/yellow/red
    /// foreground; `2` = faint intensity for the de-emphasized far-reset cell).
    /// Emitted only when the color gate is open ([`should_colorize`]); the codes
    /// carry no secret (issue #15).
    fn sgr(self) -> &'static str {
        match self {
            Severity::Green => "32",
            Severity::Yellow => "33",
            Severity::Red => "31",
            Severity::Dim => "2",
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
/// SAME window its WEEKLY reset cell shows — and is at least Red: a week-blocked account
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

/// A reset at/under this many seconds out reads as IMMINENT — fresh quota is
/// arriving, so the cell reads Green, the relief band (issue #94; direction
/// corrected per issue #90 — a soon reset is good news, not an alarm).
const RESET_IMMINENT_SECS: i64 = 60 * 60;
/// A reset beyond this many seconds out reads as FAR — the window just reset, so
/// relief is a long way off; the cell is de-emphasized (Dim), not alarmed. A reset
/// between [`RESET_IMMINENT_SECS`] and this is APPROACHING (Yellow) (issue #94).
const RESET_FAR_SECS: i64 = 24 * 60 * 60;

/// One reset cell's own reading (issue #94): its PROXIMITY, not utilization, framed
/// as RELIEF. The cell answers "how soon does THIS window flip" — a sooner reset
/// means fresh quota is arriving, so it reads Green (good); a far reset means the
/// window just reset and relief is a long way off, so it is de-emphasized (Dim)
/// rather than alarmed — independent of how depleted the account is. Bands: at/under
/// [`RESET_IMMINENT_SECS`] (1h) Green; beyond [`RESET_FAR_SECS`] (1d) Dim; in between
/// Yellow. A reset already past (non-positive delta) is maximally imminent → Green
/// (the window is fully available). `None` when the reset instant is unknown — the
/// cell shows `n/a`, which stays uncolored (absence of color must not read as a false
/// "healthy").
///
/// This RELIEF direction is intentionally CONSISTENT with the account-overall
/// [`severity`], which also treats an imminent reset as good (a depleted account
/// about to reset is recovering, so its `account` cell softens Red→Yellow). The two
/// still answer different questions — `account` "how usable is this account", a reset
/// cell "how soon does this window flip" — and per the #84 model each cell's signal
/// is independent; but they no longer disagree on whether a soon reset is good.
/// Corrected per issue #90: the cell previously read sooner = redder, which inverted
/// the relief signal and painted an imminent reset as an alarm and a just-reset,
/// full-quota account as green. Now a soon reset is Green and a far reset is Dim (not
/// red), so the freshest account is not mistaken for one that needs attention.
fn proximity_severity(reset_at: Option<i64>, now: i64) -> Option<Severity> {
    let delta = reset_at? - now;
    Some(if delta <= RESET_IMMINENT_SECS {
        Severity::Green
    } else if delta > RESET_FAR_SECS {
        Severity::Dim
    } else {
        Severity::Yellow
    })
}

/// One `status`-table column (issue #94): a `header` label (issue #99), a borrow of
/// the matching [`StatusRow`] cell, the per-cell urgency getter for the color overlay
/// (issue #84), a `lead_gap` (the spaces rendered BEFORE this column — `0` for the
/// first column, `1` to tie a reset tightly to the `%` it pairs with, `2` between
/// independent columns), and a drop priority (`None` = always keep; `Some(n)` =
/// droppable, lower `n` drops first under a narrow terminal — all columns sharing the
/// lowest present priority drop together, so a `%`+reset PAIR leaves atomically). The
/// `header` is a plain (uncolored) label printed in the header row and measured into
/// the column width alongside the cells (issue #99), so it lines up with the data; the
/// adjacency of each `%` to its own reset still disambiguates the pairing, so the two
/// reset columns can share the `RESET` label. `severity` returns this column's own
/// health for a row, or `None` for a column that is never tinted (the health-text
/// tags) or a cell with no reading — the header itself is always rendered uncolored.
struct Column {
    header: &'static str,
    get: fn(&StatusRow) -> &str,
    severity: fn(&StatusRow) -> Option<Severity>,
    lead_gap: usize,
    drop_priority: Option<u8>,
}

impl Column {
    fn keep(
        header: &'static str,
        get: fn(&StatusRow) -> &str,
        severity: fn(&StatusRow) -> Option<Severity>,
        lead_gap: usize,
    ) -> Self {
        Column {
            header,
            get,
            severity,
            lead_gap,
            drop_priority: None,
        }
    }
    fn droppable(
        header: &'static str,
        priority: u8,
        get: fn(&StatusRow) -> &str,
        severity: fn(&StatusRow) -> Option<Severity>,
        lead_gap: usize,
    ) -> Self {
        Column {
            header,
            get,
            severity,
            lead_gap,
            drop_priority: Some(priority),
        }
    }
}

/// Each included column's render width: the widest of its HEADER label (issue #99)
/// and its cells, measured in DISPLAY WIDTH ([`display_width`]) — terminal columns,
/// not `char` count — so a wide (CJK) or zero-width glyph in a label sizes the column
/// correctly and the next column still lines up (issue #73). The header participates
/// in the measurement so a label wider than every cell (e.g. `SESSION%` over `82%`)
/// still gets its own room and the header and data stay aligned.
fn column_widths(columns: &[Column], rows: &[StatusRow]) -> Vec<usize> {
    columns
        .iter()
        .map(|col| {
            rows.iter()
                .map(|row| display_width((col.get)(row)))
                .max()
                .unwrap_or(0)
                .max(display_width(col.header))
        })
        .collect()
}

/// Total rendered width of the table: summed column widths plus each column's lead
/// gap. The first column's lead gap is `0`, so it never double-counts. Used to decide
/// whether columns must drop to fit the terminal.
fn table_width(columns: &[Column], rows: &[StatusRow]) -> usize {
    let cells: usize = column_widths(columns, rows).iter().sum();
    let gaps: usize = columns.iter().map(|col| col.lead_gap).sum();
    cells + gaps
}

/// Render one table line: each cell preceded by its column's `lead_gap` and
/// left-padded to its column width, with trailing whitespace trimmed (so an empty
/// trailing cell — a healthy account's health-text — leaves no dangling spaces and
/// the line stays greppable).
///
/// The lead gap is the spacing BEFORE a column (issue #94): `0` for the first column,
/// `1` to tie a reset to the `%` it pairs with, `2` between independent columns — so
/// each `%` reads immediately followed by its own reset. The same routine renders both
/// the header row (issue #99) and the data rows, so the labels and values share one
/// set of gaps and widths. Padding is computed on DISPLAY WIDTH ([`display_width`]) — not
/// `char`/byte count, which Rust's `{:<width$}` fill would use — so a wide-glyph cell
/// lands the next column correctly. `colors` carries one entry PER cell (issue #84):
/// when a cell's entry is `Some(sgr)` that cell's text is wrapped in the ANSI color,
/// and the color math is done on the RAW cell width so the escape bytes never enter
/// it — per-cell colors keep the columns aligned exactly as the old row-wide tint did
/// (pad-before-color, issue #73). The trailing pad is appended OUTSIDE the escape so
/// the line's trailing whitespace (an empty health-text cell, a short last cell) still
/// trims away cleanly, leaving no dangling spaces — and stripping every escape
/// recovers the exact plain table (color is purely additive). An entry is `None` for
/// an untinted column, a cell with no reading, and whenever the gate is closed — then
/// that cell emits not one escape byte, keeping a piped / redirected surface clean.
fn render_cells(
    cells: &[&str],
    widths: &[usize],
    colors: &[Option<&str>],
    lead_gaps: &[usize],
) -> String {
    let mut line = String::new();
    for (((cell, width), color), gap) in cells.iter().zip(widths).zip(colors).zip(lead_gaps) {
        line.push_str(&" ".repeat(*gap));
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
/// printed, which is NOT its `char` count for non-Latin text (issue #73). Measured
/// with the canonical UAX #11 table from the `unicode-width` crate (issue #176):
/// wide East Asian glyphs (CJK, Hangul, Kana, fullwidth forms) and default
/// emoji-presentation characters count two, combining marks and zero-width
/// characters count zero, everything else one — and, unlike the wcwidth this
/// replaced, it understands ZWJ sequences, regional-indicator flags, skin-tone
/// modifiers, and emoji variation selectors, so operator-provided account labels
/// carrying those glyphs stop misaligning the table. The old hand-roll approximated
/// the whole emoji block as uniformly width-2 and covered only the ranges seen in
/// practice; `unicode-width` is a solved, versioned Unicode table with ZERO
/// transitive dependencies, so adopting it keeps the crate's minimal-dependency
/// posture intact (see `Cargo.toml`) — the one hand-rolled primitive where the
/// canonical crate is strictly more correct at ~nil dependency cost.
///
/// `pub(crate)` so the `stats` charts (issue #159) size their columns on the SAME
/// terminal-cell width this `status` view does — one wcwidth for the whole crate.
pub(crate) fn display_width(s: &str) -> usize {
    UnicodeWidthStr::width(s)
}

/// One window's compact "resets in" (issue #94): the time until `reset_at`, or `n/a`
/// when that reset instant is unknown (the poll failed, or the API gave no parseable
/// timestamp) — never a fabricated duration. Unlike the pre-#94 single "resets in"
/// (issue #72), which collapsed an account to its one binding window, each window
/// (SESSION, WEEKLY) is now rendered DIRECTLY from its own instant, so `status` shows
/// both side by side and the operator sees when work resumes AND when the account
/// fully frees up.
fn reset_cell(reset_at: Option<i64>, now: i64) -> String {
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

/// The `status --verbose` access-token expiry block (issue #143): one line per account
/// with the RAW access-token "expires in", printed under the table when `-v`/`--verbose`
/// is passed. Empty for an empty roster (the table renders its own empty state).
///
/// The clock is the wire's `access_expires_at` — the refresh-sourced access-token expiry
/// when `[refresh]` is on, else the poll-sourced fallback the daemon folds into the same
/// field (issue #141), so it is populated in the default config too. It is LABELLED
/// ("auto-refreshed by Claude Code, not a re-login deadline") because Claude Code
/// refreshes this token invisibly: a lapsed access clock is NOT the re-login signal — that
/// is the `🔴` AUTH cell's `claude /login` cue (issue #143). Kept out of the default table
/// (a raw clock there would be misread as a deadline); `--verbose` is the opt-in for the
/// raw number, mirroring the `--json` full-data contract that already carries it.
///
/// Sourced solely from each account's label + the non-secret `access_expires_at` timestamp
/// — a reprojection of fields the wire and table already carry, no new secret-bearing input —
/// so it can never print a token or email (issue #15); pure over the [`StatusResponse`] +
/// `now`, so the rendering is unit-testable without a live socket. `pub(crate)` so the issue-#15
/// redaction METER (driven from [`crate::daemon`]) routes this new operator-facing surface
/// through its scan too, alongside [`render_status`] and [`render_roster`].
pub(crate) fn render_access_token_expiry(response: &StatusResponse, now: i64) -> String {
    if response.accounts.is_empty() {
        return String::new();
    }
    // Pad each label to the widest (by char count, matching the `{:<width$}` fill and the
    // `list` view) so the expiry column lines up under a two-space gap.
    let width = response
        .accounts
        .iter()
        .map(|account| account.label.chars().count())
        .max()
        .unwrap_or(0);
    let mut out =
        String::from("\naccess token — auto-refreshed by Claude Code, not a re-login deadline:\n");
    for account in &response.accounts {
        out.push_str(&format!(
            "  {:<width$}  {}\n",
            account.label,
            access_token_expiry_cell(account.access_expires_at, now),
        ));
    }
    out
}

/// One account's access-token "expires in" for the `--verbose` block (issue #143):
/// `expires in <compact>` for a future expiry — the same two-largest-unit clock the table's
/// resets render (via [`humanize_until`]) — `expired` once at/past `now`, or `unknown` when
/// the daemon carries no expiry for the account (never a fabricated duration). The wire
/// clock is epoch SECONDS (issue #119/#141), so it differences against `now` directly —
/// unlike the `list` view's `expiry_tag`, which reduces a millisecond stash read first.
fn access_token_expiry_cell(expires_at: Option<i64>, now: i64) -> String {
    match expires_at {
        Some(at) if at <= now => "expired".to_owned(),
        Some(at) => format!("expires in {}", humanize_until(at - now)),
        None => "unknown".to_owned(),
    }
}

/// The controlling terminal's column count for stdout, or `None` when stdout is
/// not a TTY (piped / redirected) or the query fails. Drives `status`'s
/// narrow-terminal column degradation (issue #72); the `None` non-interactive case
/// keeps the full table, so `status | grep` and `status > file` stay complete.
///
/// `pub(crate)` so the `stats` charts (issue #159) share the SAME width probe: a
/// `None` there means "not a TTY", the signal that drops the charts for the numeric
/// table (a piped / redirected `stats` stays the plain, greppable surface).
pub(crate) fn terminal_cols() -> Option<usize> {
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
///
/// `pub(crate)` so the `stats` charts (issue #159) gate their ANSI overlay through the
/// SAME single discipline this `status` view uses — one definition of "may I colour".
pub(crate) fn should_colorize(no_color: bool) -> bool {
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
/// (issue #73). A pipe, a redirect, or a closed stdout is not a TTY, so color
/// stays off there. Uses [`std::io::IsTerminal`] (stable since Rust 1.70), which
/// wraps `isatty(3)` on Unix with no `unsafe` FFI (issue #178) — unlike
/// [`terminal_cols`]'s direct-libc `TIOCGWINSZ` probe, whose ioctl has no std
/// equivalent, so that sibling keeps its raw `libc` call.
fn stdout_is_tty() -> bool {
    std::io::stdout().is_terminal()
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

/// List captured accounts — the offline, read-only roster view (issue #17), enriched
/// with the static per-account auth subset (issue #120).
///
/// Reads `config.toml` for the roster, then — daemon-independently — the credential
/// STORE (each account's stash) for the access-token expiry and the event log for the
/// last-persisted refresh outcome: NO daemon, NO `/usage`, no network (the static
/// counterpart to `status`, which needs a live `run`). An absent config is the empty
/// state, surfaced as the friendly [`Error::RosterEmpty`]; a malformed config still
/// surfaces as its real parse/validation error. The store/log reads are best-effort —
/// an unreadable stash or log simply omits that account's tag, never failing the view.
/// The output is sourced solely from the roster's non-secret fields plus a
/// timestamp-derived duration and a bare refresh-outcome token, so it can never print a
/// token or email (issue #15 redaction).
async fn list() -> Result<()> {
    let roster = resolve_roster(Config::load())?;
    // The static auth subset (issue #120): a credential-STORE + event-log read, both
    // daemon-independent (no `run`, no `/usage`). Gathered AFTER the roster resolves so
    // the empty / malformed-config exits below never touch the keychain.
    let auth = gather_auth_subset(&roster).await;
    print!("{}", render_roster(&roster, &auth, now_epoch()));
    Ok(())
}

/// How an encrypted export/import sources its passphrase. NEVER an argv value (issues
/// #39 / #148 / #149): only an interactive no-echo terminal prompt, a file, or standard
/// input. Shared by both `export` (encrypt) and `import` (decrypt) for symmetry — the
/// direction-specific prompt wording is supplied by the caller to [`read`](Self::read).
enum PassphraseSource {
    /// Prompt on the controlling terminal with echo disabled (the default).
    Prompt,
    /// Read the passphrase's first line from the given file (`--passphrase-file`).
    File(PathBuf),
    /// Read the passphrase's first line from standard input (`--passphrase-stdin`).
    Stdin,
}

impl PassphraseSource {
    /// Acquire the passphrase from this source, funnelling through the #147 input
    /// paths so the bytes are held in a `Zeroizing` buffer and never pass through argv.
    /// `prompt` is used only by the interactive [`Prompt`](Self::Prompt) variant (the
    /// file / stdin paths read silently), so the caller words it for the direction —
    /// "encrypt the export" vs "decrypt the import".
    fn read(&self, prompt: &str) -> Result<Passphrase> {
        match self {
            PassphraseSource::Prompt => Passphrase::prompt(prompt),
            PassphraseSource::File(path) => Passphrase::from_file(path),
            PassphraseSource::Stdin => Passphrase::from_stdin(),
        }
    }
}

/// Whether the export body is encrypted (the default) and, if so, how its passphrase
/// is read; or `Plaintext` for the `--plaintext` opt-out.
enum Encryption {
    /// Encrypt the body under a passphrase from the given source (#147).
    Encrypted(PassphraseSource),
    /// Write the body in the clear — warned when it carries secrets.
    Plaintext,
}

/// Resolve the parsed `export` flags into an [`Encryption`] decision. `--plaintext`
/// wins outright (no passphrase is read); otherwise a `--passphrase-file` /
/// `--passphrase-stdin` source is honored, defaulting to the interactive prompt.
fn export_encryption(
    plaintext: bool,
    passphrase_file: Option<PathBuf>,
    passphrase_stdin: bool,
) -> Encryption {
    if plaintext {
        Encryption::Plaintext
    } else if let Some(path) = passphrase_file {
        Encryption::Encrypted(PassphraseSource::File(path))
    } else if passphrase_stdin {
        Encryption::Encrypted(PassphraseSource::Stdin)
    } else {
        Encryption::Encrypted(PassphraseSource::Prompt)
    }
}

/// `export [PATH]` — serialize local state into a migration artifact (issue #148).
///
/// READ-ONLY: loads the roster + tunables ([`Config::load`]) and reads each account's
/// keychain stash, mutating neither. Builds the #146 container — the rendered config
/// plus, unless `no_secrets`, every account's credential + `oauthAccount` material —
/// then either encrypts it under a passphrase (#147) or, for [`Encryption::Plaintext`],
/// leaves it in the clear (warned, unless secret-free). Writes to `path` (atomic,
/// mode 0600) or, with no `path`, to standard output.
///
/// Diagnostics carry no account label, email, or token — the passphrase is read
/// through the #147 no-argv input paths and never logged.
async fn export(path: Option<PathBuf>, no_secrets: bool, encryption: Encryption) -> Result<()> {
    let config = Config::load()?;
    let stash = RealAccountStash::new();
    let payload = gather_payload(&config, &stash, no_secrets).await?;

    // The redacted-event dimensions (issue #150), captured before `encryption` is consumed by
    // the match: whether the artifact is encrypted, and whether it carries secrets (full) or is
    // config-only (`--no-secrets`). `accounts` is the roster size the artifact carries.
    let encrypted = matches!(&encryption, Encryption::Encrypted(_));
    let mode = if no_secrets {
        ExportMode::ConfigOnly
    } else {
        ExportMode::Full
    };

    let bytes = match encryption {
        Encryption::Plaintext => {
            // The artifact then holds restorable credentials in the clear. Warn on
            // stderr — never stdout, which may carry the artifact — unless `--no-secrets`
            // made it secret-free (nothing to protect, so the warning would misinform).
            if !no_secrets {
                eprintln!("{PLAINTEXT_WARNING}");
            }
            MigrationArtifact::plaintext(payload).to_bytes()
        }
        Encryption::Encrypted(source) => {
            let passphrase = source.read("Passphrase to encrypt the export: ")?;
            // Derive the key at the operator's `[migration]` Argon2id cost (issue #150); the
            // default maps to the built-in production cost, so a default config is unchanged.
            MigrationArtifact::encrypt_with_cost(
                &payload,
                &passphrase,
                &config.migration.kdf_cost(),
            )?
            .to_bytes()
        }
    };

    write_export(path.as_deref(), &bytes)?;

    // The artifact is written — emit the single redacted audit event (issue #150). BEST-EFFORT,
    // like the #135 login event: the export already succeeded, so a log-open/append failure is
    // swallowed rather than masking it. Aggregate-only (a count + a bool + a mode token) — no
    // account handle, so nothing account-specific ever reaches the line.
    emit_export_event(config.roster.len(), encrypted, mode);
    Ok(())
}

/// Emit the single redacted [`Event::Export`] audit line (issue #150) — BEST-EFFORT, like the
/// #135 login event: the export's own result stands regardless of whether the audit log is
/// writable, so a failure to open or append it is swallowed. Carries aggregate counts only —
/// never an account handle, token, or email.
fn emit_export_event(accounts: usize, encrypted: bool, mode: ExportMode) {
    if let Ok(mut log) = EventLog::open() {
        let _ = log.emit(&Event::Export {
            // A roster far exceeding u32 is not reachable; saturate rather than wrap so the count
            // stays honest under any absurd input.
            accounts: accounts.try_into().unwrap_or(u32::MAX),
            encrypted,
            mode,
        });
    }
}

/// Gather the live state into a migration [`Payload`] — READ-ONLY, generic over the
/// stash so tests drive it with a fake in-memory `FakeAccountStash`.
///
/// `config_toml` is the canonical rendered config (roster + tunables + refresh). With
/// `no_secrets`, `accounts` is left EMPTY — a config-only artifact: the roster still
/// travels inside `config_toml`, but no credential material does, so no keychain read
/// happens at all. Otherwise each roster account's stash is read and its credential +
/// `oauthAccount` bytes carried.
async fn gather_payload(
    config: &Config,
    stash: &impl AccountStash,
    no_secrets: bool,
) -> Result<Payload> {
    let config_toml = config.render();
    let accounts = if no_secrets {
        Vec::new()
    } else {
        let mut accounts = Vec::with_capacity(config.roster.len());
        for account in &config.roster {
            let stashed = stash.read(&account.stash()).await?;
            accounts.push(ManagedAccount::new(
                account.account_uuid.clone(),
                stashed.credential.expose().to_vec(),
                stashed.oauth_account.raw_json().to_vec(),
            ));
        }
        accounts
    };
    Ok(Payload::new(config_toml, accounts))
}

/// Write the serialized artifact to `path` or, when `None`, to standard output.
///
/// The file path uses [`paths::write_private_file`]: a same-directory temp, `fsync`,
/// then an atomic `rename(2)` — so a concurrent reader sees the old file or the new
/// one, never a partial write — and the result is mode 0600 regardless of `--no-secrets`
/// (a config-only artifact is still never left world-readable; issue #148).
fn write_export(path: Option<&Path>, bytes: &[u8]) -> Result<()> {
    match path {
        Some(path) => paths::write_private_file(path, bytes),
        None => {
            use std::io::Write;
            let mut out = std::io::stdout().lock();
            out.write_all(bytes)?;
            out.flush()?;
            Ok(())
        }
    }
}

/// Resolve the parsed `import` flags into a [`PassphraseSource`]. A `--passphrase-file` /
/// `--passphrase-stdin` source is honored, defaulting to the interactive prompt. The
/// source is only CONSUMED when the artifact turns out to be encrypted (a plaintext one
/// needs no passphrase), so these flags are inert for a plaintext import.
fn import_passphrase(passphrase_file: Option<PathBuf>, passphrase_stdin: bool) -> PassphraseSource {
    if let Some(path) = passphrase_file {
        PassphraseSource::File(path)
    } else if passphrase_stdin {
        PassphraseSource::Stdin
    } else {
        PassphraseSource::Prompt
    }
}

/// `import <PATH>` — rehydrate local state from a migration artifact (issue #149), the
/// INVERSE of [`export`].
///
/// Reads the artifact, decrypts it under a passphrase (#147) when encrypted (a plaintext
/// artifact needs none), then merges its accounts into the local roster under the conflict
/// policy: an account already present on the target is SKIPPED (left untouched) unless the
/// effective policy is overwrite — forced by `--overwrite`, else the target's
/// `[migration].conflict_policy` default (#150; Skip by default). Each credential-carrying account
/// is restored through the EXISTING keychain stash write (`security -i`, off-argv, #39) and
/// read-back-verified; a config-only account (from `export --no-secrets`) lands as a roster entry
/// to be re-authenticated by `login` (#135). Writes serialize under the swap lock (#64); the
/// roster is saved atomically once, so a partial failure never dangles a half-written roster and
/// the import is safely re-runnable. Emits ONE redacted audit event (#150) — aggregate per-account
/// outcome counts only, never a handle/token/email. Any per-account failure exits non-zero after
/// committing the successes.
///
/// Diagnostics name accounts by their non-secret label only — never a token or email; the
/// passphrase is read through the #147 no-argv paths and never logged.
async fn import(path: PathBuf, overwrite: bool, passphrase: PassphraseSource) -> Result<()> {
    let bytes = std::fs::read(&path)?;
    let artifact = MigrationArtifact::from_bytes(&bytes)?;
    // Decrypt only when encrypted — a plaintext artifact never reads (or prompts for) a
    // passphrase. The decrypt path holds the plaintext in a zeroized-on-drop buffer (#147).
    let payload = if artifact.is_encrypted() {
        let passphrase = passphrase.read("Passphrase to decrypt the import: ")?;
        artifact.decrypt(&passphrase)?
    } else {
        artifact.into_plaintext_payload()?
    };

    // Ensure the native-local support dir (0700) that houses `swap.lock` exists before
    // acquiring the lock (mirrors `capture`/`use`, #64).
    paths::ensure_private_dir(&paths::support_dir()?)?;
    let swap_lock = paths::swap_lock()?;

    // Load the target config; a fresh machine (no config yet) is the `None` base.
    let local = match Config::load() {
        Ok(config) => Some(config),
        Err(Error::ConfigNotFound { .. }) => None,
        Err(other) => return Err(other),
    };

    // Conflict-policy default (issue #150): when `--overwrite` is absent, defer to the TARGET
    // operator's `[migration].conflict_policy` (Skip by default, so a default config leaves
    // behaviour unchanged). Resolved from `local` before it is moved into `apply_import`.
    let overwrite = resolve_import_overwrite(overwrite, local.as_ref());

    let (config, outcomes) = apply_import(
        Some((&swap_lock, SWAP_LOCK_MAX_WAIT)),
        &payload,
        local,
        &RealAccountStash::new(),
        overwrite,
    )
    .await?;

    // Persist the merged roster atomically (temp + rename, 0600) — OUTSIDE the swap lock
    // (config.toml is never swap-contended), mirroring `reconcile_login` (#135). One
    // write → a partial failure above leaves no half-written roster.
    config.save()?;
    // Tell a running daemon to pick up the imported accounts now (#139) — best-effort.
    crate::capture::notify_daemon_roster_reload().await;

    println!("{}", import_report(&outcomes));

    // Emit the single redacted audit event (issue #150) — BEST-EFFORT, like the export/login
    // events: aggregate per-account outcome COUNTS only (no handle), so nothing account-specific
    // reaches the line. Emitted whatever the outcome (ok / partial / failed), before the exit.
    let (imported, skipped, overwritten, failed) = count_import_outcomes(&outcomes);
    emit_import_event(imported, skipped, overwritten, failed);

    // Surface any per-account failure LOUDLY with a non-zero exit — the successful
    // accounts were still committed to the roster (honest partial result), and the
    // per-account report above names which landed and which failed.
    if failed > 0 {
        return Err(Error::MigrationImportIncomplete {
            failed: failed as usize,
        });
    }
    Ok(())
}

/// Resolve the effective import overwrite policy (issue #150). The `--overwrite` CLI flag ALWAYS
/// forces overwrite; when it is absent, defer to the TARGET operator's `[migration].conflict_policy`
/// (`local` is `None` on a fresh machine → the [`MigrationConfig`](crate::config::MigrationConfig)
/// default, Skip). Pure so the flag-over-config precedence is unit-testable without touching the
/// real config path.
fn resolve_import_overwrite(cli_overwrite: bool, local: Option<&Config>) -> bool {
    if cli_overwrite {
        return true;
    }
    local
        .map(|config| config.migration.conflict_policy)
        .unwrap_or_default()
        == ConflictPolicy::Overwrite
}

/// Tally the per-account import outcomes into `(imported, skipped, overwritten, failed)` — the four
/// counts the redacted [`Event::Import`] carries (issue #150). Saturating into `u32` (a roster far
/// exceeding `u32` is unreachable) so the counts stay honest under any absurd input.
fn count_import_outcomes(outcomes: &[AccountImport]) -> (u32, u32, u32, u32) {
    let count = |want: ImportOutcome| -> u32 {
        outcomes
            .iter()
            .filter(|o| o.outcome == want)
            .count()
            .try_into()
            .unwrap_or(u32::MAX)
    };
    (
        count(ImportOutcome::Imported),
        count(ImportOutcome::Skipped),
        count(ImportOutcome::Overwritten),
        count(ImportOutcome::Failed),
    )
}

/// Emit the single redacted [`Event::Import`] audit line (issue #150) — BEST-EFFORT, like the
/// #135 login event and the export event: the import's own result (and its per-account report /
/// exit code) stands regardless of whether the audit log is writable, so a failure to open or
/// append it is swallowed. Carries aggregate counts only — never an account handle, token, or email.
fn emit_import_event(imported: u32, skipped: u32, overwritten: u32, failed: u32) {
    if let Ok(mut log) = EventLog::open() {
        let _ = log.emit(&Event::Import {
            imported,
            skipped,
            overwritten,
            failed,
        });
    }
}

/// Merge a migration [`Payload`] into the local roster under the conflict policy —
/// PURE of the real config path, generic over the stash so tests drive it with a fake
/// in-memory `FakeAccountStash` (mirrors [`gather_payload`] on the export side).
///
/// `local` is the target's current config (`None` on a fresh machine). The returned
/// [`Config`] is the merged result the caller persists; the [`AccountImport`] vec is the
/// per-account outcome report. The swap lock (`lock`, `Some` in production) is acquired
/// BEFORE any keychain write and held across all of them, then dropped before return so
/// the caller's `config.save()` runs unlocked; it is skipped entirely for a config-only
/// artifact (no keychain write to serialize). A `lock` of `None` is the hermetic test
/// path.
async fn apply_import<S: AccountStash>(
    lock: Option<(&Path, Duration)>,
    payload: &Payload,
    local: Option<Config>,
    stash: &S,
    overwrite: bool,
) -> Result<(Config, Vec<AccountImport>)> {
    // The roster + tunables the artifact carries, held to the same invariants as any
    // on-disk config (unique non-empty account_uuid, tunable ranges).
    let incoming = Config::from_toml_str(payload.config_toml())?;

    // Base config: preserve the LOCAL config when present — its tunables / refresh / login / stats
    // / migration blocks and existing roster are authoritative (the per-account merge below only
    // touches the roster; a whole-config merge — adopting the artifact's non-roster blocks over the
    // local ones — remains future work, NOT what #150 added: #150 added the per-account
    // conflict-policy DEFAULT that resolves into `overwrite` upstream, plus the redacted events).
    // On a fresh target adopt the incoming config but start from an empty roster, so every account
    // flows through the conflict policy + integrity check below.
    let mut result = match local {
        Some(local) => local,
        None => Config {
            roster: Vec::new(),
            ..incoming.clone()
        },
    };

    // Per-account secret material, indexed by uuid — EMPTY for a config-only artifact,
    // in which case every account below imports as a roster-only "needs re-login" (#135).
    let secrets: std::collections::HashMap<&str, &ManagedAccount> = payload
        .accounts()
        .iter()
        .map(|managed| (managed.account_uuid(), managed))
        .collect();

    // Acquire the single-writer swap lock (#64) around the keychain writes — only when
    // the artifact actually carries credentials (a config-only import writes no keychain
    // item, so it needs no lock). Acquired BEFORE any write; a contended acquire fails
    // closed (`SwapLockBusy`) with ZERO writes. Held until this fn returns.
    let _guard = match (lock, secrets.is_empty()) {
        (Some((path, max_wait)), false) => Some(SwapLock::acquire(path, max_wait).await?),
        _ => None,
    };

    let mut outcomes = Vec::with_capacity(incoming.roster.len());
    for incoming_account in &incoming.roster {
        let existing = result
            .roster
            .iter()
            .position(|account| account.account_uuid == incoming_account.account_uuid);

        // Conflict policy: an account already on the target is SKIPPED — left
        // byte-for-byte untouched (its stash AND roster entry) — unless `overwrite`.
        if existing.is_some() && !overwrite {
            outcomes.push(AccountImport::skipped(&incoming_account.label));
            continue;
        }

        // Restore the credential stash if the artifact carries one for this account.
        // Stash-BEFORE-roster (like `capture`/`reconcile_login`): a write or read-back
        // failure leaves the account OUT of the roster (never a roster entry pointing at
        // an unstashed account), reported `failed`, and the remaining accounts continue.
        // A config-only account (no secret) writes nothing and lands as a roster entry
        // only → "needs re-login".
        if let Some(managed) = secrets.get(incoming_account.account_uuid.as_str()) {
            if write_and_verify(stash, &incoming_account.stash(), managed)
                .await
                .is_err()
            {
                outcomes.push(AccountImport::failed(&incoming_account.label));
                continue;
            }
        }

        let outcome = match existing {
            Some(idx) => {
                result.roster[idx] = incoming_account.clone();
                AccountImport::overwritten(&incoming_account.label)
            }
            None => {
                result.roster.push(incoming_account.clone());
                AccountImport::imported(&incoming_account.label)
            }
        };
        outcomes.push(outcome);
    }

    Ok((result, outcomes))
}

/// Restore one account's credential material into its keychain stash and VERIFY the
/// write landed (issue #149's outcome-integrity requirement).
///
/// Writes both halves through the existing off-argv stash write ([`AccountStash::write`]
/// → `security -i`, #39), then reads them back and confirms each half hash-matches what
/// was written. The comparison is over sha256 digests, never the bytes, so nothing secret
/// is printed or otherwise materialized for the check; a mismatch (a store that did not
/// persist the bytes, a locked keychain at read-back) is [`Error::MigrationImportVerifyFailed`].
async fn write_and_verify<S: AccountStash>(
    stash: &S,
    service: &str,
    managed: &ManagedAccount,
) -> Result<()> {
    let account = StashedAccount {
        credential: Credential::new(managed.credential().to_vec()),
        oauth_account: OauthAccount::from_object_bytes(managed.oauth_account())?,
    };
    stash.write(service, &account).await?;

    let readback = stash.read(service).await?;
    let credential_ok =
        sha256_hex(account.credential.expose()) == sha256_hex(readback.credential.expose());
    let oauth_ok = sha256_hex(account.oauth_account.raw_json())
        == sha256_hex(readback.oauth_account.raw_json());
    if credential_ok && oauth_ok {
        Ok(())
    } else {
        Err(Error::MigrationImportVerifyFailed)
    }
}

/// One account's `import` outcome, for the per-account report (issue #149). Non-secret
/// (an outcome label, not account material), so `Debug` is safe here.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ImportOutcome {
    /// A new account: roster entry added (+ stash written when the artifact carried one).
    Imported,
    /// Already present on the target and left untouched (no `--overwrite`).
    Skipped,
    /// Already present and replaced under `--overwrite`.
    Overwritten,
    /// A credential write or its read-back verification failed; NOT added to the roster.
    Failed,
}

impl ImportOutcome {
    /// The report word for this outcome.
    fn word(self) -> &'static str {
        match self {
            ImportOutcome::Imported => "imported",
            ImportOutcome::Skipped => "skipped",
            ImportOutcome::Overwritten => "overwritten",
            ImportOutcome::Failed => "failed",
        }
    }
}

/// One line of the per-account import report. Identifies the account by its non-secret
/// LABEL only (as `list`/`status`/`remove` do — issue #15), never a token or email.
struct AccountImport {
    label: String,
    outcome: ImportOutcome,
}

impl AccountImport {
    fn imported(label: &str) -> Self {
        Self {
            label: label.to_owned(),
            outcome: ImportOutcome::Imported,
        }
    }
    fn skipped(label: &str) -> Self {
        Self {
            label: label.to_owned(),
            outcome: ImportOutcome::Skipped,
        }
    }
    fn overwritten(label: &str) -> Self {
        Self {
            label: label.to_owned(),
            outcome: ImportOutcome::Overwritten,
        }
    }
    fn failed(label: &str) -> Self {
        Self {
            label: label.to_owned(),
            outcome: ImportOutcome::Failed,
        }
    }
}

/// Render the per-account import report: one `outcome \`label\`` line per account, then a
/// count summary. Labels only (non-secret); no token or email ever appears. Returned as a
/// String so it is unit-testable and the caller prints it.
fn import_report(outcomes: &[AccountImport]) -> String {
    let mut out = String::new();
    for entry in outcomes {
        out.push_str(&format!("{} `{}`\n", entry.outcome.word(), entry.label));
    }
    let count = |want: ImportOutcome| outcomes.iter().filter(|o| o.outcome == want).count();
    out.push_str(&format!(
        "import complete: {} imported, {} skipped, {} overwritten, {} failed",
        count(ImportOutcome::Imported),
        count(ImportOutcome::Skipped),
        count(ImportOutcome::Overwritten),
        count(ImportOutcome::Failed),
    ));
    out
}

/// Resolve a load outcome into the roster `list` renders, or the error it exits on.
///
/// Split from [`list`] so the load-outcome → roster mapping is unit-testable without
/// touching the filesystem: a present roster passes through; an absent config
/// ([`Error::ConfigNotFound`]) becomes the friendly [`Error::RosterEmpty`]; every other
/// load error (malformed / invalid config) surfaces unchanged. The per-account auth
/// subset (issue #120) is layered on in [`list`] / [`render_roster`], not here — this
/// stays pure config policy.
fn resolve_roster(loaded: Result<Config>) -> Result<Vec<Account>> {
    match loaded {
        // Both empty states read the same: an absent config, OR a well-formed
        // tunables-only file whose roster is empty (now that `capture` can load
        // such a file, #58). Either way `list` shows the friendly "nothing captured
        // yet" rather than a bare "0 accounts".
        Ok(config) if config.roster.is_empty() => Err(Error::RosterEmpty),
        Ok(config) => Ok(config.roster),
        Err(Error::ConfigNotFound { .. }) => Err(Error::RosterEmpty),
        Err(other) => Err(other),
    }
}

/// One account's offline, daemon-INDEPENDENT auth subset for the `list` view (issue
/// #120): the stored access-token expiry and the last-persisted refresh outcome.
///
/// The static counterpart of the live `status` health rollup (#119): `status` needs the
/// daemon to compute its cross-tick verdict, but a wedged daemon is frequently itself a
/// credential problem — exactly when the offline view must still answer "is this token
/// fresh, and did its last refresh work?". Both fields are NON-SECRET by construction —
/// `expires_at_ms` is the integer `refresh::stored_expires_at` extracts (never the
/// token), `last_refresh` a bare enum read back from the redaction-metered event log —
/// and each is `None` when unavailable (stash unreadable / no refresh ever recorded),
/// which [`render_roster`] renders by omitting the corresponding tag.
pub(crate) struct AuthSubset {
    /// `claudeAiOauth.expiresAt` (epoch milliseconds, CC's native unit) of the stored
    /// access token, or `None` when the stash is unreadable (locked keychain, absent
    /// item) or carries no parseable expiry.
    pub(crate) expires_at_ms: Option<i64>,
    /// The account's most recent persisted [`RefreshEventOutcome`], or `None` when the
    /// event log records no refresh for it (the common case while the opt-in `[refresh]`
    /// tick, #105, is off).
    pub(crate) last_refresh: Option<RefreshEventOutcome>,
}

/// Read the offline auth subset for each roster account (issue #120), returned PARALLEL
/// to `roster` (same length, same order) for [`render_roster`].
///
/// Daemon-independent and read-only, the only I/O the issue authorizes: a credential-
/// STORE read per account ([`refresh::stored_expires_at`] — a `security` read of the
/// account's own stash, the SAME access the refresh sweep / `poke` already make, so no
/// new keychain-prompt surface) plus ONE pass over the event log for the last refresh
/// outcome per handle ([`crate::observability::last_refresh_outcomes`]). No daemon, no
/// `/usage`, no live refresh. Best-effort: an unreadable stash or log degrades that
/// field to `None`, so `list` stays a non-failing read-only view.
async fn gather_auth_subset(roster: &[Account]) -> Vec<AuthSubset> {
    // One log read for the whole roster (last outcome per handle) — not one read per
    // account. An unresolvable log path degrades straight to an empty map (→ no refresh
    // tags), rather than reading a sentinel empty path.
    let last_refresh = crate::observability::log_path()
        .map(|path| crate::observability::last_refresh_outcomes(&path))
        .unwrap_or_default();
    let stash = RealAccountStash::new();
    let mut subsets = Vec::with_capacity(roster.len());
    for account in roster {
        subsets.push(AuthSubset {
            expires_at_ms: refresh::stored_expires_at(&stash, &account.stash()).await,
            last_refresh: last_refresh.get(&account.label).copied(),
        });
    }
    subsets
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
/// Each row then trails the inline auth tags (issue #120), parallel to `auth` (same
/// length, same order as `roster`) and measured against `now_secs`: ` · expires in 2h`
/// (or ` · expired`) from the stored access-token expiry, and ` · last refresh: <token>`
/// from the last-persisted refresh outcome. A tag is OMITTED when its datum is
/// unavailable (unreadable stash / never refreshed), so a config-only roster with the
/// refresh tick off reads exactly as the pre-#120 view. These join the existing
/// ` · disabled` rotation tag (#36) as more ` · `-delimited tags on the same row.
///
/// Sourced solely from each [`Account`]'s two non-secret display fields — `label`
/// and `account_uuid` — plus the auth tags, which are a timestamp-derived duration and
/// a bare enum token: never a token or email (issue #15 redaction). A label is
/// operator-provided free text: one that happens to contain an `@` is the
/// operator's own value, not a leak.
///
/// `pub(crate)` so the issue-#15 redaction METER (driven from [`crate::daemon`])
/// can route this exact `list`-view surface — auth tags included — through its scan.
pub(crate) fn render_roster(roster: &[Account], auth: &[AuthSubset], now_secs: i64) -> String {
    // `auth` is built parallel to `roster` by `gather_auth_subset`; the zip below pairs
    // them positionally, so a length mismatch would silently drop trailing rows.
    debug_assert_eq!(roster.len(), auth.len(), "auth subset must parallel roster");
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
    for (account, auth) in roster.iter().zip(auth) {
        // A parked account is marked inline (issue #36); an enabled one adds
        // nothing.
        let state = if account.enabled { "" } else { " · disabled" };
        let tags = auth_tags(auth, now_secs);
        out.push_str(&format!(
            "{:<width$}  {}{}{}\n",
            account.label, account.account_uuid, state, tags,
        ));
    }
    let n = roster.len();
    let noun = if n == 1 { "account" } else { "accounts" };
    out.push_str(&format!("\n{n} {noun}\n"));
    out
}

/// The trailing ` · `-delimited auth tags for one `list` row (issue #120): the
/// `expiresAt`-derived freshness, then the last-persisted refresh outcome — each part
/// included only when its datum is available, so an account with neither adds nothing
/// (the pre-#120 row). Pure over the [`AuthSubset`] + `now_secs`, so the rendering is
/// unit-testable without a keychain or log.
fn auth_tags(auth: &AuthSubset, now_secs: i64) -> String {
    let mut tags = String::new();
    if let Some(expiry) = expiry_tag(auth.expires_at_ms, now_secs) {
        tags.push_str(" · ");
        tags.push_str(&expiry);
    }
    if let Some(refresh) = refresh_tag(auth.last_refresh) {
        tags.push_str(" · ");
        tags.push_str(&refresh);
    }
    tags
}

/// The `expiresAt`-derived freshness for one account (issue #120): `expires in <compact>`
/// for a future expiry — the same two-largest-unit clock `status` renders (#94, via
/// [`humanize_until`]) — `expired` for one already at/past `now_secs`, or `None` when the
/// stored expiry is unreadable (so [`auth_tags`] omits it). The stored `expiresAt` is
/// epoch MILLISECONDS (CC's native unit); reduce it to whole seconds at the boundary
/// before differencing against `now_secs`, matching the event log's `ms / 1000` render.
fn expiry_tag(expires_at_ms: Option<i64>, now_secs: i64) -> Option<String> {
    let secs = expires_at_ms? / 1000;
    if secs <= now_secs {
        Some("expired".to_owned())
    } else {
        Some(format!("expires in {}", humanize_until(secs - now_secs)))
    }
}

/// The last-persisted refresh-outcome tag for one account (issue #120), or `None` when
/// no refresh was ever recorded (so [`auth_tags`] omits it). Rendered in the SAME token
/// the event log writes ([`RefreshEventOutcome::as_str`]) so it cross-references a
/// `sessiometer.log` the operator may grep. A `dead` outcome trails the actionable
/// `claude /login` cue — the offline echo of `status`'s dead-credential cue (#119) —
/// since a daemon-down `list` is exactly where an operator meets a dead refresh token.
fn refresh_tag(last_refresh: Option<RefreshEventOutcome>) -> Option<String> {
    let outcome = last_refresh?;
    let mut tag = format!("last refresh: {}", outcome.as_str());
    if outcome == RefreshEventOutcome::Dead {
        // The exact command `status`'s health cell prints (#119), so both views point
        // an operator at the same fix.
        tag.push_str(" — claude /login");
    }
    Some(tag)
}

/// `disable`/`enable <label>` — take an account out of the rotation, or return it
/// (issue #36). A reversible park, distinct from removal (#13): the account keeps
/// its roster entry and its stash; only its `enabled` flag flips. Resolve the
/// account by its non-secret label, set the flag, and persist via [`Config::save`]
/// so the change survives a daemon restart (config-backed). A running daemon is
/// notified to reload (#139), so the flip takes effect in the live rotation without
/// a restart (best-effort — no daemon running is a no-op, the next start loads it).
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
        // Tell a running daemon to pick up the enable/disable now (#139) — best-effort;
        // the account joins / leaves the live rotation without a restart. Skipped on a
        // no-op flip (nothing changed on disk, so nothing to reload).
        crate::capture::notify_daemon_roster_reload().await;
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
/// account is [`Error::AccountLabelNotFound`]. A running daemon is notified to reload
/// (#139), so the removal takes effect in the live rotation without a restart
/// (best-effort). Removing the ACTIVE account is
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
    // Tell a running daemon to drop the removed account from its live rotation now
    // (#139) — best-effort, so it never swaps to an account whose stash is gone.
    crate::capture::notify_daemon_roster_reload().await;
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

    /// A parallel `AuthSubset` slice of "nothing known" (both fields `None`), sized to
    /// `n` — the pre-#120 render baseline: such a subset adds no tags, so a row reads
    /// exactly as before. Lets the format / redaction tests pin the columns without a
    /// keychain or event log.
    fn no_auth(n: usize) -> Vec<AuthSubset> {
        (0..n)
            .map(|_| AuthSubset {
                expires_at_ms: None,
                last_refresh: None,
            })
            .collect()
    }

    /// One known auth subset, for the issue-#120 tag tests.
    fn auth(expires_at_ms: Option<i64>, last_refresh: Option<RefreshEventOutcome>) -> AuthSubset {
        AuthSubset {
            expires_at_ms,
            last_refresh,
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
            refresh: crate::config::RefreshConfig::default(),
            login: crate::config::LoginConfig::default(),
            stats: crate::config::StatsConfig::default(),
            migration: crate::config::MigrationConfig::default(),
        }
    }

    #[test]
    fn renders_each_account_then_the_count_total() {
        // With no auth subset available (#120), a row reads exactly as the pre-#120 view.
        let out = render_roster(
            &[
                acct("work", "11111111-1111-1111-1111-111111111111"),
                acct("personal", "22222222-2222-2222-2222-222222222222"),
            ],
            &no_auth(2),
            0,
        );
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
        let out = render_roster(&roster, &no_auth(roster.len()), 0);
        assert!(out.ends_with("\n6 accounts\n"), "got: {out:?}");
        assert!(
            !out.contains("slots"),
            "no 'slots used' denominator: {out:?}"
        );
    }

    #[test]
    fn resolve_roster_returns_a_present_roster_for_render() {
        // The load-outcome → roster mapping (the unit-testable seam #120 split from the
        // I/O-bearing `list`): a present roster passes through, and `render_roster` with
        // no auth subset reads as the pre-#120 single-account view ("1 account" singular).
        let config = config_with(vec![acct("work", "11111111-aaaa")]);
        let roster = resolve_roster(Ok(config)).expect("a present roster is not an error");
        let out = render_roster(&roster, &no_auth(roster.len()), 0);
        assert_eq!(out, "work  11111111-aaaa\n\n1 account\n");
    }

    #[test]
    fn resolve_roster_maps_an_absent_config_to_the_friendly_empty_state() {
        let loaded = Err(Error::ConfigNotFound {
            path: PathBuf::from("/nonexistent/config.toml"),
        });
        assert!(
            matches!(resolve_roster(loaded), Err(Error::RosterEmpty)),
            "an absent config must become the friendly empty state"
        );
        // The friendly message points at the next step and never leaks the path.
        assert_eq!(
            Error::RosterEmpty.to_string(),
            "no accounts captured yet — run `sessiometer capture`"
        );
    }

    #[test]
    fn resolve_roster_maps_a_roster_less_config_to_the_friendly_empty_state() {
        // #58: a well-formed tunables-only config (empty roster) reads as the same
        // friendly empty state as an absent file — `capture` can now load such a
        // file, so `list` must not show a bare "0 accounts".
        let config = config_with(vec![]);
        assert!(
            matches!(resolve_roster(Ok(config)), Err(Error::RosterEmpty)),
            "an empty roster must become the friendly empty state"
        );
    }

    #[test]
    fn resolve_roster_does_not_conflate_a_malformed_config_with_the_empty_state() {
        let loaded = Err(Error::ConfigParse("expected `=`".into()));
        assert!(
            matches!(resolve_roster(loaded), Err(Error::ConfigParse(_))),
            "a malformed config must surface as its real error, not the empty state"
        );
    }

    #[test]
    fn output_never_carries_an_email_or_token_sigil() {
        // #15 redaction: the formatter sources only the two non-secret roster fields it
        // shows (`label`, `account_uuid`) plus the #120 auth tags (a timestamp-derived
        // duration and a bare outcome token), so it never auto-introduces a token or
        // email — proven here with a POPULATED auth subset (future expiry + dead refresh,
        // the most field-rich row). (A label the operator sets to an email is their own
        // value, not a leak — see issue #69.)
        let out = render_roster(
            &[acct("work", "11111111-1111-1111-1111-111111111111")],
            &[auth(Some(7_200_000), Some(RefreshEventOutcome::Dead))],
            1,
        );
        assert!(
            !out.contains('@'),
            "list output must not contain an email: {out:?}"
        );
        assert!(
            !out.to_lowercase().contains("token"),
            "list output must not contain a token: {out:?}"
        );
    }

    // --- enable/disable (issue #36) ----------------------------------------

    #[test]
    fn render_roster_marks_a_disabled_account_and_leaves_enabled_ones_unchanged() {
        let mut work = acct("work", "11111111-1111");
        work.enabled = false;
        let spare = acct("spare", "22222222-2222");
        let out = render_roster(&[work, spare], &no_auth(2), 0);
        assert_eq!(
            out,
            "work   11111111-1111 · disabled\n\
spare  22222222-2222\n\
\n\
2 accounts\n"
        );
    }

    // --- offline auth subset (issue #120) ----------------------------------

    #[test]
    fn render_roster_trails_expiry_freshness_and_last_refresh_tags() {
        // The enriched row (#120): the `expiresAt`-derived freshness, then the
        // last-persisted refresh outcome, each a ` · `-delimited tag after the uuid.
        // now=0; expiry 7200s out → "2h"; a `refreshed` outcome.
        let out = render_roster(
            &[acct("work", "11111111-1111")],
            &[auth(Some(7_200_000), Some(RefreshEventOutcome::Refreshed))],
            0,
        );
        assert_eq!(
            out,
            "work  11111111-1111 · expires in 2h · last refresh: refreshed\n\n1 account\n"
        );
    }

    #[test]
    fn render_roster_omits_tags_when_the_auth_subset_is_unavailable() {
        // Both fields `None` (unreadable stash / no refresh recorded) → no tags, so the
        // row is byte-identical to the pre-#120 view. The common config-only case.
        let out = render_roster(&[acct("work", "11111111-1111")], &no_auth(1), 0);
        assert_eq!(out, "work  11111111-1111\n\n1 account\n");
    }

    #[test]
    fn render_roster_pairs_a_disabled_tag_with_the_auth_tags() {
        // The rotation tag (#36) and the auth tags (#120) coexist as successive ` · `
        // tags on one row, in that order.
        let mut work = acct("work", "11111111-1111");
        work.enabled = false;
        let out = render_roster(
            &[work],
            &[auth(Some(7_200_000), Some(RefreshEventOutcome::NoChange))],
            0,
        );
        assert_eq!(
            out,
            "work  11111111-1111 · disabled · expires in 2h · last refresh: no_change\n\n1 account\n"
        );
    }

    #[test]
    fn expiry_tag_marks_a_past_or_boundary_expiry_as_expired() {
        // `expiresAt` is epoch MS; reduce to seconds, then compare to now_secs. A future
        // expiry humanizes; one already at/past `now` reads `expired` (never "expires in
        // now"); an unreadable expiry yields no tag.
        assert_eq!(
            expiry_tag(Some(7_200_000), 0).as_deref(),
            Some("expires in 2h")
        );
        // Boundary: expiry second == now second → expired (`<=`).
        assert_eq!(expiry_tag(Some(5_000), 5).as_deref(), Some("expired"));
        assert_eq!(expiry_tag(Some(1_000), 5).as_deref(), Some("expired"));
        assert_eq!(expiry_tag(None, 5), None);
    }

    #[test]
    fn refresh_tag_renders_the_log_token_and_logins_a_dead_credential() {
        // The tag reuses the event log's token (so it cross-references `sessiometer.log`),
        // and a `dead` outcome trails the actionable `claude /login` cue (#119 parity).
        assert_eq!(
            refresh_tag(Some(RefreshEventOutcome::Refreshed)).as_deref(),
            Some("last refresh: refreshed")
        );
        assert_eq!(
            refresh_tag(Some(RefreshEventOutcome::RefreshedNotReStashed)).as_deref(),
            Some("last refresh: refreshed_not_restashed")
        );
        assert_eq!(
            refresh_tag(Some(RefreshEventOutcome::Dead)).as_deref(),
            Some("last refresh: dead — claude /login")
        );
        assert_eq!(refresh_tag(None), None);
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
    fn render_status_marks_a_recovering_account_recovering_not_needs_relogin() {
        // Issue #109: a quarantined account whose credential is answering again (mid
        // spontaneous-revival) reads `recovering`, NOT the alarming `needs re-login` —
        // so an operator does not swap away from a healing account toward a worse one.
        // A genuinely dead account (quarantined, not recovering) still reads
        // `needs re-login`. Mirrors `render_status_marks_a_quarantined_account_needs_relogin`.
        let mut healing = status_line("healing", false, Some(30), Some(30));
        healing.quarantined = true;
        healing.recovering = true;
        let mut dead = status_line("dead", false, None, None);
        dead.quarantined = true; // quarantined but NOT recovering — still dead
        let response = StatusResponse {
            accounts: vec![status_line("work", true, Some(50), Some(25)), healing, dead],
            next_swap: None,
        };
        let out = render_status(&response, NOW, None, false);
        let healing = out.lines().find(|l| l.contains("healing")).unwrap();
        assert!(
            healing.contains("recovering") && !healing.contains("re-login"),
            "a healing account reads `recovering`, never `needs re-login`: {healing}"
        );
        let dead = out.lines().find(|l| l.contains("dead")).unwrap();
        assert!(
            dead.contains("needs re-login") && !dead.contains("recovering"),
            "a genuinely dead account still reads `needs re-login`: {dead}"
        );
        // The tag is a plain string — no token, no email reaches the surface (#15).
        assert!(
            !out.contains('@'),
            "no email on the printed surface: {out:?}"
        );
        assert!(!out.to_lowercase().contains("token"));
    }

    // --- status: 4-state credential-health rollup (issue #119) --------------

    #[test]
    fn health_cell_projects_each_rollup_state_to_a_glyph_with_an_actionable_cue() {
        use CredentialHealth::{AtRisk, Dead, Healthy, Stale, Unknown};
        // `health == Some(verdict)`: the daemon's 4-state rollup renders as ONE self-coloring
        // glyph, plus the minimal cue an operator needs to act.
        let cell = |health, quarantined, recovering, enabled| {
            health_cell(&AccountStatusLine {
                health,
                quarantined,
                recovering,
                enabled,
                ..status_line("work", false, Some(10), Some(20))
            })
        };
        assert_eq!(cell(Some(Healthy), false, false, true), "🟢");
        // #137: no positive-liveness evidence renders a neutral ⚪ — distinct from a false 🟢,
        // and carries NO cue (only `Dead` prompts `claude /login`).
        assert_eq!(cell(Some(Unknown), false, false, true), "⚪");
        assert_eq!(cell(Some(Stale), false, false, true), "🟡");
        assert_eq!(cell(Some(AtRisk), false, false, true), "🟠");
        // A DEAD credential carries the exact recovery command (AC-1) — visibly distinct from
        // a usage-exhausted but credential-healthy account, which carries no such cue.
        assert_eq!(cell(Some(Dead), true, false, true), "🔴 claude /login");
        // A HEALING quarantined account reads `recovering`, NOT the command — so the operator
        // holds rather than re-authing or swapping away from an often-healthier account (#109).
        assert_eq!(cell(Some(Dead), true, true, true), "🔴 recovering");
        // The rotation `disabled` tag (#36) is orthogonal to credential health — a parked
        // account can be perfectly healthy — so it TRAILS the glyph rather than replacing it.
        assert_eq!(cell(Some(Healthy), false, false, false), "🟢 disabled");
        assert_eq!(
            cell(Some(Dead), true, false, false),
            "🔴 claude /login disabled"
        );
        // `health == None` (a pre-#119 daemon sent no rollup): FALL BACK to the legacy
        // quarantine text, so an old daemon's `status` is unchanged rather than mis-reading a
        // defaulted-healthy glyph over a dead account.
        assert_eq!(cell(None, true, false, true), "needs re-login");
        assert_eq!(cell(None, false, false, false), "disabled");
    }

    #[test]
    fn render_status_shows_the_health_glyph_per_account_and_the_dead_login_cue() {
        // AC-1 end-to-end: a 4-state glyph per account, the credential-dead one showing 🔴 with
        // the `claude /login` cue, and the wide emoji (two terminal cells) keeping the table
        // aligned. The healthy account is also USAGE-EXHAUSTED (maxed session + weekly, weekly
        // blocked) — yet still 🟢, because the rollup is credential health, ORTHOGONAL to usage:
        // `claude /login` is shown ONLY for the credential-dead account, never the merely-spent
        // one ("visibly distinct from usage-exhausted").
        let healthy_but_spent = AccountStatusLine {
            health: Some(CredentialHealth::Healthy),
            weekly_exhausted: true,
            ..status_line("work", true, Some(99), Some(99))
        };
        let dead = AccountStatusLine {
            health: Some(CredentialHealth::Dead),
            quarantined: true,
            ..status_line("spare", false, None, None)
        };
        let response = StatusResponse {
            accounts: vec![healthy_but_spent, dead],
            next_swap: None,
        };
        let out = render_status(&response, NOW, None, false);
        let work = out.lines().find(|l| l.contains("work")).unwrap();
        assert!(
            work.contains("🟢") && !work.contains("claude /login"),
            "a usage-exhausted but credential-healthy account is 🟢 with no login cue: {work}"
        );
        let spare = out.lines().find(|l| l.contains("spare")).unwrap();
        assert!(
            spare.contains("🔴 claude /login"),
            "the dead account shows the red glyph and the actionable cue: {spare}"
        );
        // The glyph IS the signal — present even without color, and #15-clean.
        assert!(!out.contains('@'));
        assert!(!out.to_lowercase().contains("token"));
        // The AUTH column starts at the SAME display offset in both rows — the preceding
        // columns pad to one width despite the dead row's `n/a` cells and the healthy row's
        // `%` readings (the last column's own trailing pad is trimmed, so total line widths
        // legitimately differ; the wide-glyph cell width itself is covered by
        // `display_width_counts_terminal_cells_not_chars`).
        let glyph_offset =
            |line: &str, glyph: &str| display_width(&line[..line.find(glyph).unwrap()]);
        assert_eq!(
            glyph_offset(work, "🟢"),
            glyph_offset(spare, "🔴"),
            "the AUTH column is misaligned across rows:\n{out}"
        );
    }

    // --- status: AUTH column rename + verbose access-token clock (issue #143) --

    #[test]
    fn render_status_labels_the_credential_column_auth_not_health() {
        // #143 Part A: the credential column header is `AUTH` (was `HEALTH`) — it names the
        // credential-AUTH standing, not a vague "health" (rate-limit health lives in the `%`
        // columns). Any glyph rollup materializes the column and its label.
        let response = StatusResponse {
            accounts: vec![AccountStatusLine {
                health: Some(CredentialHealth::Healthy),
                ..status_line("work", true, Some(10), Some(20))
            }],
            next_swap: None,
        };
        let out = render_status(&response, NOW, None, false);
        let header = out.lines().next().expect("a header row");
        assert!(
            header.contains("AUTH") && !header.contains("HEALTH"),
            "the credential column header is AUTH, not HEALTH: {header:?}"
        );
    }

    #[test]
    fn render_status_renders_every_rollup_state_including_unknown_under_auth() {
        // #143 + #137: the AUTH column renders each of the four states AND the neutral ⚪
        // Unknown (#137) as its self-coloring glyph, so `status` tells "unverified" apart
        // from a genuine 🟢 at a glance — and the DEAD account keeps its `claude /login` cue.
        use CredentialHealth::{AtRisk, Dead, Healthy, Stale, Unknown};
        let line = |label, health| AccountStatusLine {
            health: Some(health),
            ..status_line(label, false, Some(10), Some(20))
        };
        let response = StatusResponse {
            accounts: vec![
                line("healthy", Healthy),
                line("unknownacct", Unknown),
                line("staleacct", Stale),
                line("atriskacct", AtRisk),
                {
                    let mut dead = line("deadacct", Dead);
                    dead.quarantined = true;
                    dead
                },
            ],
            next_swap: None,
        };
        let out = render_status(&response, NOW, None, false);
        let row = |label| out.lines().find(|l| l.contains(label)).unwrap().to_owned();
        assert!(row("healthy").contains("🟢"), "{}", row("healthy"));
        assert!(row("unknownacct").contains("⚪"), "{}", row("unknownacct"));
        assert!(row("staleacct").contains("🟡"), "{}", row("staleacct"));
        assert!(row("atriskacct").contains("🟠"), "{}", row("atriskacct"));
        assert!(
            row("deadacct").contains("🔴") && row("deadacct").contains("claude /login"),
            "the dead state keeps its glyph and re-login cue: {}",
            row("deadacct")
        );
        // Rendered under the renamed AUTH header (#143).
        assert!(out.lines().next().unwrap().contains("AUTH"));
    }

    #[test]
    fn access_token_expiry_cell_renders_future_expired_and_absent() {
        // #143 Part B: the raw access-token clock — `expires in <compact>` ahead of `now`,
        // `expired` at/past it, and an honest `unknown` when no expiry is stored (never a
        // fabricated duration). The wire clock is epoch SECONDS, differenced directly.
        assert_eq!(
            access_token_expiry_cell(Some(NOW + 4 * 3_600), NOW),
            "expires in 4h"
        );
        assert_eq!(access_token_expiry_cell(Some(NOW), NOW), "expired");
        assert_eq!(access_token_expiry_cell(Some(NOW - 60), NOW), "expired");
        assert_eq!(access_token_expiry_cell(None, NOW), "unknown");
    }

    #[test]
    fn status_verbose_surfaces_the_labeled_clock_while_the_default_table_omits_it() {
        // #143 Part B: `--verbose` surfaces the raw access-token "expires in" per account,
        // LABELLED so it is never misread as a re-login deadline; an account with no stored
        // expiry reads an honest `unknown`. The DEFAULT table stays compact — no raw clock.
        let response = StatusResponse {
            accounts: vec![
                AccountStatusLine {
                    health: Some(CredentialHealth::Healthy),
                    access_expires_at: Some(NOW + 4 * 3_600),
                    ..status_line("work", true, Some(10), Some(20))
                },
                AccountStatusLine {
                    health: Some(CredentialHealth::Unknown),
                    access_expires_at: None,
                    ..status_line("spare", false, None, None)
                },
            ],
            next_swap: None,
        };
        // Default (non-verbose) table: no raw expiry clock anywhere (AC: "no raw expiry
        // clock in the default table").
        let table = render_status(&response, NOW, None, false);
        assert!(
            !table.contains("expires in") && !table.contains("access token"),
            "the default table stays compact — no raw clock: {table}"
        );
        // Verbose block: labeled, per-account, honest placeholder for the absent one.
        let verbose = render_access_token_expiry(&response, NOW);
        assert!(
            verbose.contains("not a re-login deadline"),
            "the block is labeled so the clock is not misread as a deadline: {verbose}"
        );
        let vline = |label| {
            verbose
                .lines()
                .find(|l| l.contains(label))
                .unwrap()
                .to_owned()
        };
        assert!(
            vline("work").contains("expires in 4h"),
            "the polled account shows its access-token expiry: {}",
            vline("work")
        );
        assert!(
            vline("spare").contains("unknown"),
            "an account with no stored expiry reads an honest placeholder: {}",
            vline("spare")
        );
        // #15: sourced from labels + a timestamp only, so no email rides the surface.
        assert!(
            !verbose.contains('@'),
            "no email on the verbose surface: {verbose}"
        );
    }

    #[test]
    fn render_access_token_expiry_is_empty_for_an_empty_roster() {
        // No accounts → no block at all (the table renders its own empty state), so a bare
        // `status --verbose` on an empty roster adds nothing.
        let response = StatusResponse {
            accounts: vec![],
            next_swap: None,
        };
        assert_eq!(render_access_token_expiry(&response, NOW), "");
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
            recovering: false,
            session_resets_at: None,
            weekly_resets_at: None,
            weekly_exhausted: false,
            // The layout / alignment / coloring tests below exercise the legacy
            // (pre-#119) AUTH-column text via `health: None`; the #119 glyph rollup has its
            // own dedicated tests (`health_cell` + `render_status` with `Some(..)`).
            access_expires_at: None,
            refresh_health: None,
            health: None,
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
            recovering: false,
            session_resets_at,
            weekly_resets_at,
            weekly_exhausted,
            access_expires_at: None,
            refresh_health: None,
            health: None,
        }
    }

    // A fixed `now` for the deterministic `resets in` tests (issue #72): an
    // arbitrary epoch the per-account reset instants below are offset from.
    const NOW: i64 = 1_782_777_600;

    #[test]
    fn render_status_renders_an_aligned_paired_table_with_a_labelled_header_and_next_swap() {
        // The paired layout (issue #94) under a labelled header (issue #99): a header
        // row (`ACCOUNT`, grouped `SESSION%`+`RESET`, grouped `WEEKLY%`+`RESET`) tops
        // the table, each `%` immediately followed by its OWN reset (a single space
        // ties the pair; two spaces separate the SESSION pair from the WEEKLY pair),
        // aligned in columns — header and data measured into the SAME widths — one
        // record per line, then the forward-looking next-swap footer (#88). Healthy
        // roster (no tags) → no AUTH column, so no `AUTH` label. The exact
        // match proves the header row, the paired column order, and the alignment.
        let mut work = status_line_resets(
            "work",
            Some(97),
            Some(40),
            false,
            Some(NOW + 12 * 60),
            Some(NOW + 5 * 86_400),
        );
        work.active = true;
        let response = StatusResponse {
            accounts: vec![
                work,
                status_line_resets(
                    "spare",
                    Some(10),
                    Some(20),
                    false,
                    Some(NOW + 2 * 3_600),
                    Some(NOW + 3 * 86_400),
                ),
                status_line_resets("third", None, None, false, None, None),
            ],
            next_swap: Some(NextSwap::Target {
                to: "spare".to_owned(),
            }),
        };
        // Header labels are wider than their data, so the header sizes the columns
        // (e.g. `SESSION%` = 8 over `97%` = 3); the data left-aligns under each label.
        let expected = concat!(
            "ACCOUNT  SESSION% RESET  WEEKLY% RESET\n",
            "* work   97%      12m    40%     5d\n",
            "  spare  10%      2h     20%     3d\n",
            "  third  n/a      n/a    n/a     n/a\n",
            "\n",
            "next swap: spare\n",
        );
        assert_eq!(render_status(&response, NOW, None, false), expected);
    }

    #[test]
    fn render_status_header_is_a_single_plain_line_present_in_both_colour_modes() {
        // Issue #99: the header prints in the text view regardless of the colour gate
        // (TTY or not), is exactly one greppable line, and is plain (no SGR) in BOTH
        // modes — the per-cell tint lives on the data rows only. The `--json` full-data
        // contract is a SEPARATE surface (serialized field names), so it never carries
        // these display labels.
        let response = StatusResponse {
            accounts: vec![status_line_resets(
                "work",
                Some(50),
                Some(40),
                false,
                Some(NOW + 12 * 60),
                Some(NOW + 5 * 86_400),
            )],
            next_swap: Some(NextSwap::Target {
                to: "spare".to_owned(),
            }),
        };
        for color in [false, true] {
            let out = render_status(&response, NOW, None, color);
            let header = out.lines().next().expect("a header row");
            assert_eq!(
                header, "ACCOUNT  SESSION% RESET  WEEKLY% RESET",
                "the header prints regardless of colour={color}: {out:?}"
            );
            // Exactly one header line — greppable, one record per line below it.
            assert_eq!(
                out.lines().filter(|l| l.contains("SESSION%")).count(),
                1,
                "the header is a single line (colour={color}): {out:?}"
            );
            // Plain even under colour: the header line carries no escape byte.
            assert!(
                !header.contains('\x1b'),
                "the header is uncolored (colour={color}): {header:?}"
            );
        }
        // The `--json` surface is serialized field names, not these display labels.
        // (The rollup key is the lowercase `auth` in JSON, #143; the uppercase `AUTH`
        // display label still never appears there.)
        let json = serde_json::to_string(&response).unwrap();
        for label in ["ACCOUNT", "SESSION%", "WEEKLY%", "AUTH"] {
            assert!(
                !json.contains(label),
                "the header label {label:?} is text-view only, never in --json: {json}"
            );
        }
    }

    #[test]
    fn render_status_shows_both_session_and_weekly_resets_for_every_account() {
        // The #94 core: every account shows BOTH its session reset AND its weekly
        // reset, side by side — not the single collapsed "binding window" of #72.
        // This holds even for a weekly-EXHAUSTED account (`third`): pre-#94 it showed
        // only the weekly reset; now it shows the session reset too.
        let response = StatusResponse {
            accounts: vec![
                // healthy: session 12m, weekly 5d — both appear.
                status_line_resets(
                    "work",
                    Some(30),
                    Some(40),
                    false,
                    Some(NOW + 12 * 60),
                    Some(NOW + 5 * 86_400),
                ),
                // session-depleted, weekly fine: session 4h, weekly 3d — both appear.
                status_line_resets(
                    "spare",
                    Some(100),
                    Some(60),
                    false,
                    Some(NOW + 4 * 3_600),
                    Some(NOW + 3 * 86_400),
                ),
                // weekly-exhausted: session 2h AND weekly 3d4h — BOTH shown (the #94
                // change; #72 would have shown only the binding weekly reset).
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
        assert!(
            line("work").contains("12m") && line("work").contains("5d"),
            "both resets on the healthy row: {}",
            line("work")
        );
        assert!(
            line("spare").contains("4h") && line("spare").contains("3d"),
            "both resets on the session-depleted row: {}",
            line("spare")
        );
        assert!(
            line("third").contains("2h") && line("third").contains("3d4h"),
            "the weekly-exhausted account shows BOTH resets, not just the weekly: {}",
            line("third")
        );
        // Header row (issue #99): the FIRST line labels the columns — `ACCOUNT`, then
        // the grouped `SESSION%`+`RESET` and `WEEKLY%`+`RESET` pairs (each window's
        // reset shares the `RESET` label, disambiguated by adjacency to its `%`). No
        // tags here → no `AUTH` column. This restores a header #94 had removed.
        let header = out.lines().next().expect("a header row");
        assert!(
            header.starts_with("ACCOUNT")
                && header.contains("SESSION%")
                && header.contains("WEEKLY%")
                && header.matches("RESET").count() == 2,
            "header labels the columns in paired order: {header:?}"
        );
        assert!(
            !header.contains("AUTH"),
            "no AUTH label when no account carries a tag: {header:?}"
        );
        // Greppable: one record per line — each label on exactly one line.
        for label in ["work", "spare", "third"] {
            assert_eq!(out.lines().filter(|l| l.contains(label)).count(), 1);
        }
    }

    #[test]
    fn render_status_marks_disabled_and_quarantined_in_a_status_column() {
        // A tag on any account adds the AUTH column (issue #94), labelled
        // `AUTH` (issue #99, renamed from `HEALTH` in #143); both tags can hold at once.
        let mut quarantined = status_line("dead", false, None, None);
        quarantined.enabled = false;
        quarantined.quarantined = true;
        let response = StatusResponse {
            accounts: vec![status_line("work", true, Some(50), Some(25)), quarantined],
            next_swap: None,
        };
        let out = render_status(&response, NOW, None, false);
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
    fn render_status_drops_the_weekly_pair_first_then_health_text_when_narrow() {
        // Issue #94 degradation order: drop the WEEKLY pair (weekly% + weekly-reset)
        // FIRST and ATOMICALLY, then the health-text column — always keeping the label
        // + the SESSION pair (the soonest, most actionable reset); never wrap a row.
        // Data cells are identified by their content (`25%`, `3d`, `disabled`, `50%`,
        // `2h`); the header (issue #99) carries only labels, and each dropped column
        // takes its label with it.
        let response = StatusResponse {
            accounts: vec![{
                let mut a = status_line_resets(
                    "work",
                    Some(50),
                    Some(25),
                    false,
                    Some(NOW + 2 * 3_600),
                    Some(NOW + 3 * 86_400),
                );
                a.enabled = false; // a health-text tag, so that column exists to drop
                a
            }],
            next_swap: None,
        };
        // The header now sizes each column (issue #99): account(7=`ACCOUNT`)
        // session%(8=`SESSION%`) session-reset(5=`RESET`) weekly%(7=`WEEKLY%`)
        // weekly-reset(5=`RESET`) health-text(8=`disabled`) + gaps(0+2+1+2+1+2=8) = 48;
        // dropping the weekly pair → 33; dropping health-text too → 23.
        let full = render_status(&response, NOW, Some(200), false);
        assert!(
            full.contains("25%") && full.contains("3d") && full.contains("disabled"),
            "full table keeps both pairs and the health-text: {full}"
        );
        let full_header = full.lines().next().unwrap();
        assert!(
            full_header.contains("WEEKLY%") && full_header.contains("AUTH"),
            "the full header carries every label: {full_header:?}"
        );
        // Narrow (33 ≤ 40 < 48): the WEEKLY pair drops first, atomically — NEITHER
        // weekly% nor weekly-reset survives, and its `WEEKLY%`/`RESET` labels drop with
        // it; health-text + the session pair (and their labels) stay.
        let narrow = render_status(&response, NOW, Some(40), false);
        assert!(
            !narrow.contains("25%") && !narrow.contains("3d"),
            "the weekly pair drops first and atomically (no stranded %): {narrow}"
        );
        assert!(
            narrow.contains("disabled") && narrow.contains("50%") && narrow.contains("2h"),
            "health-text and the session pair outlive the weekly pair: {narrow}"
        );
        let narrow_header = narrow.lines().next().unwrap();
        assert!(
            narrow_header.starts_with("ACCOUNT")
                && narrow_header.contains("SESSION%")
                && narrow_header.contains("AUTH")
                && !narrow_header.contains("WEEKLY%"),
            "the WEEKLY label drops with its columns; ACCOUNT + SESSION% kept: {narrow_header:?}"
        );
        // Narrower (23 ≤ 28 < 33): health-text drops next; label + session pair (and
        // their labels) remain.
        let tiny = render_status(&response, NOW, Some(28), false);
        assert!(
            !tiny.contains("25%") && !tiny.contains("3d") && !tiny.contains("disabled"),
            "weekly pair and health-text both gone: {tiny}"
        );
        assert!(
            tiny.contains("work") && tiny.contains("50%") && tiny.contains("2h"),
            "label + session pair are always kept: {tiny}"
        );
        let tiny_header = tiny.lines().next().unwrap();
        assert!(
            tiny_header.starts_with("ACCOUNT")
                && tiny_header.contains("SESSION%")
                && !tiny_header.contains("WEEKLY%")
                && !tiny_header.contains("AUTH"),
            "only ACCOUNT + the SESSION group labels remain: {tiny_header:?}"
        );
        assert_eq!(tiny.lines().filter(|l| l.contains("work")).count(), 1);
        // Even a width too small for the essentials (23 > 5): they are NEVER dropped
        // and the row is NEVER wrapped — it simply overflows, staying one greppable
        // record per line (the terminal soft-wraps it visually).
        let overflow = render_status(&response, NOW, Some(5), false);
        assert!(
            overflow.contains("work") && overflow.contains("50%") && overflow.contains("2h"),
            "label + session pair survive any width: {overflow}"
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
        // `None` (a current daemon with no active anchor, or a pre-#88 daemon that omits
        // the field) → a bare `none`.
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
        // utilization would otherwise give. Mirrors what its WEEKLY reset cell shows
        // (the far weekly reset).
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
    fn proximity_severity_colors_a_reset_by_how_soon_it_flips() {
        // Issue #94 + #90: a reset cell's colour is its PROXIMITY, not utilization,
        // framed as RELIEF — sooner means fresh quota arriving (green), farther means
        // relief is off and the just-reset window is de-emphasized (dim), independent
        // of how depleted the account is. An imminent reset (≤ 1h) is green; a far one
        // (> 1d) is dim; in between is yellow.
        assert_eq!(
            proximity_severity(Some(NOW + 12 * 60), NOW),
            Some(Severity::Green),
            "12m out is imminent → green (relief arriving)"
        );
        assert_eq!(
            proximity_severity(Some(NOW + 5 * 86_400), NOW),
            Some(Severity::Dim),
            "5d out is far → dim (just reset, nothing to act on)"
        );
        assert_eq!(
            proximity_severity(Some(NOW + 6 * 3_600), NOW),
            Some(Severity::Yellow),
            "6h out (between 1h and 1d) → yellow"
        );
        // Proximity ignores utilization: a far reset is dim even at 99% used, and an
        // imminent reset is green even at 5% used — the worked example of a dim weekly
        // beside a green session on one row.
        assert_eq!(
            proximity_severity(Some(NOW + 5 * 86_400), NOW),
            Some(Severity::Dim)
        );
        assert_eq!(
            proximity_severity(Some(NOW + 10 * 60), NOW),
            Some(Severity::Green)
        );
        // Boundaries (`<=` imminent, `>` far): exactly 1h is still green, one second
        // past is yellow; exactly 1d is yellow, one second past is dim.
        assert_eq!(
            proximity_severity(Some(NOW + RESET_IMMINENT_SECS), NOW),
            Some(Severity::Green)
        );
        assert_eq!(
            proximity_severity(Some(NOW + RESET_IMMINENT_SECS + 1), NOW),
            Some(Severity::Yellow)
        );
        assert_eq!(
            proximity_severity(Some(NOW + RESET_FAR_SECS), NOW),
            Some(Severity::Yellow)
        );
        assert_eq!(
            proximity_severity(Some(NOW + RESET_FAR_SECS + 1), NOW),
            Some(Severity::Dim)
        );
        // An already-past reset (non-positive delta) is maximally imminent → green
        // (the window is fully available right now).
        assert_eq!(
            proximity_severity(Some(NOW - 100), NOW),
            Some(Severity::Green)
        );
        // Unknown reset instant → None: the cell shows `n/a`, which stays uncolored.
        assert_eq!(proximity_severity(None, NOW), None);
    }

    #[test]
    fn display_width_counts_terminal_cells_not_chars() {
        assert_eq!(display_width("ascii"), 5);
        assert_eq!(display_width("* work"), 6);
        // Wide CJK: each glyph is two cells (three chars → six cells).
        assert_eq!(display_width("日本語"), 6);
        assert_eq!("日本語".chars().count(), 3); // the count it must NOT use
                                                 // #137's ⚪ (U+26AA, emoji-presentation) is two cells, like the 🟢/🟡/🟠/🔴
                                                 // rollup glyphs (issue #176 relies on this), so the AUTH column stays aligned.
        assert_eq!(display_width("⚪"), 2);
        assert_eq!(display_width("🟢"), 2);
        assert_eq!(display_width("🟡"), 2);
        assert_eq!(display_width("🟠"), 2);
        assert_eq!(display_width("🔴"), 2);
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
        // Header row (issue #99): the FIRST line is the plain column-label header, and
        // — proven by the strip-equality above — it carries NO colour even with the gate
        // open (it has no escape byte at all), so the per-cell tint lives only on the
        // data rows below it.
        let first_line = colored.lines().next().unwrap();
        assert!(
            first_line.starts_with("ACCOUNT") && !first_line.contains('\x1b'),
            "first line is the plain, uncolored header: {first_line:?}"
        );
        assert!(
            colored.lines().any(|l| l.contains("calm")),
            "the account rows follow the header: {colored:?}"
        );
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
    fn color_paints_each_reset_cell_by_its_own_proximity() {
        // The #94/#90 headline: on ONE row, an imminent session reset reads GREEN
        // (relief arriving) while a far weekly reset is DIM (just reset, nothing to
        // act on) — each reset cell coloured by its own proximity, independent of
        // utilization (both `%` here are a calm green).
        let response = StatusResponse {
            accounts: vec![status_line_resets(
                "mix",
                Some(50), // session %: green band
                Some(50), // weekly %: green band
                false,
                Some(NOW + 10 * 60),    // session reset imminent → green
                Some(NOW + 5 * 86_400), // weekly reset far → dim
            )],
            next_swap: None,
        };
        let colored = render_status(&response, NOW, None, true);
        let plain = render_status(&response, NOW, None, false);
        let row = colored
            .lines()
            .find(|l| l.contains("mix"))
            .expect("a row for mix");
        // The imminent session reset is green; the far weekly reset is dim — on one row.
        assert!(
            row.contains("\x1b[32m10m"),
            "imminent session reset green: {row:?}"
        );
        assert!(row.contains("\x1b[2m5d"), "far weekly reset dim: {row:?}");
        // …and not the inverse — proving proximity, not a fixed colour, drives it.
        assert!(
            !row.contains("\x1b[2m10m"),
            "the imminent reset is not dim: {row:?}"
        );
        assert!(
            !row.contains("\x1b[32m5d"),
            "the far reset is not green: {row:?}"
        );
        // Purely additive: stripping the ANSI recovers the exact plain table.
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
        // width keeps the SESSION column aligned where `.chars().count()` would
        // misalign it — and keeps the `SESSION%` header (issue #99) over its data too.
        let response = StatusResponse {
            accounts: vec![
                status_line("ascii", true, Some(50), Some(60)),
                status_line("日本語", false, Some(10), Some(20)),
            ],
            next_swap: None,
        };
        let out = render_status(&response, NOW, None, false);
        // Each row's value begins at the same DISPLAY column as the `needle`.
        let col_of = |needle: &str| {
            let line = out.lines().find(|l| l.contains(needle)).unwrap();
            let idx = line.find(needle).unwrap();
            display_width(&line[..idx])
        };
        assert_eq!(
            col_of("50%"),
            col_of("10%"),
            "wide-label and ascii rows align the SESSION column on display width:\n{out}"
        );
        // The header's SESSION% label sits at the SAME display column as its data,
        // even though the wide-glyph label widened the ACCOUNT column (issue #99 — the
        // header is measured into the same display-width columns as the rows).
        assert_eq!(
            col_of("SESSION%"),
            col_of("50%"),
            "the SESSION% header aligns with its data column on display width:\n{out}"
        );
    }

    #[test]
    fn display_width_coalesces_zwj_flag_and_modifier_emoji() {
        // The heart of issue #176: the retired hand-rolled wcwidth approximated the
        // whole emoji block as uniformly width-2 and summed per code point, so it
        // MIS-counted every multi-code-point emoji. `unicode-width` coalesces each
        // sequence into the single width-2 glyph a terminal actually draws.
        // A ZWJ family — 👨 ZWJ 👩 ZWJ 👧 — is ONE width-2 glyph (the hand-roll said 6).
        assert_eq!(display_width("👨\u{200D}👩\u{200D}👧"), 2);
        // A skin-tone modifier merges into its base glyph (the hand-roll said 4).
        assert_eq!(display_width("👍\u{1F3FD}"), 2);
        // An emoji variation selector (U+FE0F) promotes ❤ the text-heart to its
        // width-2 emoji presentation (the hand-roll said 1 — VS16 counted as zero).
        assert_eq!(display_width("❤\u{FE0F}"), 2);
        // A regional-indicator flag pair renders as one width-2 glyph.
        assert_eq!(display_width("🇺🇸"), 2);
    }

    #[test]
    fn emoji_label_row_stays_aligned_on_display_width() {
        // Issue #176 AC: a row whose operator label carries a multi-code-point emoji
        // (a ZWJ family here — the old hand-roll mis-measured it as 6 cells) keeps the
        // SESSION column aligned with an ASCII row, because `render_cells` pads on the
        // now-correct `display_width` (2 cells for the coalesced glyph), not char count.
        let response = StatusResponse {
            accounts: vec![
                status_line("ascii", true, Some(50), Some(60)),
                status_line("👨\u{200D}👩\u{200D}👧", false, Some(10), Some(20)),
            ],
            next_swap: None,
        };
        let out = render_status(&response, NOW, None, false);
        // Each row's value begins at the same DISPLAY column as the `needle`.
        let col_of = |needle: &str| {
            let line = out.lines().find(|l| l.contains(needle)).unwrap();
            let idx = line.find(needle).unwrap();
            display_width(&line[..idx])
        };
        assert_eq!(
            col_of("50%"),
            col_of("10%"),
            "emoji-label and ascii rows align the SESSION column on display width:\n{out}"
        );
        // And the header stays over its data column, as with any wide label (issue #99).
        assert_eq!(
            col_of("SESSION%"),
            col_of("50%"),
            "the SESSION% header aligns with its data column on display width:\n{out}"
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
    fn reset_cell_renders_each_window_directly_or_n_a() {
        // Issue #94: each window's reset is rendered DIRECTLY from its own instant —
        // no binding-window collapse. A known instant humanizes; an unknown one is
        // `n/a` (never a fabricated duration), independent of utilization or the
        // weekly-exhaustion flag.
        assert_eq!(reset_cell(Some(NOW + 600), NOW), "10m");
        assert_eq!(reset_cell(Some(NOW + 2 * 3_600), NOW), "2h");
        assert_eq!(reset_cell(Some(NOW + 3 * 86_400), NOW), "3d");
        assert_eq!(reset_cell(None, NOW), "n/a");
        // Both windows of one exhausted account render their OWN instants — the
        // session reset is NOT suppressed in favour of the weekly one (the pre-#94
        // binding-window behaviour). The renderer shows both side by side.
        let exhausted = status_line_resets(
            "x",
            Some(100),
            Some(100),
            true,
            Some(NOW + 4 * 3_600),
            Some(NOW + 3 * 86_400),
        );
        assert_eq!(reset_cell(exhausted.session_resets_at, NOW), "4h");
        assert_eq!(reset_cell(exhausted.weekly_resets_at, NOW), "3d");
    }

    #[test]
    fn json_exposes_both_session_and_weekly_reset_instants() {
        // Issue #94 full-data contract: `--json` carries BOTH reset instants (raw
        // epoch seconds), regardless of terminal width — the text view may drop the
        // weekly pair on a narrow terminal, but the JSON never does. (`status --json`
        // serializes this exact response verbatim, the same surface scripts consume.)
        let response = StatusResponse {
            accounts: vec![status_line_resets(
                "work",
                Some(50),
                Some(40),
                false,
                Some(NOW + 12 * 60),
                Some(NOW + 5 * 86_400),
            )],
            next_swap: None,
        };
        let json = serde_json::to_string(&response).unwrap();
        assert!(
            json.contains("\"session_resets_at\"") && json.contains("\"weekly_resets_at\""),
            "both reset keys present: {json}"
        );
        assert!(
            json.contains(&(NOW + 12 * 60).to_string())
                && json.contains(&(NOW + 5 * 86_400).to_string()),
            "both reset instants present as raw epoch seconds: {json}"
        );
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

    #[test]
    fn status_response_decodes_a_payload_that_omits_next_swap() {
        // Backward-compatible decode (#88): a pre-#88 daemon's reply carries no
        // `next_swap` key at all. `#[serde(default)]` must decode the absent field to
        // `None` rather than fail — the round-trip test above only proves the field
        // survives when PRESENT, so this pins the ABSENT case the compat guarantee
        // actually exists for (cf. the sibling `session_resets_at` added-field convention).
        let wire = r#"{"accounts":[]}"#;
        let parsed: StatusResponse =
            serde_json::from_str(wire).expect("an absent next_swap decodes, not errors");
        assert_eq!(parsed.next_swap, None);
        assert!(parsed.accounts.is_empty());
    }

    // --- `export` verb (issue #148) -----------------------------------------

    const UUID_A: &str = "11111111-1111-1111-1111-111111111111";
    const UUID_B: &str = "22222222-2222-2222-2222-222222222222";
    const TOKEN_A: &[u8] = b"CREDENTIAL-TOKEN-AAAA-abcdef0123456789";
    const TOKEN_B: &[u8] = b"CREDENTIAL-TOKEN-BBBB-9876543210fedcba";
    const EMAIL_A: &str = "alice@example.com";
    const EMAIL_B: &str = "bob@example.com";

    /// A `StashedAccount` carrying a known bearer token + an `oauthAccount` identity
    /// block (with an email, so leak assertions have a distinctive personal identifier
    /// to search for).
    fn export_stashed(token: &[u8], uuid: &str, email: &str) -> crate::stash::StashedAccount {
        crate::stash::StashedAccount {
            credential: crate::keychain::Credential::new(token.to_vec()),
            oauth_account: crate::claude_state::OauthAccount::from_object_bytes(
                format!(r#"{{"accountUuid":"{uuid}","emailAddress":"{email}"}}"#).as_bytes(),
            )
            .expect("valid oauthAccount object"),
        }
    }

    /// A two-account roster + a `FakeAccountStash` holding both accounts' secret
    /// material — the hermetic stand-in for the real config + keychain.
    async fn export_config_and_stash() -> (Config, crate::stash::FakeAccountStash) {
        let config = config_with(vec![acct("alice", UUID_A), acct("bob", UUID_B)]);
        let stash = crate::stash::FakeAccountStash::empty();
        stash
            .write(
                &config.roster[0].stash(),
                &export_stashed(TOKEN_A, UUID_A, EMAIL_A),
            )
            .await
            .unwrap();
        stash
            .write(
                &config.roster[1].stash(),
                &export_stashed(TOKEN_B, UUID_B, EMAIL_B),
            )
            .await
            .unwrap();
        (config, stash)
    }

    /// Lowercase-hex encode — the on-the-wire form of the artifact's byte fields.
    fn hex_of(bytes: &[u8]) -> String {
        bytes.iter().map(|b| format!("{b:02x}")).collect()
    }

    /// Whether `needle` occurs anywhere in `haystack`.
    fn contains_bytes(haystack: &[u8], needle: &[u8]) -> bool {
        !needle.is_empty() && haystack.windows(needle.len()).any(|w| w == needle)
    }

    /// The container that `gather_payload` assembles round-trips losslessly through
    /// encryption, and the encrypted artifact reveals neither the tokens, the emails,
    /// nor the passphrase — and is unreadable under the wrong passphrase.
    #[tokio::test]
    async fn export_encrypted_round_trips_gathered_state_and_hides_it() {
        let (config, stash) = export_config_and_stash().await;
        let payload = gather_payload(&config, &stash, false).await.unwrap();

        // Gather fidelity: the assembled payload equals one built by hand from the same
        // rendered config + per-account stash bytes (keyed uuid → credential/oauth).
        let sa = export_stashed(TOKEN_A, UUID_A, EMAIL_A);
        let sb = export_stashed(TOKEN_B, UUID_B, EMAIL_B);
        let expected = Payload::new(
            config.render(),
            vec![
                ManagedAccount::new(
                    UUID_A.to_owned(),
                    sa.credential.expose().to_vec(),
                    sa.oauth_account.raw_json().to_vec(),
                ),
                ManagedAccount::new(
                    UUID_B.to_owned(),
                    sb.credential.expose().to_vec(),
                    sb.oauth_account.raw_json().to_vec(),
                ),
            ],
        );
        assert!(
            payload == expected,
            "gather_payload must faithfully assemble the container"
        );

        // Encrypt → serialize → parse → decrypt yields an equal payload. Passphrases
        // come from files (the #147 no-argv input path), never an argv value.
        let dir = tempfile::tempdir().unwrap();
        let pp_path = dir.path().join("pp");
        std::fs::write(&pp_path, b"correct horse battery staple\n").unwrap();
        let pp = Passphrase::from_file(&pp_path).unwrap();
        let bytes = MigrationArtifact::encrypt(&payload, &pp)
            .unwrap()
            .to_bytes();
        let parsed = MigrationArtifact::from_bytes(&bytes).unwrap();
        assert!(
            parsed.decrypt(&pp).unwrap() == payload,
            "encrypted round-trip must be lossless"
        );

        // Confidentiality: the on-disk bytes reveal neither token (raw or hex form),
        // neither email, nor the passphrase.
        assert!(
            !contains_bytes(&bytes, TOKEN_A),
            "raw token A must not appear"
        );
        assert!(
            !contains_bytes(&bytes, hex_of(TOKEN_A).as_bytes()),
            "hex token A must not appear"
        );
        assert!(
            !contains_bytes(&bytes, EMAIL_A.as_bytes()),
            "email A must not appear"
        );
        assert!(
            !contains_bytes(&bytes, b"correct horse battery staple"),
            "the passphrase must never appear in the artifact",
        );

        // Not readable without the passphrase: a wrong passphrase fails closed.
        let wrong_path = dir.path().join("wrong");
        std::fs::write(&wrong_path, b"wrong passphrase\n").unwrap();
        let wrong = Passphrase::from_file(&wrong_path).unwrap();
        assert!(
            parsed.decrypt(&wrong).is_err(),
            "wrong passphrase must fail to decrypt"
        );
    }

    /// `--no-secrets` yields a config-only artifact: no credential material for any
    /// account, and no keychain read happens for it — the roster still travels in the
    /// config, but no token or email bytes do.
    #[tokio::test]
    async fn export_no_secrets_omits_every_credential_blob() {
        let (config, stash) = export_config_and_stash().await;
        let payload = gather_payload(&config, &stash, true).await.unwrap();

        // Config-only: identical to a payload with an EMPTY account set.
        assert!(payload == Payload::new(config.render(), Vec::new()));

        // Serialize it (plaintext container) and assert the credential material is
        // wholly absent — neither raw token, nor hex token, nor email, for either
        // account — while the roster (labels/uuids) is present in the config text.
        let bytes = MigrationArtifact::plaintext(payload).to_bytes();
        assert!(
            MigrationArtifact::from_bytes(&bytes).is_ok(),
            "config-only artifact must parse"
        );
        for token in [TOKEN_A, TOKEN_B] {
            assert!(!contains_bytes(&bytes, token), "no raw credential blob");
            assert!(
                !contains_bytes(&bytes, hex_of(token).as_bytes()),
                "no hex credential blob"
            );
        }
        for email in [EMAIL_A, EMAIL_B] {
            assert!(
                !contains_bytes(&bytes, email.as_bytes()),
                "no oauthAccount email"
            );
        }
        assert!(
            contains_bytes(&bytes, UUID_A.as_bytes()),
            "the roster itself is still exported"
        );
    }

    /// A `--plaintext` export round-trips structurally and — by design — carries the
    /// secret material in the clear (the contrast the plaintext warning covers).
    #[tokio::test]
    async fn export_plaintext_round_trips_and_carries_secrets_in_the_clear() {
        let (config, stash) = export_config_and_stash().await;
        let payload = gather_payload(&config, &stash, false).await.unwrap();
        let bytes = MigrationArtifact::plaintext(payload).to_bytes();

        assert!(
            MigrationArtifact::from_bytes(&bytes).is_ok(),
            "plaintext artifact must parse"
        );
        // Unencrypted → the credential blob is present (hex-encoded) — this is exactly
        // what `PLAINTEXT_WARNING` (surfaced by `export`) warns about.
        assert!(
            contains_bytes(&bytes, hex_of(TOKEN_A).as_bytes()),
            "a plaintext export carries the credential blob in the clear",
        );
    }

    /// The file target is written atomically at mode 0600, replacing any prior file
    /// and leaving no temp residue — so a reader sees the old file or the new one.
    #[test]
    fn export_to_file_is_private_atomic_and_replaces() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.smmig");

        // A pre-existing file (world-readable) must be fully replaced by the write.
        std::fs::write(&path, b"OLD CONTENT").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();

        write_export(Some(&path), b"NEW ARTIFACT BYTES").unwrap();

        assert_eq!(
            std::fs::read(&path).unwrap(),
            b"NEW ARTIFACT BYTES",
            "old-or-new, fully replaced"
        );
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(
            mode, 0o600,
            "the artifact file must be 0600, never world-readable"
        );
        let mut tmp = path.clone().into_os_string();
        tmp.push(".tmp");
        assert!(
            !std::path::Path::new(&tmp).exists(),
            "no temp residue left behind"
        );
    }

    /// The passphrase is sourced only indirectly (file / stdin / interactive prompt) —
    /// there is no argv path that carries the secret value (issues #39 / #148).
    /// `--plaintext` wins outright and reads no passphrase at all.
    #[test]
    fn export_passphrase_source_is_never_an_argv_value() {
        let file = PathBuf::from("/some/passphrase/file");
        assert!(matches!(
            export_encryption(false, Some(file.clone()), false),
            Encryption::Encrypted(PassphraseSource::File(_)),
        ));
        assert!(matches!(
            export_encryption(false, None, true),
            Encryption::Encrypted(PassphraseSource::Stdin),
        ));
        assert!(matches!(
            export_encryption(false, None, false),
            Encryption::Encrypted(PassphraseSource::Prompt),
        ));
        // `--plaintext` short-circuits: no passphrase source is consulted.
        assert!(matches!(
            export_encryption(true, Some(file), true),
            Encryption::Plaintext
        ));
    }

    // --- `import` verb (issue #149) -----------------------------------------

    /// Build a full (credential-carrying) `Payload` from a roster of `(label, uuid)`
    /// pairs, each with a known token + email — the artifact side of an import test.
    fn import_payload(accounts: &[(&str, &str, &[u8], &str)]) -> Payload {
        let roster: Vec<Account> = accounts
            .iter()
            .map(|(label, uuid, _, _)| acct(label, uuid))
            .collect();
        let managed: Vec<ManagedAccount> = accounts
            .iter()
            .map(|(_, uuid, token, email)| {
                let stashed = export_stashed(token, uuid, email);
                ManagedAccount::new(
                    (*uuid).to_owned(),
                    stashed.credential.expose().to_vec(),
                    stashed.oauth_account.raw_json().to_vec(),
                )
            })
            .collect();
        Payload::new(config_with(roster).render(), managed)
    }

    /// A stash whose `write` always fails — proves a write failure is surfaced (the
    /// account is reported `failed` and left OUT of the roster), never swallowed.
    struct FailingWriteStash;
    impl AccountStash for FailingWriteStash {
        async fn write(&self, _service: &str, _account: &StashedAccount) -> Result<()> {
            Err(Error::Io(std::io::Error::other(
                "simulated keychain write failure",
            )))
        }
        async fn read(&self, service: &str) -> Result<StashedAccount> {
            Err(Error::StashIncomplete {
                service: service.to_owned(),
            })
        }
        async fn delete(&self, _service: &str) -> Result<()> {
            Ok(())
        }
    }

    /// A stash that ACCEPTS writes but reads back DIFFERENT bytes — proves the read-back
    /// hash-compare (outcome integrity) catches a write that did not actually persist.
    struct LyingReadStash;
    impl AccountStash for LyingReadStash {
        async fn write(&self, _service: &str, _account: &StashedAccount) -> Result<()> {
            Ok(())
        }
        async fn read(&self, _service: &str) -> Result<StashedAccount> {
            Ok(StashedAccount {
                credential: Credential::new(b"NOT-WHAT-WAS-WRITTEN".to_vec()),
                oauth_account: OauthAccount::from_object_bytes(br#"{"accountUuid":"other"}"#)
                    .unwrap(),
            })
        }
        async fn delete(&self, _service: &str) -> Result<()> {
            Ok(())
        }
    }

    /// A full export → import round-trip restores every account byte-faithfully: the
    /// encrypted artifact the export writes, once decrypted (#147) and applied, lands
    /// each account's roster entry AND both keychain stash halves byte-identical to the
    /// source — through the SAME off-argv stash write the daemon uses.
    #[tokio::test]
    async fn import_round_trips_an_encrypted_export_and_restores_every_account_byte_faithfully() {
        // Export side: gather a two-account payload, encrypt it, serialize (crypto is #147).
        let (src_config, src_stash) = export_config_and_stash().await;
        let payload = gather_payload(&src_config, &src_stash, false)
            .await
            .unwrap();
        let dir = tempfile::tempdir().unwrap();
        let pp_path = dir.path().join("pp");
        std::fs::write(&pp_path, b"correct horse battery staple\n").unwrap();
        let pp = Passphrase::from_file(&pp_path).unwrap();
        let bytes = MigrationArtifact::encrypt(&payload, &pp)
            .unwrap()
            .to_bytes();

        // Import side: parse → decrypt → apply into a FRESH target (no local config).
        let restored = MigrationArtifact::from_bytes(&bytes)
            .unwrap()
            .decrypt(&pp)
            .unwrap();
        let target = crate::stash::FakeAccountStash::empty();
        let (config, outcomes) = apply_import(None, &restored, None, &target, false)
            .await
            .unwrap();

        // Every account imported into the roster...
        assert_eq!(config.roster.len(), 2);
        assert!(outcomes
            .iter()
            .all(|o| o.outcome == ImportOutcome::Imported));
        // ...and each stash restored byte-for-byte (both halves).
        for (uuid, token, email) in [(UUID_A, TOKEN_A, EMAIL_A), (UUID_B, TOKEN_B, EMAIL_B)] {
            let back = target.read(&format!("Sessiometer/{uuid}")).await.unwrap();
            assert_eq!(
                back.credential.expose(),
                token,
                "credential restored byte-for-byte"
            );
            assert_eq!(
                back.oauth_account.raw_json(),
                export_stashed(token, uuid, email).oauth_account.raw_json(),
                "oauthAccount restored byte-for-byte"
            );
        }
    }

    /// The conflict policy: an account already present on the target is SKIPPED by
    /// default — its stash left byte-for-byte untouched — and REPLACED under `--overwrite`.
    #[tokio::test]
    async fn an_existing_account_is_skipped_by_default_and_replaced_under_overwrite() {
        let local = config_with(vec![acct("alice", UUID_A)]);
        let target = crate::stash::FakeAccountStash::empty();
        // The target already holds account A with its ORIGINAL credential.
        target
            .write(
                &local.roster[0].stash(),
                &export_stashed(b"ORIGINAL-CRED-AAAA", UUID_A, EMAIL_A),
            )
            .await
            .unwrap();
        // The artifact carries account A with a DIFFERENT (incoming) credential.
        let payload = import_payload(&[("alice", UUID_A, TOKEN_A, EMAIL_A)]);
        let service = local.roster[0].stash();

        // Default: SKIP — reported skipped, stash untouched.
        let (config, outcomes) = apply_import(None, &payload, Some(local.clone()), &target, false)
            .await
            .unwrap();
        assert_eq!(config.roster.len(), 1);
        assert_eq!(outcomes[0].outcome, ImportOutcome::Skipped);
        assert_eq!(
            target.read(&service).await.unwrap().credential.expose(),
            b"ORIGINAL-CRED-AAAA",
            "skip must leave the stash byte-for-byte untouched"
        );

        // `--overwrite`: REPLACE — reported overwritten, stash now the incoming credential.
        let (config, outcomes) = apply_import(None, &payload, Some(local.clone()), &target, true)
            .await
            .unwrap();
        assert_eq!(config.roster.len(), 1);
        assert_eq!(outcomes[0].outcome, ImportOutcome::Overwritten);
        assert_eq!(
            target.read(&service).await.unwrap().credential.expose(),
            TOKEN_A,
            "overwrite must replace the stash with the incoming credential"
        );
    }

    /// An encrypted artifact fails CLEANLY without the passphrase: it reports itself
    /// encrypted (so `import` knows to prompt), a wrong passphrase fails closed with zero
    /// plaintext, and it is not readable as plaintext.
    #[tokio::test]
    async fn an_encrypted_artifact_fails_cleanly_without_the_passphrase() {
        let (src_config, src_stash) = export_config_and_stash().await;
        let payload = gather_payload(&src_config, &src_stash, false)
            .await
            .unwrap();
        let dir = tempfile::tempdir().unwrap();
        let pp_path = dir.path().join("pp");
        std::fs::write(&pp_path, b"the real passphrase\n").unwrap();
        let pp = Passphrase::from_file(&pp_path).unwrap();
        let bytes = MigrationArtifact::encrypt(&payload, &pp)
            .unwrap()
            .to_bytes();

        let artifact = MigrationArtifact::from_bytes(&bytes).unwrap();
        assert!(
            artifact.is_encrypted(),
            "an encrypted artifact must report itself so import prompts for a passphrase"
        );
        assert!(
            artifact.clone().into_plaintext_payload().is_err(),
            "an encrypted artifact must not be readable as plaintext"
        );
        let wrong_path = dir.path().join("wrong");
        std::fs::write(&wrong_path, b"not the passphrase\n").unwrap();
        let wrong = Passphrase::from_file(&wrong_path).unwrap();
        assert!(
            artifact.decrypt(&wrong).is_err(),
            "a wrong passphrase must fail closed (no plaintext)"
        );
    }

    /// A credential write failure is surfaced, not swallowed: the account is reported
    /// `failed` and left OUT of the roster (no entry pointing at an unstashed account),
    /// while the rest of the import proceeds.
    #[tokio::test]
    async fn a_credential_write_failure_is_surfaced_not_swallowed() {
        let payload = import_payload(&[("alice", UUID_A, TOKEN_A, EMAIL_A)]);
        let (config, outcomes) = apply_import(None, &payload, None, &FailingWriteStash, false)
            .await
            .unwrap();

        assert_eq!(outcomes.len(), 1);
        assert_eq!(outcomes[0].outcome, ImportOutcome::Failed);
        assert!(
            config.roster.is_empty(),
            "a failed account must not land in the roster"
        );
        let report = import_report(&outcomes);
        assert!(
            report.contains("failed `alice`") && report.contains("1 failed"),
            "the failure must be reported loudly, per-account and in the summary"
        );
    }

    /// Outcome integrity: a write that does NOT actually persist (the store reads back
    /// different bytes) is caught by the read-back hash-compare and reported `failed` —
    /// an import never CLAIMS success for a credential it did not truly write.
    #[tokio::test]
    async fn a_write_that_does_not_persist_is_caught_by_read_back_verification() {
        let payload = import_payload(&[("alice", UUID_A, TOKEN_A, EMAIL_A)]);
        let (config, outcomes) = apply_import(None, &payload, None, &LyingReadStash, false)
            .await
            .unwrap();
        assert_eq!(outcomes[0].outcome, ImportOutcome::Failed);
        assert!(
            config.roster.is_empty(),
            "a credential that fails read-back verification must not be claimed as imported"
        );
    }

    /// A config-only artifact (an `export --no-secrets`) imports each account as a roster
    /// entry to be re-authenticated by `login` (#135) — no keychain stash is written.
    #[tokio::test]
    async fn a_config_only_artifact_imports_accounts_as_roster_entries_without_a_stash() {
        let payload = Payload::new(
            config_with(vec![acct("alice", UUID_A), acct("bob", UUID_B)]).render(),
            Vec::new(),
        );
        let target = crate::stash::FakeAccountStash::empty();
        let (config, outcomes) = apply_import(None, &payload, None, &target, false)
            .await
            .unwrap();

        assert_eq!(config.roster.len(), 2);
        assert!(outcomes
            .iter()
            .all(|o| o.outcome == ImportOutcome::Imported));
        assert_eq!(
            target.len(),
            0,
            "a config-only import writes no keychain stash (accounts are needs-re-login)"
        );
    }

    /// The passphrase is sourced only indirectly (file / stdin / interactive prompt) —
    /// no argv path carries the secret value (issues #39 / #149), symmetric with export.
    #[test]
    fn import_passphrase_source_is_never_an_argv_value() {
        let file = PathBuf::from("/some/passphrase/file");
        assert!(matches!(
            import_passphrase(Some(file), false),
            PassphraseSource::File(_)
        ));
        assert!(matches!(
            import_passphrase(None, true),
            PassphraseSource::Stdin
        ));
        assert!(matches!(
            import_passphrase(None, false),
            PassphraseSource::Prompt
        ));
    }

    /// The per-account report names accounts by their non-secret LABEL only — never a
    /// token or an email (issue #15 redaction discipline), even for a full artifact
    /// carrying both.
    #[tokio::test]
    async fn the_import_report_names_labels_only_never_a_token_or_email() {
        let (src_config, src_stash) = export_config_and_stash().await;
        let payload = gather_payload(&src_config, &src_stash, false)
            .await
            .unwrap();
        let target = crate::stash::FakeAccountStash::empty();
        let (_config, outcomes) = apply_import(None, &payload, None, &target, false)
            .await
            .unwrap();
        let report = import_report(&outcomes);

        assert!(
            report.contains("imported `alice`") && report.contains("imported `bob`"),
            "the report carries the non-secret labels"
        );
        for token in [TOKEN_A, TOKEN_B] {
            assert!(
                !contains_bytes(report.as_bytes(), token),
                "no credential token may appear in the report"
            );
        }
        for email in [EMAIL_A, EMAIL_B] {
            assert!(
                !report.contains(email),
                "no account email may appear in the report"
            );
        }
    }

    // --- [migration] tunable wiring (issue #150) ----------------------------

    #[test]
    fn resolve_import_overwrite_honours_flag_then_config_default() {
        // `--overwrite` ALWAYS forces overwrite; when it is absent, the TARGET's
        // `[migration].conflict_policy` decides (Skip by default → false).
        let skip_cfg = config_with(vec![]); // default migration → Skip
        let mut overwrite_cfg = config_with(vec![]);
        overwrite_cfg.migration.conflict_policy = ConflictPolicy::Overwrite;

        // Flag on → always overwrite, whatever the config (or absence of one).
        assert!(resolve_import_overwrite(true, None));
        assert!(resolve_import_overwrite(true, Some(&skip_cfg)));

        // Flag off → defer to the config default.
        assert!(
            !resolve_import_overwrite(false, None),
            "fresh machine (no config) → Skip default → no overwrite"
        );
        assert!(!resolve_import_overwrite(false, Some(&skip_cfg)));
        assert!(resolve_import_overwrite(false, Some(&overwrite_cfg)));
    }

    #[test]
    fn count_import_outcomes_tallies_each_outcome() {
        let outcomes = vec![
            AccountImport::imported("a"),
            AccountImport::imported("b"),
            AccountImport::skipped("c"),
            AccountImport::overwritten("d"),
            AccountImport::failed("e"),
        ];
        assert_eq!(count_import_outcomes(&outcomes), (2, 1, 1, 1));
        assert_eq!(count_import_outcomes(&[]), (0, 0, 0, 0));
    }

    /// The `[migration].conflict_policy` default is genuinely CONSUMED — with `--overwrite` off, a
    /// target whose config says `overwrite` REPLACES already-present accounts, while the default
    /// `skip` leaves them untouched. Proves the tunable drives behaviour (not ceremony), through
    /// the same `apply_import` core the verb uses.
    #[tokio::test]
    async fn the_migration_conflict_policy_default_drives_import_behaviour() {
        let (src_config, src_stash) = export_config_and_stash().await;
        let payload = gather_payload(&src_config, &src_stash, false)
            .await
            .unwrap();

        // Target already carries both accounts; its conflict_policy is Overwrite. With the flag
        // OFF, the resolved policy is overwrite → both are REPLACED, not skipped.
        let mut overwrite_target = src_config.clone();
        overwrite_target.migration.conflict_policy = ConflictPolicy::Overwrite;
        let resolved = resolve_import_overwrite(false, Some(&overwrite_target));
        assert!(
            resolved,
            "config overwrite default applies when --overwrite is off"
        );
        let (_c, outcomes) = apply_import(
            None,
            &payload,
            Some(overwrite_target),
            &crate::stash::FakeAccountStash::empty(),
            resolved,
        )
        .await
        .unwrap();
        assert_eq!(
            count_import_outcomes(&outcomes),
            (0, 0, 2, 0),
            "both already-present accounts must be OVERWRITTEN under the overwrite default"
        );

        // Contrast: the same import against a Skip-default target SKIPS both.
        let mut skip_target = src_config.clone();
        skip_target.migration.conflict_policy = ConflictPolicy::Skip;
        let resolved = resolve_import_overwrite(false, Some(&skip_target));
        assert!(
            !resolved,
            "Skip default → no overwrite when the flag is off"
        );
        let (_c, skip_outcomes) = apply_import(
            None,
            &payload,
            Some(skip_target),
            &crate::stash::FakeAccountStash::empty(),
            resolved,
        )
        .await
        .unwrap();
        assert_eq!(
            count_import_outcomes(&skip_outcomes),
            (0, 2, 0, 0),
            "the Skip default must leave both already-present accounts untouched"
        );
    }

    // ---- CLI argv parser (issue #175) ------------------------------------
    //
    // `parse` is the pure, I/O-free half of the argv layer: it maps the argument vector
    // (already past `argv[0]`) to a `Command` or a strict-usage error, WITHOUT touching
    // the keychain, roster, or daemon. That is exactly what lets the mis-parse cases the
    // issue calls out be pinned here — a typo'd `--force`, `use --help`, `status --josn` —
    // without any of the side effects the old silent-ignore parser risked.

    /// Drive `parse` the way `dispatch` does — over an owned `OsString` vector.
    fn parse_argv(args: &[&str]) -> Result<Command> {
        parse(args.iter().map(|s| std::ffi::OsString::from(*s)))
    }

    #[test]
    fn no_args_and_top_level_help_flags_map_to_the_root_overview() {
        // No args, `-h`, and `--help` at the top level all print the root usage (exit 0),
        // as the prior dispatch did for the first two.
        assert_eq!(parse_argv(&[]).unwrap(), Command::Help(HelpTopic::Root));
        assert_eq!(parse_argv(&["-h"]).unwrap(), Command::Help(HelpTopic::Root));
        assert_eq!(
            parse_argv(&["--help"]).unwrap(),
            Command::Help(HelpTopic::Root)
        );
    }

    #[test]
    fn version_flag_maps_to_version_and_the_line_carries_the_cargo_version() {
        // AC2: `--version` / `-V` surface the crate version, sourced solely from
        // `CARGO_PKG_VERSION` (`Cargo.toml`).
        assert_eq!(parse_argv(&["--version"]).unwrap(), Command::Version);
        assert_eq!(parse_argv(&["-V"]).unwrap(), Command::Version);
        assert!(version_line().starts_with("sessiometer "));
        assert!(
            version_line().contains(env!("CARGO_PKG_VERSION")),
            "the --version line must print CARGO_PKG_VERSION: {}",
            version_line()
        );
    }

    #[test]
    fn a_typoed_force_is_rejected_so_use_never_runs_an_unforced_swap() {
        // AC1 (the headline footgun): `use <acct> --forc` must NOT silently drop the flag
        // and run an UNFORCED swap — it errors, naming the offending flag and pointing at
        // the right `--help`.
        match parse_argv(&["use", "spare", "--forc"]).unwrap_err() {
            Error::CliUsage {
                message,
                usage_hint,
            } => {
                assert!(
                    message.contains("--forc"),
                    "names the offending flag: {message}"
                );
                assert_eq!(usage_hint, "sessiometer use --help");
            }
            other => panic!("expected a CliUsage error, got {other:?}"),
        }
    }

    #[test]
    fn status_rejects_a_typoed_json_flag_instead_of_printing_the_human_table() {
        // AC1: `status --josn` (typo) must not silently fall through to the human table —
        // that would break `status --josn | jq` downstream. It errors.
        let err = parse_argv(&["status", "--josn"]).unwrap_err();
        assert!(matches!(err, Error::CliUsage { .. }));
        assert!(err.to_string().contains("--josn"), "got: {err}");
    }

    #[test]
    fn use_help_prints_help_rather_than_resolving_an_account_named_help() {
        // AC1/AC3: `use --help` must print help, not try to resolve an account literally
        // named `--help` (the prior `--help`-as-positional bug).
        assert_eq!(
            parse_argv(&["use", "--help"]).unwrap(),
            Command::Help(HelpTopic::Use)
        );
    }

    #[test]
    fn capture_and_login_help_never_become_a_mutating_positional_label() {
        // AC6 (owner's #175 note): `capture --help` / `login --help` must resolve to HELP,
        // never a label — proving they perform ZERO roster/keychain writes. `parse` is
        // pure, so a `Help` result cannot mutate anything; the point is precisely that it
        // is NOT a `Capture`/`Login` command carrying `--help` as the credential label
        // (which the executor would write to stash state).
        assert_eq!(
            parse_argv(&["capture", "--help"]).unwrap(),
            Command::Help(HelpTopic::Capture)
        );
        assert_eq!(
            parse_argv(&["login", "--help"]).unwrap(),
            Command::Help(HelpTopic::Login)
        );
        assert_ne!(
            parse_argv(&["capture", "--help"]).unwrap(),
            Command::Capture {
                label: Some("--help".to_owned())
            },
            "`capture --help` must not become a capture labelled `--help`"
        );
        assert_ne!(
            parse_argv(&["login", "--help"]).unwrap(),
            Command::Login {
                label: Some("--help".to_owned())
            },
            "`login --help` must not become a login labelled `--help`"
        );
    }

    #[test]
    fn subcommand_help_is_command_specific() {
        // AC3: `<subcommand> --help` prints that subcommand's own usage, and `-h` is
        // equivalent to `--help`.
        assert_eq!(
            parse_argv(&["stats", "--help"]).unwrap(),
            Command::Help(HelpTopic::Stats)
        );
        assert_eq!(
            parse_argv(&["export", "-h"]).unwrap(),
            Command::Help(HelpTopic::Export)
        );
        assert_eq!(
            parse_argv(&["import", "--help"]).unwrap(),
            Command::Help(HelpTopic::Import)
        );
        // Each topic's text names its own verb, so the help is genuinely command-specific.
        assert!(HelpTopic::Stats.help().contains("sessiometer stats"));
        assert!(HelpTopic::Export.help().contains("sessiometer export"));
        assert!(HelpTopic::Use.help().contains("sessiometer use"));
    }

    #[test]
    fn help_is_honored_in_any_position() {
        // AC3: `-h`/`--help` works even after other flags/positionals — it short-circuits,
        // discarding the partial parse.
        assert_eq!(
            parse_argv(&["use", "spare", "--force", "--help"]).unwrap(),
            Command::Help(HelpTopic::Use)
        );
        assert_eq!(
            parse_argv(&["status", "--json", "-h"]).unwrap(),
            Command::Help(HelpTopic::Status)
        );
        // A leading top-level `-h` short-circuits before the subcommand is read.
        assert_eq!(
            parse_argv(&["-h", "capture"]).unwrap(),
            Command::Help(HelpTopic::Root)
        );
    }

    #[test]
    fn an_unknown_top_level_flag_is_rejected_but_an_unknown_command_is_unchanged() {
        // AC1: a bare unknown flag before any subcommand errors (not a silent no-op)…
        let err = parse_argv(&["--bogus"]).unwrap_err();
        assert!(matches!(err, Error::CliUsage { .. }));
        assert!(err.to_string().contains("--bogus"), "got: {err}");
        // …while an unknown SUBCOMMAND stays `UnknownCommand`, exactly as before.
        assert!(matches!(
            parse_argv(&["frobnicate"]).unwrap_err(),
            Error::UnknownCommand(cmd) if cmd == "frobnicate"
        ));
    }

    #[test]
    fn use_parses_target_and_force_in_either_order() {
        // AC4: `--force` may sit on either side of the target; flag order does not matter
        // and current behavior is preserved for valid input.
        assert_eq!(
            parse_argv(&["use", "spare", "--force"]).unwrap(),
            Command::Use {
                target: Some("spare".to_owned()),
                force: true
            }
        );
        assert_eq!(
            parse_argv(&["use", "--force", "spare"]).unwrap(),
            Command::Use {
                target: Some("spare".to_owned()),
                force: true
            }
        );
        assert_eq!(
            parse_argv(&["use", "spare"]).unwrap(),
            Command::Use {
                target: Some("spare".to_owned()),
                force: false
            }
        );
    }

    #[test]
    fn status_flags_are_order_independent() {
        // AC4: `--json`/`--no-color`/`-v` in any order yield the same command.
        let both_orders = [
            parse_argv(&["status", "--json", "--no-color", "-v"]).unwrap(),
            parse_argv(&["status", "-v", "--no-color", "--json"]).unwrap(),
        ];
        for parsed in both_orders {
            assert_eq!(
                parsed,
                Command::Status {
                    json: true,
                    no_color: true,
                    verbose: true
                }
            );
        }
        assert_eq!(
            parse_argv(&["status"]).unwrap(),
            Command::Status {
                json: false,
                no_color: false,
                verbose: false
            }
        );
    }

    #[test]
    fn run_parses_verbose_and_now_rejects_a_bogus_flag() {
        assert_eq!(
            parse_argv(&["run", "--verbose"]).unwrap(),
            Command::Run { verbose: true }
        );
        assert_eq!(
            parse_argv(&["run", "-v"]).unwrap(),
            Command::Run { verbose: true }
        );
        assert_eq!(
            parse_argv(&["run"]).unwrap(),
            Command::Run { verbose: false }
        );
        // Previously a bogus `run` flag was silently ignored; now it errors (issue #175).
        assert!(matches!(
            parse_argv(&["run", "--bogus"]).unwrap_err(),
            Error::CliUsage { .. }
        ));
    }

    #[test]
    fn optional_positional_subcommands_capture_their_label() {
        assert_eq!(
            parse_argv(&["capture"]).unwrap(),
            Command::Capture { label: None }
        );
        assert_eq!(
            parse_argv(&["capture", "work"]).unwrap(),
            Command::Capture {
                label: Some("work".to_owned())
            }
        );
        assert_eq!(
            parse_argv(&["poke"]).unwrap(),
            Command::Poke { target: None }
        );
        assert_eq!(
            parse_argv(&["remove", "work"]).unwrap(),
            Command::Remove {
                label: Some("work".to_owned())
            }
        );
        assert_eq!(
            parse_argv(&["disable", "work"]).unwrap(),
            Command::SetEnabled {
                label: Some("work".to_owned()),
                enabled: false
            }
        );
        assert_eq!(
            parse_argv(&["enable", "work"]).unwrap(),
            Command::SetEnabled {
                label: Some("work".to_owned()),
                enabled: true
            }
        );
    }

    #[test]
    fn list_takes_no_flags_but_help() {
        assert_eq!(parse_argv(&["list"]).unwrap(), Command::List);
        assert_eq!(
            parse_argv(&["list", "--help"]).unwrap(),
            Command::Help(HelpTopic::List)
        );
        assert!(matches!(
            parse_argv(&["list", "--bogus"]).unwrap_err(),
            Error::CliUsage { .. }
        ));
    }

    #[test]
    fn a_double_dash_escapes_a_positional_that_looks_like_a_flag() {
        // lexopt's `--` ends option parsing, so an unusual label starting with `-` is
        // still reachable — a safety valve now that a bare `--weird` is a rejected flag.
        assert_eq!(
            parse_argv(&["capture", "--", "--weird"]).unwrap(),
            Command::Capture {
                label: Some("--weird".to_owned())
            }
        );
    }

    #[test]
    fn stats_collects_positionals_and_value_flags_in_either_form() {
        // Positionals are the account filter; `--period`/`--since` take a value either
        // space- or `=`-separated (lexopt handles the `=`). Validation lives in `stats::run`.
        assert_eq!(
            parse_argv(&["stats", "work", "personal", "--period", "day", "--json"]).unwrap(),
            Command::Stats(crate::stats::StatsArgs {
                accounts: vec!["work".to_owned(), "personal".to_owned()],
                period: Some("day".to_owned()),
                since: None,
                json: true,
                no_color: false,
                ascii: false,
            })
        );
        assert_eq!(
            parse_argv(&["stats", "--period=week"]).unwrap(),
            Command::Stats(crate::stats::StatsArgs {
                accounts: vec![],
                period: Some("week".to_owned()),
                since: None,
                json: false,
                no_color: false,
                ascii: false,
            })
        );
    }

    #[test]
    fn a_value_bearing_flag_without_a_value_is_a_clear_error() {
        // `--period` as the last token → a clear "needs a value", not a silent empty period.
        let err = parse_argv(&["stats", "--period"]).unwrap_err();
        assert!(matches!(err, Error::CliUsage { .. }));
        assert!(err.to_string().contains("period"), "got: {err}");
    }

    #[test]
    fn export_carries_its_raw_flags_for_the_executor() {
        assert_eq!(
            parse_argv(&["export", "out.json", "--plaintext", "--no-secrets"]).unwrap(),
            Command::Export {
                path: Some(PathBuf::from("out.json")),
                no_secrets: true,
                plaintext: true,
                passphrase_file: None,
                passphrase_stdin: false,
            }
        );
        assert_eq!(
            parse_argv(&["export", "--passphrase-file", "pass.txt"]).unwrap(),
            Command::Export {
                path: None,
                no_secrets: false,
                plaintext: false,
                passphrase_file: Some(PathBuf::from("pass.txt")),
                passphrase_stdin: false,
            }
        );
    }

    #[test]
    fn import_requires_a_path_and_carries_its_flags() {
        assert_eq!(
            parse_argv(&["import", "art.json", "--overwrite"]).unwrap(),
            Command::Import {
                path: PathBuf::from("art.json"),
                overwrite: true,
                passphrase_file: None,
                passphrase_stdin: false,
            }
        );
        // Behavior preserved from the prior dispatch: a missing PATH is a hard error.
        assert!(matches!(
            parse_argv(&["import", "--overwrite"]).unwrap_err(),
            Error::MigrationImportPathRequired
        ));
    }

    #[test]
    fn a_usage_error_points_at_the_right_help_and_leaks_no_secret() {
        // AC1: every strict-usage error carries a usage hint (the exact `--help` to run)…
        let use_err = parse_argv(&["use", "--forc", "spare"])
            .unwrap_err()
            .to_string();
        assert!(
            use_err.contains("run `sessiometer use --help` for usage"),
            "got: {use_err}"
        );
        // …and #15: it names only the offending flag, never a token or email.
        let messages = [
            parse_argv(&["use", "spare", "--forc"])
                .unwrap_err()
                .to_string(),
            parse_argv(&["status", "--josn"]).unwrap_err().to_string(),
            parse_argv(&["stats", "--period"]).unwrap_err().to_string(),
            parse_argv(&["--bogus"]).unwrap_err().to_string(),
        ];
        for message in messages {
            assert!(!message.contains('@'), "no email: {message}");
            assert!(
                !message.to_lowercase().contains("token"),
                "no token: {message}"
            );
        }
    }
}
