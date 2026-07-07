// Copyright (c) 2026 Oleksii PELYKH
// SPDX-License-Identifier: MIT

// The long-lived `watch` socket client (issue #323): a single off-main ACTOR that connects to the
// daemon's AF_UNIX control socket, subscribes (`{"cmd":"watch"}`), and pushes an
// `AsyncStream<TransportEvent>` of `.connected` / `.line` / `.disconnected` / `.stale` to the store
// (#324) — reconnecting with bounded exponential backoff on failure and flagging staleness when the
// daemon goes silent.
//
// This actor is the IMPERATIVE SHELL over the pure `WatchStateMachine` decision core: it performs
// the machine's `Effect`s (connect via the injected `WatchConnector`, sleep via `Task.sleep`, emit
// to the stream) and feeds `Event`s back. All socket I/O sits behind the `WatchConnector` seam and
// all timing behind injected `Duration`s, so reconnect + liveness behavior is unit-testable with an
// in-process fake — no real socket, and independent of #328's mock-socket harness.
//
// ZERO network egress by construction: the only production connector is `PosixSocketConnector`, a
// same-user local UDS. No Network.framework, no host, no analytics, no outbound call of any kind.

import Foundation
import os

private let log = Logger(subsystem: "com.sessiometer.menubar", category: "watch-transport")

/// The `{"cmd":"watch"}\n` subscribe request the daemon's `serve_control` dispatches to `serve_watch`.
private let watchSubscribeCommand: [UInt8] = Array(#"{"cmd":"watch"}"#.utf8) + [0x0A]

actor WatchTransport {
    private let connector: WatchConnector
    private var machine: WatchStateMachine

    /// The push-only event stream the store (#324) consumes with `for await`.
    nonisolated let events: AsyncStream<TransportEvent>
    private let eventsContinuation: AsyncStream<TransportEvent>.Continuation

    private var started = false
    private var currentConnection: WatchConnection?
    private var readerTask: Task<Void, Never>?
    private var livenessTask: Task<Void, Never>?
    private var backoffTask: Task<Void, Never>?

    init(
        connector: WatchConnector,
        backoff: ExponentialBackoff = .default,
        livenessWindow: Duration = .seconds(32)
    ) {
        self.connector = connector
        self.machine = WatchStateMachine(backoff: backoff, livenessWindow: livenessWindow)
        (self.events, self.eventsContinuation) = AsyncStream<TransportEvent>.makeStream()
        // If the consumer stops listening (its `for await` ends / cancels), tear everything down.
        eventsContinuation.onTermination = { [weak self] _ in
            Task { await self?.stop() }
        }
    }

    /// Begin connecting and streaming. Idempotent — a second call is a no-op.
    ///
    /// The transport is ONE-SHOT: `stop()` finishes the `events` stream permanently, and a `start()`
    /// after a `stop()` does NOT revive it (the machine is `.stopped` and the stream is already
    /// finished). To watch again after stopping, build a fresh `WatchTransport`. This matches the
    /// store's lifecycle (#324): one transport per app run, torn down on quit.
    func start() {
        guard !started else { return }
        started = true
        log.info("watch transport starting")
        feed(.start)
    }

    /// Stop and tear everything down: move the machine to `.stopped`, cancel timers + the reader,
    /// close the connection, and finish the event stream. Idempotent.
    func stop() {
        feed(.stop)                                   // → `.stopped`; invalidates in-flight timers
        livenessTask?.cancel(); livenessTask = nil
        backoffTask?.cancel(); backoffTask = nil
        readerTask?.cancel(); readerTask = nil
        currentConnection?.close(); currentConnection = nil
        eventsContinuation.finish()
    }

    // MARK: - Machine driver

    /// Feed one event to the pure machine and perform the effects it returns, in order.
    private func feed(_ event: WatchStateMachine.Event) {
        for effect in machine.advance(event) { perform(effect) }
    }

    private func perform(_ effect: WatchStateMachine.Effect) {
        switch effect {
        case .emit(let transportEvent):
            eventsContinuation.yield(transportEvent)
        case .connect:
            performConnect()
        case .closeConnection:
            teardownConnection()
        case .armLiveness(let after, let generation):
            armLiveness(after: after, generation: generation)
        case .armBackoff(let after, let generation):
            armBackoff(after: after, generation: generation)
        }
    }

    // MARK: - Effect: connect

    /// Open a connection off the actor's turn (a connect + subscribe can block briefly), then feed
    /// the outcome back. On success the connection is attached and the reader started AFTER the
    /// machine has processed `connectSucceeded` — so no line is fed while the machine is still
    /// `.connecting` (the buffered `AsyncStream` holds any early line until the reader starts).
    private func performConnect() {
        let connector = self.connector
        // `Task.detached` (NOT `Task {}`, which would inherit this actor's isolation and run the
        // blocking connect ON the actor): the brief-but-blocking `connect()` + subscribe write runs
        // off-actor, and the outcome marshals back via a genuine cross-actor hop.
        Task.detached { [weak self] in
            do {
                let connection = try connector.connect()
                try connection.send(watchSubscribeCommand)
                await self?.connectionEstablished(connection)
            } catch {
                let reason = (error as? TransportError)?.reason ?? "connect: \(error)"
                await self?.feed(.connectFailed(reason: reason))
            }
        }
    }

    private func connectionEstablished(_ connection: WatchConnection) {
        feed(.connectSucceeded)            // machine → `.connected`; emits `.connected`, arms liveness
        // If the transport was stopped (or the consumer cancelled) WHILE this connect was in flight,
        // the machine is now `.stopped` and `.connectSucceeded` was a no-op. Do NOT attach or start a
        // reader against a dead transport — the fd + reader thread would otherwise leak until the
        // daemon EOFs, and a `watch` stream may never EOF.
        guard case .connected = machine.state else {
            connection.close()
            return
        }
        currentConnection = connection
        startReading(connection)           // now safe: `.lineReceived` is handled in `.connected`
    }

    private func startReading(_ connection: WatchConnection) {
        readerTask?.cancel()
        readerTask = Task { [weak self] in
            for await line in connection.lines {
                if line.isEmpty { continue }               // blank lines are never surfaced as `.line`
                await self?.feed(.lineReceived(line))
            }
            // `lines` finished → EOF or a read error → the peer went away.
            await self?.feed(.connectionClosed(reason: "connection closed (EOF)"))
        }
    }

    // MARK: - Effect: teardown

    private func teardownConnection() {
        readerTask?.cancel(); readerTask = nil
        currentConnection?.close(); currentConnection = nil
    }

    // MARK: - Effects: timers (real `Task.sleep`; generation-guarded in the machine)

    private func armLiveness(after: Duration, generation: Int) {
        livenessTask?.cancel()
        livenessTask = Task { [weak self] in
            do { try await Task.sleep(for: after) } catch { return }   // cancelled → drop
            await self?.feed(.livenessElapsed(generation: generation))
        }
    }

    private func armBackoff(after: Duration, generation: Int) {
        backoffTask?.cancel()
        backoffTask = Task { [weak self] in
            do { try await Task.sleep(for: after) } catch { return }   // cancelled → drop
            await self?.feed(.backoffElapsed(generation: generation))
        }
    }
}

// MARK: - Production factory

extension WatchTransport {
    /// Build a production transport for the daemon's resolved socket path, or return the resolve
    /// error (sandboxed / home-unresolved) so the caller degrades LOUDLY instead of connecting to a
    /// wrong or denied path (ADR-0011 non-sandbox tripwire). The resolved path equals
    /// `src/paths.rs::control_socket()`; the connector is raw POSIX AF_UNIX (zero egress).
    static func production(
        backoff: ExponentialBackoff = .default,
        livenessWindow: Duration = .seconds(32)
    ) -> Result<WatchTransport, SocketPathResolver.ResolveError> {
        SocketPathResolver.resolve().map { path in
            WatchTransport(
                connector: PosixSocketConnector(path: path),
                backoff: backoff,
                livenessWindow: livenessWindow)
        }
    }
}
