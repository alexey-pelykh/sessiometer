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
    emit_best_effort, run_loop, AccountExpiry, AccountStatusLine, BlindActive, CanaryStatus,
    CanonicalScrub, Daemon, ExpiryCohort, ExternalLoginWatcher, InstanceLock, NextSwap,
    NextSwapReason, NoTargetCause, RealClock, RealKeepWarmEngine, RealRosterPoller, RealShutdown,
    SchemaVersion, StatusResponse, SystemicRefreshSource, UnixControl, VersionedStatus,
    STATUS_SCHEMA_VERSION,
};
use crate::error::{Error, Result};
use crate::keychain::{Credential, CredentialStore, RealCredentialStore};
use crate::migration::{ManagedAccount, MigrationArtifact, Passphrase, Payload, PLAINTEXT_WARNING};
use crate::observability::{
    CredentialHealth, Diagnostic, DiagnosticLog, Event, EventLog, ExpiryHorizon, ExportMode,
    RefreshEventOutcomeKind, Verbosity,
};
use crate::paths;
use crate::refresh;
use crate::refresh_tick::{self, RealRefreshEngine, RefreshTick};
use crate::service::AgentSupervision;
use crate::sha256::sha256_hex;
use crate::stash::{AccountStash, RealAccountStash, StashedAccount};
use crate::swap::{SwapLock, SWAP_LOCK_MAX_WAIT};
use crate::systemic_refresh;

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
    /// `run [-v|--verbose] [--managed]` — the foreground poll+swap daemon. `--managed`
    /// marks a launchd-invoked agent (the bundled `org.sessiometer.agent` / `service install`
    /// plist): on single-instance-lock contention it stands down cleanly (exit `0`) rather
    /// than returning the human-CLI exit-`3` `AlreadyRunning`, so the generated plist's
    /// conditional `KeepAlive` does not respawn it into a throttled loop (issue #742).
    Run { verbose: bool, managed: bool },
    /// `service install|uninstall|status` — the PERSISTENCE noun (issue #397): manage the
    /// background launchd LaunchAgent (install/uninstall) and report whether one is installed.
    Service { action: ServiceAction },
    /// `daemon status|stop|restart` — the daemon *process* (issues #396, #397): its liveness
    /// and management mode (`status`, read-only), plus stopping / restarting it (`stop` /
    /// `restart`). The process-lifecycle counterpart to the persistence-oriented `service` noun.
    Daemon { action: DaemonAction },
    /// `config path|validate|show|backups|restore [--origin]` — config diagnostics (issue #401)
    /// and the roster backup ring (issue #1439): resolve the `config.toml` path, parse+validate
    /// it without running, print the effective config with each value tagged `default` vs
    /// `from-file`, list retained backups, or restore one. `restore` is the only one that
    /// mutates — it writes `config.toml` and notifies a running daemon; the other four are
    /// read-only. See [`ConfigAction`] for why the noun is no longer read-only wholesale.
    Config { action: ConfigAction },
    /// `status [--json] [--no-color] [-v|--verbose]` — the live status client.
    Status {
        json: bool,
        no_color: bool,
        verbose: bool,
    },
    /// `list` — the offline roster view.
    List,
    /// `use <account> [--force]` / `use --next [--force]` — switch the active account now.
    /// `target` and `next` are mutually exclusive and the parser rejects both together
    /// (issue #960): `--next` means "the account the operator did NOT name", resolved
    /// from the daemon's published `next_swap`.
    Use {
        target: Option<String>,
        force: bool,
        next: bool,
    },
    /// `disable`/`enable <account>` — flip an account's rotation flag (`enabled`).
    /// `target` is a label OR an account-uuid, as `use`/`poke` take (issue #1005).
    SetEnabled {
        target: Option<String>,
        enabled: bool,
    },
    /// `remove <account>` — drop an account and erase its stash. `target` is a label OR
    /// an account-uuid (issue #1005).
    Remove { target: Option<String> },
    /// `poke [<account>]` — refresh a parked account's credential once.
    Poke { target: Option<String> },
    /// `stats [<account>...] [--period …] [--since …] [--json] [--no-color] [--ascii]`.
    Stats(crate::stats::StatsArgs),
    /// `reliability [--json]` — the OFFLINE reliability-SLO readout over the event log (#455).
    Reliability(crate::reliability::ReliabilityArgs),
    /// `log [--since <duration>] [--event <name>] [--json]` — the OFFLINE reader for the event
    /// log's lines themselves (issue #773), as opposed to the SLIs `reliability` folds them into.
    Log(crate::log::LogArgs),
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

/// The `config` sub-action (issues #401, #1439): config diagnostics, plus the backup ring's
/// operator surface. `path` prints the resolved `config.toml` location, `validate` parses +
/// validates it WITHOUT running (the same seam the daemon loads through), `show` prints the
/// effective config — with `--origin`, each value tagged `default` vs `from-file` so a
/// silently-defaulted absent section is visible — and `backups` enumerates what the roster
/// backup ring retains. A plain data enum, like [`ServiceAction`] / [`DaemonAction`] — the
/// parser resolves the sub-verb (and the `--origin` flag, and `restore`'s index) so `execute`
/// just dispatches.
///
/// Four of the five are READ-ONLY. `restore` is the exception and the reason this noun is no
/// longer described as read-only wholesale: issue #1439 R-9 requires an operator-invocable path
/// back from a lost roster that does not involve hand-editing TOML, and it lands here because
/// it is a `config.toml` operation and belongs beside the listing that names its argument.
#[derive(Debug, PartialEq)]
enum ConfigAction {
    /// `config path` — print the resolved `config.toml` path (honours `$XDG_CONFIG_HOME`).
    Path,
    /// `config validate` — parse + validate without running; report the first error class.
    Validate,
    /// `config show [--origin]` — print the effective config; `--origin` tags each value's provenance.
    Show { origin: bool },
    /// `config backups` — list what the roster backup ring retains (issue #1439).
    Backups,
    /// `config restore <N>` — replace `config.toml` with retained backup `N` (issue #1439).
    /// The index is 1-based and newest-first, as `config backups` prints it.
    Restore { index: usize },
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
    Log,
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
            HelpTopic::Log => "sessiometer log --help",
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
            HelpTopic::Log => LOG_USAGE,
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

/// Parse `run [-v|--verbose] [--managed]` (issues #77, #742) — the verbosity flag and the
/// launchd-managed marker, both position-independent.
fn parse_run(parser: &mut lexopt::Parser) -> Result<Command> {
    let mut verbose = false;
    let mut managed = false;
    while let Some(arg) = parser.next()? {
        match arg {
            Short('h') | Long("help") => return Ok(Command::Help(HelpTopic::Run)),
            Short('v') | Long("verbose") => verbose = true,
            Long("managed") => managed = true,
            Value(_) => {}
            other => return Err(unexpected(other, HelpTopic::Run)),
        }
    }
    Ok(Command::Run { verbose, managed })
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

/// Parse `config <path|validate|show|backups|restore> [<index>] [--origin]` (issues #401,
/// #1439): the config-diagnostics noun, plus the backup ring's two operator verbs. The first
/// positional is the sub-action and the second is `restore`'s index; the order-independent
/// `--origin` flag applies to `show` (tag each value default-vs-file). `-h`/`--help`
/// short-circuits, an unknown flag or action is a strict-usage error, and bare `config` prints
/// the config help. Mirrors [`parse_service`] / [`parse_daemon`]; the `--origin` flag is the
/// only shape difference, and it is REJECTED on every action but `show` (where alone it means
/// something) rather than silently accepted — the same strict-usage stance as an unknown flag
/// (issue #175).
fn parse_config(parser: &mut lexopt::Parser) -> Result<Command> {
    let mut action_name = None;
    let mut argument = None;
    let mut origin = false;
    while let Some(arg) = parser.next()? {
        match arg {
            Short('h') | Long("help") => return Ok(Command::Help(HelpTopic::Config)),
            Long("origin") => origin = true,
            Value(value) if action_name.is_none() => {
                action_name = Some(value.to_string_lossy().into_owned());
            }
            Value(value) if argument.is_none() => {
                argument = Some(value.to_string_lossy().into_owned());
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
        "backups" => ConfigAction::Backups,
        "restore" => ConfigAction::Restore {
            index: backup_index(argument.as_deref())?,
        },
        other => {
            return Err(Error::CliUsage {
                message: format!("unknown config action `{other}`"),
                usage_hint: HelpTopic::Config.hint(),
            })
        }
    };
    // `--origin` only means something for `show`; everywhere else it is a usage error.
    if origin && !matches!(action, ConfigAction::Show { .. }) {
        return Err(Error::CliUsage {
            message: "`--origin` applies only to `config show`".to_string(),
            usage_hint: HelpTopic::Config.hint(),
        });
    }
    Ok(Command::Config { action })
}

/// Resolve `config restore`'s positional into a 1-based ring index (issue #1439).
///
/// Both rejections are shaped at ONE construction site, so the two messages cannot drift into
/// different registers and the `CliUsage` construction-site count in
/// `every_cli_usage_construction_site_is_scanned` moves by one rather than by two. `0` is
/// rejected with the non-numeric input: the listing this index comes from is 1-based, so a `0`
/// is a miscount rather than a boundary, and answering it with the ring's oldest entry would
/// silently restore something the operator did not name.
fn backup_index(argument: Option<&str>) -> Result<usize> {
    let message = match argument {
        Some(raw) => match raw.parse::<usize>() {
            Ok(index) if index > 0 => return Ok(index),
            _ => format!("invalid backup index `{raw}`: expected a whole number from 1 upward"),
        },
        None => "`config restore` takes the index of a retained backup, as numbered by `config \
                 backups`"
            .to_string(),
    };
    Err(Error::CliUsage {
        message,
        usage_hint: HelpTopic::Config.hint(),
    })
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
    let mut next = false;
    while let Some(arg) = parser.next()? {
        match arg {
            Short('h') | Long("help") => return Ok(Command::Help(HelpTopic::Use)),
            Long("force") => force = true,
            // Set in the same flag loop as `--force` (issue #960), so `--next` is
            // ORDER-INDEPENDENT for free: `use --next --force` and `use --force --next`
            // parse identically, exactly as the two `--force` orders already do.
            Long("next") => next = true,
            Value(value) if target.is_none() => {
                target = Some(value.to_string_lossy().into_owned());
            }
            Value(_) => {}
            other => return Err(unexpected(other, HelpTopic::Use)),
        }
    }
    // Mutually exclusive, checked AFTER the loop so BOTH orders (`use spare --next` and
    // `use --next spare`) are rejected identically (issue #960). Silently preferring one
    // would swap to an account the operator did not ask for; the flag says "I am not
    // naming a target", so naming one contradicts it. The message names neither the
    // target nor a label — the operator can see their own argv, and a roster label may be
    // an operator-authored email (#444/#447) that has no business in a usage error (#15).
    if next && target.is_some() {
        return Err(Error::CliUsage {
            message: "`--next` and an explicit target are mutually exclusive: `--next` asks \
                      the daemon which account comes next, so naming one contradicts it"
                .to_owned(),
            usage_hint: HelpTopic::Use.hint(),
        });
    }
    Ok(Command::Use {
        target,
        force,
        next,
    })
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

/// Parse `log [--since <duration>] [--event <name>] [--channel <c>] [--json] [-f|--follow]`
/// (issues #773, #774, #775) — the offline reader for the daemon's own output lines. Flags only:
/// there is no positional form, because the thing one would filter by is an event name, and that
/// is `--event`.
///
/// `--channel` is validated HERE, unlike `--since` (whose grammar is resolved in `log::run`, where
/// the clock is read): its value set is closed and needs no clock, so rejecting a typo at the
/// parse boundary keeps the reader's own path total over an enum rather than a string.
fn parse_log(parser: &mut lexopt::Parser) -> Result<Command> {
    let mut json = false;
    let mut since = None;
    let mut event = None;
    let mut follow = false;
    let mut channel = crate::log::Channel::Event;
    while let Some(arg) = parser.next()? {
        match arg {
            Short('h') | Long("help") => return Ok(Command::Help(HelpTopic::Log)),
            Long("json") => json = true,
            // `-f` is the short form every tailer has had since `tail(1)`; spelling it out costs
            // nothing and not having it would surprise.
            Short('f') | Long("follow") => follow = true,
            Long("since") => {
                since = Some(
                    required_value(parser, "since", HelpTopic::Log)?
                        .to_string_lossy()
                        .into_owned(),
                );
            }
            Long("event") => {
                event = Some(
                    required_value(parser, "event", HelpTopic::Log)?
                        .to_string_lossy()
                        .into_owned(),
                );
            }
            Long("channel") => {
                let raw = required_value(parser, "channel", HelpTopic::Log)?
                    .to_string_lossy()
                    .into_owned();
                channel = crate::log::Channel::parse(&raw).ok_or(Error::LogChannelInvalid(raw))?;
            }
            other => return Err(unexpected(other, HelpTopic::Log)),
        }
    }
    Ok(Command::Log(crate::log::LogArgs {
        since,
        event,
        json,
        follow,
        channel,
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
        "disable" => parse_positional(parser, HelpTopic::Disable, |target| Command::SetEnabled {
            target,
            enabled: false,
        }),
        "enable" => parse_positional(parser, HelpTopic::Enable, |target| Command::SetEnabled {
            target,
            enabled: true,
        }),
        "remove" => parse_positional(parser, HelpTopic::Remove, |target| Command::Remove {
            target,
        }),
        "poke" => parse_positional(parser, HelpTopic::Poke, |target| Command::Poke { target }),
        "stats" => parse_stats(parser),
        "reliability" => parse_reliability(parser),
        "log" => parse_log(parser),
        "export" => parse_export(parser),
        "import" => parse_import(parser),
        other => Err(Error::UnknownCommand(other.to_owned())),
    }
}

/// The `--version` output (issue #175): the crate name plus `CARGO_PKG_VERSION` (the sole
/// version source, `Cargo.toml`), followed by a neutral provenance line naming the Claude Code
/// range the reverse-engineered internals were verified against (issue #716). The provenance
/// line is UNCONDITIONAL — it prints the baked `CC_SUPPORTED_MIN`/`MAX` constants and never
/// probes `claude`, so it is a record, not an alarm. Extracted so the parser test can assert
/// both lines without capturing stdout.
fn version_line() -> String {
    format!(
        "{}\n{}",
        concat!("sessiometer ", env!("CARGO_PKG_VERSION")),
        crate::cc_version::supported_range_provenance(),
    )
}

/// Run a parsed [`Command`]. The inverse of `parse`: this half owns the I/O (keychain,
/// roster, daemon socket), so `parse` can stay pure and testable.
async fn execute(command: Command) -> Result<()> {
    match command {
        Command::Capture { label } => crate::capture::capture(label).await,
        Command::Login { label } => crate::capture::login(label).await,
        Command::Run { verbose, managed } => {
            let verbosity = if verbose {
                Verbosity::Verbose
            } else {
                Verbosity::Quiet
            };
            run(verbosity, managed).await
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
            ConfigAction::Backups => config_backups(),
            ConfigAction::Restore { index } => config_restore(index).await,
        },
        Command::Status {
            json,
            no_color,
            verbose,
        } => status(json, no_color, verbose).await,
        Command::List => list().await,
        Command::Use {
            target,
            force,
            next,
        } => crate::use_account::use_account(target, force, next).await,
        Command::SetEnabled { target, enabled } => set_enabled(target, enabled).await,
        Command::Remove { target } => remove_account(target).await,
        Command::Poke { target } => crate::poke::poke(target).await,
        Command::Stats(args) => crate::stats::run(args).await,
        Command::Reliability(args) => crate::reliability::run(args),
        Command::Log(args) => crate::log::run(args),
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
    config <path|validate|show|backups|restore>  Config diagnostics and the backup ring: resolve the config.toml path, validate it, show the effective config (show --origin tags default vs from-file), list retained backups, or restore one
    status [--json] [--no-color] [-v|--verbose]  Show each account's usage + resets-in, and the next swap (-v adds each access token's expiry)
    list       List captured accounts
    use <account> [--force]  Switch the active account now (--force overrides the pre-swap gate)
    disable <account>    Park an account: keep it but take it out of the rotation
    enable <account>     Return a parked account to the rotation
    remove <account>     Delete an account: drop it from the rotation and erase its stash
    poke [<account>]     Run Claude Code once in an isolated config dir so it refreshes a parked account's credential (all near-expiry if omitted)
    stats [<account>...] [--period day|week|month|lifetime] [--since <when>] [--json]  Show usage over a period, offline (reads the sample store directly)
    reliability [--json]  Swap-out overshoot SLO readout, offline (reads the event log): swap-out session_pct P50/P95/P100 vs targets, time-blind, false-preempt proxy, 429 counts
    log [--since <duration>] [--event <name>] [--channel <c>] [--json] [-f|--follow]  Show the daemon's own log lines, offline (reads the log files directly) — the raw-lines counterpart to reliability; --follow keeps printing new lines as they arrive; --channel picks event (default), diag or all
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

This verb is also what recovers an account whose REFRESH-token deadline has lapsed — the
`lapsed` cell in `sessiometer status`'s EXPIRY column. That deadline is a fixed instant issued
at login and no refresh moves it, so once it passes the stored credential is past renewing and
a fresh login is what replaces it.
";

const RUN_USAGE: &str = "sessiometer run — run the foreground daemon (poll every account's usage and swap before exhaustion)

USAGE:
    sessiometer run [-v|--verbose] [--managed]

    -v, --verbose  emit per-tick run diagnostics on stderr. A launchd-managed daemon gets no
                   -v, so for THAT one set `verbose = true` under [tunables] in the config
                   (`sessiometer config path`) and restart it — effective at the next daemon
                   start, and readable with `sessiometer log --channel diag`. This flag is
                   unaffected by that knob, and wins over it.
        --managed  mark a launchd-invoked agent: on single-instance-lock contention exit 0
                   (stand down cleanly) instead of the exit-3 `already running` a bare `run`
                   returns, so the generated LaunchAgent's conditional KeepAlive does not
                   respawn it while another daemon holds the lock. Set by the generated plist;
                   not meant for interactive use.
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
with a clear message and performs no swap. This single-owner guard is a
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

const CONFIG_USAGE: &str = "sessiometer config — config diagnostics, and the config.toml backup ring

USAGE:
    sessiometer config <path|validate|show|backups|restore> [<index>] [--origin]

    path        print the resolved config.toml path (honours $XDG_CONFIG_HOME, else ~/Library/Application Support/sessiometer)
    validate    parse + validate config.toml WITHOUT running; report typo'd/unknown keys, out-of-range values, and target_max_session_usage > session_ceiling
    show        print the effective config (defaults filled in); with --origin, tag each value default (absent → compiled-in) vs from-file
    backups     list the retained config.toml backups, newest first, by timestamp and account count
    restore <N> replace config.toml with retained backup <N>, as numbered by `config backups`
    --origin    (with show) tag each value's provenance, so a silently-defaulted absent section is visible
    -h, --help  print this help

path, validate, show and backups are READ-ONLY: they never write config.toml, start/stop a daemon, or
change any state. `config show --origin` surfaces effective-vs-on-disk drift — e.g. a hand-deleted
[tunables] block shows every tunable as `default`, the very drift that once went unnoticed because the
effective config is only ever emitted once to stderr at start-up.

`config restore` is the one that writes. A config.toml holding accounts is copied into the ring before
any replacement of it, including this one, so a restore is itself reversible; a file that is absent,
unreadable, malformed or empty is neither copied nor allowed to displace what the ring already holds.
The ring keeps three, newest first.
";

const STATUS_USAGE: &str = "sessiometer status — show each account's usage + resets-in and the next swap (needs a running daemon)

USAGE:
    sessiometer status [--json] [--no-color] [-v|--verbose]

    --json         print the raw status response, uncoloured (for scripts)
    --no-color     force the urgency colour overlay off
    -v, --verbose  add each account's access-token expiry under the table
    -h, --help     print this help

The EXPIRY column carries each account's REFRESH-token deadline — the instant its stored
credential stops being renewable — as a compact time-until (`6d21h`), the word `lapsed` once
that instant has passed, or `—` when no deadline was observed. It is a cell of its own, never
folded into AUTH: the two axes are independent, so an account with no AUTH fault at all can
still sit days from its refresh-token deadline. The column is absent entirely when no account
has an observed deadline, and it is the first to go on a narrow terminal.

A time-until in BRACKETS (`[6d21h]`) marks a deadline falling inside the expiry horizon —
`expiry_horizon_secs` under [credential], seven days by default; an unbracketed one (`29d`)
falls outside it. The brackets state where the deadline sits relative to that window and
nothing further — the window's width is yours to set, so the bare duration alone cannot tell
you which side of it you are on. The colour overlay tints the same distinction where it is
available; the brackets are what carry it through --no-color, NO_COLOR, and a pipe into a
file. (They are literal brackets, so `grep -F` matches one as text — bare `grep` would read
it as a character class.)

Refreshing does NOT extend that deadline. The daemon keeps ACCESS tokens alive indefinitely —
that is the separate clock `-v` prints, and it does slide forward on every refresh — but the
refresh-token deadline is a fixed instant issued at login that no refresh moves. A credential
past it is replaced by `sessiometer login` — the only path that writes a fresh credential
rather than renewing the stored one — which lands it in the rotation without disturbing the
active session.

An `—` cell reads NOT OBSERVED, never \"not expiring\": no deadline was read for this account —
the daemon has not polled it yet, or it found none in the credential (an older Claude Code, a
changed upstream policy, a non-first-party credential) — so it reports that absence rather than
implying the account is exempt.
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
    sessiometer use --next [--force]

    <account>   the target account (its label or account-uuid)
    --next      advance to the next account in the swap chain without naming it —
                the daemon's own next-swap candidate (the one `sessiometer status`
                shows). Needs a running daemon, and cannot be combined with <account>.
    --force     override the pre-swap gate; also adopts the target when the active
                credential is gone/rotated (a forced logout), a locked keychain aside
    -h, --help  print this help
";

const DISABLE_USAGE: &str =
    "sessiometer disable — park an account: keep it but take it out of the rotation

USAGE:
    sessiometer disable <account>

    <account>   the account to park (its label or account-uuid)
    -h, --help  print this help
";

const ENABLE_USAGE: &str = "sessiometer enable — return a parked account to the rotation

USAGE:
    sessiometer enable <account>

    <account>   the parked account to re-enable (its label or account-uuid)
    -h, --help  print this help
";

const REMOVE_USAGE: &str =
    "sessiometer remove — delete an account: drop it from the rotation and erase its stash

USAGE:
    sessiometer remove <account>

    <account>   the account to delete (its label or account-uuid)
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

The `expiry` column carries the same REFRESH-token deadline `sessiometer status` reports — a
compact time-until, `lapsed` for one already past, or `—` for one never observed. It is
right-aligned and uncoloured here, since this surface's colour vocabulary is the neutral
utilisation band, and it goes first on a narrow terminal.

That column does not appear yet. `stats` reads offline, and the step that folds the daemon's
durable expiry log lines into one deadline per account is not built — so the overlay is empty
on every path and an empty column elides. Its absence HERE means that missing step, not an
account without an observed deadline. `sessiometer status` reports the deadline today.
";

const RELIABILITY_USAGE: &str = "sessiometer reliability — swap-out overshoot SLO readout, offline (reads the event log directly)

USAGE:
    sessiometer reliability [--since <duration>] [--json]

    --since <d> bound all four indicators to events at/after now - <duration>. <duration> is a
                non-negative integer with a unit: s, m, h, d, w (e.g. 30m, 24h, 7d, 2w). Omit for
                the whole-log aggregate (the default).
    --json      print the readout as JSON (schema:12, for scripts) instead of the text view
    -h, --help  print this help

READ-ONLY: it reads ~/Library/Logs/sessiometer/sessiometer.log and makes no live call, so it
works when the daemon is down. It reports four indicators, each with its target: swap-out
session_pct P50/P95/P100 (targets P50 <= 97, P100 < 99); time spent blind while near the limit; a
false-preempt proxy from the blind-window recovery reconciliation; and the usage-poll 429 vs
transient counts. By default the indicators fold the whole log; --since <duration> bounds them to a
recent window (the cutoff is documented in both output forms). The readout is roster-wide numbers
only — no per-account breakdown, no identifiers.
";

const LOG_USAGE: &str =
    "sessiometer log — show the daemon's event log, offline (reads the log file directly)

USAGE:
    sessiometer log [--since <duration>] [--event <name>] [--channel <c>] [--json] [-f|--follow]

    --since <d>    show only events at/after now - <duration>. <duration> is a non-negative
                   integer with a unit: s, m, h, d, w (e.g. 30m, 24h, 7d, 2w) — the same
                   grammar as `reliability --since`. Omit for the whole log (the default).
    --event <n>    show only lines whose kind token is EXACTLY <n> — `event=` on the event
                   channel (e.g. swap, restash, all_exhausted), `diag=` on the diagnostic one
                   (e.g. tick, poll, canonical). Omit for every kind.
    --channel <c>  which channel to read: event (the default), diag, or all. See CHANNELS below.
    --json         print the matched lines as JSON records (schema:2, for scripts) instead of
                   the text view
    -f, --follow   keep printing newly appended lines until interrupted (Ctrl-C)
    -h, --help     print this help

READ-ONLY: it reads ~/Library/Logs/sessiometer/sessiometer.log and makes no live call, so it
works when the daemon is down. This is the raw-lines counterpart to `reliability`, which reads
the same file but only to fold it into SLIs.

CHANNELS (--channel): the daemon writes two, and they are NOT the same kind of thing.

  event  (the default) ~/Library/Logs/sessiometer/sessiometer.log — the durable event log. Every
         field is a handle, an enum, a number or a timestamp by construction, and the whole
         channel is redaction-checked in CI.
  diag   ~/Library/Logs/sessiometer/daemon.err.log — a launchd-managed daemon's raw stderr, where
         the per-poll / per-tick / lifecycle diagnostics land. Being raw stderr it is NOT
         redaction-checked and can carry panic output, which is why it is strictly opt-in and
         never folded into the default view.
  all    both, interleaved in timestamp order. Each source keeps its own order internally, so a
         panic backtrace stays contiguous; ties put the event line first. A diagnostic line with
         no timestamp of its own (raw stderr, a panic payload) is placed at the timestamp of the
         nearest line before it, so it lands where it happened. Not available with --follow: a
         live merge would have to stall one stream waiting for the other.

TURNING DIAGNOSTICS ON for a background daemon: a launchd-managed daemon runs `run --managed`
with no -v, so by default it writes none. Set `verbose = true` under [tunables] in the config
(`sessiometer config path`) and restart it (`sessiometer daemon restart`) — no plist edit, which
`service install` would overwrite anyway. It takes effect at the NEXT daemon start, not live. An
interactive `sessiometer run` is unaffected by the knob; use -v there.

FOLLOWING (-f, --follow): the log is printed as usual, then newly appended lines are printed as
they arrive. The two filters do NOT behave the same way here, and the difference is deliberate:
--since bounds the initial catch-up only, because a line that arrives while you are watching is
recent by definition; --event keeps filtering every streamed line. If the log is truncated, or
rotated away and replaced, the follower says so on stderr and resumes from the new file's start
instead of stalling or reprinting what it already showed you. If the log does not exist yet, it
waits for the daemon to create it rather than exiting — a follow started before the daemon's
first write is a normal cold start. With --json the stream is JSON Lines (one complete record
per line, each carrying its own schema), NOT the single document the one-shot form prints: a
stream has no last record, so its array could never be closed.

The text view writes the matched lines to stdout VERBATIM and nothing else, so a piped
`sessiometer log` stays a clean line stream (`| grep`, `| wc -l` stay honest). The resolved
window, the active filter, the match count, and any empty result go to stderr instead — so an
empty stdout is never an ambiguous silence: it says whether there is no log file yet, an empty
one, or simply no matching event. An absent log is a normal cold state, not an error: the verb
says so and exits 0.

The log identifies accounts by the label the operator chose, written verbatim, which may be an
email address. The file is 0600 on disk; piping or pasting this output moves it somewhere that
is not, so treat it accordingly.
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
/// single-instance lock FIRST (a second `run` fails to take it and returns
/// without disturbing the first), then bind the control socket, then run.
///
/// The diagnostic channel's effective gate: the `-v` flag, OR — for a LAUNCHD-MANAGED daemon —
/// the `[tunables].verbose` knob (issue #775).
///
/// Split out as a pure function because it is the whole of the issue #775 wiring that a test can
/// actually reach: "a launchd-managed daemon writes diagnostics to its stderr file" needs real
/// launchd, but "`--managed` + the knob resolves to [`Verbosity::Verbose`], and every other
/// combination does not" is a total function over three booleans, pinned exhaustively below.
///
/// Two properties are deliberate, and each is an answer to a way this could have gone wrong:
///
/// - **The knob is `--managed`-scoped.** An interactive `sessiometer run` resolves from `-v`
///   alone, exactly as it always has. An operator who arms the knob for their background agent
///   has not also signed up for console spam the next time they run the daemon in a terminal to
///   watch it — and the issue's D2 asked for a MANAGED-daemon switch, not a global one.
/// - **`-v` still wins.** The two OR together rather than the knob overriding, so `run -v` is
///   verbose whatever the config says. A flag that a config file could silently veto would be a
///   worse surprise than the gap this closes.
fn effective_verbosity(flag: Verbosity, managed: bool, configured: bool) -> Verbosity {
    if flag == Verbosity::Verbose || (managed && configured) {
        Verbosity::Verbose
    } else {
        Verbosity::Quiet
    }
}

/// `verbosity` (issue #77) gates the operator-facing diagnostic channel: this
/// function owns the process lifecycle, so it brackets the loop with the
/// `diag=start` / `diag=stop` markers, and the per-tick diagnostics are emitted
/// inside [`run_loop`]. Default [`Verbosity::Quiet`] keeps `run` silent on that
/// channel; `-v`/`--verbose` opts in — as does `[tunables].verbose` for a MANAGED
/// daemon (issue #775), resolved by [`effective_verbosity`] below once the config
/// is in hand.
async fn run(verbosity: Verbosity, managed: bool) -> Result<()> {
    // The native-local support dir holds both the lock and the socket; ensure it
    // (0700) before either touches it.
    paths::ensure_private_dir(&paths::support_dir()?)?;

    // Single-instance lock FIRST: held for the process lifetime, released by the
    // kernel on exit (`_lock` drop). A second `run` cannot acquire it and exits
    // `3` (issue #7), without disturbing the running daemon.
    let _lock = match InstanceLock::acquire(&paths::daemon_lock()?) {
        Ok(lock) => lock,
        // A launchd-managed agent (`run --managed`) that loses the lock stands down
        // CLEANLY (issue #742): another daemon already owns the instance, the collision
        // is non-destructive (the loser never reached the socket or any state/keychain
        // write — the lock is acquired BEFORE all of that), and exiting `0` keeps the
        // generated plist's conditional `KeepAlive: {SuccessfulExit: false}` from
        // respawning it into a throttled ~10s loop that can never win while the holder
        // is alive. A human `sessiometer run` (unmanaged) still gets the exit-`3`
        // `AlreadyRunning` contract (issue #7), so an operator/supervisor can tell
        // "already running" apart from a generic failure.
        Err(Error::AlreadyRunning) if managed => {
            eprintln!(
                "sessiometer: another daemon already holds the single-instance lock; \
                 this managed agent is standing down (nothing was started)"
            );
            return Ok(());
        }
        Err(err) => return Err(err),
    };

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
    // Wire the prior-configuration witness (#1441, design D-1) at the real machine, so a socket
    // `capture` into an ABSENT `config.toml` is refused when durable local state says this machine
    // was configured before — the same rule the CLI's `capture` / `login` apply, at the entry point
    // the menu-bar capture button uses. INJECTED here for the same #315 reason as
    // `.with_usage_samples` below: the hermetic test harness never wires it, so a `FakeDaemon`
    // capture never spawns `security` against the developer's own login keychain. Construction is
    // pure path resolution and spawns nothing; `support_dir()` already gated startup via
    // `.with_swap_lock(paths::swap_lock()?)` above, so this `?` adds no new failure mode.
    .with_witness_sources(crate::witness::WitnessSources::real()?)
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
    // Wire the proactive fleet-runway warn probe (#650) to the real store-reading aggregate —
    // the SAME `NativeHistoryStore` → `build_report` → `fleet_runway` pipeline the `stats`
    // socket verb serves, pinned to the `week` window. INJECTED here (not resolved inside the
    // check) for the same #315 reason as `.with_usage_samples` above: the hermetic test harness
    // never wires it, so a `FakeDaemon` tick reads no real store — its tests inject a canned
    // closure instead. Inert regardless unless the operator opted in (`fleet_runway_warn_secs`).
    .with_fleet_runway_probe(Box::new(crate::stats::current_fleet_runway))
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
    // The refresh-token foresight horizon (#878): how far ahead each account's fixed
    // `refreshTokenExpiresAt` deadline is classified, so an operator can re-login BEFORE a lapse
    // instead of learning about it from a failed refresh. Wired unconditionally — NOT inside the
    // `[refresh].enabled` block below — because the deadline is read from the credential itself and
    // matters most precisely when the refresh tick is off.
    .with_credential_expiry_horizon(config.credential.expiry_horizon_secs)
    // The synchronized-cohort grouping window (#879): how close together several deadlines must
    // fall to count as one cohort, so the fleet-level "the pool loses N members at once" fact is
    // visible — the thing no single row can show, and the thing the upstream client (which warns
    // only for the ACTIVE account) structurally cannot see. Wired unconditionally beside the
    // horizon above, for the same reason.
    .with_credential_expiry_cohort_window(config.credential.expiry_cohort_window_secs)
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
    // verbosity selected from `-v`/`--verbose` — or, for a MANAGED daemon, from
    // `[tunables].verbose` (issue #775), which is why this is resolved HERE and not at
    // the `Command::Run` dispatch: the config is not loaded until a few lines above,
    // and re-loading it in the dispatch just to read one bool would give the process
    // two reads that could disagree. Default quiet — no console spam.
    //
    // The lifecycle markers bracket the loop HERE because `cli` owns the process
    // lifecycle: a clean shutdown through EITHER of `run_loop`'s exit paths (the
    // startup-delay or the idle loop) returns `Ok`, so a single `diag=stop` after it
    // covers both. The per-tick diagnostics are emitted inside `run_loop`. The Start
    // summary is the effective config, so one run's lines read against it.
    let verbosity = effective_verbosity(verbosity, managed, config.tunables.verbose);
    let mut diag = DiagnosticLog::new(std::io::stderr(), verbosity);
    diag.emit(&Diagnostic::Start {
        accounts: config.roster.len(),
        poll_secs: config.tunables.poll_secs,
        target_max_session_usage: config.tunables.target_max_session_usage,
        session_ceiling: config.tunables.session_ceiling,
        weekly_ceiling: config.tunables.weekly_ceiling,
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
    // The STARTUP PREFLIGHT (issue #787): probe the refresh mechanism's PRECONDITION exactly once,
    // here, so a fault that was already present survives the restart that erased the detector's
    // state. `SystemicRefreshHealth` is pure in-memory, so every restart reset an open episode and
    // the board went green over an unfixed fault for another `systemic_failure_n` sweeps — and the
    // bundled launchd job's `KeepAlive { SuccessfulExit: false }` re-opened that window on each
    // abnormal exit. Resolving once here re-establishes the episode immediately instead.
    //
    // A SIGNAL, NEVER A GATE: `preflight` returns a classification, not a `Result`, so an
    // unresolvable binary cannot `?` out of this function and the daemon still starts (the tick
    // then retries per cycle and self-heals, exactly as issue #375 requires). The resolved path is
    // DROPPED inside `preflight` and nothing here holds it — the engines above still resolve at
    // their own spawn sites, per cycle. Best-effort on the log for the same reason `run_loop` is:
    // a failed diagnostic write must not take the daemon down with it (issue #9).
    //
    // Gated on the mechanism being OBSERVABLE, because the asymmetry is in the CLEARING edge: only
    // a `SweepHealth::Working` releases the latch, so a config in which no sweep can ever run would
    // latch a mechanism-down signal nothing could clear — trading the false-GREEN this fixes for a
    // stuck false-RED, which is the same harm inverted. `refresh_tick::mechanism_is_observable`
    // owns that whole rule (the `[refresh].enabled` leg included), next to the sweep skip chain it
    // mirrors, so the three ways to be permanently blind stay one tested condition rather than a
    // call-site `&&` a later edit can half-delete.
    //
    // One bounded startup cost, deliberate: this awaits the shared resolver, whose tier-3 harvest
    // spawns the login shell under a 5 s timeout (#783/#784) — so under launchd's minimal `PATH`, a
    // `status` issued in the first seconds of a restart can wait on it. Accepted because it is
    // BOUNDED and the same shape as the `reap_orphans` / `reap_login_orphan` spawns already above.
    // Note it is NOT amortized by the harvest memo: that TTL equals the default `idle_after_secs`,
    // which is also the floor the first sweep waits, so at stock settings the memo has expired by
    // the time a cycle resolves and this genuinely is an extra harvest rather than a shared one.
    if refresh_tick::mechanism_is_observable(&config.roster, &config.refresh) {
        let preflight = systemic_refresh::preflight(|| {
            paths::claude_binary_with_override(config.refresh.claude_bin.as_deref())
        })
        .await;
        if let Some(event) = daemon.note_refresh_preflight(preflight) {
            emit_best_effort(&mut log, &event);
        }
    }

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
// One transient value per `status` invocation, immediately consumed — the payload/mismatch size
// gap is irrelevant here, and boxing would only add indirection to a render-once path.
#[allow(clippy::large_enum_variant)]
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
    /// launchd is supervising the daemon ⇒ `launchctl bootout` alone. Booting the agent out of the
    /// domain terminates the supervised daemon (SIGTERM ⇒ graceful exit) and removes its job, so
    /// nothing is left to relaunch it — the process it stops IS the running daemon.
    BootOut,
    /// The job is in the domain but launchd runs no process for it, so a foreground `run` may own
    /// the daemon ⇒ do BOTH. The bootout removes the idle registered agent from the domain; the
    /// socket shutdown stops the foreground `run` that actually holds the lock. Bootout alone
    /// leaves that daemon running; socket shutdown alone leaves the idle agent registered.
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
/// bootstrapping beside it only hands launchd a managed `run` that loses the lock and cleanly stands
/// down (exit `0`), never touching the foreground daemon the operator meant to restart. Only with
/// nothing running does registration mean "bring it up".
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
        // launchd supervises the daemon: booting the agent out of the domain IS the stop — it
        // terminates the daemon and removes its job, so nothing relaunches it.
        StopPlan::BootOut => crate::service::stop_managed().await,
        // The registered agent is idle while a foreground `run` may own the daemon. Bootout removes
        // the idle registered agent, then stop the running daemon over the socket. Neither half
        // alone is the whole story, so narrate the compound stop with one coherent message rather
        // than stacking two primitive ones.
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
/// ([`Error::ConfigInvalid`]), or `target_max_session_usage > session_ceiling`
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

/// `config backups` (issue #1439): list what the roster backup ring retains — the enumeration
/// half of R-9's operator-invocable path back from a lost roster.
///
/// Read-only. Reports a COUNT and a timestamp per entry and never an account label: the listing
/// is a more public surface than the file it describes (it is what an operator pastes into a bug
/// report), so it carries enough to choose between entries and nothing more (`docs/specs/roster-backup-qualifying-write.feature.md`, Rule 3).
fn config_backups() -> Result<()> {
    let path = paths::config_file()?;
    let retained = crate::roster_backup::list(&path)?;
    print!(
        "{}",
        render_config_backups(&crate::roster_backup::ring_dir(&path), &retained)
    );
    Ok(())
}

/// Render `config backups`' listing (issue #1439). Pure — no I/O — so the state→text mapping is
/// unit-tested without a real ring, matching [`render_config_validate`] / [`render_config_origin`].
///
/// An entry that no longer parses renders its count as `unreadable` rather than being hidden:
/// it is still restorable material an operator may want to look at, and hiding it would make
/// the numbering disagree with what `restore` accepts.
fn render_config_backups(dir: &Path, retained: &[crate::roster_backup::Retained]) -> String {
    if retained.is_empty() {
        return format!("no backups retained under {}\n", dir.display());
    }
    // The count is the ring's ACTUAL contents, with the depth named separately rather than as a
    // denominator: pruning is best-effort, so a transient failure can leave one entry over depth
    // and `4 of 3 retained` would read as a contradiction rather than as the recoverable state
    // it is (the next qualifying write prunes to depth again).
    let mut out = format!(
        "{} retained under {} (ring depth {})\n",
        retained.len(),
        dir.display(),
        crate::roster_backup::RING_DEPTH
    );
    for (ordinal, entry) in retained.iter().enumerate() {
        let when = crate::observability::rfc3339(entry.taken_at);
        let held = match entry.accounts {
            Some(1) => "1 account".to_string(),
            Some(count) => format!("{count} accounts"),
            None => "unreadable".to_string(),
        };
        out.push_str(&format!("  {}  {when}  {held}\n", ordinal + 1));
    }
    out
}

/// `config restore <N>` (issue #1439): replace `config.toml` with retained backup `N` — the
/// restore half of R-9, so a lost roster comes back without hand-editing TOML.
///
/// Three properties make this a roster write like any other rather than a privileged one.
/// It re-validates the chosen entry through [`Config::from_toml_str`] before writing anything,
/// so a backup that no longer parses is refused rather than installed. It writes through
/// [`Config::save_to`], which means the config it replaces goes into the ring first if it
/// qualifies — so a restore is itself undoable, which is what AC-5's "not silently over a
/// roster the operator has since changed" asks for. And it NAMES both sides before acting:
/// what is being installed, and what is being replaced.
///
/// It then notifies a running daemon exactly as the other roster-mutating verbs do
/// (`remove`, `enable` / `disable`), so a restored roster is live rather than pending a
/// restart. Best-effort, like theirs.
async fn config_restore(index: usize) -> Result<()> {
    let path = paths::config_file()?;
    let retained = crate::roster_backup::list(&path)?;
    let entry = retained.get(index - 1).ok_or(Error::BackupNotRetained {
        index,
        retained: retained.len(),
    })?;
    // Validate BEFORE naming what will happen, so a REFUSED ENTRY never trails a line that reads
    // as an announcement of a write that did not occur. The write itself can still fail after the
    // notice — a qualifying config that cannot be retained aborts it by design — and that error
    // follows immediately on stderr rather than being silent.
    let restored = Config::from_toml_str(&std::fs::read_to_string(&entry.path)?)?;
    // The config being replaced, described from the same seam the ring's own rule uses: a file
    // that will not load is reported as such rather than as zero accounts.
    let replaced = Config::load_path(&path).ok().map(|c| c.roster.len());
    print!(
        "{}",
        render_restore_notice(entry, restored.roster.len(), &path, replaced)
    );
    restored.save_to(&path)?;
    crate::capture::notify_daemon_roster_reload().await;
    Ok(())
}

/// Render `config restore`'s notice (issue #1439). Pure — no I/O — so the exact operator-facing
/// text is unit-tested without a real ring.
///
/// Four things, in the order they matter to someone who has just lost a roster. What is being
/// installed and what it displaces — the AC-5 property, so an operator who miscounted the index
/// sees both in the same breath. Then two disclosures that would otherwise be discoverable only
/// by reading the source:
///
/// - the installed file is RE-RENDERED from the retained entry, not copied from it, so anything
///   the current emitter does not write (a hand-added comment) does not come back. The retained
///   file is named so it can be copied verbatim instead;
/// - when the displaced config qualified it entered the ring, which makes the restore reversible
///   AND shifts every index the operator just read, since the listing is newest-first. Saying so
///   is what stops a second `restore` from silently targeting a different entry.
fn render_restore_notice(
    entry: &crate::roster_backup::Retained,
    accounts: usize,
    path: &Path,
    replaced: Option<usize>,
) -> String {
    let held = match replaced {
        Some(1) => "1 account".to_string(),
        Some(count) => format!("{count} accounts"),
        None => "no loadable config".to_string(),
    };
    let mut out = format!(
        "restoring the backup taken {} ({accounts} in its roster)\n\
         over {} ({held})\n",
        crate::observability::rfc3339(entry.taken_at),
        path.display()
    );
    out.push_str(&format!(
        "values are re-rendered; the retained file stays at {}\n",
        entry.path.display()
    ));
    // Exactly the ring's own predicate: a displaced config enters the ring iff it parses with a
    // non-empty roster, and only then does the numbering move.
    if replaced.is_some_and(|count| count > 0) {
        out.push_str(
            "the displaced config is retained, so backup numbering shifts — re-run \
             `config backups` before restoring again\n",
        );
    }
    out
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
/// REFRESH-token `EXPIRY` (issue #883), then the health-text tags (issue #94). A labelled header
/// row (issue #99) tops the table — `ACCOUNT`, the grouped `SESSION%` + `RESET`, the grouped
/// `WEEKLY%` + `RESET`, `EXPIRY`, then `AUTH` — measured into the SAME column widths as the data
/// so the labels line up; the pairing is also read by adjacency (each `%` sits immediately before
/// its OWN reset), so the two reset columns share the `RESET` label. A reset's lead gap is a
/// single space (tying it to its `%`); independent columns are two spaces apart. When
/// the full table is wider than `cols`, the lowest-priority columns drop — `EXPIRY` FIRST (the
/// slowest-moving axis, [`status_columns`]), then the WEEKLY pair (`weekly%` + `weekly-reset`)
/// ATOMICALLY (never a `%` stranded without its reset), then the health-text column, each taking
/// its own header label with it — never wrapping a row; `account` + the SESSION pair (the soonest,
/// most actionable reset) and their labels are always kept. A `None` width (piped /
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

    let mut columns = status_columns(&rows);
    fit_columns(&mut columns, &rows, cols);
    let mut out = render_table(&columns, &rows, color);

    out.push('\n');
    // Both the blind-active projection and the cornered verdict are resolved ONCE, HERE, because
    // three sections downstream read them: the cornered alarm, the per-account blind line, and the
    // next-swap footer's suppression. Both are `Copy`, so each stays usable after being passed on.
    let active_blind = active_blind_projection(response);
    let cornered = cornered_state(active_blind, response.next_swap.as_ref());

    if let Some(cornered) = cornered {
        out.push_str(&render_cornered(cornered, now, color));
    } else if let Some((label, blind)) = active_blind {
        out.push_str(&render_blind_active(label, blind, color));
    }
    out.push_str(&render_blind_preempt_swap(response));
    // The next-swap footer is SUPPRESSED when cornered (issue #479): the cornered alarm above
    // already folded in this exact `no_viable_target` relief, so re-printing `next swap: none — …`
    // would be redundant (cornered fires only on that arm).
    if cornered.is_none() {
        out.push_str(&render_next_swap(response.next_swap.as_ref(), now));
    }
    // Worst-first (ADR-0026), so the CLI's print order agrees with its colour rank AND with the
    // menubar panel's `daemonFaultBanner`: keychain unreadable (rank 1, act-now `Red`), then canonical
    // scrubbed-`exhausted` (rank 2, act-now `Red`), then the canary REFUSAL TRIO (drift-refusing /
    // ambiguous / #738 unparseable-canonical, act-now `Red` — credential writes are blocked), then
    // the landing-overshoot SLO breach
    // (#613, `Red`), then the pre-death systemic mechanism-down (rank 3, `Yellow`,
    // act-at-your-next-break), then the OVERRIDDEN canary drift (`Yellow` — writes proceed under a
    // standing operator override; an acknowledged alarm ranks below the unacknowledged systemic
    // warning), and LAST the calm canonical `recovering` (rank 4, plain — may self-heal).
    // `canonical_scrub` is exhausted-XOR-recovering and the canary verdict refusing-XOR-overridden, so
    // each variant emits at its OWN rank: the guards mirror the panel resolver's `if case .exhausted`
    // at rank 2 vs its fall-through at rank 4 — severity ranks by (fault, VARIANT), never fault
    // identity (the panel's load-bearing invariant, applied to print order too, so a `recovering`
    // scrub co-occurring with a down refresh mechanism can't sit its "no action needed" ABOVE the
    // `Yellow` warning). Before #575 `systemic` sat first AND wore the only red, ranking the
    // least-blocking fault the loudest.
    out.push_str(&render_keychain_locked(response, color));
    if matches!(response.canonical_scrub, Some(CanonicalScrub::Exhausted)) {
        out.push_str(&render_canonical_scrub(response, color));
    }
    // The act-now canary band. #738 makes it a TRIO: the unparseable-canonical refusal blocks
    // credential writes exactly as the #714 pair does, so it prints here and not below the
    // `Yellow` band. It has no overridden VARIANT to split off — the override collapses the wire
    // verdict back to the quiet `inconclusive`, which reaches no arm at all.
    if matches!(
        response.canary,
        Some(CanaryStatus::Drift {
            overridden: false,
            ..
        }) | Some(CanaryStatus::Ambiguous { .. })
            | Some(CanaryStatus::RefusedUnparseableCanonical)
    ) {
        out.push_str(&render_canary(response, color));
    }
    out.push_str(&render_landing_overshoot(response, color));
    out.push_str(&render_systemic_refresh_failure(response, color));
    if matches!(
        response.canary,
        Some(CanaryStatus::Drift {
            overridden: true,
            ..
        })
    ) {
        out.push_str(&render_canary(response, color));
    }
    // The synchronized-expiry cohort (#879) prints BELOW every band above, and the placement is a
    // claim about kind rather than about loudness: each of those reports something ALREADY WRONG
    // (credential writes refused, an identity drifting under an override, the mechanism down, a
    // breach already taken), while a cohort is forward-looking and nothing has broken yet — the
    // pool is intact today. It carries its own tint, tracking the SOONEST member's cell, rather
    // than joining the ADR-0026 fault rank; see `render_expiry_cohort` for why it is deliberately
    // not a `DaemonPayloadFault`.
    out.push_str(&render_expiry_cohort(response, now, color));
    if matches!(response.canonical_scrub, Some(CanonicalScrub::Recovering)) {
        out.push_str(&render_canonical_scrub(response, color));
    }
    out.push_str(&render_refresh_disabled_advisory(response, color));
    out
}

/// The `status` table's column set, in display order (issue #94): `account`, then the SESSION
/// pair (% + its reset), then the WEEKLY pair, then the REFRESH-token `EXPIRY` (issue #883), then
/// the health-text tags. Each column carries a lead gap (the spaces BEFORE it): `0` for the first
/// column, `1` to tie a reset tightly to the `%` it pairs with, `2` between independent columns —
/// so each `%` reads immediately followed by its own reset, the pairing the header row (issue #99)
/// also labels.
///
/// A drop priority of `None` always keeps the column; otherwise the LOWEST priority sheds first.
/// The order is `EXPIRY` (1) → the WEEKLY pair (2, shared so both leave atomically — never a `%`
/// without its reset) → the health-text `AUTH` column (3). `EXPIRY` sheds FIRST because it is the
/// SLOWEST-MOVING axis on the row: a server-issued deadline measured in days that no tick can
/// move, so a narrow terminal loses the least by deferring it — every other column reports a fact
/// that can flip inside the current session.
///
/// Two columns are conditionally present, each on the same empty-column rule: the health-text
/// column only when some account carries a tag (an all-healthy roster shows none), and `EXPIRY`
/// only when some account has an OBSERVED deadline. A roster whose credentials carry no
/// `refreshTokenExpiresAt` — every cell [`EXPIRY_GAP`] — therefore renders exactly as it did
/// before issue #883, rather than growing a column of em dashes.
fn status_columns(rows: &[StatusRow]) -> Vec<Column> {
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
            2,
            |row| &row.weekly,
            |row| row.weekly_severity,
            STATUS_COL_GAP,
        ),
        Column::droppable(
            "RESET",
            2,
            |row| &row.weekly_reset,
            |row| row.weekly_reset_severity,
            STATUS_PAIR_GAP,
        ),
    ];
    if rows.iter().any(|row| row.expiry != EXPIRY_GAP) {
        // The REFRESH-token expiry modifier (issue #883) — a cell of its OWN, deliberately NOT
        // folded into `AUTH`: the two axes are orthogonal (issue #878), so an account can be
        // 🟢 healthy AND days from needing a re-login at the same time. Its cells arrive already
        // carrying the within-horizon mark (issue #934), so the widths measured below size the
        // MARKED cell and `expiry_severity`'s tint stays purely additive over it. Placed BEFORE
        // `AUTH` so that column's ragged free-text cue (`claude /login`, `degraded — run
        // 'sessiometer poke'`) stays last on the line, where its variable length costs nothing.
        columns.push(Column::droppable(
            "EXPIRY",
            1,
            |row| &row.expiry,
            |row| row.expiry_severity,
            STATUS_COL_GAP,
        ));
    }
    if rows.iter().any(|row| !row.status.is_empty()) {
        // The AUTH column carries the credential-auth state — the 5-state+Unknown glyph
        // (issue #119/#137) plus its cues (`claude /login` on 🔴, `recovering`, `disabled`);
        // it is never tinted (issue #84) — the glyph is self-coloring and the tags are their
        // own signal, so its severity getter is always `None`. Its header is `AUTH` (issue
        // #143, renamed from the over-general `HEALTH` of issue #99 — this column reports
        // the credential-AUTH standing, while rate-limit health lives in `SESSION%`/`WEEKLY%`,
        // and the credential's forward-looking DEADLINE in `EXPIRY`).
        columns.push(Column::droppable(
            "AUTH",
            3,
            |row| &row.status,
            |_| None,
            STATUS_COL_GAP,
        ));
    }
    columns
}

/// Drop the lowest-priority droppable columns until the table fits `cols`. ALL columns sharing
/// the lowest present priority drop together, so the WEEKLY pair (both priority 2) leaves
/// atomically — never a weekly `%` stranded without its reset. A non-TTY width (`None`) never
/// enters the loop — the full table is kept.
fn fit_columns(columns: &mut Vec<Column>, rows: &[StatusRow], cols: Option<usize>) {
    while let Some(width) = cols {
        if table_width(columns, rows) <= width {
            break;
        }
        match columns.iter().filter_map(|col| col.drop_priority).min() {
            Some(min_priority) => columns.retain(|col| col.drop_priority != Some(min_priority)),
            // Only keep-columns remain: never wrap, just let the essential columns
            // overflow a very narrow terminal (predictable, one record per line).
            None => break,
        }
    }
}

/// The aligned account table: a header row (issue #99) followed by one line per account.
///
/// The header is a plain, uncolored label per column, padded to the SAME measured widths as the
/// data so labels and values line up. Printed in the text view regardless of the colour gate or
/// TTY (it is never in `--json`, the separate full-data contract). Skipped only for an empty
/// roster — a lone header labelling no data would mislead. Whichever columns survived the
/// narrow-terminal drop carry their labels with them, so a dropped WEEKLY pair takes its
/// `WEEKLY%`/`RESET` labels too while `ACCOUNT` + the always-kept SESSION pair keep theirs.
fn render_table(columns: &[Column], rows: &[StatusRow], color: bool) -> String {
    let widths = column_widths(columns, rows);
    let lead_gaps: Vec<usize> = columns.iter().map(|col| col.lead_gap).collect();
    let mut out = String::new();
    if !rows.is_empty() {
        let headers: Vec<&str> = columns.iter().map(|col| col.header).collect();
        let uncolored: Vec<Option<&str>> = vec![None; columns.len()];
        out.push_str(&render_cells(&headers, &widths, &uncolored, &lead_gaps));
    }
    for row in rows {
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
    out
}

/// The blind ACTIVE account's retained-anchor projection (issue #479, umbrella #363 Path B), or
/// `None` when the active account is not in bounded blindness. `blind_active` is set only on the
/// active account and only while blind (a pre-#479 daemon omits it → `None`).
fn active_blind_projection(response: &StatusResponse) -> Option<(&str, BlindActive)> {
    response
        .accounts
        .iter()
        .find(|account| account.active)
        .and_then(|account| {
            account
                .blind_active
                .map(|blind| (account.label.as_str(), blind))
        })
}

/// The resolved CORNERED state: the blind active account's label and projection, plus the
/// `no_viable_target` cause and reset instant FOLDED IN from `next_swap` so the cornered alarm can
/// carry the relief the suppressed next-swap footer would otherwise have printed.
type CorneredState<'a> = (&'a str, BlindActive, Option<NoTargetCause>, Option<i64>);

/// CORNERED (issue #479, surface 3): the active account is blind, ADR-0017 auto-protection is
/// DEGRADED (the preemptive gate is armed but acting on a STALE anchor), AND there is no viable
/// target to swap to — the one bounded-blindness state the daemon CANNOT resolve itself, so the
/// operator must act. Keying off `auto_protection_degraded` (blind PAST the interim gate window,
/// anchor at/over the risk band) rather than the raw last-known % is deliberate: before the gate
/// window the daemon is still self-resolving by waiting out a transient blind blip, so a loudest
/// alarm THEN would cry wolf — DEGRADED is exactly "auto-protection WOULD swap now but can't".
/// Composes two daemon verdicts (`blind_active` + `next_swap == no_viable_target`) already on the
/// wire, so it needs no new field.
fn cornered_state<'a>(
    active_blind: Option<(&'a str, BlindActive)>,
    next_swap: Option<&NextSwap>,
) -> Option<CorneredState<'a>> {
    match (active_blind, next_swap) {
        (Some((label, blind)), Some(NextSwap::NoViableTarget { cause, resets_at }))
            if blind.auto_protection_degraded =>
        {
            Some((label, blind, *cause, *resets_at))
        }
        _ => None,
    }
}

/// A daemon-fault line: `body` as DATA — printed UNCONDITIONALLY so it survives a pipe / redirect /
/// `status | grep`, an operator's health check must be able to see it — plus its newline, tinted at
/// `severity`'s SGR band only when the `color` gate is open. The overlay is the SAME
/// `\x1b[{code}m…\x1b[0m` SGR [`render_cells`] wraps a table cell in, so a fault line tints exactly
/// like the util cells it sits beneath — colour on this surface is one severity vocabulary, not a
/// per-line register (ADR-0026). The plain text carries the whole message on its own, so a
/// `--no-color` / piped reader loses nothing — the colour only ever AUGMENTS.
fn severity_line(body: &str, severity: Severity, color: bool) -> String {
    if color {
        format!("\x1b[{}m{body}\x1b[0m\n", severity.sgr())
    } else {
        format!("{body}\n")
    }
}

/// A `Severity::Red` fault line — the loudest runtime DATA faults: the per-account cornered `⊘`
/// state, a blind-DEGRADED active, a landing-overshoot SLO breach. A thin wrapper over
/// [`severity_line`] so those callers keep their one-word intent; `emphasize` is the colour gate.
fn red_line(body: &str, emphasize: bool) -> String {
    severity_line(body, Severity::Red, emphasize)
}

/// Declare `DaemonPayloadFault` and its `ALL` rank list from ONE set of tokens (issue #919).
///
/// `ALL` is the single home of the cross-surface severity RANK (ADR-0026): the parity module's
/// `manifest_from_source` builds `daemon_fault_ranks` and `arbitration_edges` from it,
/// `build/fixtures/cross-surface-severity.json` is emitted from that, and the panel's
/// `CrossSurfaceSeverityParityTests` walks whatever the manifest holds. While that list was a
/// hand-written literal, the enum and the list were two places that had to agree and nothing made
/// them: a new variant is a compile error in every total MATCH, but never in a list literal. A
/// ninth fault added to the enum and to all three matches and NOT to the list was therefore
/// invisible — unranked, with every gate green over the same 8-fault manifest, which is the issue
/// #575 mechanism ADR-0026 exists to close.
///
/// Rust cannot enumerate a variant set on its own and the derive that would is a dependency this
/// crate does not carry, so the variants are still spelled by hand — but only ONCE, with the list
/// expanded from that spelling. Attributes ride through verbatim in both positions, which is what
/// keeps each variant's #378/#469/#498/#714/#730/#738 provenance and reading-order rationale
/// attached to the variant it explains, and keeps `ALL`'s own `#[cfg(test)]` at the declaration
/// rather than buried in this expansion.
///
/// Deliberately NOT general — the one caller is directly below. `cross_surface_id` and `severity`
/// stay outside it on purpose: they are total matches, which the compiler already makes
/// exhaustive, and ordinary source is what `cargo fmt` formats.
///
/// The invocation below is PAREN-delimited and its marker is `const ALL: _;` rather than
/// `const ALL;`. Both are load-bearing for `cargo fmt`, and neither works alone (issue #1271).
/// `rustfmt` leaves a BRACE-delimited invocation body verbatim unconditionally — even a body that
/// parses cleanly — while a paren- or bracket-delimited one IS formatted, provided the body parses
/// as Rust. `const ALL;` does not parse; `const ALL: _;` does, and the `_` is honest: the macro is
/// what supplies the type. So `! { … }` with either marker is unformatted, and `! ( … )` with a
/// bare `const ALL;` is unformatted too.
///
/// What the body needs is to PARSE, not that exact spelling — among the `const ALL…` spellings an
/// ascription is necessary for that and not sufficient. `const ALL: u8;` parses too, and leaves a
/// mis-indented variant reddening `cargo fmt` just the same, while a TRUNCATED `const ALL: _` keeps
/// the ascription, stops parsing, and un-gates (measured — issues #1293, #1310, #1329). `: _` is
/// the house spelling, and the test named below pins it EXACTLY, so re-spelling this one marker is
/// a deliberate re-blessing rather than drift — a pin deliberately tighter than the property, which
/// is why a red over there is not by itself proof this region went un-gated.
///
/// Do not tidy any of these back. Restoring braces, dropping the ascription, or deleting the
/// marker outright — which strands the `#[cfg(test)]` and doc block above it, so the body stops
/// parsing the same way (measured, issue #1310) — each un-gates the ~60 lines below as far as
/// `cargo fmt --all --check` is concerned: the repo's first and cheapest push gate passes a
/// region it does not read, so the pass looks identical either way. What reds instead is
/// `the_daemon_payload_faults_invocation_stays_reachable_by_cargo_fmt` in this file's own test
/// module, which asserts BOTH conditions against this source (issue #1283) — before that landed,
/// nothing anywhere reported any of them. To confirm the FORMATTING is still armed rather than
/// merely that lint, mis-indent a variant and check `cargo fmt --all --check` FAILS; a pass over
/// an already-clean body proves nothing. This rests on `rustfmt`'s current macro handling, which
/// no gate here pins — if that changes, the region goes quietly unguarded again.
macro_rules! daemon_payload_faults {
    (
        $(#[$enum_meta:meta])*
        enum $fault:ident {
            $(
                $(#[$variant_meta:meta])*
                $variant:ident,
            )+
        }

        $(#[$all_meta:meta])*
        const ALL: _;
    ) => {
        $(#[$enum_meta])*
        enum $fault {
            $(
                $(#[$variant_meta])*
                $variant,
            )+
        }

        impl $fault {
            $(#[$all_meta])*
            const ALL: &'static [Self] = &[$(Self::$variant,)+];
        }
    };
}

daemon_payload_faults!(
    /// The daemon-level payload faults (ADR-0026): faults that ride ALONGSIDE a healthy roster,
    /// which no per-account `AUTH` cell reflects — the shared vault is one item and the refresh
    /// mechanism is one process, so neither has a row to live on. Enumerated ONCE here so their
    /// cross-surface severity RANK has a single home; #575 caught the CLI and the menubar panel
    /// ranking them in OPPOSITE order precisely because each render site re-derived the rank
    /// independently.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum DaemonPayloadFault {
        /// #498 — the login keychain is LOCKED, the shared credential is UNREADABLE.
        KeychainLocked,
        /// #469 — the shared credential is readable but EMPTIED and auto-recovery gave up.
        CanonicalScrubExhausted,
        /// #714 — the behavioral canary found identity DRIFT (the resolved credential
        /// byte-matches a DIFFERENT account's stash than the displayed active) and the
        /// override is off: credential writes are REFUSED until cleared.
        CanaryDriftRefusing,
        /// #714 — the canary's fresh resolution probe found MORE THAN ONE matching
        /// keychain item: no unique write target, credential writes REFUSED (no override).
        CanaryAmbiguous,
        /// #730/#738 — the resolved canonical matches NO stash AND parses as no Claude Code
        /// credential, so it is overwhelmingly an UNRELATED secret: credential writes are
        /// REFUSED rather than clobber it. Act-now, like its two siblings above.
        ///
        /// Declared last of the three purely as a CONVENTION: the canary reports ONE verdict
        /// at a time, so no snapshot can hold this and a drift together and nothing here ever
        /// arbitrates between them. What the position does buy is a stable reading order for
        /// the band — the two above are POSITIVE identity failures (a known-wrong match, a
        /// broken uniqueness rule) while this is the precautionary "unrecognized item, refuse
        /// to overwrite" case. Only the cross-FAULT edges are real arbitration (the vault pair
        /// above, systemic below), and those are what the rank tests exercise.
        CanaryRefusedUnparseableCanonical,
        /// #378 — the refresh MECHANISM is down (N consecutive all-error sweeps).
        SystemicRefreshFailure,
        /// #714 — identity DRIFT stands but `canary_drift_override` lets writes proceed
        /// (each logged): a standing, operator-acknowledged alarm, not a block.
        CanaryDriftOverridden,
        /// #469 — scrubbed, but the daemon is adopting a live account back; may self-heal.
        CanonicalScrubRecovering,
    }

    /// Every fault, in DECLARATION order — which IS the canonical worst-first rank (ADR-0026),
    /// the same order [`render_status`] prints them in and the same order the panel's
    /// `daemonFaultBanner` arbitrates in.
    ///
    /// GENERATED from the variant list above, by the `daemon_payload_faults!` invocation this
    /// declaration sits inside (issue #919), so the enum and the rank are ONE declaration: a
    /// variant cannot reach one without reaching the other. That is the property the hand-written
    /// literal could not have, and its absence was the defect — a ninth fault added to the enum
    /// and to every match but not to the list went unranked while all three gates read the same
    /// 8-fault manifest and reported green.
    ///
    /// The compiler still does the rest: a new variant is an error in three TOTAL matches
    /// ([`Self::severity`], [`Self::cross_surface_id`], and the one in
    /// `cross_surface_rank_is_the_declaration_order`), so its author is still routed to state the
    /// fault's band on every surface. What changed is that forgetting the LIST is no longer
    /// expressible.
    ///
    /// Test-only: the shipping binary reaches each fault through its own renderer and never needs
    /// the list.
    #[cfg(test)]
    const ALL: _;
);

impl DaemonPayloadFault {
    /// This fault's stable CROSS-SURFACE identifier (issue #768) — the name the committed
    /// `build/fixtures/cross-surface-severity.json` manifest and the menubar panel's parity gate
    /// both key on. Deliberately NOT a wire field: the wire carries `keychain_locked`,
    /// `canonical_scrub` and `canary` as three separate shapes, while the rank is over the
    /// (fault, VARIANT) pairs — the distinction ADR-0026 and the panel both insist on, since a
    /// `recovering` scrub and an `exhausted` one are not one severity.
    #[cfg(test)]
    fn cross_surface_id(self) -> &'static str {
        match self {
            Self::KeychainLocked => "keychain_locked",
            Self::CanonicalScrubExhausted => "canonical_scrub_exhausted",
            Self::CanaryDriftRefusing => "canary_drift_refusing",
            Self::CanaryAmbiguous => "canary_ambiguous",
            Self::CanaryRefusedUnparseableCanonical => "canary_refused_unparseable_canonical",
            Self::SystemicRefreshFailure => "systemic_refresh_failure",
            Self::CanaryDriftOverridden => "canary_drift_overridden",
            Self::CanonicalScrubRecovering => "canonical_scrub_recovering",
        }
    }

    /// The canonical cross-surface severity RANK of this fault (ADR-0026), in the CLI's own
    /// [`Severity`] vocabulary. It MUST agree with the menubar panel's rank
    /// (`StatusPanelFormat.daemonFaultBanner`'s `.error`/`.warning`/`.info`), though each surface
    /// renders it in its own medium — an SGR line vs a banner tint (R-2 is rank-parity, not
    /// glyph-parity). The vault pair blocks the operator NOW ⇒ `Red` (act now); a down refresh
    /// mechanism is pre-death, every account still alive ⇒ `Yellow` (act at your next break); a
    /// recovering scrub may self-heal ⇒ `None` (calm, uncoloured — colouring it would cry wolf).
    /// Red here is a SEVERITY band, not a line-register: it is the same `Severity::Red` the util
    /// cells and the cornered `⊘` state use, so systemic's pre-#575 red (over the plain vault pair)
    /// contradicted the CLI's own vocabulary — the inversion #575 fixes.
    fn severity(self) -> Option<Severity> {
        match self {
            // The canary REFUSAL TRIO joins the act-now band: credential writes are
            // blocked NOW (auto-protection cannot swap), the same operator urgency as the
            // vault pair. An OVERRIDDEN drift is next-break `Yellow` — writes proceed, but a
            // standing identity alarm rides an operator override that deserves re-checking.
            // The #738 unparseable-canonical refusal is `Red` for the SAME reason its two
            // #714 siblings are — it blocks writes now — and it has no override VARIANT to
            // rank apart: the override collapses the wire verdict back to the quiet
            // `inconclusive` (`canary_status_of`), so no fault reaches here at all.
            Self::KeychainLocked
            | Self::CanonicalScrubExhausted
            | Self::CanaryDriftRefusing
            | Self::CanaryAmbiguous
            | Self::CanaryRefusedUnparseableCanonical => Some(Severity::Red),
            Self::SystemicRefreshFailure | Self::CanaryDriftOverridden => Some(Severity::Yellow),
            Self::CanonicalScrubRecovering => None,
        }
    }
}

/// Render a daemon-payload-fault line at its canonical rank colour (ADR-0026): the fault's
/// [`DaemonPayloadFault::severity`] band as an SGR overlay when the `color` gate is open, or plain
/// when the fault is calm (`recovering`) or the gate is closed. Folds the fault sites into one
/// call so they render identically. The plain text always carries the whole message (colour only
/// augments), so a `--no-color` / piped reader loses nothing.
fn daemon_fault_line(body: &str, fault: DaemonPayloadFault, color: bool) -> String {
    match fault.severity() {
        Some(severity) => severity_line(body, severity, color),
        None => format!("{body}\n"),
    }
}

/// The loudest, distinct state: blind + DEGRADED + nowhere to swap. Names the source, how long
/// blind, the stale last-known %, THAT the fleet is out of capacity and WHEN it returns (the relief
/// reset FOLDED IN from `next_swap`, so the relief instant is not lost when this alarm replaces
/// that footer), and the ONE remedy
/// only the operator can apply — add or free an account. Printed as DATA (unconditional, survives a
/// pipe / redirect), red-emphasized when the color gate is open (the SAME SGR the DEGRADED /
/// systemic lines use); the plain text conveys it under `--no-color`. The surface only REFLECTS
/// this daemon-pushed state; it never self-swaps (issue #169).
fn render_cornered(cornered: CorneredState<'_>, now: i64, color: bool) -> String {
    let (label, blind, cause, resets_at) = cornered;
    let dur = humanize_until(blind.blind_secs as i64);
    let last_known = blind.last_known_session_pct;
    let relief = match resets_at {
        Some(at) => format!(", resets in {}", humanize_until(at - now)),
        None => String::new(),
    };
    // The CORNERED alarm appends "add or free an account" UNCONDITIONALLY below (blind + DEGRADED +
    // no target is unresolvable regardless of wait length), so — unlike the ordinary footer — the
    // remedy is not wait-gated here; only the pre-#666 false universal is dropped. `cause` names the
    // gating dimension of the SOONEST-returning spare (issue #665), NOT a fleet-wide property, so on a
    // mixed fleet "every account is weekly-exhausted / over its session limit" was literally
    // inaccurate — state only what the wire substantiates: out of capacity, and when it returns.
    let blocked = match cause {
        Some(_) => format!("out of capacity{relief}"),
        None => "no viable target".to_owned(),
    };
    let body = format!(
        "CORNERED: active {label} blind for {dur} at last-known session {last_known}% and \
         auto-protection cannot act — {blocked}; add or free an account"
    );
    red_line(&body, color)
}

/// The normal per-account blind-active line (issue #479 surface 1, shipped in #496), rendered when
/// the active account is blind but NOT cornered: narrate the REAL state — how long blind,
/// last-known session %, and whether ADR-0017 auto-protection is OK or DEGRADED — instead of the
/// content-free `n/a … 🟡`. Printed as DATA (unconditional), like the systemic-refresh line; only
/// the DEGRADED emphasis is color-gated. DEGRADED is a fault: the gate is armed but acting on a
/// STALE anchor.
fn render_blind_active(label: &str, blind: BlindActive, color: bool) -> String {
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
    red_line(&body, color && blind.auto_protection_degraded)
}

/// The #452 preemptive-swap NARRATION (issue #479, surface 2): when the daemon swapped a BLIND
/// active account away on its stale pre-blind anchor, `status` narrates it — the source, the
/// last-known % the gate FIRED on, the target, and the `use <from>` undo — so an operator can
/// reverse it if the swapped-away account has since recovered. The SAME information the durable
/// `event=swap … reason=blind_preempt` log line holds, reflected HERE because `render_status`
/// reads only this wire, never the event log — each medium in its own idiom (R-2 STATE-parity, as
/// the `canonical_scrub` footer is). Daemon-side windowed + target-still-active
/// (`recent_blind_preempt_swap`, projected only within a bounded window while its target is still
/// active), so this stays a pure render — the surface REFLECTS, never self-swaps (issue #169).
/// Empty (no line) when there is no recent-and-still-current preemptive swap.
fn render_blind_preempt_swap(response: &StatusResponse) -> String {
    let Some(swap) = &response.recent_blind_preempt_swap else {
        return String::new();
    };
    let from = &swap.from_label;
    let to = &swap.to_label;
    let pct = swap.last_known_session_pct;
    format!(
        "swapped off {from} (blind @ last-known {pct}%) → {to}; \
         undo with 'use {from}' if it recovered\n"
    )
}

/// The wait beyond which [`render_next_swap`]'s all-exhausted footer nudges "add an account" — ONE
/// session window (issue #666). Capacity returning within a session window is a TRANSIENT block the
/// operator waits out; a longer — or unknown-duration — wait is a STRUCTURAL shortage where adding
/// capacity is the real remedy. This replaces the pre-#666 [`NoTargetCause`]-label proxy (`Weekly`
/// ⇒ nudge, `Session` ⇒ silent), which mis-fired on a MIXED fleet where a `Weekly` cause can name a
/// sub-hour weekly reset (issue #665): the label was a broken stand-in for the wait it now keys off
/// directly. Keep in lockstep with the menubar twin `StatusPanelFormat.addAccountNudgeWaitSecs`
/// (`apps/menubar/Sources/StatusPanelFormat.swift`) — both clients must render the SAME nudge
/// decision (R-2 STATE-parity).
const ADD_ACCOUNT_NUDGE_WAIT_SECS: i64 = 5 * 60 * 60;

/// The forward-looking next-swap candidate (issue #88), computed daemon-side
/// ([`crate::daemon::NextSwap`]); printed plain — the footer carries no color, like the table footer
/// it replaces (per-cell health coloring is issue #84, orthogonal). A `None` field means the daemon
/// sent no candidate — either a current daemon with no active account to anchor a swap from, or
/// (via `#[serde(default)]`) a pre-#88 daemon that omits the field — and renders a bare `none`
/// either way.
fn render_next_swap(next_swap: Option<&NextSwap>, now: i64) -> String {
    match next_swap {
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
            format!("next swap: {to}{why}\n")
        }
        // When the daemon carries the fleet-capacity relief hint, tell a stranded operator (a DEAD
        // active whose 🔴 row sits above this, AND every spare exhausted) that the fleet is OUT OF
        // CAPACITY and WHEN it returns — not a content-free "no viable target". The phrase itself
        // is composed by [`out_of_capacity_phrase`], shared with `use --next`'s refusal (issue
        // #960) so the two surfaces cannot drift; only the pre-#405 `None`-cause fallback (no hint
        // on the wire ⇒ say only what it substantiates) is decided here.
        Some(NextSwap::NoViableTarget { cause, resets_at }) => match cause {
            None => "next swap: none (no viable target)\n".to_owned(),
            Some(_) => format!(
                "next swap: none — {}\n",
                out_of_capacity_phrase(*resets_at, now)
            ),
        },
        Some(NextSwap::AwaitingData) => "next swap: none (awaiting usage data)\n".to_owned(),
        None => "next swap: none\n".to_owned(),
    }
}

/// The fleet-capacity RELIEF phrase (issue #405): WHEN capacity returns, plus the "add an
/// account" nudge when the wait is STRUCTURAL rather than transient. Rendered WITHOUT the
/// pre-#666 false universal — on a MIXED fleet the daemon's `cause` names the gating dimension of
/// the SOONEST-returning spare (issue #665), NOT a fleet-wide property, so "every account is
/// weekly-exhausted" was literally inaccurate (some spares are blocked the other way). Say only
/// what the wire substantiates; that is why `cause` is render-irrelevant here and only its
/// presence (a pre-#405 daemon sends none) is consulted by the callers.
///
/// Shared by BOTH surfaces that report a blocked fleet: [`render_next_swap`]'s `status` footer and
/// — since issue #960 — `use --next`'s [`Error::UseNextNoViableTarget`] refusal. ONE composer, so
/// the two can never drift on the relief instant or on the nudge threshold, the same R-2
/// STATE-parity discipline that keeps [`ADD_ACCOUNT_NUDGE_WAIT_SECS`] in lockstep with its menubar
/// twin. Pure over (`resets_at`, `now`), so it is unit-tested without a clock, and secret-free —
/// a duration and fixed prose, never a label (issue #15).
pub(crate) fn out_of_capacity_phrase(resets_at: Option<i64>, now: i64) -> String {
    // `resets_at` humanizes with the same `humanize_until` the per-account "resets in" cells use.
    let relief = match resets_at {
        Some(at) => format!("; resets in {}", humanize_until(at - now)),
        None => String::new(),
    };
    // Nudge unless capacity is KNOWN to return within one session window: a sub-window
    // wait is transient (no nudge); a longer OR unknown-duration wait is structural.
    let structural_shortage = match resets_at {
        Some(at) => at - now > ADD_ACCOUNT_NUDGE_WAIT_SECS,
        None => true,
    };
    let nudge = if structural_shortage {
        " — add an account"
    } else {
        ""
    };
    format!("out of capacity{relief}{nudge}")
}

/// The systemic refresh-failure indicator (issue #378): the daemon reports the refresh MECHANISM is
/// down — not one account's creds, but the thing that renews them all (a stale `claude` path #375, a
/// wedged spawn). It opens on either of two brackets, which is what the whole provenance split below
/// is about: `consecutive` sweeps in a row failing with error across EVERY eligible account (#378),
/// or the startup preflight failing to resolve the binary at all (#787). Surfaced as DATA (not advisory
/// chrome like the #138 line): printed UNCONDITIONALLY so it survives a pipe / redirect /
/// `status | grep` — an operator's health check must be able to see it — tinted `Yellow` (its "act
/// at your next break" rank, ADR-0026: pre-death — every account still alive — so it sits BELOW the
/// vault pair's act-now `Red`) only when the colour gate is open. Distinct from the per-account
/// `AUTH` column: it is the whole mechanism failing, visible before any account dies. Carries only
/// the COUNT and a FIXED-TOKEN provenance class (issue #15). Mutually exclusive with the #138
/// advisory (that needs `[refresh]` OFF; this needs sweeps running, i.e. ON), so their ordering
/// never matters.
///
/// The DOWN verdict is one state, but its EVIDENCE has two shapes, and issue #813 stopped this line
/// from citing the wrong one. An episode opens either on the #378 sweep crossing or on the #787
/// startup preflight failing to resolve the binary, and the count alone cannot tell them apart —
/// the preflight path seeds it at one for pre-#813 clients' grammar, which reads identically to a
/// genuine one-sweep crossing under `systemic_failure_n = 1`. So this line branches on
/// [`SystemicRefreshSource`] and cites the count ONLY where it genuinely counts sweeps; the
/// preflight arm names the preflight instead. The `None` arm (a pre-#813 daemon, which sends no
/// discriminant) keeps the historical sweep phrasing verbatim — with no provenance on the wire
/// there is nothing better to say, and changing it would regress an old daemon's rendering for no
/// gain.
///
/// The preflight arm deliberately makes NO claim about sweeps having or not having run. A
/// preflight-opened episode clears only on a working sweep, so all-error sweeps may well have run
/// and climbed the count meanwhile; "no sweep has run" would be a fresh fabrication in place of the
/// one being removed. It reports only what was actually observed — the startup resolution failed.
///
/// DECIDED, not overlooked: that arm drops the DURATION signal the count otherwise carries, so a
/// boot-opened episode reads the same after one second as after fifty failed sweeps. The count is
/// not junk there — the preflight seeds a floor of one and only all-error sweeps climb it, so
/// `count - 1` IS the all-error sweeps since the seed. But reading it that way requires knowing the
/// seed convention, and pushing that onto every renderer is precisely the daemon-internals coupling
/// this discriminant exists to break: the wire would then carry a number whose meaning silently
/// depends on a sibling field, which is how this line came to fabricate a sweep in the first place.
/// The duration an operator actually needs is on the event log, bracketed by
/// `refresh_preflight_unresolved`. If a "how long has it been down" readout is wanted on the
/// preflight arm later, it should come from a separate, honestly-named field carrying that number
/// directly — never by asking a renderer to subtract a seed it has to know about.
fn render_systemic_refresh_failure(response: &StatusResponse, color: bool) -> String {
    let Some(consecutive) = response.systemic_refresh_failure else {
        return String::new();
    };
    // The log pointer stays PROVENANCE-NEUTRAL on both arms (issue #787): a preflight-opened
    // episode has no `reason=` line to read — its evidence is the `refresh_preflight_unresolved`
    // line — so naming `reason=` specifically would send an operator after evidence that may not
    // exist. "the daemon log" covers both, and matches how the menu-bar panel already phrases it.
    //
    // TOTAL `match`, no `_` arm — deliberately, and this is the structural half of the fix. A
    // THIRD opening bracket must not be able to reach the sweep phrasing by falling through: that
    // is precisely how #787 became #813 (a second bracket was added and this renderer silently
    // kept asserting sweeps). Written as `if let … else`, adding a variant compiles clean and
    // re-fabricates the count; written as a total `match`, the compiler makes the next author
    // decide what that bracket's evidence reads as. Keep it exhaustive.
    let body = match response.systemic_refresh_source {
        Some(SystemicRefreshSource::Preflight) => {
            // The count is the seeded floor here, not a sweep count, so it is not cited at all.
            // The tail matches the sweep arm's verbatim: the same state, the same remedy, only
            // the evidence clause differs.
            "refresh mechanism: DOWN — the startup preflight could not resolve the claude binary; \
             the mechanism is failing, not one account (check the daemon log and the [refresh] \
             claude binary)"
                .to_owned()
        }
        // `None` is a pre-#813 daemon, which sends no discriminant — the historical phrasing is
        // the best available reading there, and it is what that daemon's own CLI always printed.
        Some(SystemicRefreshSource::Sweep) | None => {
            // `consecutive` is always `>= 1` while an episode is active, so keep the noun agreement
            // right at that floor — a `systemic_failure_n` of 1 fires on the first all-error sweep.
            let sweeps = if consecutive == 1 { "sweep" } else { "sweeps" };
            format!(
                "refresh mechanism: DOWN — {consecutive} consecutive {sweeps} failed for every eligible \
                 account; the mechanism is failing, not one account (check the daemon log and the \
                 [refresh] claude binary)"
            )
        }
    };
    daemon_fault_line(&body, DaemonPayloadFault::SystemicRefreshFailure, color)
}

/// The daemon-level RUNTIME landing-overshoot notice (issue #613): THIS machine observed a
/// recently-parked (`reason=session`) account reach the SLO ceiling within the landing window — the
/// #595 landing overshoot the swap-DECISION reading is blind to, caught LIVE instead of only in a
/// later offline `reliability` run. Surfaced as DATA (not advisory chrome): printed UNCONDITIONALLY
/// so it survives a pipe / redirect / `status | grep` — an operator's health check must see the SLO
/// breach — tinted `Red` only when the color gate is open: an act-now SLO breach, the same `Red` as
/// the vault pair. A per-machine RUNTIME notice, deliberately OUTSIDE the ADR-0026 daemon-payload rank
/// (keychain / scrub / systemic), so it keeps [`red_line`] rather than [`daemon_fault_line`] — it is
/// not a shared-vault or refresh-mechanism fault, just a Red line that happens to sit among them.
/// Names the parked account and the fired-vs-landed spread, and distinguishes the two breach
/// CLASSES the offline SLI also splits: a swap that fired BELOW the SLO whose parked in-flight drain
/// then carried it over is the post-swap committed TAIL; a swap that fired already AT/OVER the SLO
/// (the daemon was blind/late) is a GAP-CROSSING — it did NOT climb after the swap, so the causal
/// clause must not mislabel it a tail. Both end with the SINGLE-MACHINE caveat: best-available
/// per-machine evidence, blind to a second machine co-consuming the same account (the
/// single-machine-sync boundary). One handle + two percents, never a token or email (issue #15).
/// Empty (no line) when no overshoot is recent.
fn render_landing_overshoot(response: &StatusResponse, color: bool) -> String {
    let Some(overshoot) = &response.recent_landing_overshoot else {
        return String::new();
    };
    let from = &overshoot.from_label;
    let decision = overshoot.decision_pct;
    let landed = overshoot.landing_pct;
    let ceiling = crate::landing::LANDING_SLO_CEILING_PCT;
    let cause = if decision < ceiling {
        // On-target swap (below the SLO); the parked account's committed tail carried it over.
        format!(
            "swapped out at {decision}% but its parked session climbed to {landed}% \
             (>= the {ceiling} SLO) — the post-swap committed tail"
        )
    } else {
        // The swap itself fired at/over the SLO — a late / gap-crossing swap-out, not a tail.
        format!(
            "swapped out already over the {ceiling} SLO at {decision}% and its parked session \
             reached {landed}% — a late swap-out, not a post-swap tail"
        )
    };
    let body = format!(
        "landing overshoot: {from} {cause}; single-machine signal \
         (a second machine co-consuming this account is invisible to it)"
    );
    red_line(&body, color)
}

/// The daemon-level KEYCHAIN-LOCKED rollup (issue #498): the macOS login keychain is LOCKED, so the
/// daemon cannot READ the shared `Claude Code-credentials` item at ALL (access denied). The
/// daemon-LEVEL sibling of the `canonical_scrub` line, but for an UNREADABLE item rather than a
/// readable-but-emptied one — so the remedy DIFFERS: UNLOCK THE KEYCHAIN, never `claude /login` (a
/// re-login cannot help while the keychain that stores the credential is locked). Surfaced as DATA
/// (unconditional, like the scrub + systemic lines — so it survives a pipe / redirect /
/// `status | grep`, an operator's health check must see it), naming the state AND the unlock remedy.
/// Content-parity with the menubar (`StatusPanelFormat.keychainLockedBanner`): same state + same
/// unlock remedy, each medium phrasing it its own way (R-2 state-parity, as ADR-0016 did for
/// `ActiveDeadNoTarget`). Tinted `Red` (rank 1, act-now) when the colour gate is open — the vault is
/// UNREADABLE, so the operator is blocked NOW; the same act-now `Red` as `canonical_scrub`
/// `exhausted`, ABOVE the systemic line's `Yellow` (ADR-0026 — the #575 rank, superseding the
/// earlier "plain footer register" framing this comment once carried). Rendered ABOVE `canonical_scrub`
/// (worst-first: an unreadable item is at least as severe as a readable-but-scrubbed one), though
/// the two are daemon-mutually-exclusive in practice (a locked keychain can't be read to know
/// scrubbed-ness). A bare BINARY state discriminant — never a token or email (issue #15). `false` (a
/// healthy / pre-#498 daemon that omits the field) prints nothing.
fn render_keychain_locked(response: &StatusResponse, color: bool) -> String {
    if response.keychain_locked {
        daemon_fault_line(
            "shared login: unreadable — the login keychain is locked; unlock it to restore access",
            DaemonPayloadFault::KeychainLocked,
            color,
        )
    } else {
        String::new()
    }
}

/// The daemon-level CANONICAL-SCRUB rollup (issue #469, umbrella #463): the shared
/// `Claude Code-credentials` canonical item has been SCRUBBED (its token cleared), so every
/// `claude` session is logged out — the fleet-wide lockout NO per-account `AUTH` column reflects
/// (each account row can read perfectly healthy while the shared item sits emptied). Surfaced as
/// DATA (unconditional, like the systemic line — so it survives a pipe / redirect /
/// `status | grep`, an operator's health check must be able to see it), naming the state and, for
/// the un-recoverable residual, the `claude /login` remedy. Content-parity with the menubar
/// (`StatusPanelFormat.canonicalScrubBanner`): same state + same `claude /login` remedy, each
/// medium phrasing it its own way (R-2 state-parity, as ADR-0016 did for `ActiveDeadNoTarget`).
/// `Exhausted` is tinted `Red` (rank 2, act-now — every session is logged out NOW), the same
/// act-now band as `keychain_locked` and ABOVE the systemic line's `Yellow`; `Recovering` stays
/// PLAIN — calm, may self-heal, colouring it would cry wolf (ADR-0026 — the #575 rank, superseding
/// the earlier "plain footer register" framing this comment once carried). A fleet-wide STATE
/// discriminant only — never per-account, never a token or email (issue #15). `None` (a healthy / pre-#516 daemon that
/// omits the field) prints nothing.
fn render_canonical_scrub(response: &StatusResponse, color: bool) -> String {
    match response.canonical_scrub {
        // Exhausted — recovery backed off (the bounded adopt churn hit its cap, or no viable adopt
        // target exists), so the canonical stays empty until a re-login. Name the state AND the
        // actionable remedy; `claude /login` is the byte-shared remedy the menubar names too. Red,
        // act-now (ADR-0026): every session is logged out NOW.
        Some(CanonicalScrub::Exhausted) => daemon_fault_line(
            "shared login: scrubbed — every session is logged out and auto-recovery is exhausted; \
             run claude /login to restore it",
            DaemonPayloadFault::CanonicalScrubExhausted,
            color,
        ),
        // Recovering — the daemon is autonomously adopting a live account back into the canonical, so
        // the fleet may self-heal with NO operator action. The calm, no-remedy cue (lower severity) —
        // rendered PLAIN (ADR-0026: `CanonicalScrubRecovering` carries no colour; colouring would cry wolf).
        Some(CanonicalScrub::Recovering) => daemon_fault_line(
            "shared login: scrubbed — recovering automatically (adopting a live account); \
             no action needed",
            DaemonPayloadFault::CanonicalScrubRecovering,
            color,
        ),
        None => String::new(),
    }
}

/// The behavioral-canary verdict line (issue #714): rendered as DATA (unconditional — like the
/// `canonical_scrub` footer, an operator's piped health check must see a refused-writes state),
/// only for the ALARM verdicts. A REFUSING drift, an AMBIGUOUS resolution and the #730/#738
/// UNPARSEABLE-CANONICAL refusal are act-now `Red` (credential writes — swaps AND
/// auto-protection — are refused until cleared); an OVERRIDDEN
/// drift is next-break `Yellow` (writes proceed under the operator's `canary_drift_override`,
/// each logged). The healthy / no-verdict states print NOTHING: `ok` and `inconclusive` are the
/// quiet normal (`inconclusive` covers the identity-unverified cases that do NOT refuse — including
/// an unparseable canonical the operator has explicitly overridden), `not_found` is already voiced
/// by the `canonical_scrub` / `keychain_locked`
/// machinery (a second line would double-report the same absent credential), and `None` is a
/// pre-#714 daemon that omits the field. Wording parity with [`Error::CanaryDrift`] /
/// [`Error::CredentialAmbiguous`] / [`Error::CanaryUnparseableCanonical`] so the refused swap's
/// stderr and this durable surface tell one story — the last of those already tells the operator to
/// "Investigate with `sessiometer status`", an instruction issue #738 is what finally makes pay off. Operator LABELS and a COUNT only — never a token, email, or account-uuid (issue #15).
fn render_canary(response: &StatusResponse, color: bool) -> String {
    match &response.canary {
        Some(CanaryStatus::Drift {
            displayed,
            matched,
            overridden,
        }) => {
            if *overridden {
                daemon_fault_line(
                    &format!(
                        "keychain canary: drift — the resolved credential belongs to {matched}, \
                         but {displayed} is named active; canary_drift_override is set, swaps \
                         proceed and are logged"
                    ),
                    DaemonPayloadFault::CanaryDriftOverridden,
                    color,
                )
            } else {
                daemon_fault_line(
                    &format!(
                        "keychain canary: drift — the resolved credential belongs to {matched}, \
                         but {displayed} is named active; credential writes are refused (false \
                         alarm? set canary_drift_override = true and restart the daemon)"
                    ),
                    DaemonPayloadFault::CanaryDriftRefusing,
                    color,
                )
            }
        }
        Some(CanaryStatus::Ambiguous { count }) => daemon_fault_line(
            &format!(
                "keychain canary: ambiguous — {count} Claude Code-credentials items found \
                 (expected exactly one); credential writes are refused until the duplicates \
                 are removed"
            ),
            DaemonPayloadFault::CanaryAmbiguous,
            color,
        ),
        // Issue #738: the #730 fail-CLOSED refuse, finally voiced. Names the EVIDENCE (the
        // resolved item does not look like a Claude Code credential), the CONSEQUENCE (writes
        // refused, so nothing overwrites it) and the ONE remedy — the same shape as the drift
        // line above, whose override it deliberately mirrors WITHOUT sharing: the tunables are
        // separate, and naming the wrong one would send the operator to a switch that cannot
        // help. Describes the item's SHAPE only, never its bytes (issue #15) — the whole point
        // is that those bytes are somebody else's secret.
        Some(CanaryStatus::RefusedUnparseableCanonical) => daemon_fault_line(
            "keychain canary: unrecognized credential — the Claude Code-credentials item \
             matches no stashed account and is not in Claude Code's own format, so it is \
             probably an unrelated secret; credential writes are refused to avoid \
             overwriting it (vetted it as safe? set canary_nostashmatch_override = true \
             and restart the daemon)",
            DaemonPayloadFault::CanaryRefusedUnparseableCanonical,
            color,
        ),
        Some(CanaryStatus::Ok | CanaryStatus::Inconclusive | CanaryStatus::NotFound) | None => {
            String::new()
        }
    }
}

/// The isolated-refresh discoverability advisory (issue #138): when the periodic refresh tick is OFF
/// (`[refresh].enabled = false`) AND ≥1 NON-ACTIVE account is unverified / stale / at-risk / dead,
/// that account's stored credential is going unmaintained — the operator would otherwise only find
/// out at `next swap: none (no viable target)`, after the fallback set is already dead. One line
/// names the remedy. ADVISORY CHROME, not data (AC-3): gated on the SAME color gate as the #73 ANSI
/// overlay, so it rides an interactive stdout TTY only — never into `--json` (this fn is not reached
/// there), a pipe, a redirect, or under NO_COLOR / CLICOLOR=0 / TERM=dumb / `--no-color`.
/// `Some(false)` is the ONLY arming value; `Some(true)` (enabled) and `None` (a pre-#138 daemon that
/// omits the field) both suppress.
fn render_refresh_disabled_advisory(response: &StatusResponse, color: bool) -> String {
    if color && response.refresh_enabled == Some(false) && has_stale_nonactive(response) {
        REFRESH_DISABLED_ADVISORY.to_owned()
    } else {
        String::new()
    }
}

/// The daemon-level SYNCHRONIZED-EXPIRY COHORT line (issue #879, REQ-CC-B-004): several accounts'
/// refresh tokens reach their deadlines inside one window, so the swap pool thins by more than one
/// member at a time.
///
/// **The fleet fact no row can carry.** The `EXPIRY` column beside it shows each member's own
/// deadline, and every one of them reads individually survivable; only this states that they go
/// TOGETHER. It is also the half the upstream Claude Code client structurally cannot provide — that
/// warns for the ACTIVE account only, so a parked account's deadline stays invisible to it until
/// swap-in, possibly after death.
///
/// Rendered as an AGGREGATE sentence — counts, a span, and one instant. Never a list of
/// handles-with-deadlines: per-account facts belong on the per-account row (the shape issues
/// #543/#544 were retired for, and design-stats.md §D-STA-5's structural rule). Membership is on the
/// wire per row as [`AccountExpiry::cohort_id`], so a reader who wants the names has them without
/// this line duplicating them.
///
/// **This is `status`, not the `stats` roster block — a deliberate departure from #879's AC2, which
/// named that block.** The `stats` verb is an OFFLINE reader of the persisted event series and never
/// talks to the daemon, so a cohort — a live fact derived from the credentials the daemon holds
/// right now — has no producer on that surface. Not merely an empty one: `stats::Report::expiry`
/// is written as an empty map on every production path, blocked on issue #917 (folding the durable
/// expiry events into the per-account report), so a roster-block cell added today would render `—`
/// unconditionally. REQ-CC-B-004 says "the DAEMON shall surface", and the issue is titled
/// `(feat) daemon:` — so the fact ships where it exists. AC2's load-bearing
/// half, the PROHIBITION on a per-account band or footer list, is honoured exactly. Its positive
/// half — a `stats` roster-block mirror — is unbuilt and unblocks with #917; the same reasoning
/// applies to whichever surface fires the fleet-level line there.
///
/// **Deliberately NOT a [`DaemonPayloadFault`].** Nothing is broken and the daemon can act
/// perfectly well; this is a forward-looking capacity fact. Routing it through that enum would
/// enrol it in the ADR-0026 cross-surface rank contract — which obliges a matching menubar panel
/// banner (issue #575's both-or-neither invariant) — and the menubar half of the expiry feature is
/// issue #884's, not this one's. It therefore prints below every band that reports something
/// ALREADY WRONG — whether the daemon is blocked by it or merely proceeding under an override — and
/// carries its own tint instead.
///
/// That tint tracks the SOONEST member — [`Severity::Red`] once its deadline has passed,
/// [`Severity::Yellow`] while it is still ahead — which is the band [`expiry_severity`] gives that
/// member's own cell, so the fleet line never reads calmer than the row that bites first. It does
/// NOT promise every member matches: a cohort can straddle the horizon (a 24h window against a 7d
/// horizon admits an anchor at 6d23h and a member at 7d12h), leaving a Yellow fleet line beside a
/// [`Severity::Dim`] `Beyond` cell. That is the honest reading — the later member IS further out —
/// and the cohort's urgency is the earliest deadline's, not an average. Resolved against the
/// RENDER's clock for the same reason [`expiry_view`] is: `status` is served from the last tick's
/// snapshot, so a deadline can pass inside the poll interval, and a line built to warn must not
/// read as calm at exactly the moment it starts mattering.
///
/// Present-tense state, never an imperative (D-CC-3's firewall condition): it says what IS, and the
/// remedy — a `sessiometer login` per member — rides the operator docs (issue #885), not this line.
///
/// Empty when the wire carries no cohort. That absence is NOT a claim the fleet is unsynchronized:
/// a roster whose credentials carry no `refreshTokenExpiresAt` produces no cohort AND no `EXPIRY`
/// column, so nothing anywhere reports a reassuring zero for a fleet that was never measured (the
/// issue #137 invariant). The `of N accounts with a known deadline` denominator carries the same
/// discipline into the line that DOES print — it names the observed set rather than letting the
/// reader assume the whole roster was seen.
fn render_expiry_cohort(response: &StatusResponse, now: i64, color: bool) -> String {
    let Some(cohort) = response.expiry_cohort else {
        return String::new();
    };
    let lapsed = cohort.earliest <= now;
    // `humanize_until` maps a non-positive remainder to `now` — this table's vocabulary for a
    // benign reset ARRIVING — so a passed deadline must be worded, not humanized.
    let earliest = if lapsed {
        "earliest already lapsed".to_owned()
    } else {
        format!("earliest in {}", humanize_until(cohort.earliest - now))
    };
    // A zero span is the sharpest form of the finding (identical deadlines), not a degenerate one,
    // so it gets its own wording rather than `humanize_until(0)`'s "now".
    let spread = if cohort.span_secs <= 0 {
        "share one deadline instant".to_owned()
    } else {
        format!(
            "fall within {} of each other",
            humanize_until(cohort.span_secs)
        )
    };
    let ExpiryCohort { size, observed, .. } = cohort;
    let body = format!(
        "expiry cohort: {size} of {observed} accounts with a known deadline {spread} — {earliest}"
    );
    let severity = if lapsed {
        Severity::Red
    } else {
        Severity::Yellow
    };
    severity_line(&body, severity, color)
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
///
/// **FRAMING firewall: IN SCOPE** (issue #1123). Scanned by
/// `the_operator_advisories_carry_no_banned_framing_but_the_guard_bites_on_injection` against
/// `crate::framing_vocabulary::scan_advisory_banned` — the central #160 vocabulary minus
/// `ADVISORY_EXEMPT_TOKENS`, which is `enable` and nothing else. This line is the sole earner of
/// that exemption: "or enable [refresh]" names a config operation the operator performs on the
/// tool's own state, the mechanical class issue #918 measured on `--help`. What stays armed is
/// everything the advisory has no business saying — it may state that credentials are going
/// unmaintained and name the two remedies, and it may not call that state `critical`, call the
/// lapse `imminent`, or tell the operator what they `should` do about it.
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
    /// The EXPIRY cell (issue #883): this account's REFRESH-token deadline as a compact
    /// time-until (`6d21h`), the state word `lapsed`, or [`EXPIRY_GAP`] when none was observed —
    /// BRACKETED (`[6d21h]`) while that deadline is inside the configured horizon (issue #934).
    /// A cell of its OWN — never folded into [`status`](Self::status) — because the expiry axis is
    /// orthogonal to [`CredentialHealth`] (issue #878). See [`expiry_table_cell`].
    expiry: String,
    /// Per-cell urgency for the color overlay (issue #84): each cell carries its OWN
    /// health, so one row can show several independent colors (a red `session` reset
    /// beside a green `weekly` reset, etc.). Each is `None` when its cell has no
    /// reading — that cell is then printed without color, since absence of color is
    /// not a false "healthy" signal. `account` is the OVERALL (binding-window)
    /// [`severity`]; `session` / `weekly` the [`util_severity`] /
    /// [`weekly_cell_severity`] utilization bands on each `%`; each reset its OWN
    /// [`proximity_severity`] (issue #94) — how soon that window flips, independent
    /// of utilization. The health-text column is never tinted (its tags are their own
    /// signal), so it has no field here; `expiry` IS tinted (issue #883) — its cell carries no
    /// self-colouring glyph, so the band is what makes the horizon readable at a glance. It is no
    /// longer the ONLY carrier: since issue #934 the cell also brackets a within-horizon deadline,
    /// so the band augments a signal that already survives `--no-color` rather than being it.
    account_severity: Option<Severity>,
    session_severity: Option<Severity>,
    session_reset_severity: Option<Severity>,
    weekly_severity: Option<Severity>,
    weekly_reset_severity: Option<Severity>,
    expiry_severity: Option<Severity>,
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
            expiry: expiry_table_cell(account.expiry, now),
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
            expiry_severity: expiry_severity(account.expiry, now),
        }
    }
}

/// The needs-REFRESH cue for a `Degraded` (bare-quarantine) credential (issue #427): the honest
/// counterpart to `Dead`'s `claude /login`. Leads with the immediate remedy (`poke`); enabling
/// `[refresh]` — the durable fix — is carried holistically by [`REFRESH_DISABLED_ADVISORY`], and
/// a genuine refresh-token death still escalates to 🔴 `claude /login`. Deliberately NOT
/// "re-login" — that is precisely the over-reaction the honest verdict prevents.
///
/// **FRAMING firewall: IN SCOPE, at full strictness** (issue #1123). This is the surface that
/// posed the issue's imperative question — `run 'sessiometer poke'` is a directive, and `--help`
/// was only ever scoped against prose that NAMES operations rather than ordering them. Measured,
/// the imperative costs nothing: the cue is clean against the WHOLE central vocabulary, so it is
/// scanned by `scan_banned` and not by the advisory subset, and needs no exemption at all.
///
/// That is not an accident of wording — it is what the #160 vocabulary actually bans. The list
/// never proscribed the imperative MOOD; it proscribes acquisition, value judgement,
/// recommendation and alarm. An imperative pointing at a free, local, mechanical remedy is a FACT
/// about what fixes the state. `degraded — buy more capacity` would trip; `degraded — run
/// 'sessiometer poke'` does not.
///
/// And *should* not, which is a separate claim needing a separate reason — the head-room permit
/// ADR-0020 records cannot supply it, since that permit is for a fact stated "as an observation,
/// not advice" and this cue is advice. The reason is that this tool is REQUIRED elsewhere to make
/// its operator guidance clear and FOLLOWABLE (issues #376 / #397 — `crate::error`'s
/// `NoManagedService` and `UnmanagedDaemonNoRestart`, the
/// `unmanaged_daemon_no_restart_guides_the_operator_with_a_followable_action` test, and the
/// "name the followable stop first" rule this file already follows). Reading the #160 firewall as
/// a ban on directives would set those two requirements against each other: the AUTH cell would
/// have to report a repairable account while withholding the one command that repairs it.
/// ADR-0020 § Status → Amended 2026-08-10 (#1123) records the boundary this extends.
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
/// (the daemon's blocked-for-the-week verdict, `weekly >= weekly_ceiling`, issue
/// #11/#37) is bound by its weekly window whatever the raw percentages say — the
/// SAME window its WEEKLY reset cell shows — and is at least Red: a week-blocked account
/// is never painted "healthy", even when the operator has lowered `weekly_ceiling`
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
/// weekly_ceiling` verdict, issue #11/#37) reads Red whatever its rounded percent —
/// a week-blocked account is never painted "healthy", even when the operator has
/// lowered `weekly_ceiling` below the Red cutoff (the same guarantee [`severity`]
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

/// The gap sentinel for the REFRESH-token expiry cell (issue #883): an em dash, DELIBERATELY not
/// the `n/a` the sibling `status` cells use, because the two absences are different facts.
///
/// `n/a` in [`reset_cell`] / [`pct`] means *this poll produced no reading* — a transient read
/// failure. `—` here is the absence that must NOT be filed under that: chiefly the account WAS
/// read and the credential carried no parseable `refreshTokenExpiresAt` at all
/// ([`ExpiryHorizon::Unknown`]) — a positive observation of absence, the issue #137 invariant. The
/// same cell also stands in when the wire carries no modifier at all (a pre-#882 daemon, an
/// unpolled account; see [`expiry_view`]), because the operator-facing fact is identical: no
/// deadline is known here. Rendering EITHER as anything an operator could mistake for "fine" is the
/// one failure mode the whole foresight feature exists to avoid, and reusing `n/a` would file the
/// observed absence under transient failure. Also the gap sentinel `stats`'
/// `render_account_table` elides a uniformly-empty column on, so ONE spelling serves both surfaces.
pub(crate) const EXPIRY_GAP: &str = "—";

/// One account's REFRESH-token expiry cell (issue #883) — the operator-facing projection of the
/// [`AccountExpiry`] modifier issue #882 put on the `status`/`watch` wire. The shared FACT only:
/// what either table actually renders is [`expiry_table_cell`], this string plus the horizon mark.
///
/// Rendered as PRESENT-TENSE STATE, never an imperative: a compact time-until (`6d21h`, `29d`,
/// reusing [`humanize_until`], the same shape the `RESET` cells carry) for an observed deadline,
/// the bare state word `lapsed` for one already past, and [`EXPIRY_GAP`] when none was observed.
/// The remedy (`sessiometer login`) is deliberately NOT named here: this cell reports a fact, and
/// the AUTH cell already owns the actionable cue once the credential actually fails.
///
/// ORTHOGONAL to [`CredentialHealth`] and so never folded into [`health_cell`] — an account is
/// routinely `Healthy` *and* [`ExpiryHorizon::Within`] its horizon at the same time, which is
/// exactly the case the operator needs to see and which no single ordinal ladder can express
/// (issue #878).
///
/// Distinct from the `--verbose` access-token block ([`render_access_token_expiry`]): that clock is
/// the ACCESS token Claude Code refreshes invisibly, explicitly *not* a re-login deadline. This one
/// is the REFRESH token — the login itself — and no refresh moves it.
///
/// `expires_at == None` under a non-`Unknown` state is unreachable from [`crate::daemon::account_expiry`],
/// which admits no such combination; it is still handled rather than unwrapped, because the wire is
/// `#[serde(default)]`-decoded and a malformed frame must degrade honestly, not panic. Degrading
/// honestly is NOT uniformly "render a gap": a declared [`ExpiryHorizon::Lapsed`] still reads
/// `lapsed` without a deadline, since that word never needed one (see [`expiry_view`]).
///
/// `pub(crate)` so the `stats` per-account table renders the SAME cell from the SAME code — one
/// spelling of this FACT across both surfaces, which is what keeps them from drifting. Only the
/// fact: `status` additionally TINTS the cell ([`expiry_severity`]) while `stats` leaves it
/// uncoloured, because that surface's colour vocabulary is the neutral utilisation band and an
/// urgency tint on a credential deadline would editorialise inside it (D-STA-6). Presentation
/// diverges deliberately; the string cannot.
///
/// **This is the CROSS-SURFACE vocabulary, and it is byte-pinned.**
/// `cross_surface::ExpiryParityCase` (test-only, so deliberately not an intra-doc link — the
/// module is `#[cfg(test)]` and absent from the doc graph) records this exact string into
/// `build/fixtures/cross-surface-severity.json`, which the panel's own
/// `CrossSurfaceSeverityParityTests` asserts against — so a change HERE is a change to a contract
/// the Swift gate reads. The per-account horizon MARK (issue #934) is therefore NOT applied here
/// but one layer out, in [`expiry_table_cell`]: R-2 pins the shared state vocabulary, while how a
/// surface makes the horizon band survive colour loss is that surface's own presentation — the
/// same split the tint above already lives under.
pub(crate) fn expiry_cell(expiry: Option<AccountExpiry>, now: i64) -> String {
    match expiry_view(expiry, now) {
        ExpiryView::Gap => EXPIRY_GAP.to_owned(),
        ExpiryView::Lapsed => "lapsed".to_owned(),
        ExpiryView::Live { at, .. } => humanize_until(at - now),
    }
}

/// One account's EXPIRY table cell (issue #934): the [`expiry_cell`] fact, bracketed when the
/// deadline falls INSIDE the configured horizon.
///
/// **Why a mark at all.** Until now the horizon band reached the operator only as colour —
/// [`expiry_severity`]'s `Yellow` vs `Dim`. `2d2h` and `28d11h` are typographically identical, so
/// under `--no-color`, `NO_COLOR=1`, a pipe, a log capture, or colour-blindness the entire
/// per-account signal was gone; `--no-color` is a first-class supported flag, so the feature was
/// degrading silently in a mode this CLI advertises. `stats` had it worse — [`crate::stats`]'s
/// `col_expiry` is uncoloured in EVERY mode, so there the band was never visible at all. This is
/// design-stats.md §D-STA-5's `Color augments only` rule applied to the one column that broke it:
/// the `trend` sparkline already carries its meaning in glyph SHAPE, and now so does this.
///
/// **Why brackets, and not `!`.** The mark is a DESCRIPTOR, never an instruction (§D-STA-6 /
/// SUR-001), and `within` is the STEADY state rather than an exception: with the 7-day default
/// horizon against ~30-day refresh tokens, every healthy account sits inside the horizon for a week
/// before every re-login — issue #884 measured that at ~23% of the time and refused a menu-bar `!`
/// on exactly that cry-wolf ground. An alarm sigil would be wrong for a state most of a healthy
/// fleet occupies most weeks. Brackets say something narrower and true: the horizon is a configured
/// WINDOW and this deadline falls inside it — which is precisely the fact a bare duration cannot
/// carry, since the window is operator-configurable and the reader cannot know its width. They also
/// stay clear of the two sigils this table already spends (`*` marks the ACTIVE account one column
/// over; `~` is `stats`' approximation prefix on `runway`), and they are pure ASCII, so the mark
/// survives a pipe, a log capture, and a terminal with no Unicode at all.
///
/// The one wrinkle, stated rather than glossed: `[` and `]` are regex metacharacters, so a cell
/// copied out of the table and pasted into a bare `grep` becomes a CHARACTER CLASS —
/// `grep '[6d21h]'` matches any row containing one of those characters, which on this table is
/// most of them. The mark survives the pipe (it is in the bytes either way); it is pattern REUSE
/// that needs `grep -F`, and the `status` help says so — a documented quoting note being the
/// cheaper cost against either alternative above.
///
/// **Beyond, gap, and `lapsed` are deliberately unmarked.** `Beyond` and the gap are the calm
/// states, and marking `lapsed` would be false twice over: a lapsed deadline is not *within* a
/// forward-looking window, and the bare word already reads as the loudest thing in a column of
/// durations, so it needs no help to be found by eye.
///
/// The arms MIRROR [`expiry_severity`] — including the defensive fallthrough — so the mark and the
/// tint cannot disagree about which cells are inside the horizon: exactly the cells this brackets
/// are exactly the cells that render `Yellow`.
/// `the_expiry_mark_and_the_tint_agree_on_which_cells_are_within` pins that, and [`expiry_view`]
/// remains the single place the staleness rule lives, so a deadline that passes inside the poll
/// window loses the mark and the `Yellow` in the same step.
///
/// COSTS ONE COLUMN, bounded by the horizon itself: a marked cell is only ever as wide as a
/// within-horizon duration, so at the 7-day default the widest is `[6d23h]` — 7 display columns
/// against the 6 that `lapsed` and the `EXPIRY` label already require. The table keeps its
/// §D-STA-5 shed behaviour untouched (EXPIRY still sheds first, at priority 1) and never wraps.
pub(crate) fn expiry_table_cell(expiry: Option<AccountExpiry>, now: i64) -> String {
    let cell = expiry_cell(expiry, now);
    if expiry_within_horizon(expiry, now) {
        format!("[{cell}]")
    } else {
        cell
    }
}

/// Whether an [`expiry_table_cell`] carries the horizon mark — the render-time answer to "is this
/// deadline inside the configured horizon?".
///
/// Written as its own match, arm-for-arm against [`expiry_severity`] rather than derived from it,
/// because the two answer different questions and only one of them is allowed to change if the
/// tint vocabulary ever does. Reading the mark off `Some(Severity::Yellow)` would couple a
/// typographic affordance to a colour band and silently re-mark every cell the day a band moves.
fn expiry_within_horizon(expiry: Option<AccountExpiry>, now: i64) -> bool {
    match expiry_view(expiry, now) {
        ExpiryView::Gap | ExpiryView::Lapsed => false,
        ExpiryView::Live {
            horizon: ExpiryHorizon::Beyond,
            ..
        } => false,
        // `Within` — and, defensively, any other class that survived the render-time check. The
        // SAME fallthrough `expiry_severity` resolves to `Yellow`.
        ExpiryView::Live { .. } => true,
    }
}

/// The colour band for an [`expiry_cell`] (issue #883), following the per-cell overlay discipline
/// (issue #84): [`Severity::Red`] for a deadline already past (only an operator `sessiometer login`
/// recovers it), [`Severity::Yellow`] for one [`ExpiryHorizon::Within`] the configured horizon, and
/// [`Severity::Dim`] for one [`ExpiryHorizon::Beyond`] it — the same de-emphasis
/// [`proximity_severity`] gives a far-off reset, since there is nothing to act on.
///
/// Takes `now` and routes through [`expiry_view`] for the same reason the cell does: a tint read
/// off the cached class alone would paint a `lapsed` cell `Yellow`.
///
/// `None` — uncoloured — for an unobserved deadline, matching every other cell with no reading:
/// absence of colour is not a false "healthy" signal.
fn expiry_severity(expiry: Option<AccountExpiry>, now: i64) -> Option<Severity> {
    match expiry_view(expiry, now) {
        ExpiryView::Gap => None,
        ExpiryView::Lapsed => Some(Severity::Red),
        ExpiryView::Live {
            horizon: ExpiryHorizon::Beyond,
            ..
        } => Some(Severity::Dim),
        // `Within` — and, defensively, any other class that survived the render-time check.
        ExpiryView::Live { .. } => Some(Severity::Yellow),
    }
}

/// What the RENDER holds about one account's refresh-token deadline: nothing usable, a deadline
/// already past, or a live one at `at` under the class that survived the render-time check.
///
/// The one place the staleness rule lives, so [`expiry_cell`] and [`expiry_severity`] cannot
/// disagree about whether a credential has lapsed. `Live` carries `horizon` for that same reason:
/// the tint reads its band OFF THE VIEW rather than back off the raw modifier, so there is no
/// second path by which a cell rendered as a duration could be coloured on the stale class.
enum ExpiryView {
    Gap,
    Lapsed,
    Live { at: i64, horizon: ExpiryHorizon },
}

/// Resolve the daemon's cached [`ExpiryHorizon`] against the clock the RENDER actually holds
/// (issue #883).
///
/// **The render-time comparison is authoritative, and that is load-bearing.** `status` is served
/// from the snapshot built at the LAST TICK, not per request — up to `poll_interval_secs` old
/// (default 300 s, 3600 s while exhausted). A deadline that passes inside that window is still
/// classified [`ExpiryHorizon::Within`]/[`ExpiryHorizon::Beyond`] when the cell renders, and
/// [`humanize_until`] maps a non-positive remainder to `now` — this table's vocabulary for a
/// *benign* reset ARRIVING. The one cell built to warn would then read as fine, at exactly the
/// moment it matters, and every token lapse passes through that window exactly once. So an
/// observed deadline at or before `now` reads `lapsed` whatever the cached class says.
///
/// The rule is MONOTONE in the safe direction: a class of [`ExpiryHorizon::Lapsed`] also stays
/// `Lapsed` even should the render clock read earlier than the tick's (backwards skew, NTP). Once
/// either clock has seen the deadline pass, the cell does not un-lapse.
///
/// [`ExpiryHorizon::Unknown`] is authoritative in the OTHER direction: it means the daemon found no
/// parseable deadline, so a stray `expires_at` beside it is not trusted into a rendered duration.
fn expiry_view(expiry: Option<AccountExpiry>, now: i64) -> ExpiryView {
    // No modifier on the wire at all: a pre-#882 daemon, or an account not yet polled.
    let Some(expiry) = expiry else {
        return ExpiryView::Gap;
    };
    match (expiry.horizon_state, expiry.expires_at) {
        // `Unknown` is authoritative FIRST: no parseable deadline was found, so a stray
        // `expires_at` beside it is not trusted into a rendered duration.
        (ExpiryHorizon::Unknown, _) => ExpiryView::Gap,
        // A DECLARED lapse outranks a missing deadline, and the order of these two arms is the
        // whole point: `lapsed` is a bare state word that never reads `at`, so an absent
        // `expires_at` is not a data-insufficiency case here. Falling through to the gap below
        // would discard the strongest negative signal the wire can carry — and on `status`,
        // where `status_columns` materialises the column only once some row is non-gap, an
        // account whose lapse is the roster's only expiry datum would take the entire column
        // down with it and render a dead login as no login problem at all.
        (ExpiryHorizon::Lapsed, _) => ExpiryView::Lapsed,
        (_, None) => ExpiryView::Gap,
        (_, Some(at)) if at <= now => ExpiryView::Lapsed,
        (horizon, Some(at)) => ExpiryView::Live { at, horizon },
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
/// Distinct from the table's `EXPIRY` column ([`expiry_cell`], issue #883), which reports the
/// REFRESH token — the login itself, a server-issued deadline no refresh moves. That IS the
/// forward-looking re-login signal this block explicitly is not, and it is why a raw clock in the
/// default table is no longer the only thing an operator could mistake for one: the two are
/// different tokens on different clocks, and only the `EXPIRY` column belongs in the table.
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

    let stash = RealAccountStash::new();

    // Which roster account is this machine currently logged into (issue #1001)? Resolved
    // BEFORE `apply_import`, because "active" is a PRE-import fact: the import is about to
    // overwrite that account's stash, after which the token-first signal below no longer
    // matches. A fresh target (`local` is `None`) has an empty roster, so no roster account
    // can resolve as active and the read is skipped entirely.
    let active_uuid = match local.as_ref() {
        Some(config) => {
            resolve_active_uuid_for_import(
                &config.roster,
                &RealCredentialStore::new(),
                &stash,
                &paths::claude_json()?,
            )
            .await
        }
        None => None,
    };

    let (config, outcomes) = apply_import(
        Some((&swap_lock, SWAP_LOCK_MAX_WAIT)),
        &payload,
        local,
        active_uuid.as_deref(),
        &stash,
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

/// Re-badge an artifact-config SHAPE failure so it names the import version floor
/// (issue #1053). Pure, so the mapping is unit-testable without an artifact on disk.
///
/// The artifact's config travels as text and is re-parsed by THIS build's parser, whose every
/// `Raw*` struct carries `deny_unknown_fields` — so a config key this build does not know, at
/// any nesting level, aborts the import as a bare `deny_unknown_fields` line that names a key
/// and explains nothing, while the container's `format_version` still reads `1` on both sides.
/// That is the same defect the rendered config has already inflicted twice on older builds
/// (see [`CONFIG_BLOCK_VERSION_FLOOR`](crate::migration::CONFIG_BLOCK_VERSION_FLOOR) for the
/// floor and how to re-derive it); this build cannot repair an already-built binary, but it
/// can stop being the one that fails mutely when the artifact is the newer side.
///
/// **Scoped to the symptom it detects.** Only [`Error::ConfigParse`] — the *shape* failure —
/// is re-badged. [`Error::ConfigInvalid`] (a range violation, a duplicate `account_uuid`) is
/// a validation verdict on a config this build parsed FINE, so the floor is not its
/// explanation and it passes through with its own precise message. Every other error class
/// is likewise untouched.
fn name_the_import_version_floor(err: Error) -> Error {
    match err {
        Error::ConfigParse(detail) => Error::MigrationImportConfigRejected { detail },
        other => other,
    }
}

/// Resolve WHICH roster account this machine is currently logged into, for `import`'s
/// non-adoption report (issue #1001) — token-first, through the SAME shared resolver
/// ([`crate::active`]) the daemon's poll loop and the `use` swap consult, so all three
/// verbs agree on what "active" means rather than each deriving its own answer.
///
/// BEST-EFFORT and never fatal, deliberately UNLIKE `use`. Nothing is written on the
/// strength of this answer: it decides only whether one advisory line prints. So a
/// canonical the keychain will not hand over (locked, ACL-denied, or the scrubbed
/// `CredentialNotFound` item) degrades to the `~/.claude.json` DISPLAY signal —
/// [`crate::active::resolve_via_display`], the same degradation the daemon's
/// locked-keychain poll takes — rather than aborting an import the operator asked for.
/// `use` treats a locked keychain as a safety abort instead, because it is about to WRITE
/// the canonical and a wrong answer there loses a credential (#211/#212). When BOTH signals
/// are unavailable the answer is `None` and no notice prints — strictly the behaviour that
/// shipped before this issue, never worse.
///
/// It is also the ONLY canonical read on the `import` path, and that item belongs to Claude
/// Code rather than to sessiometer, so on a UI session where trust was never granted
/// `security` may raise a modal prompt here ([`crate::keychain`] documents the general case).
/// Failing open above is what keeps that from being a regression: a refused prompt, a
/// headless session and an SSH session all continue the import on the display signal — and a
/// FRESH target (`local` is `None`, the primary migration path) never reaches this at all.
///
/// The caller runs this BEFORE `apply_import` acquires the #64 swap lock, which leaves a
/// window where a concurrent `use` invalidates the answer. Two separate reasons, worth
/// keeping apart: it MUST precede the stash WRITES (once import overwrites the active
/// account's stash, the token no longer matches it and resolution silently changes answer),
/// and it merely HAPPENS to precede the lock, because moving it inside would mean handing a
/// [`CredentialStore`] to [`apply_import`] — and `apply_import` holding no such seam is
/// exactly what makes "import adds no canonical writer" (R-2a, C-2) structural rather than a
/// convention. A stale advisory line is the cheaper failure.
async fn resolve_active_uuid_for_import<C: CredentialStore, S: AccountStash>(
    roster: &[Account],
    store: &C,
    stash: &S,
    claude_json: &Path,
) -> Option<String> {
    let index = match store.read().await {
        Ok(canonical) => {
            crate::active::resolve_account_for(roster, stash, claude_json, &canonical).await
        }
        Err(_) => crate::active::resolve_via_display(roster, claude_json),
    }?;
    Some(roster[index].account_uuid.clone())
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

/// How many roster accounts carry each label (issue #1005). Labels are operator handles and are
/// not required to be unique, so a count above one is a legal — if unresolvable — state; taking
/// this before and after a merge is what lets [`apply_import`] tell an ambiguity it CREATED from
/// one that was already there or that a relabel merely passed through.
fn label_bearers(roster: &[Account]) -> std::collections::HashMap<String, usize> {
    let mut bearers = std::collections::HashMap::new();
    for account in roster {
        *bearers.entry(account.label.clone()).or_insert(0) += 1;
    }
    bearers
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
///
/// `active_uuid` is the account this machine is currently logged into
/// ([`resolve_active_uuid_for_import`]; `None` when nothing resolves), and it changes only
/// the REPORT (issue #1001). Every account's credential lands in its own per-account stash
/// `Sessiometer/<uuid>` — but the ACTIVE account's live credential is served from the
/// canonical `Claude Code-credentials` item, which this function does not, and must not,
/// write: `src/swap.rs` owns that transition under the #64 single-writer lock and
/// `src/daemon/canonical.rs` reconciles out-of-band changes to it, so a second writer here
/// would race the reconciler (design § 4.1 / AD-1, PRD R-2a, C-2). Import therefore stays a
/// stage-and-roster operation and closes the gap by SAYING so — see
/// [`non_adoption_notice`].
async fn apply_import<S: AccountStash>(
    lock: Option<(&Path, Duration)>,
    payload: &Payload,
    local: Option<Config>,
    active_uuid: Option<&str>,
    stash: &S,
    overwrite: bool,
) -> Result<(Config, Vec<AccountImport>)> {
    // The roster + tunables the artifact carries, held to the same invariants as any
    // on-disk config (unique non-empty account_uuid, tunable ranges) — with the SHAPE
    // failure re-badged to name the import version floor (issue #1053).
    let incoming =
        Config::from_toml_str(payload.config_toml()).map_err(name_the_import_version_floor)?;

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

    // Issue #1005: how many accounts carried each label BEFORE the merge — the baseline the
    // duplicate-label check compares the finished roster against, captured here because the loop
    // below mutates the roster it is taken from.
    let bearers_before = label_bearers(&result.roster);

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
        let mut staged = false;
        if let Some(managed) = secrets.get(incoming_account.account_uuid.as_str()) {
            if write_and_verify(stash, &incoming_account.stash(), managed)
                .await
                .is_err()
            {
                outcomes.push(AccountImport::failed(&incoming_account.label));
                continue;
            }
            staged = true;
        }

        let mut outcome = match existing {
            Some(idx) => {
                result.roster[idx] = incoming_account.clone();
                AccountImport::overwritten(&incoming_account.label)
            }
            None => {
                result.roster.push(incoming_account.clone());
                AccountImport::imported(&incoming_account.label)
            }
        };
        // Issue #1001: a credential was STAGED into this account's own stash, and it is the
        // account the machine is live on — so the bytes just written are in a slot nothing
        // reads, and the canonical item still holds the pre-import token. Gated on `staged`
        // deliberately: a SKIPPED account (conflict policy left it byte-for-byte untouched)
        // and a config-only account (no secret in the artifact) both wrote nothing, so there
        // is nothing pending adoption and the notice would be a false alarm.
        if staged && active_uuid == Some(incoming_account.account_uuid.as_str()) {
            outcome = outcome.staged_not_adopted();
        }
        outcomes.push(outcome);
    }

    // Issue #1005: flag every row whose label this import pushed INTO ambiguity — more than one
    // bearer at the end, and MORE bearers than the target started with. Duplicate labels stay an
    // ACCEPTED state, so this neither refuses the import nor renames anything; it only makes the
    // creation audible.
    //
    // Measured as a before/after comparison over the WHOLE merge rather than per-write, and both
    // halves of that are load-bearing:
    //
    // - Reading the FINAL roster — rather than the roster as each write lands — does two things.
    //   It covers a collision arriving inside a single artifact on a fresh target, where `local` is
    //   `None` and a check written against the target's roster finds it empty and stays silent
    //   while both entries append (the exact state R-6 exists to prevent, created with every other
    //   criterion green — nothing rejects the duplicate on the way in, since `Config::validate`
    //   checks empty uuid, empty label and duplicate uuid and has no duplicate-label arm). And it
    //   is what suppresses a collision the merge only PASSES THROUGH: importing a label SWAP
    //   between two accounts the target already has (`a`/`b` becoming `b`/`a`) is transiently
    //   `b`/`b` mid-loop, but each label ends at one bearer, so `after > 1` is false for both.
    // - Comparing against the BEFORE count covers the remaining case, and only that one: a
    //   duplicate the target ALREADY had, overwritten in place. There `after > 1` holds — the label
    //   really does have two bearers — but the count did not move, so this import did not create
    //   the ambiguity and the operator was already warned when something did.
    //
    // The two clauses are therefore NOT interchangeable and neither is redundant: dropping
    // `after > 1` warns on every ordinary new account, and dropping `after > before` re-warns about
    // a pre-existing duplicate on every subsequent import. Each failure is the same one the
    // ordinary cross-machine import would be — `account_uuid` is stable across machines, so
    // same-label/same-uuid is the COMMON case, and a warning that fires where nothing was created
    // trains dismissal of the one that matters (PRD § P5).
    //
    // KNOWN LIMIT, deliberate. The rule is a count, so a count-PRESERVING substitution of bearers
    // is invisible to it: a target `[dup/A, dup/B, solo/C]` overwritten by `[solo/A, dup/C]` ends
    // as `[solo/A, dup/B, dup/C]` — `dup/C` is genuinely a new same-label/different-uuid entry, and
    // `count(dup)` is 2 either side, so nothing is said. Accepted rather than fixed: `dup` was
    // unresolvable before this import and is unresolvable after, so the operator's actionable state
    // is unchanged, and a warning there would re-open the § P5 dismissal problem to tell them
    // something they already knew. "The import did not create that one" is true at the level of the
    // count, which is the level this rule reasons at — not at the level of identity.
    let bearers_after = label_bearers(&result.roster);
    let outcomes = outcomes
        .into_iter()
        .map(|outcome| {
            let after = bearers_after.get(&outcome.label).copied().unwrap_or(0);
            let before = bearers_before.get(&outcome.label).copied().unwrap_or(0);
            let created_ambiguity = after > 1 && after > before;
            // Only rows this import actually WROTE. A skipped or failed account left the roster
            // untouched, so whatever its label's count is, this row did not move it.
            let wrote = matches!(
                outcome.outcome,
                ImportOutcome::Imported | ImportOutcome::Overwritten
            );
            if created_ambiguity && wrote {
                outcome.duplicate_label()
            } else {
                outcome
            }
        })
        .collect();

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
    /// This account is the target machine's ACTIVE one, and the import STAGED a credential
    /// into its per-account stash without adopting it into the canonical item (issue #1001).
    ///
    /// Orthogonal to [`outcome`](Self::outcome) rather than a fifth variant of it: the
    /// account genuinely WAS imported or overwritten — the roster entry and the stash both
    /// landed — so the four-way tally [`count_import_outcomes`] feeds the exit code stays
    /// exactly as it was. What this flag adds is that the landing is not the whole story.
    staged_not_adopted: bool,
    /// This import left the row's LABEL on more accounts than the target started with, and on
    /// more than one — so it CREATED (or deepened) a duplicate-label roster (issue #1005).
    ///
    /// Set after the whole merge rather than per-write, because "created" is a property of the
    /// finished roster: a per-write reading also fires on a collision the merge only passes
    /// through, such as an import that swaps two labels between accounts the target already has.
    ///
    /// A flag rather than a fifth [`ImportOutcome`] variant, for the same reason as
    /// [`staged_not_adopted`](Self::staged_not_adopted): the account genuinely WAS imported
    /// or overwritten — nothing was refused and nothing was renamed, because duplicate
    /// labels are an accepted state — so the four-way tally is untouched. What the flag adds
    /// is that the label the row names no longer resolves.
    duplicate_label: bool,
}

impl AccountImport {
    /// One report row in its plain form: an outcome and the label it happened to, with every
    /// orthogonal flag off. The flags are opt-in builders below, so a new one cannot be
    /// silently forgotten by one of the four constructors.
    fn new(label: &str, outcome: ImportOutcome) -> Self {
        Self {
            label: label.to_owned(),
            outcome,
            staged_not_adopted: false,
            duplicate_label: false,
        }
    }
    fn imported(label: &str) -> Self {
        Self::new(label, ImportOutcome::Imported)
    }
    fn skipped(label: &str) -> Self {
        Self::new(label, ImportOutcome::Skipped)
    }
    fn overwritten(label: &str) -> Self {
        Self::new(label, ImportOutcome::Overwritten)
    }
    fn failed(label: &str) -> Self {
        Self::new(label, ImportOutcome::Failed)
    }

    /// Mark this row as the target's active account whose credential was staged but not
    /// adopted (issue #1001) — see the field docs for why this is a flag, not a variant.
    fn staged_not_adopted(mut self) -> Self {
        self.staged_not_adopted = true;
        self
    }

    /// Mark this row as the one that put a second bearer of its label on the roster
    /// (issue #1005) — see the field docs for why this is a flag, not a variant.
    fn duplicate_label(mut self) -> Self {
        self.duplicate_label = true;
        self
    }
}

/// The non-adoption notice (issue #1001): the artifact carried the account this machine is
/// currently logged into, so its credential was written to that account's own stash — which
/// is NOT the item Claude Code reads for the live session.
///
/// Calls that item "the shared login" — the operator-facing noun `status`'s fault lines
/// ([`render_canonical_scrub`] / [`render_keychain_locked`]) and the menu-bar panel use, and
/// the surfaces an operator acting on this note meets next. Not "canonical": that name is for
/// [`crate::error`]'s canary refusals, which address someone inspecting the keychain item.
///
/// Names the FORCING form, `use --force <label>`, and that is load-bearing rather than
/// decorative. Unqualified `use <label>` short-circuits on service-name equality —
/// `if account.stash() == active_stash { return Ok(GateOutcome::AlreadyActive); }` in
/// `SwapTarget::resolve` — a comparison of service NAMES, never of contents, pinned by
/// `already_active_without_force_is_a_noop_success_with_zero_writes` in
/// [`crate::use_account`] (`canonical` unchanged, zero writes). Naming it would leave the
/// canonical holding the stale token while both `import` and `use` reported success:
/// the original defect, reproduced through its own remediation (PRD AC-2a).
///
/// Deliberately does NOT assert what that item currently holds. The obvious wording — "the
/// shared login still holds the pre-import token" — is FALSE in a state this path genuinely
/// reaches: when the canonical has been scrubbed, [`resolve_active_uuid_for_import`]
/// still resolves the active account from the `~/.claude.json` display, so the notice fires
/// with no canonical item to hold anything. The instruction survives that state unchanged
/// (`use --force` adopts against an absent canonical through the #212 recovery path), so the
/// line says what was and was not written, and leaves the canonical's contents to `status`.
///
/// Non-secret by construction: the account's LABEL only, never a token, uuid, or email
/// (issue #15 / C-3).
fn non_adoption_notice(label: &str) -> String {
    format!(
        "note: `{label}` is this machine's active account. Its credential was staged into \
         its own stash, but NOT adopted into the shared login — and the shared login is the \
         one Claude Code reads, so the live session is unchanged.\n      \
         Run `sessiometer use --force {label}` to adopt it. `--force` is required: without \
         it, `use` sees the account is already active and writes nothing."
    )
}

/// The duplicate-label notice (issue #1005): this import put a second account under a label
/// that another account already carries, so the label no longer resolves.
///
/// Says what happened, why it is not an error, and exactly how to act — in that order, because
/// an operator who reads only the first clause must not conclude the import failed. Duplicate
/// labels are an accepted state (`Config::validate` has no duplicate-label arm, deliberately),
/// so nothing was refused and nothing was renamed; what changed is that every label-resolving
/// site now REFUSES this label rather than guessing (`Error::UseTargetAmbiguous`).
///
/// Names the account-uuid as the remedy rather than a flag, because there is no
/// disambiguator flag: `resolve_target` matches label OR account-uuid, and passing the uuid
/// is the whole mechanism by which a refusal is actionable (design § 4.3, option (iii) not
/// chosen). Points at `list` rather than printing a uuid here — `list` shows the FULL uuid for
/// exactly this copy-into-the-next-command purpose (issue #69), and this way the notice stays
/// one line per label instead of enumerating every bearer.
///
/// Says "your own handles" where the docs say "operator handles": the reader of this line IS
/// the operator, and the second half already addresses them as `you`. The rest of the CLI's
/// output never uses the word "operator" at all.
///
/// Non-secret by construction: the account's LABEL only, never a token, uuid, or email
/// (issue #15 / C-3).
fn duplicate_label_notice(label: &str) -> String {
    format!(
        "note: `{label}` now labels more than one account. Labels are your own handles and are \
         not required to be unique, so the import neither refused anything nor renamed anything \
         — but a label matching more than one account no longer RESOLVES: `use`, `poke`, \
         `enable`, `disable` and `remove` all refuse it rather than guess.\n      \
         Run `sessiometer list` to see the bearers with their account-uuids, then pass an \
         account-uuid anywhere you would have passed `{label}`."
    )
}

/// Render the per-account import report: one `outcome \`label\`` line per account, then a
/// count summary, then the trailing notices — the duplicate-label notice for each label this
/// import duplicated (issue #1005), then the non-adoption notice for the active account when
/// the import staged one (issue #1001). Labels only (non-secret); no token or email ever
/// appears. Returned as a String so it is unit-testable and the caller prints it.
///
/// Duplicate-label FIRST, and the order is load-bearing rather than cosmetic: the non-adoption
/// notice instructs `use --force <label>`, and when that same label is one this import
/// duplicated, that instruction is itself now a refusal. Reading the duplicate notice first is
/// what makes the following instruction legible — it is the notice that says to substitute an
/// account-uuid *anywhere* the label would have gone, which includes the line below it.
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
    // Set off from the tally by a BLANK line (as the `status` verbose block separates itself):
    // these are the lines of the report the operator has to act on, and butted against the
    // counts they read as more tally.
    //
    // Issue #1005: one notice per DISTINCT duplicated label. A three-way collision flags two
    // rows carrying the same label, and that is one problem for the operator, not two — while a
    // single import that duplicates two different labels is genuinely two. Dedupe on
    // `HashSet::insert`'s was-it-new return — the primitive `Config::validate` already uses for
    // its duplicate-uuid check. The SET is unordered but the iteration is not: it walks
    // `outcomes`, so the notices still come out in roster order.
    let mut warned = std::collections::HashSet::new();
    for entry in outcomes.iter().filter(|entry| entry.duplicate_label) {
        if warned.insert(entry.label.as_str()) {
            out.push_str("\n\n");
            out.push_str(&duplicate_label_notice(&entry.label));
        }
    }
    // At most one account can be active, but the loop keeps the renderer total rather than
    // making it assert a cardinality the type does not carry.
    for entry in outcomes.iter().filter(|entry| entry.staged_not_adopted) {
        out.push_str("\n\n");
        out.push_str(&non_adoption_notice(&entry.label));
    }
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
    /// The account's most recent persisted [`RefreshEventOutcomeKind`], or `None` when the
    /// event log records no refresh for it (the common case while the opt-in `[refresh]`
    /// tick, #105, is off).
    pub(crate) last_refresh: Option<RefreshEventOutcomeKind>,
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
/// the event log writes ([`RefreshEventOutcomeKind::as_str`]) so it cross-references a
/// `sessiometer.log` the operator may grep. A `dead` outcome trails the actionable
/// `claude /login` cue — the offline echo of `status`'s dead-credential cue (#119) —
/// since a daemon-down `list` is exactly where an operator meets a dead refresh token.
fn refresh_tag(last_refresh: Option<RefreshEventOutcomeKind>) -> Option<String> {
    let outcome = last_refresh?;
    let mut tag = format!("last refresh: {}", outcome.as_str());
    if outcome == RefreshEventOutcomeKind::Dead {
        // The exact command `status`'s health cell prints (#119), so both views point
        // an operator at the same fix.
        tag.push_str(" — claude /login");
    }
    Some(tag)
}

/// `disable`/`enable <account>` — take an account out of the rotation, or return it
/// (issue #36). A reversible park, distinct from removal (#13): the account keeps
/// its roster entry and its stash; only its `enabled` flag flips. Resolve the
/// account by its non-secret label, set the flag, and persist via [`Config::save`]
/// so the change survives a daemon restart (config-backed). A running daemon is
/// notified to reload (#139), so the flip takes effect in the live rotation without
/// a restart (best-effort — no daemon running is a no-op, the next start loads it).
///
/// A missing `<account>` is [`Error::RotationLabelRequired`]; one matching no account is
/// [`Error::UseTargetNotFound`] and one matching several (a duplicated label) is
/// [`Error::UseTargetAmbiguous`] — the shared `use`/`poke`/daemon taxonomy, since issue #1005
/// routed this verb through [`resolve_target`](crate::use_account::resolve_target). `enabled`
/// selects the verb so one body serves both subcommands; the `verb` it derives names the usage
/// in errors.
async fn set_enabled(query: Option<String>, enabled: bool) -> Result<()> {
    let verb = if enabled { "enable" } else { "disable" };
    let query = query.ok_or(Error::RotationLabelRequired { verb })?;
    let mut config = Config::load()?;
    let (outcome, label) = apply_enabled(&mut config.roster, &query, enabled)?;
    // Only rewrite config.toml when the flag actually changed — re-disabling an
    // already-parked account is a friendly no-op, not a needless disk write.
    if matches!(outcome, FlipOutcome::Changed) {
        config.save()?;
        // Tell a running daemon to pick up the enable/disable now (#139) — best-effort;
        // the account joins / leaves the live rotation without a restart. Skipped on a
        // no-op flip (nothing changed on disk, so nothing to reload).
        crate::capture::notify_daemon_roster_reload().await;
    }
    // Name the RESOLVED account, not `query` — which since #1005 may be an account-uuid, and
    // echoing 36 hex characters back would name nothing the operator typed or recognizes. WHICH
    // of the two handles in scope this hands over is pinned by
    // `the_confirmations_name_the_resolved_handle` (issue #1088): the choice is made here in the
    // I/O shell, where no unit test reaches it, and swapping it survived the whole suite.
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

/// Resolve `query` in `roster` and set its `enabled` flag, reporting whether the
/// value actually changed. Pure (no I/O) so the resolve-and-flip policy is unit-
/// testable without touching `config.toml`; the caller persists only on
/// [`FlipOutcome::Changed`].
///
/// Resolution is [`resolve_target`](crate::use_account::resolve_target) — the SAME resolver
/// `use`, `poke` and the daemon's control-socket swap use (issue #1005, OQ-1). So `query` is a
/// label OR an account-uuid, an unmatched one is [`Error::UseTargetNotFound`], and a
/// DUPLICATED label is [`Error::UseTargetAmbiguous`] rather than a silent first-match.
///
/// Labels are still operator handles and uniqueness is still not enforced — duplicate labels
/// remain an accepted roster state. What changed is that this verb no longer GUESSES which
/// bearer was meant: it refuses, and the account-uuid `query` also accepts is the remedy. That
/// this verb previously took the earliest entry while `use` refused was the inconsistency #1005
/// closes, and `remove` — which shared this shape and deletes keychain material — is why it was
/// closed toward refusing.
///
/// Returns the resolved account's LABEL alongside the outcome, so the caller's confirmation
/// names the account rather than echoing `query` back — which since #1005 may be an
/// account-uuid. `use` already reads its confirmation off the resolved account rather than the
/// query; this keeps the four verbs saying the same thing.
fn apply_enabled(
    roster: &mut [Account],
    query: &str,
    enabled: bool,
) -> Result<(FlipOutcome, String)> {
    let account = &mut roster[crate::use_account::resolve_target(roster, query)?];
    let label = account.label.clone();
    if account.enabled == enabled {
        Ok((FlipOutcome::Unchanged, label))
    } else {
        account.enabled = enabled;
        Ok((FlipOutcome::Changed, label))
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

/// `remove <account>` — the DESTRUCTIVE sibling of `disable` (issue #13): drop the
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
/// A missing `<account>` is [`Error::RotationLabelRequired`]; one matching no account is
/// [`Error::UseTargetNotFound`] and one matching several (a duplicated label) is
/// [`Error::UseTargetAmbiguous`] — since issue #1005 this verb resolves through the shared
/// [`resolve_target`](crate::use_account::resolve_target), so it REFUSES a duplicated label
/// rather than deleting the earliest bearer's stash. A running daemon is notified to reload
/// (#139), so the removal takes effect in the live rotation without a restart
/// (best-effort). Removing the ACTIVE account is
/// allowed and self-heals: this touches only sessiometer's roster entry and stash,
/// never the canonical credential, so the daemon simply polls-only (resolving no
/// active account) until another account is captured or the operator `/login`s.
async fn remove_account(query: Option<String>) -> Result<()> {
    let query = query.ok_or(Error::RotationLabelRequired { verb: "remove" })?;
    let mut config = Config::load()?;
    let removed = apply_remove(&mut config.roster, &query)?;
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
    // Name the REMOVED account's label, not `query` — which since #1005 may be an
    // account-uuid, and echoing that back would not tell the operator which handle went. Pinned
    // by `the_confirmations_name_the_resolved_handle` (issue #1088), for the same reason as the
    // flip above: the choice is made here in the I/O shell, where no unit test reaches it.
    println!("{}", remove_confirmation(&removed.label));
    Ok(())
}

/// Resolve `query` in `roster` and REMOVE its entry, returning the removed account
/// (whose `stash` name the caller needs to delete the keychain stash). Pure (no
/// I/O) so the resolve-and-remove policy is unit-testable without touching
/// `config.toml`.
///
/// Resolution is [`resolve_target`](crate::use_account::resolve_target), shared with `use`,
/// `poke`, the daemon's control-socket swap and [`apply_enabled`] (issue #1005, OQ-1): `query`
/// is a label OR an account-uuid, an unmatched one is [`Error::UseTargetNotFound`], and a
/// DUPLICATED label is [`Error::UseTargetAmbiguous`].
///
/// This verb is WHY OQ-1 resolved toward refusing. It is the only label-resolving site whose
/// wrong resolution is irreversible — [`remove_account`] follows this call by deleting the
/// resolved account's keychain stash — so under the previous first-match-wins an operator
/// clearing up a duplicate label by removing "the extra one" could destroy the wrong account's
/// credential material, with no undo, while `use` on that same roster refused. The other three
/// verbs' wrong resolutions cost one command to reverse; this one costs a re-login.
fn apply_remove(roster: &mut Vec<Account>, query: &str) -> Result<Account> {
    let idx = crate::use_account::resolve_target(roster, query)?;
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
        AccountStatusLine, BlindActive, BlindPreemptSwap, LandingOvershoot, NextSwap, NoTargetCause,
    };
    // The shared framing vocabulary (issues #918, #1123) — the same list `stats.rs` scans
    // against, minus the mechanical-operation verbs each operator-facing surface legitimately
    // spends. One scanner per audience; the exemption sets are measured, not inherited.
    use crate::framing_vocabulary::{
        help_banned_tokens, scan_advisory_banned, scan_banned, scan_help_banned, scan_usage_banned,
        scan_with, ADVISORY_EXEMPT_TOKENS, BANNED_PHRASES, USAGE_EXEMPT_TOKENS,
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
    fn auth(
        expires_at_ms: Option<i64>,
        last_refresh: Option<RefreshEventOutcomeKind>,
    ) -> AuthSubset {
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
                session_ceiling: 95,
                monitor_401_n: 3,
                // `list` reads no timing strategies; default jitter is a fine
                // placeholder (issue #38).
                ..Tunables::default()
            },
            refresh: crate::config::RefreshConfig::default(),
            login: crate::config::LoginConfig::default(),
            stats: crate::config::StatsConfig::default(),
            migration: crate::config::MigrationConfig::default(),
            credential: crate::config::CredentialConfig::default(),
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
            &[auth(Some(7_200_000), Some(RefreshEventOutcomeKind::Dead))],
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
            &[auth(
                Some(7_200_000),
                Some(RefreshEventOutcomeKind::Refreshed),
            )],
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
            &[auth(
                Some(7_200_000),
                Some(RefreshEventOutcomeKind::NoChange),
            )],
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
            refresh_tag(Some(RefreshEventOutcomeKind::Refreshed)).as_deref(),
            Some("last refresh: refreshed")
        );
        assert_eq!(
            refresh_tag(Some(RefreshEventOutcomeKind::RefreshedNotReStashed)).as_deref(),
            Some("last refresh: refreshed_not_restashed")
        );
        assert_eq!(
            refresh_tag(Some(RefreshEventOutcomeKind::Dead)).as_deref(),
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
            systemic_refresh_source: None,
            canonical_scrub: None,
            keychain_locked: false,
            canary: None,
            expiry_cohort: None,
            recent_blind_preempt_swap: None,
            recent_landing_overshoot: None,
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
        let sourced = |systemic, source| StatusResponse {
            systemic_refresh_failure: systemic,
            systemic_refresh_source: source,
            canonical_scrub: None,
            keychain_locked: false,
            canary: None,
            expiry_cohort: None,
            recent_blind_preempt_swap: None,
            recent_landing_overshoot: None,
            refresh_enabled: Some(true),
            accounts: vec![status_line("work", true, Some(50), Some(25))],
            next_swap: None,
        };
        // A pre-#813 daemon sends no discriminant; the count-only frames below are exactly what it
        // puts on the wire, so they double as this render's legacy-compat case.
        let response = |systemic| sourced(systemic, None);

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

        // #575: with the colour gate OPEN, the systemic line carries the `Yellow` band — its
        // "act at your next break" rank (pre-death) — and NOT the `Red` it wore before #575 (which
        // ranked it, wrongly, ABOVE the act-now vault pair). Colour only augments; the plain text is
        // unchanged under `--no-color`.
        let down_colored = render_status(&response(Some(3)), NOW, None, true)
            .lines()
            .find(|l| l.contains("refresh mechanism: DOWN"))
            .expect("the mechanism-down line is present under --color")
            .to_owned();
        assert!(
            down_colored.contains(&format!("\x1b[{}m", Severity::Yellow.sgr())),
            "systemic is tinted Yellow (next-break rank): {down_colored:?}"
        );
        assert!(
            !down_colored.contains(&format!("\x1b[{}m", Severity::Red.sgr())),
            "systemic is NOT Red — it must not outrank the act-now vault pair (#575): {down_colored:?}"
        );

        // Issue #813 AC2 — a SWEEP-opened episode renders exactly as it did before the discriminant
        // existed. TWO legs, because they prove different things. The FROZEN literal is the line
        // this renderer printed at `9cb9248`, so it catches a re-wrap of the `\`-continued format
        // string (which strips the newline AND the leading indent — re-indenting is safe, re-wrapping
        // is not); a self-comparison could not. The equality after it pins the other half: an
        // explicit `sweep` source and a pre-#813 count-only frame stay indistinguishable.
        let swept = render_status(
            &sourced(Some(3), Some(SystemicRefreshSource::Sweep)),
            NOW,
            None,
            false,
        );
        assert_eq!(
            swept.lines().find(|l| l.contains("refresh mechanism: DOWN")),
            Some(
                "refresh mechanism: DOWN — 3 consecutive sweeps failed for every eligible account; \
                 the mechanism is failing, not one account (check the daemon log and the [refresh] \
                 claude binary)"
            ),
            "#813 AC2: the sweep line is byte-identical to the pre-#813 one"
        );
        assert_eq!(
            swept,
            render_status(&response(Some(3)), NOW, None, false),
            "#813 AC2: an explicit `sweep` source renders as a pre-#813 count-only frame does"
        );

        // Issue #813 AC1 — a PREFLIGHT-opened episode must not claim a sweep ran. Zero sweeps have
        // run when the startup preflight opens the episode; the count is a seeded floor of 1 kept
        // only so a pre-#813 client stays grammatical, so this arm cites the preflight instead.
        let pre = render_status(
            &sourced(Some(1), Some(SystemicRefreshSource::Preflight)),
            NOW,
            None,
            false,
        );
        let pre_line = pre
            .lines()
            .find(|l| l.contains("refresh mechanism: DOWN"))
            .expect("the mechanism-down verdict still fires on a preflight-opened episode");
        assert!(
            !pre_line.contains("sweep"),
            "#813 AC1: no sweep is asserted on the preflight arm: {pre_line}"
        );
        assert!(
            !pre_line.contains('1'),
            "#813 AC1: the seeded count is not cited as evidence either: {pre_line}"
        );
        assert!(
            pre_line.contains("startup preflight could not resolve the claude binary"),
            "#813 AC1: it names what was actually observed: {pre_line}"
        );
        // The verdict and the remedy are unchanged — only the evidence clause differs, so the two
        // arms stay content-parallel (and the menu-bar surfaces can mirror the same split).
        assert!(
            pre_line.contains("the mechanism is failing, not one account")
                && pre_line.contains("[refresh] claude binary"),
            "the DOWN verdict and remedy survive the rephrasing: {pre_line}"
        );
        // #15/#444: the provenance is a fixed token, so the preflight arm leaks no path or secret
        // even though its subject IS a binary location.
        assert!(
            crate::redaction::meter::unauthored_emails(&pre, &[]).is_empty()
                && !pre.to_lowercase().contains("token")
                && !pre_line.contains('/'),
            "#813 AC4: a class, never a resolved path or secret: {pre:?}"
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
            systemic_refresh_source: None,
            canonical_scrub: scrub,
            keychain_locked: false,
            canary: None,
            expiry_cohort: None,
            recent_blind_preempt_swap: None,
            recent_landing_overshoot: None,
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

        // #575: with the colour gate OPEN, `Exhausted` carries the act-now `Red` band (rank 2, the
        // same band as `keychain_locked`), while `Recovering` stays PLAIN — calm, may self-heal, so
        // colouring it would cry wolf. Both plain texts are unchanged under `--no-color`.
        let exhausted_colored =
            render_status(&response(Some(CanonicalScrub::Exhausted)), NOW, None, true)
                .lines()
                .find(|l| l.contains("shared login: scrubbed"))
                .expect("the scrubbed line is present under --color")
                .to_owned();
        assert!(
            exhausted_colored.contains(&format!("\x1b[{}m", Severity::Red.sgr())),
            "exhausted is tinted Red (act-now rank): {exhausted_colored:?}"
        );
        let recovering_colored =
            render_status(&response(Some(CanonicalScrub::Recovering)), NOW, None, true)
                .lines()
                .find(|l| l.contains("shared login: scrubbed"))
                .expect("the recovering line is present under --color")
                .to_owned();
        assert!(
            !recovering_colored.contains('\x1b'),
            "recovering stays PLAIN even under --color (calm, would cry wolf): {recovering_colored:?}"
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
            systemic_refresh_source: None,
            canonical_scrub: None,
            keychain_locked: locked,
            canary: None,
            expiry_cohort: None,
            recent_blind_preempt_swap: None,
            recent_landing_overshoot: None,
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

        // #575: with the colour gate OPEN, the keychain-locked line carries the act-now `Red` band
        // (rank 1) — the vault is UNREADABLE, the operator is blocked NOW.
        let locked_colored = render_status(&response(true), NOW, None, true)
            .lines()
            .find(|l| l.contains("shared login: unreadable"))
            .expect("the keychain-locked line is present under --color")
            .to_owned();
        assert!(
            locked_colored.contains(&format!("\x1b[{}m", Severity::Red.sgr())),
            "keychain-locked is tinted Red (act-now rank): {locked_colored:?}"
        );

        // #15/#444: no secret reaches the rendered state (a bare state discriminant only).
        assert!(
            crate::redaction::meter::unauthored_emails(&locked, &[]).is_empty()
                && !locked.to_lowercase().contains("token"),
            "no secret reaches the keychain-locked surface (#15/#444): {locked:?}"
        );
    }

    #[test]
    fn render_status_surfaces_the_canary_alarms_and_keeps_the_quiet_verdicts_silent() {
        // Issue #714: the behavioral-canary ALARM verdicts get a dedicated DATA line — a refusing
        // drift names both accounts + the override remedy at act-now `Red`; an overridden drift
        // names the standing alarm + that writes proceed, at next-break `Yellow`; an ambiguous
        // resolution names the count at `Red` with no override. The quiet verdicts (`ok`,
        // `inconclusive`, `not_found`) and a pre-#714 `None` print NOTHING — `ok` is the quiet
        // normal, and `not_found` is already voiced by the scrub/locked machinery. #15-clean:
        // labels + a count only.
        let response = |canary| StatusResponse {
            systemic_refresh_failure: None,
            systemic_refresh_source: None,
            canonical_scrub: None,
            keychain_locked: false,
            canary,
            expiry_cohort: None,
            recent_blind_preempt_swap: None,
            recent_landing_overshoot: None,
            refresh_enabled: Some(true),
            accounts: vec![status_line("work", true, Some(60), Some(25))],
            next_swap: None,
        };
        let drift = |overridden| {
            Some(CanaryStatus::Drift {
                displayed: "work".to_owned(),
                matched: "spare".to_owned(),
                overridden,
            })
        };

        // REFUSING drift → both labels + the refusal + the documented override remedy.
        let refusing = render_status(&response(drift(false)), NOW, None, false);
        let line = refusing
            .lines()
            .find(|l| l.contains("keychain canary: drift"))
            .expect("the refusing drift line is present");
        assert!(
            line.contains("belongs to spare")
                && line.contains("work is named active")
                && line.contains("credential writes are refused")
                && line.contains("canary_drift_override"),
            "the refusing drift names accounts, refusal, and remedy: {line}"
        );

        // OVERRIDDEN drift → the standing alarm, and that writes proceed logged.
        let overridden = render_status(&response(drift(true)), NOW, None, false);
        let line = overridden
            .lines()
            .find(|l| l.contains("keychain canary: drift"))
            .expect("the overridden drift line is present");
        assert!(
            line.contains("canary_drift_override is set") && line.contains("swaps proceed"),
            "the overridden drift says writes proceed under the override: {line}"
        );
        assert!(
            !line.contains("refused"),
            "an overridden drift does not claim refusal: {line}"
        );

        // AMBIGUOUS → the count + the refusal, no override remedy (there is none).
        let ambiguous = render_status(
            &response(Some(CanaryStatus::Ambiguous { count: 2 })),
            NOW,
            None,
            false,
        );
        let line = ambiguous
            .lines()
            .find(|l| l.contains("keychain canary: ambiguous"))
            .expect("the ambiguous line is present");
        assert!(
            line.contains("2 Claude Code-credentials items")
                && line.contains("credential writes are refused"),
            "ambiguous names the count and the refusal: {line}"
        );
        assert!(
            !line.contains("canary_drift_override"),
            "ambiguity has no override: {line}"
        );

        // Issue #738 — the #730 refuse, finally voiced. Names the evidence, the refusal, and its
        // OWN override (never the drift one, which cannot clear this case).
        let unparseable = render_status(
            &response(Some(CanaryStatus::RefusedUnparseableCanonical)),
            NOW,
            None,
            false,
        );
        let line = unparseable
            .lines()
            .find(|l| l.contains("keychain canary: unrecognized credential"))
            .expect("the unparseable-canonical refusal line is present");
        assert!(
            line.contains("matches no stashed account")
                && line.contains("not in Claude Code's own format")
                && line.contains("credential writes are refused")
                && line.contains("canary_nostashmatch_override"),
            "the refusal names the evidence, the refusal, and its own remedy: {line}"
        );
        assert!(
            !line.contains("canary_drift_override"),
            "the unparseable refusal must not name the DRIFT override — a separate switch that \
             cannot clear this case: {line}"
        );

        // The QUIET verdicts (and a pre-#714 daemon omitting the field) print nothing.
        for quiet in [
            Some(CanaryStatus::Ok),
            Some(CanaryStatus::Inconclusive),
            Some(CanaryStatus::NotFound),
            None,
        ] {
            assert!(
                !render_status(&response(quiet.clone()), NOW, None, false)
                    .contains("keychain canary"),
                "the quiet verdict {quiet:?} prints no canary line"
            );
        }

        // ADR-0026 colour bands: refusing drift + ambiguous wear act-now `Red`; the overridden
        // drift wears next-break `Yellow`. All three plain texts are DATA (asserted above with the
        // gate closed).
        for (canary, sgr) in [
            (drift(false), Severity::Red.sgr()),
            (
                Some(CanaryStatus::Ambiguous { count: 2 }),
                Severity::Red.sgr(),
            ),
            (drift(true), Severity::Yellow.sgr()),
            (
                Some(CanaryStatus::RefusedUnparseableCanonical),
                Severity::Red.sgr(),
            ),
        ] {
            let colored = render_status(&response(canary), NOW, None, true)
                .lines()
                .find(|l| l.contains("keychain canary"))
                .expect("the canary line is present under --color")
                .to_owned();
            assert!(
                colored.contains(&format!("\x1b[{sgr}m")),
                "the canary line carries its rank band: {colored:?}"
            );
        }

        // #15/#444: no secret reaches any rendered canary surface.
        for out in [&refusing, &overridden, &ambiguous, &unparseable] {
            assert!(
                crate::redaction::meter::unauthored_emails(out, &[]).is_empty()
                    && !out.to_lowercase().contains("token"),
                "no secret reaches the canary surface (#15/#444): {out:?}"
            );
        }
    }

    #[test]
    fn daemon_fault_severity_ranks_the_vault_pair_above_systemic_cross_surface() {
        // #575 — the acceptance test: the three daemon-level payload faults must rank the SAME way on
        // the `status` CLI as on the menubar panel (R-2). The vault pair blocks NOW (act-now `Red`,
        // panel `.error`); the systemic mechanism is pre-death (next-break `Yellow`, panel `.warning`);
        // a recovering scrub is calm (plain, panel `.info`). Before #575 the CLI inverted this —
        // systemic wore the only `Red` while the vault pair sat plain, ranking the LEAST-blocking fault
        // loudest. The rank now lives in ONE place (`DaemonPayloadFault::severity`).
        assert_eq!(
            DaemonPayloadFault::KeychainLocked.severity(),
            Some(Severity::Red),
            "keychain-locked is act-now Red (rank 1)"
        );
        assert_eq!(
            DaemonPayloadFault::CanonicalScrubExhausted.severity(),
            Some(Severity::Red),
            "scrub-exhausted is act-now Red (rank 2)"
        );
        assert_eq!(
            DaemonPayloadFault::SystemicRefreshFailure.severity(),
            Some(Severity::Yellow),
            "systemic is next-break Yellow (rank 3) — strictly below the vault pair"
        );
        assert_eq!(
            DaemonPayloadFault::CanonicalScrubRecovering.severity(),
            None,
            "recovering is calm — no colour (rank 4)"
        );
        // #714: the canary REFUSAL pair joins the act-now band (credential writes are blocked
        // NOW), while an operator-OVERRIDDEN drift drops to the next-break band (writes proceed,
        // the standing alarm deserves a re-check, not an act-now claim).
        assert_eq!(
            DaemonPayloadFault::CanaryDriftRefusing.severity(),
            Some(Severity::Red),
            "a refusing canary drift is act-now Red"
        );
        assert_eq!(
            DaemonPayloadFault::CanaryAmbiguous.severity(),
            Some(Severity::Red),
            "an ambiguous canary resolution is act-now Red"
        );
        assert_eq!(
            DaemonPayloadFault::CanaryDriftOverridden.severity(),
            Some(Severity::Yellow),
            "an overridden canary drift is next-break Yellow"
        );
        // #738: the unparseable-canonical refusal makes the act-now canary band a TRIO. It has
        // no overridden VARIANT to rank apart — the override collapses the wire verdict back to
        // the quiet `inconclusive`, so no fault ever reaches `severity()` for that case.
        assert_eq!(
            DaemonPayloadFault::CanaryRefusedUnparseableCanonical.severity(),
            Some(Severity::Red),
            "an unparseable-canonical refusal is act-now Red — it blocks writes NOW, exactly \
             like its two #714 siblings"
        );

        // Rendered together (a locked keychain makes the refresh mechanism fail too, so they CO-OCCUR):
        // the operator reading the colour sees the vault pair Red ABOVE the systemic Yellow — no longer
        // the pre-#575 inversion.
        let response = StatusResponse {
            systemic_refresh_failure: Some(5),
            systemic_refresh_source: None,
            canonical_scrub: None,
            keychain_locked: true,
            canary: None,
            expiry_cohort: None,
            recent_blind_preempt_swap: None,
            recent_landing_overshoot: None,
            refresh_enabled: Some(true),
            accounts: vec![status_line("work", true, Some(60), Some(25))],
            next_swap: None,
        };
        let out = render_status(&response, NOW, None, true);
        let vault = out
            .lines()
            .find(|l| l.contains("shared login: unreadable"))
            .expect("keychain line present");
        let systemic = out
            .lines()
            .find(|l| l.contains("refresh mechanism: DOWN"))
            .expect("systemic line present");
        assert!(
            vault.contains(&format!("\x1b[{}m", Severity::Red.sgr())),
            "vault fault is Red (act-now): {vault:?}"
        );
        assert!(
            systemic.contains(&format!("\x1b[{}m", Severity::Yellow.sgr()))
                && !systemic.contains(&format!("\x1b[{}m", Severity::Red.sgr())),
            "systemic is Yellow, never Red — the vault pair outranks it (#575): {systemic:?}"
        );
        // Worst-first PRINT order too: the act-now vault line sits ABOVE the next-break systemic line.
        // The colour asserts above would still pass under the pre-#575 push order (each line keeps its
        // own correct colour even if systemic-Yellow printed first), so guard the order explicitly — as
        // the sibling test does for the systemic↔recovering adjacency.
        let vault_at = out
            .find("shared login: unreadable")
            .expect("keychain line present");
        let systemic_at = out
            .find("refresh mechanism: DOWN")
            .expect("systemic line present");
        assert!(
            vault_at < systemic_at,
            "the act-now vault fault (rank 1) must print ABOVE the next-break systemic (rank 3), \
             mirroring the panel's worst-first rank; got vault@{vault_at} systemic@{systemic_at}\n{out}"
        );
    }

    #[test]
    fn render_status_prints_the_calm_recovering_scrub_below_the_systemic_warning() {
        // #575 print-order corollary: `canonical_scrub = recovering` is rank 4 (calm, plain) and MUST
        // sit BELOW `systemic_refresh_failure` (rank 3, Yellow) when they co-occur — the panel's
        // `daemonFaultBanner` ranks recovering LAST for exactly this reason (a self-healing "no action
        // needed" state can never outrank one that cannot self-heal; the panel's load-bearing subtlety).
        // The CLI prints ALL fault lines, so the cross-surface rank shows up as print ORDER here:
        // systemic must appear ABOVE the recovering line, never the reverse. Guards this against a
        // regression to a single fixed canonical-scrub slot.
        let response = StatusResponse {
            systemic_refresh_failure: Some(3),
            systemic_refresh_source: None,
            canonical_scrub: Some(CanonicalScrub::Recovering),
            keychain_locked: false,
            canary: None,
            expiry_cohort: None,
            recent_blind_preempt_swap: None,
            recent_landing_overshoot: None,
            refresh_enabled: Some(true),
            accounts: vec![status_line("work", true, Some(60), Some(25))],
            next_swap: None,
        };
        let out = render_status(&response, NOW, None, true);
        let systemic_at = out
            .find("refresh mechanism: DOWN")
            .expect("systemic line present");
        let recovering_at = out
            .find("recovering automatically")
            .expect("recovering scrub line present");
        assert!(
            systemic_at < recovering_at,
            "the pre-death systemic warning (rank 3) must print ABOVE the calm recovering scrub \
             (rank 4), mirroring the panel's worst-first rank; got systemic@{systemic_at} \
             recovering@{recovering_at}\n---\n{out}"
        );
        // The calm recovering line stays PLAIN (rank 4 = `None`): colouring it would cry wolf.
        let recovering = out
            .lines()
            .find(|l| l.contains("recovering automatically"))
            .expect("recovering line present");
        assert!(
            !recovering.contains('\x1b'),
            "recovering scrub is calm/plain — no SGR: {recovering:?}"
        );
        let systemic = out
            .lines()
            .find(|l| l.contains("refresh mechanism: DOWN"))
            .expect("systemic line present");
        assert!(
            systemic.contains(&format!("\x1b[{}m", Severity::Yellow.sgr())),
            "systemic is Yellow (rank 3): {systemic:?}"
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
            systemic_refresh_source: None,
            canonical_scrub: None,
            keychain_locked: false,
            canary: None,
            expiry_cohort: None,
            recent_blind_preempt_swap: None,
            recent_landing_overshoot: None,
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
                systemic_refresh_source: None,
                canonical_scrub: None,
                keychain_locked: false,
                canary: None,
                expiry_cohort: None,
                recent_blind_preempt_swap: None,
                recent_landing_overshoot: None,
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
                systemic_refresh_source: None,
                canonical_scrub: None,
                keychain_locked: false,
                canary: None,
                expiry_cohort: None,
                recent_blind_preempt_swap: None,
                recent_landing_overshoot: None,
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
            systemic_refresh_source: None,
            canonical_scrub: None,
            keychain_locked: false,
            canary: None,
            expiry_cohort: None,
            recent_blind_preempt_swap: None,
            recent_landing_overshoot: None,
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
            systemic_refresh_source: None,
            canonical_scrub: None,
            keychain_locked: false,
            canary: None,
            expiry_cohort: None,
            recent_blind_preempt_swap: None,
            recent_landing_overshoot: None,
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
        // names the source, the stale last-known %, that the fleet is out of capacity + when it
        // returns (folded in from the no-target relief), and the operator remedy — and SUPPRESSES
        // both the separate blind-DEGRADED line and the `next swap: none — …` footer, which split
        // read as two unrelated observations.
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
                && out.contains("out of capacity, resets in 2d4h")
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
        // The relief instant is folded from `next_swap`, so the operator still sees WHEN capacity
        // returns — but WITHOUT the pre-#666 false universal (the `cause` names one spare's gating
        // dimension, not a fleet property, #665): any cause reads "out of capacity, resets in ⟨dur⟩".
        // An absent cause (pre-#405 daemon) falls back to the bare "no viable target" — each still
        // carrying the unconditional "add or free an account" remedy (cornered is always unresolvable).
        let session = render_status(
            &cornered_response(Some(NoTargetCause::Session), Some(NOW + 47 * 60)),
            NOW,
            None,
            false,
        );
        assert!(
            session.contains("out of capacity, resets in 47m")
                && !session.contains("over its session limit")
                && session.contains("add or free an account"),
            "session-cause cornered folds the relief without a false universal: {session}",
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
                && out.contains("next swap: none — out of capacity"),
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
            systemic_refresh_source: None,
            canonical_scrub: None,
            keychain_locked: false,
            canary: None,
            expiry_cohort: None,
            recent_blind_preempt_swap: Some(BlindPreemptSwap {
                from_label: "spare".to_owned(),
                to_label: "work".to_owned(),
                last_known_session_pct: 68,
            }),
            recent_landing_overshoot: None,
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
            recent_landing_overshoot: None,
            ..narrated
        };
        let out = render_status(&quiet, NOW, None, false);
        assert!(
            !out.contains("swapped off") && !out.contains("undo with"),
            "no line when there is no recent preemptive swap: {out}",
        );
    }

    #[test]
    fn render_status_surfaces_a_recent_landing_overshoot() {
        // Issue #613: a daemon-pushed `recent_landing_overshoot` renders a DATA line naming the parked
        // account, the fired-vs-landed spread, the SLO ceiling, and the single-machine caveat — the
        // local landing breach caught at runtime, the same breach the offline #595 landing SLI
        // reconstructs. The causal clause distinguishes the two breach classes the offline SLI splits:
        // a swap fired BELOW the SLO whose committed tail carried it over (the post-swap TAIL, here) vs
        // a swap that fired already AT/OVER the SLO (a GAP-CROSSING, below). Absent from the wire → no
        // line.
        let breached = StatusResponse {
            systemic_refresh_failure: None,
            systemic_refresh_source: None,
            canonical_scrub: None,
            keychain_locked: false,
            canary: None,
            expiry_cohort: None,
            recent_blind_preempt_swap: None,
            recent_landing_overshoot: Some(LandingOvershoot {
                from_label: "spare".to_owned(),
                decision_pct: 95,
                landing_pct: 99,
            }),
            refresh_enabled: None,
            accounts: vec![status_line("work", true, Some(20), Some(15))],
            next_swap: None,
        };
        let out = render_status(&breached, NOW, None, false);
        assert!(
            out.contains(
                "landing overshoot: spare swapped out at 95% but its parked session climbed to 99%"
            ) && out.contains("post-swap committed tail")
                && out.contains("single-machine signal"),
            "the on-target tail overshoot is surfaced with parked account + spread + caveat: {out}",
        );

        // GAP-CROSSING class: the swap fired ALREADY at/over the SLO (decision == ceiling), so the
        // parked account did not climb after the swap — the banner must NOT call it a post-swap tail.
        let gap_crossing = StatusResponse {
            systemic_refresh_failure: None,
            systemic_refresh_source: None,
            canonical_scrub: None,
            keychain_locked: false,
            canary: None,
            expiry_cohort: None,
            recent_blind_preempt_swap: None,
            recent_landing_overshoot: Some(LandingOvershoot {
                from_label: "spare".to_owned(),
                decision_pct: 99,
                landing_pct: 100,
            }),
            refresh_enabled: None,
            accounts: vec![status_line("work", true, Some(20), Some(15))],
            next_swap: None,
        };
        let out = render_status(&gap_crossing, NOW, None, false);
        assert!(
            out.contains(
                "landing overshoot: spare swapped out already over the 99 SLO at 99% and its parked \
                 session reached 100%"
            ) && out.contains("a late swap-out, not a post-swap tail")
                && !out.contains("post-swap committed tail")
                && out.contains("single-machine signal"),
            "the gap-crossing overshoot is labelled a late swap-out, not a tail: {out}",
        );

        // No recent overshoot on the wire → no line.
        let quiet = StatusResponse {
            recent_landing_overshoot: None,
            ..breached
        };
        let out = render_status(&quiet, NOW, None, false);
        assert!(
            !out.contains("landing overshoot:"),
            "no line when there is no recent landing overshoot: {out}",
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
            systemic_refresh_source: None,
            canonical_scrub: None,
            keychain_locked: false,
            canary: None,
            expiry_cohort: None,
            recent_blind_preempt_swap: None,
            recent_landing_overshoot: None,
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
            systemic_refresh_source: None,
            canonical_scrub: None,
            keychain_locked: false,
            canary: None,
            expiry_cohort: None,
            recent_blind_preempt_swap: None,
            recent_landing_overshoot: None,
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
            systemic_refresh_source: None,
            canonical_scrub: None,
            keychain_locked: false,
            canary: None,
            expiry_cohort: None,
            recent_blind_preempt_swap: None,
            recent_landing_overshoot: None,
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

    // --- status: the REFRESH-token EXPIRY column (issue #883) ------------------

    #[test]
    fn expiry_cell_classifies_each_horizon_and_reports_an_unobserved_deadline_as_a_gap() {
        // The four #878 states, projected. An OBSERVED deadline (`Within` / `Beyond`) renders the
        // compact time-until the `RESET` cells already use; a `Lapsed` one the bare state word.
        assert_eq!(
            expiry_cell(
                Some(AccountExpiry {
                    expires_at: Some(NOW + 3 * 86_400),
                    horizon_state: ExpiryHorizon::Within,
                    cohort_id: None,
                }),
                NOW
            ),
            "3d"
        );
        assert_eq!(
            expiry_cell(
                Some(AccountExpiry {
                    expires_at: Some(NOW + 29 * 86_400 + 4 * 3_600),
                    horizon_state: ExpiryHorizon::Beyond,
                    cohort_id: None,
                }),
                NOW
            ),
            "29d4h"
        );
        assert_eq!(
            expiry_cell(
                Some(AccountExpiry {
                    expires_at: Some(NOW - 86_400),
                    horizon_state: ExpiryHorizon::Lapsed,
                    cohort_id: None,
                }),
                NOW
            ),
            "lapsed",
            "a passed deadline is a STATE word, never `now` — `humanize_until`'s zero case would \
             read as a reset arriving, the opposite of what has happened"
        );

        // GAP HONESTY (issue #137). All three absences render `—`, never a silent "fine":
        // `Unknown` (polled, credential carried no deadline) …
        assert_eq!(
            expiry_cell(
                Some(AccountExpiry {
                    expires_at: None,
                    horizon_state: ExpiryHorizon::Unknown,
                    cohort_id: None,
                }),
                NOW
            ),
            EXPIRY_GAP
        );
        // … no modifier on the wire at all (a pre-#882 daemon, or an unpolled account) …
        assert_eq!(expiry_cell(None, NOW), EXPIRY_GAP);
        // … and a MALFORMED frame that claims a FORWARD-LOOKING state with no deadline: `Within`
        // is a claim ABOUT a deadline, so without one there is nothing to say. Unreachable from
        // `account_expiry`, but the wire is `#[serde(default)]`-decoded, so it degrades honestly
        // instead of unwrapping a `None`.
        assert_eq!(
            expiry_cell(
                Some(AccountExpiry {
                    expires_at: None,
                    horizon_state: ExpiryHorizon::Within,
                    cohort_id: None,
                }),
                NOW
            ),
            EXPIRY_GAP
        );
        // But a declared `Lapsed` with no deadline is NOT that case, and this is the arm-ordering
        // pin: `lapsed` is a bare state word that never reads `expires_at`, so the missing field
        // costs it nothing. Were it to fall through to the gap above, `status` would drop the
        // whole column whenever the lapse was the roster's only expiry datum (`status_columns`
        // materialises it only once some row is non-gap) — a dead login rendered as no login
        // problem at all, the one outcome this cell exists to prevent.
        assert_eq!(
            expiry_cell(
                Some(AccountExpiry {
                    expires_at: None,
                    horizon_state: ExpiryHorizon::Lapsed,
                    cohort_id: None,
                }),
                NOW
            ),
            "lapsed",
            "a DECLARED lapse outranks a missing deadline; the gap would discard the strongest \
             negative signal the wire can carry"
        );
        // …and it is tinted RED rather than left uncoloured, so the two projections agree.
        assert_eq!(
            expiry_severity(
                Some(AccountExpiry {
                    expires_at: None,
                    horizon_state: ExpiryHorizon::Lapsed,
                    cohort_id: None,
                }),
                NOW
            ),
            Some(Severity::Red)
        );
        // `Unknown` is authoritative in the other direction too: it says the daemon found no
        // PARSEABLE deadline, so a stray timestamp beside it is not trusted into a duration.
        assert_eq!(
            expiry_cell(
                Some(AccountExpiry {
                    expires_at: Some(NOW + 3 * 86_400),
                    horizon_state: ExpiryHorizon::Unknown,
                    cohort_id: None,
                }),
                NOW
            ),
            EXPIRY_GAP
        );
    }

    #[test]
    fn a_deadline_that_passed_since_the_last_tick_reads_lapsed_never_now() {
        // The staleness trap, and the sharpest failure this cell can have. `status` is served from
        // the snapshot built at the LAST TICK — up to `poll_interval_secs` old (300 s by default,
        // 3600 s while exhausted) — so a deadline can pass while the daemon still has it filed
        // `Within`. Dispatching on that cached class alone hands `humanize_until` a non-positive
        // remainder, which renders `now`: this table's word for a reset ARRIVING. The one cell
        // built to warn would read as good news at the exact moment it stops being true, and every
        // token lapse crosses that window exactly once.
        let stale = |horizon_state| {
            Some(AccountExpiry {
                // Classified one minute before the deadline, rendered five minutes after it.
                expires_at: Some(NOW - 300),
                horizon_state,
                cohort_id: None,
            })
        };
        for horizon_state in [ExpiryHorizon::Within, ExpiryHorizon::Beyond] {
            assert_eq!(
                expiry_cell(stale(horizon_state), NOW),
                "lapsed",
                "the RENDER clock outranks the cached {horizon_state:?} class — dispatching on the \
                 class alone yields `now`, the RESET vocabulary for relief arriving, which is the \
                 opposite of a dead login"
            );
            assert_eq!(
                expiry_severity(stale(horizon_state), NOW),
                Some(Severity::Red),
                "the tint follows the same verdict — a stale {horizon_state:?} must not stay Yellow"
            );
        }
        // The boundary: `at == now` is already past (the deadline is not a reset instant).
        assert_eq!(
            expiry_cell(
                Some(AccountExpiry {
                    expires_at: Some(NOW),
                    horizon_state: ExpiryHorizon::Within,
                    cohort_id: None,
                }),
                NOW
            ),
            "lapsed"
        );
        // MONOTONE the other way too: a daemon-classified `Lapsed` does not un-lapse should the
        // render clock read EARLIER than the tick's (backwards skew, NTP step). Once either clock
        // has seen the deadline pass, the cell stays `lapsed`.
        let skewed = Some(AccountExpiry {
            expires_at: Some(NOW + 3 * 86_400),
            horizon_state: ExpiryHorizon::Lapsed,
            cohort_id: None,
        });
        assert_eq!(expiry_cell(skewed, NOW), "lapsed");
        assert_eq!(expiry_severity(skewed, NOW), Some(Severity::Red));

        // End to end, through the table: the stale row reads `lapsed`, not `now`.
        let response = expiry_response(vec![AccountStatusLine {
            health: Some(CredentialHealth::Healthy),
            expiry: stale(ExpiryHorizon::Within),
            ..status_line("work", true, Some(10), Some(20))
        }]);
        let out = render_status(&response, NOW, None, false);
        let work = out
            .lines()
            .find(|l| l.contains("work"))
            .expect("work's row");
        assert!(
            work.contains("lapsed"),
            "the rendered row states the lapse: {work:?}"
        );
    }

    #[test]
    fn the_expiry_column_is_independent_of_auth_never_folded_into_it() {
        // The load-bearing #878 invariant, rendered: the two axes are ORTHOGONAL, so an account
        // that is 🟢 Healthy RIGHT NOW and three days from needing a re-login shows BOTH facts, in
        // two INDEPENDENT cells. Folding expiry into the AUTH cell (or degrading the glyph to say
        // it) would erase exactly the case the feature exists to surface.
        let response = expiry_response(vec![AccountStatusLine {
            health: Some(CredentialHealth::Healthy),
            ..status_line_expiry("work", true, 3 * 86_400, ExpiryHorizon::Within)
        }]);
        let out = render_status(&response, NOW, None, false);
        let header = out.lines().next().expect("a header row");
        let work = out
            .lines()
            .find(|l| l.contains("work"))
            .expect("work's row");

        assert!(
            header.contains("EXPIRY") && header.contains("AUTH"),
            "two labelled columns, not one: {header:?}"
        );
        assert!(
            header.find("EXPIRY") < header.find("AUTH"),
            "EXPIRY precedes AUTH, so AUTH's ragged free-text cue stays last on the line: \
             {header:?}"
        );
        assert!(
            work.contains("🟢") && work.contains("3d"),
            "healthy AND three days out — both cells present: {work:?}"
        );
        // The AUTH cell itself is UNCHANGED. A DIFFERENTIAL over the one varied input — the same
        // healthy account with and without the modifier — so what is pinned is that the modifier
        // does not reach `health_cell`, on the very glyph path this scenario renders.
        let healthy = |expiry| AccountStatusLine {
            health: Some(CredentialHealth::Healthy),
            expiry,
            ..status_line("work", true, Some(10), Some(20))
        };
        let with_expiry =
            status_line_expiry("work", true, 3 * 86_400, ExpiryHorizon::Within).expiry;
        assert_eq!(
            health_cell(&healthy(with_expiry)),
            health_cell(&healthy(None)),
            "the expiry modifier does not reach the AUTH cell"
        );
        assert_eq!(
            health_cell(&healthy(with_expiry)),
            "🟢",
            "and that cell is the bare glyph — so the equality above is not two empty strings"
        );
        // #15-clean, and the whole render carries no imperative: this cell states a fact and
        // leaves the remedy to the operator.
        assert!(crate::redaction::meter::unauthored_emails(&out, &[]).is_empty());
        for imperative in ["re-login", "renew", "sessiometer login"] {
            assert!(
                !out.contains(imperative),
                "the expiry column names no imperative (`{imperative}`): {out}"
            );
        }
    }

    #[test]
    fn the_expiry_column_elides_until_some_account_has_an_observed_deadline() {
        // Empty-column elision, the same rule the `stats` table applies (§D-STA-5): a roster whose
        // credentials carry no `refreshTokenExpiresAt` — including every pre-#882 daemon — renders
        // exactly as it did before issue #883, rather than growing a column of em dashes.
        let none = expiry_response(vec![
            status_line("work", true, Some(10), Some(20)),
            status_line("spare", false, Some(30), Some(40)),
        ]);
        let out = render_status(&none, NOW, None, false);
        assert!(
            !out.contains("EXPIRY") && !out.contains(EXPIRY_GAP),
            "no observed deadline anywhere → no column at all: {out}"
        );

        // `Unknown` is an OBSERVATION, not a deadline — it must not materialize the column either.
        let unknown = expiry_response(vec![status_line_expiry(
            "work",
            true,
            0,
            ExpiryHorizon::Unknown,
        )]);
        assert!(
            !render_status(&unknown, NOW, None, false).contains("EXPIRY"),
            "an all-unknown roster still elides"
        );

        // ONE observed deadline materializes it — and the account WITHOUT one gets an explicit
        // gap, never a fabricated or reassuring value.
        let mixed = expiry_response(vec![
            status_line_expiry("work", true, 3 * 86_400, ExpiryHorizon::Within),
            status_line("spare", false, Some(30), Some(40)),
        ]);
        let out = render_status(&mixed, NOW, None, false);
        let spare = out
            .lines()
            .find(|l| l.contains("spare"))
            .expect("spare's row");
        assert!(out.contains("EXPIRY"), "one datum materializes it: {out}");
        assert!(
            spare.contains(EXPIRY_GAP),
            "the account with no observed deadline is an explicit gap: {spare:?}"
        );
    }

    #[test]
    fn the_expiry_column_sheds_first_under_a_narrow_terminal() {
        // Recorded shed decision (`status_columns`): EXPIRY (1) → the WEEKLY pair (2, atomic) →
        // AUTH (3). Expiry is the slowest-moving axis on the row — a server-issued deadline no
        // tick can move — so a narrow terminal loses the least by deferring it.
        let response = expiry_response(vec![AccountStatusLine {
            health: Some(CredentialHealth::Healthy),
            ..status_line_expiry("work", true, 3 * 86_400, ExpiryHorizon::Within)
        }]);
        let full = render_status(&response, NOW, None, false);
        let full_header = full.lines().next().expect("a header row");
        let width = display_width(full_header);

        // One column narrower than the full table: EXPIRY goes, and NOTHING else does yet.
        let narrowed = render_status(&response, NOW, Some(width - 1), false);
        let header = narrowed.lines().next().expect("a header row");
        assert!(!header.contains("EXPIRY"), "EXPIRY sheds first: {header:?}");
        assert!(
            header.contains("WEEKLY%") && header.contains("AUTH"),
            "and it sheds ALONE — the WEEKLY pair and AUTH both survive: {header:?}"
        );

        // Squeezed further, the established order continues unchanged: the WEEKLY pair leaves
        // atomically next, then AUTH, leaving the ACCOUNT + SESSION floor.
        let tighter = render_status(&response, NOW, Some(30), false);
        let header = tighter.lines().next().expect("a header row");
        assert!(
            !header.contains("EXPIRY") && !header.contains("WEEKLY%"),
            "the weekly pair follows expiry: {header:?}"
        );
        let floor = render_status(&response, NOW, Some(1), false);
        let header = floor.lines().next().expect("a header row");
        assert_eq!(
            header.split_whitespace().collect::<Vec<_>>(),
            ["ACCOUNT", "SESSION%", "RESET"],
            "the floor never drops and the row overflows rather than wrapping"
        );
        assert_eq!(floor.lines().filter(|l| l.contains("work")).count(), 1);
    }

    #[test]
    fn the_expiry_cell_is_tinted_by_its_own_horizon_band() {
        // Per-cell colour (issue #84): the expiry cell carries no self-colouring glyph, so its
        // band is what makes the horizon legible at a glance (since issue #934 it augments the
        // bracket rather than standing alone). Lapsed is act-now Red; Within the configured
        // horizon is Yellow; Beyond it is Dim — the same de-emphasis a far-off reset gets, since
        // there is nothing to act on. An UNOBSERVED deadline is uncoloured: absence of colour is
        // not a false "healthy" signal.
        let of = |offset, state| {
            expiry_severity(status_line_expiry("a", true, offset, state).expiry, NOW)
        };
        assert_eq!(of(-86_400, ExpiryHorizon::Lapsed), Some(Severity::Red));
        assert_eq!(
            of(3 * 86_400, ExpiryHorizon::Within),
            Some(Severity::Yellow)
        );
        assert_eq!(of(29 * 86_400, ExpiryHorizon::Beyond), Some(Severity::Dim));
        assert_eq!(of(0, ExpiryHorizon::Unknown), None);
        assert_eq!(expiry_severity(None, NOW), None);

        // End to end: the tint reaches the rendered cell, and the plain text survives underneath
        // it — colour AUGMENTS, it is never the only signal.
        let response = expiry_response(vec![status_line_expiry(
            "work",
            true,
            -86_400,
            ExpiryHorizon::Lapsed,
        )]);
        let colored = render_status(&response, NOW, None, true);
        assert!(
            colored.contains(&format!("\x1b[{}mlapsed", Severity::Red.sgr())),
            "the lapsed cell is tinted red: {colored:?}"
        );
        assert!(render_status(&response, NOW, None, false).contains("lapsed"));
    }

    // --- status: the within-horizon EXPIRY mark (issue #934) -------------------

    #[test]
    fn the_expiry_mark_brackets_within_and_leaves_every_other_state_bare() {
        // The mark answers the one question the bare duration cannot: the horizon is
        // operator-configurable, so `2d2h` alone never tells a reader which side of it they are
        // on. Bracketed means INSIDE the configured window; bare means outside it.
        let cell = |offset, state| {
            expiry_table_cell(status_line_expiry("a", true, offset, state).expiry, NOW)
        };

        assert_eq!(cell(3 * 86_400, ExpiryHorizon::Within), "[3d]");
        assert_eq!(
            cell(2 * 86_400 + 2 * 3_600, ExpiryHorizon::Within),
            "[2d2h]",
            "the measured live-fleet case from the issue: `2d2h` inside the horizon and `28d11h` \
             outside it were typographically identical before the mark"
        );

        // Beyond, the gap, and `lapsed` are all deliberately BARE. `lapsed` is not *within* a
        // forward-looking window, and it is already the loudest thing in a column of durations —
        // a bare word among digits needs no bracket to be found by eye.
        assert_eq!(
            cell(28 * 86_400 + 11 * 3_600, ExpiryHorizon::Beyond),
            "28d11h"
        );
        assert_eq!(cell(-86_400, ExpiryHorizon::Lapsed), "lapsed");
        assert_eq!(cell(0, ExpiryHorizon::Unknown), EXPIRY_GAP);
        assert_eq!(
            expiry_table_cell(None, NOW),
            EXPIRY_GAP,
            "an account the daemon never sent a modifier for stays the plain gap — the mark never \
             manufactures a signal out of an absence"
        );
    }

    #[test]
    fn the_expiry_mark_and_the_tint_agree_on_which_cells_are_within() {
        // `expiry_table_cell` and `expiry_severity` dispatch through `expiry_view` separately, so
        // this pins the invariant that keeps them honest: EXACTLY the cells that bracket are the
        // cells that render Yellow. A reordered arm on either side breaks this, including on the
        // three payloads where the daemon's cached class and the render clock DISAGREE — which no
        // per-state case can reach.
        let day = 86_400;
        let cases: &[(i64, ExpiryHorizon)] = &[
            (3 * day, ExpiryHorizon::Within),
            (29 * day, ExpiryHorizon::Beyond),
            (-day, ExpiryHorizon::Lapsed),
            (0, ExpiryHorizon::Unknown),
            // The render-time re-check: classified `Within` a tick ago, already past at the draw.
            (-300, ExpiryHorizon::Within),
            // Its boundary — at the instant counts as passed.
            (0, ExpiryHorizon::Within),
            // Backwards skew: a declared `Lapsed` does not un-lapse, so it must not re-acquire the
            // mark either.
            (3 * day, ExpiryHorizon::Lapsed),
            (-300, ExpiryHorizon::Beyond),
        ];
        for (offset, state) in cases {
            let expiry = status_line_expiry("a", true, *offset, *state).expiry;
            let marked = expiry_table_cell(expiry, NOW).starts_with('[');
            let yellow = expiry_severity(expiry, NOW) == Some(Severity::Yellow);
            assert_eq!(
                marked, yellow,
                "the mark and the tint disagree about {state:?} at {offset}s: marked={marked}, \
                 yellow={yellow} — a cell that brackets must be the same cell that tints Yellow"
            );
        }
        assert_eq!(
            expiry_table_cell(None, NOW).starts_with('['),
            expiry_severity(None, NOW) == Some(Severity::Yellow),
        );
    }

    #[test]
    fn the_within_horizon_mark_never_reaches_the_cross_surface_fact() {
        // LOAD-BEARING, and the reason the mark lives one layer out. `expiry_cell` is byte-pinned
        // for every wire state into `build/fixtures/cross-surface-severity.json`, which the panel's
        // `CrossSurfaceSeverityParityTests` asserts against — and `.github/workflows/ci.yml` runs
        // the `swift` job on `build/fixtures/**`, not only on `apps/menubar/**`. Moving the bracket
        // into `expiry_cell` would regenerate that manifest and take the Swift gate red until the
        // panel mirrored it. R-2 pins the shared STATE vocabulary; how a surface survives colour
        // loss is that surface's own presentation, exactly as the tint already is.
        let day = 86_400;
        for (offset, state) in [
            (3 * day, ExpiryHorizon::Within),
            (29 * day, ExpiryHorizon::Beyond),
            (-day, ExpiryHorizon::Lapsed),
            (0, ExpiryHorizon::Unknown),
        ] {
            let expiry = status_line_expiry("a", true, offset, state).expiry;
            let fact = expiry_cell(expiry, NOW);
            assert!(
                !fact.contains('[') && !fact.contains(']'),
                "the cross-surface fact for {state:?} must stay unmarked, got {fact:?}"
            );
            // …and the table cell is that same fact, only ever wrapped — never re-spelled.
            let table = expiry_table_cell(expiry, NOW);
            assert!(
                table == fact || table == format!("[{fact}]"),
                "the table cell must be the fact verbatim or the fact bracketed: {table:?} vs {fact:?}"
            );
        }
    }

    #[test]
    fn the_expiry_mark_survives_every_mode_that_drops_colour() {
        // AC1: the mark reaches the operator under `--no-color`, `NO_COLOR=1`, and a pipe to a
        // non-tty. All three land on `render_status`'s colour gate being closed; the pipe
        // additionally passes `cols: None`, which skips column shedding entirely.
        let response = expiry_response(vec![
            AccountStatusLine {
                health: Some(CredentialHealth::Healthy),
                ..status_line_expiry("near", true, 2 * 86_400 + 2 * 3_600, ExpiryHorizon::Within)
            },
            AccountStatusLine {
                health: Some(CredentialHealth::Healthy),
                ..status_line_expiry(
                    "far",
                    false,
                    28 * 86_400 + 11 * 3_600,
                    ExpiryHorizon::Beyond,
                )
            },
        ]);
        let row = |out: &str, handle: &str| {
            out.lines()
                .find(|l| l.contains(handle))
                .expect("the account's row")
                .to_owned()
        };

        // Uncoloured, at a wide terminal AND piped (`cols: None`).
        for out in [
            render_status(&response, NOW, Some(200), false),
            render_status(&response, NOW, None, false),
        ] {
            assert!(
                row(&out, "near").contains("[2d2h]"),
                "the within-horizon account keeps its mark with no colour at all: {out}"
            );
            assert!(
                row(&out, "far").contains("28d11h") && !row(&out, "far").contains('['),
                "and the account beyond the horizon carries none: {out}"
            );
        }

        // ADDITIVE (AC4): with colour on, the tint is still there AND the mark is inside its span,
        // so stripping every escape recovers the exact plain table.
        let colored = render_status(&response, NOW, Some(200), true);
        assert!(
            colored.contains(&format!("\x1b[{}m[2d2h]", Severity::Yellow.sgr())),
            "colour wraps the MARKED cell — the bracket is part of the tinted text, not outside \
             it, so the two never fight over the cell boundary: {colored:?}"
        );
        assert_eq!(
            strip_ansi(&colored),
            render_status(&response, NOW, Some(200), false),
            "colour is purely additive over the marked table"
        );
    }

    #[test]
    fn the_expiry_mark_costs_one_column_and_leaves_the_shed_order_untouched() {
        // AC6: §D-STA-5's width and shed rules still hold. The mark widens the column by exactly
        // one display column and no more, because a marked cell is only ever as wide as a
        // WITHIN-horizon duration — bounded by the horizon itself. At the 7-day default the widest
        // is `[6d23h]` (7 columns) against the 6 that `lapsed` and the `EXPIRY` label already need.
        let widest = expiry_table_cell(
            status_line_expiry("a", true, 6 * 86_400 + 23 * 3_600, ExpiryHorizon::Within).expiry,
            NOW,
        );
        assert_eq!(widest, "[6d23h]");
        assert_eq!(
            display_width(&widest),
            display_width("EXPIRY") + 1,
            "one column wider than the header label, which `lapsed` already matches"
        );

        // The shed order is unchanged: EXPIRY still goes first, alone, and the row never wraps.
        let response = expiry_response(vec![AccountStatusLine {
            health: Some(CredentialHealth::Healthy),
            ..status_line_expiry("work", true, 3 * 86_400, ExpiryHorizon::Within)
        }]);
        let full = render_status(&response, NOW, None, false);
        assert!(full.lines().any(|l| l.contains("[3d]")));
        let width = display_width(full.lines().next().expect("a header row"));
        let narrowed = render_status(&response, NOW, Some(width - 1), false);
        let header = narrowed.lines().next().expect("a header row");
        assert!(
            !header.contains("EXPIRY"),
            "EXPIRY still sheds first, marked or not: {header:?}"
        );
        assert!(
            header.contains("WEEKLY%") && header.contains("AUTH"),
            "and still sheds alone: {header:?}"
        );
        assert!(
            !narrowed.contains('[') && narrowed.lines().count() == full.lines().count(),
            "the shed drops the whole cell — mark included — rather than wrapping the row: \
             {narrowed:?}"
        );
    }

    #[test]
    fn the_expiry_mark_is_a_descriptor_and_never_an_instruction() {
        // AC5 / §D-STA-6 (SUR-001). A bracket is a delimiter: it names WHERE the deadline sits
        // relative to a configured window and asks for nothing. `!` was refused deliberately —
        // issue #884 measured `within` as the steady state (a 7-day horizon against ~30-day
        // tokens leaves every healthy account inside it for a week before every re-login), so an
        // alarm sigil would cry wolf on a state most of a healthy fleet occupies most weeks.
        let response = expiry_response(vec![AccountStatusLine {
            health: Some(CredentialHealth::Healthy),
            ..status_line_expiry("work", true, 3 * 86_400, ExpiryHorizon::Within)
        }]);
        let out = render_status(&response, NOW, Some(200), false);
        assert!(out.contains("[3d]"));
        assert_eq!(
            scan_expiry_help(&out),
            None,
            "the marked render names no imperative, no recommendation, and no acquisitive call"
        );
        // …and the shapes that scan does NOT carry, so the two are complements rather than one
        // guard twice: the alarm sigil this mark was chosen over, the remedy the AUTH cell owns
        // (named nowhere in a healthy row), and second-person address.
        for imperative in [
            "!",
            "re-login",
            "renew",
            "log in",
            "sessiometer login",
            "you ",
        ] {
            assert!(
                !out.contains(imperative),
                "the marked cell reaches for no instruction (`{imperative}`): {out:?}"
            );
        }
        // The help text describing the mark is bound by the same rule — its own SUR-001 scan is
        // `expiry_help_carries_no_banned_token_but_the_guard_bites_on_injection`, which already
        // covers `STATUS_USAGE`. What is new here is that the mark is documented AT ALL.
        assert!(
            STATUS_USAGE.contains("[6d21h]"),
            "STATUS_USAGE must document the mark — an undocumented sigil is a puzzle"
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
            systemic_refresh_source: None,
            canonical_scrub: None,
            keychain_locked: false,
            canary: None,
            expiry_cohort: None,
            recent_blind_preempt_swap: None,
            recent_landing_overshoot: None,
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
            systemic_refresh_source: None,
            canonical_scrub: None,
            keychain_locked: false,
            canary: None,
            expiry_cohort: None,
            recent_blind_preempt_swap: None,
            recent_landing_overshoot: None,
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
            systemic_refresh_source: None,
            canonical_scrub: None,
            keychain_locked: false,
            canary: None,
            expiry_cohort: None,
            recent_blind_preempt_swap: None,
            recent_landing_overshoot: None,
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
            systemic_refresh_source: None,
            canonical_scrub: None,
            keychain_locked: false,
            canary: None,
            expiry_cohort: None,
            recent_blind_preempt_swap: None,
            recent_landing_overshoot: None,
            refresh_enabled: None,
            accounts: vec![],
            next_swap: None,
        };
        assert_eq!(render_access_token_expiry(&response, NOW), "");
    }

    #[test]
    fn apply_enabled_flips_the_resolved_account_and_reports_change() {
        let mut roster = vec![acct("work", "u1"), acct("spare", "u2")];
        // Resolve `spare` by label and disable it; the other account is untouched. The
        // returned label is the RESOLVED account's, which is what the confirmation names.
        assert_eq!(
            apply_enabled(&mut roster, "spare", false).unwrap(),
            (FlipOutcome::Changed, "spare".to_owned())
        );
        assert!(roster[0].enabled, "the unaddressed account is left alone");
        assert!(!roster[1].enabled);
        // Re-enable flips it back.
        assert_eq!(
            apply_enabled(&mut roster, "spare", true).unwrap(),
            (FlipOutcome::Changed, "spare".to_owned())
        );
        assert!(roster[1].enabled);
    }

    #[test]
    fn apply_enabled_is_idempotent_when_already_in_the_target_state() {
        let mut roster = vec![acct("work", "u1")];
        // Already enabled → Unchanged, so the caller skips the config rewrite.
        assert_eq!(
            apply_enabled(&mut roster, "work", true).unwrap(),
            (FlipOutcome::Unchanged, "work".to_owned())
        );
        assert!(roster[0].enabled);
    }

    #[test]
    fn apply_enabled_rejects_an_unknown_label_without_touching_the_roster() {
        let mut roster = vec![acct("work", "u1")];
        let err =
            apply_enabled(&mut roster, "ghost", false).expect_err("an unmatched label is an error");
        // Issue #1005: the shared `use`/`poke`/daemon taxonomy, not the retired
        // `AccountLabelNotFound` — an observable change, since this exits 5 rather than 1.
        assert!(
            matches!(err, Error::UseTargetNotFound { ref query } if query == "ghost"),
            "got {err:?}"
        );
        assert_eq!(
            err.exit_code(),
            5,
            "and it exits 5, where it used to exit 1"
        );
        assert!(
            roster[0].enabled,
            "a failed resolve leaves the roster intact"
        );
    }

    // --- duplicate-label resolution consistency (issue #1005, OQ-1) ---------
    //
    // OQ-1 settled on refuse-on-ambiguity across all six label-resolving sites. `use`, `poke`
    // and the daemon's control-socket swap already refused (they share `resolve_target`); the
    // three below did not — they took the earliest bearer, and `remove` did so while deleting
    // keychain material. These pin the half that changed.

    #[test]
    fn apply_enabled_refuses_a_duplicate_label_without_touching_the_roster() {
        let mut roster = vec![acct("dup", "u1"), acct("dup", "u2")];
        let err = apply_enabled(&mut roster, "dup", false)
            .expect_err("a duplicated label must not resolve");
        assert!(
            matches!(err, Error::UseTargetAmbiguous { count: 2, ref query } if query == "dup"),
            "got {err:?}"
        );
        // The refusal is total: neither bearer's flag moved. Previously the FIRST flipped.
        assert!(
            roster.iter().all(|account| account.enabled),
            "an ambiguous resolve flips nothing"
        );
    }

    #[test]
    fn apply_remove_refuses_a_duplicate_label_without_touching_the_roster() {
        let mut roster = vec![acct("dup", "u1"), acct("dup", "u2"), acct("solo", "u3")];
        let err =
            apply_remove(&mut roster, "dup").expect_err("a duplicated label must not resolve");
        assert!(
            matches!(err, Error::UseTargetAmbiguous { count: 2, ref query } if query == "dup"),
            "got {err:?}"
        );
        // The load-bearing assertion of this whole issue: nothing was removed, so
        // `remove_account` never reaches the stash delete. Previously `u1` went, irreversibly.
        assert_eq!(roster.len(), 3, "an ambiguous resolve removes nothing");
        assert_eq!(roster[0].account_uuid, "u1");
    }

    #[test]
    fn the_label_resolving_verbs_accept_an_account_uuid_so_a_refusal_is_actionable() {
        // There is no `--account-uuid` disambiguator flag (design § 4.3 option (iii) was not
        // chosen), so the uuid `resolve_target` also matches IS the remedy for the refusals
        // above. Without this the refusal would be a dead end — a worse defect than the
        // first-match-wins it replaced.
        let mut roster = vec![acct("dup", "u1"), acct("dup", "u2")];
        assert_eq!(
            apply_enabled(&mut roster, "u2", false).unwrap(),
            (FlipOutcome::Changed, "dup".to_owned()),
            "the uuid resolves, and the confirmation names the account's LABEL"
        );
        assert!(roster[0].enabled, "only the named bearer flipped");
        assert!(!roster[1].enabled);

        let removed = apply_remove(&mut roster, "u1").expect("the uuid resolves for remove too");
        assert_eq!(removed.account_uuid, "u1");
        assert_eq!(roster.len(), 1);
        assert_eq!(
            roster[0].account_uuid, "u2",
            "the OTHER bearer survives — the operator picked one and got it"
        );
    }

    #[test]
    fn the_two_verbs_routed_to_the_shared_resolver_return_its_own_error() {
        // The two verbs #1005 newly routed — `apply_enabled` (`enable` / `disable`) and
        // `apply_remove` — and ONLY those two. Assert the shared resolver's verdict IS what they
        // return, on the same roster, rather than restating its behaviour independently: an
        // independent restatement would drift the moment the resolver's policy changed.
        //
        // This test was called `every_label_resolving_site_shares_one_resolver` until issue
        // #1186, and the name outran the body by four sites. `use`, `poke` and the daemon's
        // control-socket swap appeared only in a comment here, so a reader who trusted the name
        // credited them with cover they did not have — issue #1087 was filed on exactly that
        // belief. Those three now assert their own refusals where they live (#1087, PR #1184),
        // and the SET-level property the old name claimed — that no SEVENTH site grows its own
        // resolver — is owned by `use_account`'s `every_handle_read_is_dispositioned` and
        // `resolve_target_has_exactly_the_five_known_call_sites`, which are the only shape that
        // can cover a site that does not exist yet.
        let roster = vec![acct("dup", "u1"), acct("dup", "u2")];
        let shared = crate::use_account::resolve_target(&roster, "dup")
            .expect_err("the shared resolver refuses a duplicate");

        let mut for_enable = roster.clone();
        let enable = apply_enabled(&mut for_enable, "dup", false).expect_err("enable refuses");
        let mut for_remove = roster.clone();
        let remove = apply_remove(&mut for_remove, "dup").expect_err("remove refuses");

        for (verb, err) in [("enable/disable", &enable), ("remove", &remove)] {
            assert_eq!(
                err.to_string(),
                shared.to_string(),
                "{verb} must refuse with the shared resolver's own error"
            );
            assert_eq!(
                err.exit_code(),
                shared.exit_code(),
                "{verb} must carry the shared resolver's exit code"
            );
            // …and pin the literal code, not just its equality with the resolver's. Asserting
            // only the equality would stay green if `resolve_target`'s taxonomy moved, which is
            // exactly the operator-visible contract a script keys on.
            assert_eq!(err.exit_code(), 6, "{verb} exits 6 on an ambiguous target");
        }
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
        // Issue #1005: the shared taxonomy, so this exits 5 where it used to exit 1.
        assert!(
            matches!(err, Error::UseTargetNotFound { ref query } if query == "ghost"),
            "got {err:?}"
        );
        assert_eq!(
            err.exit_code(),
            5,
            "and it exits 5, where it used to exit 1"
        );
        assert_eq!(roster.len(), 1, "a failed resolve leaves the roster intact");
    }

    #[test]
    fn remove_confirmation_names_the_label() {
        assert_eq!(remove_confirmation("work"), "removed `work`");
        // #15: the confirmation carries only the operator label, never a secret.
        assert!(!remove_confirmation("work").contains('@'));
    }

    // --- the confirmations' handle choice (issue #1088) --------------------

    /// The argument list of each confirmation call site in this file's NON-TEST code, verbatim
    /// between the parentheses.
    ///
    /// Since issue #1005 `enable`, `disable` and `remove` resolve through
    /// [`crate::use_account::resolve_target`], so the `query` an operator typed may be an
    /// account-uuid. Both shells deliberately echo the handle the RESOLVER produced instead, and
    /// the comments at those two call sites say exactly that.
    ///
    /// Every OTHER link in that chain is asserted behaviourally: the policy half returns the
    /// RESOLVED label ([`apply_enabled_flips_the_resolved_account_and_reports_change`],
    /// [`apply_remove_drops_the_resolved_account_and_returns_it`]) and the formatters render
    /// whatever they are handed ([`flip_confirmation_reflects_changed_vs_already_in_state`],
    /// [`remove_confirmation_names_the_label`]). What no unit test reached is the step this pins —
    /// WHICH of the two strings in scope the async shell hands over. Handing over the wrong one
    /// survived the whole suite at both sites, which is what issue #1088 was opened about;
    /// [`the_resolved_handle_gate_bites_on_each_measured_mutation`] records the three measured
    /// mutations, and why one of them appears to be caught already but is not.
    ///
    /// It is NOT the only untested step in that shell, and this pin is not a coverage bound. The
    /// shell's own binding of `label` from [`apply_enabled`]'s return is a second one: rebind it to
    /// `query`, leaving the pinned call site byte-identical, and the whole suite stays green —
    /// measured — while the formatter is handed the raw query, which since #1005 may be the uuid.
    /// Closing that one needs the shell seamed, which the paragraph below places out of scope.
    ///
    /// So the argument list is pinned here, as a CLOSED allow-list: a site whose spelling changes
    /// reddens and must be re-blessed deliberately. That is the same spirit as
    /// [`INLINE_PROSE_REGISTER`] and `use_account`'s `HANDLE_READ_REGISTER`, which pin axes a
    /// compiler cannot check — but deliberately NOT the same strength, and the gap is worth
    /// stating. Those two extract their subjects FROM SOURCE and assert set-equality both ways, so
    /// a brand-new site reddens them. This one iterates its own list and freezes `len() == 2`: it
    /// catches a changed spelling at either pinned site, and a second call to an already-pinned
    /// formatter ([`sole_call_arguments`] refuses any cardinality but one) — but a NEW confirmation
    /// formatter whose own call site prints the raw `query` passes it. Measured: adding one reddens
    /// [`INLINE_PROSE_REGISTER`]'s sweep and nothing else, and once that is dispositioned the way
    /// its own failure messages instruct, the suite is green and this gate never fired. Enumerating
    /// confirmation sites from source is a real extension, not a wording fix. It is a source pin
    /// rather than a behavioural test because
    /// the choice is made inside an `async fn` that reaches for `Config::load`, the real keychain
    /// and a live daemon socket; seaming those is the restructuring issue #1088 placed out of
    /// scope, and the file's standing shape is pure functions tested, I/O shell not.
    const CONFIRMATION_CALL_ARGUMENTS: &[(&str, &str)] = &[
        ("flip_confirmation", "outcome, &label, enabled"),
        ("remove_confirmation", "&removed.label"),
    ];

    /// Handles a confirmation must never name in place of the resolved label, with the reason.
    ///
    /// Stated separately from the exact pin above so the PROPERTY survives a re-blessing: renaming
    /// the `label` binding is an ordinary refactor that legitimately moves the pin, and a reader
    /// updating it to match would carry a swapped argument through with it.
    const HANDLES_NO_CONFIRMATION_MAY_NAME: &[(&str, &str)] = &[
        (
            "query",
            "the operator's raw input, which since #1005 may be an account-uuid",
        ),
        (
            "account_uuid",
            "the resolved account's uuid — the right account, under a handle nobody typed",
        ),
    ];

    /// This file's non-test source, cut at its own column-0 `#[cfg(test)]` — the same structural
    /// boundary [`inline_literals`] and [`every_cli_usage_construction_site_is_scanned`] draw, for
    /// the same reason: the test module spells these call shapes freely in its own assertions.
    fn non_test_source(source: &str) -> String {
        source
            .lines()
            .take_while(|line| !line.starts_with("#[cfg(test)]"))
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// The argument list of the SOLE call to `name` in `region`, verbatim between its parentheses.
    ///
    /// Panics unless exactly one call is present, so a renamed formatter cannot leave the gate
    /// scanning nothing and a second site cannot be silently averaged with the first — the
    /// degenerate subject a source lint dies of. A DECLARATION is skipped by its `fn ` prefix; a
    /// doc-comment mention never matches at all, because a match must be followed by `(`.
    ///
    /// Takes the region as an argument rather than reaching for `include_str!` so the canary can
    /// drive THIS function over a mutated subject rather than over a paraphrase of it (ADR-0031
    /// § 4 CONSTRAINT-A).
    fn sole_call_arguments(region: &str, name: &str) -> String {
        let mut found: Vec<String> = Vec::new();
        for (at, _) in region.match_indices(name) {
            let head = &region[..at];
            if head
                .chars()
                .next_back()
                .is_some_and(|c| c.is_ascii_alphanumeric() || c == '_')
            {
                continue;
            }
            if head.ends_with("fn ") {
                continue;
            }
            let Some(args) = region[at + name.len()..].trim_start().strip_prefix('(') else {
                continue;
            };
            let mut depth = 1usize;
            let mut end = None;
            for (offset, ch) in args.char_indices() {
                match ch {
                    '(' => depth += 1,
                    ')' => {
                        depth -= 1;
                        if depth == 0 {
                            end = Some(offset);
                            break;
                        }
                    }
                    _ => {}
                }
            }
            found.push(args[..end.expect("a call site with unbalanced parentheses")].to_owned());
        }
        assert_eq!(
            found.len(),
            1,
            "expected exactly one call to `{name}` in src/cli.rs's non-test code, found {}",
            found.len()
        );
        found.remove(0)
    }

    /// Issue #1088: each confirmation names the handle the RESOLVER produced — never the
    /// operator's raw `query`, never the resolved account's uuid.
    #[test]
    fn the_confirmations_name_the_resolved_handle() {
        let region = non_test_source(include_str!("cli.rs"));

        // The corpus canary comes FIRST, because every assertion below passes identically over a
        // region truncated before the code it means to read. This file's boundary is its `mod
        // tests`, and these two say so rather than assume it.
        assert!(
            region.contains("fn remove_confirmation("),
            "the non-test region stops before the confirmations it is supposed to read"
        );
        assert!(
            !region.contains("fn the_confirmations_name_the_resolved_handle"),
            "the non-test region ran past this file's `mod tests` boundary"
        );

        // Cardinality on both registers, because the loops below iterate them: an emptied one
        // makes every assertion inside pass without evaluating anything, which is the shape of a
        // green run over no subject at all. The two sites are also what issue #1088's second
        // criterion asks for by name — the `enable` / `disable` flip AND the removal.
        assert_eq!(
            CONFIRMATION_CALL_ARGUMENTS.len(),
            2,
            "both confirmation sites must be pinned — the flip and the removal"
        );
        for verb in ["flip_confirmation", "remove_confirmation"] {
            assert!(
                CONFIRMATION_CALL_ARGUMENTS
                    .iter()
                    .any(|(name, _)| *name == verb),
                "`{verb}`'s call site is no longer pinned"
            );
        }
        assert!(
            !HANDLES_NO_CONFIRMATION_MAY_NAME.is_empty(),
            "the forbidden-handle half must have something to check"
        );

        for (formatter, expected) in CONFIRMATION_CALL_ARGUMENTS {
            let actual = sole_call_arguments(&region, formatter);
            assert_eq!(
                &actual, expected,
                "the call site now reads `{formatter}({actual})`. If that is a deliberate \
                 refactor, re-bless it in CONFIRMATION_CALL_ARGUMENTS — but confirm FIRST that it \
                 still passes the handle `resolve_target` produced, which is the swap issue #1088 \
                 was opened about"
            );
            for (forbidden, why) in HANDLES_NO_CONFIRMATION_MAY_NAME {
                assert!(
                    !actual.contains(forbidden),
                    "`{formatter}({actual})` names `{forbidden}` — {why}"
                );
            }
        }
    }

    /// CONSTRAINT-A for the gate above (ADR-0031 § 4): it is observed to REDDEN on this file
    /// carrying the real defect, rather than read and believed.
    ///
    /// The payloads are the three mutations measured AGAINST a full `cargo test` on this tree, and
    /// driven through [`sole_call_arguments`] — the predicate the real assertions read. Two of the
    /// three survive that suite; the third is caught, but only incidentally and in another file,
    /// which the paragraph below sets out.
    ///
    /// The third payload is the one that settles the SUBJECT rather than the gate. `remove`'s
    /// literal swap to `&query` does fail today, but incidentally and in another file:
    /// `use_account`'s `every_handle_read_is_dispositioned` reddens because the swap deletes
    /// `remove_account`'s only identity-field read, not because anything asserts which handle gets
    /// printed. Hand the resolved UUID over instead and the read stays, that register balances,
    /// and the operator gets 36 hex characters — measured surviving. A gate resting on that
    /// coincidence would also stop covering the moment `remove_account` gained any other `.label`
    /// read, which is why the pin above is its own gate rather than a note pointing at that one.
    #[test]
    fn the_resolved_handle_gate_bites_on_each_measured_mutation() {
        let region = non_test_source(include_str!("cli.rs"));

        for (mutation, formatter, mutated) in [
            (
                "the flip echoes the query",
                "flip_confirmation",
                "outcome, &query, enabled",
            ),
            (
                "the removal echoes the query",
                "remove_confirmation",
                "&query",
            ),
            (
                "the removal echoes the resolved uuid",
                "remove_confirmation",
                "&removed.account_uuid",
            ),
        ] {
            // The spelling to mutate AWAY from is read out of the register rather than restated
            // here, so a deliberate re-blessing flows into this canary instead of silently leaving
            // it mutating a spelling the file no longer carries — which would still pass, on a
            // subject that no longer exists.
            let (_, blessed) = CONFIRMATION_CALL_ARGUMENTS
                .iter()
                .find(|(name, _)| *name == formatter)
                .expect("every mutated formatter must be one the register pins");
            let injected = region.replace(
                &format!("{formatter}({blessed})"),
                &format!("{formatter}({mutated})"),
            );
            assert_ne!(
                injected, region,
                "{mutation}: the mutation did not apply. Either the file already carries it, or \
                 the register's `{blessed}` is not the spelling at the call site — this canary \
                 proved nothing either way"
            );

            // The SAME predicate the real gate reads, over the mutated file.
            let seen = sole_call_arguments(&injected, formatter);
            assert_eq!(
                seen, mutated,
                "{mutation}: the extractor must read the mutated argument list back"
            );
            assert_ne!(seen, *blessed, "{mutation}: the pin must redden");
            // …and the forbidden-handle half bites independently of the pin, which is the whole
            // point of stating the two separately: a re-blessed pin still catches this.
            assert!(
                HANDLES_NO_CONFIRMATION_MAY_NAME
                    .iter()
                    .any(|(forbidden, _)| seen.contains(forbidden)),
                "{mutation}: the forbidden-handle half must catch `{formatter}({seen})` too"
            );
        }
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
            // No observed refresh-token deadline, so the issue #883 `EXPIRY` column elides and
            // every layout / alignment / colouring test below keeps pinning the pre-#883 table.
            // The column's own tests build their lines with `status_line_expiry`.
            expiry: None,
        }
    }

    /// A reading carrying the issue #878 REFRESH-token expiry modifier — the axis `status_line`
    /// leaves absent. `offset_secs` is relative to [`NOW`], so a fixture reads as
    /// "`state`, `offset` from now" and the rendered cell is deterministic.
    fn status_line_expiry(
        label: &str,
        active: bool,
        offset_secs: i64,
        horizon_state: ExpiryHorizon,
    ) -> AccountStatusLine {
        AccountStatusLine {
            expiry: Some(AccountExpiry {
                // `Unknown` is the one state that CANNOT carry a deadline — `account_expiry`
                // admits no other combination — so the helper honours that invariant rather than
                // letting a test build a shape production can never produce.
                expires_at: match horizon_state {
                    ExpiryHorizon::Unknown => None,
                    _ => Some(NOW + offset_secs),
                },
                horizon_state,
                cohort_id: None,
            }),
            ..status_line(label, active, Some(10), Some(20))
        }
    }

    /// A [`StatusResponse`] carrying `accounts` and no fleet-level fault — the envelope the
    /// `EXPIRY` column tests vary only the account lines of.
    fn expiry_response(accounts: Vec<AccountStatusLine>) -> StatusResponse {
        StatusResponse {
            systemic_refresh_failure: None,
            systemic_refresh_source: None,
            canonical_scrub: None,
            keychain_locked: false,
            canary: None,
            expiry_cohort: None,
            recent_blind_preempt_swap: None,
            recent_landing_overshoot: None,
            refresh_enabled: None,
            accounts,
            next_swap: None,
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
            expiry: None,
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
            systemic_refresh_source: None,
            canonical_scrub: None,
            keychain_locked: false,
            canary: None,
            expiry_cohort: None,
            recent_blind_preempt_swap: None,
            recent_landing_overshoot: None,
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
            systemic_refresh_source: None,
            canonical_scrub: None,
            keychain_locked: false,
            canary: None,
            expiry_cohort: None,
            recent_blind_preempt_swap: None,
            recent_landing_overshoot: None,
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
            systemic_refresh_source: None,
            canonical_scrub: None,
            keychain_locked: false,
            canary: None,
            expiry_cohort: None,
            recent_blind_preempt_swap: None,
            recent_landing_overshoot: None,
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
            systemic_refresh_source: None,
            canonical_scrub: None,
            keychain_locked: false,
            canary: None,
            expiry_cohort: None,
            recent_blind_preempt_swap: None,
            recent_landing_overshoot: None,
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
            systemic_refresh_source: None,
            canonical_scrub: None,
            keychain_locked: false,
            canary: None,
            expiry_cohort: None,
            recent_blind_preempt_swap: None,
            recent_landing_overshoot: None,
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
                systemic_refresh_source: None,
                canonical_scrub: None,
                keychain_locked: false,
                canary: None,
                expiry_cohort: None,
                recent_blind_preempt_swap: None,
                recent_landing_overshoot: None,
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
        // The fleet-capacity relief hint (issue #405), rendered WITHOUT the pre-#666 false universal
        // and with the "add an account" nudge gated on the WAIT, not the `cause` label (issue #666).
        // A LONG wait (days) is a structural shortage → name the reset + nudge. `resets_at` humanizes
        // with the same `humanize_until` the per-account cells use → `2d4h`.
        assert_eq!(
            footer(Some(NextSwap::NoViableTarget {
                cause: Some(NoTargetCause::Weekly),
                resets_at: Some(NOW + 2 * 86_400 + 4 * 3_600),
            })),
            "next swap: none — out of capacity; resets in 2d4h — add an account"
        );
        // #665/#666 regression — the live mixed-fleet miscalibration: a `Weekly` cause naming a
        // SUB-SESSION-WINDOW weekly reset (soonest spare returns in 59m). The pre-#666 render keyed
        // the nudge off the `Weekly` LABEL and shouted "every account is weekly-exhausted … — add an
        // account" for a one-HOUR wait. Now the label is irrelevant: a sub-window wait is transient,
        // so NO nudge and NO false universal — just the honest relief.
        assert_eq!(
            footer(Some(NextSwap::NoViableTarget {
                cause: Some(NoTargetCause::Weekly),
                resets_at: Some(NOW + 59 * 60),
            })),
            "next swap: none — out of capacity; resets in 59m"
        );
        // Just OVER one session window (6h > 5h) → structural again, the nudge returns — proving the
        // gate keys off the wait, not the cause (this is a `Weekly` cause both times).
        assert_eq!(
            footer(Some(NextSwap::NoViableTarget {
                cause: Some(NoTargetCause::Weekly),
                resets_at: Some(NOW + 6 * 3_600),
            })),
            "next swap: none — out of capacity; resets in 6h — add an account"
        );
        // The boundary is STRICT: exactly one session window still counts as within the window —
        // the nudge needs MORE than a session window.
        assert_eq!(
            footer(Some(NextSwap::NoViableTarget {
                cause: Some(NoTargetCause::Weekly),
                resets_at: Some(NOW + ADD_ACCOUNT_NUDGE_WAIT_SECS),
            })),
            "next swap: none — out of capacity; resets in 5h"
        );
        // A cause present but no spare reported a parseable reset → wait UNKNOWN, treated as
        // structural (nudge), reset clause drops.
        assert_eq!(
            footer(Some(NextSwap::NoViableTarget {
                cause: Some(NoTargetCause::Weekly),
                resets_at: None,
            })),
            "next swap: none — out of capacity — add an account"
        );
        // A SESSION cause with a soon reset (47m ≪ one session window) → transient, no nudge, and no
        // false universal — the SAME honest render as any short-wait cause (label-independent).
        assert_eq!(
            footer(Some(NextSwap::NoViableTarget {
                cause: Some(NoTargetCause::Session),
                resets_at: Some(NOW + 47 * 60),
            })),
            "next swap: none — out of capacity; resets in 47m"
        );
        // A pre-#405 daemon carries no relief (`cause` absent) → the honest bare fallback, unchanged.
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
    fn the_out_of_capacity_phrase_is_shared_by_the_status_footer_and_use_next() {
        // Issue #960: `use --next`'s no-target refusal reports the SAME #405 relief hint the
        // `status` footer does — one composer, so the relief instant and the "add an account"
        // nudge threshold cannot drift between the two surfaces (R-2 STATE-parity). Assert the
        // parity structurally, by deriving the footer's own tail from the shared composer rather
        // than by restating a literal, so a future edit to either wording cannot pass this test
        // while splitting the surfaces apart.
        let response = StatusResponse {
            systemic_refresh_failure: None,
            systemic_refresh_source: None,
            canonical_scrub: None,
            keychain_locked: false,
            canary: None,
            expiry_cohort: None,
            recent_blind_preempt_swap: None,
            recent_landing_overshoot: None,
            refresh_enabled: None,
            accounts: vec![status_line("work", true, Some(50), Some(25))],
            next_swap: Some(NextSwap::NoViableTarget {
                cause: Some(NoTargetCause::Weekly),
                resets_at: Some(NOW + 2 * 86_400),
            }),
        };
        let footer = render_status(&response, NOW, None, false)
            .lines()
            .last()
            .unwrap()
            .to_owned();
        let phrase = out_of_capacity_phrase(Some(NOW + 2 * 86_400), NOW);
        assert_eq!(footer, format!("next swap: none — {phrase}"));
        // …and that phrase is exactly what the `--next` error carries, so the operator reads one
        // story whichever verb surfaced it.
        assert_eq!(
            Error::UseNextNoViableTarget {
                detail: phrase.clone()
            }
            .to_string(),
            format!(
                "refusing to swap: {phrase} — name a target to override: \
                 `sessiometer use <account> --force`"
            )
        );
    }

    #[test]
    fn render_status_footer_is_plain_even_under_color() {
        // The candidate footer (#88) carries no SGR even when the color gate is open —
        // per-cell health coloring is #84, orthogonal; the footer stays uncolored.
        let response = StatusResponse {
            systemic_refresh_failure: None,
            systemic_refresh_source: None,
            canonical_scrub: None,
            keychain_locked: false,
            canary: None,
            expiry_cohort: None,
            recent_blind_preempt_swap: None,
            recent_landing_overshoot: None,
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

    // --- status: synchronized-expiry cohort (issue #879) ---------------------

    /// A response carrying the fleet-level cohort condition over two synchronized rows.
    fn cohort_response(cohort: Option<ExpiryCohort>) -> StatusResponse {
        StatusResponse {
            expiry_cohort: cohort,
            ..expiry_response(vec![
                status_line_expiry("work", true, 2 * 86_400, ExpiryHorizon::Within),
                status_line_expiry("spare", false, 2 * 86_400 + 60, ExpiryHorizon::Within),
            ])
        }
    }

    /// AC-1: the cohort renders as a FLEET-level statement, distinct from any single account's
    /// state — and AC-2's structural rule: an AGGREGATE sentence, never a per-account list of
    /// handles-with-deadlines (the retired #543/#544 shape).
    ///
    /// The handle assertion is the load-bearing one. The obvious wrong implementation of "surface
    /// the cohort" is to enumerate its members on the line, which is exactly the band-keyed-per-
    /// account form design-stats.md §D-STA-5 forbids; the rows already carry membership.
    #[test]
    fn the_cohort_line_is_an_aggregate_fleet_statement_naming_no_account() {
        let cohort = ExpiryCohort {
            size: 4,
            observed: 5,
            earliest: NOW + 2 * 86_400,
            span_secs: 240,
        };
        let out = render_status(&cohort_response(Some(cohort)), NOW, None, false);
        let line = out
            .lines()
            .find(|l| l.starts_with("expiry cohort:"))
            .expect("the cohort line renders");

        assert_eq!(
            line,
            "expiry cohort: 4 of 5 accounts with a known deadline fall within 4m of each other \
             — earliest in 2d"
        );
        // No handle appears on it: the fleet fact is aggregate, membership lives on the rows.
        for handle in ["work", "spare"] {
            assert!(
                !line.contains(handle),
                "the cohort line must name no account: {line:?}"
            );
        }
        // It is ONE line, not a stacked per-account block.
        assert_eq!(
            out.lines()
                .filter(|l| l.starts_with("expiry cohort:"))
                .count(),
            1
        );
    }

    /// The line states the OBSERVED denominator, so a partly-measured fleet cannot read as a
    /// fully-measured one — the issue #137 invariant carried into an aggregate. And when no cohort
    /// is on the wire, NOTHING is printed: absence is never rendered as a reassuring "no cohort".
    #[test]
    fn the_cohort_line_names_its_denominator_and_prints_nothing_when_absent() {
        // Four of five observed — the fifth is a known deadline outside the cohort, and any
        // account with no deadline at all is outside the denominator entirely.
        let out = render_status(
            &cohort_response(Some(ExpiryCohort {
                size: 4,
                observed: 5,
                earliest: NOW + 86_400,
                span_secs: 60,
            })),
            NOW,
            None,
            false,
        );
        assert!(out.contains("4 of 5 accounts with a known deadline"));

        // No cohort on the wire ⇒ no line at all. The absence is left unstated rather than
        // rendered as a positive all-clear, because the daemon cannot distinguish "no cohort" from
        // "too few deadlines observed to tell" — so any "0 cohorts" / "no cohort" wording would
        // claim more than the reading supports (the issue #137 invariant).
        //
        // Scoped to cohort wording deliberately: the unrelated `next swap: none` footer is a
        // different fact about a different thing, and a whole-render ban on the word would pin
        // that instead of this.
        let quiet = render_status(&cohort_response(None), NOW, None, false);
        for reassurance in ["expiry cohort", "cohort"] {
            assert!(
                !quiet.contains(reassurance),
                "an absent cohort says nothing at all, reassuring or otherwise: {quiet:?}"
            );
        }
    }

    /// The wording adapts to the two edges the humanizer cannot express: identical deadlines (a
    /// zero span, which `humanize_until` would render as the reset-arriving word "now") and a
    /// soonest member that has ALREADY passed (which it would render the same way). Both are
    /// worded instead — a line built to warn must not read as calm at the moment it starts
    /// mattering, the same rule `expiry_view` follows for the per-account cell.
    #[test]
    fn the_cohort_line_words_a_zero_span_and_an_already_lapsed_deadline() {
        let same_instant = render_status(
            &cohort_response(Some(ExpiryCohort {
                size: 2,
                observed: 2,
                earliest: NOW + 3_600,
                span_secs: 0,
            })),
            NOW,
            None,
            false,
        );
        assert!(
            same_instant.contains(
                "expiry cohort: 2 of 2 accounts with a known deadline share one deadline instant \
                 — earliest in 1h"
            ),
            "{same_instant:?}"
        );
        assert!(!same_instant.contains("within now"));

        let lapsed = render_status(
            &cohort_response(Some(ExpiryCohort {
                size: 3,
                observed: 4,
                earliest: NOW - 86_400,
                span_secs: 120,
            })),
            NOW,
            None,
            false,
        );
        assert!(
            lapsed.contains("— earliest already lapsed"),
            "a passed deadline is worded, never humanized to 'now': {lapsed:?}"
        );
    }

    /// The line's tint tracks the SOONEST member — yellow while its deadline is ahead, red once it
    /// has passed — which is the band `expiry_severity` gives that member's own cell, so the fleet
    /// line never reads calmer than the row that bites first. A cohort STRADDLING the horizon is
    /// the case that proves it is the soonest and not a consensus: the line stays yellow beside a
    /// dim `Beyond` cell, because the cohort's urgency is its earliest deadline's. Under
    /// `--no-color` the plain text carries the whole message.
    #[test]
    fn the_cohort_line_tint_follows_the_soonest_member() {
        let ahead = ExpiryCohort {
            size: 2,
            observed: 2,
            earliest: NOW + 2 * 86_400,
            span_secs: 60,
        };
        let past = ExpiryCohort {
            earliest: NOW - 60,
            ..ahead
        };

        let yellow = render_status(&cohort_response(Some(ahead)), NOW, None, true);
        let yellow_line = yellow
            .lines()
            .find(|l| l.contains("expiry cohort:"))
            .unwrap();
        assert!(
            yellow_line.contains("\x1b[33m"),
            "a cohort still ahead wears the same Yellow its Within cells do: {yellow_line:?}"
        );

        let red = render_status(&cohort_response(Some(past)), NOW, None, true);
        let red_line = red.lines().find(|l| l.contains("expiry cohort:")).unwrap();
        assert!(
            red_line.contains("\x1b[31m"),
            "a lapsed cohort wears the same Red its Lapsed cells do: {red_line:?}"
        );

        // Colour only augments: the uncoloured render carries every fact.
        let plain = render_status(&cohort_response(Some(ahead)), NOW, None, false);
        assert!(!plain.contains('\x1b'));
        assert!(plain.contains("2 of 2 accounts with a known deadline"));

        // A cohort STRADDLING the horizon: the anchor is inside it, a member 13h later is past it.
        // The window (24h) is wider than the gap between the anchor and the horizon edge, so this
        // is reachable on shipped defaults, not a contrived shape. The fleet line reads the
        // SOONEST member's band — Yellow — while the later member's own cell is Dim `Beyond`. The
        // two disagreeing is correct, and pinning it here stops a later "make them consistent"
        // edit from re-tinting the line off the LATEST deadline, which would under-report.
        let straddle = StatusResponse {
            expiry_cohort: Some(ExpiryCohort {
                size: 2,
                observed: 2,
                earliest: NOW + 6 * 86_400 + 23 * 3_600,
                span_secs: 13 * 3_600,
            }),
            ..expiry_response(vec![
                status_line_expiry("work", true, 6 * 86_400 + 23 * 3_600, ExpiryHorizon::Within),
                status_line_expiry(
                    "spare",
                    false,
                    7 * 86_400 + 12 * 3_600,
                    ExpiryHorizon::Beyond,
                ),
            ])
        };
        let rendered = render_status(&straddle, NOW, None, true);
        let cohort_line = rendered
            .lines()
            .find(|l| l.contains("expiry cohort:"))
            .unwrap();
        assert!(
            cohort_line.contains("\x1b[33m"),
            "a cohort anchored inside the horizon stays Yellow however far its last member sits: \
             {cohort_line:?}"
        );
        let spare_row = rendered.lines().find(|l| l.contains("spare")).unwrap();
        assert!(
            spare_row.contains("\x1b[2m"),
            "the straddling member keeps its own Dim `Beyond` cell — the line does not restate it: \
             {spare_row:?}"
        );
    }

    /// The cohort line prints BELOW every band reporting a fault that is ALREADY REAL. The two
    /// pinned here straddle the colour rank on purpose — an unreadable vault is act-now `Red`, a
    /// down refresh mechanism next-break `Yellow` — so what the ordering encodes is KIND, not
    /// loudness: those report something already wrong, a cohort something that has not happened
    /// yet. Pinned so a later insertion cannot quietly float a forward-looking fact above a
    /// present one.
    #[test]
    fn the_cohort_line_prints_below_the_bands_reporting_a_present_fault() {
        let response = StatusResponse {
            keychain_locked: true,
            systemic_refresh_failure: Some(3),
            ..cohort_response(Some(ExpiryCohort {
                size: 2,
                observed: 2,
                earliest: NOW + 2 * 86_400,
                span_secs: 60,
            }))
        };
        let out = render_status(&response, NOW, None, false);
        let position = |needle: &str| {
            out.lines()
                .position(|l| l.contains(needle))
                .unwrap_or_else(|| panic!("{needle:?} is missing from {out:?}"))
        };
        assert!(position("shared login: unreadable") < position("expiry cohort:"));
        assert!(position("refresh mechanism") < position("expiry cohort:"));
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
            systemic_refresh_source: None,
            canonical_scrub: None,
            keychain_locked: false,
            canary: None,
            expiry_cohort: None,
            recent_blind_preempt_swap: None,
            recent_landing_overshoot: None,
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
                systemic_refresh_source: None,
                canonical_scrub: None,
                keychain_locked: false,
                canary: None,
                expiry_cohort: None,
                recent_blind_preempt_swap: None,
                recent_landing_overshoot: None,
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
            systemic_refresh_source: None,
            canonical_scrub: None,
            keychain_locked: false,
            canary: None,
            expiry_cohort: None,
            recent_blind_preempt_swap: None,
            recent_landing_overshoot: None,
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
            systemic_refresh_source: None,
            canonical_scrub: None,
            keychain_locked: false,
            canary: None,
            expiry_cohort: None,
            recent_blind_preempt_swap: None,
            recent_landing_overshoot: None,
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
            systemic_refresh_source: None,
            canonical_scrub: None,
            keychain_locked: false,
            canary: None,
            expiry_cohort: None,
            recent_blind_preempt_swap: None,
            recent_landing_overshoot: None,
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
            systemic_refresh_source: None,
            canonical_scrub: None,
            keychain_locked: false,
            canary: None,
            expiry_cohort: None,
            recent_blind_preempt_swap: None,
            recent_landing_overshoot: None,
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
            systemic_refresh_source: None,
            canonical_scrub: None,
            keychain_locked: false,
            canary: None,
            expiry_cohort: None,
            recent_blind_preempt_swap: None,
            recent_landing_overshoot: None,
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
            systemic_refresh_source: None,
            canonical_scrub: None,
            keychain_locked: false,
            canary: None,
            expiry_cohort: None,
            recent_blind_preempt_swap: None,
            recent_landing_overshoot: None,
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
            systemic_refresh_source: None,
            canonical_scrub: None,
            keychain_locked: false,
            canary: None,
            expiry_cohort: None,
            recent_blind_preempt_swap: None,
            recent_landing_overshoot: None,
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
        // over raw utilization: with a lowered `weekly_ceiling` an account can be
        // exhausted at a weekly percent well BELOW the Red cutoff, yet it is
        // blocked for days — it must read Red, never the "healthy" Green its 65%
        // utilization would otherwise give. Mirrors what its WEEKLY reset cell shows
        // (the far weekly reset).
        let blocked = status_line_resets(
            "blocked",
            Some(30),               // session is fine…
            Some(65),               // …weekly below RED_UTIL_PCT, but…
            true,                   // …exhausted (e.g. weekly_ceiling lowered to 60)
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
        // Exhausted (the daemon's weekly_ceiling verdict) → Red even at a percent
        // well below the Red cutoff: a week-blocked cell never reads "healthy",
        // honoring a lowered weekly_ceiling (issue #11/#37).
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
            systemic_refresh_source: None,
            canonical_scrub: None,
            keychain_locked: false,
            canary: None,
            expiry_cohort: None,
            recent_blind_preempt_swap: None,
            recent_landing_overshoot: None,
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
            systemic_refresh_source: None,
            canonical_scrub: None,
            keychain_locked: false,
            canary: None,
            expiry_cohort: None,
            recent_blind_preempt_swap: None,
            recent_landing_overshoot: None,
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
            systemic_refresh_source: None,
            canonical_scrub: None,
            keychain_locked: false,
            canary: None,
            expiry_cohort: None,
            recent_blind_preempt_swap: None,
            recent_landing_overshoot: None,
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
            systemic_refresh_source: None,
            canonical_scrub: None,
            keychain_locked: false,
            canary: None,
            expiry_cohort: None,
            recent_blind_preempt_swap: None,
            recent_landing_overshoot: None,
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
            systemic_refresh_source: None,
            canonical_scrub: None,
            keychain_locked: false,
            canary: None,
            expiry_cohort: None,
            recent_blind_preempt_swap: None,
            recent_landing_overshoot: None,
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
            systemic_refresh_source: None,
            canonical_scrub: None,
            keychain_locked: false,
            canary: None,
            expiry_cohort: None,
            recent_blind_preempt_swap: None,
            recent_landing_overshoot: None,
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
            systemic_refresh_source: None,
            canonical_scrub: None,
            keychain_locked: false,
            canary: None,
            expiry_cohort: None,
            recent_blind_preempt_swap: None,
            recent_landing_overshoot: None,
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
            systemic_refresh_source: None,
            canonical_scrub: None,
            keychain_locked: false,
            canary: None,
            expiry_cohort: None,
            recent_blind_preempt_swap: None,
            recent_landing_overshoot: None,
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
            systemic_refresh_source: None,
            canonical_scrub: None,
            keychain_locked: false,
            canary: None,
            expiry_cohort: None,
            recent_blind_preempt_swap: None,
            recent_landing_overshoot: None,
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
            systemic_refresh_source: None,
            canonical_scrub: None,
            keychain_locked: false,
            canary: None,
            expiry_cohort: None,
            recent_blind_preempt_swap: None,
            recent_landing_overshoot: None,
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

        // launchd owns the process: bootout terminates the daemon and removes its job, so nothing
        // relaunches it. It is the daemon, so nothing else needs stopping.
        assert_eq!(plan_stop(Supervising), StopPlan::BootOut);

        // The regression state (issue #397 review): the job sits in the domain with NO process
        // behind it — `launchctl print` still exits 0 — while a foreground `run` owns the lock and
        // the socket. Bootout alone would report a stop that did not happen (the foreground daemon
        // keeps running); a socket shutdown alone would leave the idle agent registered. Both.
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
        // would only hand launchd a managed `run` that loses the lock and cleanly stands down
        // (exit 0), never restarting the foreground daemon — so refuse.
        assert_eq!(
            plan_restart(RegisteredIdle, true, true),
            RestartPlan::RefuseUnmanaged
        );
        assert_eq!(
            plan_restart(RegisteredIdle, true, false),
            RestartPlan::RefuseUnmanaged
        );
        // Registered, idle, and nothing running: `kickstart` starts a job that is not running, so
        // no bootstrap is needed and — with nothing holding the lock — the managed `run` comes up
        // and keeps the lock.
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
                systemic_refresh_source: None,
                canonical_scrub: None,
                keychain_locked: false,
                canary: None,
                expiry_cohort: None,
                recent_blind_preempt_swap: None,
                recent_landing_overshoot: None,
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
                systemic_refresh_source: None,
                canonical_scrub: None,
                keychain_locked: false,
                canary: None,
                expiry_cohort: None,
                recent_blind_preempt_swap: None,
                recent_landing_overshoot: None,
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
        let (config, outcomes) = apply_import(None, &restored, None, None, &target, false)
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
        let (config, outcomes) =
            apply_import(None, &payload, Some(local.clone()), None, &target, false)
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
        let (config, outcomes) =
            apply_import(None, &payload, Some(local.clone()), None, &target, true)
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
        let (config, outcomes) =
            apply_import(None, &payload, None, None, &FailingWriteStash, false)
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
        let (config, outcomes) = apply_import(None, &payload, None, None, &LyingReadStash, false)
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
        let (config, outcomes) = apply_import(None, &payload, None, None, &target, false)
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
        let (_config, outcomes) = apply_import(None, &payload, None, None, &target, false)
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

    // --- non-adoption of the active account (issue #1001) -------------------

    /// The token the TARGET machine is live on before the import — what the canonical
    /// `Claude Code-credentials` item holds, and what the active account's stash holds too
    /// (the state a swap leaves behind), so token-first resolution identifies it as active.
    const TOKEN_PRE_IMPORT: &[u8] = b"CREDENTIAL-TOKEN-PRE-IMPORT-0f0f0f0f0f";
    /// A third roster account, used where the machine must be active on an account the
    /// artifact does NOT carry.
    const UUID_C: &str = "33333333-3333-3333-3333-333333333333";

    /// Cap-1.1 (R-2, AC-2, AC-2a) — importing the target's **active** account reports that
    /// the credential was staged and NOT adopted, and names the command that actually
    /// completes the adoption.
    ///
    /// The assertion is on the **`--force` token**, not on "a command is named". Unqualified
    /// `use <label>` short-circuits on service-name equality in `SwapTarget::resolve`
    /// (`if account.stash() == active_stash { … AlreadyActive }`) and writes nothing — the
    /// committed `already_active_without_force_is_a_noop_success_with_zero_writes` in
    /// [`crate::use_account`] pins exactly that. A test that accepted the unqualified form
    /// would pass while shipping guidance that leaves the canonical holding the stale token:
    /// the defect, reproduced through its own remediation (PRD AC-2a).
    #[tokio::test]
    async fn importing_the_targets_active_account_reports_non_adoption_and_names_use_force() {
        // `alice` is the account this machine is logged into; `bob` is parked.
        let local = config_with(vec![acct("alice", UUID_A), acct("bob", UUID_B)]);
        let target = crate::stash::FakeAccountStash::empty();
        let payload = import_payload(&[
            ("alice", UUID_A, TOKEN_A, EMAIL_A),
            ("bob", UUID_B, TOKEN_B, EMAIL_B),
        ]);

        let (_config, outcomes) =
            apply_import(None, &payload, Some(local), Some(UUID_A), &target, true)
                .await
                .unwrap();

        let row = |label: &str| {
            outcomes
                .iter()
                .find(|o| o.label == label)
                .unwrap_or_else(|| panic!("`{label}` must appear in the report"))
        };
        assert!(
            row("alice").staged_not_adopted,
            "the ACTIVE account's credential lands in a slot nothing reads — say so"
        );
        assert!(
            !row("bob").staged_not_adopted,
            "a PARKED account's own stash IS the slot that is read — nothing is pending for it"
        );

        let report = import_report(&outcomes);
        assert!(
            report.contains("sessiometer use --force alice"),
            "the report must name the FORCING form for the active account; got {report}"
        );
        // The `--force` assertion above is only worth something if the report cannot ALSO be
        // naming the unqualified no-op somewhere: pin that every `use` it names is the forcing
        // one, and that it names exactly the one account that is actually active. This count is
        // the AC-2a guard, not incidental tidiness — sibling scope R-4 / Cap-2.1
        // (`docs/specs/import-staleness-warning.feature.md`) also emits a `use --force <label>`
        // line from `import`, so if that one lands inside `import_report` this goes red.
        // Relaxing the count is then the cheapest green AND the wrong fix, since it deletes the
        // only assertion stopping the unqualified form from creeping back in: print the
        // staleness warning from `import()` alongside this report instead, or re-scope this to
        // the notice substring.
        assert_eq!(
            report.matches("sessiometer use ").count(),
            1,
            "exactly one adoption command is named; got {report}"
        );
        assert!(
            !report.contains("--force bob"),
            "the parked account must not be offered for adoption; got {report}"
        );
        // C-3 / issue #15: the new line is a credential-adjacent surface like every other.
        for token in [TOKEN_A, TOKEN_B, TOKEN_PRE_IMPORT] {
            assert!(
                !contains_bytes(report.as_bytes(), token),
                "no credential token may appear in the non-adoption notice"
            );
        }
        for email in [EMAIL_A, EMAIL_B] {
            assert!(
                !report.contains(email),
                "no account email may appear in the non-adoption notice"
            );
        }
        assert!(
            !report.contains(UUID_A),
            "the notice names the LABEL, never the account uuid"
        );
    }

    /// Cap-1.2 (R-2a, C-2) — import adds no canonical writer: the canonical item is
    /// BYTE-UNCHANGED across a full import that overwrites the active account's stash.
    ///
    /// The store is wired into the path under test (import reads it to resolve who is
    /// active), so this is an assertion about a seam the code actually touches rather than
    /// one it merely lacks. What it pins is the divergence the notice exists to report: the
    /// stash moves to the imported token, the canonical does not move at all.
    #[tokio::test]
    async fn import_stages_the_active_accounts_credential_and_leaves_the_canonical_untouched() {
        let local = config_with(vec![acct("alice", UUID_A), acct("bob", UUID_B)]);
        let target = crate::stash::FakeAccountStash::empty();
        // Target state: logged in as `alice` — the canonical and her stash both hold the
        // pre-import token, which is what a completed swap leaves behind.
        target
            .write(
                &local.roster[0].stash(),
                &export_stashed(TOKEN_PRE_IMPORT, UUID_A, EMAIL_A),
            )
            .await
            .unwrap();
        let store = crate::keychain::FakeCredentialStore::empty();
        store
            .write(&Credential::new(TOKEN_PRE_IMPORT.to_vec()))
            .await
            .unwrap();
        // No `~/.claude.json` on this path: resolution must succeed on the TOKEN alone, so
        // the display fallback cannot be what makes the test pass.
        let dir = tempfile::tempdir().unwrap();
        let claude_json = dir.path().join("absent-claude.json");

        let active =
            resolve_active_uuid_for_import(&local.roster, &store, &target, &claude_json).await;
        assert_eq!(
            active.as_deref(),
            Some(UUID_A),
            "token-first resolution identifies the active account"
        );

        let payload = import_payload(&[
            ("alice", UUID_A, TOKEN_A, EMAIL_A),
            ("bob", UUID_B, TOKEN_B, EMAIL_B),
        ]);
        let (_config, outcomes) = apply_import(
            None,
            &payload,
            Some(local.clone()),
            active.as_deref(),
            &target,
            true,
        )
        .await
        .unwrap();

        assert_eq!(
            target
                .read(&local.roster[0].stash())
                .await
                .unwrap()
                .credential
                .expose(),
            TOKEN_A,
            "the imported credential IS staged into the active account's own stash"
        );
        assert_eq!(
            store.read().await.unwrap().expose(),
            TOKEN_PRE_IMPORT,
            "import must not write the canonical item — `use --force` owns that transition \
             under the #64 lock (R-2a, C-2)"
        );
        assert!(
            import_report(&outcomes).contains("sessiometer use --force alice"),
            "and the report must name the command that closes the gap it just left"
        );
    }

    /// Cap-1.2, the parked half — when NO imported account is the active one, every stash
    /// holds its imported credential, the canonical is untouched, and nothing is reported as
    /// pending adoption. A parked account's own stash IS the slot that will be read, so
    /// staging it there is the whole job; the notice must stay silent.
    #[tokio::test]
    async fn parked_accounts_are_staged_with_no_canonical_interaction_and_no_notice() {
        // The machine is logged into a THIRD account that the artifact does not carry.
        let local = config_with(vec![
            acct("alice", UUID_A),
            acct("bob", UUID_B),
            acct("carol", UUID_C),
        ]);
        let target = crate::stash::FakeAccountStash::empty();
        target
            .write(
                &local.roster[2].stash(),
                &export_stashed(TOKEN_PRE_IMPORT, UUID_C, "carol@example.com"),
            )
            .await
            .unwrap();
        let store = crate::keychain::FakeCredentialStore::empty();
        store
            .write(&Credential::new(TOKEN_PRE_IMPORT.to_vec()))
            .await
            .unwrap();
        let dir = tempfile::tempdir().unwrap();
        let claude_json = dir.path().join("absent-claude.json");

        let active =
            resolve_active_uuid_for_import(&local.roster, &store, &target, &claude_json).await;
        assert_eq!(
            active.as_deref(),
            Some(UUID_C),
            "carol is the active account"
        );

        // The artifact carries only the two PARKED accounts.
        let payload = import_payload(&[
            ("alice", UUID_A, TOKEN_A, EMAIL_A),
            ("bob", UUID_B, TOKEN_B, EMAIL_B),
        ]);
        let (_config, outcomes) = apply_import(
            None,
            &payload,
            Some(local.clone()),
            active.as_deref(),
            &target,
            true,
        )
        .await
        .unwrap();

        for (idx, token) in [(0, TOKEN_A), (1, TOKEN_B)] {
            assert_eq!(
                target
                    .read(&local.roster[idx].stash())
                    .await
                    .unwrap()
                    .credential
                    .expose(),
                token,
                "each parked account's own stash holds its imported credential"
            );
        }
        assert_eq!(
            store.read().await.unwrap().expose(),
            TOKEN_PRE_IMPORT,
            "the canonical item is untouched"
        );
        let report = import_report(&outcomes);
        assert!(
            !report.contains("--force"),
            "no account is pending adoption — the notice must stay silent; got {report}"
        );
    }

    /// The notice fires only when a credential was ACTUALLY staged for the active account.
    /// Two ways nothing is staged, both of which leave the canonical consistent with the
    /// stash and so have nothing pending adoption: the conflict policy SKIPPED the account
    /// (its stash is byte-for-byte untouched), and a config-only artifact carries no secret
    /// at all. A notice in either case is a false alarm telling the operator to force a swap
    /// that would change nothing.
    #[tokio::test]
    async fn non_adoption_is_silent_when_the_active_accounts_credential_was_never_staged() {
        let local = config_with(vec![acct("alice", UUID_A)]);

        // (a) Skipped: the artifact carries `alice`, but the default conflict policy leaves
        //     her untouched.
        let target = crate::stash::FakeAccountStash::empty();
        let payload = import_payload(&[("alice", UUID_A, TOKEN_A, EMAIL_A)]);
        let (_config, outcomes) = apply_import(
            None,
            &payload,
            Some(local.clone()),
            Some(UUID_A),
            &target,
            false,
        )
        .await
        .unwrap();
        assert_eq!(outcomes[0].outcome, ImportOutcome::Skipped);
        assert!(
            !outcomes[0].staged_not_adopted,
            "a skipped account wrote nothing — there is nothing to adopt"
        );
        assert!(!import_report(&outcomes).contains("--force"));

        // (b) Config-only artifact (an `export --no-secrets`): a roster entry lands, no
        //     stash is written, and the account is a `login` away — not a `use --force`.
        let config_only = Payload::new(
            config_with(vec![acct("alice", UUID_A)]).render(),
            Vec::new(),
        );
        let (_config, outcomes) =
            apply_import(None, &config_only, Some(local), Some(UUID_A), &target, true)
                .await
                .unwrap();
        assert_eq!(outcomes[0].outcome, ImportOutcome::Overwritten);
        assert!(
            !outcomes[0].staged_not_adopted,
            "a config-only import carries no credential — there is nothing to adopt"
        );
        assert!(!import_report(&outcomes).contains("--force"));
    }

    /// Active-account resolution is BEST-EFFORT for this path: a canonical the keychain will
    /// not hand over degrades to the `~/.claude.json` display signal rather than aborting an
    /// import the operator asked for — nothing is written on the strength of the answer.
    /// Both unreadable classes are covered, because they reach `read` by different arms:
    /// the LOCKED keychain and the scrubbed, confirmed-absent item.
    #[tokio::test]
    async fn active_resolution_falls_back_to_the_display_when_the_canonical_is_unreadable() {
        let local = config_with(vec![acct("alice", UUID_A), acct("bob", UUID_B)]);
        let stash = crate::stash::FakeAccountStash::empty();
        let dir = tempfile::tempdir().unwrap();
        let claude_json = dir.path().join("claude.json");
        std::fs::write(
            &claude_json,
            format!(r#"{{"oauthAccount":{{"accountUuid":"{UUID_B}"}}}}"#),
        )
        .unwrap();

        // Each state is built where it is named, so a third one cannot be added without
        // giving it its own setup (a `match` with a catch-all arm would silently absorb it).
        let locked = crate::keychain::FakeCredentialStore::empty();
        locked.set_locked(true);
        let not_found = crate::keychain::FakeCredentialStore::empty();
        not_found.set_not_found(true);
        for (state, store) in [("locked", &locked), ("not_found", &not_found)] {
            assert_eq!(
                resolve_active_uuid_for_import(&local.roster, store, &stash, &claude_json)
                    .await
                    .as_deref(),
                Some(UUID_B),
                "a {state} canonical leaves the display as the only signal — use it"
            );
        }

        // No signal at all (the SAME unreadable canonical, now with no display) resolves to
        // nothing, and no notice prints — the behaviour that shipped before #1001, never worse.
        assert_eq!(
            resolve_active_uuid_for_import(
                &local.roster,
                &locked,
                &stash,
                &dir.path().join("absent.json")
            )
            .await,
            None
        );
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
            None,
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
            None,
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

    // ---- import must not silently create a duplicate-label roster (#1005) ----
    //
    // Every test below builds its target EXPLICITLY rather than as `src_config.clone()`.
    // That clone is why the branch is unreachable in the conflict-policy test above: it makes
    // every `account_uuid` match by construction, so `existing` is always `Some` and no
    // same-label/different-uuid entry can ever be created (AC-3).

    /// A config-only migration artifact carrying `roster` — the hermetic path into
    /// [`apply_import`] (#135's roster-only "needs re-login" shape). No stash, no keychain,
    /// no swap lock: the roster-merge policy under test is independent of credential staging,
    /// and a config-only artifact exercises it without any of that plumbing.
    fn config_only_artifact(roster: Vec<Account>) -> Payload {
        Payload::new(config_with(roster).render(), Vec::new())
    }

    /// Merge a config-only artifact into `local` and hand back the merged config + report.
    async fn import_config_only(
        incoming: Vec<Account>,
        local: Option<Config>,
        overwrite: bool,
    ) -> (Config, Vec<AccountImport>) {
        apply_import(
            None,
            &config_only_artifact(incoming),
            local,
            None,
            &crate::stash::FakeAccountStash::empty(),
            overwrite,
        )
        .await
        .expect("a config-only import over a valid roster succeeds")
    }

    /// **An unknown config key names the import version floor instead of a bare serde line**
    /// (issue #1053). The artifact's config travels as TEXT and is re-parsed by THIS build's
    /// parser, whose every `Raw*` struct carries `deny_unknown_fields` — so an artifact minted
    /// by a NEWER build aborts the import over a key this one does not know, at any nesting
    /// level, while the container's `format_version` reads `1` on both sides and says
    /// compatible.
    ///
    /// That is the defect the rendered config has already inflicted twice, running in the
    /// direction this tree can still act on: builds older than the floor refuse every artifact
    /// a current build mints, and those binaries cannot be patched. Nothing here asserts what
    /// one of them prints.
    #[tokio::test]
    async fn an_unknown_config_block_names_the_import_version_floor_not_a_bare_parse_error() {
        // A block no build in this tree knows. Deliberately NOT `[credential]`: the current
        // parser KNOWS that one, so a test built on it would be green over nothing.
        let mut text = config_with(vec![acct("work", UUID_A)]).render();
        text.push_str("\n[telemetry]\nenabled = true\n");

        let outcome = apply_import(
            None,
            &Payload::new(text, Vec::new()),
            None,
            None,
            &crate::stash::FakeAccountStash::empty(),
            false,
        )
        .await;
        let err = match outcome {
            Err(err) => err,
            Ok(_) => panic!("an unknown config block must abort the import"),
        };

        assert!(
            matches!(err, Error::MigrationImportConfigRejected { .. }),
            "the shape failure must be re-badged, not left as a bare ConfigParse: {err:?}"
        );
        let shown = err.to_string();
        // The parser's own detail survives — it names the offending key, the actionable half
        // a re-badge must not swallow.
        assert!(
            shown.contains("telemetry"),
            "the message must still name the block that failed: {shown}"
        );
        // ...and the floor is stated, which is the half a bare serde line never carries.
        assert!(
            shown.contains(crate::migration::CONFIG_BLOCK_VERSION_FLOOR),
            "the message must carry the version floor verbatim: {shown}"
        );
        assert_eq!(
            err.exit_code(),
            1,
            "re-badging changes what the operator READS, not what a script branches on"
        );
    }

    /// The re-badge is scoped to the symptom it detects (issue #1053). A config this build
    /// parsed FINE and then refused on a range or roster invariant is not a version-floor
    /// symptom; handing it the floor's explanation would send an operator chasing a
    /// compatibility problem they do not have.
    #[test]
    fn only_the_shape_failure_is_re_badged_a_validation_verdict_keeps_its_own_message() {
        assert!(
            matches!(
                name_the_import_version_floor(Error::ConfigParse("unknown field `x`".to_owned())),
                Error::MigrationImportConfigRejected { .. }
            ),
            "a SHAPE failure is the version-floor symptom"
        );
        for untouched in [
            Error::ConfigInvalid("poll_secs out of range".to_owned()),
            Error::MigrationImportVerifyFailed,
        ] {
            let shown = untouched.to_string();
            assert_eq!(
                name_the_import_version_floor(untouched).to_string(),
                shown,
                "only ConfigParse is re-badged; every other verdict keeps its own message"
            );
        }
    }

    /// The label-INDEPENDENT core of the duplicate-label notice — what a NEGATIVE assertion
    /// searches for, since its claim is "no notice at all", not "none for this one label".
    ///
    /// [`duplicate_notice_for`] is built from it on purpose. Reword [`duplicate_label_notice`]
    /// and the positive assertions go red, which forces this constant to move with it; spell
    /// the string out separately in each place and that reword instead leaves every negative
    /// assertion hunting text the notice no longer contains — passing forever, proving nothing.
    const DUPLICATE_NOTICE: &str = "now labels more than one account";

    /// The duplicate-label notice's opening clause FOR ONE LABEL, as the operator reads it —
    /// the search key for a positive assertion.
    fn duplicate_notice_for(label: &str) -> String {
        format!("`{label}` {DUPLICATE_NOTICE}")
    }

    #[tokio::test]
    async fn apply_import_warns_when_it_creates_a_same_label_different_uuid_entry() {
        // R-6: the target already labels UUID_A `work`; the artifact carries a DIFFERENT
        // Anthropic account under the same handle (the operator relabelled, or reused it).
        let local = config_with(vec![acct("work", UUID_A)]);
        let (merged, outcomes) =
            import_config_only(vec![acct("work", UUID_B)], Some(local), false).await;

        // The BUT-NOTs first: not refused, and not renamed. Duplicate labels stay accepted.
        assert_eq!(
            count_import_outcomes(&outcomes),
            (1, 0, 0, 0),
            "the account still imports — the warning does not refuse it"
        );
        assert_eq!(merged.roster.len(), 2, "both entries are kept");
        assert!(
            merged.roster.iter().all(|account| account.label == "work"),
            "neither entry was renamed to make the label unique"
        );

        assert!(outcomes[0].duplicate_label, "the creating row is flagged");
        let report = import_report(&outcomes);
        assert!(
            report.contains(&duplicate_notice_for("work")),
            "the operator is warned: {report}"
        );
        assert!(
            report.contains("imported `work`") && report.contains("1 imported"),
            "the row and the tally read as an ordinary success: {report}"
        );
    }

    #[tokio::test]
    async fn apply_import_stays_quiet_on_the_ordinary_same_label_same_uuid_case() {
        // `account_uuid` is the Claude account uuid and is STABLE across machines, so
        // same-label/same-uuid is the ordinary cross-machine import — the common case. A
        // warning here would train dismissal of the one above (PRD § P5, risk P5).
        //
        // Silent under BOTH conflict policies, for two different reasons: `skip` returns
        // before the roster is touched at all, while `overwrite` replaces the entry in place
        // and an account does not collide with itself. Asserting only one policy would leave
        // the other's reason unproven.
        for overwrite in [false, true] {
            let local = config_with(vec![acct("work", UUID_A)]);
            let (merged, outcomes) =
                import_config_only(vec![acct("work", UUID_A)], Some(local), overwrite).await;
            assert_eq!(merged.roster.len(), 1, "overwrite={overwrite}");
            assert!(
                outcomes.iter().all(|entry| !entry.duplicate_label),
                "the ordinary cross-machine import must not warn (overwrite={overwrite})"
            );
            let report = import_report(&outcomes);
            assert!(
                !report.contains(DUPLICATE_NOTICE),
                "overwrite={overwrite}: {report}"
            );
        }
    }

    #[tokio::test]
    async fn apply_import_warns_when_one_artifact_carries_its_own_duplicate_on_a_fresh_target() {
        // The case an implementer gets wrong. Reading R-6's "already exists ON THE TARGET"
        // literally means checking each incoming label against `local` — and here `local` is
        // `None`, so that check is skipped entirely, both entries append in one shot, and the
        // exact state R-6 exists to prevent is created with every other criterion green.
        //
        // On a fresh target this is the ONLY way the collision can arrive: nothing rejects a
        // duplicate label on the way in (`Config::validate` checks empty uuid, empty label and
        // duplicate uuid, and has no duplicate-label arm), so a roster already carrying the
        // accepted collision mints an artifact carrying it internally.
        let (merged, outcomes) =
            import_config_only(vec![acct("dup", UUID_A), acct("dup", UUID_B)], None, false).await;

        assert_eq!(
            count_import_outcomes(&outcomes),
            (2, 0, 0, 0),
            "both import — the warning does not refuse either"
        );
        assert_eq!(merged.roster.len(), 2, "uniqueness is not enforced");
        assert!(
            outcomes.iter().all(|entry| entry.duplicate_label),
            "both bearers this import wrote are part of the ambiguity it created"
        );
        let report = import_report(&outcomes);
        assert!(
            report.contains(&duplicate_notice_for("dup")),
            "the operator is warned on a fresh target too"
        );
        assert_eq!(
            report.matches(&duplicate_notice_for("dup")).count(),
            1,
            "two flagged rows, one label, one notice: {report}"
        );
    }

    #[tokio::test]
    async fn apply_import_stays_quiet_on_an_ordinary_import_of_distinct_labels() {
        // The most ordinary import there is: new accounts, new labels, nothing ambiguous. It has
        // to be asserted rather than assumed, because it is what gates the `after > 1` half of the
        // rule — with only `after > before`, every brand-new label (0 → 1 bearers) would warn, and
        // the notice would appear on essentially every import ever run.
        let (merged, outcomes) = import_config_only(
            vec![acct("alpha", UUID_A), acct("beta", UUID_B)],
            None,
            false,
        )
        .await;

        assert_eq!(merged.roster.len(), 2);
        assert!(
            outcomes.iter().all(|entry| !entry.duplicate_label),
            "0 → 1 bearers is not an ambiguity"
        );
        let report = import_report(&outcomes);
        assert!(
            !report.contains(DUPLICATE_NOTICE),
            "the most ordinary import there is must say nothing: {report}"
        );
    }

    #[tokio::test]
    async fn apply_import_stays_quiet_when_an_import_swaps_two_labels_between_accounts() {
        // The operator swapped the two handles on machine 1 and exported. Machine 2 already has
        // both accounts, so both are overwritten in place and the finished roster carries each
        // label exactly once — nothing to warn about.
        //
        // A per-write check reads the roster MID-MERGE and gets this wrong: replacing the first
        // entry's label with `b` leaves the roster transiently `[b, b]`, which looks exactly like
        // a collision until the second replacement makes it `[b, a]`. The warning would then name
        // a label that resolves perfectly well, and tell the operator to go substitute a uuid for
        // a handle that works — training dismissal of the warning that matters (PRD § P5).
        let local = config_with(vec![acct("a", UUID_A), acct("b", UUID_B)]);
        let (merged, outcomes) = import_config_only(
            vec![acct("b", UUID_A), acct("a", UUID_B)],
            Some(local),
            true,
        )
        .await;

        assert_eq!(
            merged
                .roster
                .iter()
                .map(|account| account.label.as_str())
                .collect::<Vec<_>>(),
            ["b", "a"],
            "the labels swapped; neither is duplicated"
        );
        assert!(
            outcomes.iter().all(|entry| !entry.duplicate_label),
            "a transient mid-merge collision is not a created one"
        );
        let report = import_report(&outcomes);
        assert!(
            !report.contains(DUPLICATE_NOTICE),
            "a swap the merge only passed through must say nothing: {report}"
        );
    }

    #[tokio::test]
    async fn apply_import_stays_quiet_when_a_duplicate_the_target_already_had_is_overwritten() {
        // AC-1 is "warns when it would CREATE" a duplicate-label entry. Re-importing an
        // already-duplicated roster onto a machine that already carries it replaces both entries
        // in place and creates nothing: the count goes 2 → 2. The operator was warned when the
        // duplicate was actually made; repeating it on every subsequent import is the same
        // dismissal-training failure as the swap case above.
        let local = config_with(vec![acct("dup", UUID_A), acct("dup", UUID_B)]);
        let (merged, outcomes) = import_config_only(
            vec![acct("dup", UUID_A), acct("dup", UUID_B)],
            Some(local),
            true,
        )
        .await;

        assert_eq!(merged.roster.len(), 2, "both replaced in place");
        assert_eq!(count_import_outcomes(&outcomes), (0, 0, 2, 0));
        assert!(
            outcomes.iter().all(|entry| !entry.duplicate_label),
            "the import did not create this duplicate — it was already there"
        );
    }

    #[tokio::test]
    async fn apply_import_warns_when_it_deepens_a_duplicate_the_target_already_had() {
        // The mirror of the test above, and the reason its rule is "more bearers than before"
        // rather than "the duplicate is new": a target already ambiguous at two bearers, made
        // WORSE by a third, is a change the operator has to know about.
        let local = config_with(vec![acct("dup", UUID_A), acct("dup", UUID_B)]);
        let (merged, outcomes) =
            import_config_only(vec![acct("dup", UUID_C)], Some(local), false).await;

        assert_eq!(merged.roster.len(), 3);
        assert_eq!(count_import_outcomes(&outcomes), (1, 0, 0, 0));
        assert!(outcomes[0].duplicate_label, "2 → 3 bearers is a creation");
    }

    #[tokio::test]
    async fn apply_import_warns_when_an_overwrite_relabels_onto_an_existing_label() {
        // The collision need not arrive by APPEND. Here the artifact re-labels UUID_B `work`,
        // which UUID_A already carries: the overwrite replaces B's entry IN PLACE, so the
        // roster gains a second `work` without gaining an entry. That is what rules out the
        // tempting shortcut of flagging the append branch (`existing.is_none()`) — it would
        // stay silent right here. Counting bearers catches it because the count is what moved:
        // `work` goes 1 → 2 while the roster stays at 2.
        let local = config_with(vec![acct("work", UUID_A), acct("spare", UUID_B)]);
        let (merged, outcomes) =
            import_config_only(vec![acct("work", UUID_B)], Some(local), true).await;

        assert_eq!(
            count_import_outcomes(&outcomes),
            (0, 0, 1, 0),
            "replaced in place, not appended"
        );
        assert_eq!(merged.roster.len(), 2, "no entry added or dropped");
        assert!(
            outcomes[0].duplicate_label,
            "the relabel created the collision"
        );
        let report = import_report(&outcomes);
        assert!(
            report.contains(&duplicate_notice_for("work")),
            "a collision that arrives by REPLACEMENT is still warned about: {report}"
        );
    }

    #[test]
    fn the_import_report_warns_once_per_duplicated_label_before_the_non_adoption_notice() {
        let outcomes = vec![
            AccountImport::imported("solo"),
            AccountImport::imported("dup").duplicate_label(),
            // A three-way collision flags a SECOND row under the same label. That is one
            // problem for the operator, so it must read as one notice.
            AccountImport::imported("dup").duplicate_label(),
            // A different duplicated label in the same import is genuinely a second problem.
            AccountImport::imported("other").duplicate_label(),
            // And this row is both the machine's active account and a duplicate bearer.
            AccountImport::imported("live")
                .staged_not_adopted()
                .duplicate_label(),
        ];
        let report = import_report(&outcomes);

        assert_eq!(
            report.matches(&duplicate_notice_for("dup")).count(),
            1,
            "one notice per DISTINCT label: {report}"
        );
        for label in ["other", "live"] {
            assert!(
                report.contains(&duplicate_notice_for(label)),
                "a second duplicated label is a second notice: {report}"
            );
        }

        // Ordering is load-bearing rather than cosmetic. The non-adoption notice instructs
        // `use --force live` — and `live` is a label this same import just made ambiguous, so
        // that instruction is itself now a refusal. The duplicate notice is the one that says
        // to substitute an account-uuid anywhere the label would have gone, which includes the
        // line below it, so it has to be read first.
        let duplicate_at = report
            .find(&duplicate_notice_for("live"))
            .expect("the active account's duplicate notice is present");
        let non_adoption_at = report
            .find("is this machine's active account")
            .expect("the non-adoption notice is present");
        assert!(
            duplicate_at < non_adoption_at,
            "the duplicate notice must precede the non-adoption one: {report}"
        );

        // Neither notice disturbs the four-way tally — every account genuinely imported.
        assert!(
            report.contains("5 imported, 0 skipped, 0 overwritten, 0 failed"),
            "{report}"
        );
    }

    #[test]
    fn the_duplicate_label_notice_names_the_remedy_and_only_the_label() {
        let notice = duplicate_label_notice("work");
        // It must not read as a failure: the import kept both entries.
        // True on BOTH import paths — an append keeps every entry, while an overwrite replaces
        // one in place. A body that claimed "kept both entries and changed neither" would be
        // false in the very report whose row above it reads `overwritten`.
        assert!(
            notice.contains("neither refused anything nor renamed anything"),
            "{notice}"
        );
        // The remedy is the account-uuid, because there is no disambiguator flag — a refusal
        // with no stated way out would be worse than the first-match-wins it replaced.
        assert!(
            notice.contains("account-uuid"),
            "the remedy must be named: {notice}"
        );
        assert!(
            notice.contains("sessiometer list"),
            "and where to read one off: {notice}"
        );
        // Issue #15: the operator's own handle, and nothing else. No uuid is printed here —
        // `list` is where the full uuids live. Two assertions, not one `&&`, so a failure says
        // WHICH kind of identifier leaked.
        assert!(
            !notice.contains(UUID_A),
            "the notice names the LABEL, never an account uuid: {notice}"
        );
        assert!(
            !notice.contains('@'),
            "no account email may appear in the notice: {notice}"
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

    /// The three help constants issue #885 documents the REFRESH-token expiry state on.
    const EXPIRY_HELP_SURFACES: &[(&str, &str)] = &[
        ("STATUS_USAGE", STATUS_USAGE),
        ("LOGIN_USAGE", LOGIN_USAGE),
        ("STATS_USAGE", STATS_USAGE),
    ];

    /// The tokens this guard ADDS to the repo-wide `--help` subset — the whole of what it
    /// contributes beyond
    /// `every_help_surface_carries_no_banned_framing_but_the_guard_bites_on_injection`, which
    /// already scans these same surfaces. That overlap is ASSERTED rather than argued, in
    /// [`the_expiry_guard_still_earns_its_extras_against_the_central_list`] below, because the
    /// remedy that test names — delete this guard — is only safe while it holds.
    ///
    /// `beat` is the circumvention CALL §D-STA-6 enumerates — "beat/bypass limits", the same list
    /// that carries buy / purchase / upgrade / cancel — of the same class as the central `bypass`,
    /// which [`crate::framing_vocabulary::BANNED_TOKENS`] already carries. This one it does not,
    /// and that gap is this guard's entire reason to still exist: issue #918 KEPT it on exactly
    /// that ground. Issue #885 is the SURFACE this guard scans, not the source of the list.
    ///
    /// It stays LOCAL rather than joining the central list, which issue #1134 asked directly, and
    /// the reason is that `beat` is a HOMOGRAPH this crate already spends neutrally:
    /// `src/daemon/socket.rs` calls the `watch` liveness frame a "beat" throughout. The central
    /// list is scanned by every audience in that module's table, one of them (`Error`'s
    /// templates, issue #1139) with NO exemption set at all, so banning `beat` there would put it
    /// in front of the daemon's own liveness vocabulary. Measured: no scanned surface spells the
    /// bare word today — `socket.rs` uses it only in comments and a local — so the exposure is
    /// prospective, and its redemption would be a per-variant ledger entry. On the surfaces below
    /// the word has one reading — outrunning a refresh-token
    /// deadline — which is where the ban is earned, so that is where it lives. The cost of that
    /// choice is stated rather than hidden: `beat` remains unscanned on every OTHER
    /// operator-facing surface.
    const EXPIRY_HELP_EXTRA_TOKENS: &[&str] = &["beat"];

    /// The token list [`scan_expiry_help`] scans against: the repo-wide help subset (issue #918)
    /// plus [`EXPIRY_HELP_EXTRA_TOKENS`], DERIVED on every call rather than hand-listed.
    ///
    /// Issue #1134: this used to be a hand-maintained token list beside its own inline word-split
    /// — a second definition of "what counts as a word" that nothing asserted agreed with
    /// `crate::framing_vocabulary`'s, and which already diverged (that tokenizer strips ANSI SGR
    /// runs; this one did not). Deriving the list is the rule
    /// `crate::framing_vocabulary::banned_tokens_except` applies to the exemption-based
    /// audiences, run the other way round: this audience ADDS to the central subset instead of
    /// subtracting from it, so a token added centrally is covered here on the next run without a
    /// second edit.
    ///
    /// The derivation also STRENGTHENS the guard, and the direction is worth being exact about.
    /// The hand list carried no value-judgement token at all — no `healthy`, no `critical`, no
    /// `risk` — so "the EXPIRY column is healthy" cleared it. That was never a live gap, because
    /// the repo-wide help guard scans these same constants and does carry them; what it was is a
    /// second list to maintain, drifting from the first in both directions at once.
    fn expiry_banned_tokens() -> Vec<&'static str> {
        help_banned_tokens()
            .into_iter()
            .chain(EXPIRY_HELP_EXTRA_TOKENS.iter().copied())
            .collect()
    }

    /// The first banned token or acquisitive phrase in `text`, or `None` when it is clean —
    /// [`crate::framing_vocabulary::scan_with`], the ONE tokenizer every framing guard shares,
    /// over [`expiry_banned_tokens`] and the central `BANNED_PHRASES`.
    ///
    /// The phrase list is the central one UNCHANGED, where this guard used to add `need more`.
    /// That extra is not dropped coverage but a provably DEAD entry: `scan_with` matches every
    /// token before any phrase, and any text where the phrase `need more` matches contains the
    /// word `need`, which the derived token list above carries. So the sentence is still caught —
    /// on `need`, one word earlier, and now also when it is spelled without `more`. The bite
    /// assertion below reports `need` rather than `need more` for exactly this reason.
    fn scan_expiry_help(text: &str) -> Option<&'static str> {
        scan_with(text, &expiry_banned_tokens(), BANNED_PHRASES)
    }

    /// Issue #885 AC1–AC3: the operator-facing help states the per-account expiry cell, the
    /// counterintuitive non-extending deadline, the not-observable reading of the gap, and names
    /// `sessiometer login` as the remedy.
    #[test]
    fn expiry_help_carries_the_state_the_fixed_deadline_and_the_login_remedy() {
        // The two surfaces that carry the lapsed state AND its remedy; `STATS_USAGE` names
        // neither (it documents a column that does not populate yet), so it is asserted apart.
        let remedy_surfaces = [("STATUS_USAGE", STATUS_USAGE), ("LOGIN_USAGE", LOGIN_USAGE)];

        // AC1 — the cell's three renderings, and the remedy named on both surfaces that carry it.
        for (name, help) in remedy_surfaces {
            assert!(
                help.contains("lapsed"),
                "{name} must name the lapsed state:\n{help}"
            );
            assert!(
                help.contains("sessiometer login"),
                "{name} must name `sessiometer login` as the remedy:\n{help}"
            );
        }
        assert!(
            STATUS_USAGE.contains("EXPIRY column"),
            "STATUS_USAGE must describe the EXPIRY column:\n{STATUS_USAGE}"
        );
        assert!(
            STATS_USAGE.contains("`expiry` column"),
            "STATS_USAGE must describe the expiry column:\n{STATS_USAGE}"
        );

        // AC2 — refreshing does NOT extend the refresh-token deadline. The single fact an
        // operator is most likely to guess wrong, because every other expiry here slides forward.
        // Pinned to the claim's subject + negated verb, not to a whole sentence: the surrounding
        // wording is free to change, the negation is not.
        assert!(
            STATUS_USAGE.contains("Refreshing does NOT extend"),
            "STATUS_USAGE must state that refreshing does not extend the deadline:\n{STATUS_USAGE}"
        );
        for (name, help) in remedy_surfaces {
            assert!(
                help.contains("no refresh moves"),
                "{name} must state the deadline is fixed against refresh:\n{help}"
            );
        }

        // AC3 — the gap is NOT OBSERVABLE, never "not expiring" (the issue #137 invariant).
        assert!(
            STATUS_USAGE.contains("NOT OBSERVED"),
            "STATUS_USAGE must read the gap as not observed:\n{STATUS_USAGE}"
        );
        assert!(
            STATUS_USAGE.contains("never \"not expiring\""),
            "STATUS_USAGE must reject the 'not expiring' reading explicitly:\n{STATUS_USAGE}"
        );
    }

    /// Issue #885 AC4: the expiry help states the deadline as a present-tense FACT and never turns
    /// it into a call to action. A refresh-token deadline clears the §D-STA-6 firewall because it
    /// is a SERVER-ISSUED timestamp about authentication lifetime — not a rate projection about
    /// capacity — and its remedy is free and local; what stays out is the imperative.
    #[test]
    fn expiry_help_carries_no_banned_token_but_the_guard_bites_on_injection() {
        for (name, help) in EXPIRY_HELP_SURFACES {
            assert_eq!(
                scan_expiry_help(help),
                None,
                "{name} must carry no banned token or imperative framing:\n{help}"
            );
        }

        // The guard BITES: it is only evidence if it can fail. Inject each shape into a real
        // surface and confirm it is caught — a scanner that matches nothing passes every
        // "no false positives" test while proving nothing.
        assert_eq!(
            scan_expiry_help(&format!("{STATUS_USAGE}\nYou should re-login.")),
            Some("should")
        );
        assert_eq!(
            scan_expiry_help(&format!("{STATUS_USAGE}\nUpgrade your plan.")),
            Some("upgrade")
        );
        assert_eq!(
            scan_expiry_help(&format!("{STATUS_USAGE}\nExpiring soon.")),
            Some("soon")
        );
        assert_eq!(
            scan_expiry_help(&format!("{STATUS_USAGE}\nRunning out — need more seats.")),
            Some("need"),
            "the acquisitive call is caught on the central token, one word before the phrase"
        );

        // The token this guard exists FOR (issue #918 kept it on this one contribution alone),
        // and until issue #1134 nothing asserted it fires at all — every bite above is a token
        // the repo-wide help guard already carries, so the whole set would have passed with
        // `beat` absent from the list.
        assert_eq!(
            scan_expiry_help(&format!("{STATUS_USAGE}\nYou can beat the deadline.")),
            Some("beat"),
            "`beat` is this guard's only unique contribution — it must bite"
        );

        // Derivation control, in the direction the hand list was WEAK: a value judgement. The
        // old list carried none, so this line cleared the expiry guard entirely and was caught
        // only by the repo-wide scan over the same constants.
        assert_eq!(
            scan_expiry_help(&format!("{STATUS_USAGE}\nYour credential is healthy.")),
            Some("healthy"),
            "the derived list must carry the central value-judgement group the hand list omitted"
        );

        // …and it does NOT bite the permitted FACT: the deadline itself, its `lapsed` state, and
        // the free local remedy are all descriptive, which is exactly what AC1 requires alongside
        // AC4. Word-boundary matching also keeps `bypasses` from tripping `bypass`.
        assert_eq!(
            scan_expiry_help(
                "expiry 6d21h; lapsed once past; recovered by `sessiometer login`; nothing bypasses it"
            ),
            None
        );
    }

    // --- the framing guard, across the WHOLE `--help` surface (issue #918) -----------

    /// Every [`HelpTopic`] there is. The framing guard below scans the help of ALL of them, so
    /// coverage is the surface rather than a hand-picked sample of it — issue #918 exists because
    /// a claim of help coverage outran the coverage itself, and a guard that skips a verb would
    /// recreate that one subcommand at a time.
    ///
    /// Two tripwires keep the list complete:
    /// 1. Adding a `HelpTopic` variant makes `topic_const_name`'s match non-exhaustive, so the
    ///    build fails until the new topic is named.
    /// 2. `every_help_constant_is_scanned` reads this file's own source and asserts every
    ///    `const *_USAGE: &str` it declares is reached from here — so writing the match arm but
    ///    forgetting this table reddens too, which the compiler alone would not catch.
    const ALL_HELP_TOPICS: &[HelpTopic] = &[
        HelpTopic::Root,
        HelpTopic::Capture,
        HelpTopic::Login,
        HelpTopic::Run,
        HelpTopic::Service,
        HelpTopic::Daemon,
        HelpTopic::Config,
        HelpTopic::Status,
        HelpTopic::List,
        HelpTopic::Use,
        HelpTopic::Disable,
        HelpTopic::Enable,
        HelpTopic::Remove,
        HelpTopic::Poke,
        HelpTopic::Stats,
        HelpTopic::Reliability,
        HelpTopic::Log,
        HelpTopic::Export,
        HelpTopic::Import,
    ];

    /// The name of the constant a topic's help text lives in — used to name the offender in an
    /// assertion message, and, being an exhaustive match, to make a new subcommand a BUILD error
    /// here rather than a silently unscanned surface.
    fn topic_const_name(topic: HelpTopic) -> &'static str {
        match topic {
            HelpTopic::Root => "ROOT_USAGE",
            HelpTopic::Capture => "CAPTURE_USAGE",
            HelpTopic::Login => "LOGIN_USAGE",
            HelpTopic::Run => "RUN_USAGE",
            HelpTopic::Service => "SERVICE_USAGE",
            HelpTopic::Daemon => "DAEMON_USAGE",
            HelpTopic::Config => "CONFIG_USAGE",
            HelpTopic::Status => "STATUS_USAGE",
            HelpTopic::List => "LIST_USAGE",
            HelpTopic::Use => "USE_USAGE",
            HelpTopic::Disable => "DISABLE_USAGE",
            HelpTopic::Enable => "ENABLE_USAGE",
            HelpTopic::Remove => "REMOVE_USAGE",
            HelpTopic::Poke => "POKE_USAGE",
            HelpTopic::Stats => "STATS_USAGE",
            HelpTopic::Reliability => "RELIABILITY_USAGE",
            HelpTopic::Log => "LOG_USAGE",
            HelpTopic::Export => "EXPORT_USAGE",
            HelpTopic::Import => "IMPORT_USAGE",
        }
    }

    /// The whole scanned surface: every topic's constant name paired with the text it prints.
    fn help_surfaces() -> Vec<(&'static str, &'static str)> {
        ALL_HELP_TOPICS
            .iter()
            .map(|topic| (topic_const_name(*topic), topic.help()))
            .collect()
    }

    /// The name of the help constant a top-level declaration on `line` introduces, or `None` when
    /// the line declares no such thing.
    ///
    /// This is a TEXTUAL parser over source, which makes it the soft spot of the completeness
    /// tripwire that calls it: a declaration spelling it fails to recognise is a help surface the
    /// gate cannot see — the very defect of issue #918, reproduced one level down inside its own
    /// fix. It therefore strips visibility STRUCTURALLY (`pub`, `pub(crate)`, `pub(super)`,
    /// `pub(in …)`) rather than matching a fixed list of prefixes, so the accepted set is CLOSED
    /// over Rust's visibility grammar instead of being three spellings someone happened to think
    /// of. `the_declaration_parser_recognises_every_visibility_spelling` pins each accepted form,
    /// because an untested parser inside a completeness guard is that same defect again.
    ///
    /// Column-0 only: the help constants are top-level, and this deliberately ignores any indented
    /// `const … : &str` a test module declares for its own fixtures. Whitespace normalisation is
    /// not a variable here — `cargo fmt --all --check` is a gate, so the source this reads is
    /// always rustfmt-shaped.
    fn declared_help_constant(line: &str) -> Option<&str> {
        declared_str_constant(line).filter(|name| name.ends_with("_USAGE"))
    }

    /// The name of ANY top-level string-valued item a declaration on `line` introduces — the
    /// shared grammar [`declared_help_constant`] and [`declared_prose_constant`] both filter.
    ///
    /// Factored out rather than copied because the doc above names this parser as the soft spot
    /// of the tripwires that call it: two hand-maintained copies of Rust's declaration grammar
    /// would be two chances to miss a spelling, and only one of them would be under test.
    ///
    /// Three axes vary independently and all three are handled STRUCTURALLY, so the accepted set
    /// is closed over each rather than being the spellings someone thought of:
    ///
    /// - **Visibility** — `pub`, `pub(crate)`, `pub(super)`, `pub(in …)`, stepped over by parsing
    ///   the scope rather than by matching a fixed list of prefixes.
    /// - **Item kind** — `const` AND `static`. Both declare a top-level value that ships, and
    ///   nothing about a framing guard cares which keyword introduced the prose.
    /// - **Type** — delegated to [`is_str_shaped`], which accepts every string spelling rather
    ///   than the literal `&str` this parser originally matched.
    ///
    /// The item-kind and type axes were added by issue #1123's merge review, which defeated the
    /// `const`-plus-`&str`-only grammar five ways — `static X: &str`, `const X: &'static str`,
    /// `&[&str]`, `[&str; N]` — every one rustfmt-stable and invisible to the disposition gate. A
    /// gate you evade by DECLARING a constant differently is no more a gate than one you evade by
    /// NAMING it differently, which is the lexical guess issue #918 already rejected.
    ///
    /// Column-0 only, which is why this never reaches for `trim_start`: the sole leading space it
    /// tolerates is the one `pub` leaves behind. An indented `const` belongs to a test module's
    /// own fixtures and is deliberately none of this gate's business.
    fn declared_str_constant(line: &str) -> Option<&str> {
        let (after_visibility, had_visibility) = match line.strip_prefix("pub") {
            // `pub(crate)` / `pub(super)` / `pub(in crate::…)` — step over the scope.
            Some(rest) => match rest.strip_prefix('(') {
                Some(scoped) => (scoped.split_once(')')?.1, true),
                None => (rest, true),
            },
            None => (line, false),
        };
        let body = if had_visibility {
            after_visibility.strip_prefix(' ')?
        } else {
            after_visibility
        };
        let declaration = body
            .strip_prefix("const ")
            .or_else(|| body.strip_prefix("static "))?;
        let (name, rest) = declaration.split_once(": ")?;
        // The type is everything up to the initialiser. A multi-line declaration breaks after the
        // `=`, so an absent ` = ` leaves the remainder as-is rather than rejecting the line.
        let ty = rest.split_once(" = ").map_or(rest, |(ty, _)| ty);
        is_str_shaped(ty).then_some(name)
    }

    /// Whether `ty` is STRING-shaped — the property that makes a declaration shipped prose rather
    /// than a number, a duration or a byte string. Decided by looking for the whole WORD `str`,
    /// so it covers `&str`, `&'static str`, `&[&str]` and `[&str; N]` without enumerating them,
    /// while `usize`, `Duration`, `&[u8]` and `&OsStr` all fail it.
    ///
    /// Deliberately errs toward ACCEPTING. A type it wrongly accepts costs one disposition entry
    /// and a moment's annoyance; a type it wrongly rejects is a shipped string no framing guard
    /// reaches, which is the whole failure class this tripwire exists to end.
    fn is_str_shaped(ty: &str) -> bool {
        ty.split(|c: char| !c.is_ascii_alphanumeric() && c != '_')
            .any(|word| word == "str")
    }

    /// The name of a top-level `&str` constant that is NOT a help surface — the candidate
    /// operator-prose constants issue #1123's disposition tripwire enumerates. The complement of
    /// [`declared_help_constant`] over the same closed visibility grammar, so the two together
    /// account for every `const … : &str` this file declares and neither can be evaded by a
    /// spelling the other would have caught.
    fn declared_prose_constant(line: &str) -> Option<&str> {
        declared_str_constant(line).filter(|name| !name.ends_with("_USAGE"))
    }

    /// Every spelling that declares a shipped string at column 0, as the PRODUCT of the three
    /// axes Rust varies independently — visibility, item kind, and type. Enumerated as a product
    /// rather than as the handful anyone happened to think of, because each axis was added to
    /// this parser only after a review found the cell it was missing.
    fn declaration_spellings(name: &str) -> Vec<String> {
        let mut out = Vec::new();
        for visibility in [
            "",
            "pub ",
            "pub(crate) ",
            "pub(super) ",
            "pub(in crate::cli) ",
        ] {
            for kind in ["const", "static"] {
                for (ty, value) in [
                    ("&str", "\"x\""),
                    ("&'static str", "\"x\""),
                    ("&[&str]", "&[\"x\"]"),
                    ("[&str; 1]", "[\"x\"]"),
                ] {
                    out.push(format!("{visibility}{kind} {name}: {ty} = {value};"));
                }
            }
        }
        out
    }

    /// [`declared_str_constant`] is the one textual link in the completeness chain, so it is
    /// tested directly rather than only through its callers. Every declaration spelling Rust
    /// would accept on a shipped string must be recognised; one that slips through is an
    /// operator-facing surface that ships entirely unscanned while every gate stays green.
    ///
    /// That is not hypothetical, and it has now happened twice on two different axes. Before this
    /// test existed the parser matched only a bare `const `, so a `pub(crate) const PURGE_USAGE`
    /// carrying banned framing was invisible to it. Before issue #1123's merge review it matched
    /// only the item kind `const` and the type `&str`, so `static DEGRADED_CUE: &str`, or a
    /// `&[&str]` list of advisories, was invisible in exactly the same way — a gate you evade by
    /// DECLARING a constant differently is no more a gate than one you evade by NAMING it
    /// differently. Hence the product above rather than a list.
    #[test]
    fn the_declaration_parser_recognises_every_declaration_spelling() {
        // Collected rather than asserted one at a time, deliberately: the subject is PRODUCT
        // coverage, so a run must name every cell the grammar misses rather than stopping at the
        // first — fixing one axis and rediscovering the next on the following run is how a
        // partial grammar gets mistaken for a complete one.
        let missed: Vec<String> = declaration_spellings("DECOY_USAGE")
            .into_iter()
            .filter(|spelling| declared_help_constant(spelling) != Some("DECOY_USAGE"))
            .chain(
                declaration_spellings("DEGRADED_CUE")
                    .into_iter()
                    .filter(|spelling| declared_prose_constant(spelling) != Some("DEGRADED_CUE")),
            )
            .collect();
        assert!(
            missed.is_empty(),
            "the declaration parser misses {} spelling(s) — each one is an operator-facing \
             surface the completeness gate cannot see:\n{}",
            missed.len(),
            missed.join("\n")
        );

        // The two filters PARTITION that grammar (issue #1123): every spelling above lands on
        // exactly one side. A constant falling through BOTH would be a shipped string no tripwire
        // owns — the shape of the defect issue #918 was opened about, one filter down.
        for spelling in declaration_spellings("DECOY_USAGE") {
            assert_eq!(
                declared_prose_constant(&spelling),
                None,
                "{spelling:?} must not also count as operator prose"
            );
        }
        for spelling in declaration_spellings("DEGRADED_CUE") {
            assert_eq!(
                declared_help_constant(&spelling),
                None,
                "{spelling:?} must not also count as a help surface"
            );
        }

        // …and it stays NARROW, or the gate would demand entries for things that are not shipped
        // prose at all: an indented fixture belonging to some test module, a commented-out line,
        // a non-const declaration, and a constant whose type is not string-shaped.
        for ignored in [
            "    const INDENTED_USAGE: &str = \"x\";",
            "    static INDENTED_CUE: &str = \"x\";",
            "// const COMMENTED_USAGE: &str = \"x\";",
            "pub fn not_a_const_USAGE() {}",
            "const NOT_A_STRING: usize = 3;",
            "const NOT_A_STRING_EITHER: &[u8] = b\"x\";",
            "const STILL_NOT_A_STRING: Duration = Duration::from_secs(2);",
        ] {
            assert_eq!(
                declared_help_constant(ignored),
                None,
                "the declaration parser must ignore {ignored:?}"
            );
            assert_eq!(
                declared_prose_constant(ignored),
                None,
                "the prose parser must ignore {ignored:?}"
            );
        }
    }

    /// The completeness tripwire the compiler cannot provide: every `const *_USAGE: &str`
    /// DECLARED in this file — under any visibility — must be reachable from
    /// [`ALL_HELP_TOPICS`]. Reads this file's own source, so a help constant added without being
    /// wired into the topic table is caught by the gate instead of quietly sitting outside it.
    ///
    /// The source is embedded with `include_str!` rather than read at run time, so the check
    /// cannot be skipped by a working directory it did not expect — the same
    /// compile-time-input discipline `crate::render_golden` applies to the committed goldens.
    #[test]
    fn every_help_constant_is_scanned() {
        let mut declared: Vec<&str> = include_str!("cli.rs")
            .lines()
            .filter_map(declared_help_constant)
            .collect();
        declared.sort_unstable();
        let mut scanned: Vec<&str> = help_surfaces().iter().map(|(name, _)| *name).collect();
        scanned.sort_unstable();
        assert_eq!(
            scanned, declared,
            "every `const *_USAGE` in src/cli.rs must be reached from ALL_HELP_TOPICS — an \
             unscanned help surface is exactly what issue #918 was opened about"
        );
        // Cardinality, stated because a gate over an empty or halved subject passes identically:
        // both sides above could agree at zero if the source scan silently matched nothing.
        assert_eq!(
            declared.len(),
            19,
            "expected 19 help constants; the count moved — wire the new verb in, then update this"
        );
    }

    /// Issue #918 AC2: the WHOLE `--help` surface is scanned against the central framing
    /// vocabulary, minus the mechanical-operation verbs a CLI must use to name its own commands
    /// (`crate::framing_vocabulary::HELP_EXEMPT_TOKENS`, which records why each is excused).
    ///
    /// The bite half is not decoration. Issue #918 exists because issue #885's AC4 asserted help
    /// coverage that did not exist, and the reason that survived review is that a "current help
    /// is clean" loop passes IDENTICALLY over a guard which inspects nothing. So each editorial
    /// group is injected into a real surface and asserted caught.
    #[test]
    fn every_help_surface_carries_no_banned_framing_but_the_guard_bites_on_injection() {
        let surfaces = help_surfaces();
        assert_eq!(surfaces.len(), 19, "the scan must cover every help topic");

        // PASSES on the real, shipped help — all of it, not a sample.
        for (name, help) in &surfaces {
            assert_eq!(
                scan_help_banned(help),
                None,
                "{name} must carry no banned framing:\n{help}"
            );
        }

        // BITES: each editorial group injected into a REAL surface is caught. A scanner matching
        // nothing would pass the loop above word for word.
        for (injected, caught) in [
            ("You should re-login.", "should"),
            ("Upgrade your plan.", "upgrade"),
            ("Your usage is critical.", "critical"),
            ("Exhaustion is imminent.", "imminent"),
            ("Running out — top up first.", "top up"),
            ("Running low — need more seats.", "need"),
        ] {
            assert_eq!(
                scan_help_banned(&format!("{ROOT_USAGE}\n{injected}")),
                Some(caught),
                "injecting {injected:?} into ROOT_USAGE must be caught"
            );
        }

        // The exemption is a carve-out, not a hole. The verb table passes…
        assert_eq!(
            scan_help_banned("disable <account>    Park an account: take it out of the rotation"),
            None
        );
        // …while a recommendation built AROUND an excused verb is still caught, on a group the
        // exemption never touched. This is why excusing `disable`/`remove` does not hand help a
        // licence to editorialise.
        assert_eq!(
            scan_help_banned("you should disable <account>"),
            Some("should")
        );
        assert_eq!(
            scan_help_banned("consider whether to disable that account"),
            Some("consider")
        );
    }

    /// Every exemption is LOAD-BEARING: each token excused from the help scan is one the shipped
    /// help measurably spends. Issue #918 rejected a hand-picked "command name" exemption set on
    /// exactly this evidence — `add` is not a command in this CLI, and `remove` in
    /// `SERVICE_USAGE` is not used as one — so the set must keep being measured rather than
    /// inherited.
    ///
    /// Reddening here means a help edit dropped the last use of an excused token: the fix is to
    /// TIGHTEN the exemption set, not to widen this test.
    #[test]
    fn every_help_exemption_is_still_earned_by_the_shipped_help() {
        let surfaces = help_surfaces();
        for exempt in crate::framing_vocabulary::HELP_EXEMPT_TOKENS {
            let earned = surfaces.iter().any(|(_, help)| {
                help.split(|c: char| !c.is_ascii_alphanumeric())
                    .any(|word| word.eq_ignore_ascii_case(exempt))
            });
            assert!(
                earned,
                "{exempt:?} is exempt from the help scan but no help surface uses it any more — \
                 tighten HELP_EXEMPT_TOKENS rather than carry a dead carve-out"
            );
        }
    }

    /// The two help guards' division of labour, ASSERTED rather than described — and since issue
    /// #1134 derived one from the other, what is left to assert has NARROWED to exactly the half
    /// that is still contingent.
    ///
    /// Subsumption itself is no longer a claim: [`expiry_banned_tokens`] IS
    /// [`help_banned_tokens`] plus [`EXPIRY_HELP_EXTRA_TOKENS`], so the repo-wide subset cannot
    /// fail to be contained. What remains open is whether those extras are still EARNED — an
    /// extra the central list has since grown is a dead entry that reads like a live carve-in,
    /// the mirror of the dead exemption `every_help_exemption_is_still_earned_by_the_shipped_help`
    /// guards against. When the central list grows `beat`, this reddens, and the remedy is to
    /// DELETE this guard: it would then contribute nothing the repo-wide scan does not, over
    /// constants that scan already covers.
    ///
    /// A structural claim needs a control or it is decoration, so the containment half is proved
    /// by DIFFERENCE rather than by construction: a central token the hand list omitted must now
    /// be present. Both halves would pass over an empty derivation; neither would over a wrong one.
    #[test]
    fn the_expiry_guard_still_earns_its_extras_against_the_central_list() {
        let repo_wide = help_banned_tokens();
        let mut still_earned: Vec<&str> = EXPIRY_HELP_EXTRA_TOKENS
            .iter()
            .copied()
            .filter(|token| !repo_wide.contains(token))
            .collect();
        still_earned.sort_unstable();
        assert_eq!(
            still_earned,
            ["beat"],
            "the expiry guard's only unique TOKEN should be `beat`, and still absent centrally; \
             if the central list grew it, this guard is now redundant — delete it rather than \
             widen this test"
        );

        // …and the remedy that names — DELETE this guard — is only safe while the repo-wide scan
        // actually reaches every constant below. That is the premise the whole division of
        // labour rests on, so it is asserted here rather than argued in the doc above.
        let scanned_repo_wide: Vec<&str> = help_surfaces().iter().map(|(name, _)| *name).collect();
        for (name, _) in EXPIRY_HELP_SURFACES {
            assert!(
                scanned_repo_wide.contains(name),
                "{name} is scanned by the expiry guard but NOT repo-wide — deleting this guard \
                 once `beat` goes central would drop that surface's coverage entirely"
            );
        }

        // The containment control. `healthy` is central, was NOT in the hand list this derivation
        // replaced, and is the group that list omitted wholesale — so its presence here is
        // evidence the derivation is wired, not merely declared.
        let derived = expiry_banned_tokens();
        assert!(
            derived.contains(&"healthy"),
            "the derived list must carry the central tokens the hand list lacked"
        );
        assert_eq!(
            derived.len(),
            repo_wide.len() + EXPIRY_HELP_EXTRA_TOKENS.len(),
            "the derived list is not exactly the repo-wide subset plus the extras — a duplicate \
             or a smuggled token leaves every containment check below green"
        );
        for token in &repo_wide {
            assert!(
                derived.contains(token),
                "{token:?} is scanned repo-wide on --help but not by the expiry guard, which \
                 derives from that same subset"
            );
        }

        // The phrase list is now the central one unchanged. The `need more` extra it used to add
        // was provably dead — `scan_with` matches every token before any phrase, and text where
        // that phrase matches always contains the word `need`, which the derived list carries. So
        // the sentence is still caught, one word earlier, and now also without the `more`.
        assert_eq!(
            scan_expiry_help("running out — need more seats"),
            Some("need"),
            "`need more` is covered by the single central token `need`"
        );
        assert_eq!(
            scan_expiry_help("running out — we need seats"),
            Some("need"),
            "and now also when the sentence omits `more`, which the phrase could not catch"
        );
    }

    /// Issue #1134's subject, proved rather than argued: the expiry guard reaches the SAME
    /// tokenizer as every other framing scan, so it can no longer disagree with them about what
    /// counts as a word.
    ///
    /// ANSI is the divergence the issue names, and it is a live concern in this file rather than
    /// a hypothetical — `render_cells` wraps table cells in SGR runs. The constants this
    /// guard scans carry none today, which is why it was filed as a drift hazard and not a
    /// defect; the point of the assertion is that the hazard is now closed at the root instead of
    /// resting on those constants staying plain.
    ///
    /// The inline split this replaced is what makes the first assertion discriminating rather
    /// than decorative: it split on every non-alphanumeric char, so `\x1b[31mupgrade` tokenised
    /// as the single word `31mupgrade` and the banned word inside it was invisible.
    #[test]
    fn the_expiry_guard_shares_the_one_tokenizer() {
        // A colour-wrapped banned word tokenises intact — the SGR run is stripped, not split on.
        assert_eq!(
            scan_expiry_help("\x1b[31mupgrade\x1b[0m your plan"),
            Some("upgrade"),
            "an SGR-wrapped word must tokenise intact, as it does for every other framing scan"
        );
        // …and the extra this guard owns goes through that same tokenizer, so the carve-in cannot
        // be the one place the old word-split survived.
        assert_eq!(
            scan_expiry_help("\x1b[33mbeat\x1b[0m the deadline"),
            Some("beat")
        );

        // The other tokenizer properties, asserted here too rather than inherited by argument:
        // case folding, punctuation boundaries, and `bypasses` not tripping `bypass`.
        assert_eq!(scan_expiry_help("you SHOULD re-login."), Some("should"));
        assert_eq!(scan_expiry_help("nothing bypasses it"), None);
        assert_eq!(scan_expiry_help("a heartbeat every 30s"), None);
    }

    // --- the framing guard, over operator advisories and usage prose (issue #1123) ----

    /// The static operator-prose this file renders into `status` — the surfaces issue #1123
    /// brings inside the firewall. Both are authored English an operator reads on the same
    /// subject and in the same voice as the help text issue #918 scanned; the firewall's rationale
    /// never distinguished them, only the guard's scope did.
    const ADVISORY_SURFACES: &[(&str, &str)] = &[
        ("REFRESH_DISABLED_ADVISORY", REFRESH_DISABLED_ADVISORY),
        ("DEGRADED_CUE", DEGRADED_CUE),
    ];

    /// Top-level string constants that are deliberately NOT operator prose, and so are outside
    /// [`ADVISORY_SURFACES`] rather than missing from it. The disposition tripwire below requires
    /// every declared string to appear in one list or the other, so this is the place a non-prose
    /// constant is EXCUSED on the record instead of by omission.
    ///
    /// Each entry is paired with the REASON it is not prose, because an excuse without one is
    /// indistinguishable from an oversight — and [`every_excusal_is_reasoned`] enforces the
    /// pairing mechanically rather than trusting this doc to be kept honest. Issue #1123's merge
    /// review excused a genuinely editorialising string here in three mechanical edits and a
    /// fully green run: the doc REQUIRED a reason, nothing TESTED for one.
    ///
    /// The bar a reason must clear is deliberately narrow, and `EXPIRY_GAP` is the shape of it:
    /// the constant carries no WORDS, so scanning it is not a weaker check but a vacuous one.
    /// "This one reads fine to me" is not that; it is the judgement the guard exists to replace.
    const NOT_OPERATOR_PROSE: &[(&str, &str)] = &[(
        "EXPIRY_GAP",
        "a single em dash — the NOT-OBSERVED sentinel for the `EXPIRY` cell, carrying no words",
    )];

    /// Every excusal names a constant that is WORD-FREE, which is the only reason
    /// [`NOT_OPERATOR_PROSE`] admits: a string with no words cannot editorialise, so scanning it
    /// would be vacuous rather than weaker. A string WITH words that someone excused anyway is a
    /// hole in the guard wearing a rationale, and the rationale is exactly what makes it survive
    /// review — so the property is asserted against the constant's own VALUE rather than argued
    /// in prose beside it.
    ///
    /// This is the discipline every exemption SET already carries (`…_is_still_earned_by_…`),
    /// applied to the one carve-out that had a documented requirement and no test.
    #[test]
    fn every_excusal_is_reasoned() {
        // Resolve each excused NAME to the value it stands for. An excusal naming a constant this
        // list cannot resolve is itself a defect — the disposition gate would still balance while
        // nothing checked what was excused.
        let resolved: &[(&str, &str)] = &[("EXPIRY_GAP", EXPIRY_GAP)];
        for (name, reason) in NOT_OPERATOR_PROSE {
            assert!(
                !reason.trim().is_empty(),
                "{name:?} is excused from the prose scan with no reason recorded"
            );
            let (_, value) = resolved
                .iter()
                .find(|(candidate, _)| candidate == name)
                .unwrap_or_else(|| {
                    panic!("{name:?} is excused but this test cannot resolve its value")
                });
            assert!(
                !carries_words(value),
                "{name:?} is excused from the prose scan but carries WORDS ({value:?}) — a \
                 word-bearing string is operator prose and belongs in ADVISORY_SURFACES, whatever \
                 the recorded reason says"
            );
        }
    }

    /// The rendered `status` AUTH cell, over every shape [`health_cell`] can produce — the
    /// operator prose this file builds from INLINE literals rather than from a named constant.
    ///
    /// [`every_prose_constant_is_dispositioned`] reads DECLARATIONS, so it is structurally blind
    /// to `cell.push_str("claude /login")` — which sits three lines from [`DEGRADED_CUE`]'s own
    /// use and is every bit as much authored English an operator reads. Issue #1123's merge
    /// review shipped exactly that payload as an inline literal past a fully green run. Scanning
    /// the RENDERED cell closes it the way [`usage_error_surfaces`] closes the same seam for
    /// `Error::CliUsage`: drive the real function over its whole input space and scan what an
    /// operator would actually see, rather than trying to parse string literals out of a function
    /// body.
    ///
    /// The scope limit, and it is now ENFORCED rather than merely stated (issue #1138): this
    /// scans the inline prose of `health_cell` and `legacy_health_tags`, and nothing else in the
    /// file. What used to make that dangerous was that a NEW function rendering operator prose
    /// from inline literals owed this list a line with no tripwire asking for it — the obligation
    /// [`ADVISORY_SURFACES`] carries for a new constant, minus the enforcement.
    /// [`INLINE_PROSE_REGISTER`] closes that: no declaration-reading tripwire can enumerate
    /// inline literals, but a LEXER can, and
    /// [`every_function_spelling_inline_prose_is_dispositioned`] reddens until the new function
    /// is dispositioned.
    ///
    /// What that register does NOT do is scan those surfaces. Measured while mechanising this, the
    /// functions in its `Unscanned` arm render operator text from inline literals that no framing
    /// scan reaches — so the two `Scanned` entries are this list, and the rest is enumerated debt.
    /// The arm's own cardinality pin is the number, deliberately in one place. Issue #1167 carries
    /// widening the firewall over them; widening it is a scoping decision of the kind issues #1123
    /// and #1139 each made deliberately for one audience — but by different means, which is the
    /// point of citing both (see the register's own doc).
    fn rendered_advisory_surfaces() -> Vec<(String, String)> {
        use CredentialHealth::{AtRisk, Dead, Degraded, Healthy, Stale, Unknown};
        let mut out = Vec::new();
        // The full product: every rollup verdict × healing × parked, plus the `health: None`
        // legacy fallback. Exhaustive rather than sampled — a cue reachable only in one corner of
        // that space is still a cue an operator reads.
        for health in [
            Some(Healthy),
            Some(Unknown),
            Some(Stale),
            Some(AtRisk),
            Some(Degraded),
            Some(Dead),
            None,
        ] {
            for recovering in [false, true] {
                for enabled in [false, true] {
                    let cell = health_cell(&AccountStatusLine {
                        health,
                        quarantined: true,
                        recovering,
                        enabled,
                        ..status_line("work", false, Some(10), Some(20))
                    });
                    out.push((
                        format!(
                            "health_cell({health:?}, recovering={recovering}, enabled={enabled})"
                        ),
                        cell,
                    ));
                }
            }
        }
        out
    }

    // --- the inline-literal completeness tripwire (issue #1138) -------------------------------

    /// Whether `value` carries a WORD — the one notion of a word the prose guards share, so no two
    /// can silently disagree about what they are looking at (the reason
    /// `crate::framing_vocabulary` hoisted its tokenizer for the scanners).
    ///
    /// Deliberately the LOOSEST reading, a single ASCII alphanumeric: `n/a` and `32` are as much
    /// a shipped string as a sentence is. A value it wrongly admits costs one register line; one
    /// it wrongly drops is a surface no tripwire owns, which is the failure class these gates
    /// exist to end — the same asymmetry [`is_str_shaped`] resolves the same way.
    fn carries_words(value: &str) -> bool {
        !value
            .split(|c: char| !c.is_ascii_alphanumeric())
            .all(str::is_empty)
    }

    /// A raw string literal beginning at `at`, and the index just past it — `None` if `at` does
    /// not begin one. Every spelling Rust accepts: the optional `b` / `c` prefix, and any hash
    /// count. Enumerated as a GRAMMAR rather than as the single spelling this file happens to use
    /// (`request_shutdown`'s `r#""ok":true"#`), for the reason
    /// [`the_declaration_parser_recognises_every_declaration_spelling`] records one level down: a
    /// scanner you evade by spelling a literal differently is not a scanner.
    ///
    /// A raw string honours no escapes, so only a `"` trailed by the same hash count closes it.
    fn raw_string_at(src: &[char], at: usize) -> Option<(String, usize)> {
        if at > 0 && (src[at - 1].is_ascii_alphanumeric() || src[at - 1] == '_') {
            return None; // an `r` inside an identifier (`for`, `char`) opens nothing
        }
        let mut i = at;
        if matches!(src.get(i), Some('b' | 'c')) {
            i += 1;
        }
        if src.get(i) != Some(&'r') {
            return None;
        }
        i += 1;
        let hashes = src[i..].iter().take_while(|c| **c == '#').count();
        i += hashes;
        if src.get(i) != Some(&'"') {
            return None;
        }
        i += 1;
        let body = i;
        while i < src.len() {
            if src[i] == '"' && (1..=hashes).all(|k| src.get(i + k) == Some(&'#')) {
                return Some((src[body..i].iter().collect(), i + 1 + hashes));
            }
            i += 1;
        }
        None
    }

    /// An ordinary or byte string literal beginning at `at`, and the index just past it — `None`
    /// if `at` does not begin one. A backslash escapes whatever follows it, which is what carries
    /// the scan across the `\`-newline continuations several shipped messages here are built from
    /// (`daemon_stop`'s four, `render_canary`'s four) and across an escaped `\"`.
    ///
    /// The value is returned with its escapes INTACT — this is a completeness scan over what the
    /// source spells, not an evaluator, and un-escaping would only add a way to be wrong.
    fn quoted_string_at(src: &[char], at: usize) -> Option<(String, usize)> {
        let mut i = at;
        if matches!(src.get(i), Some('b' | 'c')) {
            i += 1;
        }
        if src.get(i) != Some(&'"') {
            return None;
        }
        i += 1;
        let mut value = String::new();
        while i < src.len() {
            match src[i] {
                '\\' => {
                    value.push('\\');
                    if let Some(escaped) = src.get(i + 1) {
                        value.push(*escaped);
                    }
                    i += 2;
                }
                '"' => return Some((value, i + 1)),
                other => {
                    value.push(other);
                    i += 1;
                }
            }
        }
        None
    }

    /// The length of the char literal beginning at `at`, or `None` for a LIFETIME (`'a`,
    /// `'static`), which must be left alone. Told apart by the closing quote rather than by a
    /// keyword list.
    ///
    /// Load-bearing beyond tidiness: a `'"'` would otherwise open a string and swallow the rest of
    /// the file as one literal. This file spells `' '`, `'*'`, `'\n'`, `'f'`, `'h'`, `'v'`, `'V'`
    /// and `&'static str` in its non-test code, so both arms are exercised by the real subject.
    fn char_literal_len(src: &[char], at: usize) -> Option<usize> {
        if src.get(at) != Some(&'\'') {
            return None;
        }
        if src.get(at + 1) == Some(&'\\') {
            // An escape, so the closing quote is never the character at `at + 2` — start past it,
            // or `'\''` would measure itself as `'\'`. Bounded so an unclosed lifetime cannot run
            // the search to the end of the file.
            let limit = src.len().min(at + 12);
            return (at + 3..limit)
                .find(|k| src[*k] == '\'')
                .map(|k| k + 1 - at);
        }
        (src.get(at + 2) == Some(&'\'')).then_some(3)
    }

    /// Every string literal `source`'s non-test code spells INSIDE a function body, paired with
    /// the function that spells it — the population [`every_prose_constant_is_dispositioned`] is
    /// structurally blind to, because an inline literal has no declaration to key off.
    ///
    /// Takes the source as an argument rather than reaching for `include_str!` so the canary can
    /// drive THIS function over a deliberately broken subject: a gate whose predicate cannot be
    /// pointed at a known-bad input can only be verified by inspection, which ADR-0031 § 4
    /// CONSTRAINT-A rejects.
    ///
    /// A real lexer rather than a line filter, and this file's own contents earn the cost three
    /// times over: `///` doc comments are its densest source of quotes and apostrophes, several
    /// shipped messages are single literals spanning a dozen lines through `\` continuations, and
    /// `request_shutdown` spells a quote-bearing raw string. A scanner that mis-lexes any one of
    /// those desynchronizes and reports a population unrelated to the file — while still looking
    /// like it read something.
    ///
    /// TOP-LEVEL literals are deliberately excluded: those are declarations, and
    /// [`every_prose_constant_is_dispositioned`] already owns every one of them. Two tripwires
    /// sharing a subject is how each ends up assuming the other covered it.
    fn inline_literals(source: &str) -> Vec<(String, String)> {
        // Non-test code only, at this file's own column-0 `#[cfg(test)]` — the same structural
        // boundary [`every_cli_usage_construction_site_is_scanned`] draws, for the same reason:
        // the test module spells operator-shaped strings freely in its own assertions, and none of
        // them ship.
        let src: Vec<char> = source
            .lines()
            .take_while(|line| !line.starts_with("#[cfg(test)]"))
            .collect::<Vec<_>>()
            .join("\n")
            .chars()
            .collect();

        let mut out = Vec::new();
        // Each open function body, with the brace depth it opened at.
        let mut scopes: Vec<(String, usize)> = Vec::new();
        let mut pending: Option<String> = None;
        let mut depth = 0usize;
        let mut i = 0usize;

        while i < src.len() {
            // Comments FIRST. A doc comment here is prose ABOUT strings — quotes, apostrophes and
            // `//` alike — so lexing one as code is the single likeliest way to lose the place.
            if src[i] == '/' && src.get(i + 1) == Some(&'/') {
                while i < src.len() && src[i] != '\n' {
                    i += 1;
                }
                continue;
            }
            if src[i] == '/' && src.get(i + 1) == Some(&'*') {
                let mut nesting = 1usize;
                i += 2;
                while i < src.len() && nesting > 0 {
                    if src[i] == '/' && src.get(i + 1) == Some(&'*') {
                        nesting += 1;
                        i += 2;
                    } else if src[i] == '*' && src.get(i + 1) == Some(&'/') {
                        nesting -= 1;
                        i += 2;
                    } else {
                        i += 1;
                    }
                }
                continue;
            }
            if let Some((value, next)) = raw_string_at(&src, i) {
                push_literal(&mut out, &scopes, depth, value);
                i = next;
                continue;
            }
            if let Some((value, next)) = quoted_string_at(&src, i) {
                push_literal(&mut out, &scopes, depth, value);
                i = next;
                continue;
            }
            if src[i] == '\'' {
                i += char_literal_len(&src, i).unwrap_or(1);
                continue;
            }
            // `fn NAME`, remembered until its opening brace — so a signature broken across lines,
            // or carrying a `where` clause, still binds the body that follows it.
            if src[i] == 'f'
                && src.get(i + 1) == Some(&'n')
                && (i == 0 || !(src[i - 1].is_ascii_alphanumeric() || src[i - 1] == '_'))
                && src.get(i + 2).is_some_and(|c| c.is_whitespace())
            {
                let mut j = i + 2;
                while src.get(j).is_some_and(|c| c.is_whitespace()) {
                    j += 1;
                }
                let start = j;
                while src
                    .get(j)
                    .is_some_and(|c| c.is_ascii_alphanumeric() || *c == '_')
                {
                    j += 1;
                }
                if j > start {
                    pending = Some(src[start..j].iter().collect());
                    i = j;
                    continue;
                }
            }
            match src[i] {
                '{' => {
                    depth += 1;
                    if let Some(name) = pending.take() {
                        scopes.push((name, depth));
                    }
                }
                '}' => {
                    if scopes.last().is_some_and(|(_, opened)| *opened == depth) {
                        scopes.pop();
                    }
                    depth = depth.saturating_sub(1);
                }
                // A bodiless signature (a trait method, an `extern` declaration) must not leave a
                // name waiting to capture the next unrelated block.
                ';' => pending = None,
                _ => {}
            }
            i += 1;
        }
        out
    }

    /// Record one literal against the function whose body spells it.
    ///
    /// The two literals that belong to NO function are told apart by brace depth, and the
    /// distinction is the seam between the two tripwires rather than a detail. At depth 0 a
    /// literal is a top-level declaration, which [`every_prose_constant_is_dispositioned`] owns,
    /// so it is dropped here. NESTED but outside any function — an associated `const` in an
    /// `impl`, an item in an inner `mod` — is owned by NEITHER: that gate's parser is anchored at
    /// column 0 on purpose, because it reads the whole file and would otherwise collect the test
    /// module's own indented fixtures. So a nested declaration lands here, under
    /// [`ASSOCIATED_ITEM`], rather than in nobody's subject.
    ///
    /// This file's non-test code spells no such string today (its four nested consts are a
    /// `&'static [Self]` and three `i64`s), so the arm is empty and reddens if that changes.
    fn push_literal(
        out: &mut Vec<(String, String)>,
        scopes: &[(String, usize)],
        depth: usize,
        value: String,
    ) {
        match scopes.last() {
            Some((name, _)) => out.push((name.clone(), value)),
            None if depth > 0 => out.push((ASSOCIATED_ITEM.to_owned(), value)),
            None => {}
        }
    }

    /// The bucket [`push_literal`] files a nested-but-function-less declaration under. Spelled as
    /// a non-identifier so it can never collide with a real function name.
    const ASSOCIATED_ITEM: &str = "<associated item>";

    /// What a function's inline literals are, for the framing firewall's purposes.
    ///
    /// Every arm is a statement about REACHABILITY, never about editorial quality — deliberately.
    /// "Is this string prose?" is the judgement the whole firewall exists to replace, so a register
    /// built on it would record opinions and defend them. "Does a scan reach it?" and "does an
    /// operator ever see it?" are questions about the code.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum InlineProse {
        /// A framing scan reaches this function's inline prose; the reason names which one.
        Scanned,
        /// Its literals never reach an operator: argv tokens matched against input, cross-surface
        /// wire identifiers, control frames, environment keys, ANSI codes, and interpolation-only
        /// skeletons whose only words are the field names being substituted.
        NotRendered,
        /// It renders operator-facing text from inline literals and NO framing scan reaches it.
        /// The debt — counted, so it cannot grow quietly.
        Unscanned,
    }

    use InlineProse::{NotRendered, Scanned, Unscanned};

    /// Every function in this file's non-test code that spells a word-bearing string literal in
    /// its body, with what that literal population IS and why.
    ///
    /// This is the inline-literal counterpart to the pairing [`ADVISORY_SURFACES`] /
    /// [`NOT_OPERATOR_PROSE`] carry for declared constants, and it exists because the granularity
    /// question turned out to be the whole of issue #1138. Measured against this file rather than
    /// predicted:
    ///
    /// - Per LITERAL is unusable. Most of this file's inline literals are argv tokens matched
    ///   against input — the `parse_*` entries below hold the bulk of them — and the rest of the
    ///   noise is wire identifiers, ANSI codes and interpolation skeletons. Worse than the ratio
    ///   is the CHURN: a new flag on `parse_log` would owe the gate a line, which is how a gate
    ///   teaches its readers to bump past it.
    /// - Narrowing by EMITTING POSITION (a literal inside `push_str(` / `format!(`) is refuted by
    ///   shipped code: `render_roster` binds `" · disabled"` through a `let` before interpolating
    ///   it, so that filter misses operator prose already in the tree, and a one-line hoist would
    ///   defeat it anywhere else. A completeness tripwire with false negatives reads as covering
    ///   a surface it does not reach — precisely the defect issue #918 was opened about.
    /// - Narrowing by SIGNATURE (`-> String`, the "rendering functions" issue #1138 proposed) is
    ///   tempting and blind: `refresh_tag` returns `Option<String>` and renders ` — claude
    ///   /login`, the exact cue the issue was opened about.
    ///
    /// So the subject is every function body holding a word-bearing literal, and the granularity
    /// is the FUNCTION. That is what makes the 27 no-prose entries a one-time cost rather than a
    /// running tax: the register is consulted again only when a function is ADDED, which is the
    /// event the obligation is about.
    ///
    /// **What this gate does NOT do.** It disposes; it does not scan. Every `Unscanned` entry
    /// below says a surface is unscanned and the run stays green, because extending the #160
    /// firewall over those surfaces is a scoping decision of the kind issues #1123 and #1139 each
    /// made deliberately for ONE audience — and by DIFFERENT means, which is why citing them as a
    /// single precedent would be wrong: #1123 amended the ADR and earned a measured exemption set,
    /// while #1139 deliberately did neither, scanning its central lists whole with per-(variant,
    /// token) carve-outs instead (`docs/adr/0020-stats-framing-guard-permits-neutral-runway.md`
    /// § "Issue #1139 applied this ADR without amending it — deliberately" records why). What the
    /// two share is that each was a deliberate scoping act, not something a coverage ticket may
    /// settle in passing. Issue #1167 carries it. What changes
    /// today is that the hole is enumerated and its size is pinned, instead of being described in
    /// one function's doc comment as a residual.
    ///
    /// Keyed on the function NAME, so two same-named functions in different `impl` blocks share
    /// one entry. The total-literal pin in [`every_function_spelling_inline_prose_is_dispositioned`]
    /// is what closes that: a second `new` carrying a literal moves the count even though it moves
    /// no name.
    const INLINE_PROSE_REGISTER: &[(&str, InlineProse, &str)] = &[
        // --- reached by a framing scan ---------------------------------------------------------
        ("health_cell", Scanned, "`rendered_advisory_surfaces` drives it over its whole input space"),
        ("legacy_health_tags", Scanned, "same driver, through `health_cell`'s `health: None` fallback"),
        ("hint", Scanned, "every `usage_hint`, scanned through ALL_HELP_TOPICS"),
        ("from", Scanned, "the lexopt fold's usage hint; the `status --json=1` case renders it"),
        ("unexpected", Scanned, "three CliUsage messages the `status --zzz` / `-z` / `log zzz` cases render"),
        ("required_value", Scanned, "the CliUsage message the `stats --period` case renders"),
        ("parse_config", Scanned, "argv tokens, plus the two CliUsage messages `config zzz` / `config path --origin` render"),
        ("backup_index", Scanned, "the two CliUsage messages `config restore` / `config restore 0` render"),
        ("parse_daemon", Scanned, "argv tokens, plus the CliUsage message `daemon zzz` renders"),
        ("parse_service", Scanned, "argv tokens, plus the CliUsage message `service zzz` renders"),
        ("parse_use", Scanned, "argv tokens, plus the CliUsage message `use zzz --next` renders"),
        // --- never reaches an operator ---------------------------------------------------------
        ("parse", NotRendered, "argv: the two root flags"),
        ("parse_export", NotRendered, "argv: `export`'s flags"),
        ("parse_import", NotRendered, "argv: `import`'s flags"),
        ("parse_list", NotRendered, "argv: `list`'s only flag"),
        ("parse_log", NotRendered, "argv: `log`'s flags"),
        ("parse_positional", NotRendered, "argv: the shared `--help` probe"),
        ("parse_reliability", NotRendered, "argv: `reliability`'s flags"),
        ("parse_run", NotRendered, "argv: `run`'s flags"),
        ("parse_stats", NotRendered, "argv: `stats`' flags"),
        ("parse_status", NotRendered, "argv: `status`'s flags"),
        ("parse_subcommand", NotRendered, "argv: the eighteen verb names, matched against input"),
        ("cross_surface_id", NotRendered, "the eight cross-surface fault identifiers (issue #768) — a machine contract"),
        ("query_status", NotRendered, "the `{\"cmd\":\"status\"}` control frame"),
        ("should_colorize", NotRendered, "the three colour environment keys"),
        ("color_decision", NotRendered, "the `TERM` value that means no colour, plus its `0` sentinel"),
        ("sgr", NotRendered, "ANSI SGR parameter numbers"),
        ("severity_line", NotRendered, "the SGR wrapper skeleton"),
        ("render_cells", NotRendered, "the per-cell SGR wrapper and the line-join skeleton"),
        ("daemon_fault_line", NotRendered, "the plain-branch line-join skeleton"),
        ("pad_end", NotRendered, "the display-width pad skeleton"),
        ("expiry_table_cell", NotRendered, "the horizon-bracket skeleton around `expiry_cell`"),
        ("new", NotRendered, "`StatusRow`'s active-marker skeleton"),
        ("socket_shutdown", NotRendered, "interpolates its two callers' words; `daemon_stop` authors them"),
        ("status", NotRendered, "interpolates the serialized `--json` snapshot"),
        // --- operator-facing, and no framing scan reaches it (issue #1167) ---------------------
        ("access_token_expiry_cell", Unscanned, "the `-v` access-token clock's three states"),
        ("expiry_cell", Unscanned, "the EXPIRY column's `lapsed` state"),
        ("expiry_tag", Unscanned, "`list`'s expiry tag"),
        ("refresh_tag", Unscanned, "`list`'s refresh tag — and its ` — claude /login` is the cue issue #1138 named"),
        ("render_access_token_expiry", Unscanned, "the `-v` block's heading and row skeleton"),
        ("humanize_until", Unscanned, "the compact time-until words (`now`, `<1m`)"),
        ("reset_cell", Unscanned, "the RESET column's `n/a` gap"),
        ("pct", Unscanned, "the percent column's `n/a` gap — the same operator-facing absence as `reset_cell`"),
        ("status_columns", Unscanned, "the seven `status` column headers"),
        ("render_roster", Unscanned, "`list`'s parked marker and account-count noun"),
        ("word", Unscanned, "`import`'s four outcome nouns"),
        ("import_report", Unscanned, "the import tally sentence"),
        ("duplicate_label_notice", Unscanned, "the post-import ambiguous-label notice"),
        ("non_adoption_notice", Unscanned, "the post-import active-account notice"),
        ("flip_confirmation", Unscanned, "`enable` / `disable`'s confirmations"),
        ("set_enabled", Unscanned, "`enable` / `disable` — the verb `RotationLabelRequired` prints back at the operator"),
        ("remove_account", Unscanned, "`remove` — the verb `RotationLabelRequired` prints back at the operator"),
        ("remove_confirmation", Unscanned, "`remove`'s confirmation"),
        ("export", Unscanned, "the export passphrase prompt"),
        ("import", Unscanned, "the import passphrase prompt"),
        ("run", Unscanned, "the daemon's start and stand-down lines"),
        ("daemon_stop", Unscanned, "`daemon stop`'s four outcome messages"),
        ("render_daemon_status", Unscanned, "`daemon status`'s three verdicts"),
        ("management_suffix", Unscanned, "the managed / unmanaged suffix both daemon verbs append"),
        ("request_shutdown", Unscanned, "two control frames, plus two failure sentences carried inside an `Error::Io` — the construction-site residual issue #1152 records"),
        ("render_snapshot_age", Unscanned, "the snapshot-age line"),
        ("render_next_swap", Unscanned, "the next-swap footer and its four reasons"),
        ("out_of_capacity_phrase", Unscanned, "the blocked-fleet phrase both footers share"),
        ("render_cornered", Unscanned, "the CORNERED alarm"),
        ("render_blind_active", Unscanned, "the blind-active line"),
        ("render_blind_preempt_swap", Unscanned, "the preemptive-swap notice"),
        ("render_keychain_locked", Unscanned, "the locked-keychain fault line"),
        ("render_canonical_scrub", Unscanned, "the scrubbed-login fault lines"),
        ("render_canary", Unscanned, "the four keychain-canary fault lines"),
        ("render_systemic_refresh_failure", Unscanned, "the refresh-mechanism-down fault lines"),
        ("render_landing_overshoot", Unscanned, "the landing-overshoot line"),
        ("render_expiry_cohort", Unscanned, "the expiry-cohort line"),
        ("render_schema_mismatch", Unscanned, "the schema-mismatch refusal"),
        ("render_peak_runway_advisory", Unscanned, "the peak-runway tuning advisory"),
        ("render_config_validate", Unscanned, "`config validate`'s verdict"),
        ("render_config_origin", Unscanned, "`config show`'s heading, provenance tags and section lines"),
        ("render_config_backups", Unscanned, "`config backups`' ring heading and per-entry account-count noun"),
        ("render_restore_notice", Unscanned, "`config restore`'s before-and-after notice"),
        ("version_line", Unscanned, "the version banner's program name and `env!` key"),
    ];

    /// The completeness tripwire for inline-literal prose (issue #1138) — the counterpart to
    /// [`every_prose_constant_is_dispositioned`] on the axis that one is structurally blind to.
    ///
    /// That gate reads DECLARATIONS, so `cell.push_str("claude /login")` is invisible to it
    /// whatever grammar [`declared_str_constant`] accepts. Issue #1123 closed the KNOWN instance
    /// by rendering ([`rendered_advisory_surfaces`]) and recorded the general case as a residual
    /// in that function's doc; a limit stated in one function's doc comment is reachable only by
    /// whoever is already reading that function, which is not the person adding a new one.
    ///
    /// So the obligation is mechanical now: a function that spells a word-bearing literal must
    /// appear in [`INLINE_PROSE_REGISTER`], or this reddens. It disposes rather than scans — see
    /// that register for what the three arms mean and for the measurement that chose this
    /// granularity over the two narrower ones.
    #[test]
    fn every_function_spelling_inline_prose_is_dispositioned() {
        let literals: Vec<(String, String)> = inline_literals(include_str!("cli.rs"))
            .into_iter()
            .filter(|(_, value)| carries_words(value))
            .collect();

        let mut spelled: Vec<&str> = literals.iter().map(|(name, _)| name.as_str()).collect();
        spelled.sort_unstable();
        spelled.dedup();
        let mut registered: Vec<&str> = INLINE_PROSE_REGISTER
            .iter()
            .map(|(name, _, _)| *name)
            .collect();
        registered.sort_unstable();

        assert_eq!(
            spelled, registered,
            "every function in src/cli.rs's non-test code that spells a word-bearing string \
             literal must be dispositioned in INLINE_PROSE_REGISTER — an undispositioned one is \
             operator-facing text that no tripwire owns, which is what issue #918 was opened about"
        );
        // Cardinality on BOTH populations, because neither implies the other. The names would
        // agree at zero if the lexer silently matched nothing; and the literal total is what
        // catches a second same-named function, since the register keys on the name alone.
        assert_eq!(
            spelled.len(),
            79,
            "expected 79 functions spelling an inline word-bearing literal; the count moved — \
             disposition the new one, then update this"
        );
        assert_eq!(
            literals.len(),
            288,
            "expected 288 inline word-bearing literals; the count moved — check whether the \
             function that gained one is still dispositioned correctly, then update this"
        );

        // Per-arm cardinality, so the DEBT is pinned rather than merely listed: a surface moved
        // into `Unscanned` without anyone deciding to would otherwise land silently among 44
        // entries that already say so. Shrinking this number is the progress issue #1167 tracks.
        for (arm, expected) in [(Scanned, 11), (NotRendered, 24), (Unscanned, 44)] {
            let actual = INLINE_PROSE_REGISTER
                .iter()
                .filter(|(_, disposition, _)| *disposition == arm)
                .count();
            assert_eq!(
                actual, expected,
                "expected {expected} {arm:?} entries in INLINE_PROSE_REGISTER, found {actual}"
            );
        }
        // The `every_excusal_is_reasoned` discipline, applied to this register: an entry without a
        // reason is indistinguishable from an oversight, and the reason is what a reader adding
        // the next function copies the shape of.
        for (name, disposition, reason) in INLINE_PROSE_REGISTER {
            assert!(
                !reason.trim().is_empty(),
                "{name:?} is dispositioned {disposition:?} with no reason recorded"
            );
        }
    }

    /// The names [`every_function_spelling_inline_prose_is_dispositioned`] compares against the
    /// register, extracted so the canary below can drive the IDENTICAL predicate over a
    /// deliberately broken subject rather than over a paraphrase of it (ADR-0031 § 4
    /// CONSTRAINT-A: a canary must run through the same predicate the real assertion uses).
    fn functions_spelling_inline_prose(source: &str) -> Vec<String> {
        let mut names: Vec<String> = inline_literals(source)
            .into_iter()
            .filter(|(_, value)| carries_words(value))
            .map(|(name, _)| name)
            .collect();
        names.sort_unstable();
        names.dedup();
        names
    }

    /// CONSTRAINT-A for the tripwire above (ADR-0031 § 4): the gate is observed to REDDEN on a
    /// subject carrying the defect, not merely read and believed. Both cases are driven through
    /// [`functions_spelling_inline_prose`], the predicate the real assertion compares with.
    ///
    /// The payload is the one issue #1123's merge review actually shipped past a fully green run,
    /// which is why this gate exists rather than a hypothetical.
    ///
    /// The second case is the one that settles the SUBJECT rather than the gate. Hoisting the
    /// literal into a `let` before interpolating it is an ordinary refactor, and it is what a
    /// scanner keyed on emitting position (`push_str(` / `format!(`) would lose — which is not
    /// hypothetical either: `render_roster` already spells `" · disabled"` that way today. Both
    /// shapes must land, or the tripwire is one `let` away from blind.
    #[test]
    fn the_inline_prose_tripwire_bites_on_a_new_rendering_function() {
        for (shape, body) in [
            (
                "pushed directly",
                "    cell.push_str(\"your credential is critical, you should upgrade soon\");",
            ),
            (
                "hoisted through a `let`",
                "    let cue = \"your credential is critical, you should upgrade soon\";\n    \
                 cell.push_str(cue);",
            ),
        ] {
            let injected = format!(
                "const SOMETHING: &str = \"top level, owned by the other tripwire\";\n\
                 fn a_new_operator_surface() -> String {{\n\
                 {body}\n\
                 }}\n\
                 #[cfg(test)]\n\
                 mod tests {{\n\
                 fn a_test_helper() {{ let _ = \"test prose never ships\"; }}\n\
                 }}\n"
            );
            let spelled = functions_spelling_inline_prose(&injected);
            assert_eq!(
                spelled,
                ["a_new_operator_surface"],
                "the scan must see inline prose {shape}, and must see ONLY that: a top-level \
                 declaration belongs to `every_prose_constant_is_dispositioned`, and the test \
                 module's own strings do not ship"
            );
            // …and that IS a red run, because the register cannot hold a name it has never seen.
            // Stated as the real comparison rather than left as an inference about it.
            let registered: Vec<&str> = INLINE_PROSE_REGISTER
                .iter()
                .map(|(name, _, _)| *name)
                .collect();
            assert!(
                !registered.contains(&"a_new_operator_surface"),
                "the canary's function must be absent from the register, or it proves nothing"
            );
        }
    }

    /// The lexer is the one textual link in this completeness chain, so it is tested directly
    /// rather than only through its caller — the discipline
    /// [`the_declaration_parser_recognises_every_declaration_spelling`] applies to the other one,
    /// and for a sharper reason here: a line filter over this file does not merely miss things, it
    /// DESYNCHRONIZES. Mis-lex one doc comment and every literal after it is attributed to the
    /// wrong function, or swallowed into one enormous string — and the run still reports a
    /// population, which is what makes the failure survive review.
    ///
    /// Each case below is a construct this file's own non-test code actually spells.
    #[test]
    fn the_inline_literal_lexer_reads_rust_rather_than_grepping_for_quotes() {
        let source = "\
/// A doc comment that says \"quoted\" and isn't shy about apostrophes — nor about // slashes.
fn commented() {
    // A line comment mentioning \"a decoy\" and a lone ' apostrophe.
    let _ = \"kept\";
}
/* a block /* nested */ comment holding \"another decoy\" */
fn escaped() {
    let _ = \"an escaped \\\" quote, then a \\
        continuation\";
}
fn raw() {
    let _ = r#\"\"ok\":true\"#;
}
fn chars() {
    let _ = '\"';
    let _ = '\\'';
    let _: &'static str = \"after a lifetime\";
    let _ = \"tail\";
}
fn closures() {
    let _ = [1].iter().map(|n| { if *n > 0 { \"inner\" } else { \"other\" } });
}
fn bodiless_neighbour();
fn after_the_semicolon() {
    let _ = \"bound to the right function\";
}
 const TOP_LEVEL: &str = \"a declaration, owned by the other tripwire\";
impl Nested {
    const INDENTED: &'static str = \"nested, owned by NEITHER unless this catches it\";
}
";
        // That leading space is load-bearing, and not for this test. This fixture's lines are
        // lines of THIS FILE too, and `every_prose_constant_is_dispositioned` reads the whole
        // file for `const` at column 0 — so an unindented fixture declaration is picked up as a
        // real one and reddens that gate instead of this one. (Observed, not feared.) The space
        // is invisible here: this lexer keys on brace depth, never on indentation.
        let lexed = inline_literals(source);
        let found: Vec<(&str, &str)> = lexed
            .iter()
            .map(|(name, value)| (name.as_str(), value.as_str()))
            .collect();
        assert_eq!(
            found,
            [
                ("commented", "kept"),
                (
                    "escaped",
                    "an escaped \\\" quote, then a \\\n        continuation"
                ),
                ("raw", "\"ok\":true"),
                ("chars", "after a lifetime"),
                ("chars", "tail"),
                ("closures", "inner"),
                ("closures", "other"),
                ("after_the_semicolon", "bound to the right function"),
                (
                    ASSOCIATED_ITEM,
                    "nested, owned by NEITHER unless this catches it"
                ),
            ],
            "the lexer must skip comments of both kinds, carry escapes and `\\`-continuations, \
             read a raw string's quote-bearing body, leave char literals and lifetimes alone, \
             attribute a closure's literals to the enclosing function, not let a bodiless \
             signature capture the next block, drop a column-0 declaration to the tripwire that \
             owns it, and catch a NESTED one that neither tripwire's parser would otherwise reach"
        );
    }

    /// The completeness tripwire for the advisory guard — the counterpart to
    /// [`every_help_constant_is_scanned`], and the reason a new advisory cannot ship unscanned.
    ///
    /// Issue #918's lesson was that a guard nobody can extend by accident is a guard that silently
    /// stops covering the surface. Help got that tripwire; the advisories had none, so a second
    /// `const SOMETHING_ADVISORY` added next year would sit outside every scan while `cargo test`
    /// stayed green. This closes that by DISPOSITION rather than by naming convention: every
    /// top-level string this file DECLARES must be either scanned as prose or listed as
    /// [`NOT_OPERATOR_PROSE`]. Matching on a name suffix (`*_ADVISORY`, `*_CUE`) would have been
    /// the lexical guess issue #918 explicitly rejected — the next advisory may be called
    /// anything, and a gate you can evade by naming a constant differently is not a gate.
    ///
    /// Its reach is DECLARATIONS, and the boundary is worth stating because it was mistaken once:
    /// prose built from INLINE literals inside a function body has no declaration to key off and
    /// is invisible here, whatever the grammar in [`declared_str_constant`] accepts. That other
    /// half has its own tripwire now —
    /// [`every_function_spelling_inline_prose_is_dispositioned`], over
    /// [`INLINE_PROSE_REGISTER`] (issue #1138) — and the pointer belongs HERE, at the gate a
    /// reader actually meets when they add a shipped string, rather than only in the doc of the
    /// function that happens to implement the other half.
    ///
    /// The two partition the file's shipped strings between them: this one owns what is DECLARED,
    /// that one owns what a function body SPELLS. A string falling through both would be the
    /// defect issue #918 was opened about, one axis over. State the seam precisely, because the
    /// closure is not self-evident: this gate's parser is anchored at column 0 and reads the whole
    /// file, so an item placed AFTER the test module would be invisible to it — what forecloses
    /// that is clippy's `items_after_test_module`, which the required `-D warnings` turns into a
    /// build failure. The partition therefore holds, but it rests on three things rather than two,
    /// and a change that relaxes that lint reopens the gap.
    ///
    /// Reads this file's own source via `include_str!`, so the check cannot be skipped by an
    /// unexpected working directory — the same compile-time-input discipline the help tripwire and
    /// `crate::render_golden` use.
    #[test]
    fn every_prose_constant_is_dispositioned() {
        let mut declared: Vec<&str> = include_str!("cli.rs")
            .lines()
            .filter_map(declared_prose_constant)
            .collect();
        declared.sort_unstable();

        let mut dispositioned: Vec<&str> = ADVISORY_SURFACES
            .iter()
            .map(|(name, _)| *name)
            .chain(NOT_OPERATOR_PROSE.iter().map(|(name, _)| *name))
            .collect();
        dispositioned.sort_unstable();

        assert_eq!(
            declared, dispositioned,
            "every top-level string constant in src/cli.rs must be either scanned as operator \
             prose (ADVISORY_SURFACES) or excused on the record (NOT_OPERATOR_PROSE) — an \
             undispositioned one is an operator-facing string no framing guard reaches"
        );
        // Cardinality, stated because a gate over an empty subject passes identically: both sides
        // above would agree at zero if the source scan silently matched nothing.
        assert_eq!(
            declared.len(),
            3,
            "expected 3 top-level `&str` constants; the count moved — disposition the new one, \
             then update this"
        );
    }

    /// Issue #1123 AC-2 for the advisory surface: the shipped operator advisories carry no banned
    /// framing, and the guard that says so can fail.
    ///
    /// The bite half is the whole point, for the reason issue #918 recorded: a "current prose is
    /// clean" loop passes IDENTICALLY over a scanner that inspects nothing, and that is precisely
    /// how issue #885's claimed help coverage survived review. So each editorial group is injected
    /// into a REAL advisory and asserted caught.
    #[test]
    fn the_operator_advisories_carry_no_banned_framing_but_the_guard_bites_on_injection() {
        // PASSES on the real, shipped advisories.
        for (name, prose) in ADVISORY_SURFACES {
            assert_eq!(
                scan_advisory_banned(prose),
                None,
                "{name} must carry no banned framing:\n{prose}"
            );
        }
        // …and on the AUTH cell's inline prose, which no declaration-reading tripwire can see.
        let rendered = rendered_advisory_surfaces();
        assert_eq!(
            rendered.len(),
            28,
            "the rendered AUTH-cell product changed size — a scan over a shrunken subject passes \
             identically, so this is pinned rather than trusted"
        );
        for (label, cell) in &rendered {
            assert_eq!(
                scan_advisory_banned(cell),
                None,
                "{label} must render no banned framing:\n{cell}"
            );
        }
        // The cue an operator acts on is genuinely IN that subject — a product of empty cells
        // would satisfy the loop above while scanning nothing that matters.
        assert!(
            rendered
                .iter()
                .any(|(_, cell)| cell.contains("claude /login")),
            "the rendered AUTH-cell scan must actually reach the inline `claude /login` cue"
        );

        // BITES: each editorial group injected into a REAL advisory is caught.
        for (injected, caught) in [
            ("You should re-login.", "should"),
            ("Upgrade your plan.", "upgrade"),
            ("Your credentials are critical.", "critical"),
            ("Death is imminent.", "imminent"),
            ("Running out — top up first.", "top up"),
            ("Running low — need more seats.", "need"),
        ] {
            assert_eq!(
                scan_advisory_banned(&format!("{REFRESH_DISABLED_ADVISORY}\n{injected}")),
                Some(caught),
                "injecting {injected:?} into the refresh advisory must be caught"
            );
        }

        // The exemption is a carve-out, not a hole: naming the config section passes, while a
        // recommendation built AROUND that same verb is still caught on a group the exemption
        // never touched.
        assert_eq!(
            scan_advisory_banned("enable [refresh] to maintain them"),
            None
        );
        assert_eq!(
            scan_advisory_banned("you should enable [refresh]"),
            Some("should")
        );
    }

    /// Issue #1123 AC-1, asserted where the imperative actually lives: the advisory exemption is
    /// earned by `REFRESH_DISABLED_ADVISORY` ALONE, and `DEGRADED_CUE` — the imperative the issue
    /// was opened about — needs no exemption whatsoever.
    ///
    /// This is the measurement the decision rests on rather than a restatement of it. The cue
    /// orders an operation (`run 'sessiometer poke'`) and is nonetheless clean against the WHOLE
    /// central vocabulary, which is why issue #1123 concluded that the imperative MOOD is not what
    /// the #160 firewall polices and that no new vocabulary was needed to take these surfaces in.
    /// If a future edit makes the cue editorialise, the strict half here reddens even though the
    /// advisory-subset scan above would still pass it.
    ///
    /// Reddening on the earned half means an advisory edit dropped the last use of `enable`: the
    /// fix is to TIGHTEN `ADVISORY_EXEMPT_TOKENS`, not to widen this test.
    #[test]
    fn the_advisory_exemption_is_earned_by_the_advisory_alone_not_by_the_cue() {
        assert_eq!(
            scan_banned(DEGRADED_CUE),
            None,
            "the degraded cue must stay clean against the FULL central vocabulary — its \
             imperative is a remedy, not framing:\n{DEGRADED_CUE}"
        );
        for exempt in ADVISORY_EXEMPT_TOKENS {
            assert!(
                REFRESH_DISABLED_ADVISORY
                    .split(|c: char| !c.is_ascii_alphanumeric())
                    .any(|word| word.eq_ignore_ascii_case(exempt)),
                "{exempt:?} is exempt for the advisories but the refresh advisory no longer \
                 spends it — tighten ADVISORY_EXEMPT_TOKENS rather than carry a dead carve-out"
            );
        }
    }

    /// Every `Error::CliUsage` this parser can produce, rendered through its real `Display` and
    /// labelled by the argv that reaches it. Driven through [`parse_argv`] rather than hand-built,
    /// so the scan sees the shipped template, the real `usage_hint` and the `run … for usage`
    /// wrapper exactly as an operator would.
    ///
    /// The argv is deliberately NEUTRAL (`--zzz`, `zzz`). That is not the guard looking away: the
    /// interpolated half is the operator's own words, and pointing the scan at live output would
    /// make it report the operator's typo as our framing. `Error::CliUsage`'s doc comment records
    /// that split, and `the_argv_echo_is_the_operators_words_not_this_tools_framing` pins it.
    fn usage_error_surfaces() -> Vec<(&'static str, String)> {
        [
            // The three shapes `unexpected` renders — long flag, short flag, stray value — plus
            // the three verbs whose hints are what EARN the usage exemptions.
            ("status --zzz", vec!["status", "--zzz"]),
            ("status -z", vec!["status", "-z"]),
            ("log zzz", vec!["log", "zzz"]),
            ("disable --zzz", vec!["disable", "--zzz"]),
            ("enable --zzz", vec!["enable", "--zzz"]),
            ("remove --zzz", vec!["remove", "--zzz"]),
            // A value-bearing flag left dangling (`required_value`).
            ("stats --period", vec!["stats", "--period"]),
            // The three unknown-sub-action arms.
            ("service zzz", vec!["service", "zzz"]),
            ("daemon zzz", vec!["daemon", "zzz"]),
            ("config zzz", vec!["config", "zzz"]),
            // The two fully-static messages.
            ("config path --origin", vec!["config", "path", "--origin"]),
            ("use zzz --next", vec!["use", "zzz", "--next"]),
            // Both messages `backup_index` shapes at its one construction site (issue #1439):
            // the index left off entirely, and one that is not a 1-based ordinal.
            ("config restore", vec!["config", "restore"]),
            ("config restore 0", vec!["config", "restore", "0"]),
            // lexopt's own message, folded in by `From<lexopt::Error>` — third-party prose we
            // nonetheless ship, so it is scanned like the rest.
            ("status --json=1", vec!["status", "--json=1"]),
        ]
        .into_iter()
        .map(|(label, argv)| match parse_argv(&argv) {
            Err(err @ Error::CliUsage { .. }) => (label, err.to_string()),
            other => panic!("`{label}` must produce a CliUsage error, got {other:?}"),
        })
        .collect()
    }

    /// The completeness tripwire for the usage guard: every `Error::CliUsage` CONSTRUCTION SITE in
    /// this file's non-test code must be covered by [`usage_error_surfaces`].
    ///
    /// The same question issue #918's [`every_help_constant_is_scanned`] answers, asked of the
    /// other surface: without it, a new rejection path carrying "you should upgrade your plan"
    /// would ship entirely unscanned while every gate stayed green. A count rather than a
    /// name-match, because these are expressions inside function bodies with no declaration to
    /// key off.
    ///
    /// Keyed on the bare VARIANT name, not on `Error::CliUsage`: importing the variant and
    /// writing `CliUsage { … }` is ordinary Rust that the qualified spelling would have missed
    /// entirely — issue #1123's merge review found it. What the count still cannot see is a
    /// RENAMING import (`use …::CliUsage as Rejected`), so rather than leave that as an
    /// unexplained hole the test forbids one outright: this file has no reason to alias the
    /// variant, and an alias would silently empty the count below.
    ///
    /// Non-test code only — the test module constructs the variant freely in its own assertions,
    /// and those are not shipped prose. The boundary is this file's column-0 `#[cfg(test)]`, the
    /// same structural marker the module itself uses.
    #[test]
    fn every_cli_usage_construction_site_is_scanned() {
        let non_test = || {
            include_str!("cli.rs")
                .lines()
                .take_while(|line| !line.starts_with("#[cfg(test)]"))
        };
        assert!(
            !non_test().any(|line| line.contains("CliUsage as ")),
            "src/cli.rs aliases `CliUsage` under another name — the construction-site count below \
             keys on the variant name and would silently miss every site built through the alias"
        );
        let sites = non_test()
            .filter(|line| line.contains("CliUsage {"))
            .count();
        assert_eq!(
            sites, 9,
            "expected 9 `CliUsage` construction sites in src/cli.rs's non-test code; the count \
             moved — add an argv case to `usage_error_surfaces` covering the new site, then \
             update this"
        );
        // Cardinality on the other side too, pinned rather than compared: the argv cases and the
        // construction sites are DIFFERENT populations (several sites are reached by more than
        // one argv, and `status --json=1` reaches lexopt rather than a site here), so no
        // inequality between the two counts would evidence that any particular site is exercised.
        // What this pins is that the scanned subject has not silently shrunk.
        assert_eq!(
            usage_error_surfaces().len(),
            15,
            "the scanned argv cases changed count — a scan over a shrunken subject passes \
             identically, so this is pinned rather than trusted"
        );
    }

    /// Issue #1123 AC-2 for the usage surface: every authored usage hint and every rendered
    /// `Error::CliUsage` carries no banned framing, and the guard bites.
    #[test]
    fn the_usage_prose_carries_no_banned_framing_but_the_guard_bites_on_injection() {
        // Every `usage_hint` there is. Reached through [`ALL_HELP_TOPICS`], so this inherits the
        // completeness [`every_help_constant_is_scanned`] already enforces on that table rather
        // than hand-listing nineteen strings that could drift from it.
        for topic in ALL_HELP_TOPICS {
            let hint = topic.hint();
            assert_eq!(
                scan_usage_banned(hint),
                None,
                "the {} usage hint must carry no banned framing: {hint}",
                topic_const_name(*topic)
            );
        }

        // PASSES on every rendered rejection the parser can produce.
        for (label, rendered) in usage_error_surfaces() {
            assert_eq!(
                scan_usage_banned(&rendered),
                None,
                "`{label}` must render no banned framing:\n{rendered}"
            );
        }

        // BITES: each editorial group injected into a REAL rendered error is caught.
        let real = Error::CliUsage {
            message: "unknown flag `--zzz`".to_owned(),
            usage_hint: HelpTopic::Status.hint(),
        }
        .to_string();
        for (injected, caught) in [
            ("You should re-login.", "should"),
            ("Upgrade your plan.", "upgrade"),
            ("Your usage is critical.", "critical"),
            ("Exhaustion is imminent.", "imminent"),
            ("Running out — top up first.", "top up"),
            ("Running low — need more seats.", "need"),
        ] {
            assert_eq!(
                scan_usage_banned(&format!("{real}\n{injected}")),
                Some(caught),
                "injecting {injected:?} into a rendered usage error must be caught"
            );
        }

        // The exemption is a carve-out, not a hole: the hint may name the command, while a
        // recommendation built around that same verb is still caught.
        assert_eq!(
            scan_usage_banned("run `sessiometer remove --help` for usage"),
            None
        );
        assert_eq!(
            scan_usage_banned("you should remove that account"),
            Some("should")
        );
    }

    /// Every usage exemption is LOAD-BEARING: each token excused from the usage scan is one the
    /// shipped usage prose measurably spends, and the earner is identified rather than assumed.
    /// Issue #918 rejected a hand-picked "command name" exemption set on exactly this kind of
    /// evidence, and issue #1123 keeps the discipline by measuring a SEPARATE, tighter set here
    /// rather than reusing help's.
    ///
    /// Reddening means a hint edit dropped the last use of an excused token: TIGHTEN
    /// `USAGE_EXEMPT_TOKENS`, do not widen this test.
    #[test]
    fn every_usage_exemption_is_still_earned_by_the_shipped_usage_prose() {
        let surfaces: Vec<String> = ALL_HELP_TOPICS
            .iter()
            .map(|topic| topic.hint().to_owned())
            .chain(usage_error_surfaces().into_iter().map(|(_, text)| text))
            .collect();
        for exempt in USAGE_EXEMPT_TOKENS {
            let earned = surfaces.iter().any(|text| {
                text.split(|c: char| !c.is_ascii_alphanumeric())
                    .any(|word| word.eq_ignore_ascii_case(exempt))
            });
            assert!(
                earned,
                "{exempt:?} is exempt from the usage scan but no usage hint or rendered error \
                 uses it any more — tighten USAGE_EXEMPT_TOKENS rather than carry a dead carve-out"
            );
        }
        // …and the set stays TIGHTER than help's, which is why it is its own constant: `add` is
        // help-only vocabulary and no usage hint can earn it.
        assert!(
            !USAGE_EXEMPT_TOKENS.contains(&"add"),
            "`add` is not a verb in this CLI, so no usage hint can earn it"
        );
    }

    /// Issue #1123's other half of the `Error::CliUsage` verdict, PINNED rather than left
    /// implicit: a banned token arriving through argv reaches the rendered message, and that is
    /// correct.
    ///
    /// The #160 firewall governs what this tool ASSERTS. "unknown flag `--should`" asserts only
    /// that the flag was not recognised — neutral whatever the operator called it — so the
    /// `should` in it is the operator quoting themselves. Two things follow, and both are the
    /// reason this test exists rather than a comment saying so:
    ///
    /// 1. The echo must SURVIVE. Sanitising it would destroy the single diagnostic the message
    ///    exists to carry, so anyone who later "fixes" this by filtering argv reddens here and
    ///    reads the reasoning.
    /// 2. The guard's subject is therefore the TEMPLATE, driven with neutral argv, not live
    ///    output — a limitation of this surface worth stating outright, since a scan pointed at
    ///    production output would report the operator's typo as our framing.
    #[test]
    fn the_argv_echo_is_the_operators_words_not_this_tools_framing() {
        let rendered = match parse_argv(&["status", "--should"]) {
            Err(err @ Error::CliUsage { .. }) => err.to_string(),
            other => panic!("expected a CliUsage error, got {other:?}"),
        };
        assert!(
            rendered.contains("--should"),
            "the operator's own flag must survive into the diagnostic verbatim:\n{rendered}"
        );
        // Scanned as if it were our prose it WOULD trip — which is exactly why the guard above is
        // pointed at the template with neutral argv instead of at live output.
        assert_eq!(
            scan_usage_banned(&rendered),
            Some("should"),
            "the echoed token is visible to the scanner; the decision is that it is not ours"
        );
        // The authored half of that very same message is clean — the token came in through argv
        // and nowhere else.
        assert_eq!(
            scan_usage_banned(&rendered.replace("--should", "--zzz")),
            None,
            "with the operator's word removed, nothing this crate wrote trips the guard"
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
        // Issue #716: the output also carries the UNCONDITIONAL Claude Code range provenance
        // line — a record printed always, never a probe of the installed `claude`.
        assert!(
            version_line().contains("verified against Claude Code "),
            "the --version output must carry the CC-range provenance line: {}",
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
        // #401: the three read-only config diagnostics verbs route to their actions. The two
        // backup-ring verbs (#1439) are covered by `backup_verbs_parse_to_their_actions`.
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
        // can read a config or touch state as a side effect. That matters more since #1439 gave
        // the noun a mutating verb: a bare `config` still cannot restore anything, and
        // bare-noun-is-help stays consistent with `service` / `daemon`.
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

    // --- the backup ring's operator surface (issue #1439) ------------------

    #[test]
    fn backup_verbs_parse_to_their_actions() {
        assert_eq!(
            parse_argv(&["config", "backups"]).unwrap(),
            Command::Config {
                action: ConfigAction::Backups
            }
        );
        assert_eq!(
            parse_argv(&["config", "restore", "2"]).unwrap(),
            Command::Config {
                action: ConfigAction::Restore { index: 2 }
            }
        );
    }

    #[test]
    fn restore_rejects_a_missing_or_non_ordinal_index() {
        // The listing is 1-based, so `0` is a miscount rather than a boundary: answering it
        // with the ring's oldest entry would restore something the operator did not name.
        for argv in [
            vec!["config", "restore"],
            vec!["config", "restore", "0"],
            vec!["config", "restore", "x"],
            vec!["config", "restore", "-1"],
        ] {
            match parse_argv(&argv).unwrap_err() {
                Error::CliUsage { usage_hint, .. } => {
                    assert_eq!(usage_hint, "sessiometer config --help");
                }
                other => panic!("`{argv:?}` must be a CliUsage error, got {other:?}"),
            }
        }
    }

    #[test]
    fn the_origin_flag_is_rejected_on_the_backup_verbs_too() {
        // The flag means something for `show` alone; the strict-usage stance is the same
        // wherever else it lands, so the two new verbs inherit it rather than accepting it.
        for argv in [
            vec!["config", "backups", "--origin"],
            vec!["config", "restore", "1", "--origin"],
        ] {
            match parse_argv(&argv).unwrap_err() {
                Error::CliUsage { message, .. } => {
                    assert!(message.contains("--origin"), "names the flag: {message}");
                }
                other => panic!("`{argv:?}` must be a CliUsage error, got {other:?}"),
            }
        }
    }

    #[test]
    fn render_config_backups_reports_counts_and_timestamps_and_never_a_label() {
        use crate::roster_backup::Retained;
        use std::time::{Duration, UNIX_EPOCH};

        let dir = Path::new("/tmp/cfg/backups");
        assert_eq!(
            render_config_backups(dir, &[]),
            "no backups retained under /tmp/cfg/backups\n"
        );

        let entries = vec![
            Retained {
                path: dir.join("config.00001756900000.000000000.toml"),
                taken_at: UNIX_EPOCH + Duration::from_secs(1_756_900_000),
                accounts: Some(6),
            },
            Retained {
                path: dir.join("config.00001756800000.000000000.toml"),
                taken_at: UNIX_EPOCH + Duration::from_secs(1_756_800_000),
                accounts: Some(1),
            },
            Retained {
                path: dir.join("config.00001756700000.000000000.toml"),
                taken_at: UNIX_EPOCH + Duration::from_secs(1_756_700_000),
                accounts: None,
            },
        ];
        let rendered = render_config_backups(dir, &entries);
        assert_eq!(
            rendered,
            "3 retained under /tmp/cfg/backups (ring depth 3)\n\
             \x20 1  2025-09-03T11:46:40Z  6 accounts\n\
             \x20 2  2025-09-02T08:00:00Z  1 account\n\
             \x20 3  2025-09-01T04:13:20Z  unreadable\n",
        );
        // The listing is a more public surface than the file it describes — it is what an
        // operator pastes into a bug report — so it carries a count per entry and never a label
        // (`docs/specs/roster-backup-qualifying-write.feature.md`, Rule 3). That guarantee is
        // STRUCTURAL and belongs to the type: `Retained` has no field able to carry a label or a
        // uuid, so the byte-exact assertion above is the whole of what a test here can add. An
        // assertion searching the output for "label" would be vacuous for the same reason — it
        // would pass over a rendering that printed the label's VALUE — so it is deliberately not
        // written. What would actually break the guarantee is widening `Retained`; the compiler
        // is what surfaces that, and this comment is the note the author of such a change reads.
    }

    #[test]
    fn render_restore_notice_names_both_sides_before_the_write() {
        use crate::roster_backup::Retained;
        use std::time::{Duration, UNIX_EPOCH};

        let entry = Retained {
            path: PathBuf::from("/tmp/cfg/backups/config.00001756900000.000000000.toml"),
            taken_at: UNIX_EPOCH + Duration::from_secs(1_756_900_000),
            accounts: Some(6),
        };
        let path = Path::new("/tmp/cfg/config.toml");

        // What is installed, and what it displaces — the AC-5 property, so an operator who
        // miscounted sees both in the same two lines.
        let over_one = render_restore_notice(&entry, 6, path, Some(1));
        assert!(over_one.contains("6 in its roster"), "{over_one}");
        assert!(
            over_one.contains("/tmp/cfg/config.toml (1 account)"),
            "{over_one}"
        );

        assert!(
            render_restore_notice(&entry, 6, path, Some(3)).contains("(3 accounts)"),
            "plural"
        );
        // The incident's own state: nothing loadable to displace. Reported as such rather than
        // as zero accounts, which would read as a roster that exists and is empty.
        let over_nothing = render_restore_notice(&entry, 6, path, None);
        assert!(over_nothing.contains("(no loadable config)"), "absent");

        // The re-render is disclosed, and the retained file is named so it can be copied
        // verbatim by an operator who wants the bytes rather than the values.
        for rendered in [&over_one, &over_nothing] {
            assert!(
                rendered.contains("values are re-rendered")
                    && rendered.contains("/tmp/cfg/backups/config.00001756900000.000000000.toml"),
                "the notice discloses the re-render and names the retained file: {rendered}"
            );
        }

        // The renumbering warning fires on exactly the ring's own predicate: the displaced
        // config enters the ring iff it parses with a NON-EMPTY roster, and only then do the
        // indexes the operator just read move.
        assert!(over_one.contains("numbering shifts"), "{over_one}");
        assert!(
            !over_nothing.contains("numbering shifts"),
            "nothing was displaced into the ring, so nothing renumbers: {over_nothing}"
        );
        assert!(
            !render_restore_notice(&entry, 6, path, Some(0)).contains("numbering shifts"),
            "a zero-account config does not qualify, so it does not enter the ring"
        );
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
                        key: "session_ceiling",
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
            plain.contains("session_ceiling = 90"),
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
                force: true,
                next: false
            }
        );
        assert_eq!(
            parse_argv(&["use", "--force", "spare"]).unwrap(),
            Command::Use {
                target: Some("spare".to_owned()),
                force: true,
                next: false
            }
        );
        assert_eq!(
            parse_argv(&["use", "spare"]).unwrap(),
            Command::Use {
                target: Some("spare".to_owned()),
                force: false,
                next: false
            }
        );
    }

    #[test]
    fn use_parses_next_and_force_in_either_order() {
        // Issue #960 AC: `--next` is ORDER-INDEPENDENT exactly as `--force` already is —
        // both orders parse to the same command, and `--next` alone leaves `force` off.
        assert_eq!(
            parse_argv(&["use", "--next", "--force"]).unwrap(),
            Command::Use {
                target: None,
                force: true,
                next: true
            }
        );
        assert_eq!(
            parse_argv(&["use", "--force", "--next"]).unwrap(),
            Command::Use {
                target: None,
                force: true,
                next: true
            }
        );
        assert_eq!(
            parse_argv(&["use", "--next"]).unwrap(),
            Command::Use {
                target: None,
                force: false,
                next: true
            }
        );
    }

    #[test]
    fn use_next_and_an_explicit_target_are_mutually_exclusive_in_either_order() {
        // Issue #960 AC: `--next` says "I am not naming a target", so naming one contradicts
        // it. BOTH orders must be rejected identically — silently preferring one would swap
        // to an account the operator did not ask for.
        for argv in [
            ["use", "spare", "--next"].as_slice(),
            ["use", "--next", "spare"].as_slice(),
        ] {
            match parse_argv(argv).unwrap_err() {
                Error::CliUsage {
                    message,
                    usage_hint,
                } => {
                    assert!(
                        message.contains("--next") && message.contains("mutually exclusive"),
                        "names the conflict for {argv:?}: {message}"
                    );
                    assert_eq!(usage_hint, "sessiometer use --help");
                }
                other => panic!("expected a CliUsage error for {argv:?}, got {other:?}"),
            }
        }
    }

    #[test]
    fn use_next_never_leaks_a_secret_sigil_or_echoes_the_offending_target() {
        // Issue #15: the mutual-exclusion message names neither a token nor an email — and
        // deliberately does NOT echo the target either, since a roster label may itself be an
        // operator-authored email (#444/#447) with no business in a usage error.
        let message = parse_argv(&["use", "someone@example.com", "--next"])
            .unwrap_err()
            .to_string();
        assert!(!message.contains('@'), "no email: {message}");
        assert!(
            !message.to_lowercase().contains("token"),
            "no token: {message}"
        );
        assert!(
            message.contains("run `sessiometer use --help` for usage"),
            "points at the right help: {message}"
        );
    }

    #[test]
    fn use_help_advertises_next_so_the_flag_is_discoverable() {
        // Issue #960: a flag absent from `--help` is undiscoverable, which defeats the point
        // of the request — the page must show the `--next` form AND that it needs a daemon.
        let help = HelpTopic::Use.help();
        assert!(
            help.contains("sessiometer use --next"),
            "help shows the --next usage form: {help}"
        );
        assert!(
            help.contains("--next      advance to the next account in the swap chain"),
            "help describes --next: {help}"
        );
        assert!(
            help.contains("Needs a running daemon"),
            "help states the daemon precondition: {help}"
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
    fn run_parses_verbose_and_managed_and_rejects_a_bogus_flag() {
        assert_eq!(
            parse_argv(&["run", "--verbose"]).unwrap(),
            Command::Run {
                verbose: true,
                managed: false
            }
        );
        assert_eq!(
            parse_argv(&["run", "-v"]).unwrap(),
            Command::Run {
                verbose: true,
                managed: false
            }
        );
        assert_eq!(
            parse_argv(&["run"]).unwrap(),
            Command::Run {
                verbose: false,
                managed: false
            }
        );
        // `--managed` (issue #742): the launchd-agent marker, position-independent and
        // orthogonal to `-v`. A BARE `run` is NOT managed — that is exactly what preserves
        // the human-CLI exit-3 `AlreadyRunning` contract: only `--managed` flips a lost-lock
        // exit to 0, so an interactive `sessiometer run` still reports "already running".
        assert_eq!(
            parse_argv(&["run", "--managed"]).unwrap(),
            Command::Run {
                verbose: false,
                managed: true
            }
        );
        assert_eq!(
            parse_argv(&["run", "-v", "--managed"]).unwrap(),
            Command::Run {
                verbose: true,
                managed: true
            }
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
                target: Some("work".to_owned())
            }
        );
        assert_eq!(
            parse_argv(&["disable", "work"]).unwrap(),
            Command::SetEnabled {
                target: Some("work".to_owned()),
                enabled: false
            }
        );
        assert_eq!(
            parse_argv(&["enable", "work"]).unwrap(),
            Command::SetEnabled {
                target: Some("work".to_owned()),
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

    /// Issue #913: `RELIABILITY_USAGE`'s `--json` line advertises a schema version to script
    /// authors, and nothing structurally coupled that hand-typed number to the constant the wire
    /// actually emits — so every bump since 3 silently widened the gap until the help read
    /// `schema:2` against a live `schema:10`. This is that coupling.
    ///
    /// Deliberately a PARSE of the advertised token rather than a
    /// `contains(&format!("schema:{JSON_SCHEMA_VERSION}"))`: `"schema:10"` is a substring of
    /// `"schema:100"`, so a containment guard stays green whenever the help runs AHEAD of the
    /// constant — which is also the direction no reader catches by eye. Parsing survives rewording
    /// of the surrounding sentence too, where a comma-anchored literal would fail spuriously.
    ///
    /// Scoped to `RELIABILITY_USAGE` on purpose. `LOG_USAGE` carries its own `schema:2` twenty-five
    /// lines away, and that one is CORRECT against [`crate::log`]'s separate `JSON_SCHEMA_VERSION`.
    /// The two wires version independently and agree today only by coincidence, so a guard sweeping
    /// both would bind `log`'s help to `reliability`'s constant and fail the moment either moves.
    #[test]
    fn reliability_usage_advertises_the_live_json_schema_version() {
        let advertised: Vec<&str> = RELIABILITY_USAGE
            .match_indices("schema:")
            .map(|(at, marker)| {
                let rest = &RELIABILITY_USAGE[at + marker.len()..];
                let end = rest
                    .find(|c: char| !c.is_ascii_digit())
                    .unwrap_or(rest.len());
                &rest[..end]
            })
            .collect();

        // Exactly one, so the lockstep cannot be satisfied by a correct mention sitting beside a
        // stale one — the shape this defect would take if the line were ever duplicated.
        assert_eq!(
            advertised.len(),
            1,
            "RELIABILITY_USAGE must advertise the JSON schema version exactly once; found \
             {advertised:?}"
        );
        assert_eq!(
            advertised[0].parse::<u32>().ok(),
            Some(crate::reliability::JSON_SCHEMA_VERSION),
            "RELIABILITY_USAGE advertises `schema:{}` but the wire emits `schema:{}` — carry the \
             help text with the constant (issue #913)",
            advertised[0],
            crate::reliability::JSON_SCHEMA_VERSION,
        );
    }

    #[test]
    fn log_parses_bare_and_each_flag() {
        // Bare defaults to the whole log, every event, the text view, one shot.
        assert_eq!(
            parse_argv(&["log"]).unwrap(),
            Command::Log(crate::log::LogArgs {
                since: None,
                event: None,
                json: false,
                follow: false,
                channel: crate::log::Channel::Event,
            })
        );
        // `--since` / `--event` capture their RAW values (space- or `=`-separated); duration
        // parse + validation are deferred to `log::run`, so the CLI layer carries them through.
        for argv in [
            vec!["log", "--since", "7d", "--event", "swap"],
            vec!["log", "--since=7d", "--event=swap"],
        ] {
            assert_eq!(
                parse_argv(&argv).unwrap(),
                Command::Log(crate::log::LogArgs {
                    since: Some("7d".to_string()),
                    event: Some("swap".to_string()),
                    json: false,
                    follow: false,
                    channel: crate::log::Channel::Event,
                }),
                "argv {argv:?} must carry the raw flag values",
            );
        }
        // All four compose.
        assert_eq!(
            parse_argv(&["log", "--since", "24h", "--event", "restash", "--json", "--follow"])
                .unwrap(),
            Command::Log(crate::log::LogArgs {
                since: Some("24h".to_string()),
                event: Some("restash".to_string()),
                json: true,
                follow: true,
                channel: crate::log::Channel::Event,
            })
        );
    }

    #[test]
    fn log_follow_has_a_short_form_and_defaults_off() {
        // `-f` and `--follow` are the SAME flag (issue #774) — a tailer without `-f` would
        // surprise, and a short form that silently diverged from the long one would be worse.
        let long = parse_argv(&["log", "--follow"]).unwrap();
        assert_eq!(parse_argv(&["log", "-f"]).unwrap(), long);
        assert_eq!(
            long,
            Command::Log(crate::log::LogArgs {
                since: None,
                event: None,
                json: false,
                follow: true,
                channel: crate::log::Channel::Event,
            })
        );
        // Non-degeneracy: the default is genuinely `false`, so the assertions above are not
        // comparing two copies of the same default value.
        assert_eq!(
            parse_argv(&["log"]).unwrap(),
            Command::Log(crate::log::LogArgs {
                since: None,
                event: None,
                json: false,
                follow: false,
                channel: crate::log::Channel::Event,
            })
        );
    }

    #[test]
    fn log_value_bearing_flags_without_a_value_are_clear_errors() {
        // Each as the last token → a clear "needs a value", never a silent whole-log fallback.
        for flag in ["since", "event", "channel"] {
            let err = parse_argv(&["log", &format!("--{flag}")]).unwrap_err();
            assert!(matches!(err, Error::CliUsage { .. }));
            assert!(err.to_string().contains(flag), "got: {err}");
        }
    }

    /// **CONSTRAINT-C at the argv boundary (issue #775)**: the flag defaults to `event`, so a
    /// bare `sessiometer log` can never reach the ungoverned diagnostic channel. Pinned here
    /// rather than only in `log`'s own tests, because THIS is the layer that decides it.
    #[test]
    fn log_channel_defaults_to_event_and_parses_each_value() {
        use crate::log::Channel;

        // The default, from a bare invocation and from every other flag combination — the knob
        // is opt-in, and nothing else can turn it on by accident.
        for argv in [
            vec!["log"],
            vec!["log", "--json"],
            vec!["log", "--follow"],
            vec!["log", "--since", "7d", "--event", "swap", "--json"],
        ] {
            let Command::Log(args) = parse_argv(&argv).unwrap() else {
                panic!("{argv:?} must parse to a log command");
            };
            assert_eq!(
                args.channel,
                Channel::Event,
                "argv {argv:?} must default to the event channel"
            );
        }

        // Each value, space- and `=`-separated (lexopt handles both, and a short form would be
        // ambiguous with `-f`, so there deliberately is none).
        for (value, expected) in [
            ("event", Channel::Event),
            ("diag", Channel::Diag),
            ("all", Channel::All),
        ] {
            for argv in [
                vec![
                    "log".to_string(),
                    "--channel".to_string(),
                    value.to_string(),
                ],
                vec!["log".to_string(), format!("--channel={value}")],
            ] {
                let borrowed: Vec<&str> = argv.iter().map(String::as_str).collect();
                let Command::Log(args) = parse_argv(&borrowed).unwrap() else {
                    panic!("{argv:?} must parse to a log command");
                };
                assert_eq!(args.channel, expected, "argv {argv:?}");
            }
        }

        // An unrecognized value is rejected AT THE PARSE, with a message naming the closed set —
        // not deferred into the reader, and never silently falling back to the default.
        for bad in ["stderr", "both", "Event", "diagnostics", ""] {
            let err = parse_argv(&["log", "--channel", bad]).unwrap_err();
            assert!(
                matches!(err, Error::LogChannelInvalid(_)),
                "{bad:?} must be LogChannelInvalid, got {err:?}"
            );
            let shown = err.to_string();
            assert!(
                shown.contains("event") && shown.contains("diag") && shown.contains("all"),
                "the error must enumerate the accepted set, got {shown:?}"
            );
        }
    }

    /// **The issue #775 R8 gap, at the one place a test can reach it**: `run --managed` honors
    /// the `[tunables].verbose` knob, and nothing else changes.
    ///
    /// Exhaustive over the three booleans — eight cases, all of them — because the interesting
    /// content of this function is entirely in which combinations do NOT turn diagnostics on.
    /// ("A launchd-managed daemon actually writes to its stderr file" is the other half, and it
    /// needs real launchd; it is verified as the documented manual check in the PR, not faked
    /// green here.)
    #[test]
    fn managed_run_honors_the_config_knob_and_an_interactive_run_is_unchanged() {
        use crate::observability::Verbosity::{Quiet, Verbose};

        for (flag, managed, configured, expected, why) in [
            // The gap this closes: a managed daemon with the knob set is verbose WITHOUT -v.
            (
                Quiet,
                true,
                true,
                Verbose,
                "managed + knob is the whole point",
            ),
            // Unchanged defaults: silent everywhere the operator did not ask.
            (Quiet, true, false, Quiet, "managed, knob off — the default"),
            (
                Quiet,
                false,
                false,
                Quiet,
                "interactive, no -v — the default",
            ),
            // The knob is MANAGED-scoped: an interactive run is not touched by it, so arming it
            // for the background agent cannot surprise a foreground `sessiometer run`.
            (
                Quiet,
                false,
                true,
                Quiet,
                "the knob does not leak to interactive runs",
            ),
            // `-v` still wins, on either, whatever the config says.
            (Verbose, false, false, Verbose, "-v alone"),
            (Verbose, false, true, Verbose, "-v with the knob set"),
            (Verbose, true, false, Verbose, "-v on a managed run"),
            (Verbose, true, true, Verbose, "both"),
        ] {
            assert_eq!(
                effective_verbosity(flag, managed, configured),
                expected,
                "flag={flag:?} managed={managed} configured={configured}: {why}"
            );
        }
    }

    #[test]
    fn log_help_routes_and_an_unknown_flag_is_a_clear_error() {
        assert_eq!(
            parse_argv(&["log", "--help"]).unwrap(),
            Command::Help(HelpTopic::Log)
        );
        // A stray positional or a flag the reader does not accept → strict-usage error.
        assert!(matches!(
            parse_argv(&["log", "--period"]).unwrap_err(),
            Error::CliUsage { .. }
        ));
        assert!(matches!(
            parse_argv(&["log", "swap"]).unwrap_err(),
            Error::CliUsage { .. }
        ));
    }

    #[test]
    fn log_usage_lists_exactly_the_flags_the_parser_accepts() {
        // The issue #175 help/parser lockstep, both directions.
        //
        // Forward: every flag the parser accepts is documented. `--follow` joined this set in
        // issue #774 and `--channel` in issue #775, each moved out of the `foreign` reservation
        // below — which is exactly the trip wire that reservation existed to be. With `--channel`
        // gone, that reservation now holds only genuinely-foreign flags.
        // Each flag is probed with a value it would actually accept. The probe used to be a
        // literal `"x"` for everything, which worked only because `--since` and `--event` defer
        // their validation to `log::run` and so accept any string at this layer. `--channel`
        // validates AT the parse boundary (its value set is closed and needs no clock), so a
        // junk probe would fail it for the right reason and make the lockstep unsatisfiable.
        // Carrying the probe alongside the flag keeps the assertion "the parser accepts this
        // flag" rather than weakening it to "the parser accepts it OR rejects it somehow".
        for (flag, value) in [
            ("--since", Some("7d")),
            ("--event", Some("swap")),
            ("--json", None),
            ("--follow", None),
            ("--channel", Some("diag")),
        ] {
            assert!(LOG_USAGE.contains(flag), "LOG_USAGE must document {flag}");
            let argv = match value {
                Some(value) => vec!["log", flag, value],
                None => vec!["log", flag],
            };
            assert!(
                parse_argv(&argv).is_ok(),
                "{flag} must be accepted by the parser (argv {argv:?})"
            );
        }
        // The short forms, which the loop above cannot express: each must be documented in the
        // pair spelling the usage block uses, and accepted.
        for (short, long) in [("-h, --help", "-h"), ("-f, --follow", "-f")] {
            assert!(LOG_USAGE.contains(short), "LOG_USAGE must document {short}");
            assert!(
                parse_argv(&["log", long]).is_ok(),
                "{long} must be accepted by the parser"
            );
        }

        // Backward: no flag the parser REJECTS may be documented — the drift a copy-pasted
        // usage block actually produces. Every entry here is now a flag that belongs to some
        // OTHER verb; the last reserved-for-a-sibling-issue entry (`--channel`) graduated to the
        // accepted set above in issue #775.
        for foreign in [
            "--period",
            "--no-color",
            "--ascii",
            "--plaintext",
            "--overwrite",
            "--verbose",
        ] {
            assert!(
                !LOG_USAGE.contains(foreign),
                "LOG_USAGE documents {foreign}, which the parser rejects"
            );
            assert!(
                parse_argv(&["log", foreign]).is_err(),
                "{foreign} must be rejected by the parser"
            );
        }

        // And the verb is reachable from the top-level overview — carrying the SAME flags, not
        // merely present. Asserting only that the line exists let the root synopsis go stale
        // when `--follow` landed: the overview kept advertising the issue #773 flag set while
        // the parser had grown past it, and every other assertion here still passed.
        let root_line = ROOT_USAGE
            .lines()
            .find(|line| line.trim_start().starts_with("log "))
            .expect("ROOT_USAGE must carry a `log` line");
        for flag in ["--since", "--event", "--json", "--follow", "--channel"] {
            assert!(
                root_line.contains(flag),
                "the ROOT_USAGE `log` synopsis must carry {flag}, got {root_line:?}"
            );
        }
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

    // --- issue #767: FULL-OUTPUT goldens for the `status` human render ----------------
    //
    // Every `render_status` assertion above this line is either a substring check or an
    // exact-match over a deliberately tiny roster. Neither sees the whole render of a
    // REALISTIC roster, so a corruption outside the asserted fragment — a misaligned column,
    // a dropped row, a duplicated footer, a reordered pair — passes them green. These pin the
    // entire output, byte for byte, across the axes `status` actually degrades on: terminal
    // WIDTH (piped / wide / narrow / very narrow), the COLOUR gate, and a degenerate roster
    // (empty / single / all-`n/a`). The ASCII axis is deliberately absent: `status` has no
    // glyph ramp to fall back to (that is `stats`' chart surface, goldened in `stats.rs`).
    //
    // The clock is PINNED (`GOLDEN_NOW`) so every `humanize_until` cell — `12m`, `2h`, `5d` —
    // is deterministic; `render_status` takes `now` as a parameter, so no ambient clock
    // reaches this render.
    mod goldens {
        use super::*;
        use crate::render_golden::{self, Case};

        /// The pinned render instant. Every reset in [`golden_roster`] is an offset from THIS and
        /// `render_status` is handed the same value, so the humanized cells (`12m`, `2h`, `5d`)
        /// are fixed bytes and the constant's own value never reaches the render. Kept separate
        /// from the `NOW` above because that one serves the issue #72 `resets in` tests and is
        /// theirs to move; these goldens should not be reading a constant whose contract belongs
        /// to somebody else.
        const GOLDEN_NOW: i64 = 1_785_000_000;

        /// An all-quiet roster row: enabled, in no fault state, with no readings and no known
        /// resets. Every fixture row below is written as its DELTA from this via struct-update
        /// syntax, so a call site states exactly what it varies — `quiet("unseen")` says "nothing
        /// was ever read for this account" far more legibly than a wall of positional arguments
        /// whose meanings are invisible at the call site. Spelling the base out field by field here
        /// keeps the complete, auditable input a golden fixture wants.
        ///
        /// A local builder rather than the `status_line_resets` helper above, because the goldens
        /// vary fields that helper fixes (`active`, `enabled`, `quarantined`, `health`).
        fn quiet(label: &str) -> AccountStatusLine {
            AccountStatusLine {
                label: label.to_owned(),
                active: false,
                enabled: true,
                quarantined: false,
                recovering: false,
                session_pct: None,
                weekly_pct: None,
                session_resets_at: None,
                weekly_resets_at: None,
                weekly_exhausted: false,
                access_expires_at: None,
                refresh_health: None,
                health: None,
                blind_active: None,
                // No observed refresh-token deadline, so the issue #883 `EXPIRY` column elides on
                // the empty-column rule and every committed CLI-render golden stays byte-identical.
                // Keep it that way: issue #886 settled this the OTHER way round — populating the
                // shared base here would have moved every golden under `build/fixtures/cli-renders/`
                // AND destroyed the elision coverage those goldens are the only proof of. It added
                // [`expiry_roster`] beside this one instead, so the two directions are pinned
                // separately. Read that fixture's doc before reaching for this line.
                expiry: None,
            }
        }

        /// A `StatusResponse` carrying `accounts` and a next-swap target, with every fault
        /// field quiet. The fault band (keychain-locked, canonical scrub, canary, landing
        /// overshoot, systemic refresh) is OUT of this item's stated matrix and stays
        /// un-goldened here; issue #767's axes are width × colour × degenerate roster.
        fn response(
            accounts: Vec<AccountStatusLine>,
            next_swap: Option<NextSwap>,
        ) -> StatusResponse {
            StatusResponse {
                systemic_refresh_failure: None,
                systemic_refresh_source: None,
                canonical_scrub: None,
                keychain_locked: false,
                canary: None,
                expiry_cohort: None,
                recent_blind_preempt_swap: None,
                recent_landing_overshoot: None,
                refresh_enabled: None,
                accounts,
                next_swap,
            }
        }

        /// The canonical multi-account fixture. Deliberately heterogeneous, because a golden
        /// over a uniform roster proves very little:
        ///
        /// - `work` is the ACTIVE account, near its session trigger, with both resets known;
        /// - `世界` carries a WIDE-GLYPH label, so the goldens pin display-width padding
        ///   (UAX #11, issue #176) — the alignment bug class a `contains()` check cannot see;
        /// - `parked` is DISABLED and `stale` is QUARANTINED-and-recovering, so the optional
        ///   `AUTH` column is present with two different tag shapes;
        /// - `unseen` has NO readings at all, pinning that a failed poll renders `n/a` rather
        ///   than a fabricated `0%`.
        fn golden_roster() -> Vec<AccountStatusLine> {
            vec![
                AccountStatusLine {
                    active: true,
                    session_pct: Some(97),
                    weekly_pct: Some(40),
                    session_resets_at: Some(GOLDEN_NOW + 12 * 60),
                    weekly_resets_at: Some(GOLDEN_NOW + 5 * 86_400),
                    health: Some(CredentialHealth::Healthy),
                    ..quiet("work")
                },
                AccountStatusLine {
                    session_pct: Some(10),
                    weekly_pct: Some(20),
                    session_resets_at: Some(GOLDEN_NOW + 2 * 3_600),
                    weekly_resets_at: Some(GOLDEN_NOW + 3 * 86_400 + 4 * 3_600),
                    health: Some(CredentialHealth::Healthy),
                    ..quiet("世界")
                },
                AccountStatusLine {
                    enabled: false,
                    session_pct: Some(5),
                    weekly_pct: Some(8),
                    session_resets_at: Some(GOLDEN_NOW + 45 * 60),
                    weekly_resets_at: Some(GOLDEN_NOW + 86_400),
                    health: Some(CredentialHealth::Stale),
                    ..quiet("parked")
                },
                AccountStatusLine {
                    quarantined: true,
                    recovering: true,
                    session_pct: Some(60),
                    weekly_pct: Some(55),
                    session_resets_at: Some(GOLDEN_NOW + 3_600),
                    weekly_resets_at: Some(GOLDEN_NOW + 2 * 86_400),
                    health: Some(CredentialHealth::Degraded),
                    ..quiet("stale")
                },
                // Never polled — no readings at all, so every cell must render `n/a`.
                quiet("unseen"),
            ]
        }

        /// The next-swap footer target used by every non-degenerate case.
        fn to_spare() -> Option<NextSwap> {
            Some(NextSwap::Target {
                to: "世界".to_owned(),
                reason: Some(crate::daemon::NextSwapReason::OnlyCandidate),
            })
        }

        /// A REAL observed `refreshTokenExpiresAt` — 2026-07-31T12:10:02Z, from the credential of
        /// the account whose id starts `94f27044`, folded from `1785499802819` ms to seconds at the
        /// same boundary the poll path folds it. The SAME measured instant as
        /// `snapshot_build::WIRE_GOLDEN_EXPIRY_AT`; they cannot share a constant across
        /// `cfg(test)` module boundaries, so a correction to this provenance belongs in both.
        /// Real rather than round for the reason
        /// `the_measured_four_minute_cluster_of_four_accounts_is_one_cohort` gives: a fixture built
        /// from invented values can encode a shape the field never produces. Sits 5d18h50m past
        /// [`GOLDEN_NOW`], so the cell humanizes to fixed bytes.
        const EXPIRY_WITHIN_AT: i64 = 1_785_499_802;

        /// A roster carrying the REFRESH-token expiry modifier in ALL FOUR of its states (issue
        /// #886), layered onto [`golden_roster`]'s existing rows so the new column is pinned against
        /// the SAME heterogeneity the rest of the matrix already covers — an active row, a
        /// wide-glyph label, a disabled row, a quarantined one.
        ///
        /// The base roster deliberately keeps `expiry: None` on every row (see [`quiet`]): that is
        /// what makes every other `status` golden pin the column's ELISION, which is a
        /// load-bearing behaviour in its own right (issue #883 — a fleet whose credentials carry no
        /// deadline must show no column rather than a wall of `—`). Populating the shared base would
        /// have destroyed that coverage to gain this; a separate roster pins BOTH directions.
        ///
        /// - `work` and `世界` are the COHORT — deadlines an hour apart, so the fleet line below has
        ///   members whose own cells a reader can check it against;
        /// - `parked` is `Beyond`, the one state that legitimately means "not expiring soon";
        /// - `stale` has already `Lapsed`;
        /// - `unseen` was POLLED and its credential carried NO deadline, so it renders the GAP
        ///   beside four real ones. That row is the whole reason this fixture exists: `unknown` must
        ///   read as a pointed absence, never as the calm `Beyond` (issues #137/#876), and a golden
        ///   is the only assertion that shows it SIDE BY SIDE with the states it must not resemble.
        fn expiry_roster() -> Vec<AccountStatusLine> {
            let observed = |at: i64, horizon: ExpiryHorizon, cohort_id: Option<u32>| {
                Some(AccountExpiry {
                    expires_at: Some(at),
                    horizon_state: horizon,
                    cohort_id,
                })
            };
            let mut rows = golden_roster();
            rows[0].expiry = observed(EXPIRY_WITHIN_AT, ExpiryHorizon::Within, Some(0));
            rows[1].expiry = observed(EXPIRY_WITHIN_AT + 3_600, ExpiryHorizon::Within, Some(0));
            rows[2].expiry = observed(GOLDEN_NOW + 30 * 86_400, ExpiryHorizon::Beyond, None);
            rows[3].expiry = observed(GOLDEN_NOW - 86_400, ExpiryHorizon::Lapsed, None);
            // Polled, no deadline in the credential — an explicit observation, not an absence.
            rows[4].expiry = Some(AccountExpiry {
                expires_at: None,
                horizon_state: ExpiryHorizon::Unknown,
                cohort_id: None,
            });
            rows
        }

        /// The fleet-level synchronized-expiry cohort (issue #879) naming [`expiry_roster`]'s two
        /// grouped rows. `observed` is FOUR — the accounts with a known deadline — never the
        /// five-account roster, so the rendered denominator states the coverage rather than letting
        /// a reader assume the whole fleet was measured.
        fn expiry_cohort() -> Option<ExpiryCohort> {
            Some(ExpiryCohort {
                size: 2,
                observed: 4,
                earliest: EXPIRY_WITHIN_AT,
                span_secs: 3_600,
            })
        }

        /// The full expiry response — [`expiry_roster`] under the fleet [`expiry_cohort`], with the
        /// same next-swap footer every non-degenerate case carries.
        ///
        /// Named `_full` rather than the more obvious `expiry_response` because the enclosing
        /// `tests` module already owns an `expiry_response(accounts)` and `use super::*` brings it
        /// into scope here — a same-name, different-arity shadow is the kind of thing that reads
        /// fine and resolves to the wrong helper.
        fn expiry_full() -> StatusResponse {
            StatusResponse {
                expiry_cohort: expiry_cohort(),
                ..response(expiry_roster(), to_spare())
            }
        }

        /// Every goldened `status` case, freshly rendered. The single source of truth for the
        /// case list: the comparison, the canary, and the emitter all consume THIS, so a case
        /// can never be asserted in one and skipped in another.
        fn cases() -> Vec<Case> {
            let full = response(golden_roster(), to_spare());
            vec![
                // Piped / not a TTY: `cols` is `None`, so no column ever drops.
                Case::new(
                    "status-piped",
                    render_status(&full, GOLDEN_NOW, None, false),
                ),
                // A wide TTY: the full column set fits, uncoloured and coloured.
                Case::new(
                    "status-wide-plain",
                    render_status(&full, GOLDEN_NOW, Some(WIDE_COLS), false),
                ),
                Case::new(
                    "status-wide-color",
                    render_status(&full, GOLDEN_NOW, Some(WIDE_COLS), true),
                ),
                // Narrow: the lowest-priority droppable columns shed. The WEEKLY pair leaves
                // ATOMICALLY — never a `%` stranded without its reset.
                Case::new(
                    "status-narrow",
                    render_status(&full, GOLDEN_NOW, Some(NARROW_COLS), false),
                ),
                // Very narrow: only the keep-columns remain and they OVERFLOW rather than
                // wrap — one record per line is the invariant, at any width.
                Case::new(
                    "status-very-narrow",
                    render_status(&full, GOLDEN_NOW, Some(VERY_NARROW_COLS), false),
                ),
                // Degenerate rosters.
                Case::new(
                    "status-empty-roster",
                    render_status(
                        &response(Vec::new(), None),
                        GOLDEN_NOW,
                        Some(WIDE_COLS),
                        false,
                    ),
                ),
                Case::new(
                    "status-single-account",
                    render_status(
                        &response(golden_roster().into_iter().take(1).collect(), to_spare()),
                        GOLDEN_NOW,
                        Some(WIDE_COLS),
                        false,
                    ),
                ),
                Case::new(
                    "status-all-na",
                    render_status(
                        &response(
                            vec![
                                AccountStatusLine {
                                    active: true,
                                    ..quiet("work")
                                },
                                quiet("世界"),
                            ],
                            None,
                        ),
                        GOLDEN_NOW,
                        Some(WIDE_COLS),
                        false,
                    ),
                ),
                // The REFRESH-token expiry surfaces (issues #878/#882/#883/#879), pinned by #886.
                // The cases above all carry `expiry: None`, so they pin the column's
                // ELISION; these pin the POPULATED render — the four horizon cells side by side,
                // the fleet cohort line, and the shed-first degradation.
                Case::new(
                    "status-expiry-wide-plain",
                    render_status(&expiry_full(), GOLDEN_NOW, Some(WIDE_COLS), false),
                ),
                // Coloured, because the expiry cell carries its OWN per-cell overlay (red for
                // lapsed, yellow for within, dim for beyond, none for unobserved) and so does the
                // cohort line. Those tints are a *visual* claim about which row bites first; a
                // plain golden cannot see them, and a `contains()` check on an SGR escape cannot
                // see WHICH cell wears it.
                Case::new(
                    "status-expiry-wide-color",
                    render_status(&expiry_full(), GOLDEN_NOW, Some(WIDE_COLS), true),
                ),
                // The shed-FIRST width: `EXPIRY` carries `priority: Some(1)`, so it must be the
                // ONLY column gone here — the WEEKLY pair and `AUTH` both survive. Chosen at
                // exactly that boundary rather than reusing `NARROW_COLS` (which sheds WEEKLY too,
                // leaving the ORDER unobservable — two columns gone is equally consistent with the
                // wrong priority). The cohort LINE is not a column, so it must SURVIVE a width
                // that sheds the cells: the fleet fact is the half no row can carry, and losing it
                // to a narrow terminal while keeping per-account deadlines would be backwards.
                Case::new(
                    "status-expiry-shed",
                    render_status(&expiry_full(), GOLDEN_NOW, Some(EXPIRY_SHED_COLS), false),
                ),
            ]
        }

        /// Comfortably wider than the full table — nothing drops.
        const WIDE_COLS: usize = 200;
        /// Narrow enough to shed the lowest-priority columns, wide enough to keep some.
        const NARROW_COLS: usize = 40;
        /// Narrower than the `ACCOUNT · SESSION% · RESET` floor itself.
        const VERY_NARROW_COLS: usize = 12;
        /// Wide enough for every column EXCEPT `EXPIRY` — the boundary that makes the priority
        /// ORDER observable in a golden (issue #886). Asserted to shed exactly one column by
        /// [`the_expiry_shed_case_drops_only_the_expiry_column`], so a layout change that moved
        /// this width off the boundary fails loudly instead of silently degrading the case into a
        /// duplicate of [`NARROW_COLS`]'s two-column drop — which is what the first draft of this
        /// case did at 40 columns, shedding `EXPIRY` and the `WEEKLY` pair together and so proving
        /// nothing about which of them goes FIRST.
        const EXPIRY_SHED_COLS: usize = 56;

        /// The committed goldens, named by case. The macro derives each path from the name, so
        /// an entry cannot pair a case with someone else's bytes, and `include_str!` keeps every
        /// file a COMPILE-TIME input — a missing golden is a build error rather than a test that
        /// quietly skips, the same property the crate's wire goldens rely on
        /// (`src/stats.rs`, `src/daemon/snapshot_build.rs`).
        const GOLDENS: &[(&str, &str)] = render_golden::cli_render_goldens![
            "status-piped",
            "status-wide-plain",
            "status-wide-color",
            "status-narrow",
            "status-very-narrow",
            "status-empty-roster",
            "status-single-account",
            "status-all-na",
            "status-expiry-wide-plain",
            "status-expiry-wide-color",
            "status-expiry-shed",
        ];

        /// One-time emitter for the committed `status` render goldens (issue #767).
        /// `#[ignore]` — NOT part of the suite; it WRITES the bytes the gate below compares
        /// against. Run it ONLY alongside a DELIBERATE change to the `status` render:
        ///   `cargo test -- --ignored emit_cli_render_goldens`
        /// then look at the regenerated files and record why in a `CLI-Goldens-Rebaselined:`
        /// commit trailer (CI requires it — `scripts/check-cli-golden-rebaseline.sh`).
        #[test]
        #[ignore = "one-time cli-render-golden emitter — run ONLY alongside a deliberate render change"]
        fn emit_cli_render_goldens_status() {
            render_golden::emit(&cases());
        }

        #[test]
        fn the_committed_status_goldens_still_match_the_render() {
            render_golden::assert_matches_goldens("status", &cases(), GOLDENS);
        }

        /// CONSTRAINT-A: the gate can FAIL, demonstrated by MUTATION through the SAME
        /// predicate the assertion above uses — not by inspection.
        #[test]
        fn the_status_golden_gate_rejects_a_corrupted_render() {
            render_golden::assert_canary("status", &cases(), &[]);
        }

        /// The input-side half of the canary: a roster whose readings actually changed must
        /// not match the unperturbed golden. Mutating rendered bytes proves the comparison is
        /// byte-exact; this proves it is sensitive to the data flowing THROUGH the renderer,
        /// which is the shape a real regression takes.
        #[test]
        fn a_perturbed_roster_does_not_match_the_status_golden() {
            let mut perturbed = golden_roster();
            perturbed[0].session_pct = Some(96); // was 97
            render_golden::assert_perturbed_input_is_rejected(
                "status",
                "status-wide-plain",
                &render_status(
                    &response(golden_roster(), to_spare()),
                    GOLDEN_NOW,
                    Some(WIDE_COLS),
                    false,
                ),
                &render_status(
                    &response(perturbed, to_spare()),
                    GOLDEN_NOW,
                    Some(WIDE_COLS),
                    false,
                ),
            );
        }

        /// The expiry cases must actually COVER the four states they claim to (issue #886).
        ///
        /// A golden proves the bytes have not moved; it cannot tell you the bytes are the ones
        /// worth pinning. Nothing in [`assert_matches_goldens`] would notice if `expiry_roster`
        /// silently lost a state — the case would re-emit, match, and stay green while asserting
        /// less. So the coverage claim is stated as a property of the render, which also keeps it
        /// true across a re-baseline.
        ///
        /// The `unknown` row is asserted in BOTH directions on purpose. That it renders the GAP is
        /// the positive claim; that it does NOT render like the `Beyond` row beside it is the one
        /// that matters, because `beyond` is the only state that legitimately means "not expiring
        /// soon" and reading an unmeasured credential as it is the silent false-calm this whole
        /// item exists to prevent (issues #137/#876).
        #[test]
        fn the_expiry_goldens_cover_all_four_states_and_never_read_unknown_as_calm() {
            let text = render_status(&expiry_full(), GOLDEN_NOW, Some(WIDE_COLS), false);
            let row = |label: &str| {
                text.lines()
                    .find(|line| line.contains(label))
                    .unwrap_or_else(|| panic!("`{label}` is not in the expiry render:\n{text}"))
            };

            // The `EXPIRY` column materialized at all — the elision rule's other direction, which
            // the all-`None` cases pin and this one must not.
            assert!(
                text.lines()
                    .next()
                    .expect("a header row")
                    .contains("EXPIRY"),
                "a roster with observed deadlines must materialize the column:\n{text}"
            );
            // `Within` — a compact time-until, the same shape the RESET cells carry.
            assert!(row("work").contains("5d18h"), "{}", row("work"));
            // `Beyond` — further out, and still a real duration rather than a state word.
            assert!(row("parked").contains("30d"), "{}", row("parked"));
            // `Lapsed` — the bare state word, never a humanized negative remainder.
            assert!(row("stale").contains("lapsed"), "{}", row("stale"));

            // `Unknown` — the GAP, and NOT anything a reader could mistake for the calm verdict.
            let unseen = row("unseen");
            assert!(
                unseen.contains(EXPIRY_GAP),
                "a polled account with no observed deadline renders the gap: {unseen}"
            );
            assert!(
                !unseen.contains("30d"),
                "…and never borrows the calm `Beyond` cell beside it: {unseen}"
            );
            assert!(
                !unseen.contains("lapsed"),
                "…nor claims a lapse it cannot know: {unseen}"
            );
            // Stated at the CELL level as well, so the claim does not rest on substring luck: the
            // two verdicts a reader must never confuse have to render DIFFERENTLY.
            let rows = expiry_roster();
            assert_eq!(expiry_cell(rows[4].expiry, GOLDEN_NOW), EXPIRY_GAP);
            assert_ne!(
                expiry_cell(rows[4].expiry, GOLDEN_NOW),
                expiry_cell(rows[2].expiry, GOLDEN_NOW),
                "UNKNOWN and BEYOND must never render the same cell"
            );

            // The fleet half rides the same render, below the table.
            assert!(
                text.contains("expiry cohort: 2 of 4 accounts with a known deadline"),
                "the cohort line names its OBSERVED denominator, not the roster size:\n{text}"
            );
        }

        /// The shed case must drop EXACTLY the `EXPIRY` column — the assertion that keeps
        /// [`EXPIRY_SHED_COLS`] on its boundary.
        ///
        /// Without it, a layout change that widened any column would push this width past the
        /// boundary, shed the WEEKLY pair as well, and quietly turn the case into a second copy of
        /// the two-column drop — still green, but no longer showing the ORDER it was added for.
        /// The cohort LINE is checked in the same breath: it is not a column, so no width may take
        /// it away.
        #[test]
        fn the_expiry_shed_case_drops_only_the_expiry_column() {
            let shed = render_status(&expiry_full(), GOLDEN_NOW, Some(EXPIRY_SHED_COLS), false);
            let header = shed.lines().next().expect("the table has a header row");
            assert!(
                !header.contains("EXPIRY"),
                "EXPIRY has priority 1 and must be the first to go at {EXPIRY_SHED_COLS} cols: \
                 {header}"
            );
            assert!(
                header.contains("WEEKLY%") && header.contains("AUTH"),
                "…and it must be the ONLY one gone — the higher-priority WEEKLY pair and AUTH both \
                 survive, which is what makes the ORDER visible: {header}"
            );
            assert!(
                shed.contains("expiry cohort:"),
                "the fleet line is not a column and survives every width:\n{shed}"
            );
        }

        /// The matrix cells must actually DIFFER along the axis they claim to exercise.
        ///
        /// Without this a badly-chosen width silently renders `status-narrow` identically to
        /// `status-wide-plain`, and the "narrow" golden — green forever — asserts nothing
        /// about degradation. Stated as properties of the render rather than as pinned bytes,
        /// so it stays true across a re-baseline.
        #[test]
        fn each_width_case_exercises_the_degradation_it_claims() {
            let full = response(golden_roster(), to_spare());
            let header = |cols: Option<usize>| {
                render_status(&full, GOLDEN_NOW, cols, false)
                    .lines()
                    .next()
                    .expect("the table has a header row")
                    .split_whitespace()
                    .count()
            };
            let (wide, narrow, very_narrow) = (
                header(Some(WIDE_COLS)),
                header(Some(NARROW_COLS)),
                header(Some(VERY_NARROW_COLS)),
            );
            assert!(
                narrow < wide,
                "NARROW_COLS={NARROW_COLS} dropped no column (headers {narrow} vs {wide}) — the \
                 `status-narrow` golden is a duplicate of the wide one and proves nothing"
            );
            assert!(
                very_narrow < narrow,
                "VERY_NARROW_COLS={VERY_NARROW_COLS} shed nothing beyond NARROW_COLS (headers \
                 {very_narrow} vs {narrow})"
            );

            // The PIPED cell is the one width case that is identical to `status-wide-plain` BY
            // DESIGN: `fit_columns` never enters its drop loop for `cols: None`, which is
            // exactly what a comfortably-wide terminal also produces. Recorded here rather
            // than left implicit, because "two goldens are byte-identical" is otherwise
            // indistinguishable from the accident this whole test exists to catch — and
            // because the equality is the CONTRACT (a redirected `status` must not silently
            // start degrading to some assumed 80 columns). If a change ever makes piped and
            // wide diverge, this goes red and forces that to be a decision.
            assert_eq!(
                render_status(&full, GOLDEN_NOW, None, false),
                render_status(&full, GOLDEN_NOW, Some(WIDE_COLS), false),
                "the piped render diverged from the wide one — a non-TTY `status` is dropping \
                 columns, which means some width is being assumed where none is known"
            );

            // …and the floor OVERFLOWS rather than wrapping: one record per line at any width.
            let text = render_status(&full, GOLDEN_NOW, Some(VERY_NARROW_COLS), false);
            let table: Vec<&str> = text.lines().take_while(|l| !l.is_empty()).collect();
            assert_eq!(
                table.len(),
                golden_roster().len() + 1,
                "the very-narrow table wrapped: {} lines for {} accounts + a header",
                table.len(),
                golden_roster().len()
            );
            assert!(
                table.iter().any(|l| display_width(l) > VERY_NARROW_COLS),
                "nothing overflowed {VERY_NARROW_COLS} columns, so this case does not pin the \
                 overflow-rather-than-wrap invariant"
            );
        }

        /// Colour AUGMENTS: stripping every SGR escape from the coloured render must yield the
        /// uncoloured one, byte for byte. That is only true if padding is computed on display
        /// width BEFORE the colour wrap — an escape counted into a column width would leave
        /// the two rows differently padded. The one property most likely to break silently,
        /// and the reason `status-wide-color` is in the matrix at all.
        #[test]
        fn the_colour_overlay_never_enters_the_column_width_math() {
            let all = cases();
            let find = |name: &str| render_golden::rendered(&all, name);
            let coloured = find("status-wide-color");
            assert!(
                coloured.contains('\x1b'),
                "the coloured case carries no SGR escape, so it is not exercising the colour gate"
            );
            assert_eq!(
                render_golden::strip_ansi(coloured)
                    .expect("the coloured render has escapes to strip"),
                *find("status-wide-plain"),
                "the coloured render does not reduce to the plain one — colour is changing the \
                 layout, not augmenting it (pad-before-colour is broken)"
            );

            // The same property on the EXPIRY pair (issue #886). Not covered by the pair above:
            // `golden_roster()` carries no observed deadline, so `status-wide-color` ELIDES the
            // column entirely and cannot see a width bug in it. That column is the newest tinted
            // one, which makes it the likeliest place for the bug — and a drift there would look
            // like an ordinary golden move, get re-baselined with a trailer, and bake mis-padded
            // colour output in.
            let coloured_expiry = find("status-expiry-wide-color");
            assert!(
                coloured_expiry.contains("\x1b[33m"),
                "the expiry colour case carries no YELLOW band, so it is not tinting the column \
                 this pair was added for:\n{coloured_expiry}"
            );
            assert_eq!(
                render_golden::strip_ansi(coloured_expiry)
                    .expect("the coloured expiry render has escapes to strip"),
                *find("status-expiry-wide-plain"),
                "the coloured EXPIRY render does not reduce to the plain one — the tint is \
                 entering that column's width math"
            );
        }
    }

    /// The CLI half of the CROSS-SURFACE severity gate (issue #768) — see [`crate::cross_surface`]
    /// for why the contract exists and what the committed manifest is.
    ///
    /// Four INDEPENDENT observers converge on the one manifest here, and the independence is the
    /// point — any single one of them would leave a real gap:
    ///
    /// 1. **The declaration observer** — [`DaemonPayloadFault::ALL`] + `DaemonPayloadFault::
    ///    severity`. This is the rank's declared home.
    /// 2. **The RENDER observer** — the order and SGR band `render_status` actually prints. Issue
    ///    #575 was a defect of the RENDER SITES, not of any declaration (there was no shared
    ///    declaration then), so an enum-only gate would have missed it by construction: the enum
    ///    can rank correctly while a render site prints in a different order. Measured, not
    ///    argued — hoisting `render_systemic_refresh_failure` above the vault pair leaves the
    ///    declaration observer GREEN and reddens only this one.
    /// 3. **The per-account observer** — `StatusRow`'s own `session_severity` / `weekly_severity`,
    ///    the second axis issue #768's AC names.
    /// 4. **The per-account EXPIRY observer** (issue #886) — `expiry_cell` / `expiry_severity`, the
    ///    forward-looking axis carried ALONGSIDE `auth`. The only observer that pins cell TEXT as
    ///    well as band, because `StatusPanelFormat.expiryCell` claims to be "byte-identical" to
    ///    `expiry_cell` and a claim of byte-identity is worth asserting as one. It is also the only
    ///    one whose payloads are RELATIVE — each case states its deadline as an offset, so what is
    ///    pinned is a classification rule rather than an instant the two sides could read
    ///    differently (the Swift consumer supplies its own `now`).
    ///
    /// That enumeration is exhaustive over OBSERVERS OF THE RANK, not over this module's tests,
    /// and the `// ---- Observer N ----` blocks below are the weaker claim: they say where a test
    /// sits, not that everything in one is an observer. One member is deliberately outside the
    /// four — `the_daemon_payload_faults_invocation_stays_reachable_by_cargo_fmt` (issue #1283),
    /// which reads this file's own SOURCE TEXT to keep `cargo fmt` able to see the declaration.
    /// It files under Observer 1 because it guards that declaration's site, but it converges on no
    /// manifest and pins no band, so numbering it 5 would claim a fifth independent convergence
    /// that does not exist — and the independence of the four is the whole point above. A source
    /// lint over the same lines is not a fifth reading of them (issue #1293).
    ///
    /// What this module does NOT do is compare the two surfaces directly — it cannot; the panel is
    /// Swift. It pins the CLI to the committed manifest;
    /// `apps/menubar/Tests/CrossSurfaceSeverityParityTests.swift` pins the panel to the SAME
    /// committed bytes. Neither surface can then move alone.
    mod cross_surface_parity {
        use super::*;
        use crate::cross_surface::{
            self, band, ArbitrationEdge, ExclusiveGroup, FaultRank, KnownDivergence, Manifest,
            ObservedFault, UncoveredAxis,
        };

        /// A fixed render instant, shared by every observer in this module so their renders stay
        /// comparable. The fault observers never read a humanized cell; the expiry observer does —
        /// [`expiry_cases`] resolves each case's OFFSET against this instant — which is why it is a
        /// module-level constant rather than a local in whichever observer happened to need one.
        const AT: i64 = 1_785_000_000;

        /// The CLI's own band vocabulary, mapped to the manifest's medium-neutral one. Total, and
        /// deliberately so: a `Severity` with no spelling here would be silently unrepresentable in
        /// the manifest, and the panel would have nothing to be pinned against.
        ///
        /// `Dim` reached this map with the expiry axis (issue #886) — it is what `Beyond`, the one
        /// verdict that means "not expiring soon", renders as. It stays DISTINCT from `PLAIN`:
        /// de-emphasized-because-calm and uncoloured-because-unobserved are different facts, and
        /// collapsing them is exactly the #137 false-calm this axis refuses.
        fn band_of(severity: Option<Severity>) -> &'static str {
            match severity {
                Some(Severity::Red) => band::RED,
                Some(Severity::Yellow) => band::YELLOW,
                Some(Severity::Green) => band::GREEN,
                Some(Severity::Dim) => band::DIM,
                None => band::PLAIN,
            }
        }

        /// The `systemic_refresh_source` values both surfaces must rank identically. `None` is a
        /// pre-#813 daemon that sends no discriminant; the two `Some` arms are #378's sweep
        /// crossing and #787's startup preflight. Provenance picks the systemic banner's EVIDENCE
        /// clause and — per the panel resolver's own comment — "never moves this rank"; walking
        /// every variant is what turns that prose into an assertion.
        const SYSTEMIC_PROVENANCE: &[Option<SystemicRefreshSource>] = &[
            None,
            Some(SystemicRefreshSource::Sweep),
            Some(SystemicRefreshSource::Preflight),
        ];

        /// The manifest tokens for [`SYSTEMIC_PROVENANCE`], in the same order.
        fn provenance_token(source: Option<SystemicRefreshSource>) -> &'static str {
            match source {
                None => "none",
                Some(SystemicRefreshSource::Sweep) => "sweep",
                Some(SystemicRefreshSource::Preflight) => "preflight",
            }
        }

        /// A snapshot carrying exactly the named faults and nothing else noisy: one healthy
        /// account, refresh explicitly enabled (so the disabled-advisory stays silent), no swap
        /// footer, no overshoot. Everything that varies between two renders here is a fault line.
        fn response_with(faults: &[&str], source: Option<SystemicRefreshSource>) -> StatusResponse {
            let mut response = StatusResponse {
                systemic_refresh_failure: None,
                systemic_refresh_source: None,
                canonical_scrub: None,
                keychain_locked: false,
                canary: None,
                recent_blind_preempt_swap: None,
                recent_landing_overshoot: None,
                refresh_enabled: Some(true),
                accounts: vec![status_line("work", true, Some(50), Some(25))],
                next_swap: None,
                // Not a fault and deliberately absent: the synchronized-expiry cohort (issue #879)
                // is forward-looking capacity, never a `DaemonPayloadFault`, so it carries no rank
                // in the ADR-0026 table this helper feeds and would only add noise here.
                expiry_cohort: None,
            };
            for id in faults {
                match *id {
                    "keychain_locked" => response.keychain_locked = true,
                    "canonical_scrub_exhausted" => {
                        response.canonical_scrub = Some(CanonicalScrub::Exhausted);
                    }
                    "canonical_scrub_recovering" => {
                        response.canonical_scrub = Some(CanonicalScrub::Recovering);
                    }
                    "canary_drift_refusing" => {
                        response.canary = Some(CanaryStatus::Drift {
                            displayed: "work".to_owned(),
                            matched: "spare".to_owned(),
                            overridden: false,
                        });
                    }
                    "canary_drift_overridden" => {
                        response.canary = Some(CanaryStatus::Drift {
                            displayed: "work".to_owned(),
                            matched: "spare".to_owned(),
                            overridden: true,
                        });
                    }
                    "canary_ambiguous" => {
                        response.canary = Some(CanaryStatus::Ambiguous { count: 2 });
                    }
                    "canary_refused_unparseable_canonical" => {
                        response.canary = Some(CanaryStatus::RefusedUnparseableCanonical);
                    }
                    "systemic_refresh_failure" => {
                        response.systemic_refresh_failure = Some(3);
                        response.systemic_refresh_source = source;
                    }
                    other => panic!(
                        "no wire mapping for cross-surface fault `{other}` — a fault added to the \
                         manifest must also be constructible here, or the render observer silently \
                         stops covering it"
                    ),
                }
            }
            response
        }

        /// The ONE line a fault adds to the render, found by DIFFERENCE: render the same snapshot
        /// with and without the fault and take the added line. No marker substrings, so the
        /// observer cannot drift out of step with a re-worded fault line — and a fault whose
        /// renderer stopped emitting anything fails loudly here instead of going quietly unranked.
        fn fault_line(id: &str, source: Option<SystemicRefreshSource>, color: bool) -> String {
            let quiet = render_status(&response_with(&[], source), AT, None, color);
            let noisy = render_status(&response_with(&[id], source), AT, None, color);
            let added: Vec<&str> = noisy
                .lines()
                .filter(|line| !quiet.lines().any(|base| base == *line))
                .collect();
            assert_eq!(
                added.len(),
                1,
                "`{id}` added {} line(s) to the render, expected exactly 1 — the fault-line \
                 observer cannot identify it (provenance {}): {added:?}",
                added.len(),
                provenance_token(source)
            );
            added[0].to_owned()
        }

        /// The band a rendered fault line carries, read from its SGR overlay — the CLI's actual
        /// urgency signal, not a re-derivation of what it ought to be.
        fn band_of_line(line: &str) -> &'static str {
            if line.starts_with("\x1b[31m") {
                band::RED
            } else if line.starts_with("\x1b[33m") {
                band::YELLOW
            } else {
                assert!(
                    !line.starts_with('\x1b'),
                    "fault line carries an SGR that is neither red nor yellow: {line:?}"
                );
                band::PLAIN
            }
        }

        /// Read a rendered `status` back as an ordered, banded fault sequence: each named fault
        /// located by its own rendered line, ordered by where that line appears. `color` must
        /// describe how `rendered` was produced — the lines are matched exactly, SGR and all, so a
        /// mismatch would find nothing and panic below rather than quietly mis-order.
        fn observe_render(
            rendered: &str,
            present: &[&str],
            source: Option<SystemicRefreshSource>,
            color: bool,
        ) -> Vec<ObservedFault> {
            let mut located: Vec<(usize, ObservedFault)> = present
                .iter()
                .map(|id| {
                    let line = fault_line(id, source, color);
                    let at = rendered
                        .lines()
                        .position(|candidate| candidate == line)
                        .unwrap_or_else(|| {
                            panic!(
                                "`{id}`'s line is ABSENT from a render that sets it — the fault \
                                 stopped rendering, so it is unranked on this surface:\n{rendered}"
                            )
                        });
                    (at, ObservedFault::new(id, band_of_line(&line)))
                })
                .collect();
            located.sort_by_key(|(at, _)| *at);
            located.into_iter().map(|(_, fault)| fault).collect()
        }

        /// The manifest as the LIVE Rust source of truth would write it. The emitter's body and the
        /// gate's expectation are the same function, so a re-baseline cannot bless something the
        /// gate would not accept.
        fn manifest_from_source() -> Manifest {
            Manifest {
                schema: cross_surface::MANIFEST_SCHEMA,
                about: "Cross-surface severity contract (issue #768). EMITTED by the Rust gate \
                        (`cargo test -- --ignored emit_cross_surface_severity_manifest`) from \
                        `src/cli.rs`'s `DaemonPayloadFault`, which ADR-0026 makes the single home \
                        of the rank; CONSUMED by both `src/cli.rs` \
                        (mod tests::cross_surface_parity) and \
                        `apps/menubar/Tests/CrossSurfaceSeverityParityTests.swift`. Neither \
                        surface can change its rank alone: the emitting side reddens until this \
                        file is re-emitted, and re-emitting reddens the other side until it \
                        follows. Hand-editing this file is not a re-baseline — it is a way to \
                        break both gates at once."
                    .to_owned(),
                daemon_fault_ranks: DaemonPayloadFault::ALL
                    .iter()
                    .enumerate()
                    .map(|(index, fault)| FaultRank {
                        rank: u8::try_from(index + 1).expect("at most 255 daemon-payload faults"),
                        id: fault.cross_surface_id().to_owned(),
                        severity: band_of(fault.severity()).to_owned(),
                    })
                    .collect(),
                exclusive_groups: vec![
                    ExclusiveGroup {
                        wire_field: "canonical_scrub".to_owned(),
                        members: vec![
                            "canonical_scrub_exhausted".to_owned(),
                            "canonical_scrub_recovering".to_owned(),
                        ],
                        why: "`canonical_scrub` is one wire value, so a snapshot is \
                              exhausted-XOR-recovering. The two variants sit at OPPOSITE ends of \
                              the rank (2 and 8) on purpose — severity ranks by (fault, VARIANT), \
                              never fault identity — but nothing ever arbitrates between them."
                            .to_owned(),
                    },
                    ExclusiveGroup {
                        wire_field: "canary".to_owned(),
                        members: vec![
                            "canary_drift_refusing".to_owned(),
                            "canary_ambiguous".to_owned(),
                            "canary_refused_unparseable_canonical".to_owned(),
                            "canary_drift_overridden".to_owned(),
                        ],
                        why: "The canary reports ONE verdict at a time, so no snapshot holds two \
                              of these. Their relative order is a stable READING convention \
                              (positive identity failures before the precautionary refusal), not \
                              runtime arbitration — which is exactly why the real arbitration \
                              edges below are the ones against the OTHER fault families."
                            .to_owned(),
                    },
                ],
                systemic_provenance_variants: SYSTEMIC_PROVENANCE
                    .iter()
                    .map(|source| provenance_token(*source).to_owned())
                    .collect(),
                arbitration_edges: arbitration_edges(),
                account_severity_cases: account_severity_cases(),
                expiry_cases: expiry_cases(),
                known_divergences: vec![
                    KnownDivergence {
                        id: "blind-degraded-tint".to_owned(),
                        cli: "red".to_owned(),
                        panel: "orange".to_owned(),
                        why: "A DEGRADED blind-active account. The CLI emphasizes the line in \
                              `Severity::Red`; the panel deliberately uses ORANGE because the \
                              blind-DEGRADED GLANCE is `.attention`, one rung below `.noRunway`, \
                              so red would over-signal PAST the glance. A per-medium colour \
                              choice under R-2 STATE-parity — and it is a divergence in the \
                              per-account tint only, never in the daemon-payload rank above. \
                              CORNERED is `.red` on BOTH surfaces, because its glance IS \
                              `.noRunway`."
                            .to_owned(),
                        record: "apps/menubar/Sources/StatusPanelFormat.swift `blindSymbol` \
                                 (#485/#572); ../hq/strategy/design-menubar.md R-2"
                            .to_owned(),
                        pinned: true,
                    },
                    KnownDivergence {
                        id: "fault-render-medium".to_owned(),
                        cli: "one SGR-tinted text line per applicable fault, all of them printed, \
                              worst-first"
                            .to_owned(),
                        panel: "exactly ONE banner — the worst applicable fault — tinted \
                                .error/.warning/.info"
                            .to_owned(),
                        why: "R-2 is RANK-parity, not glyph-parity: the vocabulary of a terminal \
                              line and a popover banner may differ, and so may how many are shown \
                              at once. What may not differ is which fault is worst. This is why \
                              the gate compares ORDER and BAND rather than pixels or bytes — a \
                              gate that forced byte-parity here would be a wrong gate."
                            .to_owned(),
                        record: "docs/adr/0026-daemon-fault-severity-rank-is-cross-surface.md \
                                 § Decision"
                            .to_owned(),
                        pinned: true,
                    },
                    KnownDivergence {
                        id: "next-swap-footer-wording".to_owned(),
                        cli: "`next swap: …` footer, deliberately uncoloured".to_owned(),
                        panel: "medium-idiomatic swap callout".to_owned(),
                        why: "ADR-0016 settled this as CONTENT-parity, explicitly `not \
                              byte-identical … footers are medium-idiomatic`. It is a capacity \
                              signal, not a payload fault, so ADR-0026's rank does not reach it \
                              and this gate does not assert it."
                            .to_owned(),
                        record: "docs/adr/0016-dead-active-no-target-surfaced-not-relaxed.md"
                            .to_owned(),
                        pinned: false,
                    },
                    KnownDivergence {
                        id: "panel-header-gauge-vs-mock-mark".to_owned(),
                        cli: "n/a — the CLI has no header glyph".to_owned(),
                        panel:
                            "the neutral system `gauge.medium`, where the design mock draws the \
                                locked Cycle-Gauge brand mark"
                                .to_owned(),
                        why:
                            "A MOCK-vs-panel divergence, not a CLI-vs-panel one, so it is outside \
                              this contract's axis entirely — enumerated because issue #768 AC4 \
                              names it, and so a reader hunting `known divergences` finds it \
                              recorded rather than concluding it was overlooked. NOTE: #768's AC4 \
                              calls this `an issue #173 provider-neutrality divergence`, and that \
                              attribution is WRONG — measured, not assumed. #173 is the separate \
                              PROVIDER-LINE divergence recorded below; the header glyph is a BRAND \
                              one (#437/#524). `build-comparison.py` states both in one sentence, \
                              which is very likely where the two got conflated."
                                .to_owned(),
                        record:
                            "apps/menubar/design/build-comparison.py (the healthy-status STATES \
                                 note) — NOT design/README.md § Expected reconciliations, whose \
                                 list carries the provider line but no header-glyph entry"
                                .to_owned(),
                        pinned: false,
                    },
                    KnownDivergence {
                        id: "panel-provider-line-absent".to_owned(),
                        cli: "n/a — the CLI's table has no provider column".to_owned(),
                        panel:
                            "no provider secondary line under each account name, where the mock \
                                draws one"
                                .to_owned(),
                        why: "The ACTUAL issue #173 provider-neutrality divergence, recorded here \
                              beside the header-glyph one precisely because the two are routinely \
                              conflated. The wire carries no `provider` field yet, so the panel \
                              has nothing to render — and, like the glyph above, this is \
                              MOCK-vs-panel rather than CLI-vs-panel, hence outside this \
                              contract's axis."
                            .to_owned(),
                        record: "apps/menubar/design/README.md § Expected reconciliations \
                                 (first entry)"
                            .to_owned(),
                        pinned: false,
                    },
                ],
                uncovered_axes: vec![
                    UncoveredAxis {
                        id: "blind-active-session-band".to_owned(),
                        why: "The CLI colours a BLIND active account's stale `~%` off \
                              `util_severity(last_known_session_pct)`, while \
                              `StatusPanelFormat.sessionSeverity` takes only the fresh percent \
                              and the panel reaches the blind row through `blindSeverity` / \
                              `blindSymbol` instead. The two surfaces may well agree, but they \
                              agree through DIFFERENT functions, and this contract has not \
                              measured it — so the account-severity cases below are all \
                              non-blind. Stating the boundary beats implying coverage."
                            .to_owned(),
                    },
                    UncoveredAxis {
                        id: "reset-proximity-bands".to_owned(),
                        why: "`proximity_severity` (issue #94) colours the reset cells by how \
                              soon a window flips, framed as RELIEF. It is a third band family, \
                              not part of ADR-0026's rank nor of the utilization bands, and no \
                              cross-surface claim about it has been ratified."
                            .to_owned(),
                    },
                    UncoveredAxis {
                        id: "runtime-notice-lines".to_owned(),
                        why: "`render_landing_overshoot` (issue #613) is a per-machine RUNTIME \
                              notice that prints Red among the fault lines but is deliberately \
                              OUTSIDE the ADR-0026 daemon-payload rank (its own doc comment says \
                              so, and it keeps `red_line` rather than `daemon_fault_line`). It is \
                              therefore not ranked here, and the render observer's snapshots \
                              leave it unset."
                            .to_owned(),
                    },
                ],
            }
        }

        /// Every ordered pair of faults that can co-occur — the arbitration EDGES, derived from
        /// the declaration order minus the mutually-exclusive families. Emitted into the manifest
        /// so the Swift gate provably walks the same universe.
        fn arbitration_edges() -> Vec<ArbitrationEdge> {
            let excluded = |a: DaemonPayloadFault, b: DaemonPayloadFault| {
                use DaemonPayloadFault::{
                    CanaryAmbiguous, CanaryDriftOverridden, CanaryDriftRefusing,
                    CanaryRefusedUnparseableCanonical, CanonicalScrubExhausted,
                    CanonicalScrubRecovering,
                };
                let scrub = |f| matches!(f, CanonicalScrubExhausted | CanonicalScrubRecovering);
                let canary = |f| {
                    matches!(
                        f,
                        CanaryDriftRefusing
                            | CanaryAmbiguous
                            | CanaryRefusedUnparseableCanonical
                            | CanaryDriftOverridden
                    )
                };
                (scrub(a) && scrub(b)) || (canary(a) && canary(b))
            };
            let mut edges = Vec::new();
            for (i, winner) in DaemonPayloadFault::ALL.iter().enumerate() {
                for loser in &DaemonPayloadFault::ALL[i + 1..] {
                    if !excluded(*winner, *loser) {
                        edges.push(ArbitrationEdge {
                            winner: winner.cross_surface_id().to_owned(),
                            loser: loser.cross_surface_id().to_owned(),
                        });
                    }
                }
            }
            edges
        }

        /// The per-account utilization cases, classified by the CLI's own `StatusRow` — the same
        /// code path the table colours cells with, not a re-statement of the band constants.
        fn account_severity_cases() -> Vec<cross_surface::AccountSeverityCase> {
            // Chosen to pin every BOUNDARY (the two thresholds, from both sides), the exhaustion
            // override, and the two no-reading arms — the cases where a band mistake is a real
            // operator-visible error rather than a rounding argument.
            let inputs: &[(&str, Option<u8>, Option<u8>, bool)] = &[
                ("both-green", Some(0), Some(10), false),
                ("just-below-yellow", Some(74), Some(74), false),
                ("at-yellow", Some(75), Some(75), false),
                ("just-below-red", Some(89), Some(89), false),
                ("at-red", Some(90), Some(90), false),
                ("full", Some(100), Some(100), false),
                (
                    "weekly-exhausted-overrides-a-green-percent",
                    Some(20),
                    Some(3),
                    true,
                ),
                ("no-session-reading", None, Some(50), false),
                ("no-weekly-reading", Some(50), None, false),
                ("no-readings-at-all", None, None, false),
            ];
            inputs
                .iter()
                .map(|(name, session, weekly, exhausted)| {
                    let account =
                        status_line_resets(name, *session, *weekly, *exhausted, None, None);
                    let row = StatusRow::new(&account, AT);
                    cross_surface::AccountSeverityCase {
                        name: (*name).to_owned(),
                        session_pct: *session,
                        weekly_pct: *weekly,
                        weekly_exhausted: *exhausted,
                        session_severity: row.session_severity.map(|s| band_of(Some(s)).to_owned()),
                        weekly_severity: row.weekly_severity.map(|s| band_of(Some(s)).to_owned()),
                    }
                })
                .collect()
        }

        /// The REFRESH-token expiry cases, classified by the CLI's OWN `expiry_cell` /
        /// `expiry_severity` — the same functions the table renders with, never a re-statement of
        /// the rule (issue #886).
        ///
        /// Chosen to walk all four wire states AND every REACHABLE arm of the render-time re-check
        /// that `expiry_view` and `StatusPanelFormat.expiryView` are documented as mirroring
        /// "arm-for-arm, INCLUDING the arm ORDER". The order is the interesting part and the part
        /// no per-state case can reach: three of these payloads are ones where the daemon's cached
        /// classification and the client's clock DISAGREE, and the arms decide which wins. A
        /// surface that reordered them would still pass a naive one-case-per-state set.
        ///
        /// Two arms are deliberately OUT of this set rather than silently missed. The
        /// `expiry: None` arm — never polled at all — is not expressible here: an
        /// [`cross_surface::ExpiryParityCase`] always builds a `Some(AccountExpiry { .. })`, which
        /// is the right shape for a payload-classification contract and the wrong one for
        /// "no payload". It is covered on its own surfaces instead — CLI
        /// `the_expiry_column_elides_until_some_account_has_an_observed_deadline`, panel
        /// `testUnpolledAccountOmitsTheExpiryKeyEntirely`. The defensive `(_, None)` arm — an
        /// observed-but-deadline-less `Within`/`Beyond` — is unreachable by construction from the
        /// daemon, so pinning it here would pin a shape no wire can carry.
        fn expiry_cases() -> Vec<cross_surface::ExpiryParityCase> {
            let day = 86_400;
            // (name, deadline offset from `AT`, cached wire classification)
            let inputs: &[(&str, Option<i64>, ExpiryHorizon)] = &[
                // The three ordinary observed verdicts.
                ("within", Some(3 * day), ExpiryHorizon::Within),
                ("beyond", Some(29 * day), ExpiryHorizon::Beyond),
                ("lapsed", Some(-day), ExpiryHorizon::Lapsed),
                // The absent field — the issue #137 invariant, and the case issue #886 exists for.
                ("unknown-no-deadline", None, ExpiryHorizon::Unknown),
                // `unknown` is authoritative in the OTHER direction: the daemon found no parseable
                // deadline, so a stray `expires_at` beside it must NOT be narrated into a duration.
                (
                    "unknown-outranks-a-stray-deadline",
                    Some(3 * day),
                    ExpiryHorizon::Unknown,
                ),
                // The render-time re-check: a snapshot is up to one poll interval old, so a
                // deadline can pass between the daemon's classification and the draw. `within` on
                // the wire plus a past deadline must read `lapsed` on BOTH surfaces — the one line
                // built to warn must not read calm at exactly the moment it starts mattering.
                (
                    "within-but-already-past-at-render",
                    Some(-60),
                    ExpiryHorizon::Within,
                ),
                // The boundary of that same rule: at-the-instant counts as passed.
                ("within-exactly-at-now", Some(0), ExpiryHorizon::Within),
                // A DECLARED lapse outranks a missing deadline — the arm ORDER, and the one whose
                // reversal is otherwise invisible: `lapsed` is a bare word that never reads the
                // instant, so falling through to the gap would discard the strongest negative
                // signal the wire carries and hide a dead login as no login problem at all.
                ("lapsed-without-a-deadline", None, ExpiryHorizon::Lapsed),
            ];
            inputs
                .iter()
                .map(|(name, offset, horizon)| {
                    let expiry = Some(AccountExpiry {
                        expires_at: offset.map(|secs| AT + secs),
                        horizon_state: *horizon,
                        cohort_id: None,
                    });
                    cross_surface::ExpiryParityCase {
                        name: (*name).to_owned(),
                        offset_secs: *offset,
                        // Taken through SERDE rather than the enum's own `as_str`, deliberately:
                        // the manifest claims to name the WIRE spelling, and serde is what puts
                        // the token on the wire. (The two agree — `snapshot_build`'s
                        // `expiry_horizon_tokens_match_their_serde_spelling` pins that — but
                        // agreeing is not the same as being the source.)
                        horizon_state: serde_json::to_value(horizon)
                            .ok()
                            .and_then(|value| value.as_str().map(str::to_owned))
                            .expect("ExpiryHorizon serializes to a bare string"),
                        cell: expiry_cell(expiry, AT),
                        severity: expiry_severity(expiry, AT).map(|s| band_of(Some(s)).to_owned()),
                    }
                })
                .collect()
        }

        /// `#[ignore]` — NOT part of the suite; it WRITES the bytes both gates compare against.
        /// Run it ONLY alongside a DELIBERATE change to the cross-surface severity contract:
        ///   `cargo test -- --ignored emit_cross_surface_severity_manifest`
        /// then move the PANEL to match, or `CrossSurfaceSeverityParityTests` stays red. That is
        /// the mechanism, not an inconvenience.
        #[test]
        #[ignore = "one-time cross-surface manifest emitter — run ONLY alongside a deliberate rank change"]
        fn emit_cross_surface_severity_manifest() {
            cross_surface::emit(&manifest_from_source());
        }

        // ---- Observer 1: the declaration ------------------------------------------------------

        #[test]
        fn the_committed_manifest_still_describes_the_declaration() {
            let committed = cross_surface::committed_manifest();
            let live = manifest_from_source();
            // Name a RANK move in the module's own prose first. The whole-manifest compare below
            // is the complete check, but it fails by printing ~300 lines of JSON twice, and a rank
            // change is both the likeliest reason to be standing here and the one thing this
            // contract exists for — so say which rank moved, in the same vocabulary the render
            // observer uses.
            let declared: Vec<ObservedFault> = live
                .daemon_fault_ranks
                .iter()
                .map(|entry| ObservedFault::new(&entry.id, &entry.severity))
                .collect();
            let findings =
                cross_surface::rank_divergences(&committed.daemon_fault_ranks, &declared);
            assert!(
                findings.is_empty(),
                "`DaemonPayloadFault` no longer declares the rank the committed manifest pins:\n  \
                 {}{}",
                findings.join("\n  "),
                cross_surface::rebaseline_hint()
            );
            // …and everything else the manifest carries: the exclusive groups, the arbitration
            // edges, the account cases, the divergence register, the uncovered axes, the `about`
            // prose.
            assert_eq!(
                cross_surface::to_committed_bytes(&committed),
                cross_surface::to_committed_bytes(&live),
                "the committed cross-surface manifest no longer matches what `src/cli.rs` \
                 declares.{}",
                cross_surface::rebaseline_hint()
            );
        }

        #[test]
        fn cross_surface_rank_is_the_declaration_order() {
            // `ALL` is generated from the enum's own variant list (issue #919), so
            // EXHAUSTIVENESS is no longer this test's to guard — a variant that is not in the list
            // is a variant that was never declared. What is still this test's: the BAND each fault
            // ranks at, pinned by the total match below, which a ninth variant fails to compile
            // until it answers; and the two SHAPE tripwires — a duplicated id, and the count,
            // which is a degenerate-subject guard first (an emptied list passes every assertion
            // inside the loop) and the re-emit-the-manifest prompt second.
            let mut seen = std::collections::BTreeSet::new();
            for fault in DaemonPayloadFault::ALL {
                assert!(
                    seen.insert(fault.cross_surface_id()),
                    "`{}` appears twice in DaemonPayloadFault::ALL",
                    fault.cross_surface_id()
                );
                // Total match: adding a variant to the enum breaks the build here.
                let expected_band = match fault {
                    DaemonPayloadFault::KeychainLocked
                    | DaemonPayloadFault::CanonicalScrubExhausted
                    | DaemonPayloadFault::CanaryDriftRefusing
                    | DaemonPayloadFault::CanaryAmbiguous
                    | DaemonPayloadFault::CanaryRefusedUnparseableCanonical => band::RED,
                    DaemonPayloadFault::SystemicRefreshFailure
                    | DaemonPayloadFault::CanaryDriftOverridden => band::YELLOW,
                    DaemonPayloadFault::CanonicalScrubRecovering => band::PLAIN,
                };
                assert_eq!(
                    band_of(fault.severity()),
                    expected_band,
                    "`{}` renders band `{}`",
                    fault.cross_surface_id(),
                    band_of(fault.severity())
                );
            }
            assert_eq!(
                seen.len(),
                8,
                "DaemonPayloadFault::ALL lists {} faults, expected 8 — the daemon-payload fault \
                 set changed shape. Re-emit the manifest so the panel is handed the new rank, then \
                 update this count",
                seen.len()
            );
        }

        /// Issue #1283: the `daemon_payload_faults!` invocation must stay reachable by `cargo fmt`.
        ///
        /// TWO conditions carry that, and neither works alone (issue #1271; the macro's own
        /// declaration doc states both). The invocation must be delimited with `(` or `[` —
        /// `rustfmt` leaves a BRACE-delimited body verbatim, unconditionally, even one that parses
        /// cleanly. AND its marker must leave the body PARSING as Rust — such a body is formatted
        /// only if it does, and neither a bare `const ALL;` nor a TRUNCATED `const ALL: _`,
        /// ascription intact, does. Break either and the ~60 variant lines leave `cargo fmt` with
        /// nothing else in the tree reporting it.
        ///
        /// The marker assert pins the exact spelling `const ALL: _;`, which is TIGHTER than the
        /// property above: any ascription parses, so `: u8` would leave the body just as reachable
        /// (measured — issue #1293). The pin is house style, the same precedent
        /// `CONFIRMATION_CALL_ARGUMENTS` sets, so that re-spelling the declaration's one marker is
        /// re-blessed deliberately rather than drifting in. Its failure message reads `left` back
        /// and sorts the measured edits by whether the region actually went un-gated (issues #1310,
        /// #1329) — what un-gates is any marker that stops the body PARSING, a TRUNCATED spelling
        /// as much as a dropped ascription, while the re-spellings and splits that keep it parsing
        /// never do, and an EMPTY `left` is not one state at all: it reports only that nothing in
        /// the body matched, which a marker still PRESENT but spelled past the filter reaches as
        /// readily as a deleted one — so do not read a red here, on its own, as proof of either
        /// verdict.
        #[test]
        fn the_daemon_payload_faults_invocation_stays_reachable_by_cargo_fmt() {
            let region = non_test_source(include_str!("cli.rs"));

            // Corpus canary FIRST, both directions, before anything is concluded from `region`:
            // truncation has to red on its own terms rather than surface as the cardinality
            // assert's `found 0` — whose message points the reader at a DUPLICATED invocation,
            // which is the opposite fault — or masquerade as a brace. The lower bound is the
            // invocation itself, read delimiter-agnostically so this canary cannot be satisfied
            // by the very property the gate exists to check; the upper is this file's `mod tests`.
            assert!(
                region.contains("\ndaemon_payload_faults!"),
                "no column-0 `daemon_payload_faults!` invocation in the non-test region. SEVERAL \
                 edits reach this and it cannot tell them apart; three causes were measured \
                 (issue #1310). The REGION may no longer reach the invocation — `non_test_source` \
                 stopped early, or a column-0 `#[cfg(test)]` was introduced above the invocation, \
                 which moves the same boundary without touching that function. Or the macro was \
                 RENAMED and this gate — which hard-codes the old name — was not renamed with it. \
                 Or the invocation itself was INDENTED off column 0, which this predicate reads \
                 as absence. Grep the name to settle which: gone at both sites is the rename; \
                 still at column 0 is the truncated region; there but indented is the third, and \
                 that one reds `cargo fmt --all --check` as well, naming the line (issue #1293)"
            );
            assert!(
                !region.contains("fn the_daemon_payload_faults_invocation_stays_reachable"),
                "the non-test region ran past this file's `mod tests` boundary"
            );

            let lines: Vec<&str> = region.lines().collect();

            // Every ITEM-LEVEL invocation. The anchor is column 0 plus the trailing `!`, and that
            // pair is what makes the subject unambiguous: `macro_rules!` spells the name with no
            // trailing `!`, and every other mention of it in the region sits in a doc comment,
            // where the `///` prefix keeps the name from ever being the line's FIRST token, which
            // is what `starts_with` requires. Indentation is not the discriminator — one of those
            // doc mentions sits at column 0.
            let sites: Vec<usize> = lines
                .iter()
                .enumerate()
                .filter(|(_, line)| line.starts_with("daemon_payload_faults!"))
                .map(|(at, _)| at)
                .collect();
            assert_eq!(
                sites.len(),
                1,
                "expected exactly one item-level `daemon_payload_faults!` invocation, found {} — a \
                 second site would let this gate pass on one while the other was un-gated",
                sites.len()
            );
            let at = sites[0];

            // CONDITION 1 — the delimiter. `[` is accepted alongside `(` because `rustfmt` formats
            // both; only `{` is the hazard. Measured, not assumed: with one variant mis-indented,
            // `cargo fmt --all --check` reds pointing straight at that variant under EITHER `(` or
            // `[`, and goes green under `{`.
            let opener = lines[at]["daemon_payload_faults!".len()..]
                .trim_start()
                .chars()
                .next();
            assert!(
                matches!(opener, Some('(' | '[')),
                "`daemon_payload_faults!` must be invoked with `(` or `[` on the same line as its \
                 name; found `{}` (`?` = nothing after the name on that line). That delimiter \
                 is LOAD-BEARING, not a stylistic accident: `rustfmt` leaves a brace-delimited \
                 macro body verbatim unconditionally, so `{{` silently returns the whole variant \
                 list to being invisible to `cargo fmt` (issue #1271) — and nothing else in this \
                 repo reds when it does. A SPLIT head (`?`) is the benign case and is NOT that: \
                 `cargo fmt` reaches it, rejoins the head and formats the body anyway, so it reds \
                 there first and this assert is only holding the one-line form (issue #1293)",
                opener.unwrap_or('?')
            );

            // CONDITION 2 — the marker, read from the INVOCATION's body, which is the token stream
            // `rustfmt` actually parses. The matcher's copy up at `macro_rules!` is deliberately
            // NOT asserted: it cannot diverge from this one silently, because dropping `: _` from
            // either site alone is a hard `no rules expected` compile error. Only dropping BOTH is
            // quiet, and that is what this catches. Asserting the matcher too would duplicate the
            // compiler — the same reason this file asserts the delimiter directly rather than
            // re-checking that the variants came out formatted.
            let body_end = lines[at + 1..]
                .iter()
                .position(|line| line.starts_with([')', ']', '}']))
                .expect("the `daemon_payload_faults!` invocation never closes at column 0");
            let markers: Vec<&str> = lines[at + 1..at + 1 + body_end]
                .iter()
                .map(|line| line.trim())
                .filter(|line| line.starts_with("const ALL"))
                .collect();
            assert_eq!(
                markers,
                ["const ALL: _;"],
                "the `daemon_payload_faults!` invocation body must carry exactly one marker, \
                 spelled `const ALL: _;`. SEVERAL edits land here; `left` reads back the lines \
                 that MATCHED, so it names the edit only when one of them survives the filter. The \
                 states below were each measured (issues #1310, #1329) and are not the whole set. \
                 Reproducing one is not always a single edit: the macro matches over the TOKEN \
                 stream, so the whitespace-and-comment rows below need only the invocation's \
                 marker touched, while any row that changes the marker's TOKENS — a different \
                 ascription, none at all, a rename, the line deleted — needs the matcher's copy up \
                 at `macro_rules!` changed with it, or the crate stops compiling and this assert \
                 never runs (CONDITION 2 above). `left: [\"const ALL;\"]` — the ascription GONE — \
                 really is un-gated: `rustfmt` formats this body only when it PARSES as Rust, and \
                 a bare `const ALL;` does not, so that edit leaves the whole variant list \
                 unformatted with `cargo fmt --all --check` still GREEN (issue #1271), which is \
                 exactly the state this gate exists to make loud. `left: [\"const ALL: u8;\"]` — \
                 merely a DIFFERENT ascription — un-gates nothing: any ascription parses, so the \
                 body stays formatted and a mis-indented variant still reds `cargo fmt`. `left: \
                 [\"const ALL\"]`, `[\"const ALL:\"]` or `[\"const ALL: _\"]` says only that the \
                 matched line is a PREFIX of the pinned spelling — never which edit produced it, \
                 and the edits that reach these three do not agree on the verdict. A SPLIT reads \
                 back its FIRST FRAGMENT, so those three are breaking after `ALL`, after `ALL:` \
                 and after `ALL: _`, while breaking BEFORE the name matches nothing, which is `[]` \
                 below; all four splits are benign the way a split head is, since the token stream \
                 is unchanged, so `rustfmt` rejoins the marker and reds `cargo fmt` on that line \
                 first. TRUNCATING the marker to those same three prefixes — at BOTH sites, so it \
                 still compiles — reads back the identical value and un-gates instead: none of \
                 them PARSES, whether the ascription went with the cut or survived it, so the body \
                 is left verbatim with `cargo fmt --all --check` still GREEN (each measured). That \
                 command separates the two with no perturbation at all — red at the marker's own \
                 line is a split, green is a truncation. `left: []` says only that NO line matched \
                 the filter — never that the marker is GONE. A marker still PRESENT reaches it \
                 whenever its trimmed line stops BEGINNING with `const ALL`: split before the \
                 name, a second space or a tab between those two tokens, a comment between them — \
                 each measured, each still compiling, each redding `cargo fmt --all --check` at \
                 the marker's own line. That is a PREFIX test, not a word one, so a renamed `const \
                 ALLOWED: _;` reads itself back rather than landing here (measured). A DELETED \
                 marker lands here too, and needs the matcher's copy dropped with it or the crate \
                 stops at `unexpected end of macro invocation` before any of this — and among the \
                 spellings above, only for that one is the un-gating question live: deleting the \
                 line alone strands the `#[cfg(test)]` and doc block above it, the body stops \
                 parsing and the region IS un-gated, while deleting those with it leaves the body \
                 parsing and the region gated. So `[]` settles nothing by itself. The check that \
                 does is the one the macro's declaration doc prescribes — mis-indent a variant and \
                 run `cargo fmt --all --check` — but read its DIFF here, not its exit status. That \
                 doc prescribes the status for an otherwise CLEAN body, and every re-spelling \
                 above that `rustfmt` itself rewrites — the splits, the extra space, the tab, the \
                 comment — already reds that command on the marker's own line, so it exits \
                 non-zero with the perturbation and without it. What moves is whether the \
                 mis-indented VARIANT's line is in the diff: present, the region is gated; absent, \
                 it is not. Read that way, every spelling above that leaves the body PARSING \
                 measured GATED — the different ascription, all four splits, the extra space, the \
                 tab, the comment and the rename — and every spelling that stops it parsing \
                 measured UN-GATED, the three truncations as surely as the ascription-dropped \
                 `const ALL;` at the top. The pin is exact regardless, so that re-spelling this \
                 marker is a deliberate re-blessing rather than drift — if you meant it, say so in \
                 this assertion's expected value (issue #1293)"
            );
        }

        // ---- Observer 2: what `render_status` actually prints ----------------------------------

        #[test]
        fn every_fault_renders_a_distinct_line_so_the_observer_can_identify_it() {
            // Degenerate-subject guard for the render observer: it locates a fault by its exact
            // rendered line, so two faults sharing a line would make every position lookup
            // ambiguous and the ordering assertions below meaningless.
            let manifest = cross_surface::committed_manifest();
            for source in SYSTEMIC_PROVENANCE {
                let lines: Vec<(String, String)> = manifest
                    .ordered_ids()
                    .iter()
                    .map(|id| ((*id).to_owned(), fault_line(id, *source, false)))
                    .collect();
                assert_eq!(
                    lines.len(),
                    8,
                    "expected 8 fault lines, observed {}",
                    lines.len()
                );
                for (i, (id_a, line_a)) in lines.iter().enumerate() {
                    for (id_b, line_b) in &lines[i + 1..] {
                        assert_ne!(
                            line_a,
                            line_b,
                            "`{id_a}` and `{id_b}` render the SAME line at provenance {} — the \
                             render observer cannot tell them apart",
                            provenance_token(*source)
                        );
                    }
                }
            }
        }

        #[test]
        fn a_maximal_snapshot_prints_the_faults_worst_first_at_their_pinned_bands() {
            // Issue #768 AC1 on this surface: ONE fixture snapshot, rendered, its fault ordering
            // read back out of the text. Four faults is the maximum a single snapshot can hold —
            // `canonical_scrub` and `canary` are each one wire value — so this is the widest real
            // co-occurrence, and it spans all three bands.
            let present = [
                "keychain_locked",
                "canonical_scrub_exhausted",
                "canary_drift_refusing",
                "systemic_refresh_failure",
            ];
            let manifest = cross_surface::committed_manifest();
            // `projection` itself asserts every named fault is pinned, and names the one that is
            // not — so no weaker count check is needed here.
            let expected = manifest.projection(&present);

            // Once per systemic PROVENANCE. Provenance picks the systemic line's evidence clause
            // (#787/#813) and must never move the rank — an invariant the panel states in prose
            // and, before this walk, nothing asserted on either surface.
            for source in SYSTEMIC_PROVENANCE {
                let rendered = render_status(&response_with(&present, *source), AT, None, true);
                let observed = observe_render(&rendered, &present, *source, true);
                let findings = cross_surface::rank_divergences(&expected, &observed);
                assert!(
                    findings.is_empty(),
                    "the `status` render diverges from the committed cross-surface contract at \
                     provenance {}:\n  {}{}",
                    provenance_token(*source),
                    findings.join("\n  "),
                    cross_surface::rebaseline_hint()
                );

                // …and the CONSTRAINT-A canary, over this very observation: prove it can fail.
                cross_surface::assert_canary(
                    "status render (maximal snapshot)",
                    &expected,
                    &observed,
                );
            }
        }

        #[test]
        fn every_arbitration_edge_prints_the_worse_fault_first() {
            // The total order, edge by edge. A maximal snapshot can only hold four faults at once,
            // so the pairwise walk is what actually establishes the full worst-first rank — and it
            // is the SAME edge list the panel's gate walks, read from the same manifest.
            let manifest = cross_surface::committed_manifest();
            assert_eq!(
                manifest.systemic_provenance_variants.len(),
                SYSTEMIC_PROVENANCE.len(),
                "the manifest pins {} provenance variant(s), this gate walks {} — the two must \
                 agree or one side is covering less than the contract claims",
                manifest.systemic_provenance_variants.len(),
                SYSTEMIC_PROVENANCE.len()
            );
            let mut checked = 0;
            for edge in &manifest.arbitration_edges {
                let present = [edge.winner.as_str(), edge.loser.as_str()];
                // An edge that involves systemic is walked once per provenance; the rest have no
                // provenance to vary, so one pass covers them.
                let involves_systemic = present.contains(&"systemic_refresh_failure");
                let sources: &[Option<SystemicRefreshSource>] = if involves_systemic {
                    SYSTEMIC_PROVENANCE
                } else {
                    &[None]
                };
                for source in sources {
                    let rendered = render_status(&response_with(&present, *source), AT, None, true);
                    let observed = observe_render(&rendered, &present, *source, true);
                    let findings =
                        cross_surface::rank_divergences(&manifest.projection(&present), &observed);
                    assert!(
                        findings.is_empty(),
                        "`{}` vs `{}` diverges from the contract at provenance {}:\n  {}{}",
                        edge.winner,
                        edge.loser,
                        provenance_token(*source),
                        findings.join("\n  "),
                        cross_surface::rebaseline_hint()
                    );
                    checked += 1;
                }
            }
            // Cardinality: a pass over a shrunken edge list is not evidence. 21 co-occurrable
            // pairs over 8 faults (28 total minus 1 scrub pair and 6 canary pairs); 7 of them
            // involve systemic and are walked once per provenance, so 14 + 7 × 3 = 35 comparisons.
            let systemic_edges = manifest
                .arbitration_edges
                .iter()
                .filter(|edge| {
                    edge.winner == "systemic_refresh_failure"
                        || edge.loser == "systemic_refresh_failure"
                })
                .count();
            assert_eq!(
                manifest.arbitration_edges.len(),
                21,
                "the edge universe changed shape"
            );
            assert_eq!(systemic_edges, 7, "systemic should meet all 7 other faults");
            let expected_comparisons =
                (21 - systemic_edges) + systemic_edges * SYSTEMIC_PROVENANCE.len();
            assert_eq!(
                checked,
                expected_comparisons,
                "walked {checked} comparisons — expected {expected_comparisons} ({} \
                 provenance-free edges + {systemic_edges} systemic edges × {} provenance variants)",
                21 - systemic_edges,
                SYSTEMIC_PROVENANCE.len()
            );
        }

        #[test]
        fn the_colour_gate_changes_only_the_band_never_the_fault_order() {
            // Colour is additive (ADR-0026): a `--no-color` / piped reader must lose the band but
            // never the ranking. Asserted rather than assumed, because a piped `status | grep` is
            // how an operator's health check most often reads these lines.
            let present = [
                "keychain_locked",
                "canonical_scrub_recovering",
                "canary_drift_overridden",
                "systemic_refresh_failure",
            ];
            // Through the SAME observer the ranking assertions use, so this compares what those
            // read rather than a second, parallel way of reading it. Bands necessarily differ
            // between the two runs — the ORDER is the claim.
            let order = |color: bool| -> Vec<String> {
                let rendered = render_status(&response_with(&present, None), AT, None, color);
                observe_render(&rendered, &present, None, color)
                    .into_iter()
                    .map(|fault| fault.id)
                    .collect()
            };
            assert_eq!(
                order(false),
                order(true),
                "the colour gate changed the fault ORDER — colour must be purely additive, so a \
                 piped `status | grep` reader keeps the ranking even without the bands"
            );
            let plain_render = render_status(&response_with(&present, None), AT, None, false);
            assert!(
                plain_render.lines().all(|line| !line.starts_with('\x1b')),
                "the uncoloured render still carries an SGR overlay"
            );
            // …and the coloured render genuinely exercised the bands, so the equality above is not
            // two identical uncoloured runs agreeing with each other.
            let coloured_render = render_status(&response_with(&present, None), AT, None, true);
            assert!(
                coloured_render.contains('\x1b'),
                "the coloured render carries no SGR at all — this case is not exercising the gate"
            );
        }

        // ---- Observer 3: the per-account utilization bands --------------------------------------

        #[test]
        fn the_committed_account_severity_cases_still_match_the_table_classifier() {
            // The per-account half of issue #768's AC2, on this surface. `StatusRow::new` is the
            // real code path the table colours cells with — not a re-statement of the band
            // constants, which would only prove the constants equal themselves.
            let manifest = cross_surface::committed_manifest();
            let live = account_severity_cases();
            assert_eq!(
                manifest.account_severity_cases,
                live,
                "the committed per-account severity cases no longer match `StatusRow`'s \
                 classification.{}",
                cross_surface::rebaseline_hint()
            );
            // Cardinality + non-degeneracy: cases that were all-`None` would compare equal while
            // asserting nothing about the bands.
            assert!(
                live.len() >= 8,
                "only {} account-severity cases — too few to pin both thresholds from both sides",
                live.len()
            );
            assert!(
                live.iter()
                    .any(|c| c.session_severity.as_deref() == Some(band::GREEN))
                    && live
                        .iter()
                        .any(|c| c.session_severity.as_deref() == Some(band::YELLOW))
                    && live
                        .iter()
                        .any(|c| c.session_severity.as_deref() == Some(band::RED))
                    && live.iter().any(|c| c.session_severity.is_none()),
                "the account-severity cases do not span all four session outcomes \
                 (green/yellow/red/no-reading), so a band mistake could hide in an uncovered arm"
            );
        }

        // ---- Observer 4: the per-account REFRESH-token expiry axis (issue #886) -----------------

        #[test]
        fn the_committed_expiry_cases_still_match_the_cli_cell_and_tint() {
            // The CLI half of the R-2 expiry contract. `expiry_cell` / `expiry_severity` are the
            // real functions `StatusRow::new` renders the column with, so this compares the
            // manifest against the SHIPPING classification rather than a restatement of it — and
            // `CrossSurfaceSeverityParityTests` compares the same bytes against the panel's.
            // Neither surface can move alone: change this one and the committed manifest goes
            // stale; re-emit it and the panel's gate reddens until it follows.
            let manifest = cross_surface::committed_manifest();
            let live = expiry_cases();
            assert_eq!(
                manifest.expiry_cases,
                live,
                "the committed expiry parity cases no longer match `expiry_cell` / \
                 `expiry_severity`.{}",
                cross_surface::rebaseline_hint()
            );

            // Non-degeneracy: the set must span every OUTCOME, not merely every input state. Two
            // different inputs can land on the same cell (three of these deliberately do — that IS
            // the arm-order claim), so a case list could grow while the outcomes it distinguishes
            // shrink.
            let cells: std::collections::BTreeSet<&str> =
                live.iter().map(|c| c.cell.as_str()).collect();
            assert!(
                cells.contains("3d") && cells.contains("29d") && cells.contains("lapsed"),
                "the expiry cases do not span the observed outcomes (a duration, a far duration, \
                 the lapse word): {cells:?}"
            );
            assert!(
                cells.contains(EXPIRY_GAP),
                "…nor the GAP, which is the unobserved-deadline outcome and the one that matters \
                 most: {cells:?}"
            );
            let bands: std::collections::BTreeSet<Option<&str>> =
                live.iter().map(|c| c.severity.as_deref()).collect();
            assert_eq!(
                bands,
                [Some(band::RED), Some(band::YELLOW), Some(band::DIM), None]
                    .into_iter()
                    .collect::<std::collections::BTreeSet<_>>(),
                "the expiry cases must span all four tint outcomes — red (lapsed), yellow \
                 (within), dim (beyond), and UNCOLOURED (unobserved). An uncovered band is a band \
                 the two surfaces can disagree on unobserved"
            );

            // The #137 invariant, asserted here as a claim about the CLI rather than only about
            // the manifest: an unobserved deadline renders the gap, uncoloured — and the two
            // states a reader must never confuse do not render alike.
            let unknown = live
                .iter()
                .find(|c| c.name == "unknown-no-deadline")
                .expect("the absent-field case is in the set");
            assert_eq!(unknown.cell, EXPIRY_GAP);
            assert_eq!(unknown.severity, None);
            let beyond = live
                .iter()
                .find(|c| c.name == "beyond")
                .expect("the beyond case is in the set");
            assert_ne!(
                (unknown.cell.as_str(), unknown.severity.as_deref()),
                (beyond.cell.as_str(), beyond.severity.as_deref()),
                "UNKNOWN must never render like the reassuring `Beyond` verdict — neither in text \
                 nor in tint"
            );
        }

        // ---- The enumerated legitimate divergences (issue #768 AC4) ----------------------------

        #[test]
        fn the_cli_side_of_each_pinned_divergence_is_still_true() {
            // A divergence that is merely DOCUMENTED drifts. Each pinned entry is asserted on this
            // surface here and on the panel there, so the register cannot quietly come to describe
            // a divergence that no longer exists — or stop describing one that does.
            let manifest = cross_surface::committed_manifest();
            let pinned: Vec<&KnownDivergence> = manifest
                .known_divergences
                .iter()
                .filter(|entry| entry.pinned)
                .collect();
            assert!(
                pinned.len() >= 2,
                "expected at least two PINNED divergences, found {}",
                pinned.len()
            );
            for entry in pinned {
                match entry.id.as_str() {
                    "blind-degraded-tint" => {
                        // The CLI's half: a DEGRADED blind-active line is Red. (The panel's half —
                        // orange — is asserted in CrossSurfaceSeverityParityTests.)
                        assert_eq!(
                            entry.cli,
                            band::RED,
                            "the register claims the CLI paints blind-DEGRADED `{}`",
                            entry.cli
                        );
                        let blind = BlindActive {
                            blind_secs: 900,
                            last_known_session_pct: 80,
                            auto_protection_degraded: true,
                        };
                        let line = render_blind_active("work", blind, true);
                        assert!(
                            line.starts_with("\x1b[31m"),
                            "the CLI's DEGRADED blind line is no longer Red, so the enumerated \
                             divergence no longer describes reality: {line:?}"
                        );
                        let ok = render_blind_active(
                            "work",
                            BlindActive {
                                auto_protection_degraded: false,
                                ..blind
                            },
                            true,
                        );
                        assert!(
                            !ok.starts_with('\x1b'),
                            "an OK blind line should carry no emphasis: {ok:?}"
                        );
                    }
                    "fault-render-medium" => {
                        // The CLI's half: ALL applicable fault lines print, not just the worst.
                        // This is what makes byte-parity with the panel's single banner a WRONG
                        // gate, and why this contract compares rank rather than bytes.
                        let present = ["keychain_locked", "systemic_refresh_failure"];
                        let rendered =
                            render_status(&response_with(&present, None), AT, None, false);
                        for id in present {
                            assert!(
                                rendered
                                    .lines()
                                    .any(|line| line == fault_line(id, None, false)),
                                "`{id}` is missing — the CLI is meant to print EVERY applicable \
                                 fault line, which is the divergence this entry records"
                            );
                        }
                    }
                    other => panic!(
                        "divergence `{other}` is marked pinned but nothing on this surface asserts \
                         it — either assert it here or set `pinned: false` and re-emit"
                    ),
                }
            }
        }

        #[test]
        fn the_uncovered_axes_are_declared_rather_than_implied() {
            // Honest scope: this gate covers the daemon-payload rank and the per-account
            // utilization bands. Everything else it deliberately does not touch is named in the
            // manifest, so a reader can tell "not covered" from "covered and green".
            let manifest = cross_surface::committed_manifest();
            assert!(
                !manifest.uncovered_axes.is_empty(),
                "no uncovered axes declared — there are some, and an undeclared gap reads exactly \
                 like coverage"
            );
            for axis in &manifest.uncovered_axes {
                assert!(
                    axis.why.len() > 40,
                    "uncovered axis `{}` has no real rationale — a bare id is not an enumeration",
                    axis.id
                );
            }
        }
    }
}
