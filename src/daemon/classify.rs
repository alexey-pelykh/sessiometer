// Copyright (c) 2026 Oleksii PELYKH
// SPDX-License-Identifier: MIT

//! Pure poll- and failure-CLASSIFICATION: the free functions that fold a raw outcome onto the typed
//! axis its consumer reasons about (issues #42, #76, #77, #167, #359, #399, #628).
//!
//! Four axes, deliberately separate because they answer different questions about the same
//! `Result`: liveness/death for the dead-credential health machine ([`classify_poll`]), the
//! operator-facing diagnostic taxonomy that splits a `429` out on its own ([`diag_poll_class`]),
//! the back-off signal that asks the loop to slow down ([`backoff_signal`]), and the per-account
//! usage delta between two readings ([`usage_velocity`]). Alongside them sit the engine-error →
//! redacted-wire-reason mappers the control-socket acks carry ([`classify_swap_failure`],
//! [`classify_capture_failure`], [`classify_config_set_failure`]) — secret-free by construction,
//! since each inspects only an error discriminant.
//!
//! Every one is `&self`-free and reads only its arguments, so each policy is unit-tested without a
//! `Daemon`. Extracted verbatim from `daemon` per the God-module decomposition (issue #637 step 1,
//! issue #656) — a behavior-preserving move, re-exported under `crate::daemon::*` so every existing
//! call site resolves unchanged. The state TRANSITIONS these classifications drive
//! (`note_poll_outcome`, `note_account_backoff`, the ack writers) stay in `daemon`.

use super::*;

/// The health-relevant classification of ONE account's poll this tick — the typed
/// poll outcome (issue #42) the per-account health state machine consumes. Derived
/// from the poll `Result` by [`classify_poll`]; distinct from the raw HTTP taxonomy
/// (`usage`'s status classes) in that it folds every non-liveness-bearing error into
/// one `Transient` class and separates the two liveness signals — `Live` (the
/// credential works) from `Unauthorized` (the token was rejected). "Dead" and
/// "exhausted" are not single-poll outcomes: death is the ACCUMULATION of
/// `Unauthorized` across ticks (the per-account 401 streak reaching `monitor_401_n`),
/// and exhaustion is derived from a `Live` reading's usage against the swap triggers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PollOutcome {
    /// A successful usage reading — the credential is alive. Resets the death streak;
    /// for a quarantined account, advances the recovery probe.
    Live,
    /// HTTP 401 — the stored token was rejected. Advances the consecutive-401 death
    /// streak; the Nth (`monitor_401_n`) quarantines the account.
    Unauthorized,
    /// HTTP 403 — the token authenticated but lacks the usage scope (a non-interactive
    /// setup token). NON-dead (it authenticated), surfaced distinctly (#5).
    ScopeMissing,
    /// Any other failure (5xx / network / 429 / other 4xx / keychain-locked /
    /// unreadable token / unparseable body): no liveness signal — neither advances
    /// nor, by itself, distinguishes death. Resets the death streak (a 401 streak
    /// must be unbroken).
    Transient,
}

/// Classify a poll `Result` into its [`PollOutcome`] — the typed poll outcome the
/// dead-credential health state machine consumes (issue #42). Pure: the single place
/// the HTTP error taxonomy is mapped onto the liveness/death axis, so the policy is
/// testable in isolation and `note_poll_outcome` stays a state-transition.
pub(crate) fn classify_poll(result: &Result<Usage>) -> PollOutcome {
    match result {
        Ok(_) => PollOutcome::Live,
        Err(Error::UsageUnauthorized) => PollOutcome::Unauthorized,
        Err(Error::UsageScopeMissing) => PollOutcome::ScopeMissing,
        Err(_) => PollOutcome::Transient,
    }
}

/// Classify a poll `Result` into its operator-facing [`PollClass`] for the diagnostic
/// channel (issue #77). Distinct from [`classify_poll`] in ONE place: a `429`
/// (rate-limited) is its OWN class here, where the dead-credential machine folds it
/// into `Transient` — an operator debugging a throttling storm needs to see
/// `rate_limited` rather than a generic transient (the very signal #77 surfaces). The
/// `5xx` / network / unreadable / unparseable remainder is `Transient`.
pub(crate) fn diag_poll_class(result: &Result<Usage>) -> PollClass {
    match result {
        Ok(_) => PollClass::Live,
        Err(Error::UsageUnauthorized) => PollClass::Unauthorized,
        Err(Error::UsageScopeMissing) => PollClass::Scope,
        Err(Error::UsageRateLimited { .. }) => PollClass::RateLimited,
        Err(_) => PollClass::Transient,
    }
}

/// A poll outcome that asks the loop to back off (issue #76): a `429`
/// (rate-limited) or a `5xx` / network transient. Carries the throttle `class` (issue
/// #399, so the durable back-off event can tell a `429` from a transient) and the
/// server-advised `Retry-After` the response supplied, if any.
pub(crate) struct BackoffSignal {
    pub(crate) class: BackoffClass,
    pub(crate) retry_after: Option<Duration>,
}

/// Classify a poll `Result` for the rate-limit / transient back-off (issue #76):
/// `Some` when it is a back-off outcome (`429` or `5xx` / network), carrying any
/// `Retry-After`; `None` otherwise. A success, a `401`, a `403`, or any other error
/// does NOT, by itself, widen the poll spacing. Deliberately separate from
/// [`classify_poll`] (which feeds the #42 dead-credential health machine): back-off
/// is orthogonal — a `429` both resets the 401 streak (via `classify_poll`'s
/// `Transient`) AND asks the loop to slow down (here).
pub(crate) fn backoff_signal(result: &Result<Usage>) -> Option<BackoffSignal> {
    match result {
        // The `class` (issue #399) distinguishes the two back-off outcomes so the durable
        // `usage_backoff` event can carry which one armed the window — a `429` is the
        // rate-limit the "429 count" acceptance counts, a `5xx` / network the transient.
        Err(Error::UsageRateLimited { retry_after, .. }) => Some(BackoffSignal {
            class: BackoffClass::RateLimited,
            retry_after: *retry_after,
        }),
        Err(Error::UsageTransient { retry_after, .. }) => Some(BackoffSignal {
            class: BackoffClass::Transient,
            retry_after: *retry_after,
        }),
        _ => None,
    }
}

/// The per-account usage VELOCITY between two consecutive readings (issue #399): the signed
/// change in each dimension as a rounded percent, `to_pct(next) - to_pct(prev)`. Computed off the
/// RAW carried readings and reusing [`to_pct`], so the delta agrees with the percents `status`
/// shows for the same readings, and shares its rounding with the swap line (whose `session_pct`
/// may since #614 carry a plausibility-corrected value on a stale-low tick — see
/// `plausible_active_usage`). A difference of two `0..=100` percents lands in `-100..=100`, well
/// inside `i16`. Positive ⇒ usage climbing; negative ⇒ a window reset dropped the reading. Pure,
/// so the quantization is unit-tested without a daemon.
pub(crate) fn usage_velocity(prev: &Usage, next: &Usage) -> (i16, i16) {
    let session = i16::from(to_pct(next.session)) - i16::from(to_pct(prev.session));
    let weekly = i16::from(to_pct(next.weekly)) - i16::from(to_pct(prev.weekly));
    (session, weekly)
}

/// Map a swap-engine failure to the redacted wire reason for a `swap` ack (issue #167). The two
/// SAFETY aborts `force` can NEVER bypass get their own codes — a LOCKED keychain
/// ([`Error::KeychainLocked`], the engine's step-1 read aborts even under `force`) and a contended
/// single-writer swap lock ([`Error::SwapLockBusy`], fail-closed) — so the "force cannot bypass the
/// locked-keychain abort" invariant is observable in the ack. A canonical that is GONE
/// ([`Error::CredentialNotFound`], scrubbed since the daemon last resolved active) routes to the
/// recovery signal (adopt-target is the standalone path); everything else is the opaque `Failed`.
pub(crate) fn classify_swap_failure(err: &Error) -> SwapRejection {
    match err {
        Error::KeychainLocked { .. } => SwapRejection::KeychainLocked,
        Error::SwapLockBusy => SwapRejection::SwapLockBusy,
        Error::CredentialNotFound => SwapRejection::NoActiveAccount,
        _ => SwapRejection::Failed,
    }
}

/// Map a capture failure (from the #357 [`capture_locked`](crate::capture::capture_locked)
/// primitive, or a post-stash roster save) to the redacted wire reason for a `capture` ack (issue
/// #359) — the capture counterpart of [`classify_swap_failure`]. The two SAFETY aborts get their own
/// codes: a LOCKED keychain ([`Error::KeychainLocked`], the token read aborts even mid-capture) and
/// a contended single-writer swap lock ([`Error::SwapLockBusy`], fail-closed BEFORE any read). A
/// missing active account — not logged in to Claude Code (an absent / no-`oauthAccount`
/// `~/.claude.json`) or the canonical credential gone — routes to
/// [`CaptureRejection::NoActiveAccount`]; everything else (an I/O error, a roster save failure) is
/// the opaque `Failed`. Secret-free by construction: it inspects only the error's discriminant.
pub(crate) fn classify_capture_failure(err: &Error) -> CaptureRejection {
    match err {
        Error::KeychainLocked { .. } => CaptureRejection::KeychainLocked,
        Error::SwapLockBusy => CaptureRejection::SwapLockBusy,
        // Not logged in (absent `~/.claude.json` or no `oauthAccount` block) or the canonical token
        // is gone — there is no active account to capture.
        Error::ClaudeStateNotFound { .. }
        | Error::OauthAccountMissing
        | Error::CredentialNotFound => CaptureRejection::NoActiveAccount,
        _ => CaptureRejection::Failed,
    }
}

/// Map an [`apply_settings`](crate::config::Config::apply_settings) failure to the redacted
/// `(reason, detail)` for a `config-set` ack (issue #268) — the config-set counterpart of
/// [`classify_capture_failure`]. A malformed / unparseable BASELINE (the existing on-disk file
/// cannot be understood) is `ConfigUnreadable`, carrying the secret-free TOML parse message as
/// `detail` (issue #628) — config-set never overwrites a file it cannot re-render; a label edit
/// naming an unknown roster uuid (a stale settings client) is
/// `UnknownAccount`; a range or cross-field violation on the FINAL edited config is `Invalid`,
/// carrying the non-secret field-named message as `detail` so the UI can point at the offending
/// field. Secret-free by construction: the config file holds no secrets (issue #15) and the reason
/// is a stable discriminant.
pub(crate) fn classify_config_set_failure(err: &Error) -> (ConfigSetRejection, Option<String>) {
    match err {
        // The baseline on-disk file is malformed — the overlay re-parse failed on the existing text;
        // refuse rather than clobber a file the daemon cannot re-render. Surface the parse message as
        // `detail` (issue #628) — a secret-free TOML error (the config holds no secrets, issue #15) —
        // so a stale / version-skewed on-disk config is diagnosable, not a bare envelope.
        Error::ConfigParse(_) => (ConfigSetRejection::ConfigUnreadable, Some(err.to_string())),
        // A label edit named an `account_uuid` no roster account has (a stale client — the account
        // was `remove`d since its `config-get`).
        Error::AccountUuidNotFound { .. } => (ConfigSetRejection::UnknownAccount, None),
        // A range / cross-field rule failed on the FINAL edited config (an out-of-range tunable, an
        // empty label, `exhausted_poll_secs < poll_secs`, `target_max_session_usage >
        // session_ceiling`); surface the non-secret field-named message as `detail`.
        Error::ConfigTargetMaxSessionAboveTrigger { .. } => {
            (ConfigSetRejection::Invalid, Some(err.to_string()))
        }
        Error::ConfigInvalid(msg) => (ConfigSetRejection::Invalid, Some(msg.clone())),
        // `apply_settings` yields only the four above; any other error is a defensive `Invalid`
        // with its redacted message rather than a silent mismap.
        other => (ConfigSetRejection::Invalid, Some(other.to_string())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A successful reading, for the poll-classification cases below. A local copy of `daemon`'s
    /// same-named test helper: test helpers are private to their own `mod tests`, so the split
    /// leaves both modules self-contained rather than coupling one test module to the other.
    fn live(session: f64, weekly: f64) -> Result<Usage> {
        Ok(Usage {
            session,
            weekly,
            weekly_resets_at: None,
            session_resets_at: None,
        })
    }

    #[tokio::test]
    async fn classify_poll_maps_each_result_to_its_liveness_class() {
        // The typed poll outcome (issue #42 CODE PREREQUISITE): the HTTP taxonomy is
        // folded onto the liveness/death axis in exactly one place. Success is Live,
        // 401 is Unauthorized (the death signal), 403 is its own ScopeMissing class,
        // and EVERY other failure collapses into the single Transient class.
        assert_eq!(classify_poll(&live(0.5, 0.5)), PollOutcome::Live);
        assert_eq!(
            classify_poll(&Err(Error::UsageUnauthorized)),
            PollOutcome::Unauthorized
        );
        assert_eq!(
            classify_poll(&Err(Error::UsageScopeMissing)),
            PollOutcome::ScopeMissing
        );
        for err in [
            Error::UsageTransient {
                status: 0,
                retry_after: None,
            },
            Error::UsageRateLimited {
                status: 429,
                retry_after: None,
            },
            Error::UsageRejected { status: 400 },
            Error::KeychainLocked { op: "read" },
            Error::UsageTokenUnreadable,
            Error::UsageParse("no dimension".to_owned()),
        ] {
            assert_eq!(
                classify_poll(&Err(err)),
                PollOutcome::Transient,
                "every non-401/403 failure folds into Transient",
            );
        }
    }

    #[test]
    fn diag_poll_class_separates_rate_limited_from_transient() {
        // The DIAGNOSTIC taxonomy (#77) splits a `429` (rate-limited) out as its own
        // class — the signal an operator debugging a throttling storm needs — whereas
        // the dead-credential `classify_poll` folds it into the generic transient.
        assert_eq!(
            diag_poll_class(&Err(Error::UsageUnauthorized)),
            PollClass::Unauthorized
        );
        assert_eq!(
            diag_poll_class(&Err(Error::UsageScopeMissing)),
            PollClass::Scope
        );
        assert_eq!(
            diag_poll_class(&Err(Error::UsageRateLimited {
                status: 429,
                retry_after: None,
            })),
            PollClass::RateLimited
        );
        assert_eq!(
            diag_poll_class(&Err(Error::UsageTransient {
                status: 503,
                retry_after: None,
            })),
            PollClass::Transient
        );
        assert_eq!(
            diag_poll_class(&Err(Error::UsageTokenUnreadable)),
            PollClass::Transient
        );
        // Contrast on the SAME 429: the health axis folds it into `Transient`.
        assert_eq!(
            classify_poll(&Err(Error::UsageRateLimited {
                status: 429,
                retry_after: None,
            })),
            PollOutcome::Transient
        );
    }

    #[test]
    fn usage_velocity_computes_signed_rounded_percent_deltas() {
        // The pure quantization (issue #399): `to_pct(next) - to_pct(prev)`, so a velocity agrees
        // with the percents `status` / the swap line show, and a difference of two `0..=100`
        // percents is a signed value in `-100..=100`.
        let r = |session: f64, weekly: f64| Usage {
            session,
            weekly,
            weekly_resets_at: None,
            session_resets_at: None,
        };
        // Climbing: both dimensions POSITIVE.
        assert_eq!(usage_velocity(&r(0.10, 0.20), &r(0.17, 0.22)), (7, 2));
        // A window reset dropped the reading: NEGATIVE session delta.
        assert_eq!(usage_velocity(&r(0.95, 0.40), &r(0.03, 0.40)), (-92, 0));
        // Flat: zero in both dimensions (the no-op the emitter stays silent on).
        assert_eq!(usage_velocity(&r(0.50, 0.50), &r(0.50, 0.50)), (0, 0));
    }

    // --- classify_swap_failure (engine error → redacted reason, issue #167) ---

    #[test]
    fn classify_swap_failure_maps_the_two_force_proof_safety_aborts_to_their_own_codes() {
        // AC (force cannot bypass a SAFETY abort): both surface as their OWN redacted reason (not the
        // opaque `Failed`), making "force cannot bypass the locked-keychain abort / the swap lock"
        // observable in the ack. A locked keychain and a fail-closed single-writer lock each map
        // through distinctly.
        assert_eq!(
            classify_swap_failure(&Error::KeychainLocked { op: "read" }),
            SwapRejection::KeychainLocked
        );
        assert_eq!(
            classify_swap_failure(&Error::SwapLockBusy),
            SwapRejection::SwapLockBusy
        );
    }

    #[test]
    fn classify_swap_failure_routes_a_vanished_canonical_to_no_active_and_else_to_failed() {
        // A canonical scrubbed since the daemon last resolved active → the recovery signal (adopt-
        // target is the standalone path); every other engine error is the opaque `Failed` (#15: no
        // internal detail on the wire). The #211 wrong-identity re-stash guard is one such `Failed`.
        assert_eq!(
            classify_swap_failure(&Error::CredentialNotFound),
            SwapRejection::NoActiveAccount
        );
        assert_eq!(
            classify_swap_failure(&Error::SwapWrongIdentityRestash),
            SwapRejection::Failed
        );
        // The #714 canary refusals ride the SAME opaque `Failed` — deliberately, so the closed
        // wire enum never grows a variant an old Swift decoder would throw on (the detail lives
        // in the event log + the `status` canary field, not the ack).
        assert_eq!(
            classify_swap_failure(&Error::CanaryDrift {
                displayed: "work".to_owned(),
                matched: "spare".to_owned(),
            }),
            SwapRejection::Failed
        );
        assert_eq!(
            classify_swap_failure(&Error::CredentialAmbiguous { count: 2 }),
            SwapRejection::Failed
        );
    }

    // --- classify_capture_failure (engine error → redacted reason, issue #359) ---

    #[test]
    fn classify_capture_failure_maps_each_engine_error_to_its_redacted_reason() {
        // The two SAFETY aborts surface as their OWN redacted codes (not the opaque `Failed`): a
        // LOCKED keychain (the token read aborts mid-capture; locked ≠ gone) and a contended
        // single-writer swap lock (fail-closed BEFORE any read). A missing active account — an
        // absent / no-`oauthAccount` `~/.claude.json`, or a vanished canonical credential — routes
        // to `NoActiveAccount`; every other engine error is the opaque `Failed` (#15: no internal
        // detail on the wire). The capture mirror of `classify_swap_failure`.
        assert_eq!(
            classify_capture_failure(&Error::KeychainLocked { op: "read" }),
            CaptureRejection::KeychainLocked
        );
        assert_eq!(
            classify_capture_failure(&Error::SwapLockBusy),
            CaptureRejection::SwapLockBusy
        );
        assert_eq!(
            classify_capture_failure(&Error::ClaudeStateNotFound {
                path: PathBuf::from("/nope/.claude.json"),
            }),
            CaptureRejection::NoActiveAccount
        );
        assert_eq!(
            classify_capture_failure(&Error::OauthAccountMissing),
            CaptureRejection::NoActiveAccount
        );
        assert_eq!(
            classify_capture_failure(&Error::CredentialNotFound),
            CaptureRejection::NoActiveAccount
        );
        assert_eq!(
            classify_capture_failure(&Error::SwapWrongIdentityRestash),
            CaptureRejection::Failed
        );
    }
}
