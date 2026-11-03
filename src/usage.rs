// Copyright (c) 2026 Oleksii PELYKH
// SPDX-License-Identifier: MIT

//! Per-account usage polling.
//!
//! Reads one account's quota from the Claude usage endpoint, read-only:
//!
//! ```text
//! GET https://api.anthropic.com/api/oauth/usage
//! Authorization: Bearer <accessToken>
//! anthropic-beta: oauth-2025-04-20
//! ```
//!
//! and projects the response into a [`Usage`] reading (every quota window the
//! account has; a window the reply did not carry is `None`) for the swap decision.
//! The bearer is the `accessToken` inside the stored `Claude Code-credentials`
//! blob — read through the existing [`CredentialStore`] seam
//! ([`crate::keychain`]); the poller never re-reads or re-mints a token.
//!
//! ## Transport: the `curl` CLI, not an HTTP crate
//!
//! Like [`crate::keychain`] / [`crate::stash`] drive `/usr/bin/security`, this
//! drives `/usr/bin/curl` (absolute path, so a hijacked `PATH` cannot substitute
//! a binary for this network call). No HTTP crate is pulled into the graph; the
//! request rides the system tool that is always present on macOS. The bearer
//! token is fed on `curl`'s **stdin** (a `--config -` file), so — unlike the
//! keychain CLI's `-w <secret>` argv — it never appears in this process's argv.
//!
//! ## HTTP outcome taxonomy (issue #5)
//!
//! Each poll is classified — these are runtime states, not process exits:
//!   - **success** (`2xx`) → a [`Usage`] reading.
//!   - **Transient** (`5xx`, or no HTTP response: DNS / connection / TLS /
//!     timeout) → [`Error::UsageTransient`]. Back off, skip, never swap on
//!     missing data.
//!   - **RateLimited** (`429`) → [`Error::UsageRateLimited`]; other non-401/403
//!     `4xx` → [`Error::UsageRejected`]. Both: back off, skip (design G4).
//!   - **Unauthorized** (`401`) → [`Error::UsageUnauthorized`]; the consecutive
//!     count feeds the re-stash trigger (below).
//!   - **ScopeFailed** (`403`) → [`Error::UsageScopeMissing`], surfaced
//!     **distinctly** from 401 — the hallmark of a non-interactive setup token.
//!
//! ## 401 streak → dead-credential detection (issue #42)
//!
//! The poll path is stateless: each poll classifies one HTTP outcome and returns a
//! [`Usage`] reading or a typed error. Consecutive-401 streak tracking — and the
//! dead-credential quarantine / emergency-swap / recovery it drives — lives in the
//! daemon's per-account health state ([`crate::daemon`]), which is the only place a
//! streak can persist ACROSS polls (a per-poll counter here would reset every tick
//! and never accumulate). The poller never self-refreshes a token.

use std::process::Stdio;
use std::time::Duration;

#[cfg(test)]
use std::cell::RefCell;

use serde_json::Value;
use tokio::io::AsyncWriteExt;
use tokio::process::Command;
use zeroize::Zeroizing;

use crate::error::{Error, Result};
use crate::keychain::CredentialStore;

/// Absolute path to the system `curl`. Absolute (not `$PATH`-resolved) so a
/// hijacked `PATH` cannot substitute a different binary for this network call —
/// the same discipline [`crate::keychain`] applies to `security`.
const CURL: &str = "/usr/bin/curl";

/// The read-only per-account usage endpoint.
const USAGE_URL: &str = "https://api.anthropic.com/api/oauth/usage";

/// The OAuth beta opt-in header the usage endpoint requires.
const BETA_HEADER: &str = "anthropic-beta: oauth-2025-04-20";

/// Hard ceiling (seconds) on a single poll, so a hung request can never wedge the
/// poll loop. Comfortably below the default 300 s (5 min) poll interval.
const POLL_TIMEOUT_SECS: u32 = 30;

/// A point-in-time usage reading for one account, across every quota window the
/// account has.
///
/// The swap decision compares each dimension against its OWN threshold (issue
/// #41: session vs `session_ceiling`, weekly vs the separate `weekly_ceiling`),
/// so the reading carries each present fraction and projects neither to a single
/// worst-case scalar. A window the served reply did not carry is `None` — "does
/// not apply to this account" (since 2026-10-30 the state of a verified Ultra
/// account's session window) — never a fabricated `0`: a present `0.0` is a measured
/// idle window. Invariant I-1, pinned at the parser (issue #1596) and carried here
/// as a documented contract with no runtime check: a `*_resets_at` is `Some` only
/// when its window's fraction is `Some`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Usage {
    /// Fraction in `[0.0, 1.0]` of the rolling 5-hour session window consumed, or
    /// `None` when the account has no session window.
    pub(crate) session: Option<f64>,
    /// Fraction in `[0.0, 1.0]` of the weekly window consumed, or `None` when the
    /// account has no weekly window (no observed instance; the operator's stated
    /// future case).
    pub(crate) weekly: Option<f64>,
    /// Epoch seconds at which the WEEKLY window resets, when the API reported a
    /// parseable timestamp; `None` otherwise. The all-exhausted terminal logic
    /// (issue #11) consumes it: when every account is weekly-exhausted the daemon
    /// holds on the account whose weekly window resets soonest. Only the weekly
    /// dimension drives that terminal signal — the weekly window is the hard limit
    /// whose reset actually ends the all-exhausted state.
    pub(crate) weekly_resets_at: Option<i64>,
    /// Epoch seconds at which the rolling 5-hour SESSION window resets, when the
    /// API reported a parseable timestamp; `None` otherwise. Carried so `status`
    /// can show a per-account "resets in" (issue #72): in normal rotation an
    /// account is session-exhausted (out for hours) while its weekly window is
    /// fine, so the SESSION reset — not the weekly one — is when it becomes usable
    /// again. The all-exhausted terminal signal ignores it (that keys off the weekly
    /// reset above), but since issue #614 the swap-decision path DOES read it as the
    /// session window's IDENTITY: usage is monotonic within one window, so a session
    /// value that drops while this stamp is unchanged is a stale / cache-lagged
    /// reading rather than real drain (see [`crate::swap::SessionHighWater`]).
    pub(crate) session_resets_at: Option<i64>,
}

/// A poll's full reading: the lean swap-decision [`Usage`] plus the sample-only
/// fields the decision path does not consume (issue #156). Kept SEPARATE from
/// [`Usage`] so the pervasive, `Copy` decision type stays lean — the daemon
/// projects a `PolledReading` straight to its `Usage` on the decision path, and the
/// usage-sample collector reads the extras here.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct PolledReading {
    /// The swap-decision reading (each quota window the account has + resets).
    pub(crate) usage: Usage,
    /// The provider-reported severity label for this reading (e.g. `"critical"`),
    /// when the API supplied one — the WEEKLY window's label, falling back to the
    /// SESSION window's. Persisted to the usage-sample store
    /// ([`crate::usage_store::Sample`]); a tolerant free string, never parsed to an
    /// enum, so an unrecognised future value is retained rather than dropped.
    pub(crate) severity: Option<String>,
}

/// The full parse of a usage response: every quota window the response carries, plus
/// each present window's reset timestamp, kept VERBATIM as the API rendered them (ISO
/// string or epoch). A window the response does not carry is `None` on every one of
/// its fields (issue #1596). The reset timestamps are extracted here (issue #5
/// acceptance: "returns session%, account%, resets_at");
/// [`to_usage`](UsageReport::to_usage) is the swap-decision projection, normalizing
/// the weekly reset to epoch seconds for the all-exhausted terminal logic (#11).
/// Keeping the raw form here means this extraction stays a faithful mirror of the
/// response; normalization is a separate, tested concern.
#[derive(Debug, Clone, PartialEq)]
struct UsageReport {
    /// The session window's fraction in `[0.0, 1.0]`, or `None` when the response
    /// carries no session window (the account has none): never a fabricated `0`.
    session: Option<f64>,
    /// The weekly window's fraction in `[0.0, 1.0]`, or `None` when the response
    /// carries no weekly window: never a fabricated `0`.
    weekly: Option<f64>,
    /// Raw `resets_at` of the session window, as the API rendered it (ISO string
    /// or epoch-as-string); tolerant — `None` if absent/unrecognized. Projected to
    /// epoch seconds by [`to_usage`](UsageReport::to_usage) for the `status`
    /// per-account "resets in" display (issue #72).
    session_resets_at: Option<String>,
    /// Raw `resets_at` of the weekly window (see `session_resets_at`). Projected
    /// to epoch seconds by [`to_usage`](UsageReport::to_usage) for the
    /// all-exhausted terminal logic (#11).
    weekly_resets_at: Option<String>,
    /// The session window's provider-reported `severity`, verbatim, when present.
    /// Kept per-window here (the faithful raw mirror); [`to_usage`](UsageReport::to_usage) collapses the
    /// pair to a single reading-level label (issue #156 parse widening).
    session_severity: Option<String>,
    /// The weekly window's provider-reported `severity`, verbatim, when present
    /// (see `session_severity`).
    weekly_severity: Option<String>,
}

impl UsageReport {
    /// The swap-decision projection: each usage window the account has, plus each
    /// window's reset normalized to epoch seconds. The weekly reset feeds
    /// the all-exhausted logic (issue #11) so it can compare reset times across
    /// accounts; the session reset feeds the `status` "resets in" display (issue
    /// #72). A reset the API did not supply — or that does not parse — projects to
    /// `None`.
    fn to_usage(&self) -> Usage {
        Usage {
            session: self.session,
            weekly: self.weekly,
            weekly_resets_at: self
                .weekly_resets_at
                .as_deref()
                .and_then(epoch_from_resets_at),
            session_resets_at: self
                .session_resets_at
                .as_deref()
                .and_then(epoch_from_resets_at),
        }
    }

    /// The reading's single severity label (issue #156): prefer the WEEKLY window's
    /// (the hard/binding limit the store emphasises), falling back to the SESSION
    /// window's; `None` when the API supplied neither. Kept off [`to_usage`](UsageReport::to_usage) so the
    /// lean `Usage` decision type is unaffected — only [`PolledReading`] carries it.
    fn severity(&self) -> Option<String> {
        self.weekly_severity
            .clone()
            .or_else(|| self.session_severity.clone())
    }
}

/// Seam: reads one account's usage quota as a [`PolledReading`] — the swap-decision
/// [`Usage`] plus the sample-only `severity`, from a SINGLE usage-API call (issue
/// #156). The only impl ([`RealUsageSource`]) polls the API; it is exercised in tests
/// through its [`UsageTransport`] seam (a fake transport returns scripted responses),
/// not a separate fake source.
pub(crate) trait UsageSource {
    async fn usage(&self) -> Result<PolledReading>;
}

/// Seam: performs the raw usage HTTP GET and reports the `(status, body)`. The
/// real impl ([`CurlTransport`]) reads the token and shells out to `curl`; a
/// test impl returns scripted responses, so the classification / parsing / 401
/// logic in [`RealUsageSource`] is exercised without a network.
pub(crate) trait UsageTransport {
    async fn fetch(&self) -> Result<HttpResponse>;
}

/// One HTTP exchange's result. `status == 0` is the sentinel for "no HTTP
/// response" (curl could not reach the endpoint), classified as Transient.
pub(crate) struct HttpResponse {
    status: u16,
    body: String,
    /// The server-advised `Retry-After` (delta-seconds form) when the response
    /// carried that header; `None` otherwise. Threaded into the rate-limited /
    /// transient errors so the daemon can honour it as a MINIMUM back-off wait
    /// (issue #76). Only a `4xx`/`5xx` carrying it is meaningful; on a `2xx` it is
    /// simply ignored.
    retry_after: Option<Duration>,
}

/// Real usage poller: classify the HTTP outcome and parse a success. Generic over
/// the transport so the whole `usage()` flow is testable against a fake. Stateless
/// — consecutive-401 streak tracking lives in the daemon's per-account health state
/// (issue #42), the only place a streak can persist across polls.
pub(crate) struct RealUsageSource<Tp> {
    transport: Tp,
}

impl<Tp> RealUsageSource<Tp> {
    pub(crate) fn new(transport: Tp) -> Self {
        Self { transport }
    }
}

impl<Tp> UsageSource for RealUsageSource<Tp>
where
    Tp: UsageTransport,
{
    async fn usage(&self) -> Result<PolledReading> {
        let response = self.transport.fetch().await?;
        let status = response.status;
        match classify_status(status) {
            UsageStatus::Unauthorized => Err(Error::UsageUnauthorized),
            // The one fetch+classify success path: parse ONCE and keep both the
            // decision `Usage` and the sample-only `severity` (issue #156), so the
            // daemon's sample piggybacks this poll with no extra API call.
            UsageStatus::Success => {
                let report = parse_usage(&response.body)?;
                Ok(PolledReading {
                    usage: report.to_usage(),
                    severity: report.severity(),
                })
            }
            UsageStatus::ScopeMissing => Err(Error::UsageScopeMissing),
            UsageStatus::RateLimited => Err(Error::UsageRateLimited {
                status,
                retry_after: response.retry_after,
            }),
            UsageStatus::ClientError => Err(Error::UsageRejected { status }),
            UsageStatus::Transient => Err(Error::UsageTransient {
                status,
                retry_after: response.retry_after,
            }),
        }
    }
}

/// Real transport: read the bearer from the stored credential and `curl` the
/// usage endpoint. Generic over [`CredentialStore`] so the token source can be
/// the canonical active item (now) or a per-account stash adapter (#6 / #7).
pub(crate) struct CurlTransport<C> {
    store: C,
}

impl<C: CredentialStore> CurlTransport<C> {
    pub(crate) fn new(store: C) -> Self {
        Self { store }
    }
}

impl<C: CredentialStore> UsageTransport for CurlTransport<C> {
    async fn fetch(&self) -> Result<HttpResponse> {
        let credential = self.store.read().await?;
        let token = access_token_from_blob(credential.expose())?;
        // The token-bearing config rides stdin (never argv) and is zeroized on
        // drop. argv is only `curl --config -`.
        let config = curl_config(&token);

        let mut child = Command::new(CURL)
            .arg("--config")
            .arg("-")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()?;
        {
            let mut stdin = child.stdin.take().expect("stdin was piped");
            stdin.write_all(config.as_bytes()).await?;
            // EOF so `curl` stops reading config and performs the request.
            stdin.shutdown().await?;
        }
        let output = child.wait_with_output().await?;

        // `curl` ran but got no HTTP response (DNS / connection / TLS / timeout):
        // a non-zero exit and no status line. Report the `0` "no response"
        // sentinel → classified Transient. (A failure to *spawn* curl is a true
        // I/O error and propagated via `?` above.)
        if !output.status.success() {
            return Ok(HttpResponse {
                status: 0,
                body: String::new(),
                retry_after: None,
            });
        }
        Ok(parse_curl_output(&output.stdout))
    }
}

/// The internal classification of one HTTP status.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum UsageStatus {
    Success,
    Transient,
    RateLimited,
    ClientError,
    Unauthorized,
    ScopeMissing,
}

/// Map an HTTP status to its taxonomy class. `401` / `403` / `429` are matched
/// before the `4xx` catch-all; `0` (no response) and any unexpected code fall to
/// Transient (skip + back off, never swap on it).
fn classify_status(status: u16) -> UsageStatus {
    match status {
        200..=299 => UsageStatus::Success,
        401 => UsageStatus::Unauthorized,
        403 => UsageStatus::ScopeMissing,
        429 => UsageStatus::RateLimited,
        400..=499 => UsageStatus::ClientError,
        500..=599 => UsageStatus::Transient,
        _ => UsageStatus::Transient,
    }
}

/// Extract the OAuth bearer from a `Claude Code-credentials` blob.
///
/// Documented shape: `{"claudeAiOauth":{"accessToken":"sk-ant-oat-…"}}` (the
/// canonical credential; `build/version-compat.md`: the token is the bearer).
/// Tolerant of a couple of plausible reshapes — top-level `accessToken` /
/// `access_token` — so a future change degrades to a clear typed error rather
/// than a mis-poll. Never echoes any blob bytes (issue #15 redaction).
fn access_token_from_blob(blob: &[u8]) -> Result<Zeroizing<String>> {
    let value: Value = serde_json::from_slice(blob).map_err(|_| Error::UsageTokenUnreadable)?;
    let token = value
        .get("claudeAiOauth")
        .and_then(|oauth| oauth.get("accessToken"))
        .or_else(|| value.get("accessToken"))
        .or_else(|| value.get("access_token"))
        .and_then(Value::as_str)
        .filter(|token| !token.is_empty())
        .ok_or(Error::UsageTokenUnreadable)?;
    Ok(Zeroizing::new(token.to_owned()))
}

/// Build the `curl --config -` body fed on stdin. Keeps the bearer out of argv;
/// `write-out` appends `\n<status>\n<retry-after>` after the body so the caller
/// can recover the HTTP status and the `Retry-After` response header (empty when
/// absent — `%header{}` requires curl ≥ 7.84, present on every supported macOS;
/// an unparseable value just degrades to `None`, issue #76). The token is an
/// opaque `sk-ant-…` string (no `"`/newline), so it needs no escaping inside the
/// quoted `header` value. Zeroized on drop.
fn curl_config(token: &str) -> Zeroizing<String> {
    let mut config = String::new();
    config.push_str("url = \"");
    config.push_str(USAGE_URL);
    config.push_str("\"\n");
    config.push_str("header = \"Authorization: Bearer ");
    config.push_str(token);
    config.push_str("\"\n");
    config.push_str("header = \"");
    config.push_str(BETA_HEADER);
    config.push_str("\"\n");
    config.push_str("header = \"Accept: application/json\"\n");
    // Quiet progress, but still print transport errors (to stderr, never stdout).
    config.push_str("silent\n");
    config.push_str("show-error\n");
    config.push_str(&format!("max-time = {POLL_TIMEOUT_SECS}\n"));
    // Body, then a newline and the numeric status, then a newline and the
    // `Retry-After` header value (empty when absent), on stdout.
    config.push_str("write-out = \"\\n%{http_code}\\n%header{retry-after}\"\n");
    Zeroizing::new(config)
}

/// Split `curl`'s stdout (`<body>\n<status>\n<retry-after>`, per the `write-out`
/// above) into a [`HttpResponse`]. The two metadata lines are peeled from the END,
/// so a multi-line body is preserved. Tolerant: the `Retry-After` line is empty
/// (→ `None`) on most responses; an unparseable trailing code becomes `0` (→
/// Transient); a no-body output is just the metadata.
fn parse_curl_output(stdout: &[u8]) -> HttpResponse {
    let text = String::from_utf8_lossy(stdout);
    // Peel the last line (the `Retry-After` value, empty when the header was
    // absent), then the new-last line (the status); everything before is the body.
    let Some((rest, retry_after_line)) = text.rsplit_once('\n') else {
        // No newline at all — degenerate; treat the whole output as the code.
        return HttpResponse {
            status: text.trim().parse().unwrap_or(0),
            body: String::new(),
            retry_after: None,
        };
    };
    let retry_after = parse_retry_after(retry_after_line);
    match rest.rsplit_once('\n') {
        Some((body, code)) => HttpResponse {
            status: code.trim().parse().unwrap_or(0),
            body: body.to_owned(),
            retry_after,
        },
        None => HttpResponse {
            status: rest.trim().parse().unwrap_or(0),
            body: String::new(),
            retry_after,
        },
    }
}

/// Parse a `Retry-After` header value into a back-off floor. Only the
/// delta-seconds form (a non-negative integer count of seconds) is honoured →
/// `Some(Duration)`; the HTTP-date form, and any empty or unparseable value,
/// yields `None`, leaving the daemon's exponential back-off to govern the wait
/// (issue #76). A negative or fractional value parses as neither a `u64` → `None`.
fn parse_retry_after(value: &str) -> Option<Duration> {
    let secs: u64 = value.trim().parse().ok()?;
    Some(Duration::from_secs(secs))
}

/// Parse a `200` usage body into every quota window it carries, plus each window's
/// reset timestamp and severity.
///
/// Tolerant of the two documented response shapes for each window:
///   - a top-level object — `five_hour` (session) / `seven_day` (weekly) — with a
///     `utilization` percentage (`0..=100`), or
///   - a `limits[]` entry whose `kind` matches the window, with a `percent`
///     (also `0..=100`).
///
/// Each window is independently optional: a window the reply does not carry is
/// reported as `None` (not applicable to this account), never a fabricated `0`;
/// a reply carrying no window at all is a hard [`Error::UsageParse`] (decision D1),
/// so the loop skips, not swaps. A window the reply carries but that no shape above
/// yields a number for also reads as `None`, so it cannot yet be told from an absent
/// one; whether it should become a parse error instead is issue #1614's open question.
fn parse_usage(body: &str) -> Result<UsageReport> {
    let root: Value =
        serde_json::from_str(body).map_err(|_| Error::UsageParse("body is not JSON".into()))?;

    let session = dimension(&root, "five_hour", &["session", "five_hour"]);
    let weekly = dimension(&root, "seven_day", &["weekly_all", "seven_day", "weekly"]);
    if session.is_none() && weekly.is_none() {
        // Decision D1, option (a): a 200 reply with no applicable window stays a hard
        // parse error, so the loop skips (`PollOutcome::Transient`), never swaps. The
        // text is a structural hint naming both windows, never response bytes.
        return Err(Error::UsageParse(
            "no quota window (neither five_hour nor seven_day)".into(),
        ));
    }
    let (session, session_resets_at, session_severity) = window_parts(session);
    let (weekly, weekly_resets_at, weekly_severity) = window_parts(weekly);
    Ok(UsageReport {
        session,
        weekly,
        session_resets_at,
        weekly_resets_at,
        session_severity,
        weekly_severity,
    })
}

/// Spread one window's lookup over the report's three per-window fields. An absent
/// window is `(None, None, None)`: a reset stamp or a severity never outlives its
/// fraction, because [`dimension`] returns the three together (invariant I-1).
fn window_parts(
    window: Option<(f64, Option<String>, Option<String>)>,
) -> (Option<f64>, Option<String>, Option<String>) {
    match window {
        Some((fraction, resets_at, severity)) => (Some(fraction), resets_at, severity),
        None => (None, None, None),
    }
}

/// Find one window's `(fraction, resets_at, severity)`, trying the top-level
/// `{top_key: {...}}` object first, then a `limits[]` entry whose `kind` is one
/// of `kinds`. Among matching `limits[]` entries an active one (`is_active` true
/// or absent) is preferred, but an explicitly inactive entry is used as a
/// fallback rather than dropped: `is_active: false` means "not the currently
/// binding limit" — the live API marks `weekly_all` so — NOT "absent" (issue
/// #66). Dropping it entirely would lose a window whenever it is the only match.
/// `severity` is the entry's optional provider label, extracted from the SAME
/// matched entry as the fraction (issue #156 parse widening).
fn dimension(
    root: &Value,
    top_key: &str,
    kinds: &[&str],
) -> Option<(f64, Option<String>, Option<String>)> {
    if let Some(obj) = root.get(top_key) {
        if let Some(fraction) = fraction_of(obj) {
            return Some((fraction, resets_at_of(obj), severity_of(obj)));
        }
    }
    let limits = root.get("limits").and_then(Value::as_array)?;
    // An active matching entry wins; an inactive one is kept only as a fallback
    // (is_active:false = "not the binding limit", not "absent" — issue #66).
    let mut fallback: Option<(f64, Option<String>, Option<String>)> = None;
    for entry in limits {
        let kind = entry.get("kind").and_then(Value::as_str);
        if !kind.is_some_and(|kind| kinds.contains(&kind)) {
            continue;
        }
        let Some(fraction) = fraction_of(entry) else {
            continue;
        };
        let active = entry
            .get("is_active")
            .and_then(Value::as_bool)
            .unwrap_or(true);
        if active {
            return Some((fraction, resets_at_of(entry), severity_of(entry)));
        }
        if fallback.is_none() {
            fallback = Some((fraction, resets_at_of(entry), severity_of(entry)));
        }
    }
    fallback
}

/// Read a usage fraction in `[0.0, 1.0]` from an object: a `utilization`
/// percentage or a `percent`, each divided by 100. Both fields are on the same
/// `0..=100` scale — `utilization` is the top-level window's spelling, `percent`
/// the `limits[]` entry's (issue #66) — and `utilization` is tried first, with
/// `percent` the fallback when it is absent. Clamped, so a stray `> 100` reading
/// can never exceed a full window.
fn fraction_of(obj: &Value) -> Option<f64> {
    let percent = obj
        .get("utilization")
        .and_then(Value::as_f64)
        .or_else(|| obj.get("percent").and_then(Value::as_f64))?;
    Some((percent / 100.0).clamp(0.0, 1.0))
}

/// Read `resets_at` as the API rendered it — a string verbatim, or a number
/// stringified — tolerant of either; `None` if absent or another type.
fn resets_at_of(obj: &Value) -> Option<String> {
    match obj.get("resets_at") {
        Some(Value::String(at)) => Some(at.clone()),
        Some(Value::Number(at)) => Some(at.to_string()),
        _ => None,
    }
}

/// Read a window's optional `severity` label verbatim (issue #156 parse widening).
/// A tolerant free string — only a JSON string is taken; any other type (or an
/// absent field) is `None`, so a reading without a severity still parses cleanly.
fn severity_of(obj: &Value) -> Option<String> {
    match obj.get("severity") {
        Some(Value::String(label)) => Some(label.clone()),
        _ => None,
    }
}

/// Normalize a raw `resets_at` (as [`resets_at_of`] captured it) to epoch seconds,
/// so the all-exhausted logic (#11) can order reset times across accounts.
/// Tolerant of the two shapes the API uses: a whole-second epoch rendered as
/// digits, or an RFC 3339 instant (`2026-06-30T00:00:00Z`). `None` for anything
/// it cannot parse — a missing reset time is never fatal; the terminal signal just
/// omits it.
fn epoch_from_resets_at(raw: &str) -> Option<i64> {
    let raw = raw.trim();
    if let Ok(epoch) = raw.parse::<i64>() {
        return Some(epoch);
    }
    epoch_from_rfc3339(raw)
}

/// Parse an RFC 3339 / ISO 8601 instant to epoch seconds, second-granular.
/// Tolerant: accepts a `Z`/`z` suffix, an explicit `±HH:MM` offset, or none
/// (treated as UTC); a fractional-seconds part is dropped. `None` on any deviation
/// from the expected `YYYY-MM-DDTHH:MM:SS` shape, so a surprising format degrades
/// to "reset time unknown" rather than a wrong instant.
///
/// `pub(crate)` so [`crate::observability::last_swap_at`] can parse the event log's
/// `ts=` field back to an instant through this SAME canonical parser (the log
/// renders it via `observability::rfc3339`), rather than hand-rolling a second
/// copy of the leap-year arithmetic for the cooldown gate (#63).
pub(crate) fn epoch_from_rfc3339(s: &str) -> Option<i64> {
    let (date, rest) = s.split_once('T').or_else(|| s.split_once(' '))?;

    let mut date_parts = date.split('-');
    let year: i64 = date_parts.next()?.parse().ok()?;
    let month: u32 = date_parts.next()?.parse().ok()?;
    let day: u32 = date_parts.next()?.parse().ok()?;
    // Reject anything the civil calendar does not admit, so a malformed upstream
    // value degrades to "reset unknown" rather than a fabricated instant (#177):
    // a 4-digit year bound keeps `days_from_civil` clear of `i64` overflow, and
    // the per-month day length rejects `2026-02-30` / `2025-02-29` instead of
    // silently normalizing them forward. `||` short-circuits, so `days_in_month`
    // only sees a `1..=12` month.
    if date_parts.next().is_some()
        || !(0..=9999).contains(&year)
        || !(1..=12).contains(&month)
        || !(1..=days_in_month(year, month)).contains(&day)
    {
        return None;
    }

    let (time, offset_secs) = split_offset(rest)?;
    let time = time.split('.').next().unwrap_or(time);
    let mut time_parts = time.split(':');
    let hour: i64 = time_parts.next()?.parse().ok()?;
    let minute: i64 = time_parts.next()?.parse().ok()?;
    let second: i64 = time_parts.next().unwrap_or("0").parse().ok()?;
    if time_parts.next().is_some()
        || !(0..=23).contains(&hour)
        || !(0..=59).contains(&minute)
        || !(0..=60).contains(&second)
    {
        return None;
    }

    let days = days_from_civil(year, month, day);
    Some(days * 86_400 + hour * 3_600 + minute * 60 + second - offset_secs)
}

/// Split a time-of-day from its timezone suffix, returning the bare time and the
/// offset in seconds EAST of UTC (so `epoch = local - offset`). `Z`/`z` or no
/// suffix is UTC (offset 0); an explicit `±HH:MM` / `±HHMM` / `±HH` is parsed.
fn split_offset(rest: &str) -> Option<(&str, i64)> {
    if let Some(time) = rest.strip_suffix(['Z', 'z']) {
        return Some((time, 0));
    }
    let Some(pos) = rest.rfind(['+', '-']) else {
        return Some((rest, 0)); // no offset → UTC
    };
    let (time, tz) = rest.split_at(pos);
    let sign = if tz.starts_with('-') { -1 } else { 1 };
    let tz = &tz[1..];
    let (hours, minutes) = match tz.split_once(':') {
        Some((h, m)) => (h.parse::<i64>().ok()?, m.parse::<i64>().ok()?),
        // `±HHMM`: slice via `get` (not `tz[..2]`) so a non-ASCII byte landing
        // mid-char yields None instead of panicking on the daemon poll loop (#177).
        None if tz.len() == 4 => (tz.get(..2)?.parse().ok()?, tz.get(2..)?.parse().ok()?),
        None => (tz.parse::<i64>().ok()?, 0),
    };
    Some((time, sign * (hours * 3_600 + minutes * 60)))
}

/// Days since 1970-01-01 for a proleptic-Gregorian civil date — Howard Hinnant's
/// `days_from_civil`, the inverse of the `civil_from_days` the event log uses to
/// render the reset back. Correct across leap years and the 100/400 century rules
/// for the post-epoch dates the usage API returns.
fn days_from_civil(year: i64, month: u32, day: u32) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = (if year >= 0 { year } else { year - 399 }) / 400;
    let yoe = year - era * 400; // [0, 399]
    let month = i64::from(month);
    let day = i64::from(day);
    let doy = (153 * (if month > 2 { month - 3 } else { month + 9 }) + 2) / 5 + (day - 1); // [0, 365]
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy; // [0, 146096]
    era * 146_097 + doe - 719_468
}

/// Whether `year` is a Gregorian leap year — the 4 / 100 / 400 rule (divisible by
/// 4, except whole centuries, except those divisible by 400, so 2000 is a leap
/// year and 2100 is not). Only bounds February in [`days_in_month`]; the epoch
/// arithmetic itself lives in [`days_from_civil`].
fn is_leap_year(year: i64) -> bool {
    year % 4 == 0 && (year % 100 != 0 || year % 400 == 0)
}

/// The length of `month` (`1..=12`) in `year`, honouring leap February. Lets
/// [`epoch_from_rfc3339`] reject an impossible calendar date instead of letting
/// [`days_from_civil`] normalize it to a wrong instant (#177). Callers guarantee
/// `month ∈ 1..=12`; any other value is treated as zero-length so it can never
/// masquerade as a valid day count.
fn days_in_month(year: i64, month: u32) -> u32 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if is_leap_year(year) => 29,
        2 => 28,
        _ => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;

    // --- classify_status (pure, the taxonomy) ---

    #[test]
    fn classify_maps_each_status_to_its_taxonomy_class() {
        assert_eq!(classify_status(200), UsageStatus::Success);
        assert_eq!(classify_status(204), UsageStatus::Success);
        assert_eq!(classify_status(401), UsageStatus::Unauthorized);
        assert_eq!(classify_status(403), UsageStatus::ScopeMissing);
        assert_eq!(classify_status(429), UsageStatus::RateLimited);
        // Other non-401/403 4xx are ClientError (G4: back off + skip).
        assert_eq!(classify_status(400), UsageStatus::ClientError);
        assert_eq!(classify_status(404), UsageStatus::ClientError);
        assert_eq!(classify_status(422), UsageStatus::ClientError);
        // 5xx and the no-response sentinel are Transient.
        assert_eq!(classify_status(500), UsageStatus::Transient);
        assert_eq!(classify_status(503), UsageStatus::Transient);
        assert_eq!(classify_status(0), UsageStatus::Transient);
    }

    #[test]
    fn forbidden_is_classified_distinctly_from_unauthorized() {
        // Issue #5 acceptance: 403 (scope) must be distinct from 401.
        assert_ne!(classify_status(403), classify_status(401));
    }

    // --- parse_usage (tolerant, both shapes) ---

    // --- fixtures (frozen captures; never re-baselined) ---

    /// `CAPTURED_NULL_SESSION_QUOTA_FIELDS` (#1591's constant) with the
    /// `"five_hour":null` member removed and nothing else changed — the "absent
    /// key" half of the null-versus-absent pair.
    const CAPTURED_NULL_SESSION_QUOTA_FIELDS_WITHOUT_FIVE_HOUR_KEY: &str = r#"{"seven_day":{"utilization":90.0,"resets_at":"2026-11-06T22:59:59.916061+00:00","limit_dollars":null,"used_dollars":null,"remaining_dollars":null,"locked_reason":null},"limits":[{"kind":"weekly_all","group":"weekly","percent":90,"severity":"critical","resets_at":"2026-11-06T22:59:59.916061+00:00","scope":null,"is_active":true},{"kind":"weekly_scoped","group":"weekly","percent":12,"severity":"normal","resets_at":"2026-11-06T22:59:59.916182+00:00","scope":{"model":{"id":null,"display_name":"Fable"},"surface":null},"is_active":false}]}"#;

    /// The captured two-entry `limits[]` alone — no top-level window objects — so a
    /// weekly lookup has to take the `limits[]` scan rather than the top-level
    /// short-circuit. The entries are the captured bytes, verbatim.
    const CAPTURED_WEEKLY_LIMITS_ONLY: &str = r#"{"limits":[{"kind":"weekly_all","group":"weekly","percent":90,"severity":"critical","resets_at":"2026-11-06T22:59:59.916061+00:00","scope":null,"is_active":true},{"kind":"weekly_scoped","group":"weekly","percent":12,"severity":"normal","resets_at":"2026-11-06T22:59:59.916182+00:00","scope":{"model":{"id":null,"display_name":"Fable"},"surface":null},"is_active":false}]}"#;

    /// `parse_usage`'s session lookup at the `dimension` seam, the alias list copied
    /// byte for byte from its call site.
    fn session_lookup(body: &str) -> Option<(f64, Option<String>, Option<String>)> {
        let root: Value = serde_json::from_str(body).unwrap();
        dimension(&root, "five_hour", &["session", "five_hour"])
    }

    /// `parse_usage`'s weekly lookup at the `dimension` seam, the alias list copied
    /// byte for byte from its call site.
    fn weekly_lookup(body: &str) -> Option<(f64, Option<String>, Option<String>)> {
        let root: Value = serde_json::from_str(body).unwrap();
        dimension(&root, "seven_day", &["weekly_all", "seven_day", "weekly"])
    }

    #[test]
    fn the_captured_null_session_reply_is_frozen_with_its_three_quota_fields() {
        let root: Value = serde_json::from_str(CAPTURED_NULL_SESSION_QUOTA_FIELDS).unwrap();
        let mut keys: Vec<&str> = root
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        keys.sort_unstable();
        assert_eq!(keys, ["five_hour", "limits", "seven_day"]);
        assert_eq!(root["five_hour"], Value::Null);
        let seven_day = &root["seven_day"];
        assert_eq!(seven_day["utilization"].as_f64(), Some(90.0));
        assert_eq!(
            seven_day["resets_at"].as_str(),
            Some("2026-11-06T22:59:59.916061+00:00")
        );
        assert!(seven_day.get("severity").is_none());
        let limits = root["limits"].as_array().unwrap();
        assert_eq!(limits.len(), 2);
        let kinds: Vec<&str> = limits
            .iter()
            .map(|entry| entry["kind"].as_str().unwrap())
            .collect();
        assert_eq!(kinds, ["weekly_all", "weekly_scoped"]);
        let actives: Vec<bool> = limits
            .iter()
            .map(|entry| entry["is_active"].as_bool().unwrap())
            .collect();
        assert_eq!(actives, [true, false]);
        let percents: Vec<u64> = limits
            .iter()
            .map(|entry| entry["percent"].as_u64().unwrap())
            .collect();
        assert_eq!(percents, [90, 12]);
        let severities: Vec<&str> = limits
            .iter()
            .map(|entry| entry["severity"].as_str().unwrap())
            .collect();
        assert_eq!(severities, ["critical", "normal"]);
        assert_eq!(
            limits[1]["resets_at"].as_str(),
            Some("2026-11-06T22:59:59.916182+00:00")
        );
        assert_eq!(CAPTURED_NULL_SESSION_QUOTA_FIELDS.len(), 555);
        assert_eq!(SESSION_ONLY_QUOTA_FIELDS.len(), 80);
        assert_eq!(WINDOWLESS_QUOTA_FIELDS.len(), 47);
        assert_eq!(
            CAPTURED_NULL_SESSION_QUOTA_FIELDS_WITHOUT_FIVE_HOUR_KEY,
            CAPTURED_NULL_SESSION_QUOTA_FIELDS.replacen("\"five_hour\":null,", "", 1)
        );
        assert!(CAPTURED_NULL_SESSION_QUOTA_FIELDS.ends_with(&CAPTURED_WEEKLY_LIMITS_ONLY[1..]));
    }

    #[test]
    fn the_issue_66_live_capture_is_frozen_with_its_byte_length_and_key_set() {
        fn sorted_keys(value: &Value) -> Vec<&str> {
            let mut keys: Vec<&str> = value
                .as_object()
                .unwrap()
                .keys()
                .map(String::as_str)
                .collect();
            keys.sort_unstable();
            keys
        }
        let source = include_str!("usage.rs");
        // Assembled from pieces: this test sits before the one it reads, so a whole-string
        // needle would match its own text first.
        let anchor = [
            "fn ",
            "parses_the_live_response_shape_utilization_on_a_0_to_100_scale",
            "() {",
        ]
        .concat();
        assert_eq!(source.matches(anchor.as_str()).count(), 1);
        let after_anchor = &source[source.find(anchor.as_str()).unwrap() + anchor.len()..];
        let open = after_anchor.find("r#\"").unwrap() + 3;
        let close = open + after_anchor[open..].find("\"#").unwrap();
        let body = &after_anchor[open..close];
        assert_eq!(body.len(), 399);
        let root: Value = serde_json::from_str(body).unwrap();
        assert_eq!(sorted_keys(&root), ["five_hour", "limits", "seven_day"]);
        assert_eq!(
            sorted_keys(&root["five_hour"]),
            ["resets_at", "utilization"]
        );
        assert_eq!(
            sorted_keys(&root["seven_day"]),
            ["resets_at", "utilization"]
        );
        let limits = root["limits"].as_array().unwrap();
        assert_eq!(limits.len(), 2);
        for entry in limits {
            assert_eq!(sorted_keys(entry), ["is_active", "kind", "percent"]);
        }
        assert_eq!(
            root,
            serde_json::json!({
                "five_hour": {"utilization": 82.0, "resets_at": "2026-06-29T13:40:00.475727+00:00"},
                "seven_day": {"utilization": 15.0, "resets_at": "2026-07-06T00:00:00.475752+00:00"},
                "limits": [
                    {"kind": "session", "percent": 82, "is_active": true},
                    {"kind": "weekly_all", "percent": 15, "is_active": false}
                ]
            })
        );
    }

    #[test]
    fn no_fixture_body_carries_an_identity_field() {
        for body in [
            CAPTURED_NULL_SESSION_QUOTA_FIELDS,
            CAPTURED_NULL_SESSION_QUOTA_FIELDS_WITHOUT_FIVE_HOUR_KEY,
            CAPTURED_WEEKLY_LIMITS_ONLY,
            SESSION_ONLY_QUOTA_FIELDS,
            WINDOWLESS_QUOTA_FIELDS,
        ] {
            assert!(!body.contains('@'), "an `@` in {body}");
            let lowered = body.to_ascii_lowercase();
            for needle in ["email", "uuid", "label", "token", "organization", "account"] {
                assert!(!lowered.contains(needle), "`{needle}` in {body}");
            }
        }
    }

    #[test]
    fn the_captured_null_session_reply_has_no_session_window_at_the_dimension_seam() {
        assert_eq!(session_lookup(CAPTURED_NULL_SESSION_QUOTA_FIELDS), None);
        assert_eq!(
            session_lookup(CAPTURED_NULL_SESSION_QUOTA_FIELDS_WITHOUT_FIVE_HOUR_KEY),
            None
        );
        assert_eq!(session_lookup(CAPTURED_WEEKLY_LIMITS_ONLY), None);
    }

    #[test]
    fn the_captured_null_session_reply_reads_its_weekly_window_from_the_top_level_object() {
        for body in [
            CAPTURED_NULL_SESSION_QUOTA_FIELDS,
            CAPTURED_NULL_SESSION_QUOTA_FIELDS_WITHOUT_FIVE_HOUR_KEY,
        ] {
            let (fraction, resets_at, severity) = weekly_lookup(body).expect("a weekly window");
            assert!(
                (fraction - 0.90).abs() < 1e-9,
                "weekly fraction was {fraction}"
            );
            assert_eq!(
                resets_at.as_deref(),
                Some("2026-11-06T22:59:59.916061+00:00")
            );
            // `None` deliberately (issue #1606): `dimension` returns the top-level `seven_day`
            // object, which carries no `severity`, before it would reach the `weekly_all`
            // entry's "critical". Whoever closes that provenance gap moves this one assertion.
            assert_eq!(severity, None);
        }
    }

    #[test]
    fn the_captured_weekly_limits_read_weekly_all_and_skip_weekly_scoped() {
        let (fraction, resets_at, severity) =
            weekly_lookup(CAPTURED_WEEKLY_LIMITS_ONLY).expect("a weekly window from limits[]");
        assert!(
            (fraction - 0.90).abs() < 1e-9,
            "weekly fraction was {fraction}"
        );
        assert!(
            (fraction - 0.12).abs() >= 1e-9,
            "weekly_scoped's percent was read: {fraction}"
        );
        assert_eq!(
            resets_at.as_deref(),
            Some("2026-11-06T22:59:59.916061+00:00")
        );
        assert_ne!(
            resets_at.as_deref(),
            Some("2026-11-06T22:59:59.916182+00:00")
        );
        assert_eq!(severity.as_deref(), Some("critical"));
        assert_ne!(severity.as_deref(), Some("normal"));
    }

    #[test]
    fn a_null_five_hour_and_an_absent_five_hour_key_read_identically() {
        let a = CAPTURED_NULL_SESSION_QUOTA_FIELDS;
        let b = CAPTURED_NULL_SESSION_QUOTA_FIELDS_WITHOUT_FIVE_HOUR_KEY;
        assert_eq!(session_lookup(a), session_lookup(b));
        assert_eq!(session_lookup(a), None);
        assert_eq!(weekly_lookup(a), weekly_lookup(b));
        match (parse_usage(a), parse_usage(b)) {
            (Ok(x), Ok(y)) => assert_eq!(x, y),
            (Err(x), Err(y)) => assert_eq!(x.to_string(), y.to_string()),
            _ => panic!("the two bodies parsed differently"),
        }
    }

    #[test]
    fn the_session_only_and_windowless_bodies_read_their_absent_windows_as_none_at_the_dimension_seam(
    ) {
        let (fraction, resets_at, severity) =
            session_lookup(SESSION_ONLY_QUOTA_FIELDS).expect("a session window");
        assert!(
            (fraction - 0.42).abs() < 1e-9,
            "session fraction was {fraction}"
        );
        // A `null` `resets_at` reads as no stamp, and the window carries no severity.
        assert_eq!(resets_at, None);
        assert_eq!(severity, None);
        assert_eq!(weekly_lookup(SESSION_ONLY_QUOTA_FIELDS), None);
        assert_eq!(session_lookup(WINDOWLESS_QUOTA_FIELDS), None);
        assert_eq!(weekly_lookup(WINDOWLESS_QUOTA_FIELDS), None);
    }

    #[test]
    fn parses_the_limits_array_shape_with_percent() {
        let body = r#"{
            "limits": [
                {"kind": "session", "percent": 42, "resets_at": "2026-06-26T18:00:00Z", "is_active": true},
                {"kind": "weekly_all", "percent": 88, "severity": "critical", "resets_at": "2026-06-30T00:00:00Z", "is_active": true}
            ]
        }"#;
        let report = parse_usage(body).unwrap();
        let session = report.session.expect("session window present");
        let weekly = report.weekly.expect("weekly window present");
        assert!((session - 0.42).abs() < 1e-9);
        assert!((weekly - 0.88).abs() < 1e-9);
        assert_eq!(
            report.session_resets_at.as_deref(),
            Some("2026-06-26T18:00:00Z")
        );
        assert_eq!(
            report.weekly_resets_at.as_deref(),
            Some("2026-06-30T00:00:00Z")
        );
    }

    #[test]
    fn parse_widening_retains_severity_and_prefers_weekly() {
        // Issue #156 parse widening: the `limits[]` `severity` (previously dropped)
        // is retained per window, and the reading's single label prefers the WEEKLY
        // window's, falling back to the session window's.
        let both = r#"{
            "limits": [
                {"kind": "session", "percent": 42, "severity": "warning", "is_active": true},
                {"kind": "weekly_all", "percent": 88, "severity": "critical", "is_active": true}
            ]
        }"#;
        let report = parse_usage(both).unwrap();
        assert_eq!(report.session_severity.as_deref(), Some("warning"));
        assert_eq!(report.weekly_severity.as_deref(), Some("critical"));
        assert_eq!(
            report.severity().as_deref(),
            Some("critical"),
            "weekly wins"
        );

        // Only the session window labels its severity → it is used.
        let session_only = r#"{
            "limits": [
                {"kind": "session", "percent": 20, "severity": "warning", "is_active": true},
                {"kind": "weekly_all", "percent": 10, "is_active": true}
            ]
        }"#;
        assert_eq!(
            parse_usage(session_only).unwrap().severity().as_deref(),
            Some("warning"),
            "falls back to session"
        );

        // Neither window labels a severity → None; the reading still parses cleanly
        // (issue #156 AC: a sample is valid when the optional field is absent).
        let neither = r#"{
            "limits": [
                {"kind": "session", "percent": 20, "is_active": true},
                {"kind": "weekly_all", "percent": 10, "is_active": true}
            ]
        }"#;
        let report = parse_usage(neither).unwrap();
        assert_eq!(report.severity(), None);
        let session = report.session.expect("session window present");
        assert!((session - 0.20).abs() < 1e-9, "dimensions still parse");
    }

    #[test]
    fn parses_the_top_level_window_shape_with_utilization() {
        // `utilization` is a 0..=100 percentage (issue #66): 10.0 → 0.10 fraction.
        let body = r#"{
            "five_hour": {"utilization": 10.0, "resets_at": "2026-06-26T18:00:00Z"},
            "seven_day": {"utilization": 55.0, "resets_at": "2026-06-30T00:00:00Z"}
        }"#;
        let report = parse_usage(body).unwrap();
        let session = report.session.expect("session window present");
        let weekly = report.weekly.expect("weekly window present");
        assert!((session - 0.10).abs() < 1e-9);
        assert!((weekly - 0.55).abs() < 1e-9);
    }

    #[test]
    fn parses_the_live_response_shape_utilization_on_a_0_to_100_scale() {
        // Regression fixture (issue #66): the verbatim shape captured from a live
        // `GET /api/oauth/usage` (HTTP 200). `utilization` is on a 0..=100 scale
        // — matching the sibling `limits[].percent` — NOT a 0..=1 fraction, so
        // `82.0` is 82 % (fraction 0.82), not a saturated full window. Before the
        // fix, `fraction_of` clamped `82.0 → 1.0`, reporting every account `100 %`.
        let body = r#"{
            "five_hour": { "utilization": 82.0, "resets_at": "2026-06-29T13:40:00.475727+00:00" },
            "seven_day": { "utilization": 15.0, "resets_at": "2026-07-06T00:00:00.475752+00:00" },
            "limits": [
                { "kind": "session",   "percent": 82, "is_active": true  },
                { "kind": "weekly_all", "percent": 15, "is_active": false }
            ]
        }"#;
        let report = parse_usage(body).unwrap();
        let session = report.session.expect("session window present");
        let weekly = report.weekly.expect("weekly window present");
        assert!(
            (session - 0.82).abs() < 1e-9,
            "session fraction was {session}"
        );
        assert!((weekly - 0.15).abs() < 1e-9, "weekly fraction was {weekly}");
    }

    #[test]
    fn parses_kind_aliases_in_the_limits_array() {
        // five_hour / seven_day as `kind` values inside limits[].
        let body = r#"{
            "limits": [
                {"kind": "five_hour", "percent": 25},
                {"kind": "seven_day", "percent": 60}
            ]
        }"#;
        let report = parse_usage(body).unwrap();
        let session = report.session.expect("session window present");
        let weekly = report.weekly.expect("weekly window present");
        assert!((session - 0.25).abs() < 1e-9);
        assert!((weekly - 0.60).abs() < 1e-9);
    }

    #[test]
    fn prefers_an_active_limit_entry_over_an_inactive_duplicate() {
        // When the same kind appears twice, the active entry wins over the
        // inactive one (issue #66: inactive is a fallback, not a hard skip).
        let body = r#"{
            "limits": [
                {"kind": "session", "percent": 99, "is_active": false},
                {"kind": "session", "percent": 12, "is_active": true},
                {"kind": "weekly_all", "percent": 30}
            ]
        }"#;
        let report = parse_usage(body).unwrap();
        let session = report.session.expect("session window present");
        assert!((session - 0.12).abs() < 1e-9);
    }

    #[test]
    fn falls_back_to_an_inactive_limit_when_no_active_one_exists() {
        // `is_active: false` means "not the currently-binding limit", NOT "absent"
        // (issue #66). When the ONLY matching entry is inactive, read it rather
        // than fabricating a missing-dimension error — the live API marks
        // `weekly_all` inactive, and here no top-level `seven_day` masks it.
        let body = r#"{
            "limits": [
                {"kind": "session", "percent": 40, "is_active": true},
                {"kind": "weekly_all", "percent": 15, "is_active": false}
            ]
        }"#;
        let report = parse_usage(body).unwrap();
        let session = report.session.expect("session window present");
        let weekly = report.weekly.expect("weekly window present");
        assert!((session - 0.40).abs() < 1e-9);
        assert!((weekly - 0.15).abs() < 1e-9);
    }

    #[test]
    fn an_over_full_reading_clamps_to_a_full_window() {
        // Both spellings are a 0..=100 percentage (issue #66); an over-100 stray
        // reading clamps to a single full window, never beyond — whether it
        // arrives as `percent` or `utilization`.
        let body = r#"{
            "limits": [
                {"kind": "session", "percent": 150},
                {"kind": "weekly_all", "utilization": 150}
            ]
        }"#;
        let report = parse_usage(body).unwrap();
        assert_eq!(report.session, Some(1.0));
        assert_eq!(report.weekly, Some(1.0));
    }

    #[test]
    fn resets_at_tolerates_a_numeric_timestamp() {
        let body = r#"{
            "five_hour": {"utilization": 10.0, "resets_at": 1750960800},
            "seven_day": {"utilization": 20.0}
        }"#;
        let report = parse_usage(body).unwrap();
        assert_eq!(report.session_resets_at.as_deref(), Some("1750960800"));
        // An absent resets_at is tolerated as None, not an error.
        assert_eq!(report.weekly_resets_at, None);
    }

    #[test]
    fn a_two_window_reply_is_served_with_both_windows_present() {
        // A reply carrying both windows parses as it always did: each present window is read
        // as its `Some` value.
        let body = r#"{
            "limits": [
                {"kind": "session", "percent": 42, "resets_at": "2026-06-26T18:00:00Z", "is_active": true},
                {"kind": "weekly_all", "percent": 88, "severity": "critical", "resets_at": "2026-06-30T00:00:00Z", "is_active": true}
            ]
        }"#;
        let report = parse_usage(body).unwrap();
        let session = report.session.expect("session window present");
        let weekly = report.weekly.expect("weekly window present");
        assert!((session - 0.42).abs() < 1e-9);
        assert!((weekly - 0.88).abs() < 1e-9);
    }

    #[test]
    fn a_reply_with_neither_window_is_a_parse_error() {
        // Decision D1 (a): a reply with no applicable window at all stays a hard parse error,
        // never a reading with two absent windows (Oracle C pins the same on the frozen
        // constant; this keeps the inline body).
        let body = r#"{"five_hour":null,"seven_day":null,"limits":[]}"#;
        assert!(matches!(parse_usage(body), Err(Error::UsageParse(_))));
    }

    /// A session-only body is served with its weekly window absent (REQ-RPL-B-002), and the one
    /// rejection left, a windowless body, names both windows it lacks (decision D1 (a)) and
    /// carries no response bytes. The text is read where the error is printed whole, the CLI's
    /// `sessiometer: malformed usage response: …` line from `main`; the daemon's poll
    /// diagnostics keep only the class, `transient`.
    #[test]
    fn a_session_only_body_is_served_and_a_windowless_body_names_both_windows_it_lacks() {
        let session_only = r#"{"limits": [{"kind": "session", "percent": 10}]}"#;
        let report = parse_usage(session_only).expect("a session-only body is served");
        let session = report.session.expect("session window present");
        assert!((session - 0.10).abs() < 1e-9);
        assert_eq!(
            report.weekly, None,
            "the weekly window is absent, not failed"
        );

        let neither = r#"{"five_hour":null,"seven_day":null,"limits":[]}"#;
        match parse_usage(neither) {
            Err(Error::UsageParse(message)) => {
                assert_eq!(message, "no quota window (neither five_hour nor seven_day)")
            }
            other => panic!("expected UsageParse, got {other:?}"),
        }
    }

    // --- every shape the session lookup cannot read is served with the session window absent ---

    /// Asserts the absent-session reading on `body`: served (`Ok`), the session window `None`
    /// with no reset stamp and no severity, and the weekly window read. The weekly fraction is
    /// the caller's, so a body whose weekly entry was mis-read fails here rather than passing on
    /// the session assertions alone.
    fn assert_served_without_a_session_window(body: &str, weekly: f64) {
        let report = parse_usage(body).expect("a weekly-only reply must parse");
        assert_eq!(
            report.session, None,
            "the absent session window reads as None"
        );
        assert_eq!(
            report.session_resets_at, None,
            "no session reset stamp is carried"
        );
        assert_eq!(
            report.session_severity, None,
            "no session severity is carried"
        );
        let read = report.weekly.expect("weekly window present");
        assert!((read - weekly).abs() < 1e-9, "the weekly window is read");
    }

    /// The absent window keys on the session LOOKUP returning `None`, not on `five_hour` being
    /// JSON `null`: a reply with no `five_hour` key at all (and no session entry in `limits[]`)
    /// is the same absent window (decision D2).
    #[test]
    fn a_reply_with_no_five_hour_key_is_served_without_a_session_window() {
        let body = r#"{"seven_day":{"utilization":40.0,"resets_at":"2026-11-06T22:59:59.916061+00:00"},"limits":[{"kind":"weekly_all","group":"weekly","percent":40,"severity":"normal","resets_at":"2026-11-06T22:59:59.916061+00:00","is_active":true}]}"#;
        assert_served_without_a_session_window(body, 0.40);
    }

    /// A `five_hour` OBJECT whose `utilization` is `null` and which carries no `percent`
    /// has no readable fraction, so the lookup returns `None` for it too. Its `resets_at`
    /// and `severity` are deliberately present: the lookup found no window, so neither is
    /// carried onto the reading (invariant I-1).
    #[test]
    fn a_five_hour_object_with_a_null_utilization_is_served_without_a_session_window() {
        let body = r#"{"five_hour":{"utilization":null,"resets_at":"2026-11-02T03:00:00Z","severity":"normal"},"seven_day":{"utilization":40.0,"resets_at":"2026-11-06T22:59:59.916061+00:00"}}"#;
        assert_served_without_a_session_window(body, 0.40);
    }

    /// With no readable top-level `five_hour`, a `limits[]` entry of kind `session` that
    /// carries neither `percent` nor `utilization` is skipped by the lookup (`fraction_of`
    /// finds no fraction), so the lookup finds no readable session window here either —
    /// again with a `resets_at` and `severity` on the entry that must NOT reach the reading.
    #[test]
    fn a_limits_session_entry_with_no_readable_fraction_is_served_without_a_session_window() {
        let body = r#"{"limits":[{"kind":"session","severity":"normal","resets_at":"2026-11-02T03:00:00Z","is_active":true},{"kind":"weekly_all","percent":40,"severity":"normal","resets_at":"2026-11-06T22:59:59.916061+00:00","is_active":true}]}"#;
        assert_served_without_a_session_window(body, 0.40);
    }

    #[test]
    fn non_json_body_is_a_parse_error() {
        assert!(matches!(parse_usage("not json"), Err(Error::UsageParse(_))));
    }

    // --- optional quota windows: the parse-level tests of issue #1596 (umbrella #1590) ---
    //
    // Every fraction below is read through #1591's `optional_fraction`, so each test compiles,
    // and passes `clippy -D warnings`, while `UsageReport.session` / `.weekly` are `f64` and once
    // they are `Option<f64>`: that is what lets N1, N2 and the moved test record their RED against
    // the base before the parser moves. The bodies declared here are synthetic (no observed
    // instance), carry quota fields only, and are never re-baselined.

    /// Synthetic: a session-only `limits[]` entry that carries a reset stamp AND a severity beside
    /// an absent weekly window. No fixture body of #1591 or #1592 has a session stamp or severity
    /// beside an absent weekly window (`SESSION_ONLY_QUOTA_FIELDS` carries `"resets_at":null` and
    /// no severity), so without this body a parser that routes a session stamp or severity into a
    /// weekly slot would pass `a_window_stamp_or_severity_is_present_only_with_its_fraction`.
    /// Stands beside #1591's `SESSION_ONLY_QUOTA_FIELDS` in purpose; declared here because the
    /// oracle block below is frozen (#1596 AC-1).
    const SESSION_LIMITS_ONLY_WITH_STAMP_AND_SEVERITY: &str = r#"{"limits":[{"kind":"session","percent":10,"severity":"normal","resets_at":"2026-11-01T12:00:00+00:00"}]}"#;

    /// The session-only `limits[]` body the moved test
    /// `a_session_only_limits_body_is_served_with_its_weekly_window_absent` parses, byte for
    /// byte, so the invariant tests can list it.
    const SESSION_ONLY_LIMITS_BODY: &str = r#"{"limits": [{"kind": "session", "percent": 10}]}"#;

    /// A two-window `limits[]` body with the values `parses_the_limits_array_shape_with_percent`
    /// parses, on one line so the invariant tests can list it.
    const TWO_WINDOW_LIMITS_BODY: &str = r#"{"limits":[{"kind":"session","percent":42,"resets_at":"2026-06-26T18:00:00Z","is_active":true},{"kind":"weekly_all","percent":88,"severity":"critical","resets_at":"2026-06-30T00:00:00Z","is_active":true}]}"#;

    /// Synthetic: the null-versus-absent pair of
    /// `a_null_five_hour_and_an_absent_five_hour_key_read_identically` (#1592's T6) over bodies
    /// whose `limits[]` DOES carry a readable session entry. T6's bodies carry none, so a session
    /// lookup that returns `None` on a JSON-`null` top-level `five_hour` without scanning
    /// `limits[]` passes T6 at both seams; over this pair the null twin and the absent-key twin
    /// read differently under such a lookup (review finding RF-01 on PR #1636).
    const NULL_FIVE_HOUR_BESIDE_A_LIMITS_SESSION_ENTRY: &str = r#"{"five_hour":null,"seven_day":{"utilization":60.0,"resets_at":"2026-11-06T22:59:59.916061+00:00"},"limits":[{"kind":"session","percent":25,"severity":"normal","resets_at":"2026-11-02T03:00:00+00:00","is_active":true},{"kind":"weekly_all","percent":60,"severity":"warning","resets_at":"2026-11-06T22:59:59.916061+00:00","is_active":true}]}"#;

    /// `NULL_FIVE_HOUR_BESIDE_A_LIMITS_SESSION_ENTRY` with the `"five_hour":null` member removed
    /// and nothing else changed — the absent-key twin.
    const ABSENT_FIVE_HOUR_KEY_BESIDE_A_LIMITS_SESSION_ENTRY: &str = r#"{"seven_day":{"utilization":60.0,"resets_at":"2026-11-06T22:59:59.916061+00:00"},"limits":[{"kind":"session","percent":25,"severity":"normal","resets_at":"2026-11-02T03:00:00+00:00","is_active":true},{"kind":"weekly_all","percent":60,"severity":"warning","resets_at":"2026-11-06T22:59:59.916061+00:00","is_active":true}]}"#;

    /// Synthetic: a top-level `five_hour` OBJECT with no readable fraction (`"utilization":null`,
    /// no `percent`) beside a readable `limits[]` session entry. `dimension` tries the top-level
    /// object first and THEN `limits[]`, so the entry is the session window here; a lookup that
    /// short-circuits on the key's presence (RF-21 mutant (b)) reads no session window. The
    /// entry's reset stamp differs from the object's, so the stamp's provenance is observable.
    const UNREADABLE_FIVE_HOUR_OBJECT_BESIDE_A_LIMITS_SESSION_ENTRY: &str = r#"{"five_hour":{"utilization":null,"resets_at":"2026-11-02T03:00:00+00:00"},"seven_day":{"utilization":60.0,"resets_at":"2026-11-06T22:59:59.916061+00:00"},"limits":[{"kind":"session","percent":25,"severity":"normal","resets_at":"2026-11-02T04:00:00+00:00","is_active":true},{"kind":"weekly_all","percent":60,"severity":"warning","resets_at":"2026-11-06T22:59:59.916061+00:00","is_active":true}]}"#;

    /// Every body the two invariant tests (N3, N4) run over, named: the five fixture bodies of
    /// #1591 and #1592, the synthetic session-only body with a stamp and a severity, the
    /// session-only `limits[]` body of the moved test, the empty object, and one two-window body.
    const INVARIANT_BODIES: [(&str, &str); 9] = [
        (
            "CAPTURED_NULL_SESSION_QUOTA_FIELDS",
            CAPTURED_NULL_SESSION_QUOTA_FIELDS,
        ),
        (
            "CAPTURED_NULL_SESSION_QUOTA_FIELDS_WITHOUT_FIVE_HOUR_KEY",
            CAPTURED_NULL_SESSION_QUOTA_FIELDS_WITHOUT_FIVE_HOUR_KEY,
        ),
        ("CAPTURED_WEEKLY_LIMITS_ONLY", CAPTURED_WEEKLY_LIMITS_ONLY),
        ("SESSION_ONLY_QUOTA_FIELDS", SESSION_ONLY_QUOTA_FIELDS),
        ("WINDOWLESS_QUOTA_FIELDS", WINDOWLESS_QUOTA_FIELDS),
        (
            "SESSION_LIMITS_ONLY_WITH_STAMP_AND_SEVERITY",
            SESSION_LIMITS_ONLY_WITH_STAMP_AND_SEVERITY,
        ),
        ("SESSION_ONLY_LIMITS_BODY", SESSION_ONLY_LIMITS_BODY),
        ("empty object", "{}"),
        ("TWO_WINDOW_LIMITS_BODY", TWO_WINDOW_LIMITS_BODY),
    ];

    /// The synthetic bodies this item declares carry quota fields only — the same scan #1592's
    /// `no_fixture_body_carries_an_identity_field` runs over the five frozen fixtures.
    #[test]
    fn the_synthetic_bodies_of_the_parser_fix_carry_only_quota_fields() {
        for body in [
            SESSION_LIMITS_ONLY_WITH_STAMP_AND_SEVERITY,
            SESSION_ONLY_LIMITS_BODY,
            TWO_WINDOW_LIMITS_BODY,
            NULL_FIVE_HOUR_BESIDE_A_LIMITS_SESSION_ENTRY,
            ABSENT_FIVE_HOUR_KEY_BESIDE_A_LIMITS_SESSION_ENTRY,
            UNREADABLE_FIVE_HOUR_OBJECT_BESIDE_A_LIMITS_SESSION_ENTRY,
        ] {
            let root: Value = serde_json::from_str(body).expect("well-formed JSON");
            assert!(root.is_object(), "a JSON object: {body}");
            assert!(!body.contains('@'), "an `@` in {body}");
            let lowered = body.to_ascii_lowercase();
            for needle in ["email", "uuid", "label", "token", "organization", "account"] {
                assert!(!lowered.contains(needle), "`{needle}` in {body}");
            }
        }
    }

    /// N1 (#1596 § 4b; decision D2, *identical*): `"five_hour":null` and an absent `five_hour`
    /// key are served the same. RED at `fd8640b` (both `Err(UsageParse)`, the `expect` fails)
    /// and on a tree that substitutes `0.0` for an absent session window (both `Ok`, the
    /// session read as `0.0` where `None` is expected).
    #[test]
    fn the_absent_five_hour_key_reply_is_served_exactly_like_the_null_one() {
        let null_body = parse_usage(CAPTURED_NULL_SESSION_QUOTA_FIELDS)
            .expect("N1: the null-session reply must parse Ok");
        let absent_key = parse_usage(CAPTURED_NULL_SESSION_QUOTA_FIELDS_WITHOUT_FIVE_HOUR_KEY)
            .expect("N1: the absent-key reply must parse Ok");
        assert_eq!(null_body, absent_key, "N1: the two reports must be equal");
        for report in [&null_body, &absent_key] {
            let session = optional_fraction(report.session);
            assert_eq!(session, None, "N1: session window — must be absent");
            assert_eq!(
                report.session_resets_at, None,
                "N1: session window — an absent window has no reset stamp"
            );
            assert_eq!(
                report.session_severity, None,
                "N1: session window — an absent window has no severity"
            );
            let weekly = optional_fraction(report.weekly);
            assert!(
                weekly.is_some_and(|fraction| (fraction - 0.90).abs() < 1e-9),
                "N1: weekly window — expected Some(0.90) within 1e-9, got {weekly:?}"
            );
            assert_eq!(
                report.weekly_resets_at.as_deref(),
                Some("2026-11-06T22:59:59.916061+00:00"),
                "N1: weekly window — the raw reset stamp is kept verbatim"
            );
            // `None` while #1606 is unmerged: `dimension` reads the top-level `seven_day`
            // object, which carries no `severity` (#1592's T4 pins the same `None`).
            assert_eq!(
                report.weekly_severity, None,
                "N1: weekly window — the top-level `seven_day` carries no severity"
            );
        }
    }

    /// N2 (#1596 § 4b; REQ-QW-B-001): the captured weekly-only `limits[]` is served from the
    /// active `weekly_all` entry, the unlisted `weekly_scoped` kind neither fails the parse nor is
    /// read as a window, and the session window is absent. RED at `fd8640b` (`Err(UsageParse)`,
    /// the `expect` fails) and on a tree that substitutes `0.0` for an absent session window
    /// (`Ok`, the session read as `0.0` where `None` is expected).
    #[test]
    fn a_weekly_limits_only_reply_is_served_from_weekly_all_and_skips_weekly_scoped() {
        let report = parse_usage(CAPTURED_WEEKLY_LIMITS_ONLY)
            .expect("N2: the weekly-limits-only reply must parse Ok");
        let session = optional_fraction(report.session);
        assert_eq!(session, None, "N2: session window — must be absent");
        let weekly = optional_fraction(report.weekly).expect("N2: weekly window — must be present");
        assert!(
            (weekly - 0.90).abs() < 1e-9,
            "N2: weekly window — expected 0.90 within 1e-9, got {weekly}"
        );
        assert!(
            (weekly - 0.12).abs() >= 1e-9,
            "N2: weekly window — weekly_scoped's percent was read: {weekly}"
        );
        assert_eq!(
            report.weekly_resets_at.as_deref(),
            Some("2026-11-06T22:59:59.916061+00:00"),
            "N2: weekly window — weekly_all's reset stamp"
        );
        assert_ne!(
            report.weekly_resets_at.as_deref(),
            Some("2026-11-06T22:59:59.916182+00:00"),
            "N2: weekly window — weekly_scoped's reset stamp was read"
        );
        assert_eq!(
            report.weekly_severity.as_deref(),
            Some("critical"),
            "N2: weekly window — weekly_all's severity"
        );
        assert_ne!(
            report.weekly_severity.as_deref(),
            Some("normal"),
            "N2: weekly window — weekly_scoped's severity was read"
        );
    }

    /// N3 (#1596 § 4b; invariant I-1): a window's reset stamp or severity is present only
    /// together with its fraction, on both axes, for every served body in `INVARIANT_BODIES`.
    /// Green by construction (`dimension` returns the three together); kept because a later
    /// consumer may read a stamp without re-checking its fraction, and it detects a parser that
    /// spreads one window's lookup over the other window's slots (mutant rows (i), (iii), (iv)
    /// and (v) of #1596 § 6).
    #[test]
    fn a_window_stamp_or_severity_is_present_only_with_its_fraction() {
        for (name, body) in INVARIANT_BODIES {
            let Ok(report) = parse_usage(body) else {
                continue;
            };
            let session = optional_fraction(report.session);
            if report.session_resets_at.is_some() || report.session_severity.is_some() {
                assert!(
                    session.is_some(),
                    "N3: {name}: a session reset stamp or severity beside no session fraction: {report:?}"
                );
            }
            let weekly = optional_fraction(report.weekly);
            if report.weekly_resets_at.is_some() || report.weekly_severity.is_some() {
                assert!(
                    weekly.is_some(),
                    "N3: {name}: a weekly reset stamp or severity beside no weekly fraction: {report:?}"
                );
            }
        }
    }

    /// N4 (#1596 § 4b; invariant I-2 under decision D1 (a)): a served reading carries at least
    /// one window, and exactly the windowless body and the empty object are rejected. Detects a
    /// parser that serves two absent windows (mutant row (ii) of #1596 § 6).
    #[test]
    fn a_served_reading_carries_at_least_one_window() {
        let mut rejected = Vec::new();
        for (name, body) in INVARIANT_BODIES {
            match parse_usage(body) {
                Err(Error::UsageParse(_)) => rejected.push(name),
                Err(other) => panic!("N4: {name}: not a parse error: {other:?}"),
                Ok(report) => {
                    let session = optional_fraction(report.session);
                    let weekly = optional_fraction(report.weekly);
                    assert!(
                        session.is_some() || weekly.is_some(),
                        "N4: {name}: a served reading with no window: {report:?}"
                    );
                }
            }
        }
        assert_eq!(
            rejected,
            ["WINDOWLESS_QUOTA_FIELDS", "empty object"],
            "N4: exactly the windowless body and the empty object are parse errors"
        );
    }

    /// RF-01 (PR #1636's review, carried onto #1596): a JSON-`null` top-level `five_hour` falls
    /// through to the `limits[]` session entry exactly as an absent key does, at the lookup seam
    /// and at `parse_usage`. A lookup that short-circuits on `null` (RF-21 mutant (a)) reads the
    /// null twin as having no session window and the absent-key twin as `0.25`, so the two
    /// outcomes differ and this fails; T6's bodies carry no session entry, so that lookup passes
    /// T6.
    #[test]
    fn a_null_five_hour_falls_through_to_the_limits_session_entry_like_an_absent_key() {
        let null_twin = NULL_FIVE_HOUR_BESIDE_A_LIMITS_SESSION_ENTRY;
        let absent_key_twin = ABSENT_FIVE_HOUR_KEY_BESIDE_A_LIMITS_SESSION_ENTRY;
        assert_eq!(
            absent_key_twin,
            null_twin.replacen("\"five_hour\":null,", "", 1),
            "the twins differ in exactly the one member"
        );
        assert_eq!(session_lookup(null_twin), session_lookup(absent_key_twin));
        let (fraction, resets_at, severity) = session_lookup(null_twin)
            .expect("RF-01: the limits[] session entry is the session window");
        assert!(
            (fraction - 0.25).abs() < 1e-9,
            "RF-01: session fraction at the lookup seam was {fraction}"
        );
        assert_eq!(resets_at.as_deref(), Some("2026-11-02T03:00:00+00:00"));
        assert_eq!(severity.as_deref(), Some("normal"));
        let null_report = parse_usage(null_twin).expect("RF-01: the null twin must parse Ok");
        let absent_key_report =
            parse_usage(absent_key_twin).expect("RF-01: the absent-key twin must parse Ok");
        assert_eq!(
            null_report, absent_key_report,
            "RF-01: the two reports must be equal"
        );
        let session = optional_fraction(null_report.session);
        assert!(
            session.is_some_and(|fraction| (fraction - 0.25).abs() < 1e-9),
            "RF-01: session window — expected Some(0.25) from the limits[] entry, got {session:?}"
        );
        assert_eq!(
            null_report.session_resets_at.as_deref(),
            Some("2026-11-02T03:00:00+00:00"),
            "RF-01: session window — the limits[] entry's reset stamp"
        );
        assert_eq!(
            null_report.session_severity.as_deref(),
            Some("normal"),
            "RF-01: session window — the limits[] entry's severity"
        );
        let weekly = optional_fraction(null_report.weekly);
        assert!(
            weekly.is_some_and(|fraction| (fraction - 0.60).abs() < 1e-9),
            "RF-01: weekly window — expected Some(0.60), got {weekly:?}"
        );
    }

    /// RF-21 (b)'s pin: a top-level `five_hour` object with no readable fraction falls through
    /// to the `limits[]` session entry, whose fraction, reset stamp and severity are read
    /// together (never the unreadable object's stamp). A top-level read that short-circuits on
    /// the key's presence reads no session window here and fails; RF-21 mutant (a) does not
    /// reach this body (the object is not JSON `null`), which is why the null twins above and
    /// this body are separate pins.
    #[test]
    fn a_five_hour_object_without_a_readable_fraction_falls_through_to_the_limits_session_entry() {
        let body = UNREADABLE_FIVE_HOUR_OBJECT_BESIDE_A_LIMITS_SESSION_ENTRY;
        let (fraction, resets_at, severity) = session_lookup(body)
            .expect("RF-21 (b): the limits[] session entry is the session window");
        assert!(
            (fraction - 0.25).abs() < 1e-9,
            "RF-21 (b): session fraction at the lookup seam was {fraction}"
        );
        assert_eq!(
            resets_at.as_deref(),
            Some("2026-11-02T04:00:00+00:00"),
            "the entry's stamp, not the unreadable object's"
        );
        assert_eq!(severity.as_deref(), Some("normal"));
        let report = parse_usage(body).expect("RF-21 (b): the body must parse Ok");
        let session = optional_fraction(report.session);
        assert!(
            session.is_some_and(|fraction| (fraction - 0.25).abs() < 1e-9),
            "RF-21 (b): session window — expected Some(0.25) from the limits[] entry, got {session:?}"
        );
        assert_eq!(
            report.session_resets_at.as_deref(),
            Some("2026-11-02T04:00:00+00:00"),
            "RF-21 (b): session window — the limits[] entry's reset stamp"
        );
        assert_eq!(
            report.session_severity.as_deref(),
            Some("normal"),
            "RF-21 (b): session window — the limits[] entry's severity"
        );
        let weekly = optional_fraction(report.weekly);
        assert!(
            weekly.is_some_and(|fraction| (fraction - 0.60).abs() < 1e-9),
            "RF-21 (b): weekly window — expected Some(0.60), got {weekly:?}"
        );
    }

    /// The moved test (#1596 § 4c; ledger row 10, formerly
    /// `a_missing_dimension_is_a_parse_error_not_a_fabricated_zero`, which asserted
    /// `Err(UsageParse)` on this body). Oracle D covers the top-level-object form of the same
    /// reading; this keeps the `limits[]` form the old test pinned. RED at `fd8640b` and on
    /// every tree that rejects a reply with no weekly window: `Err`, the `expect` fails.
    #[test]
    fn a_session_only_limits_body_is_served_with_its_weekly_window_absent() {
        // A window the reply does not carry is `None`, never a fabricated `0` — the weekly
        // window here is absent, not failed.
        let body = r#"{"limits": [{"kind": "session", "percent": 10}]}"#;
        let report = parse_usage(body).expect("the session-only limits[] body must parse Ok");
        let session = optional_fraction(report.session);
        assert!(
            session.is_some_and(|fraction| (fraction - 0.10).abs() < 1e-9),
            "session window — expected Some(0.10) within 1e-9, got {session:?}"
        );
        let weekly = optional_fraction(report.weekly);
        assert_eq!(
            weekly, None,
            "weekly window — must be absent, never 0.0 or any fraction"
        );
        assert_eq!(
            report.weekly_resets_at, None,
            "weekly window — an absent window has no reset stamp"
        );
        assert_eq!(
            report.weekly_severity, None,
            "weekly window — an absent window has no severity"
        );
    }

    // --- optional quota windows: RED oracles A–D, landed ignored (issue #1591, umbrella #1590) ---
    //
    // Since 2026-10-30 a verified Ultra account's usage reply carries `"five_hour": null`. At
    // `fd8640b`, `parse_usage` rejected that whole reply because it required both windows. The
    // four oracles below grade the parser fix that serves such a reply (design blocks B-1 and B-2
    // under #1590) and land BEFORE it: `#[ignore]`d so the suite stays green, each proven RED by
    // an exact-name `--ignored` run: A, B and D against `fd8640b`, C against the scratch mutant
    // that fabricates both windows (at `fd8640b` C is GREEN, as its doc comment says). On a tree
    // that carries the #1613 hot-fix, which reads that reply's session window as `0.0`, A passes
    // and B fails on its session-window assertion instead of its `expect`; C and D are unchanged.
    // The fix item removes the four `#[ignore]` attributes and edits nothing else in this block.
    // The fixture bodies carry quota fields only, never an identity field.

    /// The captured quota fields of a verified Ultra account's reply (2026-11-01T07:20:52Z):
    /// `five_hour` is JSON null, the weekly window is in `seven_day` and in `limits[weekly_all]`,
    /// and a `weekly_scoped` entry is present and inactive. No identity field, ever.
    const CAPTURED_NULL_SESSION_QUOTA_FIELDS: &str = r#"{"five_hour":null,"seven_day":{"utilization":90.0,"resets_at":"2026-11-06T22:59:59.916061+00:00","limit_dollars":null,"used_dollars":null,"remaining_dollars":null,"locked_reason":null},"limits":[{"kind":"weekly_all","group":"weekly","percent":90,"severity":"critical","resets_at":"2026-11-06T22:59:59.916061+00:00","scope":null,"is_active":true},{"kind":"weekly_scoped","group":"weekly","percent":12,"severity":"normal","resets_at":"2026-11-06T22:59:59.916182+00:00","scope":{"model":{"id":null,"display_name":"Fable"},"surface":null},"is_active":false}]}"#;

    /// A reply that carries no window at all (no observed instance; pins decision D1).
    const WINDOWLESS_QUOTA_FIELDS: &str = r#"{"five_hour":null,"seven_day":null,"limits":[]}"#;

    /// A reply that carries a session window and no weekly window (no observed instance; the
    /// operator's "maybe eventually they will make some account w/o weekly limits").
    const SESSION_ONLY_QUOTA_FIELDS: &str =
        r#"{"five_hour":{"utilization":42.0,"resets_at":null},"seven_day":null,"limits":[]}"#;

    /// Reads a window's fraction the same way before and after the `f64` → `Option<f64>` lift
    /// (issue #1590), so one assertion text compiles, and passes `clippy -D warnings`, on both sides.
    fn optional_fraction(window: impl Into<Option<f64>>) -> Option<f64> {
        window.into()
    }

    /// Oracle A grades REQ-RPL-B-001, "a null session window is served, not rejected": the
    /// captured null-session reply parses `Ok`, its weekly window reads `0.90` from the top-level
    /// `seven_day` (the active `weekly_all` entry agrees; the inactive `weekly_scoped` entry's
    /// `0.12` would be wrong), its raw weekly reset stamp is kept, and its weekly severity is
    /// `None` because the top-level object carries none — pinned so that the separate
    /// top-level-shape severity defect (#1606) is never conflated with this fix.
    /// RED at `fd8640b`: the `expect` fails with `UsageParse("no session (five_hour) dimension")`.
    /// Un-ignore obligation: the parser fix item under #1590 removes the attributes and edits
    /// nothing else here.
    #[test]
    fn oracle_a_the_captured_null_session_reply_parses_with_its_weekly_window() {
        let report = parse_usage(CAPTURED_NULL_SESSION_QUOTA_FIELDS)
            .expect("oracle A: the captured null-session reply must parse Ok");
        let weekly = optional_fraction(report.weekly);
        assert!(
            weekly.is_some_and(|fraction| (fraction - 0.90).abs() < 1e-9),
            "oracle A: weekly window — expected Some(0.90) within 1e-9, got {weekly:?}"
        );
        assert_eq!(
            report.weekly_resets_at.as_deref(),
            Some("2026-11-06T22:59:59.916061+00:00"),
            "oracle A: weekly window — the raw reset stamp must be kept verbatim"
        );
        assert_eq!(
            report.weekly_severity, None,
            "oracle A: weekly window — the top-level `seven_day` carries no severity"
        );
    }

    /// Oracle B grades REQ-RPL-C-001, "never a fabricated window value, kept and widened", and
    /// the session half of REQ-RPL-B-001: the captured reply's absent session window is reported
    /// absent, never as `0.0` or any fraction, and carries no reset stamp and no severity (a
    /// window that does not apply has neither). A parser that fills the missing window with
    /// `0.0` passes Oracle A and fails here, which is why A alone does not suffice.
    /// RED at `fd8640b`: the `expect` fails with `UsageParse("no session (five_hour) dimension")`.
    /// Un-ignore obligation: the parser fix item under #1590 removes the attributes and edits
    /// nothing else here.
    #[test]
    fn oracle_b_the_captured_reply_reports_no_session_window_never_a_fraction() {
        let report = parse_usage(CAPTURED_NULL_SESSION_QUOTA_FIELDS).expect(
            "oracle B: the captured reply must parse Ok before its session window can be read",
        );
        let session = optional_fraction(report.session);
        assert_eq!(
            session, None,
            "oracle B: session window — must be absent, never 0.0 or any fraction"
        );
        assert_eq!(
            report.session_resets_at, None,
            "oracle B: session window — an absent window has no reset stamp"
        );
        assert_eq!(
            report.session_severity, None,
            "oracle B: session window — an absent window has no severity"
        );
    }

    /// Oracle C grades REQ-RPL-B-003, "a reply with no applicable window takes D1's answer":
    /// under decision D1 (a), the standing default, a 200 reply with no applicable window stays a
    /// hard parse error and never yields a fraction. The error text is deliberately not pinned,
    /// because D1 decides the outcome, not the wording: at `fd8640b` the rejection names only
    /// the session window, and from the #1613 hot-fix on it names both. GREEN at `fd8640b` and
    /// on a tree carrying #1613, and RED against a parser that fabricates both windows as `0.0`.
    /// Un-ignore obligation: the parser fix item under #1590 removes the attributes and edits
    /// nothing else here.
    #[test]
    fn oracle_c_a_reply_with_no_window_is_never_two_fabricated_windows() {
        assert!(
            matches!(
                parse_usage(WINDOWLESS_QUOTA_FIELDS),
                Err(Error::UsageParse(_))
            ),
            "oracle C: a windowless reply is a parse error under D1 (a), never a reading"
        );
    }

    /// Oracle D grades REQ-RPL-B-002, "an absent weekly window is served too" (marked `[ASSUMED]`
    /// in the PRD: no such reply has been observed): a session-only reply parses `Ok`, its session
    /// window reads `0.42`, its weekly window is reported absent, never as `0.0` or any fraction,
    /// with no weekly reset stamp and no weekly severity; and its session reset stamp is `None`
    /// because the body's `resets_at` is JSON null.
    /// RED at `fd8640b`: the `expect` fails with `UsageParse("no weekly (seven_day) dimension")`.
    /// Un-ignore obligation: the parser fix item under #1590 removes the attributes and edits
    /// nothing else here.
    #[test]
    fn oracle_d_a_session_only_reply_parses_with_its_weekly_window_absent() {
        let report = parse_usage(SESSION_ONLY_QUOTA_FIELDS)
            .expect("oracle D: a session-only reply must parse Ok");
        let session = optional_fraction(report.session);
        let weekly = optional_fraction(report.weekly);
        assert!(
            session.is_some_and(|fraction| (fraction - 0.42).abs() < 1e-9),
            "oracle D: session window — expected Some(0.42) within 1e-9, got {session:?}"
        );
        assert_eq!(
            weekly, None,
            "oracle D: weekly window — must be absent, never 0.0 or any fraction"
        );
        assert_eq!(
            report.weekly_resets_at, None,
            "oracle D: weekly window — an absent window has no reset stamp"
        );
        assert_eq!(
            report.weekly_severity, None,
            "oracle D: weekly window — an absent window has no severity"
        );
        assert_eq!(
            report.session_resets_at, None,
            "oracle D: session window — the body's `resets_at` is JSON null, so no stamp"
        );
    }

    /// The three oracle fixture bodies are well-formed JSON objects whose top-level keys are
    /// exactly `five_hour`, `seven_day` and `limits`, and the window an oracle reads as absent is
    /// JSON null in its body: `five_hour` in the captured and windowless bodies, `seven_day` in
    /// the windowless and session-only bodies. A RED above is therefore never a malformed
    /// fixture, and no identity field was ever pasted into a body. Not ignored: it passes today
    /// and stays green.
    #[test]
    fn the_oracle_fixture_bodies_are_well_formed_json_carrying_only_quota_fields() {
        // (constant name, body, `five_hour` must be JSON null, `seven_day` must be JSON null)
        let bodies = [
            (
                "CAPTURED_NULL_SESSION_QUOTA_FIELDS",
                CAPTURED_NULL_SESSION_QUOTA_FIELDS,
                true,
                false,
            ),
            (
                "WINDOWLESS_QUOTA_FIELDS",
                WINDOWLESS_QUOTA_FIELDS,
                true,
                true,
            ),
            (
                "SESSION_ONLY_QUOTA_FIELDS",
                SESSION_ONLY_QUOTA_FIELDS,
                false,
                true,
            ),
        ];
        let quota_keys = std::collections::BTreeSet::from(["five_hour", "limits", "seven_day"]);
        for (name, body, five_hour_is_null, seven_day_is_null) in bodies {
            let root: Value = serde_json::from_str(body)
                .unwrap_or_else(|err| panic!("{name} is not well-formed JSON: {err}"));
            let keys: std::collections::BTreeSet<&str> = root
                .as_object()
                .unwrap_or_else(|| panic!("{name} is not a JSON object"))
                .keys()
                .map(String::as_str)
                .collect();
            assert_eq!(
                keys, quota_keys,
                "{name}: the top-level keys must be exactly five_hour, seven_day and limits"
            );
            if five_hour_is_null {
                assert_eq!(
                    root.get("five_hour"),
                    Some(&Value::Null),
                    "{name}: five_hour must be JSON null"
                );
            }
            if seven_day_is_null {
                assert_eq!(
                    root.get("seven_day"),
                    Some(&Value::Null),
                    "{name}: seven_day must be JSON null"
                );
            }
        }
    }

    // --- resets_at normalization to epoch (issue #11) ---

    #[test]
    fn epoch_from_resets_at_parses_a_digit_epoch_string() {
        // The numeric API shape arrives stringified (see `resets_at_of`).
        assert_eq!(epoch_from_resets_at("1750960800"), Some(1_750_960_800));
        assert_eq!(epoch_from_resets_at("  1750960800  "), Some(1_750_960_800));
    }

    #[test]
    fn epoch_from_resets_at_parses_an_rfc3339_instant() {
        // Cross-checked against the event log's `rfc3339` renderer, which maps
        // 1_750_960_800 -> 2025-06-26T18:00:00Z (see the observability tests), so
        // the two halves of the round-trip agree.
        assert_eq!(
            epoch_from_resets_at("2025-06-26T18:00:00Z"),
            Some(1_750_960_800)
        );
        // A lower-case `z` and a fractional-seconds part are both tolerated; the
        // fraction is dropped (the log is second-granular).
        assert_eq!(
            epoch_from_resets_at("2025-06-26T18:00:00.512z"),
            Some(1_750_960_800)
        );
    }

    #[test]
    fn epoch_from_rfc3339_applies_a_timezone_offset() {
        // 20:00:00+02:00 and 13:00:00-05:00 are both the 18:00:00Z instant.
        assert_eq!(
            epoch_from_rfc3339("2025-06-26T20:00:00+02:00"),
            Some(1_750_960_800)
        );
        assert_eq!(
            epoch_from_rfc3339("2025-06-26T13:00:00-05:00"),
            Some(1_750_960_800)
        );
    }

    #[test]
    fn epoch_from_rfc3339_handles_a_leap_day() {
        // 2024-02-29 exists (a leap year): pins the leap-day arithmetic.
        assert_eq!(
            epoch_from_rfc3339("2024-02-29T00:00:00Z"),
            Some(1_709_164_800)
        );
        // 2000-02-29 exists too (the 400-year century rule is a leap year), so a
        // valid leap day survives regardless of which leap sub-rule qualifies it.
        assert!(epoch_from_rfc3339("2000-02-29T00:00:00Z").is_some());
    }

    #[test]
    fn epoch_from_rfc3339_rejects_impossible_calendar_dates() {
        // Issue #177: the day was validated only as `1..=31`, so a day past the
        // month's real length silently normalized to a WRONG instant instead of
        // degrading to "reset unknown". A total parser rejects each of these.
        for bad in [
            "2026-02-30T00:00:00Z", // February never has 30 days
            "2025-02-29T00:00:00Z", // 2025 is not a leap year — no Feb 29
            "2100-02-29T00:00:00Z", // century non-leap (÷100 but not ÷400)
            "2026-04-31T00:00:00Z", // April has 30 days
            "2026-06-31T00:00:00Z", // June has 30 days
            "2026-00-15T00:00:00Z", // month 0 is not a real month
            "2026-01-00T00:00:00Z", // day 0 is not a real day
        ] {
            assert_eq!(epoch_from_rfc3339(bad), None, "{bad} must be rejected");
        }
    }

    #[test]
    fn epoch_from_rfc3339_does_not_panic_on_a_non_ascii_timezone() {
        // Issue #177 (follow-up): the `±HHMM` offset was byte-sliced (`tz[..2]`),
        // which panics when byte index 2 lands mid-char. A `from_utf8_lossy`-decoded
        // upstream `resets_at` can carry such a byte and must degrade to None, not
        // abort the daemon poll loop.
        assert_eq!(epoch_from_rfc3339("2026-06-26T00:00:00+1é2"), None);
        assert_eq!(epoch_from_rfc3339("2026-06-26T00:00:00+é0"), None);
    }

    #[test]
    fn epoch_from_rfc3339_does_not_panic_on_an_overflowing_year() {
        // Issue #177 (follow-up): an unbounded year overflowed the civil-day
        // arithmetic (`era * 146_097`) — a panic on the poll loop. A bounded parser
        // rejects an out-of-range year instead of overflowing.
        assert_eq!(
            epoch_from_rfc3339("99999999999999999-02-15T00:00:00Z"),
            None
        );
        assert_eq!(epoch_from_rfc3339("10000-01-01T00:00:00Z"), None);
    }

    #[test]
    fn epoch_from_resets_at_rejects_unparseable_input() {
        for bad in [
            "",
            "not-a-date",
            "2025-13-01T00:00:00Z", // month out of range
            "2025-06-26",           // date only, no time
            "2025-06-26T25:00:00Z", // hour out of range
        ] {
            assert_eq!(epoch_from_resets_at(bad), None, "{bad} should not parse");
        }
    }

    #[test]
    fn to_usage_normalizes_both_resets_to_epoch() {
        let report = UsageReport {
            session: Some(0.1),
            weekly: Some(0.2),
            session_resets_at: Some("2025-01-01T00:00:00Z".to_owned()),
            weekly_resets_at: Some("2025-06-26T18:00:00Z".to_owned()),
            session_severity: None,
            weekly_severity: None,
        };
        let usage = report.to_usage();
        assert_eq!(usage.weekly_resets_at, Some(1_750_960_800));
        // Both windows' resets are projected for the `status` display (issue #72).
        assert_eq!(usage.session_resets_at, Some(1_735_689_600));
        // An absent reset (either window) projects to None (no fabricated value).
        let no_reset = UsageReport {
            session_resets_at: None,
            weekly_resets_at: None,
            ..report.clone()
        };
        let usage = no_reset.to_usage();
        assert_eq!(usage.weekly_resets_at, None);
        assert_eq!(usage.session_resets_at, None);
    }

    #[test]
    fn to_usage_copies_an_absent_window_through_as_none() {
        // Issue #1597 (M1): the captured weekly-only reply's report — its session window
        // absent, its weekly window present with its reset — projects to a reading whose
        // session is `None`, "does not apply to this account". Nothing between the parser
        // and the reading turns that absence into a number: a present `0.0` would be a measured
        // idle window, a different fact (REQ-RDG-B-001).
        let report = UsageReport {
            session: None,
            weekly: Some(0.90),
            session_resets_at: None,
            weekly_resets_at: Some("2026-11-06T22:59:59.916061+00:00".to_owned()),
            session_severity: None,
            weekly_severity: None,
        };
        let usage = report.to_usage();
        assert_eq!(
            usage.session, None,
            "an absent session window copies through as None, never a fabricated fraction"
        );
        let weekly = usage.weekly.expect("the weekly window is present");
        assert!(
            (weekly - 0.90).abs() < 1e-9,
            "the present weekly fraction copies through: {weekly}"
        );
        assert_eq!(
            usage.session_resets_at, None,
            "no session window, no session reset"
        );
        assert!(
            usage.weekly_resets_at.is_some(),
            "the weekly reset is projected beside its window"
        );
    }

    #[test]
    fn to_usage_copies_an_absent_weekly_window_through_as_none() {
        // Issue #1597 (M6), M1's weekly-axis sibling: the session-only body of #1591's
        // Oracle D — a session window present, no weekly window, neither reset — projects to
        // a reading whose weekly is `None`. The lift is per window and symmetric (the
        // operator's stated direction), so the projection is graded on both axes.
        let report = UsageReport {
            session: Some(0.42),
            weekly: None,
            session_resets_at: None,
            weekly_resets_at: None,
            session_severity: None,
            weekly_severity: None,
        };
        let usage = report.to_usage();
        assert_eq!(
            usage.weekly, None,
            "an absent weekly window copies through as None, never a fabricated fraction"
        );
        let session = usage.session.expect("the session window is present");
        assert!(
            (session - 0.42).abs() < 1e-9,
            "the present session fraction copies through: {session}"
        );
        assert_eq!(usage.session_resets_at, None);
        assert_eq!(usage.weekly_resets_at, None);
    }

    #[test]
    fn the_lifted_reading_types_stay_copy() {
        // Issue #1597 (M2): a compile-time pin of the derive. `Option<f64>` is `Copy`, so the
        // lift leaves `Usage` the lean by-value decision type it was — every
        // `let (prev, next) = (*a, *b)` and `if let Some(reading) = slot` keeps working. The
        // `LastGood` / `BlindAnchor` half of this pin sits in `daemon::records::tests`.
        fn assert_copy<T: Copy>() {}
        assert_copy::<Usage>();
    }

    // --- access_token_from_blob (pure) ---

    #[test]
    fn extracts_the_nested_access_token() {
        let blob = br#"{"claudeAiOauth":{"accessToken":"sk-ant-oat-EXAMPLE","refreshToken":"sk-ant-ort-EXAMPLE"}}"#;
        let token = access_token_from_blob(blob).unwrap();
        assert_eq!(&*token, "sk-ant-oat-EXAMPLE");
    }

    #[test]
    fn tolerates_a_top_level_access_token() {
        assert_eq!(
            &*access_token_from_blob(br#"{"accessToken":"top-level"}"#).unwrap(),
            "top-level"
        );
        assert_eq!(
            &*access_token_from_blob(br#"{"access_token":"snake"}"#).unwrap(),
            "snake"
        );
    }

    #[test]
    fn a_blob_without_a_token_is_unreadable() {
        assert!(matches!(
            access_token_from_blob(br#"{"claudeAiOauth":{"refreshToken":"x"}}"#),
            Err(Error::UsageTokenUnreadable)
        ));
        assert!(matches!(
            access_token_from_blob(br#"{"claudeAiOauth":{"accessToken":""}}"#),
            Err(Error::UsageTokenUnreadable)
        ));
        assert!(matches!(
            access_token_from_blob(b"not json"),
            Err(Error::UsageTokenUnreadable)
        ));
    }

    // --- curl_config (pure, the request shape + token-on-stdin) ---

    #[test]
    fn curl_config_carries_the_url_headers_and_writeout() {
        let config = curl_config("sk-ant-oat-TESTTOKEN");
        assert!(config.contains(&format!("url = \"{USAGE_URL}\"")));
        assert!(config.contains("header = \"Authorization: Bearer sk-ant-oat-TESTTOKEN\""));
        assert!(config.contains(&format!("header = \"{BETA_HEADER}\"")));
        assert!(config.contains("write-out = \"\\n%{http_code}\\n%header{retry-after}\""));
        assert!(config.contains(&format!("max-time = {POLL_TIMEOUT_SECS}")));
    }

    // --- parse_curl_output (pure) ---

    #[test]
    fn parse_curl_output_splits_body_status_and_empty_retry_after() {
        // write-out shape: <body>\n<status>\n<retry-after> — the trailing
        // retry-after line is empty on a normal response.
        let resp = parse_curl_output(b"{\"limits\":[]}\n200\n");
        assert_eq!(resp.status, 200);
        assert_eq!(resp.body, "{\"limits\":[]}");
        assert_eq!(resp.retry_after, None);
    }

    #[test]
    fn parse_curl_output_handles_a_multiline_body() {
        // The two metadata lines are peeled from the end, so newlines inside the
        // body survive.
        let resp = parse_curl_output(b"{\n  \"a\": 1\n}\n403\n");
        assert_eq!(resp.status, 403);
        assert_eq!(resp.body, "{\n  \"a\": 1\n}");
        assert_eq!(resp.retry_after, None);
    }

    #[test]
    fn parse_curl_output_captures_a_numeric_retry_after() {
        // A 429 with `Retry-After: 30` and an empty body.
        let resp = parse_curl_output(b"\n429\n30");
        assert_eq!(resp.status, 429);
        assert_eq!(resp.body, "");
        assert_eq!(resp.retry_after, Some(Duration::from_secs(30)));
    }

    #[test]
    fn parse_curl_output_ignores_an_http_date_retry_after() {
        // The HTTP-date form is not honoured (delta-seconds only) → None, leaving
        // exponential back-off to govern the wait.
        let resp = parse_curl_output(b"\n503\nWed, 21 Oct 2015 07:28:00 GMT");
        assert_eq!(resp.status, 503);
        assert_eq!(resp.retry_after, None);
    }

    #[test]
    fn parse_curl_output_tolerates_a_missing_or_unparseable_status() {
        // No newline at all + non-numeric → status 0 (→ Transient).
        assert_eq!(parse_curl_output(b"garbage").status, 0);
        // A lone newline → empty status + empty retry-after → status 0.
        assert_eq!(parse_curl_output(b"\n").status, 0);
    }

    #[test]
    fn parse_retry_after_honours_only_the_delta_seconds_form() {
        assert_eq!(parse_retry_after("30"), Some(Duration::from_secs(30)));
        assert_eq!(parse_retry_after("  7 "), Some(Duration::from_secs(7)));
        assert_eq!(parse_retry_after("0"), Some(Duration::from_secs(0)));
        // Empty, negative, fractional, and HTTP-date forms all decline to None.
        assert_eq!(parse_retry_after(""), None);
        assert_eq!(parse_retry_after("-5"), None);
        assert_eq!(parse_retry_after("1.5"), None);
        assert_eq!(parse_retry_after("Wed, 21 Oct 2015 07:28:00 GMT"), None);
    }

    // --- RealUsageSource end-to-end (fake transport) ---

    /// Scripts a sequence of transport results, one per `fetch` call.
    struct FakeUsageTransport {
        responses: RefCell<VecDeque<Result<HttpResponse>>>,
    }

    impl FakeUsageTransport {
        fn new(responses: Vec<Result<HttpResponse>>) -> Self {
            Self {
                responses: RefCell::new(responses.into_iter().collect()),
            }
        }

        fn ok(status: u16, body: &str) -> Result<HttpResponse> {
            Ok(HttpResponse {
                status,
                body: body.to_owned(),
                retry_after: None,
            })
        }

        /// A bodyless response carrying a `Retry-After`, for the throttled paths.
        fn ok_retry_after(status: u16, retry_after: Option<Duration>) -> Result<HttpResponse> {
            Ok(HttpResponse {
                status,
                body: String::new(),
                retry_after,
            })
        }
    }

    impl UsageTransport for FakeUsageTransport {
        async fn fetch(&self) -> Result<HttpResponse> {
            self.responses
                .borrow_mut()
                .pop_front()
                .expect("a scripted response for each fetch")
        }
    }

    fn source(responses: Vec<Result<HttpResponse>>) -> RealUsageSource<FakeUsageTransport> {
        RealUsageSource::new(FakeUsageTransport::new(responses))
    }

    #[tokio::test]
    async fn a_success_yields_the_reading() {
        let body =
            r#"{"limits":[{"kind":"session","percent":30},{"kind":"weekly_all","percent":70}]}"#;
        let src = source(vec![FakeUsageTransport::ok(200, body)]);
        let reading = src.usage().await.unwrap();
        let session = reading.usage.session.expect("session window present");
        assert!((session - 0.30).abs() < 1e-9);
        let weekly = reading.usage.weekly.expect("weekly window present");
        assert!((weekly - 0.70).abs() < 1e-9);
        // No `severity` on either limit → the reading carries none (issue #156).
        assert_eq!(reading.severity, None);
    }

    #[tokio::test]
    async fn a_weekly_only_reply_through_the_poll_path_reads_its_session_window_as_none() {
        // The `parse_usage` tests above never reach the `PolledReading` that
        // `RealUsageSource::usage` builds, so the absent window is pinned on the poll path too:
        // `to_usage` copies the parser's `None` through, never a substituted number.
        //
        // A captured weekly-only reply (`five_hour` null, no session limit).
        let weekly_only = r#"{"five_hour":null,"seven_day":{"utilization":90.0,"resets_at":"2026-11-06T22:59:59.916061+00:00","limit_dollars":null,"used_dollars":null,"remaining_dollars":null,"locked_reason":null},"limits":[{"kind":"weekly_all","group":"weekly","percent":90,"severity":"critical","resets_at":"2026-11-06T22:59:59.916061+00:00","scope":null,"is_active":true},{"kind":"weekly_scoped","group":"weekly","percent":12,"severity":"normal","resets_at":"2026-11-06T22:59:59.916182+00:00","scope":{"model":{"id":null,"display_name":"Fable"},"surface":null},"is_active":false}]}"#;
        let src = source(vec![FakeUsageTransport::ok(200, weekly_only)]);
        let reading = src
            .usage()
            .await
            .expect("a weekly-only reply is served through the poll path, not rejected");
        let session = optional_fraction(reading.usage.session);
        assert_eq!(
            session, None,
            "the poll path reads the absent session window as None, never a fraction"
        );
        assert_eq!(
            reading.usage.session_resets_at, None,
            "no session reset stamp"
        );
        let weekly = optional_fraction(reading.usage.weekly);
        assert!(
            weekly.is_some_and(|fraction| (fraction - 0.90).abs() < 1e-9),
            "the weekly window is read: {weekly:?}"
        );

        // The same seam with both windows present reads each as its `Some` value.
        let two_window =
            r#"{"limits":[{"kind":"session","percent":30},{"kind":"weekly_all","percent":70}]}"#;
        let src = source(vec![FakeUsageTransport::ok(200, two_window)]);
        let reading = src
            .usage()
            .await
            .expect("a two-window reply is served through the poll path");
        let session = optional_fraction(reading.usage.session);
        assert!(
            session.is_some_and(|fraction| (fraction - 0.30).abs() < 1e-9),
            "a two-window reply reads its session window as Some: {session:?}"
        );
    }

    #[tokio::test]
    async fn a_present_idle_session_window_through_the_poll_path_reads_as_some_zero() {
        // A present session window at a zero fraction with no reset stamp (a parked account,
        // the warm-up design's row C-1) is a real idle reading: `Some` of zero, never `None`,
        // because the absent window keys on the LOOKUP finding none, not on a zero fraction.
        let idle_session = r#"{"five_hour":{"utilization":0.0,"resets_at":null},"seven_day":{"utilization":40.0,"resets_at":"2026-11-06T22:59:59.916061+00:00"}}"#;
        let src = source(vec![FakeUsageTransport::ok(200, idle_session)]);
        let reading = src
            .usage()
            .await
            .expect("a two-window reply with an idle session is served through the poll path");
        let session = optional_fraction(reading.usage.session);
        assert_eq!(
            session,
            Some(0.0),
            "a present idle session window reads as Some(0.0), never None"
        );
    }

    /// A reply with no `five_hour` key at all, through the poll path: the absent session window
    /// reaches the `PolledReading` for this shape exactly as it does for the captured `null`
    /// body (`a_weekly_only_reply_through_the_poll_path_reads_its_session_window_as_none`).
    #[tokio::test]
    async fn a_reply_with_no_five_hour_key_through_the_poll_path_reads_its_session_window_as_none()
    {
        let no_five_hour = r#"{"seven_day":{"utilization":40.0,"resets_at":"2026-11-06T22:59:59.916061+00:00"},"limits":[{"kind":"weekly_all","group":"weekly","percent":40,"severity":"normal","resets_at":"2026-11-06T22:59:59.916061+00:00","is_active":true}]}"#;
        let src = source(vec![FakeUsageTransport::ok(200, no_five_hour)]);
        let reading = src
            .usage()
            .await
            .expect("a reply with no five_hour key is served through the poll path");
        let session = optional_fraction(reading.usage.session);
        assert_eq!(
            session, None,
            "the poll path reads the missing five_hour key as an absent session window"
        );
        assert_eq!(
            reading.usage.session_resets_at, None,
            "no session reset stamp"
        );
        let weekly = optional_fraction(reading.usage.weekly);
        assert!(
            weekly.is_some_and(|fraction| (fraction - 0.40).abs() < 1e-9),
            "the weekly window is read: {weekly:?}"
        );
    }

    #[tokio::test]
    async fn a_200_with_an_unparseable_body_is_a_parse_error() {
        let src = source(vec![FakeUsageTransport::ok(200, "{}")]);
        assert!(matches!(src.usage().await, Err(Error::UsageParse(_))));
    }

    #[tokio::test]
    async fn forbidden_surfaces_distinctly_from_unauthorized() {
        // Issue #5 acceptance: a 403 (missing usage scope) surfaces as its own typed
        // error, never collapsed into the 401 the dead-credential detection counts.
        let src = source(vec![FakeUsageTransport::ok(403, "")]);
        assert!(matches!(src.usage().await, Err(Error::UsageScopeMissing)));
    }

    #[tokio::test]
    async fn rate_limited_and_server_errors_are_typed() {
        let src = source(vec![
            FakeUsageTransport::ok(429, ""),
            FakeUsageTransport::ok(503, ""),
            FakeUsageTransport::ok(400, ""),
        ]);
        assert!(matches!(
            src.usage().await,
            Err(Error::UsageRateLimited { status: 429, .. })
        ));
        assert!(matches!(
            src.usage().await,
            Err(Error::UsageTransient { status: 503, .. })
        ));
        assert!(matches!(
            src.usage().await,
            Err(Error::UsageRejected { status: 400 })
        ));
    }

    #[tokio::test]
    async fn a_retry_after_threads_into_the_throttled_errors() {
        // Issue #76: the server-advised Retry-After rides the rate-limited and
        // transient errors so the daemon can honour it as a minimum back-off.
        let src = source(vec![
            FakeUsageTransport::ok_retry_after(429, Some(Duration::from_secs(42))),
            FakeUsageTransport::ok_retry_after(503, Some(Duration::from_secs(7))),
            FakeUsageTransport::ok_retry_after(429, None),
        ]);
        assert!(matches!(
            src.usage().await,
            Err(Error::UsageRateLimited { retry_after: Some(d), .. }) if d == Duration::from_secs(42)
        ));
        assert!(matches!(
            src.usage().await,
            Err(Error::UsageTransient { retry_after: Some(d), .. }) if d == Duration::from_secs(7)
        ));
        assert!(matches!(
            src.usage().await,
            Err(Error::UsageRateLimited {
                retry_after: None,
                ..
            })
        ));
    }

    #[tokio::test]
    async fn unauthorized_and_a_transport_failure_each_surface_their_typed_error() {
        let src = source(vec![
            FakeUsageTransport::ok(401, ""),
            Err(Error::Io(std::io::Error::other("boom"))),
        ]);
        assert!(matches!(src.usage().await, Err(Error::UsageUnauthorized)));
        assert!(matches!(src.usage().await, Err(Error::Io(_))));
    }

    // --- no-telemetry / no-other-egress privacy invariant (issue #271) ---
    //
    // Sessiometer phones home for nothing: no analytics, usage telemetry, crash
    // reporting, update pings, or beacons. The ONLY outbound network egress it
    // originates is the documented per-account usage GET the swap logic polls
    // (`USAGE_URL`). These are *capability* guards — deliberately stronger than a
    // behavioural "we observed no egress on this run" check: the same discipline
    // as `scripts/check-no-security-framework.sh`, guarding what the binary CAN
    // do so a refactor cannot silently open a second egress. Because the guarantee
    // is about capability, it holds across every operating mode — start, poll,
    // swap, idle — and everything else: there is simply no code path that could
    // reach the network another way.
    //
    // The tool reaches the network through exactly one mechanism: spawning
    // `/usr/bin/curl` (`CURL`). It links no in-process HTTP/TLS client and opens
    // no raw TCP or UDP socket, so it *cannot* originate a connection except by
    // that one subprocess — which only ever targets `USAGE_URL`. (The daemon's
    // control channel is local on both targets and never leaves the machine: a
    // Unix-domain socket on Unix, and since #1511 a named pipe on Windows, created
    // with `reject_remote_clients` so it is unreachable over SMB. The external
    // `claude` CLI the daemon drives for refresh/login makes its own calls under
    // the user's own credential — that is not Sessiometer's egress.)

    /// Read a repo-root-relative file, anchored at `CARGO_MANIFEST_DIR` so the
    /// scan is tied to the crate under test regardless of the test CWD.
    fn repo_file(rel: &str) -> String {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(rel);
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
    }

    /// Every `.rs` file under `src/`, as `(path, contents)`.
    fn source_files() -> Vec<(std::path::PathBuf, String)> {
        fn walk(dir: &std::path::Path, out: &mut Vec<(std::path::PathBuf, String)>) {
            for entry in std::fs::read_dir(dir).expect("read_dir under src") {
                let path = entry.expect("dir entry").path();
                if path.is_dir() {
                    walk(&path, out);
                } else if path.extension().is_some_and(|ext| ext == "rs") {
                    let text = std::fs::read_to_string(&path).expect("read .rs source");
                    out.push((path, text));
                }
            }
        }
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut out = Vec::new();
        walk(&root, &mut out);
        assert!(!out.is_empty(), "found no source files under src/ to scan");
        out
    }

    #[test]
    fn the_sole_egress_endpoint_is_the_documented_usage_url() {
        // The one allowed destination, and the absolute (PATH-proof) binary that
        // reaches it. A change here is a deliberate change to the privacy contract
        // — keep it in step with the README no-telemetry statement.
        assert_eq!(USAGE_URL, "https://api.anthropic.com/api/oauth/usage");
        assert_eq!(CURL, "/usr/bin/curl");

        // The request we actually build points nowhere else: its only `url = ` line
        // is the documented endpoint.
        let config = curl_config("sk-ant-oat-TESTTOKEN");
        let url_lines: Vec<&str> = config
            .lines()
            .filter(|line| line.trim_start().starts_with("url = "))
            .collect();
        assert_eq!(url_lines.len(), 1, "exactly one request URL is configured");
        assert_eq!(url_lines[0], format!("url = \"{USAGE_URL}\"").as_str());
    }

    #[test]
    fn no_in_process_http_tls_or_telemetry_client_is_linked() {
        // Without an HTTP/TLS client crate the process cannot originate an
        // in-process HTTP(S) request; without a telemetry/analytics/crash SDK it
        // cannot beacon to one. The daemon's local control channel needs none of
        // these on either target — a Unix-domain socket, or a named pipe on Windows,
        // so the dependency graph stays empty of them — the network surface analogue
        // of `scripts/check-no-security-framework.sh` and the CONTRIBUTING.md
        // transport rule ("No TLS / HTTP client ... the one network call rides the
        // system curl"). Scans the committed lockfile, where every package appears
        // as a `name = "<crate>"` line.
        //
        // (`socket2` / `mio` — tokio's transitive socket layer, needed for the LOCAL
        // Unix socket — are intentionally NOT listed; raw-socket *usage* is guarded
        // by kind in the next test, not by banning the layer.)
        const FORBIDDEN_CRATES: &[&str] = &[
            // HTTP clients / stacks
            "reqwest",
            "hyper",
            "h2",
            "ureq",
            "isahc",
            "curl",
            "curl-sys",
            "surf",
            "attohttpc",
            "http-req",
            "minreq",
            // in-process TLS (only needed for in-process HTTPS; the curl subprocess needs none)
            "native-tls",
            "rustls",
            "openssl",
            "openssl-sys",
            "boring",
            "boring-sys",
            "schannel",
            // RPC clients
            "tonic",
            "grpcio",
            // telemetry / analytics / crash-reporting SaaS SDKs
            "sentry",
            "sentry-core",
            "opentelemetry",
            "tracing-opentelemetry",
            "posthog",
            "posthog-rs",
            "segment",
            "amplitude",
            "mixpanel",
            "rudderanalytics",
        ];
        let lockfile = repo_file("Cargo.lock");
        let has_crate = |name: &str| {
            lockfile
                .lines()
                .any(|line| line.trim() == format!("name = \"{name}\""))
        };

        // Positive control: the same predicate MUST find a crate we know is present,
        // so a passing test can never be a silently-broken matcher.
        assert!(
            has_crate("tokio"),
            "lockfile scan is broken: did not find tokio"
        );

        let present: Vec<&str> = FORBIDDEN_CRATES
            .iter()
            .copied()
            .filter(|name| has_crate(name))
            .collect();
        assert!(
            present.is_empty(),
            "forbidden network/telemetry crate(s) entered the dependency graph: {present:?}. \
             Sessiometer must reach the network only by spawning /usr/bin/curl for the usage \
             endpoint — no in-process HTTP/TLS client, no telemetry SDK (issue #271)."
        );
    }

    #[test]
    fn no_raw_tcp_or_udp_socket_primitive_is_used() {
        // tokio's `net` feature is on for the daemon's LOCAL IPC — but that IPC is a
        // Unix-domain socket (UnixStream / UnixListener) on Unix and a NAMED PIPE on
        // Windows since #1511, and neither ever leaves the machine. Every pipe
        // instance sets `reject_remote_clients`, so the Windows half is local-only
        // by construction rather than by convention. A raw TCP or UDP socket WOULD
        // be outbound egress that bypasses the curl seam, so guard the primitives
        // by name; neither the Unix-socket types nor the named-pipe types match.
        // Tokens are assembled from fragments so this guard never matches its own
        // source (usage.rs is itself scanned).
        let forbidden: [String; 4] = [
            concat!("Tcp", "Stream").to_string(),
            concat!("Tcp", "Listener").to_string(),
            concat!("Tcp", "Socket").to_string(),
            concat!("Udp", "Socket").to_string(),
        ];
        let files = source_files();

        // Positive control: the scanner must find a Unix-socket type we know is used,
        // proving a clean result is a real scan, not a scan that read nothing.
        assert!(
            files
                .iter()
                .any(|(_, text)| text.contains(concat!("Unix", "Listener"))),
            "source scan is broken: did not find UnixListener"
        );

        let mut offenders = Vec::new();
        for (path, text) in &files {
            for token in &forbidden {
                if text.contains(token.as_str()) {
                    offenders.push(format!("{}: {token}", path.display()));
                }
            }
        }
        assert!(
            offenders.is_empty(),
            "raw TCP/UDP socket primitive(s) found — outbound egress must ride the curl seam, \
             never a raw socket (issue #271): {offenders:?}"
        );
    }

    #[test]
    fn curl_is_the_only_network_capable_binary_referenced() {
        // The two guards above prove the tool cannot reach the network in-process,
        // so the only residual egress vector is spawning an external network tool.
        // The one sanctioned spawn is `/usr/bin/curl` -> USAGE_URL, in the usage
        // poller (this file). Guard the two realistic shapes of a sneaked-in beacon:
        //   (a) a curl binary literal in any OTHER module, and
        //   (b) another well-known network fetcher anywhere in the tree.
        // The other system tools the daemon drives — /usr/bin/security (keychain),
        // /bin/launchctl, /usr/bin/plutil, osascript — are not network-capable and
        // are not guarded here. The external `claude` CLI (isolated_spawn) is a
        // separate program's egress, resolved dynamically rather than as an absolute
        // literal. Fetcher tokens are assembled from fragments so this guard never
        // matches its own source.
        let other_fetchers: [String; 3] = [
            concat!("wg", "et").to_string(),
            concat!("so", "cat").to_string(),
            concat!("tel", "net").to_string(),
        ];
        let files = source_files();

        // Positive control: the sanctioned poller (skipped below) must itself
        // reference the curl binary — proving the scan actually reads file contents.
        assert!(
            files
                .iter()
                .any(|(path, text)| path.ends_with("usage.rs") && text.contains(CURL)),
            "source scan is broken: usage.rs does not reference the curl binary"
        );

        let mut offenders = Vec::new();
        for (path, text) in &files {
            for tool in &other_fetchers {
                if text.contains(tool.as_str()) {
                    offenders.push(format!("{}: references `{tool}`", path.display()));
                }
            }
            // A curl binary literal is sanctioned ONLY in the usage poller.
            if !path.ends_with("usage.rs") && text.contains(CURL) {
                offenders.push(format!(
                    "{}: references the curl binary `{CURL}`",
                    path.display()
                ));
            }
        }
        assert!(
            offenders.is_empty(),
            "an unsanctioned network CLI reference was found — the only sanctioned network spawn \
             is /usr/bin/curl in the usage poller (issue #271): {offenders:?}"
        );
    }
}
