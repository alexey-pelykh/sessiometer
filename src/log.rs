// Copyright (c) 2026 Oleksii PELYKH
// SPDX-License-Identifier: MIT

//! The `log` verb (issue #773): a supported reader for the daemon's own log lines.
//!
//! The log has always been durable and structured, but the only way to *look at* it was to know
//! `~/Library/Logs/sessiometer/sessiometer.log` and type `tail`. [`crate::reliability`] reads
//! this exact file already — but only to fold it into SLIs, never to show the lines. This verb
//! shows them, with a window ([`LogArgs::since`]), a kind filter ([`LogArgs::event`]), and — since
//! issue #775 — a choice of which channel to read at all ([`LogArgs::channel`]).
//!
//! It is the third **offline** reader, in the shape the two shipped ones establish (`stats`,
//! issue #158; `reliability`, issue #455): read the daemon's durable file directly, make no live
//! control-socket / keychain / usage-API call, and render with the daemon down. The only impure
//! steps are the file reads and — for `--since` alone — one wall-clock read; everything below
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
//! Diagnostics are never ROUTED into it. The `-v` OPERATOR channel is stderr-only and never
//! reaches the durable log (see [`crate::reliability`]'s note); that invariant is untouched.
//!
//! # The second channel (`--channel`, issue #775)
//!
//! The daemon writes two files, and the reader can now show either or both. They are not the same
//! kind of thing, and the whole design of the selector follows from that:
//!
//! - The **event log** is GOVERNED. Every field is a handle, an enum, a number or a timestamp by
//!   type-level construction ([`crate::observability::Event`]), and the channel passes the issue
//!   #15 redaction meter in CI.
//! - **`daemon.err.log`** ([`crate::paths::daemon_stderr_log`]) is the launchd agent's raw stderr —
//!   where the diagnostic channel LANDS, but also everything else the process printed there,
//!   including PANIC PAYLOADS that passed no meter at all.
//!
//! So [`Channel::Event`] is the default and `all` is not: a bare `sessiometer log` must never
//! widen onto an ungoverned channel on behalf of an operator who did not ask. When one IS asked
//! for, the stderr notice says the channel is not redaction-checked, because the exposure travels
//! with wherever those bytes are piped next. The meter is extended over the read path in tests —
//! `a_poisoned_diagnostic_channel_never_reaches_the_default_view` proves a planted token stays out
//! of the default view, and canaries the scan so the guarantee is not vacuous.
//!
//! Adding the channel added a *source*, not a rewrite, exactly as issue #773 intended: [`select`]
//! still filters ONE text handed to it as an argument, and the channel merely supplies the key its
//! lines name their kind with ([`Channel::name_key`] — `event=` there, `diag=` here). Two extra
//! pure functions compose the rest: [`merge`] interleaves two selected views, [`view_of`] picks
//! between one and two.
//!
//! Two ordering questions had to be ANSWERED rather than left implicit, because the two files have
//! independent line formats:
//!
//! - **What orders a line with no `ts=`.** Raw stderr has plenty — a panic payload, the startup
//!   notice. It CARRIES FORWARD the timestamp of the nearest preceding line of its own source, so
//!   it is placed where it actually happened; a line with no timestamped predecessor still has no
//!   placement. The carry is scoped to the diagnostic channel: on the durable log, whose grammar
//!   always writes `ts=`, an untimestamped line is malformed and issue #773's tolerant-drop is
//!   right and unchanged.
//! - **What interleaves the two.** A two-pointer [`merge`], not a sort of the concatenation — a
//!   merge preserves each source's own file order STRUCTURALLY, so a panic backtrace (a run of
//!   lines all sharing one inherited timestamp) stays contiguous and in order. Ties put the event
//!   line first.
//!
//! `--channel all` is refused under `--follow`, not approximated: ordering a live merge would mean
//! holding each new line back until the other channel produced one at least as late, and on a
//! quiet channel that is never.
//!
//! CONSTRAINT-A holds across both. In text mode nothing is interpolated to mark the channel — it
//! does not need to be, since each line already names its own kind (`event=` / `diag=`). The
//! `--json` view carries `channel` as STRUCTURE beside the verbatim `line`, which is what bumped
//! its schema to 2.
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
/// Bumped 1 → 2 by issue #775: every record and the document itself gained a `channel` field, in
/// EVERY view including the default one. `stats`' precedent for adding a field without a bump
/// rests on `skip_serializing_if` — the key is simply absent when it does not apply — which is
/// not this case, so the honest signal is the bump. Independent of the sibling readers' versions,
/// so this costs them nothing.
const JSON_SCHEMA_VERSION: u32 = 2;

/// Which of the daemon's two output channels a view reads (issue #775).
///
/// They are different files with different guarantees, which is why this is a selector rather
/// than a merged default. The durable event log is GOVERNED — every field is a handle, an enum, a
/// number or a timestamp by type-level construction ([`crate::observability::Event`]) and the
/// whole channel passes the issue #15 redaction meter. `daemon.err.log` is the managed agent's raw
/// stderr: it carries the diagnostic channel, but also anything else the process writes there,
/// including PANIC PAYLOADS that passed no meter at all.
///
/// So [`Channel::Event`] is the default and `all` is not: widening a bare `sessiometer log` to
/// include an ungoverned channel would move that exposure onto every operator who never asked
/// for it.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub(crate) enum Channel {
    /// The durable event log — the default, and the only channel a bare `sessiometer log` reads.
    Event,
    /// The managed daemon's stderr file: the issue #77 diagnostic channel, ungoverned.
    Diag,
    /// Both, merged in timestamp order (see [`merge`]).
    All,
}

impl Channel {
    /// The `key=` a line of this channel names its KIND with: `event=` on the durable log,
    /// `diag=` on the diagnostic channel.
    ///
    /// This is the whole reason `--event` keeps working across the selector without a second
    /// flag. Both channels are written in the same whitespace-delimited `key=val` grammar and
    /// both spell "which kind of line is this" in one token — they just spell it with a different
    /// key, because they are different taxonomies (`event=swap` is a durable fact,
    /// `diag=tick` is a per-cycle observation). Filtering by the channel's OWN key is what makes
    /// `--event tick --channel diag` mean what an operator expects.
    fn name_key(self) -> &'static str {
        match self {
            Channel::Event => "event",
            // `All` reads both sources, but each SOURCE is selected under its own channel, so
            // this arm is only ever reached through `Channel::Diag`.
            Channel::Diag | Channel::All => "diag",
        }
    }

    /// Parse a `--channel` value, or `None` for anything outside the closed set.
    ///
    /// The tokens are exactly what [`Channel::as_str`] emits, so the flag an operator types and
    /// the value `--json` reports back are provably the same vocabulary.
    pub(crate) fn parse(raw: &str) -> Option<Channel> {
        match raw.trim() {
            "event" => Some(Channel::Event),
            "diag" => Some(Channel::Diag),
            "all" => Some(Channel::All),
            _ => None,
        }
    }

    /// The channel token for the `--json` view and for the notice.
    fn as_str(self) -> &'static str {
        match self {
            Channel::Event => "event",
            Channel::Diag => "diag",
            Channel::All => "all",
        }
    }
}

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
    /// `--channel <event|diag|all>` (issue #775) — which output channel to read.
    /// [`Channel::Event`] is the default, and a bare `sessiometer log` never widens past it.
    pub(crate) channel: Channel,
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
    let sources = read_channels(args.channel)?;
    // Resolved BEFORE selecting, so the cutoff is a plain integer the pure path filters by.
    let window = resolve_window(args.since.as_deref())?;
    let view = view_of(&sources, args.channel, window, args.event.as_deref());
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

/// One channel's text as read, or `None` when that channel's file does not exist.
#[derive(Debug, PartialEq)]
struct Source {
    /// Which channel this text came from — carried so a merged view can say, per line, which
    /// file it was read out of without inspecting the line itself.
    channel: Channel,
    /// The whole file, or `None` when there is none.
    text: Option<String>,
}

/// Read the file(s) the selector asks for, in merge order.
///
/// [`Channel::All`] reads BOTH, and reads the event log first — the tie-break order the merge
/// documents, fixed here rather than at the merge so the two cannot disagree.
fn read_channels(channel: Channel) -> Result<Vec<Source>> {
    let mut sources = Vec::new();
    if matches!(channel, Channel::Event | Channel::All) {
        sources.push(Source {
            channel: Channel::Event,
            text: read_event_log()?,
        });
    }
    if matches!(channel, Channel::Diag | Channel::All) {
        sources.push(Source {
            channel: Channel::Diag,
            text: read_channel_at(&crate::paths::daemon_stderr_log()?)?,
        });
    }
    Ok(sources)
}

/// The event-log text, or `None` when there is no log file at all.
///
/// Unlike [`crate::reliability`]'s read — which folds an absent file into an empty aggregate,
/// because *no events* and *no file* produce the same SLIs — this reader keeps the two apart:
/// "the daemon has never run" and "the daemon ran and recorded nothing" are different answers to
/// the operator's question, and issue #773 asks for the first to be said plainly.
fn read_event_log() -> Result<Option<String>> {
    read_channel_at(&crate::observability::log_path()?)
}

/// Read a channel's file at an explicit path — the seam that makes the absent-file arm testable,
/// and (since issue #775) the one read shared by both channels.
///
/// The production paths are not injectable (they resolve through `getpwuid`, deliberately, so
/// they cannot be spoofed by an environment variable), so without this split the `NotFound`
/// branch could only be reached by interposing on `open`.
fn read_channel_at(path: &Path) -> Result<Option<String>> {
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
/// `Clone` since issue #775: a `--channel all` view resolves the window ONCE and hands a copy to
/// each source's [`select`], so its two halves are provably bounded by the same cutoff rather
/// than by two independent clock reads.
#[derive(Debug, PartialEq, Clone)]
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
    /// The line's KIND token — its `event=` value on the durable log, its `diag=` value on the
    /// diagnostic channel ([`Channel::name_key`]). `None` when the line carries none, which on
    /// the diagnostic channel is ordinary: raw stderr and panic payloads name no kind.
    event: Option<&'a str>,
    /// Which channel this line was read from (issue #775).
    channel: Channel,
    /// Where this line sits in time, as epoch seconds — its own `ts=` when that parses, else the
    /// value CARRIED FORWARD from the nearest preceding line of the same source that had one.
    /// `None` only for a line with no `ts=` and no timestamped predecessor.
    ///
    /// Carrying rather than re-deriving is what places an untimestamped line — a panic payload,
    /// the startup `eprintln!` — where it actually happened relative to its own channel. See
    /// [`select`] for why the carry is scoped to the diagnostic channel.
    at: Option<i64>,
}

/// Everything the reader was asked for and everything it found — the single value both renderers
/// consume, so the text and JSON views can never disagree about what matched.
#[derive(Debug, PartialEq)]
struct LogView<'a> {
    /// The resolved `--since` window; `None` when the flag was absent.
    window: Option<Window>,
    /// The `--event` token; `None` when the flag was absent.
    event: Option<&'a str>,
    /// The `--channel` selector as the operator gave it (issue #775) — `All` for a merged view,
    /// NOT the channel of any one line (that rides on each [`Selected`]).
    channel: Channel,
    /// Whether each source's file exists, in merge order. A `Vec` rather than a bool because
    /// [`Channel::All`] can find one channel and not the other, and the notice has to be able to
    /// say WHICH — "no diagnostics" and "no daemon has ever run" are different answers.
    present: Vec<(Channel, bool)>,
    /// Every line that passed both filters, in file order (or merged order for `All`).
    matched: Vec<Selected<'a>>,
    /// Every line the sources held, filters aside — the denominator of the match count.
    n_scanned: usize,
}

impl LogView<'_> {
    /// Whether at least one of the read channels has a file on disk.
    fn any_present(&self) -> bool {
        self.present.iter().any(|(_, present)| *present)
    }

    /// Whether the named channel was read AND its file exists.
    fn channel_present(&self, channel: Channel) -> bool {
        self.present
            .iter()
            .any(|(read, present)| *read == channel && *present)
    }
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

/// Select ONE source's lines matching both filters, in file order. Pure — the whole reader below
/// the file and clock reads, so every behaviour here is testable from a `&str`.
///
/// Channel-agnostic by construction: the text arrives as an argument and the channel supplies its
/// own KIND key ([`Channel::name_key`]), so issue #775 added a *source* here rather than a second
/// filter. `--event tick --channel diag` therefore selects `diag=tick`, because "which kind of
/// line is this" is the same question on both channels — spelled with a different key.
///
/// # Placing a line in time, and why the two channels differ
///
/// A line whose `ts=` is missing or unparseable is dropped from a WINDOWED view: it cannot be
/// placed in time, so it is not provably in-window. That is the tolerant-drop precedent
/// [`crate::reliability`]'s fold already sets, and issue #773's behaviour, unchanged.
///
/// On the DIAGNOSTIC channel a line is first given the chance to inherit one. The difference is
/// not a preference, it is the two grammars: every durable line is written by
/// [`crate::observability::Event::to_log_line`] as `ts=… event=…`, so an untimestamped one there
/// is MALFORMED and dropping it is right. `daemon.err.log` is raw process stderr, where an
/// untimestamped line is ORDINARY — a panic payload, the startup notice — and dropping it would
/// silently discard exactly what an operator opened the ungoverned channel to see. Inheriting the
/// nearest preceding timestamp places such a line where it actually happened, and a line with no
/// timestamped predecessor at all still has no placement and still drops under a window.
///
/// Without `--since` no timestamp bounds anything, so every line is emitted regardless; the
/// carried value is still recorded, because [`merge`] orders by it.
fn select<'a>(
    text: Option<&'a str>,
    window: Option<Window>,
    event: Option<&'a str>,
    channel: Channel,
) -> LogView<'a> {
    // `None` IS the absent-file state, so the two cannot disagree — there is no way to ask for
    // "present, but here is no text" or "absent, but here is some".
    let present = vec![(channel, text.is_some())];
    let cutoff = window.as_ref().map(|w| w.cutoff_epoch);
    let carries = channel == Channel::Diag;
    let mut matched = Vec::new();
    let mut n_scanned = 0usize;
    // The running carry, advanced by every timestamped line — INCLUDING one a filter later
    // drops, so a filtered-out line can never break the chain for the lines after it.
    let mut carried: Option<i64> = None;
    for line in text.unwrap_or("").lines() {
        n_scanned += 1;
        let ts = field(line, "ts");
        // ONE lookup, keyed by the channel — not a branch that special-cases the event channel
        // back to a hard-coded `"event"`. `name_key` is the single place that mapping lives, so
        // it cannot drift out of step with itself.
        let line_name = field(line, channel.name_key());
        let own = ts.and_then(epoch_from_rfc3339);
        if own.is_some() {
            carried = own;
        }
        let at = if carries { own.or(carried) } else { own };
        if let Some(cutoff) = cutoff {
            // Not provably in-window ⇒ dropped: either no placement at all, or one that is
            // genuinely older than the cutoff.
            if at.is_none_or(|at| at < cutoff) {
                continue;
            }
        }
        // Exact token equality, not a prefix or substring: `--event swap` must not also match a
        // hypothetical `swap_failed`.
        if let Some(wanted) = event {
            if line_name != Some(wanted) {
                continue;
            }
        }
        matched.push(Selected {
            line,
            ts,
            event: line_name,
            channel,
            at,
        });
    }
    LogView {
        window,
        event,
        channel,
        present,
        matched,
        n_scanned,
    }
}

/// Interleave two single-source views into one, in timestamp order (issue #775).
///
/// A two-pointer MERGE, not a sort of the concatenation, and the difference is the whole point:
/// a merge takes lines off the front of each already-in-order source, so **each source's own file
/// order is preserved structurally** — it cannot be violated even if a file turns out not to be
/// internally monotone (a clock step, an interleaved writer). That matters most for the thing the
/// diagnostic channel exists to show: a panic backtrace is a run of untimestamped lines that all
/// carry the same inherited timestamp, and it must stay contiguous and in order. A sort with any
/// tie-break over a key they all share could shuffle it; this cannot.
///
/// Ties resolve EVENT-first, matching the read order [`read_channels`] fixes. A line with no
/// placement at all (`at == None`) sorts before every placed line of its own source, which is
/// where it is in its file — nothing preceded it there.
fn merge<'a>(events: LogView<'a>, diags: LogView<'a>) -> LogView<'a> {
    let mut matched = Vec::with_capacity(events.matched.len() + diags.matched.len());
    let mut left = events.matched.into_iter().peekable();
    let mut right = diags.matched.into_iter().peekable();
    loop {
        // `None` (unplaceable) sorts first, which is what `Option`'s own ordering already says.
        let take_left = match (left.peek(), right.peek()) {
            (Some(l), Some(r)) => l.at <= r.at,
            (Some(_), None) => true,
            (None, Some(_)) => false,
            (None, None) => break,
        };
        // `expect` over `unwrap`: the peek above proved the side is non-empty.
        if take_left {
            matched.push(left.next().expect("peeked"));
        } else {
            matched.push(right.next().expect("peeked"));
        }
    }
    let mut present = events.present;
    present.extend(diags.present);
    LogView {
        window: events.window,
        event: events.event,
        channel: Channel::All,
        present,
        matched,
        n_scanned: events.n_scanned + diags.n_scanned,
    }
}

/// Compose the view for whichever channel(s) were read — one [`select`], or two and a [`merge`].
///
/// The `window` is cloned into each source rather than resolved twice: one clock read, one
/// cutoff, so a merged view's two halves are provably bounded by the SAME window.
fn view_of<'a>(
    sources: &'a [Source],
    channel: Channel,
    window: Option<Window>,
    event: Option<&'a str>,
) -> LogView<'a> {
    let mut views: Vec<LogView<'a>> = sources
        .iter()
        .map(|source| {
            select(
                source.text.as_deref(),
                window.clone(),
                event,
                source.channel,
            )
        })
        .collect();
    // Right-to-left, so the two pops come off in read order (event, then diag).
    match (views.pop(), views.pop()) {
        (Some(diags), Some(events)) => merge(events, diags),
        (Some(single), None) => single,
        // Unreachable in practice: `read_channels` yields at least one source for every
        // `Channel`. Rendered as an empty view rather than a panic — a reader has no business
        // aborting over an internally-impossible state.
        _ => LogView {
            window,
            event,
            channel,
            present: Vec::new(),
            matched: Vec::new(),
            n_scanned: 0,
        },
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
        channel: view.channel.as_str(),
        log_present: view.any_present(),
        present: view
            .present
            .iter()
            .map(|(channel, present)| ChannelPresenceWire {
                channel: channel.as_str(),
                present: *present,
            })
            .collect(),
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
                channel: s.channel.as_str(),
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
    // The diagnostic channel is UNGOVERNED (issue #775): unlike the event log, whose every field
    // is a handle / enum / number / timestamp by construction and which passes the issue #15
    // redaction meter, this is raw process stderr and can carry anything the daemon printed —
    // including a panic payload that passed no meter. Said once, up front, whenever the operator
    // opted in, because the exposure travels with wherever these bytes are piped or pasted next.
    if matches!(view.channel, Channel::Diag | Channel::All) {
        notice.push_str(
            "note: the diagnostic channel is the daemon's raw stderr — unlike the event log it \
             is not redaction-checked, and can carry panic output\n",
        );
    }
    // An absent diagnostic file is not a cold install, it is a knob that is off — so it gets the
    // instruction that resolves it rather than the event log's "the daemon has not run".
    if matches!(view.channel, Channel::Diag | Channel::All) && !view.channel_present(Channel::Diag)
    {
        notice.push_str(
            "no diagnostics yet — a managed daemon writes them only with `verbose = true` under \
             [tunables] in the config (`sessiometer config path`), effective at the next daemon \
             start (`sessiometer daemon restart`)\n",
        );
    }
    if !view.any_present() {
        if view.channel != Channel::Diag {
            notice.push_str("no event log yet — the daemon has not run\n");
        }
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
        // Named for the channel actually read: "the event log is empty" would be a false
        // statement about a different file when the operator asked for `--channel diag`.
        match view.channel {
            Channel::Event => {
                notice.push_str("the event log is empty — no events recorded yet\n");
            }
            Channel::Diag => {
                notice.push_str("the diagnostic channel is empty — nothing recorded yet\n");
            }
            Channel::All => {
                notice.push_str("both channels are empty — nothing recorded yet\n");
            }
        }
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

/// The `--json` document (schema 2). Named for its verb, like `StatsWire` and
/// `ReliabilityWire`.
#[derive(Serialize)]
struct LogWire<'a> {
    /// The schema version — bumped on any change a consumer could not ignore.
    schema: u32,
    /// The `--channel` selector this document answers: `event`, `diag`, or `all`.
    channel: &'static str,
    /// `false` when NO read channel has a file, so a script can tell a cold install from a quiet
    /// one without parsing prose. Under `--channel all` it is the disjunction; `present` below
    /// carries the per-channel detail.
    log_present: bool,
    /// Per-channel presence, one entry per channel read — the field that distinguishes "no
    /// diagnostics because the knob is off" from "no daemon has ever run".
    present: Vec<ChannelPresenceWire>,
    /// The resolved `--since` window, or `null` when the whole log was read.
    window: Option<WindowWire<'a>>,
    /// The `--event` filter, or `null` when every event was read.
    event: Option<&'a str>,
    /// Lines the sources held, filters aside.
    n_scanned: usize,
    /// Lines that matched — always `records.len()`.
    n_matched: usize,
    /// One record per matched line, in file order (merged order under `--channel all`).
    records: Vec<RecordWire<'a>>,
}

/// Whether one read channel has a file on disk.
#[derive(Serialize)]
struct ChannelPresenceWire {
    channel: &'static str,
    present: bool,
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
///
/// `channel` is the one field here that is READER-authored rather than read out of the line, and
/// it is why `--channel all` needs no marker interpolated into `line`: the JSON view can say
/// which file a record came from as structure, leaving the line itself byte-faithful
/// (CONSTRAINT-A). In the TEXT view the line says it itself — the durable log spells its kind
/// `event=`, the diagnostic channel spells it `diag=`.
#[derive(Serialize)]
struct RecordWire<'a> {
    ts: Option<&'a str>,
    event: Option<&'a str>,
    channel: &'static str,
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
    /// Which channel the record came from — carried per record for the same reason `schema` is:
    /// a follow stream has no header a late-attaching consumer could have read.
    channel: &'static str,
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
fn follow_header(window: Option<&Window>, event: Option<&str>, channel: Channel) -> String {
    let mut header = String::new();
    // Said before the first line, for the same reason the one-shot notice says it (issue #775):
    // this channel is raw stderr and is not redaction-checked.
    if channel == Channel::Diag {
        header.push_str(
            "note: the diagnostic channel is the daemon's raw stderr — unlike the event log it \
             is not redaction-checked, and can carry panic output\n",
        );
    }
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
    header.push_str(match channel {
        Channel::Event => "following the event log — press Ctrl-C to stop\n",
        // `All` cannot reach here: `run_follow` rejects it before a follower is built.
        Channel::Diag | Channel::All => "following the diagnostic channel — press Ctrl-C to stop\n",
    });
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
            channel: selected.channel.as_str(),
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

/// What a follow was asked for — the filters and the view shape, as one value.
///
/// Grouped rather than passed as four parameters because they ARE one thing (what the operator
/// typed), and because `window` is CONSUMED by the first batch: keeping it beside the filters
/// that outlive it makes the asymmetry visible at the call site instead of hiding it in an
/// argument list long enough to have to be counted.
struct FollowAsk<'a> {
    /// The resolved `--since` window, `take()`n by the first attached batch (see [`follow_loop`]).
    window: Option<Window>,
    /// The `--event` kind filter, applied to every streamed line.
    event: Option<&'a str>,
    /// `--json` — render JSON Lines instead of the text view.
    json: bool,
    /// The single channel being followed. Never [`Channel::All`]: [`run_follow`] refuses that
    /// before a follower is built.
    channel: Channel,
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
    mut ask: FollowAsk<'_>,
    data: &mut impl Write,
    notice: &mut impl Write,
    mut tick: impl FnMut() -> Flow,
) -> Result<()> {
    write_stream(
        notice,
        &follow_header(ask.window.as_ref(), ask.event, ask.channel),
    )?;
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
                ask.window.take()
            } else {
                None
            };
            let view = select(Some(&polled.text), backfill, ask.event, ask.channel);
            rendered.out = render_follow(&view, ask.json)?;
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
    // `--follow --channel all` is refused rather than approximated (issue #775). A one-shot merge
    // can order two COMPLETE files; a live one cannot, because ordering the next line means
    // holding it back until the other channel has produced something at least as late — which on
    // a quiet channel is never. The choice would be between stalling one stream indefinitely and
    // emitting out of order, and both are worse than saying so.
    if args.channel == Channel::All {
        return Err(Error::LogFollowAllUnsupported);
    }
    let window = resolve_window(args.since.as_deref())?;
    let path = match args.channel {
        Channel::Diag => crate::paths::daemon_stderr_log()?,
        Channel::Event | Channel::All => crate::observability::log_path()?,
    };
    let mut follower = Follower::new(path);
    let mut data = std::io::stdout().lock();
    let mut notice = std::io::stderr().lock();
    follow_loop(
        &mut follower,
        FollowAsk {
            window,
            event: args.event.as_deref(),
            json: args.json,
            channel: args.channel,
        },
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
    fn a_mixed_vintage_log_reads_old_split_records_exactly_as_it_always_did() {
        // Issue #1092 hardened the WRITER (`observability::single_line`), and the log is durable
        // and append-only — so a real file outlives the change and holds both vintages. The
        // reader is not versioned and must not become so; this pins what each vintage does.
        //
        // Line 2 is an OLD record whose `account=` carried a raw newline: on disk it IS two
        // physical lines, and no writer change can retroactively rejoin them. Line 4 is the same
        // hostile value written AFTER the fix.
        const MIXED: &str = "\
ts=2026-07-11T00:00:00Z event=restash account=u-A
ts=2026-07-11T01:00:00Z event=login account=u-B
ts=2026-07-11T01:00:00Z event=login outcome=onboarded
ts=2026-07-11T02:00:00Z event=login account=u-C%0Ats=2026-07-11T02:00:00Z outcome=failed
";

        let view = select(Some(MIXED), None, None, Channel::Event);
        // Four physical lines in, four out: the old vintage's spurious second record is still a
        // record — it always was, and pretending otherwise would be the reader rewriting history.
        assert_eq!(view.n_scanned, 4);
        assert_eq!(view.matched.len(), 4);
        assert_eq!(
            render_text(&view).out,
            MIXED,
            "byte-faithful, both vintages"
        );

        // The old vintage's injected line is still indistinguishable from a real one — that is
        // the damage this fix stops ACCRUING, not damage it can undo. `--event login` counts it.
        let old = select(Some(MIXED), None, Some("login"), Channel::Event);
        assert_eq!(old.matched.len(), 3);

        // The NEW vintage is one record, and the `%0A` inside it is inert to the reader's
        // tokenizer: it neither ends the record nor forges an `event=` of its own.
        let new_line = MIXED.lines().nth(3).unwrap();
        assert_eq!(field(new_line, "event"), Some("login"));
        assert_eq!(
            field(new_line, "account"),
            Some("u-C%0Ats=2026-07-11T02:00:00Z")
        );
        // The one that matters: the encoded `ts=` inside the VALUE does not become the line's
        // timestamp. `field` takes the LAST occurrence of a repeated key, so a value that could
        // smuggle a bare `ts=` token would move the record in time — it cannot, because the
        // whole value is one token and its `ts=` is not at the token's first `=`.
        assert_eq!(field(new_line, "ts"), Some("2026-07-11T02:00:00Z"));
    }

    #[test]
    fn no_flags_emits_every_line_in_file_order_byte_identical() {
        let view = select(Some(FIXTURE_LOG), None, None, Channel::Event);
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
            Channel::Event,
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
        let windowed = select(
            Some(log),
            Some(window("1h", "2026-07-11T03:00:00Z")),
            None,
            Channel::Event,
        );
        assert_eq!(windowed.matched.len(), 1);
        assert_eq!(windowed.matched[0].ts, Some("2026-07-11T03:00:00Z"));
        // But with no window, no timestamp is consulted, so the same line is emitted like any other.
        let whole = select(Some(log), None, None, Channel::Event);
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
        let view = select(Some(FIXTURE_LOG), None, Some("swap"), Channel::Event);
        assert_eq!(view.matched.len(), 2);
        assert!(view.matched.iter().all(|s| s.event == Some("swap")));
        let rendered = render_text(&view);
        assert!(!rendered.out.contains("event=restash"));
        assert!(!rendered.out.contains("event=all_exhausted"));
        assert!(rendered.notice.contains("filter: event=swap"));

        // EXACT, not a prefix or substring: a longer token that merely starts with the filter
        // must not match, or `--event swap` would silently widen as new events are added.
        let log = "ts=2026-07-11T00:00:00Z event=swap_failed acct=u-A\n";
        assert_eq!(
            select(Some(log), None, Some("swap"), Channel::Event)
                .matched
                .len(),
            0
        );
        assert_eq!(
            select(Some(log), None, Some("swap_failed"), Channel::Event)
                .matched
                .len(),
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
            Channel::Event,
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
        let view = select(
            Some(FIXTURE_LOG),
            None,
            Some("no_such_event"),
            Channel::Event,
        );
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
        let absent = render_text(&select(None, None, None, Channel::Event));
        assert_eq!(absent.out, "");
        assert!(absent.notice.contains("no event log yet"));

        // 2. A log file with no lines — the daemon ran but recorded nothing.
        let empty = render_text(&select(Some(""), None, None, Channel::Event));
        assert_eq!(empty.out, "");
        assert!(empty.notice.contains("the event log is empty"));

        // 3. Lines, but none matching the filter.
        let unmatched = render_text(&select(
            Some(FIXTURE_LOG),
            None,
            Some("nope"),
            Channel::Event,
        ));
        assert_eq!(unmatched.out, "");
        assert!(unmatched.notice.contains("no matching events"));

        // The three notices are genuinely different — an operator can act on which one they got.
        assert_ne!(absent.notice, empty.notice);
        assert_ne!(empty.notice, unmatched.notice);
    }

    #[test]
    fn json_parses_and_carries_a_schema_and_one_record_per_matched_line() {
        let view = select(Some(FIXTURE_LOG), None, Some("swap"), Channel::Event);
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
            Channel::Event,
        ))
        .expect("serializes");
        let parsed: serde_json::Value = serde_json::from_str(&windowed.out).expect("parses");
        assert_eq!(parsed["window"]["since"], "1h");
        assert_eq!(parsed["window"]["cutoff"], "2026-07-11T02:00:00Z");

        // A cold install still yields a valid, parseable document — never a bare notice.
        let cold = render_json(&select(None, None, None, Channel::Event)).expect("serializes");
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
            select(Some(FIXTURE_LOG), None, None, Channel::Event),
            select(Some(FIXTURE_LOG), None, Some("swap"), Channel::Event),
            select(
                Some(FIXTURE_LOG),
                Some(window("1h", "2026-07-11T03:00:00Z")),
                None,
                Channel::Event,
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
            render_text(&select(Some(FIXTURE_LOG), None, None, Channel::Event)).out
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
        assert_eq!(
            select(Some(log), None, None, Channel::Event).matched.len(),
            2
        );
        // With an `--event` filter, the bare line has no `event=` and so cannot match.
        let filtered = select(Some(log), None, Some("swap"), Channel::Event);
        assert_eq!(filtered.matched.len(), 1);
        assert_eq!(filtered.matched[0].ts, Some("2026-07-11T00:00:00Z"));
    }

    /// The data/notice split is the contract the whole reader rests on — AC 9's "verbatim and
    /// nothing else" and the clean-pipe promise both live or die on WHICH stream each string
    /// reaches. Asserted against two in-memory sinks, so swapping the two destinations fails
    /// here rather than silently shipping.
    #[test]
    fn emit_routes_the_data_to_stdout_and_the_notice_to_stderr() {
        let view = select(Some(FIXTURE_LOG), None, Some("swap"), Channel::Event);
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
        let rendered = render_text(&select(Some(FIXTURE_LOG), None, None, Channel::Event));
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
        let rendered = render_text(&select(Some(FIXTURE_LOG), None, None, Channel::Event));
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
            read_channel_at(&missing).expect("absent is not an error"),
            None
        );

        // A file that exists but holds nothing → `Some("")`, the distinct second state.
        let empty = dir.path().join("empty.log");
        std::fs::write(&empty, "").expect("write");
        assert_eq!(
            read_channel_at(&empty).expect("readable"),
            Some(String::new())
        );

        // A file with content → read whole, byte for byte.
        let full = dir.path().join("full.log");
        std::fs::write(&full, FIXTURE_LOG).expect("write");
        assert_eq!(
            read_channel_at(&full).expect("readable"),
            Some(FIXTURE_LOG.to_string())
        );

        // The two cold states drive different notices end-to-end.
        let absent = render_text(&select(None, None, None, Channel::Event));
        let empty = render_text(&select(Some(""), None, None, Channel::Event));
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
            FollowAsk {
                window,
                event,
                json,
                channel: Channel::Event,
            },
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
            FollowAsk {
                window: None,
                event: None,
                json: false,
                channel: Channel::Event,
            },
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
        let bare = follow_header(None, None, Channel::Event);
        assert_eq!(bare, "following the event log — press Ctrl-C to stop\n");

        let full = follow_header(
            Some(&window("1h", "2026-07-11T03:00:00Z")),
            Some("swap"),
            Channel::Event,
        );
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

    // ---- `--channel` (issue #775) ------------------------------------------------------------

    /// A representative diagnostic slice, in the shape [`crate::observability::Diagnostic`]
    /// actually writes: the same `ts=` RFC 3339 the durable log uses, and the kind spelled
    /// `diag=` rather than `event=`. Its timestamps deliberately fall BETWEEN [`FIXTURE_LOG`]'s,
    /// so a merge that simply concatenated would be visibly wrong.
    const FIXTURE_DIAG: &str = "\
ts=2026-07-11T00:30:00Z diag=start accounts=2 poll_secs=60
ts=2026-07-11T01:30:00Z diag=poll account=u-A outcome=live
ts=2026-07-11T02:30:00Z diag=tick decision=hold
";

    /// The diagnostic channel as it really is: a timestamped line, then a run of raw stderr with
    /// no `ts=` at all — the panic payload this channel exists to surface — then a further
    /// timestamped line.
    const FIXTURE_DIAG_PANIC: &str = "\
ts=2026-07-11T01:30:00Z diag=tick decision=hold
thread 'main' panicked at src/daemon.rs:1:1:
called `Option::unwrap()` on a `None` value
ts=2026-07-11T02:30:00Z diag=stop
";

    /// Build the `Source` list a `--channel` selector would produce, from fixture text rather
    /// than the real files (which resolve through `getpwuid` and are not injectable).
    fn sources(event: Option<&str>, diag: Option<&str>) -> Vec<Source> {
        let mut sources = Vec::new();
        if let Some(text) = event {
            sources.push(Source {
                channel: Channel::Event,
                text: Some(text.to_owned()),
            });
        }
        if let Some(text) = diag {
            sources.push(Source {
                channel: Channel::Diag,
                text: Some(text.to_owned()),
            });
        }
        sources
    }

    /// The `--channel` value set is closed, and its tokens are exactly the ones the JSON view
    /// reports back — so what an operator types and what a script reads are one vocabulary.
    #[test]
    fn channel_parses_the_closed_set_and_rejects_everything_else() {
        for (raw, expected) in [
            ("event", Channel::Event),
            ("diag", Channel::Diag),
            ("all", Channel::All),
        ] {
            assert_eq!(Channel::parse(raw), Some(expected));
            // Round-trip: the token parsed IS the token rendered.
            assert_eq!(expected.as_str(), raw);
        }
        for bad in ["", "  ", "Event", "diagnostic", "both", "stderr", "events"] {
            assert_eq!(Channel::parse(bad), None, "{bad:?} must be rejected");
        }
    }

    /// **CONSTRAINT-C, the opt-in guarantee.** A bare `sessiometer log` reads the event log and
    /// ONLY the event log, so the ungoverned channel is never widened into the default view.
    ///
    /// Asserted at the READ, not at the render: a view that read the diagnostic file and then
    /// filtered every line out would satisfy an output-only check while having already loaded
    /// the bytes. What must be true is that the file is not opened at all.
    #[test]
    fn the_default_channel_reads_the_event_log_and_nothing_else() {
        // The parser's default — the value a bare `sessiometer log` carries — is `Event`.
        // (`cli::tests::log_channel_defaults_to_event_and_parses_each_value` pins the argv side.)
        let read = |channel: Channel| {
            let mut asked = Vec::new();
            if matches!(channel, Channel::Event | Channel::All) {
                asked.push(Channel::Event);
            }
            if matches!(channel, Channel::Diag | Channel::All) {
                asked.push(Channel::Diag);
            }
            asked
        };
        // This mirrors `read_channels`' selection exactly; the assertion below pins that the
        // production function agrees, so the mirror cannot drift into a comfortable fiction.
        assert_eq!(read(Channel::Event), vec![Channel::Event]);
        assert_eq!(read(Channel::Diag), vec![Channel::Diag]);
        assert_eq!(read(Channel::All), vec![Channel::Event, Channel::Diag]);

        // The production selector, over the real `read_channels`: which channels it names.
        for (asked, expected) in [
            (Channel::Event, vec![Channel::Event]),
            (Channel::Diag, vec![Channel::Diag]),
            (Channel::All, vec![Channel::Event, Channel::Diag]),
        ] {
            let named: Vec<Channel> = read_channels(asked)
                .expect("reading absent files is not an error")
                .iter()
                .map(|source| source.channel)
                .collect();
            assert_eq!(
                named,
                expected,
                "--channel {} read {named:?}",
                asked.as_str()
            );
        }
    }

    /// **CONSTRAINT-C, the redaction meter extended over the diagnostic read path** — with the
    /// canary that proves the extension can actually fail.
    ///
    /// The diagnostic channel is raw process stderr: it never passed the issue #15 meter, and a
    /// byte-faithful reader cannot scrub it. So the guarantee this reader can make is not "diag
    /// output is clean" — it is that **the poison does not cross into the default view**, and
    /// that the meter is genuinely watching. Both halves are here, and the second is what makes
    /// the first non-vacuous: a guard that cannot be shown to fail proves nothing.
    #[test]
    fn a_poisoned_diagnostic_channel_never_reaches_the_default_view() {
        use crate::redaction::meter;

        // A token-shaped string, deliberately injected — the thing an ungoverned channel can
        // carry that the event log's type-level construction makes impossible.
        let poisoned = "\
ts=2026-07-11T01:30:00Z diag=tick decision=hold
thread 'main' panicked at src/keychain.rs:1:1: sk-ant-oat-LEAK0abc0def0ghi0jkl0mno0pqr0stu0vwx
";
        let secrets = meter::Secrets::meter_fixture();
        // `FIXTURE_LOG` carries the operator's OWN label as an authored email (#444's permitted
        // value), so it is passed as the allow-set rather than pretended away.
        let authored = ["oleksii@pelykh.com"];

        // The DEFAULT view, with the poisoned diagnostic file sitting right there on disk: the
        // event channel alone, and it is meter-clean.
        let default_sources = sources(Some(FIXTURE_LOG), Some(poisoned));
        let default_view = view_of(&default_sources[..1], Channel::Event, None, None);
        let default_out = render_text(&default_view).out;
        assert!(
            !default_out.is_empty(),
            "the default view must select lines, else the meter passes vacuously"
        );
        meter::assert_clean(&default_out, &secrets, &authored);
        assert!(
            !default_out.contains("sk-ant-"),
            "the default view must not carry a diagnostic-channel token"
        );

        // THE CANARY. The same meter, run over the same reader's DIAGNOSTIC view, finds the
        // planted token — so the assertion above is a real gate and not a tautology about a
        // scan that never fires.
        let diag_view = view_of(&default_sources[1..], Channel::Diag, None, None);
        let diag_out = render_text(&diag_view).out;
        let findings = meter::scan(&diag_out, &secrets, &authored);
        assert!(
            findings
                .iter()
                .any(|f| matches!(f, meter::Finding::TokenPrefix { .. })),
            "the meter must catch a token planted in the diagnostic channel, got {findings:?}"
        );

        // And `--channel all` DOES carry it — stated, not hidden. That is the whole reason the
        // channel is opt-in and the notice says the channel is not redaction-checked.
        let all_view = view_of(&default_sources, Channel::All, None, None);
        let all = render_text(&all_view);
        assert!(all.out.contains("sk-ant-"));
        assert!(
            all.notice.contains("not redaction-checked"),
            "an opted-in view must say the channel is ungoverned, got {:?}",
            all.notice
        );
    }

    /// `--channel all` interleaves the two files by timestamp, and each source keeps its own
    /// order. The fixtures' timestamps alternate, so a concatenation would be visibly wrong.
    #[test]
    fn all_merges_the_two_channels_in_timestamp_order() {
        let sources = sources(Some(FIXTURE_LOG), Some(FIXTURE_DIAG));
        let view = view_of(&sources, Channel::All, None, None);
        assert_eq!(view.n_scanned, 7, "4 event lines + 3 diagnostic lines");

        let timestamps: Vec<&str> = view.matched.iter().map(|s| s.ts.expect("ts")).collect();
        assert_eq!(
            timestamps,
            vec![
                "2026-07-11T00:00:00Z",
                "2026-07-11T00:30:00Z",
                "2026-07-11T01:00:00Z",
                "2026-07-11T01:30:00Z",
                "2026-07-11T02:00:00Z",
                "2026-07-11T02:30:00Z",
                "2026-07-11T03:00:00Z",
            ],
            "the merge must alternate, not concatenate"
        );
        // Each line's channel alternates with it — and comes from the SOURCE, not from guessing
        // at the line's content.
        let channels: Vec<Channel> = view.matched.iter().map(|s| s.channel).collect();
        assert_eq!(
            channels,
            vec![
                Channel::Event,
                Channel::Diag,
                Channel::Event,
                Channel::Diag,
                Channel::Event,
                Channel::Diag,
                Channel::Event,
            ]
        );

        // Within each source, file order is preserved exactly.
        for (channel, expected) in [(Channel::Event, FIXTURE_LOG), (Channel::Diag, FIXTURE_DIAG)] {
            let kept: Vec<&str> = view
                .matched
                .iter()
                .filter(|s| s.channel == channel)
                .map(|s| s.line)
                .collect();
            assert_eq!(kept, expected.lines().collect::<Vec<_>>());
        }
    }

    /// An untimestamped diagnostic line — a panic payload — is placed at the timestamp of the
    /// line before it, so it lands where it happened instead of being dropped or floated to the
    /// front. And the run stays CONTIGUOUS: a backtrace split across an event line would be
    /// unreadable exactly when it matters most.
    #[test]
    fn an_untimestamped_diagnostic_line_inherits_its_predecessors_place() {
        let sources = sources(Some(FIXTURE_LOG), Some(FIXTURE_DIAG_PANIC));
        let view = view_of(&sources, Channel::All, None, None);

        let lines: Vec<&str> = view.matched.iter().map(|s| s.line).collect();
        let panicked = lines
            .iter()
            .position(|l| l.starts_with("thread 'main' panicked"))
            .expect("the panic line must survive the merge");
        // Contiguous with the diagnostic line it followed, and in its own order.
        assert!(lines[panicked - 1].contains("diag=tick"));
        assert!(lines[panicked + 1].contains("Option::unwrap()"));
        // Placed at 01:30 (inherited), so it sits after the 01:00 event line and before 02:00.
        assert!(lines[..panicked]
            .iter()
            .any(|l| l.contains("ts=2026-07-11T01:00:00Z")));
        assert!(lines[panicked..]
            .iter()
            .any(|l| l.contains("ts=2026-07-11T02:00:00Z")));

        // The inheritance is recorded, not merely implied by position.
        let inherited = view.matched[panicked].at;
        assert_eq!(inherited, view.matched[panicked - 1].at);
        assert!(
            view.matched[panicked].ts.is_none(),
            "it carries no ts= of its own"
        );
    }

    /// A window keeps an inherited placement — which is the point of inheriting one. Under
    /// `--since` the panic payload survives with the diagnostic line it belongs to, rather than
    /// being dropped as unplaceable and leaving a truncated crash.
    ///
    /// The EVENT channel keeps issue #773's tolerant-drop unchanged: there, an untimestamped line
    /// is malformed (the log's grammar always writes `ts=`), not ordinary.
    #[test]
    fn a_window_keeps_an_inherited_placement_on_diag_and_still_drops_a_malformed_event_line() {
        // 01:00 window at 03:00 admits the 01:30 tick — and the panic lines that inherit 01:30.
        let diag = select(
            Some(FIXTURE_DIAG_PANIC),
            Some(window("2h", "2026-07-11T03:00:00Z")),
            None,
            Channel::Diag,
        );
        let lines: Vec<&str> = diag.matched.iter().map(|s| s.line).collect();
        assert!(
            lines
                .iter()
                .any(|l| l.starts_with("thread 'main' panicked")),
            "a windowed diagnostic view must keep the panic payload, got {lines:?}"
        );
        assert_eq!(lines.len(), 4, "all four lines are at/after the cutoff");

        // A cutoff ABOVE the inherited timestamp drops the run with the line it belongs to —
        // the inheritance places a line, it does not exempt it.
        let later = select(
            Some(FIXTURE_DIAG_PANIC),
            Some(window("45m", "2026-07-11T03:00:00Z")),
            None,
            Channel::Diag,
        );
        assert_eq!(
            later.matched.iter().map(|s| s.line).collect::<Vec<_>>(),
            vec!["ts=2026-07-11T02:30:00Z diag=stop"]
        );

        // The event channel is UNCHANGED: an unplaceable line still drops, even after a
        // well-formed one (no inheritance there).
        let log = "ts=2026-07-11T03:00:00Z event=swap from=a to=b\nts=nonsense event=swap\n";
        let events = select(
            Some(log),
            Some(window("1h", "2026-07-11T03:00:00Z")),
            None,
            Channel::Event,
        );
        assert_eq!(events.matched.len(), 1);
        assert_eq!(events.matched[0].ts, Some("2026-07-11T03:00:00Z"));
    }

    /// `--event` filters by the channel's OWN kind key: `event=` on the durable log, `diag=` on
    /// the diagnostic channel. Without this the flag would silently match nothing on `diag`.
    #[test]
    fn the_event_filter_uses_each_channels_own_kind_key() {
        let diag = select(Some(FIXTURE_DIAG), None, Some("tick"), Channel::Diag);
        assert_eq!(diag.matched.len(), 1);
        assert_eq!(diag.matched[0].event, Some("tick"));
        assert!(diag.matched[0].line.contains("diag=tick"));

        // Still EXACT, not a prefix: `--event poll` must not also match `poll_failed`.
        let log = "ts=2026-07-11T00:00:00Z diag=poll_failed account=u-A\n";
        assert_eq!(
            select(Some(log), None, Some("poll"), Channel::Diag)
                .matched
                .len(),
            0
        );

        // And an `event=` token is NOT what the diagnostic channel matches on — a durable line
        // that somehow landed there would not be selected by its `event=` name.
        assert_eq!(
            select(Some(FIXTURE_LOG), None, Some("swap"), Channel::Diag)
                .matched
                .len(),
            0
        );

        // Under `--channel all` each half filters by its own key, in one pass.
        let both = sources(Some(FIXTURE_LOG), Some(FIXTURE_DIAG));
        let all = view_of(&both, Channel::All, None, Some("start"));
        assert_eq!(all.matched.len(), 1);
        assert_eq!(all.matched[0].channel, Channel::Diag);
    }

    /// An absent diagnostic file is not a cold install — it is a knob that is off. So it gets the
    /// instruction that resolves it, rather than an empty view that reads as "nothing happened".
    #[test]
    fn an_absent_diagnostic_file_says_how_to_turn_diagnostics_on() {
        let absent = vec![Source {
            channel: Channel::Diag,
            text: None,
        }];
        let view = view_of(&absent, Channel::Diag, None, None);
        let rendered = render_text(&view);
        assert_eq!(rendered.out, "", "nothing to show");
        for expected in [
            "no diagnostics yet",
            "verbose = true",
            "[tunables]",
            "daemon restart",
        ] {
            assert!(
                rendered.notice.contains(expected),
                "the notice must carry {expected:?}, got {:?}",
                rendered.notice
            );
        }
        // NOT the event log's cold-start line: that would name the wrong file and the wrong fix.
        assert!(
            !rendered.notice.contains("the daemon has not run"),
            "an absent diag file is not an absent event log, got {:?}",
            rendered.notice
        );

        // `--channel all` with only the diagnostics missing still renders the event lines, and
        // says which half was missing rather than going quiet about it.
        let partial = vec![
            Source {
                channel: Channel::Event,
                text: Some(FIXTURE_LOG.to_owned()),
            },
            Source {
                channel: Channel::Diag,
                text: None,
            },
        ];
        let mixed = render_text(&view_of(&partial, Channel::All, None, None));
        assert_eq!(mixed.out, FIXTURE_LOG);
        assert!(mixed.notice.contains("no diagnostics yet"));
        assert!(!mixed.notice.contains("the daemon has not run"));
    }

    /// The empty-state notices name the CHANNEL that was read. "The event log is empty" would be
    /// a true-sounding statement about a file the operator did not ask about.
    #[test]
    fn the_empty_notice_names_the_channel_that_was_read() {
        for (channel, expected) in [
            (Channel::Event, "the event log is empty"),
            (Channel::Diag, "the diagnostic channel is empty"),
        ] {
            let source = vec![Source {
                channel,
                text: Some(String::new()),
            }];
            let rendered = render_text(&view_of(&source, channel, None, None));
            assert!(
                rendered.notice.contains(expected),
                "--channel {} must say {expected:?}, got {:?}",
                channel.as_str(),
                rendered.notice
            );
        }
        let both = vec![
            Source {
                channel: Channel::Event,
                text: Some(String::new()),
            },
            Source {
                channel: Channel::Diag,
                text: Some(String::new()),
            },
        ];
        let rendered = render_text(&view_of(&both, Channel::All, None, None));
        assert!(rendered.notice.contains("both channels are empty"));
    }

    /// **CONSTRAINT-A over the new channels.** Every byte the reader writes to stdout already
    /// existed in one of the sources — the reader interpolates nothing, including no channel
    /// marker, which is why the text view leans on `event=`/`diag=` being in the lines already.
    #[test]
    fn stdout_bytes_all_exist_in_their_source_on_every_channel() {
        let both = sources(Some(FIXTURE_LOG), Some(FIXTURE_DIAG));
        let corpus = format!("{FIXTURE_LOG}{FIXTURE_DIAG}");
        for (asked, slice) in [
            (Channel::Event, &both[..1]),
            (Channel::Diag, &both[1..]),
            (Channel::All, &both[..]),
        ] {
            let view = view_of(slice, asked, None, None);
            assert!(
                !view.matched.is_empty(),
                "--channel {} must select lines, else its guard proves nothing",
                asked.as_str()
            );
            let text = render_text(&view);
            assert!(
                every_line_is_in(&text.out, &corpus),
                "--channel {} carried a line absent from both sources: {:?}",
                asked.as_str(),
                text.out
            );
            // Exact stream, not mere membership: each selected line once, in merged order.
            let expected: String = view
                .matched
                .iter()
                .map(|s| format!("{}\n", s.line))
                .collect();
            assert_eq!(text.out, expected);
        }

        // The distinguishability the text view relies on (CONSTRAINT-A forbids adding a marker):
        // every durable line names its kind with `event=`, every diagnostic one with `diag=`.
        let merged = view_of(&both, Channel::All, None, None);
        for selected in &merged.matched {
            let key = match selected.channel {
                Channel::Event => "event=",
                Channel::Diag | Channel::All => "diag=",
            };
            assert!(
                selected.line.contains(key),
                "a {} line must name its kind with {key}: {:?}",
                selected.channel.as_str(),
                selected.line
            );
        }
    }

    /// The JSON view carries the channel as STRUCTURE — per document and per record — so a script
    /// never has to infer it from the line, and per-channel presence stays distinguishable.
    #[test]
    fn json_carries_the_channel_and_per_channel_presence() {
        let both = sources(Some(FIXTURE_LOG), Some(FIXTURE_DIAG));
        let json = render_json(&view_of(&both, Channel::All, None, None)).expect("serializes");
        let parsed: serde_json::Value = serde_json::from_str(&json.out).expect("parses");
        assert_eq!(parsed["schema"], 2, "the channel field bumped the schema");
        assert_eq!(parsed["channel"], "all");
        assert_eq!(parsed["log_present"], true);
        assert_eq!(
            parsed["present"],
            serde_json::json!([
                {"channel": "event", "present": true},
                {"channel": "diag", "present": true},
            ])
        );
        let records = parsed["records"].as_array().expect("array");
        assert_eq!(records.len(), 7);
        assert_eq!(records[0]["channel"], "event");
        assert_eq!(records[1]["channel"], "diag");
        // `line` stays verbatim — the channel rides beside it, never inside it.
        assert_eq!(
            records[1]["line"],
            FIXTURE_DIAG.lines().next().expect("line")
        );

        // A missing diagnostic file is distinguishable from a missing daemon, in the wire.
        let partial = vec![
            Source {
                channel: Channel::Event,
                text: Some(FIXTURE_LOG.to_owned()),
            },
            Source {
                channel: Channel::Diag,
                text: None,
            },
        ];
        let json = render_json(&view_of(&partial, Channel::All, None, None)).expect("serializes");
        let parsed: serde_json::Value = serde_json::from_str(&json.out).expect("parses");
        assert_eq!(parsed["log_present"], true, "one of the two exists");
        assert_eq!(parsed["present"][1]["channel"], "diag");
        assert_eq!(parsed["present"][1]["present"], false);
    }

    /// `--follow --channel all` is refused rather than approximated, and the message says why and
    /// what to do instead. The two single-channel follows are accepted.
    #[test]
    fn follow_refuses_to_merge_both_channels_and_says_why() {
        let args = |channel| LogArgs {
            since: None,
            event: None,
            json: false,
            follow: true,
            channel,
        };
        let err = run_follow(args(Channel::All)).expect_err("a live merge must be refused");
        assert!(matches!(err, Error::LogFollowAllUnsupported), "got {err:?}");
        let shown = err.to_string();
        assert!(
            shown.contains("--channel event") && shown.contains("--channel diag"),
            "the refusal must name the followable alternatives, got {shown:?}"
        );

        // And the follow header names the channel being followed, so a `--follow --channel diag`
        // stream is not mistakable for the event log — plus the ungoverned-channel warning.
        let diag = follow_header(None, None, Channel::Diag);
        assert!(diag.contains("following the diagnostic channel"));
        assert!(diag.contains("not redaction-checked"));
        let event = follow_header(None, None, Channel::Event);
        assert!(event.contains("following the event log"));
        assert!(
            !event.contains("not redaction-checked"),
            "the governed channel must not carry the warning, got {event:?}"
        );
    }

    /// The merge is a two-pointer MERGE, not a sort — so each source's own order survives even
    /// when a file is NOT internally monotone (a clock step, an interleaved writer). A sort over
    /// a shared key could reorder those lines; this cannot.
    #[test]
    fn a_non_monotone_source_keeps_its_own_order_through_the_merge() {
        // The second diagnostic line is EARLIER than the first — the file is out of order.
        let jumbled = "ts=2026-07-11T02:30:00Z diag=tick decision=hold\n\
                       ts=2026-07-11T00:30:00Z diag=poll account=u-A outcome=live\n";
        let sources = sources(Some(FIXTURE_LOG), Some(jumbled));
        let view = view_of(&sources, Channel::All, None, None);
        let diag_order: Vec<&str> = view
            .matched
            .iter()
            .filter(|s| s.channel == Channel::Diag)
            .map(|s| s.ts.expect("ts"))
            .collect();
        assert_eq!(
            diag_order,
            vec!["2026-07-11T02:30:00Z", "2026-07-11T00:30:00Z"],
            "the source's own order must survive, however odd it looks"
        );
        // Nothing is lost or duplicated by the merge, whatever the ordering.
        assert_eq!(view.matched.len(), 6);
    }
}
