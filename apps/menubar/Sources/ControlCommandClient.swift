// Copyright (c) 2026 Oleksii PELYKH
// SPDX-License-Identifier: MIT

// The short-lived control-COMMAND client (issue #358): the menu-bar app's FIRST client→daemon write
// path, the sibling of the long-lived `watch` subscriber (#323). A `watch` connection subscribes once
// and then passively streams; a control command is a ONE-SHOT request→reply — connect, write one
// newline-JSON command line, read exactly ONE redacted ack line, close — with the connect+write and
// the ack read EACH bounded by a timeout so a missing / wedged daemon can never hang the caller.
//
// SHARED infrastructure by design: swap-on-click (#169) and the in-app capture affordance (#360) both
// send their commands through this ONE transport — only the request VERB + payload differ (a
// caller-supplied `Encodable`), the transport does not. The daemon redaction contract means an ack
// carries only non-secret machine tags + roster LABELS (issue #15), so decoding a specific verb's ack
// (e.g. `SwapAck`) is the CALLER's concern; the transport is verb-agnostic and hands back the raw ack
// line for the caller to decode.
//
// It REUSES the exact socket-I/O seam the `watch` transport uses — `WatchConnector` / `WatchConnection`
// (`WatchConnection.swift`), whose production impl `PosixSocketConnector` is a raw POSIX AF_UNIX
// same-user local socket (ADR-0011). So this client, too, has ZERO network egress by construction and
// never opens a network seam. Peer-auth is satisfied by that same-user local connection (the daemon's
// `peer_is_same_user` check, `src/daemon/socket.rs`), so the client performs NO credential handling of
// any kind — it moves the caller's non-secret command bytes out and a redacted ack line back, nothing
// more.

import Foundation
import os

/// Why a control-command exchange did not yield a redacted ack. Non-secret: a socket/timeout reason is
/// not a credential (the whole channel is redacted, issue #15), so each case carries a plain string for
/// `os_log` / the UI.
enum ControlCommandError: Error, Equatable {
    /// `connect()` was refused / the socket was absent — the daemon-down signal. A caller may treat
    /// this as "no daemon" (e.g. a standalone fallback), exactly as the `use`-side `request_swap`
    /// (`src/daemon/socket.rs`) distinguishes a refused connect from a mid-exchange failure.
    case connectionRefused(reason: String)
    /// The exchange exceeded the bounded timeout before an ack arrived — a wedged / silent daemon.
    case timedOut
    /// The connection closed (EOF) before any ack line — the daemon went away mid-exchange.
    case closedBeforeAck
    /// The request could not be JSON-encoded (a bug in the caller's request type, not an I/O fault).
    case encodeFailed(reason: String)
    /// A socket-layer failure creating the socket or writing the command (the `socket()` / `write()`
    /// path), distinct from a plain refused connect.
    case io(reason: String)

    /// Map a transport-layer `TransportError` (thrown by the shared socket seam) into this taxonomy. A
    /// refused / failed `connect` is the daemon-down signal; every other socket failure is `io`.
    init(_ error: TransportError) {
        switch error {
        case .connect(let reason): self = .connectionRefused(reason: reason)
        case .socket(let reason): self = .io(reason: reason)
        case .write(let reason): self = .io(reason: reason)
        case .pathTooLong(let bytes, let cap):
            self = .io(reason: "socket path too long (\(bytes) ≥ \(cap) bytes)")
        }
    }
}

/// A short-lived control-command transport over the daemon's local control socket. Stateless between
/// calls — each `send` is an independent connect→command→ack→close exchange — so a plain `struct`, not
/// the `watch` transport's long-lived actor. `Sendable`: both stored properties are (`WatchConnector`
/// is `Sendable`, `Duration` is a value type), so a caller can hold it and hand it across isolation.
struct ControlCommandClient: Sendable {
    private let connector: WatchConnector

    /// The per-phase upper bound, applied to BOTH the connect+write and the ack read (so a fully wedged
    /// daemon is bounded by ~2× in the worst case, each phase independently). Mirrors the daemon's own
    /// `CONTROL_EXCHANGE_TIMEOUT` (`src/daemon/socket.rs`, 2 s) and the `use`-side `CLIENT_NOTIFY_TIMEOUT`:
    /// a control request + its one-shot reply is one short line each way, so a peer that never completes
    /// it is a wedged daemon, not slow work. A command whose ack legitimately waits longer (the `swap`
    /// #167 exchange, which may hold the swap lock) passes its own larger budget at the #169 call site.
    private let timeout: Duration

    init(connector: WatchConnector, timeout: Duration = .seconds(2)) {
        self.connector = connector
        self.timeout = timeout
    }

    /// Send one control command and return the daemon's single redacted ack LINE (verbatim, the
    /// trailing `\n` already stripped by the reader), or a bounded error. `request` is any `Encodable`
    /// the caller shapes to `{"cmd":"<verb>", …}` — the verb + payload are the caller's, so the
    /// transport is not duplicated per verb. Any connection established is closed before returning —
    /// except one whose connect was abandoned on a connect-phase timeout, which a background sweep
    /// closes when it resolves. The caller decodes the returned line into the verb's ack (e.g. `SwapAck`).
    func send(_ request: some Encodable) async -> Result<String, ControlCommandError> {
        let requestBytes: [UInt8]
        do {
            requestBytes = try Self.encodeLine(request)
        } catch {
            return .failure(.encodeFailed(reason: "\(error)"))
        }

        // Connect + write the command on a DETACHED task, off the caller's executor: both are brief but
        // blocking syscalls that must never run on the UI / cooperative context. `request` was already
        // serialized above, so the task captures only the byte buffer. A `connect()` throw is the
        // daemon-down / socket-failure signal.
        let connector = self.connector
        let connect = Task.detached(priority: .utility) { () throws -> WatchConnection in
            let connection = try connector.connect()
            try connection.send(requestBytes)
            return connection
        }

        // Phase 1 — bound the connect+write. A blocking `connect()` syscall is NOT cancellable, so it is
        // RACED against the deadline rather than merely cancelled: on timeout `send` returns without
        // waiting, and a background sweep closes the abandoned connection if the connect ever resolves,
        // so nothing leaks. (Local AF_UNIX connects fast-fail in practice — absent → immediate refuse,
        // present → immediate accept — so this is the honest backstop for a saturated / wedged peer.)
        switch await awaitConnect(connect) {
        case .failed(let error):
            return .failure(error)
        case .timedOut:
            Task.detached { if let leaked = try? await connect.value { leaked.close() } }
            return .failure(.timedOut)
        case .connected(let connection):
            // Phase 2 — read exactly ONE ack line, bounded by `timeout`, then close.
            return await readAck(from: connection)
        }
    }

    /// The outcome of the bounded connect+write phase (see `awaitConnect`).
    private enum ConnectOutcome: Sendable {
        case connected(WatchConnection)
        case failed(ControlCommandError)
        case timedOut
    }

    /// Await the detached connect+write, giving up after `timeout`. A blocking `connect()` cannot be
    /// cancelled, so the deadline is RACED against it via a resume-once continuation and the losing task
    /// is ABANDONED (never awaited — awaiting would reintroduce the hang); `send` sweeps an abandoned
    /// connection closed. Resolves exactly once, to the connection / a mapped failure / `.timedOut`.
    private func awaitConnect(_ connect: Task<WatchConnection, Error>) async -> ConnectOutcome {
        await withCheckedContinuation { continuation in
            let resumed = OSAllocatedUnfairLock(initialState: false)
            @Sendable func resumeOnce(_ outcome: ConnectOutcome) {
                let first = resumed.withLock { done -> Bool in
                    if done { return false }
                    done = true
                    return true
                }
                if first { continuation.resume(returning: outcome) }
            }
            // The connect+write outcome.
            Task {
                do {
                    resumeOnce(.connected(try await connect.value))
                } catch let error as TransportError {
                    resumeOnce(.failed(ControlCommandError(error)))
                } catch {
                    resumeOnce(.failed(.io(reason: "\(error)")))
                }
            }
            // The deadline — fires once; a no-op if the connect already won.
            Task { [timeout] in
                do { try await Task.sleep(for: timeout) } catch { return }
                resumeOnce(.timedOut)
            }
        }
    }

    /// Read exactly one ack line, bounded by `timeout`, then close. The timeout is armed as a real
    /// `Task.sleep` that, on elapse, `close()`s the connection — which unblocks the reader so its
    /// `lines` stream finishes (the same fd-close teardown the `watch` transport relies on). A
    /// `timedOut` flag then distinguishes a genuine timeout from a daemon that EOF'd before acking.
    private func readAck(from connection: WatchConnection) async -> Result<String, ControlCommandError> {
        let timedOut = OSAllocatedUnfairLock(initialState: false)
        let deadline = Task { [timeout] in
            do { try await Task.sleep(for: timeout) } catch { return }  // cancelled (ack arrived) → drop
            timedOut.withLock { $0 = true }
            connection.close()                                          // unblock the reader → `lines` ends
        }
        // Whatever the outcome, cancel the timer and close the connection — exactly once, idempotently.
        defer {
            deadline.cancel()
            connection.close()
        }

        for await line in connection.lines {
            return .success(line)  // the FIRST line IS the redacted ack; the caller decodes it
        }
        // The stream finished with no line: either the deadline closed it (timeout) or the daemon EOF'd
        // before replying.
        return timedOut.withLock { $0 } ? .failure(.timedOut) : .failure(.closedBeforeAck)
    }

    /// Encode a request to one newline-delimited JSON line (`{"cmd":…}\n`), the framing every daemon
    /// control command speaks (`src/daemon/socket.rs`). Keys are emitted in a deterministic sorted
    /// order (`.sortedKeys`) so the wire line is reproducible for logging + tests — the daemon parses
    /// by key, so ordering is immaterial to it, but a stable serialization is not (`JSONEncoder`'s
    /// default key order is unspecified).
    private static func encodeLine(_ request: some Encodable) throws -> [UInt8] {
        let encoder = JSONEncoder()
        encoder.outputFormatting = .sortedKeys
        var bytes = Array(try encoder.encode(request))
        bytes.append(0x0A)
        return bytes
    }
}

// MARK: - Production factory

extension ControlCommandClient {
    /// Build a production client for the daemon's resolved control-socket path, or return the resolve
    /// error (sandboxed / home-unresolved) so the caller degrades LOUDLY rather than targeting a wrong
    /// or denied path (ADR-0011 non-sandbox tripwire). The resolved path equals
    /// `src/paths.rs::control_socket()`; the connector is the SAME raw POSIX AF_UNIX one the `watch`
    /// transport uses (zero egress).
    static func production(
        timeout: Duration = .seconds(2)
    ) -> Result<ControlCommandClient, SocketPathResolver.ResolveError> {
        SocketPathResolver.resolve().map { path in
            ControlCommandClient(connector: PosixSocketConnector(path: path), timeout: timeout)
        }
    }
}
