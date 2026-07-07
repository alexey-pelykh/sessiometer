// Copyright (c) 2026 Oleksii PELYKH
// SPDX-License-Identifier: MIT

// The pure, synchronous decision core of `WatchTransport` (issue #323).
//
// A functional-core / imperative-shell split: ALL reconnect + liveness logic (when to reconnect,
// when to declare stale, how the backoff counter grows and resets) lives here as a value type with
// no I/O, no clock, and no concurrency, so every state transition is unit-testable without a real
// socket, real time, or async — exactly the testability the #323 scope asks for (the #328 mock
// socket harness is a SEPARATE item). `WatchTransport` is the thin async shell that performs the
// `Effect`s this machine returns (connect via the injected connector, sleep via `Task.sleep`, emit
// to the event stream) and feeds `Event`s back in.
//
// Timer races are handled WITHOUT relying on cancellation: every armed liveness / backoff timer
// carries a monotonically-increasing GENERATION token; a superseded timer that still fires is
// ignored because its token no longer matches the machine's current generation. This keeps the core
// correct even if the shell's `Task.cancel()` lands a moment late.

import Foundation

// MARK: - Bounded exponential backoff

/// A bounded exponential backoff schedule for reconnect attempts (issue #323): the delay before the
/// 0-based retry attempt *n* is `min(base · multiplier^n, cap)`. A persistently-unreachable daemon
/// is therefore retried at a growing, capped cadence — NEVER a tight loop (the #323 acceptance
/// requirement). Pure and deterministic (a function of the attempt index), with no clock and no
/// jitter: a single local menu-bar client talking to ONE local daemon has no thundering herd to
/// de-correlate — unlike the network-facing per-account backoff of ADR-0009 — so jitter would only
/// cost determinism here.
struct ExponentialBackoff: Sendable, Equatable {
    /// The delay for the first retry (attempt 0).
    let base: Duration
    /// Per-attempt growth factor (2 ⇒ doubling).
    let multiplier: Double
    /// The ceiling no computed delay exceeds, however many attempts have failed.
    let cap: Duration

    /// The production schedule: 1 s → 2 s → 4 s → 8 s → 16 s → 30 s (capped). A base of 1 s keeps
    /// even the degenerate "connect succeeds, daemon streams an error frame, immediately closes"
    /// reconnect — a pre-#164 dev-daemon artifact; the shipped app version-locks a compatible daemon
    /// (ADR-0011 / #171) — at ≤ 1 attempt per second: bounded, never tight.
    static let `default` = ExponentialBackoff(base: .seconds(1), multiplier: 2, cap: .seconds(30))

    /// The delay before the given 0-based retry attempt. `attempt` is clamped to ≥ 0. The result is
    /// monotonically non-decreasing in `attempt` and never exceeds `cap`.
    func delay(forAttempt attempt: Int) -> Duration {
        let n = max(0, attempt)
        // `Duration` has no `pow`, so compute the ceiling in seconds and clamp. An overflowing
        // `pow` yields `.infinity`, which `min` collapses to the cap — so a huge attempt index is
        // safe, not a trap.
        let grown = base.seconds * pow(multiplier, Double(n))
        return .seconds(min(grown, cap.seconds))
    }
}

private extension Duration {
    /// This duration as a `Double` count of seconds (the attoseconds remainder folded in).
    var seconds: Double {
        Double(components.seconds) + Double(components.attoseconds) / 1e18
    }
}

// MARK: - The state machine

struct WatchStateMachine {

    /// Where the subscription currently is.
    enum State: Equatable {
        /// Before `start()`.
        case idle
        /// A `connect()` is in flight.
        case connecting
        /// Connected and streaming. `stale` is `true` once the liveness window has elapsed with no
        /// line; it is cleared by the next line.
        case connected(stale: Bool)
        /// Waiting out a backoff delay before the next `connect()`.
        case backingOff
        /// After `stop()` — terminal; all further events are ignored.
        case stopped
    }

    /// Something that happened, fed in by the shell.
    enum Event: Equatable {
        /// `start()` was called.
        case start
        /// A `connect()` + subscribe write succeeded.
        case connectSucceeded
        /// A `connect()` or the subscribe write failed (daemon absent / reset).
        case connectFailed(reason: String)
        /// A line arrived on the stream.
        case lineReceived(String)
        /// The stream ended — EOF or a read error (the peer went away).
        case connectionClosed(reason: String)
        /// A previously-armed liveness timer fired; `generation` disambiguates superseded timers.
        case livenessElapsed(generation: Int)
        /// A previously-armed backoff timer fired; `generation` disambiguates superseded timers.
        case backoffElapsed(generation: Int)
        /// `stop()` was called.
        case stop
    }

    /// A side effect for the shell to perform. The machine never performs I/O itself.
    enum Effect: Equatable {
        /// Push a `TransportEvent` to the consumer's stream.
        case emit(TransportEvent)
        /// Attempt a connection (connect + subscribe), reporting back `connectSucceeded` /
        /// `connectFailed`.
        case connect
        /// Tear down the current connection (idempotent close of the fd + reader).
        case closeConnection
        /// (Re)arm the liveness timer: after `after`, feed back `livenessElapsed(generation)`.
        case armLiveness(after: Duration, generation: Int)
        /// Arm the backoff timer: after `after`, feed back `backoffElapsed(generation)`.
        case armBackoff(after: Duration, generation: Int)
    }

    private(set) var state: State = .idle

    /// The backoff attempt counter — the index handed to `backoff.delay(forAttempt:)`. Advanced on
    /// each failed attempt / drop; reset to 0 on a successful connect (a reachable endpoint).
    private var attempt = 0
    /// Bumped every time a liveness timer is (re)armed OR invalidated (line, close, stop) — a fired
    /// timer whose generation ≠ this is stale and ignored.
    private var livenessGeneration = 0
    /// Bumped every time a backoff timer is armed OR invalidated (stop).
    private var backoffGeneration = 0

    let backoff: ExponentialBackoff
    /// How long the transport tolerates silence before declaring `.stale`. Defaults to 32 s: two of
    /// the daemon's 15 s `WATCH_HEARTBEAT` intervals plus a 2 s scheduling grace, so a single missed
    /// beat never trips it and the threshold is strictly greater than the 30 s the #323 AC names.
    let livenessWindow: Duration

    init(backoff: ExponentialBackoff = .default, livenessWindow: Duration = .seconds(32)) {
        self.backoff = backoff
        self.livenessWindow = livenessWindow
    }

    /// Advance the machine by one event, returning the effects the shell must perform (in order).
    /// Pure aside from mutating `self` — the same (state, counters, event) always yields the same
    /// (next state, effects).
    mutating func advance(_ event: Event) -> [Effect] {
        switch (state, event) {

        // ── idle ─────────────────────────────────────────────────────────────
        case (.idle, .start):
            state = .connecting
            return [.connect]

        // ── connecting ───────────────────────────────────────────────────────
        case (.connecting, .connectSucceeded):
            state = .connected(stale: false)
            attempt = 0                       // reachable endpoint → reset the backoff growth
            livenessGeneration += 1
            return [.emit(.connected),
                    .armLiveness(after: livenessWindow, generation: livenessGeneration)]

        case (.connecting, .connectFailed(let reason)):
            return beginBackoff(reason: reason, closeFirst: false)

        // ── connected ────────────────────────────────────────────────────────
        case (.connected, .lineReceived(let line)):
            // A line proves liveness: surface it, clear any stale flag, and re-arm the liveness
            // timer under a NEW generation so the prior timer is ignored if it still fires.
            state = .connected(stale: false)
            livenessGeneration += 1
            return [.emit(.line(line)),
                    .armLiveness(after: livenessWindow, generation: livenessGeneration)]

        case (.connected(let stale), .livenessElapsed(let generation)):
            guard generation == livenessGeneration else { return [] }  // superseded timer → ignore
            guard !stale else { return [] }                            // emit `.stale` once per gap
            state = .connected(stale: true)
            return [.emit(.stale)]            // connection stays OPEN — `.stale` is a warning, not a drop

        case (.connected, .connectionClosed(let reason)):
            livenessGeneration += 1           // invalidate the pending liveness timer
            return beginBackoff(reason: reason, closeFirst: true)

        // ── backingOff ───────────────────────────────────────────────────────
        case (.backingOff, .backoffElapsed(let generation)):
            guard generation == backoffGeneration else { return [] }   // superseded timer → ignore
            state = .connecting
            return [.connect]

        // ── stop (from any state) ────────────────────────────────────────────
        case (_, .stop):
            let wasConnected: Bool = { if case .connected = state { return true } else { return false } }()
            state = .stopped
            livenessGeneration += 1           // invalidate any in-flight timers
            backoffGeneration += 1
            return wasConnected ? [.closeConnection] : []

        // ── everything else (stale timers, events in the wrong state) ────────
        default:
            return []
        }
    }

    /// Transition into `.backingOff`: optionally close the dropped connection, emit `.disconnected`,
    /// and arm the backoff timer for the current attempt (then advance the attempt counter).
    private mutating func beginBackoff(reason: String, closeFirst: Bool) -> [Effect] {
        state = .backingOff
        backoffGeneration += 1
        let delay = backoff.delay(forAttempt: attempt)
        attempt += 1
        var effects: [Effect] = []
        if closeFirst { effects.append(.closeConnection) }
        effects.append(.emit(.disconnected(reason: reason)))
        effects.append(.armBackoff(after: delay, generation: backoffGeneration))
        return effects
    }
}
