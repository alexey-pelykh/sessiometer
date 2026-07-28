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
//! later channel adds a *source*, not a rewrite.
//!
//! # Following (`--follow`, issue #774)
//!
//! Watching a *running* daemon is the case a reader exists for, and a one-shot render does not
//! serve it. `--follow` backfills once and then streams: [`Follower::poll`] retains a byte offset
//! and re-reads forward from it, cycle after cycle. The mechanism is **poll-with-seek** and the
//! alternative was rejected during scoping — FSEvents/kqueue is new machinery, and the crate holds
//! a minimal-dependency line (`CONTRIBUTING.md`), so no file-watching crate enters the graph for
//! a `stat` this reader can make itself.
//!
//! Four properties carry the whole design, and each answers a specific way a naive tailer breaks:
//!
//! - **The follower holds a PATH and an offset — never an open handle.** Re-opening per cycle is
//!   what makes reattachment possible at all: an open handle stays bound to the *unlinked inode*
//!   after a rotation, so it can never see the file that replaced it — and, holding no path to
//!   re-resolve, it cannot even say so. Resolving the path each cycle is what lets a `dev`/`ino`
//!   change be SEEN ([`Transition::Replaced`]). It also makes "no orphaned handle" structural
//!   rather than merely asserted — there is no handle to orphan.
//! - **Only COMPLETE lines are emitted, and only complete lines advance the offset.** Bytes past
//!   the final `\n` are a half-written line; they are neither emitted nor consumed, so the next
//!   cycle re-reads them once the writer finishes. This is what keeps CONSTRAINT-A exact under a
//!   concurrent writer: the follower never CONSTRUCTS a partial line. (It cannot promise more than
//!   that. An interrupt during a large backfill can still land inside the kernel's `write(2)` loop
//!   and cut the stream mid-line, exactly as it can for `cat` — what is guaranteed is that every
//!   byte which did reach stdout is a byte of the durable log.) Because the offset is only ever
//!   advanced to just past a `\n`, that position is also an invariant a later cycle can CHECK —
//!   which is how [`Transition::Rewritten`] is caught.
//! - **`--since` bounds the BACKFILL; `--event` filters every line.** The window is a statement
//!   about *history*, so it applies to the first batch read from an attached file and is then
//!   dropped — a line that arrives afterwards is live by construction. `--event` is a content
//!   filter with no time in it, so it keeps applying to streamed lines (which is what makes a
//!   streamed filter falsifiable rather than vacuous — see the canary test).
//! - **A closed downstream ENDS the follow, and is not waited for.** [`write_stream`] treats
//!   `EPIPE` as success, which is right for a one-shot render but would spin a follower forever
//!   against a dead `| head`; the seam therefore reports [`Pipe::Closed`] and [`follow_loop`]
//!   stops on it. That alone is not enough, because a writer only discovers `EPIPE` by WRITING,
//!   and a follower's next write is the daemon's next event — never, once the daemon stops. So the
//!   liveness question is also asked directly, between cycles, by [`hung_up`].
//!
//! There is no signal handler: SIGINT's default disposition already exits cleanly for a reader
//! that holds no lock, no temp file, no open handle, and no partial write. Installing one would
//! be new machinery buying nothing.
//!
//! The stream-shape invariant is unchanged under follow. stdout stays the durable lines and
//! nothing else — every transition the follower reports (attached, truncated, replaced, waiting
//! for a log that does not exist yet) is operator metadata and goes to stderr, so `--follow | grep`
//! stays as honest as the one-shot form. `--follow --json` is the one shape that differs, and it
//! must: a `records: [...]` array cannot be closed on a stream, so the follow view is **JSON
//! Lines** — see [`render_follow`].

use crate::error::{Error, Result};
use crate::usage::epoch_from_rfc3339;
use serde::Serialize;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

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
    /// `-f` / `--follow` (issue #774) — after the initial render, keep reading newly appended
    /// lines until the process is interrupted or the downstream pipe closes.
    pub(crate) follow: bool,
}

/// How long the follower waits between cycles.
///
/// A cycle is one `stat`, plus — only when the file actually grew — the one-byte anchor probe and
/// one short read, so the cost of polling twice a second is negligible against a daemon that
/// writes an event every few tens of seconds. It is chosen for the READER's latency rather than
/// the writer's rate: half a second is under the threshold at which a human watching a tail
/// notices a delay, which is the whole point of the verb.
const POLL_INTERVAL: std::time::Duration = std::time::Duration::from_millis(500);

/// Entry point for the `log` verb: one-shot by default, streaming under `--follow`.
pub(crate) fn run(args: LogArgs) -> Result<()> {
    if args.follow {
        run_follow(args)
    } else {
        run_once(args)
    }
}

/// The one-shot reader (issue #773): read the event log once, select, and render.
///
/// An absent log file is a normal cold state (a fresh install), not an error: the verb says so
/// and exits `0`. So does a log with no matching line — the notice distinguishes *no file*, *an
/// empty file*, and *no match*, so a silent exit never has to be guessed at.
fn run_once(args: LogArgs) -> Result<()> {
    let text = read_event_log()?;
    // Resolved BEFORE selecting, so the cutoff is a plain integer the pure path filters by.
    let window = resolve_window(args.since.as_deref())?;
    let view = select(text.as_deref(), window, args.event.as_deref());
    let rendered = if args.json {
        render_json(&view)?
    } else {
        render_text(&view)
    };
    // stdout is the data stream, stderr the operator notice — see the module docs. The one-shot
    // path has nothing left to do after the single write, so whether the downstream is still
    // open is not information it can act on; `--follow` is the caller that reads it.
    emit(
        &rendered,
        &mut std::io::stdout().lock(),
        &mut std::io::stderr().lock(),
    )
    .map(|_| ())
}

/// Whether a downstream is still accepting bytes.
///
/// A one-shot render does not care — it is finished either way. A FOLLOWER does: without this
/// distinction `sessiometer log --follow | head -3` would spin forever, re-rendering into a pipe
/// nobody reads, because [`write_stream`] deliberately reports `EPIPE` as success.
#[derive(Debug, PartialEq, Clone, Copy)]
enum Pipe {
    /// The downstream accepted the write.
    Open,
    /// The downstream closed the pipe — the normal end of `… | head`.
    Closed,
}

/// Write a [`Rendered`] to its two destinations, reporting whether the DATA stream survived.
///
/// Split out of [`run_once`] so the stream ROUTING — the data/notice separation this reader's
/// whole contract rests on — is assertable against two in-memory sinks rather than the process's
/// real descriptors. Swapping the two arguments must fail a test; that is the point of the seam.
///
/// Only the data stream's fate is reported. A follower whose stderr closed but whose stdout is
/// still read is doing exactly what it was asked to do, so a closed NOTICE stream is not a reason
/// to stop delivering lines.
fn emit(rendered: &Rendered, data: &mut impl Write, notice: &mut impl Write) -> Result<Pipe> {
    let pipe = write_stream(data, &rendered.out)?;
    write_stream(notice, &rendered.notice)?;
    Ok(pipe)
}

/// Write `text` to `sink`, treating a closed downstream as success.
///
/// `sessiometer log | head -3` closes the pipe after three lines — the ordinary use of a
/// line-stream reader, and the one this verb actively advertises. The crate's other readers use
/// `print!`, which PANICS on `EPIPE`; over a 2 MB log that panic is easy to hit, so this reader
/// treats it as the normal end of a pipe and exits `0`. Every other IO error still propagates.
///
/// The closure is reported rather than merely swallowed ([`Pipe`]) — success and "there is
/// nobody left to write to" are the same EXIT CODE but not the same fact, and a follower has to
/// act on the second.
fn write_stream(sink: &mut impl Write, text: &str) -> Result<Pipe> {
    if text.is_empty() {
        return Ok(Pipe::Open);
    }
    match sink.write_all(text.as_bytes()).and_then(|()| sink.flush()) {
        Ok(()) => Ok(Pipe::Open),
        Err(err) if err.kind() == std::io::ErrorKind::BrokenPipe => Ok(Pipe::Closed),
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

/// Resolve the optional `--since` value into a [`Window`], reading the wall clock only when the
/// flag was actually given.
///
/// Both entry points resolve it HERE, before any output: a malformed span is
/// [`Error::LogSinceInvalid`] rather than a silent whole-log fallback, and under `--follow` the
/// window stated in the header is provably the one that filtered the backfill. Shared rather than
/// forked, so the two paths cannot come to disagree about when the clock is read or which error a
/// bad span maps to.
fn resolve_window(since: Option<&str>) -> Result<Option<Window>> {
    since
        .map(|raw| Window::resolve(raw, now_epoch()))
        .transpose()
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

/// One matched line as a STANDALONE `--follow --json` record (JSON Lines).
///
/// [`RecordWire`] rides inside a document that carries `schema` once, for all of it. A follow
/// stream has no document — there is no header a late-attaching consumer could have read — so the
/// version travels on every record. That is the whole difference; the three data fields are the
/// same fields, holding the same borrowed subslices.
#[derive(Serialize)]
struct FollowRecordWire<'a> {
    /// The same [`JSON_SCHEMA_VERSION`] the one-shot document carries, repeated per record so a
    /// consumer can version-gate a line it read without a header.
    schema: u32,
    ts: Option<&'a str>,
    event: Option<&'a str>,
    line: &'a str,
}

/// What the file at the followed path did between two cycles.
///
/// Every variant is a distinct thing to TELL the operator (or to deliberately stay quiet about),
/// which is why this is an enum rather than a pair of booleans: the recovery a size regression
/// needs and the recovery an inode change needs are the same *mechanic* (resume from the new
/// start) but not the same *event*, and an operator watching a stream jump deserves to know
/// which one happened.
#[derive(Debug, PartialEq, Clone, Copy)]
enum Transition {
    /// No file at the path right now — a cold start before the daemon's first write (a normal
    /// state, per issue #774's acceptance), or a rotation caught between the unlink and create.
    Absent,
    /// A file was opened and read from its start: the first attach, or the one that resolves an
    /// [`Transition::Absent`] wait.
    Attached,
    /// Same file, new bytes — the ordinary case.
    Appended,
    /// Same file, no new bytes — the other ordinary case.
    Idle,
    /// Same file, now SHORTER than the offset already consumed: truncated in place. Resumed from
    /// its new start, which cannot re-emit the old content because the old content is gone.
    Truncated,
    /// Same file, NOT shorter — but the bytes under the retained offset are no longer the ones
    /// that put it there: rewritten in place by something that is not an append.
    ///
    /// Caught by [`anchored`] rather than by size, which is why it needs its own variant: a
    /// rewrite that leaves the file the same length or longer looks exactly like an append to a
    /// size check, and reading forward from a stale offset would emit a MID-LINE FRAGMENT — a
    /// string that is not any durable line, which is precisely what CONSTRAINT-A forbids.
    Rewritten,
    /// A DIFFERENT file at the same path: rotated away by an operator or by `newsyslog`.
    /// Reattached at the new file's start.
    ///
    /// The daemon holds its own log open for its whole run ([`crate::observability::EventLog`]),
    /// so a rotation leaves it appending to the moved-aside inode until it restarts. Reattaching
    /// to the PATH is therefore right about where the events will BE, not about where the last
    /// one went — and because the notice names the rotation, the quiet until the daemon restarts
    /// is explained rather than mysterious.
    Replaced,
}

/// One follow cycle's outcome.
#[derive(Debug)]
struct Polled {
    /// The complete lines newly available, verbatim — empty when nothing (complete) arrived.
    text: String,
    /// What the file did.
    transition: Transition,
}

/// Whether [`follow_loop`] should run another cycle.
#[derive(Debug, PartialEq, Clone, Copy)]
enum Flow {
    /// Poll again.
    Continue,
    /// Stop the follow and return.
    Stop,
}

/// The identity of the file an offset refers to, so a REPLACEMENT can be told from an append.
///
/// `(dev, ino)` and not `ino` alone: an inode number is only unique within a device, and the log
/// directory is not guaranteed to stay on one. Unix-only, matching the crate's existing posture —
/// `src/paths.rs` already imports `std::os::unix::fs::MetadataExt` at module level, ungated.
#[derive(Debug, PartialEq, Clone, Copy)]
struct FileIdentity {
    dev: u64,
    ino: u64,
}

impl FileIdentity {
    fn of(meta: &std::fs::Metadata) -> FileIdentity {
        use std::os::unix::fs::MetadataExt;
        FileIdentity {
            dev: meta.dev(),
            ino: meta.ino(),
        }
    }
}

/// The file the follower is currently reading, and how far into it.
///
/// Named for the STATE, not the event: [`Transition::Attached`] is the moment a file was picked
/// up, this is the position retained from then on.
#[derive(Debug, PartialEq, Clone, Copy)]
struct Attachment {
    /// Which file `offset` refers to — a change means the path now names a different file.
    identity: FileIdentity,
    /// Bytes already CONSUMED — read and handed to the filters. Not "already emitted": a line the
    /// `--event` filter drops is consumed and never printed, and so is one the backfill window
    /// rejects. Emission is decided downstream, in [`follow_loop`], after `poll` has advanced.
    ///
    /// Only COMPLETE lines advance it, so it always lands just past a `\n` (or at `0`). That is an
    /// invariant, not a coincidence — [`anchored`] checks it to catch a rewrite underneath us.
    offset: u64,
}

/// A position in the event log that survives the log being truncated or rotated under it.
///
/// Holds a PATH and an offset, never an open handle — see the module docs for why that is the
/// load-bearing choice rather than an implementation detail.
struct Follower {
    /// The path re-resolved every cycle. Resolving it (rather than keeping a handle) is what
    /// makes [`Transition::Replaced`] observable.
    path: PathBuf,
    /// `None` before the first attach, and again whenever the file goes away.
    attached: Option<Attachment>,
}

impl Follower {
    /// A follower that has not yet attached to anything at `path`.
    fn new(path: PathBuf) -> Follower {
        Follower {
            path,
            attached: None,
        }
    }

    /// Run one cycle: classify what the file did, then read forward from the retained offset.
    ///
    /// # Recovery
    ///
    /// Three checks, in this order, because each is more specific than the next:
    ///
    /// 1. **Identity** — a different `(dev, ino)` is a replacement, and the operator deserves to
    ///    be told that rather than a vaguer fact a later check would produce.
    /// 2. **Size below the offset** — a truncation.
    /// 3. **The anchor byte** ([`anchored`]) — the offset always lands just past a `\n`, so if the
    ///    byte before it is not one, the bytes underneath were rewritten. This is what catches a
    ///    same-inode rewrite that leaves the file the same length or LONGER, which the first two
    ///    checks read as an ordinary append; reading forward from the stale offset would then emit
    ///    a mid-line fragment — a string that is not any durable line, which CONSTRAINT-A forbids.
    ///    It also catches an inode number a filesystem recycled for the replacement file, which
    ///    defeats check 1.
    ///
    /// # The remaining hole, stated
    ///
    /// A file rewritten to EXACTLY its previous length still reads as [`Transition::Idle`]: the
    /// offset is then the file's end, so its anchor byte is the final `\n` a log always ends with,
    /// and check 3 cannot discriminate. Closing that would mean re-reading and comparing the whole
    /// file every cycle — a cost this reader is not worth, and one no tailer pays (`tail -F`
    /// included). The daemon appends and never rewrites, so it is reachable only by an external
    /// tool doing something the log's own writer never does.
    fn poll(&mut self) -> Result<Polled> {
        let meta = match std::fs::metadata(&self.path) {
            Ok(meta) => meta,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                // Drop the position: whatever appears at this path next is a different file and
                // must be read from ITS start, never from a stale offset into a file that is gone.
                self.attached = None;
                return Ok(Polled {
                    text: String::new(),
                    transition: Transition::Absent,
                });
            }
            Err(err) => return Err(Error::Io(err)),
        };
        let identity = FileIdentity::of(&meta);
        // Classify and pick the read position together, in ONE match over the retained position:
        // a resume reads from the new file's start, an append from where the last cycle stopped.
        // Deriving the offset separately would mean a second match that has to be kept in step
        // with this one — and an `Appended` arm with no position to continue from, which the
        // first arm here has already ruled out.
        let (transition, from) = match self.attached {
            None => (Transition::Attached, 0),
            Some(attached) if attached.identity != identity => (Transition::Replaced, 0),
            Some(attached) if meta.len() < attached.offset => (Transition::Truncated, 0),
            Some(attached) if meta.len() > attached.offset => {
                // Grown — but grown by an APPEND only if the offset still sits where a complete
                // line ended. Otherwise the bytes under it were rewritten (see the doc above).
                if anchored(&self.path, attached.offset)? {
                    (Transition::Appended, attached.offset)
                } else {
                    (Transition::Rewritten, 0)
                }
            }
            // Nothing new to read — skip the open entirely rather than pay for a no-op read
            // twice a second forever.
            Some(_) => {
                return Ok(Polled {
                    text: String::new(),
                    transition: Transition::Idle,
                });
            }
        };
        let (text, offset) = read_forward(&self.path, from)?;
        self.attached = Some(Attachment { identity, offset });
        Ok(Polled { text, transition })
    }
}

/// Whether `offset` still sits just past a `\n` in `path` — i.e. whether it still means what it
/// meant when it was recorded.
///
/// [`read_forward`] only ever advances the offset to just past a line terminator, so that is an
/// INVARIANT of a correctly-tracked file. When it no longer holds, the bytes underneath were
/// rewritten by something that is not an append, and reading forward from there would slice into
/// the middle of a line. Offset `0` is trivially anchored — there is nothing before it.
///
/// One byte, on a file that is about to be opened and read anyway. Cheap enough to pay every
/// cycle, which is what makes it a guard rather than a diagnostic.
fn anchored(path: &Path, offset: u64) -> Result<bool> {
    let Some(previous) = offset.checked_sub(1) else {
        return Ok(true);
    };
    let mut file = std::fs::File::open(path)?;
    file.seek(SeekFrom::Start(previous))?;
    let mut byte = [0u8; 1];
    match file.read_exact(&mut byte) {
        Ok(()) => Ok(byte[0] == b'\n'),
        // The byte is not there any more, so the offset certainly does not mean what it did.
        Err(err) if err.kind() == std::io::ErrorKind::UnexpectedEof => Ok(false),
        Err(err) => Err(Error::Io(err)),
    }
}

/// Read `path` from `offset` and return the COMPLETE lines found there, plus the offset past
/// them.
///
/// Bytes after the final `\n` are a line the writer has not finished: they are neither returned
/// nor counted into the new offset, so the next cycle re-reads them once the terminator lands.
/// That is what keeps a concurrent writer from ever splitting a rendered line: the follower never
/// CONSTRUCTS a partial one, so there is never a partial line for an interrupt to catch it
/// mid-way through. (The kernel is a separate matter — a signal can still land inside the
/// `write(2)` loop of a large backfill, exactly as it can for `cat`. Every byte that reached
/// stdout is still a byte of the durable log; no userspace program can make a write atomic
/// against an async signal without blocking it around the call.)
///
/// Non-UTF-8 is an error rather than a lossy substitution: `read_to_string` already rejects it on
/// the one-shot path, and CONSTRAINT-A forbids emitting a replacement character the durable line
/// never held. Slicing at a `\n` is always a char boundary in valid UTF-8, since no continuation
/// byte can be `0x0A`.
fn read_forward(path: &Path, offset: u64) -> Result<(String, u64)> {
    let mut file = std::fs::File::open(path)?;
    file.seek(SeekFrom::Start(offset))?;
    let mut buf = Vec::new();
    file.read_to_end(&mut buf)?;
    // Everything up to and INCLUDING the final `\n`; a read holding no terminator at all yields
    // nothing, because the whole of it is a line the writer has not finished. The offset advances
    // by exactly this, which is what keeps it landing just past a `\n` for [`anchored`] to check.
    let complete = buf
        .iter()
        .rposition(|byte| *byte == b'\n')
        .map_or(0, |last| last + 1);
    buf.truncate(complete);
    let text = String::from_utf8(buf).map_err(|_| {
        Error::Io(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "the event log is not valid UTF-8",
        ))
    })?;
    Ok((text, offset + complete as u64))
}

/// The one-time stderr preamble: what the follow was asked for, and that it is a follow.
///
/// The window is described as bounding the BACKFILL specifically, because that is the only thing
/// it can bound — see the module docs.
fn follow_header(window: Option<&Window>, event: Option<&str>) -> String {
    let mut header = String::new();
    if let Some(window) = window {
        header.push_str(&format!(
            "window: backfilling events at/after {} (--since {}); newer lines stream as they arrive\n",
            window.cutoff_rfc3339(),
            window.since_arg
        ));
    }
    if let Some(event) = event {
        header.push_str(&format!("filter: event={event}\n"));
    }
    header.push_str("following the event log — press Ctrl-C to stop\n");
    header
}

/// The stderr notice for one transition, given the previous one — empty when there is nothing new
/// to say.
///
/// Pure, so the "say it once" rule is a unit test rather than an observation about a running
/// process. Two rules: the ordinary cycles stay silent (the lines speak for themselves), and
/// [`Transition::Absent`] is STICKY — a cold start can last minutes, and saying so twice a second
/// would bury the lines that eventually arrive.
fn follow_notice(transition: Transition, previous: Option<Transition>) -> String {
    match transition {
        Transition::Idle | Transition::Appended => String::new(),
        Transition::Absent if previous == Some(Transition::Absent) => String::new(),
        Transition::Absent => "no event log yet — waiting for the daemon to create it\n".to_owned(),
        // A plain start renders its backfill immediately, so announcing the attach adds nothing.
        // Announcing the one that ENDS a wait does: it is the answer to the notice above it.
        Transition::Attached if previous.is_none() => String::new(),
        Transition::Attached => "the event log appeared — following it\n".to_owned(),
        Transition::Truncated => {
            "the event log was truncated — resuming from its new start\n".to_owned()
        }
        Transition::Rewritten => {
            "the event log was rewritten — resuming from its new start\n".to_owned()
        }
        Transition::Replaced => {
            "the event log was replaced — reattached at its new start\n".to_owned()
        }
    }
}

/// Render one streamed batch to the DATA stream.
///
/// Text mode delegates to [`render_text`] and keeps only its `out`, so a streamed line and a
/// one-shot line are rendered by the same code and cannot drift; the notice is dropped because a
/// follow states its own ([`follow_notice`]).
///
/// `--json` is the one shape that differs, and it has to: the one-shot view is a single document
/// whose `records: [...]` array must be closed, and a stream has no last element. The follow view
/// is therefore **JSON Lines** — one complete [`FollowRecordWire`] object per line. Each record is
/// serialized whole before any byte of it is written, and a batch reaches the sink as one
/// `write_all` + flush, so a consumer never meets a half-built object.
fn render_follow(view: &LogView, json: bool) -> Result<String> {
    if !json {
        return Ok(render_text(view).out);
    }
    let mut out = String::new();
    for selected in &view.matched {
        let record = FollowRecordWire {
            schema: JSON_SCHEMA_VERSION,
            ts: selected.ts,
            event: selected.event,
            line: selected.line,
        };
        out.push_str(
            &serde_json::to_string(&record)
                .map_err(|_| Error::LogSerialize("a log record was not serializable"))?,
        );
        out.push('\n');
    }
    Ok(out)
}

/// Poll, render, emit — until the downstream closes or `tick` says to stop.
///
/// `tick` is the seam that keeps this loop testable: production passes a closure that sleeps
/// [`POLL_INTERVAL`] and never stops, while a test passes one that mutates the log file and
/// returns [`Flow::Stop`] after a fixed number of cycles. The loop therefore runs in a test
/// exactly as it runs in production, with no clock and no risk of hanging CI.
///
/// `window` is consumed by the first [`Transition::Attached`] batch and is `None` for every batch
/// after it — the backfill/live split the module docs describe, expressed as an ownership move
/// rather than a flag that could be read twice.
///
/// The one-time [`follow_header`] is written HERE rather than by the caller, so that what an
/// operator actually sees on stderr — header first, then transitions in order — is what the tests
/// drive. A header emitted by the wiring above would be a line no test could observe.
fn follow_loop(
    follower: &mut Follower,
    mut window: Option<Window>,
    event: Option<&str>,
    json: bool,
    data: &mut impl Write,
    notice: &mut impl Write,
    mut tick: impl FnMut() -> Flow,
) -> Result<()> {
    write_stream(notice, &follow_header(window.as_ref(), event))?;
    let mut previous = None;
    loop {
        let polled = follower.poll()?;
        let mut rendered = Rendered {
            out: String::new(),
            notice: follow_notice(polled.transition, previous),
        };
        previous = Some(polled.transition);
        if !polled.text.is_empty() {
            let backfill = if polled.transition == Transition::Attached {
                window.take()
            } else {
                None
            };
            let view = select(Some(&polled.text), backfill, event);
            rendered.out = render_follow(&view, json)?;
        }
        // A closed `| head` is the ordinary end of a follow, not a failure — but it must END it,
        // or the loop would re-render into a pipe nobody reads for as long as the daemon runs.
        if emit(&rendered, data, notice)? == Pipe::Closed {
            return Ok(());
        }
        if tick() == Flow::Stop {
            return Ok(());
        }
    }
}

/// Whether `fd`'s downstream has gone away — asked WITHOUT writing to it.
///
/// [`write_stream`] can only discover a dead pipe by writing into it, and a follower's next write
/// is the daemon's next event: tens of seconds away on a quiet fleet, and never at all once the
/// daemon stops. So `sessiometer log --follow | head -3` would leave a process polling a pipe
/// nobody holds open, invisible behind a shell prompt that has already returned. `poll(2)` asks
/// the question directly instead.
///
/// # Why the mask is `POLLOUT` and not empty
///
/// POSIX says `POLLHUP`/`POLLERR`/`POLLNVAL` are reported in `revents` whatever `events` asked
/// for, which would make an EMPTY mask the exact "tell me if it broke, I am not asking to write"
/// query. macOS does not honour that for a pipe's write end — measured on macOS 25.5, an empty
/// mask returns `0` with `revents == 0` whether the reader is live or gone, so an empty-mask probe
/// silently never fires. Asking for `POLLOUT` gets a truthful answer on both platforms: macOS
/// reports `POLLOUT` while a reader holds the pipe and `POLLHUP` (alone, without `POLLOUT`) once
/// it lets go, and Linux reports `POLLERR` — which is why both bits are accepted rather than
/// whichever one this machine happens to raise.
///
/// A full pipe buffer behind a live-but-slow reader reports neither, which reads as "still open" —
/// correct, and the same conservative direction as a `poll` that fails outright. Stopping late
/// costs one cycle; stopping early would drop lines the operator asked for.
///
/// Raw `libc` FFI, kept un-wrapped by ADR-0004: `poll` has no std equivalent, so wrapping it would
/// mean a production dependency the crate's minimalism rejects for one sound POD probe.
fn hung_up(fd: std::os::unix::io::RawFd) -> bool {
    // SAFETY: `pollfd` is plain-old-data we fully initialize and pass as a live one-element
    // array; `poll` writes only into `revents` through that pointer, and the `0` timeout makes
    // the call non-blocking. The same direct-libc idiom as `cli::terminal_cols`'s `TIOCGWINSZ`.
    let mut probe = libc::pollfd {
        fd,
        events: libc::POLLOUT,
        revents: 0,
    };
    let ready = unsafe { libc::poll(&mut probe, 1, 0) };
    ready > 0 && probe.revents & (libc::POLLHUP | libc::POLLERR | libc::POLLNVAL) != 0
}

/// The `--follow` entry point: resolve the window once, state it, then stream until interrupted.
///
/// The window is resolved before the first read ([`resolve_window`]), so a malformed `--since` is
/// rejected before any output — exactly as on the one-shot path.
fn run_follow(args: LogArgs) -> Result<()> {
    let window = resolve_window(args.since.as_deref())?;
    let mut follower = Follower::new(crate::observability::log_path()?);
    let mut data = std::io::stdout().lock();
    let mut notice = std::io::stderr().lock();
    follow_loop(
        &mut follower,
        window,
        args.event.as_deref(),
        args.json,
        &mut data,
        &mut notice,
        || {
            // A BLOCKING sleep, deliberately. The verb is the only work this process performs and
            // the runtime is `current_thread` with nothing else scheduled, so nothing is starved
            // — and keeping the reader synchronous is what lets the loop above be driven from a
            // test by a counter instead of a clock.
            std::thread::sleep(POLL_INTERVAL);
            // Ask whether anyone is still reading, rather than waiting to find out by writing
            // (see [`hung_up`]). Living in the TICK keeps the platform probe out of the loop,
            // which stays generic over `Write` and therefore drivable from a test.
            if hung_up(libc::STDOUT_FILENO) {
                Flow::Stop
            } else {
                Flow::Continue
            }
        },
    )
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

    // ---- `--follow` (issue #774) -------------------------------------------------------------

    /// One scripted change to the followed file, applied BETWEEN two cycles.
    ///
    /// Each variant is a real event the follower must survive, performed the way the real actor
    /// performs it — so a test exercises the actual filesystem transition rather than a
    /// stand-in for it.
    enum Step {
        /// The daemon writing another event.
        Append(&'static str),
        /// Truncated and rewritten IN PLACE — same inode, smaller (or shorter-than-consumed)
        /// file. `std::fs::write` opens with `truncate(true)`, which is exactly that.
        Overwrite(&'static str),
        /// Rotated away and replaced — a NEW inode at the same path.
        Rotate(&'static str),
        /// Deleted, leaving the path empty.
        Remove,
        /// Nothing happened this cycle.
        Idle,
    }

    impl Step {
        fn apply(&self, path: &Path) {
            match self {
                Step::Append(text) => {
                    let mut file = std::fs::OpenOptions::new()
                        .create(true)
                        .append(true)
                        .open(path)
                        .expect("open for append");
                    file.write_all(text.as_bytes()).expect("append");
                }
                Step::Overwrite(text) => std::fs::write(path, text).expect("truncate in place"),
                Step::Rotate(text) => {
                    // What `newsyslog` actually does: move the live log aside, then create a
                    // fresh one. The moved-aside file keeps the OLD inode alive, so the new file
                    // cannot reuse it — the identity change under test is guaranteed by
                    // construction rather than left to a filesystem's allocation luck.
                    let aside = path.with_extension("0");
                    std::fs::rename(path, &aside).expect("rotate aside");
                    std::fs::write(path, text).expect("write the fresh log");
                }
                Step::Remove => std::fs::remove_file(path).expect("remove"),
                Step::Idle => {}
            }
        }
    }

    /// Drive the REAL [`follow_loop`] over a real file, applying one scripted step per cycle.
    ///
    /// Bounded by construction: when the script runs out the tick returns [`Flow::Stop`], so a
    /// follow test cannot hang CI and never sleeps. Because it drives the production loop rather
    /// than a reimplementation of it, the backfill/live window split and the notice sequencing
    /// are exercised as they actually ship.
    ///
    /// A script of N steps runs N+1 cycles — one poll before each step, plus the poll that
    /// observes the last one — so the final step's effect is always in the returned streams.
    /// Returns `(stdout, stderr)`.
    fn drive(
        path: &Path,
        window: Option<Window>,
        event: Option<&str>,
        json: bool,
        script: &[Step],
    ) -> (String, String) {
        let mut follower = Follower::new(path.to_path_buf());
        let (mut data, mut notice) = (Vec::new(), Vec::new());
        let mut steps = script.iter();
        follow_loop(
            &mut follower,
            window,
            event,
            json,
            &mut data,
            &mut notice,
            || match steps.next() {
                Some(step) => {
                    step.apply(path);
                    Flow::Continue
                }
                None => Flow::Stop,
            },
        )
        .expect("the follow loop must not error");
        (
            String::from_utf8(data).expect("stdout is utf-8"),
            String::from_utf8(notice).expect("stderr is utf-8"),
        )
    }

    /// A tempdir plus the path of a log file seeded with `initial` — or, for `None`, a path where
    /// no file exists yet. The `TempDir` is returned so the caller keeps it alive.
    fn log_file(initial: Option<&str>) -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("sessiometer.log");
        if let Some(text) = initial {
            std::fs::write(&path, text).expect("seed the log");
        }
        (dir, path)
    }

    /// AC 1: appended lines are emitted as they arrive, and earlier lines are NOT re-emitted.
    #[test]
    fn follow_emits_new_lines_once_and_never_re_emits_earlier_ones() {
        let (_dir, path) = log_file(Some(FIXTURE_LOG));
        let arriving = "ts=2026-07-11T04:00:00Z event=restash acct=u-B outcome=ok\n";
        let later = "ts=2026-07-11T05:00:00Z event=all_exhausted hold=u-A\n";
        let (out, _) = drive(
            &path,
            None,
            None,
            false,
            &[
                Step::Append(arriving),
                // An idle cycle between the two appends: a follower that re-read from the top
                // each cycle would duplicate everything here, and a follower that advanced its
                // offset on an idle poll would skip the next line.
                Step::Idle,
                Step::Append(later),
            ],
        );
        // The backfill, then each new line exactly once, in arrival order.
        assert_eq!(out, format!("{FIXTURE_LOG}{arriving}{later}"));
        // Stated as a count too, so a duplicate that happened to land in order still fails.
        assert_eq!(out.matches("event=restash acct=u-B").count(), 1);
        assert_eq!(
            out.matches("ts=2026-07-11T00:00:00Z").count(),
            1,
            "a backfilled line must not be re-emitted by a later cycle"
        );
    }

    /// AC 2's substance: a line still being written is never emitted in halves.
    ///
    /// This is the renderer half of AC 2's "no partial line": the follower only ever writes
    /// complete lines, and only complete lines advance its offset, so it never CONSTRUCTS a
    /// half-rendered one. It does not claim the stronger property that no interrupt can ever cut
    /// the byte stream — a signal can land inside the kernel's `write(2)` loop during a large
    /// backfill, as it can for `cat`, and no userspace program can prevent that without blocking
    /// the signal around the write. (The signal itself uses its default disposition: this reader
    /// holds no lock, no temp file, and no open handle to clean up — see [`Follower`].)
    #[test]
    fn follow_emits_only_complete_lines_never_a_partial_one() {
        let (_dir, path) = log_file(Some(""));
        let (out, _) = drive(
            &path,
            None,
            None,
            false,
            &[
                // A writer caught mid-line: no terminator yet.
                Step::Append("ts=2026-07-11T00:00:00Z event=swap from=oleksii@pelykh.com"),
                Step::Idle,
                // …and now it finishes the line and starts another.
                Step::Append(" to=spare reason=session\nts=2026-07-11T01:00:00Z event=rest"),
            ],
        );
        assert_eq!(
            out,
            "ts=2026-07-11T00:00:00Z event=swap from=oleksii@pelykh.com to=spare reason=session\n",
            "only the completed line may be emitted, and only once it is whole"
        );
        // Discriminating: the trailing fragment is held back entirely, not emitted early.
        assert!(
            !out.contains("event=rest"),
            "an unterminated trailing line must not be emitted: {out:?}"
        );
        // And every emitted byte ends a line — never a dangling fragment.
        assert!(out.ends_with('\n'));
    }

    /// AC 3: truncation is detected by the size regression and resumed from the new start.
    #[test]
    fn follow_resumes_from_the_new_start_when_the_log_is_truncated() {
        let (_dir, path) = log_file(Some(FIXTURE_LOG));
        let after = "ts=2026-07-11T06:00:00Z event=swap from=spare to=oleksii@pelykh.com\n";
        let (out, notice) = drive(
            &path,
            None,
            None,
            false,
            // Truncated AND rewritten in one step, which is how an external tool actually does
            // it: the new length is far below the offset already consumed.
            &[Step::Overwrite(after)],
        );
        assert_eq!(
            out,
            format!("{FIXTURE_LOG}{after}"),
            "the backfill, then the truncated file's new content — resumed, not stalled"
        );
        // Not re-emitted from the top: the pre-truncation lines appear exactly once each.
        assert_eq!(out.matches("ts=2026-07-11T00:00:00Z").count(), 1);
        assert!(
            notice.contains("the event log was truncated — resuming from its new start"),
            "the operator must be told why the stream jumped, got {notice:?}"
        );
    }

    /// AC 4: replacement is detected by the inode change and reattached at the new file's start.
    ///
    /// This can only pass because the follower re-resolves the PATH each cycle. A follower
    /// holding an open handle would keep tailing the rotated-away inode and emit nothing here —
    /// so this test is the one that pins that design choice.
    #[test]
    fn follow_reattaches_when_the_log_is_rotated_away_and_replaced() {
        let (_dir, path) = log_file(Some(FIXTURE_LOG));
        let fresh = "ts=2026-07-11T07:00:00Z event=restash acct=u-C outcome=ok\n";
        let (out, notice) = drive(&path, None, None, false, &[Step::Rotate(fresh)]);
        assert_eq!(out, format!("{FIXTURE_LOG}{fresh}"));
        assert!(
            notice.contains("the event log was replaced — reattached at its new start"),
            "a rotation must be named as a rotation, not as a truncation: {notice:?}"
        );
        // Non-degeneracy: the rotation really did change the file's identity, so the assertion
        // above is about the `Replaced` arm rather than an accidental `Truncated` one.
        assert!(!notice.contains("was truncated"));
    }

    /// A same-inode rewrite that leaves the file LONGER is caught, not mistaken for an append.
    ///
    /// Found by adversarial review of the first draft, which classified purely on `(identity,
    /// size)`: a rewrite that grows past the retained offset passes both checks, so the follower
    /// seeked into the middle of the NEW content and emitted a mid-line fragment — a string that
    /// is no durable line at all, which is exactly what CONSTRAINT-A forbids. The anchor-byte
    /// check ([`anchored`]) closes it. The fixture is built so the rewrite is strictly LONGER,
    /// which is the half a size check cannot see.
    #[test]
    fn follow_catches_an_in_place_rewrite_that_grows_the_log() {
        let original = "ts=2026-07-11T00:00:00Z event=swap from=oleksii@pelykh.com to=spare\n";
        let (_dir, path) = log_file(Some(original));
        // Strictly longer than the original, so `len > offset` reads as an append on size alone.
        let rewritten = "ts=2026-07-11T01:00:00Z event=restash acct=u-A outcome=ok padding=xxxxx\n\
                         ts=2026-07-11T02:00:00Z event=all_exhausted hold=spare padding=yyyyyyyy\n";
        assert!(
            rewritten.len() > original.len(),
            "the fixture must grow, or this tests the truncation path instead"
        );

        let (out, notice) = drive(&path, None, None, false, &[Step::Overwrite(rewritten)]);
        assert_eq!(
            out,
            format!("{original}{rewritten}"),
            "a rewrite must resume from the new start, emitting whole lines only"
        );
        // The regression this pins: no emitted line may be a fragment of one.
        let durable = format!("{original}{rewritten}");
        assert!(
            every_line_is_in(&out, &durable),
            "a mid-line fragment reached stdout: {out:?}"
        );
        assert!(
            notice.contains("the event log was rewritten — resuming from its new start"),
            "a rewrite must be named as one, not as an append or a truncation: {notice:?}"
        );

        // And the classification itself, directly — an append must still read as an append, so
        // the new check cannot pass by calling everything a rewrite.
        let (_dir2, path2) = log_file(Some(original));
        let mut follower = Follower::new(path2.clone());
        assert_eq!(
            follower.poll().expect("poll").transition,
            Transition::Attached
        );
        Step::Append("ts=2026-07-11T03:00:00Z event=swap from=spare to=x\n").apply(&path2);
        assert_eq!(
            follower.poll().expect("poll").transition,
            Transition::Appended,
            "a genuine append must not be misread as a rewrite"
        );
    }

    /// AC 5: `--since` bounds the BACKFILL; what arrives afterwards streams.
    #[test]
    fn follow_windows_the_backfill_only_and_streams_what_arrives_after() {
        let (_dir, path) = log_file(Some(FIXTURE_LOG));
        // A line whose `ts=` is OLDER than the cutoff, appended while the follow runs. It must
        // still be emitted: the window is a statement about the log's history, and a line that
        // arrives while you are watching is new whatever its timestamp claims. Silently hiding it
        // would hide a daemon clock bug rather than surface one.
        let stale = "ts=2026-07-11T00:30:00Z event=restash acct=u-D outcome=ok\n";
        let (out, notice) = drive(
            &path,
            Some(window("1h", "2026-07-11T03:00:00Z")),
            None,
            false,
            &[Step::Append(stale)],
        );
        // Backfill: only the two lines at/after the 02:00 cutoff.
        assert!(out.contains("ts=2026-07-11T02:00:00Z"));
        assert!(out.contains("ts=2026-07-11T03:00:00Z"));
        assert!(
            !out.contains("ts=2026-07-11T01:00:00Z"),
            "the backfill must honour the cutoff: {out:?}"
        );
        // …and the live line streams regardless of its timestamp.
        assert!(out.contains("acct=u-D"), "a live line must stream: {out:?}");
        assert!(
            notice
                .contains("window: backfilling events at/after 2026-07-11T02:00:00Z (--since 1h)")
                && notice.contains("newer lines stream as they arrive"),
            "the header must state that the window bounds the backfill only, got {notice:?}"
        );
    }

    /// AC 6 + AC 9 (canary): `--event` keeps filtering STREAMED lines, and can fail.
    ///
    /// The canary is the second append: a non-matching line arrives alongside a matching one, so
    /// a follower that had simply stopped filtering after the backfill would emit it and fail
    /// here. Without that line the test would pass vacuously on any implementation.
    #[test]
    fn follow_event_filter_applies_to_streamed_lines_and_can_reject_one() {
        let (_dir, path) = log_file(Some(FIXTURE_LOG));
        let excluded = "ts=2026-07-11T08:00:00Z event=restash acct=u-E outcome=ok\n";
        let included = "ts=2026-07-11T09:00:00Z event=swap from=spare to=oleksii@pelykh.com\n";
        let (out, notice) = drive(
            &path,
            None,
            Some("swap"),
            false,
            &[Step::Append(excluded), Step::Append(included)],
        );
        assert!(
            !out.contains("acct=u-E"),
            "a streamed non-matching line must be filtered out: {out:?}"
        );
        assert!(
            out.contains("ts=2026-07-11T09:00:00Z"),
            "a streamed matching line must be emitted: {out:?}"
        );
        // Every emitted line is a swap — the backfill's two included.
        assert!(out.lines().all(|line| line.contains("event=swap")));
        assert_eq!(out.lines().count(), 3, "two backfilled swaps plus one live");
        assert!(notice.contains("filter: event=swap"));
    }

    /// AC 7: a follow started before the daemon's first write waits, then picks the log up.
    #[test]
    fn follow_waits_for_a_log_that_does_not_exist_yet_and_attaches_when_it_appears() {
        let (_dir, path) = log_file(None);
        let first = "ts=2026-07-11T00:00:00Z event=swap from=oleksii@pelykh.com to=spare\n";
        let (out, notice) = drive(
            &path,
            None,
            None,
            false,
            &[
                // Two cycles with no file at all — the follower must not exit, and must not say
                // so twice.
                Step::Idle,
                Step::Append(first),
            ],
        );
        assert_eq!(out, first, "the log's first content must be picked up");
        assert_eq!(
            notice.matches("no event log yet").count(),
            1,
            "a sticky cold state must be stated once, not once per poll: {notice:?}"
        );
        assert!(
            notice.contains("the event log appeared — following it"),
            "the wait must be resolved out loud: {notice:?}"
        );
    }

    /// AC 8: `--follow --json` is JSON Lines — every emitted line is a COMPLETE record.
    #[test]
    fn follow_json_emits_one_complete_record_per_line() {
        let (_dir, path) = log_file(Some(FIXTURE_LOG));
        let arriving = "ts=2026-07-11T10:00:00Z event=swap from=spare to=oleksii@pelykh.com\n";
        let (out, _) = drive(&path, None, Some("swap"), true, &[Step::Append(arriving)]);

        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(lines.len(), 3, "two backfilled swaps plus one live");
        for line in &lines {
            // Each line parses ON ITS OWN — the property a stream needs and a single document
            // cannot give. A half-flushed object would fail right here.
            let record: serde_json::Value = serde_json::from_str(line)
                .unwrap_or_else(|err| panic!("{line:?} must parse: {err}"));
            assert_eq!(
                record["schema"], JSON_SCHEMA_VERSION,
                "each record carries its own schema: there is no header to read"
            );
            assert_eq!(record["event"], "swap");
            let durable = record["line"].as_str().expect("line is a string");
            assert!(
                FIXTURE_LOG.lines().any(|d| d == durable) || arriving.trim_end() == durable,
                "each record's line must be a durable line: {record}"
            );
        }
        // Discriminating: this is NOT the one-shot document. A consumer that tried to parse the
        // whole stream as one value must fail, which is what makes the JSON Lines claim real.
        assert!(
            serde_json::from_str::<serde_json::Value>(&out).is_err(),
            "a follow stream must not be a single JSON document"
        );
    }

    /// CONSTRAINT-A under follow: a streamed line is as byte-faithful as a rendered one.
    ///
    /// Issue #773 shipped the exact-stream assertion and its canary for the one-shot path; this
    /// re-asserts both over the streaming path so `--follow` cannot become the hole in it.
    #[test]
    fn follow_stdout_bytes_all_exist_in_the_durable_log() {
        let arriving = "ts=2026-07-11T11:00:00Z event=swap from=spare to=oleksii@pelykh.com\n";
        // Non-degeneracy: both the seed and the streamed line must carry an email, or
        // "interpolates nothing" would be a claim about content that was never at risk.
        assert!(!crate::redaction::meter::unauthored_emails(FIXTURE_LOG, &[]).is_empty());
        assert!(!crate::redaction::meter::unauthored_emails(arriving, &[]).is_empty());

        let (_dir, path) = log_file(Some(FIXTURE_LOG));
        let (out, notice) = drive(&path, None, None, false, &[Step::Append(arriving)]);
        let durable = format!("{FIXTURE_LOG}{arriving}");

        assert!(
            !out.is_empty(),
            "an empty stream would satisfy this vacuously"
        );
        assert!(
            every_line_is_in(&out, &durable),
            "follow stdout carried a line absent from the durable log: {out:?}"
        );
        // Exactness, not just membership: each durable line once, in file order.
        assert_eq!(out, durable);
        // And no reader-authored transition notice leaked into the data stream.
        assert!(
            !out.contains("following the event log") && !out.contains("no event log yet"),
            "operator notices must stay on stderr: {out:?}"
        );
        assert!(notice.contains("following the event log — press Ctrl-C to stop"));
    }

    /// A closed downstream (`… | head -3`) ENDS the follow rather than spinning forever.
    ///
    /// The one-shot path may treat `EPIPE` as plain success because it is already finished; a
    /// follower that did the same would re-render into a dead pipe for as long as the daemon
    /// runs. The tick here NEVER says stop, so only the loop's own `EPIPE` handling can end it —
    /// and a loop that failed to end trips the cycle guard and fails loudly rather than hanging
    /// CI. The failure is unmissable rather than a wrong assertion.
    #[test]
    fn follow_stops_when_the_downstream_closes() {
        let (_dir, path) = log_file(Some(FIXTURE_LOG));
        let mut follower = Follower::new(path.clone());
        let mut cycles = 0usize;
        follow_loop(
            &mut follower,
            None,
            None,
            false,
            &mut FailingSink(std::io::ErrorKind::BrokenPipe),
            &mut Vec::new(),
            || {
                cycles += 1;
                assert!(cycles < 100, "the follow must stop on a closed downstream");
                Flow::Continue
            },
        )
        .expect("a closed downstream is a clean end, not an error");
        assert_eq!(cycles, 0, "it must stop on the very first closed write");
    }

    /// The transition notices: which are said, which stay silent, and which are said once.
    #[test]
    fn follow_notices_are_stated_once_and_only_when_they_add_something() {
        // The ordinary cycles say nothing — the lines already speak.
        for transition in [Transition::Idle, Transition::Appended] {
            assert_eq!(follow_notice(transition, None), "");
            assert_eq!(follow_notice(transition, Some(Transition::Appended)), "");
        }
        // A cold start is STICKY: said on the first poll, silent on every one after it.
        assert!(follow_notice(Transition::Absent, None).contains("no event log yet"));
        assert_eq!(
            follow_notice(Transition::Absent, Some(Transition::Absent)),
            ""
        );
        // …and said again if the log goes away a SECOND time, having existed in between.
        assert!(
            follow_notice(Transition::Absent, Some(Transition::Appended))
                .contains("no event log yet")
        );

        // The first attach renders its backfill, so announcing it adds nothing; the attach that
        // ENDS a wait is the answer to the notice above it, so it is said.
        assert_eq!(follow_notice(Transition::Attached, None), "");
        assert!(
            follow_notice(Transition::Attached, Some(Transition::Absent))
                .contains("the event log appeared")
        );

        // The three recoveries share one mechanic (resume from the new start) but are three
        // different events, so each is named distinctly — an operator watching a stream jump
        // should learn WHICH happened, not just that something did.
        let recoveries: Vec<String> = [
            Transition::Truncated,
            Transition::Rewritten,
            Transition::Replaced,
        ]
        .iter()
        .map(|t| follow_notice(*t, Some(Transition::Appended)))
        .collect();
        assert!(recoveries[0].contains("truncated"));
        assert!(recoveries[1].contains("rewritten"));
        assert!(recoveries[2].contains("replaced"));
        for (i, one) in recoveries.iter().enumerate() {
            for other in &recoveries[i + 1..] {
                assert_ne!(one, other, "each recovery must read differently");
            }
        }
    }

    /// The header states what the follow was asked for — and that the window bounds the backfill.
    #[test]
    fn follow_header_states_the_window_the_filter_and_the_follow() {
        let bare = follow_header(None, None);
        assert_eq!(bare, "following the event log — press Ctrl-C to stop\n");

        let full = follow_header(Some(&window("1h", "2026-07-11T03:00:00Z")), Some("swap"));
        assert!(full.contains("backfilling events at/after 2026-07-11T02:00:00Z (--since 1h)"));
        assert!(full.contains("newer lines stream as they arrive"));
        assert!(full.contains("filter: event=swap"));
        assert!(full.ends_with("following the event log — press Ctrl-C to stop\n"));
    }

    /// The transitions the follower reports, as a sequence — the classification the recovery
    /// arms and the notices both key off.
    #[test]
    fn the_follower_classifies_each_file_transition() {
        let (_dir, path) = log_file(None);
        let mut follower = Follower::new(path.clone());

        // Nothing there yet.
        assert_eq!(
            follower.poll().expect("poll").transition,
            Transition::Absent
        );

        // It appears: attach and read it whole.
        std::fs::write(&path, FIXTURE_LOG).expect("write");
        let attached = follower.poll().expect("poll");
        assert_eq!(attached.transition, Transition::Attached);
        assert_eq!(attached.text, FIXTURE_LOG);

        // Unchanged, then grown.
        assert_eq!(follower.poll().expect("poll").transition, Transition::Idle);
        Step::Append("ts=2026-07-11T04:00:00Z event=swap from=a to=b\n").apply(&path);
        let appended = follower.poll().expect("poll");
        assert_eq!(appended.transition, Transition::Appended);
        assert_eq!(
            appended.text, "ts=2026-07-11T04:00:00Z event=swap from=a to=b\n",
            "an append yields only the new bytes"
        );

        // Truncated in place, then replaced outright.
        Step::Overwrite("ts=2026-07-11T05:00:00Z event=swap from=c to=d\n").apply(&path);
        assert_eq!(
            follower.poll().expect("poll").transition,
            Transition::Truncated
        );
        Step::Rotate("ts=2026-07-11T06:00:00Z event=swap from=e to=f\n").apply(&path);
        let replaced = follower.poll().expect("poll");
        assert_eq!(replaced.transition, Transition::Replaced);
        assert_eq!(
            replaced.text,
            "ts=2026-07-11T06:00:00Z event=swap from=e to=f\n"
        );

        // Deleted: back to absent, and the stale offset is dropped so the NEXT file is read from
        // its own start rather than from a position that meant something in a file now gone.
        Step::Remove.apply(&path);
        assert_eq!(
            follower.poll().expect("poll").transition,
            Transition::Absent
        );
        std::fs::write(&path, FIXTURE_LOG).expect("write");
        let reattached = follower.poll().expect("poll");
        assert_eq!(reattached.transition, Transition::Attached);
        assert_eq!(reattached.text, FIXTURE_LOG);
    }

    /// A follow over a log the writer never touches emits nothing and says nothing beyond the
    /// one-time header — no per-cycle chatter twice a second for as long as it runs.
    #[test]
    fn an_idle_follow_emits_nothing_beyond_its_header() {
        let (_dir, path) = log_file(Some(""));
        let (out, notice) = drive(&path, None, None, false, &[Step::Idle, Step::Idle]);
        assert_eq!(out, "", "an empty log streams nothing");
        assert_eq!(
            notice, "following the event log — press Ctrl-C to stop\n",
            "an attach onto an empty log plus two idle polls must add nothing to the header"
        );
    }

    /// The liveness probe must answer BOTH ways over a real pipe.
    ///
    /// One that always said "open" would leave the very lingering process it exists to stop —
    /// which is the bug that motivated it, found by running `--follow --event swap | head -2`
    /// against a backfill small enough to fit the pipe buffer, so no write ever failed. One that
    /// always said "closed" would truncate every follow after a single cycle. Only a test that
    /// exercises both directions can tell those apart from a correct probe.
    ///
    /// It is also the regression guard for the `events` mask: the first draft passed `0`, which
    /// POSIX says still reports `POLLHUP`, and which macOS answers with a flat `revents == 0` in
    /// BOTH directions — a probe that could never fire. This test is what caught that, and it is
    /// what will catch it again.
    #[test]
    fn hung_up_reports_a_closed_read_end_and_only_that() {
        let mut fds = [0 as libc::c_int; 2];
        // SAFETY: `fds` is a live two-element `c_int` array, which is exactly the out-parameter
        // `pipe(2)` writes the two descriptors into.
        assert_eq!(
            unsafe { libc::pipe(fds.as_mut_ptr()) },
            0,
            "pipe(2) must succeed"
        );
        let (read, write) = (fds[0], fds[1]);

        // A reader is still holding the other end: the downstream is alive.
        assert!(
            !hung_up(write),
            "a pipe with a live reader must not read as hung up"
        );

        // Close the read end — exactly what `| head -3` does once it has had enough.
        // SAFETY: `read` is a descriptor this test owns and never uses again.
        assert_eq!(unsafe { libc::close(read) }, 0);
        assert!(
            hung_up(write),
            "a pipe whose reader is gone must read as hung up"
        );

        // SAFETY: `write` is a descriptor this test owns and never uses again.
        unsafe { libc::close(write) };
    }

    /// Non-UTF-8 in the log is an error, never a lossy substitution that would invent bytes the
    /// durable line never held (CONSTRAINT-A). Matches the one-shot path, where
    /// `read_to_string` already rejects it.
    #[test]
    fn a_non_utf8_log_is_an_error_rather_than_a_lossy_render() {
        let (_dir, path) = log_file(None);
        std::fs::write(&path, b"ts=2026-07-11T00:00:00Z event=swap from=\xff\xfe\n")
            .expect("write");
        let mut follower = Follower::new(path);
        let err = follower
            .poll()
            .expect_err("invalid utf-8 must not be rendered lossily");
        assert!(matches!(err, Error::Io(_)), "got {err:?}");
    }
}
