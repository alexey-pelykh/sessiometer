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
//! Note for #15: a handle is an operator-chosen label, and NOTHING constrains its
//! charset. Config validation rejects a label only when it is empty after `trim`
//! (`config::validate`); the `[A-Za-z0-9_-]{1,128}` rule is the `account_uuid`'s
//! alone (issue #1052); and the meter scans for token, blob and email SHAPES, never
//! a charset. So a label containing a space or `=` splits the `key=val` grammar into
//! extra FIELDS, and no component prevents it — by decision, not by omission
//! (ADR-0034, issue #1185): the label is written VERBATIM, and the obligation to
//! survive one belongs to each READER of the durable log. What this module owes, it
//! owes as a reader — see [`last_swap_at`] and [`last_refresh_outcomes`], which read
//! the event key by field POSITION rather than by substring. Readers outside it do not
//! yet meet that bar ([`crate::log`]'s `field`, [`crate::reliability`]'s `parse_events`
//! and [`crate::usage_stats`]'s `parse_swap_events`, all of which tokenize on
//! whitespace); ADR-0034 names them. The list carries the count deliberately — an
//! earlier revision said "two" and omitted the third, which this file already named
//! beside `parse_events` further down.
//!
//! The one FREE-FORM value on this channel is the resolved `claude` path
//! ([`Event::RefreshBinaryResolved`], issue #786) — a filesystem location, still
//! never a token or an email. Because a path can legally contain a space, it is the
//! one value this module DOES police at FIELD level: it renders percent-encoded
//! ([`path_value`]), so the whitespace-free grammar every parser assumes holds by
//! construction rather than by the operator's good luck in naming their directories.
//!
//! ## Record integrity (issue #1092)
//!
//! Splitting a line into extra FIELDS is a nuisance; splitting it into extra
//! RECORDS is a different class of harm, and it is the one thing this module
//! guarantees for every value regardless of provenance. A handle is not always
//! charset-constrained by the time it reaches a line: a roster `account_uuid` is
//! (`[A-Za-z0-9_-]{1,128}`, issue #1052), but a `label` is deliberately free-form,
//! and the login-FAILURE path logs an `account_uuid` harvested straight from
//! `~/.claude.json` — checked for non-emptiness only — *before* the roster gate that
//! would have rejected it. A newline in any of those would end one record and open
//! another that a line-oriented reader cannot tell from a real one.
//!
//! So both renderers on this module's two channels percent-encode every CONTROL
//! character on their way out ([`single_line`]), at the single exit each already
//! has. Deliberately control characters ALONE: a value with none renders
//! byte-for-byte as before, which is what keeps the frozen grammar
//! ([`crate::log`]) frozen and every already-written record readable unchanged.
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
use std::path::{Path, PathBuf};
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
    /// The #452 bounded-blindness preemptive gate fired (ADR-0017): the active account
    /// was blind past `session_blind_swap_secs` with a retained pre-blind anchor — since
    /// issue #619 plausibility-corrected to its window high-water mark, so a stale-low
    /// pre-blind reading cannot disarm the gate — at/over `session_blind_risk_band`, and a
    /// viable target existed, so it swapped away before it could self-exhaust unobserved.
    /// `session_pct` carries the STALE pre-blind anchor VERBATIM — the only session signal
    /// available while blind — not a fresh reading, and NOT the #619-corrected value the
    /// gate decided on (the correction is applied at the decision read, never stored or
    /// logged, so this field stays a raw measurement).
    BlindPreempt,
    /// The #539 velocity-projection preemptive gate fired (ADR-0017): the active account's
    /// OBSERVED reading was below the trigger, but its projected session usage
    /// (`last + velocity × session_velocity_horizon_secs`, keyed off the #399 velocity signal)
    /// crossed it, so it swapped away before the observed reading would trip the reactive
    /// trigger — closing the OBSERVED reactive overshoot (#363). Unlike `BlindPreempt`,
    /// `session_pct` carries the reading the projection actually fired on — a LIVE reading, never a
    /// stale blind anchor. Since issue #614 that is the *plausibility-corrected* live reading: when
    /// the response came back implausibly low for its own session window, the retained high-water
    /// mark (a true lower bound on the account's usage, and the value the swap was decided on) is
    /// reported instead of the number the daemon did not act on. Distinct from `Session` so the
    /// false-projection SLI and the projected swap-out overshoot readout (`sessiometer reliability`)
    /// can separate the projective swaps from the reactive residual.
    VelocityPreempt,
}

impl SwapReason {
    /// The `reason=` token.
    fn as_str(self) -> &'static str {
        match self {
            SwapReason::Session => "session",
            SwapReason::Weekly => "weekly",
            SwapReason::Manual => "manual",
            SwapReason::Forced => "forced",
            SwapReason::BlindPreempt => "blind_preempt",
            SwapReason::VelocityPreempt => "velocity_preempt",
        }
    }
}

/// The throttle CLASS that armed a per-account poll back-off — the `class=` of an
/// [`Event::UsageBackoff`] (issue #399). A closed enum, secret-free BY CONSTRUCTION (#15):
/// it separates a rate-limit (`429`) from a generic transient (`5xx` / network), so the
/// DURABLE log makes the #399 "usage-endpoint 429 count" queryable (`grep class=rate_limited`).
/// The durable-event mirror of the stderr-only [`PollClass::RateLimited`] / [`PollClass::Transient`]
/// distinction the diagnostic channel carries — narrowed to the two outcomes that actually arm a
/// back-off, so an invalid class (a `live` / `unauthorized` reading) is unrepresentable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BackoffClass {
    /// HTTP 429 — the usage endpoint rate-limited the poll (the per-Anthropic-org throttle).
    RateLimited,
    /// A `5xx` / network / unreadable transient — no liveness signal, backs off the same way.
    Transient,
}

impl BackoffClass {
    /// The `class=` token.
    fn as_str(self) -> &'static str {
        match self {
            BackoffClass::RateLimited => "rate_limited",
            BackoffClass::Transient => "transient",
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
///
/// The two REFRESHED variants carry `rotated` as a payload; the other three cannot carry
/// it at all (issue #1004). That asymmetry is the point: `rotated=` is only evidence where
/// an exchange actually happened, and a sibling `bool` field alongside `outcome` let a
/// `dead` line assert a rotation it could not have observed. Because the flag now lives
/// INSIDE the variants that admit it, [`Event::to_log_line`] cannot render it on the other
/// three even by mistake — the guarantee is type-level, not a formatting convention.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RefreshEventOutcome {
    /// Claude Code refreshed the parked token and the CAS re-stash stored it.
    Refreshed {
        /// Whether the exchange also ROTATED the refresh-token VALUE (issue #279), from the
        /// cycle's [`crate::refresh::RefreshOutcome::Refreshed`] payload. A boolean only,
        /// never either token value, so the #15 single-surface guarantee holds.
        rotated: bool,
    },
    /// CC refreshed the token (so the refresh token was still valid — the credential is
    /// alive) but a concurrent swap / login took precedence, so it was not re-stashed.
    RefreshedNotReStashed {
        /// Carried here for the same reason as on [`Refreshed`](Self::Refreshed): this is a
        /// genuine exchange — the expiry slid forward — and only the re-stash did not happen
        /// (a lost CAS, or the keep-warm path, which PROMOTES instead of re-stashing). The
        /// rotation compare is therefore just as well-evidenced as on a re-stashed refresh.
        rotated: bool,
    },
    /// CC returned the seeded token unchanged — no refresh happened.
    NoChange,
    /// CC cleared the refresh token in place — the credential is dead and needs an
    /// operator re-login.
    Dead,
    /// The cycle ran but produced no usable result (a spawn / read-back / lock failure,
    /// or a whole-cycle timeout).
    Error,
}

/// The bare `outcome=` TOKEN vocabulary — [`RefreshEventOutcome`] with its payloads
/// projected away (issue #1004).
///
/// This exists because parsing is not the inverse of rendering once a variant carries data
/// the token does not spell. A log line's `outcome=refreshed` says WHICH arm fired; the
/// rotation lives in a separate `rotated=` field, so a reader handed only the token can
/// recover the arm and nothing more. Returning a fabricated `rotated: false` from
/// [`from_token`](Self::from_token) would reintroduce exactly the defect this change
/// removes — a flag asserted without evidence — so the parse target is this payload-free
/// kind instead, and every consumer that only needs "which arm" (the offline `list` view,
/// the sweep-health fold) takes it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RefreshEventOutcomeKind {
    Refreshed,
    RefreshedNotReStashed,
    NoChange,
    Dead,
    Error,
}

impl RefreshEventOutcome {
    /// Project away the payload, leaving the bare `outcome=` token vocabulary.
    pub(crate) fn kind(self) -> RefreshEventOutcomeKind {
        match self {
            RefreshEventOutcome::Refreshed { .. } => RefreshEventOutcomeKind::Refreshed,
            RefreshEventOutcome::RefreshedNotReStashed { .. } => {
                RefreshEventOutcomeKind::RefreshedNotReStashed
            }
            RefreshEventOutcome::NoChange => RefreshEventOutcomeKind::NoChange,
            RefreshEventOutcome::Dead => RefreshEventOutcomeKind::Dead,
            RefreshEventOutcome::Error => RefreshEventOutcomeKind::Error,
        }
    }

    /// The `outcome=` token — the payload-free [`kind`](Self::kind)'s spelling.
    pub(crate) fn as_str(self) -> &'static str {
        self.kind().as_str()
    }

    /// The `rotated=` field this outcome renders, or `None` where the concept does not
    /// apply — the ONLY way the emitter can obtain it (issue #1004).
    pub(crate) fn rotated(self) -> Option<bool> {
        match self {
            RefreshEventOutcome::Refreshed { rotated }
            | RefreshEventOutcome::RefreshedNotReStashed { rotated } => Some(rotated),
            RefreshEventOutcome::NoChange
            | RefreshEventOutcome::Dead
            | RefreshEventOutcome::Error => None,
        }
    }
}

impl RefreshEventOutcomeKind {
    /// The `outcome=` token. `pub(crate)` so the offline `list` view (issue #120) can
    /// render the last-persisted outcome it reads back via [`last_refresh_outcomes`]
    /// in the SAME vocabulary the log writes — an operator who greps `sessiometer.log`
    /// for `outcome=` sees the identical token `list` shows.
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            RefreshEventOutcomeKind::Refreshed => "refreshed",
            RefreshEventOutcomeKind::RefreshedNotReStashed => "refreshed_not_restashed",
            RefreshEventOutcomeKind::NoChange => "no_change",
            RefreshEventOutcomeKind::Dead => "dead",
            RefreshEventOutcomeKind::Error => "error",
        }
    }

    /// Parse an `outcome=` token back into its kind — the inverse of [`as_str`],
    /// for reading the last-persisted refresh outcome out of the event log (issue
    /// #120). `None` for an unrecognized token (a truncated / future / corrupt line),
    /// so a malformed record is skipped rather than mis-classified.
    ///
    /// [`as_str`]: RefreshEventOutcomeKind::as_str
    pub(crate) fn from_token(token: &str) -> Option<Self> {
        Some(match token {
            "refreshed" => RefreshEventOutcomeKind::Refreshed,
            "refreshed_not_restashed" => RefreshEventOutcomeKind::RefreshedNotReStashed,
            "no_change" => RefreshEventOutcomeKind::NoChange,
            "dead" => RefreshEventOutcomeKind::Dead,
            "error" => RefreshEventOutcomeKind::Error,
            _ => return None,
        })
    }
}

/// WHY an isolated-refresh cycle produced an `outcome=error` — the non-secret `reason=` sub-class
/// of an error [`Event::Refresh`] line (issue #377). The event-level mirror of the engine's
/// [`crate::refresh::RefreshErrorReason`], carrying TWO variants the engine cannot: [`Timeout`],
/// the tick's `tokio::time::timeout` bound firing, and [`Unresolved`], the `claude` binary failing
/// to resolve at all — neither a value a completed cycle produces, because both are detected
/// OUTSIDE one (the same event-adds-a-variant split by which [`RefreshEventOutcome`] adds
/// `RefreshedNotReStashed`). A FIXED, secret-free-BY-CONSTRUCTION classification: it makes a
/// wholesale refresh failure (the #375 incident — every account a bare `error` for hours)
/// diagnosable from the log without ever folding a token / path / email onto the #15 channel.
///
/// Rendered ONLY on an error line, and ONLY for a sub-cause classifiable secret-free.
/// [`Unresolved`] (issue #786) meets that test BY CONSTRUCTION — a fixed token, with the resolved
/// path carried on its own [`Event::RefreshBinaryResolved`] line rather than folded in here — and
/// it is the sub-cause this enum most needed: it exists so a #375-class outage is diagnosable from
/// the log, and an unresolvable `claude` is precisely the cause that PRODUCED one (24 h of bare
/// `outcome=error` lines while `status` pointed the operator at a `reason=` no refresh event had
/// ever carried).
///
/// The REMAINING hard engine `Err`s — a locked keychain, a contended lock, an FS error — still
/// have no class here and still render a bare `outcome=error` with no `reason=`. That narrowness
/// is deliberate, and argued where it is enforced (`refresh_tick::engine_error_reason`).
///
/// [`Timeout`]: RefreshEventReason::Timeout
/// [`Unresolved`]: RefreshEventReason::Unresolved
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RefreshEventReason {
    /// The `claude` binary resolved but could not be spawned / exec'd.
    SpawnFailed,
    /// The read-back item was unreadable.
    ReadbackUnreadable,
    /// The stored / read-back blob was unparseable.
    Malformed,
    /// The cycle exceeded `[refresh].timeout_secs` — the tick's whole-cycle timeout bound (one of
    /// the two sub-causes detected OUTSIDE a completed engine cycle, hence event-level only).
    Timeout,
    /// The `claude` binary could not be located at all (issue #786,
    /// [`crate::error::Error::ClaudeBinaryNotFound`]): no `[refresh].claude_bin` pin, no
    /// `$CLAUDE_BIN`, and no `claude` on the harvested login-shell `PATH`. Event-level only for
    /// the same structural reason as [`Timeout`](Self::Timeout) — it is detected at RESOLUTION
    /// time, before any engine cycle exists to classify. The token names the failure; the paths
    /// searched never ride it.
    Unresolved,
}

impl RefreshEventReason {
    /// The `reason=` token — the same snake_case grep vocabulary as the rest of this module.
    fn as_str(self) -> &'static str {
        match self {
            RefreshEventReason::SpawnFailed => "spawn_failed",
            RefreshEventReason::ReadbackUnreadable => "readback_unreadable",
            RefreshEventReason::Malformed => "malformed",
            RefreshEventReason::Timeout => "timeout",
            RefreshEventReason::Unresolved => "unresolved",
        }
    }
}

/// What prompted an isolated PARKED-account poll-refresh cycle (issue #1367) — the `trigger=`
/// token of an [`Event::PollRefresh`] line, and the parked-side sibling of [`KeepWarmTrigger`].
///
/// The line carried a hard-coded `poll_401` literal from issue #255 until now, on the premise that
/// the reactive #162 poll path was the only condition that reached it. Issue #643 falsified that
/// premise without noticing: it routed the `restored`-driven re-probe of a `Dead` PARKED credential
/// (`reprobe_dead_parked_credential`) through the SAME event, so every recovery re-probe wrote a
/// durable line blaming a 401 that never happened. #643 DID add [`KeepWarmTrigger::Recovery`] for
/// the active-side re-probe in the same change; this enum is the parked-side equivalent it omitted.
///
/// A non-secret classification only — never a token or email.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PollRefreshTrigger {
    /// The reactive #162 poll path: the FIRST usage-401 of a streak episode on a parked account,
    /// where one isolated refresh plus a re-poll may revive a merely-expired ACCESS token before
    /// the 401 advances the #42 death streak.
    Poll401,
    /// The issue #643 recovery re-probe: a `restored` control signal named an account carrying the
    /// terminal 🔴 `Dead` verdict, so the daemon drove one isolated refresh on the spot rather than
    /// latching the stale verdict until the next natural sweep. Operator-initiated — no usage-401
    /// is involved, and the SAME `recovery` token [`KeepWarmTrigger::Recovery`] renders for the
    /// active-side half of that same fix.
    Recovery,
}

impl PollRefreshTrigger {
    /// The `trigger=` token — the same grep vocabulary the rest of the event log uses, and
    /// deliberately the same `recovery` spelling [`KeepWarmTrigger`] renders, so one grep finds
    /// both halves of the issue #643 re-probe.
    fn as_str(self) -> &'static str {
        match self {
            PollRefreshTrigger::Poll401 => "poll_401",
            PollRefreshTrigger::Recovery => "recovery",
        }
    }
}

/// What prompted an in-place ACTIVE-account keep-warm cycle (issue #282) — the `trigger=`
/// token of an [`Event::KeepWarm`] line. Keep-warm fires from three distinct conditions, so the
/// discriminant is a carried enum field: a `proactive` mint scheduled before the active token
/// nears expiry, a `reactive` backstop mint on an active usage-401 (revive the canonical before
/// the 401 counts toward the #42 death streak), or a `recovery` mint forced when an account that
/// is `Dead` on `use`-activation is re-probed on the spot (issue #643). A non-secret
/// classification only — never a token or email.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum KeepWarmTrigger {
    /// A scheduled mint fired because the active token entered its (staggered) near-expiry
    /// horizon — the pre-emptive path that keeps the token warm BEFORE any 401.
    Proactive,
    /// A backstop mint fired on an active usage-401, reviving the canonical in place before
    /// the 401 advances the #42 death streak toward a false quarantine.
    Reactive,
    /// A forced mint fired when a `use`-activated account carries the terminal 🔴 `Dead` verdict
    /// (issue #643): the active-safe re-probe, folded three ways — a live mint clears the `Dead`
    /// latch, a transient `Error` un-quarantines to `AtRisk`, and only a `Dead` re-mint keeps an
    /// honest 🔴, instead of latching the stale verdict until the account is parked and swept again.
    Recovery,
}

impl KeepWarmTrigger {
    /// The `trigger=` token — the same grep vocabulary the rest of the event log uses.
    fn as_str(self) -> &'static str {
        match self {
            KeepWarmTrigger::Proactive => "proactive",
            KeepWarmTrigger::Reactive => "reactive",
            KeepWarmTrigger::Recovery => "recovery",
        }
    }
}

/// The 5-state per-account CREDENTIAL-health rollup `status` surfaces (issue #119): the
/// daemon-computed verdict the thin read-only `status` client just projects to a glyph
/// (🟢/🟡/🟠/🔴). Lives HERE — the base observability module, with no `daemon`
/// dependency — so the [`Event::CredentialHealth`] transition event can name it without
/// a `daemon` ↔ `observability` dependency cycle; `daemon` (which computes it) and `cli`
/// (which renders it) both import it.
///
/// Non-secret by construction: a bare classification, never a token, an expiry, or an
/// email — the same #15 discipline as [`RefreshEventOutcome`]. The SEVERITY variants are
/// ordered `Healthy` < `Stale` < `AtRisk` < `Degraded` < `Dead`, matching the issue's green →
/// yellow → orange → red ladder; `Unknown` (issue #137) is OFF that severity axis — a
/// non-active account with NO positive-liveness evidence (never successfully polled, no
/// refresh telemetry, no refresh-sourced expiry), reported honestly rather than as a false
/// 🟢. It sits just above `Healthy` in declaration order, the "unverified" caution.
///
/// `Degraded` (issue #427) splits the old `Dead` catch-all in two: a bare `quarantined`
/// (the #42 access-token 401-streak) is NON-TERMINAL — the *access* token was rejected, but
/// a usage-endpoint 401 says nothing about the *refresh* token (a resource server never sees
/// it), so `poke` / a daemon restart revive the account. It is `Degraded`, not `Dead`; only a
/// PROVEN refresh-token death (a sweep-refresh that actually returned `Dead`, the #261 /
/// `CredentialUnrecoverable` cue) justifies the terminal 🔴 `Dead` / "claude /login".
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
    /// The stored ACCESS token was rejected and the account 401-streaked into quarantine (the
    /// #42 verdict), so it is out of rotation right now — but this is NON-TERMINAL (issue
    /// #427): a usage-endpoint 401 never sees the refresh token, so the refresh token is
    /// (very likely) still valid and `poke` / a restart revive the account. Needs a REFRESH,
    /// not a re-login. Renders 🟠 with a needs-refresh cue, distinct from the terminal 🔴
    /// `Dead`. Only escalates to `Dead` once a refresh actually returns `Dead`.
    Degraded,
    /// The credential is PROVABLY DEAD — a sweep-refresh returned `Dead` (the #261 /
    /// `CredentialUnrecoverable` cue: the refresh token itself was rejected) — and genuinely
    /// needs an operator `claude /login`. 🔴 Reserved for this proven case; a bare quarantine
    /// (an access-token 401-streak) is `Degraded`, not `Dead` (issue #427).
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
            CredentialHealth::Degraded => "degraded",
            CredentialHealth::Dead => "dead",
        }
    }
}

/// Where an account's REFRESH-token deadline (`refreshTokenExpiresAt`, issue #878) sits relative
/// to the operator's configurable foresight horizon (`[credential].expiry_horizon_secs`).
///
/// **ORTHOGONAL to [`CredentialHealth`], deliberately not a variant of it.** That enum is a
/// severity RAMP over *current* validity; this is a *forward-looking* axis over a FIXED deadline
/// refreshing does not move. An account whose refresh token expires in five days is
/// [`CredentialHealth::Healthy`] **right now** — a valid access token and a working refresh path —
/// yet more urgent than a plain healthy account and less severe than [`CredentialHealth::Stale`].
/// It has no defensible position in that ordinal ladder, so it gets its own axis; the two render as
/// INDEPENDENT cells. This follows the ratified ADR-0017 `blind_active` precedent: a per-account
/// MODIFIER on an otherwise-healthy row, never a new state.
///
/// Non-secret by construction: a bare classification, never a token, an email, or a timestamp — the
/// same issue #15 discipline as [`CredentialHealth`].
///
/// The `snake_case` serde rename mirrors [`CredentialHealth`]'s, so the tokens an event log and a
/// `--json` field carry agree by construction. Issue #882 has since put the modifier on the
/// `status`/`watch` wire ([`AccountStatusLine::expiry`](crate::daemon::AccountStatusLine),
/// schema 1.12), and these renames ARE those wire tokens — pinned by
/// `account_line_encodes_the_expiry_modifier_only_once_the_account_has_been_polled`, so respelling a
/// variant here is a wire-contract change, not a local edit.
///
/// Issue #878 shipped this enum with NO `as_str` renderer, on the principle that the consumer needing
/// one brings it; issue #880's [`Event::CredentialExpiryHorizon`] is that consumer, so [`Self::as_str`]
/// now exists. It is pinned AGAINST the serde spelling by a test rather than trusted to stay in
/// agreement — and with #882 landed that pin is load-bearing in both directions: it is what keeps the
/// event-log token and the schema-1.12 wire token the same string, so an operator correlating a
/// `state=` on the durable log against an `expiry` on the wire is reading one vocabulary, not two that
/// happen to match today.
///
/// That vocabulary is for MACHINES, and issue #883's operator surfaces deliberately do not speak it:
/// [`crate::cli::expiry_cell`] renders a time-until (`3d`, `29d4h`) for `Within` / `Beyond` and the
/// gap `—` for `Unknown`, so `Lapsed` is the only variant whose cell coincides with its token at all.
/// Routing a UI cell through [`Self::as_str`] to save a match would be precisely the
/// wire-token-as-UI-string coupling the pin above exists to keep visible.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ExpiryHorizon {
    /// No usable `refreshTokenExpiresAt` was observed — the field is absent from the credential
    /// blob (an older Claude Code, a changed upstream policy, a non-first-party credential), or the
    /// blob was unreadable.
    ///
    /// **NEVER read this as "not expiring."** It is the issue #137 invariant applied to foresight:
    /// *absence of a NEGATIVE signal alone is not health.* The daemon cannot vouch for a deadline it
    /// never saw, so it says "unknown" rather than the false-reassuring "fine" that would let an
    /// account lapse silently. This single rule is what makes the whole feature degrade SAFELY if
    /// upstream ever drops the field — and is why this variant, not `Beyond`, is the enum's
    /// [`Default`]: a consumer that forgets to classify gets the honest answer, not the dangerous
    /// one.
    #[default]
    Unknown,
    /// A deadline was observed and it is FURTHER OUT than the horizon — the only variant that
    /// actually means "not expiring soon", and it is reachable ONLY from a parsed timestamp.
    Beyond,
    /// A deadline was observed and it falls WITHIN the horizon: still valid, but the operator
    /// should re-login before it lapses. The foresight the feature exists to provide.
    Within,
    /// A deadline was observed and it has already PASSED. No refresh can recover the account —
    /// only an operator `claude /login`. Distinct from [`CredentialHealth::Dead`], which is set
    /// only once a refresh has actually FAILED (`src/poke.rs`): this is the same fact seen
    /// BEFORE the failure rather than after it.
    Lapsed,
}

impl ExpiryHorizon {
    /// The `state=` token for the [`Event::CredentialExpiryHorizon`] log line (issue #880).
    ///
    /// Issue #878 deliberately shipped this enum WITHOUT a renderer — *"the consumer that needs one
    /// brings it"* — and this event is that consumer. Matches the `snake_case` serde rename exactly,
    /// so the token this line carries and the token issue #882's `--json` field will carry agree BY
    /// CONSTRUCTION rather than by two hand-maintained lists happening to say the same thing; the
    /// test below pins that agreement against the serde output rather than against a copy of it.
    fn as_str(self) -> &'static str {
        match self {
            ExpiryHorizon::Unknown => "unknown",
            ExpiryHorizon::Beyond => "beyond",
            ExpiryHorizon::Within => "within",
            ExpiryHorizon::Lapsed => "lapsed",
        }
    }
}

/// WHO wrote the `refreshTokenExpiresAt` deadline change an [`Event::CredentialExpiryObserved`]
/// line reports (issue #880) — the load-bearing half of that record.
///
/// Logging the deadline alone answers nothing: a change has more than one possible cause, and the
/// causes imply OPPOSITE operational conclusions. Only a provenance-tagged observation separates
/// them, which is what makes the spike question in issue #877 — *does a re-login reset the
/// refresh-token deadline?* — answerable from the durable log alone instead of from a hand-run
/// browser experiment.
///
/// **Deliberately NOT `re_login`.** The tempting fourth variant would name the operator action
/// rather than the daemon's observation, and the daemon cannot see that action:
/// [`crate::daemon::Daemon::reconcile_canonical_change`]'s own contract is that a CHANGED canonical
/// means *"the operator ran `claude /login`* **or** *the active token silently refreshed in
/// place"*, and those two are indistinguishable from the blob. So every variant here names what was
/// OBSERVED, never what it is taken to mean.
///
/// The re-login-vs-in-place-refresh reading is NOT recoverable by joining the sibling events, and it
/// is worth being precise about why, because the join looks available and is not: `event=restash` is
/// emitted at the very same edge as this record, one-for-one, so it adds no fact — and it carries the
/// free-form `account=` label where this line carries `acct=`, so the two share no key to join ON.
/// `event=login` covers only the MANAGED `sessiometer login` (#132/#134/#135); an operator's own
/// `claude /login` — precisely the population #877 asks about — produces no such line at all. The
/// discriminator therefore lives ON this record, as
/// [`Event::CredentialExpiryObserved::grant_replaced`]: whether a NEW OAuth grant was minted. Even that
/// narrows rather than decides, and the variant naming stays observational for the same reason.
///
/// Carries NO serde derive, unlike [`ExpiryHorizon`]: this discriminant reaches no JSON wire (the
/// reliability SLI attribution of issue #881 reads the event log as TEXT), so [`Self::as_str`] is
/// its sole renderer and there is no second spelling to keep in agreement.
///
/// Non-secret by construction: a bare classification, never a token, an email, or a timestamp — the
/// same issue #15 discipline as [`ExpiryHorizon`] and [`CredentialHealth`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ExpiryProvenance {
    /// The FIRST deadline this run observed for the account — an absolute anchor, not a change.
    ///
    /// Exists because the change baseline is IN-MEMORY: a restart drops it, so without an anchor a
    /// deadline that moved while the daemon was down would be folded in silently and be
    /// indistinguishable, offline, from *nothing happened* — the absence-is-not-evidence trap this
    /// record exists to close. One line per account per daemon lifetime bounds the cost, and it
    /// makes every account's deadline durable regardless of horizon band (an account sitting
    /// [`ExpiryHorizon::Beyond`] emits no horizon edge, so this is the only line that carries it).
    FirstObservation,
    /// The daemon's OWN refresh cycle ran for this account between the two observations — the
    /// isolated #119 sweep, the #255 reactive poll-refresh, or the #282 keep-warm mint — so the
    /// change is the SERVER extending the deadline on refresh.
    ///
    /// Essentially free to attribute: each of those paths writes the credential through a
    /// CAS-protected flow the daemon drives, so it KNOWS it caused the write; there is nothing to
    /// infer. This follows the observe-through-the-real-flow discipline `crate::refresh`'s AC-3
    /// already establishes — *"the engine's OWN first days of operation are the safe multi-day
    /// observation … gathered through this CAS-protected flow, never a bespoke probe"* — so no
    /// probe is added here either.
    MyRefresh,
    /// NO daemon refresh ran for this account, yet its deadline moved: something ELSE wrote the
    /// credential. The inferred half of the pair, and the row that would license a proactive
    /// re-login cadence if re-logins turned out to reset the deadline.
    ///
    /// "Something else" means EXTERNAL TO THE DAEMON PROCESS — read it that literally, because the
    /// population is wider than *another Claude Code instance*. The latch behind this inference is
    /// armed only by the daemon's own in-process refresh paths, so any first-party write from
    /// ANOTHER process lands here too. Known members, and the reason the tag is an observation and
    /// not a verdict:
    ///
    /// - an operator's own `claude /login` or a concurrent Claude Code's in-place refresh — the
    ///   intended population;
    /// - **`sessiometer poke`** ([`crate::poke`]) — the operator refreshing an account from a
    ///   SEPARATE process. Its isolated-refresh engine re-stashes the credential and the daemon never
    ///   learns it ran, so a deadline the poke moved is reported here. First-party in spirit,
    ///   external to this process in fact, and the record cannot tell them apart. `poke` emits no
    ///   durable event of its own either, so there is nothing to correlate against — tracked as
    ///   issue #906;
    /// - a swap-lock-serialized write by any other CLI verb on the same stash;
    /// - a change of READ SOURCE — **no longer this variant, since issue #907.** The baseline now
    ///   carries the item it was read out of, so a delta measured across a switch between the
    ///   canonical and a stash is classified [`Self::ReadSourceSwitched`] and never reaches here.
    ///   Nothing about the underlying two-item design changed and no reconciliation was added: what
    ///   changed is that the daemon now KNOWS the comparison spanned two items and says so. Reaching
    ///   this variant is therefore a positive statement that both readings came out of the SAME item
    ///   — which is what makes the population below countable.
    ///
    /// The inference is one-directional in the OTHER direction from what a reader might hope: an
    /// external write that RACES the daemon's own refresh inside one poll interval is attributed
    /// [`Self::MyRefresh`] instead (the latch wins), so this variant can both MISS a real external
    /// write and — via the out-of-process paths above — name one that was really the operator's own
    /// tooling. Neither error is silent, because both leave the deadline delta on the line; what
    /// matters for issue #877 is that a `my_refresh` attribution is HARD evidence (the daemon knows
    /// it caused the write) while this one is a residual category. Weigh them differently when the
    /// spike is read, and cross-check a surprising cluster against `event=refresh` for the same
    /// `acct=` before concluding anything about re-login behaviour.
    ExternalChange,
    /// The two compared observations were read out of DIFFERENT credential items (issue #907): the
    /// deadline moved across a change of READ SOURCE, so the delta may be an artifact of where we
    /// looked rather than evidence that anything was written.
    ///
    /// [`crate::daemon::Daemon::read_poll_clocks`] reads the ACTIVE account's deadline from the
    /// shared canonical keychain item and a PARKED account's from its own stash copy, so an account
    /// that swapped in or out between two polls has its deadline read from a different item on
    /// either side of the comparison. Most swap boundaries still produce no delta and therefore no
    /// line at all — but NOT because the swap engine reconciles the two items. `swap::swap` step 2
    /// re-stashes the outgoing account from the canonical as it stands at SWAP time, so the item it
    /// parks carries that canonical's CURRENT deadline: the delta a later parked poll can see is
    /// exactly the canonical's own movement between that account's last active poll and the swap.
    /// Usually zero, and then there is no line; a token that rotated IN PLACE inside that window is
    /// parked with a changed deadline, and the daemon's own swap does produce this variant.
    ///
    /// What actually keeps the line rare is a different mechanism: the top-of-tick
    /// [`crate::daemon::Daemon::reconcile_canonical_change`] normally catches an out-of-band
    /// canonical write on the next tick, re-stashes it, and folds the baseline forward as
    /// [`Self::CanonicalRestash`] before that account is polled again. So this variant appears
    /// where that reconciliation did not get there first, which makes it a positive signal in its
    /// own right: the re-stash discipline has a gap for that account right now (a re-stash that
    /// failed on a locked keychain, a canonical write not yet reconciled, a rotation landing
    /// between the poll's own canonical read and the swap's — which the NEXT tick cannot recover
    /// either, because `record_swap` has by then committed the canonical watch to the INCOMING
    /// credential — or a swap path that re-stashes nothing for the departing account: an
    /// out-of-band `claude /login` / `use` adoption, or the #467 scrub adopt).
    ///
    /// **Marked, not suppressed, and the choice is about the reader.** Dropping the line would
    /// assert *this was not a real change* — a claim the daemon cannot support, since a genuine
    /// external write may ALSO have landed in that interval; the daemon knows only that the
    /// comparison spanned two items. Worse, it would be silent exactly where it hurts: swap
    /// boundaries are frequent and correlate with the periods an operator is most likely to have
    /// re-logged in, so suppression would blind the record over the very population issue #877 is
    /// asking about. Keeping the line preserves the delta and both deadlines for an operator, and
    /// gives an offline reader a one-token filter — the same partition-by-`provenance=` the other
    /// rows already support — instead of an absence to reason about. Naming it also keeps the
    /// residual [`Self::ExternalChange`] population countable BY DEFAULT: a reader who knows
    /// nothing of this variant now counts a cleaner set, and one who wants the union adds it back
    /// deliberately.
    ///
    /// Observational, like every variant here: it names what was true of the COMPARISON, never what
    /// the delta means. It does NOT claim the deadline is unchanged, and it does not rule an
    /// external write out — it says the evidence cannot separate the two. It is also strictly a
    /// subset of what [`Self::ExternalChange`] used to absorb; a change the daemon's own refresh
    /// caused still reports [`Self::MyRefresh`], because that attribution is hard evidence and a
    /// concurrent swap does not weaken it (see `Daemon::note_polled_expiry` for the ordering).
    ReadSourceSwitched,
    /// The daemon HEALED an out-of-band canonical write (the issue #13 re-auth re-stash) — the same
    /// external class as [`Self::ExternalChange`], but caught AT the write edge rather than inferred
    /// from a later poll delta.
    ///
    /// The ONE provenance that also records a NON-change, and the reason it exists: *unchanged
    /// across an external credential write* is the third row of the issue #877 table — the row that
    /// KILLS the proactive re-login lever — and it is structurally unobservable from change records
    /// alone, because its evidence is the absence of one. Emitting it positively at the edge turns
    /// that row from absence-reasoning into a line (`delta_secs=0`).
    ///
    /// Not per-tick noise: the EDGE is the external write (discrete and rare), not the deadline.
    CanonicalRestash,
}

impl ExpiryProvenance {
    /// The `provenance=` token for the [`Event::CredentialExpiryObserved`] log line. A FIXED
    /// `snake_case` token per variant — never dynamic text, so the line cannot carry a secret
    /// (issue #15).
    fn as_str(self) -> &'static str {
        match self {
            ExpiryProvenance::FirstObservation => "first_observation",
            ExpiryProvenance::MyRefresh => "my_refresh",
            ExpiryProvenance::ExternalChange => "external_change",
            ExpiryProvenance::ReadSourceSwitched => "read_source_switched",
            ExpiryProvenance::CanonicalRestash => "canonical_restash",
        }
    }
}

/// The shared canonical `Claude Code-credentials` item's OWN per-poll liveness (issue #464) — the
/// one keychain item every local `claude` session reads, distinct from any single roster account's
/// [`CredentialHealth`]. A closed classification carried on the `diag=canonical` per-poll line and
/// driving the edge-triggered [`Event::CanonicalScrubbed`] / [`Event::CanonicalRestored`] pair.
/// Non-secret — a bare discriminant (issue #15).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CanonicalLiveness {
    /// Readable AND carrying a live (non-empty) refresh token — a usable shared credential every
    /// session can authenticate with.
    Present,
    /// SCRUBBED / empty: readable but the tokens are cleared in place (an empty refresh token —
    /// Claude Code's first-`invalid_grant` scrub, the DEAD signal per [`crate::refresh::refresh_token`]),
    /// OR the item is gone entirely (`CredentialNotFound`). Either way no session can authenticate —
    /// the "Not logged in" state umbrella #463 exists to make visible.
    Scrubbed,
    /// UNKNOWN: the read failed for a transient / non-lock, non-not-found reason, so this poll
    /// carries no liveness evidence either way. The edge-trigger HOLDS its current signal rather
    /// than invent a scrub or a recovery from a flaky read.
    Unknown,
}

impl CanonicalLiveness {
    /// The `state=` token for the `diag=canonical` line.
    fn as_str(self) -> &'static str {
        match self {
            CanonicalLiveness::Present => "present",
            CanonicalLiveness::Scrubbed => "scrubbed",
            CanonicalLiveness::Unknown => "unknown",
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

/// The PROJECTION a #539 velocity-preemptive swap fired on (issue #634) — the ingredients that make
/// the decision self-explaining, carried on its [`Event::Swap`] as optional trailing tokens.
///
/// Both preemptive arms decide on a PROJECTION while the log recorded only the OBSERVED reading, so a
/// `reason=velocity_preempt` line at `session_pct=70` read like a bug when it had in fact fired on
/// `70 + rate × horizon >= ceiling`. Re-deriving that offline is not possible: the durable
/// [`Event::UsageVelocity`] carries a ROUNDED `i16` percent delta, not the full-precision EMA the
/// decision projected from. So the ingredient is PERSISTED here rather than re-folded.
///
/// The CONSTANTS IN FORCE are stamped alongside the ingredient, deliberately — a reader must never
/// have to apply TODAY's constants to an OLD record. `ceiling_pct` is therefore the EFFECTIVE ceiling
/// this decision actually compared against ([`crate::swap::effective_ceiling`] of the tick's ceiling
/// draw — the configured ceiling less the tail margin), not the raw configured ceiling: stamping the
/// comparand makes the whole predicate `projected_pct >= ceiling_pct` checkable from the line alone,
/// with no knowledge of the tail margin or of which ceiling semantics were in force at write time.
///
/// Every field is a bare number — a percent, a rate, or a duration — never a token or email
/// (issue #15).
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct SwapProjection {
    /// The projected session usage at the horizon, as a PERCENT — `observed + rate × horizon`,
    /// the value that crossed the ceiling and fired the swap.
    ///
    /// Deliberately an UNCLAMPED, unrounded percent (rendered to 2 decimals), unlike the `u8`
    /// `session_pct` beside it: a projection routinely exceeds 100 (a steep rate over a 120 s
    /// horizon), and clamping it to the `u8` reading domain would erase exactly the "how far over
    /// did this project?" signal the field exists to record.
    ///
    /// Stored DIRECTLY at full precision, so no reconstruction is needed — `projected_pct >=
    /// ceiling_pct` reproduces the fire decision exactly. A reader who instead cross-checks it
    /// against the DISPLAYED anchor as `projected_pct ≟ session_pct + rate_pct_per_sec × horizon_secs`
    /// should expect ±0.5 pp of slack: `projected_pct` was computed from the unrounded fraction while
    /// the `session_pct` beside it carries the `u8` field's rounding (the same anchor-rounding caveat
    /// [`BlindVelocity`] notes for its own offline recomputation).
    pub(crate) projected_pct: f64,
    /// The #539 EMA rate the projection was taken at, in **PERCENT PER SECOND**.
    ///
    /// Converted from the daemon's internal fraction-per-second at the emit so it is dimensionally
    /// consistent with the percent-valued `session_pct` / `projected_pct` / `ceiling_pct` beside it
    /// — an offline reader recomputes the projection from the line's own tokens with no unit
    /// conversion. FULL PRECISION (rendered to 6 decimals), not the rounded `i16` percent delta of
    /// [`Event::UsageVelocity`], which is too lossy to reproduce a decision.
    pub(crate) rate_pct_per_sec: f64,
    /// The projection horizon in whole seconds — the daemon's `session_velocity_horizon_secs` at
    /// the decision (an operator tunable, so it is stamped rather than assumed).
    pub(crate) horizon_secs: u64,
    /// The EFFECTIVE ceiling the projection was compared against, as a PERCENT — the tick's ceiling
    /// draw less the post-swap committed-tail margin ([`crate::swap::effective_ceiling`]). The
    /// comparand itself, so `projected_pct >= ceiling_pct` reproduces the fire decision exactly;
    /// rendered to 2 decimals because the draw is per-cycle jittered, not a round number.
    pub(crate) ceiling_pct: f64,
}

/// The retained #539 velocity signal in force during a blind window (issue #634) — the ingredient
/// that makes the report-only blind velocity-projection arm reconstructable offline, carried on
/// [`Event::BlindWindow`] as optional trailing tokens.
///
/// The blind arm ([`crate::daemon`]'s `blind_velocity_projected_armed`, issues #584/#600) reports a
/// blind account whose stale anchor, carried forward at its retained EMA rate, plausibly reaches the
/// ceiling before the daemon sees it again. It fires no swap and emits no event of its own, so its
/// inputs were invisible. `rate` is the missing one — the anchor (`session_pct`) and the window
/// (`duration_secs`) are already on the line.
///
/// Following the house log-the-ingredients / derive-the-views-offline idiom, the DERIVED projection
/// is deliberately NOT stored: an offline reader recomputes
/// `projected = anchor + rate_pct_per_sec × inflation × duration_secs` and compares it against
/// `ceiling_pct` — every term present on the one line. The `anchor` term is the #632-corrected base
/// `session_pct.max(session_high_water_pct)`: since issue #632 the live arm projects off the #619
/// plausibility-corrected anchor, and issue #670 carries the frozen high-water mark
/// (`session_high_water_pct`, present only when the anchor was stale-low) beside the RAW `session_pct`
/// precisely so the reader can apply that same `swap::plausible_anchor_session` correction and
/// reproduce the corrected arm. Absent the mark token the anchor is simply `session_pct`. (The
/// anchor term carries the `session_pct` / `session_high_water_pct` fields'
/// existing `u8` rounding, so a recomputed projection inherits up to ±0.5 pp of anchor error; over
/// any window long enough to arm the gate the rate term dominates it.)
///
/// Both constants are STAMPED rather than left to be re-derived, so a record stays interpretable
/// after either drifts — an offline reader applying today's inflation factor or today's ceiling to an
/// old window would silently mis-report it.
///
/// Present only when the arm had a USABLE signal: a retained EMA that is SUSTAINED
/// (`>= MIN_VELOCITY_SAMPLES` blended intervals). Absent tokens therefore mean "no sustained velocity
/// — this arm could not have armed here", never "unknown".
///
/// Every field is a bare number, never a token or email (issue #15).
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct BlindVelocity {
    /// The retained #539 EMA rate at the arm, in **PERCENT PER SECOND** — the rate in force through
    /// the blind window, converted from the daemon's internal fraction-per-second at the emit so it
    /// is dimensionally consistent with the percent-valued `session_pct` anchor beside it.
    ///
    /// Genuinely the PRE-BLIND rate, not a post-recovery one: the velocity fold requires a previous
    /// live reading, which a blind window by definition cleared, so the recovery poll cannot blend
    /// into the EMA before this event is emitted. FULL PRECISION (rendered to 6 decimals) — this is
    /// a load-bearing contract for the offline recomputation (issue #636 recomputes `projected` from
    /// this token), and the rounded `i16` delta of [`Event::UsageVelocity`] would not reproduce it.
    pub(crate) rate_pct_per_sec: f64,
    /// The worst-case rate-inflation factor in force ([`crate::daemon`]'s
    /// `BLIND_VELOCITY_RATE_INFLATION`) — the multiplicative bias-HIGH bound the arm applies to the
    /// EMA before projecting the stale anchor forward. Stamped because it is an interim,
    /// ratification-pending constant: it is expected to move, and an old record must not be read
    /// through a new value.
    pub(crate) inflation: f64,
    /// The session ceiling the projection is compared against, as a PERCENT — the BASE (un-jittered)
    /// ceiling, which is what this arm projects against. Distinct from [`SwapProjection`]'s
    /// `ceiling_pct`, which is the *effective* (tail-margin-subtracted) ceiling of the per-tick
    /// jittered draw: the two arms genuinely compare against different lines, so each stamps its own
    /// comparand rather than a shared one an offline reader would have to disambiguate.
    pub(crate) ceiling_pct: f64,
}

/// One observable daemon state change, rendered as a single `key=val` log line by
/// [`Event::to_log_line`].
///
/// Every field is a handle / enum / number / timestamp — or, in the single case of the resolved
/// `claude` path (issue #786), a percent-encoded filesystem location. Never a token or email
/// (issue #15). That is the type-level guarantee behind the single-surface redaction claim in the
/// module docs.
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
        /// The projection that fired a #539 velocity-preemptive swap (issue #634) — `Some` for
        /// `reason=velocity_preempt` (the arm cannot fire without one), `None` for every other
        /// reason, whose decision is fully described by the observed `session_pct` already.
        ///
        /// Rendered as additive trailing tokens, so a non-projective swap line stays byte-for-byte
        /// unchanged. See [`SwapProjection`].
        projection: Option<SwapProjection>,
    },
    /// `account`'s canonical credential changed underneath the daemon — the
    /// operator ran `claude /login` and re-authenticated it — so its stash was
    /// refreshed to the new token (issue #13 re-auth re-stash). `account` is the
    /// HANDLE (operator label), resolved from the new canonical's identity.
    ReStash { account: String },
    /// The active account is over a trigger but no other account is a viable swap
    /// target — the all-exhausted terminal state (issue #11). `hold` is the
    /// account relief arrives on FIRST. `cause` (issue #398) is WHY relief is blocked:
    /// the dimension gating that account — [`SwapReason::Session`] when it is held out
    /// by session, [`SwapReason::Weekly`] when by its weekly window (or, if blocked on
    /// both, whichever clears LAST, since it needs both). Which account wins is decided
    /// ACROSS dimensions (issue #665): a spare deep into its weekly window can return
    /// before a session-blocked one, so neither dimension outranks the other a priori.
    /// `hold` and `resets_at` name that cause's reset. `resets_at` is that reset as epoch seconds,
    /// rendered to RFC 3339 by [`Event::to_log_line`] and present whenever the API
    /// supplied a parseable timestamp; `None` (the field is omitted) when no account
    /// reported one, keeping the line forward-compatible.
    AllExhausted {
        hold: String,
        cause: SwapReason,
        resets_at: Option<i64>,
    },
    /// The daemon LEFT the all-exhausted terminal state (issue #11): a viable swap target is
    /// possible again. The edge-triggered EXIT partner of [`Self::AllExhausted`]'s ENTER, so an
    /// exhaustion episode's SPAN is bracketed in the DURABLE log — the sibling idiom of
    /// [`Self::UsageBackoffCleared`] / [`Self::ExhaustedSlowPollCleared`].
    ///
    /// Durable since issue #800 (REQ-STA-B-011): previously a stderr-bound
    /// `Diagnostic::AllExhaustedCleared`, so the bracket closed in CODE but not in the LOG.
    /// Issue #775 later made the diagnostic channel *reachable* for a background daemon
    /// (`[tunables] verbose` honored under `--managed`, plus `sessiometer log --channel diag`),
    /// which is why the promotion no longer rests on reachability — it rests on being always-on
    /// and on GOVERNANCE. Diagnostics are opt-in and default OFF (the reader defaults to the event
    /// channel), so on a default install the LEAVE edge is simply absent; and `daemon.err.log` is
    /// an ungoverned channel that can carry panic payloads which passed no redaction meter,
    /// whereas every field of this log is a handle, enum, number or timestamp by type-level
    /// construction and passes the issue #15 meter. A LEAVE marker whose ENTER partner is a
    /// durable, governed event belongs on the same sink as it. This variant is the family's
    /// canonical statement of that argument: [`Self::ActiveDeadNoTargetCleared`] and
    /// [`Self::FleetRunwayRecovered`] were promoted for the same reason and point here.
    ///
    /// What the original defect looked like, measured when issue #800 was filed: 2026-07-28, 155
    /// `event=all_exhausted` lines against 0 `diag=` lines of any kind, a `run -v` daemon's fd 2
    /// being a TTY. A hold's END was therefore not reconstructable offline, and the tightest bound
    /// the durable log alone allowed (`min(next ENTER, next swap) − ENTER`) overstated duration
    /// ~25×. That measurement is the empirical record of why the bracket was unreconstructable —
    /// history, not the standing justification.
    ///
    /// NOT sufficient on its own to recover true episode counts, and a consumer MUST NOT assume
    /// every post-#800 ENTER has a matching END. Three distinct sources of an unclosed or
    /// duplicated bracket survive this change:
    ///
    /// 1. The ENTER guard re-arms on ANY non-`NoViableTarget` tick, so ENTER edges OVER-count
    ///    episodes — ENTER/LEAVE pairs still need debouncing (REQ-STA-B-010).
    /// 2. `DecisionState::signaled_all_exhausted` is IN-MEMORY only, never persisted. A daemon
    ///    restart while a hold is open therefore drops the pending LEAVE (an ENTER with no END)
    ///    and, because the guard comes back clear, re-emits a DUPLICATE ENTER on the next
    ///    `NoViableTarget` tick. Deriving duration from that second ENTER understates the hold.
    /// 3. Historical windows predating #800 carry ENTERs with no END at all.
    ///
    /// Duration is not backfillable, and REQ-STA-B-010's "report as a bound, marked as such"
    /// discipline still has to hold for any bracket left open by (2) or (3).
    ///
    /// Payload-free by design: the ENTER line already carries `hold` / `cause` / `resets_at`, and
    /// the leave is just "the state ended". Secret-free by construction (issue #15) — the line is
    /// a timestamp and a closed token, with no field that could hold a label, email, or token.
    AllExhaustedCleared,
    /// The ACTIVE account's credential is DEAD *and* no live account is a viable emergency
    /// swap target — the strictly-WORSE sibling of [`Self::AllExhausted`] (issue #405). The
    /// emergency path (issue #42) drops the `target_max_session_usage` reserve AND the session gate, so
    /// the ONLY remaining filter is weekly exhaustion: reaching here means every live spare is
    /// weekly-exhausted, and the daemon HOLDS on the dead active with no way to escape. Until now
    /// this state returned SILENTLY — a strictly-worse condition than `all_exhausted` yet emitting
    /// strictly-less signal; this event closes that asymmetry so the strand is diagnosable from the
    /// log alone (issue #399), naming the real blocker and when it lifts.
    ///
    /// `hold` is the DEAD active's HANDLE (operator label) — the account the daemon is stuck on,
    /// and the account to run `claude /login` against (it doubles as the re-login target). `cause`
    /// (always [`SwapReason::Weekly`] on this path — the session gate is bypassed, so a
    /// session-only block cannot arise) and `resets_at` are the fleet-capacity RELIEF hint from
    /// `all_exhausted_relief`, naming WHEN a spare's weekly window frees capacity; `resets_at` is
    /// that reset as epoch seconds (RFC 3339 in [`Event::to_log_line`], present whenever a spare
    /// reported a parseable weekly reset, omitted otherwise). Edge-triggered like `all_exhausted`:
    /// emitted exactly ONCE on entering the strand, cleared by [`Self::ActiveDeadNoTargetCleared`].
    /// Secret-free by construction (#15): a closed enum + a label + a timestamp, never a token or email.
    ActiveDeadNoTarget {
        hold: String,
        cause: SwapReason,
        resets_at: Option<i64>,
    },
    /// The daemon LEFT the active-dead-no-target strand (issue #405): the dead active recovered
    /// (re-login / spontaneous revive) or a live target became reachable again. The edge-triggered
    /// EXIT partner of [`Self::ActiveDeadNoTarget`]'s ENTER, so the strand's SPAN is bracketed in
    /// the DURABLE log — the sibling idiom of [`Self::AllExhaustedCleared`].
    ///
    /// Durable since issue #827, for the same reason as [`Self::AllExhaustedCleared`]: the
    /// diagnostic channel is opt-in, default OFF, and ungoverned, while this log is always-on and
    /// passes the issue #15 redaction meter by type-level construction. See that variant for the
    /// full governance argument.
    ///
    /// A consumer MUST NOT assume every ENTER has a matching END, nor that ENTER edges count
    /// episodes. Three sources of an unclosed or duplicated bracket survive this change, mirroring
    /// [`Self::AllExhaustedCleared`]'s:
    ///
    /// 1. The ENTER guard re-arms on ANY non-strand tick, so a strand that flickers — stranded,
    ///    one non-strand tick, stranded again — emits two ENTER/LEAVE pairs for what an operator
    ///    would call one episode. ENTER edges therefore OVER-count, and pairs still need debouncing.
    /// 2. `DecisionState::signaled_active_dead_no_target` is IN-MEMORY only, never persisted, so a
    ///    daemon restart mid-strand drops the pending LEAVE (an ENTER with no END) and, because
    ///    the guard comes back clear, re-emits a DUPLICATE ENTER on the next strand tick —
    ///    deriving duration from that second ENTER understates the strand.
    /// 3. Windows predating issue #827 carry ENTERs with no END at all, and are not backfillable.
    ///
    /// Payload-free by design: the ENTER line already carries `hold` / `cause` / `resets_at`, and
    /// the leave is just "the strand ended". Secret-free by construction (issue #15) — the line is
    /// a timestamp and a closed token, with no field that could hold a label, email, or token.
    ActiveDeadNoTargetCleared,
    /// The aggregate fleet runway (issue #544 — the roster's combined weekly head-room over its
    /// combined observed burn) dropped BELOW the operator's `fleet_runway_warn_secs` threshold —
    /// the PROACTIVE lead-time warning (issue #650) ahead of the all-exhausted terminal state
    /// (#11), which [`Self::AllExhausted`] reports only reactively, AT exhaustion. Purely an
    /// operator-visibility signal: no swap decision reads it (the non-goal the issue pins).
    ///
    /// Edge-triggered like `all_exhausted`: emitted exactly ONCE on the downward crossing, held
    /// silent while the runway stays below, re-armed by the daemon once a KNOWN reading is back
    /// at/over the threshold ([`Self::FleetRunwayRecovered`] marks that leave edge).
    /// `runway_secs` is the crossing reading, `threshold_secs` the configured warn line, and
    /// `counted`/`observed` the aggregate's `n of m` honesty cardinality (how many accounts
    /// backed the figure vs were seen — #544's honest-degradation surface, carried so the line
    /// states how much fleet stands behind it). Secret-free by construction (#15): integers
    /// only, never an account handle, email, or token.
    FleetRunwayLow {
        runway_secs: i64,
        threshold_secs: i64,
        counted: usize,
        observed: usize,
    },
    /// The aggregate fleet runway recovered to at/over the operator's `fleet_runway_warn_secs`
    /// threshold (issue #650): the warning re-armed, so a LATER downward crossing signals afresh.
    /// The edge-triggered EXIT partner of [`Self::FleetRunwayLow`]'s ENTER, so a low-runway
    /// episode's SPAN is bracketed in the DURABLE log — the sibling idiom of
    /// [`Self::AllExhaustedCleared`].
    ///
    /// Durable since issue #827, for the same reason as [`Self::AllExhaustedCleared`]: the
    /// diagnostic channel is opt-in, default OFF, and ungoverned, while this log is always-on and
    /// passes the issue #15 redaction meter by type-level construction. See that variant for the
    /// full governance argument.
    ///
    /// Its bracket caveats apply here too, with differences in the first and third. A consumer
    /// MUST NOT assume every ENTER has a matching END, nor that ENTER edges count episodes:
    ///
    /// 1. A runway OSCILLATING around the threshold across successive cadence windows emits one
    ///    ENTER/LEAVE pair per crossing, so ENTER edges OVER-count what an operator would call one
    ///    low-runway episode. Unlike the sibling guards, though, this one re-arms ONLY on a KNOWN
    ///    at/over reading — an UNKNOWN aggregate holds it — so a merely-unreadable window cannot
    ///    split an episode.
    /// 2. `DecisionState::signaled_fleet_runway_low` is IN-MEMORY only, so a daemon restart
    ///    mid-episode drops the pending LEAVE and re-emits a duplicate ENTER on the next downward
    ///    crossing.
    /// 3. Windows predating issue #827 carry ENTERs with no END at all, and are not backfillable.
    ///
    /// Fires only on a KNOWN at/over-threshold reading: an UNKNOWN aggregate (store hiccup,
    /// degraded overlay, or a counted-but-FLAT fleet) HOLDS the armed state instead, so a flaky
    /// read never fabricates a recovery. Payload-free by design — the ENTER line carried the
    /// reading, and the recovery is just "back over". Secret-free by construction (issue #15): a
    /// timestamp and a closed token, never an account handle, email, or token.
    FleetRunwayRecovered,
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
    /// The SHARED canonical `Claude Code-credentials` item was observed SCRUBBED/EMPTY — its
    /// tokens cleared in place (Claude Code's first-`invalid_grant` scrub) or the item gone
    /// entirely — so every local `claude` session now reads "Not logged in" (issue #464,
    /// umbrella #463). Edge-triggered: emitted exactly ONCE on the transition INTO the scrubbed
    /// state, never per poll while it stays scrubbed, and afresh if it recovers and is scrubbed
    /// again. `account` is the last-known active HANDLE (operator label) the scrub emptied, or
    /// absent when no active account was resolved (e.g. a daemon started against an already-empty
    /// item) — never a token or email (issue #15). Renders `mode=scrub` (issue #475): the
    /// UNRECOVERABLE (`/login`-needed) half of the two-mode `mode=(yank|scrub)` classification — the
    /// RECOVERABLE rotation-yank is carried as `mode=yank` on the `diag=canonical` line, so an
    /// operator `grep mode=` sees both "Not logged in" causes and their opposite remedies. Distinct
    /// from [`Event::CredentialDead`] (a PER-ACCOUNT quarantine edge keyed on a 401-streak): this is
    /// the SHARED item's OWN liveness, the fleet-wide lockout no `credential_dead` fires for — the
    /// umbrella's core observability gap.
    CanonicalScrubbed { account: Option<String> },
    /// The shared canonical item RECOVERED — read live again (a non-empty refresh token) after a
    /// scrub, so sessions can authenticate once more (issue #464). The clearing counterpart of
    /// [`Event::CanonicalScrubbed`], mirroring the [`Event::CredentialDead`] /
    /// [`Event::CredentialRestored`] durable-pair idiom. Edge-triggered: exactly ONCE on the
    /// recovery transition. `account` is the newly-resolved HANDLE, or absent — never a token or
    /// email (issue #15).
    CanonicalRestored { account: Option<String> },
    /// The daemon AUTONOMOUSLY adopted a viable roster account's token into a scrubbed/empty
    /// canonical (issue #467), healing every local `claude` session on its next request — no
    /// operator action. The narrow carve-out from ADR-0007 decision 4 that ADR-0018 decision 1
    /// automates: recovery for a scrubbed canonical was `use --force`-gated, but when the canonical
    /// is empty AND a live target exists (NOT the genuinely-all-dead `active_dead_no_target` case),
    /// the daemon may adopt without the gate. `account` is the ADOPTED target's HANDLE (operator
    /// label), never a token or email (issue #15). Distinct from [`Event::CanonicalRestored`], which
    /// is the OBSERVATION that the item reads live again on a later poll (and also fires after an
    /// operator `claude /login`): this names the daemon's own recovery ACTION and WHICH account it
    /// adopted, emitted at adopt time.
    CanonicalRecovered { account: String },
    /// The daemon BACKED OFF autonomous scrubbed-canonical recovery (issue #467): the canonical was
    /// re-scrubbed more than the bound allows within the churn window (a persistent multi-session
    /// rotation churn), so continuing to adopt would thrash the re-auth loop. The daemon holds and
    /// leaves the `canonical_scrubbed` signal up for the operator (status + menubar, issue #469)
    /// rather than churning. Edge-triggered: emitted ONCE per back-off episode, afresh after the
    /// churn window resets and recovery resumes. `account` is the last-known active HANDLE the scrub
    /// emptied, or absent when none was resolved — never a token or email (issue #15).
    CanonicalRecoveryExhausted { account: Option<String> },
    /// The behavioral canary (issue #714) detected keychain-identity DRIFT: the resolved canonical
    /// credential byte-matches `matched`'s stash while Claude Code's own state (`~/.claude.json`)
    /// names `displayed` active — evidence the #100 derivation no longer points at the credential
    /// Claude Code is actually using. Unless `overridden` (the documented `canary_drift_override`
    /// tunable let the write proceed), the daemon REFUSES credential writes while this holds
    /// (pre-mutation, zero writes); reads / poll / `status` stay live. DAEMON-side this is
    /// edge-triggered: emitted exactly ONCE on entering the drift state (boot canary or a
    /// pre-swap re-check), never per refused swap while it persists, with
    /// [`Event::CanaryCleared`] marking the leave edge. The STANDALONE daemon-down `use` path
    /// has no carried state to edge off, so it emits one line per refused/overridden
    /// invocation instead — each line there IS one operator action. Both fields are HANDLES
    /// (operator labels), never a token, email, or account-uuid (issue #15).
    CanaryDrift {
        displayed: String,
        matched: String,
        overridden: bool,
    },
    /// The behavioral canary (issue #730) refused a credential write because the resolved
    /// canonical matches NO account stash AND does not parse as a Claude Code credential —
    /// overwhelmingly an UNRELATED secret an atomic `-U` upsert would clobber unrecoverably.
    /// Unless `overridden` (the dedicated `canary_nostashmatch_override` tunable let the write
    /// proceed), the write is refused (pre-mutation, zero writes); reads / poll / `status` stay
    /// live. Emitted at the REFUSAL site (the daemon pre-swap gate and the standalone daemon-down
    /// `use` path) — one line per refused/overridden attempt. Issue #738 gave the refusal its own
    /// `status` surface ([`CanaryStatus::RefusedUnparseableCanonical`](crate::daemon::CanaryStatus::RefusedUnparseableCanonical)),
    /// which does NOT retire this per-attempt line: the two carry different facts. The wire is
    /// LEVEL-triggered — it says the refusal stands right now — while this log COUNTS ATTEMPTS,
    /// and a verdict edge cannot count the swaps that kept being blocked while the verdict held
    /// steady. That is why this event stays per-attempt rather than adopting the edge-triggered
    /// idiom of [`Event::CanaryDrift`]. Secret-free (issue #15): only the `overridden` flag, never
    /// a token or the canonical's bytes.
    CanaryUnparseableCanonical { overridden: bool },
    /// The behavioral canary's opt-in Layer-3 ONLINE liveness probe (issue #736) did not
    /// confirm that the resolved canonical credential still authenticates. `verdict` is the
    /// probe class — `rejected` (the endpoint answered `401`) or `inconclusive` (no HTTP
    /// response, a `429`/`5xx`, a missing scope, an unparseable body, an unreadable
    /// keychain). `refused` says whether that cost the swap: `true` only under the opt-in
    /// `canary_online_probe_strict`, where the write is refused pre-mutation (ZERO writes);
    /// `false` is the default graceful-degrade posture, where the swap PROCEEDED and this
    /// line is the only trace of the failed probe — which is exactly why it is emitted even
    /// then.
    ///
    /// ALARM-ONLY, like its `canary_drift` / `canary_unparseable_canonical` siblings: a probe
    /// that confirms liveness emits nothing, and a DISARMED probe emits nothing (it never ran).
    /// Emitted at the pre-swap gates — the daemon's and the standalone daemon-down `use`
    /// path's — one line per probed ATTEMPT rather than per verdict edge, since the probe
    /// yields no standing verdict to edge off (it runs per swap, not per tick). Secret-free
    /// (issue #15): a verdict CLASS and a flag, never a status code, a response body, or a
    /// bearer.
    CanaryOnlineProbe {
        verdict: &'static str,
        refused: bool,
    },
    /// The behavioral canary's FRESH Layer-1 resolution probe (issue #714) found MORE THAN ONE
    /// item under the derived canonical service — the #100 uniqueness rule fails, so the
    /// derivation no longer addresses a single credential and the atomic in-place write has no
    /// unique, safe target. Credential writes are refused while this holds (no override — unlike
    /// drift, ambiguity has no false-positive story); reads keep answering from the boot-pinned
    /// item. Edge-triggered like [`Event::CanaryDrift`]; [`Event::CanaryCleared`] marks the leave
    /// edge. Carries only the item COUNT (issue #15).
    CanaryAmbiguous { count: usize },
    /// The behavioral canary (issue #714) left a [`Event::CanaryDrift`] or
    /// [`Event::CanaryAmbiguous`] episode for a verdict that opens no alarm of its OWN — the quiet
    /// `ok` / `inconclusive` / `not_found`, or (since issue #738) `refused_unparseable_canonical`,
    /// whose durable line is per-attempt and owned by the refuse sites rather than the verdict
    /// edge. The closing bracket of the alarm edge, mirroring the [`Event::CanonicalScrubbed`] /
    /// [`Event::CanonicalRestored`] durable-pair idiom (an OVERRIDDEN drift still counts as an
    /// episode: the alarm was real even though the write proceeded). Edge-triggered: exactly ONCE
    /// per episode. No fields — the fresh verdict is on the `status` surface (issue #15).
    CanaryCleared,
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
    /// after the cycle — each `None` only when the stored expiry was unreadable; when BOTH
    /// are present, [`to_log_line`](Event::to_log_line) also renders their difference as a
    /// `window_secs=` field (issue #409 — the sliding-window slide, in whole seconds). `account`
    /// is the HANDLE (operator label), never a token or email; the expiry is a plain
    /// timestamp. A cycle that refreshes a quarantined account back to life additionally
    /// drives a separate [`Event::CredentialRestored`] (the restore, applied daemon-side).
    Refresh {
        account: String,
        outcome: RefreshEventOutcome,
        expires_before: Option<i64>,
        expires_after: Option<i64>,
        /// The non-secret error sub-class on an `outcome=error` line (issue #377), rendered as
        /// an additive TRAILING `reason=` field (precedent: `late=` / `rotated=`) so existing
        /// `key=val` parsers are unaffected. `Some` ONLY on an error whose sub-cause is
        /// classifiable secret-free (the three completed-cycle sub-causes, plus `timeout` and —
        /// since issue #786 — `unresolved`); `None` on every non-error outcome AND on a hard
        /// engine `Err` that STILL carries no such class (a locked keychain, a contended lock,
        /// an FS error) — in both cases NO `reason=` is rendered. A [`RefreshEventReason`] is a
        /// fixed enum token, never dynamic error text, so it cannot carry a secret onto the line.
        reason: Option<RefreshEventReason>,
        /// The per-account refresh back-off armed by THIS `outcome=error` cycle (issue #408),
        /// in seconds — the window before this account's next sweep-refresh is permitted. `Some`
        /// ONLY on an `error` outcome that advanced the account's back-off streak (the periodic
        /// #105 sweep; the widening mirror of the poll path's `backoff_secs` on `diag=tick`);
        /// `None` on every non-error outcome (which CLEARS the streak) and on the poll-path
        /// refresh (#162, which does not sweep-throttle). Rendered as an additive TRAILING
        /// `backoff_secs=` field after `reason=`, so a non-throttled refresh line is byte-for-byte
        /// unchanged and every existing `key=val` parser is unaffected. A bare integer, never a
        /// token or handle — the #15 single-surface guarantee holds.
        backoff_secs: Option<u64>,
    },
    /// One isolated refresh of the PARKED `account` fired (issue #255). This records the *action*
    /// — that an isolated refresh fired and how it classified — the durable complement to the
    /// DOWNSTREAM poll outcome ([`Event::Monitor401`] / [`Event::CredentialDead`]) that
    /// [`crate::daemon`]'s `note_poll_outcome` already logs. `outcome` is the same non-secret
    /// [`RefreshEventOutcome`] projection [`Event::Refresh`] carries (one shared refresh-outcome
    /// vocabulary). Never fires for the ACTIVE account, on either path (issue #253). `account` is
    /// the HANDLE (operator label), never a token or email — the #15 single-surface guarantee.
    ///
    /// `trigger` names WHICH condition fired it — [`PollRefreshTrigger`], two-valued since issue
    /// #1367. Read that enum for why the field exists rather than the `poll_401` literal this line
    /// rendered from #255 until then; the short version is that issue #643 gave the event a second
    /// origin (the `restored`-driven re-probe of a `Dead` parked credential) and left the literal
    /// standing. `trigger` is ALSO what [`crate::reliability`]'s refresh-token-loss split reads to
    /// keep the two origins in separate buckets.
    PollRefresh {
        account: String,
        trigger: PollRefreshTrigger,
        outcome: RefreshEventOutcome,
    },
    /// The in-place ACTIVE-account keep-warm (issue #282, the FOURTH refresh mechanism) ran one
    /// cycle for `account`: the daemon minted a fresh token by driving `claude` and — on a real
    /// refresh — PROMOTED it to the canonical `Claude Code-credentials` item a live session reads
    /// (never the STASH the #253-excluded engine writes). Records the *action* — that a keep-warm
    /// fired, what `trigger`ed it (the three [`KeepWarmTrigger`] conditions), and how it classified.
    /// `outcome` is the same non-secret [`RefreshEventOutcome`] projection [`Event::Refresh`] /
    /// [`Event::PollRefresh`] carry (one shared refresh-outcome vocabulary):
    /// `refreshed_not_restashed` on a real mint (a
    /// keep-warm PROMOTES rather than re-stashes, so it never renders `refreshed`), else
    /// `no_change` / `dead` / `error`. `account` is the HANDLE (operator label), never a token or
    /// email — the #15 single-surface guarantee.
    KeepWarm {
        account: String,
        trigger: KeepWarmTrigger,
        outcome: RefreshEventOutcome,
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
    /// nothing account-specific to carry (#15). The CLOSING bracket of a systemic episode however
    /// it was opened — by the streak crossing above, or by the startup preflight below.
    RefreshSystemicRecovered,
    /// The daemon's STARTUP PREFLIGHT could not resolve the `claude` binary (issue #787): before a
    /// single refresh sweep ran, the mechanism's precondition was already broken, so every eligible
    /// account's cycle is guaranteed to fail (`reason=unresolved`, issue #786) until an operator
    /// intervenes.
    ///
    /// The SECOND opening bracket of a systemic-failure episode. Until #787 the state behind
    /// [`Event::RefreshSystemicFailure`] was pure in-memory, so a daemon restart ERASED an open
    /// episode and the board went green over an unfixed fault for another N sweeps — with launchd's
    /// `KeepAlive { SuccessfulExit: false }` re-opening that window on every abnormal exit. The
    /// preflight re-establishes the fault AT startup instead of re-deriving it N sweeps later.
    ///
    /// Deliberately DISTINCT from [`Event::RefreshSystemicFailure`] rather than a synthesized
    /// `consecutive=1` of it: an episode opened by a startup probe and one opened by a genuine
    /// N-sweep crossing are different facts, and a DIAGNOSABILITY fix must not make them
    /// indistinguishable on the log. The same call issue #786 made in splitting
    /// [`Event::RefreshBinaryResolved`] out of `reason=` rather than overloading it. So an episode
    /// has TWO possible opening brackets — this one or `refresh_systemic_failure` — and exactly one
    /// closing bracket, [`Event::RefreshSystemicRecovered`]: the log-balance audit that found #787
    /// in the first place (one `failure`, zero `recovered`) still works, on a two-token alternation.
    ///
    /// NO fields at all, so it is #15-clean by construction with nothing to redact: a preflight is
    /// one daemon-global observation with no count to carry, and the resolved path deliberately
    /// stays off it — a SUCCESSFUL resolution has its own [`Event::RefreshBinaryResolved`] line,
    /// and a failed one has no path to name.
    RefreshPreflightUnresolved,
    /// The absolute path of the `claude` binary the refresh sweep RESOLVED and is spawning
    /// (issue #786) — the "WHICH `claude` did the daemon pick?" question the log previously could
    /// not answer at all. With three resolution tiers (`[refresh].claude_bin`, `$CLAUDE_BIN`, the
    /// harvested login-shell `PATH`, #784) and no symlink canonicalization, an operator staring at
    /// a failing refresh cannot otherwise tell a shadowed `claude` from the one they meant.
    ///
    /// EDGE-TRIGGERED, not per-resolution. Resolution runs per ACCOUNT per CYCLE (#375 put it at
    /// the spawn site deliberately), so an unconditional line would be N identical lines every
    /// sweep, forever — burying the signal it exists to give and working against #15's "carry only
    /// what is needed". Emitted on the FIRST resolution and on every CHANGE of the resolved path,
    /// including the first success after a failure (a `claude` that came back is as worth seeing
    /// as one that moved); an unchanged re-resolution is silent, which is also what collapses one
    /// sweep's N same-binary resolutions to a single line.
    ///
    /// DELIBERATELY a SEPARATE event from the `reason=unresolved` [`Refresh`](Self::Refresh) line,
    /// and judged against issue #15 on its own footing: a filesystem location is not a credential
    /// — the standing this crate's [`crate::error::Error`] variants already give a path — and
    /// keeping it off `reason=` is what preserves that field's fixed-token guarantee.
    ///
    /// `path` is the first FREE-FORM value this channel has ever carried — every other field is a
    /// handle, an enum, a number or a timestamp — and a path may legally contain a space, so it is
    /// rendered percent-encoded rather than raw. [`path_value`] owns that argument, including why
    /// field POSITION alone would not have been enough.
    RefreshBinaryResolved { path: PathBuf },
    /// `account`'s 5-state credential-health rollup (issue #119) TRANSITIONED to `state`
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
    /// A per-account usage poll ARMED (or widened) its rate-limit / transient back-off window
    /// (issue #399): the durable complement of the stderr-only `diag=tick backoff_secs=…
    /// retry_after_secs=…` line, so a back-off episode is diagnosable from
    /// `~/Library/Logs/sessiometer/sessiometer.log` alone. `class` distinguishes a `429`
    /// (rate-limit) from a `5xx` / network transient — what makes the "429 count" queryable;
    /// `consecutive` is the account's per-account back-off streak (#293, the exponential-widening
    /// driver, the same running-count idiom [`Event::Monitor401`] carries); `retry_after_secs` is
    /// the RAW server-advised `Retry-After` when the response supplied one, BEFORE any
    /// [`crate::daemon`] `POLL_BACKOFF_CAP` clamp (#294/#295 — peer-only since #453, where the
    /// active account hard-floors `Retry-After` un-clamped); and `backoff_secs` is the resulting
    /// armed window (the effective wait). Emitted on EACH throttled poll, not just the first, so
    /// the durable log shows the window WIDEN across the episode — the residual-late-swap signal
    /// (#363/#368/#369) that a single first-throttle line would hide. `account` is the account
    /// UUID — a non-PII identifier secret-free BY CONSTRUCTION, never the operator `label` (which
    /// is free-form and PII-capable); the same uuid handle [`Event::UncapturedLogin`] carries (#15).
    UsageBackoff {
        account: String,
        class: BackoffClass,
        consecutive: u32,
        retry_after_secs: Option<u64>,
        backoff_secs: u64,
    },
    /// A per-account back-off window CLEARED (issue #399): after an armed window, the account
    /// polled a NON-throttling outcome (a success / 401 / 403), so its streak + window reset and it
    /// re-polls on the normal cadence. The edge-triggered EXIT partner of [`Event::UsageBackoff`]'s
    /// ENTER, so a back-off episode's SPAN is bracketed in the durable log. Emitted ONLY when a
    /// window was actually armed — a plain clean poll (no prior back-off) stays silent, mirroring
    /// [`Event::UsageRollup`]'s no-op silence. `account` is the account UUID — never the operator
    /// `label` (#15), matching [`Event::UsageBackoff`].
    UsageBackoffCleared { account: String },
    /// A NON-active peer entered the WIDENED, reset-aware slow-poll cadence (issue #537): a
    /// successful poll read it out of rotation (weekly- or session-exhausted), so the daemon
    /// arms a per-account `exhausted_poll_until` window and SKIPS its poll until the window
    /// elapses — bounded by `exhausted_poll_secs` (the hourly ceiling) and pulled earlier by a
    /// known `resets_at`. The `window_secs` is that armed window. Edge-triggered: emitted ONCE
    /// on the normal→slow transition (NOT re-emitted on each re-arm while the peer stays
    /// exhausted — it never LEFT the widened cadence in between), the entry partner of
    /// [`Event::ExhaustedSlowPollCleared`]. The sibling idiom of [`Event::UsageBackoff`], but a
    /// DISTINCT signal: this is a quota-exhaustion poll-cadence policy, not a 429/5xx rate-limit
    /// back-off — overloading `usage_backoff` would fire spurious rate-limit events on an
    /// HTTP-200 success (ADR-0009 / ADR-0019). `account` is the account UUID — a non-PII
    /// identifier secret-free BY CONSTRUCTION, never the operator `label` (issue #15).
    ExhaustedSlowPoll { account: String, window_secs: u64 },
    /// A peer LEFT the widened slow-poll cadence (issue #537): a later poll read it viable
    /// again (below both triggers) OR it was promoted to the active account (which is exempt
    /// and polled at full cadence), so its `exhausted_poll_until` window is cleared and it
    /// returns to the normal cadence. The edge-triggered EXIT partner of
    /// [`Event::ExhaustedSlowPoll`]'s ENTER, so the slow-poll episode's SPAN is bracketed in
    /// the durable log. Emitted ONLY when a window was actually armed — a plain viable poll
    /// with no prior slow-poll stays silent, mirroring [`Event::UsageBackoffCleared`].
    /// `account` is the account UUID — never the operator `label` (issue #15).
    ExhaustedSlowPollCleared { account: String },
    /// The ACTIVE account entered the near-limit poll-coverage fast-poll (issue #540): its reading —
    /// or the #539 velocity projection — reached the near-limit band, so the daemon TIGHTENED its
    /// poll sub-interval to `sub_interval_secs` (the `near_limit_poll_secs` cap) so no long poll gap
    /// opens on the final climb to the limit. The near-limit-scoped MIRROR of
    /// [`Event::ExhaustedSlowPoll`] (#537), which WIDENS an idle peer — same edge-triggered idiom,
    /// opposite direction. Emitted ONCE on the below-band → near-limit transition (NOT re-emitted on
    /// each held near-limit tick while the active stays in the band), and UNPAIRED: the band ends at
    /// a swap (its own [`Event::Swap`], `session_pct` at swap-out) or a below-band / blind reading,
    /// so no CLEARED partner is needed to bracket the span. A quota-poll-cadence policy, distinct
    /// from the 429/5xx `usage_backoff` rate-limit (ADR-0009), like its `ExhaustedSlowPoll` sibling.
    /// `account` is the account UUID — a non-PII identifier secret-free BY CONSTRUCTION, never the
    /// operator `label` (issue #15); `sub_interval_secs` is a bare duration, never a token.
    NearLimitPollCoverage {
        account: String,
        sub_interval_secs: u64,
    },
    /// The per-account usage VELOCITY between the last two readings (issue #399, normalized to
    /// %/min by issue #449): the SIGNED change in each rounded-percent dimension since the account's
    /// previous reading (`to_pct(next) - to_pct(prev)`), carried alongside the `elapsed_secs`
    /// interval between those two readings so the durable log expresses how fast each account is
    /// climbing as a TIME-NORMALIZED rate — the measurement the gated bounded-blindness spike (#451)
    /// and the adaptive-trigger follow-up (#368) are waiting on. A raw per-reading delta is
    /// ambiguous without its interval (a `+7` over one minute burns far faster than `+7` over ten),
    /// so [`to_log_line`](Event::to_log_line) derives and renders `session_pct_per_min` /
    /// `weekly_pct_per_min` from the stored delta and `elapsed_secs` — the same
    /// store-the-ingredients-derive-the-view idiom as the refresh line's `window_secs`. Emitted only
    /// when the account measurably MOVED (a non-zero delta in either dimension) AND the interval is
    /// known and positive, so a flat idle account stays silent (again mirroring
    /// [`Event::UsageRollup`]); never emitted across a poll gap (a throttle / failure clears the
    /// prior reading AND its timestamp), so a delta always spans two real consecutive readings.
    /// `session_delta_pct` / `weekly_delta_pct` are POSITIVE when usage is rising and NEGATIVE when a
    /// window reset dropped it. `account` is the account UUID (#15); both deltas are bare signed
    /// percents (a difference of two `0..=100` values, so within `-100..=100`) and `elapsed_secs` a
    /// bare duration, never a token.
    UsageVelocity {
        account: String,
        session_delta_pct: i16,
        weekly_delta_pct: i16,
        /// Whole seconds between the two readings this velocity spans (issue #449) — the interval
        /// [`to_log_line`](Event::to_log_line) divides the deltas by to render the %/min rate.
        /// Always `> 0` at emission (the daemon suppresses a zero / unknown interval), so the rate
        /// derivation never divides by zero.
        elapsed_secs: u64,
    },
    /// The ACTIVE account's session-usage BLIND WINDOW just closed (issue #449, umbrella #363 Path
    /// B): the account had gone blind — a `429` / `5xx` / failed poll cleared its
    /// [`crate::daemon`] `last_reading` slot so [`crate::swap::decide`] had no reading to act on —
    /// and this poll read it live again. `duration_secs` is how long it was blind, measured from the
    /// retained pre-blind anchor ([`crate::daemon`]'s `last_good`, issue #450) to this recovery — the
    /// same `blind_elapsed` the bounded-blindness preemptive swap (#452, ADR-0017) keys off, now made
    /// durable so the spike (#451) can derive its constants from real blind-window distributions
    /// rather than a replay. `session_pct` is the anchor's session usage (how NEAR the limit the
    /// account was when it went blind), and `near_limit` tags whether that anchor was at/over the
    /// session trigger (the "risk band"): the two together export BOTH umbrella SLIs — the
    /// blind-window duration (`duration_secs`) and time-blind-near-limit (`duration_secs` summed over
    /// the `near_limit=true` episodes, the anchor being fixed for a whole window). Edge-triggered:
    /// emitted exactly ONCE per blind episode, on the recovery poll — never per held blind tick, and
    /// only for the ACTIVE account (the anchor belongs to it by identity). `account` is the account
    /// UUID (#15) — matching the usage-family `acct=` (never the free-form label); every other field
    /// a bare number / bool, never a token or email.
    ///
    /// `session_at_recovery` is the POST-RECOVERY swap-necessity SLI (issue #482): the FRESH session
    /// pct the account read live at this recovery poll, distinct from `session_pct` (the STALE
    /// pre-blind anchor). Together with the anchor (`session_pct`) and the anchor's age
    /// (`duration_secs` doubles as the "anchor_age" #482 names — both are `now - anchor.at`), it
    /// reconciles whether a HYPOTHETICAL (or, once #452 lands, actual) preemptive swap-away keyed on
    /// the stale anchor would have been `swap_necessary` (the account really was climbing toward the
    /// ceiling — `session_at_recovery` held at/above the anchor) or `wasted` (it had already reset /
    /// wasn't climbing — `session_at_recovery` dropped well below). The RAW recovery pct is recorded,
    /// NOT a baked classification: the necessary/wasted THRESHOLD is #451/#484's to derive against
    /// production, so this SLI supplies the ingredient and leaves the verdict a query-time view.
    BlindWindow {
        account: String,
        duration_secs: u64,
        session_pct: u8,
        session_at_recovery: u8,
        near_limit: bool,
        /// The retained #539 velocity signal in force through this blind window (issue #634), or
        /// `None` when no SUSTAINED EMA was retained — in which case the report-only blind
        /// velocity-projection arm could not have armed here at all.
        ///
        /// Rendered as additive trailing tokens, so a window with no retained velocity leaves the
        /// line byte-for-byte unchanged. See [`BlindVelocity`].
        velocity: Option<BlindVelocity>,
        /// The frozen per-window session HIGH-WATER MARK as a percent (issue #670), present ONLY
        /// when it EXCEEDS the raw `session_pct` anchor (compared on the un-rounded fractions, so
        /// after `u8` rounding the rendered token can TIE `session_pct`, never sit below it) — i.e.
        /// the anchor's own pre-blind reading came back stale-low and
        /// [`crate::swap::plausible_anchor_session`] would raise it. This is the value the live arms
        /// decide on (`gate_session`), the one-sided FLOOR the RAW `session_pct` above is raised to
        /// — that field stays a verbatim measurement (the #614 / #619 / #632 read-time-only
        /// contract).
        ///
        /// Since issue #632 the live #584 velocity-projection status arm projects off that corrected
        /// base, not the raw anchor. Carrying the mark here lets the two OFFLINE reconstructions —
        /// the [`BlindVelocity`] recompute recipe and the `blind_projection_error` SLI
        /// ([`crate::reliability`]) — apply the SAME `plausible_anchor_session` correction
        /// (`corrected = session_pct.max(session_high_water_pct)`) and reproduce the corrected arm,
        /// instead of projecting off the stale-low base and UNDER-computing relative to the live arm.
        ///
        /// `None` (token omitted, line byte-for-byte unchanged) when there is no retained mark or the
        /// anchor was already plausible — precisely the no-op case of `plausible_anchor_session`, so
        /// an ABSENT token means "no stale-low correction applies", never "unknown". Orthogonal to
        /// `velocity`: a stale-low anchor is a fact about the window whether or not a sustained EMA
        /// was retained. A bare number, never a token or email (issue #15).
        session_high_water_pct: Option<u8>,
    },
    /// An account ENTERED a blind window (issue #583, umbrella #363 Path B): the poll that just
    /// cleared its [`crate::daemon`] `last_reading` slot took it from a live reading to none — a
    /// `429` / `5xx` / network failure. The OPENING half of the uncensored blind-episode pair
    /// ([`Event::BlindExit`] closes it), and the answer to `blind_window`'s FIRST censoring tail:
    /// `blind_window` fires only on the `None -> live` RECOVERY edge, so an account that goes dark
    /// and STAYS dark emits nothing at all — the episode is invisible precisely when it is worst.
    /// This fires on the ENTRY edge instead, so the episode is durable the moment it starts,
    /// whether or not it ever recovers.
    ///
    /// Per-ACCOUNT, deliberately NOT scoped to the active account: `blind_window`'s `active ==
    /// Some(i)` guard is its SECOND censoring tail (once the daemon swaps off a blind account the
    /// episode stops being recorded), and this pair is anchored per-account so a swap-away can
    /// neither suppress the entry nor drop the anchor the exit is measured from. `was_active` tags
    /// which case this was — it is CONTEXT for the episode, never a filter on it.
    ///
    /// `session_pct` / `weekly_pct` are the pre-blind anchor's rounded usage in BOTH windows — the
    /// baseline [`Event::BlindExit`] differences against to answer "did it burn behind the
    /// blindness?". `near_limit` tags whether the anchor sat at/over the session trigger (the risk
    /// band), matching [`Event::BlindWindow`]'s tag so the two families filter alike. `account` is
    /// the account UUID (#15) — matching the usage-family `acct=` (never the free-form label);
    /// every other field a bare number / bool, never a token or email.
    BlindEnter {
        account: String,
        session_pct: u8,
        weekly_pct: u8,
        was_active: bool,
        near_limit: bool,
    },
    /// An account LEFT a blind window (issue #583, umbrella #363 Path B): a poll read it live again
    /// after [`Event::BlindEnter`] opened the episode. The CLOSING half of the uncensored pair, and
    /// the SLI that answers "did it burn behind the blindness?" from the log alone.
    ///
    /// DISTINCT from [`Event::BlindWindow`], which is retained UNCHANGED as the recovery-edge
    /// duration histogram for SLO reporting (it is not wrong for that purpose — it was assigned the
    /// wrong purpose, detection). Two differences make this the uncensored instrument:
    ///
    /// - **Not active-scoped.** `blind_window` requires `active == Some(i)` AND is built from the
    ///   ACTIVE-only anchor (`last_good`, #450) that every swap-away site DROPS, so an episode the
    ///   daemon swaps off is unrecoverable even when the account later reads live as a peer. This
    ///   is measured from a per-account anchor no swap touches, so it fires for any account.
    ///   `swapped_away` tags the case `blind_window` structurally could not see.
    /// - **Both usage windows, not just session.** `blind_window` carries session only. The session
    ///   window RESETS on its own 5 h cadence, so a session-only record reads a reset as
    ///   `29 -> 0` — indistinguishable from "never burned", and the exact reading that hid a real
    ///   +12 pp WEEKLY burn behind a 2 h blindness in production on 2026-07-17. Carrying the weekly
    ///   anchor + its recovery value is what makes the burn question answerable at all.
    ///
    /// `duration_secs` is how long the account was blind (anchor → this recovery). `session_pct` /
    /// `weekly_pct` are the pre-blind anchor; `session_at_recovery` / `weekly_at_recovery` the
    /// FRESH readings at this poll. [`to_log_line`](Event::to_log_line) DERIVES the signed burn in
    /// each window from those pairs and renders it alongside the raw ingredients — the same
    /// store-the-ingredients-derive-the-view idiom as [`Event::UsageVelocity`]'s `%/min` rate — so
    /// the line answers the burn question at a glance without a query-time join. RAW pcts are
    /// stored, NOT a baked burned/not-burned classification: that THRESHOLD is #451/#484's to
    /// derive against production, exactly as with `blind_window`'s `session_at_recovery`.
    ///
    /// `was_active` is whether the account was active when it went blind, `swapped_away` whether it
    /// was active at entry but is no longer active now (the #582 interaction — swapping away on
    /// blindness is precisely what made an episode invisible). `near_limit` carries the anchor's
    /// risk-band tag through from the entry. `account` is the account UUID (#15) — matching the
    /// usage-family `acct=`; every other field a bare number / bool, never a token or email.
    BlindExit {
        account: String,
        duration_secs: u64,
        session_pct: u8,
        session_at_recovery: u8,
        weekly_pct: u8,
        weekly_at_recovery: u8,
        was_active: bool,
        swapped_away: bool,
        near_limit: bool,
    },
    /// The #452 bounded-blindness preemptive-swap GATE became ELIGIBLE for the ACTIVE account
    /// (issue #482, umbrella #363 Path B) — the no-viable-target-at-gate-fire SLI, the FALSIFIER for
    /// the ADR-0017 cost-asymmetry premise ("firing early on a stale anchor is cheap because a
    /// viable target reliably exists to catch the swap"). Emitted at the moment the gate's first two
    /// conditions hold — the active account has been blind past the interim `T`
    /// ([`crate::daemon`]'s `BLIND_GATE_SECS`) AND its retained pre-blind anchor (`last_good`, #450)
    /// sat at/over the interim `risk_band` ([`crate::daemon`]'s `BLIND_GATE_RISK_BAND`, LOWER than
    /// the reactive session trigger) — recording whether the gate's THIRD condition, a viable swap
    /// target (a peer under `target_max_session_usage`, ADR-0013), is present. `viable_target=false`
    /// is the premise's counter-evidence: if it is non-trivial, #452's predicate must be revisited.
    ///
    /// Instruments the gate premise BEFORE #452 is built (ADR-0017 implementation pending), keyed on
    /// the interim constants the ADR names, so #451/#484 can finalize `T` / `risk_band` against
    /// production rather than a replay. DISTINCT from #449's `blind_window` (which closes on
    /// RECOVERY) and #455's swap-out overshoot SLO (which measures the swap): this measures the GATE
    /// and its premise, whether or not any swap follows. Edge-triggered: emitted exactly ONCE per
    /// blind episode (the gate would swap once, ending the episode), on the tick the gate first turns
    /// eligible — never per held blind tick. `blind_secs` is the blind_elapsed at that moment and
    /// `session_pct` the anchor's session usage (context for the reading). `account` is the account
    /// UUID (#15) — matching the usage-family `acct=`; every other field a bare number / bool, never
    /// a token or email.
    BlindGateEligible {
        account: String,
        viable_target: bool,
        blind_secs: u64,
        session_pct: u8,
    },
    /// The #582 server-`Retry-After` swap-away path was ARMED on the active account but HELD
    /// because firing would have spent the LAST viable target (issue #582). The swap-away is
    /// speculative — it acts on a server directive, not on an observed near-limit reading — so it
    /// must yield the final target to a CONFIRMED-exhaustion swap (the reactive `session_ceiling`
    /// path, which fires on evidence) rather than consume it on a guess.
    ///
    /// The AC's "reported, not hidden" surface: without this line the daemon would sit silently
    /// blind on a server-throttled account, exactly the invisibility the issue is about — the hold
    /// is a DELIBERATE choice, and an operator watching an account go dark deserves to see it made.
    /// Distinct from [`Self::AllExhausted`], which reports having no viable target at all; here a
    /// viable target EXISTS and is deliberately being preserved.
    ///
    /// Edge-triggered: emitted once per blind episode (on the tick the hold is first taken), never
    /// per held tick — a 3600 s window spans hundreds of ticks. `retry_after_secs` is the RAW
    /// (pre-cap) server directive that armed the path; `blind_secs` how long the active had been
    /// blind at that moment. `account` is the account UUID (#15), matching the usage-family
    /// `acct=`; every other field a bare number, never a token or email.
    BlindPreemptReserveHold {
        account: String,
        retry_after_secs: u64,
        blind_secs: u64,
    },
    /// ALARM (issue #582): the server `Retry-After` throttle is WALKING the roster — it follows
    /// the ACTIVE ROLE rather than any one account, so repeated preemptive swaps inside the
    /// daemon's walk window have each merely handed the `429` to the next account. Rotation STOPS
    /// here: holding a blind-but-low account beats walking a 3600 s throttle onto the last good
    /// one. (The count and window live as `RETRY_AFTER_WALK_MAX` / `RETRY_AFTER_WALK_WINDOW` in
    /// the daemon module, which owns the decision; this event only reports what they resolved to.)
    ///
    /// The loudest line this path emits, and the one that wants an operator: unlike the reserve
    /// hold (a routine, self-correcting yield), a detected walk means the daemon has exhausted its
    /// automatic remedy and is now deliberately sitting on a throttled account. `swaps` is how
    /// many server-throttled preemptive swaps were counted inside `window_secs`;
    /// `retry_after_secs` the RAW server directive on the account being held. Edge-triggered like
    /// [`Self::BlindPreemptReserveHold`]. `account` is the account UUID (#15); every other field a
    /// bare number, never a token or email.
    RetryAfterWalk {
        account: String,
        swaps: usize,
        window_secs: u64,
        retry_after_secs: u64,
    },
    /// An account's REFRESH-token deadline ENTERED the operator's foresight horizon (issue #880):
    /// its `refreshTokenExpiresAt` (issue #878) now falls inside `[credential].expiry_horizon_secs`
    /// — [`ExpiryHorizon::Within`] — or has already passed it, [`ExpiryHorizon::Lapsed`]. The
    /// operator must `claude /login` before it lapses; no refresh and no `poke` can recover a lapsed
    /// refresh token.
    ///
    /// DURABLE, not a diagnostic, and the distinction is the whole point (issue #800 / #827): the
    /// stderr-bound [`DiagnosticLog`] is OPT-IN and DEFAULT OFF (`[tunables] verbose`, and
    /// `sessiometer log` defaults to `--channel event`), so on a default install a diagnostic would
    /// be absent entirely — while this is exactly the state change an operator must be able to
    /// reconstruct OFFLINE, weeks later, from the always-on governed log.
    ///
    /// EDGE-TRIGGERED on the BAND, not per tick: emitted when the classification first becomes
    /// `within`, and again if it later becomes `lapsed` (an escalation the operator needs, since the
    /// remedy window has closed). A held band re-emits nothing — a seven-day window spans thousands
    /// of polls. A daemon that first meets an ALREADY-lapsed account emits `state=lapsed` directly:
    /// the `within` edge is not a precondition, so a restart or a long absence can never swallow the
    /// more severe fact.
    ///
    /// Three caveats, stated rather than implied:
    /// - The latch is IN-MEMORY, so a restart mid-band RE-EMITS the entry. Band entries therefore
    ///   over-count episodes, exactly as issue #827's ENTER guards do; deduplicate on `acct=` when
    ///   counting.
    /// - A poll that cannot READ a deadline emits nothing here and leaves the latch standing — but by
    ///   a shorter route than a reader might assume: the fold returns at its no-deadline guard and
    ///   this edge is never reached, so there is no [`ExpiryHorizon::Unknown`] classification to
    ///   retain the latch against. The behaviour is the intended one (an unreadable credential is *we
    ///   can no longer see*, never *recovered* — the issue #137 invariant; resetting would let a flaky
    ///   keychain read manufacture a duplicate entry on every flap), and the `Unknown` arm exists for
    ///   a future caller that classifies without a parsed deadline in hand.
    /// - There is NO paired CLEARED partner, deliberately: leaving the band requires the FIXED
    ///   deadline to move, and every move emits [`Self::CredentialExpiryObserved`] — which carries
    ///   strictly MORE than a bare marker would (both deadlines and the provenance). A `cleared` line
    ///   would be a second, poorer line about the same tick. Same reasoning as
    ///   [`Self::NearLimitPollCoverage`]'s silent band exit, which is unpaired for the same cause.
    ///
    /// `expires_at` is the observed deadline in epoch SECONDS and is NON-optional by type: only a
    /// PARSED deadline can classify `within` / `lapsed` ([`crate::daemon::account_expiry`]), so
    /// "entered the horizon with no deadline" is unrepresentable rather than merely unreachable.
    /// `horizon_secs` is the lookahead it was classified against, carried so the line EXPLAINS its
    /// own verdict (a six-day deadline is `within` only because the horizon is seven) and stays
    /// readable after an operator retunes the knob — the same store-the-decision's-ingredients idiom
    /// as [`SwapProjection`]. `account` is the account UUID (#15) — matching the usage-family
    /// `acct=`, never the free-form label; every other field an enum or a bare number.
    CredentialExpiryHorizon {
        account: String,
        state: ExpiryHorizon,
        expires_at: i64,
        horizon_secs: u64,
    },
    /// One PROVENANCE-TAGGED observation of an account's REFRESH-token deadline (issue #880) — the
    /// record that makes the issue #877 spike answerable from production data alone.
    ///
    /// Named *observed* rather than *changed* on purpose. The third row of #877's table is
    /// "UNCHANGED across an external credential write" — see [`ExpiryProvenance::CanonicalRestash`]
    /// for what turns on it — so the record MUST be able to say *nothing moved*, which a
    /// change-only record structurally cannot. [`ExpiryProvenance::CanonicalRestash`] is the
    /// provenance that carries that case; every other provenance fires only on an actual change.
    ///
    /// Emitted on exactly three conditions, none of them per-tick:
    /// - the FIRST deadline observed for the account this run ([`ExpiryProvenance::FirstObservation`]
    ///   — an absolute anchor, see that variant for the restart gap it closes),
    /// - a CHANGE against the last observed deadline ([`ExpiryProvenance::MyRefresh`] /
    ///   [`ExpiryProvenance::ReadSourceSwitched`] / [`ExpiryProvenance::ExternalChange`]),
    /// - the daemon healing an out-of-band canonical write ([`ExpiryProvenance::CanonicalRestash`]),
    ///   change or no change.
    ///
    /// A poll that could NOT observe a deadline — a locked keychain, an absent stash — emits
    /// NOTHING and leaves the baseline standing. That is not a gap: *we could not look* and *we
    /// looked and the field is gone* are different facts, and only the first is what an unreadable
    /// credential proves. Reporting it as a `Some -> None` change would let a flaky read fabricate
    /// "upstream dropped the field", the same discipline
    /// [`crate::daemon`]'s `signaled_canonical_scrubbed` applies to a flaky canonical read. (The
    /// DISPLAY axis still reports [`ExpiryHorizon::Unknown`] for that poll — issue #878 owns that
    /// decision and it is untouched here; the two answer different questions.)
    ///
    /// `before` / `after` are epoch SECONDS, rendered RFC 3339 through the same formatter as the line
    /// `ts`. Their types are deliberately ASYMMETRIC. `after` is a bare `i64` because there is nothing
    /// to observe if no deadline was read — the fold returns before emitting — so *an observation with
    /// no deadline* is unrepresentable here rather than merely unreachable, the same reasoning that
    /// makes [`Self::CredentialExpiryHorizon`]'s `expires_at` an `i64`. `before` is genuinely optional:
    /// the anchor line has no baseline to have changed from, and it is OMITTED from the line rather
    /// than rendered empty (an empty value would split the `key=val` grammar — the same handling as
    /// [`Self::Refresh`]'s optional `expires_before` / `expires_after`).
    ///
    /// `delta_secs` is DERIVED at render time, and only when `before` is known — so *unchanged* reads
    /// as `delta_secs=0` at a glance instead of requiring the reader to subtract two timestamps by
    /// hand, and the anchor claims no delta against a baseline it does not have. Mirrors
    /// `event=refresh`'s derived `window_secs`. `account` is the account UUID (#15) — matching the
    /// usage-family `acct=`, never the free-form label. No token material: only the non-secret
    /// deadlines and a fixed provenance token, exactly as
    /// [`crate::refresh::refresh_token_expires_at`] reads only the deadline and never the token it
    /// belongs to.
    ///
    /// `grant_replaced` is the GRANT-IDENTITY discriminator, and it is what makes row 3 of the #877
    /// table answerable rather than merely representable. [`ExpiryProvenance::CanonicalRestash`]
    /// proves *somebody else wrote the credential*, but a `claude /login` and an in-place token
    /// refresh by another Claude Code instance are the SAME observation to
    /// [`crate::daemon::Daemon::reconcile_canonical_change`] — and row 3 needs a re-login
    /// specifically. `Some(true)` says the stored refresh token is different bytes than the one the
    /// daemon last had — consistent with a NEW OAuth grant having been issued; `Some(false)` says the
    /// grant is byte-identical, which RULES A RE-LOGIN OUT. `None` means this path could not tell:
    /// every poll path (the poll fold reads the deadline from the account's clocks and never sees two
    /// credential blobs to compare), a first change with no baseline yet, and — deliberately — a
    /// CLEARED refresh token. `Some(empty)` is [`crate::refresh::refresh_token`]'s documented DEAD
    /// signal, not a grant, so an emptied token must not render `true`; it is filtered to the unknown
    /// at the computation site rather than being allowed to masquerade as a fresh login.
    ///
    /// NOT the same field as `rotated=` on `event=refresh` / `poll_refresh` / `keep_warm`, though the
    /// two read alike. That one comes from the refresh engine's own report — *the refresh I just ran
    /// rotated the token* — and is therefore about an action the daemon took. This one is a byte diff
    /// against the canonical-watch baseline, about a write the daemon did NOT make. Different
    /// derivations, different provenance, and only this one can appear on a line the daemon did not
    /// cause; do not fold them together when reading the log.
    ///
    /// Named for the GRANT, not the token, and the name is load-bearing twice over. A grant is the
    /// OAuth thing that was or was not re-issued; the refresh token is merely the artifact
    /// representing it, and this field says nothing about that artifact's value. It also keeps the key
    /// free of the substring `token`, which matters mechanically: `no_event_line_carries_an_email_or_
    /// token_sigil` rejects any rendered line containing it, and that check earns its bluntness — do
    /// not respell this key into tripping it, and do not relax the check to accommodate a respelling.
    ///
    /// Read it as a NECESSARY, not sufficient, condition: a new grant is consistent with a re-login,
    /// it does not prove one, because whether Claude Code rotates the refresh token on an ordinary
    /// refresh is exactly the kind of upstream behaviour issue #876 had to establish empirically for
    /// the deadline. The analytical value is the NARROWING plus the deadline delta on the same line:
    /// `grant_replaced=true delta_secs=0` is row 3's signature (a fresh grant that did NOT move the
    /// deadline), and `Some(false)` excludes a population that would otherwise pollute it. It is
    /// deliberately ON this record and not left to a join — the sibling `event=restash` shares no
    /// key with this line, and `event=login` fires only for the MANAGED `sessiometer login`, so
    /// neither can supply this fact after the fact.
    ///
    /// A BOOLEAN, computed from a byte comparison and thrown away: the sanctioned use of
    /// [`crate::refresh::refresh_token`], whose contract is that its value "is only ever
    /// emptiness-checked or byte-compared, never logged". No token material reaches the line.
    CredentialExpiryObserved {
        account: String,
        provenance: ExpiryProvenance,
        before: Option<i64>,
        after: i64,
        grant_replaced: Option<bool>,
    },
    /// The ACTIVE account's last COMPLETED observation aged past the re-observation bound
    /// (issue #1453) — the OPENING half of the observation-gap pair ([`Event::ObservationGapExit`]
    /// closes it), and the one blindness signal in this file that does not need a poll to have
    /// happened.
    ///
    /// Every other detector in the family keys on a poll that RAN AND FAILED:
    /// [`Event::BlindEnter`]'s entry edge needs an `Err`, and [`Event::UsageBackoff`] needs a `429`
    /// or a `5xx`/transient — either way, a poll.
    /// A poll that was never ATTEMPTED produces neither, so an active account the poll schedule
    /// stops selecting goes dark with nothing recording it. This event keys on ELAPSED TIME instead,
    /// so the never-attempted case — and the narrower scheduled-but-skipped one (a back-off /
    /// slow-poll filter dropping the scheduled index) — are both visible.
    ///
    /// Edge-triggered, mirroring the [`Event::BlindEnter`] / [`Event::BlindExit`] episode shape: one
    /// line on the crossing, one on re-observation, never one per tick for the duration of the gap.
    ///
    /// `elapsed_secs` is the gap at the crossing, measured from the LATER of the account's last
    /// completed reading and the swap that made it active — so on a freshly-promoted account it is
    /// time-since-promotion, not time-since-a-stale-peer-reading, and the two regimes report the
    /// same quantity. `threshold_secs` is the bound it crossed — the daemon's `2 · poll_secs / N`
    /// re-observation interval, carried because it is derived from config the event log does not
    /// otherwise contain, so a reader can tell a breach from a re-tuned bound without a second
    /// source. (`ADR-0012` decision 4 states that interval for a STATIC active; it was never derived
    /// for the mid-cycle change of active this event's primary population is made of, which that ADR
    /// now records itself — § Status → Amended 2026-09-02.) `was_active` names the population: the emitter opens
    /// an episode only on the ACTIVE account, so it reads `true` on every line the daemon writes
    /// today — it states which population the line belongs to rather than leaving a reader to infer
    /// it from the emitter's scoping, exactly as [`Event::BlindEnter`] carries the same-named field.
    /// `account` is the account UUID (#15) — matching the usage-family `acct=` (never the free-form
    /// label); every other field a bare number / bool, never a token or email.
    ObservationGapEnter {
        account: String,
        elapsed_secs: u64,
        threshold_secs: u64,
        was_active: bool,
    },
    /// The account inside an observation gap was OBSERVED again (issue #1453): a poll completed and
    /// refreshed its reading, closing the episode [`Event::ObservationGapEnter`] opened. The CLOSING
    /// half of the pair, and the line the post-swap first-sight latency SLI is computed from.
    ///
    /// `elapsed_secs` is the WHOLE gap — anchor to this observation — not the increment since the
    /// entry line, so the latency reads straight off one line without differencing two. The anchor
    /// is the later of the account's last completed reading and the change of active designation
    /// (see [`Event::ObservationGapEnter`]), which is what makes the number ON THIS LINE the SLI's
    /// own quantity rather than a figure a reader would first have to net a preceding `event=swap`
    /// out of. It spans any failed polls in between: a poll that errored is not an observation, so it neither closes the
    /// episode nor stops the clock (that population is [`Event::BlindEnter`]'s, and the two records
    /// are deliberately allowed to overlap rather than either suppressing the other).
    ///
    /// What the emitted set IS, because a percentile over it is easy to misread: the pair is
    /// edge-triggered PAST the bound, so the exits form the tail
    /// `{ latency > 2 · poll_secs / N }`, never the whole first-sight distribution. A p50 over these
    /// lines is therefore the median of BREACHES, and the healthier the fleet the smaller and worse
    /// that population reads — a within-bound first sight emits nothing at all. The `FAIL`-side
    /// criterion (any single occurrence beyond the bound) and the breach tail are what this line
    /// supports honestly; a p50 of first-sight latency needs a source that also records the
    /// non-breaches, which no event here is.
    ///
    /// `was_active` carries the entry's population tag through, and `swapped_away` is the tail that
    /// makes it usable: an episode that opened on the active account and closed after the daemon
    /// had already swapped away is NOT a first-sight-of-the-new-active sample, so a reader that
    /// folded it would overstate the latency. The same distinction, for the same reason, as
    /// [`Event::BlindExit`]'s field of that name. `account` is the account UUID (#15) — matching the
    /// usage-family `acct=`; every other field a bare number / bool, never a token or email.
    ObservationGapExit {
        account: String,
        elapsed_secs: u64,
        was_active: bool,
        swapped_away: bool,
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
    ///
    /// The per-variant arms below format freely; the record-integrity guarantee
    /// (issue #1092) is applied ONCE, here, to whatever they produce. That is the
    /// point of the shape: the arms are a match EXPRESSION with a single exit, so a
    /// variant added later cannot render a line that skips [`single_line`] — the
    /// guarantee holds by construction rather than by every future author
    /// remembering a per-field helper.
    pub(crate) fn to_log_line(&self, ts: SystemTime) -> String {
        let ts = rfc3339(ts);
        let line = match self {
            Event::Swap {
                from,
                to,
                reason,
                session_pct,
                projection,
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
                // The #539 projection ingredients (issue #634), so a `reason=velocity_preempt` line
                // explains its own decision — `projected >= ceiling` is checkable from the line
                // alone — instead of reading like a bug at a below-trigger `session_pct`. Trailing
                // and conditional, exactly like `late=` above: absent for every non-projective
                // swap, so those lines are byte-for-byte unchanged, and the tolerant `key=val`
                // field-map parsers (`usage_stats::parse_swap_events`, `reliability::parse_events`)
                // are unaffected. `rate` carries 6 decimals — full precision, because a rounded
                // rate cannot reproduce a decision (see `SwapProjection`); the percents carry 2.
                // Numbers only, never a token / email (#15).
                let projection = match projection {
                    Some(p) => format!(
                        " projected={:.2} rate={:.6} horizon={} ceiling={:.2}",
                        p.projected_pct, p.rate_pct_per_sec, p.horizon_secs, p.ceiling_pct
                    ),
                    None => String::new(),
                };
                format!(
                    "ts={ts} event=swap from={from} to={to} reason={reason} session_pct={session_pct}{late}{projection}"
                )
            }
            Event::ReStash { account } => {
                format!("ts={ts} event=restash account={account}")
            }
            Event::AllExhausted {
                hold,
                cause,
                resets_at,
            } => {
                // `cause` (#398) is a required field after `hold`; `resets_at` trails
                // optionally (an empty value would split the key=val grammar), mirroring
                // the swap line's optional `late=`.
                let cause = cause.as_str();
                let resets = match resets_at {
                    Some(secs) => format!(" resets_at={}", rfc3339(system_time_from_epoch(*secs))),
                    None => String::new(),
                };
                format!("ts={ts} event=all_exhausted hold={hold} cause={cause}{resets}")
            }
            Event::AllExhaustedCleared => {
                format!("ts={ts} event=all_exhausted_cleared")
            }
            Event::ActiveDeadNoTarget {
                hold,
                cause,
                resets_at,
            } => {
                // Same `key=val` grammar as `all_exhausted` (its strictly-worse sibling, #405):
                // `cause` required after `hold`, `resets_at` trailing + optional (an empty value
                // would split the grammar).
                let cause = cause.as_str();
                let resets = match resets_at {
                    Some(secs) => format!(" resets_at={}", rfc3339(system_time_from_epoch(*secs))),
                    None => String::new(),
                };
                format!("ts={ts} event=active_dead_no_target hold={hold} cause={cause}{resets}")
            }
            Event::ActiveDeadNoTargetCleared => {
                format!("ts={ts} event=active_dead_no_target_cleared")
            }
            Event::FleetRunwayLow {
                runway_secs,
                threshold_secs,
                counted,
                observed,
            } => {
                // All four fields required, plain integers (#15): the crossing reading, the
                // configured line it crossed, and the aggregate's `n of m` honesty cardinality.
                format!(
                    "ts={ts} event=fleet_runway_low runway_secs={runway_secs} \
                     threshold_secs={threshold_secs} counted={counted} observed={observed}"
                )
            }
            Event::FleetRunwayRecovered => {
                format!("ts={ts} event=fleet_runway_recovered")
            }
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
            Event::CanonicalScrubbed { account } => {
                // `account` trails optionally (an empty value would split the key=val grammar) —
                // absent when no active account was resolved at scrub time. Mirrors
                // `all_exhausted`'s optional `resets_at`.
                let account = match account {
                    Some(label) => format!(" account={label}"),
                    None => String::new(),
                };
                // `mode=scrub` (issue #475): the durable half of the two-mode `mode=(yank|scrub)`
                // classification the umbrella needs — this UNRECOVERABLE empty-canonical scrub (needs
                // `/login`) vs the RECOVERABLE rotation-yank carried as `mode=yank` on the
                // `diag=canonical` line. A constant token (not a stored field — the event's identity
                // already fixes the mode), so an operator `grep mode=` surfaces BOTH modes in one
                // vocabulary. Renders before the optional `account` to keep a stable line prefix.
                format!("ts={ts} event=canonical_scrubbed mode=scrub{account}")
            }
            Event::CanonicalRestored { account } => {
                let account = match account {
                    Some(label) => format!(" account={label}"),
                    None => String::new(),
                };
                format!("ts={ts} event=canonical_restored{account}")
            }
            Event::CanonicalRecovered { account } => {
                format!("ts={ts} event=canonical_recovered account={account}")
            }
            Event::CanonicalRecoveryExhausted { account } => {
                // `account` trails optionally (an empty value would split the key=val grammar) —
                // absent when no active account was resolved at back-off time. Mirrors
                // `canonical_scrubbed`'s optional `account`.
                let account = match account {
                    Some(label) => format!(" account={label}"),
                    None => String::new(),
                };
                format!("ts={ts} event=canonical_recovery_exhausted{account}")
            }
            Event::CanaryDrift {
                displayed,
                matched,
                overridden,
            } => {
                // `overridden` trails conditionally (the `late=true` idiom): a refused
                // drift line stays minimal, and the token appears exactly when the
                // `canary_drift_override` tunable let the write proceed anyway.
                let overridden = if *overridden { " overridden=true" } else { "" };
                format!(
                    "ts={ts} event=canary_drift displayed={displayed} matched={matched}{overridden}"
                )
            }
            Event::CanaryUnparseableCanonical { overridden } => {
                // `overridden` trails conditionally (the same idiom as `canary_drift`),
                // appearing exactly when `canary_nostashmatch_override` let the write proceed.
                let overridden = if *overridden { " overridden=true" } else { "" };
                format!("ts={ts} event=canary_unparseable_canonical{overridden}")
            }
            Event::CanaryOnlineProbe { verdict, refused } => {
                // `refused` trails conditionally (the same idiom as the `overridden` flag on
                // the two canary siblings above): the graceful-degrade default keeps the line
                // minimal, and the token appears exactly when strict mode cost the swap.
                let refused = if *refused { " refused=true" } else { "" };
                format!("ts={ts} event=canary_online_probe verdict={verdict}{refused}")
            }
            Event::CanaryAmbiguous { count } => {
                format!("ts={ts} event=canary_ambiguous count={count}")
            }
            Event::CanaryCleared => {
                format!("ts={ts} event=canary_cleared")
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
                reason,
                backoff_secs,
            } => {
                let rotated = rotated_field(*outcome);
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
                // `rotated=` trails the optional expiry fields (issue #279) — the AC-3 rotation
                // signal made durable. It sits AFTER `outcome=`, so `last_refresh_outcomes`'
                // ` outcome=`-then-first-token parse is unaffected. Present on the two REFRESHED
                // outcomes and ABSENT on `no_change` / `dead` / `error` (issue #1004), because
                // only an actual exchange can have rotated anything; it is sourced from the
                // outcome's own payload, so the three arms that cannot carry it cannot render it.
                //
                // `reason=` (issue #377) trails the WHOLE line — after the optional
                // `rotated=` — mirroring the swap line's optional trailing `late=`. Present
                // ONLY on an error whose sub-cause is classifiable secret-free; omitted
                // otherwise (a non-error outcome, or a hard `Err`), so a normal refresh line is
                // byte-for-byte unchanged and every existing `key=val` parser is unaffected.
                let reason = match reason {
                    Some(reason) => format!(" reason={}", reason.as_str()),
                    None => String::new(),
                };
                // `backoff_secs=` (issue #408) trails the WHOLE line — after the optional
                // `reason=` — mirroring the swap line's optional trailing `late=` and the tick
                // line's `backoff_secs=`. Present ONLY on an error that armed a per-account
                // back-off; omitted otherwise, so a non-throttled refresh line is byte-for-byte
                // unchanged and every existing `key=val` parser is unaffected.
                let backoff = match backoff_secs {
                    Some(secs) => format!(" backoff_secs={secs}"),
                    None => String::new(),
                };
                // `window_secs=` (issue #409): the stored expiry's forward SLIDE this cycle,
                // `expires_after − expires_before` in whole seconds — the sliding-window-vs-cap
                // signal (the engine's [`crate::refresh::RefreshReport`] `expires_at_delta_secs`)
                // made durable on the line, so an operator reads the granted-lifetime extension
                // WITHOUT deriving it from the two timestamps by hand. Rendered only when BOTH
                // expiries are readable (else the slide is undefined); an additive TRAILING field
                // (like `reason=`/`backoff_secs=`) so a partial line is byte-for-byte unchanged and
                // every existing `key=val` parser is unaffected. A bare integer derived from two
                // non-secret timestamps already on the line — the #15 single-surface guarantee holds.
                let window = match (expires_before, expires_after) {
                    (Some(before_ms), Some(after_ms)) => {
                        format!(" window_secs={}", (after_ms - before_ms) / 1000)
                    }
                    _ => String::new(),
                };
                format!(
                    "ts={ts} event=refresh account={account} outcome={outcome}{before}{after}{rotated}{reason}{backoff}{window}"
                )
            }
            Event::PollRefresh {
                account,
                trigger,
                outcome,
            } => {
                // The isolated poll-refresh ACTION (issue #255). `trigger=` carries the
                // reactive-401-vs-recovery discriminant as a CARRIED field (issue #1367) — it was a
                // hard-coded `poll_401` literal until then, on a single-condition premise issue
                // #643 had already falsified by routing its parked re-probe through this event.
                // `outcome` reuses the SAME non-secret token vocabulary `event=refresh` renders. The
                // DISTINCT `poll_refresh` event name keeps it clear of the periodic #106
                // `event=refresh` line that the `list` view's [`last_refresh_outcomes`] reader
                // parses.
                // `rotated=` trails `outcome=` (issue #279) — the same non-secret rotation
                // signal `event=refresh` carries, on the poll path, and omitted on the same
                // three non-refreshed outcomes for the same reason (issue #1004).
                let trigger = trigger.as_str();
                let rotated = rotated_field(*outcome);
                let outcome = outcome.as_str();
                format!(
                    "ts={ts} event=poll_refresh account={account} trigger={trigger} outcome={outcome}{rotated}"
                )
            }
            Event::KeepWarm {
                account,
                trigger,
                outcome,
            } => {
                // The in-place keep-warm ACTION (issue #282). `trigger=` carries the
                // proactive / reactive / recovery discriminant (three-valued since issue #643);
                // `outcome` reuses the SAME non-secret token vocabulary `event=refresh` renders.
                // The DISTINCT `keep_warm` event name keeps it
                // clear of both the periodic #106 `event=refresh` line and the `poll_refresh`
                // line — three separate refresh mechanisms, three separate event names. The
                // `poll_refresh` sibling carries its own `trigger=` too (issue #1367), so the
                // token vocabularies are read per event name, never pooled.
                let trigger = trigger.as_str();
                // Omitted on the three non-refreshed outcomes, as on the sibling lines (#1004).
                let rotated = rotated_field(*outcome);
                let outcome = outcome.as_str();
                format!(
                    "ts={ts} event=keep_warm account={account} trigger={trigger} outcome={outcome}{rotated}"
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
            Event::RefreshPreflightUnresolved => {
                // The preflight-established OPENING edge of a #378 episode (issue #787). No fields
                // at all: one daemon-global observation with no count to carry and, by
                // construction, no path — so it is #15-clean with nothing to redact.
                format!("ts={ts} event=refresh_preflight_unresolved")
            }
            Event::RefreshBinaryResolved { path } => {
                // Percent-encoded (see `path_value`) so the value can never split into a second
                // token, and kept LAST anyway — a reader's eye lands on it, and any future field
                // is appended after a value that cannot be mistaken for one. A filesystem
                // location, never a credential (#15), and deliberately NOT folded into the
                // `reason=unresolved` refresh line — the variant doc says why.
                format!(
                    "ts={ts} event=refresh_binary_resolved path={}",
                    path_value(path)
                )
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
            Event::UsageBackoff {
                account,
                class,
                consecutive,
                retry_after_secs,
                backoff_secs,
            } => {
                // `acct=` carries the account UUID (never the free-form `label`, #15) — the same key
                // the usage-family `usage_gap` and the uuid-carrying `uncaptured_login` use.
                // `backoff_secs` (the armed window) is always present; `retry_after_secs` trails
                // OPTIONALLY — an empty value after `=` would split the key=val grammar (mirrors
                // `all_exhausted`'s optional `resets_at`) — present iff the server advised a
                // `Retry-After`, absent for a self-capped exponential. Field ORDER mirrors the
                // sibling `diag=tick` line (`backoff_secs` then `retry_after_secs`).
                let class = class.as_str();
                let retry_after = match retry_after_secs {
                    Some(secs) => format!(" retry_after_secs={secs}"),
                    None => String::new(),
                };
                format!(
                    "ts={ts} event=usage_backoff acct={account} class={class} consecutive={consecutive} backoff_secs={backoff_secs}{retry_after}"
                )
            }
            Event::UsageBackoffCleared { account } => {
                format!("ts={ts} event=usage_backoff_cleared acct={account}")
            }
            Event::ExhaustedSlowPoll {
                account,
                window_secs,
            } => {
                // `acct=` carries the account UUID (never the free-form `label`, #15), matching
                // the sibling `usage_backoff` line; `window_secs` is the armed slow-poll window
                // (a bare duration, never a token). Redacted to uuid + window ONLY (issue #537).
                format!(
                    "ts={ts} event=exhausted_slow_poll acct={account} window_secs={window_secs}"
                )
            }
            Event::ExhaustedSlowPollCleared { account } => {
                format!("ts={ts} event=exhausted_slow_poll_cleared acct={account}")
            }
            Event::NearLimitPollCoverage {
                account,
                sub_interval_secs,
            } => {
                // `acct=` carries the account UUID (never the free-form `label`, #15), matching the
                // sibling `exhausted_slow_poll` line; `sub_interval_secs` is the tightened near-limit
                // poll cadence (a bare duration, never a token). Redacted to uuid + cadence ONLY
                // (issue #540 / #15).
                format!(
                    "ts={ts} event=near_limit_poll_coverage acct={account} sub_interval_secs={sub_interval_secs}"
                )
            }
            Event::UsageVelocity {
                account,
                session_delta_pct,
                weekly_delta_pct,
                elapsed_secs,
            } => {
                // Normalized to %/min (issue #449): the raw per-reading delta was ambiguous without
                // its interval, so the velocity is rendered as a TIME rate — each delta divided by
                // the interval in minutes (`elapsed_secs / 60`), to two decimals. Derived HERE from
                // the stored delta + `elapsed_secs` (the same store-ingredients-derive-view idiom the
                // refresh line's `window_secs` uses); `elapsed_secs > 0` at emission, so this never
                // divides by zero. The raw signed deltas + interval trail the rate so the exact
                // measurement stays recoverable for the #451 spike. `acct=` is the UUID (#15),
                // matching the sibling `usage_backoff` line; a negative rate / delta renders its `-`
                // sign, a `key=val` token existing parsers read as-is.
                let minutes = *elapsed_secs as f64 / 60.0;
                let session_pct_per_min = f64::from(*session_delta_pct) / minutes;
                let weekly_pct_per_min = f64::from(*weekly_delta_pct) / minutes;
                format!(
                    "ts={ts} event=usage_velocity acct={account} session_pct_per_min={session_pct_per_min:.2} weekly_pct_per_min={weekly_pct_per_min:.2} elapsed_secs={elapsed_secs} session_delta_pct={session_delta_pct} weekly_delta_pct={weekly_delta_pct}"
                )
            }
            Event::BlindWindow {
                account,
                duration_secs,
                session_pct,
                session_at_recovery,
                near_limit,
                velocity,
                session_high_water_pct,
            } => {
                // The active account's blind-window CLOSE (issue #449) + the post-recovery
                // swap-necessity SLI (issue #482). All bare numbers + a bool (#15): how long it was
                // blind (= the anchor's age), the pre-blind anchor's session pct (how near the limit
                // it was), the FRESH session pct read at recovery, and whether that anchor sat in the
                // risk band. `session_at_recovery` vs `session_pct` reconciles a stale-anchor swap as
                // necessary-vs-wasted (the threshold left to #451/#484). `acct=` is the UUID, matching
                // the usage-family lines.
                //
                // Issue #634: the retained #539 velocity in force through the window trails as
                // additive optional tokens, so the report-only blind velocity-projection arm
                // (#584/#600) — which fires no swap and emits no event of its own — becomes
                // reconstructable offline from THIS line:
                // `projected = anchor + rate × inflation × duration_secs` (anchor = `session_pct`,
                // raised by the #670 mark token below when present), armed iff it reaches
                // `ceiling`. The derived projection is deliberately NOT stored (log the ingredients,
                // derive the views), while the two CONSTANTS are, so a drift in either cannot make an
                // old record read wrong. Absent when no sustained EMA was retained — a line the arm
                // could not have armed on — leaving it byte-for-byte unchanged. `rate` carries 6
                // decimals: it is the load-bearing contract for that recomputation, and the rounded
                // `usage_velocity` delta cannot reproduce it. Numbers only, never a token / email.
                let velocity = match velocity {
                    Some(v) => format!(
                        " rate={:.6} inflation={:.2} ceiling={:.2}",
                        v.rate_pct_per_sec, v.inflation, v.ceiling_pct
                    ),
                    None => String::new(),
                };
                // Issue #670: the frozen window high-water mark trails as ONE more additive optional
                // token, present only when the pre-blind anchor was stale-low (so
                // `swap::plausible_anchor_session` would raise it). An offline reader applies the SAME
                // correction — `corrected = session_pct.max(session_high_water_pct)` — to reproduce the
                // #632-corrected arm's base rather than projecting off the stale-low `session_pct`.
                // Absent when no correction applies, leaving the line byte-for-byte unchanged;
                // rendered as its own optional group AFTER the #634 velocity tokens, keeping the
                // trio's documented position stable. A bare number (#15).
                let high_water = match session_high_water_pct {
                    Some(pct) => format!(" session_high_water_pct={pct}"),
                    None => String::new(),
                };
                format!(
                    "ts={ts} event=blind_window acct={account} duration_secs={duration_secs} session_pct={session_pct} session_at_recovery={session_at_recovery} near_limit={near_limit}{velocity}{high_water}"
                )
            }
            Event::BlindEnter {
                account,
                session_pct,
                weekly_pct,
                was_active,
                near_limit,
            } => {
                // The blind-episode OPEN (issue #583) — emitted the moment the account goes dark, so
                // a never-recovering episode is still durable. All bare numbers + bools (#15): the
                // pre-blind anchor's usage in BOTH windows (the baseline `blind_exit` differences
                // against), whether the account was the active one, and whether the anchor sat in the
                // risk band. `acct=` is the UUID, matching the usage-family lines.
                format!(
                    "ts={ts} event=blind_enter acct={account} session_pct={session_pct} weekly_pct={weekly_pct} was_active={was_active} near_limit={near_limit}"
                )
            }
            Event::BlindExit {
                account,
                duration_secs,
                session_pct,
                session_at_recovery,
                weekly_pct,
                weekly_at_recovery,
                was_active,
                swapped_away,
                near_limit,
            } => {
                // The blind-episode CLOSE (issue #583) — the uncensored counterpart of
                // `blind_window`: fires for ANY account (not just the still-active one) and carries
                // BOTH usage windows. The signed per-window burn is DERIVED here from the stored
                // anchor/recovery pairs and rendered FIRST — the same store-the-ingredients-
                // derive-the-view idiom as `usage_velocity`'s %/min rate — so "did it burn behind the
                // blindness?" reads straight off the line; the raw pcts trail it so the exact
                // measurement stays recoverable and the necessary/wasted threshold stays a query-time
                // view (#451/#484). `weekly_burn_pct` is the dimension a session-only record misses
                // entirely when the 5 h session window resets mid-blindness. Both burns are signed —
                // NEGATIVE across a window reset — and a `-` renders as a `key=val` token existing
                // parsers read as-is. All bare numbers + bools (#15); `acct=` is the UUID.
                let session_burn_pct = i16::from(*session_at_recovery) - i16::from(*session_pct);
                let weekly_burn_pct = i16::from(*weekly_at_recovery) - i16::from(*weekly_pct);
                format!(
                    "ts={ts} event=blind_exit acct={account} duration_secs={duration_secs} session_burn_pct={session_burn_pct} weekly_burn_pct={weekly_burn_pct} session_pct={session_pct} session_at_recovery={session_at_recovery} weekly_pct={weekly_pct} weekly_at_recovery={weekly_at_recovery} was_active={was_active} swapped_away={swapped_away} near_limit={near_limit}"
                )
            }
            Event::BlindGateEligible {
                account,
                viable_target,
                blind_secs,
                session_pct,
            } => {
                // The #452 gate-eligibility SLI (issue #482). All bare numbers + a bool (#15):
                // whether a viable swap target existed when the preemptive gate turned eligible, how
                // long the active account had been blind, and the pre-blind anchor's session pct.
                // `viable_target=false` is the premise falsifier. `acct=` is the UUID, matching the
                // usage-family lines.
                format!(
                    "ts={ts} event=blind_gate_eligible acct={account} viable_target={viable_target} blind_secs={blind_secs} session_pct={session_pct}"
                )
            }
            Event::BlindPreemptReserveHold {
                account,
                retry_after_secs,
                blind_secs,
            } => {
                // The #582 reserve hold (issue #582): the swap-away was armed by a server
                // `Retry-After` but held to preserve the LAST viable target for a confirmed-
                // exhaustion swap. All bare numbers (#15); `acct=` is the UUID, matching the
                // usage-family lines.
                format!(
                    "ts={ts} event=blind_preempt_reserve_hold acct={account} retry_after_secs={retry_after_secs} blind_secs={blind_secs}"
                )
            }
            Event::RetryAfterWalk {
                account,
                swaps,
                window_secs,
                retry_after_secs,
            } => {
                // The #582 throttle-walk alarm (issue #582): rotation stopped because the server
                // throttle is following the ACTIVE ROLE around the roster. All bare numbers (#15);
                // `acct=` is the UUID, matching the usage-family lines.
                format!(
                    "ts={ts} event=retry_after_walk acct={account} swaps={swaps} window_secs={window_secs} retry_after_secs={retry_after_secs}"
                )
            }
            Event::CredentialExpiryHorizon {
                account,
                state,
                expires_at,
                horizon_secs,
            } => {
                let state = state.as_str();
                // The epoch-SECOND deadline is rendered to RFC 3339 through the SAME formatter as
                // the line `ts` (and `all_exhausted`'s `resets_at`), so a human reads the date
                // without converting. Unlike `event=refresh`'s expiries this field is already
                // seconds — the daemon folds MS→s at the credential-read boundary (issue #878) —
                // so there is no `/ 1000` here, and adding one would silently render 1970.
                let expires_at = rfc3339(system_time_from_epoch(*expires_at));
                // An enum token, a timestamp and two bare numbers (#15); `acct=` is the UUID,
                // matching the usage-family lines.
                format!(
                    "ts={ts} event=credential_expiry_horizon acct={account} state={state} expires_at={expires_at} horizon_secs={horizon_secs}"
                )
            }
            Event::CredentialExpiryObserved {
                account,
                provenance,
                before,
                after,
                grant_replaced,
            } => {
                let provenance = provenance.as_str();
                // Each deadline is OMITTED when absent rather than rendered empty (an empty value
                // after `=` would split the `key=val` grammar) — the same handling as
                // `event=refresh`'s optional `expires_before` / `expires_after`. Epoch SECONDS
                // already, so no `/ 1000`: see the horizon arm above.
                let before_field = match before {
                    Some(secs) => {
                        format!(" before={}", rfc3339(system_time_from_epoch(*secs)))
                    }
                    None => String::new(),
                };
                // ALWAYS present — an observation without a deadline never reaches a line at all.
                let after_field = format!(" after={}", rfc3339(system_time_from_epoch(*after)));
                // `delta_secs=` is DERIVED from the pair, not stored — the same
                // store-the-ingredients-derive-the-view idiom as `event=refresh`'s `window_secs=`
                // and `event=blind_exit`'s burn fields. Rendered only when the BASELINE is known, so
                // it never claims a delta against one we do not have; a `delta_secs=0` on a
                // `provenance=canonical_restash` line IS the issue #877 third row (an external
                // credential write that did NOT move the deadline). Trails the whole line, so the
                // tolerant `key=val` field-map parsers are unaffected by its absence.
                let delta = match before {
                    Some(before) => format!(" delta_secs={}", after.saturating_sub(*before)),
                    None => String::new(),
                };
                // OMITTED when the path could not tell (`None`) — same absent-not-empty rule as the
                // deadlines above, and it keeps every poll-fold line byte-unchanged. A BOOL derived
                // from a byte comparison: no token material, by construction.
                let rt_field = match grant_replaced {
                    Some(replaced) => format!(" grant_replaced={replaced}"),
                    None => String::new(),
                };
                format!(
                    "ts={ts} event=credential_expiry_observed acct={account} provenance={provenance}{before_field}{after_field}{delta}{rt_field}"
                )
            }
            Event::ObservationGapEnter {
                account,
                elapsed_secs,
                threshold_secs,
                was_active,
            } => {
                // The observation-gap OPEN (issue #1453) — emitted the moment the active account's
                // last completed reading ages past the `2 · poll_secs / N` re-observation bound,
                // WITHOUT any poll of it having run. `threshold_secs` is the bound crossed: it is
                // derived from config, which no other line on this channel carries, so an offline
                // reader can separate a genuine breach from a re-tuned bound without a second
                // source. All bare numbers + bools (#15); `acct=` is the UUID, matching the
                // usage-family lines.
                format!(
                    "ts={ts} event=observation_gap_enter acct={account} elapsed_secs={elapsed_secs} threshold_secs={threshold_secs} was_active={was_active}"
                )
            }
            Event::ObservationGapExit {
                account,
                elapsed_secs,
                was_active,
                swapped_away,
            } => {
                // The observation-gap CLOSE (issue #1453) — the post-swap first-sight latency
                // sample. `elapsed_secs` is the WHOLE anchor-to-observation gap, so the latency
                // reads off this one line rather than by differencing it against the entry;
                // `swapped_away` is what lets a reader drop the episodes that closed after the
                // daemon had already moved on, which are not first sights of a still-active
                // account. All bare numbers + bools (#15); `acct=` is the UUID.
                format!(
                    "ts={ts} event=observation_gap_exit acct={account} elapsed_secs={elapsed_secs} was_active={was_active} swapped_away={swapped_away}"
                )
            }
        };
        single_line(line)
    }
}

/// Render a filesystem path as a log-line VALUE — percent-encoded so it can never contain
/// whitespace (issue #786).
///
/// Every other value on this channel is a handle, an enum, a number or a timestamp, and is
/// whitespace-free by construction. That is not incidental: `reliability::parse_events` tokenizes a
/// line on spaces and says so — *"handles/values are whitespace-free by the log's grammar, so
/// tokenizing on spaces is exact"*. A path is the first free-form value the log carries, and a path
/// may legally contain a space, so rendering one raw would not bend that invariant but BREAK it:
/// `path=/Users/o/x event=swap from=a to=b/claude` tokenizes into a spurious `event=swap` that
/// OVERWRITES the real `event` in a field map, and the reliability parser would then run the line
/// through its swap arm. Field POSITION cannot prevent that; a whitespace-free value can.
///
/// So whitespace is encoded, and `%` with it — `%` first, so the mapping stays reversible and a
/// literal `%` in a path (`%25`) is never confusable with an encoded space. Multi-byte whitespace
/// (U+00A0 and friends) is encoded per UTF-8 byte, as percent-encoding is defined on bytes.
/// NOTHING else is touched, deliberately: an `=` inside the value is harmless once the value is a
/// single token (`split_once('=')` splits at the FIRST `=`, so the key stays `path`), and leaving
/// it alone keeps the overwhelmingly common case — a path with no space and no `%` — byte-for-byte
/// identical to the raw path, so `grep path=` and copy-paste behave exactly as an operator expects.
///
/// `display()` is lossy for a non-UTF-8 path. A byte-level encoding over the raw `OsStr` COULD
/// carry one intact — this is a deliberate omission, not a limitation of the grammar. Such a path
/// cannot name an existing `claude` on the filesystems this daemon targets, so the resolver never
/// yields one, and the extra machinery would buy fidelity for a value that cannot occur. The
/// grammar holds either way: U+FFFD is not whitespace.
fn path_value(path: &Path) -> String {
    percent_encode(&path.display().to_string(), |ch| {
        ch == '%' || ch.is_whitespace()
    })
}

/// Percent-encode exactly the characters of `raw` that `needs_encoding` selects.
///
/// The shared body of this channel's two encoders — [`path_value`] (issue #786, one FIELD) and
/// [`single_line`] (issue #1092, a whole LINE) — which differ only in which characters they may
/// not let through, never in how one is spelled. Factored so a future third caller inherits the
/// same encoding rather than growing a fourth hex loop that drifts from these two.
///
/// Encoding is defined on BYTES, so a selected multi-byte character renders one `%XX` triple per
/// UTF-8 byte (U+00A0 → `%C2%A0`). Uppercase hex, the percent-encoding convention.
fn percent_encode(raw: &str, needs_encoding: impl Fn(char) -> bool) -> String {
    /// Uppercase hex digits, the percent-encoding convention.
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let mut out = String::with_capacity(raw.len());
    for ch in raw.chars() {
        if !needs_encoding(ch) {
            out.push(ch);
            continue;
        }
        let mut buf = [0u8; 4];
        for byte in ch.encode_utf8(&mut buf).as_bytes() {
            out.push('%');
            out.push(HEX[usize::from(byte >> 4)] as char);
            out.push(HEX[usize::from(byte & 0x0F)] as char);
        }
    }
    out
}

/// Force a rendered log line to BE one line: percent-encode every control character in it
/// (issue #1092).
///
/// Applied at the single exit of [`Event::to_log_line`] and its diagnostic sibling
/// [`Diagnostic::to_log_line`], which is the whole design. A per-field helper would have to be
/// remembered at ~40 interpolation sites and at every one a future field adds; one call on the
/// finished line cannot be forgotten, and it costs nothing to be total, since every key, enum
/// token, number and timestamp on these channels is control-free by construction — only a
/// free-form value can ever be changed by this.
///
/// # What it is for
///
/// A record separator is the one piece of grammar a reader cannot recover from. Not every value
/// that reaches a line is charset-constrained by then — a roster `account_uuid` is (issue #1052),
/// but `label` is deliberately free-form, and the login-failure path logs an `account_uuid`
/// harvested from `~/.claude.json` *before* the roster gate applies. A newline in one of those
/// ends the record early and opens a second that can be spelled to look exactly like a real
/// event, which is worse than a mangled line: a mangled line announces itself.
///
/// # Why CONTROL characters only, and not `%` with them
///
/// `path_value` encodes `%` so ITS mapping stays reversible. This one deliberately does not, and
/// the two are not in tension — they are answering different questions:
///
/// - Encoding `%` here would double-encode [`path_value`]'s own output (`%20` → `%2520`) on the
///   one line that carries it, since this runs after the arms.
/// - It would also REFORMAT well-formed lines. The event log's grammar is frozen
///   ([`crate::log`]) and its records are durable, so the bar is that a value which cannot split
///   a record renders byte-for-byte as it always has. Touching `%` — or whitespace, which is a
///   field-splitting concern and not a record-splitting one — would break that for values that
///   were never a problem.
///
/// The cost is a residual ambiguity, stated rather than hidden: a label containing the literal
/// text `%0A` renders identically to one containing a newline. Both come from the same operator,
/// and neither can split the record, so the ambiguity is cosmetic where the split was not.
///
/// # Bound
///
/// `char::is_control` is Unicode category Cc — `\n`, `\r`, `\t`, `\0`, ESC, DEL and the C1 block
/// (which includes U+0085 NEL, a line separator for some readers). It does NOT cover U+2028 /
/// U+2029, which are not control characters and are not record separators for any reader of this
/// log: [`crate::log`], [`crate::reliability`] and [`crate::usage_stats`] all split on `\n` via
/// `str::lines`. This is the record-integrity boundary, not a terminal-safety one.
fn single_line(line: String) -> String {
    // The overwhelmingly common case is a line with no control character at all: return the
    // string that was already built rather than a re-encoded copy of it, so the ordinary path
    // allocates nothing extra and is byte-identical by inspection, not merely by argument.
    if line.contains(char::is_control) {
        percent_encode(&line, char::is_control)
    } else {
        line
    }
}

/// Render the ` rotated=<bool>` field for a refresh-family line, or the empty string where
/// the outcome cannot carry one (issue #1004).
///
/// The three refresh-family events (`refresh`, `poll_refresh`, `keep_warm`) share this so the
/// omission rule is stated ONCE: an outcome that admits no rotation renders no `rotated=`,
/// rather than rendering a fabricated `false` — an absent field says "this outcome has no
/// rotation to report", which is the truth, whereas `rotated=false` would assert a compare
/// was performed and came back negative. The leading space belongs to the field, so a line
/// that omits it is byte-for-byte free of it.
fn rotated_field(outcome: RefreshEventOutcome) -> String {
    match outcome.rotated() {
        Some(rotated) => format!(" rotated={rotated}"),
        None => String::new(),
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
///
/// `pub(crate)` so the windowed `reliability` readout (#494) can render its `--since`
/// cutoff back to the log's own `ts=` shape through this SAME renderer — the inverse
/// of [`crate::usage::epoch_from_rfc3339`], which parses `ts=` the other way — rather
/// than hand-rolling a second copy of the civil-date arithmetic.
pub(crate) fn rfc3339(ts: SystemTime) -> String {
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
/// the daemon's cooldown floor, so both count here. The event key is read by field
/// POSITION — `event=` is always the second field — so a free-form handle spelling
/// ` event=swap ` inside its own value cannot make a non-swap line answer this query
/// (ADR-0034, issue #1185). Best-effort: an unreadable file or an unparseable timestamp
/// yields `None`, so a one-shot manual swap is never blocked by a missing or corrupt log
/// (the cooldown then reads as inactive).
pub(crate) fn last_swap_at(path: &std::path::Path) -> Option<SystemTime> {
    let text = std::fs::read_to_string(path).ok()?;
    // Scan from the END: the log is append-only chronological, so the last swap
    // line is the most recent swap. The event key is read by POSITION — the second
    // whitespace-delimited field — so a handle that merely spells the text cannot
    // be mistaken for it (issue #1185).
    let line = text.lines().rev().find(|line| {
        matches!(
            line.split(' ').nth(1),
            Some("event=swap" | "event=emergency_swap")
        )
    })?;
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
/// The `status` view computes its 5-state rollup live in the daemon (issue #119); when
/// the daemon is down — often exactly when a wedged credential needs inspecting — that
/// path is unavailable, so `list` reads the durable record the refresh sweep already
/// wrote: each [`Event::Refresh`] line (`event=refresh account={handle} outcome={token}`,
/// issue #106). The SAME file-read sibling of [`last_swap_at`] — scan the append-only,
/// chronological log; the last `refresh` line for a handle is its most recent outcome.
///
/// One pass (not one read per account): later lines overwrite earlier, so each handle
/// ends mapped to its newest outcome. A handle is free-form and may spell any of this
/// reader's own landmarks (ADR-0034, issue #1185), so neither end of the account field is
/// found by substring search: the line is a refresh line only when `event=refresh` is its
/// SECOND field — a position no later value can occupy — and the handle then runs from
/// `account=` to the LAST ` outcome=` on the line. That last one is always the writer's,
/// because [`Event::to_log_line`] emits `outcome=` once and every field after it is a
/// number or an enum token (`expires_*`, `rotated`, `reason`, `backoff_secs`,
/// `window_secs`). A handle carrying a space, an `=`, or the literal text ` outcome=` is
/// therefore matched WHOLE, and no line is attributed to an account that did not write it.
/// Best-effort like [`last_swap_at`]: an absent/unreadable log yields an empty map, so
/// `list` simply omits the refresh tag.
///
/// Non-secret: the event log is itself a redaction-metered surface (issue #15) — every
/// line is a handle / enum / timestamp — so the returned handles and outcomes carry no
/// token or email.
pub(crate) fn last_refresh_outcomes(
    path: &std::path::Path,
) -> std::collections::HashMap<String, RefreshEventOutcomeKind> {
    let mut outcomes = std::collections::HashMap::new();
    let Ok(text) = std::fs::read_to_string(path) else {
        return outcomes;
    };
    for line in text.lines() {
        // ts=… event=refresh account={handle} outcome={token}[ expires_before=…][ expires_after=…]
        //
        // The event key is field 1 by POSITION, and the handle runs to the LAST ` outcome=` on
        // the line — the two things a free-form handle cannot forge (issue #1185).
        let mut fields = line.splitn(3, ' ');
        let _ts = fields.next();
        if fields.next() != Some("event=refresh") {
            continue;
        }
        let Some(rest) = fields.next().and_then(|rest| rest.strip_prefix("account=")) else {
            continue;
        };
        let Some((handle, after)) = rest.rsplit_once(" outcome=") else {
            continue;
        };
        let token = after.split(' ').next().unwrap_or(after);
        if let Some(outcome) = RefreshEventOutcomeKind::from_token(token) {
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
    /// Preemptively swapped away from a BLIND active account before it could
    /// self-exhaust unobserved (issue #452, ADR-0017) — the bounded-blindness gate fired.
    PreemptiveSwap,
    /// Preemptively swapped away from an OBSERVED active account whose PROJECTED session usage
    /// crossed the trigger before the observed reading did (issue #539, ADR-0017) — the
    /// velocity-projection gate fired. Distinct from `PreemptiveSwap` (blind, stale anchor):
    /// this fires on a fresh reading + its velocity.
    VelocityPreemptiveSwap,
    /// Over the trigger but no viable target — the all-exhausted hold (issue #11).
    AllExhausted,
    /// The active credential is dead and no target is viable — held, unable to
    /// escape (issue #42).
    ActiveDeadNoTarget,
    /// The shared canonical was scrubbed/empty and the daemon autonomously adopted a
    /// viable target's token into it, healing every session (issue #467).
    CanonicalAdopted,
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
            DecisionClass::PreemptiveSwap => "preemptive_swap",
            DecisionClass::VelocityPreemptiveSwap => "velocity_preemptive_swap",
            DecisionClass::AllExhausted => "all_exhausted",
            DecisionClass::ActiveDeadNoTarget => "active_dead_no_target",
            DecisionClass::CanonicalAdopted => "canonical_adopted",
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
        target_max_session_usage: u8,
        session_ceiling: u8,
        weekly_ceiling: u8,
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
    /// server-advised `Retry-After` (delta-seconds, BEFORE any daemon cap — the
    /// `POLL_BACKOFF_CAP` clamp is peer-only since #453) the throttled poll's response
    /// supplied, when any. `Some` ⇒ the server
    /// advised a floor; `None` ⇒ the wait is the daemon's self-capped exponential (or
    /// the keychain-lock back-off), with no server advice. It disambiguates a
    /// `backoff_secs` an operator otherwise cannot place, by comparison: absent ⇒
    /// self-capped exponential; `== backoff_secs` ⇒ the server-advised wait governed;
    /// `< backoff_secs` ⇒ the server advised a smaller floor but the exponential governed;
    /// `> backoff_secs` ⇒ the #294 cap clamped a pathological value on a PEER (e.g.
    /// `backoff_secs=3600 retry_after_secs=86400`). Pre-cap on purpose, so that clamped
    /// value stays visible rather than collapsing into an indistinguishable `backoff_secs=3600`.
    /// The ACTIVE account never shows `> backoff_secs`: its `Retry-After` is an un-clamped
    /// floor (issue #453), so `backoff_secs >= retry_after_secs` always holds for it.
    Tick {
        decision: DecisionClass,
        backoff_secs: Option<u64>,
        retry_after_secs: Option<u64>,
    },
    /// The shared canonical `Claude Code-credentials` item's OWN per-poll reading (issue #464):
    /// its liveness `state` ([`CanonicalLiveness`]: present / scrubbed / unknown), a redaction-safe
    /// `fingerprint` of the current refresh token (a SHA-256 hex PREFIX — identity, never the
    /// token), the resolved account `handle`, and the access-token `expires_at` (epoch seconds).
    /// The LEVEL record emitted every poll — the measurable substrate the rotation-interference
    /// rate (#465) and the autonomous-recovery trigger (#467) consume; its edge-crossings are the
    /// durable [`Event::CanonicalScrubbed`] / [`Event::CanonicalRestored`] pair. Every field is a
    /// discriminant / hash-prefix / handle / timestamp — never a secret (issue #15). The three
    /// optional fields are absent on a scrubbed / unknown read (no live token to fingerprint, no
    /// expiry, possibly no resolvable handle).
    Canonical {
        state: CanonicalLiveness,
        fingerprint: Option<String>,
        account: Option<String>,
        expires_at: Option<i64>,
        /// The PRIOR poll's canonical fingerprint when THIS poll observed a Present→Present
        /// rotation (issue #475) — `Some(prev)` iff the refresh-token fingerprint CHANGED since
        /// the last Present observation (a rotation-YANK: the shared item rotated under mid-flight
        /// sessions, which get a RECOVERABLE 401 while the item stays live — distinct from the
        /// UNRECOVERABLE scrub the durable [`Event::CanonicalScrubbed`] carries). Renders the
        /// additive trailing `mode=yank prev=<prev>` marker, giving the `diag=canonical` line the
        /// same `mode=(yank|scrub)` vocabulary the durable scrub event carries — so `grep mode=`
        /// surfaces both "Not logged in" modes. `None` on a non-rotating poll, the seeding first
        /// observation, and every non-Present read. A non-secret 16-hex SHA-256 prefix (issue #15),
        /// the same shape as `fingerprint` — never a token.
        rotated_from: Option<String>,
    },
}

impl Diagnostic {
    /// Render this diagnostic as its single line (no trailing newline), stamped with
    /// `ts`. Pure and the *only* place a diagnostic becomes text, so — exactly like
    /// [`Event::to_log_line`] — the redaction surface is this method alone. `ts` is a
    /// parameter (not read here) so the formatting is deterministically unit-testable;
    /// [`DiagnosticLog::emit`] supplies `SystemTime::now()` at write time.
    ///
    /// Same single-exit [`single_line`] guarantee as the event renderer (issue #1092), and for
    /// the same reason rather than for symmetry's sake: this channel's `account=` is the same
    /// free-form operator label, and `sessiometer log --channel diag` reads the file it lands in.
    /// That channel is the UNGOVERNED one — it also carries panic payloads, and the reader says
    /// so — but "ungoverned" is about what may APPEAR on a line, not about a value being allowed
    /// to forge a second one.
    pub(crate) fn to_log_line(&self, ts: SystemTime) -> String {
        let ts = rfc3339(ts);
        let line = match self {
            Diagnostic::Start {
                accounts,
                poll_secs,
                target_max_session_usage,
                session_ceiling,
                weekly_ceiling,
                monitor_401_n,
                monitor_recovery_m,
            } => {
                // target_max_session_usage (#398) is always-valued — render its percent directly,
                // like the other counts/percentages (no `off` sentinel to carry).
                format!(
                    "ts={ts} diag=start accounts={accounts} poll_secs={poll_secs} \
                     target_max_session_usage={target_max_session_usage} session_ceiling={session_ceiling} \
                     weekly_ceiling={weekly_ceiling} monitor_401_n={monitor_401_n} \
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
            Diagnostic::Canonical {
                state,
                fingerprint,
                account,
                expires_at,
                rotated_from,
            } => {
                let state = state.as_str();
                // Each optional field renders only when present — an empty value after `=` would
                // split the `key=val` grammar (mirrors `diag=tick`'s optional `backoff_secs`).
                // `expires_at` is raw epoch SECONDS, matching this channel's `*_secs` convention
                // and the wire's `access_expires_at` unit.
                let fingerprint = match fingerprint {
                    Some(fp) => format!(" fingerprint={fp}"),
                    None => String::new(),
                };
                let account = match account {
                    Some(label) => format!(" account={label}"),
                    None => String::new(),
                };
                let expires_at = match expires_at {
                    Some(secs) => format!(" expires_at={secs}"),
                    None => String::new(),
                };
                // The rotation-YANK marker (issue #475): additive TRAILING tokens present ONLY on a
                // Present→Present fingerprint delta, so a non-rotating poll line is byte-for-byte
                // unchanged (every existing `key=val` parser is unaffected). `prev` is the prior
                // fingerprint — a non-secret hash prefix like `fingerprint`, never the prior token.
                let rotated = match rotated_from {
                    Some(prev) => format!(" mode=yank prev={prev}"),
                    None => String::new(),
                };
                format!(
                    "ts={ts} diag=canonical state={state}{fingerprint}{account}{expires_at}{rotated}"
                )
            }
        };
        single_line(line)
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
pub(crate) mod tests {
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
            projection: None,
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
            projection: None,
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
            projection: None,
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
            projection: None,
        }
        .to_log_line(at_epoch(0));
        assert!(line.contains("reason=weekly"), "got: {line}");
    }

    #[test]
    fn swap_line_renders_the_velocity_preempt_reason_redaction_clean() {
        // Issue #539: the projective swap renders `reason=velocity_preempt` with `session_pct`
        // carrying the FRESH observed reading at swap-out — the exact wire token the reliability
        // parser greps to fold the projected-swap-out-overshoot SLI. Below the ceiling it carries no
        // `late=` marker (a projective swap fires while observed < trigger ≤ 99, so it is never late).
        // The line is redaction-clean (#15): only the operator HANDLES + a percent, never the email
        // or token — the same single-surface discipline every other swap reason rides.
        //
        // Issue #634: with `projection: None` (the pre-#634 shape) the line is BYTE-FOR-BYTE what it
        // was before the field existed — the projection tokens are strictly additive, so an old log
        // and the tolerant field-map parsers are unaffected. The WITH-projection rendering is
        // asserted separately by `swap_line_appends_the_projection_ingredients_for_a_velocity_preempt`.
        let line = Event::Swap {
            from: "work".to_owned(),
            to: "spare".to_owned(),
            reason: SwapReason::VelocityPreempt,
            session_pct: 92,
            projection: None,
        }
        .to_log_line(at_epoch(0));
        assert_eq!(
            line,
            format!("{TS0} event=swap from=work to=spare reason=velocity_preempt session_pct=92")
        );
        assert!(
            !line.contains("late"),
            "a projective swap is never late: {line}"
        );
    }

    #[test]
    fn swap_line_appends_the_projection_ingredients_for_a_velocity_preempt() {
        // Issue #634: the projective swap that fired at an OBSERVED session_pct=70 is
        // SELF-EXPLAINING — the projection it decided on trails as additive tokens, so the offline
        // reader sees `projected=96.30 >= ceiling=89.00` and understands why a below-trigger reading
        // swapped. The ingredient (`rate`) is FULL PRECISION (6 decimals) — a rounded rate cannot
        // reproduce the decision — and the constants IN FORCE (horizon, effective ceiling) are
        // stamped beside it, so the record stays interpretable if either tunable later drifts.
        // Percents render to 2 decimals, the rate to 6. Redaction-clean (#15): every new token is a
        // bare number.
        let line = Event::Swap {
            from: "work".to_owned(),
            to: "spare".to_owned(),
            reason: SwapReason::VelocityPreempt,
            session_pct: 70,
            projection: Some(SwapProjection {
                projected_pct: 96.3,
                rate_pct_per_sec: 0.218333,
                horizon_secs: 120,
                ceiling_pct: 89.0,
            }),
        }
        .to_log_line(at_epoch(0));
        assert_eq!(
            line,
            format!(
                "{TS0} event=swap from=work to=spare reason=velocity_preempt session_pct=70 projected=96.30 rate=0.218333 horizon=120 ceiling=89.00"
            )
        );
        // The reader can reproduce the fire decision from the line's own tokens: the projection is
        // at/over the stamped effective ceiling.
        assert!(
            crate::redaction::meter::unauthored_emails(line.as_str(), &[]).is_empty(),
            "no email sigil (#15): {line}"
        );
        assert!(
            !line.to_lowercase().contains("token"),
            "no token (#15): {line}"
        );
    }

    #[test]
    fn swap_line_renders_the_blind_preempt_reason_redaction_clean() {
        // Issue #479 (surface 2, LOG medium): the #452 bounded-blindness preemptive swap renders
        // `reason=blind_preempt` with `session_pct` carrying the STALE pre-blind anchor (the only
        // session signal available while blind) — the durable event-log narration of the same swap the
        // `status` wire reflects as prose (`recent_blind_preempt_swap`) and the daemon retains for the
        // `use <label>` undo. Each medium in its own idiom (R-2 STATE-parity): the log carries the
        // machine-greppable `reason=`/`from=`/`to=` tuple, `status` carries the operator prose. Below
        // the ceiling (a blind-preempt fires on an anchor at/over the risk band but under the reactive
        // trigger ≤ 99) it carries no `late=` marker. Redaction-clean (#15): only the operator HANDLES
        // + a percent ride the line, never the email or token — the single-surface discipline every
        // other swap reason rides.
        let line = Event::Swap {
            from: "work".to_owned(),
            to: "spare".to_owned(),
            reason: SwapReason::BlindPreempt,
            session_pct: 68,
            // The blind-preempt swap carries no projection on its own line (issue #634): the
            // retained velocity rides the paired `blind_window` event instead.
            projection: None,
        }
        .to_log_line(at_epoch(0));
        assert_eq!(
            line,
            format!("{TS0} event=swap from=work to=spare reason=blind_preempt session_pct=68")
        );
        assert!(
            !line.contains("late"),
            "a blind-preempt swap fires under the reactive trigger, never late: {line}"
        );
    }

    #[test]
    fn all_exhausted_renders_cause_and_resets_at_when_known_and_omits_reset_otherwise() {
        // No reset reported (#11 fallback) → resets_at is simply absent and the line
        // stays well-formed; cause (#398) is always present.
        let absent = Event::AllExhausted {
            hold: "work".to_owned(),
            cause: SwapReason::Weekly,
            resets_at: None,
        }
        .to_log_line(at_epoch(0));
        assert_eq!(
            absent,
            format!("{TS0} event=all_exhausted hold=work cause=weekly")
        );
        assert!(!absent.contains("resets_at"), "got: {absent}");

        // A known reset (epoch seconds, #11) is rendered to RFC 3339 by the same
        // single formatter — 1_782_777_600 is 2026-06-30T00:00:00Z. A session-wide
        // block (#398) reports cause=session and keys off the SESSION reset.
        let present = Event::AllExhausted {
            hold: "work".to_owned(),
            cause: SwapReason::Session,
            resets_at: Some(1_782_777_600),
        }
        .to_log_line(at_epoch(0));
        assert_eq!(
            present,
            format!(
                "{TS0} event=all_exhausted hold=work cause=session resets_at=2026-06-30T00:00:00Z"
            )
        );
    }

    #[test]
    fn all_exhausted_cleared_line_is_bare() {
        // The edge-triggered EXIT partner (issue #800): a bare token — the episode's span is
        // bracketed by pairing this with the last `all_exhausted` ENTER line, exactly as
        // `usage_backoff_cleared` / `exhausted_slow_poll_cleared` bracket theirs.
        let line = Event::AllExhaustedCleared.to_log_line(at_epoch(0));
        assert_eq!(line, format!("{TS0} event=all_exhausted_cleared"));
    }

    #[test]
    fn active_dead_no_target_mirrors_all_exhausted_grammar_and_omits_an_absent_reset() {
        // #405: the strictly-worse sibling of `all_exhausted` renders the SAME `key=val` grammar —
        // `hold` (the dead active), a required `cause`, and a trailing OPTIONAL `resets_at`. On the
        // emergency (dead-active) path the cause is always weekly (the session gate is bypassed).
        let absent = Event::ActiveDeadNoTarget {
            hold: "work".to_owned(),
            cause: SwapReason::Weekly,
            resets_at: None,
        }
        .to_log_line(at_epoch(0));
        assert_eq!(
            absent,
            format!("{TS0} event=active_dead_no_target hold=work cause=weekly")
        );
        assert!(!absent.contains("resets_at"), "got: {absent}");

        // A known spare weekly reset (epoch → RFC 3339 by the same formatter) trails the line.
        let present = Event::ActiveDeadNoTarget {
            hold: "work".to_owned(),
            cause: SwapReason::Weekly,
            resets_at: Some(1_782_777_600),
        }
        .to_log_line(at_epoch(0));
        assert_eq!(
            present,
            format!(
                "{TS0} event=active_dead_no_target hold=work cause=weekly resets_at=2026-06-30T00:00:00Z"
            )
        );
    }

    #[test]
    fn active_dead_no_target_cleared_line_is_bare() {
        // Issue #827: the strand's edge-triggered EXIT partner — a bare token, mirroring
        // `event=all_exhausted_cleared`. The strand's span is bracketed by pairing this with the
        // last `active_dead_no_target` ENTER line, so the payload lives on the ENTER alone.
        let line = Event::ActiveDeadNoTargetCleared.to_log_line(at_epoch(0));
        assert_eq!(line, format!("{TS0} event=active_dead_no_target_cleared"));
    }

    #[test]
    fn fleet_runway_low_renders_the_crossing_the_threshold_and_the_cardinality() {
        // #650: the proactive warn line carries four plain integers — the crossing reading, the
        // configured line it dropped below, and the `n of m` honesty cardinality (how many
        // accounts backed the figure vs were seen). Secret-free by construction (#15): no handle,
        // email, or token, so the line is as safe to log as `all_exhausted`.
        let line = Event::FleetRunwayLow {
            runway_secs: 1800,
            threshold_secs: 3600,
            counted: 2,
            observed: 3,
        }
        .to_log_line(at_epoch(0));
        assert_eq!(
            line,
            format!(
                "{TS0} event=fleet_runway_low runway_secs=1800 threshold_secs=3600 counted=2 observed=3"
            )
        );
    }

    #[test]
    fn fleet_runway_recovered_line_is_bare() {
        // Issue #827: the proactive warn's edge-triggered EXIT partner — a bare token, mirroring
        // `event=all_exhausted_cleared`. No payload: the ENTER line carried the reading, and the
        // recovery is just "back over", so the episode's span comes from pairing the two lines.
        let line = Event::FleetRunwayRecovered.to_log_line(at_epoch(0));
        assert_eq!(line, format!("{TS0} event=fleet_runway_recovered"));
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
    fn canary_unparseable_canonical_trails_the_override_only_when_it_fired() {
        // Issue #730: the fail-CLOSED refusal of an unparseable no-stash-match canonical logs one
        // line per attempt. `overridden` trails EXACTLY when `canary_nostashmatch_override` let the
        // write proceed anyway — the refused (default) line stays minimal and carries NO
        // `overridden=` token at all, so an operator grepping `event=canary_unparseable_canonical`
        // without `overridden=true` sees only genuine refusals. Redaction-clean (#15): the line is
        // pure classification — never a token, email, or canonical byte. Pinned byte-exact here
        // because the integration `log.contains(...)` assertions cannot catch a malformed spacing
        // or a stray `overridden=false` leaking into the refused branch.
        let refused =
            Event::CanaryUnparseableCanonical { overridden: false }.to_log_line(at_epoch(0));
        assert_eq!(refused, format!("{TS0} event=canary_unparseable_canonical"));

        let overridden =
            Event::CanaryUnparseableCanonical { overridden: true }.to_log_line(at_epoch(0));
        assert_eq!(
            overridden,
            format!("{TS0} event=canary_unparseable_canonical overridden=true")
        );
    }

    #[test]
    fn canary_online_probe_carries_the_verdict_and_trails_refused_only_when_it_fired() {
        // Issue #736: the Layer-3 probe's only durable surface. `verdict` always rides the
        // line (the operator needs to tell a dead bearer apart from a network blip — the two
        // demand different responses); `refused` trails EXACTLY when strict mode cost the
        // swap, mirroring the `overridden` idiom of its two canary siblings above. So the
        // graceful-degrade ride — probe failed, swap proceeded — is a minimal line an
        // operator can grep for as `event=canary_online_probe` without `refused=true`.
        // Redaction-clean (#15): a verdict CLASS and a flag, never a status code, a response
        // body, or a bearer. Pinned byte-exact — an integration `log.contains(...)` cannot
        // catch malformed spacing or a stray `refused=false` leaking into the degrade branch.
        let degraded = Event::CanaryOnlineProbe {
            verdict: "inconclusive",
            refused: false,
        }
        .to_log_line(at_epoch(0));
        assert_eq!(
            degraded,
            format!("{TS0} event=canary_online_probe verdict=inconclusive")
        );

        let refused = Event::CanaryOnlineProbe {
            verdict: "rejected",
            refused: true,
        }
        .to_log_line(at_epoch(0));
        assert_eq!(
            refused,
            format!("{TS0} event=canary_online_probe verdict=rejected refused=true")
        );
    }

    #[test]
    fn canonical_scrubbed_carries_the_scrub_mode_and_optional_handle() {
        // Issue #464/#475: the shared-item scrub renders the constant `mode=scrub` classification
        // (the durable half of `mode=(yank|scrub)`) then the last-known active HANDLE (never a
        // token or email) — and omits `account` cleanly when no active account was resolved (a
        // daemon started against an already-empty item), keeping a stable `... mode=scrub` prefix.
        let with_handle = Event::CanonicalScrubbed {
            account: Some("work".to_owned()),
        }
        .to_log_line(at_epoch(0));
        assert_eq!(
            with_handle,
            format!("{TS0} event=canonical_scrubbed mode=scrub account=work")
        );

        let no_handle = Event::CanonicalScrubbed { account: None }.to_log_line(at_epoch(0));
        assert_eq!(
            no_handle,
            format!("{TS0} event=canonical_scrubbed mode=scrub")
        );
    }

    #[test]
    fn canonical_restored_carries_the_handle_when_known_and_omits_it_otherwise() {
        // Issue #464: the clearing counterpart, same optional-handle grammar.
        let with_handle = Event::CanonicalRestored {
            account: Some("work".to_owned()),
        }
        .to_log_line(at_epoch(0));
        assert_eq!(
            with_handle,
            format!("{TS0} event=canonical_restored account=work")
        );

        let no_handle = Event::CanonicalRestored { account: None }.to_log_line(at_epoch(0));
        assert_eq!(no_handle, format!("{TS0} event=canonical_restored"));
    }

    #[test]
    fn canonical_recovered_carries_the_adopted_account_handle() {
        // Issue #467: the autonomous adopt-target recovery renders the adopted account HANDLE
        // (never a token or email). `account` is required — recovery only fires with a viable
        // target in hand, so there is always a handle to name.
        let line = Event::CanonicalRecovered {
            account: "spare".to_owned(),
        }
        .to_log_line(at_epoch(0));
        assert_eq!(
            line,
            format!("{TS0} event=canonical_recovered account=spare")
        );
    }

    #[test]
    fn canonical_recovery_exhausted_carries_the_handle_when_known_and_omits_it_otherwise() {
        // Issue #467: the back-off surface when re-scrub churn exceeds the window bound — same
        // optional-handle grammar as `canonical_scrubbed` (an empty value would split key=val),
        // absent when no active account was resolved at back-off time.
        let with_handle = Event::CanonicalRecoveryExhausted {
            account: Some("work".to_owned()),
        }
        .to_log_line(at_epoch(0));
        assert_eq!(
            with_handle,
            format!("{TS0} event=canonical_recovery_exhausted account=work")
        );

        let no_handle =
            Event::CanonicalRecoveryExhausted { account: None }.to_log_line(at_epoch(0));
        assert_eq!(
            no_handle,
            format!("{TS0} event=canonical_recovery_exhausted")
        );
    }

    #[test]
    fn canonical_diagnostic_renders_state_and_appends_each_present_field() {
        // Issue #464: the per-poll level line. A live read carries all four fields; a scrubbed /
        // unknown read carries only `state` (no live token to fingerprint, no expiry), each
        // optional field appended only when present (an empty value would split the grammar).
        let present = Diagnostic::Canonical {
            state: CanonicalLiveness::Present,
            fingerprint: Some("0123456789abcdef".to_owned()),
            account: Some("work".to_owned()),
            expires_at: Some(1_782_777_600),
            rotated_from: None,
        }
        .to_log_line(at_epoch(0));
        assert_eq!(
            present,
            format!(
                "{TS0} diag=canonical state=present fingerprint=0123456789abcdef account=work expires_at=1782777600"
            )
        );

        let scrubbed = Diagnostic::Canonical {
            state: CanonicalLiveness::Scrubbed,
            fingerprint: None,
            account: Some("work".to_owned()),
            expires_at: None,
            rotated_from: None,
        }
        .to_log_line(at_epoch(0));
        assert_eq!(
            scrubbed,
            format!("{TS0} diag=canonical state=scrubbed account=work")
        );

        let unknown = Diagnostic::Canonical {
            state: CanonicalLiveness::Unknown,
            fingerprint: None,
            account: None,
            expires_at: None,
            rotated_from: None,
        }
        .to_log_line(at_epoch(0));
        assert_eq!(unknown, format!("{TS0} diag=canonical state=unknown"));
    }

    #[test]
    fn canonical_diagnostic_appends_the_yank_marker_only_on_a_rotation() {
        // Issue #475: a Present→Present fingerprint delta appends the additive TRAILING
        // `mode=yank prev=<prior-fingerprint>` marker (after every #464 field), giving the level
        // line the same `mode=(yank|scrub)` vocabulary the durable scrub event carries. `prev` is a
        // non-secret hash prefix, never a token.
        let yank = Diagnostic::Canonical {
            state: CanonicalLiveness::Present,
            fingerprint: Some("0123456789abcdef".to_owned()),
            account: Some("work".to_owned()),
            expires_at: Some(1_782_777_600),
            rotated_from: Some("fedcba9876543210".to_owned()),
        }
        .to_log_line(at_epoch(0));
        assert_eq!(
            yank,
            format!(
                "{TS0} diag=canonical state=present fingerprint=0123456789abcdef account=work expires_at=1782777600 mode=yank prev=fedcba9876543210"
            )
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
    fn refresh_preflight_unresolved_carries_no_fields_at_all() {
        // Issue #787 / AC6: the preflight-established opening edge is a bare daemon-global line —
        // no count (one observation, nothing to count) and NO PATH. The unresolvable location is
        // exactly what a #15 review would object to, and it is absent by construction: a
        // SUCCESSFUL resolution has its own `refresh_binary_resolved` line (#786), and a failed one
        // has no path to name.
        let line = Event::RefreshPreflightUnresolved.to_log_line(at_epoch(0));
        assert_eq!(line, format!("{TS0} event=refresh_preflight_unresolved"));
    }

    #[test]
    fn an_episode_has_two_opening_brackets_and_one_closing_bracket() {
        // Issue #787: the log-balance forensics that FOUND #787 (one `refresh_systemic_failure`,
        // zero `refresh_systemic_recovered` ⇒ an episode that was erased, not closed) must survive
        // the fix. It does, as a two-token alternation: an episode opens with EITHER the sweep
        // crossing OR the preflight, and closes with the one recovery event. Pinned here so a
        // future event rename cannot silently break an operator's grep vocabulary.
        let sweep_open = Event::RefreshSystemicFailure { consecutive: 3 }.to_log_line(at_epoch(0));
        let preflight_open = Event::RefreshPreflightUnresolved.to_log_line(at_epoch(0));
        let close = Event::RefreshSystemicRecovered.to_log_line(at_epoch(0));

        assert!(sweep_open.contains(" event=refresh_systemic_failure "));
        assert!(preflight_open.ends_with(" event=refresh_preflight_unresolved"));
        assert!(close.ends_with(" event=refresh_systemic_recovered"));
        // The two openers are distinguishable — the whole reason the preflight got its own event
        // rather than a synthesized `consecutive=1` of the sweep one.
        assert_ne!(sweep_open, preflight_open);
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
        // A successful refresh: handle + outcome token + the before/after expiry rendered to
        // RFC 3339 (epoch ms → whole-second UTC), plus the explicit `window_secs=` slide (#409 —
        // the +1h forward move, `expires_after − expires_before` in whole seconds).
        let refreshed = Event::Refresh {
            account: "spare".to_owned(),
            outcome: RefreshEventOutcome::Refreshed { rotated: true },
            expires_before: Some(1_782_777_600_000),
            expires_after: Some(1_782_781_200_000), // +1h
            reason: None,                           // a success carries no error reason (#377)
            backoff_secs: None,                     // a success carries no back-off (#408)
        }
        .to_log_line(at_epoch(0));
        assert_eq!(
            refreshed,
            format!(
                "{TS0} event=refresh account=spare outcome=refreshed \
                 expires_before=2026-06-30T00:00:00Z expires_after=2026-06-30T01:00:00Z \
                 rotated=true window_secs=3600"
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
            reason: None,
            backoff_secs: None,
        }
        .to_log_line(at_epoch(0));
        assert_eq!(
            unknown,
            format!("{TS0} event=refresh account=spare outcome=error")
        );
        assert!(!unknown.contains("expires_"), "got: {unknown}");
        assert!(!unknown.contains("reason="), "got: {unknown}");
    }

    #[test]
    fn refresh_line_carries_the_window_secs_slide() {
        // Issue #409: the stored expiry's forward slide is first-class as an additive TRAILING
        // `window_secs=` field — `expires_after − expires_before` in whole seconds — so the
        // sliding-window-vs-cap signal is monitorable without deriving it from the two timestamps
        // by hand. It is the EXPIRY difference, independent of the line's own `ts`.

        // A +2h slide (7200 s). The non-zero line `ts` proves the field is
        // `expires_after − expires_before`, NOT `expires_after − ts`.
        let slid = Event::Refresh {
            account: "spare".to_owned(),
            outcome: RefreshEventOutcome::Refreshed { rotated: true },
            expires_before: Some(1_782_777_600_000),
            expires_after: Some(1_782_784_800_000), // +7200 s
            reason: None,
            backoff_secs: None,
        }
        .to_log_line(at_epoch(1_000));
        assert!(
            slid.ends_with(" window_secs=7200"),
            "the slide trails the line: {slid}"
        );

        // No slide (before == after — a held expiry, e.g. `no_change`): `window_secs=0`.
        let held = Event::Refresh {
            account: "spare".to_owned(),
            outcome: RefreshEventOutcome::NoChange,
            expires_before: Some(1_782_777_600_000),
            expires_after: Some(1_782_777_600_000),
            reason: None,
            backoff_secs: None,
        }
        .to_log_line(at_epoch(0));
        assert!(
            held.contains(" window_secs=0"),
            "a held expiry renders a zero slide: {held}"
        );

        // An unreadable expiry: the slide is undefined, so `window_secs=` is OMITTED entirely
        // (never an empty value that would split the key=val grammar), exactly like `expires_*`.
        let unknown = Event::Refresh {
            account: "spare".to_owned(),
            outcome: RefreshEventOutcome::Refreshed { rotated: true },
            expires_before: None,
            expires_after: Some(1_782_784_800_000),
            reason: None,
            backoff_secs: None,
        }
        .to_log_line(at_epoch(0));
        assert!(
            !unknown.contains("window_secs="),
            "a missing expiry omits the slide: {unknown}"
        );
    }

    #[test]
    fn refresh_error_line_carries_the_trailing_reason() {
        // Issue #377: the non-secret error sub-class rides an additive TRAILING `reason=` field,
        // AFTER the OPTIONAL `rotated=` (mirroring the swap line's optional trailing `late=`), so
        // a normal-outcome line is byte-for-byte unchanged and existing `key=val` parsers are
        // unaffected. `rotated=` is absent on `error` since issue #1004 — the outcome exchanged
        // no token — so on THIS line `reason=` follows `outcome=` directly. Each fixed class renders its documented token — including
        // `unresolved`, added the same way `timeout` was (issue #786).
        for (reason, token) in [
            (RefreshEventReason::SpawnFailed, "spawn_failed"),
            (
                RefreshEventReason::ReadbackUnreadable,
                "readback_unreadable",
            ),
            (RefreshEventReason::Malformed, "malformed"),
            (RefreshEventReason::Timeout, "timeout"),
            (RefreshEventReason::Unresolved, "unresolved"),
        ] {
            let line = Event::Refresh {
                account: "spare".to_owned(),
                outcome: RefreshEventOutcome::Error,
                expires_before: None,
                expires_after: None,
                reason: Some(reason),
                backoff_secs: None,
            }
            .to_log_line(at_epoch(0));
            assert_eq!(
                line,
                format!("{TS0} event=refresh account=spare outcome=error reason={token}")
            );
        }
    }

    #[test]
    fn refresh_error_line_carries_the_trailing_backoff_secs() {
        // Issue #408: the per-account back-off an error armed rides an additive TRAILING
        // `backoff_secs=` field, AFTER the optional `reason=` (mirroring the tick line's
        // `backoff_secs=` and the swap line's `late=`), so it is observable without disturbing any
        // existing `key=val` parser. Three cases pin the field's placement and its omission.

        // With BOTH a reason and a back-off: `backoff_secs=` trails `reason=`.
        let timed_out = Event::Refresh {
            account: "spare".to_owned(),
            outcome: RefreshEventOutcome::Error,
            expires_before: None,
            expires_after: None,
            reason: Some(RefreshEventReason::Timeout),
            backoff_secs: Some(240),
        }
        .to_log_line(at_epoch(0));
        assert_eq!(
            timed_out,
            format!(
                "{TS0} event=refresh account=spare outcome=error reason=timeout backoff_secs=240"
            )
        );

        // A hard `Err` (no reason) that armed a back-off: `backoff_secs=` rides in `rotated=`'s
        // slot — which an `error` outcome leaves empty (issue #1004) — with no `reason=` between.
        let hard = Event::Refresh {
            account: "spare".to_owned(),
            outcome: RefreshEventOutcome::Error,
            expires_before: None,
            expires_after: None,
            reason: None,
            backoff_secs: Some(120),
        }
        .to_log_line(at_epoch(0));
        assert_eq!(
            hard,
            format!("{TS0} event=refresh account=spare outcome=error backoff_secs=120")
        );

        // A successful refresh (no back-off): the field is OMITTED entirely — the line is
        // byte-for-byte the pre-#408 shape.
        let ok = Event::Refresh {
            account: "spare".to_owned(),
            outcome: RefreshEventOutcome::Refreshed { rotated: true },
            expires_before: None,
            expires_after: None,
            reason: None,
            backoff_secs: None,
        }
        .to_log_line(at_epoch(0));
        assert!(
            !ok.contains("backoff_secs="),
            "a success omits backoff_secs: {ok}"
        );
    }

    #[test]
    fn unresolved_keeps_reason_a_trailing_additive_field() {
        // T6 / issue #786: the new class changes nothing about the line's SHAPE. `reason=` stays
        // where #377 put it — after the optional `rotated=` (absent on `error`, issue #1004),
        // before `backoff_secs=` and `window_secs=` — so every existing `key=val` parser reads the line exactly as before
        // and simply sees one more token value. Asserted with the full field set present, which
        // is the only arrangement that can catch a placement regression.
        let line = Event::Refresh {
            account: "spare".to_owned(),
            outcome: RefreshEventOutcome::Error,
            expires_before: Some(1_000_000),
            expires_after: Some(1_000_000),
            reason: Some(RefreshEventReason::Unresolved),
            backoff_secs: Some(120),
        }
        .to_log_line(at_epoch(0));
        assert_eq!(
            line,
            format!(
                "{TS0} event=refresh account=spare outcome=error \
                 expires_before=1970-01-01T00:16:40Z expires_after=1970-01-01T00:16:40Z \
                 reason=unresolved backoff_secs=120 window_secs=0"
            )
        );
    }

    #[test]
    fn every_refresh_reason_token_is_distinct_and_grep_safe() {
        // T9 + T10 / issue #15. Every `reason=` value is a FIXED snake_case token: no `/` (a
        // path), no `@` (an email), no separator that could split the `key=val` grammar, and
        // nothing long enough to be a credential. Distinctness is the other half — two classes
        // sharing a token would silently merge two causes in the log, which is the failure #786
        // is fixing, one level down.
        //
        // Exhaustiveness is enforced by the COMPILER, not here: `RefreshEventReason::as_str` has
        // no `_` arm, so a new variant fails to build until it is given a token. This list is
        // what asserts the tokens themselves are usable once they exist.
        let reasons = [
            RefreshEventReason::SpawnFailed,
            RefreshEventReason::ReadbackUnreadable,
            RefreshEventReason::Malformed,
            RefreshEventReason::Timeout,
            RefreshEventReason::Unresolved,
        ];
        let mut seen: Vec<&str> = Vec::new();
        for reason in reasons {
            let token = reason.as_str();
            assert!(!token.is_empty(), "{reason:?} renders an empty token");
            assert!(
                token.chars().all(|c| c.is_ascii_lowercase() || c == '_'),
                "{reason:?} renders {token:?}, which is not a fixed snake_case token"
            );
            assert!(
                !token.contains('/') && !token.contains('@') && !token.contains(char::is_whitespace),
                "{reason:?} renders {token:?}, which could carry a path / email / field split (#15)"
            );
            assert!(
                !seen.contains(&token),
                "{reason:?} reuses the token {token:?} — two causes would merge in the log"
            );
            seen.push(token);
        }
    }

    #[test]
    fn the_resolved_binary_line_is_its_own_event_carrying_the_absolute_path() {
        // T8 / issue #786 AC6: the resolved PATH rides a DISTINCT event, never folded into
        // `reason=`. That separation is what lets each be judged against #15 on its own terms —
        // `reason=` keeps its fixed-token guarantee, and the path is evaluated as what it is, a
        // filesystem location rather than a credential.
        let line = Event::RefreshBinaryResolved {
            path: PathBuf::from("/opt/homebrew/bin/claude"),
        }
        .to_log_line(at_epoch(0));
        assert_eq!(
            line,
            format!("{TS0} event=refresh_binary_resolved path=/opt/homebrew/bin/claude")
        );
        assert!(
            !line.contains("reason="),
            "the path must never ride `reason=`: {line}"
        );

        // And the `reason=unresolved` line stays free of the path, from the other direction.
        let unresolved = Event::Refresh {
            account: "spare".to_owned(),
            outcome: RefreshEventOutcome::Error,
            expires_before: None,
            expires_after: None,
            reason: Some(RefreshEventReason::Unresolved),
            backoff_secs: None,
        }
        .to_log_line(at_epoch(0));
        assert_eq!(
            unresolved,
            format!("{TS0} event=refresh account=spare outcome=error reason=unresolved")
        );
        assert!(
            !unresolved.contains('/'),
            "no path may reach the classification line: {unresolved}"
        );
    }

    #[test]
    fn a_resolved_path_never_splits_the_key_val_grammar() {
        // The grammar guard. A path is the only free-form value on this channel and may legally
        // contain a space, while `reliability::parse_events` tokenizes on whitespace and states
        // that values are whitespace-free. Percent-encoding is what keeps that true, and this
        // pins it against the two adversarial shapes — a plain space, and the injection a
        // trailing-but-raw path would still allow — with an ordinary path as the control.
        let plain = Event::RefreshBinaryResolved {
            path: PathBuf::from("/opt/homebrew/bin/claude"),
        }
        .to_log_line(at_epoch(0));

        let spaced = Event::RefreshBinaryResolved {
            path: PathBuf::from("/Users/o/My Tools/claude"),
        }
        .to_log_line(at_epoch(0));
        assert_eq!(
            spaced,
            format!("{TS0} event=refresh_binary_resolved path=/Users/o/My%20Tools/claude")
        );

        // The injection a raw path would permit: a directory whose NAME carries ` event=swap `.
        // Rendered raw this would tokenize into a real `event=swap` field and OVERWRITE the
        // line's own `event`, sending it through `reliability::parse_events`' swap arm.
        let hostile = Event::RefreshBinaryResolved {
            path: PathBuf::from("/tmp/x event=swap from=a to=b/claude"),
        }
        .to_log_line(at_epoch(0));
        for line in [&plain, &spaced, &hostile] {
            let fields: Vec<(&str, &str)> = line
                .split_whitespace()
                .filter_map(|token| token.split_once('='))
                .collect();
            assert_eq!(
                fields.len(),
                3,
                "exactly ts/event/path — no injected field: {line}"
            );
            assert_eq!(fields[1], ("event", "refresh_binary_resolved"));
            assert_eq!(fields[2].0, "path");
            assert!(
                !line
                    .rsplit_once("path=")
                    .expect("the line carries a path")
                    .1
                    .contains(char::is_whitespace),
                "the rendered value must be whitespace-free: {line}"
            );
        }
    }

    #[test]
    fn path_encoding_is_reversible_and_leaves_ordinary_paths_alone() {
        // `%` is encoded too, so an encoded space is never confusable with a literal `%20` that
        // was already in the path — the property that makes the rendering decodable rather than
        // merely safe. Ordinary paths (the overwhelming majority) pass through untouched, which
        // is what keeps `grep path=` and copy-paste working.
        assert_eq!(
            path_value(Path::new("/usr/local/bin/claude")),
            "/usr/local/bin/claude"
        );
        assert_eq!(path_value(Path::new("/a b/claude")), "/a%20b/claude");
        assert_eq!(path_value(Path::new("/a%20b/claude")), "/a%2520b/claude");
        assert_eq!(path_value(Path::new("/a\tb/claude")), "/a%09b/claude");
        // Multi-byte whitespace is encoded per UTF-8 byte (U+00A0 → C2 A0).
        assert_eq!(
            path_value(Path::new("/a\u{a0}b/claude")),
            "/a%C2%A0b/claude"
        );
        // An `=` needs no encoding: the value is a single token, and `split_once('=')` splits at
        // the FIRST `=`, so the key stays `path` and the rest is the value verbatim.
        assert_eq!(path_value(Path::new("/a=b/claude")), "/a=b/claude");
    }

    // --- Record integrity (issue #1092) -------------------------------------

    /// A control-bearing handle carrying every class of control character that could end a
    /// record: `\n` (the actual separator), `\r`, `\t`, NUL, ESC, DEL, and NEL — the one C1
    /// character some readers also treat as a line break, and the one that proves the encoding is
    /// defined on BYTES rather than chars.
    ///
    /// Deliberately SPACE-FREE, so the round-trip assertions below can be exact. A space in a
    /// handle splits the line into extra FIELDS — a real, pre-existing, and deliberately
    /// out-of-scope concern (issue #1092 explicitly declines to constrain `label`, and the module
    /// docs record that field-splitting stays the #15 meter's business). `FORGED_RECORD` below
    /// carries spaces and is asserted on the property that IS in scope: the record count.
    const HOSTILE_HANDLE: &str = "a\n\r\t\0\u{1b}\u{7f}\u{85}b";

    /// The issue's own failure scenario: a handle whose newline is followed by text spelled to
    /// look exactly like a well-formed event, so the injected line would be indistinguishable
    /// from a real one to any line-oriented reader.
    const FORGED_RECORD: &str = "u-1\nts=1970-01-01T00:00:00Z event=login outcome=onboarded";

    /// Decode `%XX` triples back to bytes — the inverse of [`single_line`] over a value known to
    /// contain no literal `%` of its own.
    ///
    /// Deliberately a test-local re-implementation rather than a shipped decoder: the log has no
    /// decoding consumer (readers are byte-faithful), so a production one would be dead code, and
    /// asserting a value *round-trips* is stronger than asserting it merely "looks escaped" — an
    /// assertion that a rejected-upstream value would also satisfy.
    fn percent_decode(value: &str) -> String {
        let raw = value.as_bytes();
        let mut out: Vec<u8> = Vec::with_capacity(raw.len());
        let mut i = 0;
        while i < raw.len() {
            if raw[i] == b'%' && i + 2 < raw.len() {
                let hex = std::str::from_utf8(&raw[i + 1..i + 3]).expect("ASCII hex");
                out.push(u8::from_str_radix(hex, 16).expect("a `%XX` triple"));
                i += 3;
            } else {
                out.push(raw[i]);
                i += 1;
            }
        }
        String::from_utf8(out).expect("decoding a per-UTF-8-byte encoding yields UTF-8")
    }

    /// The value of `key` on a rendered line, tokenized exactly as the shipped readers do
    /// (`crate::log`'s `field`, `crate::reliability`'s fold): split on whitespace, then at the
    /// token's FIRST `=`.
    fn field_of<'a>(line: &'a str, key: &str) -> Option<&'a str> {
        line.split_whitespace()
            .filter_map(|token| token.split_once('='))
            .find(|(k, _)| *k == key)
            .map(|(_, value)| value)
    }

    #[test]
    fn a_rendered_line_encodes_control_characters_and_nothing_else() {
        // Issue #1092, both halves. The first is the guarantee; the second is its canary — an
        // over-broad scrub would satisfy "no control character survives" while REFORMATTING every
        // durable record ever written, which the frozen grammar (`crate::log`) forbids.
        assert_eq!(single_line("a\nb".to_owned()), "a%0Ab");
        assert_eq!(single_line("a\rb".to_owned()), "a%0Db");
        assert_eq!(single_line("a\tb".to_owned()), "a%09b");
        assert_eq!(single_line("a\0b".to_owned()), "a%00b");
        assert_eq!(single_line("a\u{1b}[31mb".to_owned()), "a%1B[31mb");
        assert_eq!(single_line("a\u{7f}b".to_owned()), "a%7Fb");
        // NEL (U+0085) is two UTF-8 bytes, and encoding is defined on bytes.
        assert_eq!(single_line("a\u{85}b".to_owned()), "a%C2%85b");

        // Untouched, deliberately — each of these appears on real lines today and must render
        // byte-for-byte as it always has: `%` (which `path_value` already emits), a space and an
        // `=` (field-splitting, not record-splitting), and a non-control non-ASCII label.
        for unchanged in [
            "ts=1970-01-01T00:00:00Z event=refresh_binary_resolved path=/a%20b/claude",
            "ts=1970-01-01T00:00:00Z event=swap from=my work to=spare reason=session",
            "ts=1970-01-01T00:00:00Z event=restash account=r\u{e9}sum\u{e9}",
        ] {
            assert_eq!(single_line(unchanged.to_owned()), unchanged);
        }
    }

    #[test]
    fn no_free_form_field_can_split_an_event_into_a_second_record() {
        // Issue #1092 AC: a control byte in ANY value that reaches a line — a harvested
        // `account_uuid` or a free-form operator `label` — must not end the record early.
        //
        // Every distinct free-form field POSITION the `Event` grammar has, one entry each. It is
        // a list rather than a sweep over `every_event_variant` because that corpus is clean by
        // design (it is the #15 redaction sweep's subject), which is exactly why its own
        // `lines().count() == 1` assertion passes vacuously and this one does not.
        //
        // Totality here does not rest on the list being complete: `to_log_line`'s arms are a
        // match EXPRESSION with a single exit, so every variant — including ones added after
        // this test — leaves through the same `single_line`. The list pins that the wiring is
        // real and that each field's value stays RECOVERABLE, not merely mangled.
        let h = || HOSTILE_HANDLE.to_owned();
        let cases: Vec<(&str, Event, &[&str])> = vec![
            (
                "swap.from/to (operator labels)",
                Event::Swap {
                    from: h(),
                    to: h(),
                    reason: SwapReason::Session,
                    session_pct: 97,
                    projection: None,
                },
                &["from", "to"],
            ),
            (
                "emergency_swap.from/to",
                Event::EmergencySwap { from: h(), to: h() },
                &["from", "to"],
            ),
            (
                "restash.account",
                Event::ReStash { account: h() },
                &["account"],
            ),
            (
                "all_exhausted.hold",
                Event::AllExhausted {
                    hold: h(),
                    cause: SwapReason::Weekly,
                    resets_at: Some(1_782_777_600),
                },
                &["hold"],
            ),
            (
                "active_dead_no_target.hold",
                Event::ActiveDeadNoTarget {
                    hold: h(),
                    cause: SwapReason::Weekly,
                    resets_at: None,
                },
                &["hold"],
            ),
            (
                "canary_drift.displayed/matched",
                Event::CanaryDrift {
                    displayed: h(),
                    matched: h(),
                    overridden: false,
                },
                &["displayed", "matched"],
            ),
            (
                "canonical_scrubbed.account (optional)",
                Event::CanonicalScrubbed { account: Some(h()) },
                &["account"],
            ),
            (
                "credential_health.account",
                Event::CredentialHealth {
                    account: h(),
                    state: CredentialHealth::Dead,
                },
                &["account"],
            ),
            (
                "refresh.account",
                Event::Refresh {
                    account: h(),
                    outcome: RefreshEventOutcome::Dead,
                    expires_before: None,
                    expires_after: None,
                    reason: None,
                    backoff_secs: None,
                },
                &["account"],
            ),
            // The issue's named path: `login` reports the uuid harvested from `~/.claude.json`
            // when the reconcile FAILS — including when it failed *because* the roster's
            // `[A-Za-z0-9_-]{1,128}` gate (issue #1052) rejected that very uuid.
            (
                "login.account (the harvested uuid, failure path)",
                Event::Login {
                    account: Some(h()),
                    outcome: LoginEventOutcome::Failed,
                },
                &["account"],
            ),
            (
                "capture.account",
                Event::Capture {
                    account: Some(h()),
                    outcome: CaptureEventOutcome::Failed,
                },
                &["account"],
            ),
            (
                "uncaptured_login.acct (optional uuid)",
                Event::UncapturedLogin {
                    account_uuid: Some(h()),
                },
                &["acct"],
            ),
            (
                "usage_gap.acct",
                Event::UsageGap {
                    account: h(),
                    since: 1_782_777_600,
                },
                &["acct"],
            ),
            (
                "usage_backoff.acct",
                Event::UsageBackoff {
                    account: h(),
                    class: BackoffClass::RateLimited,
                    consecutive: 2,
                    retry_after_secs: Some(60),
                    backoff_secs: 120,
                },
                &["acct"],
            ),
            (
                "credential_expiry_observed.acct",
                Event::CredentialExpiryObserved {
                    account: h(),
                    provenance: ExpiryProvenance::MyRefresh,
                    before: Some(1_782_777_000),
                    after: 1_782_777_600,
                    grant_replaced: Some(true),
                },
                &["acct"],
            ),
            // The one value already encoded at FIELD level (issue #786). It must stay single-line
            // WITHOUT being encoded twice — `path_value` turns the newline into `%0A` first, and
            // `single_line` must then find nothing left to do.
            (
                "refresh_binary_resolved.path (pre-encoded)",
                Event::RefreshBinaryResolved {
                    path: PathBuf::from("/a\nb/claude"),
                },
                &["path"],
            ),
        ];

        for (name, event, keys) in &cases {
            let line = event.to_log_line(at_epoch(0));
            assert_eq!(
                line.lines().count(),
                1,
                "{name}: a control byte must not open a second record: {line:?}"
            );
            assert!(
                !line.contains(char::is_control),
                "{name}: no control character survives: {line:?}"
            );
            for key in *keys {
                let value = field_of(&line, key)
                    .unwrap_or_else(|| panic!("{name}: the line carries `{key}=`: {line:?}"));
                let expected = if *key == "path" {
                    // `path_value` also encodes `%` and whitespace, so this value decodes to the
                    // raw path — the assertion that matters is that it was encoded ONCE.
                    "/a\nb/claude".to_owned()
                } else {
                    HOSTILE_HANDLE.to_owned()
                };
                assert_eq!(
                    percent_decode(value),
                    expected,
                    "{name}: `{key}=` must stay recoverable, not merely escaped: {line:?}"
                );
            }
        }
    }

    #[test]
    fn a_login_failure_planting_a_forged_record_appends_exactly_one_line() {
        // Issue #1092's acceptance criterion, driven through the REAL sink to a real file rather
        // than asserted on a formatted string: the login-failure path is the one that logs an
        // `account_uuid` harvested from `~/.claude.json` — checked for non-emptiness only — and it
        // is reached *by* the roster gate rejecting that uuid, so this value is not hypothetical.
        //
        // The assertion is the AC's own: the log gains EXACTLY ONE record. Asserting the output
        // "looks escaped" would also pass if the value never reached the line at all — the
        // round-trip below is what rules that out.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sessiometer.log");
        let mut log = EventLog::at(&path).unwrap();

        log.emit(&Event::Login {
            account: Some(FORGED_RECORD.to_owned()),
            outcome: LoginEventOutcome::Failed,
        })
        .unwrap();

        let logged = std::fs::read_to_string(&path).unwrap();
        assert_eq!(
            logged.lines().count(),
            1,
            "the log gains exactly one record: {logged:?}"
        );
        // Not merely one line: exactly one line NAMES an event, so nothing that reads the file
        // by kind can see a second, forged `event=login`.
        assert_eq!(
            logged.lines().filter(|l| l.contains(" event=")).count(),
            1,
            "exactly one line names an event: {logged:?}"
        );
        let line = logged.lines().next().unwrap();
        assert!(
            line.ends_with(" outcome=failed"),
            "the real outcome: {line}"
        );
        // The forged text is still THERE — it was neutralized, not dropped — and `account=`
        // recovers it up to the first SPACE, since a space is what ends a token in this grammar
        // and this fix deliberately leaves spaces alone (see `HOSTILE_HANDLE`). The newline that
        // used to end the RECORD now sits inside the value, recoverable.
        let account = field_of(line, "account").expect("an account= field");
        assert_eq!(percent_decode(account), "u-1\nts=1970-01-01T00:00:00Z");
        assert!(
            logged.contains("outcome=onboarded"),
            "the planted text is preserved verbatim-modulo-encoding, not silently discarded: \
             {logged:?}"
        );
    }

    #[test]
    fn a_hostile_diagnostic_handle_cannot_open_a_second_record() {
        // The sibling surface (issue #1092): `diag=` lines carry the same free-form operator
        // label and land in the file `sessiometer log --channel diag` reads. That channel is the
        // ungoverned one, but "ungoverned" bounds what may APPEAR on a line, not whether a value
        // may forge a second one.
        let line = Diagnostic::Poll {
            account: HOSTILE_HANDLE.to_owned(),
            outcome: PollClass::Live,
        }
        .to_log_line(at_epoch(0));
        assert_eq!(line.lines().count(), 1, "single record: {line:?}");
        assert_eq!(
            percent_decode(field_of(&line, "account").expect("an account= field")),
            HOSTILE_HANDLE
        );
    }

    #[test]
    fn poll_refresh_line_carries_handle_trigger_and_outcome() {
        // Issue #255: the #162 poll-refresh ACTION renders the redacted handle, the trigger, and
        // the outcome token — the SAME vocabulary `event=refresh` uses, under a DISTINCT
        // `poll_refresh` event name (so it never collides with the periodic #106 refresh line the
        // `list` view's `last_refresh_outcomes` reader parses).
        let dead = Event::PollRefresh {
            account: "spare".to_owned(),
            trigger: PollRefreshTrigger::Poll401,
            outcome: RefreshEventOutcome::Dead,
        }
        .to_log_line(at_epoch(0));
        assert_eq!(
            dead,
            format!("{TS0} event=poll_refresh account=spare trigger=poll_401 outcome=dead")
        );
        // The `outcome=` token tracks the variant (the shared refresh vocabulary); `rotated=`
        // trails it (issue #279), and no expiry fields ride this line (unlike `event=refresh`).
        let refreshed = Event::PollRefresh {
            account: "spare".to_owned(),
            trigger: PollRefreshTrigger::Poll401,
            outcome: RefreshEventOutcome::Refreshed { rotated: true },
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
    fn poll_refresh_trigger_renders_the_recovery_origin_distinctly() {
        // Issue #1367: the parked recovery re-probe (#643) shares this event with the reactive
        // #162 poll path, so the ONLY thing separating them on the durable line is `trigger=`.
        // Pinned as a whole line, both origins side by side, because the defect this replaced was
        // precisely that the two rendered byte-identically.
        let poll_401 = Event::PollRefresh {
            account: "spare".to_owned(),
            trigger: PollRefreshTrigger::Poll401,
            outcome: RefreshEventOutcome::Dead,
        }
        .to_log_line(at_epoch(0));
        let recovery = Event::PollRefresh {
            account: "spare".to_owned(),
            trigger: PollRefreshTrigger::Recovery,
            outcome: RefreshEventOutcome::Dead,
        }
        .to_log_line(at_epoch(0));
        assert_eq!(
            recovery,
            format!("{TS0} event=poll_refresh account=spare trigger=recovery outcome=dead")
        );
        assert_ne!(
            poll_401, recovery,
            "the two origins must not render the same line"
        );
        // The `recovery` token is deliberately the one `event=keep_warm` already renders for the
        // ACTIVE half of the same #643 fix, so one grep finds both halves. The EVENT NAME is what
        // keeps them apart — which is also how `reliability` buckets them.
        let active_half = Event::KeepWarm {
            account: "spare".to_owned(),
            trigger: KeepWarmTrigger::Recovery,
            outcome: RefreshEventOutcome::Dead,
        }
        .to_log_line(at_epoch(0));
        assert!(
            active_half.contains(" trigger=recovery "),
            "got: {active_half}"
        );
        assert!(
            active_half.contains(" event=keep_warm "),
            "got: {active_half}"
        );
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
            assert!(
                crate::redaction::meter::unauthored_emails(line.as_str(), &[]).is_empty(),
                "no non-authored email may appear (#15/#444): {line}"
            );
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
        assert!(
            crate::redaction::meter::unauthored_emails(&with_uuid, &[]).is_empty(),
            "no non-authored email may appear (#15/#444): {with_uuid}"
        );
        assert!(
            !with_uuid.contains("token"),
            "no token may appear: {with_uuid}"
        );
    }

    #[test]
    fn refresh_renders_each_outcome_token() {
        // Issue #1004: `rotated=` is rendered ONLY where the outcome can carry one. The two
        // refreshed arms spell it; `no_change` / `dead` / `error` end at the outcome token.
        // This is the emitted-side half of the guarantee — the type-side half is that the
        // three bare variants below have no field to supply, so no future edit to this
        // formatter can put the value back without first changing the enum.
        for (outcome, token, rotated) in [
            (
                RefreshEventOutcome::Refreshed { rotated: true },
                "refreshed",
                " rotated=true",
            ),
            (
                RefreshEventOutcome::Refreshed { rotated: false },
                "refreshed",
                " rotated=false",
            ),
            (
                RefreshEventOutcome::RefreshedNotReStashed { rotated: true },
                "refreshed_not_restashed",
                " rotated=true",
            ),
            (RefreshEventOutcome::NoChange, "no_change", ""),
            (RefreshEventOutcome::Dead, "dead", ""),
            (RefreshEventOutcome::Error, "error", ""),
        ] {
            let line = Event::Refresh {
                account: "work".to_owned(),
                outcome,
                expires_before: None,
                expires_after: None,
                reason: None, // outcome-token render only — the #377 reason has its own test
                backoff_secs: None, // and no back-off — the #408 field has its own test
            }
            .to_log_line(at_epoch(0));
            assert_eq!(
                line,
                format!("{TS0} event=refresh account=work outcome={token}{rotated}")
            );
        }
    }

    /// Issue #1004, across all THREE refresh-family lines: a non-refreshed outcome renders no
    /// `rotated=` at all — not `rotated=false`.
    ///
    /// The 2026-07-31 incident was a `dead` cycle asserting `rotated=true` while its own
    /// `window_secs=0` proved no exchange had occurred. The daemon's own log holds five such
    /// lines, all on `keep_warm` / `poll_refresh` — which render no expiry fields at all, so
    /// nothing on the line contradicted the claim. `poll_refresh` and `keep_warm` are covered
    /// here alongside `refresh` because they share the rule but not the formatter branch.
    #[test]
    fn a_non_refreshed_outcome_renders_no_rotation_field_on_any_refresh_family_line() {
        for outcome in [
            RefreshEventOutcome::NoChange,
            RefreshEventOutcome::Dead,
            RefreshEventOutcome::Error,
        ] {
            let lines = [
                Event::Refresh {
                    account: "work".to_owned(),
                    outcome,
                    expires_before: Some(1_000_000),
                    expires_after: Some(1_000_000),
                    reason: None,
                    backoff_secs: None,
                }
                .to_log_line(at_epoch(0)),
                Event::PollRefresh {
                    account: "work".to_owned(),
                    trigger: PollRefreshTrigger::Poll401,
                    outcome,
                }
                .to_log_line(at_epoch(0)),
                Event::KeepWarm {
                    account: "work".to_owned(),
                    trigger: KeepWarmTrigger::Reactive,
                    outcome,
                }
                .to_log_line(at_epoch(0)),
            ];
            for line in lines {
                assert!(
                    !line.contains("rotated="),
                    "outcome={} must render no rotation claim, got: {line}",
                    outcome.as_str()
                );
            }
        }
    }

    // --- issue #15 redaction sweep: mechanically total variant coverage (issue #891) ------
    //
    // The two sweeps below render every `Event` / `Diagnostic` variant and scan the line for
    // an email or token sigil. WHICH variants they scan is enforced, not curated. A
    // hand-listed array lets a new payload-carrying variant ship with ZERO redaction coverage
    // while every test stays green — and it had: the `Event` array covered 18 of 51 variants
    // when issue #891 was filed. Two layers close that.
    //
    //   LAYER 1, at COMPILE TIME. `event_variant_name` / `diagnostic_variant_name` match
    //   exhaustively over their enum, so adding a variant does not compile until it is named
    //   there — a compile error rather than a runtime assertion. Both helpers are `#[cfg(test)]`,
    //   so that error surfaces when the TEST target is built (`cargo test`, including
    //   `--no-run`); a bare `cargo build` satisfies only the production `to_log_line` match and
    //   will not raise it.
    //
    //   LAYER 2, at TEST TIME. Layer 1 forces an ARM, not a SAMPLE — a variant named in the
    //   match but missing from the sample list would still never be rendered, so the hole
    //   would reopen one step later. `every_event_variant` / `every_diagnostic_variant`
    //   therefore assert that the set of variants they sample EQUALS the set the enum
    //   declares, read out of this file by `declared_variant_names` (the source-scan idiom
    //   the `usage.rs` egress meta-tests already use in this crate).
    //
    // Together these prove RENDERING, not merely absence of omission: every declared variant
    // is shown to reach `to_log_line` and be scanned. More than one sample per variant is
    // welcome — several below carry two, to exercise optional-field renderings — because the
    // assertion is on the SET of variants covered, never on a sample count.

    /// Split one enum-body line on the commas that separate VARIANTS, ignoring commas nested
    /// inside a variant's own `{ … }` / `( … )` body.
    ///
    /// The job it does on EVERY line is stripping the trailing comma, and that is what makes a
    /// FIELDLESS variant visible at all: `AllExhaustedCleared,` yields `AllExhaustedCleared` —
    /// an identifier with nothing after it, which is one of the remainders the
    /// `declares_variant` predicate inside `variant_names_in` accepts. Drop the call and the
    /// seven bare-token `Event` variants and `Diagnostic::Stop` go undeclared. Today that fails
    /// loudly, because they are all sampled; but a bare-token variant added LATER would be
    /// missing from BOTH sets at once, and the sweep would pass over it in silence — which is
    /// precisely the hole issue #891 closes.
    ///
    /// Separating variants that share a line is the secondary job: `A, B,` yields `A` and `B`,
    /// while `Monitor401 { account: String, consecutive: u32 },` yields the single whole
    /// declaration, because its inner comma sits at nesting depth 1. `cargo fmt` gives every
    /// variant its own line, so that case guards against future packing rather than answering a
    /// live need — under-matching being the silently-passing direction either way.
    fn split_top_level(line: &str) -> Vec<&str> {
        let (mut segments, mut start, mut nesting) = (Vec::new(), 0, 0i32);
        for (offset, ch) in line.char_indices() {
            match ch {
                '{' | '(' => nesting += 1,
                '}' | ')' => nesting -= 1,
                ',' if nesting == 0 => {
                    segments.push(line[start..offset].trim());
                    start = offset + 1;
                }
                _ => {}
            }
        }
        segments.push(line[start..].trim());
        segments.into_iter().filter(|s| !s.is_empty()).collect()
    }

    /// Step over the attributes a variant may carry AHEAD OF ITS NAME, so the identifier run
    /// `declared_variant_names` takes below starts where the name starts.
    ///
    /// `#[allow(dead_code)] Scheduled,` arrives as one segment beginning with `#`, so that run is
    /// EMPTY, the variant never enters `declared`, and layer 2 passes VACUOUSLY for it — the
    /// silent direction this scan exists to remove. Issue #1397.
    ///
    /// The attribute on its OWN line already parsed, and still does: its segment yields no name
    /// and is discarded, the variant's yields the name. `cargo fmt` rewrites the same-line
    /// spelling into exactly that two-line form, which is why issue #1397 rated this latent —
    /// but `fmt` is not the backstop it looks like, and the reason is worth stating rather than
    /// re-discovering. Measured on that issue, in this tree: `#[rustfmt::skip]` on the enum
    /// carries the same-line spelling through `cargo fmt --all --check` at exit 0, and the
    /// variant so declared stayed out of `declared` while `cargo test` reported `0 failed` over
    /// the whole suite — the consuming guard passing vacuously, nothing else catching it. A
    /// silent-failure mode closed by a formatter is closed only until someone writes the one
    /// attribute that turns the formatter off, in band, for a reason having nothing to do with
    /// this scan. So the shape is parsed here.
    ///
    /// A BRACE inside such an attribute defeats the same-line spelling even so, and that is
    /// [`split_top_level`]'s raw `{` count rather than this function's: the nesting never
    /// returns to 0, so the trailing comma does not split and the segment handed back is
    /// `Scheduled,`, whose remainder `,` the `declares_variant` predicate refuses. Measured on
    /// issue #1428 and left there — that issue is the DEPTH walk, which [`code_brace_counts`]
    /// reports as unmoved here — filed as issue #1437, and pinned as a drop in
    /// `the_variant_scan_reads_the_shapes_its_parser_names` so this entry stays answerable.
    ///
    /// Bracket DEPTH is tracked rather than seeking the first `]`, so a nested one does not end
    /// the attribute early. An unbalanced run returns the segment untouched: it then still opens
    /// with `#`, yields no name, and the variant stays out of `declared` exactly as it did
    /// before this function existed. That is the pre-existing under-match, never a wrong name —
    /// the one direction a parser here must not invent.
    fn strip_leading_attributes(segment: &str) -> &str {
        let mut remainder = segment;
        while let Some(after_hash) = remainder.strip_prefix('#') {
            // Not `#[`: an inner `#![…]`, or something that is not an attribute at all. Either
            // way this is not a variant declaration, so hand back the segment unchanged.
            let Some(body) = after_hash.strip_prefix('[') else {
                return segment;
            };
            let mut depth = 1i32;
            let mut close = None;
            for (offset, ch) in body.char_indices() {
                match ch {
                    '[' => depth += 1,
                    ']' => {
                        depth -= 1;
                        if depth == 0 {
                            close = Some(offset);
                            break;
                        }
                    }
                    _ => {}
                }
            }
            let Some(close) = close else { return segment };
            remainder = body[close + 1..].trim_start();
        }
        remainder
    }

    /// The `{` and `}` counts on `line` that are CODE, skipping the ones a string literal or a
    /// line comment carries.
    ///
    /// The depth walk in [`variant_names_in`] decides which lines sit DIRECTLY inside the enum
    /// body, and it decides it by counting braces. A brace that is not a block delimiter moves
    /// that counter exactly as a real one does, and nothing downstream can tell them apart:
    /// measured on issue #1428, against the walk as it stood, `#[doc = "{"]` above an ordinary
    /// variant takes the depth from 1 to 2, so the variant reads as a FIELD and scans to
    /// `{"Anchor"}` — dropped, and dropped in SILENCE, because the only guard against a vacuous
    /// pass is a hard failure on an EMPTY parse and a set short of one variant satisfies it.
    /// `#[doc = "}"]` is the same defect mirrored: the depth reaches 0 and the walk leaves the
    /// enum body a line early, measured to the same `{"Anchor"}`.
    ///
    /// COMMENTS were already the known carrier — [`variant_names_in`] drops whole-line ones
    /// ahead of the walk, saying so — but that filter reads the START of a line, so a trailing
    /// comment reached the counter intact: `Gated, // {` measured to swallow the variant on the
    /// next line. Counting here rather than filtering there covers both, and leaves that filter
    /// the two jobs only it can do, enumerated where it stands: keeping a comment that happens
    /// to end in `enum Foo {` from being taken for the header, and keeping its COMMAS away from
    /// [`split_top_level`], which splits on them whatever they sit inside — measured,
    /// `// Covers A, B` above `Gated,` scans to `{"Anchor", "B", "Gated"}` without it, and this
    /// counter cannot prevent that, returning `(0, 0)` for such a line.
    ///
    /// Carriers left UNSEEN are catalogued above [`declared_variant_names`], pinned in
    /// `the_variant_scan_reads_the_shapes_its_parser_names` and filed as issue #1433. Some are
    /// not merely unseen but REGRESSED — inert to the walk as it stood, which counted the real
    /// brace beside them, and live to this one. Every arm here that SKIPS can do that: the
    /// line-comment arm as readily as the two string arms, needing only a real brace BEHIND the
    /// skip on the same line. Measured instances, pinned below and measured against both walks:
    /// a quote inside a BLOCK comment, a quote inside a CHAR literal, and a `//` inside a block
    /// comment — an ordinary URL is enough for the last. Deliberately NOT stated as a closed
    /// class: a claim about the complement is not something a pin over enumerated shapes can
    /// back. No enum this scan reads carries any of it, so nothing is under-swept today.
    ///
    /// The catalogued carriers are a BLOCK comment, a CHAR literal, and a string spanning lines
    /// with the brace on a CONTINUATION line — a brace on such a string's opening line is
    /// already skipped, the unterminated quote swallowing the rest of that line. The comment and
    /// the wrapped string need state carried BETWEEN lines, which this walk does not have; the
    /// char literal needs `'{'` told from the `'a` of a lifetime, a rule the string arms do not
    /// need.
    ///
    /// NEITHER direction is safe, and that is worth stating because the skipping one reads as
    /// though it were. Over-COUNTING is the silence above: the depth runs high and the variant
    /// behind the brace reads as a field. Over-SKIPPING depends on the BODY it skips into, and
    /// both cases are PINNED rather than argued, because prose about this direction has already
    /// been wrong in both directions. A missed OPENING brace reads the field lines below it at
    /// variant depth. A field type fitting ONE line is refused silently — `note: String` on its
    /// remainder, which begins `:` where `declares_variant` admits only empty, `{`, `(` or `=`.
    /// A WRAPPED type is LOUD: `cargo fmt` breaks a long one across lines, and a continuation
    /// line is a bare uppercase identifier whose comma leaves an EMPTY remainder, so `Vec<` /
    /// `String,` / `>,` puts `String` in the set — a name nobody wrote, which fails in
    /// `assert_samples_every_variant`. Loud is not SAVED: either way that missed brace leaves
    /// its `}` counted against nothing, the depth reaches 0 on the variant's own closing line,
    /// and the walk BREAKS out of the body, dropping the variant after it.
    /// Measured on issue #1428: `/* " */ Wrapped {`, where the quote inside the block comment
    /// opens a string that swallows the real `{`, scans over a `note: String` body to
    /// `{"Anchor"}` — `After` gone, nothing failing — where the same body with that brace
    /// counted scans to `{"Anchor", "After"}`. `Wrapped` is absent from BOTH, and separating
    /// that out is what keeps this measurement answerable: the block comment sits ahead of its
    /// NAME, so the identifier run starts at `/` and yields nothing whatever the depth walk
    /// does — a name-run drop, not the depth one this paragraph is about. Pinned in
    /// `the_variant_scan_reads_the_shapes_its_parser_names` beside the carrier that reaches it.
    /// So a new arm is answerable in BOTH directions rather than waved through on a safe one.
    ///
    /// One shape moves from a silent wrong answer to a LOUD one, and it is worth naming because
    /// it reads like a regression: `depth` is `usize`, so a line whose CODE closes exceed it
    /// underflows. That path is not new — a bare `}}` line at depth 1 panics on `attempt to
    /// subtract with overflow` against either walk — but a `{` a string or a comment carries no
    /// longer offsets a real `}` beside it, so `#[doc = "{"]}}` above a variant reaches it where
    /// the walk as it stood balanced that line to depth 0 and scanned to `{"Anchor"}`. Malformed
    /// Rust in both readings, and loud is the direction that gets diagnosed.
    fn code_brace_counts(line: &str) -> (usize, usize) {
        let (mut opens, mut closes) = (0usize, 0usize);
        let bytes = line.as_bytes();
        let mut i = 0usize;
        while i < bytes.len() {
            match bytes[i] {
                b'{' => {
                    opens += 1;
                    i += 1;
                }
                b'}' => {
                    closes += 1;
                    i += 1;
                }
                // A line comment runs to the end of the line, so nothing past it is code.
                b'/' if bytes.get(i + 1) == Some(&b'/') => break,
                // A raw string: `r"…"`, or `r#"…"#` with any run of hashes, which is what fixes
                // its terminator — no escape applies inside one. `br"…"` arrives here too, the
                // `b` having fallen through the default arm. A raw IDENTIFIER does not: the run
                // of hashes must be followed by a quote, and `r#type` has a `t` there.
                b'r' => {
                    let mut hashes = 0;
                    while bytes.get(i + 1 + hashes) == Some(&b'#') {
                        hashes += 1;
                    }
                    if bytes.get(i + 1 + hashes) != Some(&b'"') {
                        i += 1;
                        continue;
                    }
                    i += 2 + hashes;
                    while i < bytes.len() {
                        let terminates = bytes[i] == b'"'
                            && bytes.len() >= i + 1 + hashes
                            && bytes[i + 1..i + 1 + hashes].iter().all(|b| *b == b'#');
                        if terminates {
                            i += 1 + hashes;
                            break;
                        }
                        i += 1;
                    }
                }
                // An ordinary string, `b"…"` included on the same reasoning as `br"…"` above.
                // A backslash escapes the byte after it, so an escaped quote does not end it.
                b'"' => {
                    i += 1;
                    while i < bytes.len() && bytes[i] != b'"' {
                        i += if bytes[i] == b'\\' { 2 } else { 1 };
                    }
                    i += 1;
                }
                // Byte-wise is safe for the rest: every byte of a multi-byte character is
                // greater than any ASCII one, so none of the arms above can match inside one,
                // and no `&str` is sliced at these offsets.
                _ => i += 1,
            }
        }
        (opens, closes)
    }

    /// The variant names declared by `enum {enum_name}` in this file, parsed from its source.
    ///
    /// Anchored at `CARGO_MANIFEST_DIR`, so the scan is tied to the crate under test whatever
    /// the test CWD — the same idiom as the `usage.rs` egress meta-tests.
    ///
    /// `pub(crate)` because the shape it guards is not confined to the sweeps below: any
    /// hand-maintained list of variants has the same hole, and the fix is the same scan. Issue
    /// #1386 is the second consumer — `crate::reliability`'s
    /// `poll_refresh_trigger_tokens_all_reach_a_named_bucket`, which replays every
    /// [`PollRefreshTrigger`] through the emitter and had no way to know its list was short. This
    /// follows `crate::error::tests::declared_variant_names`, published `pub(crate)` for
    /// `crate::daemon`'s redaction meter on the same reasoning (issue #1085): what makes a walk
    /// worth publishing rather than re-writing per consumer is the scrutiny already pointed at
    /// THIS one — the two-layer note above, the empty-parse refusal below, and `split_top_level`'s
    /// under-matching rules. A private second copy would inherit none of it.
    ///
    /// The scan reads THIS file, so it answers for enums declared here and no others. That is not
    /// a limitation worth generalizing away on speculation: both live consumers name an enum from
    /// this file, and a caller naming one that is not here fails loudly rather than passing
    /// vacuously — the header lookup below refuses a declaration it cannot find, and the parse
    /// refuses a body it read nothing out of.
    ///
    /// This set is layer 2's ground truth, so where it could diverge from the COMPILER's variant
    /// set a variant would be forced to have a match arm but not a sample — reopening the very
    /// hole issue #891 closes, one level down. Divergence is therefore avoided in the parser
    /// rather than delegated to convention: identifiers admit `_` (so a non-camel-case variant
    /// is still seen, not silently dropped), a body line is split on its top-level commas (so
    /// `A, B,` on one line yields both), a segment's leading attributes are stepped over (so
    /// `#[allow(dead_code)] Scheduled,` yields `Scheduled` rather than nothing — issue #1397),
    /// and a name may be followed by an EXPLICIT DISCRIMINANT (so `Scheduled = 3,` yields
    /// `Scheduled` rather than nothing — issue #1410). Over-matching is safe — a name that is
    /// not really a variant lands in `declared`, goes unsampled, and FAILS loudly; it is
    /// under-matching that would pass quietly, and that is what the rules above remove.
    ///
    /// Safe is not free, though, and the discriminant arm is where that begins to bite — so the
    /// rule bounding it is stated here rather than left to be re-derived. An attribute spread
    /// over several LINES leaves its `key = value` continuations at depth 1 as well: the depth
    /// counter moves on BRACES, and the brackets and parens an attribute is built out of do not
    /// touch it. On the remainder test alone, such a continuation reads as a declaration. What
    /// rejects it is the ASCII-uppercase INITIAL, which a conventionally snake_case attribute
    /// key does not carry.
    /// Measured on issue #1410 over synthetic text, a three-line `#[cfg(all(…))]` whose middle
    /// line is `target_os = "macos",`, sitting above an ordinary variant: with the initial rule
    /// dropped the scan returns `target_os` beside the variant's own name, and with it kept it
    /// returns the variant alone. Still the loud direction — but loud about a name nobody wrote,
    /// on a spelling that has nothing to do with this scan. Relaxing that rule is therefore not
    /// free either, which is what the identifier-rule entry in the catalogue below turns on.
    ///
    /// Read that as a bound on the leading token's CASE rather than on the `key = value` FORM,
    /// because a key may be spelled with an uppercase initial and then the form does not save
    /// it. Measured the same way, `Bar = 1,` inside a multi-line `#[foo(…)]` scans to
    /// `{"Anchor", "Bar", "Gated"}` — and to `{"Anchor", "Gated"}` with the `=` arm removed and
    /// nothing else touched, so that one is a shape this arm makes reachable. A continuation
    /// whose remainder is empty or opens a body is read on the same rule and was already:
    /// `Foo,` inside a multi-line `#[cfg(any(…))]` carries the initial, takes the
    /// `rest.is_empty()` arm and measures `{"Anchor", "Foo", "Gated"}` on both sides of that
    /// removal. Both run in the loud direction, so they are recorded rather than closed; what a
    /// later reader must not inherit is the bound stated over the remainder form.
    ///
    /// The rules above remove the shapes they name, which is NOT the same as removing
    /// under-matching, and the difference is what a later consumer would otherwise inherit as a
    /// guarantee. The parser models a subset of Rust's variant grammar; a spelling outside that
    /// subset yields no name and passes vacuously, silently. THAT sentence is the bound. What
    /// follows is the list of shapes KNOWN to sit outside it — a record of what has been looked
    /// at, never a guarantee that whatever is absent from it must be unreachable or loud. A
    /// shape not listed is unclassified, not handled.
    ///
    /// A variant whose declaration is produced by a MACRO is not in this file's text at all. A
    /// `#[cfg(…)]`-gated variant is counted whether or not the compiler kept it, so a
    /// declared-but-compiled-out variant reads here as unsampled and fails LOUDLY — the safe
    /// direction, and the reason it is left. Both are out of `cargo fmt`'s reach in the same way
    /// `#[rustfmt::skip]` puts the same-line attribute above out of it, and neither is reachable
    /// in this crate today: no enum this scan reads is macro-generated or carries a `cfg` on a
    /// variant.
    ///
    /// A variant name the IDENTIFIER rule above does not admit joins them, and it is neither of
    /// those things: it is ordinary Rust, it needs no attribute, and this scan is silent about
    /// it. A RAW IDENTIFIER is the spelling of it a person would actually write. `r#type,` stops
    /// the run at `r` — lowercase, so the initial rejects it, and the remainder `#type` matches
    /// no arm either — so the variant stays out of `declared` exactly as an explicit
    /// discriminant did before issue #1410.
    ///
    /// It is left unhandled, and the obvious reason to give here does not survive measurement,
    /// so it is not given. Reading it means stepping over the `r#` and then admitting a name
    /// with no uppercase initial, which READS like the rule bounding the discriminant arm this
    /// scan now leans on. Scoped to the `r#` branch it is not that rule: measured on issue
    /// #1410, stripping the escape and admitting a lowercase name BEHIND IT ONLY reads `r#type,`
    /// as `type` while the multi-line `cfg` above still refuses `target_os`, and the only
    /// assertion that moves is the one pinning today's drop. The two shapes are separable, so
    /// deferring this one is a choice about scope rather than a cost the arm just added forces —
    /// and what follows is the backstop that choice leaves standing.
    ///
    /// What stands between that shape and a merged tree is a LINT rather than this parser:
    /// `non_camel_case_types` fires on `r#type,`, warn-by-default in `rustc` and an error under
    /// the `-D warnings` clippy gate this repo runs — measured on issue #1410, in this tree.
    /// That is a backstop of exactly the kind the same-line attribute above teaches not to lean
    /// on: one `#[allow(non_camel_case_types)]` switches it off in band, written by whoever
    /// wanted the keyword-shaped name to begin with.
    ///
    /// Other names outside the run land in this entry too, and they are not all lint-backed. A
    /// non-ASCII initial (`Ünicode,`) draws no NAMING lint at all — measured on issue #1410 with
    /// both spellings added to one enum and put through that same gate in a single run, where
    /// `non_camel_case_types` named `type` and said nothing whatever about `Ünicode` — and the
    /// run here is `is_ascii_alphanumeric`, so that one is dropped in silence with nothing else
    /// to catch it. Widening that predicate is deliberately not done here: it is the same
    /// `is_ascii_alphanumeric`-versus-Unicode question the sibling F3 finding raises about the
    /// two sides of the reliability comparison. That finding has no issue of its own — it is
    /// recorded in issue #1410's body, and the asymmetry itself is stated in band above
    /// `crate::reliability`'s own Unicode `is_alphanumeric` run — so moving one side of it from
    /// inside this issue would answer it blind.
    ///
    /// The last entry is not about a NAME at all: it is a variant this scan never reaches,
    /// because a brace ahead of it moved the DEPTH walk off the enum body. [`code_brace_counts`]
    /// removes the carrier issue #1428 names — a string literal in an attribute — and a TRAILING
    /// line comment with it, which that issue does not name: it quotes the whole-line comment
    /// filter as something already in place and scopes itself to the depth counter, so the
    /// trailing case was closed BEYOND it rather than as part of it. Others are left, filed as
    /// issue #1433 and measured there — among them a BLOCK comment, a CHAR literal, and a string
    /// spanning lines with the brace on a continuation line. Each drops the variant behind it in silence,
    /// which is the failure the identifier entries above describe arriving by a different route,
    /// so it is catalogued with them and pinned beside them.
    ///
    /// Adding any of these puts the weight back on this parser, so widen it in that change
    /// rather than trusting this list.
    pub(crate) fn declared_variant_names(enum_name: &str) -> std::collections::BTreeSet<String> {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/observability.rs");
        let text = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
        variant_names_in(&text, enum_name, &path.display().to_string())
    }

    /// The variant names `enum {enum_name}` declares in `text`; `source` names where `text` came
    /// from, and is used in the two failure messages and nowhere else.
    ///
    /// Split out of [`declared_variant_names`] so the parse can be driven by spellings THIS FILE
    /// DOES NOT CONTAIN. The split is what makes the walk testable at all: the scan reads the
    /// crate's own source, nothing in it is spelled `#[allow(dead_code)] Scheduled,`, and so the
    /// LOOP BODY of [`strip_leading_attributes`] was reached by no test — measured on issue
    /// #1397, replacing the call below with a no-op reference left the whole suite at `0 failed`.
    /// `the_variant_scan_reads_the_shapes_its_parser_names` now drives shapes those
    /// doc-comments name — the two helpers' and the remainder rule's own, above
    /// [`declared_variant_names`] — each in the direction its comment claims, and records in its
    /// own headline what it leaves undriven.
    fn variant_names_in(
        text: &str,
        enum_name: &str,
        source: &str,
    ) -> std::collections::BTreeSet<String> {
        let header = format!("enum {enum_name} {{");
        let mut lines = text
            .lines()
            .map(str::trim)
            // Keeps a comment out of the walk on two counts, neither of them the braces it
            // carries — those are `code_brace_counts`' job now. One is the HEADER: a comment
            // ending in `enum Foo {` would otherwise be taken for the declaration below. The
            // other is its COMMAS, which `split_top_level` splits on whatever they sit inside,
            // so an uppercase word ending a comment's last clause is read as a fieldless
            // variant — measured, `// Covers A, B` above `Gated,` scans to
            // `{"Anchor", "B", "Gated"}` without this filter. That one is a name nobody wrote,
            // so it fails loudly rather than silently.
            //
            // This filter did the BRACE job for a whole-line comment and never could for a
            // trailing one, because it reads the START of a line: measured against the walk as
            // it stood, `// {` above `Gated,` scans to `{"Anchor", "Gated"}` with this filter
            // and to `{"Anchor"}` without it, while `Gated, // {` scans to
            // `{"Anchor", "Gated"}` either way.
            //
            // Measured on issue #1428, no comment in this file reaches it — removing it
            // outright leaves the suite at `0 failed` — and a header so shadowed trips the
            // empty-parse refusal below rather than parsing wrongly: measured by adding a
            // shadowing line above `enum Diagnostic` and removing this filter, the scan panics
            // with its own "parsed no variants" message. That refusal is not something this
            // counter introduced: measured the same way, the walk as it stood reaches the
            // identical message by a different route. Kept because a refusal is still a failure
            // to diagnose, the comma reading is a name nobody wrote, and refusing both shapes
            // costs one predicate.
            .filter(|line| !line.starts_with("//"))
            .skip_while(|line| !line.ends_with(header.as_str()));
        let opening = lines
            .next()
            .unwrap_or_else(|| panic!("no `{header}` declaration in {source}"));

        let (opens, closes) = code_brace_counts(opening);
        let mut depth = opens - closes;
        let mut names = std::collections::BTreeSet::new();
        for line in lines {
            if depth == 0 {
                break;
            }
            // Only a line DIRECTLY inside the enum body declares a variant: deeper is a
            // field, shallower is past the enum.
            if depth == 1 {
                for segment in split_top_level(line) {
                    let segment = strip_leading_attributes(segment);
                    let name: String = segment
                        .chars()
                        .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
                        .collect();
                    let rest = segment[name.len()..].trim_start();
                    // A variant is an identifier that ends the segment, is followed by its
                    // struct/tuple body, or is given an explicit discriminant (issue #1410).
                    // Anything else at this depth is not a declaration.
                    //
                    // The uppercase initial is what BOUNDS that last arm: an attribute spread
                    // over several lines leaves its `key = value` continuations at depth 1 too,
                    // and a conventionally snake_case key is rejected on the initial rather than
                    // on the remainder. The bound is the leading token's CASE, so an
                    // uppercase-initial key IS read — see the measurement in the doc-comment
                    // above.
                    let declares_variant = name.starts_with(|c: char| c.is_ascii_uppercase())
                        && (rest.is_empty()
                            || rest.starts_with('{')
                            || rest.starts_with('(')
                            || rest.starts_with('='));
                    if declares_variant {
                        names.insert(name);
                    }
                }
            }
            let (opens, closes) = code_brace_counts(line);
            depth = depth + opens - closes;
        }

        // A scan that finds nothing would let the sweeps pass vacuously, so an empty parse is
        // a hard failure rather than a silently permissive one.
        assert!(
            !names.is_empty(),
            "parsed no variants out of `enum {enum_name}` in {source} — the source scan is broken"
        );
        names
    }

    /// Shapes `split_top_level`, `strip_leading_attributes`, [`code_brace_counts`] and the
    /// remainder rule inside [`variant_names_in`] are written for, driven through the parse that
    /// consumes them — but not all of them. A comma inside `( … )`, the second nesting context
    /// `split_top_level`'s own doc names, is not driven here; that gap is issue #1419's, filed
    /// off PR #1407.
    ///
    /// Held against synthetic text because the scan reads THIS FILE, and no enum it is pointed
    /// at carries an attribute on a variant, a discriminant, a raw identifier, or a brace
    /// anywhere but a block delimiter — so those walks ran on nothing and the shapes their
    /// doc-comments argue about had no way in. Each case is one claim from those doc-comments,
    /// asserted in the direction the comment makes: where a comment says a name is read it must
    /// appear, and where a comment says a name is dropped or invented, that is what the case
    /// pins.
    ///
    /// Some cases assert a claim about a shape deliberately NOT read — the raw identifier, the
    /// non-ASCII initial, the `target_os` an attribute's continuation line must not yield, the
    /// brace carriers issue #1433 leaves — and some about one deliberately OVER-read,
    /// where the pinned set carries a name nobody wrote.
    /// A catalogue entry saying a spelling is dropped is exactly as capable of going stale as
    /// one saying a spelling is handled or over-read, and none of them has a consuming
    /// assertion of its own to fail when it does; pinning them here is what keeps those entries
    /// answerable to the parser.
    ///
    /// `Anchor` rides in every case for two reasons: an empty parse is a hard failure inside the
    /// scan, which would mask a case that yields nothing; and its presence proves the walk
    /// reached the body at all, so an expected-empty case means the shape was dropped rather
    /// than the header missed.
    #[test]
    fn the_variant_scan_reads_the_shapes_its_parser_names() {
        let scan = |body: &str| {
            variant_names_in(
                &format!("enum Probe {{\n    Anchor,\n{body}\n}}\n"),
                "Probe",
                "<synthetic>",
            )
        };
        let expect = |names: &[&str]| -> std::collections::BTreeSet<String> {
            names.iter().map(|n| (*n).to_owned()).collect()
        };

        // The shape issue #1397 closes. Before the strip, the identifier run started at `#` and
        // yielded the empty string, so the variant never entered the set and layer 2 passed
        // vacuously for it.
        assert_eq!(
            scan("    #[allow(dead_code)] Scheduled,"),
            expect(&["Anchor", "Scheduled"])
        );
        // The attribute on its OWN line already parsed, and still does: its segment yields no
        // name and is discarded, the variant's yields the name.
        assert_eq!(
            scan("    #[allow(dead_code)]\n    Scheduled,"),
            expect(&["Anchor", "Scheduled"])
        );
        // The walk is a loop, so a run of them is stepped over rather than just the first.
        assert_eq!(
            scan("    #[allow(dead_code)] #[allow(unused)] Scheduled,"),
            expect(&["Anchor", "Scheduled"])
        );
        // Bracket DEPTH, not a seek to the first `]`: that seek would stop at the INNER one,
        // leaving `] Scheduled`, whose identifier run is empty — and the variant would be dropped.
        assert_eq!(
            scan("    #[foo[bar]] Scheduled,"),
            expect(&["Anchor", "Scheduled"])
        );
        // An UNBALANCED run returns the segment untouched. It then still opens with `#`, yields
        // no name, and the variant stays out — the pre-existing under-match, never a wrong name.
        assert_eq!(
            scan("    #[allow(dead_code) Scheduled,"),
            expect(&["Anchor"])
        );
        // `#` not followed by `[` is handed back the same way, for the same reason.
        assert_eq!(
            scan("    #![allow(dead_code)] Scheduled,"),
            expect(&["Anchor"])
        );
        // A BRACE inside a same-line attribute drops the variant, and this is not the depth walk
        // `code_brace_counts` repairs — measured, it returns `(0, 0)` here and the depth never
        // moves. `split_top_level` counts `{` raw, so the string's brace leaves its nesting at 1,
        // the trailing comma never splits, and the segment handed back is `Scheduled,` — an
        // identifier whose remainder `,` `declares_variant` refuses. Filed as issue #1437 and
        // pinned here as a DROP, unchanged by issue #1428's fix and measured identically against
        // the parser as it stood.
        assert_eq!(scan("    #[doc = \"{\"] Scheduled,"), expect(&["Anchor"]));

        // `split_top_level`'s two jobs. The trailing comma is what makes a FIELDLESS variant
        // visible; variants sharing a line are separated on the commas between them.
        assert_eq!(scan("    Bare,"), expect(&["Anchor", "Bare"]));
        assert_eq!(scan("    A, B,"), expect(&["Anchor", "A", "B"]));
        // A comma inside a variant's own body sits at nesting depth 1 and does not split it.
        assert_eq!(
            scan("    Monitor401 { account: String, consecutive: u32 },"),
            expect(&["Anchor", "Monitor401"])
        );
        // The non-empty remainders `declares_variant` admits, and the identifier rule that
        // keeps a non-camel-case variant from being silently dropped.
        assert_eq!(scan("    Wrapped(u32),"), expect(&["Anchor", "Wrapped"]));
        assert_eq!(scan("    Poll_401,"), expect(&["Anchor", "Poll_401"]));
        // The shape issue #1410 closes. An EXPLICIT DISCRIMINANT leaves a remainder none of the
        // other arms admits, so the variant was dropped and the consuming guard passed
        // vacuously for it — reachable in ordinary Rust, needing no attribute, and accepted by
        // `cargo fmt` where the issue #1397 spelling above is rewritten away.
        assert_eq!(scan("    Scheduled = 3,"), expect(&["Anchor", "Scheduled"]));
        // A discriminant is any const expression, so the arm tests the `=` rather than what
        // follows it; both of these reach `declared` under the same rule.
        assert_eq!(
            scan("    Scheduled = 1 + 2,"),
            expect(&["Anchor", "Scheduled"])
        );
        assert_eq!(
            scan("    Scheduled = SOME_CONST,"),
            expect(&["Anchor", "Scheduled"])
        );
        // Only a line DIRECTLY inside the body declares: the field line below is at depth 2.
        assert_eq!(
            scan("    Monitor401 {\n        account: String,\n    },"),
            expect(&["Anchor", "Monitor401"])
        );

        // The shape issue #1428 closes. A brace inside a STRING LITERAL is not a block
        // delimiter, but the depth walk counted it as one, so `#[doc = "{"]` took the depth
        // from 1 to 2 and the variant below it read as a FIELD. Dropped in SILENCE: the scan's
        // only guard against a vacuous pass is a hard failure on an EMPTY parse, and a set
        // merely short of one variant satisfies it — `Anchor` is still in this one.
        assert_eq!(
            scan("    #[doc = \"{\"]\n    Gated,"),
            expect(&["Anchor", "Gated"])
        );
        // The same defect in the other direction, which an open-brace case cannot reach: a
        // closing brace took the depth to 0, and the walk left the enum body a line early.
        assert_eq!(
            scan("    #[doc = \"}\"]\n    Gated,"),
            expect(&["Anchor", "Gated"])
        );
        // An escaped quote does not end the literal, so the brace behind it is still inside it.
        assert_eq!(
            scan("    #[doc = \"\\\"{\"]\n    Gated,"),
            expect(&["Anchor", "Gated"])
        );
        // A RAW string is where a brace is likeliest to be written on purpose.
        assert_eq!(
            scan("    #[doc = r\"{\"]\n    Gated,"),
            expect(&["Anchor", "Gated"])
        );
        // The case above holds even if a raw string is scanned as an ordinary one, so it pins
        // the CLASS rather than the arm. This is the one answerable to the arm: a raw string
        // exists to carry quotes, and read as an ordinary one it ends at the first of them,
        // leaving the `}` behind it counted. The hashes are what fix the real terminator, so
        // they are counted rather than assumed absent.
        assert_eq!(
            scan("    #[doc = r#\"{\"}\"#]\n    Gated,"),
            expect(&["Anchor", "Gated"])
        );
        assert_eq!(
            scan("    #[doc = r##\"{\"}\"##]\n    Gated,"),
            expect(&["Anchor", "Gated"])
        );
        // A LINE COMMENT is the carrier the pre-walk filter already names, but that filter
        // reads the start of a line, so a trailing one reached the counter intact.
        assert_eq!(
            scan("    Gated, // {\n    After,"),
            expect(&["Anchor", "Gated", "After"])
        );
        // The counter still counts the braces that ARE delimiters: the nested body below opens
        // and closes around a field line, and the variant after the string-literal brace is
        // read at the depth that leaves.
        assert_eq!(
            scan("    Wrapped {\n        note: String,\n    },\n    #[doc = \"{\"]\n    After,"),
            expect(&["Anchor", "Wrapped", "After"])
        );

        // Dropped ON PURPOSE, and named in the catalogue above `declared_variant_names`
        // rather than handled: a RAW IDENTIFIER stops the run at `r`, which is lowercase, and
        // leaves the remainder `#type`, which matches no arm. Reading it would mean admitting a
        // name with no uppercase initial behind the escape — which the case below does not
        // forbid: measured, an `r#`-scoped relaxation reads this one and still refuses that
        // one. So this assertion holds a deferral, not a constraint.
        assert_eq!(scan("    r#type,"), expect(&["Anchor"]));
        // The other spelling that entry names, and the one it says nothing else catches: a
        // non-ASCII initial sits outside the ASCII run AND outside the ASCII-uppercase initial,
        // so it is dropped in silence. Widening the run alone leaves it dropped; this reddens
        // only when the run and the initial are BOTH taken to Unicode, which is the widening
        // the entry defers.
        assert_eq!(scan("    Ünicode,"), expect(&["Anchor"]));
        // The bound on the discriminant arm. A multi-line attribute's `key = value`
        // continuation sits at depth 1 too, because only braces move the depth counter, so the
        // remainder rule alone would read it as a declaration. What rejects this one is the
        // leading token's CASE, and `target_os` must not appear beside the variant it gates.
        assert_eq!(
            scan("    #[cfg(all(\n        target_os = \"macos\",\n    ))]\n    Gated,"),
            expect(&["Anchor", "Gated"])
        );
        // The other side of that bound, and the reason it is stated over the case rather than
        // the form: an UPPERCASE-initial key is read, so the arm above invents a name here.
        // Loud — `Bar` lands in `declared`, goes unsampled and fails — but reachable only
        // because of that arm, which is what makes this the pin the entry above needs.
        assert_eq!(
            scan("    #[foo(\n        Bar = 1,\n    )]\n    Gated,"),
            expect(&["Anchor", "Bar", "Gated"])
        );
        // Same rule, remainder empty rather than `=`: a BARE uppercase continuation token takes
        // the `rest.is_empty()` arm. Loud in the same direction and it predates the arm above —
        // recorded rather than closed, and pinned so the entry stays answerable to the parser.
        assert_eq!(
            scan("    #[cfg(any(\n        Foo,\n    ))]\n    Gated,"),
            expect(&["Anchor", "Foo", "Gated"])
        );

        // The carriers the issue #1428 counter still does not see, catalogued above
        // `declared_variant_names` rather than handled and pinned here so those entries stay
        // answerable to the parser. A BLOCK comment's braces are counted, and the variant
        // behind one is dropped in the same silence the string-literal shape was.
        assert_eq!(scan("    /* { */\n    Gated,"), expect(&["Anchor"]));
        // The same carrier in its OTHER direction, which is what makes the silence symmetric
        // and is the claim `code_brace_counts`' doc-comment now argues: a `"` inside the block
        // comment opens a string that swallows the real `{` beside it, so the matching `}`
        // takes the depth to 0 and the walk leaves the body a variant early. `After` goes, and
        // nothing fails. `Wrapped` goes for a SECOND reason that outlives any brace rule — the
        // comment sits ahead of its name, so the identifier run starts at `/` — which is why
        // the delimiter case above carries the same `Wrapped { … }, … After,` shape with no
        // comment on that line, and keeps both.
        //
        // This shape is a REGRESSION: measured, the walk as it stood scans this same input to
        // `{"Anchor", "After"}` — right, because it counted the real `{` and a quote inside a
        // comment was inert to it. The string arm makes that quote live.
        //
        // Other shapes regress the same way, and this comment deliberately does not close the
        // set: bounding it was tried as a count, then as a definition, and each was falsified
        // by the next reader, because a claim about the complement is not something these pins
        // can back. What they do back is one instance each, measured against both walks. The
        // char literal: `Quote = '"' as isize, Wrapped {` scans to `{"Anchor", "Quote",
        // "Wrapped"}` here against `{"Anchor", "Quote", "Wrapped", "After"}` as it stood, while
        // the same literal on its OWN line regresses nothing, there being no brace behind it to
        // lose. The line-comment arm reaches it too, without opening any string: a `//` inside a
        // block comment breaks the scan at that point, so `/* see https://example.com */` — an
        // ordinary URL — swallows the brace behind it. Nothing in the enums this scan reads
        // carries any of it (issue #1433 § Reachability), so the blast radius is zero today; it
        // is stated because a carrier this change CREATED is not the same thing as one it merely
        // left unseen, and #1433 — which owns these carriers — records it.
        assert_eq!(
            scan("    /* \" */ Wrapped {\n        note: String,\n    },\n    After,"),
            expect(&["Anchor"])
        );
        // The control that separates those two mechanisms, which the case above fuses into a
        // single `{"Anchor"}`: drop the quote and the block comment carries no string, so the
        // real `{` IS counted, the depth holds, and `After` survives — while `Wrapped` is
        // dropped just the same, the comment still sitting ahead of its name. Measured
        // `{"Anchor", "After"}`. Without it either mechanism could be credited with the whole
        // of the case above, which is how that case came to be described as restoring `Wrapped`
        // once the brace was counted — a set no brace rule reaches.
        assert_eq!(
            scan("    /* x */ Wrapped {\n        note: String,\n    },\n    After,"),
            expect(&["Anchor", "After"])
        );
        // The same skip over a WRAPPED field type, which is the direction the doc-comment above
        // had wrong twice — first as uniformly loud, then as uniformly silent. It is neither:
        // `cargo fmt` breaks a long field type across lines, and a continuation line is a bare
        // uppercase identifier whose comma leaves an empty remainder, which is exactly the shape
        // of a fieldless variant. So `String` enters the set — a name nobody wrote, which fails
        // in `assert_samples_every_variant` where the simple-field case above fails nothing.
        // Loud is still not saved: `After` is dropped here too, the walk having left the body.
        assert_eq!(
            scan(
                "    /* \" */ Wrapped {\n        inner: Vec<\n            String,\n        >,\n    },\n    After,"
            ),
            expect(&["Anchor", "String"])
        );
        // Its control, holding the wrapping constant and dropping only the quote: the real `{`
        // is counted, the depth holds, the continuation lines are read at FIELD depth where
        // nothing admits them, and the set is the plain one. So the invented name above is
        // answerable to the skipped brace, not to the wrapping.
        assert_eq!(
            scan(
                "    /* x */ Wrapped {\n        inner: Vec<\n            String,\n        >,\n    },\n    After,"
            ),
            expect(&["Anchor", "After"])
        );
        // A CHAR literal likewise. Reading it means telling `'{'` from the `'a` of a lifetime,
        // which is a rule the string arms do not need and this counter does not have.
        assert_eq!(
            scan("    Gated = '{' as isize,\n    After,"),
            expect(&["Anchor", "Gated"])
        );
        // The char literal's OTHER direction, and the second instance of the regression class
        // stated over `code_brace_counts`: a literal holding a QUOTE opens a string that runs to
        // end of line, so a real `{` behind it on the same line is lost. Measured, the walk as
        // it stood scans this to `{"Anchor", "Quote", "Wrapped", "After"}` — right — where this
        // one drops `After`, the walk never having entered the body. The `'{'` case above is the
        // over-count direction and regresses nothing; this one is why the shape is pinned rather
        // than described. Issue #1433 owns both and carries the old-vs-new measurement.
        assert_eq!(
            scan("    Quote = '\"' as isize, Wrapped {\n        note: String,\n    },\n    After,"),
            expect(&["Anchor", "Quote", "Wrapped"])
        );
        // Its control: the same literal on its OWN line, no brace behind it to lose. Both walks
        // agree here, which is what confines the regression to the same-line shape.
        assert_eq!(
            scan("    Quote = '\"' as isize,\n    After,"),
            expect(&["Anchor", "After", "Quote"])
        );
        // The line-comment arm reaches the same shape without opening any string at all: a `//`
        // inside a BLOCK comment breaks the scan there, so an ordinary URL swallows the brace
        // behind it. Measured, the walk as it stood scans this to `{"Anchor", "After"}` — right,
        // having counted the real `{` — where this one drops `After`.
        assert_eq!(
            scan(
                "    /* see https://example.com */ Wrapped {\n        note: String,\n    },\n    After,"
            ),
            expect(&["Anchor"])
        );
        // Its control: the same comment with no `//` inside it. Both walks agree here, which is
        // what isolates the slashes rather than the comment.
        assert_eq!(
            scan("    /* see example.com */ Wrapped {\n        note: String,\n    },\n    After,"),
            expect(&["Anchor", "After"])
        );
        // And a string SPANNING lines, where the asymmetry is worth pinning rather than
        // describing: a brace on the OPENING line is already skipped, the unterminated quote
        // swallowing the rest of that line, so it is a brace on a CONTINUATION line that is
        // read as code. Both variants behind it are lost.
        assert_eq!(
            scan("    #[doc = \"a\n        {\"]\n    Gated,\n    After,"),
            expect(&["Anchor"])
        );
    }

    /// Assert `samples` covers every variant `enum {enum_name}` declares — layer 2 of the
    /// issue #891 guarantee.
    fn assert_samples_every_variant<T>(
        enum_name: &str,
        samples: &[T],
        variant_name: fn(&T) -> &'static str,
    ) {
        let declared = declared_variant_names(enum_name);
        let sampled: std::collections::BTreeSet<String> =
            samples.iter().map(|s| variant_name(s).to_owned()).collect();
        // Equality BOTH ways. Declared-but-unsampled is the hole this exists to close;
        // sampled-but-undeclared means the source scan itself has drifted, which would
        // quietly weaken the check rather than fail it.
        let unsampled: Vec<&String> = declared.difference(&sampled).collect();
        let undeclared: Vec<&String> = sampled.difference(&declared).collect();
        assert!(
            unsampled.is_empty() && undeclared.is_empty(),
            "issue #891: the `{enum_name}` issue #15 redaction sweep must render EVERY declared \
             variant.\n  declared but never sampled (would ship with zero redaction coverage): \
             {unsampled:?}\n  sampled but absent from the enum source (the scan has drifted): \
             {undeclared:?}"
        );
    }

    /// This `Event`'s variant name, spelled as the enum declares it.
    ///
    /// Layer 1 of the issue #891 guarantee: the match is exhaustive, so adding a variant to
    /// `Event` FAILS TO COMPILE until it is named here — after which
    /// `assert_samples_every_variant` fails until it is also sampled in `every_event_variant`.
    fn event_variant_name(event: &Event) -> &'static str {
        match event {
            Event::Swap { .. } => "Swap",
            Event::ReStash { .. } => "ReStash",
            Event::AllExhausted { .. } => "AllExhausted",
            Event::AllExhaustedCleared => "AllExhaustedCleared",
            Event::ActiveDeadNoTarget { .. } => "ActiveDeadNoTarget",
            Event::ActiveDeadNoTargetCleared => "ActiveDeadNoTargetCleared",
            Event::FleetRunwayLow { .. } => "FleetRunwayLow",
            Event::FleetRunwayRecovered => "FleetRunwayRecovered",
            Event::Monitor401 { .. } => "Monitor401",
            Event::CredentialDead { .. } => "CredentialDead",
            Event::EmergencySwap { .. } => "EmergencySwap",
            Event::CredentialRestored { .. } => "CredentialRestored",
            Event::CanonicalScrubbed { .. } => "CanonicalScrubbed",
            Event::CanonicalRestored { .. } => "CanonicalRestored",
            Event::CanonicalRecovered { .. } => "CanonicalRecovered",
            Event::CanonicalRecoveryExhausted { .. } => "CanonicalRecoveryExhausted",
            Event::CanaryDrift { .. } => "CanaryDrift",
            Event::CanaryUnparseableCanonical { .. } => "CanaryUnparseableCanonical",
            Event::CanaryOnlineProbe { .. } => "CanaryOnlineProbe",
            Event::CanaryAmbiguous { .. } => "CanaryAmbiguous",
            Event::CanaryCleared => "CanaryCleared",
            Event::CredentialUnrecoverable { .. } => "CredentialUnrecoverable",
            Event::KeychainLockedWait => "KeychainLockedWait",
            Event::UsageScopeFail { .. } => "UsageScopeFail",
            Event::Refresh { .. } => "Refresh",
            Event::PollRefresh { .. } => "PollRefresh",
            Event::KeepWarm { .. } => "KeepWarm",
            Event::RefreshSystemicFailure { .. } => "RefreshSystemicFailure",
            Event::RefreshSystemicRecovered => "RefreshSystemicRecovered",
            Event::RefreshPreflightUnresolved => "RefreshPreflightUnresolved",
            Event::RefreshBinaryResolved { .. } => "RefreshBinaryResolved",
            Event::CredentialHealth { .. } => "CredentialHealth",
            Event::Login { .. } => "Login",
            Event::Capture { .. } => "Capture",
            Event::UsageRollup { .. } => "UsageRollup",
            Event::UsageGap { .. } => "UsageGap",
            Event::UncapturedLogin { .. } => "UncapturedLogin",
            Event::Export { .. } => "Export",
            Event::Import { .. } => "Import",
            Event::UsageBackoff { .. } => "UsageBackoff",
            Event::UsageBackoffCleared { .. } => "UsageBackoffCleared",
            Event::ExhaustedSlowPoll { .. } => "ExhaustedSlowPoll",
            Event::ExhaustedSlowPollCleared { .. } => "ExhaustedSlowPollCleared",
            Event::NearLimitPollCoverage { .. } => "NearLimitPollCoverage",
            Event::UsageVelocity { .. } => "UsageVelocity",
            Event::BlindWindow { .. } => "BlindWindow",
            Event::BlindEnter { .. } => "BlindEnter",
            Event::BlindExit { .. } => "BlindExit",
            Event::BlindGateEligible { .. } => "BlindGateEligible",
            Event::BlindPreemptReserveHold { .. } => "BlindPreemptReserveHold",
            Event::RetryAfterWalk { .. } => "RetryAfterWalk",
            Event::CredentialExpiryHorizon { .. } => "CredentialExpiryHorizon",
            Event::CredentialExpiryObserved { .. } => "CredentialExpiryObserved",
            Event::ObservationGapEnter { .. } => "ObservationGapEnter",
            Event::ObservationGapExit { .. } => "ObservationGapExit",
        }
    }

    /// Every `Event` variant, as the subject of the issue #15 sweep below — totality asserted
    /// here rather than trusted, per the two-layer note above `declared_variant_names`.
    ///
    /// Handles are plain (non-email) labels throughout: the sweep asserts a rendered line
    /// carries no NON-AUTHORED email, and an operator's own email label would be permitted,
    /// so sampling one here would prove nothing about redaction.
    fn every_event_variant() -> Vec<Event> {
        let samples = vec![
            Event::Swap {
                from: "work".to_owned(),
                to: "spare".to_owned(),
                reason: SwapReason::Session,
                session_pct: 97,
                projection: None,
            },
            // Issue #634: the projection-carrying + velocity-carrying variants ride the same
            // sweep — the extra tokens are bare numbers, so they add no email/token surface.
            Event::Swap {
                from: "work".to_owned(),
                to: "spare".to_owned(),
                reason: SwapReason::VelocityPreempt,
                session_pct: 70,
                projection: Some(SwapProjection {
                    projected_pct: 96.5,
                    rate_pct_per_sec: 0.221667,
                    horizon_secs: 120,
                    ceiling_pct: 89.0,
                }),
            },
            Event::ReStash {
                account: "work".to_owned(),
            },
            Event::AllExhausted {
                hold: "work".to_owned(),
                cause: SwapReason::Weekly,
                resets_at: Some(1_782_777_600),
            },
            Event::AllExhaustedCleared,
            Event::ActiveDeadNoTarget {
                hold: "work".to_owned(),
                cause: SwapReason::Weekly,
                resets_at: Some(1_782_777_600),
            },
            Event::ActiveDeadNoTargetCleared,
            // Issue #650: the proactive fleet-runway warn line — four bare integers, no handle.
            Event::FleetRunwayLow {
                runway_secs: 1800,
                threshold_secs: 3600,
                counted: 2,
                observed: 3,
            },
            Event::FleetRunwayRecovered,
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
            Event::CanonicalScrubbed {
                account: Some("work".to_owned()),
            },
            Event::CanonicalRestored {
                account: Some("work".to_owned()),
            },
            Event::CanonicalRecovered {
                account: "work".to_owned(),
            },
            Event::CanonicalRecoveryExhausted {
                account: Some("work".to_owned()),
            },
            Event::CanaryDrift {
                displayed: "work".to_owned(),
                matched: "spare".to_owned(),
                overridden: true,
            },
            Event::CanaryUnparseableCanonical { overridden: false },
            Event::CanaryOnlineProbe {
                verdict: "rejected",
                refused: true,
            },
            Event::CanaryAmbiguous { count: 2 },
            Event::CanaryCleared,
            Event::CredentialUnrecoverable {
                account: "work".to_owned(),
            },
            Event::KeychainLockedWait,
            Event::UsageScopeFail {
                account: "work".to_owned(),
            },
            Event::Refresh {
                account: "work".to_owned(),
                outcome: RefreshEventOutcome::Refreshed { rotated: true },
                expires_before: Some(1_782_777_600_000),
                expires_after: Some(1_782_781_200_000),
                reason: Some(RefreshEventReason::Timeout),
                backoff_secs: Some(240),
            },
            Event::PollRefresh {
                account: "work".to_owned(),
                trigger: PollRefreshTrigger::Recovery,
                outcome: RefreshEventOutcome::RefreshedNotReStashed { rotated: true },
            },
            Event::KeepWarm {
                account: "work".to_owned(),
                trigger: KeepWarmTrigger::Proactive,
                outcome: RefreshEventOutcome::NoChange,
            },
            Event::RefreshSystemicFailure { consecutive: 3 },
            Event::RefreshSystemicRecovered,
            Event::RefreshPreflightUnresolved,
            // Issue #786: the one variant whose payload is a filesystem location rather than a
            // handle/enum/number, percent-encoded by the formatter.
            Event::RefreshBinaryResolved {
                path: PathBuf::from("/opt/homebrew/bin/claude"),
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
            Event::Capture {
                account: Some("work".to_owned()),
                outcome: CaptureEventOutcome::Captured,
            },
            Event::UsageRollup {
                rolled_through: 1_782_777_600,
                raw_lines: 128,
            },
            Event::UsageGap {
                account: "work".to_owned(),
                since: 1_782_777_600,
            },
            // An opaque account UUID — an identifier, but not a token or an email.
            Event::UncapturedLogin {
                account_uuid: Some("u-Z".to_owned()),
            },
            Event::Export {
                accounts: 3,
                encrypted: true,
                mode: ExportMode::Full,
            },
            Event::Import {
                imported: 2,
                skipped: 1,
                overwritten: 1,
                failed: 0,
            },
            Event::UsageBackoff {
                account: "work".to_owned(),
                class: BackoffClass::RateLimited,
                consecutive: 2,
                retry_after_secs: Some(3600),
                backoff_secs: 120,
            },
            Event::UsageBackoffCleared {
                account: "work".to_owned(),
            },
            Event::ExhaustedSlowPoll {
                account: "work".to_owned(),
                window_secs: 3600,
            },
            Event::ExhaustedSlowPollCleared {
                account: "work".to_owned(),
            },
            Event::NearLimitPollCoverage {
                account: "work".to_owned(),
                sub_interval_secs: 60,
            },
            Event::UsageVelocity {
                account: "work".to_owned(),
                session_delta_pct: 7,
                weekly_delta_pct: -1,
                elapsed_secs: 300,
            },
            Event::BlindWindow {
                account: "work".to_owned(),
                duration_secs: 480,
                session_pct: 29,
                session_at_recovery: 61,
                near_limit: false,
                velocity: Some(BlindVelocity {
                    rate_pct_per_sec: 0.05,
                    inflation: 1.75,
                    ceiling_pct: 95.0,
                }),
                // Issue #670: the corrected-anchor mark rides the same sweep — a bare number,
                // so it adds no email/token surface even when the anchor was stale-low.
                session_high_water_pct: Some(88),
            },
            Event::BlindEnter {
                account: "work".to_owned(),
                session_pct: 82,
                weekly_pct: 44,
                was_active: true,
                near_limit: true,
            },
            Event::BlindExit {
                account: "work".to_owned(),
                duration_secs: 480,
                session_pct: 82,
                session_at_recovery: 91,
                weekly_pct: 44,
                weekly_at_recovery: 46,
                was_active: true,
                swapped_away: true,
                near_limit: true,
            },
            Event::BlindGateEligible {
                account: "work".to_owned(),
                viable_target: true,
                blind_secs: 300,
                session_pct: 88,
            },
            Event::BlindPreemptReserveHold {
                account: "work".to_owned(),
                retry_after_secs: 600,
                blind_secs: 300,
            },
            Event::RetryAfterWalk {
                account: "work".to_owned(),
                swaps: 3,
                window_secs: 900,
                retry_after_secs: 600,
            },
            // Issue #880: the two refresh-token-expiry lines. The file's first CREDENTIAL-DERIVED
            // payload beyond a classification — the deadlines come out of the credential blob itself,
            // so the scan matters more here than for a variant built from counters. Issue #891's
            // two-layer guarantee is what got them here rather than diligence: `event_variant_name`
            // would not compile without them, and `assert_samples_every_variant` would then fail
            // until they were sampled. `grant_replaced` is populated on the observation so
            // the grant verdict — the one field derived from the token itself — is scanned rather
            // than sitting omitted, and `before` is populated so the derived trailing `delta_secs`
            // rides the scan too.
            Event::CredentialExpiryHorizon {
                account: "u-A".to_owned(),
                state: ExpiryHorizon::Within,
                expires_at: 1_785_499_802,
                horizon_secs: 604_800,
            },
            Event::CredentialExpiryObserved {
                account: "u-A".to_owned(),
                provenance: ExpiryProvenance::CanonicalRestash,
                before: Some(1_785_499_802),
                after: 1_785_586_202,
                grant_replaced: Some(true),
            },
            // Issue #1453: the observation-gap pair. Both carry the account UUID and bare
            // numbers/bools only, so they add no email/token surface; the exit is sampled with
            // `swapped_away: true` so the scan covers the tail field as well.
            Event::ObservationGapEnter {
                account: "u-A".to_owned(),
                elapsed_secs: 90,
                threshold_secs: 75,
                was_active: true,
            },
            Event::ObservationGapExit {
                account: "u-A".to_owned(),
                elapsed_secs: 638,
                was_active: true,
                swapped_away: true,
            },
        ];
        assert_samples_every_variant("Event", &samples, event_variant_name);
        samples
    }

    #[test]
    fn no_event_line_carries_an_email_or_token_sigil() {
        // #15/#444: every field is a handle / enum / number / timestamp — or, in the single
        // case of the resolved `claude` path (#786), a percent-encoded filesystem location —
        // so a token or a NON-authored email can never reach a rendered line. Total over
        // `Event` by construction: see the two-layer note above `declared_variant_names`.
        for event in &every_event_variant() {
            let line = event.to_log_line(at_epoch(0));
            assert!(
                crate::redaction::meter::unauthored_emails(line.as_str(), &[]).is_empty(),
                "no non-authored email sigil (#15/#444): {line}"
            );
            assert!(!line.to_lowercase().contains("token"), "no token: {line}");
            // Exactly one line — no embedded newline could split or forge a record.
            assert_eq!(line.lines().count(), 1, "single line: {line}");
        }
    }

    #[test]
    fn every_event_line_carries_its_event_key_as_the_second_field() {
        // Issue #1185. `last_swap_at` and `last_refresh_outcomes` select a line by the POSITION
        // of its event key rather than by substring, because a free-form handle can spell any
        // substring but cannot occupy a field ahead of itself. That is only sound while the
        // grammar this module documents — `ts=<RFC3339> event=<name> …` — actually holds for
        // EVERY variant, so it is asserted here rather than argued: total over `Event` by
        // construction (see the two-layer note above `declared_variant_names`), so a future
        // variant that renders differently fails this test instead of silently un-anchoring
        // both readers.
        for event in &every_event_variant() {
            let line = event.to_log_line(at_epoch(0));
            let mut fields = line.split(' ');
            let ts = fields.next();
            assert_eq!(
                ts.map(|ts| ts.starts_with("ts=")),
                Some(true),
                "the timestamp is field 0: {line}"
            );
            assert!(
                !ts.unwrap().contains(char::is_whitespace),
                "an RFC 3339 stamp is space-free, so it occupies exactly one field: {line}"
            );
            let event_field = fields.next();
            assert_eq!(
                event_field.map(|field| field.starts_with("event=")),
                Some(true),
                "the event key is field 1 — the position both readers select on: {line}"
            );
        }
    }

    #[test]
    fn credential_health_line_carries_the_handle_and_state_token() {
        // Issue #119 (+ #427 `degraded`): the health-transition event is the handle + a bare
        // rollup token — never a token, an expiry, or an email. Each rollup state renders its
        // `snake_case` token, matching the `--json` wire serialization of `CredentialHealth`.
        for (state, token) in [
            (CredentialHealth::Healthy, "healthy"),
            (CredentialHealth::Unknown, "unknown"),
            (CredentialHealth::Stale, "stale"),
            (CredentialHealth::AtRisk, "at_risk"),
            (CredentialHealth::Degraded, "degraded"),
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
            projection: None,
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
        assert!(crate::redaction::meter::unauthored_emails(&logged, &[]).is_empty());
    }

    #[test]
    fn all_exhausted_cleared_survives_a_log_file_round_trip() {
        // REQ-STA-B-011's whole point: the LEAVE edge must be reconstructable OFFLINE. Drive the
        // real sink to a real file and read a full bracket back — emit → persist → parse, end to
        // end. The readback re-implements the flat `key=val` tokenizer rather than calling a
        // consumer, so it pins the LINE's own recoverability independently of any one consumer's
        // folding; that each real consumer tolerates the new kind is pinned separately, in
        // `reliability` and `usage_stats`' own test modules.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sessiometer.log");
        let mut log = EventLog::at(&path).unwrap();

        // ENTER, the relief swap, then the LEAVE.
        log.emit(&Event::AllExhausted {
            hold: "spare".to_owned(),
            cause: SwapReason::Session,
            resets_at: Some(1_782_777_600),
        })
        .unwrap();
        log.emit(&Event::Swap {
            from: "work".to_owned(),
            to: "spare".to_owned(),
            reason: SwapReason::Session,
            session_pct: 97,
            projection: None,
        })
        .unwrap();
        log.emit(&Event::AllExhaustedCleared).unwrap();

        let logged = std::fs::read_to_string(&path).unwrap();
        let parsed: Vec<std::collections::BTreeMap<&str, &str>> = logged
            .lines()
            .map(|line| {
                line.split_whitespace()
                    .filter_map(|token| token.split_once('='))
                    .collect()
            })
            .collect();

        // Each line recovers its own `event=` kind, in order — so the bracket is closed in the
        // LOG, not merely in code.
        assert_eq!(
            parsed
                .iter()
                .map(|fields| fields.get("event").copied().unwrap())
                .collect::<Vec<_>>(),
            vec!["all_exhausted", "swap", "all_exhausted_cleared"],
            "got: {logged:?}"
        );
        // …and the LEAVE line's `ts=` is readable by the crate's one canonical RFC-3339 parser,
        // so a hold's END is placeable in time — the offline reconstructability the requirement
        // asks for.
        assert!(
            crate::usage::epoch_from_rfc3339(parsed[2].get("ts").copied().unwrap()).is_some(),
            "the LEAVE edge must be placeable in time: {logged:?}"
        );
    }

    #[test]
    fn the_promoted_leave_edges_survive_a_log_file_round_trip() {
        // Issue #827's whole point, for BOTH promoted edges: each must be reconstructable
        // OFFLINE, so drive the real sink to a real file and read both full brackets back —
        // emit → persist → parse, end to end. Sibling of
        // `all_exhausted_cleared_survives_a_log_file_round_trip`; the readback re-implements the
        // flat `key=val` tokenizer rather than calling a consumer, so it pins each LINE's own
        // recoverability independently of any one consumer's folding. That the real consumers
        // tolerate the new kinds is pinned separately, in `reliability` and `usage_stats`' own
        // test modules.
        //
        // Both brackets go into ONE file, interleaved, because that is the production shape: the
        // two episodes are independent and can be open at the same time, so a reader has to pair
        // each LEAVE with its OWN ENTER by kind rather than by adjacency.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sessiometer.log");
        let mut log = EventLog::at(&path).unwrap();

        log.emit(&Event::ActiveDeadNoTarget {
            hold: "work".to_owned(),
            cause: SwapReason::Weekly,
            resets_at: Some(1_782_777_600),
        })
        .unwrap();
        log.emit(&Event::FleetRunwayLow {
            runway_secs: 1800,
            threshold_secs: 3600,
            counted: 2,
            observed: 2,
        })
        .unwrap();
        log.emit(&Event::ActiveDeadNoTargetCleared).unwrap();
        log.emit(&Event::FleetRunwayRecovered).unwrap();

        let logged = std::fs::read_to_string(&path).unwrap();
        let parsed: Vec<std::collections::BTreeMap<&str, &str>> = logged
            .lines()
            .map(|line| {
                line.split_whitespace()
                    .filter_map(|token| token.split_once('='))
                    .collect()
            })
            .collect();

        // Each line recovers its own `event=` kind, in order — so both brackets close in the LOG,
        // not merely in code.
        assert_eq!(
            parsed
                .iter()
                .map(|fields| fields.get("event").copied().unwrap())
                .collect::<Vec<_>>(),
            vec![
                "active_dead_no_target",
                "fleet_runway_low",
                "active_dead_no_target_cleared",
                "fleet_runway_recovered",
            ],
            "got: {logged:?}"
        );
        // …and each LEAVE line's `ts=` is readable by the crate's one canonical RFC-3339 parser,
        // so each episode's END is placeable in time — the offline reconstructability the issue
        // asks for. A bare token that could not be timestamped would close nothing.
        for fields in &parsed[2..] {
            assert!(
                crate::usage::epoch_from_rfc3339(fields.get("ts").copied().unwrap()).is_some(),
                "every LEAVE edge must be placeable in time: {logged:?}"
            );
        }
        // Neither promoted line adds an email/token surface to the file the operator ships
        // around — the #15 guarantee that is half of why the two moved onto this channel
        // (`Event::AllExhaustedCleared` carries the argument).
        assert!(crate::redaction::meter::unauthored_emails(&logged, &[]).is_empty());
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
                projection: None,
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
    fn last_swap_at_ignores_the_all_exhausted_cleared_leave_edge() {
        // Issue #800 put a NEW `event=` kind into the log, and this reader gates the one-shot
        // `use` verb's swap cooldown (#63/#10) — a production path that would otherwise be
        // covered only transitively. The daemon emits the clear on the relief swap's OWN tick,
        // immediately AFTER the swap line, so the clear is the LAST line exactly when the
        // cooldown is most load-bearing. `last_swap_at` scans from the end for a line whose
        // SECOND field is `event=swap` / `event=emergency_swap`, so it must walk past the clear
        // and still return the swap's instant — never `None` (which would read as "cooldown
        // inactive" and wrongly permit an immediate second swap).
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sessiometer.log");
        std::fs::write(
            &path,
            "ts=1970-01-01T00:00:10Z event=all_exhausted hold=b cause=session\n\
ts=1970-01-01T00:00:30Z event=swap from=a to=b reason=session session_pct=97\n\
ts=1970-01-01T00:00:30Z event=all_exhausted_cleared\n",
        )
        .unwrap();
        assert_eq!(last_swap_at(&path), Some(at_epoch(30)));

        // And a log carrying ONLY the bracket (no swap — the daemon left the state because the
        // active's usage fell, not because it swapped) still reports no swap, so neither edge can
        // fabricate a cooldown floor out of nothing.
        std::fs::write(
            &path,
            "ts=1970-01-01T00:00:10Z event=all_exhausted hold=b cause=session\n\
ts=1970-01-01T00:00:20Z event=all_exhausted_cleared\n",
        )
        .unwrap();
        assert_eq!(last_swap_at(&path), None);
    }

    #[test]
    fn last_swap_at_ignores_the_promoted_leave_edges() {
        // Issue #827 put two MORE `event=` kinds into the log, and this reader gates the one-shot
        // `use` verb's swap cooldown (#63/#10) — the same production path issue #800's sibling
        // test guards. Both new kinds land AFTER the swap on their own tick (the daemon pushes
        // each clear post-`decide_action`), so they are the last lines exactly when the cooldown
        // matters most. `last_swap_at` scans from the end for a line whose SECOND field is
        // `event=swap` / `event=emergency_swap`, so it must walk past both and still return it
        // — never `None`, which would read as "cooldown inactive" and wrongly permit an immediate
        // second swap.
        //
        // The active-dead strand's real exit is an EMERGENCY swap (#42/#405), so that arm of the
        // reader — not just the plain-swap one — is what this pins.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sessiometer.log");
        std::fs::write(
            &path,
            "ts=1970-01-01T00:00:10Z event=active_dead_no_target hold=a cause=weekly\n\
ts=1970-01-01T00:00:30Z event=emergency_swap from=a to=b\n\
ts=1970-01-01T00:00:30Z event=active_dead_no_target_cleared\n\
ts=1970-01-01T00:00:40Z event=fleet_runway_recovered\n",
        )
        .unwrap();
        assert_eq!(last_swap_at(&path), Some(at_epoch(30)));

        // And a log carrying ONLY the two brackets (no swap at all — the strand ended because the
        // dead active recovered, and the runway rose on its own) still reports no swap, so no new
        // edge can fabricate a cooldown floor out of nothing.
        std::fs::write(
            &path,
            "ts=1970-01-01T00:00:10Z event=active_dead_no_target hold=a cause=weekly\n\
ts=1970-01-01T00:00:20Z event=active_dead_no_target_cleared\n\
ts=1970-01-01T00:00:30Z event=fleet_runway_low runway_secs=1800 threshold_secs=3600 counted=2 observed=2\n\
ts=1970-01-01T00:00:40Z event=fleet_runway_recovered\n",
        )
        .unwrap();
        assert_eq!(last_swap_at(&path), None);
    }

    #[test]
    fn last_swap_at_ignores_a_swap_shaped_handle_on_a_non_swap_line() {
        // Issue #1185. `label` is free-form by design (README: written VERBATIM as the account
        // handle), so an operator-chosen handle can carry the literal text this reader anchors
        // on. A handle spelled `a event=swap b` renders that text INSIDE a non-swap line, and a
        // substring scan then reads a `monitor_401` as the fleet's most recent swap — fabricating
        // a cooldown floor that blocks a `use` swap the operator is entitled to (#63/#10).
        //
        // The event key is the SECOND whitespace-delimited field on every line this module
        // writes (`ts=<RFC3339> event=<name> …`, and an RFC 3339 stamp is space-free), so the
        // field POSITION is what distinguishes the real key from a handle that merely spells it.
        //
        // Both vintages of the durable log (issue #1092 / PR #1183): a handle carrying a raw
        // control byte reads as that byte on a pre-#1183 line and as `%09` on a post-#1183 one,
        // and neither space nor `=` is a control character — so the swap-shaped text renders
        // identically either way, and the reader must reject BOTH.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sessiometer.log");
        for handle in ["a\tb event=swap c", "a%09b event=swap c"] {
            std::fs::write(
                &path,
                format!(
                    "ts=1970-01-01T00:00:40Z event=monitor_401 account={handle} consecutive=1\n"
                ),
            )
            .unwrap();
            assert_eq!(
                last_swap_at(&path),
                None,
                "a handle spelling ` event=swap ` must not make a monitor_401 line answer the \
                 cooldown query (handle: {handle:?})"
            );

            // And with a REAL swap earlier in the log, the reader returns THAT instant — not the
            // later, nearer, swap-shaped non-swap line. Scanning from the end must walk past it.
            std::fs::write(
                &path,
                format!(
                    "ts=1970-01-01T00:00:10Z event=swap from=a to=b reason=session session_pct=97\n\
ts=1970-01-01T00:00:40Z event=monitor_401 account={handle} consecutive=1\n"
                ),
            )
            .unwrap();
            assert_eq!(last_swap_at(&path), Some(at_epoch(10)));
        }
    }

    #[test]
    fn refresh_outcome_token_round_trips() {
        // Issue #120: `from_token` is the exact inverse of `as_str`, so the offline
        // `list` view reads back precisely the variant the log wrote. An unrecognized
        // token (a truncated / future / corrupt line) is `None`, never mis-classified.
        for outcome in [
            RefreshEventOutcome::Refreshed { rotated: true },
            RefreshEventOutcome::RefreshedNotReStashed { rotated: true },
            RefreshEventOutcome::NoChange,
            RefreshEventOutcome::Dead,
            RefreshEventOutcome::Error,
        ] {
            assert_eq!(
                RefreshEventOutcomeKind::from_token(outcome.as_str()),
                Some(outcome.kind()),
                "the token round-trips to the KIND; the rotation payload is a separate \
                 field on the line, not recoverable from `outcome=` alone (issue #1004)"
            );
        }
        assert_eq!(RefreshEventOutcomeKind::from_token("bogus"), None);
        assert_eq!(RefreshEventOutcomeKind::from_token(""), None);
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
        assert_eq!(outcomes.get("work"), Some(&RefreshEventOutcomeKind::Dead));
        // `spare`'s only refresh is `refreshed`; the trailing `expires_*` AND `rotated=true`
        // fields are stripped, leaving the bare outcome token.
        assert_eq!(
            outcomes.get("spare"),
            Some(&RefreshEventOutcomeKind::Refreshed)
        );
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
        // A handle is operator free text that may contain spaces; the account field runs
        // from `account=` to the LAST ` outcome=` on a line whose second field is
        // `event=refresh`, so `my work` is matched whole rather than truncated at the space.
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
            Some(&RefreshEventOutcomeKind::Refreshed)
        );
    }

    #[test]
    fn last_refresh_outcomes_attributes_an_outcome_shaped_handle_to_its_own_account() {
        // Issue #1185. The sibling of the space case above, and the one the anchoring alone does
        // NOT survive: a handle spelled `<text> outcome=refreshed x` puts a SECOND ` outcome=` on
        // the line, and splitting at the FIRST one truncates the handle to `<text>` and reads the
        // handle's own text as the outcome. If the roster also holds an account genuinely named
        // `<text>` — the truncation PREFIX, not some shorter word inside it — the offline `list`
        // view (#120) then shows IT with a refresh outcome it never had, overwriting its real one.
        //
        // Both log lines below are therefore derived from the same `sib`, deliberately. An earlier
        // revision named the sibling `my` while the hostile handle truncated to `my work`, so the
        // second assertion held before AND after the fix and pinned nothing.
        //
        // The writer emits `outcome=` exactly once and every field after it is a number or an
        // enum token (`expires_*`, `rotated`, `reason`, `backoff_secs`, `window_secs`), so the
        // LAST ` outcome=` on the line is always the writer's and everything before it is the
        // handle, whole.
        //
        // Both vintages of the durable log (issue #1092 / PR #1183): the tab reads raw on a
        // pre-#1183 line and as `%09` on a post-#1183 one; the outcome-shaped text is unchanged
        // by either, so both must attribute to their own full handle.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sessiometer.log");
        for sib in ["my\twork", "my%09work"] {
            let handle = format!("{sib} outcome=refreshed x");
            std::fs::write(
                &path,
                format!(
                    "ts=1970-01-01T00:00:10Z event=refresh account={sib} outcome=dead\n\
ts=1970-01-01T00:00:20Z event=refresh account={handle} outcome=no_change\n"
                ),
            )
            .unwrap();
            let outcomes = last_refresh_outcomes(&path);
            assert_eq!(
                outcomes.get(handle.as_str()),
                Some(&RefreshEventOutcomeKind::NoChange),
                "the handle is matched whole, up to the writer's own `outcome=` ({handle:?})"
            );
            assert_eq!(
                outcomes.get(sib),
                Some(&RefreshEventOutcomeKind::Dead),
                "the account the pre-fix reader truncated TO keeps its own outcome ({handle:?})"
            );
        }
    }

    #[test]
    fn last_refresh_outcomes_ignores_a_refresh_shaped_handle_on_another_event() {
        // Issue #1185, the prefix half. The `account=` value of ANY event can spell this reader's
        // ` event=refresh account=` anchor — including the login-FAILURE path, which logs an
        // `account_uuid` harvested from `~/.claude.json` BEFORE the roster charset gate (#1052)
        // applies (issue #1092 / PR #1183 states that ordering). A substring scan then reads a
        // `login` line as a refresh outcome for whatever account the hostile value names.
        //
        // `event=` is the second field on every line, so position — not a substring — is what
        // selects a real refresh line.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sessiometer.log");
        std::fs::write(
            &path,
            "ts=1970-01-01T00:00:10Z event=login account=z event=refresh account=work outcome=dead \
outcome=failed\n",
        )
        .unwrap();
        assert!(
            last_refresh_outcomes(&path).is_empty(),
            "a login line must not report a refresh outcome for any account"
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
            assert!(
                crate::redaction::meter::unauthored_emails(line.as_str(), &[]).is_empty(),
                "no non-authored email may appear (#15/#444): {line}"
            );
            assert!(!line.contains("token"), "no token may appear: {line}");
            assert!(!line.contains("Bearer"), "no bearer may appear: {line}");
            assert!(!line.contains("sk-ant"), "no api key may appear: {line}");
            // No per-account identity field: the events are aggregate-only.
            assert!(!line.contains("account="), "no account handle: {line}");
            assert!(!line.contains("acct="), "no account handle: {line}");
        }
    }

    // --- Event::UsageBackoff / UsageBackoffCleared / UsageVelocity (durable #399 signals) ---

    #[test]
    fn usage_backoff_line_carries_the_uuid_class_streak_and_window() {
        // The durable ENTER line (#399): the account UUID (not a label), the throttle class, the
        // running back-off streak, and the armed window — the previously stderr-only 429 signal
        // made durable. No server `Retry-After` here, so the optional trailing field is ABSENT.
        let line = Event::UsageBackoff {
            account: "u-A".to_owned(),
            class: BackoffClass::RateLimited,
            consecutive: 3,
            retry_after_secs: None,
            backoff_secs: 480,
        }
        .to_log_line(at_epoch(0));
        assert_eq!(
            line,
            format!("{TS0} event=usage_backoff acct=u-A class=rate_limited consecutive=3 backoff_secs=480")
        );
    }

    #[test]
    fn usage_backoff_line_appends_the_raw_retry_after_when_present() {
        // A server-advised `Retry-After` (the #295 source label, RAW/pre-cap) trails the line AFTER
        // `backoff_secs`, mirroring the sibling `diag=tick` field order — so the pathological-value
        // case (`backoff_secs=3600 retry_after_secs=86400`, the #294 clamp) stays visible durably.
        let line = Event::UsageBackoff {
            account: "u-A".to_owned(),
            class: BackoffClass::RateLimited,
            consecutive: 6,
            retry_after_secs: Some(86_400),
            backoff_secs: 3600,
        }
        .to_log_line(at_epoch(0));
        assert_eq!(
            line,
            format!("{TS0} event=usage_backoff acct=u-A class=rate_limited consecutive=6 backoff_secs=3600 retry_after_secs=86400")
        );
    }

    #[test]
    fn usage_backoff_line_distinguishes_a_transient_from_a_rate_limit() {
        // The `class=` token is what makes the #399 "429 count" queryable: a `5xx` / network
        // transient renders `transient`, a `429` renders `rate_limited` — so `grep class=rate_limited`
        // counts genuine rate-limits, not every back-off.
        let transient = Event::UsageBackoff {
            account: "u-B".to_owned(),
            class: BackoffClass::Transient,
            consecutive: 1,
            retry_after_secs: None,
            backoff_secs: 120,
        }
        .to_log_line(at_epoch(0));
        assert_eq!(
            transient,
            format!(
                "{TS0} event=usage_backoff acct=u-B class=transient consecutive=1 backoff_secs=120"
            )
        );
    }

    #[test]
    fn usage_backoff_cleared_line_carries_the_uuid() {
        // The edge-triggered EXIT partner: just the account UUID — the window's span is bracketed
        // by pairing this with the last ENTER line.
        let line = Event::UsageBackoffCleared {
            account: "u-A".to_owned(),
        }
        .to_log_line(at_epoch(0));
        assert_eq!(line, format!("{TS0} event=usage_backoff_cleared acct=u-A"));
    }

    #[test]
    fn exhausted_slow_poll_line_carries_the_uuid_and_window() {
        // The durable ENTER line (issue #537): the account UUID (not a label, #15) and the armed
        // slow-poll window — redacted to uuid + window ONLY. No token/email surface exists.
        let line = Event::ExhaustedSlowPoll {
            account: "u-A".to_owned(),
            window_secs: 3600,
        }
        .to_log_line(at_epoch(0));
        assert_eq!(
            line,
            format!("{TS0} event=exhausted_slow_poll acct=u-A window_secs=3600")
        );
        // #15: no non-authored email, no token/bearer/api-key, and the identity is the UUID.
        assert!(
            crate::redaction::meter::unauthored_emails(line.as_str(), &[]).is_empty(),
            "no non-authored email may appear (#15): {line}"
        );
        assert!(!line.contains("token") && !line.contains("Bearer") && !line.contains("sk-ant"));
    }

    #[test]
    fn exhausted_slow_poll_cleared_line_carries_the_uuid() {
        // The edge-triggered EXIT partner (issue #537): just the account UUID — the slow-poll
        // episode's span is bracketed by pairing this with the last ENTER line.
        let line = Event::ExhaustedSlowPollCleared {
            account: "u-A".to_owned(),
        }
        .to_log_line(at_epoch(0));
        assert_eq!(
            line,
            format!("{TS0} event=exhausted_slow_poll_cleared acct=u-A")
        );
    }

    #[test]
    fn near_limit_poll_coverage_line_carries_the_uuid_and_cadence() {
        // The durable band-ENTER line (issue #540): the active account UUID (not a label, #15) and
        // the tightened near-limit poll cadence — redacted to uuid + cadence ONLY, the same
        // single-surface discipline as its `exhausted_slow_poll` sibling. No token/email surface
        // exists (the mirror-image sibling of `exhausted_slow_poll_line_carries_the_uuid_and_window`).
        let line = Event::NearLimitPollCoverage {
            account: "u-A".to_owned(),
            sub_interval_secs: 60,
        }
        .to_log_line(at_epoch(0));
        assert_eq!(
            line,
            format!("{TS0} event=near_limit_poll_coverage acct=u-A sub_interval_secs=60")
        );
        // #15: no non-authored email, no token/bearer/api-key, and the identity is the UUID.
        assert!(
            crate::redaction::meter::unauthored_emails(line.as_str(), &[]).is_empty(),
            "no non-authored email may appear (#15): {line}"
        );
        assert!(!line.contains("token") && !line.contains("Bearer") && !line.contains("sk-ant"));
    }

    #[test]
    fn usage_velocity_line_normalizes_the_signed_deltas_to_percent_per_minute() {
        // A climbing account, normalized to %/min (issue #449): session +7 over a 300 s (5 min)
        // interval renders `session_pct_per_min=1.40` (7 / 5), weekly +2 → `0.40` — the raw deltas +
        // interval trail so the exact measurement stays recoverable for the #451 spike.
        let climbing = Event::UsageVelocity {
            account: "u-A".to_owned(),
            session_delta_pct: 7,
            weekly_delta_pct: 2,
            elapsed_secs: 300,
        }
        .to_log_line(at_epoch(0));
        assert_eq!(
            climbing,
            format!("{TS0} event=usage_velocity acct=u-A session_pct_per_min=1.40 weekly_pct_per_min=0.40 elapsed_secs=300 session_delta_pct=7 weekly_delta_pct=2")
        );

        // A window reset dropped the reading over a 60 s interval: a NEGATIVE rate renders with its
        // `-` sign (−92 / 1 min = −92.00), a `key=val` token existing parsers read as-is.
        let reset = Event::UsageVelocity {
            account: "u-A".to_owned(),
            session_delta_pct: -92,
            weekly_delta_pct: 0,
            elapsed_secs: 60,
        }
        .to_log_line(at_epoch(0));
        assert_eq!(
            reset,
            format!("{TS0} event=usage_velocity acct=u-A session_pct_per_min=-92.00 weekly_pct_per_min=0.00 elapsed_secs=60 session_delta_pct=-92 weekly_delta_pct=0")
        );
    }

    #[test]
    fn usage_velocity_rate_depends_on_the_interval_not_just_the_delta() {
        // The point of the #449 normalization: the SAME +6 delta is a different %/min depending on
        // how long it took. Over 1 min it is 6.00 %/min; over 6 min it is 1.00 %/min — the durable
        // log now distinguishes a fast burn from a slow climb that a raw delta alone conflated.
        let fast = Event::UsageVelocity {
            account: "u-A".to_owned(),
            session_delta_pct: 6,
            weekly_delta_pct: 0,
            elapsed_secs: 60,
        }
        .to_log_line(at_epoch(0));
        let slow = Event::UsageVelocity {
            account: "u-A".to_owned(),
            session_delta_pct: 6,
            weekly_delta_pct: 0,
            elapsed_secs: 360,
        }
        .to_log_line(at_epoch(0));
        assert!(fast.contains("session_pct_per_min=6.00"), "{fast}");
        assert!(slow.contains("session_pct_per_min=1.00"), "{slow}");
    }

    #[test]
    fn blind_window_line_carries_duration_anchor_pct_recovery_pct_and_near_limit() {
        // The active account recovered after 8 min (480 s) blind while its pre-blind anchor sat at
        // 96 % — in the risk band, so `near_limit=true` — and read live at 98 % on recovery (still
        // climbing above the anchor → a stale-anchor preemptive swap would have been NECESSARY,
        // issue #482). Three umbrella #363 SLIs read off this line: the blind-window duration,
        // time-blind-near-limit (filtered to `near_limit=true`), and post-recovery swap-necessity
        // (`session_at_recovery` vs the `session_pct` anchor).
        let line = Event::BlindWindow {
            account: "u-A".to_owned(),
            duration_secs: 480,
            session_pct: 96,
            session_at_recovery: 98,
            near_limit: true,
            // No retained velocity and no stale-low mark — the line is byte-for-byte the pre-#634 /
            // pre-#670 shape (both are additive optional tokens).
            velocity: None,
            session_high_water_pct: None,
        }
        .to_log_line(at_epoch(0));
        assert_eq!(
            line,
            format!("{TS0} event=blind_window acct=u-A duration_secs=480 session_pct=96 session_at_recovery=98 near_limit=true")
        );
    }

    #[test]
    fn blind_window_line_marks_a_below_band_recovery_not_near_limit() {
        // A blind window whose anchor was comfortably below the trigger (40 %) is STILL recorded —
        // the spike wants the full distribution — but `near_limit=false` keeps it out of the
        // time-blind-near-limit sum. Recovery read 42 % (near the anchor, never climbed toward the
        // ceiling): the #482 recovery pct is recorded regardless of the near-limit tag.
        let line = Event::BlindWindow {
            account: "u-B".to_owned(),
            duration_secs: 90,
            session_pct: 40,
            session_at_recovery: 42,
            near_limit: false,
            velocity: None,
            session_high_water_pct: None,
        }
        .to_log_line(at_epoch(0));
        assert_eq!(
            line,
            format!("{TS0} event=blind_window acct=u-B duration_secs=90 session_pct=40 session_at_recovery=42 near_limit=false")
        );
    }

    #[test]
    fn blind_window_line_appends_the_retained_velocity_ingredients() {
        // Issue #634: the retained #539 velocity in force through the window trails the line, so the
        // REPORT-ONLY blind velocity-projection arm (#584/#600) — which fires no swap and emits no
        // event of its own — is reconstructable OFFLINE from this one line. Here a 29 % anchor was
        // blind for 8 min (480 s) at 0.05 %/s, inflated 1.75× against the 95 % base ceiling:
        // `projected = 29 + 0.05 × 1.75 × 480 = 71 %` — below the ceiling, so the arm would NOT have
        // armed; the log records the ingredients regardless so the offline reader draws that
        // conclusion itself. `rate` is FULL PRECISION (6 decimals) — a rounded rate cannot reproduce
        // the projection — and both CONSTANTS (inflation, ceiling) are stamped so a drift in either
        // cannot make this old record read wrong. Redaction-clean (#15): bare numbers only.
        let line = Event::BlindWindow {
            account: "u-A".to_owned(),
            duration_secs: 480,
            session_pct: 29,
            session_at_recovery: 61,
            near_limit: false,
            velocity: Some(BlindVelocity {
                rate_pct_per_sec: 0.05,
                inflation: 1.75,
                ceiling_pct: 95.0,
            }),
            // A plausible anchor (no stale-low correction) — the #670 mark token is omitted, so the
            // #634 velocity tokens remain the tail exactly as before.
            session_high_water_pct: None,
        }
        .to_log_line(at_epoch(0));
        assert_eq!(
            line,
            format!(
                "{TS0} event=blind_window acct=u-A duration_secs=480 session_pct=29 session_at_recovery=61 near_limit=false rate=0.050000 inflation=1.75 ceiling=95.00"
            )
        );
        assert!(
            crate::redaction::meter::unauthored_emails(line.as_str(), &[]).is_empty(),
            "no email sigil (#15): {line}"
        );
        assert!(
            !line.to_lowercase().contains("token"),
            "no token (#15): {line}"
        );
    }

    #[test]
    fn blind_window_line_appends_the_stale_low_corrected_mark() {
        // Issue #670: when the pre-blind anchor was STALE-LOW, the frozen window high-water mark —
        // the base the #632-corrected live arm actually decides on — trails the line as ONE more
        // additive token, AFTER the #634 velocity trio. The offline recompute becomes
        // `max(session_pct, session_high_water_pct) + rate × inflation × duration_secs`:
        // `max(20, 50) + 0.05 × 1.75 × 600 = 102.5 %` — at/over the 95 % ceiling, so the arm WAS
        // armed — where the raw base reads `72.5 %`, below it, and an offline reader would conclude
        // "the arm never armed" on a window the corrected arm armed on. `session_pct` stays the RAW
        // measurement (the #614/#619/#632 read-time-only contract); the mark is the additive
        // correction term beside it.
        let line = Event::BlindWindow {
            account: "u-A".to_owned(),
            duration_secs: 600,
            session_pct: 20,
            session_at_recovery: 55,
            near_limit: false,
            velocity: Some(BlindVelocity {
                rate_pct_per_sec: 0.05,
                inflation: 1.75,
                ceiling_pct: 95.0,
            }),
            session_high_water_pct: Some(50),
        }
        .to_log_line(at_epoch(0));
        assert_eq!(
            line,
            format!(
                "{TS0} event=blind_window acct=u-A duration_secs=600 session_pct=20 session_at_recovery=55 near_limit=false rate=0.050000 inflation=1.75 ceiling=95.00 session_high_water_pct=50"
            )
        );
        assert!(
            crate::redaction::meter::unauthored_emails(line.as_str(), &[]).is_empty(),
            "no email sigil (#15): {line}"
        );
        assert!(
            !line.to_lowercase().contains("token"),
            "no token (#15): {line}"
        );
    }

    #[test]
    fn blind_window_line_carries_the_mark_without_velocity() {
        // Issue #670: the mark is ORTHOGONAL to the #634 velocity trio — a stale-low anchor is a
        // fact about the window whether or not a sustained EMA was retained. With no velocity the
        // mark trails `near_limit` directly; the `blind_projection_error` reader still counts this
        // line `without_velocity` (no `rate=` token), so the census partition is untouched.
        let line = Event::BlindWindow {
            account: "u-B".to_owned(),
            duration_secs: 400,
            session_pct: 20,
            session_at_recovery: 55,
            near_limit: false,
            velocity: None,
            session_high_water_pct: Some(50),
        }
        .to_log_line(at_epoch(0));
        assert_eq!(
            line,
            format!(
                "{TS0} event=blind_window acct=u-B duration_secs=400 session_pct=20 session_at_recovery=55 near_limit=false session_high_water_pct=50"
            )
        );
    }

    #[test]
    fn blind_enter_line_opens_the_episode_with_both_windows() {
        // The uncensored pair's OPEN (issue #583): emitted the instant the account goes dark, so an
        // episode that never recovers — the tail `blind_window`'s recovery edge structurally cannot
        // see — is still durable. Carries the pre-blind anchor in BOTH windows (the baseline
        // `blind_exit` differences against) and tags whether this was the active account.
        let line = Event::BlindEnter {
            account: "u-A".to_owned(),
            session_pct: 96,
            weekly_pct: 30,
            was_active: true,
            near_limit: true,
        }
        .to_log_line(at_epoch(0));
        assert_eq!(
            line,
            format!(
                "{TS0} event=blind_enter acct=u-A session_pct=96 weekly_pct=30 was_active=true near_limit=true"
            )
        );
    }

    #[test]
    fn blind_exit_line_derives_the_burn_in_both_windows() {
        // The uncensored pair's CLOSE (issue #583), replaying the production episode the issue was
        // filed on: last seen at session 29 / weekly 5, dark for ~2 h, back at session 0 / weekly 17.
        // The SESSION window reset behind the blindness, so `session_burn_pct=-29` reads as a quiet
        // reset — exactly the wrong story, and the only one `blind_window` (session-only) can tell.
        // `weekly_burn_pct=12` is the burn that actually happened. Both are DERIVED here from the
        // stored anchor/recovery pairs and rendered first, with the raw pcts trailing, so the line
        // answers "did it burn?" at a glance while the exact measurement stays recoverable.
        let line = Event::BlindExit {
            account: "u-A".to_owned(),
            duration_secs: 7512,
            session_pct: 29,
            session_at_recovery: 0,
            weekly_pct: 5,
            weekly_at_recovery: 17,
            was_active: true,
            swapped_away: true,
            near_limit: false,
        }
        .to_log_line(at_epoch(0));
        assert_eq!(
            line,
            format!(
                "{TS0} event=blind_exit acct=u-A duration_secs=7512 session_burn_pct=-29 weekly_burn_pct=12 session_pct=29 session_at_recovery=0 weekly_pct=5 weekly_at_recovery=17 was_active=true swapped_away=true near_limit=false"
            )
        );
    }

    #[test]
    fn the_observation_gap_pair_renders_the_gap_and_the_bound_it_crossed() {
        // Issue #1453. The pair's whole job is that ONE line answers "how long was the active
        // account unobserved?" — the post-swap first-sight latency, with no second source to join
        // against. So the ENTRY carries the gap at the crossing AND `threshold_secs`, the bound it
        // crossed: that bound is derived from config (`2 · poll_secs / N`), which appears nowhere
        // else on this channel, so without it a reader cannot tell a real breach from a re-tuned
        // bound. The figures here are the live configuration's — 75 s at `poll_secs=300`, `N=8`.
        let enter = Event::ObservationGapEnter {
            account: "u-B".to_owned(),
            elapsed_secs: 76,
            threshold_secs: 75,
            was_active: true,
        }
        .to_log_line(at_epoch(0));
        assert_eq!(
            enter,
            format!(
                "{TS0} event=observation_gap_enter acct=u-B elapsed_secs=76 threshold_secs=75 was_active=true"
            )
        );

        // The CLOSE carries the WHOLE gap — 638 s, the worst episode actually measured — not the
        // increment since the entry, so the latency is read off this line alone. `swapped_away=false`
        // is what makes it a first sight of a still-active account, and so a usable SLI sample.
        let exit = Event::ObservationGapExit {
            account: "u-B".to_owned(),
            elapsed_secs: 638,
            was_active: true,
            swapped_away: false,
        }
        .to_log_line(at_epoch(0));
        assert_eq!(
            exit,
            format!(
                "{TS0} event=observation_gap_exit acct=u-B elapsed_secs=638 was_active=true swapped_away=false"
            )
        );
    }

    #[test]
    fn an_observation_gap_that_outlived_its_active_says_so() {
        // The tail a naive reading of the pair would over-count: the gap opened on the active
        // account and closed only after the daemon had already swapped away, so the account was
        // observed as a PEER. It is a real episode — the account genuinely went unseen — but it is
        // not a first sight of the current active, and folding it into the latency SLI would
        // overstate that figure. The line says which it is rather than leaving it to be inferred
        // from a join against the swap stream.
        let line = Event::ObservationGapExit {
            account: "u-C".to_owned(),
            elapsed_secs: 420,
            was_active: true,
            swapped_away: true,
        }
        .to_log_line(at_epoch(0));
        assert_eq!(
            line,
            format!(
                "{TS0} event=observation_gap_exit acct=u-C elapsed_secs=420 was_active=true swapped_away=true"
            )
        );
    }

    #[test]
    fn blind_exit_line_reports_a_zero_burn_when_nothing_moved() {
        // The negative the SLI needs just as much: an account that went blind and came back with both
        // windows untouched burned NOTHING (`session_burn_pct=0 weekly_burn_pct=0`) — a bounded
        // blindness that cost nothing, and the case #484's bar must be able to tell apart from the
        // one above when it re-derives the gate constants. `swapped_away=false` on an episode the
        // daemon stayed on throughout.
        let line = Event::BlindExit {
            account: "u-B".to_owned(),
            duration_secs: 120,
            session_pct: 40,
            session_at_recovery: 40,
            weekly_pct: 12,
            weekly_at_recovery: 12,
            was_active: true,
            swapped_away: false,
            near_limit: false,
        }
        .to_log_line(at_epoch(0));
        assert_eq!(
            line,
            format!(
                "{TS0} event=blind_exit acct=u-B duration_secs=120 session_burn_pct=0 weekly_burn_pct=0 session_pct=40 session_at_recovery=40 weekly_pct=12 weekly_at_recovery=12 was_active=true swapped_away=false near_limit=false"
            )
        );
    }

    #[test]
    fn blind_gate_eligible_line_carries_viable_target_blind_secs_and_anchor_pct() {
        // The #452 preemptive gate turned eligible for the active account after 360 s blind with its
        // anchor at 70 % (in the interim risk band) — and a viable swap target WAS present, so the
        // ADR-0017 cost-asymmetry premise holds this time (`viable_target=true`, issue #482).
        let present = Event::BlindGateEligible {
            account: "u-A".to_owned(),
            viable_target: true,
            blind_secs: 360,
            session_pct: 70,
        }
        .to_log_line(at_epoch(0));
        assert_eq!(
            present,
            format!("{TS0} event=blind_gate_eligible acct=u-A viable_target=true blind_secs=360 session_pct=70")
        );
        // The FALSIFIER case: the gate was eligible but NO viable target existed — the premise's
        // counter-evidence, the whole reason #482 measures at gate-fire and not only at swap.
        let absent = Event::BlindGateEligible {
            account: "u-B".to_owned(),
            viable_target: false,
            blind_secs: 900,
            session_pct: 88,
        }
        .to_log_line(at_epoch(0));
        assert_eq!(
            absent,
            format!("{TS0} event=blind_gate_eligible acct=u-B viable_target=false blind_secs=900 session_pct=88")
        );
    }

    #[test]
    fn blind_preempt_reserve_hold_line_carries_the_directive_and_blind_duration() {
        // Issue #582: the swap-away was armed by a `Retry-After: 3600` on the active account but
        // HELD — firing would have spent the last viable target, which a speculative swap must
        // yield to a confirmed-exhaustion one. The line is the AC's "reported, not hidden".
        let line = Event::BlindPreemptReserveHold {
            account: "u-A".to_owned(),
            retry_after_secs: 3600,
            blind_secs: 301,
        }
        .to_log_line(at_epoch(0));
        assert_eq!(
            line,
            format!(
                "{TS0} event=blind_preempt_reserve_hold acct=u-A retry_after_secs=3600 blind_secs=301"
            )
        );
    }

    #[test]
    fn retry_after_walk_line_carries_the_swap_count_and_window() {
        // Issue #582: the throttle is following the ACTIVE ROLE around the roster — two
        // server-throttled preemptive swaps inside the hour — so rotation STOPPED. The alarm names
        // the evidence (`swaps` inside `window_secs`) and the directive on the account being held.
        let line = Event::RetryAfterWalk {
            account: "u-A".to_owned(),
            swaps: 2,
            window_secs: 3600,
            retry_after_secs: 3600,
        }
        .to_log_line(at_epoch(0));
        assert_eq!(
            line,
            format!(
                "{TS0} event=retry_after_walk acct=u-A swaps=2 window_secs=3600 retry_after_secs=3600"
            )
        );
    }

    /// Issue #880 (AC-1): the durable horizon-entry line. `expires_at` renders as whole-second
    /// RFC 3339 through the same formatter as the line `ts`, and `horizon_secs` carries the lookahead
    /// the verdict was reached against so the line explains itself.
    ///
    /// The deadline is the REAL observed capture issue #878 pinned at the extractor
    /// (`1785499802819` ms, account prefix `94f27044`), folded to the epoch SECONDS this event
    /// carries — so the fixture vocabulary stays continuous across the two items rather than
    /// inventing a value that merely satisfies the formatter.
    #[test]
    fn credential_expiry_horizon_line_carries_the_band_deadline_and_lookahead() {
        let line = Event::CredentialExpiryHorizon {
            account: "u-A".to_owned(),
            state: ExpiryHorizon::Within,
            expires_at: 1_785_499_802,
            horizon_secs: 604_800,
        }
        .to_log_line(at_epoch(0));
        assert_eq!(
            line,
            format!(
                "{TS0} event=credential_expiry_horizon acct=u-A state=within expires_at=2026-07-31T12:10:02Z horizon_secs=604800"
            )
        );
    }

    /// Issue #880: the ESCALATION within the actionable band renders its own token. A daemon that
    /// first meets an already-lapsed account emits this directly, with no preceding `within` — which
    /// is why `state=` is a field rather than the entry being implicit in the event name.
    #[test]
    fn credential_expiry_horizon_line_renders_the_lapsed_escalation() {
        let line = Event::CredentialExpiryHorizon {
            account: "u-A".to_owned(),
            state: ExpiryHorizon::Lapsed,
            expires_at: 1_785_499_802,
            horizon_secs: 604_800,
        }
        .to_log_line(at_epoch(0));
        assert_eq!(
            line,
            format!(
                "{TS0} event=credential_expiry_horizon acct=u-A state=lapsed expires_at=2026-07-31T12:10:02Z horizon_secs=604800"
            )
        );
    }

    /// Issue #880: every [`ExpiryHorizon`] token this event can carry agrees with the `snake_case`
    /// serde spelling issue #882 will put on the `--json` wire.
    ///
    /// Pinned against the SERDE output rather than against a hand-copied token list, so the two
    /// spellings cannot drift: a renamed variant, or an `as_str` arm edited in isolation, fails here.
    /// Issue #878's doc makes this agreement an explicit promise ("the tokens an event log and a
    /// `--json` field would carry agree by construction"); this is the test that keeps it one.
    #[test]
    fn expiry_horizon_tokens_match_their_serde_spelling() {
        for state in [
            ExpiryHorizon::Unknown,
            ExpiryHorizon::Beyond,
            ExpiryHorizon::Within,
            ExpiryHorizon::Lapsed,
        ] {
            let serde_token = serde_json::to_string(&state).expect("a bare enum serializes");
            assert_eq!(
                format!("\"{}\"", state.as_str()),
                serde_token,
                "as_str must match the serde rename: {state:?}"
            );
        }
    }

    /// Issue #880 (AC-2): a FIRST observation is an anchor, not a change — it carries `after` alone,
    /// with no `before` and therefore no derived `delta_secs`. Rendering an empty `before=` would
    /// split the `key=val` grammar, so the key is OMITTED entirely, exactly as `event=refresh`
    /// handles an unreadable expiry.
    #[test]
    fn credential_expiry_observed_line_omits_an_absent_baseline() {
        let line = Event::CredentialExpiryObserved {
            account: "u-A".to_owned(),
            provenance: ExpiryProvenance::FirstObservation,
            before: None,
            after: 1_785_499_802,
            grant_replaced: None,
        }
        .to_log_line(at_epoch(0));
        assert_eq!(
            line,
            format!(
                "{TS0} event=credential_expiry_observed acct=u-A provenance=first_observation after=2026-07-31T12:10:02Z"
            )
        );
    }

    /// Issue #880 (AC-2): a change the daemon's OWN refresh caused carries both deadlines and the
    /// DERIVED forward delta — so an operator reads "the server extended it by a day" off the line
    /// without subtracting two timestamps, the same store-the-ingredients-derive-the-view idiom as
    /// `event=refresh`'s `window_secs`.
    #[test]
    fn credential_expiry_observed_line_derives_the_delta_from_both_deadlines() {
        let line = Event::CredentialExpiryObserved {
            account: "u-A".to_owned(),
            provenance: ExpiryProvenance::MyRefresh,
            before: Some(1_785_499_802),
            after: 1_785_586_202,
            grant_replaced: None,
        }
        .to_log_line(at_epoch(0));
        assert_eq!(
            line,
            format!(
                "{TS0} event=credential_expiry_observed acct=u-A provenance=my_refresh before=2026-07-31T12:10:02Z after=2026-08-01T12:10:02Z delta_secs=86400"
            )
        );
    }

    /// Issue #880 (AC-3, the third row of the issue #877 table): an external credential write that
    /// minted a NEW grant and did NOT move the deadline. `delta_secs=0 grant_replaced=true` IS
    /// that row — [`ExpiryProvenance::CanonicalRestash`] carries what turns on it.
    ///
    /// Both halves are load-bearing, which is why this pins the whole line. `delta_secs=0` alone says
    /// only *something rewrote the credential and the deadline held* — true of an unrelated blob edit
    /// that never issued a grant, and #877 is not asking about those. `grant_replaced=true` is what makes
    /// the line evidence about RE-LOGIN behaviour specifically (narrowing, not deciding — see the
    /// variant's own doc).
    ///
    /// This is the case a change-only record structurally cannot express — its evidence is the
    /// absence of a change — which is why the provenance exists and why the fold emits it even
    /// though nothing moved.
    #[test]
    fn credential_expiry_observed_line_records_an_unmoved_deadline_across_an_external_write() {
        let line = Event::CredentialExpiryObserved {
            account: "u-A".to_owned(),
            provenance: ExpiryProvenance::CanonicalRestash,
            before: Some(1_785_499_802),
            after: 1_785_499_802,
            grant_replaced: Some(true),
        }
        .to_log_line(at_epoch(0));
        assert_eq!(
            line,
            format!(
                "{TS0} event=credential_expiry_observed acct=u-A provenance=canonical_restash before=2026-07-31T12:10:02Z after=2026-07-31T12:10:02Z delta_secs=0 grant_replaced=true"
            )
        );
    }

    /// Issue #880: `grant_replaced=false` renders as the explicit NEGATIVE rather than being folded in
    /// with the unknown case. It is the one direction of this field that is a hard exclusion — a
    /// byte-identical refresh token cannot have come from a fresh login — so collapsing it into the
    /// omitted-when-unknown branch would discard the field's only conclusive reading.
    #[test]
    fn credential_expiry_observed_line_distinguishes_an_unreplaced_grant_from_an_unknown_one() {
        let same_grant = Event::CredentialExpiryObserved {
            account: "u-A".to_owned(),
            provenance: ExpiryProvenance::CanonicalRestash,
            before: Some(1_785_499_802),
            after: 1_785_499_802,
            grant_replaced: Some(false),
        }
        .to_log_line(at_epoch(0));
        assert!(
            same_grant.ends_with(" delta_secs=0 grant_replaced=false"),
            "a same-grant write says so out loud: {same_grant}"
        );

        let unknown = Event::CredentialExpiryObserved {
            account: "u-A".to_owned(),
            provenance: ExpiryProvenance::CanonicalRestash,
            before: Some(1_785_499_802),
            after: 1_785_499_802,
            grant_replaced: None,
        }
        .to_log_line(at_epoch(0));
        assert!(
            unknown.ends_with(" delta_secs=0"),
            "an unknown is OMITTED, never rendered as a value: {unknown}"
        );
    }

    /// Issue #880: a deadline pulled EARLIER renders a negative delta rather than saturating to
    /// zero. The direction is diagnostic — a shrinking deadline is the opposite operational story
    /// from a server extension — so a `u64` subtraction that clamped it would erase the signal while
    /// still looking plausible on the line.
    #[test]
    fn credential_expiry_observed_line_renders_a_backwards_delta_signed() {
        let line = Event::CredentialExpiryObserved {
            account: "u-A".to_owned(),
            provenance: ExpiryProvenance::ExternalChange,
            before: Some(1_785_586_202),
            after: 1_785_499_802,
            grant_replaced: None,
        }
        .to_log_line(at_epoch(0));
        assert!(
            line.ends_with(" delta_secs=-86400"),
            "a shrinking deadline keeps its sign: {line}"
        );
    }

    /// Issue #907: the cross-source observation renders its OWN token, and the provenance set stays
    /// a closed list of distinct, fixed `snake_case` literals.
    ///
    /// The token is the analysis interface — issue #877's population is partitioned by
    /// `provenance=`, exactly as `the_three_provenance_rows_are_distinguishable_from_the_rendered_
    /// log_alone` reads it — so a delta that spanned two credential items has to be filterable off
    /// the rendered line, not merely distinguishable in memory.
    ///
    /// The EXHAUSTIVE match below is the mechanical half, the same idiom as the `CredentialHealth`
    /// ramp guard in [`crate::daemon`]: a fifth variant fails to COMPILE here, so no provenance can
    /// reach the log without someone stating its token (and extending the list beside it). The
    /// literals are spelled out again rather than routed through `as_str`, which would be a
    /// tautology — this is the second, independent statement of the mapping. That each arm is a
    /// LITERAL is itself the issue #15 guarantee: the `provenance=` value is never dynamic text, so
    /// the line cannot carry a secret by construction rather than by review.
    #[test]
    fn the_expiry_provenance_tokens_are_a_closed_set_of_fixed_snake_case_literals() {
        let line = Event::CredentialExpiryObserved {
            account: "u-A".to_owned(),
            provenance: ExpiryProvenance::ReadSourceSwitched,
            before: Some(1_785_499_802),
            after: 1_785_586_202,
            grant_replaced: None,
        }
        .to_log_line(at_epoch(0));
        assert_eq!(
            line,
            format!(
                "{TS0} event=credential_expiry_observed acct=u-A provenance=read_source_switched before=2026-07-31T12:10:02Z after=2026-08-01T12:10:02Z delta_secs=86400"
            )
        );

        let mut tokens = Vec::new();
        for provenance in [
            ExpiryProvenance::FirstObservation,
            ExpiryProvenance::MyRefresh,
            ExpiryProvenance::ExternalChange,
            ExpiryProvenance::ReadSourceSwitched,
            ExpiryProvenance::CanonicalRestash,
        ] {
            let expected = match provenance {
                ExpiryProvenance::FirstObservation => "first_observation",
                ExpiryProvenance::MyRefresh => "my_refresh",
                ExpiryProvenance::ExternalChange => "external_change",
                ExpiryProvenance::ReadSourceSwitched => "read_source_switched",
                ExpiryProvenance::CanonicalRestash => "canonical_restash",
            };
            assert_eq!(provenance.as_str(), expected, "{provenance:?}");
            assert!(
                expected
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b == b'_'),
                "a provenance token is bare `snake_case`: {expected}"
            );
            // The same blunt rule `no_event_line_carries_an_email_or_token_sigil` applies to every
            // line, restated on the token itself so a respelling is rejected where it is CHOSEN
            // rather than three tests away — see `grant_replaced`'s doc for why that check earns
            // its bluntness and must not be relaxed to accommodate a name.
            assert!(!expected.contains("token"), "{expected}");
            tokens.push(expected);
        }
        let mut distinct = tokens.clone();
        distinct.sort_unstable();
        distinct.dedup();
        assert_eq!(
            distinct.len(),
            tokens.len(),
            "two provenances sharing a token would silently merge two populations: {tokens:?}"
        );
    }

    /// Issue #880: the #15 guarantee for both new lines, mirroring `the_durable_582_lines_carry_no_pii`
    /// — every field is a UUID, an enum token, a bare number or a timestamp; never an email, a token,
    /// or the free-form operator `label`. The deadlines are the ONLY credential-derived values, and
    /// `crate::refresh::refresh_token_expires_at` reads exactly those two integers and never the
    /// token they belong to.
    #[test]
    fn the_durable_880_lines_carry_no_pii() {
        let lines = [
            Event::CredentialExpiryHorizon {
                account: "u-A".to_owned(),
                state: ExpiryHorizon::Within,
                expires_at: 1_785_499_802,
                horizon_secs: 604_800,
            },
            Event::CredentialExpiryObserved {
                account: "u-A".to_owned(),
                provenance: ExpiryProvenance::MyRefresh,
                before: Some(1_785_499_802),
                after: 1_785_586_202,
                grant_replaced: None,
            },
            // The SAME variant with `grant_replaced` PRESENT. Listed separately on purpose: that field
            // is the only one here derived from the refresh token itself, so a sweep that only ever
            // rendered it as the omitted `None` would be exercising the wrong shape — it would pass
            // while saying nothing about the field whose provenance most warrants the check.
            Event::CredentialExpiryObserved {
                account: "u-A".to_owned(),
                provenance: ExpiryProvenance::CanonicalRestash,
                before: Some(1_785_499_802),
                after: 1_785_499_802,
                grant_replaced: Some(true),
            },
        ];
        let rendered: Vec<String> = lines
            .iter()
            .map(|event| event.to_log_line(at_epoch(0)))
            .collect();
        for line in &rendered {
            assert!(!line.contains('@'), "no email: {line}");
            // The SAME blunt whole-line check `no_event_line_carries_an_email_or_token_sigil` applies
            // to every variant, restated here because these two lines are the file's only
            // credential-DERIVED payload and because the check constrains the field NAMES as well as
            // their values — which is why the grant verdict is spelled `grant_replaced` (see the
            // variant's doc) rather than for the refresh token it is computed from.
            assert!(!line.to_lowercase().contains("token"), "no token: {line}");
            assert!(!line.contains("sk-ant"), "no credential sigil: {line}");
            assert!(line.contains("acct=u-A"), "uuid identity: {line}");
        }
        // NON-DEGENERATE on the field that most warrants the check: `grant_replaced` is the only one
        // here derived from the refresh token itself, and it is OMITTED on two of these three lines.
        // Without this count, deleting the one event that carries it would leave the loop above
        // passing while asserting nothing at all about it.
        let carrying: Vec<&String> = rendered
            .iter()
            .filter(|l| l.contains(" grant_replaced="))
            .collect();
        assert_eq!(
            carrying.len(),
            1,
            "exactly one line must exercise the rendered grant verdict: {rendered:?}"
        );
        assert!(
            carrying[0].ends_with(" grant_replaced=true"),
            "and it renders as a bare bool: {}",
            carrying[0]
        );
    }

    #[test]
    fn the_durable_582_lines_carry_no_pii() {
        // The #15 guarantee for the #582 signals, mirroring `the_durable_399_lines_carry_no_pii`:
        // every field is a UUID or a bare number — never an email, token, or the free-form operator
        // `label`. The identity is `acct=<uuid>`, secret-free BY CONSTRUCTION.
        let lines = [
            Event::BlindPreemptReserveHold {
                account: "u-A".to_owned(),
                retry_after_secs: 3600,
                blind_secs: 301,
            }
            .to_log_line(at_epoch(0)),
            Event::RetryAfterWalk {
                account: "u-A".to_owned(),
                swaps: 2,
                window_secs: 3600,
                retry_after_secs: 3600,
            }
            .to_log_line(at_epoch(0)),
        ];
        for line in &lines {
            // A positive identity check the sibling PII tests omit: the account handle is present.
            assert!(line.contains("acct=u-A"), "identity is the UUID: {line}");
            // The SAME battery as `the_durable_399_lines_carry_no_pii`, so the two genuinely mirror:
            // the project's real email detector (#444), then the credential-field probes, then the
            // structural check that no free-form `label=` field exists to leak an operator handle.
            assert!(
                crate::redaction::meter::unauthored_emails(line.as_str(), &[]).is_empty(),
                "no non-authored email may appear (#15/#444): {line}"
            );
            assert!(!line.contains("token"), "no token may appear: {line}");
            assert!(!line.contains("Bearer"), "no bearer may appear: {line}");
            assert!(!line.contains("sk-ant"), "no api key may appear: {line}");
            assert!(!line.contains("label="), "no operator label: {line}");
        }
    }

    #[test]
    fn the_durable_399_lines_carry_no_pii() {
        // The #15 guarantee for the #399 signals: every field is a UUID / closed-enum token /
        // number — never an email, token, or the free-form operator `label`. The identity is
        // `acct=<uuid>`, secret-free BY CONSTRUCTION.
        let lines = [
            Event::UsageBackoff {
                account: "u-A".to_owned(),
                class: BackoffClass::RateLimited,
                consecutive: 2,
                retry_after_secs: Some(300),
                backoff_secs: 300,
            }
            .to_log_line(at_epoch(0)),
            Event::UsageBackoffCleared {
                account: "u-A".to_owned(),
            }
            .to_log_line(at_epoch(0)),
            Event::UsageVelocity {
                account: "u-A".to_owned(),
                session_delta_pct: -5,
                weekly_delta_pct: 1,
                elapsed_secs: 120,
            }
            .to_log_line(at_epoch(0)),
            Event::BlindWindow {
                account: "u-A".to_owned(),
                duration_secs: 300,
                session_pct: 97,
                session_at_recovery: 99,
                near_limit: true,
                // Issue #634: the velocity ingredients ride this #15/#444 sweep too — bare numbers,
                // no email/token/label surface added.
                velocity: Some(BlindVelocity {
                    rate_pct_per_sec: 0.045,
                    inflation: 1.75,
                    ceiling_pct: 95.0,
                }),
                session_high_water_pct: None,
            }
            .to_log_line(at_epoch(0)),
            Event::BlindEnter {
                account: "u-A".to_owned(),
                session_pct: 97,
                weekly_pct: 40,
                was_active: true,
                near_limit: true,
            }
            .to_log_line(at_epoch(0)),
            Event::BlindExit {
                account: "u-A".to_owned(),
                duration_secs: 300,
                session_pct: 97,
                session_at_recovery: 99,
                weekly_pct: 40,
                weekly_at_recovery: 44,
                was_active: true,
                swapped_away: true,
                near_limit: true,
            }
            .to_log_line(at_epoch(0)),
            Event::BlindGateEligible {
                account: "u-A".to_owned(),
                viable_target: false,
                blind_secs: 600,
                session_pct: 88,
            }
            .to_log_line(at_epoch(0)),
        ];
        for line in &lines {
            assert!(
                crate::redaction::meter::unauthored_emails(line.as_str(), &[]).is_empty(),
                "no non-authored email may appear (#15/#444): {line}"
            );
            assert!(!line.contains("token"), "no token may appear: {line}");
            assert!(!line.contains("Bearer"), "no bearer may appear: {line}");
            assert!(!line.contains("sk-ant"), "no api key may appear: {line}");
            // The identity is the uuid handle only — never the free-form `label=` field.
            assert!(!line.contains("label="), "no operator label: {line}");
        }
    }

    // --- Diagnostic::to_log_line (the diagnostic channel's redaction surface, #77) ---

    #[test]
    fn start_line_renders_the_effective_config_summary() {
        // target_max_session_usage (#398, always-valued) renders as its percent, like the rest —
        // counts and percentages only, no handle.
        let line = Diagnostic::Start {
            accounts: 3,
            poll_secs: 30,
            target_max_session_usage: 70,
            session_ceiling: 90,
            weekly_ceiling: 98,
            monitor_401_n: 5,
            monitor_recovery_m: 4,
        }
        .to_log_line(at_epoch(0));
        assert_eq!(
            line,
            format!(
                "{TS0} diag=start accounts=3 poll_secs=30 target_max_session_usage=70 \
                 session_ceiling=90 weekly_ceiling=98 monitor_401_n=5 monitor_recovery_m=4"
            )
        );
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
            (DecisionClass::CanonicalAdopted, "canonical_adopted"),
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

    /// This `Diagnostic`'s variant name, spelled as the enum declares it.
    ///
    /// Layer 1 of the issue #891 guarantee for the diagnostic channel — the sibling of
    /// `event_variant_name`; see the two-layer note above `declared_variant_names`.
    fn diagnostic_variant_name(diagnostic: &Diagnostic) -> &'static str {
        match diagnostic {
            Diagnostic::Start { .. } => "Start",
            Diagnostic::Stop => "Stop",
            Diagnostic::Poll { .. } => "Poll",
            Diagnostic::Tick { .. } => "Tick",
            Diagnostic::Canonical { .. } => "Canonical",
        }
    }

    /// Every `Diagnostic` variant, as the subject of the issue #15 sweep below — totality
    /// asserted before returning, as in `every_event_variant`.
    fn every_diagnostic_variant() -> Vec<Diagnostic> {
        let samples = vec![
            Diagnostic::Start {
                accounts: 2,
                poll_secs: 30,
                target_max_session_usage: 70,
                session_ceiling: 90,
                weekly_ceiling: 98,
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
            // Issue #464, listed by issue #890: the identifier-shaped variant — a hash-prefix
            // fingerprint, an operator handle, a timestamp. All four optional fields are
            // populated (a live `Present` read), so the issue #475 rotation-yank marker's
            // trailing `prev=<prior-fingerprint>` — a second fingerprint — rides the scan too.
            Diagnostic::Canonical {
                state: CanonicalLiveness::Present,
                fingerprint: Some("0123456789abcdef".to_owned()),
                account: Some("work".to_owned()),
                expires_at: Some(1_782_777_600),
                rotated_from: Some("fedcba9876543210".to_owned()),
            },
        ];
        assert_samples_every_variant("Diagnostic", &samples, diagnostic_variant_name);
        samples
    }

    #[test]
    fn no_diagnostic_line_carries_an_email_or_token_sigil() {
        // #15: every diagnostic field is a handle / enum / number / timestamp / hash prefix,
        // so a token or email can never reach a rendered line. Mirrors the event-log guard,
        // and is total over `Diagnostic` by the same two-layer mechanism (issue #891).
        for diagnostic in &every_diagnostic_variant() {
            let line = diagnostic.to_log_line(at_epoch(0));
            assert!(
                crate::redaction::meter::unauthored_emails(line.as_str(), &[]).is_empty(),
                "no non-authored email sigil (#15/#444): {line}"
            );
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
