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
///
/// # Adding a variant: its message is inside the FRAMING firewall (issue #1139)
///
/// `src/main.rs` prints whatever reaches it as `sessiometer: {err}`, so a variant's `#[error(...)]`
/// message is operator-facing prose of the same register as `--help` (issue #918) and the `status`
/// advisories (issue #1123) — and it is scanned as such. The SUBJECT is the `#[error(...)]`
/// ATTRIBUTE, which is narrower than "what this crate wrote": a template holds `{query}`, never the
/// operator's query, and `{0}`, never the TOML parser's sentence — but a crate-authored string built
/// at a CONSTRUCTION site and handed in as a payload (`Error::ConfigInvalid(format!(…))`) is equally
/// outside it, though this crate wrote that too. [`Error::CliUsage`] below draws a WIDER seam: it
/// renders through the real parser with neutral argv, so its construction-site prose IS scanned and
/// only the operator's echoed argv is out. Issue #1152 tracks this residual.
///
/// Adding a variant opts its TEMPLATE in with nothing further needed: `thiserror` will not compile
/// one without an `#[error(...)]`, so the guard's walk over the attributes is a walk over the
/// variants. Prose you build at the call site and pass in as a payload is NOT covered. What that
/// means in practice is that a message spending the editorialising vocabulary of issue #160 — a
/// value judgement, an acquisitive imperative, a recommendation, an alarmist projection — reddens
/// `every_error_template_carries_no_banned_framing_beyond_the_pinned_ledger` on first run.
///
/// That is a DECISION to make, not a lint to silence. There is deliberately no
/// `ERROR_EXEMPT_TOKENS` set to add to: one would excuse the token across every message here, so
/// carve-outs are per-(variant, token) in `ERROR_PROSE_LEDGER`, each with its reasoning. Judge the
/// token the way those entries do, then either reword the message or add an entry. See
/// `crate::framing_vocabulary`'s module doc for the boundary itself (test-only, so rustdoc does
/// not build it — read the source).
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
    ///
    /// **FRAMING firewall: the AUTHORED half is IN SCOPE; interpolated argv is OUT** (issue
    /// #1123). This variant is the one operator-facing surface whose rendered text is not wholly
    /// ours, so the verdict splits along that seam rather than covering or skipping the variant
    /// whole:
    ///
    /// - **In scope** — everything this crate wrote: each `message` TEMPLATE, every `usage_hint`,
    ///   and the `run … for usage` wrapper below. `src/cli.rs`'s
    ///   `the_usage_prose_carries_no_banned_framing_but_the_guard_bites_on_injection` drives every
    ///   construction site through the real parser with NEUTRAL argv and scans the rendered
    ///   `Display` against `crate::framing_vocabulary::scan_usage_banned`. The exemption it
    ///   allows is `USAGE_EXEMPT_TOKENS` — `disable` / `enable` / `remove`, earned solely because
    ///   a hint whose job is to name the command to run must spell that command.
    /// - **Out of scope** — the operator's own argv, echoed back through the flag or action name.
    ///   `sessiometer status --should` renders "unknown flag `--should`", and that `should` is
    ///   the operator quoting themselves, not this tool editorialising: the issue #160 firewall
    ///   governs what the tool ASSERTS, and the assertion here is "I do not recognise this",
    ///   which is neutral whatever the flag was called. Eliding or sanitising the echoed token
    ///   would corrupt the one diagnostic the message exists to deliver, so the guard is pointed
    ///   at the template rather than at live output.
    ///   `src/cli.rs`'s `the_argv_echo_is_the_operators_words_not_this_tools_framing` pins that
    ///   boundary, so it is a recorded decision rather than a gap someone later "fixes".
    ///
    /// The `From<lexopt::Error>` path is third-party prose rather than ours, and is scanned
    /// anyway — we ship it, so we own how it reads even though we did not write it.
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

    /// `log --since` got a value that is not a relative duration — a non-negative integer with
    /// a unit `s`/`m`/`h`/`d`/`w` (e.g. `30m`, `24h`, `7d`, `2w`). Duration-only, exactly like
    /// `reliability --since` (both resolve through [`crate::duration::parse_duration_secs`]);
    /// an absolute date is not accepted here. Distinct from the `reliability` variant so the
    /// message names the verb whose flag the operator actually mistyped (issue #773).
    #[error(
        "invalid --since `{0}`: expected a relative duration (e.g. 30m, 24h, 7d, 2w — units s/m/h/d/w)"
    )]
    LogSinceInvalid(String),

    /// The `log --json` view could not be serialized. Unreachable — the wire is the durable
    /// lines plus bare integers and strings (issue #773); mapped, never panicked.
    #[error("could not render the log view as JSON: {0}")]
    LogSerialize(&'static str),

    /// `log --channel` got a value that is not one of the three (issue #775). The accepted set
    /// is closed and small, so the message enumerates it rather than pointing at `--help`.
    #[error("invalid --channel `{0}`: expected one of event, diag, all")]
    LogChannelInvalid(String),

    /// `log --follow --channel all` (issue #775). Refused rather than approximated: ordering a
    /// live merge means holding each new line back until the other channel has produced one at
    /// least as late, which on a quiet channel never happens — so the only options are stalling
    /// a stream or emitting out of order.
    #[error(
        "--follow cannot merge both channels: ordering a live merge would have to stall one \
         stream waiting for the other. Follow one at a time (--channel event or --channel diag); \
         `--channel all` works without --follow."
    )]
    LogFollowAllUnsupported,

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
    /// database (`getpwuid(getuid())->pw_name`, see [`crate::paths`]).
    ///
    /// The login name is the FALLBACK source for the keychain item's `acct`, not the
    /// `acct` itself — Claude Code's own derivation prefers `$USER` (issue #711) — so
    /// the isolated-refresh engine (issue #102) does NOT abort on this: it mirrors
    /// CC's `catch` arm and substitutes the literal `claude-code-user`, exactly as a
    /// live CC would. Constructed only by `paths::username`, whose callers today all
    /// choose that fallback over propagating.
    #[error("could not resolve the login name for the current user")]
    UserUnresolved,

    /// The current user's login shell could not be resolved from the password
    /// database (`getpwuid(getuid())->pw_shell`, see [`crate::paths`]): the entry is
    /// absent, or does not name an absolute path — empty (a `nologin`-class account
    /// names nothing to execute) or relative (which `Command` would resolve against
    /// the very `PATH` the harvest exists to reconstruct) — so there is no shell to
    /// run (issue #783). Also raised at the exec boundary, before any spawn is
    /// attempted: a non-absolute `pw_shell` is a passwd-entry problem, never a failed
    /// harvest.
    ///
    /// Non-fatal by contract, like [`Error::LoginShellPathUnharvested`] below: the
    /// resolution caller records it and retries next cycle rather than permanently
    /// disabling the tick (the issue #375 stale-path contract).
    #[error("could not resolve the login shell for the current user")]
    LoginShellUnresolved,

    /// The user-level `PATH` could not be harvested by running the login shell
    /// (issue #783) — it could not be spawned, exited non-zero (a `nologin`-class
    /// shell that refuses `-l -c`), outran the harvest timeout, or produced no usable
    /// `PATH=` line.
    ///
    /// `shell` is a filesystem location and `reason` a `&'static str`, so the crate's
    /// "an [`Error`] never carries secret material" invariant holds by construction:
    /// the child's output is the user's whole environment and is therefore NEVER
    /// embedded here, only classified.
    ///
    /// One variant for every harvest failure mode on purpose — the caller's contract
    /// is a single non-fatal degrade (fall back to the ambient `PATH`, record, retry
    /// next cycle), so it should match once rather than enumerate causes.
    #[error("could not harvest the user-level PATH by running the login shell {shell}: {reason}")]
    LoginShellPathUnharvested {
        shell: PathBuf,
        reason: &'static str,
    },

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
    ///
    /// FRAMING (issue #1139): the `must` here is a CONSTRAINT STATEMENT — the modal's subject is a
    /// config VALUE, so it cites the schema rule the value broke rather than advising the operator
    /// — and is carved out for this variant alone in `ERROR_PROSE_LEDGER`. Editing this message
    /// away from that reading, or dropping the word, means updating that entry.
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

    /// The logged-in account's `oauthAccount` carries a `field` that is PRESENT but
    /// malformed — the value counterpart of [`Error::OauthAccountFieldMissing`], raised
    /// when `capture`/`login` would key the roster on it (issue #1052).
    ///
    /// Distinct from [`Error::ConfigInvalid`] on purpose: on this path the offending value
    /// came from `~/.claude.json`, not `config.toml`, and reporting it as an invalid config
    /// would send the operator to a file that is not at fault — and on a first capture, to
    /// one that does not exist yet. `rule` is the constraint that failed; it echoes the
    /// offending value escaped, exactly as the parse-side rejection does. That is within
    /// the issue #15 redaction discipline this variant's sibling states in terms of field
    /// names: an `account_uuid` is an account identifier, not credential material.
    #[error("the logged-in account's `{field}` is malformed: it {rule} — cannot key the roster")]
    OauthAccountFieldMalformed { field: &'static str, rule: String },

    // --- Account enable/disable (issue #36) ----------------------------------
    /// `sessiometer disable`/`enable`/`remove` was invoked without the required
    /// `<account>`. Carries the subcommand (a static, secret-free string) so the message
    /// names the exact usage.
    ///
    /// Says `<account>`, not `<label>`: since issue #1005 these verbs resolve through
    /// [`resolve_target`](crate::use_account::resolve_target) and accept a label OR an
    /// account-uuid, so naming a label as *required* would both contradict their `--help` and
    /// be untrue. The variant keeps its `Rotation…` name — renaming a variant nothing else
    /// reads would churn call sites without changing a word the operator sees.
    #[error("an account is required: `sessiometer {verb} <account>`")]
    RotationLabelRequired { verb: &'static str },

    // `AccountLabelNotFound` lived here until issue #1005. `disable`/`enable`/`remove`
    // resolved a label with their own exact-match `.find()`/`.position()` and raised it when
    // nothing matched — a private taxonomy that also silently took the FIRST match on a
    // duplicated label, while `use`/`poke`/the daemon refused. OQ-1 settled that toward
    // refusing, so all six now resolve through `use_account::resolve_target` and the not-found
    // case is [`UseTargetNotFound`](Error::UseTargetNotFound) (exit 5), the ambiguous one
    // [`UseTargetAmbiguous`](Error::UseTargetAmbiguous) (exit 6). Retiring the variant is
    // forced rather than chosen: with both constructors routed away, a never-constructed
    // variant is `dead_code`, which is a build failure under `-D warnings`.
    /// A `config-set` (issue #268) label edit named an `account_uuid` that matches no
    /// roster account — a stale settings client (the account was `remove`d between its
    /// `config-get` read and the edit) or a client bug. The uuid is a non-secret roster
    /// key (issue #15), safe to quote. Distinct from
    /// [`UseTargetNotFound`](Error::UseTargetNotFound) (a `<account>` lookup, which resolves
    /// by label OR uuid): the settings path keys label edits by the immutable uuid ALONE, so
    /// it stays unambiguous on a duplicate-label roster where the shared resolver refuses.
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
    /// `sessiometer use` was invoked with neither an `<account>` nor `--next`. There is
    /// deliberately no IMPLICIT "cycle to the next account" fallback (out of scope, #63):
    /// a bare `use` still names the usage rather than guessing. Issue #960 makes that
    /// advance OPT-IN as `--next` — which resolves the target from the daemon's own
    /// published `next_swap` — so this error now names both ways to supply one.
    #[error("a target is required: `sessiometer use <account>`, or `--next` to advance to the daemon's own next-swap candidate")]
    UseTargetRequired,

    /// `use --next` (issue #960) could not ask the daemon which account comes next: no
    /// daemon is reachable. A NAMED target still works with no daemon (the standalone
    /// write path), but `--next` CANNOT — the candidate it advances to is the daemon's
    /// published `next_swap`, and a client cannot re-derive it (the session trigger /
    /// floor `pick_target` consumes are daemon-only, never on the wire). So `--next`
    /// fails CLEANLY here rather than falling through to the standalone write path with
    /// a guessed or unresolved target. ZERO writes.
    ///
    /// Generic exit `1`: an inability to RUN the resolution, not a gate refusal, so it
    /// stays out of the exit-`7` refusal taxonomy — the same distinction
    /// [`Error::UseViabilityUnverifiable`] draws for the un-runnable viability gate.
    /// Secret-free.
    #[error(
        "`--next` needs a running daemon to know which account comes next — \
         start it with `sessiometer run`, or name the target: `sessiometer use <account>`"
    )]
    UseNextRequiresDaemon,

    /// `use --next` (issue #960) asked the daemon for its next-swap candidate and the
    /// daemon answered that there ISN'T one: `pick_target` excluded every account
    /// (weekly-exhausted #11/#37, session-saturated, quarantined #42, disabled #36, or
    /// inside the weekly tail-margin band). `detail` carries the daemon's own #405
    /// fleet-capacity RELIEF hint — WHEN capacity returns, and whether the shortage is
    /// structural enough to warrant adding an account — composed by the SAME
    /// [`crate::cli::out_of_capacity_phrase`] the `status` next-swap footer renders, so
    /// the two surfaces can never drift on the relief instant or the nudge threshold. A
    /// pre-#405 daemon carries no hint, and `detail` then degrades to the bare "no viable
    /// target" that footer also falls back to. ZERO writes; secret-free (a duration and a
    /// fixed phrase, never a label — issue #15).
    ///
    /// Exit `7`, the gate-refused class: this is the FLEET-WIDE instance of exactly the
    /// per-target refusals that already own `7` (weekly-exhausted / quarantined /
    /// cooldown). The gate RAN and refused — which is what `7` means — and a supervisor
    /// re-running `use --next` on a timer needs "no capacity, back off" distinguishable
    /// from a generic `1`. Unlike its exit-`7` siblings, `--force` does NOT override it:
    /// there is no target to force ONTO. The remedy is to name one.
    #[error(
        "refusing to swap: {detail} — name a target to override: `sessiometer use <account> --force`"
    )]
    UseNextNoViableTarget { detail: String },

    /// `use --next` (issue #960) reached the daemon but could not get an answer it can
    /// act on: the staggered poll loop (#80) has not read the rotation yet
    /// (`next_swap: awaiting_data`), the daemon published no candidate at all (no active
    /// account to anchor a swap FROM, or a pre-#88 daemon that omits the field), or the
    /// exchange itself failed (a timeout, or a reply this build cannot read — including
    /// a daemon whose contract MAJOR has moved on, issue #164). `detail` names which.
    /// Every case fails CLOSED with ZERO writes: `--next` never guesses a target.
    ///
    /// Generic exit `1`: an inability to RUN the resolution, not a gate refusal, so it
    /// stays out of the exit-`7` refusal taxonomy — the same distinction
    /// [`Error::UseViabilityUnverifiable`] draws for the un-runnable viability gate.
    /// Secret-free: `detail` is one of a fixed set of authored phrases (issue #15).
    #[error(
        "cannot pick the next account: {detail} — retry shortly, or name the target: \
         `sessiometer use <account>`"
    )]
    UseNextUnresolved { detail: String },

    /// A `<query>` matched no roster account by label OR account-uuid. The
    /// resolver never guesses (issue #17): an unresolvable target is a hard error
    /// with ZERO writes. `query` is the operator's non-secret input.
    ///
    /// Raised by every site that resolves an operator-supplied account: `use`, `poke`, the
    /// daemon's control-socket swap, and — since issue #1005 routed them through the shared
    /// [`resolve_target`](crate::use_account::resolve_target) — `enable`, `disable` and
    /// `remove`. Those three previously raised their own `AccountLabelNotFound`, which carried
    /// no entry in [`exit_code`](Error::exit_code) and so exited a generic `1`; they now exit
    /// `5` with the rest.
    #[error("no account matches `{query}` — run `sessiometer list` to see the roster")]
    UseTargetNotFound { query: String },

    /// A `<query>` matched MORE THAN ONE roster account (a duplicated label).
    /// The resolver refuses to guess (issue #17): disambiguate with the
    /// account-uuid. ZERO writes. `query` is the operator's non-secret input.
    ///
    /// Raised by the same six sites as [`UseTargetNotFound`](Error::UseTargetNotFound). For
    /// `enable`, `disable` and `remove` this outcome did not exist before issue #1005 — each
    /// silently took the earliest bearer of the label, which on `remove` meant deleting a
    /// keychain stash the operator had not named. That is the harm OQ-1 resolved by routing
    /// them here.
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
    ///
    /// FRAMING (issues #1139 / #1151): `add it to the rotation` is the permitted non-acquisitive
    /// remedy directive (ADR-0020 § Status → Amended 2026-08-10), carved out for this variant
    /// alone in `ERROR_PROSE_LEDGER`. This message also read `adopt a **healthy** account`, which
    /// issue #1139 graded a VALUE JUDGEMENT and ledgered as the firewall's one violation: the path
    /// it points at computes no health at all — `crate::use_account` never imports
    /// [`CredentialHealth`](crate::observability::CredentialHealth), and the only check `--force`
    /// runs is `warn_if_forcing_onto_non_viable`, which warns and PROCEEDS on weekly `Viability`
    /// — so the word gated nothing and could not be read as naming the enum.
    ///
    /// Issue #1151 DROPPED the adjective rather than replacing it, and that is worth stating
    /// because the obvious repair is to substitute a better word. A selection adjective here has
    /// to name a property that DISCRIMINATES between roster accounts, and none is available.
    /// Health is daemon-computed, and this error fires when the canonical credential was scrubbed
    /// — which is also when no daemon may be running, so the `status` screen that would name it
    /// answers `daemon not running` and the operator cannot look the word up. Weekly `Viability`
    /// is no better: it is warn-only on this path, so `viable` would be false in exactly the way
    /// `healthy` was. The reason is structural rather than incidental — NOT discriminating is
    /// what `--force` IS: it adopts whatever is named, in ANY state, `Dead` included. So any word
    /// telling the operator how to CHOOSE necessarily asserts something this path does not
    /// compute, and the honest message names the command and stops.
    ///
    /// What `--force` really requires of the target is two things, both daemon-independent and
    /// both enforced: it resolves in the LOCAL roster (`resolve_target`, else
    /// [`UseTargetNotFound`](Self::UseTargetNotFound) or
    /// [`UseTargetAmbiguous`](Self::UseTargetAmbiguous)), and its stash is readable
    /// (`crate::swap::adopt_target` reads it first, aborting with ZERO writes). Neither narrows
    /// the operator's choice — every account they would consider has both — so neither earns a
    /// word here either.
    #[error(
        "cannot determine the active account to swap away from \
         (no logged-in account matches the roster — run `sessiometer login` to \
         re-authenticate and add it to the rotation, or `sessiometer use <account> \
         --force` to adopt that account directly)"
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

    /// The behavioral canary's pre-swap cross-check found the resolved canonical
    /// credential matches NO account stash AND does not parse as a Claude Code
    /// credential (issue #730): overwhelmingly an UNRELATED secret under the derived
    /// service (a future CC storage-format change), NOT Claude Code's own item. The
    /// credential WRITE is refused pre-mutation (ZERO writes; an atomic in-place `-U`
    /// overwrite would clobber that secret unrecoverably) unless the operator set the
    /// dedicated `canary_nostashmatch_override` (separate from `canary_drift_override`);
    /// reads / poll / `status` stay live. A well-formed unmatched canonical (a benign
    /// in-place refresh) never reaches here — it fails OPEN. A generic exit `1`, like
    /// its sibling [`Error::CanaryDrift`] — not a "retry shortly" (`4`) condition,
    /// since the shape mismatch does not clear on its own. Secret-free (issue #15): no
    /// token bytes, no credential content.
    #[error(
        "refusing the credential write: the keychain-identity canary found the active canonical \
         credential matches no account stash and does not parse as a Claude Code credential — an \
         atomic in-place overwrite would clobber an unrelated secret unrecoverably. Investigate \
         with `sessiometer status`; if this canonical is a legitimate new Claude Code credential \
         format, set `canary_nostashmatch_override = true` under `[tunables]` in config.toml and \
         restart the daemon"
    )]
    CanaryUnparseableCanonical,

    /// The behavioral canary's opt-in Layer-3 ONLINE liveness probe (issue #736) did
    /// not confirm the resolved canonical credential still authenticates, and the
    /// operator had armed `canary_online_probe_strict`. The credential WRITE is
    /// refused pre-mutation (ZERO writes). Reachable ONLY with BOTH `[tunables]`
    /// switches on — with the default `canary_online_probe = false` no probe runs at
    /// all, and with the default `canary_online_probe_strict = false` a failed probe
    /// is logged and the swap proceeds.
    ///
    /// A generic exit `1` via the `exit_code` catch-all, like its siblings
    /// [`Error::CanaryDrift`] / [`Error::CanaryUnparseableCanonical`]. Deliberately
    /// NOT the "retry shortly" `4`: a `rejected` verdict does not clear on its own,
    /// and one exit code for both verdicts keeps a caller from having to tell a
    /// dead token apart from a network blip by exit status — the log line and the
    /// message carry that distinction. Secret-free (issue #15): the verdict CLASS
    /// only, never a status code, a response body, or a token.
    // The remedy deliberately does NOT say "investigate with `sessiometer status`" the way
    // its two offline siblings above do. For them that is true — a standing canary verdict
    // is on the status wire. This probe adds nothing there by design (it is per-attempt, not
    // a standing verdict), and on the STANDALONE path it fires precisely when no daemon is
    // reachable, so `status` would answer "daemon not running". Naming the wrong surface
    // would send the operator to a screen that shows a healthy canary and read the refusal
    // as spurious. The event log is where the probe's durable line actually is.
    #[error(
        "refusing the credential write: the keychain-identity canary's online liveness probe \
         did not confirm the active canonical credential still authenticates (probe: {verdict}), \
         and `canary_online_probe_strict` is set. The probe's verdict is logged as \
         `event=canary_online_probe` in ~/Library/Logs/sessiometer/sessiometer.log; \
         `verdict=rejected` means the endpoint refused the credential, `verdict=inconclusive` \
         means it could not be reached. Use `sessiometer use --force <account>` for a one-off \
         swap past this check, or set `canary_online_probe_strict = false` under `[tunables]` \
         in config.toml and restart the daemon to stop refusing on unconfirmed probes"
    )]
    CanaryProbeNotLive {
        /// The probe's verdict class — `rejected` or `inconclusive`.
        verdict: &'static str,
    },

    // --- Daemon-routed swap (issue #167) -------------------------------------
    /// The running daemon performed a `use` swap on our behalf (issue #167 — `use`
    /// routes THROUGH the daemon when one is up) and its swap engine aborted for a
    /// reason other than the redacted-and-remapped ones (a locked keychain → exit
    /// `4`, a contended swap lock → exit `4`, a gone canonical → the recovery
    /// signal): a wrong-identity re-stash guard (#211), an absent stash, an I/O
    /// error, or a canary refusal ([`Error::CanaryDrift`],
    /// [`Error::CanaryUnparseableCanonical`], [`Error::CanaryProbeNotLive`]). The
    /// daemon aborted with ZERO writes. A generic exit `1`, like its sibling engine
    /// aborts. Secret-free: the daemon's ack is redacted to a machine reason code,
    /// never a token or email (issue #15).
    //
    // The message names the event LOG beside `status`, and does not promise a retry will
    // help. Since issue #736 not every cause of this rejection has a `status` surface: the
    // standing canary verdicts do, but the Layer-3 online probe is per-ATTEMPT and adds
    // nothing to the wire by design, so an operator sent to `status` alone would find a
    // healthy canary and read a real refusal as spurious. The log carries the reason for
    // EVERY cause that lands here, which is why it is named unconditionally rather than
    // per-cause — the wire enum stays closed (`classify_swap_failure`), so this message
    // cannot know which one it is.
    #[error(
        "the daemon could not complete the swap (it made no changes); check `sessiometer status` \
         and the daemon's log at ~/Library/Logs/sessiometer/sessiometer.log for the reason"
    )]
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
    /// `claude` is on the PATH scanned. Since issue #784 that PATH is the user's
    /// LOGIN-SHELL PATH, not the daemon's own — which is why the message names it:
    /// under launchd the daemon's inherited `$PATH` is a bare
    /// `/usr/bin:/bin:/usr/sbin:/sbin`, so "on your PATH" would have sent an
    /// operator looking in the wrong environment. Secret-free — a missing
    /// executable, never a credential.
    #[error(
        "could not locate the `claude` binary — install Claude Code so `claude` is on \
         your login shell's PATH, or set `$CLAUDE_BIN` to its absolute path"
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
    ///
    /// FRAMING (issue #1139): the `must` here is a CONSTRAINT STATEMENT — it states the isolation
    /// INVARIANT this engine enforces and is reporting a breach of, so the modal governs the tool
    /// rather than the operator — and is carved out for this variant alone in `ERROR_PROSE_LEDGER`.
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

    /// The config an artifact carries did not PARSE under this build's config parser
    /// (issue #1053) — the shape failure only, never a range/roster violation, which keeps
    /// its own [`Error::ConfigInvalid`]. Wraps the parser's own message (which names the
    /// offending key — the actionable half) and appends
    /// [`CONFIG_BLOCK_VERSION_FLOOR`](crate::migration::CONFIG_BLOCK_VERSION_FLOOR), so the
    /// operator gets the compatibility rule instead of a bare `deny_unknown_fields` line
    /// that names a key and explains nothing.
    ///
    /// **Redaction: inherited, not widened.** This path emits the parser message TODAY as
    /// [`Error::ConfigParse`], whose doc records why that is secret-free (a config holds only
    /// labels, account UUIDs, stash names and integer tunables — issue #15). So this variant
    /// exposes exactly the bytes the seam already exposed; it is the one member of the import
    /// group whose payload is that class rather than the group comment's "a count or a static
    /// reason", and it carries no credential, token or email either way.
    #[error(
        "the migration artifact's config was rejected by this build: {detail}\n  {}",
        crate::migration::CONFIG_BLOCK_VERSION_FLOOR
    )]
    MigrationImportConfigRejected { detail: String },

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
    /// or ambiguous (`6`) target, and a gate refusal (`7` — the pre-swap gate
    /// refusing a NAMED target without `--force`, or, since issue #960, `use
    /// --next` finding the whole fleet gated). Every other error is a generic
    /// failure (`1`). The mapping lives here so the `main` exit-code branch stays a
    /// thin lookup.
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
            // The gate refused — weekly-exhausted, cooldown, or quarantined all share
            // one "gate-refused" code, each with its own specific message. Issue #960
            // adds the FLEET-WIDE instance of the same verdict: `use --next` asked the
            // daemon for a candidate and `pick_target` excluded every account, for
            // exactly these reasons. The gate RAN and refused, so it belongs here rather
            // than in the generic `1` — a supervisor re-running `use --next` on a timer
            // needs "no capacity, back off" told apart from a malformed invocation. Note
            // the one asymmetry inside the family: `--force` overrides the three
            // per-target refusals but CANNOT override the fleet-wide one, because there
            // is no target to force onto (the remedy is to name one).
            Error::UseTargetWeeklyExhausted { .. }
            | Error::UseCooldownActive
            | Error::UseTargetQuarantined { .. }
            | Error::UseNextNoViableTarget { .. } => 7,
            _ => 1,
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    // The FRAMING firewall over this file's authored prose (issue #1139) — see the
    // `--- the FRAMING firewall ---` section at the foot of this module.
    use crate::framing_vocabulary::{
        scan_all_banned, scan_banned, ADVISORY_EXEMPT_TOKENS, BANNED_PHRASES, BANNED_TOKENS,
        HELP_EXEMPT_TOKENS, USAGE_EXEMPT_TOKENS,
    };

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
        // The `log` reader's two variants (issue #773) reach the generic arm via the `_ => 1`
        // catch-all, so the compiler never forced a decision about them — pinned here instead.
        // A malformed `--since` must be non-zero (never a silent whole-log fallback), and both
        // match their `reliability` counterparts rather than entering the swap/lock taxonomy.
        assert_eq!(Error::LogSinceInvalid("7x".to_owned()).exit_code(), 1);
        assert_eq!(Error::LogSerialize("boom").exit_code(), 1);
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

    // Issue #1005 / OQ-1 note: `enable`, `disable` and `remove` now route through
    // `use_account::resolve_target`, so their failures are `UseTargetNotFound` (5) and
    // `UseTargetAmbiguous` (6) rather than the retired `AccountLabelNotFound` — which appeared
    // nowhere in `exit_code` and so fell through to the generic `_ => 1`. The mapping asserted
    // below is UNCHANGED by that routing; what changed is which verbs reach it. A test here
    // restating these two codes would therefore pass against the pre-fix tree and gate nothing.
    // The observable move (1 → 5, and 6 becoming reachable at all) is gated where the verbs
    // actually produce the errors: `apply_enabled_rejects_an_unknown_label_without_touching_the_roster`,
    // `apply_remove_rejects_an_unknown_label_without_touching_the_roster` and
    // `the_two_verbs_routed_to_the_shared_resolver_return_its_own_error` in `crate::cli`, each
    // asserting the literal code off a real verb path.

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
        // Issue #960: `use --next` joins the SAME taxonomy rather than adding a code.
        // The daemon RAN its selection and refused (every account gated) → the exit-`7`
        // gate-refused class its per-target siblings above already own, so a supervisor
        // re-running `use --next` on a timer reads "no capacity, back off" rather than a
        // generic failure.
        assert_eq!(
            Error::UseNextNoViableTarget {
                detail: "out of capacity; resets in 2d4h — add an account".into()
            }
            .exit_code(),
            7
        );
        // …but an INABILITY to run the selection is not a refusal by it, so both of these
        // stay a generic `1` — the same line `UseViabilityUnverifiable` draws for the
        // un-runnable viability gate. Collapsing them into `7` would report "no capacity"
        // when the daemon was merely absent or had not polled yet.
        assert_eq!(Error::UseNextRequiresDaemon.exit_code(), 1);
        assert_eq!(
            Error::UseNextUnresolved {
                detail: "the daemon has not polled the rotation yet".into()
            }
            .exit_code(),
            1
        );
        assert_eq!(
            Error::UseViabilityUnverifiable {
                label: "spare".into()
            }
            .exit_code(),
            1
        );
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
            // Issue #960: the `--next` trio carries a duration and fixed authored prose —
            // never a label, so never an operator-authored email (#444/#447) either.
            Error::UseNextRequiresDaemon.to_string(),
            Error::UseNextNoViableTarget {
                detail: "out of capacity; resets in 2d4h — add an account".into(),
            }
            .to_string(),
            Error::UseNextUnresolved {
                detail: "the daemon has not polled the rotation yet".into(),
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
            Error::UseTargetNotFound {
                query: authored.into(),
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

    // --- the FRAMING firewall over this file's authored prose (issue #1139) --------------
    //
    // Issue #1123 took `Error::CliUsage` inside the #160 firewall and scoped itself to that ONE
    // variant. Issue #1139 carries the rest, and its first acceptance criterion is a SCOPING
    // DECISION rather than a patch. Both halves are recorded in
    // `crate::framing_vocabulary`'s module doc (§ "The fifth audience has no exemption set");
    // what follows is the mechanism, and `ERROR_PROSE_LEDGER` is the per-token reasoning.

    /// What issue #1139 decided about ONE banned token in ONE variant's message.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum Verdict {
        /// Measured, judged against the four editorial groups, and found OUTSIDE them. The
        /// message stands as shipped.
        Permitted,
        /// A real breach of the #160 firewall, tracked at the named issue and deliberately NOT
        /// reworded here: issue #1139 scoped a GUARD, and editing a shipped message to make a
        /// new guard pass is how a guard comes to certify the prose it was pointed at.
        Violation(&'static str),
    }

    struct LedgerEntry {
        variant: &'static str,
        token: &'static str,
        verdict: Verdict,
        why: &'static str,
    }

    /// The per-(variant, token) carve-out ledger — deliberately NOT an `ERROR_EXEMPT_TOKENS`
    /// set, which is the move issue #1139 was opened to refuse.
    ///
    /// The three exemption sets in `crate::framing_vocabulary` are earned by whole SURFACES
    /// whose every member spends the token: a `usage_hint`'s job is to name a command, so all of
    /// them may spell `disable`. `Error` is not a surface — it is dozens of independent messages
    /// that share a type. A set carrying `{add, must}` would excuse those tokens in EVERY one of
    /// them, so the day a new variant read "you must add an account" the guard would wave it
    /// through, having been widened by earlier messages that had nothing to do with it. Scoping
    /// each carve-out to the ONE variant that earned it keeps that from being possible, and it
    /// is why the guard scans all hits rather than the first: a second banned token in an
    /// already-ledgered message still bites.
    ///
    /// Reddening means either a message changed or a new one spends central vocabulary. Both are
    /// decisions — judge the token against the four groups the way the entries below do, then
    /// reword the message or add an entry with its reasoning. Never widen this into a set.
    ///
    /// # Why the bar here is a judgement, where `NOT_OPERATOR_PROSE`'s is not
    ///
    /// `src/cli.rs`'s `NOT_OPERATOR_PROSE` admits only a mechanically vacuous reason — the excused
    /// constant carries no WORDS — and says outright that "this one reads fine to me" is the
    /// judgement the guard exists to replace. That bar is right THERE and would be wrong here,
    /// because the two carve-outs remove different things. An excusal there takes a string out of
    /// the SUBJECT: it is never scanned, so nothing would ever catch it drifting, and only a
    /// property that makes scanning pointless can justify that. This ledger removes nothing from
    /// the subject. Every template is scanned, every hit is found, and an entry only records what
    /// was decided about a hit the guard already made. That is the same class of carve-out as
    /// `HELP_EXEMPT_TOKENS` and `USAGE_EXEMPT_TOKENS`, which are judgements too (the mechanical
    /// verb / editorial framing line issue #918 settled on the evidence), held honest the same way:
    /// pinned, and asserted to still be earned by the prose they excuse.
    ///
    /// What DOES transfer is that precedent's actual lesson — it was written because a doc
    /// requiring a reason while nothing tested for one let issue #1123's merge review excuse an
    /// editorialising string in three edits and a green run. So the reasoning below is enforced
    /// mechanically, not trusted: see `every_ledger_entry_is_earned_reasoned_and_pinned`.
    const ERROR_PROSE_LEDGER: &[LedgerEntry] = &[
        LedgerEntry {
            variant: "ConfigTargetMaxSessionAboveTrigger",
            token: "must",
            verdict: Verdict::Permitted,
            why: "CONSTRAINT STATEMENT, not recommendation framing. The group bans the \
                  operator-directed modal the issue #160 caller quoted (`you should`); here the \
                  modal's subject is a CONFIG VALUE — `target_max_session_usage must not exceed \
                  session_ceiling` cites the schema rule the value broke, in the third person, \
                  and directs nobody. The discriminator a future author should apply is whose \
                  behaviour the modal governs: the operator (recommendation, banned) or a value \
                  / this tool's own invariant (constraint, permitted).",
        },
        LedgerEntry {
            variant: "SharedCredentialMutated",
            token: "must",
            verdict: Verdict::Permitted,
            why: "CONSTRAINT STATEMENT, same test as the entry above: `the live session's \
                  credential must stay untouched` states the ISOLATION PREMISE this engine \
                  enforces — and is reporting a breach of — so the modal governs the tool's own \
                  invariant rather than the operator.",
        },
        LedgerEntry {
            variant: "ActiveAccountUnresolved",
            token: "add",
            verdict: Verdict::Permitted,
            why: "The non-acquisitive REMEDY DIRECTIVE that ADR-0020 § Status → Amended \
                  2026-08-10 (issue #1123) settled. The discriminator is the OBJECT of the \
                  imperative, not its mood: `add it to the rotation` adds THIS account to this \
                  tool's own roster — a free, local, mechanical operation on its own state — \
                  whereas the banned sense is acquisition, `add an account` in the sense of \
                  obtaining more capacity, which is the reading `BANNED_TOKENS`' own issue #160 \
                  note quotes (`add / buy / upgrade / cancel / bypass / need more`).",
        },
    ];

    /// One variant's authored `#[error(...)]` attribute, parsed out of this file's own source.
    struct ErrorProse {
        variant: String,
        /// Every string literal in the attribute, concatenated with Rust's line-continuation
        /// semantics resolved. EMPTY for `#[error(transparent)]`, whose `Display` is a foreign
        /// type's.
        template: String,
        /// The attribute's NON-literal arguments, verbatim (`transparent`, or the path of an
        /// interpolated constant).
        args: Vec<String>,
    }

    /// Every `#[error(...)]` attribute in this file's NON-test half, paired with the variant it
    /// decorates.
    ///
    /// The SUBJECT is each template, and that is the authored/interpolated seam
    /// [`Error::CliUsage`]'s own doc comment already draws rather than a new one: a template
    /// holds `{query}`, never the operator's query; `{0}`, never the TOML parser's sentence. It
    /// also buys the completeness argument — `thiserror` refuses to compile a variant carrying
    /// no `#[error(...)]`, so walking the attributes IS walking the variants, enforced by the
    /// compiler rather than by this walk being careful.
    ///
    /// Comment lines are dropped BEFORE anything is matched, and that cut is load-bearing twice
    /// over. This file's doc comments are dense with the very words the guard bans (`SAFETY
    /// ALARM` and `healthy` both occur here in prose) — they are DEVELOPER prose, never printed to
    /// an operator, so scanning them would report a firewall breach every other paragraph while
    /// telling an operator nothing. And dropping them first means the variant identifier is
    /// simply the next word after the attribute, with no doc block to walk over.
    fn error_prose() -> Vec<ErrorProse> {
        let source: Vec<char> = include_str!("error.rs")
            .lines()
            .take_while(|line| !line.starts_with("#[cfg(test)]"))
            .filter(|line| !line.trim_start().starts_with("//"))
            .collect::<Vec<_>>()
            .join("\n")
            .chars()
            .collect();
        let needle: Vec<char> = "#[error(".chars().collect();
        let mut out = Vec::new();
        let mut i = 0;
        while i + needle.len() <= source.len() {
            if source[i..i + needle.len()] != needle[..] {
                i += 1;
                continue;
            }
            // Balanced-paren walk, ignoring parens INSIDE a string literal — several messages
            // carry a parenthetical, and a naive walk would end the attribute on the first one.
            let body_start = i + needle.len();
            let mut j = body_start;
            let mut depth = 1_usize;
            let mut in_string = false;
            while j < source.len() && depth > 0 {
                match source[j] {
                    '\\' if in_string => j += 1,
                    '"' => in_string = !in_string,
                    '(' if !in_string => depth += 1,
                    ')' if !in_string => depth -= 1,
                    _ => {}
                }
                j += 1;
            }
            let body: String = source[body_start..j - 1].iter().collect();
            // Past the attribute's `]`, any further attributes, and the whitespace between, to
            // the variant identifier itself.
            let mut k = j;
            while k < source.len() {
                match source[k] {
                    c if c.is_whitespace() || c == ']' => k += 1,
                    '#' => {
                        while k < source.len() && source[k] != ']' {
                            k += 1;
                        }
                    }
                    _ => break,
                }
            }
            let variant: String = source[k..]
                .iter()
                .take_while(|c| c.is_alphanumeric() || **c == '_')
                .collect();
            let (template, args) = split_attr(&body);
            out.push(ErrorProse {
                variant,
                template,
                args,
            });
            i = j;
        }
        out
    }

    /// Every `Error` variant's identifier, in declaration order.
    ///
    /// [`error_prose`] is the walk; this is the same walk read for its NAMES rather than its
    /// prose, published `pub(crate)` for `crate::daemon`'s redaction meter, whose list of
    /// representative values is asserted complete against it (issue #1085).
    ///
    /// What makes this the DECLARED set rather than an approximation of it is `thiserror`,
    /// which refuses to compile a variant carrying no `#[error(...)]` — so walking the
    /// attributes is walking the variants. And what makes it worth publishing rather than
    /// letting each consumer parse this file for itself is the scrutiny already pointed at
    /// THIS walk: [`every_error_template_is_scanned_and_the_parse_cannot_be_evaded`] pins its
    /// cardinality, rejects a name that is not an identifier, and canaries the extraction
    /// against templates chosen to break a walk that truncates. A private second copy would
    /// inherit none of that and would need its own.
    pub(crate) fn declared_variant_names() -> Vec<String> {
        error_prose().into_iter().map(|p| p.variant).collect()
    }

    /// Split an `#[error(...)]` body into its concatenated string literals and its non-literal
    /// arguments.
    ///
    /// This models `rustc`'s string-literal escapes, and issue #1161 is what a divergence between
    /// the model and the language costs: the operator reads what `rustc` rendered, the guard
    /// scans what this function returned, so a banned token can be present in the first and
    /// absent from the second with every test green. Both halves below were demonstrated that
    /// way — by checking what the message RENDERS to, not by reading this function.
    ///
    /// Rust's line continuation — a `\` at end of line — eats the newline AND the run of ASCII
    /// whitespace after it, and resolves to NOTHING. It is emphatically not a space: a space is
    /// what this function used to push, which is why `"up\<newline>grade your plan"` reached an
    /// operator as `upgrade your plan` while the guard scanned `up grade your plan` and found
    /// nothing to report.
    ///
    /// State the skipped run precisely, because BOTH of its bounds are places to be wrong and an
    /// earlier revision of this fix was wrong at both. It is **ASCII** whitespace, so a NBSP
    /// (`U+00A0`) after the `\` is KEPT — `rustc` warns about it and moves on — whereas
    /// `char::is_whitespace`, the Unicode predicate, would eat it; and since a NBSP renders as a
    /// space, eating it is how `top\<newline><NBSP>up` reaches an operator as `top up` while the
    /// guard scans `topup`. And the run does not stop at the next newline: `rustc` skips a blank
    /// line after the `\` (warning "multiple lines skipped by escaped newline"), so a model that
    /// halted at `\n` would leave the far side unattached. `char::is_ascii_whitespace` is
    /// exactly the predicate `rustc`'s own unescaper uses, which is why this arm spells it that
    /// way rather than enumerating characters.
    ///
    /// Resolving the continuation to nothing does not SWALLOW the remainder — the far side
    /// is still appended, which is what the canary in
    /// `every_error_template_is_scanned_and_the_parse_cannot_be_evaded` pins, on the two of its
    /// three templates that carry their banned token past a continuation.
    ///
    /// # Why the escape set is CLOSED, and why anything outside it panics
    ///
    /// The other half of issue #1161 was `\u{75}` and `\x75`, which fell through to a
    /// push-it-verbatim arm: `u` followed by a literal `{75}`, so the banned word was never
    /// reassembled and never scanned. The fix could have RESOLVED them. It refuses instead, on
    /// the evidence of this file's own templates — measured when issue #1161 was decided, over
    /// the 87 then present: the only escapes any of them spend are the line continuation (43
    /// sites across 19 templates) and `\n` (2). No `\u{…}`, no `\x..`, while the 34 templates
    /// that need a non-ASCII character write it directly, `—` and all. The construct a resolver
    /// would model is one the house style does not reach for even where it is the obvious tool.
    ///
    /// Those counts are a snapshot and will drift; the claim resting on them does not, because
    /// the refusal below enforces it. The day a template does spend a `\u{…}`, this stops being
    /// a matter of anyone's recollection and becomes a failing test.
    ///
    /// Against that, a resolver is a strictly larger model and so a strictly larger drift
    /// surface, in the very way that produced this defect. `\u{75}`, `\u{7_5}` (underscores are
    /// legal between the hex digits), `\u{000075}` and `\x75` all render `upgrade`; surrogates
    /// and anything past `10FFFF` are compile errors rather than characters. Every one of those
    /// is a place a hand-rolled decoder can be subtly wrong, and wrong SILENTLY — which is the
    /// property being fixed, not a property worth re-buying.
    ///
    /// So the set below is closed and everything else panics naming the template. The cost is
    /// real and is accepted: a legitimate future `\u{…}` reddens this guard until someone teaches
    /// this arm and gives it a canary. That is the correct direction for the asymmetry — a false
    /// RED costs one edit, is paid at authoring time by the author who caused it, and says which
    /// template; a false GREEN is a firewall breach nobody learns about.
    ///
    /// `\\`, `\"` and `\'` are modeled rather than refused because this function already handled
    /// them correctly, by accident, in the arm that mangled `\u{…}`. Refusing them would narrow
    /// behaviour that was never wrong.
    fn split_attr(body: &str) -> (String, Vec<String>) {
        let mut literal = String::new();
        let mut rest = String::new();
        let mut chars = body.chars().peekable();
        while let Some(c) = chars.next() {
            if c != '"' {
                rest.push(c);
                continue;
            }
            while let Some(c2) = chars.next() {
                match c2 {
                    '"' => break,
                    '\\' => match chars.next() {
                        // Resolves to NOTHING — see this function's doc comment. Pushing a space
                        // here is what let `up\<newline>grade` past the guard as `up grade`.
                        Some('\n') => {
                            // ASCII whitespace, and newlines INCLUDED — both halves matter, and
                            // an earlier revision of this fix had each of them backwards. See the
                            // doc comment: `char::is_whitespace` is the Unicode predicate and
                            // would eat a NBSP `rustc` keeps, while stopping at `\n` would leave
                            // a blank line after the `\` that `rustc` skips.
                            while chars.peek().is_some_and(char::is_ascii_whitespace) {
                                chars.next();
                            }
                        }
                        Some('n') => literal.push('\n'),
                        Some('t') => literal.push('\t'),
                        Some('r') => literal.push('\r'),
                        Some(verbatim @ ('\\' | '"' | '\'')) => literal.push(verbatim),
                        Some(other) => panic!(
                            "`\\{other}` is an escape this extractor does not model, in the \
                             template {body:?}. It models `rustc` over a CLOSED set — `\\n`, \
                             `\\t`, `\\r`, `\\\\`, `\\\"`, `\\'`, and a line continuation — and \
                             REFUSES the rest rather than guess, because guessing is the issue \
                             #1161 defect: `\\u{{75}}pgrade your plan` reached an operator's \
                             terminal as `upgrade your plan` while this function returned \
                             `u{{75}}pgrade your plan` and the guard passed it. Write the \
                             character directly, as every non-ASCII template here already does, \
                             or teach this arm and give it a canary that reddens without it"
                        ),
                        None => break,
                    },
                    _ => literal.push(c2),
                }
            }
        }
        let args = rest
            .split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
            .collect();
        (literal, args)
    }

    /// Issue #1161's fidelity canary: what [`split_attr`] returns for a template is what `rustc`
    /// renders from the same source, across every escape the model claims to handle.
    ///
    /// The construction is the point, and it is why this cannot rot into the defect it fixes.
    /// Each case is the SAME source text twice — on the left as the characters [`error_prose`]
    /// reads out of this file (a raw string, where `\` is just a backslash), on the right as a
    /// real Rust literal, so the COMPILER supplies the expected value. One side of every pair IS
    /// the language being modeled, so no hand-typed expectation sits between the two and there is
    /// nothing to keep in sync: teaching the model a new escape means adding a pair, and a pair
    /// that disagrees with `rustc` cannot be written down.
    #[test]
    fn split_attr_resolves_every_modeled_escape_exactly_as_rustc_does() {
        // (the source text, what `rustc` makes of it)
        let cases: &[(&str, &str)] = &[
            // Issue #1161's own fixture — a continuation SPLITTING a word. RED before the fix:
            // the arm pushed a space, so this read `up grade your plan`.
            (
                r#""up\
                 grade your plan""#,
                "up\
                 grade your plan",
            ),
            // The idiom this file actually writes, at most of its continuation sites: an
            // explicit space BEFORE the `\`. Also RED before the fix, which produced a DOUBLE
            // space — the same divergence as the case above, harmless to the tokenizer and
            // therefore never noticed.
            (
                r#""refusing to swap: \
                 name a target""#,
                "refusing to swap: \
                 name a target",
            ),
            // (the continuation's skipped run has two BOUNDS, and an earlier revision of this fix
            // had each of them backwards; both are pinned below rather than here, because
            // neither case can be written as a compiling pair — see the comment there)
            //
            // The rest are GREEN before and after: regression pins on the arms that were already
            // faithful, so a later edit cannot quietly trade one divergence for another.
            (r#""line\nbreak""#, "line\nbreak"),
            (r#""col\tumn""#, "col\tumn"),
            (r#""ret\rurn""#, "ret\rurn"),
            (r#""back\\slash""#, "back\\slash"),
            (r##""he said \"no\"""##, "he said \"no\""),
            (r#""it\'s""#, "it\'s"),
            (r#""no escapes at all""#, "no escapes at all"),
        ];
        for (source, rendered) in cases {
            let (extracted, _) = split_attr(source);
            assert_eq!(
                &extracted, rendered,
                "the extractor and `rustc` disagree about {source:?} — the operator reads \
                 `rustc`'s answer and the guard scans this one, so any gap here is a message \
                 that can carry banned framing past a green run (issue #1161)"
            );
        }

        // The continuation's two BOUNDS. Both were RED at an earlier revision of this fix, both
        // are word-SPLITTING — so each is a live evasion, not a cosmetic difference — and neither
        // can join the compiling pairs above:
        //
        //   * the UPPER bound needs a real literal whose `\` is followed by a BLANK LINE, and
        //     `rustc` warns "multiple lines skipped by escaped newline" on exactly that. The
        //     warning is a lexer warning rather than a lint, so `-D warnings` does not escalate
        //     it — but it also cannot be silenced, and a permanent warning in every build is a
        //     worse thing to ship than a hand-written expectation with its provenance stated.
        //   * the LOWER bound needs a literal NBSP in this file, which would be invisible to
        //     every reader and easy to "tidy away".
        //
        // So both build the LEFT side — the characters `error_prose` would read out of a file —
        // and state the right side, which was established by running `rustc` over each source as
        // a real literal and printing the result. What that probe showed, verbatim:
        //
        //     H1 rustc renders: "upgrade your plan"
        //     H2 rustc renders: "running low — top\u{a0}\u{a0}\u{a0}up first"
        //
        // Upper bound: the skipped run does NOT stop at the next newline, so a model halting at
        // `\n` leaves the far side unattached and the split word never reassembles.
        let blank_line_source = "\"up\\\n\n             grade your plan\"";
        let (blank_line_extracted, _) = split_attr(blank_line_source);
        assert_eq!(
            blank_line_extracted, "upgrade your plan",
            "`rustc` skips a BLANK LINE after a continuation — it warns and skips it anyway — so \
             stopping the skipped run at `\\n` leaves the operator reading `upgrade` while the \
             guard scans two harmless halves (issue #1161)"
        );
        assert_eq!(
            scan_all_banned(&blank_line_extracted),
            vec!["upgrade"],
            "the guard's verdict, not merely the extracted string, is what a divergence here costs"
        );

        // Lower bound: the run is ASCII whitespace, so the NBSP is KEPT. The Unicode predicate
        // would eat it — and because a NBSP RENDERS as a space, eating it is how an operator
        // reads `top up` while the guard scans `topup`.
        let nbsp = '\u{a0}';
        let nbsp_source = format!("\"running low — top\\\n{nbsp}{nbsp}{nbsp}up first\"");
        let (nbsp_extracted, _) = split_attr(&nbsp_source);
        assert_eq!(
            nbsp_extracted,
            format!("running low — top{nbsp}{nbsp}{nbsp}up first"),
            "a NBSP after a line continuation is KEPT by `rustc`, so eating it here would let \
             `top<NBSP>up` — which an operator READS as `top up` — reach the scan as `topup` \
             (issue #1161)"
        );
        assert_eq!(
            scan_all_banned(&nbsp_extracted),
            scan_all_banned("running low — top up first"),
            "the guard's verdict must not depend on WHICH space character separates two words, \
             since the operator cannot tell them apart on a terminal"
        );

        // END TO END, which is how issue #1161 was demonstrated in the first place: not that the
        // extracted string changed, but that the GUARD'S VERDICT on it did. Before the fix this
        // scan came back empty over a message that renders `upgrade your plan`.
        let (template, _) = split_attr(
            r#""up\
             grade your plan""#,
        );
        assert_eq!(
            scan_all_banned(&template),
            vec!["upgrade"],
            "a banned token split across a line continuation must be scanned as the operator \
             reads it — reassembled, not left as two harmless-looking halves"
        );
    }

    /// Issue #1161's refusal canary: an escape outside the modeled set stops the guard LOUDLY and
    /// says which template did it, instead of being guessed at and scanned as a string the
    /// operator will never see.
    ///
    /// Both halves are asserted, because loud and legible are different properties and only the
    /// second one is actionable. A panic that said merely "unsupported escape" would pass a
    /// did-it-refuse test while leaving the next author to bisect 87 templates by hand.
    #[test]
    fn split_attr_refuses_an_unmodeled_escape_and_names_the_offending_template() {
        for (source, escape) in [
            // The issue's fixture. Before the fix this returned `u{75}pgrade your plan`, which
            // tokenises to `u` / `75` / `pgrade` and reports clean.
            (r#""\u{75}pgrade your plan""#, r"\u"),
            // `\x..` is the same hole differently spelled — and so are `\u{7_5}` (underscores
            // are legal between the hex digits) and `\u{000075}`. All four render `upgrade`,
            // which is the case for refusing the CLASS rather than racing its spellings.
            (r#""\x75pgrade your plan""#, r"\x"),
            (r#""\u{7_5}pgrade your plan""#, r"\u"),
            // `\0` is NUL to `rustc` and was the digit `0` here. Nothing to do with the guard's
            // vocabulary, caught anyway by closing the set rather than patching known holes.
            (r#""\0""#, r"\0"),
        ] {
            let payload = match std::panic::catch_unwind(|| split_attr(source)) {
                Ok((extracted, _)) => panic!(
                    "`split_attr` did not refuse {source:?} — it silently returned \
                     {extracted:?}, which is not what `rustc` renders. That gap IS issue #1161"
                ),
                Err(payload) => payload,
            };
            let message = payload
                .downcast_ref::<String>()
                .cloned()
                .unwrap_or_else(|| "<the panic payload was not a String>".to_owned());
            // Split on the template echo FIRST and assert the escape is named in what precedes
            // it. Asserting `message.contains(escape)` over the whole message cannot fail: the
            // Debug echo of the template already spells the escape, so the check below would
            // subsume it and the escape half would be pure ceremony.
            let echo = format!("{source:?}");
            let (before_echo, _) = message.split_once(&echo).unwrap_or_else(|| {
                panic!("the refusal does not name the offending template {source:?}: {message}")
            });
            assert!(
                before_echo.contains(escape),
                "the refusal echoes the template but never names the offending escape \
                 {escape:?} in its own right, so a reader has to spot it inside the quoted \
                 template: {message}"
            );
        }
    }

    /// What is wrong with one entry's VIOLATION bookkeeping — a violation must name its tracking
    /// issue, and its reasoning must cite the same one — or empty when the entry is sound. Every
    /// `Permitted` entry is sound here by construction: these two rules are scoped to debts.
    ///
    /// Factored out of the audit for the reason [`unaccounted_framing`] is, and issue #1151 is
    /// what made it necessary. It spent the ledger's only debt, and the shipped subject these
    /// rules run over is now EMPTY — over zero violations they pass identically whether they check
    /// anything or nothing. So the audit calls this, and so does a bite proof that can supply the
    /// entries the ledger no longer has.
    fn violation_defects(entry: &LedgerEntry) -> Vec<String> {
        let Verdict::Violation(issue) = entry.verdict else {
            return Vec::new();
        };
        let mut defects = Vec::new();
        // The digit run is asserted NON-EMPTY in its own right, which is the half issue #1182
        // was opened over: `all` over an empty iterator is vacuously TRUE, so `"#"` cleared the
        // old `issue[1..].chars().all(..)` by carrying no digit that could fail it, and cleared
        // `why.contains(issue)` too, since any reasoning that spells a `#` at all satisfies it. A
        // reference structurally incapable of naming one is an untracked violation that reads as
        // tracked. `strip_prefix` also retires the slice: the empty string was safe before only
        // because `starts_with` short-circuited ahead of `issue[1..]`, and a guard that depends
        // on the order of its own conjuncts is one reorder from panicking. Both inputs are
        // pinned in `the_violation_bookkeeping_bites_over_a_ledger_that_carries_no_violation`.
        let digits = issue.strip_prefix('#').unwrap_or_default();
        if digits.is_empty() || !digits.chars().all(|c| c.is_ascii_digit()) {
            defects.push(format!(
                "{}: {issue:?} is not an issue reference",
                entry.variant
            ));
        }
        if !entry.why.contains(issue) {
            defects.push(format!(
                "{}: the reasoning does not cite the issue tracking it",
                entry.variant
            ));
        }
        defects
    }

    /// Every banned token in an authored error template that [`ERROR_PROSE_LEDGER`] does not
    /// account for — the audit itself, factored out of the assertion so the BITE proof can drive
    /// the identical code path over a deliberately poisoned copy of the REAL shipped prose.
    fn unaccounted_framing(prose: &[ErrorProse]) -> Vec<String> {
        let mut findings = Vec::new();
        for item in prose {
            for hit in scan_all_banned(&item.template) {
                if !ERROR_PROSE_LEDGER
                    .iter()
                    .any(|entry| entry.variant == item.variant && entry.token == hit)
                {
                    findings.push(format!("{}: {hit:?}", item.variant));
                }
            }
        }
        findings
    }

    /// Issue #1139's completeness tripwire: a NEW variant cannot ship unscanned, and the parse
    /// that guarantees it cannot be defeated by spelling something differently.
    ///
    /// The nearest precedent is `src/cli.rs`'s `every_cli_usage_construction_site_is_scanned`,
    /// which pins a construction-site COUNT and needs a hand-written argv case per site. This
    /// one is structurally stronger and needs neither: `thiserror` will not compile a variant
    /// carrying no `#[error(...)]`, so a new variant is scanned the moment it exists. What is
    /// left to guard is the PARSE — and a parse that silently matched fewer attributes, or
    /// returned empty templates, would satisfy every assertion in the guard below over nothing.
    #[test]
    fn every_error_template_is_scanned_and_the_parse_cannot_be_evaded() {
        let prose = error_prose();

        // CARDINALITY, pinned rather than compared — the degenerate-subject guard. Growing the
        // enum is expected and cheap: add the variant, bump this in the same commit.
        assert_eq!(
            prose.len(),
            87,
            "the `#[error(...)]` count moved. A new variant is scanned automatically (that is \
             the point), but the subject's SIZE is pinned so a parse that matched fewer cannot \
             pass silently — which is issue #918's failure, one variant down instead of one \
             surface down"
        );

        // The names are real, distinct variant identifiers — not `]`, whitespace, or a repeat
        // that a mis-stepped walk to the identifier would collect.
        for item in &prose {
            assert!(
                item.variant.chars().next().is_some_and(char::is_uppercase),
                "parsed {:?} as a variant name — the walk past the attribute is wrong",
                item.variant
            );
        }
        let mut names: Vec<&str> = prose.iter().map(|p| p.variant.as_str()).collect();
        names.sort_unstable();
        let before = names.len();
        names.dedup();
        assert_eq!(before, names.len(), "a variant name was parsed twice");

        let by_name = |name: &str| -> String {
            prose
                .iter()
                .find(|p| p.variant == name)
                .unwrap_or_else(|| panic!("{name} is missing from the parse"))
                .template
                .clone()
        };

        // CANARY the extraction. A parser returning empty strings carries no banned framing
        // perfectly, so the three templates issue #1139 was filed about are asserted to hold
        // real, distinctive text — and specifically text on the FAR side of a `\` line
        // continuation, which is exactly where a walk that stopped at the first backslash would
        // truncate while still looking like it worked.
        for (variant, fragment) in [
            (
                "ConfigTargetMaxSessionAboveTrigger",
                "must not exceed session_ceiling",
            ),
            ("SharedCredentialMutated", "credential must stay untouched"),
            ("ActiveAccountUnresolved", "adopt that account directly"),
        ] {
            let template = by_name(variant);
            assert!(
                template.contains(fragment),
                "{variant}'s template lost {fragment:?} — the literal walk truncated it: \
                 {template:?}"
            );
        }

        // Every template is non-empty EXCEPT the one `#[error(transparent)]`, whose `Display` is
        // a foreign type's and cannot be scanned here. Pinned BY NAME: a second transparent
        // variant is a new hole in the subject and should be a deliberate edit that reddens.
        let foreign: Vec<&str> = prose
            .iter()
            .filter(|p| p.template.is_empty())
            .map(|p| p.variant.as_str())
            .collect();
        assert_eq!(
            foreign,
            ["Io"],
            "the set of variants with no authored template moved — `#[error(transparent)]` \
             delegates `Display` to another type, so its prose is outside this guard's reach"
        );

        // The ONE non-literal argument any attribute passes, besides `transparent`, is an
        // interpolated constant — and it is OURS, so it is scanned here rather than waved
        // through as "not a literal". Without this, `#[error("{}", SOME_EDITORIAL_CONST)]` would
        // carry banned prose into an operator's terminal straight past a literal-only walk.
        let mut args: Vec<&str> = prose
            .iter()
            .flat_map(|p| p.args.iter().map(String::as_str))
            .collect();
        args.sort_unstable();
        assert_eq!(
            args,
            [
                "crate::migration::CONFIG_BLOCK_VERSION_FLOOR",
                "transparent"
            ],
            "an `#[error(...)]` grew a non-literal argument. Its VALUE is operator-facing prose \
             this walk cannot see, so scan it explicitly beside the import floor below"
        );
        assert_eq!(
            scan_all_banned(crate::migration::CONFIG_BLOCK_VERSION_FLOOR),
            Vec::<&str>::new(),
            "the interpolated import-floor constant carries banned framing"
        );
    }

    /// Issue #1139's guard: no authored error template spends central FRAMING vocabulary that
    /// [`ERROR_PROSE_LEDGER`] does not account for.
    #[test]
    fn every_error_template_carries_no_banned_framing_beyond_the_pinned_ledger() {
        assert_eq!(
            unaccounted_framing(&error_prose()),
            Vec::<String>::new(),
            "an authored error template spends central FRAMING vocabulary (issue #160) that no \
             `ERROR_PROSE_LEDGER` entry accounts for. This is a DECISION, not a lint to silence: \
             judge the token against the four editorial groups the way the ledger's own entries \
             do, then either reword the message or add an entry carrying that reasoning. Do NOT \
             reach for an `ERROR_EXEMPT_TOKENS` set — issue #1139 settled that this audience has \
             none, because one set would excuse the token across every message in this file"
        );
    }

    /// Issue #1139 AC-2, to the standard issue #918 established: the guard BITES, demonstrated
    /// on the REAL shipped prose rather than on a string this test wrote for itself.
    ///
    /// That distinction is the whole of the standard. Issue #885 asserted coverage that did not
    /// exist and nothing caught it, because a scan over prose that is already clean passes
    /// identically over a scanner that inspects nothing. Here the subject is
    /// [`error_prose`]'s real output, poisoned one token at a time and reverted between, driven
    /// through the same [`unaccounted_framing`] the assertion above uses.
    #[test]
    fn the_error_guard_bites_on_every_editorial_group_injected_into_real_shipped_prose() {
        // GREEN before: the unpoisoned shipped subject is clean.
        assert_eq!(unaccounted_framing(&error_prose()), Vec::<String>::new());

        // RED on each editorial group, injected into a real shipped message.
        for (group, injection, caught) in [
            ("acquisitive imperative", "Upgrade your plan.", "upgrade"),
            ("value judgement", "Your usage is critical.", "critical"),
            ("recommendation framing", "You should re-login.", "should"),
            ("alarmist projection", "Exhaustion is imminent.", "imminent"),
            (
                "acquisitive phrase",
                "Running out — top up first.",
                "top up",
            ),
            (
                "acquisitive phrase",
                "Running low — get more seats.",
                "get more",
            ),
        ] {
            let mut poisoned = error_prose();
            poisoned
                .iter_mut()
                .find(|p| p.variant == "DaemonNotRunning")
                .expect("DaemonNotRunning is a real shipped variant")
                .template
                .push_str(injection);
            assert_eq!(
                unaccounted_framing(&poisoned),
                vec![format!("DaemonNotRunning: {caught:?}")],
                "the {group} group must bite when injected into a real shipped error template"
            );
        }

        // A LEDGERED variant is not a blanket pass. The ledger excuses a TOKEN on a VARIANT, so
        // a SECOND banned token in the same message still bites — the property an
        // `ERROR_EXEMPT_TOKENS` set could not have given, and the reason the scan is all-hits
        // rather than first-hit.
        let mut poisoned = error_prose();
        poisoned
            .iter_mut()
            .find(|p| p.variant == "ActiveAccountUnresolved")
            .expect("ActiveAccountUnresolved is a real shipped variant")
            .template
            .push_str("Upgrade your plan.");
        assert_eq!(
            unaccounted_framing(&poisoned),
            vec!["ActiveAccountUnresolved: \"upgrade\"".to_owned()],
            "a ledgered variant must still bite on a token its entry does not name — and its \
             ledgered `add` must stay accounted for while it does"
        );

        // GREEN after: reverted, the shipped subject is clean again.
        assert_eq!(unaccounted_framing(&error_prose()), Vec::<String>::new());
    }

    /// Issue #1139 AC-3, the does-the-carve-out-swallow-the-guard proof, from both ends: every
    /// ledger entry is EARNED by the variant it names, and the ledger as a whole is PINNED.
    ///
    /// Reddening on the earned half means a message edit dropped the last use of a carve-out:
    /// DELETE the entry, never widen this test. That is also the completion signal for the
    /// violation entry — issue #1151 is done exactly when its token stops being spent.
    #[test]
    fn every_ledger_entry_is_earned_reasoned_and_pinned() {
        let prose = error_prose();

        // PINNED to exactly the three measured entries, for the reason issue #918 pinned the help
        // exemption set: growing this is a design decision that must redden a test, never
        // something that accretes an entry at a time to silence an inconvenient message.
        let pairs: Vec<(&str, &str)> = ERROR_PROSE_LEDGER
            .iter()
            .map(|entry| (entry.variant, entry.token))
            .collect();
        assert_eq!(
            pairs,
            [
                ("ConfigTargetMaxSessionAboveTrigger", "must"),
                ("SharedCredentialMutated", "must"),
                ("ActiveAccountUnresolved", "add"),
            ],
            "the ledger moved — see issue #1139: these are the (variant, token) pairs the \
             shipped error prose measurably spends, and each was judged one at a time"
        );

        for entry in ERROR_PROSE_LEDGER {
            // No carve-out for a token nobody bans: it would read as a real one while carving
            // out nothing — the same discipline as `every_derived_exemption_names_a_real_central_token`.
            assert!(
                BANNED_TOKENS.contains(&entry.token),
                "{}: {:?} is carved out but is not in BANNED_TOKENS",
                entry.variant,
                entry.token
            );
            // …and it is STILL SPENT by the variant it names.
            let template = prose
                .iter()
                .find(|p| p.variant == entry.variant)
                .unwrap_or_else(|| panic!("{} names no variant", entry.variant));
            assert!(
                scan_all_banned(&template.template).contains(&entry.token),
                "{}: {:?} is carved out but that message no longer spends it — DELETE the \
                 entry rather than carry a dead carve-out",
                entry.variant,
                entry.token
            );
            // EVERY entry records why, violation or not — the lesson `src/cli.rs`'s
            // `every_excusal_is_reasoned` was written to teach: a doc that REQUIRES a reason while
            // nothing TESTS for one let issue #1123's merge review excuse an editorialising string
            // in three mechanical edits and a fully green run. Asserted for the `Permitted` entries
            // especially, since those are the ones whose only defence IS their reasoning.
            assert!(
                !entry.why.trim().is_empty(),
                "{}: {:?} is carved out with no reasoning recorded — an unreasoned carve-out is \
                 indistinguishable from an oversight",
                entry.variant,
                entry.token
            );
            // Every VIOLATION names its tracking issue, and its reasoning cites the same one. An
            // untracked violation is an exemption with a disapproving tone. Vacuous today by
            // design — the ledger carries no debt since issue #1151 — so the rules themselves are
            // proved to still bite in
            // `the_violation_bookkeeping_bites_over_a_ledger_that_carries_no_violation`, which
            // gives each conjunct a case that reddens when that conjunct is dropped. Several of
            // its cases share an arm rather than owning one: a reference carrying no `#`
            // (`"1151"`), one whose digit run is empty (`"#"`), and the empty string all fail
            // `digits.is_empty()`, since `strip_prefix` folds a missing `#` into an empty run.
            // They are kept apart because they arrived as separate defects and can separate
            // again — `"#"` is issue #1182, which the digit-CLASS arm alone let through, and the
            // empty string is the input the old `issue[1..]` slice relied on `starts_with`
            // short-circuiting ahead of. The digit-class arm needs its own case too (`"#abc"`),
            // and for one revision was carried by this comment rather than by a test.
            assert_eq!(
                violation_defects(entry),
                Vec::<String>::new(),
                "{}: a violation entry's bookkeeping is unsound",
                entry.variant
            );
        }

        // The violation set is pinned too: it is what tells a reader which entries are DECISIONS
        // and which are debts, and a Permitted entry quietly re-labelled would erase that. It is
        // EMPTY since issue #1151 spent the one debt this ledger ever carried — asserted rather
        // than deleted along with it, because "no outstanding violations" is itself a claim worth
        // holding honest, and an empty pin is what makes the next one arrive as a deliberate edit
        // carrying its own issue rather than as an accretion nobody had to defend.
        let violations: Vec<&str> = ERROR_PROSE_LEDGER
            .iter()
            .filter_map(|entry| match entry.verdict {
                Verdict::Violation(issue) => Some(issue),
                Verdict::Permitted => None,
            })
            .collect();
        assert_eq!(
            violations,
            Vec::<&str>::new(),
            "the ledger's violation set moved — a new one needs its own issue, and a resolved \
             one needs its entry deleted rather than flipped to Permitted"
        );
    }

    /// The VIOLATION bookkeeping still BITES, over a ledger that no longer carries a violation.
    ///
    /// Issue #1151 spent the ledger's only debt, and in doing so emptied the subject of the two
    /// rules `every_ledger_entry_is_earned_reasoned_and_pinned` applies to a debt: over zero
    /// violation entries they pass identically whether they check anything or nothing. That is the
    /// degenerate subject this file refuses everywhere else — the standard
    /// `the_error_guard_bites_on_every_editorial_group_injected_into_real_shipped_prose` states,
    /// and the failure issue #918 measured. So the rules are driven here over entries built for
    /// the purpose, through the same [`violation_defects`] the audit calls.
    ///
    /// Hand-built entries are weaker evidence than the real shipped prose, and are used here for
    /// the one reason that makes them the ONLY evidence available: the ledger's violation set is
    /// asserted EMPTY, so there is no real entry to poison. They stop being the only evidence the
    /// day a violation is recorded again, at which point the audit's own call carries it.
    #[test]
    fn the_violation_bookkeeping_bites_over_a_ledger_that_carries_no_violation() {
        // The premise: this test exists BECAUSE the shipped subject is empty. Should a real
        // violation return, this is the line that says so and the synthetic cases become a
        // supplement rather than the whole proof.
        assert!(
            !ERROR_PROSE_LEDGER
                .iter()
                .any(|entry| matches!(entry.verdict, Verdict::Violation(_))),
            "a real violation is back in the ledger — the audit now exercises these rules over \
             live prose, so re-read whether this synthetic proof is still the right shape"
        );

        // A sound debt: tracked at an issue reference, and its reasoning cites the same one.
        let sound = LedgerEntry {
            variant: "Synthetic",
            token: "healthy",
            verdict: Verdict::Violation("#1151"),
            why: "the value judgement issue #1151 removed from ActiveAccountUnresolved",
        };
        assert_eq!(
            violation_defects(&sound),
            Vec::<String>::new(),
            "a violation naming its issue and citing it in the reasoning is sound"
        );

        // …and a `Permitted` entry is out of scope for both rules, whatever it carries — the
        // assertion that these are debt rules rather than entry rules.
        assert_eq!(
            violation_defects(&LedgerEntry {
                verdict: Verdict::Permitted,
                why: "cites no issue at all, and owes none",
                ..sound
            }),
            Vec::<String>::new(),
            "the violation rules must not fire on a Permitted entry"
        );

        // RED: a violation whose tracking reference is not an issue number. `1151` is the shape
        // an author reaches for when the `#` is forgotten, and it would read as tracked.
        assert_eq!(
            violation_defects(&LedgerEntry {
                verdict: Verdict::Violation("1151"),
                ..sound
            }),
            vec!["Synthetic: \"1151\" is not an issue reference".to_owned()],
            "an untracked violation is an exemption with a disapproving tone"
        );

        // RED, and it is the case that proves the digit-CLASS arm rather than the emptiness one.
        // `1151` above carries no `#`, so `strip_prefix` leaves it an empty run and the
        // `is_empty()` arm short-circuits the `||`: with only that case the class check is never
        // evaluated, and replacing `c.is_ascii_digit()` with `true` leaves the whole suite green.
        // Measured on an independent review of this branch, which is how the hole was found. This
        // case has the `#` and a non-empty run behind it, so it reaches the class check and is
        // the one that reddens when that half is dropped.
        //
        // `why` is overridden rather than inherited from `sound`, and that is what isolates the
        // rule: the reasoning must CITE the reference, so a `why` that does not mention `#abc`
        // fires the second rule too and the expectation would no longer distinguish which one
        // caught it. `1151` gets away with inheriting because `#1151` contains it as a substring.
        assert_eq!(
            violation_defects(&LedgerEntry {
                verdict: Verdict::Violation("#abc"),
                why: "tracked at #abc, which is not an issue number at all",
                ..sound
            }),
            vec!["Synthetic: \"#abc\" is not an issue reference".to_owned()],
            "the reference's tail must be digits — a `#` alone does not make it an issue number"
        );

        // RED, and the one-character case `"#abc"` above cannot reach. `all` over an EMPTY
        // iterator is vacuously TRUE, so a bare `#` satisfied the digit run by carrying no digit
        // that could fail it — issue #1182. It defeats the SECOND rule in the same breath, which
        // is why `why` is inherited from `sound` here rather than overridden: `contains("#")`
        // is satisfied by any reasoning that spells a `#`, which `sound`'s citation of #1151 does.
        // Both halves green over a reference that names nothing was the whole defect.
        assert_eq!(
            violation_defects(&LedgerEntry {
                verdict: Verdict::Violation("#"),
                ..sound
            }),
            vec!["Synthetic: \"#\" is not an issue reference".to_owned()],
            "a `#` with no digits behind it tracks nothing — an untracked violation that reads \
             as tracked"
        );

        // RED, and it pins a property this fix could LOSE rather than one it adds. The empty
        // string was already rejected, but by a different conjunct: `starts_with('#')`
        // short-circuited ahead of the `issue[1..]` slice, which is equally what kept it from
        // panicking on a zero-length reference. The predicate no longer slices, so that ordering
        // is gone and so is the guarantee riding on it — issue #1182's third criterion offered
        // keep-the-ordering or assert-it, and this is the assertion.
        assert_eq!(
            violation_defects(&LedgerEntry {
                verdict: Verdict::Violation(""),
                ..sound
            }),
            vec!["Synthetic: \"\" is not an issue reference".to_owned()],
            "an empty tracking reference is rejected, and reaching that verdict must not panic"
        );

        // RED: tracked at a real issue, but the reasoning cites a DIFFERENT one — the drift a
        // reader cannot see, since both halves look filled in.
        assert_eq!(
            violation_defects(&LedgerEntry {
                verdict: Verdict::Violation("#1139"),
                ..sound
            }),
            vec!["Synthetic: the reasoning does not cite the issue tracking it".to_owned()],
            "the reasoning must cite the issue the entry is tracked at, not merely some issue"
        );
    }

    /// Issue #1139 AC-5: `Error::CliUsage`'s existing coverage, and the stats-side and help-side
    /// coverage, are UNCHANGED — asserted here rather than assumed, from the side issue #1139
    /// could have broken them.
    ///
    /// It could have broken them two ways, and both are checked. The fifth audience was added
    /// BESIDE the existing four rather than by widening one, so the central lists and all three
    /// exemption sets must be exactly what they were (this deliberately duplicates the pins in
    /// `crate::framing_vocabulary`, as a regression check on THIS change rather than a
    /// restatement of theirs). And `scan_with` was re-expressed as the first element of the new
    /// `scan_all_with` rather than left as a second walk, so first-hit and all-hits are asserted
    /// to agree — a fork there would have changed what every other audience sees.
    #[test]
    fn the_error_audience_neither_widened_nor_narrowed_the_other_four() {
        assert_eq!(BANNED_TOKENS.len(), 51);
        assert_eq!(BANNED_PHRASES, &["top up", "get more"]);
        assert_eq!(HELP_EXEMPT_TOKENS, &["add", "disable", "enable", "remove"]);
        assert_eq!(ADVISORY_EXEMPT_TOKENS, &["enable"]);
        assert_eq!(USAGE_EXEMPT_TOKENS, &["disable", "enable", "remove"]);

        // This audience really does scan the WHOLE list. With no exemption set, every editorial
        // group is armed on every variant BY CONSTRUCTION — there is no subset that could have
        // dropped one — so the proof is that each central token and phrase still bites in
        // error-shaped prose.
        for token in BANNED_TOKENS {
            assert_eq!(
                scan_all_banned(&format!("refusing to swap: {token} something")),
                vec![*token],
                "the Error audience must see every central token — it has no exemption set"
            );
        }
        for phrase in BANNED_PHRASES {
            assert_eq!(
                scan_all_banned(&format!("running out — {phrase} first")),
                vec![*phrase],
                "the Error audience must see every central phrase"
            );
        }

        // First-hit IS the first element of all-hits, over prose that exercises the tokenizer's
        // documented behaviours: clean, single token, multiple tokens, a phrase, ANSI, and case
        // plus punctuation.
        for text in [
            "daemon not running — start it with `sessiometer run`",
            "refusing to swap: a swap cooldown is active",
            "you should upgrade and add another",
            "runs out in ~4h — top up before then",
            "\x1b[31mcritical\x1b[0m",
            "period — you SHOULD.",
            "nothing bypasses it",
        ] {
            assert_eq!(
                scan_banned(text),
                scan_all_banned(text).into_iter().next(),
                "first-hit and all-hits disagreed on {text:?} — the shared tokenizer forked"
            );
        }
    }
}
