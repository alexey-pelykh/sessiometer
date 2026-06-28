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
//! and projects the response into a [`Usage`] reading (both windows) for the swap
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
//!     **distinctly** from 401 — the hallmark of a non-interactive setup token.
//!
//! ## 401 monitor → re-stash trigger seam (issues #13 / #6)
//!
//! [`Monitor401`] counts *consecutive* 401s and resets on any non-401 outcome.
//! On the `monitor_401_n`-th consecutive 401 it fires the [`ReStashTrigger`]
//! seam — a signal only. The actual re-stash (canonical re-read) and the back-off
//! loop are out of scope here (#13 / #6); production wires a [`NoopReStashTrigger`].
//! The poller never self-refreshes a token.

use std::cell::Cell;
use std::process::Stdio;

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
/// poll loop. Comfortably below the default 60s poll interval.
const POLL_TIMEOUT_SECS: u32 = 30;

/// A point-in-time usage reading for one account, across both quota windows.
///
/// The swap decision compares each dimension against its OWN threshold (issue
/// #41: session vs `session_trigger`, weekly vs the separate `weekly_trigger`),
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
    /// dimension is projected here — the weekly window is the hard limit whose
    /// reset actually ends the all-exhausted state.
    pub(crate) weekly_resets_at: Option<i64>,
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
    /// or epoch-as-string); tolerant — `None` if absent/unrecognized. Extracted
    /// for completeness; no consumer needs the session reset yet (the terminal
    /// logic keys off the weekly window), so it stays unread.
    #[allow(dead_code)]
    session_resets_at: Option<String>,
    /// Raw `resets_at` of the weekly window (see `session_resets_at`). Projected
    /// to epoch seconds by [`to_usage`](UsageReport::to_usage) for the
    /// all-exhausted terminal logic (#11).
    weekly_resets_at: Option<String>,
}

impl UsageReport {
    /// The swap-decision projection: the two usage dimensions the loop acts on,
    /// plus the weekly reset normalized to epoch seconds (issue #11) so the
    /// all-exhausted logic can compare reset times across accounts. A weekly
    /// reset the API did not supply — or that does not parse — projects to `None`.
    fn to_usage(&self) -> Usage {
        Usage {
            session: self.session,
            weekly: self.weekly,
            weekly_resets_at: self
                .weekly_resets_at
                .as_deref()
                .and_then(epoch_from_resets_at),
        }
    }
}

/// Seam: reads one account's usage quota. The real impl ([`RealUsageSource`])
/// polls the usage API; the test impl ([`FakeUsageSource`]) returns scripted
/// readings.
pub(crate) trait UsageSource {
    async fn usage(&self) -> Result<Usage>;
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
}

/// Seam: signals that an account's stored token has been rejected
/// `monitor_401_n` times in a row and should be re-stashed (a canonical re-read;
/// issue #13 / #6). A signal only — the implementor performs (or schedules) the
/// re-stash. The poller never self-refreshes.
pub(crate) trait ReStashTrigger {
    fn request_restash(&self);
}

/// Production trigger: a no-op. Counting and firing happen here in #5; acting on
/// the signal (the canonical re-read) lands in #13 / #6, which replaces this.
pub(crate) struct NoopReStashTrigger;

impl ReStashTrigger for NoopReStashTrigger {
    fn request_restash(&self) {}
}

/// Consecutive-401 counter behind the re-stash trigger seam.
///
/// Increments on each 401 and resets on **any** non-401 outcome (success, 403,
/// 429, transient, even an I/O failure) — per issue #5's "counter resets on a
/// non-401 outcome". The trigger fires exactly once per streak, at the crossing
/// (`count == threshold`): the count keeps climbing past the threshold so the
/// equality is not re-satisfied until a non-401 resets it.
struct Monitor401 {
    /// `monitor_401_n` (config-validated `1..=20`), so the first 401 can never
    /// trip a zero threshold.
    threshold: u8,
    count: Cell<u8>,
}

impl Monitor401 {
    fn new(threshold: u8) -> Self {
        Self {
            threshold,
            count: Cell::new(0),
        }
    }

    /// Record one poll outcome and fire the trigger on the Nth consecutive 401.
    fn observe(&self, unauthorized: bool, trigger: &impl ReStashTrigger) {
        if unauthorized {
            let count = self.count.get().saturating_add(1);
            self.count.set(count);
            if count == self.threshold {
                trigger.request_restash();
            }
        } else {
            self.count.set(0);
        }
    }

    #[cfg(test)]
    fn count(&self) -> u8 {
        self.count.get()
    }
}

/// Real usage poller: classify the HTTP outcome, parse a success, and drive the
/// 401 monitor. Generic over the transport and the re-stash trigger so the whole
/// `usage()` flow is testable against fakes (matching the daemon's seam design).
pub(crate) struct RealUsageSource<Tp, Tr> {
    transport: Tp,
    trigger: Tr,
    monitor: Monitor401,
}

impl<Tp, Tr> RealUsageSource<Tp, Tr> {
    /// `monitor_401_n` is the consecutive-401 threshold (config tunable).
    pub(crate) fn new(transport: Tp, trigger: Tr, monitor_401_n: u8) -> Self {
        Self {
            transport,
            trigger,
            monitor: Monitor401::new(monitor_401_n),
        }
    }

    #[cfg(test)]
    fn trigger(&self) -> &Tr {
        &self.trigger
    }

    #[cfg(test)]
    fn monitor_count(&self) -> u8 {
        self.monitor.count()
    }
}

impl<Tp, Tr> UsageSource for RealUsageSource<Tp, Tr>
where
    Tp: UsageTransport,
    Tr: ReStashTrigger,
{
    async fn usage(&self) -> Result<Usage> {
        // A failed transport (I/O, or an unreadable token) is itself a non-401
        // outcome, so it resets the consecutive-401 counter before propagating.
        let response = match self.transport.fetch().await {
            Ok(response) => response,
            Err(err) => {
                self.monitor.observe(false, &self.trigger);
                return Err(err);
            }
        };

        let status = response.status;
        match classify_status(status) {
            // Only a 401 advances the monitor; every other arm resets it.
            UsageStatus::Unauthorized => {
                self.monitor.observe(true, &self.trigger);
                Err(Error::UsageUnauthorized)
            }
            UsageStatus::Success => {
                self.monitor.observe(false, &self.trigger);
                Ok(parse_usage(&response.body)?.to_usage())
            }
            UsageStatus::ScopeMissing => {
                self.monitor.observe(false, &self.trigger);
                Err(Error::UsageScopeMissing)
            }
            UsageStatus::RateLimited => {
                self.monitor.observe(false, &self.trigger);
                Err(Error::UsageRateLimited { status })
            }
            UsageStatus::ClientError => {
                self.monitor.observe(false, &self.trigger);
                Err(Error::UsageRejected { status })
            }
            UsageStatus::Transient => {
                self.monitor.observe(false, &self.trigger);
                Err(Error::UsageTransient { status })
            }
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
/// `write-out` appends `\n<status>` after the body so the caller can recover the
/// HTTP status. The token is an opaque `sk-ant-…` string (no `"`/newline), so it
/// needs no escaping inside the quoted `header` value. Zeroized on drop.
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
    // Body, then a newline and the numeric status, on stdout.
    config.push_str("write-out = \"\\n%{http_code}\"\n");
    Zeroizing::new(config)
}

/// Split `curl`'s stdout (`<body>\n<status>`, per the `write-out` above) into a
/// [`HttpResponse`]. Tolerant: an unparseable trailing code becomes `0` (→
/// Transient), and a no-body output is just the code.
fn parse_curl_output(stdout: &[u8]) -> HttpResponse {
    let text = String::from_utf8_lossy(stdout);
    match text.rsplit_once('\n') {
        Some((body, code)) => HttpResponse {
            status: code.trim().parse().unwrap_or(0),
            body: body.to_owned(),
        },
        None => HttpResponse {
            status: text.trim().parse().unwrap_or(0),
            body: String::new(),
        },
    }
}

/// Parse a `200` usage body into both dimensions plus reset timestamps.
///
/// Tolerant of the two documented response shapes for each window:
///   - a top-level object — `five_hour` (session) / `seven_day` (weekly) — with a
///     `utilization` fraction, or
///   - a `limits[]` entry whose `kind` matches the window, with a `percent`.
///
/// A window that cannot be found is a hard [`Error::UsageParse`] (never a
/// fabricated `0`): the loop must skip, not swap, on missing data.
fn parse_usage(body: &str) -> Result<UsageReport> {
    let root: Value =
        serde_json::from_str(body).map_err(|_| Error::UsageParse("body is not JSON".into()))?;

    let (session, session_resets_at) = dimension(&root, "five_hour", &["session", "five_hour"])
        .ok_or_else(|| Error::UsageParse("no session (five_hour) dimension".into()))?;
    let (weekly, weekly_resets_at) =
        dimension(&root, "seven_day", &["weekly_all", "seven_day", "weekly"])
            .ok_or_else(|| Error::UsageParse("no weekly (seven_day) dimension".into()))?;

    Ok(UsageReport {
        session,
        weekly,
        session_resets_at,
        weekly_resets_at,
    })
}

/// Find one window's `(fraction, resets_at)`, trying the top-level
/// `{top_key: {...}}` object first, then a `limits[]` entry whose `kind` is one
/// of `kinds` (skipping any entry explicitly `is_active: false`).
fn dimension(root: &Value, top_key: &str, kinds: &[&str]) -> Option<(f64, Option<String>)> {
    if let Some(obj) = root.get(top_key) {
        if let Some(fraction) = fraction_of(obj) {
            return Some((fraction, resets_at_of(obj)));
        }
    }
    let limits = root.get("limits").and_then(Value::as_array)?;
    for entry in limits {
        let active = entry
            .get("is_active")
            .and_then(Value::as_bool)
            .unwrap_or(true);
        let kind = entry.get("kind").and_then(Value::as_str);
        if active && kind.is_some_and(|kind| kinds.contains(&kind)) {
            if let Some(fraction) = fraction_of(entry) {
                return Some((fraction, resets_at_of(entry)));
            }
        }
    }
    None
}

/// Read a usage fraction in `[0.0, 1.0]` from an object: a `utilization`
/// fraction as-is, else a `percent` (`0..=100`) divided by 100. Clamped, so a
/// stray `> 100` (or `> 1.0`) reading can never exceed a full window.
fn fraction_of(obj: &Value) -> Option<f64> {
    if let Some(utilization) = obj.get("utilization").and_then(Value::as_f64) {
        return Some(utilization.clamp(0.0, 1.0));
    }
    if let Some(percent) = obj.get("percent").and_then(Value::as_f64) {
        return Some((percent / 100.0).clamp(0.0, 1.0));
    }
    None
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
fn epoch_from_rfc3339(s: &str) -> Option<i64> {
    let (date, rest) = s.split_once('T').or_else(|| s.split_once(' '))?;

    let mut date_parts = date.split('-');
    let year: i64 = date_parts.next()?.parse().ok()?;
    let month: u32 = date_parts.next()?.parse().ok()?;
    let day: u32 = date_parts.next()?.parse().ok()?;
    if date_parts.next().is_some() || !(1..=12).contains(&month) || !(1..=31).contains(&day) {
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
        None if tz.len() == 4 => (tz[..2].parse().ok()?, tz[2..].parse().ok()?),
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
    fn parses_the_top_level_window_shape_with_utilization() {
        let body = r#"{
            "five_hour": {"utilization": 0.10, "resets_at": "2026-06-26T18:00:00Z"},
            "seven_day": {"utilization": 0.55, "resets_at": "2026-06-30T00:00:00Z"}
        }"#;
        let report = parse_usage(body).unwrap();
        assert!((report.session - 0.10).abs() < 1e-9);
        assert!((report.weekly - 0.55).abs() < 1e-9);
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
    fn skips_inactive_limit_entries() {
        // The inactive session entry must be ignored in favor of the active one.
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
    fn percent_is_normalized_and_clamped() {
        // percent 150 clamps to a full window; utilization passes through clamped.
        let body = r#"{
            "limits": [
                {"kind": "session", "percent": 150},
                {"kind": "weekly_all", "utilization": 1.5}
            ]
        }"#;
        let report = parse_usage(body).unwrap();
        assert_eq!(report.session, 1.0);
        assert_eq!(report.weekly, 1.0);
    }

    #[test]
    fn resets_at_tolerates_a_numeric_timestamp() {
        let body = r#"{
            "five_hour": {"utilization": 0.1, "resets_at": 1750960800},
            "seven_day": {"utilization": 0.2}
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
    fn to_usage_normalizes_the_weekly_reset_to_epoch() {
        let report = UsageReport {
            session: 0.1,
            weekly: 0.2,
            session_resets_at: Some("2025-01-01T00:00:00Z".to_owned()),
            weekly_resets_at: Some("2025-06-26T18:00:00Z".to_owned()),
        };
        assert_eq!(report.to_usage().weekly_resets_at, Some(1_750_960_800));
        // An absent weekly reset projects to None (no fabricated value).
        let no_reset = UsageReport {
            weekly_resets_at: None,
            ..report.clone()
        };
        assert_eq!(no_reset.to_usage().weekly_resets_at, None);
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
        assert!(config.contains("write-out = \"\\n%{http_code}\""));
        assert!(config.contains(&format!("max-time = {POLL_TIMEOUT_SECS}")));
    }

    // --- parse_curl_output (pure) ---

    #[test]
    fn parse_curl_output_splits_body_and_trailing_status() {
        let resp = parse_curl_output(b"{\"limits\":[]}\n200");
        assert_eq!(resp.status, 200);
        assert_eq!(resp.body, "{\"limits\":[]}");
    }

    #[test]
    fn parse_curl_output_handles_a_multiline_body() {
        let resp = parse_curl_output(b"{\n  \"a\": 1\n}\n403");
        assert_eq!(resp.status, 403);
        assert_eq!(resp.body, "{\n  \"a\": 1\n}");
    }

    #[test]
    fn parse_curl_output_tolerates_a_missing_or_unparseable_status() {
        // No trailing newline + non-numeric → status 0 (→ Transient).
        assert_eq!(parse_curl_output(b"garbage").status, 0);
        assert_eq!(parse_curl_output(b"\n").status, 0);
    }

    // --- RealUsageSource end-to-end (fake transport + recording trigger) ---

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

    /// Records how many times the re-stash trigger fired.
    struct RecordingTrigger {
        fires: Cell<u32>,
    }

    impl RecordingTrigger {
        fn new() -> Self {
            Self {
                fires: Cell::new(0),
            }
        }
        fn fires(&self) -> u32 {
            self.fires.get()
        }
    }

    impl ReStashTrigger for RecordingTrigger {
        fn request_restash(&self) {
            self.fires.set(self.fires.get() + 1);
        }
    }

    fn source(
        responses: Vec<Result<HttpResponse>>,
        monitor_401_n: u8,
    ) -> RealUsageSource<FakeUsageTransport, RecordingTrigger> {
        RealUsageSource::new(
            FakeUsageTransport::new(responses),
            RecordingTrigger::new(),
            monitor_401_n,
        )
    }

    #[tokio::test]
    async fn a_success_yields_the_reading_and_arms_nothing() {
        let body =
            r#"{"limits":[{"kind":"session","percent":30},{"kind":"weekly_all","percent":70}]}"#;
        let src = source(vec![FakeUsageTransport::ok(200, body)], 3);
        let usage = src.usage().await.unwrap();
        assert!((usage.session - 0.30).abs() < 1e-9);
        assert!((usage.weekly - 0.70).abs() < 1e-9);
        assert_eq!(src.monitor_count(), 0);
        assert_eq!(src.trigger().fires(), 0);
    }

    #[tokio::test]
    async fn a_200_with_an_unparseable_body_is_a_parse_error() {
        let src = source(vec![FakeUsageTransport::ok(200, "{}")], 3);
        assert!(matches!(src.usage().await, Err(Error::UsageParse(_))));
        // A 200 is still a non-401 outcome: the counter is reset.
        assert_eq!(src.monitor_count(), 0);
    }

    #[tokio::test]
    async fn forbidden_surfaces_distinctly_and_does_not_arm_the_monitor() {
        let src = source(vec![FakeUsageTransport::ok(403, "")], 3);
        assert!(matches!(src.usage().await, Err(Error::UsageScopeMissing)));
        assert_eq!(src.monitor_count(), 0);
        assert_eq!(src.trigger().fires(), 0);
    }

    #[tokio::test]
    async fn rate_limited_and_server_errors_are_typed_and_reset_the_counter() {
        let src = source(
            vec![
                FakeUsageTransport::ok(429, ""),
                FakeUsageTransport::ok(503, ""),
                FakeUsageTransport::ok(400, ""),
            ],
            3,
        );
        assert!(matches!(
            src.usage().await,
            Err(Error::UsageRateLimited { status: 429 })
        ));
        assert!(matches!(
            src.usage().await,
            Err(Error::UsageTransient { status: 503 })
        ));
        assert!(matches!(
            src.usage().await,
            Err(Error::UsageRejected { status: 400 })
        ));
        assert_eq!(src.trigger().fires(), 0);
    }

    #[tokio::test]
    async fn a_transport_failure_propagates_and_resets_the_counter() {
        let src = source(
            vec![
                FakeUsageTransport::ok(401, ""),
                Err(Error::Io(std::io::Error::other("boom"))),
            ],
            3,
        );
        assert!(matches!(src.usage().await, Err(Error::UsageUnauthorized)));
        assert_eq!(src.monitor_count(), 1);
        // An I/O failure is a non-401 outcome → counter resets.
        assert!(matches!(src.usage().await, Err(Error::Io(_))));
        assert_eq!(src.monitor_count(), 0);
    }

    #[tokio::test]
    async fn consecutive_401s_fire_the_trigger_exactly_at_the_threshold() {
        let src = source(
            vec![
                FakeUsageTransport::ok(401, ""),
                FakeUsageTransport::ok(401, ""),
                FakeUsageTransport::ok(401, ""),
                FakeUsageTransport::ok(401, ""),
            ],
            3,
        );
        // Below threshold: no fire.
        assert!(matches!(src.usage().await, Err(Error::UsageUnauthorized)));
        assert!(matches!(src.usage().await, Err(Error::UsageUnauthorized)));
        assert_eq!(src.trigger().fires(), 0);
        // The 3rd consecutive 401 fires the trigger exactly once.
        assert!(matches!(src.usage().await, Err(Error::UsageUnauthorized)));
        assert_eq!(src.trigger().fires(), 1);
        // A 4th 401 does NOT re-fire (one signal per streak).
        assert!(matches!(src.usage().await, Err(Error::UsageUnauthorized)));
        assert_eq!(src.trigger().fires(), 1);
    }

    #[tokio::test]
    async fn a_non_401_outcome_resets_the_consecutive_streak() {
        let body =
            r#"{"limits":[{"kind":"session","percent":1},{"kind":"weekly_all","percent":1}]}"#;
        let src = source(
            vec![
                FakeUsageTransport::ok(401, ""),
                FakeUsageTransport::ok(401, ""),
                FakeUsageTransport::ok(200, body), // resets the streak
                FakeUsageTransport::ok(401, ""),
                FakeUsageTransport::ok(401, ""),
            ],
            3,
        );
        for _ in 0..2 {
            let _ = src.usage().await;
        }
        assert_eq!(src.monitor_count(), 2);
        src.usage().await.unwrap(); // the 200 resets
        assert_eq!(src.monitor_count(), 0);
        for _ in 0..2 {
            let _ = src.usage().await;
        }
        // Two fresh 401s after the reset — still below threshold, never fired.
        assert_eq!(src.monitor_count(), 2);
        assert_eq!(src.trigger().fires(), 0);
    }

    #[tokio::test]
    async fn a_threshold_of_one_fires_on_the_first_401() {
        let src = source(vec![FakeUsageTransport::ok(401, "")], 1);
        assert!(matches!(src.usage().await, Err(Error::UsageUnauthorized)));
        assert_eq!(src.trigger().fires(), 1);
    }
}
