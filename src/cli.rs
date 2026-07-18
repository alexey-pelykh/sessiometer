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
use crate::config::{Account, Config, ConflictPolicy, Origin, OriginReport};
use crate::daemon::{
    run_loop, AccountStatusLine, CanonicalScrub, Daemon, ExternalLoginWatcher, InstanceLock,
    NextSwap, NextSwapReason, NoTargetCause, RealClock, RealKeepWarmEngine, RealRosterPoller,
    RealShutdown, SchemaVersion, StatusResponse, UnixControl, VersionedStatus,
    STATUS_SCHEMA_VERSION,
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
use crate::service::AgentSupervision;
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
    /// `service install|uninstall|status` — the PERSISTENCE noun (issue #397): manage the
    /// background launchd LaunchAgent (install/uninstall) and report whether one is installed.
    Service { action: ServiceAction },
    /// `daemon status|stop|restart` — the daemon *process* (issues #396, #397): its liveness
    /// and management mode (`status`, read-only), plus stopping / restarting it (`stop` /
    /// `restart`). The process-lifecycle counterpart to the persistence-oriented `service` noun.
    Daemon { action: DaemonAction },
    /// `config path|validate|show [--origin]` — READ-ONLY config diagnostics (issue #401):
    /// resolve the `config.toml` path, parse+validate it without running, or print the effective
    /// config with each value tagged `default` vs `from-file`. Never mutates the file or the daemon.
    Config { action: ConfigAction },
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
    /// `reliability [--json]` — the OFFLINE reliability-SLO readout over the event log (#455).
    Reliability(crate::reliability::ReliabilityArgs),
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

/// The `service` sub-action (issues #166, #376, #397): the PERSISTENCE noun —
/// install/uninstall the background LaunchAgent and report whether one is installed. The
/// #397 split re-homed process lifecycle (stop/restart) to the `daemon` noun, so the pre-0.1.0
/// `start`/`stop`/`restart` sub-verbs are removed (no deprecation cycle). A plain data enum,
/// like [`Command`] — the parser resolves the sub-verb so `execute` just dispatches.
#[derive(Debug, PartialEq)]
enum ServiceAction {
    /// `service install` — write + load the LaunchAgent so `run` starts at login.
    Install,
    /// `service uninstall` — unload + remove the LaunchAgent.
    Uninstall,
    /// `service status` — is a managed service installed / enabled at login? (the "is-enabled"
    /// question; the running-process question is [`DaemonAction::Status`]).
    Status,
}

/// The `daemon` sub-action (issue #396 scaffold, extended by #397): the PROCESS-lifecycle
/// noun — counterpart to the persistence-oriented [`ServiceAction`]. `status` reports the
/// running process (read-only, #396); `stop`/`restart` (#397) act on it. A plain data enum —
/// the parser resolves the sub-verb so `execute` just dispatches. There is deliberately NO
/// standalone `start`: a daemon is started by `service install` (managed) or `sessiometer run`
/// (unmanaged), so a `daemon start` would error on an unmanaged setup and be redundant with
/// `service install` on a managed one.
#[derive(Debug, PartialEq)]
enum DaemonAction {
    /// `daemon status` — report whether a daemon is running, and how it is managed.
    Status,
    /// `daemon stop` — stop the running daemon now. Managed → `launchctl bootout`; unmanaged →
    /// a same-user-gated `{"cmd":"shutdown"}` control request. Post-condition: not running.
    Stop,
    /// `daemon restart` — restart the running daemon. Managed → `launchctl kickstart -k`;
    /// unmanaged → a clear error (nothing supervises a bare `run` to respawn it).
    Restart,
}

/// The `config` sub-action (issue #401): READ-ONLY config diagnostics. `path` prints the
/// resolved `config.toml` location, `validate` parses + validates it WITHOUT running (the same
/// seam the daemon loads through), and `show` prints the effective config — with `--origin`,
/// each value tagged `default` vs `from-file` so a silently-defaulted absent section is visible.
/// None of the three mutates the file or the daemon. A plain data enum, like [`ServiceAction`] /
/// [`DaemonAction`] — the parser resolves the sub-verb (and the `--origin` flag) so `execute`
/// just dispatches.
#[derive(Debug, PartialEq)]
enum ConfigAction {
    /// `config path` — print the resolved `config.toml` path (honours `$XDG_CONFIG_HOME`).
    Path,
    /// `config validate` — parse + validate without running; report the first error class.
    Validate,
    /// `config show [--origin]` — print the effective config; `--origin` tags each value's provenance.
    Show { origin: bool },
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
    Service,
    Daemon,
    Config,
    Status,
    List,
    Use,
    Disable,
    Enable,
    Remove,
    Poke,
    Stats,
    Reliability,
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
            HelpTopic::Service => "sessiometer service --help",
            HelpTopic::Daemon => "sessiometer daemon --help",
            HelpTopic::Config => "sessiometer config --help",
            HelpTopic::Status => "sessiometer status --help",
            HelpTopic::List => "sessiometer list --help",
            HelpTopic::Use => "sessiometer use --help",
            HelpTopic::Disable => "sessiometer disable --help",
            HelpTopic::Enable => "sessiometer enable --help",
            HelpTopic::Remove => "sessiometer remove --help",
            HelpTopic::Poke => "sessiometer poke --help",
            HelpTopic::Stats => "sessiometer stats --help",
            HelpTopic::Reliability => "sessiometer reliability --help",
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
            HelpTopic::Service => SERVICE_USAGE,
            HelpTopic::Daemon => DAEMON_USAGE,
            HelpTopic::Config => CONFIG_USAGE,
            HelpTopic::Status => STATUS_USAGE,
            HelpTopic::List => LIST_USAGE,
            HelpTopic::Use => USE_USAGE,
            HelpTopic::Disable => DISABLE_USAGE,
            HelpTopic::Enable => ENABLE_USAGE,
            HelpTopic::Remove => REMOVE_USAGE,
            HelpTopic::Poke => POKE_USAGE,
            HelpTopic::Stats => STATS_USAGE,
            HelpTopic::Reliability => RELIABILITY_USAGE,
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

/// Parse `service <install|uninstall|status>` (issues #166, #376, #397): the first positional
/// is the sub-action, `-h`/`--help` short-circuits to help, an unknown flag is rejected, and an
/// unrecognized action is a strict-usage error. Bare `service` (no action) prints the service
/// help. The #397 split removed `start`/`stop`/`restart` — they now fall into the unknown-action
/// arm (a strict error pointing at `service --help`), never a silent no-op.
fn parse_service(parser: &mut lexopt::Parser) -> Result<Command> {
    let mut action = None;
    while let Some(arg) = parser.next()? {
        match arg {
            Short('h') | Long("help") => return Ok(Command::Help(HelpTopic::Service)),
            Value(value) if action.is_none() => {
                let name = value.to_string_lossy();
                action = Some(match name.as_ref() {
                    "install" => ServiceAction::Install,
                    "uninstall" => ServiceAction::Uninstall,
                    "status" => ServiceAction::Status,
                    other => {
                        return Err(Error::CliUsage {
                            message: format!("unknown service action `{other}`"),
                            usage_hint: HelpTopic::Service.hint(),
                        })
                    }
                });
            }
            Value(_) => {} // extra positional ignored, matching the other parsers
            other => return Err(unexpected(other, HelpTopic::Service)),
        }
    }
    match action {
        Some(action) => Ok(Command::Service { action }),
        None => Ok(Command::Help(HelpTopic::Service)),
    }
}

/// Parse `daemon <status|stop|restart>` (issues #396, #397): the process-lifecycle noun. The
/// first positional is the sub-action, `-h`/`--help` short-circuits to help, an unknown flag or
/// action is a strict-usage error, and bare `daemon` (no action) prints the daemon help. Mirrors
/// [`parse_service`]; #397 grew the action set (`stop`/`restart`) without reshaping the parser.
/// There is deliberately no `start` — it falls into the unknown-action arm (see [`DaemonAction`]).
fn parse_daemon(parser: &mut lexopt::Parser) -> Result<Command> {
    let mut action = None;
    while let Some(arg) = parser.next()? {
        match arg {
            Short('h') | Long("help") => return Ok(Command::Help(HelpTopic::Daemon)),
            Value(value) if action.is_none() => {
                let name = value.to_string_lossy();
                action = Some(match name.as_ref() {
                    "status" => DaemonAction::Status,
                    "stop" => DaemonAction::Stop,
                    "restart" => DaemonAction::Restart,
                    other => {
                        return Err(Error::CliUsage {
                            message: format!("unknown daemon action `{other}`"),
                            usage_hint: HelpTopic::Daemon.hint(),
                        })
                    }
                });
            }
            Value(_) => {} // extra positional ignored, matching the other parsers
            other => return Err(unexpected(other, HelpTopic::Daemon)),
        }
    }
    match action {
        Some(action) => Ok(Command::Daemon { action }),
        None => Ok(Command::Help(HelpTopic::Daemon)),
    }
}

/// Parse `config <path|validate|show> [--origin]` (issue #401): the READ-ONLY config
/// diagnostics noun. The first positional is the sub-action; the order-independent `--origin`
/// flag applies to `show` (tag each value default-vs-file). `-h`/`--help` short-circuits, an
/// unknown flag or action is a strict-usage error, and bare `config` prints the config help.
/// Mirrors [`parse_service`] / [`parse_daemon`]; the `--origin` flag is the only shape
/// difference, and it is REJECTED on `path`/`validate` (where it is meaningless) rather than
/// silently accepted — the same strict-usage stance as an unknown flag (issue #175).
fn parse_config(parser: &mut lexopt::Parser) -> Result<Command> {
    let mut action_name = None;
    let mut origin = false;
    while let Some(arg) = parser.next()? {
        match arg {
            Short('h') | Long("help") => return Ok(Command::Help(HelpTopic::Config)),
            Long("origin") => origin = true,
            Value(value) if action_name.is_none() => {
                action_name = Some(value.to_string_lossy().into_owned());
            }
            Value(_) => {} // extra positional ignored, matching the other parsers
            other => return Err(unexpected(other, HelpTopic::Config)),
        }
    }
    let Some(name) = action_name else {
        // Bare `config` (no action) prints the config help — never a side effect.
        return Ok(Command::Help(HelpTopic::Config));
    };
    let action = match name.as_str() {
        "path" => ConfigAction::Path,
        "validate" => ConfigAction::Validate,
        "show" => ConfigAction::Show { origin },
        other => {
            return Err(Error::CliUsage {
                message: format!("unknown config action `{other}`"),
                usage_hint: HelpTopic::Config.hint(),
            })
        }
    };
    // `--origin` only means something for `show`; on `path`/`validate` it is a usage error.
    if origin && !matches!(action, ConfigAction::Show { .. }) {
        return Err(Error::CliUsage {
            message: "`--origin` applies only to `config show`".to_string(),
            usage_hint: HelpTopic::Config.hint(),
        });
    }
    Ok(Command::Config { action })
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

/// Parse `reliability [--since <duration>] [--json]` (issues #455/#494) — the offline
/// reliability-SLO readout. `--since` takes a relative-duration value (space- or
/// `=`-separated, handled by lexopt); there are no positionals. Duration parse and
/// validation live in `reliability::run`, so this layer just captures the raw string
/// (mirrors `parse_stats`).
fn parse_reliability(parser: &mut lexopt::Parser) -> Result<Command> {
    let mut json = false;
    let mut since = None;
    while let Some(arg) = parser.next()? {
        match arg {
            Short('h') | Long("help") => return Ok(Command::Help(HelpTopic::Reliability)),
            Long("json") => json = true,
            Long("since") => {
                since = Some(
                    required_value(parser, "since", HelpTopic::Reliability)?
                        .to_string_lossy()
                        .into_owned(),
                );
            }
            other => return Err(unexpected(other, HelpTopic::Reliability)),
        }
    }
    Ok(Command::Reliability(crate::reliability::ReliabilityArgs {
        json,
        since,
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
        "service" => parse_service(parser),
        "daemon" => parse_daemon(parser),
        "config" => parse_config(parser),
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
        "reliability" => parse_reliability(parser),
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
        Command::Service { action } => match action {
            ServiceAction::Install => crate::service::install().await,
            ServiceAction::Uninstall => crate::service::uninstall().await,
            ServiceAction::Status => crate::service::status().await,
        },
        Command::Daemon { action } => match action {
            DaemonAction::Status => daemon_status().await,
            DaemonAction::Stop => daemon_stop().await,
            DaemonAction::Restart => daemon_restart().await,
        },
        Command::Config { action } => match action {
            ConfigAction::Path => config_path(),
            ConfigAction::Validate => config_validate(),
            ConfigAction::Show { origin } => config_show(origin),
        },
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
        Command::Reliability(args) => crate::reliability::run(args),
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
    login [<label>]      Log in to an account (claude /login) in isolation and land it in the rotation, keeping the active account
    run [-v|--verbose]   Run the foreground daemon (poll + swap; -v adds run diagnostics)
    service <install|uninstall|status>  Persistence: install/uninstall the background launchd LaunchAgent, and report whether one is installed (auto-start at login)
    daemon <status|stop|restart>  Process lifecycle: report the running daemon (status), stop it, or restart it
    config <path|validate|show>  Read-only config diagnostics: resolve the config.toml path, validate it, or show the effective config (show --origin tags default vs from-file)
    status [--json] [--no-color] [-v|--verbose]  Show each account's usage + resets-in, and the next swap (-v adds each access token's expiry)
    list       List captured accounts
    use <account> [--force]  Switch the active account now (--force overrides the pre-swap gate)
    disable <label>      Park an account: keep it but take it out of the rotation
    enable <label>       Return a parked account to the rotation
    remove <label>       Delete an account: drop it from the rotation and erase its stash
    poke [<account>]     Run Claude Code once in an isolated config dir so it refreshes a parked account's credential (all near-expiry if omitted)
    stats [<account>...] [--period day|week|month|lifetime] [--since <when>] [--json]  Show usage over a period, offline (reads the sample store directly)
    reliability [--json]  Swap-out overshoot SLO readout, offline (reads the event log): swap-out session_pct P50/P95/P100 vs targets, time-blind, false-preempt proxy, 429 counts
    export [PATH] [--plaintext] [--no-secrets] [--passphrase-stdin]  Serialize state to an (encrypted by default) migration artifact — a file (0600) or stdout
    import <PATH> [--overwrite] [--passphrase-stdin]  Rehydrate accounts from a migration artifact — skips accounts already present unless --overwrite

OPTIONS:
    -h, --help     Print help (append it to a command for that command's usage)
    -V, --version  Print version

Run `sessiometer <command> --help` for command-specific usage.

sessiometer is unofficial: not affiliated with or endorsed by Anthropic. Claude
and Claude Code are trademarks of Anthropic, referenced only to describe what
sessiometer works with.
";

/// Per-subcommand help (issue #175): a one-line summary, a usage line, then the accepted
/// arguments and flags. Each is what `sessiometer <verb> --help` prints and matches the
/// flags the corresponding `parse_*` accepts, so help and parser stay in lockstep.
const CAPTURE_USAGE: &str = "sessiometer capture — stash the active account into the rotation

USAGE:
    sessiometer capture [<label>]

    <label>     a name for the captured account. Omit it at a terminal and capture
                offers the account's email as an editable, pre-filled default —
                press Enter to accept it, or type a shorter handle (e.g. `work`).
                Omit it when piped/scripted and the label auto-derives from the
                account-uuid (no prompt, and never the email unconfirmed).
    -h, --help  print this help
";

const LOGIN_USAGE: &str = "sessiometer login — log in to an account (claude /login) in isolation and land it in the rotation, keeping the active account

USAGE:
    sessiometer login [<label>]

    <label>     a name for the new account (auto-derived from its account-uuid if omitted)
    -h, --help  print this help

Runs the interactive login in an isolated CLAUDE_CONFIG_DIR, so a live session is
never disturbed. The login becomes the active account ONLY when it is the
already-active account (re-auth in place) or no account is active (bootstrap);
logging in a different account adds or revives it in the rotation without a swap,
and a revived quarantined account is un-quarantined at once. Switch to it with
`sessiometer use <account>` when you're ready.
";

const RUN_USAGE: &str = "sessiometer run — run the foreground daemon (poll every account's usage and swap before exhaustion)

USAGE:
    sessiometer run [-v|--verbose]

    -v, --verbose  emit per-tick run diagnostics on stderr
    -h, --help     print this help
";

const SERVICE_USAGE: &str = "sessiometer service — install the daemon as a background launchd LaunchAgent so it auto-starts at login (persistence)

USAGE:
    sessiometer service <install|uninstall|status>

    install     write + load a per-user LaunchAgent that runs `sessiometer run` at login and keeps it up across the session
    uninstall   unload + remove that LaunchAgent (stops it now, and it won't return at next login)
    status      is a managed service installed / enabled at login? (the persistence question)
    -h, --help  print this help

`service` owns PERSISTENCE — whether the daemon auto-starts at login — not the running
process. To act on the process itself (stop it, restart it) or ask whether one is running,
use `daemon` (`sessiometer daemon status|stop|restart`): `service status` answers \"is a
managed service installed?\", while `daemon status` answers \"is a daemon running?\".

The agent invokes the lock-guarded `sessiometer run`, so the background agent and a
foreground `run` can never both drive the swap loop: whichever starts second refuses
with a clear message and exits 3, performing no swap. This single-owner guard is a
safety guard — nothing bypasses it.
";

const DAEMON_USAGE: &str = "sessiometer daemon — the running daemon process: report it, stop it, restart it (process lifecycle)

USAGE:
    sessiometer daemon <status|stop|restart>

    status      report whether a daemon is running, and whether it is managed (launchd) or unmanaged (a foreground / detached `sessiometer run`)
    stop        stop the running daemon now — managed: launchctl bootout; unmanaged: a graceful control-socket shutdown (an in-flight swap completes first)
    restart     restart a managed daemon (launchctl kickstart -k); an unmanaged daemon has no restart (see below)
    -h, --help  print this help

`daemon` is the process-lifecycle counterpart to `service` (which owns the launchd
registration / auto-start persistence). `status` is READ-ONLY — it starts, stops, and
signals nothing; it asks the control socket first (a responsive daemon answers), then falls
back to the single-instance lock, so a daemon alive but not yet answering (starting up) is
reported honestly rather than as not running. If one is running, it also asks launchd whether
that process is the one it supervises.

A MANAGED daemon is one launchd is supervising right now, so it can be stopped (booted out of
the domain, which also suppresses the auto-respawn) and restarted (killed and relaunched in one
step). An UNMANAGED daemon (a foreground / detached `sessiometer run`) has no supervisor:
`daemon stop` still stops it (it shuts down gracefully over the control socket), but there is
nothing to relaunch it, so `daemon restart` is a clear error — install a managed service
(`service install`) for a supervised daemon with restart, or stop it and start a new
`sessiometer run`.

Managed means supervised, not merely registered: `daemon stop` leaves the service installed, so
a `sessiometer run` started afterwards is unmanaged even while `service status` still reports an
installed service. These verbs follow the running process.

You do not start a daemon with `daemon`: one is started by `service install` (managed, at
login) or `sessiometer run` (unmanaged, foreground) — which is why there is no `daemon start`.
";

const CONFIG_USAGE: &str = "sessiometer config — read-only config diagnostics: resolve, validate, and inspect config.toml

USAGE:
    sessiometer config <path|validate|show> [--origin]

    path        print the resolved config.toml path (honours $XDG_CONFIG_HOME, else ~/Library/Application Support/sessiometer)
    validate    parse + validate config.toml WITHOUT running; report typo'd/unknown keys, out-of-range values, and target_max_session_usage > session_trigger
    show        print the effective config (defaults filled in); with --origin, tag each value default (absent → compiled-in) vs from-file
    --origin    (with show) tag each value's provenance, so a silently-defaulted absent section is visible
    -h, --help  print this help

All three are READ-ONLY: they never write config.toml, start/stop a daemon, or change any state. `config
show --origin` surfaces effective-vs-on-disk drift — e.g. a hand-deleted [tunables] block shows every
tunable as `default`, the very drift that once went unnoticed because the effective config is only ever
emitted once to stderr at start-up.
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
    --force     override the pre-swap gate; also adopts the target when the active
                credential is gone/rotated (a forced logout), a locked keychain aside
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

const RELIABILITY_USAGE: &str = "sessiometer reliability — swap-out overshoot SLO readout, offline (reads the event log directly)

USAGE:
    sessiometer reliability [--since <duration>] [--json]

    --since <d> bound all four indicators to events at/after now - <duration>. <duration> is a
                non-negative integer with a unit: s, m, h, d, w (e.g. 30m, 24h, 7d, 2w). Omit for
                the whole-log aggregate (the default).
    --json      print the readout as JSON (schema:2, for scripts) instead of the text view
    -h, --help  print this help

READ-ONLY: it reads ~/Library/Logs/sessiometer/sessiometer.log and makes no live call, so it
works when the daemon is down. It reports four indicators, each with its target: swap-out
session_pct P50/P95/P100 (targets P50 <= 97, P100 < 99); time spent blind while near the limit; a
false-preempt proxy from the blind-window recovery reconciliation; and the usage-poll 429 vs
transient counts. By default the indicators fold the whole log; --since <duration> bounds them to a
recent window (the cutoff is documented in both output forms). The readout is roster-wide numbers
only — no per-account breakdown, no identifiers.
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

    // Load the real config (roster + tunables). A malformed or absent config FILE
    // is fatal — never silently replaced wholesale by defaults (issue #3). That
    // guarantee is per-FILE, NOT per-section: in an existing file, an absent
    // `[section]` or key silently takes its documented default (every `RawConfig`
    // field is `#[serde(default)]`) — correct and designed, but invisible, so
    // deleting a section quietly shifts effective values. #401 (`config show
    // --origin`) will surface effective-vs-on-disk; a non-roster edit reaches a
    // running daemon only on restart (#400, no hot-reload — roster is the live
    // exception, #139).
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
    )
    // Wire the per-poll usage-sample collector (#156) at the real store path so the daemon
    // records one redacted sample per successful poll. The path is INJECTED here rather than
    // resolved inside the collector (#315), so the hermetic test harness — which never wires
    // it — writes nothing to the real store. `support_dir()` already gated startup via
    // `.with_swap_lock(paths::swap_lock()?)` above, so this `?` adds no new failure mode.
    .with_usage_samples(paths::usage_samples()?)
    // Carry the CONFIG `[refresh].enabled` (#105) onto the display snapshot so the thin
    // `status` client can surface the isolated-refresh discoverability advisory (#138): with
    // the tick OFF, non-active accounts get no maintenance and their credentials silently
    // lapse. The advisory keys off the CONFIG value — what the operator set, per AC-2 — which
    // since #375 is exactly the tick's effective switch (the `claude` binary is resolved
    // per-cycle at the spawn site, no longer gated on a startup resolution below).
    .with_refresh_enabled(config.refresh.enabled)
    // The systemic refresh-failure threshold (#378): after this many consecutive sweeps fail with
    // error across every eligible account, the daemon surfaces a mechanism-down signal (event +
    // `status` indicator), distinct from per-account at-risk. Config-backed (ADR-0005 hand-emit).
    .with_systemic_failure_n(config.refresh.systemic_failure_n)
    // Arm the per-daemon target-selection seed (#612): a once-drawn process-entropy value enables
    // the velocity-aware + per-daemon-jittered selection so independent daemons over the same roster
    // disperse instead of co-selecting (and hammering) one target. Drawn from the same coarse
    // process entropy as the per-cycle jitter RNG; no new dependency, so `cargo deny` stays green.
    .with_tiebreak_seed(crate::timing::SplitMix64::from_entropy().next_u64());
    let mut shutdown = RealShutdown::new()?;

    // Name the followable stop first (issue #397): a DETACHED `run` has no controlling terminal
    // to Ctrl-C, so `daemon stop` — which reaches it over the control socket — is the guidance
    // that always works. Ctrl-C / SIGTERM stay listed for the terminal-attached case.
    eprintln!(
        "sessiometer: daemon started (polling about every {}s, jittered); \
         stop it with `sessiometer daemon stop`, Ctrl-C, or SIGTERM",
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
        target_max_session_usage: config.tunables.target_max_session_usage,
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
    // path off the poll→usage→swap seam. Wired whenever `[refresh]` is enabled — the spawn
    // binary is NO LONGER resolved here. Issue #375: each engine holds the `[refresh].claude_bin`
    // OVERRIDE and resolves `claude` PER CYCLE at its spawn site (via
    // `paths::claude_binary_with_override`), so a symlink / `$PATH` / version change AFTER startup
    // is picked up on the next cycle with no daemon restart. Resolving once here froze a `PathBuf`
    // for the daemon's whole life, so a mid-run change silently failed EVERY refresh until a manual
    // restart. A per-cycle resolution failure is non-fatal (the sweep records an `error` event; the
    // #162 poll / #282 keep-warm paths treat the `Err` fail-safe) and retried next cycle — it never
    // permanently disables the tick. When `[refresh]` is disabled the PROACTIVE paths (this periodic
    // tick + the #282 keep-warm below) are not wired; the #162 REACTIVE engine, by contrast, is
    // ALWAYS wired (#426) so on-401 recovery of a parked account is unconditional.
    let refresh_enabled = config.refresh.enabled;
    // Issue #426: the #162 REACTIVE refresh-then-retry is hoisted OUT of the `[refresh].enabled`
    // gate so `poll_refresh` is ALWAYS `Some`. A usage 401 (usually a merely-expired access token)
    // attempts one isolated refresh + re-poll BEFORE it counts toward the #42 dead-credential
    // streak — closing the false-death window the ~10×-slower periodic sweep (#105) structurally
    // cannot. This is a CORRECTNESS path, not proactive maintenance: without it a PARKED account
    // whose ~8h access token expires 401-streaks into quarantine holding a still-valid refresh
    // token (the false-🔴 the re-scope fixes). The path's #253 safety guards travel with the engine
    // UNCHANGED by the hoist: once-per-episode (`consec_401 == 0`, no `claude -p` storm) and the
    // active-account exclusion (`state.active != Some(i)`, token-first #207) — the isolated engine
    // rotates the server-side token but CAS-writes only the STASH, never the live canonical, so it
    // targets PARKED accounts only. A swap that later promotes such an account reads that SAME
    // freshened stash (`incoming = target.stash()`) and runs strictly AFTER the reactive refresh in
    // the single-threaded tick, so a refresh can never race a promotion into a torn canonical
    // (ADR-0015). The `[refresh].enabled` toggle now gates ONLY the PROACTIVE maintenance below.
    daemon = daemon.with_refresh_engine(Box::new(RealRefreshEngine::new(
        RealAccountStash::new(),
        config.refresh.claude_bin.clone(),
    )));
    if refresh_enabled {
        // Issue #282 (PROACTIVE maintenance — stays opt-in behind `[refresh].enabled`, #426): the
        // active account's canonical token is kept warm IN PLACE (proactively before expiry + a
        // reactive backstop on an active 401), minted via the isolated spawn and promoted to the
        // canonical item a live session reads. UNLIKE the #162 reactive path above, this rotates
        // the LIVE canonical token, so it stays behind the operator's opt-in: with `[refresh]` off
        // the active account lapses at expiry and recovers via the #42 emergency swap to a live
        // spare, exactly as before. `cadence()` (`[refresh].cadence_secs`) is the near-expiry
        // horizon and the proactive throttle (the near-expiry cadence is a single knob; the #468
        // proactive on/off opt-in wired below is a separate boolean gate, not a second cadence).
        // Issue #468 / finding #476 predicate C: the PROACTIVE path (the pre-emptive near-expiry
        // mint) is a SECOND, default-off opt-in NESTED here. `with_proactive_keep_warm` gates ONLY
        // that path; the REACTIVE backstop (`should_keep_warm_retry`, on an active 401) keys off the
        // engine seam alone, so it fires whenever `[refresh].enabled` wires the engine, regardless
        // of this flag. With `proactive_keep_warm = false` (the default) the active account is kept
        // warm reactively + recovered by the #467 autonomous adopt-target, cutting the ~44 % of
        // canonical churn the pre-emptive mint contributed (#476) — safe only because #467 re-based
        // the scrub it guards against to `continue`-recoverable.
        daemon = daemon
            .with_keep_warm_engine(
                Box::new(RealKeepWarmEngine::new(config.refresh.claude_bin.clone())),
                config.refresh.cadence(),
            )
            .with_proactive_keep_warm(config.refresh.proactive_keep_warm);
    }
    let mut refresh_tick = RefreshTick::new(
        config.roster.clone(),
        config.refresh.clone(),
        // The effective switch is now `[refresh].enabled` ALONE (issue #375): the engine resolves
        // `claude` per cycle, so the tick is no longer gated on a successful startup resolution —
        // that gate is exactly what froze a stale path and blocked self-healing.
        refresh_enabled,
        RealRefreshEngine::new(RealAccountStash::new(), config.refresh.claude_bin.clone()),
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
    let line = query_status(&paths::control_socket()?).await?;
    if json {
        // The full-data machine contract, regardless of terminal width (issue #72/#164): the
        // raw snapshot pretty-printed for scripts (`status --json | jq`) — the frozen envelope
        // (`schema_version` + `generated_at`) AND the payload, so a machine consumer reads the
        // version and self-gates. Emitted EVEN on a major mismatch (the raw data carries the
        // version to gate on); decoded into the typed envelope and re-serialized so the key
        // order is the struct's (serde_json has no `preserve_order`). Non-secret — the same
        // redacted payload plus a version object and a timestamp (issue #15). Never colored;
        // `--verbose` is inert here (the raw clock is already present).
        let versioned: VersionedStatus =
            serde_json::from_str(&line).map_err(|err| Error::Io(std::io::Error::other(err)))?;
        let rendered = serde_json::to_string_pretty(&versioned)
            .map_err(|err| Error::Io(std::io::Error::other(err)))?;
        println!("{rendered}");
        return Ok(());
    }
    match gate_status(&line, STATUS_SCHEMA_VERSION)? {
        // A mismatched contract MAJOR (issue #164): the daemon's snapshot field set may have
        // changed incompatibly, so DEGRADE VISIBLY — one banner, no table — rather than
        // mis-render. The raw snapshot is still available via `status --json`.
        StatusView::Mismatch { wire, supported } => {
            print!("{}", render_schema_mismatch(wire, supported));
        }
        StatusView::Render(versioned) => {
            let color = should_colorize(no_color);
            // One `now` for the freshness header, the table's "resets in", AND the verbose expiry
            // block, so they never read against different clocks within a single render.
            let now = now_epoch();
            // The snapshot-freshness header (council / #164 `generated_at`): "updated Ns ago" above
            // the table so a reader never assumes the numbers are fresh when the daemon has wedged.
            // Omitted for an empty roster (nothing to age) and a never-generated snapshot, mirroring
            // the panel (which omits the age for its connecting / empty / unsupported banners).
            if !versioned.status.accounts.is_empty() {
                print!("{}", render_snapshot_age(versioned.generated_at, now));
            }
            print!(
                "{}",
                render_status(&versioned.status, now, terminal_cols(), color)
            );
            // The verbose access-token expiry block (issue #143) trails the table — content,
            // not color, so it shows through a pipe like the rest of the table (the
            // color gate governs only the ANSI overlay).
            if verbose {
                print!("{}", render_access_token_expiry(&versioned.status, now));
            }
        }
    }
    Ok(())
}

/// Connect to the daemon's control socket at `path`, request `status`, and return the one-line
/// JSON reply VERBATIM. A connect failure that means "no daemon" — the socket is absent, or
/// present but refusing — maps to the friendly [`Error::DaemonNotRunning`]; any other connect
/// error surfaces as itself.
///
/// Returns the raw line (not a decoded struct) so the caller can apply the issue-#164
/// schema-version gate — probing the contract version INDEPENDENT of the payload
/// ([`gate_status`]) so a future incompatible major degrades to a named mismatch rather than a
/// field-level decode error — and so `--json` can re-emit the snapshot verbatim.
async fn query_status(path: &Path) -> Result<String> {
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
    Ok(line.trim_end().to_owned())
}

/// A minimal probe over a status reply that reads ONLY the frozen contract version (issue #164),
/// independent of the payload — so a future daemon whose MAJOR changed incompatibly (a field
/// removed / renamed / re-typed) is reported as a clean version mismatch rather than a confusing
/// field-level decode error. `#[serde(default)]` so a PRE-#164 reply (no `schema_version`) probes
/// as major `0`, which mismatches the current major and degrades (fail-safe).
#[derive(serde::Deserialize)]
struct SchemaProbe {
    #[serde(default)]
    schema_version: SchemaVersion,
}

/// The reference `status` client's view of a reply after the issue-#164 MAJOR gate: either the
/// compatible envelope to render, or a mismatch to report visibly.
enum StatusView {
    /// The daemon's contract major matches — render its payload.
    Render(VersionedStatus),
    /// The daemon speaks a major this build does not understand — degrade visibly.
    Mismatch {
        wire: SchemaVersion,
        supported: SchemaVersion,
    },
}

/// Apply the frozen-contract MAJOR gate (issue #164) to a raw status reply `line`: probe the
/// schema version FIRST (independent of the payload), and only fully decode the snapshot when the
/// major matches `supported`. A mismatched major returns [`StatusView::Mismatch`] so the client
/// degrades visibly rather than mis-render a payload whose fields may have shifted. Pure over the
/// line + the supported version, so the gate is unit-tested without a socket.
fn gate_status(line: &str, supported: SchemaVersion) -> Result<StatusView> {
    let probe: SchemaProbe =
        serde_json::from_str(line).map_err(|err| Error::Io(std::io::Error::other(err)))?;
    if probe.schema_version.major != supported.major {
        return Ok(StatusView::Mismatch {
            wire: probe.schema_version,
            supported,
        });
    }
    let versioned: VersionedStatus =
        serde_json::from_str(line).map_err(|err| Error::Io(std::io::Error::other(err)))?;
    Ok(StatusView::Render(versioned))
}

/// The visible degrade a text `status` prints when the daemon's snapshot contract MAJOR (issue
/// #164) differs from the one this build renders: name BOTH versions and point at the raw
/// `--json` view, rather than mis-render a table whose fields may have changed. Pure, so the
/// message is unit-tested. Carries no account data — only the two version numbers and static
/// text — so it is redaction-clean (issue #15) by construction.
fn render_schema_mismatch(wire: SchemaVersion, supported: SchemaVersion) -> String {
    format!(
        "status: the daemon speaks snapshot schema v{}.{}, but this build renders v{}.{} — \
         refusing to render a contract it may mis-read. Upgrade sessiometer; \
         `sessiometer status --json` still emits the raw snapshot.\n",
        wire.major, wire.minor, supported.major, supported.minor,
    )
}

/// How long `daemon status` waits for the control socket to answer before falling back to the
/// single-instance lock (issue #396). A local daemon answers `status` off an in-memory
/// snapshot near-instantly, so this is generous headroom — not a latency budget. It exists
/// only so a mid-startup daemon (socket bound but not yet accepting) or a wedged one does not
/// hang the report; on timeout the lock fallback still tells alive-but-unresponsive from
/// not-running.
const DAEMON_STATUS_SOCKET_TIMEOUT: Duration = Duration::from_secs(2);

/// How long `daemon stop` waits for the daemon to acknowledge a `{"cmd":"shutdown"}` request
/// (issue #397). Much larger than [`DAEMON_STATUS_SOCKET_TIMEOUT`], and NOT a latency budget: the
/// daemon accepts control connections only *between* ticks, and a tick can span a per-account poll
/// (`curl --max-time 30`) or — when refresh is enabled (opt-in) — a sweep that walks the parked
/// roster SEQUENTIALLY, each account bounded by `RefreshConfig::timeout()` (default 90s). So the
/// true worst case scales with the roster and can exceed this 60s on a busy refresh cycle; the
/// value trades a bounded wait against out-waiting the *common* poll-length window rather than
/// pretending to cover every configuration.
///
/// That residual is deliberately SAFE, not merely tolerated. A daemon busier than this still
/// RECEIVES the request — it is queued in the socket buffer — and the daemon's control handler
/// honours it even if the ack can no longer be delivered. So an over-budget cycle produces an honest
/// "did not acknowledge" (exit `1`) while the stop still happens on the next between-ticks gap: a
/// false FAILURE the operator can retry, NEVER a stop that silently did not happen, and NEVER a
/// success that silently did. A `status` probe that misses the window has a lock fallback and still
/// answers; a `stop` has none, which is why it waits far longer than `status`.
const DAEMON_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(60);

/// The daemon *process* liveness, as `daemon status` projects it (issue #396) from two
/// read-only probes: the control socket (primary) and the single-instance lock (fallback).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DaemonLiveness {
    /// The control socket answered a `status` request — a live, responsive daemon.
    Responsive,
    /// The socket did not answer, but the single-instance lock is held — a live daemon not
    /// answering yet (starting up, or wedged). Reported honestly, NOT as "not running".
    AliveUnresponsive,
    /// Neither the socket answers nor the lock is held — no daemon is running.
    NotRunning,
}

/// Probe the daemon *process* liveness (issues #396, #397), socket-primary and lock-fallback: a
/// responsive control socket ⇒ running; otherwise a held single-instance lock ⇒ alive-but-
/// unresponsive (the honest startup / wedged case); otherwise not running. Both probes are
/// READ-ONLY — nothing is started, stopped, or signalled. Shared by `daemon status` (which
/// reports it) and `daemon restart` (which refuses to bootstrap over a running daemon).
async fn daemon_liveness() -> Result<DaemonLiveness> {
    if probe_socket_responsive(&paths::control_socket()?).await {
        Ok(DaemonLiveness::Responsive)
    } else if InstanceLock::is_held(&paths::daemon_lock()?)? {
        Ok(DaemonLiveness::AliveUnresponsive)
    } else {
        Ok(DaemonLiveness::NotRunning)
    }
}

/// Report the daemon *process* — is it alive, and is launchd supervising it (issue #396)? The
/// process-lifecycle counterpart to `service status`, which speaks only to the launchd
/// registration and exits non-zero when none is installed (even beside a healthy daemon).
/// This is READ-ONLY — it starts, stops, and signals nothing: a socket `status` query, a
/// non-blocking lock probe, and a `launchctl print` probe.
///
/// Liveness comes from [`daemon_liveness`]. Management mode is *supervision*
/// ([`AgentSupervision::Supervising`]) — NOT plist existence, and not mere registration either. A
/// booted-out agent leaves its plist on disk, and a registered-but-idle job leaves its label in the
/// domain; in both states a foreground `run` can own the process, so either weaker signal would
/// mislabel it "managed by launchd" (issue #397). Prints one report line to stdout and returns `Ok`
/// (exit `0`) whenever it can determine state.
async fn daemon_status() -> Result<()> {
    let liveness = daemon_liveness().await?;
    // Management mode is only meaningful for a running daemon — the renderer ignores it otherwise —
    // so skip the `launchctl` probe entirely when nothing is running. That keeps the not-running
    // report from depending on a subprocess that could fail, and spares a spawn.
    let managed = match liveness {
        DaemonLiveness::NotRunning => false,
        DaemonLiveness::Responsive | DaemonLiveness::AliveUnresponsive => {
            crate::service::agent_supervision().await? == AgentSupervision::Supervising
        }
    };
    print!("{}", render_daemon_status(liveness, managed));
    Ok(())
}

/// How `daemon stop` reaches its "not running" post-condition (issue #397). Dispatch turns on what
/// launchd is doing about the agent ([`AgentSupervision`]) — never on `plist.exists()`
/// ([`crate::service::is_managed`]), which is registration, and never on mere domain membership,
/// which is not supervision.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StopPlan {
    /// launchd is supervising the daemon ⇒ `launchctl bootout` alone. REQUIRED, not merely
    /// sufficient: the agent is `KeepAlive`, so a socket shutdown would just be respawned. Only
    /// leaving the domain makes "stopped" stick, and the process it stops IS the running daemon.
    BootOut,
    /// The job is in the domain but launchd runs no process for it, so a foreground `run` may own
    /// the daemon ⇒ do BOTH, in order. The bootout is what stops `KeepAlive` from respawning a
    /// replacement the instant the real daemon exits; the socket shutdown is what actually stops
    /// that daemon. Either alone leaves a daemon running.
    BootOutThenSocketShutdown,
    /// No job in the domain ⇒ nothing supervises anything: a same-user-gated `{"cmd":"shutdown"}`
    /// control request, driving the daemon's graceful exit (an in-flight swap completes first).
    /// Nothing listening ⇒ the post-condition already holds.
    SocketShutdown,
}

/// Pure dispatch for `daemon stop` — see [`StopPlan`] for why all three cases exist.
fn plan_stop(agent: AgentSupervision) -> StopPlan {
    match agent {
        AgentSupervision::Supervising => StopPlan::BootOut,
        AgentSupervision::RegisteredIdle => StopPlan::BootOutThenSocketShutdown,
        AgentSupervision::Unregistered => StopPlan::SocketShutdown,
    }
}

/// How `daemon restart` acts on each reachable daemon state (issue #397). Restart is the one verb
/// that cannot be made to work universally: only launchd can kill-and-relaunch, so a daemon it does
/// not supervise gets a clear error instead of a half-restart.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RestartPlan {
    /// launchd owns the daemon (or owns an idle job with nothing else running) ⇒ `kickstart -k`,
    /// the atomic kill+relaunch. `kickstart` also STARTS a registered job that is not running.
    Kickstart,
    /// Registered nowhere, plist on disk, nothing running ⇒ `launchctl bootstrap`: load it now.
    Bootstrap,
    /// A daemon is running that launchd does not supervise ⇒ [`Error::UnmanagedDaemonNoRestart`].
    RefuseUnmanaged,
    /// Nothing running and no service registered ⇒ [`Error::NoManagedService`].
    RefuseNoService,
}

/// Pure dispatch for `daemon restart` (issue #397), exhaustively unit-tested.
///
/// [`AgentSupervision::Supervising`] settles it: that process holds the single-instance lock, so no
/// foreground daemon can coexist and a kickstart restarts the daemon the operator meant. Otherwise a
/// RUNNING daemon is one launchd does not supervise, and must be refused — kickstarting or
/// bootstrapping beside it hands launchd a `run` that loses the lock, exits `3`, and is respawned
/// into a throttled crash loop. Only with nothing running does registration mean "bring it up".
fn plan_restart(
    agent: AgentSupervision,
    daemon_running: bool,
    service_installed: bool,
) -> RestartPlan {
    match agent {
        AgentSupervision::Supervising => RestartPlan::Kickstart,
        AgentSupervision::RegisteredIdle if daemon_running => RestartPlan::RefuseUnmanaged,
        AgentSupervision::RegisteredIdle => RestartPlan::Kickstart,
        AgentSupervision::Unregistered if daemon_running => RestartPlan::RefuseUnmanaged,
        AgentSupervision::Unregistered if service_installed => RestartPlan::Bootstrap,
        AgentSupervision::Unregistered => RestartPlan::RefuseNoService,
    }
}

/// Stop the running daemon now (issue #397's `daemon stop`), reaching a uniform "not running"
/// post-condition in every state — see [`StopPlan`] for the dispatch and why supervision, not
/// registration, decides it.
///
/// NEVER discovers a PID to signal: there is no pidfile, the single-instance `flock` carries no
/// holder PID, and `kill(2)` is PID-reuse-racy.
async fn daemon_stop() -> Result<()> {
    match plan_stop(crate::service::agent_supervision().await?) {
        // launchd supervises the daemon: booting the agent out IS the stop, and it disarms the
        // `KeepAlive` respawn in the same step.
        StopPlan::BootOut => crate::service::stop_managed().await,
        // The registered agent is idle while a foreground `run` may own the daemon. Bootout FIRST,
        // to disarm `KeepAlive` so it cannot respawn a replacement, THEN stop the running daemon
        // over the socket. Neither half alone is the whole story, so narrate the compound stop with
        // one coherent message rather than stacking two primitive ones.
        StopPlan::BootOutThenSocketShutdown => {
            crate::service::bootout_agent().await?;
            socket_shutdown(
                "sessiometer: daemon stop requested. The registered launchd agent was idle (booted \
                 out so it cannot respawn); the running `sessiometer run` exits gracefully after \
                 any in-flight swap completes.",
                "sessiometer: daemon is not running — the idle launchd agent has been booted out so \
                 it cannot respawn.",
            )
            .await
        }
        // Nothing in the launchd domain: ask whatever is running to stop itself.
        StopPlan::SocketShutdown => {
            socket_shutdown(
                "sessiometer: daemon stop requested (unmanaged `sessiometer run`). It exits \
                 gracefully after any in-flight swap completes.",
                "sessiometer: daemon is not running (nothing to stop).",
            )
            .await
        }
    }
}

/// Send a graceful `{"cmd":"shutdown"}` to the daemon over the control socket and report the outcome
/// (issue #397). Shared by `daemon stop`'s two socket-driven branches, which differ only in wording.
///
/// A missing / refused socket means no daemon is running — the stop post-condition already holds, so
/// that maps to an idempotent success (`on_not_running`), never the `DaemonNotRunning` error a
/// `status` client would raise. Any other failure (timeout, an unexpected reply) propagates.
async fn socket_shutdown(on_ok: &str, on_not_running: &str) -> Result<()> {
    match request_shutdown(&paths::control_socket()?).await {
        Ok(()) => {
            eprintln!("{on_ok}");
            Ok(())
        }
        Err(Error::DaemonNotRunning) => {
            eprintln!("{on_not_running}");
            Ok(())
        }
        Err(err) => Err(err),
    }
}

/// Restart the running daemon (issue #397's `daemon restart`) — see [`RestartPlan`] for the four
/// reachable outcomes. Only launchd can kill-and-relaunch, so a daemon it does not supervise gets a
/// clear, actionable error rather than a half-restart.
async fn daemon_restart() -> Result<()> {
    let agent = crate::service::agent_supervision().await?;
    // Probe only what can still change the decision: a supervising agent settles it (see
    // `plan_restart`), and liveness settles the unsupervised case before registration is consulted.
    let supervising = agent == AgentSupervision::Supervising;
    let daemon_running = !supervising && daemon_liveness().await? != DaemonLiveness::NotRunning;
    let service_installed =
        agent == AgentSupervision::Unregistered && !daemon_running && crate::service::is_managed()?;
    match plan_restart(agent, daemon_running, service_installed) {
        RestartPlan::Kickstart => crate::service::kickstart_managed().await,
        RestartPlan::Bootstrap => crate::service::bootstrap_managed().await,
        RestartPlan::RefuseUnmanaged => Err(Error::UnmanagedDaemonNoRestart),
        RestartPlan::RefuseNoService => Err(Error::NoManagedService),
    }
}

/// Ask a running UNMANAGED daemon to stop over its control socket (issue #397): connect, send the
/// same-user-gated `{"cmd":"shutdown"}` verb, and read the one-line ack. Returns `Ok(())` once the
/// daemon acknowledged (`{"ok":true}`) — the daemon then drives its existing graceful shutdown (an
/// in-flight swap completes before exit). A connect failure that means "no daemon" — the socket is
/// absent, or present but refusing — maps to [`Error::DaemonNotRunning`] (the caller, `daemon_stop`,
/// treats that as an idempotent "already not running"). Any other reply than the `{"ok":true}` ack
/// (an `{"error":…}` from the same-user peer gate — which should not happen for our own uid — or an
/// unexpected line) surfaces as an I/O error carrying the reply, never a false success.
///
/// The request carries NO credential and NO payload — a pure stop signal, gated same-user on the
/// daemon side ([`crate::daemon::peer_is_same_user`]). Time-boxed by [`DAEMON_SHUTDOWN_TIMEOUT`] —
/// generous, because a busy daemon serves the socket only between ticks — so a wedged daemon that
/// binds the socket but never answers cannot hang `daemon stop` forever.
async fn request_shutdown(path: &Path) -> Result<()> {
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

    // The same newline-delimited JSON the daemon's `serve_control` speaks: write one request
    // line, read one reply line. The whole exchange is time-boxed so a socket-bound-but-wedged
    // daemon cannot hang the verb.
    let mut buffered = tokio::io::BufReader::new(stream);
    let exchange = async {
        buffered.write_all(b"{\"cmd\":\"shutdown\"}\n").await?;
        buffered.flush().await?;
        let mut line = String::new();
        buffered.read_line(&mut line).await?;
        Ok::<String, Error>(line.trim_end().to_owned())
    };
    let reply = match tokio::time::timeout(DAEMON_SHUTDOWN_TIMEOUT, exchange).await {
        Ok(result) => result?,
        Err(_) => {
            return Err(Error::Io(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "daemon did not acknowledge the shutdown request",
            )))
        }
    };
    // The daemon acks a same-user shutdown with `{"ok":true}`; an `{"error":…}` (the peer gate
    // refusing) or anything unexpected is NOT a success — never report a stop that did not happen.
    if reply.contains(r#""ok":true"#) {
        Ok(())
    } else {
        Err(Error::Io(std::io::Error::other(format!(
            "daemon refused the shutdown request: {reply}"
        ))))
    }
}

/// Probe whether the daemon's control socket answers a `status` request within
/// [`DAEMON_STATUS_SOCKET_TIMEOUT`] (issue #396). Read-only: it opens a client connection and
/// sends the EXISTING `status` verb (no new wire verb — issue note), then waits for any reply.
/// `true` only if the daemon answered; a missing/refused socket, a bounded timeout (socket
/// bound but not accepting yet, or wedged), or a read error all read as "not responsive",
/// leaving the lock fallback to tell alive-but-unresponsive from not-running. Dropping the
/// timed-out future closes only this client connection — the daemon is neither signalled nor
/// disturbed.
async fn probe_socket_responsive(path: &Path) -> bool {
    matches!(
        tokio::time::timeout(DAEMON_STATUS_SOCKET_TIMEOUT, query_status(path)).await,
        Ok(Ok(_))
    )
}

/// `config path` (issue #401): print the resolved `config.toml` path — the SAME
/// [`paths::config_file`] the daemon and every verb load through, so it names the real
/// location (honouring `$XDG_CONFIG_HOME`, else the native support dir) rather than a
/// re-derived guess. Read-only.
fn config_path() -> Result<()> {
    println!("{}", paths::config_file()?.display());
    Ok(())
}

/// `config validate` (issue #401): parse + validate `config.toml` WITHOUT running, routing
/// through the SAME [`Config::load_path`] seam the daemon loads through — so a typo'd/unknown
/// key (`deny_unknown_fields` → [`Error::ConfigParse`]), an out-of-range value
/// ([`Error::ConfigInvalid`]), or `target_max_session_usage > session_trigger`
/// ([`Error::ConfigTargetMaxSessionAboveTrigger`]) surfaces here with the identical message the daemon
/// would fail on, and a clean file reports valid. Read-only: it loads and validates, nothing
/// more. A validation failure propagates as the loader's error, so it exits non-zero (usable
/// in a pre-flight check) — `main` prints it and maps the exit code.
///
/// A VALID file may still trail a non-fatal advisory (issue #608): this is the one surface that
/// renders [`Config::peak_runway_advisory`], the swap-target reserve's peak-velocity runway
/// coupling. It does not affect the exit code — the file IS valid — so the pre-flight use stays
/// intact.
fn config_validate() -> Result<()> {
    let path = paths::config_file()?;
    let config = Config::load_path(&path)?;
    print!("{}", render_config_validate(&path, &config));
    Ok(())
}

/// Render `config validate`'s output: the valid-file line, plus the non-fatal peak-velocity runway
/// advisory when the reserve exceeds its bound (issue #608). Pure — no I/O — so the
/// state→text mapping is unit-tested without touching a real config path, matching
/// [`render_config_origin`].
fn render_config_validate(path: &Path, config: &Config) -> String {
    let count = config.roster.len();
    let plural = if count == 1 { "" } else { "s" };
    let mut out = format!("{} is valid ({count} account{plural})\n", path.display());
    if let Some(a) = config.peak_runway_advisory() {
        out.push_str(&render_peak_runway_advisory(&a));
    }
    out
}

/// Render the non-fatal peak-velocity runway advisory line (issue #608). Pure — a function of the
/// [`crate::config::PeakRunwayAdvisory`] alone — so its exact operator-facing text is unit-tested
/// without a `Config`. Actionable-first (the remedy names the exact tunables and a concrete value); the
/// mechanism follows so the number is not a bare oracle. No internal cross-references — an operator
/// cannot resolve an issue or ADR number from a terminal (CLAUDE.md audience fidelity).
fn render_peak_runway_advisory(a: &crate::config::PeakRunwayAdvisory) -> String {
    // Locals + inline captures (this file's idiom — `{count}`, `{p50}`, `{edge}` …) so the two
    // values each used twice (`reserve`, `bound`) read by name rather than by positional count.
    let (reserve, bound, window) = (a.target_max_session_usage, a.bound_pct, a.window_secs);
    let v_peak = crate::swap::V_PEAK_SESSION_PCT_PER_MIN;
    format!(
        "advisory: target_max_session_usage ({reserve}) exceeds the peak-velocity runway bound \
         ({bound}).\n\
         \x20 Lower it to {bound} or below, or narrow near_limit_poll_secs / \
         session_velocity_horizon_secs\n\
         \x20 (together they set the {window}s swap lookahead). At the assumed {v_peak} %/min peak, \
         an account swapped\n\
         \x20 to at {reserve}% is already past its own swap fire point over that lookahead, so it \
         can swap\n\
         \x20 straight back out. A tuning note, not an error — the shipped defaults sit here too.\n",
    )
}

/// `config show [--origin]` (issue #401): print the effective config the daemon WOULD load
/// (defaults filled in). With `--origin`, each value trails a `default` / `from-file` tag and
/// an absent `[section]` is flagged — surfacing the effective-vs-on-disk drift that motivated
/// #401 (a hand-deleted `[tunables]` block reads as all-`default`). Read-only: it loads and
/// formats, never writes. An invalid file surfaces the same error as `config validate`.
fn config_show(origin: bool) -> Result<()> {
    let path = paths::config_file()?;
    let report = Config::load_with_origin(&path)?;
    print!("{}", render_config_origin(&path, &report, origin));
    Ok(())
}

/// Render the effective-config view for `config show [--origin]` (issue #401). With `origin`,
/// each value trails a `default` / `from-file` tag and an absent `[section]` is flagged, so
/// silently-defaulted drift is visible; without it, the same values print untagged. Columns are
/// aligned per section (by Unicode-scalar count, matching Rust's fill semantics); pure — no I/O,
/// no colour — so the state→text mapping is unit-tested without touching a real config path.
fn render_config_origin(path: &Path, report: &OriginReport, origin: bool) -> String {
    let mut out = String::new();
    out.push_str("# effective configuration\n");
    out.push_str(&format!("# {}\n", path.display()));

    for section in &report.sections {
        out.push('\n');
        if origin && !section.present {
            out.push_str(&format!("{}  (absent — all defaults)\n", section.header));
        } else {
            out.push_str(section.header);
            out.push('\n');
        }
        let key_w = section
            .entries
            .iter()
            .map(|e| e.key.chars().count())
            .max()
            .unwrap_or(0);
        // The value column is padded only in --origin mode, to align the trailing tag;
        // without a tag there is nothing to align to, so the scan is skipped.
        let val_w = if origin {
            section
                .entries
                .iter()
                .map(|e| e.value.chars().count())
                .max()
                .unwrap_or(0)
        } else {
            0
        };
        for e in &section.entries {
            if origin {
                let tag = match e.origin {
                    Origin::Default => "default",
                    Origin::FromFile => "from-file",
                };
                out.push_str(&format!(
                    "  {key:<key_w$} = {value:<val_w$}  {tag}\n",
                    key = e.key,
                    value = e.value,
                ));
            } else {
                out.push_str(&format!(
                    "  {key:<key_w$} = {value}\n",
                    key = e.key,
                    value = e.value
                ));
            }
        }
    }

    // The roster is `list`'s detailed job; here it is a one-line effective summary.
    out.push('\n');
    let count = report.roster_count;
    let plural = if count == 1 { "" } else { "s" };
    if origin {
        let roster_origin = if report.roster_present {
            "from-file"
        } else {
            "default"
        };
        out.push_str(&format!(
            "[[account]]  ({count} account{plural}, {roster_origin})\n"
        ));
    } else {
        out.push_str(&format!("[[account]]  ({count} account{plural})\n"));
    }
    out
}

/// The report `daemon status` prints for a [`DaemonLiveness`] × management-mode pair (issue
/// #396). Pure (no I/O) so the state→text mapping is unit-tested without a socket, lock, or
/// plist. `managed` is read only for a running daemon (managed = launchd is supervising it, i.e.
/// the agent is loaded — NOT merely that a plist is installed; unmanaged = a foreground / detached
/// `sessiometer run`); the not-running report carries no management mode. Trailing newline included.
fn render_daemon_status(liveness: DaemonLiveness, managed: bool) -> String {
    match liveness {
        DaemonLiveness::Responsive => format!(
            "sessiometer: daemon is running and responsive{}\n",
            management_suffix(managed),
        ),
        DaemonLiveness::AliveUnresponsive => format!(
            "sessiometer: daemon is running but not answering the control socket yet — \
             starting up or busy{}\n",
            management_suffix(managed),
        ),
        DaemonLiveness::NotRunning => "sessiometer: daemon is not running.\n".to_owned(),
    }
}

/// The management-mode tail shared by the two running-daemon reports (issue #396): managed
/// (launchd LaunchAgent) vs unmanaged (a foreground / detached `sessiometer run`). Carries the
/// trailing period so each base report reads as one sentence.
fn management_suffix(managed: bool) -> &'static str {
    if managed {
        " (managed by launchd)."
    } else {
        " (unmanaged: a foreground or detached `sessiometer run`)."
    }
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
/// reset instants, a next-swap candidate label — so it can never print a token, nor any
/// email EXCEPT an operator-authored account label the operator chose to set as their own
/// label (issue #15; #444 — an authored email label is a permitted value, never a leak);
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
        // The AUTH column carries the credential-auth state — the 5-state+Unknown glyph
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
    // The blind ACTIVE account's retained-anchor projection (issue #479, umbrella #363 Path B), or
    // `None` when the active account is not in bounded blindness. Resolved ONCE so the per-account
    // blind line AND the cornered-state detection below read the SAME value. `blind_active` is set
    // only on the active account and only while blind (a pre-#479 daemon omits it → `None`).
    let active_blind = response
        .accounts
        .iter()
        .find(|account| account.active)
        .and_then(|account| account.blind_active.map(|blind| (&account.label, blind)));

    // CORNERED (issue #479, surface 3): the active account is blind, ADR-0017 auto-protection is
    // DEGRADED (the preemptive gate is armed but acting on a STALE anchor), AND there is no viable
    // target to swap to — the one bounded-blindness state the daemon CANNOT resolve itself, so the
    // operator must act. Keying off `auto_protection_degraded` (blind PAST the interim gate window,
    // anchor at/over the risk band) rather than the raw last-known % is deliberate: before the gate
    // window the daemon is still self-resolving by waiting out a transient blind blip, so a loudest
    // alarm THEN would cry wolf — DEGRADED is exactly "auto-protection WOULD swap now but can't".
    // Composes two daemon verdicts (`blind_active` + `next_swap == no_viable_target`) already on the
    // wire, so it needs no new field. `active_blind` is `Copy`, so this leaves it usable below.
    let cornered = match (active_blind, &response.next_swap) {
        (Some((label, blind)), Some(NextSwap::NoViableTarget { cause, resets_at }))
            if blind.auto_protection_degraded =>
        {
            Some((label, blind, cause, resets_at))
        }
        _ => None,
    };

    if let Some((label, blind, cause, resets_at)) = cornered {
        // The loudest, distinct state: blind + DEGRADED + nowhere to swap. Name the source, how long
        // blind, the stale last-known %, WHY the fleet is blocked (the relief cause/reset FOLDED IN
        // from `next_swap`, so the remedy is not lost when this alarm replaces that footer), and the
        // ONE remedy only the operator can apply — add or free an account. Printed as DATA
        // (unconditional, survives a pipe / redirect), red-emphasized when the color gate is open
        // (the SAME SGR the DEGRADED / systemic lines use); the plain text conveys it under
        // --no-color. The surface only REFLECTS this daemon-pushed state; it never self-swaps (#169).
        let dur = humanize_until(blind.blind_secs as i64);
        let last_known = blind.last_known_session_pct;
        let relief = match resets_at {
            Some(at) => format!(", resets in {}", humanize_until(at - now)),
            None => String::new(),
        };
        let blocked = match cause {
            Some(NoTargetCause::Weekly) => format!("every account is weekly-exhausted{relief}"),
            Some(NoTargetCause::Session) => {
                format!("every account is over its session limit{relief}")
            }
            None => "no viable target".to_owned(),
        };
        let body = format!(
            "CORNERED: active {label} blind for {dur} at last-known session {last_known}% and \
             auto-protection cannot act — {blocked}; add or free an account"
        );
        if color {
            out.push_str(&format!("\x1b[{}m{body}\x1b[0m\n", Severity::Red.sgr()));
        } else {
            out.push_str(&body);
            out.push('\n');
        }
    } else if let Some((label, blind)) = active_blind {
        // Not cornered — the normal per-account blind-active line (issue #479 surface 1, shipped in
        // #496): narrate the REAL state — how long blind, last-known session %, and whether ADR-0017
        // auto-protection is OK or DEGRADED — instead of the content-free `n/a … 🟡`. Printed as DATA
        // (unconditional), like the systemic-refresh line below; only the DEGRADED emphasis is
        // color-gated. DEGRADED is a fault: the gate is armed but acting on a STALE anchor.
        let dur = humanize_until(blind.blind_secs as i64);
        let last_known = blind.last_known_session_pct;
        let verdict = if blind.auto_protection_degraded {
            "DEGRADED (acting on a stale anchor)"
        } else {
            "OK"
        };
        let body = format!(
            "active {label}: blind for {dur} — last-known session {last_known}% — \
             auto-protection {verdict}"
        );
        if color && blind.auto_protection_degraded {
            out.push_str(&format!("\x1b[{}m{body}\x1b[0m\n", Severity::Red.sgr()));
        } else {
            out.push_str(&body);
            out.push('\n');
        }
    }

    // The #452 preemptive-swap NARRATION (issue #479, surface 2): when the daemon swapped a BLIND
    // active account away on its stale pre-blind anchor, `status` narrates it — the source, the
    // last-known % the gate FIRED on, the target, and the `use <from>` undo — so an operator can
    // reverse it if the swapped-away account has since recovered. The SAME information the durable
    // `event=swap … reason=blind_preempt` log line holds, reflected HERE because `render_status`
    // reads only this wire, never the event log — each medium in its own idiom (R-2 STATE-parity, as
    // the `canonical_scrub` footer is). Daemon-side windowed + target-still-active
    // (`recent_blind_preempt_swap`, projected only within a bounded window while its target is still
    // active), so this stays a pure render — the surface REFLECTS, never self-swaps (#169). Omitted
    // from the wire (no line) when there is no recent-and-still-current preemptive swap.
    if let Some(swap) = &response.recent_blind_preempt_swap {
        let from = &swap.from_label;
        let to = &swap.to_label;
        let pct = swap.last_known_session_pct;
        out.push_str(&format!(
            "swapped off {from} (blind @ last-known {pct}%) → {to}; \
             undo with 'use {from}' if it recovered\n"
        ));
    }

    // The forward-looking next-swap candidate (issue #88), computed daemon-side
    // ([`crate::daemon::NextSwap`]); printed plain — the footer carries no color, like
    // the table footer it replaces (per-cell health coloring is #84, orthogonal). A
    // `None` field means the daemon sent no candidate — either a current daemon with no
    // active account to anchor a swap from, or (via `#[serde(default)]`) a pre-#88 daemon
    // that omits the field — and renders a bare `none` either way. SUPPRESSED when cornered
    // (issue #479): the cornered alarm above already folded in this exact `no_viable_target` relief,
    // so re-printing `next swap: none — …` would be redundant (cornered fires only on that arm).
    if cornered.is_none() {
        match &response.next_swap {
            // The daemon's own selection rationale (issue #393) trails the target as a parenthetical,
            // so the CLI operator sees WHY this account — the identical "why this target?" the panel
            // answers, each medium rendering the shared discriminant its own way (R-2 state-parity). A
            // pre-#393 daemon carries no reason (`None`) → the bare label, the honest fallback.
            Some(NextSwap::Target { to, reason }) => {
                let why = match reason {
                    Some(NextSwapReason::SoonestReset { .. }) => " (weekly resets soonest)",
                    Some(NextSwapReason::OnlyCandidate) => " (only viable target)",
                    Some(NextSwapReason::RosterOrder) => " (first eligible; no reset times known)",
                    None => "",
                };
                out.push_str(&format!("next swap: {to}{why}\n"));
            }
            // When the daemon carries the fleet-capacity relief hint, name WHY the fleet is blocked and
            // WHEN capacity returns — so a stranded operator (a DEAD active whose 🔴 row sits above this,
            // AND every spare exhausted) sees the REAL blocker and its escape, not a content-free "no
            // viable target". Terse + action-first in the degraded-cue register. A pre-schema-1.3 daemon
            // carries no cause → the honest bare fallback. `resets_at` humanizes with the same
            // `humanize_until` the per-account "resets in" cells use, so the vocabulary matches.
            Some(NextSwap::NoViableTarget { cause, resets_at }) => {
                let relief = match resets_at {
                    Some(at) => format!("; resets in {}", humanize_until(at - now)),
                    None => String::new(),
                };
                match cause {
                    // Weekly exhaustion is the TERMINAL capacity signal — the wait is long (days), so
                    // the meaningful escape is more accounts.
                    Some(NoTargetCause::Weekly) => out.push_str(&format!(
                    "next swap: none — every account is weekly-exhausted{relief} — add an account\n"
                )),
                    // A session-wide block lifts at the sooner session reset (minutes/hours), so the
                    // reset time itself is the remedy.
                    Some(NoTargetCause::Session) => out.push_str(&format!(
                        "next swap: none — every account is over its session limit{relief}\n"
                    )),
                    None => out.push_str("next swap: none (no viable target)\n"),
                }
            }
            Some(NextSwap::AwaitingData) => out.push_str("next swap: none (awaiting usage data)\n"),
            None => out.push_str("next swap: none\n"),
        }
    }

    // The systemic refresh-failure indicator (issue #378): the daemon reports the refresh
    // MECHANISM is down — `consecutive` sweeps in a row failed with error across EVERY eligible
    // account (a stale `claude` path #375, a wedged spawn), not one account's creds. Surfaced as
    // DATA (not advisory chrome like the #138 line below): printed UNCONDITIONALLY so it survives a
    // pipe / redirect / `status | grep` — an operator's health check must be able to see it —
    // with a red emphasis added only when the color gate is open. Distinct from the per-account
    // `AUTH` column: it is the whole mechanism failing, visible before any account dies. Carries
    // only the COUNT (#15). Mutually exclusive with the #138 advisory (that needs `[refresh]` OFF;
    // this needs sweeps running, i.e. ON), so their ordering here never matters.
    if let Some(consecutive) = response.systemic_refresh_failure {
        // `consecutive` is a valid `1..=100` count, so keep the noun agreement right at the `n=1`
        // floor (a threshold of 1 fires on the first all-error sweep → "1 consecutive sweep").
        let sweeps = if consecutive == 1 { "sweep" } else { "sweeps" };
        let body = format!(
            "refresh mechanism: DOWN — {consecutive} consecutive {sweeps} failed for every eligible \
             account; the mechanism is failing, not one account (check the daemon log 'reason=' \
             and the [refresh] claude binary)"
        );
        if color {
            // Same SGR overlay `render_cells` uses (`\x1b[{code}m…\x1b[0m`), red for the fault.
            out.push_str(&format!("\x1b[{}m{body}\x1b[0m\n", Severity::Red.sgr()));
        } else {
            out.push_str(&body);
            out.push('\n');
        }
    }

    // The daemon-level KEYCHAIN-LOCKED rollup (issue #498): the macOS login keychain is LOCKED, so the
    // daemon cannot READ the shared `Claude Code-credentials` item at ALL (access denied). The
    // daemon-LEVEL sibling of the `canonical_scrub` line below, but for an UNREADABLE item rather than a
    // readable-but-emptied one — so the remedy DIFFERS: UNLOCK THE KEYCHAIN, never `claude /login` (a
    // re-login cannot help while the keychain that stores the credential is locked). Surfaced as DATA
    // (unconditional, like the scrub + systemic lines — so it survives a pipe / redirect / `status |
    // grep`, an operator's health check must see it), naming the state AND the unlock remedy.
    // Content-parity with the menubar (`StatusPanelFormat.keychainLockedBanner`): same state + same
    // unlock remedy, each medium phrasing it its own way (R-2 state-parity, as ADR-0016 did for
    // `ActiveDeadNoTarget`). Printed PLAIN — the action-first footer register of the `shared login:
    // scrubbed …` sibling (ADR-0016), NOT the systemic line's red SGR. Rendered ABOVE `canonical_scrub`
    // (worst-first: an unreadable item is at least as severe as a readable-but-scrubbed one), though the
    // two are daemon-mutually-exclusive in practice (a locked keychain can't be read to know
    // scrubbed-ness). A bare BINARY state discriminant — never a token or email (#15). `false` (a
    // healthy / pre-#498 daemon that omits the field) prints nothing.
    if response.keychain_locked {
        out.push_str(
            "shared login: unreadable — the login keychain is locked; unlock it to restore access\n",
        );
    }

    // The daemon-level CANONICAL-SCRUB rollup (issue #469, umbrella #463): the shared
    // `Claude Code-credentials` canonical item has been SCRUBBED (its token cleared), so every
    // `claude` session is logged out — the fleet-wide lockout NO per-account `AUTH` column reflects
    // (each account row can read perfectly healthy while the shared item sits emptied). Surfaced as
    // DATA (unconditional, like the systemic line above — so it survives a pipe / redirect /
    // `status | grep`, an operator's health check must be able to see it), naming the state and, for
    // the un-recoverable residual, the `claude /login` remedy. Content-parity with the menubar
    // (`StatusPanelFormat.canonicalScrubBanner`): same state + same `claude /login` remedy, each
    // medium phrasing it its own way (R-2 state-parity, as ADR-0016 did for `ActiveDeadNoTarget`).
    // Printed PLAIN — no color overlay, in the action-first footer register of the `next swap: none
    // — …` footer above (ADR-0016), NOT the systemic line's red SGR. A fleet-wide STATE discriminant
    // only — never per-account, never a token or email (#15). `None` (a healthy / pre-#516 daemon
    // that omits the field) prints nothing.
    match response.canonical_scrub {
        // Exhausted — recovery backed off (the bounded adopt churn hit its cap, or no viable adopt
        // target exists), so the canonical stays empty until a re-login. Name the state AND the
        // actionable remedy; `claude /login` is the byte-shared remedy the menubar names too.
        Some(CanonicalScrub::Exhausted) => out.push_str(
            "shared login: scrubbed — every session is logged out and auto-recovery is exhausted; \
             run claude /login to restore it\n",
        ),
        // Recovering — the daemon is autonomously adopting a live account back into the canonical, so
        // the fleet may self-heal with NO operator action. The calm, no-remedy cue (lower severity).
        Some(CanonicalScrub::Recovering) => out.push_str(
            "shared login: scrubbed — recovering automatically (adopting a live account); \
             no action needed\n",
        ),
        None => {}
    }

    // The isolated-refresh discoverability advisory (issue #138): when the periodic refresh
    // tick is OFF (`[refresh].enabled = false`) AND ≥1 NON-ACTIVE account is unverified / stale
    // / at-risk / dead, that account's stored credential is going unmaintained — the operator
    // would otherwise only find out at `next swap: none (no viable target)`, after the fallback
    // set is already dead. One line names the remedy. ADVISORY CHROME, not data (AC-3): gated on
    // the SAME color gate as the #73 ANSI overlay, so it rides an interactive stdout TTY only —
    // never into `--json` (this fn is not reached there), a pipe, a redirect, or under
    // NO_COLOR / CLICOLOR=0 / TERM=dumb / --no-color. `Some(false)` is the ONLY arming value;
    // `Some(true)` (enabled) and `None` (a pre-#138 daemon that omits the field) both suppress.
    if color && response.refresh_enabled == Some(false) && has_stale_nonactive(response) {
        out.push_str(REFRESH_DISABLED_ADVISORY);
    }
    out
}

/// The age (in seconds) past which a snapshot's data is UNAMBIGUOUSLY stale — the maximum possible
/// poll cadence (`POLL_SECS_HI` = 3600 in `src/daemon.rs`). A snapshot older than this has outlived
/// even the slowest legitimate poll interval, so it cannot be dismissed as "just a long cadence." A
/// deliberately conservative bound: the CLI does not know the configured cadence, so a lower bar would
/// cry wolf on a healthy-but-slow daemon. Mirrors the panel's `staleAgeSecs` (`StatusPanelFormat.swift`).
const STALE_AGE_SECS: i64 = 3600;

/// The snapshot-freshness header line (council / issue #164 `generated_at`): `updated Ns ago` above
/// the table, the CLI's parity render of the panel banner's age — surfaced so a `status` reader never
/// assumes the numbers are current when the daemon's poll loop has wedged (`generated_at` stops
/// advancing while the control socket keeps answering the held snapshot). Empty when there is no
/// generation instant (`generated_at <= 0`, the wire's all-defaults sentinel). A snapshot older than
/// [`STALE_AGE_SECS`] gets a trailing ` (stale)` marker — the age NUMBER already conveys staleness, so
/// this is plain text, not the color-gated #73 severity overlay. Mirrors the panel's `snapshotAgeText`
/// + `snapshotIsStale`; the age humanizes with the SAME [`humanize_until`] the reset-in uses.
fn render_snapshot_age(generated_at: i64, now: i64) -> String {
    if generated_at <= 0 {
        return String::new();
    }
    let age = (now - generated_at).max(0);
    let humanized = if age == 0 {
        "just now".to_owned()
    } else {
        format!("{} ago", humanize_until(age))
    };
    let stale = if age > STALE_AGE_SECS { " (stale)" } else { "" };
    format!("updated {humanized}{stale}\n")
}

/// The issue-#138 signal: ≥1 NON-ACTIVE account carries a non-healthy / unverified credential
/// rollup, so its stored credential may be lapsing while the refresh tick is off. Keys off the
/// daemon's 5-state rollup (`Some(h)`, a #119+ daemon): any of Unknown ⚪ / Stale 🟡 / AtRisk 🟠 /
/// Degraded 🟠 / Dead 🔴 counts; Healthy 🟢 and a pre-#119 `None` (no rollup to judge) do not. The ACTIVE
/// account is excluded — the live daemon maintains it via the poll path (#162), so it is never
/// the stale-fallback concern this advisory is about.
fn has_stale_nonactive(response: &StatusResponse) -> bool {
    response.accounts.iter().any(|account| {
        !account.active
            && matches!(account.health, Some(health) if health != CredentialHealth::Healthy)
    })
}

/// The issue-#138 advisory line: the periodic refresh tick is off while a non-active account's
/// credential is going unmaintained. Names BOTH remedies — the one-shot `poke` and enabling
/// `[refresh]`. Lowercase and terse, matching the `next swap:` footer register; carries no
/// account labels (AC-4, no PII). Leading blank line separates it from the footer (mirroring the
/// verbose block's leading `\n`); trailing newline closes it.
const REFRESH_DISABLED_ADVISORY: &str = "\nadvisory: [refresh] is off and non-active accounts \
    are going stale — run 'sessiometer poke' or enable [refresh] to maintain them\n";

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
    /// The AUTH cell (issue #119, #427): the daemon's credential rollup as ONE glyph
    /// (🟢 healthy · ⚪ unknown · 🟡 stale · 🟠 at-risk · 🟠 degraded · 🔴 dead), with the
    /// `claude /login` cue appended for a PROVEN-dead account and the needs-refresh cue for a
    /// `degraded` (quarantined-but-refreshable) one — each softened to `recovering` for a healing
    /// account (#109) — and a trailing `disabled` for a parked account (#36, orthogonal).
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
            // A blind active account with a retained anchor shows its last-known session % with a
            // `~` (stale / approximate) marker, NOT a bare `n/a` — the row stops reporting "no data"
            // when the daemon holds a pre-blind anchor (#479); the full state (blind duration +
            // auto-protection OK/DEGRADED) trails as the footer line. Every other account keeps the
            // fresh-reading-or-`n/a` cell.
            session: match account.blind_active {
                Some(blind) => format!("~{}%", blind.last_known_session_pct),
                None => pct(account.session_pct),
            },
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
            // A blind active account colors its stale `~%` by the last-known utilization band — the
            // anchor's near-limit reading IS the risk the operator should see (#479); otherwise the
            // fresh reading's band, or uncolored when there is no reading.
            session_severity: match account.blind_active {
                Some(blind) => Some(util_severity(blind.last_known_session_pct)),
                None => account.session_pct.map(util_severity),
            },
            session_reset_severity: proximity_severity(account.session_resets_at, now),
            weekly_severity: weekly_cell_severity(account),
            weekly_reset_severity: proximity_severity(account.weekly_resets_at, now),
        }
    }
}

/// The needs-REFRESH cue for a `Degraded` (bare-quarantine) credential (issue #427): the honest
/// counterpart to `Dead`'s `claude /login`. Leads with the immediate remedy (`poke`); enabling
/// `[refresh]` — the durable fix — is carried holistically by [`REFRESH_DISABLED_ADVISORY`], and
/// a genuine refresh-token death still escalates to 🔴 `claude /login`. Deliberately NOT
/// "re-login" — that is precisely the over-reaction the honest verdict prevents.
const DEGRADED_CUE: &str = "degraded — run 'sessiometer poke'";

/// The `status` AUTH cell for one account (issue #119, extended by #427): the daemon's credential
/// rollup as ONE glyph plus the minimal cue an operator needs to act, with the `disabled`
/// rotation tag (#36) — orthogonal to credential health — appended.
///
/// `health == Some(verdict)` (a current daemon) renders the glyph; a PROVEN-`Dead` account carries
/// the `claude /login` cue and a `Degraded` (quarantined-but-refreshable) account the needs-refresh
/// [`DEGRADED_CUE`] (AC-1: a refreshable account NEVER reads "claude /login"), each softened to
/// `recovering` for a healing account so the operator neither acts needlessly nor swaps away from a
/// recovering — often healthier — account (#109). `health == None` (a pre-#119 daemon that sent no
/// rollup) falls back to the legacy comma-joined tags, so an old daemon's `status` is unchanged
/// rather than mis-reading a defaulted glyph over a dead account.
fn health_cell(account: &AccountStatusLine) -> String {
    let Some(health) = account.health else {
        return legacy_health_tags(account);
    };
    let mut cell = health_glyph(health).to_owned();
    // The actionable cue an operator needs, keyed off the honest verdict (issue #427): a PROVEN
    // `Dead` credential needs a re-login (`claude /login`); a `Degraded` one (a bare quarantine)
    // needs only a REFRESH — distinct advice, so the false "claude /login" never fires for a
    // still-refreshable account. Either state softens to `recovering` for a healing account
    // (#109) so the operator holds rather than acting or swapping away.
    match health {
        CredentialHealth::Dead => {
            cell.push(' ');
            cell.push_str(if account.recovering {
                "recovering"
            } else {
                "claude /login"
            });
        }
        CredentialHealth::Degraded => {
            cell.push(' ');
            cell.push_str(if account.recovering {
                "recovering"
            } else {
                DEGRADED_CUE
            });
        }
        _ => {}
    }
    // `disabled` (rotation #36) is independent of credential health — a parked account can
    // be perfectly healthy — so it trails the glyph rather than replacing it.
    if !account.enabled {
        cell.push_str(" disabled");
    }
    cell
}

/// The emoji glyph for a 5-state rollup verdict (issue #119). Self-coloring (the glyph is
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
        // #427: a quarantined-but-refreshable credential is a NON-TERMINAL warning — it shares
        // the warm 🟠 band with `AtRisk` (both "act soon, recoverable"), reserving 🔴 for a
        // PROVEN refresh-token death that truly needs `claude /login`. The two orange states are
        // told apart by the actionable TEXT cue in `health_cell` (needs-refresh vs no cue), and
        // the operator's load-bearing distinction — 🟠 poke-to-refresh vs 🔴 re-login — is the
        // one carried by color.
        CredentialHealth::Degraded => "🟠",
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

/// Left-justify `s` in a field `width` DISPLAY columns wide, right-padding with spaces
/// measured on [`display_width`] — the wide-glyph-correct analogue of Rust's `{:<width$}`
/// fill, which pads by `char` count and so mis-aligns any cell carrying a CJK, emoji, or
/// combining glyph (issue #249). Returns `s` unchanged when it already fills or overflows
/// `width` (never truncates — matching the `{:<width$}` fill it replaces). The shared
/// primitive for the block renderers that pad a label column inline rather than through
/// `render_cells` (this `status` table) or the `stats` view's `render_line`: the `list`
/// and `--verbose` label columns here, and the `stats` bars / heatmap / percentiles /
/// numeric-table charts. `pub(crate)` so those `stats` renderers share this one helper.
pub(crate) fn pad_end(s: &str, width: usize) -> String {
    format!("{s}{}", " ".repeat(width.saturating_sub(display_width(s))))
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
    // Pad each label to the widest on DISPLAY width (issue #249, as the `status` table and
    // the `list` view now do) so the expiry column lines up under a two-space gap even when
    // a label carries a wide CJK / emoji glyph that `.chars().count()` and the `{:<width$}`
    // fill would mis-measure.
    let width = response
        .accounts
        .iter()
        .map(|account| display_width(&account.label))
        .max()
        .unwrap_or(0);
    let mut out =
        String::from("\naccess token — auto-refreshed by Claude Code, not a re-login deadline:\n");
    for account in &response.accounts {
        out.push_str(&format!(
            "  {}  {}\n",
            pad_end(&account.label, width),
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
    // Raw `libc` FFI, kept un-wrapped by ADR-0004: `TIOCGWINSZ` has no std
    // equivalent (unlike `isatty` -> `IsTerminal`, #178), so wrapping it would mean
    // a production `rustix` / `terminal_size` dependency the crate's minimalism
    // rejects for a single, sound POD probe.
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
    // Pad the label column to the widest label on DISPLAY width (issue #249, matching the
    // `status` table fixed in #176) so the uuid column aligns even when a label carries a
    // wide CJK / emoji glyph — `.chars().count()` and the `{:<width$}` fill would stagger
    // it. The offline `list` never renders an empty roster (that maps to the friendly
    // `RosterEmpty`), but `unwrap_or(0)` keeps this total for the METER's direct callers.
    let width = roster
        .iter()
        .map(|account| display_width(&account.label))
        .max()
        .unwrap_or(0);
    let mut out = String::new();
    for (account, auth) in roster.iter().zip(auth) {
        // A parked account is marked inline (issue #36); an enabled one adds
        // nothing.
        let state = if account.enabled { "" } else { " · disabled" };
        let tags = auth_tags(auth, now_secs);
        out.push_str(&format!(
            "{}  {}{}{}\n",
            pad_end(&account.label, width),
            account.account_uuid,
            state,
            tags,
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
    use crate::daemon::{
        AccountStatusLine, BlindActive, BlindPreemptSwap, NextSwap, NoTargetCause,
    };
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
            crate::redaction::meter::unauthored_emails(&out, &[]).is_empty(),
            "list output must not contain a non-authored email (#15/#444): {out:?}"
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
            systemic_refresh_failure: None,
            canonical_scrub: None,
            keychain_locked: false,
            recent_blind_preempt_swap: None,
            refresh_enabled: None,
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
    fn render_status_surfaces_the_systemic_refresh_failure_when_the_mechanism_is_down() {
        // Issue #378: when the daemon reports the refresh MECHANISM is down, `status` shows a
        // dedicated DOWN line carrying the count — visible without waiting for an account to die,
        // and distinct from the per-account `needs re-login`. #15-clean: a count only, no token/email.
        let response = |systemic| StatusResponse {
            systemic_refresh_failure: systemic,
            canonical_scrub: None,
            keychain_locked: false,
            recent_blind_preempt_swap: None,
            refresh_enabled: Some(true),
            accounts: vec![status_line("work", true, Some(50), Some(25))],
            next_swap: None,
        };

        let out = render_status(&response(Some(3)), NOW, None, false);
        let down = out
            .lines()
            .find(|l| l.contains("refresh mechanism: DOWN"))
            .expect("the mechanism-down line is present");
        assert!(
            down.contains("3 consecutive sweeps failed"),
            "carries the count: {down}"
        );
        assert!(
            crate::redaction::meter::unauthored_emails(&out, &[]).is_empty()
                && !out.to_lowercase().contains("token"),
            "no secret reaches the surface (#15/#444): {out:?}"
        );

        // A threshold-of-1 config fires at the first all-error sweep — the noun stays singular.
        assert!(
            render_status(&response(Some(1)), NOW, None, false)
                .contains("1 consecutive sweep failed"),
            "singular at n=1"
        );

        // Healthy (None) prints no mechanism-down line at all.
        assert!(
            !render_status(&response(None), NOW, None, false).contains("refresh mechanism"),
            "no DOWN line when the mechanism is healthy"
        );
    }

    #[test]
    fn render_status_surfaces_the_canonical_scrub_rollup_with_the_relogin_remedy() {
        // Issue #469: when the daemon reports the shared canonical is SCRUBBED, `status` shows a
        // dedicated footer line — the fleet-wide lockout no per-account `AUTH` column reflects —
        // naming the state and, for the un-recoverable residual, the `claude /login` remedy. The
        // account rows can read perfectly healthy (60/25) while the shared item sits emptied.
        // #15-clean: a bare state discriminant, never a token or email.
        let response = |scrub| StatusResponse {
            systemic_refresh_failure: None,
            canonical_scrub: scrub,
            keychain_locked: false,
            recent_blind_preempt_swap: None,
            refresh_enabled: Some(true),
            accounts: vec![status_line("work", true, Some(60), Some(25))],
            next_swap: None,
        };

        // Exhausted → names the state AND the actionable `claude /login` remedy (byte-shared with
        // the menubar's `canonicalScrubBanner` — content-parity, R-2 state-parity).
        let exhausted = render_status(&response(Some(CanonicalScrub::Exhausted)), NOW, None, false);
        let line = exhausted
            .lines()
            .find(|l| l.contains("shared login: scrubbed"))
            .expect("the scrubbed line is present");
        assert!(
            line.contains("auto-recovery is exhausted") && line.contains("claude /login"),
            "exhausted names the state + the re-login remedy: {line}"
        );

        // Recovering → the calm, no-action cue; NEVER the `claude /login` remedy (the daemon may
        // self-heal by adopting a live account — surfacing a re-login would cry wolf).
        let recovering = render_status(
            &response(Some(CanonicalScrub::Recovering)),
            NOW,
            None,
            false,
        );
        let line = recovering
            .lines()
            .find(|l| l.contains("shared login: scrubbed"))
            .expect("the scrubbed line is present");
        assert!(
            line.contains("recovering automatically") && line.contains("no action needed"),
            "recovering is a calm no-action cue: {line}"
        );
        assert!(
            !recovering.contains("claude /login"),
            "recovering carries no re-login remedy: {recovering:?}"
        );

        // Healthy (None) prints no scrubbed line at all.
        assert!(
            !render_status(&response(None), NOW, None, false).contains("shared login"),
            "no scrubbed line when the canonical is healthy"
        );

        // The scrubbed line is DATA — it survives with the color gate CLOSED (--no-color) exactly as
        // it does open, so a piped `status | grep` health check sees it (like the systemic line).
        assert!(
            render_status(&response(Some(CanonicalScrub::Exhausted)), NOW, None, true)
                .contains("shared login: scrubbed"),
            "the scrubbed line is unconditional data, present under --color too"
        );

        // #15/#444: no secret reaches EITHER rendered state (a state discriminant only).
        for out in [&exhausted, &recovering] {
            assert!(
                crate::redaction::meter::unauthored_emails(out, &[]).is_empty()
                    && !out.to_lowercase().contains("token"),
                "no secret reaches the canonical-scrub surface (#15/#444): {out:?}"
            );
        }
    }

    #[test]
    fn render_status_surfaces_the_keychain_locked_rollup_with_the_unlock_remedy() {
        // Issue #498: when the daemon reports the login keychain is LOCKED (so the shared canonical is
        // UNREADABLE — access denied, distinct from #469's readable-but-scrubbed item), `status` shows a
        // dedicated footer line naming the state AND the UNLOCK remedy (NOT `claude /login` — a re-login
        // cannot help while the keychain is locked). The account rows can read perfectly healthy (60/25)
        // while the shared item sits unreadable. #15-clean: a bare state discriminant, never a token or
        // email.
        let response = |locked| StatusResponse {
            systemic_refresh_failure: None,
            canonical_scrub: None,
            keychain_locked: locked,
            recent_blind_preempt_swap: None,
            refresh_enabled: Some(true),
            accounts: vec![status_line("work", true, Some(60), Some(25))],
            next_swap: None,
        };

        // Locked → names the state (keychain locked) AND the unlock remedy — content-parity with the
        // menubar's `keychainLockedBanner` (same state + same UNLOCK remedy, R-2 state-parity).
        let locked = render_status(&response(true), NOW, None, false);
        let line = locked
            .lines()
            .find(|l| l.contains("shared login: unreadable"))
            .expect("the keychain-locked line is present");
        assert!(
            line.contains("keychain is locked") && line.contains("unlock"),
            "locked names the state + the unlock remedy: {line}"
        );
        // NEVER the `claude /login` remedy — that is #469's (a readable-but-scrubbed item); a re-login
        // cannot help while the keychain that STORES the credential is locked (the #498-vs-#469 point).
        assert!(
            !locked.contains("claude /login"),
            "keychain-locked carries the UNLOCK remedy, never the re-login one: {locked:?}"
        );

        // Unlocked (false) prints no keychain line at all.
        assert!(
            !render_status(&response(false), NOW, None, false).contains("shared login: unreadable"),
            "no keychain-locked line when the keychain is unlocked"
        );

        // The keychain-locked line is DATA — it survives with the color gate CLOSED (--no-color) exactly
        // as it does open, so a piped `status | grep` health check sees it (like the scrub line).
        assert!(
            render_status(&response(true), NOW, None, true).contains("shared login: unreadable"),
            "the keychain-locked line is unconditional data, present under --color too"
        );

        // #15/#444: no secret reaches the rendered state (a bare state discriminant only).
        assert!(
            crate::redaction::meter::unauthored_emails(&locked, &[]).is_empty()
                && !locked.to_lowercase().contains("token"),
            "no secret reaches the keychain-locked surface (#15/#444): {locked:?}"
        );
    }

    #[test]
    fn render_status_narrates_a_blind_active_account_instead_of_bare_n_a() {
        // Issue #479: a blind active account with a retained anchor renders a SEMANTIC line (blind
        // duration + last-known session % + auto-protection state) plus a stale `~%` cell, not the
        // content-free `n/a … 🟡` the bare failed-poll row shows.
        let degraded = AccountStatusLine {
            health: Some(CredentialHealth::Stale),
            blind_active: Some(BlindActive {
                blind_secs: 480,
                last_known_session_pct: 87,
                auto_protection_degraded: true,
            }),
            ..status_line("work", true, None, None)
        };
        let response = StatusResponse {
            systemic_refresh_failure: None,
            canonical_scrub: None,
            keychain_locked: false,
            recent_blind_preempt_swap: None,
            refresh_enabled: None,
            accounts: vec![degraded],
            next_swap: None,
        };
        let out = render_status(&response, NOW, None, false);
        // The semantic footer line states the REAL state — not "no data".
        assert!(
            out.contains("active work: blind for")
                && out.contains("last-known session 87%")
                && out.contains("auto-protection DEGRADED"),
            "the blind active account is narrated with its retained anchor + degraded gate: {out}",
        );
        // The row's SESSION% cell shows the stale last-known `~87%`, not a bare `n/a`.
        assert!(
            out.contains("~87%"),
            "the session cell shows the stale anchor pct, not n/a: {out}",
        );
    }

    #[test]
    fn render_status_blind_active_ok_below_the_gate_and_absent_when_not_blind() {
        // Issue #479: OK (blind but not past the gate) says auto-protection OK, no DEGRADED alarm;
        // a non-blind active account carries no projection → no line and the usual `n/a` cell.
        let ok = AccountStatusLine {
            blind_active: Some(BlindActive {
                blind_secs: 30,
                last_known_session_pct: 42,
                auto_protection_degraded: false,
            }),
            ..status_line("work", true, None, None)
        };
        let out = render_status(
            &StatusResponse {
                systemic_refresh_failure: None,
                canonical_scrub: None,
                keychain_locked: false,
                recent_blind_preempt_swap: None,
                refresh_enabled: None,
                accounts: vec![ok],
                next_swap: None,
            },
            NOW,
            None,
            false,
        );
        assert!(
            out.contains("auto-protection OK") && !out.contains("DEGRADED"),
            "a blind-but-not-yet-degraded active reads OK: {out}",
        );

        // A normal (non-blind) active account: no `blind_active` → no narration line, bare `n/a` cell.
        let normal = render_status(
            &StatusResponse {
                systemic_refresh_failure: None,
                canonical_scrub: None,
                keychain_locked: false,
                recent_blind_preempt_swap: None,
                refresh_enabled: None,
                accounts: vec![status_line("work", true, None, None)],
                next_swap: None,
            },
            NOW,
            None,
            false,
        );
        assert!(
            !normal.contains("blind for") && normal.contains("n/a"),
            "a non-blind account is unchanged — no line, bare n/a: {normal}",
        );
    }

    #[test]
    fn render_status_blind_active_colors_only_the_degraded_footer_under_color() {
        // Issue #479: the blind footer's color gate is `color && auto_protection_degraded`, so under
        // `--color` the DEGRADED line is red-wrapped (the SAME SGR overlay the systemic-refresh line
        // uses) while the OK line stays PLAIN — an OK line is never emphasized even with color on.
        let blind = |degraded: bool| StatusResponse {
            systemic_refresh_failure: None,
            canonical_scrub: None,
            keychain_locked: false,
            recent_blind_preempt_swap: None,
            refresh_enabled: None,
            accounts: vec![AccountStatusLine {
                blind_active: Some(BlindActive {
                    blind_secs: 480,
                    last_known_session_pct: 87,
                    auto_protection_degraded: degraded,
                }),
                ..status_line("work", true, None, None)
            }],
            next_swap: None,
        };
        // DEGRADED + color → the footer body is wrapped in the red SGR (the reset directly follows it).
        let degraded = render_status(&blind(true), NOW, None, true);
        assert!(
            degraded.contains("auto-protection DEGRADED (acting on a stale anchor)\x1b[0m"),
            "the degraded blind footer is red-wrapped under --color: {degraded:?}",
        );
        // OK + color → the footer stays PLAIN (newline-terminated, no SGR) — the `&& degraded` guard.
        let ok = render_status(&blind(false), NOW, None, true);
        assert!(
            ok.contains("auto-protection OK\n") && !ok.contains("auto-protection OK\x1b[0m"),
            "the OK blind footer stays plain even under --color: {ok:?}",
        );
    }

    /// A cornered response: the active account is blind + DEGRADED, and `next_swap` is
    /// `NoViableTarget` with the given cause/reset — the composition that fires the surface-3 alarm.
    fn cornered_response(cause: Option<NoTargetCause>, resets_at: Option<i64>) -> StatusResponse {
        StatusResponse {
            systemic_refresh_failure: None,
            canonical_scrub: None,
            keychain_locked: false,
            recent_blind_preempt_swap: None,
            refresh_enabled: None,
            accounts: vec![AccountStatusLine {
                blind_active: Some(BlindActive {
                    blind_secs: 480,
                    last_known_session_pct: 87,
                    auto_protection_degraded: true,
                }),
                ..status_line("work", true, None, None)
            }],
            next_swap: Some(NextSwap::NoViableTarget { cause, resets_at }),
        }
    }

    #[test]
    fn render_status_cornered_is_the_loudest_state_and_names_the_remedy() {
        // Issue #479 (surface 3): active blind + DEGRADED + no viable target = the one bounded-
        // blindness state the daemon cannot resolve itself. It renders ONE loud, distinct alarm that
        // names the source, the stale last-known %, WHY the fleet is blocked (folded in from the
        // no-target relief), and the operator remedy — and SUPPRESSES both the separate blind-DEGRADED
        // line and the `next swap: none — …` footer, which split read as two unrelated observations.
        let out = render_status(
            &cornered_response(
                Some(NoTargetCause::Weekly),
                Some(NOW + 2 * 86_400 + 4 * 3_600),
            ),
            NOW,
            None,
            false,
        );
        assert!(
            out.contains("CORNERED: active work blind for")
                && out.contains("last-known session 87%")
                && out.contains("auto-protection cannot act")
                && out.contains("every account is weekly-exhausted, resets in 2d4h")
                && out.contains("add or free an account"),
            "the cornered alarm names source + stale pct + blocker + remedy: {out}",
        );
        // The two constituent lines are FOLDED INTO the alarm, not printed separately.
        assert!(
            !out.contains("auto-protection DEGRADED"),
            "the separate blind-DEGRADED line is suppressed when cornered: {out}",
        );
        assert!(
            !out.contains("next swap:"),
            "the next-swap footer is suppressed when cornered (folded into the alarm): {out}",
        );
    }

    #[test]
    fn render_status_cornered_folds_each_no_target_cause() {
        // The remedy relief is folded from `next_swap`'s cause, so the operator still sees WHY. A
        // SESSION-wide block names the sooner reset; an absent cause (pre-#405 daemon) falls back to
        // the bare "no viable target" — each still carrying the "add or free an account" remedy.
        let session = render_status(
            &cornered_response(Some(NoTargetCause::Session), Some(NOW + 47 * 60)),
            NOW,
            None,
            false,
        );
        assert!(
            session.contains("every account is over its session limit, resets in 47m")
                && session.contains("add or free an account"),
            "session-cause cornered folds the session relief: {session}",
        );
        let bare = render_status(&cornered_response(None, None), NOW, None, false);
        assert!(
            bare.contains("CORNERED: active work")
                && bare.contains("no viable target")
                && bare.contains("add or free an account"),
            "a causeless cornered still alarms with the remedy: {bare}",
        );
    }

    #[test]
    fn render_status_is_not_cornered_without_both_degraded_and_no_target() {
        // Cornered requires BOTH auto-protection DEGRADED AND no viable target. Either alone renders
        // the ordinary (non-alarming) surfaces — the two guards that keep the loudest state rare.

        // (a) Blind + DEGRADED but a VIABLE target exists → the daemon WILL swap; the normal
        //     blind-DEGRADED line + the ordinary `next swap: <target>` footer, NOT the cornered alarm.
        let has_target = StatusResponse {
            next_swap: Some(NextSwap::Target {
                to: "spare".to_owned(),
                reason: Some(NextSwapReason::OnlyCandidate),
            }),
            ..cornered_response(None, None)
        };
        let out = render_status(&has_target, NOW, None, false);
        assert!(
            !out.contains("CORNERED")
                && out.contains("auto-protection DEGRADED")
                && out.contains("next swap: spare (only viable target)"),
            "degraded + a viable target is NOT cornered — the daemon will swap: {out}",
        );

        // (b) Blind but auto-protection OK (not yet past the gate) + no viable target → the daemon is
        //     still self-resolving by waiting out the blip; the normal blind-OK line + the ordinary
        //     no-target footer, NOT the loudest alarm (the anti-cry-wolf guard).
        let ok_no_target = StatusResponse {
            accounts: vec![AccountStatusLine {
                blind_active: Some(BlindActive {
                    blind_secs: 30,
                    last_known_session_pct: 62,
                    auto_protection_degraded: false,
                }),
                ..status_line("work", true, None, None)
            }],
            ..cornered_response(Some(NoTargetCause::Weekly), None)
        };
        let out = render_status(&ok_no_target, NOW, None, false);
        assert!(
            !out.contains("CORNERED")
                && out.contains("auto-protection OK")
                && out.contains("next swap: none — every account is weekly-exhausted"),
            "blind-OK (pre-gate) + no target is NOT cornered — cry-wolf guard: {out}",
        );
    }

    #[test]
    fn render_status_cornered_is_red_under_color() {
        // The cornered alarm is unconditionally red-emphasized under the color gate (the loudest
        // state) — the SAME SGR the DEGRADED / systemic lines use — while the plain text conveys the
        // crisis under --no-color / a pipe.
        let colored = render_status(
            &cornered_response(Some(NoTargetCause::Weekly), None),
            NOW,
            None,
            true,
        );
        assert!(
            colored.contains("add or free an account\x1b[0m"),
            "the cornered alarm is red-wrapped under --color: {colored:?}",
        );
        let plain = render_status(
            &cornered_response(Some(NoTargetCause::Weekly), None),
            NOW,
            None,
            false,
        );
        assert!(
            plain.contains("add or free an account\n") && !plain.contains("\x1b["),
            "the cornered alarm is plain under --no-color: {plain:?}",
        );
    }

    #[test]
    fn render_status_narrates_a_recent_preemptive_swap_with_the_undo() {
        // Issue #479 (surface 2): a daemon-pushed `recent_blind_preempt_swap` renders a narration line
        // naming the source, the last-known % the gate fired on, the target, and the `use <from>` undo
        // — reflected in `status` (the same information the `event=swap … reason=blind_preempt` log
        // line holds). Absent from the wire → no line.
        let narrated = StatusResponse {
            systemic_refresh_failure: None,
            canonical_scrub: None,
            keychain_locked: false,
            recent_blind_preempt_swap: Some(BlindPreemptSwap {
                from_label: "spare".to_owned(),
                to_label: "work".to_owned(),
                last_known_session_pct: 68,
            }),
            refresh_enabled: None,
            accounts: vec![status_line("work", true, Some(20), Some(15))],
            next_swap: None,
        };
        let out = render_status(&narrated, NOW, None, false);
        assert!(
            out.contains(
                "swapped off spare (blind @ last-known 68%) → work; \
                 undo with 'use spare' if it recovered"
            ),
            "the preemptive swap is narrated with source + stale pct + target + undo: {out}",
        );

        // No recent preemptive swap on the wire → no narration line.
        let quiet = StatusResponse {
            recent_blind_preempt_swap: None,
            ..narrated
        };
        let out = render_status(&quiet, NOW, None, false);
        assert!(
            !out.contains("swapped off") && !out.contains("undo with"),
            "no line when there is no recent preemptive swap: {out}",
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
            systemic_refresh_failure: None,
            canonical_scrub: None,
            keychain_locked: false,
            recent_blind_preempt_swap: None,
            refresh_enabled: None,
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
            crate::redaction::meter::unauthored_emails(&out, &[]).is_empty(),
            "no non-authored email on the printed surface (#15/#444): {out:?}"
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
            systemic_refresh_failure: None,
            canonical_scrub: None,
            keychain_locked: false,
            recent_blind_preempt_swap: None,
            refresh_enabled: None,
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
            crate::redaction::meter::unauthored_emails(&out, &[]).is_empty(),
            "no non-authored email on the printed surface (#15/#444): {out:?}"
        );
        assert!(!out.to_lowercase().contains("token"));
    }

    // --- status: 5-state credential-health rollup (issue #119) --------------

    #[test]
    fn health_cell_projects_each_rollup_state_to_a_glyph_with_an_actionable_cue() {
        use CredentialHealth::{AtRisk, Dead, Degraded, Healthy, Stale, Unknown};
        // `health == Some(verdict)`: the daemon's rollup renders as ONE self-coloring glyph,
        // plus the minimal cue an operator needs to act.
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
        // and carries NO cue (only `Dead` / `Degraded` prompt an action).
        assert_eq!(cell(Some(Unknown), false, false, true), "⚪");
        assert_eq!(cell(Some(Stale), false, false, true), "🟡");
        assert_eq!(cell(Some(AtRisk), false, false, true), "🟠");
        // #427: a DEGRADED (quarantined-but-refreshable) credential is 🟠 with a needs-REFRESH
        // cue — NEVER the 🔴 "claude /login" of a proven death. This is the honesty fix: the cue
        // points at `poke`, distinguishing needs-refresh from needs-re-login.
        assert_eq!(
            cell(Some(Degraded), true, false, true),
            "🟠 degraded — run 'sessiometer poke'"
        );
        // A HEALING degraded account reads `recovering` — the operator holds while it heals (#109).
        assert_eq!(cell(Some(Degraded), true, true, true), "🟠 recovering");
        // A DEAD credential carries the exact recovery command (AC-1) — visibly distinct from
        // a usage-exhausted but credential-healthy account, which carries no such cue. Reserved
        // for PROVEN refresh-token death (#427).
        assert_eq!(cell(Some(Dead), true, false, true), "🔴 claude /login");
        // A HEALING quarantined account reads `recovering`, NOT the command — so the operator
        // holds rather than re-authing or swapping away from an often-healthier account (#109).
        assert_eq!(cell(Some(Dead), true, true, true), "🔴 recovering");
        // The rotation `disabled` tag (#36) is orthogonal to credential health — a parked
        // account can be perfectly healthy — so it TRAILS the glyph rather than replacing it.
        assert_eq!(cell(Some(Healthy), false, false, false), "🟢 disabled");
        assert_eq!(
            cell(Some(Degraded), true, false, false),
            "🟠 degraded — run 'sessiometer poke' disabled"
        );
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
        // AC-1 end-to-end: a 5-state glyph per account, the credential-dead one showing 🔴 with
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
            systemic_refresh_failure: None,
            canonical_scrub: None,
            keychain_locked: false,
            recent_blind_preempt_swap: None,
            refresh_enabled: None,
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
        assert!(crate::redaction::meter::unauthored_emails(&out, &[]).is_empty());
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
            systemic_refresh_failure: None,
            canonical_scrub: None,
            keychain_locked: false,
            recent_blind_preempt_swap: None,
            refresh_enabled: None,
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
        // #143 + #137 + #427: the AUTH column renders each rollup state as its self-coloring
        // glyph — the neutral ⚪ Unknown (#137) told apart from a genuine 🟢, the 🟠 `Degraded`
        // (quarantined-but-refreshable) with a needs-refresh cue and NEVER "claude /login", and
        // the 🔴 PROVEN-`Dead` account keeping its re-login cue.
        use CredentialHealth::{AtRisk, Dead, Degraded, Healthy, Stale, Unknown};
        let line = |label, health| AccountStatusLine {
            health: Some(health),
            ..status_line(label, false, Some(10), Some(20))
        };
        let response = StatusResponse {
            systemic_refresh_failure: None,
            canonical_scrub: None,
            keychain_locked: false,
            recent_blind_preempt_swap: None,
            refresh_enabled: None,
            accounts: vec![
                line("healthy", Healthy),
                line("unknownacct", Unknown),
                line("staleacct", Stale),
                line("atriskacct", AtRisk),
                {
                    // A quarantined-but-refreshable account: 🟠 Degraded, needs a refresh.
                    let mut degraded = line("degradedacct", Degraded);
                    degraded.quarantined = true;
                    degraded
                },
                {
                    // A PROVEN-dead account (a refresh returned Dead): 🔴, needs a re-login.
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
        // AC-1: the degraded account is 🟠 with a needs-refresh cue, and NEVER "claude /login".
        assert!(
            row("degradedacct").contains("🟠")
                && row("degradedacct").contains("sessiometer poke")
                && !row("degradedacct").contains("claude /login"),
            "the degraded state is 🟠 needs-refresh, never the re-login cue: {}",
            row("degradedacct")
        );
        // AC-2: 🔴 / "claude /login" appears ONLY for the proven-dead account.
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
            systemic_refresh_failure: None,
            canonical_scrub: None,
            keychain_locked: false,
            recent_blind_preempt_swap: None,
            refresh_enabled: None,
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
        // #15/#444: labels + a timestamp only, so no NON-authored email rides the surface.
        assert!(
            crate::redaction::meter::unauthored_emails(&verbose, &[]).is_empty(),
            "no non-authored email on the verbose surface (#15/#444): {verbose}"
        );
    }

    #[test]
    fn render_access_token_expiry_is_empty_for_an_empty_roster() {
        // No accounts → no block at all (the table renders its own empty state), so a bare
        // `status --verbose` on an empty roster adds nothing.
        let response = StatusResponse {
            systemic_refresh_failure: None,
            canonical_scrub: None,
            keychain_locked: false,
            recent_blind_preempt_swap: None,
            refresh_enabled: None,
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
            blind_active: None,
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
            blind_active: None,
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
            systemic_refresh_failure: None,
            canonical_scrub: None,
            keychain_locked: false,
            recent_blind_preempt_swap: None,
            refresh_enabled: None,
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
                reason: None,
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
            systemic_refresh_failure: None,
            canonical_scrub: None,
            keychain_locked: false,
            recent_blind_preempt_swap: None,
            refresh_enabled: None,
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
                reason: None,
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
            systemic_refresh_failure: None,
            canonical_scrub: None,
            keychain_locked: false,
            recent_blind_preempt_swap: None,
            refresh_enabled: None,
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
            systemic_refresh_failure: None,
            canonical_scrub: None,
            keychain_locked: false,
            recent_blind_preempt_swap: None,
            refresh_enabled: None,
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
            systemic_refresh_failure: None,
            canonical_scrub: None,
            keychain_locked: false,
            recent_blind_preempt_swap: None,
            refresh_enabled: None,
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
                systemic_refresh_failure: None,
                canonical_scrub: None,
                keychain_locked: false,
                recent_blind_preempt_swap: None,
                refresh_enabled: None,
                accounts: vec![status_line("work", true, Some(50), Some(25))],
                next_swap,
            };
            render_status(&response, NOW, None, false)
                .lines()
                .last()
                .unwrap()
                .to_owned()
        };
        // The daemon's own selection rationale (issue #393) renders as a parenthetical: the #37
        // soonest-reset axis, the sole-candidate default, or the no-tiebreak roster-order fallback.
        // The `resets_at` value is not shown (the per-account "resets in" already carries the
        // clock) — only WHICH axis chose it.
        assert_eq!(
            footer(Some(NextSwap::Target {
                to: "spare".to_owned(),
                reason: Some(NextSwapReason::SoonestReset {
                    resets_at: NOW + 3600
                }),
            })),
            "next swap: spare (weekly resets soonest)"
        );
        assert_eq!(
            footer(Some(NextSwap::Target {
                to: "spare".to_owned(),
                reason: Some(NextSwapReason::OnlyCandidate),
            })),
            "next swap: spare (only viable target)"
        );
        // ≥2 viable but no reset times to compare → the footer must NOT claim "only viable target".
        assert_eq!(
            footer(Some(NextSwap::Target {
                to: "spare".to_owned(),
                reason: Some(NextSwapReason::RosterOrder),
            })),
            "next swap: spare (first eligible; no reset times known)"
        );
        // A pre-#393 daemon carries a target with no reason (`None`) → the bare label, the honest
        // fallback (strictly more honest than the superseded "most headroom" story it replaced).
        assert_eq!(
            footer(Some(NextSwap::Target {
                to: "spare".to_owned(),
                reason: None,
            })),
            "next swap: spare"
        );
        // The fleet-capacity relief hint (issue #405): a WEEKLY-wide block names the terminal
        // signal + the reset that ends it + the escape action (the wait is days). `resets_at`
        // humanizes with the same `humanize_until` the per-account cells use → `2d4h`.
        assert_eq!(
            footer(Some(NextSwap::NoViableTarget {
                cause: Some(NoTargetCause::Weekly),
                resets_at: Some(NOW + 2 * 86_400 + 4 * 3_600),
            })),
            "next swap: none — every account is weekly-exhausted; resets in 2d4h — add an account"
        );
        // Weekly-exhausted but no spare reported a parseable reset → the reset clause drops, the
        // terminal signal + action remain.
        assert_eq!(
            footer(Some(NextSwap::NoViableTarget {
                cause: Some(NoTargetCause::Weekly),
                resets_at: None,
            })),
            "next swap: none — every account is weekly-exhausted — add an account"
        );
        // A SESSION-wide block lifts at the sooner session reset (minutes/hours) — the reset time
        // itself is the remedy, so no "add an account" nudge.
        assert_eq!(
            footer(Some(NextSwap::NoViableTarget {
                cause: Some(NoTargetCause::Session),
                resets_at: Some(NOW + 47 * 60),
            })),
            "next swap: none — every account is over its session limit; resets in 47m"
        );
        // A pre-schema-1.3 daemon carries no relief (`cause` absent) → the honest bare fallback.
        assert_eq!(
            footer(Some(NextSwap::NoViableTarget {
                cause: None,
                resets_at: None,
            })),
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
            systemic_refresh_failure: None,
            canonical_scrub: None,
            keychain_locked: false,
            recent_blind_preempt_swap: None,
            refresh_enabled: None,
            accounts: vec![status_line("work", true, Some(99), Some(40))],
            next_swap: Some(NextSwap::Target {
                to: "spare".to_owned(),
                reason: None,
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

    // --- status: isolated-refresh discoverability advisory (issue #138) -------

    /// One account line with a chosen credential rollup, layered over `status_line`
    /// (a #138 fixture: the advisory keys off `active` + `health`). Labels use the
    /// `account-a/b/c` placeholders (AC-4, no PII).
    fn health_line(label: &str, active: bool, health: CredentialHealth) -> AccountStatusLine {
        AccountStatusLine {
            health: Some(health),
            ..status_line(label, active, Some(10), Some(20))
        }
    }

    #[test]
    fn render_status_advises_poke_when_refresh_off_and_a_nonactive_account_is_unhealthy() {
        // AC-1: `[refresh].enabled = false` (wire `Some(false)`) AND ≥1 NON-ACTIVE account not
        // healthy (here ⚪ Unknown — the "unverified" case the issue calls out) → one advisory
        // line that names BOTH remedies (`poke` and enabling `[refresh]`). Color gate open (an
        // interactive TTY).
        let response = StatusResponse {
            systemic_refresh_failure: None,
            canonical_scrub: None,
            keychain_locked: false,
            recent_blind_preempt_swap: None,
            refresh_enabled: Some(false),
            accounts: vec![
                health_line("account-a", true, CredentialHealth::Healthy),
                health_line("account-b", false, CredentialHealth::Unknown),
            ],
            next_swap: None,
        };
        let out = render_status(&response, NOW, None, true);
        let advisory = out
            .lines()
            .find(|l| l.starts_with("advisory:"))
            .expect("the #138 advisory line is present");
        assert!(
            advisory.contains("poke") && advisory.contains("[refresh]"),
            "the advisory names both remedies (poke / enable [refresh]): {advisory:?}"
        );
        // AC-4: no PII — the advisory never names a specific account.
        assert!(
            !advisory.contains("account-a") && !advisory.contains("account-b"),
            "the advisory carries no account labels: {advisory:?}"
        );
    }

    #[test]
    fn render_status_advisory_fires_for_every_non_healthy_nonactive_rollup() {
        // AC-1 breadth: each of ⚪ Unknown / 🟡 Stale / 🟠 AtRisk / 🟠 Degraded / 🔴 Dead on a
        // NON-ACTIVE account arms the advisory (all are "unhealthy/unverified"); only 🟢 Healthy
        // does not. A degraded account is exactly the refresh-off case the advisory points at
        // ("run 'sessiometer poke' or enable [refresh]") — issue #427.
        use CredentialHealth::{AtRisk, Dead, Degraded, Healthy, Stale, Unknown};
        for health in [Unknown, Stale, AtRisk, Degraded, Dead] {
            let response = StatusResponse {
                systemic_refresh_failure: None,
                canonical_scrub: None,
                keychain_locked: false,
                recent_blind_preempt_swap: None,
                refresh_enabled: Some(false),
                accounts: vec![
                    health_line("account-a", true, Healthy),
                    health_line("account-b", false, health),
                ],
                next_swap: None,
            };
            let out = render_status(&response, NOW, None, true);
            assert!(
                out.lines().any(|l| l.starts_with("advisory:")),
                "a non-active {health:?} account arms the #138 advisory:\n{out}"
            );
        }
    }

    #[test]
    fn render_status_advisory_suppressed_when_refresh_enabled() {
        // AC-2: `[refresh]` enabled (`Some(true)`) suppresses the advisory even with an unhealthy
        // non-active account — the maintenance mechanism is already on.
        let response = StatusResponse {
            systemic_refresh_failure: None,
            canonical_scrub: None,
            keychain_locked: false,
            recent_blind_preempt_swap: None,
            refresh_enabled: Some(true),
            accounts: vec![
                health_line("account-a", true, CredentialHealth::Healthy),
                health_line("account-b", false, CredentialHealth::Dead),
            ],
            next_swap: None,
        };
        let out = render_status(&response, NOW, None, true);
        assert!(
            !out.contains("advisory:"),
            "an enabled [refresh] suppresses the #138 advisory:\n{out}"
        );
    }

    #[test]
    fn render_status_advisory_suppressed_when_no_nonactive_account_is_unhealthy() {
        // AC-2: refresh off, but every NON-ACTIVE account is 🟢 Healthy → nothing to advise.
        let response = StatusResponse {
            systemic_refresh_failure: None,
            canonical_scrub: None,
            keychain_locked: false,
            recent_blind_preempt_swap: None,
            refresh_enabled: Some(false),
            accounts: vec![
                health_line("account-a", true, CredentialHealth::Healthy),
                health_line("account-b", false, CredentialHealth::Healthy),
                health_line("account-c", false, CredentialHealth::Healthy),
            ],
            next_swap: None,
        };
        let out = render_status(&response, NOW, None, true);
        assert!(
            !out.contains("advisory:"),
            "an all-healthy non-active roster suppresses the #138 advisory:\n{out}"
        );
    }

    #[test]
    fn render_status_advisory_ignores_the_active_account_health() {
        // AC-1 scoping: the ACTIVE account is maintained live by the daemon (poll-path refresh,
        // #162) — it is never the stale-fallback concern. An unhealthy ACTIVE account with all
        // non-active accounts healthy does NOT arm the advisory.
        let response = StatusResponse {
            systemic_refresh_failure: None,
            canonical_scrub: None,
            keychain_locked: false,
            recent_blind_preempt_swap: None,
            refresh_enabled: Some(false),
            accounts: vec![
                health_line("account-a", true, CredentialHealth::Dead),
                health_line("account-b", false, CredentialHealth::Healthy),
            ],
            next_swap: None,
        };
        let out = render_status(&response, NOW, None, true);
        assert!(
            !out.contains("advisory:"),
            "only NON-active staleness arms the #138 advisory:\n{out}"
        );
    }

    #[test]
    fn render_status_advisory_rides_the_color_gate() {
        // AC-3: the advisory is chrome, not data — it rides the #73 color gate. With the gate
        // CLOSED (`color = false`: a pipe / redirect / NO_COLOR / --no-color / non-TTY) it is
        // suppressed, so `status | grep` and `status > file` stay advisory-free, exactly like the
        // ANSI overlay. Same response as AC-1, only the gate differs.
        let response = StatusResponse {
            systemic_refresh_failure: None,
            canonical_scrub: None,
            keychain_locked: false,
            recent_blind_preempt_swap: None,
            refresh_enabled: Some(false),
            accounts: vec![
                health_line("account-a", true, CredentialHealth::Healthy),
                health_line("account-b", false, CredentialHealth::Unknown),
            ],
            next_swap: None,
        };
        let piped = render_status(&response, NOW, None, false);
        assert!(
            !piped.contains("advisory:"),
            "a closed color gate suppresses the #138 advisory (never into a pipe/redirect):\n{piped}"
        );
        // Sanity: the SAME response with the gate open DOES advise (isolating the gate as the
        // only difference).
        let interactive = render_status(&response, NOW, None, true);
        assert!(interactive.contains("advisory:"), "{interactive}");
    }

    #[test]
    fn render_status_advisory_suppressed_for_a_pre_138_daemon() {
        // A pre-#138 daemon omits `refresh_enabled` → the client decodes `None` → "unknown", and
        // suppresses rather than mis-firing a stale advisory against a daemon whose refresh state
        // it cannot know.
        let response = StatusResponse {
            systemic_refresh_failure: None,
            canonical_scrub: None,
            keychain_locked: false,
            recent_blind_preempt_swap: None,
            refresh_enabled: None,
            accounts: vec![
                health_line("account-a", true, CredentialHealth::Healthy),
                health_line("account-b", false, CredentialHealth::Dead),
            ],
            next_swap: None,
        };
        let out = render_status(&response, NOW, None, true);
        assert!(
            !out.contains("advisory:"),
            "an unknown (pre-#138) refresh state suppresses the #138 advisory:\n{out}"
        );
    }

    #[test]
    fn status_json_carries_the_refresh_flag_never_the_advisory_text() {
        // AC-3 (`--json`): the JSON view serializes the raw `StatusResponse` — it carries the
        // `refresh_enabled` SIGNAL (a bonus for scripts) but NEVER the advisory TEXT, which is a
        // human-only render_status string. This is the exact payload `status --json` prints
        // (cli.rs:951-953), so the advisory can never reach a `--json | jq` consumer as data.
        let response = StatusResponse {
            systemic_refresh_failure: None,
            canonical_scrub: None,
            keychain_locked: false,
            recent_blind_preempt_swap: None,
            refresh_enabled: Some(false),
            accounts: vec![
                health_line("account-a", true, CredentialHealth::Healthy),
                health_line("account-b", false, CredentialHealth::Dead),
            ],
            next_swap: None,
        };
        let json = serde_json::to_string_pretty(&response).unwrap();
        assert!(
            json.contains("\"refresh_enabled\": false"),
            "the flag is on the wire for scripts: {json}"
        );
        assert!(
            !json.contains("advisory") && !json.contains("poke"),
            "the advisory text is never serialized into --json: {json}"
        );
    }

    #[test]
    fn render_status_never_carries_an_email_or_token_sigil() {
        // #15: the printer sources only labels + percentages + reset instants + a
        // next-swap candidate label, so a token / email can never reach the printed surface.
        let response = StatusResponse {
            systemic_refresh_failure: None,
            canonical_scrub: None,
            keychain_locked: false,
            recent_blind_preempt_swap: None,
            refresh_enabled: None,
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
                reason: None,
            }),
        };
        let out = render_status(&response, NOW, None, false);
        assert!(
            crate::redaction::meter::unauthored_emails(&out, &[]).is_empty(),
            "status output must not contain a non-authored email (#15/#444): {out:?}"
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
            systemic_refresh_failure: None,
            canonical_scrub: None,
            keychain_locked: false,
            recent_blind_preempt_swap: None,
            refresh_enabled: None,
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
            systemic_refresh_failure: None,
            canonical_scrub: None,
            keychain_locked: false,
            recent_blind_preempt_swap: None,
            refresh_enabled: None,
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
                reason: None,
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
            systemic_refresh_failure: None,
            canonical_scrub: None,
            keychain_locked: false,
            recent_blind_preempt_swap: None,
            refresh_enabled: None,
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
            systemic_refresh_failure: None,
            canonical_scrub: None,
            keychain_locked: false,
            recent_blind_preempt_swap: None,
            refresh_enabled: None,
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
            systemic_refresh_failure: None,
            canonical_scrub: None,
            keychain_locked: false,
            recent_blind_preempt_swap: None,
            refresh_enabled: None,
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
            systemic_refresh_failure: None,
            canonical_scrub: None,
            keychain_locked: false,
            recent_blind_preempt_swap: None,
            refresh_enabled: None,
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
            systemic_refresh_failure: None,
            canonical_scrub: None,
            keychain_locked: false,
            recent_blind_preempt_swap: None,
            refresh_enabled: None,
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
    fn pad_end_fills_on_display_width_and_never_truncates() {
        // pad_end is the wide-glyph-correct analogue of `{:<width$}` (issue #249). For ASCII
        // it is byte-identical to the fill it replaces — the "zero golden churn" guarantee.
        assert_eq!(pad_end("ab", 5), format!("{:<5}", "ab"));
        assert_eq!(pad_end("ab", 5), "ab   ");
        // A CJK triple is 6 display columns, so padding to 8 adds TWO spaces (not five, as
        // char-count `{:<8}` would) — and the padded field is exactly `width` cells wide.
        assert_eq!(pad_end("日本語", 8), "日本語  ");
        assert_eq!(display_width(&pad_end("日本語", 8)), 8);
        // Already at or over `width` → returned untouched (never truncates), matching the
        // `{:<width$}` fill it replaces.
        assert_eq!(pad_end("日本語", 6), "日本語");
        assert_eq!(pad_end("日本語", 4), "日本語");
        // Degenerate widths.
        assert_eq!(pad_end("x", 0), "x");
        assert_eq!(pad_end("", 3), "   ");
    }

    #[test]
    fn render_roster_label_column_aligns_on_display_width() {
        // The `list` view sized AND padded the label column on char count; a wide-glyph
        // label shifts the uuid column right of the ASCII rows. Padding on display width
        // lands every uuid at one display column (issue #249) — as the `status` table does.
        let roster = [
            acct("ascii", "11111111-1111"),
            acct("日本語", "22222222-2222"),
            acct("👨\u{200D}👩\u{200D}👧", "33333333-3333"),
        ];
        let out = render_roster(&roster, &no_auth(roster.len()), 0);
        // Each row's uuid begins at the same DISPLAY column as the ASCII row's.
        let uuid_col = |label: &str, uuid: &str| {
            let line = out.lines().find(|l| l.contains(label)).unwrap();
            display_width(&line[..line.find(uuid).unwrap()])
        };
        assert_eq!(
            uuid_col("ascii", "11111111-1111"),
            uuid_col("日本語", "22222222-2222"),
            "the CJK row's uuid aligns with the ASCII row's on display width:\n{out}"
        );
        assert_eq!(
            uuid_col("ascii", "11111111-1111"),
            uuid_col("👨\u{200D}👩\u{200D}👧", "33333333-3333"),
            "the emoji row's uuid aligns with the ASCII row's on display width:\n{out}"
        );
    }

    #[test]
    fn render_access_token_expiry_label_column_aligns_on_display_width() {
        // The `--verbose` access-token block (#143) sized AND padded the label on char
        // count; a wide-glyph label shifts the expiry column. Display-width padding aligns
        // every expiry cell (issue #249). The cells differ per row; the column they START
        // at must not.
        let line_for = |label: &str, exp: Option<i64>| AccountStatusLine {
            access_expires_at: exp,
            ..status_line(label, false, None, None)
        };
        let response = StatusResponse {
            systemic_refresh_failure: None,
            canonical_scrub: None,
            keychain_locked: false,
            recent_blind_preempt_swap: None,
            refresh_enabled: None,
            accounts: vec![
                line_for("ascii", Some(NOW + 4 * 3_600)),
                line_for("日本語", Some(NOW + 2 * 3_600)),
                line_for("👨\u{200D}👩\u{200D}👧", None),
            ],
            next_swap: None,
        };
        let out = render_access_token_expiry(&response, NOW);
        let cell_col = |label: &str| {
            let line = out.lines().find(|l| l.contains(label)).unwrap();
            let after = line.find(label).unwrap() + label.len();
            let gap = line[after..].find(|c: char| c != ' ').unwrap();
            display_width(&line[..after + gap])
        };
        assert_eq!(
            cell_col("ascii"),
            cell_col("日本語"),
            "the CJK row's expiry cell aligns with the ASCII row's on display width:\n{out}"
        );
        assert_eq!(
            cell_col("ascii"),
            cell_col("👨\u{200D}👩\u{200D}👧"),
            "the emoji row's expiry cell aligns with the ASCII row's on display width:\n{out}"
        );
    }

    #[test]
    fn colored_output_never_carries_an_email_or_token_sigil() {
        // #15 holds with the #73 overlay: the ANSI codes add only `\x1b[3Xm`…,
        // never an `@`-email or a token sigil.
        let response = StatusResponse {
            systemic_refresh_failure: None,
            canonical_scrub: None,
            keychain_locked: false,
            recent_blind_preempt_swap: None,
            refresh_enabled: None,
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
                reason: None,
            }),
        };
        let out = render_status(&response, NOW, None, true);
        assert!(out.contains('\x1b'), "the overlay is active: {out:?}");
        assert!(
            crate::redaction::meter::unauthored_emails(&out, &[]).is_empty(),
            "no non-authored email on the colored surface (#15/#444): {out:?}"
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
    fn render_snapshot_age_reads_updated_ago_or_empty_without_an_instant() {
        let now = 1_000_000;
        // No generation instant (the wire's 0 sentinel) → no header line at all.
        assert_eq!(render_snapshot_age(0, now), "");
        assert_eq!(render_snapshot_age(-5, now), "");
        // Same instant → "just now"; older → the two-largest-unit humanization (panel parity).
        assert_eq!(render_snapshot_age(now, now), "updated just now\n");
        assert_eq!(render_snapshot_age(now - 600, now), "updated 10m ago\n");
        // Client-ahead clock skew clamps to "just now" — never a negative age.
        assert_eq!(render_snapshot_age(now + 30, now), "updated just now\n");
    }

    #[test]
    fn render_snapshot_age_marks_stale_beyond_the_max_poll_cadence() {
        let now = 1_000_000;
        // AT the boundary (== the max poll cadence) → fresh, no marker.
        assert_eq!(
            render_snapshot_age(now - STALE_AGE_SECS, now),
            "updated 1h ago\n"
        );
        // One second past it → the ` (stale)` marker, even though the humanized age is unchanged:
        // the threshold is the exact second, not the humanized unit.
        assert_eq!(
            render_snapshot_age(now - STALE_AGE_SECS - 1, now),
            "updated 1h ago (stale)\n"
        );
        // A comfortably-stale snapshot.
        assert_eq!(
            render_snapshot_age(now - 2 * 3_600, now),
            "updated 2h ago (stale)\n"
        );
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
            systemic_refresh_failure: None,
            canonical_scrub: None,
            keychain_locked: false,
            recent_blind_preempt_swap: None,
            refresh_enabled: None,
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

    #[test]
    fn plan_stop_covers_every_supervision_state() {
        use AgentSupervision::{RegisteredIdle, Supervising, Unregistered};

        // launchd owns the process: bootout stops the daemon AND suppresses the `KeepAlive`
        // respawn. It is the daemon, so nothing else needs stopping.
        assert_eq!(plan_stop(Supervising), StopPlan::BootOut);

        // The regression state (issue #397 review): the job sits in the domain with NO process
        // behind it — `launchctl print` still exits 0 — while a foreground `run` owns the lock and
        // the socket. Bootout alone would report a stop that did not happen; a socket shutdown alone
        // would be undone the instant `KeepAlive` respawned the agent. Both, in order.
        assert_eq!(
            plan_stop(RegisteredIdle),
            StopPlan::BootOutThenSocketShutdown
        );

        // No job in the domain — even with a plist on disk from a prior `daemon stop`. Nothing
        // supervises anything, so ask the daemon itself. `plan_stop` cannot see `plist.exists()`;
        // that is the point of its signature, not an omission.
        assert_eq!(plan_stop(Unregistered), StopPlan::SocketShutdown);
    }

    #[test]
    fn plan_restart_covers_every_supervision_state() {
        use AgentSupervision::{RegisteredIdle, Supervising, Unregistered};

        // Supervising settles it: that process holds the single-instance lock, so no foreground
        // daemon can coexist and the other two signals cannot change the answer.
        for daemon_running in [true, false] {
            for service_installed in [true, false] {
                assert_eq!(
                    plan_restart(Supervising, daemon_running, service_installed),
                    RestartPlan::Kickstart,
                    "supervising ⇒ kickstart (running={daemon_running}, installed={service_installed})"
                );
            }
        }

        // Registered but idle: whatever is running, launchd is not supervising it. Kickstarting
        // would hand launchd a `run` that loses the lock, exits 3, and crash-loops under KeepAlive.
        assert_eq!(
            plan_restart(RegisteredIdle, true, true),
            RestartPlan::RefuseUnmanaged
        );
        assert_eq!(
            plan_restart(RegisteredIdle, true, false),
            RestartPlan::RefuseUnmanaged
        );
        // Registered, idle, and nothing running: `kickstart` starts a job that is not running, so
        // no bootstrap is needed and none of the crash-loop hazard applies.
        assert_eq!(
            plan_restart(RegisteredIdle, false, true),
            RestartPlan::Kickstart
        );
        assert_eq!(
            plan_restart(RegisteredIdle, false, false),
            RestartPlan::Kickstart
        );

        // Unregistered with a daemon alive ⇒ a foreground `run`, whatever the plist says.
        assert_eq!(
            plan_restart(Unregistered, true, true),
            RestartPlan::RefuseUnmanaged
        );
        assert_eq!(
            plan_restart(Unregistered, true, false),
            RestartPlan::RefuseUnmanaged
        );
        // Nothing running: a plist on disk is loaded; with no plist there is nothing to restart and
        // nothing to supervise, so `restart` routes to `service install`.
        assert_eq!(
            plan_restart(Unregistered, false, true),
            RestartPlan::Bootstrap
        );
        assert_eq!(
            plan_restart(Unregistered, false, false),
            RestartPlan::RefuseNoService
        );
    }

    #[tokio::test]
    async fn request_shutdown_is_daemon_not_running_when_no_socket_is_bound() {
        // Issue #397: `daemon stop` (unmanaged) over an absent socket means no unmanaged daemon is
        // running. `request_shutdown` maps the connect failure to `DaemonNotRunning`, which the
        // caller (`daemon_stop`) treats as an idempotent "already not running" — a `stop` no-op,
        // never a hard failure. The same friendly remap `query_status` makes.
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("daemon.sock"); // never bound
        let err = request_shutdown(&socket)
            .await
            .expect_err("no daemon → error");
        assert!(matches!(err, Error::DaemonNotRunning), "got {err:?}");
    }

    #[tokio::test]
    async fn request_shutdown_sends_the_shutdown_verb_and_accepts_the_ok_ack() {
        // Issue #397: the client sends exactly one newline-delimited `{"cmd":"shutdown"}` request —
        // the wire contract the daemon's #397 `control_reply` handler parses into
        // `ShutdownRequested` — and returns Ok once the daemon acks `{"ok":true}`. This is the CLI
        // half of the `daemon stop` unmanaged path; the daemon then drives its graceful shutdown.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("daemon.sock");
        let listener = UnixListener::bind(&path).unwrap();

        // Server: accept one connection, assert the exact request line, ack once.
        let server = async {
            use tokio::io::{AsyncBufReadExt, AsyncWriteExt};
            let (stream, _addr) = listener.accept().await.unwrap();
            let mut buffered = tokio::io::BufReader::new(stream);
            let mut request = String::new();
            buffered.read_line(&mut request).await.unwrap();
            assert_eq!(request.trim_end(), r#"{"cmd":"shutdown"}"#);
            buffered.write_all(br#"{"ok":true}"#).await.unwrap();
            buffered.write_all(b"\n").await.unwrap();
            buffered.flush().await.unwrap();
        };

        let (_, result) = tokio::join!(server, request_shutdown(&path));
        result.expect("an `{\"ok\":true}` ack is a successful stop request");
    }

    #[tokio::test]
    async fn request_shutdown_does_not_report_success_on_an_unauthorized_refusal() {
        // Issue #397: the daemon same-user-gates `shutdown` and fail-closes an unauthorized peer
        // with `{"error":"unauthorized"}`. That is NOT a stop — `request_shutdown` must surface it
        // as an error, never a false success that would let `daemon stop` claim a stop that did not
        // happen. (Our own uid always authenticates in practice; this proves the negative path.)
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("daemon.sock");
        let listener = UnixListener::bind(&path).unwrap();

        let server = async {
            use tokio::io::{AsyncBufReadExt, AsyncWriteExt};
            let (stream, _addr) = listener.accept().await.unwrap();
            let mut buffered = tokio::io::BufReader::new(stream);
            let mut request = String::new();
            buffered.read_line(&mut request).await.unwrap();
            buffered
                .write_all(br#"{"error":"unauthorized"}"#)
                .await
                .unwrap();
            buffered.write_all(b"\n").await.unwrap();
            buffered.flush().await.unwrap();
        };

        let (_, result) = tokio::join!(server, request_shutdown(&path));
        assert!(
            result.is_err(),
            "an unauthorized refusal must not read as a successful stop",
        );
    }

    #[tokio::test]
    async fn query_status_round_trips_over_a_real_socket() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("daemon.sock");
        let listener = UnixListener::bind(&path).unwrap();

        // The daemon replies with the FROZEN versioned envelope (issue #164): the payload plus
        // the contract version + freshness stamp.
        let wire = serde_json::to_string(&VersionedStatus {
            schema_version: STATUS_SCHEMA_VERSION,
            generated_at: 1_782_777_600,
            status: StatusResponse {
                systemic_refresh_failure: None,
                canonical_scrub: None,
                keychain_locked: false,
                recent_blind_preempt_swap: None,
                refresh_enabled: None,
                accounts: vec![status_line("work", true, Some(50), Some(25))],
                next_swap: Some(NextSwap::Target {
                    to: "spare".to_owned(),
                    reason: Some(NextSwapReason::SoonestReset {
                        resets_at: 1_782_781_200,
                    }),
                }),
            },
        })
        .unwrap();

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

        // `query_status` returns the raw line; decode it as the caller (`status`) does.
        let (_, line) = tokio::join!(server, query_status(&path));
        let line = line.expect("a live socket round-trips");
        let parsed: VersionedStatus = serde_json::from_str(&line).unwrap();
        // The contract version + freshness stamp survive the round trip (issue #164).
        assert_eq!(parsed.schema_version, STATUS_SCHEMA_VERSION);
        assert_eq!(parsed.generated_at, 1_782_777_600);
        // The flattened payload round-trips intact.
        assert_eq!(parsed.status.accounts.len(), 1);
        assert_eq!(parsed.status.accounts[0].label, "work");
        assert_eq!(parsed.status.accounts[0].session_pct, Some(50));
        // The next-swap candidate — label AND the #393 structured reason — round-trips intact
        // (#88 + #393): the daemon-authoritative rationale survives serialization, so a client
        // reads the SoonestReset epoch off the wire rather than re-deriving any selection heuristic.
        assert_eq!(
            parsed.status.next_swap,
            Some(NextSwap::Target {
                to: "spare".to_owned(),
                reason: Some(NextSwapReason::SoonestReset {
                    resets_at: 1_782_781_200,
                }),
            })
        );
    }

    // --- the frozen snapshot contract's version gate (issue #164) --------------

    /// A wire line for the frozen envelope at an arbitrary contract version, over a one-account
    /// payload — the reference-client gate's input.
    fn versioned_wire(major: u32, minor: u32, generated_at: i64) -> String {
        serde_json::to_string(&VersionedStatus {
            schema_version: SchemaVersion { major, minor },
            generated_at,
            status: StatusResponse {
                systemic_refresh_failure: None,
                canonical_scrub: None,
                keychain_locked: false,
                recent_blind_preempt_swap: None,
                refresh_enabled: None,
                accounts: vec![status_line("work", true, Some(50), Some(25))],
                next_swap: None,
            },
        })
        .unwrap()
    }

    #[test]
    fn gate_status_renders_a_matching_major() {
        // A reply at the build's own contract major decodes to the payload to render.
        let line = versioned_wire(
            STATUS_SCHEMA_VERSION.major,
            STATUS_SCHEMA_VERSION.minor,
            1_782_777_600,
        );
        match gate_status(&line, STATUS_SCHEMA_VERSION).unwrap() {
            StatusView::Render(versioned) => {
                assert_eq!(versioned.schema_version, STATUS_SCHEMA_VERSION);
                assert_eq!(versioned.generated_at, 1_782_777_600);
                assert_eq!(versioned.status.accounts[0].label, "work");
            }
            StatusView::Mismatch { .. } => panic!("a matching major must render"),
        }
    }

    #[test]
    fn gate_status_tolerates_a_higher_minor_at_the_same_major() {
        // A MINOR bump is additive (issue #164): an older client renders it, ignoring what it
        // does not know — only the MAJOR gates.
        let line = versioned_wire(
            STATUS_SCHEMA_VERSION.major,
            STATUS_SCHEMA_VERSION.minor + 7,
            9,
        );
        assert!(matches!(
            gate_status(&line, STATUS_SCHEMA_VERSION).unwrap(),
            StatusView::Render(_)
        ));
    }

    #[test]
    fn gate_status_degrades_on_a_mismatched_major() {
        // A MAJOR bump is breaking: the client must degrade (issue #164 AC-2), never render.
        let line = versioned_wire(STATUS_SCHEMA_VERSION.major + 1, 0, 9);
        match gate_status(&line, STATUS_SCHEMA_VERSION).unwrap() {
            StatusView::Mismatch { wire, supported } => {
                assert_eq!(wire.major, STATUS_SCHEMA_VERSION.major + 1);
                assert_eq!(supported, STATUS_SCHEMA_VERSION);
            }
            StatusView::Render(_) => panic!("a mismatched major must degrade, not render"),
        }
    }

    #[test]
    fn gate_status_degrades_on_a_pre_freeze_reply() {
        // A PRE-#164 daemon omits `schema_version`; it probes as major 0 (fail-safe default),
        // which mismatches the frozen major, so the client degrades rather than assume compat.
        let line = r#"{"accounts":[],"next_swap":null}"#;
        match gate_status(line, STATUS_SCHEMA_VERSION).unwrap() {
            StatusView::Mismatch { wire, .. } => assert_eq!(wire, SchemaVersion::default()),
            StatusView::Render(_) => panic!("a versionless reply must degrade"),
        }
    }

    #[test]
    fn gate_status_probes_the_version_even_when_the_payload_is_incompatible() {
        // The robustness the probe-first design buys (issue #164): a future major whose PAYLOAD
        // no longer decodes into this build's struct (here `accounts` is renamed away and typed
        // as a string) is STILL reported as a clean version mismatch — never a field-level decode
        // error, never a silent mis-render.
        let line = r#"{"schema_version":{"major":2,"minor":0},"generated_at":5,"accts":"gone"}"#;
        match gate_status(line, STATUS_SCHEMA_VERSION).unwrap() {
            StatusView::Mismatch { wire, .. } => assert_eq!(wire.major, 2),
            StatusView::Render(_) => panic!("an incompatible-major payload must degrade"),
        }
    }

    #[test]
    fn render_schema_mismatch_names_both_versions_and_stays_redaction_clean() {
        let banner =
            render_schema_mismatch(SchemaVersion { major: 2, minor: 3 }, STATUS_SCHEMA_VERSION);
        // Names the daemon's version and the build's version, and points at the raw view.
        assert!(banner.contains("v2.3"), "got {banner}");
        assert!(
            banner.contains(&format!(
                "v{}.{}",
                STATUS_SCHEMA_VERSION.major, STATUS_SCHEMA_VERSION.minor
            )),
            "got {banner}"
        );
        assert!(banner.contains("--json"), "got {banner}");
        // #15: the degrade banner is version integers + static text only — no account handle,
        // no email, no token.
        assert!(!banner.contains('@'), "got {banner}");
        assert!(!banner.to_lowercase().contains("token"), "got {banner}");
    }

    #[test]
    fn json_view_carries_schema_version_and_generated_at() {
        // What the `--json` branch emits: the raw envelope re-serialized, carrying BOTH frozen
        // meta fields (issue #164 AC-1) alongside the flat payload.
        let line = versioned_wire(
            STATUS_SCHEMA_VERSION.major,
            STATUS_SCHEMA_VERSION.minor,
            1_782_777_600,
        );
        let versioned: VersionedStatus = serde_json::from_str(&line).unwrap();
        let json = serde_json::to_string_pretty(&versioned).unwrap();
        assert!(json.contains("\"schema_version\""), "got {json}");
        assert!(json.contains("\"major\": 1"), "got {json}");
        assert!(json.contains("\"generated_at\": 1782777600"), "got {json}");
        // The payload stays FLAT at the top level (not nested under a key).
        assert!(json.contains("\"accounts\""), "got {json}");
        // Redaction (#15/#444): no NON-authored email, no token, on the `--json` wire.
        assert!(
            crate::redaction::meter::unauthored_emails(&json, &[]).is_empty(),
            "got {json}"
        );
        assert!(!json.to_lowercase().contains("token"), "got {json}");
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
    fn root_help_carries_the_unofficial_not_affiliated_notice() {
        // Issue #273: the root `--help` overview must carry the 'unofficial /
        // not affiliated' notice, referencing Anthropic's marks only nominatively.
        let help = HelpTopic::Root.help();
        assert!(
            help.contains("unofficial"),
            "root help must state the tool is unofficial:\n{help}"
        );
        assert!(
            help.contains("not affiliated with or endorsed by Anthropic"),
            "root help must carry the not-affiliated notice:\n{help}"
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
    fn service_install_and_uninstall_parse_to_their_actions() {
        // Issue #166: the two background-service sub-verbs route to their actions.
        assert_eq!(
            parse_argv(&["service", "install"]).unwrap(),
            Command::Service {
                action: ServiceAction::Install
            }
        );
        assert_eq!(
            parse_argv(&["service", "uninstall"]).unwrap(),
            Command::Service {
                action: ServiceAction::Uninstall
            }
        );
    }

    #[test]
    fn service_status_parses_but_the_removed_lifecycle_verbs_are_rejected() {
        // Issue #397: `service` keeps only the PERSISTENCE verbs — `status` still parses (the
        // "is-a-managed-service-installed?" question), while the pre-0.1.0 `start`/`stop`/`restart`
        // are REMOVED (process lifecycle moved to `daemon`). The removed verbs must now be
        // strict-usage errors pointing at `service --help`, never a silent no-op nor a stale action.
        assert_eq!(
            parse_argv(&["service", "status"]).unwrap(),
            Command::Service {
                action: ServiceAction::Status
            },
            "`service status` still parses to its action",
        );
        for verb in ["start", "stop", "restart"] {
            match parse_argv(&["service", verb]).unwrap_err() {
                Error::CliUsage {
                    message,
                    usage_hint,
                } => {
                    assert!(
                        message.contains(verb),
                        "names the removed action `{verb}`: {message}",
                    );
                    assert_eq!(usage_hint, "sessiometer service --help");
                }
                other => panic!("`service {verb}` must be a CliUsage error now, got {other:?}"),
            }
        }
    }

    #[test]
    fn service_help_and_bare_service_print_help_never_a_mutating_action() {
        // `service --help` and a bare `service` (no sub-action) both resolve to HELP —
        // pure `Help`, so neither can load/unload a LaunchAgent as a side effect.
        assert_eq!(
            parse_argv(&["service", "--help"]).unwrap(),
            Command::Help(HelpTopic::Service)
        );
        assert_eq!(
            parse_argv(&["service"]).unwrap(),
            Command::Help(HelpTopic::Service)
        );
    }

    #[test]
    fn service_rejects_an_unknown_action_instead_of_silently_installing() {
        // A typo'd sub-action (`instal`) must not fall through to a default — it errors,
        // naming the bad action and pointing at `service --help`.
        match parse_argv(&["service", "instal"]).unwrap_err() {
            Error::CliUsage {
                message,
                usage_hint,
            } => {
                assert!(
                    message.contains("instal"),
                    "names the bad action: {message}"
                );
                assert_eq!(usage_hint, "sessiometer service --help");
            }
            other => panic!("expected a CliUsage error, got {other:?}"),
        }
    }

    #[test]
    fn service_install_rejects_a_force_style_flag_so_nothing_can_pretend_to_bypass_the_guard() {
        // The single-owner guard is a SAFETY guard with no bypass. A `--force` on
        // `service install` is not a silently-accepted no-op — it is rejected as an
        // unknown flag, so no `--force`-shaped incantation can appear to disable it.
        let err = parse_argv(&["service", "install", "--force"]).unwrap_err();
        assert!(matches!(err, Error::CliUsage { .. }));
        assert!(err.to_string().contains("--force"), "got: {err}");
    }

    #[test]
    fn daemon_lifecycle_verbs_parse_to_their_actions() {
        // Issues #396 + #397: the process-lifecycle noun routes `status` (#396) plus the
        // #397-added `stop` / `restart` to their actions, so `execute` dispatches each to
        // `daemon_status` / `daemon_stop` / `daemon_restart`.
        for (verb, expected) in [
            ("status", DaemonAction::Status),
            ("stop", DaemonAction::Stop),
            ("restart", DaemonAction::Restart),
        ] {
            assert_eq!(
                parse_argv(&["daemon", verb]).unwrap(),
                Command::Daemon { action: expected },
                "`daemon {verb}` parses to its action",
            );
        }
    }

    #[test]
    fn daemon_start_is_rejected_because_there_is_no_such_verb() {
        // Issue #397 (recorded verb-set decision): there is deliberately NO `daemon start` — a
        // daemon is started by `service install` (managed) or `sessiometer run` (unmanaged). So
        // `daemon start` is a strict-usage error naming the bad action and pointing at `daemon
        // --help`, never a silent fall-through.
        match parse_argv(&["daemon", "start"]).unwrap_err() {
            Error::CliUsage {
                message,
                usage_hint,
            } => {
                assert!(message.contains("start"), "names the bad action: {message}");
                assert_eq!(usage_hint, "sessiometer daemon --help");
            }
            other => panic!("expected a CliUsage error, got {other:?}"),
        }
    }

    #[test]
    fn daemon_help_and_bare_daemon_print_help_never_an_action() {
        // `daemon --help` and a bare `daemon` (no sub-action) both resolve to HELP — a pure
        // `Help`, so neither can fall through to an action.
        assert_eq!(
            parse_argv(&["daemon", "--help"]).unwrap(),
            Command::Help(HelpTopic::Daemon)
        );
        assert_eq!(
            parse_argv(&["daemon"]).unwrap(),
            Command::Help(HelpTopic::Daemon)
        );
    }

    #[test]
    fn daemon_rejects_an_unknown_action_instead_of_defaulting() {
        // A typo'd sub-action (`statu`) errors, naming the bad action and pointing at
        // `daemon --help` — it never silently falls through to `status`.
        match parse_argv(&["daemon", "statu"]).unwrap_err() {
            Error::CliUsage {
                message,
                usage_hint,
            } => {
                assert!(message.contains("statu"), "names the bad action: {message}");
                assert_eq!(usage_hint, "sessiometer daemon --help");
            }
            other => panic!("expected a CliUsage error, got {other:?}"),
        }
    }

    // --- config diagnostics verbs (issue #401) -----------------------------

    #[test]
    fn config_verbs_parse_to_their_actions() {
        // #401: the three READ-ONLY config diagnostics verbs route to their actions.
        assert_eq!(
            parse_argv(&["config", "path"]).unwrap(),
            Command::Config {
                action: ConfigAction::Path
            }
        );
        assert_eq!(
            parse_argv(&["config", "validate"]).unwrap(),
            Command::Config {
                action: ConfigAction::Validate
            }
        );
        assert_eq!(
            parse_argv(&["config", "show"]).unwrap(),
            Command::Config {
                action: ConfigAction::Show { origin: false }
            }
        );
    }

    #[test]
    fn config_show_origin_flag_sets_origin_order_independently() {
        // `--origin` applies to `show`, before OR after the verb (flag order-independent).
        assert_eq!(
            parse_argv(&["config", "show", "--origin"]).unwrap(),
            Command::Config {
                action: ConfigAction::Show { origin: true }
            }
        );
        assert_eq!(
            parse_argv(&["config", "--origin", "show"]).unwrap(),
            Command::Config {
                action: ConfigAction::Show { origin: true }
            }
        );
    }

    #[test]
    fn config_origin_flag_is_rejected_on_path_and_validate() {
        // `--origin` means nothing for `path`/`validate` — a strict-usage error naming the
        // flag and pointing at `config --help`, never a silent accept.
        for verb in ["path", "validate"] {
            match parse_argv(&["config", verb, "--origin"]).unwrap_err() {
                Error::CliUsage {
                    message,
                    usage_hint,
                } => {
                    assert!(message.contains("--origin"), "names the flag: {message}");
                    assert_eq!(usage_hint, "sessiometer config --help");
                }
                other => panic!("`config {verb} --origin` must be a CliUsage error, got {other:?}"),
            }
        }
    }

    #[test]
    fn config_help_and_bare_config_print_help_never_an_action() {
        // `config --help` and a bare `config` both resolve to HELP — pure `Help`, so neither
        // can read a config or touch state as a side effect (all three verbs are read-only
        // anyway, but bare-noun-is-help stays consistent with `service` / `daemon`).
        assert_eq!(
            parse_argv(&["config", "--help"]).unwrap(),
            Command::Help(HelpTopic::Config)
        );
        assert_eq!(
            parse_argv(&["config"]).unwrap(),
            Command::Help(HelpTopic::Config)
        );
    }

    #[test]
    fn config_rejects_an_unknown_action() {
        // A typo'd sub-action (`shwo`) errors, naming the bad action and pointing at
        // `config --help` — never a silent fall-through.
        match parse_argv(&["config", "shwo"]).unwrap_err() {
            Error::CliUsage {
                message,
                usage_hint,
            } => {
                assert!(message.contains("shwo"), "names the bad action: {message}");
                assert_eq!(usage_hint, "sessiometer config --help");
            }
            other => panic!("expected a CliUsage error, got {other:?}"),
        }
    }

    #[test]
    fn config_rejects_an_unknown_flag() {
        let err = parse_argv(&["config", "show", "--verbose"]).unwrap_err();
        assert!(matches!(err, Error::CliUsage { .. }));
        assert!(err.to_string().contains("--verbose"), "got: {err}");
    }

    #[test]
    fn render_config_origin_tags_values_and_flags_absent_sections() {
        // #401 formatting: with --origin each value trails its provenance tag and an absent
        // `[section]` is flagged; the roster summary carries its own origin.
        use crate::config::{OriginEntry, OriginSection};
        let report = OriginReport {
            sections: vec![OriginSection {
                header: "[tunables]",
                present: false,
                entries: vec![
                    OriginEntry {
                        key: "poll_secs",
                        value: "300".to_string(),
                        origin: Origin::Default,
                    },
                    OriginEntry {
                        key: "session_trigger",
                        value: "90".to_string(),
                        origin: Origin::FromFile,
                    },
                ],
            }],
            roster_count: 2,
            roster_present: true,
        };
        let path = Path::new("/x/config.toml");

        let tagged = render_config_origin(path, &report, true);
        assert!(
            tagged.contains("# /x/config.toml"),
            "names the path: {tagged}"
        );
        assert!(
            tagged.contains("[tunables]") && tagged.contains("absent"),
            "flags the absent section: {tagged}",
        );
        assert!(
            tagged.contains("default"),
            "tags the defaulted value: {tagged}"
        );
        assert!(
            tagged.contains("from-file"),
            "tags the file value: {tagged}"
        );
        assert!(
            tagged.contains("2 accounts") && tagged.contains("from-file"),
            "summarizes the roster with its origin: {tagged}",
        );

        // Without --origin: values only — no tags, no absent-flag.
        let plain = render_config_origin(path, &report, false);
        assert!(
            !plain.contains("from-file"),
            "no tags without --origin: {plain}"
        );
        assert!(
            !plain.contains("absent"),
            "no absent-flag without --origin: {plain}"
        );
        assert!(
            plain.contains("session_trigger = 90"),
            "still prints the value: {plain}",
        );
    }

    #[test]
    fn render_config_origin_pluralizes_a_single_account() {
        // The roster summary reads "1 account" (singular) for a lone account.
        let report = OriginReport {
            sections: vec![],
            roster_count: 1,
            roster_present: true,
        };
        let out = render_config_origin(Path::new("/x/config.toml"), &report, true);
        assert!(out.contains("1 account,"), "singular roster: {out}");
        assert!(!out.contains("1 accounts"), "no plural for one: {out}");
    }

    #[test]
    fn peak_runway_advisory_line_leads_with_the_remedy_and_leaks_no_internal_refs() {
        // Issue #608: the `config validate` advisory line names the offending reserve, the bound,
        // the concrete remedy value, the two lookahead knobs, and the assumed peak — actionable
        // first. It must carry NO internal cross-reference an operator cannot resolve from a
        // terminal (no ADR / issue number — CLAUDE.md audience fidelity), and stay a "tuning note".
        let advisory = crate::config::PeakRunwayAdvisory {
            target_max_session_usage: 80,
            bound_pct: 52,
            window_secs: 313,
        };
        let line = render_peak_runway_advisory(&advisory);
        assert!(
            line.starts_with("advisory: "),
            "leads with the label: {line}"
        );
        assert!(line.contains("target_max_session_usage (80)"), "{line}");
        assert!(line.contains("bound (52)"), "names the bound: {line}");
        assert!(
            line.contains("Lower it to 52 or below"),
            "concrete remedy: {line}"
        );
        assert!(
            line.contains("near_limit_poll_secs") && line.contains("session_velocity_horizon_secs"),
            "names both lookahead knobs: {line}"
        );
        assert!(
            line.contains("313s swap lookahead"),
            "names the window: {line}"
        );
        assert!(
            line.contains("6.95 %/min"),
            "names the assumed peak: {line}"
        );
        assert!(
            line.contains("not an error"),
            "framed as a tuning note: {line}"
        );
        // No internal cross-references / secrets in an operator-facing string.
        for banned in ["ADR-", "#608", "#597", "token", "Bearer"] {
            assert!(!line.contains(banned), "must not leak {banned:?}: {line}");
        }
    }

    #[test]
    fn daemon_status_rejects_an_unknown_flag() {
        // `daemon status --nope` is a strict-usage error (issue #175 posture), pointing at the
        // daemon help — not a silently-dropped flag.
        let err = parse_argv(&["daemon", "status", "--nope"]).unwrap_err();
        assert!(matches!(err, Error::CliUsage { .. }));
        assert!(err.to_string().contains("--nope"), "got: {err}");
    }

    #[test]
    fn daemon_status_report_distinguishes_liveness_and_management_mode() {
        // Issue #396 AC-2 + AC-3: the five states each render an honest, distinct report —
        // responsive vs alive-but-unresponsive vs not-running, crossed with managed vs
        // unmanaged for the two running states.
        let responsive_managed = render_daemon_status(DaemonLiveness::Responsive, true);
        assert!(
            responsive_managed.contains("running and responsive"),
            "{responsive_managed}"
        );
        assert!(
            responsive_managed.contains("managed by launchd"),
            "{responsive_managed}"
        );

        let responsive_unmanaged = render_daemon_status(DaemonLiveness::Responsive, false);
        assert!(
            responsive_unmanaged.contains("running and responsive"),
            "{responsive_unmanaged}"
        );
        assert!(
            responsive_unmanaged.contains("unmanaged"),
            "{responsive_unmanaged}"
        );

        // AC-3 (the headline honesty case): alive-but-unresponsive is reported as RUNNING, NOT
        // as "not running", with the management mode still surfaced.
        let starting_managed = render_daemon_status(DaemonLiveness::AliveUnresponsive, true);
        assert!(
            starting_managed.contains("running but not answering"),
            "{starting_managed}"
        );
        assert!(
            !starting_managed.contains("not running"),
            "alive-but-unresponsive must not read as not-running: {starting_managed}"
        );
        assert!(
            starting_managed.contains("managed by launchd"),
            "{starting_managed}"
        );

        let starting_unmanaged = render_daemon_status(DaemonLiveness::AliveUnresponsive, false);
        assert!(
            starting_unmanaged.contains("running but not answering"),
            "{starting_unmanaged}"
        );
        assert!(
            starting_unmanaged.contains("unmanaged"),
            "{starting_unmanaged}"
        );

        // Not-running is unambiguous and carries no management mode (the `managed` flag is
        // inert), so both plist states render identically.
        assert_eq!(
            render_daemon_status(DaemonLiveness::NotRunning, true),
            "sessiometer: daemon is not running.\n"
        );
        assert_eq!(
            render_daemon_status(DaemonLiveness::NotRunning, false),
            render_daemon_status(DaemonLiveness::NotRunning, true),
        );

        // Every report is a single trailing-newline-terminated line (a clean report to stdout).
        for report in [responsive_managed, starting_managed] {
            assert!(report.ends_with('\n'), "trailing newline: {report:?}");
            assert_eq!(report.matches('\n').count(), 1, "one line only: {report:?}");
        }
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
    fn reliability_parses_bare_json_and_since() {
        // Bare defaults to the human view with no window.
        assert_eq!(
            parse_argv(&["reliability"]).unwrap(),
            Command::Reliability(crate::reliability::ReliabilityArgs {
                json: false,
                since: None,
            })
        );
        assert_eq!(
            parse_argv(&["reliability", "--json"]).unwrap(),
            Command::Reliability(crate::reliability::ReliabilityArgs {
                json: true,
                since: None,
            })
        );
        // `--since` captures its RAW value (space- or `=`-separated); duration parse + validation
        // are deferred to `reliability::run`, so the CLI layer just carries the string through.
        for argv in [
            vec!["reliability", "--since", "7d"],
            vec!["reliability", "--since=7d"],
        ] {
            assert_eq!(
                parse_argv(&argv).unwrap(),
                Command::Reliability(crate::reliability::ReliabilityArgs {
                    json: false,
                    since: Some("7d".to_string()),
                }),
                "argv {argv:?} must carry the raw --since value",
            );
        }
        // `--since` composes with `--json`.
        assert_eq!(
            parse_argv(&["reliability", "--since", "24h", "--json"]).unwrap(),
            Command::Reliability(crate::reliability::ReliabilityArgs {
                json: true,
                since: Some("24h".to_string()),
            })
        );
    }

    #[test]
    fn reliability_since_without_a_value_is_a_clear_error() {
        // `--since` as the last token → a clear "needs a value", not a silent empty window.
        let err = parse_argv(&["reliability", "--since"]).unwrap_err();
        assert!(matches!(err, Error::CliUsage { .. }));
        assert!(err.to_string().contains("since"), "got: {err}");
    }

    #[test]
    fn reliability_help_routes_and_an_unknown_flag_is_a_clear_error() {
        assert_eq!(
            parse_argv(&["reliability", "--help"]).unwrap(),
            Command::Help(HelpTopic::Reliability)
        );
        // A stray positional or flag the readout does not accept → strict-usage error.
        let err = parse_argv(&["reliability", "--period"]).unwrap_err();
        assert!(matches!(err, Error::CliUsage { .. }));
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
