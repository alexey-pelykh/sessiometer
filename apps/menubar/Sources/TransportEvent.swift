// Copyright (c) 2026 Oleksii PELYKH
// SPDX-License-Identifier: MIT

// The push-only event a `WatchTransport` (issue #323) emits to its consumer — the status store
// (#324). The transport is a thin byte-and-connection-state pump: it surfaces raw newline-delimited
// `.line`s plus connection-state transitions, and deliberately does NOT interpret frame CONTENT
// (decoding a line into a snapshot / heartbeat is #324's job, via `parseWatchFrame` in
// `WireModel.swift`). Keeping interpretation out of the transport is what lets it stay egress-free
// and trivially testable, and lets a pre-#164 daemon that streams only `{"error":…}` degrade
// gracefully instead of the transport hanging on a first-snapshot precondition (ADR-0011).

/// A single event from the `watch` subscription. The four cases are the entire transport contract;
/// the consumer builds its own view state from this ordered stream.
enum TransportEvent: Equatable, Sendable {
    /// A `connect()` succeeded and the `{"cmd":"watch"}` subscribe was written. Emitted exactly once
    /// per successful connection, BEFORE any line — so a consumer can render "connected" immediately
    /// and never blocks awaiting a first snapshot that a degraded daemon may never send (ADR-0011
    /// graceful-degrade requirement).
    case connected

    /// One newline-delimited line from the stream, verbatim (the trailing `\n` stripped, empty lines
    /// skipped). The consumer decodes it; the transport does not look inside.
    case line(String)

    /// The connection dropped — daemon absent (`connect()` failed), the subscribe write failed, or
    /// the stream hit EOF / a read error. Carries a human-readable reason for `os_log` / the UI. A
    /// bounded exponential-backoff reconnect follows automatically; the consumer re-arms nothing.
    case disconnected(reason: String)

    /// No line (snapshot OR heartbeat) has arrived for longer than the liveness window
    /// (> 2× the daemon's 15 s `WATCH_HEARTBEAT`). The connection is still open — this is a
    /// "daemon silent / view may be frozen" warning, NOT a disconnect — so the consumer can show
    /// "stale" instead of a silently-frozen snapshot. Cleared by the next `.line`.
    case stale
}
