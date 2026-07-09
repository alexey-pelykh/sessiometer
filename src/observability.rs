// Copyright (c) 2026 Oleksii PELYKH
// SPDX-License-Identifier: MIT

//! The structured event log.
//!
//! One line per daemon EVENT, in a flat space-separated `key=val` grammar:
//!
//! ```text
//! ts=<RFC3339> event=<name> <key=val>…
//! ```
//!
//! emitted through the single [`Event::to_log_line`] formatter to
//! `~/Library/Logs/sessiometer/sessiometer.log` (macOS-native, surfaced in
//! Console.app) via the path-resolution module (#1). No logging framework: no
//! levels, rotation, or filtering — plain timestamped lines suffice (issue #9).
//!
//! ## Redaction surface (issue #15)
//!
//! Every [`Event`] field is a HANDLE (the operator label), an enum, a number, or
//! a timestamp — never free-form, secret-bearing text. That type-level constraint
//! is what makes [`Event::to_log_line`] the *sole* place an event becomes a log
//! line, and therefore the one surface the redaction METER (#15) has to check:
//! nothing else interpolates account data onto this channel. Identity is always
//! the stable handle — never an email, never a token.
//!
//! Note for #15: a handle is an operator-chosen label; config validation forbids
//! an *empty* label but not whitespace, so a label containing a space or `=` would
//! split the `key=val` grammar. Enforcing the handle charset is the meter's job
//! (#15); this module localizes the surface but does not police it.
//!
//! ## The diagnostic channel (issue #77)
//!
//! Separate from the event log above, the OPERATOR-FACING diagnostic channel
//! answers "what is `run` doing right now" — per-poll outcomes, the per-tick
//! decision, and lifecycle markers — for an operator debugging the daemon. Where
//! the event log records durable STATE CHANGES (edge-triggered, levelless), the
//! diagnostic channel is per-cycle DETAIL behind a verbosity gate ([`Verbosity`]):
//! default [`Verbosity::Quiet`] emits nothing, `-v`/`--verbose` opts in. It rides
//! its own single redaction surface — [`Diagnostic::to_log_line`], the sibling of
//! [`Event::to_log_line`] — under the SAME field discipline (every field a handle /
//! enum / number / timestamp, never a token or email), so the #15 METER scans
//! rendered diagnostics alongside events and the channel inherits the redaction
//! guarantee without weakening it.

use std::fs::File;
use std::io::Write;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::error::Result;
use crate::paths;

/// Why a swap happened — the `reason=` of an [`Event::Swap`].
///
/// The two AUTONOMOUS reasons are re-derived at swap time from the readings (the
/// binary [`crate::swap::decide`] does not carry which dimension fired); when BOTH
/// dimensions are at/over their triggers, the daemon reports [`SwapReason::Session`]
/// — session-first precedence. The two MANUAL reasons (issue #63) are operator-driven,
/// NOT usage-triggered: [`SwapReason::Manual`] is a `sessiometer use <account>` whose
/// pre-swap gate passed, and [`SwapReason::Forced`] is one whose policy gate was
/// bypassed with `--force`. A manual swap records `session_pct=0` (it was not driven
/// by session usage — this `reason=` is what distinguishes it).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SwapReason {
    /// The session-window trigger fired (or both did — session takes precedence).
    Session,
    /// The weekly-window trigger fired while session was below its own.
    Weekly,
    /// An operator `sessiometer use <account>` whose pre-swap gate PASSED (#63).
    Manual,
    /// An operator `sessiometer use <account> --force` whose policy gate was
    /// BYPASSED (#63). Safety behavior is never bypassed — only policy.
    Forced,
}

impl SwapReason {
    /// The `reason=` token.
    fn as_str(self) -> &'static str {
        match self {
            SwapReason::Session => "session",
            SwapReason::Weekly => "weekly",
            SwapReason::Manual => "manual",
            SwapReason::Forced => "forced",
        }
    }
}

/// How the periodic isolated-refresh tick classified one cycle — the `outcome=` of an
/// [`Event::Refresh`] (issue #106).
///
/// A NON-SECRET projection of the engine's refresh report (its outcome classification
/// plus whether the CAS re-stash stored the fresh token); the report's secret-bearing
/// internals (the token blobs it inspects) never reach this enum. The tick maps the
/// report to this; rendering it here keeps the event log the single redaction surface.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RefreshEventOutcome {
    /// Claude Code refreshed the parked token and the CAS re-stash stored it.
    Refreshed,
    /// CC refreshed the token (so the refresh token was still valid — the credential is
    /// alive) but a concurrent swap / login took precedence, so it was not re-stashed.
    RefreshedNotReStashed,
    /// CC returned the seeded token unchanged — no refresh happened.
    NoChange,
    /// CC cleared the refresh token in place — the credential is dead and needs an
    /// operator re-login.
    Dead,
    /// The cycle ran but produced no usable result (a spawn / read-back / lock failure,
    /// or a whole-cycle timeout).
    Error,
}

impl RefreshEventOutcome {
    /// The `outcome=` token. `pub(crate)` so the offline `list` view (issue #120) can
    /// render the last-persisted outcome it reads back via [`last_refresh_outcomes`]
    /// in the SAME vocabulary the log writes — an operator who greps `sessiometer.log`
    /// for `outcome=` sees the identical token `list` shows.
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            RefreshEventOutcome::Refreshed => "refreshed",
            RefreshEventOutcome::RefreshedNotReStashed => "refreshed_not_restashed",
            RefreshEventOutcome::NoChange => "no_change",
            RefreshEventOutcome::Dead => "dead",
            RefreshEventOutcome::Error => "error",
        }
    }

    /// Parse an `outcome=` token back into its variant — the inverse of [`as_str`],
    /// for reading the last-persisted refresh outcome out of the event log (issue
    /// #120). `None` for an unrecognized token (a truncated / future / corrupt line),
    /// so a malformed record is skipped rather than mis-classified.
    ///
    /// [`as_str`]: RefreshEventOutcome::as_str
    pub(crate) fn from_token(token: &str) -> Option<Self> {
        Some(match token {
            "refreshed" => RefreshEventOutcome::Refreshed,
            "refreshed_not_restashed" => RefreshEventOutcome::RefreshedNotReStashed,
            "no_change" => RefreshEventOutcome::NoChange,
            "dead" => RefreshEventOutcome::Dead,
            "error" => RefreshEventOutcome::Error,
            _ => return None,
        })
    }
}

/// WHY an isolated-refresh cycle produced an `outcome=error` — the non-secret `reason=` sub-class
/// of an error [`Event::Refresh`] line (issue #377). The event-level mirror of the engine's
/// [`crate::refresh::RefreshErrorReason`], carrying one variant the engine cannot: [`Timeout`],
/// the tick's `tokio::time::timeout` bound firing — never a value a completed cycle produces (the
/// same event-adds-a-variant split by which [`RefreshEventOutcome`] adds `RefreshedNotReStashed`).
/// A FIXED, secret-free-BY-CONSTRUCTION classification: it makes a wholesale refresh failure (the
/// #375 incident — every account a bare `error` for hours) diagnosable from the log without ever
/// folding a token / path / email onto the #15 channel.
///
/// Rendered ONLY on an error line, and ONLY for a sub-cause classifiable secret-free — a hard
/// engine `Err` (a locked keychain, a contended lock, an FS error, an unresolved binary) has no
/// such class, so it stays a bare `outcome=error` with no `reason=`.
///
/// [`Timeout`]: RefreshEventReason::Timeout
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RefreshEventReason {
    /// The `claude` binary resolved but could not be spawned / exec'd.
    SpawnFailed,
    /// The read-back item was unreadable.
    ReadbackUnreadable,
    /// The stored / read-back blob was unparseable.
    Malformed,
    /// The cycle exceeded `[refresh].timeout_secs` — the tick's whole-cycle timeout bound (the
    /// one sub-cause detected OUTSIDE a completed engine cycle, hence event-level only).
    Timeout,
}

impl RefreshEventReason {
    /// The `reason=` token — the same snake_case grep vocabulary as the rest of this module.
    fn as_str(self) -> &'static str {
        match self {
            RefreshEventReason::SpawnFailed => "spawn_failed",
            RefreshEventReason::ReadbackUnreadable => "readback_unreadable",
            RefreshEventReason::Malformed => "malformed",
            RefreshEventReason::Timeout => "timeout",
        }
    }
}

/// What prompted an in-place ACTIVE-account keep-warm cycle (issue #282) — the `trigger=`
/// token of an [`Event::KeepWarm`] line. Unlike the poll-path [`Event::PollRefresh`]'s fixed
/// `poll_401` literal, keep-warm fires from two distinct conditions, so the discriminant is a
/// carried enum field: a `proactive` mint scheduled before the active token nears expiry, or a
/// `reactive` backstop mint on an active usage-401 (revive the canonical before the 401 counts
/// toward the #42 death streak). A non-secret classification only — never a token or email.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum KeepWarmTrigger {
    /// A scheduled mint fired because the active token entered its (staggered) near-expiry
    /// horizon — the pre-emptive path that keeps the token warm BEFORE any 401.
    Proactive,
    /// A backstop mint fired on an active usage-401, reviving the canonical in place before
    /// the 401 advances the #42 death streak toward a false quarantine.
    Reactive,
}

impl KeepWarmTrigger {
    /// The `trigger=` token — the same grep vocabulary the rest of the event log uses.
    fn as_str(self) -> &'static str {
        match self {
            KeepWarmTrigger::Proactive => "proactive",
            KeepWarmTrigger::Reactive => "reactive",
        }
    }
}

/// The 4-state per-account CREDENTIAL-health rollup `status` surfaces (issue #119): the
/// daemon-computed verdict the thin read-only `status` client just projects to a glyph
/// (🟢/🟡/🟠/🔴). Lives HERE — the base observability module, with no `daemon`
/// dependency — so the [`Event::CredentialHealth`] transition event can name it without
/// a `daemon` ↔ `observability` dependency cycle; `daemon` (which computes it) and `cli`
/// (which renders it) both import it.
///
/// Non-secret by construction: a bare classification, never a token, an expiry, or an
/// email — the same #15 discipline as [`RefreshEventOutcome`]. The four SEVERITY variants
/// are ordered `Healthy` < `Stale` < `AtRisk` < `Dead`, matching the issue's green →
/// yellow → orange → red ladder; `Unknown` (issue #137) is OFF that severity axis — a
/// non-active account with NO positive-liveness evidence (never successfully polled, no
/// refresh telemetry, no refresh-sourced expiry), reported honestly rather than as a false
/// 🟢. It sits just above `Healthy` in declaration order, the "unverified" caution.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum CredentialHealth {
    /// Access token valid and the refresh path working. 🟢 Requires a POSITIVE liveness
    /// signal (a fresh successful poll, refresh telemetry, or a refresh-sourced expiry);
    /// absence of a NEGATIVE signal alone is not health (issue #137) — that is `Unknown`.
    #[default]
    Healthy,
    /// No positive-liveness EVIDENCE yet (issue #137): a non-active account never
    /// successfully polled, `[refresh]` off (no telemetry, no refresh-sourced expiry, no
    /// fresh reading). Distinct from `Healthy` — the daemon cannot vouch for the credential,
    /// so it says so rather than a false 🟢 that would jump straight to 🔴 the moment the
    /// #42 401-streak quarantines it. ⚪
    Unknown,
    /// The stored access token has EXPIRED but the refresh token is still valid — a
    /// transient window the next refresh recovers. 🟡 (least severe non-healthy state).
    Stale,
    /// The refresh safety-net is FAILING (a streak of refresh errors): the mechanism that
    /// prevents staleness/death is struggling, so the account trends toward dead even
    /// while its token may still work for now. 🟠
    AtRisk,
    /// The credential is DEAD — quarantined (the #42 401-streak verdict) OR its refresh
    /// token was cleared in place — and needs an operator `claude /login`. 🔴
    Dead,
}

impl CredentialHealth {
    /// The `state=` token for the [`Event::CredentialHealth`] log line. Matches the
    /// `snake_case` serde rename so the event log and the `--json` wire agree.
    fn as_str(self) -> &'static str {
        match self {
            CredentialHealth::Healthy => "healthy",
            CredentialHealth::Unknown => "unknown",
            CredentialHealth::Stale => "stale",
            CredentialHealth::AtRisk => "at_risk",
            CredentialHealth::Dead => "dead",
        }
    }
}

/// The outcome of one `sessiometer login` invocation (issue #135) — the `outcome=` token of the
/// single redacted [`Event::Login`] the verb emits. The four terminal states of a login:
/// `Onboarded` / `Revived` map from the reconcile's [`crate::capture::LoginOutcome`] (a new vs an
/// already-rostered account), while `Failed` / `Cancelled` cover the paths that never reach a
/// reconcile — an error (e.g. a locked keychain aborts one-shot) and an operator abandon
/// (timeout / SIGINT), respectively. A bare classification, never a token or email (#15).
#[cfg_attr(not(test), allow(dead_code))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LoginEventOutcome {
    /// The harvested account was NEW to the roster — a fresh entry was appended.
    Onboarded,
    /// The harvested account was ALREADY in the roster — its entry was updated in place and its
    /// stash re-pointed to the fresh credential. The canonical item is re-pointed too (the
    /// re-login that un-quarantines a parked account in place) ONLY when the login becomes active
    /// (#274: it IS the current active account, or none is); a `Revived` event for a NON-active
    /// account means the stash + roster were refreshed while the active slot was preserved.
    Revived,
    /// The login did not land: the capture engine or the reconcile returned an error (e.g. a
    /// locked keychain, a spawn failure). Nothing was written to the roster.
    Failed,
    /// The operator did not complete the login within the timeout, or cancelled it (SIGINT):
    /// nothing was captured.
    Cancelled,
}

impl LoginEventOutcome {
    /// The `outcome=` token for the [`Event::Login`] log line.
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            LoginEventOutcome::Onboarded => "onboarded",
            LoginEventOutcome::Revived => "revived",
            LoginEventOutcome::Failed => "failed",
            LoginEventOutcome::Cancelled => "cancelled",
        }
    }
}

/// The outcome of one daemon-routed `capture` control command (issue #359) — the `outcome=`
/// token of the single redacted [`Event::Capture`] the run loop emits. Folds the two SUCCESS
/// kinds and the redacted FAILURE tags into ONE self-describing token (the same single-field
/// shape [`LoginEventOutcome`] uses): a new account (`Captured`) vs an idempotent refresh of an
/// already-rostered one (`Refreshed`), and — on refusal — the SAME bare machine reason the
/// redacted ack carries (`CaptureRejection`) folded onto this axis, so a `grep` of the event log
/// tells WHY a capture failed. A bare classification, never a token or email (#15).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CaptureEventOutcome {
    /// A NEW account was captured into the roster.
    Captured,
    /// An already-rostered active account was refreshed IN PLACE — no duplicate row.
    Refreshed,
    /// No active account to capture: not logged in to Claude Code (absent / unreadable identity),
    /// or the canonical credential is gone.
    NoActiveAccount,
    /// The keychain was LOCKED when the daemon went to read the active credential — a SAFETY
    /// abort; the capture read nothing and wrote nothing (retry when unlocked).
    KeychainLocked,
    /// The single-writer swap lock (#64) stayed held the whole bounded wait — fail-closed, ZERO
    /// work (no read, no stash, no roster write).
    SwapLockBusy,
    /// The capture aborted for another reason (an I/O error, or the post-stash roster save failed).
    Failed,
}

impl CaptureEventOutcome {
    /// The `outcome=` token for the [`Event::Capture`] log line.
    fn as_str(self) -> &'static str {
        match self {
            CaptureEventOutcome::Captured => "captured",
            CaptureEventOutcome::Refreshed => "refreshed",
            CaptureEventOutcome::NoActiveAccount => "no_active_account",
            CaptureEventOutcome::KeychainLocked => "keychain_locked",
            CaptureEventOutcome::SwapLockBusy => "swap_lock_busy",
            CaptureEventOutcome::Failed => "failed",
        }
    }
}

/// Whether an `export` carried credential material or only the roster/config — the `mode=` of the
/// redacted [`Event::Export`] (issue #150). A bare classification (never a handle, token, or
/// email): the SAME #15 discipline as the other outcome enums here. Maps from the `export` verb's
/// `--no-secrets` flag.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ExportMode {
    /// A full export: every account's credential + `oauthAccount` material travels (+ the roster).
    Full,
    /// A config-only export (`--no-secrets`): the roster + tunables travel, but no credential
    /// material — each imported account then needs a re-`login`.
    ConfigOnly,
}

impl ExportMode {
    /// The `mode=` token. Snake_case, matching every other token in this module's `key=val`
    /// grammar — the issue's illustrative `config-only` is spelled `config_only` for that house
    /// consistency (a hyphen would not split the grammar, but the underscore matches the rest).
    fn as_str(self) -> &'static str {
        match self {
            ExportMode::Full => "full",
            ExportMode::ConfigOnly => "config_only",
        }
    }
}

/// The whole-`import` verdict — the `outcome=` of the redacted [`Event::Import`] (issue #150),
/// DERIVED from the per-account outcome counts rather than stored. A rollup of what happened:
/// `ok` (nothing failed), `partial` (some failed, some landed / were skipped), or `failed` (every
/// account failed). Non-secret — a bare verdict token.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ImportRollup {
    /// No account failed (all imported / skipped / overwritten, or an empty artifact).
    Ok,
    /// At least one account failed AND at least one did not (imported / skipped / overwritten).
    Partial,
    /// Every account failed — none imported, skipped, or overwritten.
    Failed,
}

impl ImportRollup {
    /// Roll the per-account outcome counts up into the whole-import verdict. `skipped` counts as a
    /// NON-failure — an already-present account intentionally left untouched by the conflict policy
    /// is a success, not a failure — so a mix of skips and failures is `Partial`, and only an
    /// all-failed import (nothing imported / skipped / overwritten) is `Failed`.
    fn from_counts(imported: u32, skipped: u32, overwritten: u32, failed: u32) -> Self {
        if failed == 0 {
            ImportRollup::Ok
        } else if imported + skipped + overwritten == 0 {
            ImportRollup::Failed
        } else {
            ImportRollup::Partial
        }
    }

    /// The `outcome=` token.
    fn as_str(self) -> &'static str {
        match self {
            ImportRollup::Ok => "ok",
            ImportRollup::Partial => "partial",
            ImportRollup::Failed => "failed",
        }
    }
}

/// One observable daemon state change, rendered as a single `key=val` log line by
/// [`Event::to_log_line`].
///
/// Every field is a handle / enum / number / timestamp — never a token or email
/// (issue #15). That is the type-level guarantee behind the single-surface
/// redaction claim in the module docs.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Event {
    /// The active credential was rotated away from `from` to `to` because `reason`
    /// reached its trigger; `session_pct` is the outgoing account's session usage
    /// (percent) at swap time. When `session_pct >= 100` the account was already at
    /// the usage ceiling, so [`Event::to_log_line`] appends a `late=true` marker
    /// (issue #365); the marker is omitted otherwise, leaving the in-band swap line
    /// unchanged. It is a formatter-derived flag, not a stored field.
    ///
    /// `from`/`to` are HANDLES (operator labels), NOT roster indices — unlike the
    /// same-named fields of [`crate::daemon::TickAction::Swapped`].
    Swap {
        from: String,
        to: String,
        reason: SwapReason,
        session_pct: u8,
    },
    /// `account`'s canonical credential changed underneath the daemon — the
    /// operator ran `claude /login` and re-authenticated it — so its stash was
    /// refreshed to the new token (issue #13 re-auth re-stash). `account` is the
    /// HANDLE (operator label), resolved from the new canonical's identity.
    ReStash { account: String },
    /// The active account is over a trigger but no other account is a viable swap
    /// target — the all-exhausted terminal state (issue #11). `hold` is the
    /// least-bad account the daemon holds on: the one whose weekly window resets
    /// soonest. `resets_at` is that account's weekly reset as epoch seconds,
    /// rendered to RFC 3339 by [`Event::to_log_line`] and present whenever the API
    /// supplied a parseable timestamp; `None` (the field is omitted) when no
    /// account reported one, keeping the line forward-compatible.
    AllExhausted {
        hold: String,
        resets_at: Option<i64>,
    },
    /// `account`'s stored token was rejected with HTTP 401 `consecutive` times in a
    /// row — the climbing streak toward the dead-credential threshold (issue #42).
    /// Emitted per 401 while the account is still healthy; once it crosses
    /// `monitor_401_n` and is quarantined, the streak stops being logged (the
    /// [`Event::CredentialDead`] transition is signaled instead, and a quarantined
    /// account is no longer polled). Distinct from a re-stash, which is driven by
    /// canonical-change detection ([`Event::ReStash`]).
    Monitor401 { account: String, consecutive: u32 },
    /// `account`'s stored credential is DEAD: its token was rejected `monitor_401_n`
    /// times in a row, so the daemon quarantines it — it stops polling and selecting
    /// it for the rotation until the operator re-logs-in (issue #42). Edge-triggered:
    /// emitted exactly ONCE on the death transition, never per failed poll. The
    /// durable "needs re-login" status is surfaced separately by `status`. `account`
    /// is the HANDLE (operator label) — never a token or email.
    CredentialDead { account: String },
    /// The ACTIVE account's credential died, blocking the live session, so the daemon
    /// emergency-swapped from `from` to `to` — the soonest-reset viable account —
    /// bypassing the normal swap-away trigger and post-swap cooldown (issue #42).
    /// Edge-triggered: exactly ONE per emergency swap. `from`/`to` are HANDLES
    /// (operator labels), never tokens or emails.
    EmergencySwap { from: String, to: String },
    /// A quarantined (dead) `account` recovered: the operator re-logged-in (its
    /// canonical credential changed and was re-stashed, #13) and it then polled
    /// successfully `monitor_recovery_m` times in a row, so the daemon un-quarantined
    /// it and returned it to the rotation (issue #42). Edge-triggered: exactly ONCE
    /// on the recovery transition. `account` is the HANDLE — never a token or email.
    CredentialRestored { account: String },
    /// `account`'s refresh token is confirmed DEAD and UNRECOVERABLE by automation: a
    /// quarantined account's isolated #106-sweep refresh returned `outcome=dead` (the
    /// stored refresh token is revoked/empty), so no daemon path can revive it — only
    /// an operator `claude /login` mints a new one (issue #261).
    ///
    /// Distinct from [`Event::CredentialDead`], and the distinction is load-bearing:
    /// `credential_dead` is the QUARANTINE edge (the access token was rejected
    /// `monitor_401_n` times) and is NOT terminal — it still auto-recovers via the
    /// #106 sweep, spontaneous revival, or a re-login. `credential_unrecoverable` fires
    /// only once those automated recoveries are EXHAUSTED (the sweep's own refresh came
    /// back dead), so it is the operator's cue to act. Edge-triggered: exactly ONCE per
    /// quarantine episode (a sticky per-account latch, reset when the account
    /// re-quarantines), never per sweep re-probe. `account` is the HANDLE (operator
    /// label) — never a token or email (issue #15).
    CredentialUnrecoverable { account: String },
    /// The keychain was locked when the daemon went to read the canonical
    /// credential, so this tick's work is deferred and the daemon backs off (issue
    /// #13). Edge-triggered: emitted ONCE when the lock is first observed, not every
    /// tick it stays locked. No `account` — a locked keychain is a process-global
    /// condition (every account's stash is unreadable), not tied to one account.
    KeychainLockedWait,
    /// `account`'s token authenticated but lacks the usage scope (HTTP 403) — the
    /// hallmark of a non-interactive setup token (#5). Always `status=403`.
    UsageScopeFail { account: String },
    /// The periodic isolated-refresh tick (#105) ran one cycle for the PARKED `account`
    /// (issue #106): `outcome` is the non-secret classification, and `expires_before` /
    /// `expires_after` are the stored token's `expiresAt` (epoch milliseconds) before and
    /// after the cycle — each `None` only when the stored expiry was unreadable. `account`
    /// is the HANDLE (operator label), never a token or email; the expiry is a plain
    /// timestamp. A cycle that refreshes a quarantined account back to life additionally
    /// drives a separate [`Event::CredentialRestored`] (the restore, applied daemon-side).
    Refresh {
        account: String,
        outcome: RefreshEventOutcome,
        expires_before: Option<i64>,
        expires_after: Option<i64>,
        /// Whether the cycle ROTATED the refresh token (issue #279), sourced from the
        /// cycle's [`crate::refresh::RefreshReport`] `refresh_token_rotated` — the AC-3
        /// rotation signal made durable. A boolean only, never either token value, so the
        /// #15 single-surface guarantee holds. A cycle that produced no report (a hard
        /// engine `Err` / whole-cycle timeout — `refresh_tick::error_refresh_event`)
        /// renders `false`.
        refresh_token_rotated: bool,
        /// The non-secret error sub-class on an `outcome=error` line (issue #377), rendered as
        /// an additive TRAILING `reason=` field (precedent: `late=` / `rotated=`) so existing
        /// `key=val` parsers are unaffected. `Some` ONLY on an error whose sub-cause is
        /// classifiable secret-free (the three completed-cycle sub-causes plus `timeout`);
        /// `None` on every non-error outcome AND on a hard engine `Err` that carries no such
        /// class — in both cases NO `reason=` is rendered. A [`RefreshEventReason`] is a fixed
        /// enum token, never dynamic error text, so it cannot carry a secret onto the line.
        reason: Option<RefreshEventReason>,
    },
    /// The `#162` poll-path refresh-then-retry fired for the PARKED `account` (issue #255): on
    /// the FIRST usage-401 of a streak episode the daemon ran ONE isolated refresh (the #102
    /// engine) plus a single re-poll, hoping a merely-expired access token is revived before it
    /// counts toward the #42 death streak. This records the *action* — that an isolated refresh
    /// fired and how it classified — the durable complement to the DOWNSTREAM poll outcome
    /// ([`Event::Monitor401`] / [`Event::CredentialDead`]) that
    /// [`crate::daemon`]'s `note_poll_outcome` already logs. `outcome` is the same non-secret
    /// [`RefreshEventOutcome`] projection [`Event::Refresh`] carries (one shared refresh-outcome
    /// vocabulary); the trigger is the fixed `poll_401` — the only condition that fires this path,
    /// and never for the ACTIVE account (issue #253). `account` is the HANDLE (operator label),
    /// never a token or email — the #15 single-surface guarantee.
    PollRefresh {
        account: String,
        outcome: RefreshEventOutcome,
        /// Whether the poll-path cycle ROTATED the refresh token (issue #279), sourced from
        /// the cycle's [`crate::refresh::RefreshReport`] `refresh_token_rotated` — the same
        /// non-secret rotation signal [`Event::Refresh`] carries. A boolean only, never a
        /// token value (the #15 single-surface guarantee); an engine `Err` (could-not-run)
        /// renders `false`.
        refresh_token_rotated: bool,
    },
    /// The in-place ACTIVE-account keep-warm (issue #282, the FOURTH refresh mechanism) ran one
    /// cycle for `account`: the daemon minted a fresh token by driving `claude` and — on a real
    /// refresh — PROMOTED it to the canonical `Claude Code-credentials` item a live session reads
    /// (never the STASH the #253-excluded engine writes). Records the *action* — that a keep-warm
    /// fired, what `trigger`ed it ([`KeepWarmTrigger::Proactive`] near-expiry schedule vs
    /// [`KeepWarmTrigger::Reactive`] active-401 backstop), and how it classified. `outcome` is the
    /// same non-secret [`RefreshEventOutcome`] projection [`Event::Refresh`] / [`Event::PollRefresh`]
    /// carry (one shared refresh-outcome vocabulary): `refreshed_not_restashed` on a real mint (a
    /// keep-warm PROMOTES rather than re-stashes, so it never renders `refreshed`), else
    /// `no_change` / `dead` / `error`. `account` is the HANDLE (operator label), never a token or
    /// email — the #15 single-surface guarantee.
    KeepWarm {
        account: String,
        trigger: KeepWarmTrigger,
        outcome: RefreshEventOutcome,
        /// Whether the keep-warm cycle ROTATED the refresh token (issue #279/#282), sourced from
        /// the cycle's [`crate::refresh::RefreshReport`] `refresh_token_rotated` — the same
        /// non-secret rotation signal [`Event::Refresh`] carries. A boolean only, never a token
        /// value (the #15 single-surface guarantee); an engine `Err` (could-not-run) renders
        /// `false`.
        refresh_token_rotated: bool,
    },
    /// The refresh MECHANISM is systemically DOWN (issue #378): `consecutive` refresh sweeps in a
    /// row failed with `outcome=error` across EVERY eligible (parked, allowlisted) account — a
    /// failure of the mechanism itself (a stale `claude` path #375, a wedged spawn), not one
    /// account's credentials. Edge-triggered: emitted exactly ONCE per episode, on the sweep that
    /// crosses the configured threshold ([`crate::config::RefreshConfig::systemic_failure_n`]),
    /// NOT re-emitted per subsequent all-error sweep; the matching [`Event::RefreshSystemicRecovered`]
    /// signals the clear. Distinct from the per-account [`Event::CredentialHealth`] `at_risk`
    /// rollup — visible without waiting for any account to die. Carries only the COUNT — never a
    /// token, path, or email (the #15 single-surface guarantee).
    RefreshSystemicFailure { consecutive: u32 },
    /// The refresh mechanism RECOVERED from a systemic failure (issue #378): after a
    /// [`Event::RefreshSystemicFailure`] episode, a sweep produced at least one non-`error`
    /// refresh cycle, so the mechanism demonstrably works again. Edge-triggered: emitted exactly
    /// ONCE, on the first working sweep that clears the episode — the closing bracket of the
    /// systemic-failure edge, mirroring the [`Event::CredentialDead`] / [`Event::CredentialRestored`]
    /// two-edge idiom at the refresh-MECHANISM scope. No fields — a daemon-global recovery with
    /// nothing account-specific to carry (#15).
    RefreshSystemicRecovered,
    /// `account`'s 4-state credential-health rollup (issue #119) TRANSITIONED to `state`
    /// this cycle. Edge-triggered: emitted exactly ONCE per change (not per poll while
    /// the state holds), so the event log carries the per-account health timeline. The
    /// daemon computes the rollup ([`crate::daemon`]) and diffs it across cycles; the
    /// very first observation SEEDS the baseline silently (no transition to report).
    /// `account` is the HANDLE (operator label); `state` is a bare classification —
    /// never a token, expiry, or email (the #15 single-surface guarantee).
    CredentialHealth {
        account: String,
        state: CredentialHealth,
    },
    /// One `sessiometer login` invocation completed (issue #135) — the single redacted audit line
    /// the verb emits. `account` is the operator HANDLE (label or, for a post-harvest failure, the
    /// account uuid) — a redacted, non-PII handle, never the email or token; it is `None` when no
    /// account was ever identified (a cancel before completion, or a failure before harvest), in
    /// which case the `account=` field is omitted. `outcome` is the terminal classification.
    Login {
        account: Option<String>,
        outcome: LoginEventOutcome,
    },
    /// One daemon-routed `capture` control command completed (issue #359) — the single redacted
    /// audit line the run loop emits after performing the capture inside the swap lock (the #357
    /// `capture_locked` primitive). `account` is the operator HANDLE (the resolved roster label on
    /// success, or the operator-supplied label hint on a failure) — a redacted, non-PII handle,
    /// never the email or token; it is `None` when no account handle was available (a failure with
    /// no label hint), in which case the `account=` field is omitted. `outcome` is the terminal
    /// classification (a capture / an idempotent refresh, or a redacted failure tag).
    Capture {
        account: Option<String>,
        outcome: CaptureEventOutcome,
    },
    /// The usage-stats store compacted and rolled aged raw samples down into its hourly/daily
    /// aggregates (issue #161). Edge-ish: emitted only when a pass actually folds something
    /// (`raw_lines > 0`), so a no-op maintenance pass is silent. `rolled_through` is the roll
    /// watermark AFTER the pass (the newest sample epoch now folded, epoch seconds, rendered to
    /// RFC 3339); `raw_lines` is how many raw samples that pass folded. Store-global — NO
    /// `account` field (a roll spans every account's samples), and every field is a plain
    /// integer / timestamp, never a handle, token, or email.
    UsageRollup { rolled_through: i64, raw_lines: u32 },
    /// A poll produced no reading for `account`, so the usage-stats store recorded no sample
    /// for it (issue #161, honouring #156's gap-honesty: a gap is an ABSENCE, never a fabricated
    /// zero). Rate-limited by the daemon (at most one per account per re-emit interval) rather
    /// than per failed poll. `account` is the operator HANDLE (label) — never a token or email;
    /// `since` is the epoch second the current gap streak began (rendered to RFC 3339), fixed
    /// across a streak's re-emissions so the line reads "gapping since X".
    UsageGap { account: String, since: i64 },
    /// An out-of-band `claude /login` rewrote the canonical credential to a token that maps to
    /// NO roster account — an UN-CAPTURED login the daemon detected but does NOT auto-onboard
    /// (issue #140 scope decision: `sessiometer login` is the sanctioned capture path). Surfaced
    /// so the operator knows to run it. Edge-triggered: emitted ONCE per distinct un-captured
    /// login (the daemon commits the canonical baseline after surfacing, so the same blob is not
    /// re-detected). `account_uuid` is the displayed `accountUuid` when readable — a redacted,
    /// non-PII handle (as #135's post-harvest `Login` uses), never a token or email — and `None`
    /// (the field omitted) when the display identity could not be read.
    UncapturedLogin { account_uuid: Option<String> },
    /// A `sessiometer export` wrote a migration artifact (issue #150) — the single redacted audit
    /// line the verb emits. `accounts` is the roster size exported, `encrypted` whether the
    /// artifact is passphrase-encrypted (vs `--plaintext`), and `mode` whether credential material
    /// travelled ([`ExportMode::Full`]) or only the roster/config (`--no-secrets`,
    /// [`ExportMode::ConfigOnly`]). Carries NO account field — aggregate count + a bool + a mode
    /// token only, so nothing account-specific (never a handle, token, or email) reaches the line.
    Export {
        accounts: u32,
        encrypted: bool,
        mode: ExportMode,
    },
    /// A `sessiometer import` rehydrated accounts from a migration artifact (issue #150) — the
    /// single redacted audit line the verb emits. Carries the per-account outcome COUNTS only:
    /// `imported` (new), `skipped` (already present, left untouched), `overwritten` (replaced), and
    /// `failed` (a credential write / read-back verify failed). The line derives `accounts=` (their
    /// sum) and the `outcome=` rollup ([`ImportRollup`]) from them. NO account field — aggregate
    /// counts only, so nothing account-specific (never a handle, token, or email) reaches the line.
    Import {
        imported: u32,
        skipped: u32,
        overwritten: u32,
        failed: u32,
    },
}

impl Event {
    /// Render this event as its single log line (no trailing newline), stamped
    /// with `ts`.
    ///
    /// Pure and the *only* place an event becomes text, so the redaction surface
    /// (#15) is exactly this method. The timestamp is a parameter (not read here)
    /// so the formatting is deterministically unit-testable; [`EventLog::emit`]
    /// supplies `SystemTime::now()` at write time.
    pub(crate) fn to_log_line(&self, ts: SystemTime) -> String {
        let ts = rfc3339(ts);
        match self {
            Event::Swap {
                from,
                to,
                reason,
                session_pct,
            } => {
                let reason = reason.as_str();
                // `late=true` marks a swap whose outgoing account was already at the
                // usage ceiling (`session_pct >= 100`) when it fired (issue #365).
                // Appended only when true — a trailing `key=val` existing parsers
                // ignore, mirroring the optional `resets_at` / `expires_*` fields —
                // so a normal in-band swap line is byte-for-byte unchanged. A
                // number-derived bool: no new field type, no new redaction surface (#15).
                let late = if *session_pct >= 100 {
                    " late=true"
                } else {
                    ""
                };
                format!(
                    "ts={ts} event=swap from={from} to={to} reason={reason} session_pct={session_pct}{late}"
                )
            }
            Event::ReStash { account } => {
                format!("ts={ts} event=restash account={account}")
            }
            Event::AllExhausted { hold, resets_at } => match resets_at {
                Some(secs) => {
                    let resets_at = rfc3339(system_time_from_epoch(*secs));
                    format!("ts={ts} event=all_exhausted hold={hold} resets_at={resets_at}")
                }
                None => format!("ts={ts} event=all_exhausted hold={hold}"),
            },
            Event::Monitor401 {
                account,
                consecutive,
            } => {
                format!("ts={ts} event=monitor_401 account={account} consecutive={consecutive}")
            }
            Event::CredentialDead { account } => {
                format!("ts={ts} event=credential_dead account={account}")
            }
            Event::EmergencySwap { from, to } => {
                format!("ts={ts} event=emergency_swap from={from} to={to}")
            }
            Event::CredentialRestored { account } => {
                format!("ts={ts} event=credential_restored account={account}")
            }
            Event::CredentialUnrecoverable { account } => {
                format!("ts={ts} event=credential_unrecoverable account={account}")
            }
            Event::KeychainLockedWait => {
                format!("ts={ts} event=keychain_locked_wait")
            }
            Event::UsageScopeFail { account } => {
                format!("ts={ts} event=usage_scope_fail account={account} status=403")
            }
            Event::Refresh {
                account,
                outcome,
                expires_before,
                expires_after,
                refresh_token_rotated,
                reason,
            } => {
                let outcome = outcome.as_str();
                // Each expiry is omitted when unreadable (an empty value after `=` would
                // split the key=val grammar — mirrors `all_exhausted`'s optional
                // `resets_at`). The epoch-ms timestamp is rendered to whole-second RFC 3339
                // through the SAME formatter as the line `ts` and `resets_at`.
                let before = match expires_before {
                    Some(ms) => {
                        format!(
                            " expires_before={}",
                            rfc3339(system_time_from_epoch(ms / 1000))
                        )
                    }
                    None => String::new(),
                };
                let after = match expires_after {
                    Some(ms) => {
                        format!(
                            " expires_after={}",
                            rfc3339(system_time_from_epoch(ms / 1000))
                        )
                    }
                    None => String::new(),
                };
                // `rotated=` is always present (a bare bool) and trails the optional
                // expiry fields (issue #279) — the AC-3 rotation signal made durable. It sits
                // AFTER `outcome=`, so `last_refresh_outcomes`' ` outcome=`-then-first-token
                // parse is unaffected.
                //
                // `reason=` (issue #377) trails the WHOLE line — after the always-present
                // `rotated=` — mirroring the swap line's optional trailing `late=`. Present
                // ONLY on an error whose sub-cause is classifiable secret-free; omitted
                // otherwise (a non-error outcome, or a hard `Err`), so a normal refresh line is
                // byte-for-byte unchanged and every existing `key=val` parser is unaffected.
                let reason = match reason {
                    Some(reason) => format!(" reason={}", reason.as_str()),
                    None => String::new(),
                };
                format!(
                    "ts={ts} event=refresh account={account} outcome={outcome}{before}{after} rotated={refresh_token_rotated}{reason}"
                )
            }
            Event::PollRefresh {
                account,
                outcome,
                refresh_token_rotated,
            } => {
                // The isolated poll-refresh ACTION (issue #255). The trigger is the fixed
                // `poll_401` — the only condition that fires the #162 path — rendered as a literal
                // (a single-valued discriminant needs no enum field); `outcome` reuses the SAME
                // non-secret token vocabulary `event=refresh` renders. The DISTINCT `poll_refresh`
                // event name keeps it clear of the periodic #106 `event=refresh` line that the
                // `list` view's [`last_refresh_outcomes`] reader parses.
                let outcome = outcome.as_str();
                // `rotated=` trails `outcome=` (issue #279) — the same non-secret rotation
                // signal `event=refresh` carries, on the poll path.
                format!(
                    "ts={ts} event=poll_refresh account={account} trigger=poll_401 outcome={outcome} rotated={refresh_token_rotated}"
                )
            }
            Event::KeepWarm {
                account,
                trigger,
                outcome,
                refresh_token_rotated,
            } => {
                // The in-place keep-warm ACTION (issue #282). `trigger=` carries the
                // proactive-vs-reactive discriminant (a two-valued condition, unlike
                // `poll_refresh`'s fixed `poll_401`); `outcome` reuses the SAME non-secret token
                // vocabulary `event=refresh` renders. The DISTINCT `keep_warm` event name keeps it
                // clear of both the periodic #106 `event=refresh` line and the #162 `poll_refresh`
                // line — three separate refresh mechanisms, three separate event names.
                let trigger = trigger.as_str();
                let outcome = outcome.as_str();
                format!(
                    "ts={ts} event=keep_warm account={account} trigger={trigger} outcome={outcome} rotated={refresh_token_rotated}"
                )
            }
            Event::RefreshSystemicFailure { consecutive } => {
                // The systemic refresh-mechanism-down edge (issue #378): the streak count is the
                // only field — no account (a whole-mechanism condition), no token/path (#15).
                format!("ts={ts} event=refresh_systemic_failure consecutive={consecutive}")
            }
            Event::RefreshSystemicRecovered => {
                // The closing edge of the #378 systemic-failure episode — a daemon-global recovery
                // with nothing account-specific to carry.
                format!("ts={ts} event=refresh_systemic_recovered")
            }
            Event::CredentialHealth { account, state } => {
                let state = state.as_str();
                format!("ts={ts} event=credential_health account={account} state={state}")
            }
            Event::Login { account, outcome } => {
                let outcome = outcome.as_str();
                // The account handle is omitted when absent (a cancel/early-failure that never
                // identified an account) — mirroring `all_exhausted`'s optional `resets_at`, so
                // the key=val grammar never carries an empty `account=`.
                match account {
                    Some(handle) => {
                        format!("ts={ts} event=login account={handle} outcome={outcome}")
                    }
                    None => format!("ts={ts} event=login outcome={outcome}"),
                }
            }
            Event::Capture { account, outcome } => {
                let outcome = outcome.as_str();
                // The account handle is omitted when absent (a failure with no label hint) —
                // mirroring `login`'s optional `account=`, so the key=val grammar never carries an
                // empty `account=`. The handle is a redacted label; `outcome` is a bare tag (#15).
                match account {
                    Some(handle) => {
                        format!("ts={ts} event=capture account={handle} outcome={outcome}")
                    }
                    None => format!("ts={ts} event=capture outcome={outcome}"),
                }
            }
            Event::UsageRollup {
                rolled_through,
                raw_lines,
            } => {
                // `rolled_through` is epoch seconds, rendered through the SAME formatter as the
                // line `ts` (and `resets_at` / refresh expiries), so watermarks read uniformly.
                let rolled_through = rfc3339(system_time_from_epoch(*rolled_through));
                format!(
                    "ts={ts} event=usage_rollup rolled_through={rolled_through} raw_lines={raw_lines}"
                )
            }
            Event::UsageGap { account, since } => {
                // `account` is the handle; `since` is the gap-streak start, rendered to RFC 3339
                // through the shared formatter. Both non-secret (the #15 single-surface guarantee).
                let since = rfc3339(system_time_from_epoch(*since));
                format!("ts={ts} event=usage_gap acct={account} since={since}")
            }
            Event::UncapturedLogin { account_uuid } => match account_uuid {
                // The `acct=` handle is omitted when the display identity was unreadable — an
                // empty value would split the `key=val` grammar (mirrors `all_exhausted`'s
                // optional `resets_at`). The uuid is a redacted, non-PII handle (never a token
                // or email — the #15 single-surface guarantee).
                Some(uuid) => format!("ts={ts} event=uncaptured_login acct={uuid}"),
                None => format!("ts={ts} event=uncaptured_login"),
            },
            Event::Export {
                accounts,
                encrypted,
                mode,
            } => {
                // Aggregate-only: a count, a bool, and a mode token — no account field, so nothing
                // account-specific reaches the line (the #15 guarantee holds here trivially, the
                // export event having no per-account field at all).
                let mode = mode.as_str();
                format!(
                    "ts={ts} event=export accounts={accounts} encrypted={encrypted} mode={mode}"
                )
            }
            Event::Import {
                imported,
                skipped,
                overwritten,
                failed,
            } => {
                // `accounts=` is the total processed — every account gets exactly one outcome, so
                // the sum — and `outcome=` the rollup derived from the counts. Aggregate-only: no
                // account field, so no per-account identity reaches the line.
                let accounts = imported + skipped + overwritten + failed;
                let outcome =
                    ImportRollup::from_counts(*imported, *skipped, *overwritten, *failed).as_str();
                format!(
                    "ts={ts} event=import accounts={accounts} outcome={outcome} \
                     imported={imported} skipped={skipped} overwritten={overwritten} failed={failed}"
                )
            }
        }
    }
}

/// A [`SystemTime`] from epoch seconds — used to render an `all_exhausted`
/// event's `resets_at` (issue #11) through the same [`rfc3339`] formatter as the
/// line timestamp, so reset times read identically regardless of whether the API
/// gave an epoch or an ISO string. A negative (pre-epoch) input is not expected
/// for a reset time but is handled so this best-effort log path can never panic
/// (it renders via `rfc3339`'s epoch sentinel).
fn system_time_from_epoch(secs: i64) -> SystemTime {
    if secs >= 0 {
        UNIX_EPOCH + Duration::from_secs(secs as u64)
    } else {
        UNIX_EPOCH - Duration::from_secs(secs.unsigned_abs())
    }
}

/// Format a wall-clock instant as whole-second UTC RFC 3339 (`YYYY-MM-DDTHH:MM:SSZ`).
///
/// Dependency-free (there is no date crate in the graph): epoch seconds → civil
/// date via [`civil_from_days`]. Events are second-granular, so no fractional part
/// is emitted. A pre-1970 clock (a `duration_since` error) renders as the epoch — a
/// clearly-wrong but safe sentinel, so a skewed clock can never panic a log write
/// (the daemon's logging is best-effort).
fn rfc3339(ts: SystemTime) -> String {
    let secs = ts
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let days = (secs / 86_400) as i64;
    let rem = secs % 86_400;
    let (hour, minute, second) = (rem / 3600, (rem % 3600) / 60, rem % 60);
    let (year, month, day) = civil_from_days(days);
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}Z")
}

/// Convert a count of days since 1970-01-01 to a `(year, month, day)` proleptic
/// Gregorian civil date — Howard Hinnant's `civil_from_days`. Correct across leap
/// years and the 100/400 century rules (e.g. 2000 is a leap year, 2100 is not).
/// Every intermediate is non-negative for the post-epoch range we format, so the
/// `as u32` narrowings on the final month/day (each well within range) cannot lose
/// information. [`rfc3339`] only ever passes `z >= 0` (a pre-epoch clock renders as
/// the epoch sentinel), so Hinnant's negative-`z` branch is retained verbatim but
/// unreached here.
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = (if z >= 0 { z } else { z - 146_096 }) / 146_097;
    let doe = z - era * 146_097; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365; // [0, 399]
    let year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let day = doy - (153 * mp + 2) / 5 + 1; // [1, 31]
    let month = if mp < 10 { mp + 3 } else { mp - 9 }; // [1, 12]
    (year + i64::from(month <= 2), month as u32, day as u32)
}

/// The path of the structured event log: `sessiometer.log` under the native log
/// directory (`~/Library/Logs/sessiometer/`, #1).
///
/// Factored out as the single source of truth for the filename so its two
/// consumers cannot drift: [`EventLog::open`] (which writes it) and the one-shot
/// `use` verb's cooldown gate (#63), which reads the durable swap record from the
/// same file via [`last_swap_at`].
pub(crate) fn log_path() -> Result<std::path::PathBuf> {
    Ok(paths::logs_dir()?.join("sessiometer.log"))
}

/// The structured event log at `~/Library/Logs/sessiometer/sessiometer.log`
/// (`0600`).
pub(crate) struct EventLog {
    file: File,
}

impl EventLog {
    /// Open the event log, creating the log directory (`0700`) and file (`0600`)
    /// if needed.
    pub(crate) fn open() -> Result<Self> {
        let path = log_path()?;
        // `log_path()` is always `<logs_dir>/sessiometer.log`, so the parent is the
        // native log directory — ensure it (`0700`) before creating the file.
        paths::ensure_private_dir(
            path.parent()
                .expect("log_path() always has a logs-dir parent"),
        )?;
        let file = paths::create_private_file(&path)?;
        Ok(Self { file })
    }

    /// Append `event` as exactly one line, stamped with the current wall-clock
    /// time. The line is built whole and written in a single `write_all`, so a
    /// concurrent reader (Console.app) never observes a torn line.
    pub(crate) fn emit(&mut self, event: &Event) -> Result<()> {
        let mut line = event.to_log_line(SystemTime::now());
        line.push('\n');
        self.file.write_all(line.as_bytes())?;
        Ok(())
    }

    /// Open an event log at an explicit path (tests only), bypassing the native
    /// log directory so the run loop can be exercised hermetically.
    #[cfg(test)]
    pub(crate) fn at(path: &std::path::Path) -> Result<Self> {
        Ok(Self {
            file: paths::create_private_file(path)?,
        })
    }
}

/// The wall-clock instant of the MOST RECENT swap recorded in the event log at
/// `path`, or `None` when the log is absent/unreadable or records no swap.
///
/// The durable, daemon-INDEPENDENT swap record the one-shot `use` verb (#63)
/// consults for its cooldown gate (#10): the daemon's in-memory `last_swap` is not
/// persisted (it is surfaced only over the live control socket), so the structured
/// log — which records every swap through [`Event::to_log_line`] — is the only
/// source a standalone command can read. Both a normal `event=swap` (now including
/// the `use` verb's own `reason=manual|forced`) and an `event=emergency_swap` update
/// the daemon's cooldown floor, so both count here. Best-effort: an unreadable file
/// or an unparseable timestamp yields `None`, so a one-shot manual swap is never
/// blocked by a missing or corrupt log (the cooldown then reads as inactive).
pub(crate) fn last_swap_at(path: &std::path::Path) -> Option<SystemTime> {
    let text = std::fs::read_to_string(path).ok()?;
    // Scan from the END: the log is append-only chronological, so the last swap
    // line is the most recent swap. The surrounding spaces anchor the event key so
    // a label that merely contains the text cannot be mistaken for it.
    let line = text
        .lines()
        .rev()
        .find(|line| line.contains(" event=swap ") || line.contains(" event=emergency_swap "))?;
    let raw_ts = line.strip_prefix("ts=")?.split(' ').next()?;
    let epoch = crate::usage::epoch_from_rfc3339(raw_ts)?;
    // The log only ever writes post-epoch instants; guard the cast so a malformed
    // pre-epoch stamp degrades to `None` rather than wrapping into a wrong instant.
    (epoch >= 0).then(|| UNIX_EPOCH + Duration::from_secs(epoch as u64))
}

/// The LAST-persisted refresh outcome per account in the event log at `path`, keyed by
/// account HANDLE — the daemon-INDEPENDENT read the offline `list` view (issue #120)
/// surfaces alongside each account's stored-token expiry.
///
/// The `status` view computes its 4-state rollup live in the daemon (issue #119); when
/// the daemon is down — often exactly when a wedged credential needs inspecting — that
/// path is unavailable, so `list` reads the durable record the refresh sweep already
/// wrote: each [`Event::Refresh`] line (`event=refresh account={handle} outcome={token}`,
/// issue #106). The SAME file-read sibling of [`last_swap_at`] — scan the append-only,
/// chronological log; the last `refresh` line for a handle is its most recent outcome.
///
/// One pass (not one read per account): later lines overwrite earlier, so each handle
/// ends mapped to its newest outcome. The account field is anchored between the literal
/// ` event=refresh account=` prefix and the ` outcome=` that always immediately follows
/// it (the [`Event::to_log_line`] grammar), so a handle is matched verbatim — a handle
/// that merely *contains* ` outcome=` truncates to "no recognized outcome" (skipped)
/// rather than mis-attributing. Best-effort like [`last_swap_at`]: an absent/unreadable
/// log yields an empty map, so `list` simply omits the refresh tag.
///
/// Non-secret: the event log is itself a redaction-metered surface (issue #15) — every
/// line is a handle / enum / timestamp — so the returned handles and outcomes carry no
/// token or email.
pub(crate) fn last_refresh_outcomes(
    path: &std::path::Path,
) -> std::collections::HashMap<String, RefreshEventOutcome> {
    let mut outcomes = std::collections::HashMap::new();
    let Ok(text) = std::fs::read_to_string(path) else {
        return outcomes;
    };
    for line in text.lines() {
        // ts=… event=refresh account={handle} outcome={token}[ expires_before=…][ expires_after=…]
        let Some((_, rest)) = line.split_once(" event=refresh account=") else {
            continue;
        };
        let Some((handle, after)) = rest.split_once(" outcome=") else {
            continue;
        };
        let token = after.split(' ').next().unwrap_or(after);
        if let Some(outcome) = RefreshEventOutcome::from_token(token) {
            // Last line wins: the log is chronological, so the final insert per handle
            // is its most recent refresh outcome.
            outcomes.insert(handle.to_owned(), outcome);
        }
    }
    outcomes
}

/// Operator-facing diagnostic verbosity (issue #77) for the `run` daemon. Default
/// [`Quiet`](Self::Quiet) — no console spam without opt-in; `-v`/`--verbose` selects
/// [`Verbose`](Self::Verbose). The gate is applied by [`DiagnosticLog::emit`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Verbosity {
    /// Default: drop every diagnostic line (the diagnostic channel is silent).
    Quiet,
    /// Emit per-poll, per-tick, and lifecycle diagnostics to the channel sink.
    Verbose,
}

/// The per-poll outcome class on the DIAGNOSTIC channel (issue #77) — the operator
/// taxonomy that SEPARATES a rate-limit (`429`) from a generic transient (`5xx` /
/// network / unreadable), unlike the daemon's poll classification
/// ([`crate::daemon`]'s health-machine `PollOutcome`), which folds both into one
/// transient class. The two views are deliberately different: a rate-limit storm and
/// a flaky network read are the same to the back-off, but an operator staring at the
/// channel needs to tell "I am being throttled" apart from "the endpoint is flaky".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PollClass {
    /// A successful usage reading — the credential is alive.
    Live,
    /// HTTP 401 — the stored token was rejected.
    Unauthorized,
    /// HTTP 403 — authenticated but lacking the usage scope (issue #5).
    Scope,
    /// HTTP 429 — rate-limited; the daemon backs off (issue #76).
    RateLimited,
    /// Any other failure (`5xx` / network / unreadable token / unparseable body) —
    /// a generic transient carrying no liveness signal.
    Transient,
}

impl PollClass {
    /// The `outcome=` token.
    fn as_str(self) -> &'static str {
        match self {
            PollClass::Live => "live",
            PollClass::Unauthorized => "unauthorized",
            PollClass::Scope => "scope",
            PollClass::RateLimited => "rate_limited",
            PollClass::Transient => "transient",
        }
    }
}

/// The per-tick DECISION class on the diagnostic channel (issue #77) — the operator
/// rendering of the daemon's per-cycle verdict, one token per
/// [`crate::daemon::TickAction`]. The swap PARTICIPANTS (the from/to handles) are
/// deliberately NOT carried here: they already ride the event log's `swap` line and
/// the foreground swap echo, so the diagnostic decision line stays a pure label.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DecisionClass {
    /// Active is below the swap-away trigger — stay put.
    Hold,
    /// Swapped the active credential to a viable target.
    Swap,
    /// Emergency-swapped away from a dead active account (issue #42).
    EmergencySwap,
    /// Over the trigger but no viable target — the all-exhausted hold (issue #11).
    AllExhausted,
    /// The active credential is dead and no target is viable — held, unable to
    /// escape (issue #42).
    ActiveDeadNoTarget,
    /// The active account could not be identified — poll-only.
    SkipActiveUnknown,
    /// The active account's reading was unavailable this cycle — never swap on
    /// missing data.
    SkipActiveUnavailable,
    /// Over the trigger but within the post-swap cooldown (issue #10).
    SkipCooldown,
    /// A swap was attempted but the engine returned an error (#6 no-half-swap).
    SwapFailed,
    /// The keychain was locked — the whole tick was deferred (issue #13).
    KeychainLocked,
}

impl DecisionClass {
    /// The `decision=` token.
    fn as_str(self) -> &'static str {
        match self {
            DecisionClass::Hold => "hold",
            DecisionClass::Swap => "swap",
            DecisionClass::EmergencySwap => "emergency_swap",
            DecisionClass::AllExhausted => "all_exhausted",
            DecisionClass::ActiveDeadNoTarget => "active_dead_no_target",
            DecisionClass::SkipActiveUnknown => "skip_active_unknown",
            DecisionClass::SkipActiveUnavailable => "skip_active_unavailable",
            DecisionClass::SkipCooldown => "skip_cooldown",
            DecisionClass::SwapFailed => "swap_failed",
            DecisionClass::KeychainLocked => "keychain_locked",
        }
    }
}

/// One operator-facing diagnostic line (issue #77), rendered by the single
/// [`Diagnostic::to_log_line`] formatter — the diagnostic channel's redaction
/// surface, the sibling of [`Event::to_log_line`].
///
/// Every field is a HANDLE (an operator label), an enum, a number, or a timestamp —
/// never a token or email (issue #15). That type-level constraint is what lets this
/// channel reuse the event log's redaction guarantee without weakening it: the #15
/// METER scans rendered diagnostics alongside events, and there is no field through
/// which a secret could reach the line.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Diagnostic {
    /// The daemon started: the effective config summary, so one run's lines can be
    /// read against the configuration that produced them. `accounts` is the roster
    /// size; the rest are the swap/poll tunables — counts and percentages only, no
    /// handle.
    Start {
        accounts: usize,
        poll_secs: u64,
        session_floor: Option<u8>,
        session_trigger: u8,
        weekly_trigger: u8,
        monitor_401_n: u8,
        monitor_recovery_m: u8,
    },
    /// The daemon is stopping on a clean shutdown (SIGINT / SIGTERM).
    Stop,
    /// One account's poll outcome this tick: its handle and the outcome class.
    Poll { account: String, outcome: PollClass },
    /// The per-tick decision, plus the back-off wait this tick imposed when any —
    /// the locked-keychain back-off (issue #13) or the rate-limit / transient
    /// back-off (issue #76). `None` ⇒ the field is omitted and the next poll uses
    /// the normal jittered interval.
    ///
    /// `retry_after_secs` LABELS the SOURCE of that wait (issue #295): the RAW
    /// server-advised `Retry-After` (delta-seconds, BEFORE the `POLL_BACKOFF_CAP`
    /// clamp) the throttled poll's response supplied, when any. `Some` ⇒ the server
    /// advised a floor; `None` ⇒ the wait is the daemon's self-capped exponential (or
    /// the keychain-lock back-off), with no server advice. It disambiguates a
    /// `backoff_secs` an operator otherwise cannot place, by comparison: absent ⇒
    /// self-capped exponential; `== backoff_secs` ⇒ the server-advised wait governed;
    /// `< backoff_secs` ⇒ the server advised a smaller floor but the exponential governed;
    /// `> backoff_secs` ⇒ the #294 cap clamped a pathological value (e.g.
    /// `backoff_secs=3600 retry_after_secs=86400`). Pre-cap on purpose, so that clamped
    /// value stays visible rather than collapsing into an indistinguishable `backoff_secs=3600`.
    Tick {
        decision: DecisionClass,
        backoff_secs: Option<u64>,
        retry_after_secs: Option<u64>,
    },
    /// The daemon LEFT the all-exhausted terminal state (issue #11): a viable swap
    /// target is possible again. The edge-triggered LEAVE marker — the symmetric
    /// partner of the event log's edge-triggered `all_exhausted` ENTER — so a stale
    /// "all exhausted" reading from an earlier episode can be told from a current
    /// one (the very confusion that motivated #77).
    AllExhaustedCleared,
}

impl Diagnostic {
    /// Render this diagnostic as its single line (no trailing newline), stamped with
    /// `ts`. Pure and the *only* place a diagnostic becomes text, so — exactly like
    /// [`Event::to_log_line`] — the redaction surface is this method alone. `ts` is a
    /// parameter (not read here) so the formatting is deterministically unit-testable;
    /// [`DiagnosticLog::emit`] supplies `SystemTime::now()` at write time.
    pub(crate) fn to_log_line(&self, ts: SystemTime) -> String {
        let ts = rfc3339(ts);
        match self {
            Diagnostic::Start {
                accounts,
                poll_secs,
                session_floor,
                session_trigger,
                weekly_trigger,
                monitor_401_n,
                monitor_recovery_m,
            } => {
                // session_floor is opt-in (#10): render the disabled state as an
                // explicit `off` sentinel rather than omitting the key, so the
                // summary always STATES whether the swap-target session guard is on.
                let session_floor = match session_floor {
                    Some(floor) => floor.to_string(),
                    None => "off".to_owned(),
                };
                format!(
                    "ts={ts} diag=start accounts={accounts} poll_secs={poll_secs} \
                     session_floor={session_floor} session_trigger={session_trigger} \
                     weekly_trigger={weekly_trigger} monitor_401_n={monitor_401_n} \
                     monitor_recovery_m={monitor_recovery_m}"
                )
            }
            Diagnostic::Stop => format!("ts={ts} diag=stop"),
            Diagnostic::Poll { account, outcome } => {
                let outcome = outcome.as_str();
                format!("ts={ts} diag=poll account={account} outcome={outcome}")
            }
            Diagnostic::Tick {
                decision,
                backoff_secs,
                retry_after_secs,
            } => {
                let decision = decision.as_str();
                // Each optional field is rendered only when present — an empty value after
                // `=` would split the `key=val` grammar (mirrors `all_exhausted`'s optional
                // `resets_at`). `backoff_secs` is the wait imposed; `retry_after_secs` LABELS
                // its source (issue #295) — the raw server-advised `Retry-After`, present iff
                // the server sent one, so a server-driven wait is told apart from the
                // daemon's self-capped exponential.
                let backoff = match backoff_secs {
                    Some(secs) => format!(" backoff_secs={secs}"),
                    None => String::new(),
                };
                let retry_after = match retry_after_secs {
                    Some(secs) => format!(" retry_after_secs={secs}"),
                    None => String::new(),
                };
                format!("ts={ts} diag=tick decision={decision}{backoff}{retry_after}")
            }
            Diagnostic::AllExhaustedCleared => format!("ts={ts} diag=all_exhausted_cleared"),
        }
    }
}

/// The operator-facing diagnostic SINK (issue #77): writes each [`Diagnostic`] as one
/// line when [`Verbosity::Verbose`], and DROPS every line when [`Verbosity::Quiet`]
/// (the default — no console spam without opt-in). Generic over its `Write` sink:
/// production wires `std::io::stderr()` — the foreground daemon's operator channel,
/// where the lifecycle line and swap echo already go — while tests wire a `Vec<u8>`
/// and read the buffer back.
pub(crate) struct DiagnosticLog<W> {
    sink: W,
    verbosity: Verbosity,
}

impl<W: Write> DiagnosticLog<W> {
    /// Wrap `sink`, emitting only when `verbosity` is [`Verbosity::Verbose`].
    pub(crate) fn new(sink: W, verbosity: Verbosity) -> Self {
        Self { sink, verbosity }
    }

    /// Emit `diag` as one stamped line — unless [`Verbosity::Quiet`], when it is
    /// dropped before any work. Best-effort like the event log: a diagnostic write
    /// failure must never kill the daemon, so a write error is ignored (the
    /// diagnostic channel is a debugging aid, not a durable guarantee).
    pub(crate) fn emit(&mut self, diag: &Diagnostic) {
        if self.verbosity == Verbosity::Quiet {
            return;
        }
        let mut line = diag.to_log_line(SystemTime::now());
        line.push('\n');
        let _ = self.sink.write_all(line.as_bytes());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    /// A fixed wall-clock instant `secs` after the epoch, for deterministic `ts=`.
    fn at_epoch(secs: u64) -> SystemTime {
        UNIX_EPOCH + Duration::from_secs(secs)
    }

    // --- rfc3339 / civil_from_days (the dependency-free date math) ----------

    #[test]
    fn rfc3339_renders_the_epoch_and_a_time_of_day() {
        // Epoch, then a within-day split, then the last second of the first day —
        // pins the H:M:S derivation and the zero-padding of single-digit fields.
        assert_eq!(rfc3339(at_epoch(0)), "1970-01-01T00:00:00Z");
        assert_eq!(rfc3339(at_epoch(3_661)), "1970-01-01T01:01:01Z");
        assert_eq!(rfc3339(at_epoch(86_399)), "1970-01-01T23:59:59Z");
    }

    #[test]
    fn rfc3339_handles_leap_years_and_the_century_rules() {
        // %4 leap year (1972-02-29), the 400-rule leap year (2000-02-29 exists),
        // and the 100-not-400 NON-leap year (2100 has no Feb 29: Feb 28 → Mar 1).
        // Ground truth from macOS `date -u`.
        assert_eq!(rfc3339(at_epoch(68_169_600)), "1972-02-29T00:00:00Z");
        assert_eq!(rfc3339(at_epoch(951_782_400)), "2000-02-29T00:00:00Z");
        assert_eq!(rfc3339(at_epoch(951_868_800)), "2000-03-01T00:00:00Z");
        assert_eq!(rfc3339(at_epoch(4_107_456_000)), "2100-02-28T00:00:00Z");
        assert_eq!(rfc3339(at_epoch(4_107_542_400)), "2100-03-01T00:00:00Z");
    }

    #[test]
    fn rfc3339_renders_recent_and_far_future_instants() {
        assert_eq!(rfc3339(at_epoch(1_735_689_600)), "2025-01-01T00:00:00Z");
        assert_eq!(rfc3339(at_epoch(1_750_960_800)), "2025-06-26T18:00:00Z");
        // The largest 4-digit year — proves the `{year:04}` width holds at the top.
        assert_eq!(rfc3339(at_epoch(253_402_300_799)), "9999-12-31T23:59:59Z");
    }

    #[test]
    fn rfc3339_treats_a_pre_epoch_clock_as_the_epoch_sentinel() {
        // A clock set before 1970 yields a `duration_since` error; rather than
        // panic a best-effort log write, it renders the epoch sentinel.
        let before = UNIX_EPOCH - Duration::from_secs(1);
        assert_eq!(rfc3339(before), "1970-01-01T00:00:00Z");
    }

    // --- Event::to_log_line (the single redaction surface) ------------------

    const TS0: &str = "ts=1970-01-01T00:00:00Z";

    #[test]
    fn swap_line_carries_handles_reason_and_session_pct() {
        let line = Event::Swap {
            from: "work".to_owned(),
            to: "spare".to_owned(),
            reason: SwapReason::Session,
            session_pct: 97,
        }
        .to_log_line(at_epoch(0));
        assert_eq!(
            line,
            format!("{TS0} event=swap from=work to=spare reason=session session_pct=97")
        );
    }

    #[test]
    fn swap_line_marks_late_when_at_or_above_the_ceiling() {
        // `session_pct >= 100` — the outgoing account was already at the usage
        // ceiling, so the line carries a `late=true` marker (issue #365), appended
        // after `session_pct` as a trailing key=val.
        let line = Event::Swap {
            from: "work".to_owned(),
            to: "spare".to_owned(),
            reason: SwapReason::Session,
            session_pct: 100,
        }
        .to_log_line(at_epoch(0));
        assert_eq!(
            line,
            format!("{TS0} event=swap from=work to=spare reason=session session_pct=100 late=true")
        );
    }

    #[test]
    fn swap_line_omits_late_below_the_ceiling() {
        // Below the ceiling the marker is absent — a normal in-band swap line is
        // byte-for-byte what it was before the field existed (backward-compatible,
        // issue #365). 99 pins the `>= 100` boundary from just below.
        let line = Event::Swap {
            from: "work".to_owned(),
            to: "spare".to_owned(),
            reason: SwapReason::Session,
            session_pct: 99,
        }
        .to_log_line(at_epoch(0));
        assert_eq!(
            line,
            format!("{TS0} event=swap from=work to=spare reason=session session_pct=99")
        );
        assert!(!line.contains("late"), "got: {line}");
    }

    #[test]
    fn swap_line_renders_the_weekly_reason() {
        let line = Event::Swap {
            from: "work".to_owned(),
            to: "spare".to_owned(),
            reason: SwapReason::Weekly,
            session_pct: 40,
        }
        .to_log_line(at_epoch(0));
        assert!(line.contains("reason=weekly"), "got: {line}");
    }

    #[test]
    fn all_exhausted_renders_resets_at_when_known_and_omits_it_otherwise() {
        // No reset reported (#11 fallback) → the field is simply absent and the
        // line stays well-formed.
        let absent = Event::AllExhausted {
            hold: "work".to_owned(),
            resets_at: None,
        }
        .to_log_line(at_epoch(0));
        assert_eq!(absent, format!("{TS0} event=all_exhausted hold=work"));
        assert!(!absent.contains("resets_at"), "got: {absent}");

        // A known reset (epoch seconds, #11) is rendered to RFC 3339 by the same
        // single formatter — 1_782_777_600 is 2026-06-30T00:00:00Z.
        let present = Event::AllExhausted {
            hold: "work".to_owned(),
            resets_at: Some(1_782_777_600),
        }
        .to_log_line(at_epoch(0));
        assert_eq!(
            present,
            format!("{TS0} event=all_exhausted hold=work resets_at=2026-06-30T00:00:00Z")
        );
    }

    #[test]
    fn monitor_401_carries_the_account_and_consecutive_count() {
        let line = Event::Monitor401 {
            account: "work".to_owned(),
            consecutive: 3,
        }
        .to_log_line(at_epoch(0));
        assert_eq!(
            line,
            format!("{TS0} event=monitor_401 account=work consecutive=3")
        );
    }

    #[test]
    fn credential_dead_carries_only_the_account_handle() {
        let line = Event::CredentialDead {
            account: "work".to_owned(),
        }
        .to_log_line(at_epoch(0));
        assert_eq!(line, format!("{TS0} event=credential_dead account=work"));
    }

    #[test]
    fn emergency_swap_carries_the_from_and_to_handles() {
        let line = Event::EmergencySwap {
            from: "work".to_owned(),
            to: "spare".to_owned(),
        }
        .to_log_line(at_epoch(0));
        assert_eq!(
            line,
            format!("{TS0} event=emergency_swap from=work to=spare")
        );
    }

    #[test]
    fn credential_restored_carries_only_the_account_handle() {
        let line = Event::CredentialRestored {
            account: "work".to_owned(),
        }
        .to_log_line(at_epoch(0));
        assert_eq!(
            line,
            format!("{TS0} event=credential_restored account=work")
        );
    }

    #[test]
    fn credential_unrecoverable_carries_only_the_account_handle() {
        let line = Event::CredentialUnrecoverable {
            account: "work".to_owned(),
        }
        .to_log_line(at_epoch(0));
        assert_eq!(
            line,
            format!("{TS0} event=credential_unrecoverable account=work")
        );
    }

    #[test]
    fn refresh_systemic_failure_carries_only_the_consecutive_count() {
        // Issue #378: the systemic-down edge renders just the streak count — no account, no path,
        // no token (a whole-mechanism condition, #15-clean by construction).
        let line = Event::RefreshSystemicFailure { consecutive: 3 }.to_log_line(at_epoch(0));
        assert_eq!(
            line,
            format!("{TS0} event=refresh_systemic_failure consecutive=3")
        );
    }

    #[test]
    fn refresh_systemic_recovered_carries_nothing_account_specific() {
        // Issue #378: the recovery edge is a bare daemon-global line — nothing to leak.
        let line = Event::RefreshSystemicRecovered.to_log_line(at_epoch(0));
        assert_eq!(line, format!("{TS0} event=refresh_systemic_recovered"));
    }

    #[test]
    fn restash_carries_the_account_handle() {
        let line = Event::ReStash {
            account: "work".to_owned(),
        }
        .to_log_line(at_epoch(0));
        assert_eq!(line, format!("{TS0} event=restash account=work"));
    }

    #[test]
    fn keychain_locked_wait_is_accountless() {
        // A locked keychain is process-global, so the line carries no account —
        // just the event name and timestamp (issue #13).
        let line = Event::KeychainLockedWait.to_log_line(at_epoch(0));
        assert_eq!(line, format!("{TS0} event=keychain_locked_wait"));
    }

    #[test]
    fn login_renders_the_handle_and_outcome() {
        // The redacted `login` audit line (issue #135): the account is the operator HANDLE, the
        // outcome its terminal classification — never a token or email.
        let line = Event::Login {
            account: Some("work".to_owned()),
            outcome: LoginEventOutcome::Onboarded,
        }
        .to_log_line(at_epoch(0));
        assert_eq!(
            line,
            format!("{TS0} event=login account=work outcome=onboarded")
        );
    }

    #[test]
    fn login_omits_the_account_when_absent() {
        // A cancel (or a failure before any account was identified) carries no handle, so the
        // `account=` field is omitted — the line stays well-formed, like `all_exhausted` without
        // a `resets_at`.
        let line = Event::Login {
            account: None,
            outcome: LoginEventOutcome::Cancelled,
        }
        .to_log_line(at_epoch(0));
        assert_eq!(line, format!("{TS0} event=login outcome=cancelled"));
        assert!(!line.contains("account="), "got: {line}");
    }

    #[test]
    fn login_renders_each_outcome_token() {
        // All four terminal outcomes render their exact `outcome=` token (issue #135 AC).
        for (outcome, token) in [
            (LoginEventOutcome::Onboarded, "onboarded"),
            (LoginEventOutcome::Revived, "revived"),
            (LoginEventOutcome::Failed, "failed"),
            (LoginEventOutcome::Cancelled, "cancelled"),
        ] {
            assert_eq!(outcome.as_str(), token);
            let line = Event::Login {
                account: Some("work".to_owned()),
                outcome,
            }
            .to_log_line(at_epoch(0));
            assert_eq!(
                line,
                format!("{TS0} event=login account=work outcome={token}")
            );
        }
    }

    #[test]
    fn usage_scope_fail_carries_the_account_and_constant_403() {
        let line = Event::UsageScopeFail {
            account: "work".to_owned(),
        }
        .to_log_line(at_epoch(0));
        assert_eq!(
            line,
            format!("{TS0} event=usage_scope_fail account=work status=403")
        );
    }

    #[test]
    fn refresh_line_carries_handle_outcome_and_optional_expiries() {
        // A successful refresh: handle + outcome token + the before/after expiry rendered
        // to RFC 3339 (epoch ms → whole-second UTC), so the slide forward is visible.
        let refreshed = Event::Refresh {
            account: "spare".to_owned(),
            outcome: RefreshEventOutcome::Refreshed,
            expires_before: Some(1_782_777_600_000),
            expires_after: Some(1_782_781_200_000), // +1h
            refresh_token_rotated: true,
            reason: None, // a success carries no error reason (#377)
        }
        .to_log_line(at_epoch(0));
        assert_eq!(
            refreshed,
            format!(
                "{TS0} event=refresh account=spare outcome=refreshed \
                 expires_before=2026-06-30T00:00:00Z expires_after=2026-06-30T01:00:00Z \
                 rotated=true"
            )
        );
        assert!(!refreshed.contains("reason="), "got: {refreshed}");

        // An unreadable expiry: both expiry fields are OMITTED (never an empty value that
        // would split the key=val grammar), yet `rotated=` still renders — it trails the
        // (absent) expiries directly after `outcome=` (issue #279). With `reason: None`
        // (e.g. a hard `Err`), no `reason=` rides the error line either (#377).
        let unknown = Event::Refresh {
            account: "spare".to_owned(),
            outcome: RefreshEventOutcome::Error,
            expires_before: None,
            expires_after: None,
            refresh_token_rotated: false,
            reason: None,
        }
        .to_log_line(at_epoch(0));
        assert_eq!(
            unknown,
            format!("{TS0} event=refresh account=spare outcome=error rotated=false")
        );
        assert!(!unknown.contains("expires_"), "got: {unknown}");
        assert!(!unknown.contains("reason="), "got: {unknown}");
    }

    #[test]
    fn refresh_error_line_carries_the_trailing_reason() {
        // Issue #377: the non-secret error sub-class rides an additive TRAILING `reason=` field,
        // AFTER the always-present `rotated=` (mirroring the swap line's optional trailing
        // `late=`), so a normal-outcome line is byte-for-byte unchanged and existing `key=val`
        // parsers are unaffected. Each fixed class renders its documented token.
        for (reason, token) in [
            (RefreshEventReason::SpawnFailed, "spawn_failed"),
            (
                RefreshEventReason::ReadbackUnreadable,
                "readback_unreadable",
            ),
            (RefreshEventReason::Malformed, "malformed"),
            (RefreshEventReason::Timeout, "timeout"),
        ] {
            let line = Event::Refresh {
                account: "spare".to_owned(),
                outcome: RefreshEventOutcome::Error,
                expires_before: None,
                expires_after: None,
                refresh_token_rotated: false,
                reason: Some(reason),
            }
            .to_log_line(at_epoch(0));
            assert_eq!(
                line,
                format!(
                    "{TS0} event=refresh account=spare outcome=error rotated=false reason={token}"
                )
            );
        }
    }

    #[test]
    fn poll_refresh_line_carries_handle_trigger_and_outcome() {
        // Issue #255: the #162 poll-refresh ACTION renders the redacted handle, the fixed
        // `trigger=poll_401`, and the outcome token — the SAME vocabulary `event=refresh` uses,
        // under a DISTINCT `poll_refresh` event name (so it never collides with the periodic
        // #106 refresh line the `list` view's `last_refresh_outcomes` reader parses).
        let dead = Event::PollRefresh {
            account: "spare".to_owned(),
            outcome: RefreshEventOutcome::Dead,
            refresh_token_rotated: false,
        }
        .to_log_line(at_epoch(0));
        assert_eq!(
            dead,
            format!(
                "{TS0} event=poll_refresh account=spare trigger=poll_401 outcome=dead rotated=false"
            )
        );
        // The `outcome=` token tracks the variant (the shared refresh vocabulary); `rotated=`
        // trails it (issue #279), and no expiry fields ride this line (unlike `event=refresh`).
        let refreshed = Event::PollRefresh {
            account: "spare".to_owned(),
            outcome: RefreshEventOutcome::Refreshed,
            refresh_token_rotated: true,
        }
        .to_log_line(at_epoch(0));
        assert_eq!(
            refreshed,
            format!(
                "{TS0} event=poll_refresh account=spare trigger=poll_401 outcome=refreshed rotated=true"
            )
        );
        assert!(!refreshed.contains("expires_"), "got: {refreshed}");
    }

    #[test]
    fn usage_rollup_carries_the_watermark_and_raw_line_count() {
        // Issue #161: the store rolled `raw_lines` samples through to `rolled_through`
        // (epoch seconds → whole-second RFC 3339 via the shared formatter). Store-global —
        // no `account` field.
        let line = Event::UsageRollup {
            rolled_through: 1_782_777_600, // 2026-06-30T00:00:00Z
            raw_lines: 288,
        }
        .to_log_line(at_epoch(0));
        assert_eq!(
            line,
            format!("{TS0} event=usage_rollup rolled_through=2026-06-30T00:00:00Z raw_lines=288")
        );
        assert!(!line.contains("account"), "rollup is store-global: {line}");
        assert!(!line.contains("acct="), "rollup is store-global: {line}");
    }

    #[test]
    fn usage_gap_carries_the_handle_and_streak_start() {
        // Issue #161: a no-reading poll surfaces the account HANDLE + the gap-streak start
        // (`since`, epoch seconds → RFC 3339). Handle-only identity — never a token or email.
        let line = Event::UsageGap {
            account: "work".to_owned(),
            since: 1_782_777_600, // 2026-06-30T00:00:00Z
        }
        .to_log_line(at_epoch(0));
        assert_eq!(
            line,
            format!("{TS0} event=usage_gap acct=work since=2026-06-30T00:00:00Z")
        );
    }

    #[test]
    fn usage_rollup_and_gap_lines_carry_no_pii() {
        // The #15 single-surface guarantee for the #161 events: every rendered field is a
        // handle, an integer, or a timestamp — never an email or token. Even the gap event's
        // only free field is the operator handle we passed; there is no separate identity
        // field that could leak an email/token.
        let rollup = Event::UsageRollup {
            rolled_through: 1_782_777_600,
            raw_lines: 5,
        }
        .to_log_line(at_epoch(0));
        let gap = Event::UsageGap {
            account: "work".to_owned(),
            since: 1_782_777_600,
        }
        .to_log_line(at_epoch(0));
        for line in [&rollup, &gap] {
            assert!(!line.contains('@'), "no email may appear: {line}");
            // The refresh/login events use `outcome=`/`token`; ours carry no credential field.
            assert!(!line.contains("token"), "no token may appear: {line}");
            assert!(!line.contains("Bearer"), "no bearer may appear: {line}");
            assert!(!line.contains("sk-ant"), "no api key may appear: {line}");
        }
    }

    #[test]
    fn uncaptured_login_renders_with_and_without_the_uuid() {
        // Issue #140: an un-captured login surfaces the displayed uuid as an `acct=` handle when
        // readable, and omits the field entirely when it is not (an empty value would split the
        // `key=val` grammar — the same optional-field discipline as `all_exhausted`'s `resets_at`).
        let with_uuid = Event::UncapturedLogin {
            account_uuid: Some("u-Z".to_owned()),
        }
        .to_log_line(at_epoch(0));
        assert_eq!(with_uuid, format!("{TS0} event=uncaptured_login acct=u-Z"));

        let without_uuid = Event::UncapturedLogin { account_uuid: None }.to_log_line(at_epoch(0));
        assert_eq!(without_uuid, format!("{TS0} event=uncaptured_login"));

        // The #15 single-surface guarantee: the only free field is the redacted uuid handle —
        // never an email or token.
        assert!(!with_uuid.contains('@'), "no email may appear: {with_uuid}");
        assert!(
            !with_uuid.contains("token"),
            "no token may appear: {with_uuid}"
        );
    }

    #[test]
    fn refresh_renders_each_outcome_token() {
        for (outcome, token) in [
            (RefreshEventOutcome::Refreshed, "refreshed"),
            (
                RefreshEventOutcome::RefreshedNotReStashed,
                "refreshed_not_restashed",
            ),
            (RefreshEventOutcome::NoChange, "no_change"),
            (RefreshEventOutcome::Dead, "dead"),
            (RefreshEventOutcome::Error, "error"),
        ] {
            let line = Event::Refresh {
                account: "work".to_owned(),
                outcome,
                expires_before: None,
                expires_after: None,
                refresh_token_rotated: false,
                reason: None, // outcome-token render only — the #377 reason has its own test
            }
            .to_log_line(at_epoch(0));
            assert_eq!(
                line,
                format!("{TS0} event=refresh account=work outcome={token} rotated=false")
            );
        }
    }

    #[test]
    fn no_event_line_carries_an_email_or_token_sigil() {
        // #15: every field is a handle / enum / number / timestamp, so a token or
        // email can never reach a rendered line. Handles here are plain labels.
        let events = [
            Event::Swap {
                from: "work".to_owned(),
                to: "spare".to_owned(),
                reason: SwapReason::Session,
                session_pct: 97,
            },
            Event::AllExhausted {
                hold: "work".to_owned(),
                resets_at: Some(1_782_777_600),
            },
            Event::ReStash {
                account: "work".to_owned(),
            },
            Event::Monitor401 {
                account: "work".to_owned(),
                consecutive: 2,
            },
            Event::CredentialDead {
                account: "work".to_owned(),
            },
            Event::EmergencySwap {
                from: "work".to_owned(),
                to: "spare".to_owned(),
            },
            Event::CredentialRestored {
                account: "work".to_owned(),
            },
            Event::KeychainLockedWait,
            Event::UsageScopeFail {
                account: "work".to_owned(),
            },
            Event::Refresh {
                account: "work".to_owned(),
                outcome: RefreshEventOutcome::Refreshed,
                expires_before: Some(1_782_777_600_000),
                expires_after: Some(1_782_781_200_000),
                refresh_token_rotated: true,
                reason: None,
            },
            Event::CredentialHealth {
                account: "work".to_owned(),
                state: CredentialHealth::Dead,
            },
            Event::Login {
                account: Some("work".to_owned()),
                outcome: LoginEventOutcome::Onboarded,
            },
            Event::Login {
                account: None,
                outcome: LoginEventOutcome::Cancelled,
            },
        ];
        for event in &events {
            let line = event.to_log_line(at_epoch(0));
            assert!(!line.contains('@'), "no email sigil: {line}");
            assert!(!line.to_lowercase().contains("token"), "no token: {line}");
            // Exactly one line — no embedded newline could split or forge a record.
            assert_eq!(line.lines().count(), 1, "single line: {line}");
        }
    }

    #[test]
    fn credential_health_line_carries_the_handle_and_state_token() {
        // Issue #119: the health-transition event is the handle + a bare 4-state token —
        // never a token, an expiry, or an email. Each rollup state renders its `snake_case`
        // token, matching the `--json` wire serialization of `CredentialHealth`.
        for (state, token) in [
            (CredentialHealth::Healthy, "healthy"),
            (CredentialHealth::Unknown, "unknown"),
            (CredentialHealth::Stale, "stale"),
            (CredentialHealth::AtRisk, "at_risk"),
            (CredentialHealth::Dead, "dead"),
        ] {
            let line = Event::CredentialHealth {
                account: "work".to_owned(),
                state,
            }
            .to_log_line(at_epoch(0));
            assert_eq!(
                line,
                format!("{TS0} event=credential_health account=work state={token}")
            );
        }
    }

    // --- EventLog (the sink) -----------------------------------------------

    #[test]
    fn emit_appends_one_stamped_line_per_event() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sessiometer.log");
        let mut log = EventLog::at(&path).unwrap();

        log.emit(&Event::Swap {
            from: "work".to_owned(),
            to: "spare".to_owned(),
            reason: SwapReason::Session,
            session_pct: 97,
        })
        .unwrap();
        log.emit(&Event::Monitor401 {
            account: "spare".to_owned(),
            consecutive: 1,
        })
        .unwrap();

        let logged = std::fs::read_to_string(&path).unwrap();
        // One line per event, each carrying its own `ts=` and `event=` keys.
        assert_eq!(logged.lines().count(), 2, "got: {logged:?}");
        assert!(logged.contains("event=swap from=work to=spare"));
        assert!(logged.contains("event=monitor_401 account=spare consecutive=1"));
        for line in logged.lines() {
            assert!(line.starts_with("ts="), "every line is stamped: {line:?}");
        }
        assert!(!logged.contains('@'));
    }

    #[test]
    fn the_log_file_is_created_private() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sessiometer.log");
        let _log = EventLog::at(&path).unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }

    // --- the `use` verb's new swap reasons + cooldown source (issue #63) -----

    #[test]
    fn swap_line_renders_the_manual_and_forced_reasons() {
        // The operator-driven `use` verb emits the STANDARD swap event with the new
        // reason tokens; a manual swap records session_pct=0 (not session-triggered
        // — the reason is what distinguishes it).
        for (reason, token) in [
            (SwapReason::Manual, "manual"),
            (SwapReason::Forced, "forced"),
        ] {
            let line = Event::Swap {
                from: "work".to_owned(),
                to: "spare".to_owned(),
                reason,
                session_pct: 0,
            }
            .to_log_line(at_epoch(0));
            assert_eq!(
                line,
                format!("{TS0} event=swap from=work to=spare reason={token} session_pct=0")
            );
        }
    }

    #[test]
    fn last_swap_at_is_none_for_an_absent_or_swapless_log() {
        let dir = tempfile::tempdir().unwrap();
        // Absent file → None (best-effort: a one-shot swap is never blocked by a
        // missing log; the cooldown then reads as inactive).
        assert_eq!(last_swap_at(&dir.path().join("absent.log")), None);
        // A present log with NO swap line → None.
        let path = dir.path().join("sessiometer.log");
        let mut log = EventLog::at(&path).unwrap();
        log.emit(&Event::Monitor401 {
            account: "work".to_owned(),
            consecutive: 1,
        })
        .unwrap();
        log.emit(&Event::KeychainLockedWait).unwrap();
        assert_eq!(last_swap_at(&path), None);
    }

    #[test]
    fn last_swap_at_returns_the_most_recent_swap_instant() {
        // The log is append-only chronological; `last_swap_at` returns the LAST
        // swap's `ts`, parsed back through the same RFC 3339 the writer rendered.
        // A manual `reason=manual` swap (#63) and an `emergency_swap` both count;
        // a later NON-swap line (monitor_401) is ignored. Hand-written so the
        // instants are deterministic (`emit` stamps with the live clock).
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sessiometer.log");
        let content = "\
ts=1970-01-01T00:00:10Z event=swap from=a to=b reason=session session_pct=97\n\
ts=1970-01-01T00:00:30Z event=swap from=b to=c reason=manual session_pct=0\n\
ts=1970-01-01T00:00:40Z event=monitor_401 account=c consecutive=1\n";
        std::fs::write(&path, content).unwrap();
        // The most recent SWAP line is the manual swap at epoch 30 — not the later
        // monitor_401 line, and not the earlier session swap at epoch 10.
        assert_eq!(last_swap_at(&path), Some(at_epoch(30)));

        // An `emergency_swap` is also a swap for cooldown purposes.
        std::fs::write(
            &path,
            "ts=1970-01-01T00:01:00Z event=emergency_swap from=a to=b\n",
        )
        .unwrap();
        assert_eq!(last_swap_at(&path), Some(at_epoch(60)));
    }

    #[test]
    fn refresh_outcome_token_round_trips() {
        // Issue #120: `from_token` is the exact inverse of `as_str`, so the offline
        // `list` view reads back precisely the variant the log wrote. An unrecognized
        // token (a truncated / future / corrupt line) is `None`, never mis-classified.
        for outcome in [
            RefreshEventOutcome::Refreshed,
            RefreshEventOutcome::RefreshedNotReStashed,
            RefreshEventOutcome::NoChange,
            RefreshEventOutcome::Dead,
            RefreshEventOutcome::Error,
        ] {
            assert_eq!(
                RefreshEventOutcome::from_token(outcome.as_str()),
                Some(outcome)
            );
        }
        assert_eq!(RefreshEventOutcome::from_token("bogus"), None);
        assert_eq!(RefreshEventOutcome::from_token(""), None);
    }

    #[test]
    fn last_refresh_outcomes_maps_each_handle_to_its_latest_outcome() {
        // Issue #120: the daemon-independent read the offline `list` view surfaces.
        // The log is append-only chronological, so per handle the LAST `refresh` line
        // wins; a non-refresh line is ignored, and the optional expiry fields AND the
        // trailing `rotated=` field (issue #279) after `outcome=` do not bleed into the
        // parsed token. The lines deliberately mix formats: a pre-#279 line with no
        // `rotated=` (`work`'s `no_change`) still parses, so historical logs keep working.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sessiometer.log");
        let content = "\
ts=1970-01-01T00:00:10Z event=refresh account=work outcome=no_change\n\
ts=1970-01-01T00:00:20Z event=refresh account=spare outcome=refreshed expires_before=1970-01-01T00:00:00Z expires_after=1970-01-01T02:00:00Z rotated=true\n\
ts=1970-01-01T00:00:30Z event=monitor_401 account=work consecutive=1\n\
ts=1970-01-01T00:00:40Z event=refresh account=work outcome=dead rotated=false\n";
        std::fs::write(&path, content).unwrap();
        let outcomes = last_refresh_outcomes(&path);
        // `work`'s latest refresh is the `dead` at epoch 40 — not the earlier `no_change`,
        // and the intervening `monitor_401` line is not a refresh. The trailing `rotated=false`
        // does not corrupt the `dead` token.
        assert_eq!(outcomes.get("work"), Some(&RefreshEventOutcome::Dead));
        // `spare`'s only refresh is `refreshed`; the trailing `expires_*` AND `rotated=true`
        // fields are stripped, leaving the bare outcome token.
        assert_eq!(outcomes.get("spare"), Some(&RefreshEventOutcome::Refreshed));
        assert_eq!(outcomes.len(), 2);
    }

    #[test]
    fn last_refresh_outcomes_is_empty_for_an_absent_or_refreshless_log() {
        let dir = tempfile::tempdir().unwrap();
        // Absent file → empty (best-effort: `list` simply omits the refresh tag).
        assert!(last_refresh_outcomes(&dir.path().join("absent.log")).is_empty());
        // A present log with NO refresh line → empty.
        let path = dir.path().join("sessiometer.log");
        let mut log = EventLog::at(&path).unwrap();
        log.emit(&Event::CredentialDead {
            account: "work".to_owned(),
        })
        .unwrap();
        assert!(last_refresh_outcomes(&path).is_empty());
    }

    #[test]
    fn last_refresh_outcomes_matches_a_handle_with_a_space_verbatim() {
        // A handle is operator free text that may contain spaces; the account field is
        // anchored between ` event=refresh account=` and the ` outcome=` that always
        // follows it, so `my work` is matched whole rather than truncated at the space.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sessiometer.log");
        std::fs::write(
            &path,
            "ts=1970-01-01T00:00:10Z event=refresh account=my work outcome=refreshed\n",
        )
        .unwrap();
        let outcomes = last_refresh_outcomes(&path);
        assert_eq!(
            outcomes.get("my work"),
            Some(&RefreshEventOutcome::Refreshed)
        );
    }

    // --- export / import events (issue #150) --------------------------------

    #[test]
    fn export_line_carries_accounts_encrypted_and_mode() {
        // A full, encrypted export: the roster size, the encrypted bool, and mode=full — no
        // account field at all (aggregate-only).
        let full = Event::Export {
            accounts: 3,
            encrypted: true,
            mode: ExportMode::Full,
        }
        .to_log_line(at_epoch(0));
        assert_eq!(
            full,
            format!("{TS0} event=export accounts=3 encrypted=true mode=full")
        );

        // A config-only, plaintext export: mode=config_only + encrypted=false.
        let config_only = Event::Export {
            accounts: 2,
            encrypted: false,
            mode: ExportMode::ConfigOnly,
        }
        .to_log_line(at_epoch(0));
        assert_eq!(
            config_only,
            format!("{TS0} event=export accounts=2 encrypted=false mode=config_only")
        );
    }

    #[test]
    fn import_line_derives_accounts_and_the_ok_rollup() {
        // A clean import: nothing failed → outcome=ok, and accounts= is the count sum. The line
        // carries the full per-account breakdown incl. failed=0.
        let line = Event::Import {
            imported: 2,
            skipped: 1,
            overwritten: 1,
            failed: 0,
        }
        .to_log_line(at_epoch(0));
        assert_eq!(
            line,
            format!(
                "{TS0} event=import accounts=4 outcome=ok \
                 imported=2 skipped=1 overwritten=1 failed=0"
            )
        );
    }

    #[test]
    fn import_line_renders_partial_and_failed_rollups() {
        // Some landed, some failed → partial.
        let partial = Event::Import {
            imported: 1,
            skipped: 0,
            overwritten: 0,
            failed: 1,
        }
        .to_log_line(at_epoch(0));
        assert_eq!(
            partial,
            format!(
                "{TS0} event=import accounts=2 outcome=partial \
                 imported=1 skipped=0 overwritten=0 failed=1"
            )
        );

        // Every account failed → failed.
        let failed = Event::Import {
            imported: 0,
            skipped: 0,
            overwritten: 0,
            failed: 3,
        }
        .to_log_line(at_epoch(0));
        assert_eq!(
            failed,
            format!(
                "{TS0} event=import accounts=3 outcome=failed \
                 imported=0 skipped=0 overwritten=0 failed=3"
            )
        );
    }

    #[test]
    fn import_rollup_treats_skipped_as_a_non_failure() {
        // The rollup logic directly (the derivation the line depends on):
        // - no failures anywhere → ok
        assert_eq!(ImportRollup::from_counts(0, 0, 0, 0), ImportRollup::Ok);
        assert_eq!(ImportRollup::from_counts(2, 3, 0, 0), ImportRollup::Ok);
        // - all failed → failed
        assert_eq!(ImportRollup::from_counts(0, 0, 0, 4), ImportRollup::Failed);
        // - a skip alongside a failure is PARTIAL, not failed: the skip is an intentional
        //   success (the conflict policy applied), so the import did not wholly fail.
        assert_eq!(ImportRollup::from_counts(0, 1, 0, 1), ImportRollup::Partial);
        // - a landed account alongside a failure → partial
        assert_eq!(ImportRollup::from_counts(1, 0, 0, 2), ImportRollup::Partial);
        assert_eq!(ImportRollup::from_counts(0, 0, 1, 1), ImportRollup::Partial);
    }

    #[test]
    fn export_and_import_lines_carry_no_pii() {
        // The #15 guarantee for the #150 events: the export/import lines carry ONLY aggregate
        // counts, a bool, and a fixed vocabulary token — never a handle, email, or token. There
        // is no per-account field through which a secret could reach the line at all.
        let export = Event::Export {
            accounts: 5,
            encrypted: true,
            mode: ExportMode::Full,
        }
        .to_log_line(at_epoch(0));
        let import = Event::Import {
            imported: 5,
            skipped: 0,
            overwritten: 0,
            failed: 0,
        }
        .to_log_line(at_epoch(0));
        for line in [&export, &import] {
            assert!(!line.contains('@'), "no email may appear: {line}");
            assert!(!line.contains("token"), "no token may appear: {line}");
            assert!(!line.contains("Bearer"), "no bearer may appear: {line}");
            assert!(!line.contains("sk-ant"), "no api key may appear: {line}");
            // No per-account identity field: the events are aggregate-only.
            assert!(!line.contains("account="), "no account handle: {line}");
            assert!(!line.contains("acct="), "no account handle: {line}");
        }
    }

    // --- Diagnostic::to_log_line (the diagnostic channel's redaction surface, #77) ---

    #[test]
    fn start_line_renders_the_effective_config_summary() {
        // session_floor present → its percent; the rest are counts/percentages.
        let on = Diagnostic::Start {
            accounts: 3,
            poll_secs: 30,
            session_floor: Some(70),
            session_trigger: 90,
            weekly_trigger: 98,
            monitor_401_n: 5,
            monitor_recovery_m: 4,
        }
        .to_log_line(at_epoch(0));
        assert_eq!(
            on,
            format!(
                "{TS0} diag=start accounts=3 poll_secs=30 session_floor=70 \
                 session_trigger=90 weekly_trigger=98 monitor_401_n=5 monitor_recovery_m=4"
            )
        );

        // session_floor absent → the explicit `off` sentinel (the guard is disabled,
        // #10), never an empty value that would split the key=val grammar.
        let off = Diagnostic::Start {
            accounts: 1,
            poll_secs: 60,
            session_floor: None,
            session_trigger: 80,
            weekly_trigger: 95,
            monitor_401_n: 3,
            monitor_recovery_m: 2,
        }
        .to_log_line(at_epoch(0));
        assert!(off.contains("session_floor=off"), "got: {off}");
    }

    #[test]
    fn stop_line_is_bare() {
        assert_eq!(
            Diagnostic::Stop.to_log_line(at_epoch(0)),
            format!("{TS0} diag=stop")
        );
    }

    #[test]
    fn poll_line_carries_the_handle_and_each_outcome_class() {
        // The 5-way diagnostic taxonomy — rate_limited is SEPARATE from transient.
        for (outcome, token) in [
            (PollClass::Live, "live"),
            (PollClass::Unauthorized, "unauthorized"),
            (PollClass::Scope, "scope"),
            (PollClass::RateLimited, "rate_limited"),
            (PollClass::Transient, "transient"),
        ] {
            let line = Diagnostic::Poll {
                account: "work".to_owned(),
                outcome,
            }
            .to_log_line(at_epoch(0));
            assert_eq!(
                line,
                format!("{TS0} diag=poll account=work outcome={token}")
            );
        }
    }

    #[test]
    fn tick_line_renders_the_decision_and_omits_backoff_when_absent() {
        // No back-off → both back-off fields are simply absent (the line stays well-formed).
        let held = Diagnostic::Tick {
            decision: DecisionClass::Hold,
            backoff_secs: None,
            retry_after_secs: None,
        }
        .to_log_line(at_epoch(0));
        assert_eq!(held, format!("{TS0} diag=tick decision=hold"));
        assert!(!held.contains("backoff_secs"), "got: {held}");
        assert!(!held.contains("retry_after_secs"), "got: {held}");

        // A self-capped back-off (#13 keychain-lock / #76 exponential) → the wait in whole
        // seconds, and NO `retry_after_secs` (no server advised it — issue #295).
        let backed_off = Diagnostic::Tick {
            decision: DecisionClass::KeychainLocked,
            backoff_secs: Some(8),
            retry_after_secs: None,
        }
        .to_log_line(at_epoch(0));
        assert_eq!(
            backed_off,
            format!("{TS0} diag=tick decision=keychain_locked backoff_secs=8")
        );
    }

    #[test]
    fn tick_line_labels_the_retry_after_source() {
        // Issue #295: `retry_after_secs` LABELS a server-advised wait, distinguishing it from
        // the self-capped exponential. Present → it renders after `backoff_secs`, so an
        // operator can place a `backoff_secs` that was previously ambiguous.
        let server_advised = Diagnostic::Tick {
            decision: DecisionClass::SkipActiveUnavailable,
            backoff_secs: Some(3600),
            retry_after_secs: Some(86_400),
        }
        .to_log_line(at_epoch(0));
        // The pathological `Retry-After` the #294 cap bit is now VISIBLE beside the clamped
        // wait — the `backoff_secs=3600` ambiguity #295 set out to resolve.
        assert_eq!(
            server_advised,
            format!("{TS0} diag=tick decision=skip_active_unavailable backoff_secs=3600 retry_after_secs=86400")
        );

        // Order is fixed: `backoff_secs` then `retry_after_secs`, both after `decision`.
        let idx_backoff = server_advised.find("backoff_secs").unwrap();
        let idx_retry = server_advised.find("retry_after_secs").unwrap();
        assert!(idx_backoff < idx_retry, "field order: {server_advised}");
    }

    #[test]
    fn every_decision_class_renders_its_token() {
        // One token per TickAction (the map is exhaustive, #77).
        for (decision, token) in [
            (DecisionClass::Hold, "hold"),
            (DecisionClass::Swap, "swap"),
            (DecisionClass::EmergencySwap, "emergency_swap"),
            (DecisionClass::AllExhausted, "all_exhausted"),
            (DecisionClass::ActiveDeadNoTarget, "active_dead_no_target"),
            (DecisionClass::SkipActiveUnknown, "skip_active_unknown"),
            (
                DecisionClass::SkipActiveUnavailable,
                "skip_active_unavailable",
            ),
            (DecisionClass::SkipCooldown, "skip_cooldown"),
            (DecisionClass::SwapFailed, "swap_failed"),
            (DecisionClass::KeychainLocked, "keychain_locked"),
        ] {
            let line = Diagnostic::Tick {
                decision,
                backoff_secs: None,
                retry_after_secs: None,
            }
            .to_log_line(at_epoch(0));
            assert_eq!(line, format!("{TS0} diag=tick decision={token}"));
        }
    }

    #[test]
    fn all_exhausted_cleared_line_is_bare() {
        assert_eq!(
            Diagnostic::AllExhaustedCleared.to_log_line(at_epoch(0)),
            format!("{TS0} diag=all_exhausted_cleared")
        );
    }

    #[test]
    fn no_diagnostic_line_carries_an_email_or_token_sigil() {
        // #15: every diagnostic field is a handle / enum / number / timestamp, so a
        // token or email can never reach a rendered line. Mirrors the event-log guard.
        let diags = [
            Diagnostic::Start {
                accounts: 2,
                poll_secs: 30,
                session_floor: Some(70),
                session_trigger: 90,
                weekly_trigger: 98,
                monitor_401_n: 5,
                monitor_recovery_m: 4,
            },
            Diagnostic::Stop,
            Diagnostic::Poll {
                account: "work".to_owned(),
                outcome: PollClass::RateLimited,
            },
            Diagnostic::Tick {
                decision: DecisionClass::Swap,
                backoff_secs: Some(16),
                // Exercise the #295 source-label field through the #15 redaction scan too.
                retry_after_secs: Some(3600),
            },
            Diagnostic::AllExhaustedCleared,
        ];
        for diag in &diags {
            let line = diag.to_log_line(at_epoch(0));
            assert!(!line.contains('@'), "no email sigil: {line}");
            assert!(!line.to_lowercase().contains("token"), "no token: {line}");
            assert_eq!(line.lines().count(), 1, "single line: {line}");
        }
    }

    // --- DiagnosticLog (the verbosity-gated sink, #77) ----------------------

    #[test]
    fn diagnostic_log_is_silent_when_quiet() {
        // Default QUIET → nothing reaches the sink (no console spam without opt-in).
        let mut log = DiagnosticLog::new(Vec::<u8>::new(), Verbosity::Quiet);
        log.emit(&Diagnostic::Stop);
        log.emit(&Diagnostic::Poll {
            account: "work".to_owned(),
            outcome: PollClass::Live,
        });
        assert!(
            log.sink.is_empty(),
            "quiet must emit nothing: {:?}",
            log.sink
        );
    }

    #[test]
    fn diagnostic_log_emits_one_line_per_diagnostic_when_verbose() {
        let mut log = DiagnosticLog::new(Vec::<u8>::new(), Verbosity::Verbose);
        log.emit(&Diagnostic::Poll {
            account: "work".to_owned(),
            outcome: PollClass::RateLimited,
        });
        log.emit(&Diagnostic::Tick {
            decision: DecisionClass::Hold,
            backoff_secs: None,
            retry_after_secs: None,
        });
        let out = String::from_utf8(log.sink).unwrap();
        assert_eq!(out.lines().count(), 2, "one line per emit: {out:?}");
        assert!(out.contains("diag=poll account=work outcome=rate_limited"));
        assert!(out.contains("diag=tick decision=hold"));
        // Each line is stamped and newline-terminated.
        assert!(out.ends_with('\n'));
        for line in out.lines() {
            assert!(line.starts_with("ts="), "stamped: {line:?}");
        }
    }
}
