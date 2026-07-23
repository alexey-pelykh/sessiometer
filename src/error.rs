// Copyright (c) 2026 Oleksii PELYKH
// SPDX-License-Identifier: MIT

//! Crate-wide error type.
//!
//! Invariant: an [`Error`] value never carries secret material (OAuth blobs,
//! keychain secrets). Its `Display` and `Debug` are therefore safe to print on
//! any channel — the foundation the output-redaction work (issue #15) builds
//! on.

use std::path::PathBuf;
use std::time::Duration;

/// The result type used throughout the crate.
pub(crate) type Result<T> = std::result::Result<T, Error>;

/// Every fallible operation in the crate surfaces one of these.
#[derive(Debug, thiserror::Error)]
pub(crate) enum Error {
    /// A subsystem exists as a seam but its behavior lands in a later work
    /// item. The payload is a static, secret-free hint (e.g. `"usage polling
    /// (#5)"`).
    #[error("not yet implemented: {0}")]
    Unimplemented(&'static str),

    /// An unrecognized CLI subcommand was given.
    #[error("unknown command: {0}")]
    UnknownCommand(String),

    /// The argv layer rejected the invocation (issue #175): an unknown flag, a
    /// value-less option, or otherwise malformed usage — the strict counterpart to
    /// the old silent no-op. `message` is the specific problem and `usage_hint` names
    /// the exact `--help` to run; both are secret-free (argv never carries a token or
    /// passphrase — the passphrase is read off-argv, cf. #39). Maps to the generic
    /// exit `1`, matching [`Error::UnknownCommand`] — both are "you asked for
    /// something that isn't a thing", distinct from a runtime failure.
    #[error("{message}\n  run `{usage_hint}` for usage")]
    CliUsage {
        message: String,
        usage_hint: &'static str,
    },

    /// `stats --period` got a value outside `day|week|month|lifetime`.
    #[error("invalid --period `{0}`: expected one of day, week, month, lifetime")]
    StatsPeriodInvalid(String),

    /// `stats --since` got a value that is neither a relative offset (e.g. `7d`, `24h`,
    /// `30m`, `2w`) nor an absolute date (`YYYY-MM-DD` or RFC 3339).
    #[error(
        "invalid --since `{0}`: expected a relative offset (e.g. 7d, 24h) or a date (YYYY-MM-DD)"
    )]
    StatsSinceInvalid(String),

    /// `stats` got both `--period` and `--since`, which select the window two different
    /// ways — the caller must pick one.
    #[error("--period and --since are mutually exclusive")]
    StatsPeriodSinceConflict,

    /// A usage value was not finite and so could not be rendered as `stats --json`.
    /// Unreachable under the aggregator's finite-output guarantee; mapped, never panicked.
    #[error("could not render stats as JSON: {0}")]
    StatsSerialize(&'static str),

    /// The `reliability --json` readout could not be serialized. Unreachable — the wire is
    /// bare integers / bools / nulls (issue #455); mapped, never panicked.
    #[error("could not render reliability readout as JSON: {0}")]
    ReliabilitySerialize(&'static str),

    /// `reliability --since` got a value that is not a relative duration — a non-negative
    /// integer with a unit `s`/`m`/`h`/`d`/`w` (e.g. `30m`, `24h`, `7d`, `2w`). Unlike
    /// `stats --since`, this window is duration-only (issue #494): an absolute date is not
    /// accepted here.
    #[error(
        "invalid --since `{0}`: expected a relative duration (e.g. 30m, 24h, 7d, 2w — units s/m/h/d/w)"
    )]
    ReliabilitySinceInvalid(String),

    /// The current user's home directory could not be resolved — from the
    /// password database on Unix, or from the Windows user-profile ladder
    /// (`%USERPROFILE%`, then the `FOLDERID_Profile` Known Folder); see
    /// [`crate::paths`].
    #[error("could not resolve the home directory for the current user")]
    HomeUnresolved,

    /// A directory that must be private is owned by a different uid.
    #[error("directory {0} is not owned by the current user")]
    ForeignOwnership(PathBuf),

    /// The current user's login name could not be resolved from the password
    /// database (`getpwuid(getuid())->pw_name`, see [`crate::paths`]). The
    /// isolated-refresh engine (issue #102) seeds its keychain item under this
    /// `acct` (the name Claude Code reads with), so it cannot proceed without it.
    #[error("could not resolve the login name for the current user")]
    UserUnresolved,

    /// The ephemeral isolated-refresh directory (`<support>/refresh/<account-uuid>`,
    /// issue #102) could not be created as a safe private directory: a pre-existing
    /// entry at that path is a symlink, refused rather than followed — a planted
    /// symlink could redirect the seeded `.claude.json` or the spawned `claude`'s
    /// writes outside our `0700` tree. The path is a filesystem location, never a
    /// secret.
    #[error("refusing to use the isolated-refresh directory {path}: it is a symlink, not a private directory")]
    UnsafeIsolatedDir { path: PathBuf },

    /// No `Claude Code-credentials` item is present in the keychain — an account
    /// must be captured before it can be read or swapped.
    #[error("no Claude Code credential found in the keychain (capture an account first)")]
    CredentialNotFound,

    /// More than one `Claude Code-credentials` item is present, so the active
    /// account is ambiguous. The resolve step refuses to guess (issue #2).
    #[error(
        "ambiguous keychain: {count} `Claude Code-credentials` items found (expected exactly one)"
    )]
    CredentialAmbiguous { count: usize },

    /// The keychain is locked: `security` exited 36 (`errSecInteractionNotAllowed`)
    /// during `{op}`. Detection only — wait/backoff handling lives in #13.
    #[error("keychain is locked (security exit 36) during {op}")]
    KeychainLocked { op: &'static str },

    /// A `security` CLI keychain operation failed for another reason. `op` is the
    /// operation (`"resolve"` / `"read"` / `"write"`) and `code` is the exit
    /// status (`-1` if signal-terminated). Deliberately carries neither secret
    /// material nor raw CLI output.
    #[error("keychain {op} via `security` failed (exit status {code})")]
    Keychain { op: &'static str, code: i32 },

    /// `CLAUDE_CONFIG_DIR` (or `CLAUDE_SECURESTORAGE_CONFIG_DIR`) holds a non-ASCII
    /// value, which sessiometer cannot map to Claude Code's keychain service name.
    /// CC hashes the **NFC-normalized** config-dir path; for an ASCII path NFC is
    /// the identity, so the raw bytes hash byte-identically and no Unicode-normalizer
    /// dependency is pulled in (issue #100). A non-ASCII path could differ between
    /// its NFC form and its raw bytes, so rather than compute a suffix that may
    /// silently address the **wrong** keychain item, resolution refuses. The
    /// offending value is a filesystem path and is deliberately NOT echoed.
    #[error(
        "CLAUDE_CONFIG_DIR (or CLAUDE_SECURESTORAGE_CONFIG_DIR) contains non-ASCII characters, \
         which sessiometer cannot map to Claude Code's keychain service name"
    )]
    NonAsciiConfigDir,

    /// No `config.toml` exists yet at the expected path. Carries the path (a
    /// filesystem location, never a secret) so the message can name it.
    #[error("no config file at {path} — run `sessiometer capture` to create one")]
    ConfigNotFound { path: PathBuf },

    /// No accounts in the roster to act on. The friendly, user-facing empty state
    /// for two consumers: the offline `list` view (an absent config, OR a
    /// well-formed tunables-only file whose roster is empty) and the daemon's
    /// [`crate::config::Config::require_roster`] precondition (`run` refuses to
    /// start with nothing to rotate across). Both read as "nothing captured yet"
    /// instead of leaking a lower-level "file missing" or "invalid config". An
    /// empty roster is a legitimate state — `capture` loads it to add the first
    /// account (#58) — so it is NOT a validation error. A *malformed* config is
    /// deliberately not remapped: it keeps surfacing as its real
    /// [`Error::ConfigParse`] / [`Error::ConfigInvalid`]. Secret-free.
    #[error("no accounts captured yet — run `sessiometer capture`")]
    RosterEmpty,

    /// `config.toml` is not valid TOML, or a field has the wrong type. The
    /// wrapped message comes from the TOML parser; it is secret-free because the
    /// config file holds no secrets — only labels, account UUIDs, stash names
    /// and integer tunables (issue #15).
    #[error("malformed config: {0}")]
    ConfigParse(String),

    /// A config value is out of range, or the roster is malformed (duplicate
    /// `account_uuid`/`stash`, or an empty field). An empty roster is NOT in this
    /// set — it is a valid state ([`Error::RosterEmpty`] is the daemon/`list`
    /// empty-state, #58). Carries a precise, secret-free message naming the
    /// offending field.
    #[error("invalid config: {0}")]
    ConfigInvalid(String),

    /// The cross-field rule failed: `target_max_session_usage` exceeds `session_ceiling`
    /// (no account could ever become a swap target, since the ceiling a candidate
    /// must sit below is itself above the trigger). A distinct variant from
    /// [`Error::ConfigInvalid`] so this case can be matched specifically
    /// (issue #3).
    #[error("invalid config: target_max_session_usage ({target_max_session_usage}) must not exceed session_ceiling ({trigger})")]
    ConfigTargetMaxSessionAboveTrigger {
        target_max_session_usage: i64,
        trigger: i64,
    },

    /// The peak-velocity runway coupling is UNSATISFIABLE (issue #608, discharging ADR-0023
    /// § Alternatives 3): the config stacks its swap lookahead — `near_limit_poll_secs`
    /// (via the reactive re-observation gap) and/or `session_velocity_horizon_secs` — so wide
    /// against so low a `session_ceiling` that at the assumed peak velocity
    /// (`swap::V_PEAK_SESSION_PCT_PER_MIN`) NO `target_max_session_usage` in its legal
    /// `1..=session_ceiling` range keeps a swapped-to account any runway. Equivalently: the
    /// composed fire point sits at or below 0, so every account would swap at any usage —
    /// ADR-0023 § Consequences' "absurd-config corner". Distinct from
    /// [`Error::ConfigTargetMaxSessionAboveTrigger`] (which bounds the reserve by the CEILING,
    /// a looser rule that this stack can satisfy while still being unswappable) and from
    /// [`Error::ConfigInvalid`], so the corner can be matched specifically. Carries the three
    /// offending tunables — all bare integers, never secrets (issue #15).
    #[error(
        "invalid config: no target_max_session_usage can keep runway — at peak session velocity \
         ({v_peak_pct_per_min} %/min) an account climbs past session_ceiling ({trigger}) \
         within the {window_secs}s swap lookahead, so the reserve bound is {bound_pct} (not positive). \
         Lower near_limit_poll_secs ({near_limit_poll_secs}) or session_velocity_horizon_secs \
         ({horizon_secs}), or raise session_ceiling."
    )]
    ConfigPeakRunwayUnsatisfiable {
        trigger: i64,
        near_limit_poll_secs: u64,
        horizon_secs: u64,
        window_secs: u64,
        bound_pct: i64,
        v_peak_pct_per_min: f64,
    },

    /// Claude Code's state file (`~/.claude.json`) does not exist — Claude Code
    /// has not run / no account is logged in, so there is nothing to capture.
    /// Carries the path (a filesystem location, never a secret).
    #[error("no Claude Code state at {path} — log in with `claude` first")]
    ClaudeStateNotFound { path: PathBuf },

    /// `~/.claude.json` is not valid JSON. Only the parser's `line`/`column` are
    /// carried — never the surrounding bytes, which include the account's
    /// `oauthAccount` identity block (issue #15 redaction).
    #[error(
        "malformed Claude Code state (~/.claude.json): JSON error at line {line} column {column}"
    )]
    ClaudeStateParse { line: usize, column: usize },

    /// `~/.claude.json` has no `oauthAccount` object — Claude Code is installed
    /// but no account is logged in, so there is no identity to record.
    #[error("no account is logged in to Claude Code (~/.claude.json has no oauthAccount)")]
    OauthAccountMissing,

    /// The logged-in account's `oauthAccount` is missing a required field (e.g.
    /// `accountUuid`, the roster key). `field` is a static field name, never a
    /// value (issue #15 redaction).
    #[error("the logged-in account is missing its `{field}` — cannot key the roster")]
    OauthAccountFieldMissing { field: &'static str },

    // --- Account enable/disable (issue #36) ----------------------------------
    /// `sessiometer disable`/`enable` was invoked without the required `<label>`.
    /// Carries the subcommand (a static, secret-free string) so the message names
    /// the exact usage.
    #[error("a label is required: `sessiometer {verb} <label>`")]
    RotationLabelRequired { verb: &'static str },

    /// `sessiometer disable`/`enable` was given a `<label>` that matches no roster
    /// account. The label is the operator's non-secret handle (issue #15), safe to
    /// quote; the message points at `list` to show the valid handles.
    #[error("no account labelled `{label}` — run `sessiometer list` to see the roster")]
    AccountLabelNotFound { label: String },

    /// A `config-set` (issue #268) label edit named an `account_uuid` that matches no
    /// roster account — a stale settings client (the account was `remove`d between its
    /// `config-get` read and the edit) or a client bug. The uuid is a non-secret roster
    /// key (issue #15), safe to quote. Distinct from [`AccountLabelNotFound`](Error::AccountLabelNotFound)
    /// (a `<label>` lookup): the settings path keys label edits by the immutable uuid, not
    /// the mutable label, so a duplicate-label roster stays unambiguous.
    #[error("no account with account_uuid `{account_uuid}` in the roster")]
    AccountUuidNotFound { account_uuid: String },

    /// A per-account stash is missing one or both of its keychain items
    /// (credential / oauthAccount), so the account cannot be restored. Carries
    /// the `service` (the `Sessiometer/<account_uuid>` stash name — a config value, never
    /// a secret). Surfaced by the swap engine (#6) reading a target's stash.
    #[error(
        "stash `{service}` is incomplete or absent (re-run `sessiometer capture` for this account)"
    )]
    StashIncomplete { service: String },

    // --- Usage polling (issue #5) ---------------------------------------------
    //
    // The HTTP outcome taxonomy for the read-only usage poll, as typed errors so
    // the poll loop (#7) and the 401 monitor (#13) can route each runtime state.
    // All are secret-free: the usage response carries only percentages / reset
    // timestamps (no token, no email), and these variants deliberately echo none
    // of the body — only a structural hint or the HTTP status code.
    /// The stored credential blob has no usable OAuth access token (no
    /// `claudeAiOauth.accessToken`), so there is no bearer to poll with. A
    /// capture/setup problem, not a transient one. Carries nothing — never the
    /// blob bytes (issue #15 redaction).
    #[error("the stored credential has no usable OAuth access token (re-capture this account)")]
    UsageTokenUnreadable,

    /// The poll did not complete: a `5xx` server error, or — when `status` is
    /// `0` — `curl` returned no HTTP response at all (DNS / connection / TLS /
    /// timeout). Transient by the taxonomy (5xx / network): back off and skip the
    /// cycle, never swap on missing data. `retry_after` carries the server-advised
    /// `Retry-After` (a `503` may send one), parsed from its delta-seconds form;
    /// the daemon honours it as a MINIMUM back-off wait (issue #76).
    #[error("usage poll did not complete (HTTP status {status}; 0 means no HTTP response)")]
    UsageTransient {
        status: u16,
        retry_after: Option<Duration>,
    },

    /// The usage endpoint rate-limited the poll (`HTTP 429`). Back off, log, skip
    /// the cycle — never swap on a throttled (missing) reading. `retry_after`
    /// carries the server-advised `Retry-After` (delta-seconds form) when present;
    /// the daemon honours it as a MINIMUM back-off wait (issue #76).
    #[error("usage poll was rate-limited (HTTP {status})")]
    UsageRateLimited {
        status: u16,
        retry_after: Option<Duration>,
    },

    /// A non-401, non-403 `4xx` other than 429 (e.g. `400` / `404` / `422`). Like
    /// 429 on the monitor path (design G4): back off, log, skip — never swap on a
    /// rejected reading. `status` preserves the actual code for the log.
    #[error("usage poll rejected (HTTP {status})")]
    UsageRejected { status: u16 },

    /// The stored access token was rejected with `HTTP 401` (and the consecutive
    /// count has not yet reached `monitor_401_n`). A transient 401 → back off and
    /// log; the re-stash trigger is a separate seam fired at the Nth consecutive
    /// 401 (issue #13 / #6). The poller never self-refreshes a token.
    #[error("usage poll unauthorized (HTTP 401) — the stored token was rejected")]
    UsageUnauthorized,

    /// The token authenticated but lacks the usage scope (`HTTP 403`) — the
    /// hallmark of a non-interactive setup token. Surfaced **distinctly** from a
    /// 401 (issue #5 acceptance): the fix is a fully-scoped re-capture, not a
    /// re-stash/retry.
    #[error(
        "usage poll forbidden (HTTP 403) — the stored token lacks the usage scope \
         (re-capture this account with an interactive login)"
    )]
    UsageScopeMissing,

    /// The poll returned `200` but the body could not be parsed into both quota
    /// dimensions. The wrapped message is a structural hint (a field/shape name)
    /// — never any response bytes. Treated like missing data: skip, never swap.
    #[error("malformed usage response: {0}")]
    UsageParse(String),

    // --- Daemon lifecycle (issue #7) -----------------------------------------
    /// Another `sessiometer` daemon already holds the single-instance lock, so
    /// this `run` must not start a second poll loop. Maps to process exit code
    /// `3` (see [`Error::exit_code`]) so a supervisor can tell "already running"
    /// apart from a generic failure. Secret-free.
    #[error("another sessiometer daemon is already running (the single-instance lock is held)")]
    AlreadyRunning,

    // --- CLI status client (issue #8) ----------------------------------------
    /// `sessiometer status` could not reach a running daemon: the control socket
    /// is absent, or present but refusing connections (no live `run`). A friendly,
    /// user-facing remap of the raw connect failure — the live counterpart to
    /// [`Error::RosterEmpty`] for the offline `list` (#17) — that points the
    /// operator at the next step instead of leaking a connection error. Secret-free.
    #[error("daemon not running — start it with `sessiometer run`")]
    DaemonNotRunning,

    // --- Background service (`sessiometer service`, issues #166, #376) --------
    /// A `launchctl` invocation (`bootstrap` / `bootout`) while installing or
    /// uninstalling the LaunchAgent exited non-zero. The wrapped detail is the
    /// launchctl subcommand, its exit code, and its stderr — all non-secret (a
    /// label, a plist path, a domain target), so it is safe to surface verbatim.
    /// A generic failure exit `1` (via the `_` arm of [`Error::exit_code`]).
    #[error("launchctl failed: {0}")]
    LaunchctlFailed(String),

    /// No LaunchAgent is installed, and the verb needs one. Two callers, both with nothing
    /// managed to act on: `service status` (issue #376; the surviving `service` lifecycle verb
    /// after the #397 noun split), and `daemon restart` (issue #397) when no daemon is running
    /// either — nothing to restart, and no service to bring up.
    ///
    /// Surfaced as clear, FOLLOWABLE guidance: never a silent no-op, never a raw/confusing
    /// launchctl "Could not find service", and — since #397 — never the un-followable "Ctrl-C and
    /// re-run it" (a detached `run` has no controlling terminal to Ctrl-C). It routes to `service
    /// install` to enable a managed service, and names `run` / `daemon status` / `daemon stop` for
    /// the foreground case. The wording stays neutral about whether a daemon is *currently* running,
    /// because the two callers disagree on that. Generic failure exit `1` (via the `_` arm of
    /// [`Error::exit_code`]). Secret-free — names only non-secret commands.
    #[error(
        "no managed service installed — `sessiometer service install` enables auto-start at \
         login. Without one, a daemon runs only in the foreground: start it with `sessiometer \
         run`, inspect it with `sessiometer daemon status`, or stop it with `sessiometer daemon \
         stop`."
    )]
    NoManagedService,

    /// `daemon restart` (issue #397) was invoked against an UNMANAGED daemon — a foreground
    /// / detached `sessiometer run`. Nothing supervises a bare `run` to respawn it, so there
    /// is no clean automated restart (unlike a managed launchd agent, which `kickstart -k`
    /// kills and relaunches in one step). Surfaced as clear, FOLLOWABLE guidance — install a
    /// managed service for a supervised daemon with restart, or stop the current one and
    /// start a new `run` — never a raw error or a silent no-op. Generic failure exit `1`
    /// (via the `_` arm of [`Error::exit_code`]). Secret-free — names only non-secret commands.
    #[error(
        "can't restart an unmanaged daemon — nothing supervises a foreground `sessiometer \
         run` to respawn it. Install a managed service with `sessiometer service install` \
         for a supervised daemon with restart, or stop this one with `sessiometer daemon \
         stop` and start a new `sessiometer run`."
    )]
    UnmanagedDaemonNoRestart,

    // --- Manual account selection (`sessiometer use`, issue #63) -------------
    //
    // The one-shot `use <account>` verb's own exit conditions, EXTENDING the
    // existing taxonomy (no parallel scheme): a missing/unresolvable/ambiguous
    // target, a pre-swap gate refusal, and the always-enforced keychain-locked
    // abort (which now carries its own exit code — see [`Error::exit_code`]). All
    // are secret-free: each names only the operator's non-secret query/label
    // (issue #15), never a token or email.
    /// `sessiometer use` was invoked without the required `<account>`. There is
    /// deliberately no "cycle to the next account" fallback (out of scope, #63);
    /// a missing target is an error that names the usage.
    #[error("a target is required: `sessiometer use <account>`")]
    UseTargetRequired,

    /// `use <query>` matched no roster account by label OR account-uuid. The
    /// resolver never guesses (issue #17): an unresolvable target is a hard error
    /// with ZERO writes. `query` is the operator's non-secret input.
    #[error("no account matches `{query}` — run `sessiometer list` to see the roster")]
    UseTargetNotFound { query: String },

    /// `use <query>` matched MORE THAN ONE roster account (a duplicated label).
    /// The resolver refuses to guess (issue #17): disambiguate with the
    /// account-uuid. ZERO writes. `query` is the operator's non-secret input.
    #[error("`{query}` is ambiguous: {count} accounts match — disambiguate with the account-uuid")]
    UseTargetAmbiguous { query: String, count: usize },

    /// `use` (WITHOUT `--force`) could not identify the active account to swap AWAY
    /// from: the canonical keychain token matches no captured stash AND
    /// `~/.claude.json`'s logged-in `oauthAccount.accountUuid` matches no roster entry
    /// either (issue #207 resolves the active account token-first, with the display as
    /// the fallback). The normal swap re-stashes the outgoing account, so its roster
    /// identity must be known — mirrors the daemon's "can't identify active ⇒ don't
    /// swap". A LOCKED keychain does NOT surface here: it aborts earlier as
    /// [`KeychainLocked`](Self::KeychainLocked), never swallowed to this. With `--force`
    /// this instead becomes the adopt-target RECOVERY (issue #212) — the target is
    /// installed directly, no outgoing re-stash — so this error is the non-forced path
    /// only. ZERO writes. Secret-free.
    #[error(
        "cannot determine the active account to swap away from \
         (no logged-in account matches the roster — run `sessiometer login` to \
         re-authenticate and add it to the rotation, or `sessiometer use <account> \
         --force` to adopt a healthy account directly)"
    )]
    ActiveAccountUnresolved,

    /// The pre-swap gate REFUSED `use <label>` (without `--force`) because the
    /// target's WEEKLY window is exhausted (issue #11/#37 viability). ZERO writes;
    /// `--force` overrides. `label` is the target's non-secret handle.
    #[error(
        "refusing to swap to `{label}`: its weekly window is exhausted — use `--force` to override"
    )]
    UseTargetWeeklyExhausted { label: String },

    /// The pre-swap gate REFUSED `use` (without `--force`) because a swap COOLDOWN
    /// is currently active (issue #10 anti-oscillation). ZERO writes; `--force`
    /// overrides. Secret-free.
    #[error("refusing to swap: a swap cooldown is active — use `--force` to override")]
    UseCooldownActive,

    /// The pre-swap gate REFUSED `use <label>` (without `--force`) because the
    /// target is QUARANTINED — its stored ACCESS token was rejected (`401`/`403`), so
    /// it is out of rotation. NOT proven dead: a resource-server 401 never sees the
    /// refresh token, so the remedy is a refresh (`sessiometer poke`), not a re-login
    /// (issue #427). ZERO writes; `--force` overrides (warn-and-proceed). `label` is
    /// the target's non-secret handle.
    #[error(
        "refusing to swap to `{label}`: it is quarantined (out of rotation) — run `sessiometer poke` to refresh, or `--force` to override"
    )]
    UseTargetQuarantined { label: String },

    /// The pre-swap gate could not VERIFY `use <label>`'s viability (issue #75):
    /// with no daemon running to consult a CACHED reading, the single live fallback
    /// poll was rate-limited (`HTTP 429`). Distinct from the daemon-internal
    /// [`Error::UsageRateLimited`] so the operator gets actionable guidance instead
    /// of an opaque abort — start the daemon so the gate reads its cached verdict,
    /// or `--force` to swap anyway. This is an inability to RUN the gate, not a gate
    /// refusal, so it is NOT in the exit-`7` refusal taxonomy — a generic `1`, the
    /// same transient class the raw rate-limit mapped to before. ZERO writes;
    /// `label` is the target's non-secret handle (issue #15).
    #[error(
        "cannot verify `{label}`: the usage check was rate-limited (HTTP 429) and no \
         daemon is running to consult a cached reading — start it with `sessiometer run`, \
         or use `--force` to swap anyway"
    )]
    UseViabilityUnverifiable { label: String },

    // --- Single-writer swap lock (issue #64) ---------------------------------
    /// The single-writer swap lock (issue #64) could not be acquired within the
    /// bounded wait — another swap (a concurrent `use`, or the daemon's own swap
    /// routine) held it the whole time. The lock is FAIL-CLOSED: rather than write
    /// without it and risk a torn canonical/`~/.claude.json` pair, the swap ABORTS
    /// with ZERO writes. Maps to exit `4`, the same "could not write safely, retry
    /// shortly" class as [`Error::KeychainLocked`] (a locked keychain) — see
    /// [`Error::exit_code`]. Secret-free.
    #[error("another swap is in progress — could not acquire the swap lock; retry shortly")]
    SwapLockBusy,

    // --- Swap engine identity guard (issue #211) -----------------------------
    /// SAFETY ABORT: the out-of-band swap engine (#6) was about to re-stash the
    /// outgoing account, but the LIVE canonical credential belongs to the account
    /// being swapped TO — not the one being swapped away from. The caller resolved
    /// the outgoing account from a STALE `~/.claude.json` (its `oauthAccount` names
    /// an account that is no longer the active one), so re-stashing the live token
    /// under the outgoing account's stash key + identity would staple a DIFFERENT
    /// account's credential onto it, silently CORRUPTING that stash. Refused with
    /// ZERO writes — the guard fires before the engine mutates anything, mirroring
    /// the daemon's "never staple a different account's identity" (`restash_account`,
    /// `src/daemon.rs`). Secret-free: the mismatch is detected by comparing credential
    /// blobs, never by exposing either. A generic exit `1`, like its sibling
    /// precondition abort [`Error::ActiveAccountUnresolved`] — not a "retry shortly"
    /// (`4`) condition, since a stale display does not clear on its own. Reconcile
    /// with `sessiometer status` (or re-login) so `~/.claude.json` names the
    /// truly-active account.
    #[error(
        "refusing the swap: the active credential does not belong to the account being \
         swapped away from — re-stashing it would corrupt that account's stash \
         (reconcile with `sessiometer status`, or re-login)"
    )]
    SwapWrongIdentityRestash,

    /// The behavioral canary's pre-swap identity cross-check found DRIFT (issue
    /// #714): the resolved canonical credential byte-matches a DIFFERENT roster
    /// account's stash (`matched`) than the one Claude Code's own state names
    /// active (`displayed`) — evidence the #100 keychain derivation no longer
    /// points at the credential Claude Code is actually using. The credential
    /// WRITE is refused pre-mutation (ZERO writes; an atomic in-place `-U`
    /// overwrite of a drifted target would clobber an unrelated secret
    /// unrecoverably); reads / poll / `status` stay live. A generic exit `1`,
    /// like its engine-guard sibling [`Error::SwapWrongIdentityRestash`] — not a
    /// "retry shortly" (`4`) condition, since drift does not clear on its own.
    /// Carries only operator LABELS (issue #15), never a token, email, or
    /// account-uuid.
    #[error(
        "refusing the credential write: the keychain-identity canary detected drift — the \
         resolved credential belongs to `{matched}`, but Claude Code's state names `{displayed}` \
         active. Investigate with `sessiometer status`; if this is a false alarm, set \
         `canary_drift_override = true` under `[tunables]` in config.toml and restart the daemon"
    )]
    CanaryDrift {
        /// Label of the account `~/.claude.json` names active.
        displayed: String,
        /// Label of the account whose stashed token the canonical actually matches.
        matched: String,
    },

    // --- Daemon-routed swap (issue #167) -------------------------------------
    /// The running daemon performed a `use` swap on our behalf (issue #167 — `use`
    /// routes THROUGH the daemon when one is up) and its swap engine aborted for a
    /// reason other than the redacted-and-remapped ones (a locked keychain → exit
    /// `4`, a contended swap lock → exit `4`, a gone canonical → the recovery
    /// signal): a wrong-identity re-stash guard (#211), an absent stash, or an I/O
    /// error. The daemon aborted with ZERO writes. A generic exit `1`, like its
    /// sibling engine aborts. Secret-free: the daemon's ack is redacted to a machine
    /// reason code, never a token or email (issue #15).
    #[error("the daemon could not complete the swap; check `sessiometer status` and retry")]
    DaemonSwapFailed,

    // --- One-shot `poke` (issue #104) ----------------------------------------
    /// `poke <account>` named the ACTIVE account. The isolated-refresh engine
    /// refreshes only PARKED (non-active) accounts (`src/refresh.rs` Caller
    /// contract): a concurrent promotion of the refreshed account to active cannot
    /// be observed by the engine's CAS re-stash, so the active account is never a
    /// safe target. REFUSED with ZERO effect; `label` is the target's non-secret
    /// handle (issue #15). The all-accounts mode skips the active account silently
    /// instead — this fires only when an operator names it explicitly.
    #[error("refusing to poke `{label}`: it is the active account — poke only refreshes parked accounts")]
    PokeTargetActive { label: String },

    /// The `claude` binary the isolated refresh spawns (issue #102 step 4) could
    /// not be located: `$CLAUDE_BIN` is unset (or not an existing file) and no
    /// `claude` is on `$PATH`. Secret-free — a missing executable, never a
    /// credential.
    #[error(
        "could not locate the `claude` binary — install Claude Code so `claude` is on \
         your PATH, or set `$CLAUDE_BIN` to its absolute path"
    )]
    ClaudeBinaryNotFound,

    // --- Isolated interactive-login capture (issue #132) ----------------------
    /// The login-capture engine spawns `claude /login` inheriting the operator's
    /// terminal so the OAuth URL + prompts render directly to them (issue #132) —
    /// which requires a real TTY on stdout. When stdout is NOT a terminal (a pipe,
    /// a file, a CI runner), the engine ABORTS rather than allocate a mediated pty
    /// the operator could not interact with. Secret-free — a precondition failure,
    /// never a credential.
    #[error(
        "cannot capture an interactive login: stdout is not a terminal — run `capture-login` \
         from an interactive terminal (it drives `claude /login` on your own TTY)"
    )]
    LoginRequiresTty,

    /// SAFETY ALARM (issue #132): the shared `Claude Code-credentials` item that a
    /// live Claude Code session reads per-request changed DURING an isolated login
    /// capture — its baseline hash before the spawn no longer matches after. The
    /// isolation premise (the spawned `claude /login` writes ONLY the suffixed
    /// isolated item — `build/version-compat.md` #130) was violated, so the engine
    /// refuses to harvest and surfaces the breach loudly. Secret-free — the mutation
    /// is detected via non-secret sha256 hashes, never by exposing either blob.
    #[error(
        "aborting login capture: the shared `Claude Code-credentials` item changed during the \
         isolated login — refusing to proceed (the live session's credential must stay untouched)"
    )]
    SharedCredentialMutated,

    // --- Migration artifact format (issue #146) -------------------------------
    //
    // The versioned on-disk migration container's own parse/validation outcomes
    // (see [`crate::migration`]). All secret-free: a migration file carries an
    // account's credential + `oauthAccount` material, so — like the `~/.claude.json`
    // parser — these echo only a position or a static reason, never the bytes.
    /// The bytes are not a sessiometer migration artifact: the magic marker is
    /// absent or wrong. Secret-free.
    #[error("not a sessiometer migration artifact (missing or unrecognized magic)")]
    MigrationBadMagic,

    /// The migration artifact declares a `format_version` this build does not
    /// understand. The container structure is version-gated, so an unknown version
    /// is rejected up front rather than mis-parsed. Carries `found` vs `supported`
    /// (plain integers, never secrets).
    #[error("unsupported migration format version {found} (this build supports {supported})")]
    MigrationUnsupportedVersion { found: u16, supported: u16 },

    /// The migration artifact is not valid JSON (or a field has the wrong shape).
    /// Only the parser's `line`/`column` are carried — never the surrounding bytes,
    /// which may hold an account's credential / `oauthAccount` material (issue #15
    /// redaction), mirroring [`Error::ClaudeStateParse`].
    #[error("malformed migration artifact: JSON error at line {line} column {column}")]
    MigrationMalformed { line: usize, column: usize },

    /// The migration artifact parsed but violates a structural invariant (e.g. the
    /// `encrypted` flag disagrees with the body, or an encrypted artifact is missing
    /// its KDF/cipher parameters). The wrapped reason is a static, secret-free string.
    #[error("invalid migration artifact: {0}")]
    MigrationInvalid(&'static str),

    // --- Migration encryption envelope (issue #147) ---------------------------
    //
    // The optional passphrase-encryption layer's own outcomes (see
    // [`crate::migration`]). All secret-free: they carry no passphrase, no key, and
    // no plaintext — a decrypt failure never distinguishes wrong-passphrase from
    // tamper (no decryption oracle) and never echoes any byte.
    /// The passphrase supplied for encryption was EMPTY. Encrypt mode refuses this as
    /// a hard error — it must never silently fall back to plaintext, nor "encrypt"
    /// under an empty key. Secret-free.
    #[error("a passphrase is required — an empty passphrase is refused")]
    MigrationEmptyPassphrase,

    /// A migration artifact could not be encrypted (the AEAD refused, e.g. the payload
    /// exceeded the cipher's message limit). Carries no plaintext. Secret-free.
    #[error("could not encrypt the migration artifact")]
    MigrationEncryptFailed,

    /// Authentication FAILED while decrypting a migration artifact: a wrong passphrase,
    /// or a tampered / downgraded / truncated file. A single variant for all three so
    /// it is not a decryption oracle; ZERO plaintext is produced. Secret-free.
    #[error(
        "could not decrypt the migration artifact: wrong passphrase, or the file was \
         tampered with or truncated"
    )]
    MigrationDecryptFailed,

    /// The migration artifact's KDF / cipher parameters are unsupported or malformed —
    /// an unrecognized algorithm, an out-of-range Argon2 cost, or a wrong-length nonce.
    /// A static, secret-free reason; never the parameter bytes.
    #[error("unsupported or malformed migration crypto parameters: {0}")]
    MigrationCryptoParams(&'static str),

    // --- Migration import (issue #149) ----------------------------------------
    //
    // The `import` verb's own outcomes (see [`crate::cli`]). All secret-free: they
    // carry a count or a static reason, never an account label, token, or email.
    /// `import` was invoked without the required `<file>` argument. The artifact path
    /// is mandatory — the passphrase may ride stdin (`--passphrase-stdin`), so the
    /// artifact itself is never read from stdin. Secret-free.
    #[error("import requires a migration artifact path: sessiometer import <file>")]
    MigrationImportPathRequired,

    /// An imported credential failed READ-BACK verification: the stash was written but
    /// a re-read did not hash-match what was written (a locked keychain at read-back, or
    /// a store that did not persist the bytes). The account is reported `failed` and left
    /// out of the roster rather than claimed as imported. Carries no bytes — only the
    /// hashes are compared, never logged. Secret-free.
    #[error("an imported credential failed read-back verification")]
    MigrationImportVerifyFailed,

    /// One or more accounts could not be imported (a write or read-back failure). The
    /// successfully-imported accounts were still committed to the roster (honest partial
    /// result); this non-zero exit surfaces the failure loudly for a caller/script. The
    /// per-account report names which landed and which failed. Carries only the failed
    /// COUNT — no label, token, or email. Secret-free.
    #[error("{failed} account(s) could not be imported — see the per-account report above")]
    MigrationImportIncomplete { failed: usize },

    // --- Usage-sample datastore (issue #155) ----------------------------------
    //
    // The local usage-sample store's own outcomes (see [`crate::usage_store`]).
    // Both secret-free: the store holds only percentages, epoch timestamps and
    // redacted handles (never a token or email), so neither can carry one.
    /// A usage-store record or rollup could not be serialized to JSON — reachable
    /// only for a non-finite float in a usage fraction/spend, which JSON cannot
    /// represent. The payload is a static, secret-free hint.
    #[error("could not serialize a usage-store record: {0}")]
    UsageStoreSerialize(&'static str),

    /// The usage-rollup file is not valid JSON. Only the parser's `line`/`column`
    /// is carried — never the surrounding bytes (issue #15 redaction discipline,
    /// mirroring [`Error::MigrationMalformed`]); secret-free regardless, since the
    /// store holds no secret.
    #[error("malformed usage rollup: JSON error at line {line} column {column}")]
    UsageRollupMalformed { line: usize, column: usize },

    /// The single-writer store lock (issue #188) could not be acquired within the
    /// bounded wait — a concurrent [`append_sample`](crate::usage_store::append_sample)
    /// / [`compact_and_roll`](crate::usage_store::compact_and_roll) held it the whole
    /// time. FAIL-CLOSED: rather than write
    /// without it and race a torn read-modify-rewrite of the raw sample file, the
    /// store operation ABORTS with ZERO writes. Both producers are fail-open (the
    /// daemon logs and skips a busy store, never breaking the poll loop), so this is
    /// swallowed telemetry in practice. Maps to exit `4`, the same "could not write
    /// safely, retry shortly" class as [`Error::SwapLockBusy`]. Secret-free.
    #[error("the usage store is busy — could not acquire the store lock; retry shortly")]
    UsageStoreBusy,

    /// An underlying I/O failure.
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

impl Error {
    /// The process exit code for this error.
    ///
    /// A held single-instance lock exits `3` ([`Error::AlreadyRunning`], issue
    /// #7) so a second `run` is distinguishable from a generic failure (`1`). The
    /// one-shot `use` verb (issue #63) EXTENDS this same taxonomy — no parallel
    /// scheme — so a caller (or supervisor) can tell its distinct outcomes apart:
    /// a locked keychain (`4`, the always-enforced abort), an unresolvable (`5`)
    /// or ambiguous (`6`) target, and a pre-swap gate refusal without `--force`
    /// (`7`). Every other error is a generic failure (`1`). The mapping lives here
    /// so the `main` exit-code branch stays a thin lookup.
    pub(crate) fn exit_code(&self) -> u8 {
        match self {
            Error::AlreadyRunning => 3,
            // A locked keychain, a contended swap lock (issue #64) AND a contended
            // usage-store lock (issue #188) share exit `4`: all are the "could not
            // write safely right now, retry shortly" class — each aborted with ZERO
            // writes rather than tear state.
            Error::KeychainLocked { .. } | Error::SwapLockBusy | Error::UsageStoreBusy => 4,
            Error::UseTargetNotFound { .. } => 5,
            Error::UseTargetAmbiguous { .. } => 6,
            // The pre-swap gate refused without `--force` — weekly-exhausted,
            // cooldown, or quarantined all share one "gate-refused" code, each
            // with its own specific message.
            Error::UseTargetWeeklyExhausted { .. }
            | Error::UseCooldownActive
            | Error::UseTargetQuarantined { .. } => 7,
            _ => 1,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn already_running_exits_three_so_a_supervisor_can_tell_it_apart() {
        // The "second `run` exits 3" acceptance (issue #7): a held single-instance
        // lock maps to exit code 3, distinct from a generic failure.
        assert_eq!(Error::AlreadyRunning.exit_code(), 3);
    }

    #[test]
    fn every_other_error_is_a_generic_failure() {
        assert_eq!(Error::CredentialNotFound.exit_code(), 1);
        assert_eq!(Error::Unimplemented("x").exit_code(), 1);
        assert_eq!(Error::Io(std::io::Error::other("boom")).exit_code(), 1);
        // A launchctl install/uninstall failure (issue #166) is a generic failure —
        // it does not touch the swap/lock taxonomy (2–7).
        assert_eq!(Error::LaunchctlFailed("boom".to_owned()).exit_code(), 1);
        // A lifecycle verb run with no LaunchAgent installed (issue #376) is a
        // generic failure — non-zero so the verb is never a silent no-op, but it does
        // not touch the swap/lock taxonomy (2–7).
        assert_eq!(Error::NoManagedService.exit_code(), 1);
        // `daemon restart` against an unmanaged daemon (issue #397) is a generic failure —
        // non-zero so the verb is never a silent no-op, outside the swap/lock taxonomy (2–7).
        assert_eq!(Error::UnmanagedDaemonNoRestart.exit_code(), 1);
        // A strict-usage rejection (issue #175) is a generic failure, matching the
        // sibling `UnknownCommand` — both are "you asked for something that isn't a
        // thing", distinct from a runtime failure.
        assert_eq!(
            Error::CliUsage {
                message: "unknown flag `--forc`".to_owned(),
                usage_hint: "sessiometer use --help",
            }
            .exit_code(),
            1
        );
        assert_eq!(
            Error::UnknownCommand("frobnicate".to_owned()).exit_code(),
            1
        );
    }

    #[test]
    fn no_managed_service_guides_the_operator_instead_of_a_raw_launchctl_error() {
        // Issue #376 + #397 AC: `service status` with no installed agent yields CLEAR,
        // FOLLOWABLE guidance — it names the enable path (`service install`) and routes an
        // unmanaged (`sessiometer run`) daemon to the `daemon` lifecycle verbs — never a
        // bare/confusing launchctl "Could not find service", and never the un-followable
        // "Ctrl-C and re-run it" a detached `run` cannot obey (the #397 guidance fix).
        let message = Error::NoManagedService.to_string();
        assert!(
            message.contains("no managed service"),
            "leads with the diagnosis: {message}",
        );
        assert!(
            message.contains("sessiometer service install"),
            "points at the install/enable recovery path: {message}",
        );
        assert!(
            message.contains("sessiometer daemon status")
                && message.contains("sessiometer daemon stop"),
            "routes an unmanaged daemon to the `daemon` lifecycle verbs: {message}",
        );
        assert!(
            !message.to_lowercase().contains("ctrl-c"),
            "drops the un-followable Ctrl-C advice (#397 guidance fix): {message}",
        );
    }

    #[test]
    fn unmanaged_daemon_no_restart_guides_the_operator_with_a_followable_action() {
        // Issue #397 AC: `daemon restart` against an unmanaged (foreground `run`) daemon
        // returns a CLEAR, ACTIONABLE error — it explains nothing supervises a bare `run` to
        // respawn it and points at `service install` for a managed daemon with restart (and at
        // `daemon stop` + a fresh `run` as the manual path) — never a raw launchctl error.
        let message = Error::UnmanagedDaemonNoRestart.to_string();
        assert!(
            message.contains("unmanaged daemon"),
            "names the condition: {message}",
        );
        assert!(
            message.contains("sessiometer service install"),
            "points at the managed-service recovery path: {message}",
        );
        assert!(
            message.contains("sessiometer daemon stop"),
            "offers the manual stop-and-rerun path: {message}",
        );
    }

    #[test]
    fn a_contended_swap_lock_shares_the_locked_keychain_exit_code() {
        // Issue #64: a fail-closed swap-lock abort joins the locked keychain in
        // exit `4` — both are the "could not write safely now, ZERO writes, retry
        // shortly" class, distinct from a generic failure (`1`).
        assert_eq!(Error::SwapLockBusy.exit_code(), 4);
        assert_eq!(
            Error::SwapLockBusy.exit_code(),
            Error::KeychainLocked { op: "write" }.exit_code(),
        );
    }

    #[test]
    fn a_busy_usage_store_shares_the_retry_shortly_exit_code() {
        // Issue #188: a fail-closed usage-store-lock abort joins the locked keychain
        // and the swap lock in exit `4` — the "could not write safely now, ZERO
        // writes, retry shortly" class, distinct from a generic failure (`1`).
        assert_eq!(Error::UsageStoreBusy.exit_code(), 4);
        assert_eq!(
            Error::UsageStoreBusy.exit_code(),
            Error::SwapLockBusy.exit_code(),
        );
    }

    #[test]
    fn use_verb_extends_the_exit_code_taxonomy_with_distinct_codes() {
        // Issue #63: the `use` verb's new conditions each get their own code,
        // extending the existing taxonomy (no parallel scheme) so a caller can
        // tell them apart from a generic failure (`1`) and from each other.
        assert_eq!(Error::KeychainLocked { op: "read" }.exit_code(), 4);
        assert_eq!(
            Error::UseTargetNotFound {
                query: "ghost".into()
            }
            .exit_code(),
            5
        );
        assert_eq!(
            Error::UseTargetAmbiguous {
                query: "dup".into(),
                count: 2
            }
            .exit_code(),
            6
        );
        // The three gate-refusal reasons share one "gate-refused-without-force" code.
        assert_eq!(
            Error::UseTargetWeeklyExhausted {
                label: "spare".into()
            }
            .exit_code(),
            7
        );
        assert_eq!(Error::UseCooldownActive.exit_code(), 7);
        assert_eq!(
            Error::UseTargetQuarantined {
                label: "spare".into()
            }
            .exit_code(),
            7
        );
        // A missing argument and an unresolvable active account are precondition
        // errors, not part of the named new taxonomy → generic `1`.
        assert_eq!(Error::UseTargetRequired.exit_code(), 1);
        assert_eq!(Error::ActiveAccountUnresolved.exit_code(), 1);
    }

    #[test]
    fn use_verb_error_messages_carry_no_secret_sigil() {
        // Issue #15: every `use` error names only the operator's non-secret
        // query/label, never a token or email.
        let messages = [
            Error::UseTargetRequired.to_string(),
            Error::UseTargetNotFound {
                query: "ghost".into(),
            }
            .to_string(),
            Error::UseTargetAmbiguous {
                query: "dup".into(),
                count: 2,
            }
            .to_string(),
            Error::ActiveAccountUnresolved.to_string(),
            Error::UseTargetWeeklyExhausted {
                label: "spare".into(),
            }
            .to_string(),
            Error::UseCooldownActive.to_string(),
            Error::UseTargetQuarantined {
                label: "spare".into(),
            }
            .to_string(),
            Error::UseViabilityUnverifiable {
                label: "spare".into(),
            }
            .to_string(),
        ];
        for message in messages {
            assert!(!message.contains('@'), "no email: {message}");
            assert!(
                !message.to_lowercase().contains("token"),
                "no token: {message}"
            );
        }
    }

    #[test]
    fn label_bearing_errors_carry_an_authored_email_label_but_flag_an_unauthored_one() {
        // #444/#447: the label-bearing errors quote the account's roster label, which
        // MAY now be an operator-authored email (the capture prompt pre-fills it). The
        // handle-fixture tests above stay green because their labels are handles; this
        // guards the email-label case directly — an authored email label is PERMITTED
        // (it is the operator's own value, shown back to them), while an UNAUTHORED
        // email spilled into the same message would still be caught. Provenance-scoped,
        // consistent with the render/event/store channels (see
        // `redaction::meter::unauthored_emails`).
        let authored = "alice@example.com";
        for message in [
            Error::AccountLabelNotFound {
                label: authored.into(),
            }
            .to_string(),
            Error::UseTargetQuarantined {
                label: authored.into(),
            }
            .to_string(),
        ] {
            // The authored email label IS quoted in the operator-facing message…
            assert!(message.contains(authored), "label is quoted: {message}");
            // …and permitted WHEN authored…
            assert!(
                crate::redaction::meter::unauthored_emails(&message, &[authored]).is_empty(),
                "an operator-authored email label is permitted: {message}"
            );
            // …but the same shape reads as a leak WITHOUT the provenance allow-set
            // (the assertion is not vacuous — the message really does carry an `@`).
            assert_eq!(
                crate::redaction::meter::unauthored_emails(&message, &[]),
                vec![authored.to_owned()],
                "without provenance the label reads as an unauthored email: {message}"
            );
        }
    }

    #[test]
    fn active_account_unresolved_names_an_actionable_recovery_not_a_viewer() {
        // Issue #210: when `use` cannot identify the active account to swap away from,
        // the message must point the operator at a REAL recovery next step —
        // `sessiometer login` re-authenticates and lands the account in the rotation —
        // NOT the read-only `sessiometer list` viewer, which fixes nothing precisely
        // when the ACTIVE account is the one missing from the roster.
        let message = Error::ActiveAccountUnresolved.to_string();
        assert!(
            message.contains("sessiometer login"),
            "must name the actionable recovery verb: {message}"
        );
        assert!(
            !message.contains("sessiometer list"),
            "must not point at the read-only viewer: {message}"
        );
    }

    #[test]
    fn wrong_identity_restash_is_a_secret_free_generic_abort() {
        // Issue #211: the swap engine's identity guard is a precondition safety abort
        // like `ActiveAccountUnresolved` — a generic exit `1` (NOT the "retry shortly"
        // `4` class, since a stale display does not clear on its own), and secret-free
        // (no token / email in the message — the mismatch is found by comparing blobs).
        assert_eq!(Error::SwapWrongIdentityRestash.exit_code(), 1);
        let message = Error::SwapWrongIdentityRestash.to_string();
        assert!(!message.contains('@'), "no email: {message}");
        assert!(
            !message.to_lowercase().contains("token"),
            "no token: {message}"
        );
    }
}
