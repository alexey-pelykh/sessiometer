// Copyright (c) 2026 Oleksii PELYKH
// SPDX-License-Identifier: MIT

//! Per-account usage polling.
//!
//! Normally reads one account's quota from the Claude usage endpoint, read-only:
//!
//! ```text
//! GET https://api.anthropic.com/api/oauth/usage
//! Authorization: Bearer <accessToken>
//! anthropic-beta: oauth-2025-04-20
//! ```
//!
//! Inference-only credentials automatically issue one minimal Messages
//! request and project its quota headers into the same reading. Those probes consume
//! provider usage; they are never an implicit fallback after an HTTP failure.
//!
//! Each method projects its response into a [`Usage`] reading (both windows) for the swap
//! decision. The bearer is the `accessToken` inside the stored
//! `Claude Code-credentials` blob — read through the existing [`CredentialStore`]
//! seam ([`crate::keychain`]); the poller never re-reads or re-mints a token.
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
//!     **distinctly** from 401 on the usage API. An inference-probe 403 is instead
//!     [`Error::UsageRejected`], not a claim about the profile scope.
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
use crate::keychain::{Credential, CredentialStore};

/// Absolute path to the system `curl`. Absolute (not `$PATH`-resolved) so a
/// hijacked `PATH` cannot substitute a different binary for this network call —
/// the same discipline [`crate::keychain`] applies to `security`.
const CURL: &str = "/usr/bin/curl";

/// The read-only per-account usage endpoint.
const USAGE_URL: &str = "https://api.anthropic.com/api/oauth/usage";

/// Inference endpoint for credentials with inference scope but no profile scope.
const INFERENCE_URL: &str = "https://api.anthropic.com/v1/messages";
/// The quota-header probe follows the Claude Code request convention.
const PROBE_USER_AGENT: &str = "claude-code/2.1.0";
/// A real inference request: consumes usage even though the output is discarded.
const PROBE_BODY: &str = concat!(
    r#"{"model":"claude-haiku-4-5-20251001","#,
    r#""max_tokens":1,"messages":["#,
    r#"{"role":"user","content":"hi"}]}"#,
);

/// The OAuth beta opt-in header the usage endpoint requires.
const BETA_HEADER: &str = "anthropic-beta: oauth-2025-04-20";

/// Hard ceiling (seconds) on a single poll, so a hung request can never wedge the
/// poll loop. Comfortably below the default 300 s (5 min) poll interval.
const POLL_TIMEOUT_SECS: u32 = 30;

/// A point-in-time usage reading for one account, across both quota windows.
///
/// The swap decision compares each dimension against its OWN threshold (issue
/// #41: session vs `session_ceiling`, weekly vs the separate `weekly_ceiling`),
/// so the reading carries both fractions and projects neither to a single
/// worst-case scalar.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Usage {
    /// Fraction in `[0.0, 1.0]` of the rolling 5-hour session window consumed.
    pub(crate) session: f64,
    /// Fraction in `[0.0, 1.0]` of the weekly window consumed.
    pub(crate) weekly: f64,
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
    /// The swap-decision reading (both quota dimensions + resets).
    pub(crate) usage: Usage,
    /// The provider-reported severity label for this reading (e.g. `"critical"`),
    /// when the API supplied one — the WEEKLY window's label, falling back to the
    /// SESSION window's. Persisted to the usage-sample store
    /// ([`crate::usage_store::Sample`]); a tolerant free string, never parsed to an
    /// enum, so an unrecognised future value is retained rather than dropped.
    pub(crate) severity: Option<String>,
}

/// The full parse of a usage response: both dimensions plus each window's reset
/// timestamp, kept VERBATIM as the API rendered them (ISO string or epoch). The
/// reset timestamps are extracted here (issue #5 acceptance: "returns session%,
/// account%, resets_at"); [`to_usage`](UsageReport::to_usage) is the swap-decision
/// projection, normalizing the weekly reset to epoch seconds for the all-exhausted
/// terminal logic (#11). Keeping the raw form here means this extraction stays a
/// faithful mirror of the response; normalization is a separate, tested concern.
#[derive(Debug, Clone, PartialEq)]
struct UsageReport {
    session: f64,
    weekly: f64,
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
    /// The swap-decision projection: the two usage dimensions the loop acts on,
    /// plus each window's reset normalized to epoch seconds. The weekly reset feeds
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

/// One request method selected from credential scopes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum UsageMethod {
    /// Read quota JSON without issuing inference.
    UsageApi,
    /// Issue one minimal inference request and read its quota headers.
    InferenceProbe,
}

/// Scope metadata chooses the request, independently of identity and refreshability.
/// Unknown metadata retains the legacy usage API; an HTTP failure never selects a
/// different method. Inference scope without profile scope selects the probe directly.
pub(crate) fn method_for_credential(credential: &Credential) -> Result<UsageMethod> {
    #[derive(serde::Deserialize)]
    struct Scopes<'a> {
        #[serde(rename = "claudeAiOauth", borrow)]
        oauth: OauthScopes<'a>,
    }
    #[derive(serde::Deserialize)]
    struct OauthScopes<'a> {
        scopes: Vec<std::borrow::Cow<'a, str>>,
    }

    let Ok(metadata) = serde_json::from_slice::<Scopes<'_>>(credential.expose()) else {
        return Ok(UsageMethod::UsageApi);
    };
    if metadata
        .oauth
        .scopes
        .iter()
        .any(|scope| scope == "user:profile")
    {
        return Ok(UsageMethod::UsageApi);
    }
    if metadata
        .oauth
        .scopes
        .iter()
        .any(|scope| scope == "user:inference")
    {
        return Ok(UsageMethod::InferenceProbe);
    }
    Err(Error::UsageUnavailable)
}

/// Seam returning the same normalized reading for either retrieval method.
pub(crate) trait UsageSource {
    async fn usage(&self) -> Result<PolledReading>;
}

/// Seam: performs one raw usage HTTP exchange and reports its method and metadata. The
/// real impl ([`CurlTransport`]) reads the token and shells out to `curl`; a
/// test impl returns scripted responses, so the classification / parsing / 401
/// logic in [`RealUsageSource`] is exercised without a network.
pub(crate) trait UsageTransport {
    async fn fetch(&self) -> Result<HttpResponse>;
}

/// One HTTP exchange's result. `status == 0` is the sentinel for "no HTTP
/// response" (curl could not reach the endpoint), classified as Transient.
pub(crate) struct HttpResponse {
    method: UsageMethod,
    status: u16,
    body: String,
    /// The server-advised `Retry-After` (delta-seconds form) when the response
    /// carried that header; `None` otherwise. Threaded into the rate-limited /
    /// transient errors so the daemon can honour it as a MINIMUM back-off wait
    /// (issue #76). Only a `4xx`/`5xx` carrying it is meaningful; on a `2xx` it is
    /// simply ignored.
    retry_after: Option<Duration>,
    quota_headers: QuotaHeaders,
}

/// Only the four quota headers are retained; arbitrary response headers are not logged.
#[derive(Default)]
struct QuotaHeaders {
    session_utilization: String,
    weekly_utilization: String,
    session_reset: String,
    weekly_reset: String,
}

impl HttpResponse {
    fn unreachable(method: UsageMethod) -> Self {
        Self {
            method,
            status: 0,
            body: String::new(),
            retry_after: None,
            quota_headers: QuotaHeaders::default(),
        }
    }
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
            UsageStatus::Success => match response.method {
                UsageMethod::UsageApi => {
                    let report = parse_usage(&response.body)?;
                    Ok(PolledReading {
                        usage: report.to_usage(),
                        severity: report.severity(),
                    })
                }
                UsageMethod::InferenceProbe => Ok(PolledReading {
                    usage: parse_probe_headers(&response.quota_headers)?,
                    severity: None,
                }),
            },
            UsageStatus::ScopeMissing => match response.method {
                UsageMethod::UsageApi => Err(Error::UsageScopeMissing),
                UsageMethod::InferenceProbe => Err(Error::UsageRejected { status }),
            },
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

/// Real transport: read the bearer and perform the selected usage request.
/// Generic over [`CredentialStore`] so the token source can be
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
        let method = method_for_credential(&credential)?;
        // The token-bearing config rides stdin (never argv) and is zeroized on
        // drop. Ambient curl configuration cannot add requests, retries or redirects.
        let config = curl_config(&token, method)?;

        let mut child = Command::new(CURL)
            .arg("--disable")
            .arg("--config")
            .arg("-")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true)
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
            return Ok(HttpResponse::unreachable(method));
        }
        Ok(parse_curl_output(&output.stdout, method))
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

/// Build the secret-bearing stdin configuration, with no ambient curl config or
/// automatic retries. The fixed metadata trailer uses the same `%header{}` facility
/// as the existing Retry-After capture (curl >= 7.84). Probe bodies are discarded.
fn curl_config(token: &str, method: UsageMethod) -> Result<Zeroizing<String>> {
    if token.is_empty()
        || token
            .chars()
            .any(|c| c.is_control() || matches!(c, '"' | '\\'))
    {
        return Err(Error::UsageTokenUnreadable);
    }
    let url = match method {
        UsageMethod::UsageApi => USAGE_URL,
        UsageMethod::InferenceProbe => INFERENCE_URL,
    };
    let mut config = Zeroizing::new(String::new());
    config.push_str("url = \"");
    config.push_str(url);
    config.push_str("\"\n");
    config.push_str("header = \"Authorization: Bearer ");
    config.push_str(token);
    config.push_str("\"\n");
    config.push_str("header = \"");
    config.push_str(BETA_HEADER);
    config.push_str("\"\n");
    config.push_str("header = \"Accept: application/json\"\n");
    if method == UsageMethod::InferenceProbe {
        config.push_str("request = \"POST\"\n");
        config.push_str("header = \"anthropic-version: 2023-06-01\"\n");
        config.push_str("header = \"Content-Type: application/json\"\n");
        config.push_str("header = \"User-Agent: ");
        config.push_str(PROBE_USER_AGENT);
        config.push_str("\"\n");
        let escaped = PROBE_BODY.replace('\\', "\\\\").replace('"', "\\\"");
        config.push_str("data = \"");
        config.push_str(&escaped);
        config.push_str("\"\n");
        config.push_str("output = \"/dev/null\"\n");
    }
    // Quiet progress, but still print transport errors (to stderr, never stdout).
    config.push_str("silent\n");
    config.push_str("show-error\n");
    config.push_str(&format!("max-time = {POLL_TIMEOUT_SECS}\n"));
    config.push_str(concat!(
        "write-out = \"\\n%{http_code}\\n%header{retry-after}",
        "\\n%header{anthropic-ratelimit-unified-5h-utilization}",
        "\\n%header{anthropic-ratelimit-unified-7d-utilization}",
        "\\n%header{anthropic-ratelimit-unified-5h-reset}",
        "\\n%header{anthropic-ratelimit-unified-7d-reset}\"\n",
    ));
    Ok(config)
}

/// Peel six metadata lines from the end, preserving multiline JSON bodies. Empty
/// headers stay empty; an incomplete trailer is a transport failure, never a reading.
fn parse_curl_output(stdout: &[u8], method: UsageMethod) -> HttpResponse {
    let text = String::from_utf8_lossy(stdout);
    let parts: Vec<&str> = text.rsplitn(7, '\n').collect();
    if parts.len() != 7 {
        return HttpResponse::unreachable(method);
    }
    HttpResponse {
        method,
        status: parts[5].trim().parse().unwrap_or(0),
        body: parts[6].to_owned(),
        retry_after: parse_retry_after(parts[4]),
        quota_headers: QuotaHeaders {
            session_utilization: parts[3].to_owned(),
            weekly_utilization: parts[2].to_owned(),
            session_reset: parts[1].to_owned(),
            weekly_reset: parts[0].to_owned(),
        },
    }
}

/// Header utilization is already a fraction, unlike the usage API's percentages.
fn header_fraction(raw: &str, window: &'static str) -> Result<f64> {
    let value = raw
        .trim()
        .parse::<f64>()
        .ok()
        .filter(|value| value.is_finite())
        .ok_or_else(|| {
            Error::UsageParse(format!("missing or invalid {window} utilization header"))
        })?;
    Ok(value.clamp(0.0, 1.0))
}

fn header_reset(raw: &str) -> Option<i64> {
    raw.trim().parse::<i64>().ok().filter(|value| *value > 0)
}

fn parse_probe_headers(headers: &QuotaHeaders) -> Result<Usage> {
    Ok(Usage {
        session: header_fraction(&headers.session_utilization, "five-hour")?,
        weekly: header_fraction(&headers.weekly_utilization, "weekly")?,
        session_resets_at: header_reset(&headers.session_reset),
        weekly_resets_at: header_reset(&headers.weekly_reset),
    })
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

/// Parse a `200` usage body into both dimensions plus reset timestamps.
///
/// Tolerant of the two documented response shapes for each window:
///   - a top-level object — `five_hour` (session) / `seven_day` (weekly) — with a
///     `utilization` percentage (`0..=100`), or
///   - a `limits[]` entry whose `kind` matches the window, with a `percent`
///     (also `0..=100`).
///
/// A window that cannot be found is a hard [`Error::UsageParse`] (never a
/// fabricated `0`): the loop must skip, not swap, on missing data.
fn parse_usage(body: &str) -> Result<UsageReport> {
    let root: Value =
        serde_json::from_str(body).map_err(|_| Error::UsageParse("body is not JSON".into()))?;

    let (session, session_resets_at, session_severity) =
        dimension(&root, "five_hour", &["session", "five_hour"])
            .ok_or_else(|| Error::UsageParse("no session (five_hour) dimension".into()))?;
    let (weekly, weekly_resets_at, weekly_severity) =
        dimension(&root, "seven_day", &["weekly_all", "seven_day", "weekly"])
            .ok_or_else(|| Error::UsageParse("no weekly (seven_day) dimension".into()))?;

    Ok(UsageReport {
        session,
        weekly,
        session_resets_at,
        weekly_resets_at,
        session_severity,
        weekly_severity,
    })
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

    #[tokio::test]
    async fn unsupported_scopes_are_unavailable_without_spawning_curl() {
        use crate::keychain::{Credential, FakeCredentialStore};
        let store = FakeCredentialStore::empty();
        store
            .write(&Credential::new(
                br#"{"claudeAiOauth":{"accessToken":"dummy-token","scopes":["unknown:scope"]}}"#
                    .to_vec(),
            ))
            .await
            .unwrap();
        assert!(matches!(
            CurlTransport::new(store).fetch().await,
            Err(Error::UsageUnavailable)
        ));
    }

    #[test]
    fn method_selection_uses_scopes_without_a_runtime_toggle() {
        for (scopes, expected) in [
            ("[]", None),
            (r#"["user:inference"]"#, Some(UsageMethod::InferenceProbe)),
            (
                r#"["user:profile","user:inference"]"#,
                Some(UsageMethod::UsageApi),
            ),
            (r#"["user:profile"]"#, Some(UsageMethod::UsageApi)),
            (r#"["unknown:scope"]"#, None),
            (
                r#"["user\u003ainference"]"#,
                Some(UsageMethod::InferenceProbe),
            ),
            ("null", Some(UsageMethod::UsageApi)),
            ("42", Some(UsageMethod::UsageApi)),
        ] {
            for refresh in ["null", r#""refresh-present""#] {
                let blob = format!(
                    r#"{{"claudeAiOauth":{{"accessToken":"dummy","scopes":{scopes},"refreshToken":{refresh},"expiresAt":null}}}}"#
                );
                let credential = Credential::new(blob.into_bytes());
                let result = method_for_credential(&credential);
                match expected {
                    Some(method) => assert_eq!(result.unwrap(), method),
                    None => assert!(matches!(result, Err(Error::UsageUnavailable))),
                }
            }
        }
        for blob in [
            br#"{"claudeAiOauth":{"accessToken":"dummy"}}"#.as_slice(),
            br#"{"accessToken":"dummy"}"#.as_slice(),
            b"not json".as_slice(),
        ] {
            let credential = Credential::new(blob.to_vec());
            assert_eq!(
                method_for_credential(&credential).unwrap(),
                UsageMethod::UsageApi
            );
        }
    }
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

    #[test]
    fn parses_the_limits_array_shape_with_percent() {
        let body = r#"{
            "limits": [
                {"kind": "session", "percent": 42, "resets_at": "2026-06-26T18:00:00Z", "is_active": true},
                {"kind": "weekly_all", "percent": 88, "severity": "critical", "resets_at": "2026-06-30T00:00:00Z", "is_active": true}
            ]
        }"#;
        let report = parse_usage(body).unwrap();
        assert!((report.session - 0.42).abs() < 1e-9);
        assert!((report.weekly - 0.88).abs() < 1e-9);
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
        assert!(
            (report.session - 0.20).abs() < 1e-9,
            "dimensions still parse"
        );
    }

    #[test]
    fn parses_the_top_level_window_shape_with_utilization() {
        // `utilization` is a 0..=100 percentage (issue #66): 10.0 → 0.10 fraction.
        let body = r#"{
            "five_hour": {"utilization": 10.0, "resets_at": "2026-06-26T18:00:00Z"},
            "seven_day": {"utilization": 55.0, "resets_at": "2026-06-30T00:00:00Z"}
        }"#;
        let report = parse_usage(body).unwrap();
        assert!((report.session - 0.10).abs() < 1e-9);
        assert!((report.weekly - 0.55).abs() < 1e-9);
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
        assert!(
            (report.session - 0.82).abs() < 1e-9,
            "session fraction was {}",
            report.session
        );
        assert!(
            (report.weekly - 0.15).abs() < 1e-9,
            "weekly fraction was {}",
            report.weekly
        );
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
        assert!((report.session - 0.25).abs() < 1e-9);
        assert!((report.weekly - 0.60).abs() < 1e-9);
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
        assert!((report.session - 0.12).abs() < 1e-9);
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
        assert!((report.session - 0.40).abs() < 1e-9);
        assert!((report.weekly - 0.15).abs() < 1e-9);
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
        assert_eq!(report.session, 1.0);
        assert_eq!(report.weekly, 1.0);
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
    fn a_missing_dimension_is_a_parse_error_not_a_fabricated_zero() {
        // Only session present → weekly missing → hard error (never swap on
        // missing data).
        let body = r#"{"limits": [{"kind": "session", "percent": 10}]}"#;
        assert!(matches!(parse_usage(body), Err(Error::UsageParse(_))));
    }

    #[test]
    fn non_json_body_is_a_parse_error() {
        assert!(matches!(parse_usage("not json"), Err(Error::UsageParse(_))));
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
            session: 0.1,
            weekly: 0.2,
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
        let config = curl_config("sk-ant-oat-TESTTOKEN", UsageMethod::UsageApi).unwrap();
        assert!(config.contains(&format!("url = \"{USAGE_URL}\"")));
        assert!(config.contains("header = \"Authorization: Bearer sk-ant-oat-TESTTOKEN\""));
        assert!(config.contains(&format!("header = \"{BETA_HEADER}\"")));
        assert!(config.contains("write-out = \"\\n%{http_code}\\n%header{retry-after}"));
        assert!(config.contains(&format!("max-time = {POLL_TIMEOUT_SECS}")));
        assert!(!config.contains("request ="));
        assert!(!config.contains("data ="));
        assert!(!config.contains("output ="));
        assert!(!config.contains(INFERENCE_URL));
    }

    // --- parse_curl_output (pure) ---

    #[test]
    fn probe_request_is_one_fixed_minimal_inference_with_no_identity() {
        let config = curl_config("sk-ant-oat-TESTTOKEN", UsageMethod::InferenceProbe).unwrap();
        assert!(config.contains(&format!("url = \"{INFERENCE_URL}\"")));
        assert!(config.contains("request = \"POST\""));
        assert!(config.contains("header = \"anthropic-version: 2023-06-01\""));
        assert!(config.contains(&format!("header = \"User-Agent: {PROBE_USER_AGENT}\"")));
        assert!(config.contains("header = \"Authorization: Bearer sk-ant-oat-TESTTOKEN\""));
        assert!(config.contains("output = \"/dev/null\""));
        assert!(!config.contains(USAGE_URL));
        assert!(!config.contains("retry ="));
        assert!(!config.contains("location"));
        let body: Value = serde_json::from_str(PROBE_BODY).unwrap();
        assert_eq!(body["max_tokens"], 1);
        assert_eq!(
            body["messages"],
            serde_json::json!([{"role":"user","content":"hi"}])
        );
        assert_eq!(body.as_object().unwrap().len(), 3);
        assert!(body.get("metadata").is_none());
        assert!(body.get("account_uuid").is_none());
        assert_eq!(
            config
                .lines()
                .filter(|line| line.starts_with("url ="))
                .count(),
            1
        );
    }

    #[test]
    fn both_request_methods_reject_curl_config_injection_without_echoing_the_token() {
        for method in [UsageMethod::UsageApi, UsageMethod::InferenceProbe] {
            for token in [
                "",
                "secret\nurl = bad",
                "secret\rheader: bad",
                "secret\"",
                "secret\\",
                "secret\0",
            ] {
                let error = curl_config(token, method).unwrap_err();
                assert!(matches!(error, Error::UsageTokenUnreadable));
                assert!(!error.to_string().contains("secret"));
            }
        }
    }

    #[test]
    fn probe_trailer_preserves_fractions_and_epoch_resets() {
        let response = parse_curl_output(
            b"\n200\n\n0.42\n0.17\n1900000000\n1900500000",
            UsageMethod::InferenceProbe,
        );
        assert_eq!(response.status, 200);
        assert_eq!(response.method, UsageMethod::InferenceProbe);
        assert!(response.body.is_empty());
        let usage = parse_probe_headers(&response.quota_headers).unwrap();
        assert_eq!(usage.session, 0.42);
        assert_eq!(usage.weekly, 0.17);
        assert_eq!(usage.session_resets_at, Some(1_900_000_000));
        assert_eq!(usage.weekly_resets_at, Some(1_900_500_000));
    }

    #[test]
    fn probe_usage_requires_both_finite_dimensions_but_resets_are_optional() {
        for raw in ["", "not-a-number", "NaN", "inf", "-inf"] {
            for (session, weekly) in [(raw, "0.17"), ("0.42", raw)] {
                let headers = QuotaHeaders {
                    session_utilization: session.into(),
                    weekly_utilization: weekly.into(),
                    ..QuotaHeaders::default()
                };
                assert!(matches!(
                    parse_probe_headers(&headers),
                    Err(Error::UsageParse(_))
                ));
            }
        }
        for reset in ["", "invalid", "0", "-1", "99999999999999999999999999"] {
            let headers = QuotaHeaders {
                session_utilization: "1.7".into(),
                weekly_utilization: "-0.2".into(),
                session_reset: reset.into(),
                weekly_reset: reset.into(),
            };
            let usage = parse_probe_headers(&headers).unwrap();
            assert_eq!(usage.session, 1.0);
            assert_eq!(usage.weekly, 0.0);
            assert_eq!(usage.session_resets_at, None);
            assert_eq!(usage.weekly_resets_at, None);
        }
    }

    #[test]
    fn parse_curl_output_splits_body_status_and_empty_retry_after() {
        // Six write-out metadata lines follow the body. Retry-After and the
        // four quota headers may all be empty on a normal usage-API response.
        let resp = parse_curl_output(b"{\"limits\":[]}\n200\n\n\n\n\n", UsageMethod::UsageApi);
        assert_eq!(resp.status, 200);
        assert_eq!(resp.body, "{\"limits\":[]}");
        assert_eq!(resp.retry_after, None);
    }

    #[test]
    fn parse_curl_output_handles_a_multiline_body() {
        // The six metadata lines are peeled from the end, so newlines inside the
        // body survive.
        let resp = parse_curl_output(b"{\n  \"a\": 1\n}\n403\n\n\n\n\n", UsageMethod::UsageApi);
        assert_eq!(resp.status, 403);
        assert_eq!(resp.body, "{\n  \"a\": 1\n}");
        assert_eq!(resp.retry_after, None);
    }

    #[test]
    fn parse_curl_output_captures_a_numeric_retry_after() {
        // A 429 with `Retry-After: 30` and an empty body.
        let resp = parse_curl_output(b"\n429\n30\n\n\n\n", UsageMethod::UsageApi);
        assert_eq!(resp.status, 429);
        assert_eq!(resp.body, "");
        assert_eq!(resp.retry_after, Some(Duration::from_secs(30)));
    }

    #[test]
    fn parse_curl_output_ignores_an_http_date_retry_after() {
        // The HTTP-date form is not honoured (delta-seconds only) → None, leaving
        // exponential back-off to govern the wait.
        let resp = parse_curl_output(
            b"\n503\nWed, 21 Oct 2015 07:28:00 GMT\n\n\n\n",
            UsageMethod::UsageApi,
        );
        assert_eq!(resp.status, 503);
        assert_eq!(resp.retry_after, None);
    }

    #[test]
    fn parse_curl_output_tolerates_a_missing_or_unparseable_status() {
        // No newline at all + non-numeric → status 0 (→ Transient).
        assert_eq!(
            parse_curl_output(b"garbage", UsageMethod::UsageApi).status,
            0
        );
        // A lone newline → empty status + empty retry-after → status 0.
        assert_eq!(parse_curl_output(b"\n", UsageMethod::UsageApi).status, 0);
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
                method: UsageMethod::UsageApi,
                status,
                body: body.to_owned(),
                retry_after: None,
                quota_headers: QuotaHeaders::default(),
            })
        }

        /// A bodyless response carrying a `Retry-After`, for the throttled paths.
        fn ok_retry_after(status: u16, retry_after: Option<Duration>) -> Result<HttpResponse> {
            Ok(HttpResponse {
                method: UsageMethod::UsageApi,
                status,
                body: String::new(),
                retry_after,
                quota_headers: QuotaHeaders::default(),
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

    fn probe_response(status: u16) -> Result<HttpResponse> {
        Ok(HttpResponse {
            method: UsageMethod::InferenceProbe,
            status,
            body: "ignored model output or error body".into(),
            retry_after: Some(Duration::from_secs(37)),
            quota_headers: QuotaHeaders {
                session_utilization: "0.42".into(),
                weekly_utilization: "0.17".into(),
                session_reset: "1900000000".into(),
                weekly_reset: "1900500000".into(),
            },
        })
    }

    #[tokio::test]
    async fn a_probe_success_returns_the_existing_reading_without_invented_severity() {
        let reading = source(vec![probe_response(200)]).usage().await.unwrap();
        assert_eq!(reading.usage.session, 0.42);
        assert_eq!(reading.usage.weekly, 0.17);
        assert_eq!(reading.usage.session_resets_at, Some(1_900_000_000));
        assert_eq!(reading.usage.weekly_resets_at, Some(1_900_500_000));
        assert_eq!(reading.severity, None);
    }

    #[tokio::test]
    async fn usage_api_failures_never_fall_back_to_inference() {
        for status in [0, 400, 401, 403, 429, 500, 503] {
            let src = source(vec![
                FakeUsageTransport::ok(status, "{}"),
                probe_response(200),
            ]);
            assert!(src.usage().await.is_err());
            assert_eq!(src.transport.responses.borrow().len(), 1);
        }
    }

    #[tokio::test]
    async fn probe_failures_do_not_consume_headers_as_success_or_make_a_fallback_request() {
        for status in [0, 401, 403, 429, 500, 503] {
            let src = source(vec![probe_response(status), probe_response(200)]);
            let error = src.usage().await.unwrap_err();
            match status {
                401 => assert!(matches!(error, Error::UsageUnauthorized)),
                403 => assert!(matches!(error, Error::UsageRejected { status: 403 })),
                429 => assert!(
                    matches!(error, Error::UsageRateLimited { status: 429, retry_after: Some(delay) } if delay == Duration::from_secs(37))
                ),
                _ => assert!(
                    matches!(error, Error::UsageTransient { status: observed, retry_after: Some(delay) } if observed == status && delay == Duration::from_secs(37))
                ),
            }
            assert_eq!(src.transport.responses.borrow().len(), 1);
        }
    }

    #[tokio::test]
    async fn probe_success_without_headers_is_not_zero_usage() {
        let mut response = probe_response(200).unwrap();
        response.quota_headers = QuotaHeaders::default();
        assert!(matches!(
            source(vec![Ok(response)]).usage().await,
            Err(Error::UsageParse(_))
        ));
    }

    #[tokio::test]
    async fn a_success_yields_the_reading() {
        let body =
            r#"{"limits":[{"kind":"session","percent":30},{"kind":"weekly_all","percent":70}]}"#;
        let src = source(vec![FakeUsageTransport::ok(200, body)]);
        let reading = src.usage().await.unwrap();
        assert!((reading.usage.session - 0.30).abs() < 1e-9);
        assert!((reading.usage.weekly - 0.70).abs() < 1e-9);
        // No `severity` on either limit → the reading carries none (issue #156).
        assert_eq!(reading.severity, None);
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
    // reporting, update pings, or beacons. Its usage transport makes the normal
    // usage GET or the scope-selected minimal inference probe at fixed URLs.
    // These are *capability* guards — deliberately stronger than a
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
    // that one subprocess — targeting `USAGE_URL` or `INFERENCE_URL`. (The daemon's
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
    fn egress_endpoints_are_fixed_and_each_method_configures_exactly_one_request() {
        assert_eq!(USAGE_URL, "https://api.anthropic.com/api/oauth/usage");
        assert_eq!(INFERENCE_URL, "https://api.anthropic.com/v1/messages");
        assert_eq!(CURL, "/usr/bin/curl");
        for (method, url) in [
            (UsageMethod::UsageApi, USAGE_URL),
            (UsageMethod::InferenceProbe, INFERENCE_URL),
        ] {
            let config = curl_config("sk-ant-oat-TESTTOKEN", method).unwrap();
            let url_lines: Vec<&str> = config
                .lines()
                .filter(|line| line.trim_start().starts_with("url = "))
                .collect();
            assert_eq!(url_lines.len(), 1, "exactly one request URL is configured");
            assert_eq!(url_lines[0], format!("url = \"{url}\"").as_str());
        }
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
             methods — no in-process HTTP/TLS client, no telemetry SDK (issue #271)."
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
