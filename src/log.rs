// Copyright (c) 2026 Oleksii PELYKH
// SPDX-License-Identifier: MIT

//! The `log` verb (issue #773): a supported reader for the daemon's durable event log.
//!
//! The log has always been durable and structured, but the only way to *look at* it was to know
//! `~/Library/Logs/sessiometer/sessiometer.log` and type `tail`. [`crate::reliability`] reads
//! this exact file already — but only to fold it into SLIs, never to show the lines. This verb
//! shows them, with a window ([`LogArgs::since`]) and an event filter ([`LogArgs::event`]).
//!
//! It is the third **offline** reader, in the shape the two shipped ones establish (`stats`,
//! issue #158; `reliability`, issue #455): read the daemon's durable file directly, make no live
//! control-socket / keychain / usage-API call, and render with the daemon down. The only impure
//! steps are the one file read and — for `--since` alone — one wall-clock read; everything below
//! them is a pure function of the text, so the whole reader is unit-testable from a `&str`.
//!
//! # Byte-faithfulness, and why the two streams are split
//!
//! The reader is **byte-faithful**: it interpolates no account data of its own. The event log is
//! mode `0600` and carries operator labels VERBATIM — in practice emails (`event=swap
//! from=you@example.com …`, the provenance-scoped waiver of issue #444). Rendering it to stdout
//! widens an exposure the `0600` mode was containing, so the guarantee has to be *checkable*
//! rather than merely intended.
//!
//! It is made checkable by splitting the two streams:
//!
//! - **stdout is the data stream.** In text mode it is the selected durable lines and *nothing
//!   else* — so every stdout byte already existed in the log, and the guarantee is a containment
//!   assertion a test can make (`stdout_bytes_all_exist_in_the_durable_log`, which also canaries
//!   its own predicate). In `--json` mode it is one JSON document whose every `line` value is a
//!   durable line held verbatim.
//! - **stderr is the operator notice**: the resolved window, the active filter, the match count,
//!   and the explicit empty-result / absent-log statements. These are reader-authored metadata,
//!   not log content, so keeping them off stdout is what lets the containment assertion above be
//!   exact — and it keeps a piped `sessiometer log` a clean line stream, so `| grep` and `| wc -l`
//!   stay honest. [`emit`] is the seam that pins WHICH string reaches which stream; `| head` —
//!   closing the pipe early — exits `0` rather than panicking (see [`write_stream`]).
//!
//! The split has one honest cost: in text mode the window is stated on stderr, so
//! `sessiometer log --since 1h > audit.txt` keeps the lines but not the record of which window
//! produced them. `--json` carries its `window` object on stdout with the data, so provenance
//! travels there.
//!
//! # What this reader must not do
//!
//! The event log's grammar is **frozen**: [`crate::reliability`] parses this same file, so a
//! durable line may not be added, reordered, or reformatted. This reader therefore never
//! re-serializes a line from its parsed fields — text mode emits the borrowed `&str` itself. It
//! reads `ts=` / `event=` only to *filter*, through the same whitespace/`key=val` tokenization
//! the sibling reader folds through, so the two cannot disagree about what a line says.
//!
//! Diagnostics stay out of it. The `-v` OPERATOR channel is stderr-only and never reaches the
//! durable log (see [`crate::reliability`]'s note); that invariant is untouched here, and this
//! verb reads the durable channel alone. Reading `daemon.err.log` — raw stderr and panic payloads,
//! an ungoverned channel that never passed the issue #15 redaction meter — is out of scope; the
//! `--channel event|diag|all` selector that would reach it is issue #775's. Nothing here forecloses
//! it: [`select`] filters a channel-agnostic line stream and takes its text as an argument, so a
//! later channel adds a *source*, not a rewrite. `--follow` is issue #774's.

use crate::error::{Error, Result};
use crate::usage::epoch_from_rfc3339;
use serde::Serialize;
use std::io::Write;
use std::path::Path;

/// The stable `--json` schema version. Owned by this reader, independent of
/// [`crate::stats`]' and [`crate::reliability`]'s own `JSON_SCHEMA_VERSION` — the three readers
/// version their wires separately, so one can change without forcing a bump on the others.
const JSON_SCHEMA_VERSION: u32 = 1;

/// Parsed `log` options (issue #773). A plain comparable value so the CLI parser is
/// unit-testable by value, like `StatsArgs` and `ReliabilityArgs`.
#[derive(Debug, PartialEq)]
pub(crate) struct LogArgs {
    /// `--since <duration>` — emit only lines whose `ts=` is at/after `now − duration`. The RAW
    /// value as given (e.g. `"7d"`); parsed and validated in [`run`], where the wall clock is
    /// read. `None` = the whole log (the default).
    pub(crate) since: Option<String>,
    /// `--event <name>` — emit only lines whose `event=` token is EXACTLY this. `None` = every
    /// event.
    pub(crate) event: Option<String>,
    /// `--json` — print machine-readable records carrying an explicit schema version instead of
    /// the text view.
    pub(crate) json: bool,
}

/// Entry point for the `log` verb: read the event log once, select, and render.
///
/// An absent log file is a normal cold state (a fresh install), not an error: the verb says so
/// and exits `0`. So does a log with no matching line — the notice distinguishes *no file*, *an
/// empty file*, and *no match*, so a silent exit never has to be guessed at.
pub(crate) fn run(args: LogArgs) -> Result<()> {
    let text = read_event_log()?;
    // Resolve the optional window against the wall clock BEFORE selecting, so the cutoff is a
    // plain integer the pure path filters by. A malformed `--since` fails here, before any
    // output, as `Error::LogSinceInvalid` — never a silent whole-log fallback.
    let window = match args.since.as_deref() {
        Some(raw) => Some(Window::resolve(raw, now_epoch())?),
        None => None,
    };
    let view = select(text.as_deref(), window, args.event.as_deref());
    let rendered = if args.json {
        render_json(&view)?
    } else {
        render_text(&view)
    };
    // stdout is the data stream, stderr the operator notice — see the module docs.
    emit(
        &rendered,
        &mut std::io::stdout().lock(),
        &mut std::io::stderr().lock(),
    )
}

/// Write a [`Rendered`] to its two destinations.
///
/// Split out of [`run`] so the stream ROUTING — the data/notice separation this reader's whole
/// contract rests on — is assertable against two in-memory sinks rather than the process's real
/// descriptors. Swapping the two arguments must fail a test; that is the point of the seam.
fn emit(rendered: &Rendered, data: &mut impl Write, notice: &mut impl Write) -> Result<()> {
    write_stream(data, &rendered.out)?;
    write_stream(notice, &rendered.notice)
}

/// Write `text` to `sink`, treating a closed downstream as success.
///
/// `sessiometer log | head -3` closes the pipe after three lines — the ordinary use of a
/// line-stream reader, and the one this verb actively advertises. The crate's other readers use
/// `print!`, which PANICS on `EPIPE`; over a 2 MB log that panic is easy to hit, so this reader
/// treats it as the normal end of a pipe and exits `0`. Every other IO error still propagates.
fn write_stream(sink: &mut impl Write, text: &str) -> Result<()> {
    if text.is_empty() {
        return Ok(());
    }
    match sink.write_all(text.as_bytes()).and_then(|()| sink.flush()) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == std::io::ErrorKind::BrokenPipe => Ok(()),
        Err(err) => Err(Error::Io(err)),
    }
}

/// The event-log text, or `None` when there is no log file at all.
///
/// Unlike [`crate::reliability`]'s read — which folds an absent file into an empty aggregate,
/// because *no events* and *no file* produce the same SLIs — this reader keeps the two apart:
/// "the daemon has never run" and "the daemon ran and recorded nothing" are different answers to
/// the operator's question, and issue #773 asks for the first to be said plainly.
fn read_event_log() -> Result<Option<String>> {
    read_event_log_at(&crate::observability::log_path()?)
}

/// [`read_event_log`] against an explicit path — the seam that makes the absent-file arm
/// testable. The production path is not injectable (it resolves through `getpwuid`, deliberately,
/// so it cannot be spoofed by an environment variable), so without this split the `NotFound`
/// branch could only be reached by interposing on `open`.
fn read_event_log_at(path: &Path) -> Result<Option<String>> {
    match std::fs::read_to_string(path) {
        Ok(text) => Ok(Some(text)),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(Error::Io(err)),
    }
}

/// Current wall clock as epoch seconds (`0` on the pre-1970 impossible case) — the crate's
/// display-path clock read (mirrors [`crate::reliability`]'s). Only reached when `--since` is
/// given; the default whole-log path reads no clock at all.
fn now_epoch() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// The active `--since` window. Carries the raw span (echoed back exactly as the operator typed
/// it) plus the absolute cutoff, so the notice can document the window [`select`] filtered by.
///
/// [`crate::reliability`]'s `Window` is structurally identical to this one — the same two fields
/// and the same two methods. The issue #773 lift was scoped to the span *grammar*
/// ([`crate::duration`]) because that is the half a copy could let DRIFT; the half left behind is
/// which error a rejection maps to, which is per-verb by design — that module returning `Option`
/// rather than `Result` IS this split. Folding the type itself would save ~20 lines at the cost of
/// a shared surface neither verb has yet asked to evolve together.
#[derive(Debug, PartialEq)]
struct Window {
    /// The raw `--since` value, trimmed, echoed verbatim in the notice (e.g. `"7d"`).
    since_arg: String,
    /// Lines whose `ts=` is `<` this epoch-second cutoff are dropped; at/after are kept.
    /// Clamped to `>= 0`, so a span wider than the log's age simply means "the whole log".
    cutoff_epoch: i64,
}

impl Window {
    /// Resolve a raw `--since` value against `now` (epoch seconds).
    ///
    /// The span grammar is [`crate::duration::parse_duration_secs`] — shared with `reliability`
    /// so the two offline readers cannot drift — and its rejection is mapped HERE, to
    /// [`Error::LogSinceInvalid`], so the message names the flag this operator mistyped.
    /// Saturating throughout: an absurd span can never overflow into a future cutoff, and a span
    /// reaching past the epoch clamps to `0`.
    fn resolve(raw: &str, now: i64) -> Result<Window> {
        let secs = crate::duration::parse_duration_secs(raw)
            .ok_or_else(|| Error::LogSinceInvalid(raw.trim().to_owned()))?;
        let cutoff_epoch = now.saturating_sub_unsigned(secs).max(0);
        Ok(Window {
            since_arg: raw.trim().to_owned(),
            cutoff_epoch,
        })
    }

    /// The cutoff rendered back to the event log's own RFC 3339 UTC shape, for the notice —
    /// through the SAME [`crate::observability::rfc3339`] the log writes `ts=` with, so a
    /// documented window reads in the identical format as the lines it bounds.
    fn cutoff_rfc3339(&self) -> String {
        use std::time::{Duration, UNIX_EPOCH};
        // cutoff_epoch is clamped `>= 0`, so the `as u64` cast is lossless (no wraparound).
        crate::observability::rfc3339(UNIX_EPOCH + Duration::from_secs(self.cutoff_epoch as u64))
    }
}

/// One selected durable line, held VERBATIM.
///
/// `line` is the borrowed source text itself, never a re-serialization — it is the only thing
/// text mode writes to stdout. `ts` / `event` are borrowed SUBSLICES of that same line, read to
/// filter and echoed in the JSON view as the tokens they already are.
#[derive(Debug, PartialEq)]
struct Selected<'a> {
    /// The durable line exactly as it appears in the file, without its terminator.
    line: &'a str,
    /// The `ts=` value, or `None` when the line carries none.
    ts: Option<&'a str>,
    /// The `event=` value, or `None` when the line carries none.
    event: Option<&'a str>,
}

/// Everything the reader was asked for and everything it found — the single value both renderers
/// consume, so the text and JSON views can never disagree about what matched.
#[derive(Debug, PartialEq)]
struct LogView<'a> {
    /// The resolved `--since` window; `None` when the flag was absent.
    window: Option<Window>,
    /// The `--event` token; `None` when the flag was absent.
    event: Option<&'a str>,
    /// `false` when there is no log file at all — distinct from a file with zero lines.
    log_present: bool,
    /// Every line that passed both filters, in file order.
    matched: Vec<Selected<'a>>,
    /// Every line the log held, filters aside — the denominator of the match count.
    n_scanned: usize,
}

/// The value of `key=` in `line`, or `None` when the key is absent.
///
/// Tokenizes on whitespace and splits each token at its FIRST `=`, exactly as
/// [`crate::reliability`]'s fold does — handles and values are whitespace-free by the log's
/// frozen grammar, so this is exact rather than approximate. A repeated key resolves to its LAST
/// occurrence, matching the `BTreeMap::insert` the sibling reader folds through, so the two
/// readers cannot disagree about what a line says.
fn field<'a>(line: &'a str, key: &str) -> Option<&'a str> {
    // `next_back`, not `last`: the same last-occurrence semantics without `Iterator::last`'s
    // forward fold over the whole token stream.
    line.split_whitespace()
        .filter_map(|token| token.split_once('='))
        .filter(|(k, _)| *k == key)
        .map(|(_, value)| value)
        .next_back()
}

/// Select the lines matching both filters, in file order. Pure — the whole reader below the file
/// and clock reads, so every behaviour here is testable from a `&str`.
///
/// A line whose `ts=` is missing or unparseable is dropped from a WINDOWED view: it cannot be
/// placed in time, so it is not provably in-window. That is the tolerant-drop precedent
/// [`crate::reliability`]'s fold already sets. Without `--since` no timestamp is consulted at
/// all, so such a line is emitted like any other.
fn select<'a>(
    text: Option<&'a str>,
    window: Option<Window>,
    event: Option<&'a str>,
) -> LogView<'a> {
    // `None` IS the absent-file state, so the two cannot disagree — there is no way to ask for
    // "present, but here is no text" or "absent, but here is some".
    let log_present = text.is_some();
    let cutoff = window.as_ref().map(|w| w.cutoff_epoch);
    let mut matched = Vec::new();
    let mut n_scanned = 0usize;
    for line in text.unwrap_or("").lines() {
        n_scanned += 1;
        let ts = field(line, "ts");
        let line_event = field(line, "event");
        if let Some(cutoff) = cutoff {
            // A line with no parseable `ts=` cannot be placed in time, so it is not provably
            // in-window and drops alongside the genuinely older lines — the tolerant-drop
            // precedent, stated as the drop condition it is.
            let out_of_window = ts.and_then(epoch_from_rfc3339).is_none_or(|at| at < cutoff);
            if out_of_window {
                continue;
            }
        }
        // Exact token equality, not a prefix or substring: `--event swap` must not also match a
        // hypothetical `swap_failed`.
        if let Some(wanted) = event {
            if line_event != Some(wanted) {
                continue;
            }
        }
        matched.push(Selected {
            line,
            ts,
            event: line_event,
        });
    }
    LogView {
        window,
        event,
        log_present,
        matched,
        n_scanned,
    }
}

/// What the verb writes, split by stream (see the module docs for why).
#[derive(Debug, PartialEq)]
struct Rendered {
    /// The DATA stream → stdout.
    out: String,
    /// The OPERATOR notice → stderr; empty when there is nothing to say.
    notice: String,
}

/// The text view: the selected durable lines, verbatim, one per line, and nothing else.
///
/// Each line is re-terminated with a single `\n`, so the stream is newline-normalized; for the
/// newline-terminated file the daemon actually writes, a no-filter render reproduces the log
/// byte for byte.
fn render_text(view: &LogView) -> Rendered {
    let mut out = String::new();
    for selected in &view.matched {
        out.push_str(selected.line);
        out.push('\n');
    }
    Rendered {
        out,
        notice: notice(view),
    }
}

/// The `--json` view: one document, one record per matched line, carrying an explicit schema
/// version so a consumer can version-gate.
fn render_json(view: &LogView) -> Result<Rendered> {
    let wire = LogWire {
        schema: JSON_SCHEMA_VERSION,
        log_present: view.log_present,
        window: view.window.as_ref().map(|w| WindowWire {
            since: w.since_arg.as_str(),
            cutoff: w.cutoff_rfc3339(),
        }),
        event: view.event,
        n_scanned: view.n_scanned,
        n_matched: view.matched.len(),
        records: view
            .matched
            .iter()
            .map(|s| RecordWire {
                ts: s.ts,
                event: s.event,
                line: s.line,
            })
            .collect(),
    };
    let mut out = serde_json::to_string_pretty(&wire)
        .map_err(|_| Error::LogSerialize("a log record was not serializable"))?;
    out.push('\n');
    Ok(Rendered {
        out,
        notice: notice(view),
    })
}

/// The operator notice → stderr: what was filtered, and — when nothing came back — which of the
/// three empty states it was, so an empty stdout never has to be guessed at.
///
/// Silent when a plain `sessiometer log` renders a non-empty log: there is nothing to say that
/// the lines do not already say, and silence keeps the common case clean.
fn notice(view: &LogView) -> String {
    let mut notice = String::new();
    if !view.log_present {
        notice.push_str("no event log yet — the daemon has not run\n");
        return notice;
    }
    if let Some(window) = &view.window {
        notice.push_str(&format!(
            "window: events at/after {} (--since {})\n",
            window.cutoff_rfc3339(),
            window.since_arg
        ));
    }
    if let Some(event) = view.event {
        notice.push_str(&format!("filter: event={event}\n"));
    }
    if view.n_scanned == 0 {
        notice.push_str("the event log is empty — no events recorded yet\n");
    } else if view.matched.is_empty() {
        notice.push_str("no matching events\n");
    } else if view.window.is_some() || view.event.is_some() {
        notice.push_str(&format!(
            "matched {} of {} lines\n",
            view.matched.len(),
            view.n_scanned
        ));
    }
    notice
}

/// The `--json` document (schema 1). Named for its verb, like `StatsWire` and
/// `ReliabilityWire`.
#[derive(Serialize)]
struct LogWire<'a> {
    /// The schema version — bumped on any change a consumer could not ignore.
    schema: u32,
    /// `false` when there is no log file at all, so a script can tell a cold install from a
    /// quiet one without parsing prose.
    log_present: bool,
    /// The resolved `--since` window, or `null` when the whole log was read.
    window: Option<WindowWire<'a>>,
    /// The `--event` filter, or `null` when every event was read.
    event: Option<&'a str>,
    /// Lines the log held, filters aside.
    n_scanned: usize,
    /// Lines that matched — always `records.len()`.
    n_matched: usize,
    /// One record per matched line, in file order.
    records: Vec<RecordWire<'a>>,
}

/// The resolved window, on the wire.
#[derive(Serialize)]
struct WindowWire<'a> {
    /// The raw span exactly as the operator typed it.
    since: &'a str,
    /// The resolved cutoff, in the log's own RFC 3339 UTC shape.
    cutoff: String,
}

/// One matched line, on the wire. `ts` and `event` are the line's own tokens; `line` is the
/// durable line itself, verbatim.
#[derive(Serialize)]
struct RecordWire<'a> {
    ts: Option<&'a str>,
    event: Option<&'a str>,
    line: &'a str,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A representative log slice. Swap lines carry real-shaped account **emails** in
    /// `from=`/`to=` — exactly as the production log does — so the byte-faithfulness guard below
    /// genuinely exercises the case that matters instead of passing on inert handles. Spans three
    /// hours so a `--since` window can bisect it, and mixes event families so `--event` has
    /// something to exclude.
    const FIXTURE_LOG: &str = "\
ts=2026-07-11T00:00:00Z event=swap from=oleksii@pelykh.com to=spare reason=session session_pct=95
ts=2026-07-11T01:00:00Z event=restash acct=u-A outcome=ok
ts=2026-07-11T02:00:00Z event=all_exhausted hold=spare
ts=2026-07-11T03:00:00Z event=swap from=spare to=oleksii@pelykh.com reason=session session_pct=96
";

    /// Parse a fixture `ts=` through the SAME canonical reader the production window path uses.
    fn epoch(ts: &str) -> i64 {
        epoch_from_rfc3339(ts).expect("valid RFC 3339 fixture")
    }

    /// A window resolved as `now − since`, with `now` given as a fixture instant.
    fn window(since: &str, now: &str) -> Window {
        Window::resolve(since, epoch(now)).expect("valid duration")
    }

    /// The CONSTRAINT-A predicate: does every line of `rendered` appear verbatim in `durable`?
    ///
    /// Deliberately a named helper rather than an inline assertion, so the canary below can
    /// exercise the predicate ITSELF — a check that cannot be shown to fail proves nothing.
    fn every_line_is_in(rendered: &str, durable: &str) -> bool {
        rendered
            .lines()
            .all(|line| durable.lines().any(|d| d == line))
    }

    #[test]
    fn no_flags_emits_every_line_in_file_order_byte_identical() {
        let view = select(Some(FIXTURE_LOG), None, None);
        assert_eq!(view.n_scanned, 4);
        assert_eq!(view.matched.len(), 4);
        let rendered = render_text(&view);
        // The whole point of the verb: what comes out IS the log.
        assert_eq!(rendered.out, FIXTURE_LOG);
        // And a plain read says nothing on stderr — the lines already said it.
        assert_eq!(rendered.notice, "");
    }

    #[test]
    fn since_keeps_only_lines_at_or_after_the_cutoff_and_states_it() {
        // A 1 h window taken at 03:00 keeps 02:00 (at the boundary — IN) and 03:00, dropping the
        // two older lines.
        let view = select(
            Some(FIXTURE_LOG),
            Some(window("1h", "2026-07-11T03:00:00Z")),
            None,
        );
        assert_eq!(view.matched.len(), 2);
        assert_eq!(
            view.matched.iter().map(|s| s.ts).collect::<Vec<_>>(),
            vec![Some("2026-07-11T02:00:00Z"), Some("2026-07-11T03:00:00Z")],
        );
        let rendered = render_text(&view);
        assert!(rendered.out.contains("event=all_exhausted"));
        assert!(!rendered.out.contains("event=restash"));
        // The resolved cutoff is STATED — an operator can see which window they got.
        assert!(
            rendered
                .notice
                .contains("window: events at/after 2026-07-11T02:00:00Z (--since 1h)"),
            "notice must state the resolved cutoff, got {:?}",
            rendered.notice
        );
        assert!(rendered.notice.contains("matched 2 of 4 lines"));
    }

    #[test]
    fn a_windowed_view_drops_a_line_it_cannot_place_in_time() {
        // Tolerant-drop, mirroring the sibling reader: unplaceable ⇒ not provably in-window.
        let log =
            "ts=nonsense event=swap from=a to=b\nts=2026-07-11T03:00:00Z event=swap from=b to=a\n";
        let windowed = select(Some(log), Some(window("1h", "2026-07-11T03:00:00Z")), None);
        assert_eq!(windowed.matched.len(), 1);
        assert_eq!(windowed.matched[0].ts, Some("2026-07-11T03:00:00Z"));
        // But with no window, no timestamp is consulted, so the same line is emitted like any other.
        let whole = select(Some(log), None, None);
        assert_eq!(whole.matched.len(), 2);
    }

    #[test]
    fn malformed_since_is_a_parse_error_never_a_whole_log_fallback() {
        let now = epoch("2026-07-11T03:00:00Z");
        for bad in ["7x", "-1d", "", "   ", "7", "d", "1.5h"] {
            let err = Window::resolve(bad, now).unwrap_err();
            assert!(
                matches!(err, Error::LogSinceInvalid(_)),
                "{bad:?} must be rejected as LogSinceInvalid, got {err:?}"
            );
            // The message must name the flag and show what was rejected.
            let shown = err.to_string();
            assert!(
                shown.contains("--since") && shown.contains(bad.trim()),
                "error must name --since and echo the bad value, got {shown:?}"
            );
        }
    }

    #[test]
    fn event_filter_matches_the_token_exactly() {
        let view = select(Some(FIXTURE_LOG), None, Some("swap"));
        assert_eq!(view.matched.len(), 2);
        assert!(view.matched.iter().all(|s| s.event == Some("swap")));
        let rendered = render_text(&view);
        assert!(!rendered.out.contains("event=restash"));
        assert!(!rendered.out.contains("event=all_exhausted"));
        assert!(rendered.notice.contains("filter: event=swap"));

        // EXACT, not a prefix or substring: a longer token that merely starts with the filter
        // must not match, or `--event swap` would silently widen as new events are added.
        let log = "ts=2026-07-11T00:00:00Z event=swap_failed acct=u-A\n";
        assert_eq!(select(Some(log), None, Some("swap")).matched.len(), 0);
        assert_eq!(
            select(Some(log), None, Some("swap_failed")).matched.len(),
            1
        );
    }

    #[test]
    fn the_two_filters_compose_as_an_and_not_an_or() {
        // A 1 h window at 03:00 admits 02:00 (all_exhausted) and 03:00 (swap); `--event swap`
        // then leaves one. An OR would leave three — both in-window lines plus the out-of-window
        // 00:00 swap — so this pins the conjunction, which neither single-filter test can.
        let view = select(
            Some(FIXTURE_LOG),
            Some(window("1h", "2026-07-11T03:00:00Z")),
            Some("swap"),
        );
        assert_eq!(view.matched.len(), 1);
        assert_eq!(view.matched[0].ts, Some("2026-07-11T03:00:00Z"));
        let rendered = render_text(&view);
        assert!(rendered.notice.contains("matched 1 of 4 lines"));
        // Both filters are documented in the notice, not just the last one applied.
        assert!(rendered.notice.contains("--since 1h") && rendered.notice.contains("event=swap"));
    }

    #[test]
    fn an_absent_event_token_says_no_matching_events_in_both_views() {
        let view = select(Some(FIXTURE_LOG), None, Some("no_such_event"));
        assert!(view.matched.is_empty());

        // Text view: empty stdout, but never an ambiguous silence.
        let rendered = render_text(&view);
        assert_eq!(rendered.out, "");
        assert!(
            rendered.notice.contains("no matching events"),
            "an empty result must be stated, got {:?}",
            rendered.notice
        );

        // JSON view: still a VALID document rather than a bare notice, so a script's parse does
        // not fail just because nothing matched. (`the_three_empty_states_are_distinguishable`
        // covers this input's text notice among the other two cold states; the JSON shape of an
        // empty *match* — as opposed to an empty log — is pinned only here.)
        let json = render_json(&view).expect("serializes");
        let parsed: serde_json::Value = serde_json::from_str(&json.out).expect("parses");
        assert_eq!(
            parsed["log_present"], true,
            "the log exists; nothing matched"
        );
        assert_eq!(parsed["n_scanned"], 4);
        assert_eq!(parsed["n_matched"], 0);
        assert_eq!(parsed["records"].as_array().expect("array").len(), 0);
        assert_eq!(parsed["event"], "no_such_event");
    }

    #[test]
    fn the_three_empty_states_are_distinguishable() {
        // 1. No log file at all — a fresh install. A normal cold state, said plainly.
        let absent = render_text(&select(None, None, None));
        assert_eq!(absent.out, "");
        assert!(absent.notice.contains("no event log yet"));

        // 2. A log file with no lines — the daemon ran but recorded nothing.
        let empty = render_text(&select(Some(""), None, None));
        assert_eq!(empty.out, "");
        assert!(empty.notice.contains("the event log is empty"));

        // 3. Lines, but none matching the filter.
        let unmatched = render_text(&select(Some(FIXTURE_LOG), None, Some("nope")));
        assert_eq!(unmatched.out, "");
        assert!(unmatched.notice.contains("no matching events"));

        // The three notices are genuinely different — an operator can act on which one they got.
        assert_ne!(absent.notice, empty.notice);
        assert_ne!(empty.notice, unmatched.notice);
    }

    #[test]
    fn json_parses_and_carries_a_schema_and_one_record_per_matched_line() {
        let view = select(Some(FIXTURE_LOG), None, Some("swap"));
        let rendered = render_json(&view).expect("serializes");
        let parsed: serde_json::Value =
            serde_json::from_str(&rendered.out).expect("output parses as JSON");

        assert_eq!(parsed["schema"], JSON_SCHEMA_VERSION);
        assert_eq!(parsed["log_present"], true);
        assert_eq!(parsed["event"], "swap");
        assert_eq!(parsed["n_scanned"], 4);
        assert_eq!(parsed["n_matched"], 2);

        let records = parsed["records"].as_array().expect("records is an array");
        assert_eq!(records.len(), 2, "one record per matched line");
        for record in records {
            assert_eq!(record["event"], "swap");
            // Each record's `line` is the durable line itself.
            assert!(
                FIXTURE_LOG
                    .lines()
                    .any(|d| d == record["line"].as_str().expect("line is a string")),
                "record line must be a durable line: {record}"
            );
        }
    }

    #[test]
    fn json_documents_the_window_and_the_absent_log() {
        let windowed = render_json(&select(
            Some(FIXTURE_LOG),
            Some(window("1h", "2026-07-11T03:00:00Z")),
            None,
        ))
        .expect("serializes");
        let parsed: serde_json::Value = serde_json::from_str(&windowed.out).expect("parses");
        assert_eq!(parsed["window"]["since"], "1h");
        assert_eq!(parsed["window"]["cutoff"], "2026-07-11T02:00:00Z");

        // A cold install still yields a valid, parseable document — never a bare notice.
        let cold = render_json(&select(None, None, None)).expect("serializes");
        let parsed: serde_json::Value = serde_json::from_str(&cold.out).expect("parses");
        assert_eq!(parsed["log_present"], false);
        assert_eq!(parsed["n_matched"], 0);
        assert_eq!(parsed["records"].as_array().expect("array").len(), 0);
        assert_eq!(parsed["window"], serde_json::Value::Null);
        assert_eq!(parsed["event"], serde_json::Value::Null);
    }

    /// CONSTRAINT-A (issue #773): every byte the reader writes to **stdout** must already exist
    /// in the durable log — the reader interpolates no account data of its own.
    ///
    /// Structured like [`crate::reliability`]'s `readout_carries_no_pii`: a non-degeneracy guard
    /// first, so the property cannot pass vacuously, then the property over every view, then a
    /// CANARY proving the property's predicate can actually fail. A guard that cannot be shown
    /// to fail is not a guard.
    #[test]
    fn stdout_bytes_all_exist_in_the_durable_log() {
        // Non-degeneracy: the fixture MUST carry an email in its swap `from=`/`to=`, as the
        // production log does — otherwise "interpolates nothing" would be a claim about content
        // that was never at risk.
        assert!(
            !crate::redaction::meter::unauthored_emails(FIXTURE_LOG, &[]).is_empty(),
            "fixture must contain an email so the containment guard is a real regression catch"
        );

        for view in [
            select(Some(FIXTURE_LOG), None, None),
            select(Some(FIXTURE_LOG), None, Some("swap")),
            select(
                Some(FIXTURE_LOG),
                Some(window("1h", "2026-07-11T03:00:00Z")),
                None,
            ),
        ] {
            // Non-degeneracy per view: an empty selection satisfies containment vacuously.
            assert!(
                !view.matched.is_empty(),
                "each view must select lines, else its guard proves nothing"
            );

            let text = render_text(&view);
            assert!(
                every_line_is_in(&text.out, FIXTURE_LOG),
                "text stdout carried a line absent from the durable log: {:?}",
                text.out
            );
            // Membership alone is not enough: it is blind to MULTIPLICATION and REORDERING —
            // a render that emitted every line twice, or shuffled them, would satisfy it while
            // doubling the exposure of every email-bearing line. Pin the exact stream: the
            // selected lines, each once, in order.
            let expected: String = view
                .matched
                .iter()
                .map(|s| format!("{}\n", s.line))
                .collect();
            assert_eq!(
                text.out, expected,
                "text stdout must be exactly the selected lines, each once, in file order"
            );

            // The JSON document is reader-authored STRUCTURE, so the constraint is checked where
            // it actually governs: every embedded `line` value must be a durable line.
            let json = render_json(&view).expect("serializes");
            let parsed: serde_json::Value = serde_json::from_str(&json.out).expect("parses");
            for record in parsed["records"].as_array().expect("array") {
                assert!(
                    FIXTURE_LOG
                        .lines()
                        .any(|d| d == record["line"].as_str().expect("line is a string")),
                    "json record carried a line absent from the durable log: {record}"
                );
            }
        }

        // CANARY: the predicate MUST reject a render carrying a token-shaped string that is NOT
        // in the durable log. Without this, a predicate that always returned `true` would satisfy
        // every assertion above.
        let smuggled = "ts=2026-07-11T00:00:00Z event=swap token=sk-ant-INJECTED-NOT-IN-LOG\n";
        assert!(
            !every_line_is_in(smuggled, FIXTURE_LOG),
            "the containment guard must trip on an interpolated, token-shaped line"
        );
        // …and it must still trip when that line hides inside an otherwise-valid render, which is
        // how a real interpolation regression would actually look.
        let poisoned = format!(
            "{}{smuggled}",
            render_text(&select(Some(FIXTURE_LOG), None, None)).out
        );
        assert!(
            !every_line_is_in(&poisoned, FIXTURE_LOG),
            "the containment guard must trip on a smuggled line appended to a valid render"
        );
    }

    #[test]
    fn a_repeated_key_resolves_to_its_last_occurrence() {
        // Matches the `BTreeMap::insert` the sibling reader folds through, so the two readers
        // cannot disagree about what a line says. The frozen grammar emits no duplicate keys —
        // this pins the tie-break so a future one cannot silently diverge.
        let line = "ts=2026-07-11T00:00:00Z event=swap event=restash";
        assert_eq!(field(line, "event"), Some("restash"));
        assert_eq!(field(line, "missing"), None);
    }

    #[test]
    fn a_line_without_the_filtered_keys_is_handled_not_crashed() {
        let log = "a bare line with no key=val at all\nts=2026-07-11T00:00:00Z event=swap\n";
        // No window, no filter: everything is emitted, including the bare line.
        assert_eq!(select(Some(log), None, None).matched.len(), 2);
        // With an `--event` filter, the bare line has no `event=` and so cannot match.
        let filtered = select(Some(log), None, Some("swap"));
        assert_eq!(filtered.matched.len(), 1);
        assert_eq!(filtered.matched[0].ts, Some("2026-07-11T00:00:00Z"));
    }

    /// The data/notice split is the contract the whole reader rests on — AC 9's "verbatim and
    /// nothing else" and the clean-pipe promise both live or die on WHICH stream each string
    /// reaches. Asserted against two in-memory sinks, so swapping the two destinations fails
    /// here rather than silently shipping.
    #[test]
    fn emit_routes_the_data_to_stdout_and_the_notice_to_stderr() {
        let view = select(Some(FIXTURE_LOG), None, Some("swap"));
        let rendered = render_text(&view);
        // Non-degeneracy: both streams must be non-empty, else the routing assertions below
        // would hold vacuously.
        assert!(!rendered.out.is_empty() && !rendered.notice.is_empty());

        let (mut data, mut notice) = (Vec::new(), Vec::new());
        emit(&rendered, &mut data, &mut notice).expect("both sinks accept the write");
        let data = String::from_utf8(data).expect("utf-8");
        let notice = String::from_utf8(notice).expect("utf-8");

        // The data stream is exactly the rendered lines …
        assert_eq!(data, rendered.out);
        assert!(
            !data.contains("filter:") && !data.contains("matched "),
            "reader-authored notice text must never reach the data stream: {data:?}"
        );
        // … and not one durable log line reaches the notice stream. Together these fail if the
        // two sinks are ever transposed.
        assert_eq!(notice, rendered.notice);
        assert!(
            !notice
                .lines()
                .any(|line| FIXTURE_LOG.lines().any(|d| d == line)),
            "no durable log line may reach the notice stream: {notice:?}"
        );
    }

    #[test]
    fn emit_writes_nothing_to_the_notice_stream_when_there_is_nothing_to_say() {
        // A plain `sessiometer log` over a non-empty log: stdout is the log, stderr untouched.
        let rendered = render_text(&select(Some(FIXTURE_LOG), None, None));
        let (mut data, mut notice) = (Vec::new(), Vec::new());
        emit(&rendered, &mut data, &mut notice).expect("writes");
        assert_eq!(data, FIXTURE_LOG.as_bytes());
        assert!(notice.is_empty(), "stderr must stay silent: {notice:?}");
    }

    /// A sink whose every write fails with the given kind — stands in for a downstream that
    /// closed the pipe (`… | head`) and for a genuine IO failure.
    struct FailingSink(std::io::ErrorKind);

    impl Write for FailingSink {
        fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
            Err(std::io::Error::from(self.0))
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Err(std::io::Error::from(self.0))
        }
    }

    #[test]
    fn a_closed_downstream_is_success_but_a_real_io_error_is_not() {
        let rendered = render_text(&select(Some(FIXTURE_LOG), None, None));
        // `sessiometer log | head -3` closes the pipe after three lines. For a reader that
        // advertises piping, that is the ordinary end of a stream, not a failure — and it must
        // not be the panic the crate's `print!`-based verbs produce.
        assert!(
            emit(
                &rendered,
                &mut FailingSink(std::io::ErrorKind::BrokenPipe),
                &mut Vec::new()
            )
            .is_ok(),
            "a closed downstream must exit cleanly"
        );
        // Discriminating: a genuine IO failure is NOT swallowed by the same arm.
        assert!(matches!(
            emit(
                &rendered,
                &mut FailingSink(std::io::ErrorKind::PermissionDenied),
                &mut Vec::new()
            ),
            Err(Error::Io(_))
        ));
    }

    /// AC 7's headline behaviour — an absent log file is a cold state, not an error — reaching
    /// the real `NotFound` arm rather than a hand-fabricated `log_present: false`.
    #[test]
    fn an_absent_log_file_reads_as_none_and_an_empty_one_as_empty() {
        let dir = tempfile::tempdir().expect("tempdir");

        // No file at all → `None`, and NOT an error.
        let missing = dir.path().join("sessiometer.log");
        assert_eq!(
            read_event_log_at(&missing).expect("absent is not an error"),
            None
        );

        // A file that exists but holds nothing → `Some("")`, the distinct second state.
        let empty = dir.path().join("empty.log");
        std::fs::write(&empty, "").expect("write");
        assert_eq!(
            read_event_log_at(&empty).expect("readable"),
            Some(String::new())
        );

        // A file with content → read whole, byte for byte.
        let full = dir.path().join("full.log");
        std::fs::write(&full, FIXTURE_LOG).expect("write");
        assert_eq!(
            read_event_log_at(&full).expect("readable"),
            Some(FIXTURE_LOG.to_string())
        );

        // The two cold states drive different notices end-to-end.
        let absent = render_text(&select(None, None, None));
        let empty = render_text(&select(Some(""), None, None));
        assert!(absent.notice.contains("no event log yet"));
        assert!(empty.notice.contains("the event log is empty"));
    }
}
