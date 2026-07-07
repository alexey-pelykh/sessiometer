// Copyright (c) 2026 Oleksii PELYKH
// SPDX-License-Identifier: MIT

// The menu-bar status store (issue #324): the single `@MainActor ObservableObject` source of truth
// the UI renders. It is the thin IMPERATIVE SHELL over the pure `HonestStateMachine` decision core —
// it consumes the transport's `AsyncStream<TransportEvent>` (#323), folds each event into the machine
// (decoding `.line`s via `parseWatchFrame`, #322), and mirrors the machine's derived state out on two
// surfaces:
//
//   * the `@Published` projection (`connectionState` + `rows` + `nextSwap` + `refreshEnabled` +
//     `generatedAt`) that the SwiftUI detail panel observes; and
//   * the `presentations` stream (glyph + a11y label) that the AppKit `NSStatusItem` glance consumes.
//
// ALL honest-state logic — and the crown-jewel "never healthy on a degraded/absent daemon" invariant
// (ADR-0003 UI analogue) — lives in the pure core, so this shell has no branching state logic to get
// wrong: it pumps events in and copies derived values out. Because the store consumes an INJECTED
// stream (not a `WatchTransport` it builds), it is unit-testable against a synthetic
// `AsyncStream<TransportEvent>` — no socket, and independent of #328's mock-socket harness.
//
// macOS 13 floor → `ObservableObject` + `@Published` (Combine), not the 14+ `@Observable` macro. The
// consume loop runs ON the MainActor (the `Task` inherits `@MainActor`), so every `@Published`
// mutation and the small pure decode happen on main — exactly where SwiftUI expects them
// (design-menubar "bg reader → hop to MainActor → ObservableObject → SwiftUI re-renders").

import Combine
import Foundation
import os

private let storeLog = Logger(subsystem: "com.sessiometer.menubar", category: "watch-store")

@MainActor
final class WatchStatusStore: ObservableObject {

    // MARK: - Published view state (the SwiftUI panel observes these)

    /// The honest connection state — the load-bearing output. `.connected` (healthy) is only ever set
    /// from a fresh, schema-supported snapshot with accounts on a live connection.
    @Published private(set) var connectionState: ConnectionState = .connecting
    /// The redacted per-account roster from the last applied snapshot (retained, shown stale, across
    /// a drop / silence — never blanked, never shown as live once degraded).
    @Published private(set) var rows: [AccountRow] = []
    /// The daemon's next swap candidate, or `nil` when there is no active anchor.
    @Published private(set) var nextSwap: NextSwap?
    /// Whether the daemon's periodic isolated-refresh tick is enabled; `nil` for a pre-#138 daemon.
    @Published private(set) var refreshEnabled: Bool?
    /// The `generated_at` of the last snapshot (or the last heartbeat's freshness stamp) — the
    /// live-vs-stale signal the panel compares against the wall clock.
    @Published private(set) var generatedAt: Int64?

    // MARK: - The glance presentation stream (the status item consumes this)

    /// One `PresentationState` (glyph + a11y label) per state change, for the AppKit `NSStatusItem`
    /// glance to consume with `for await`. Latest-wins buffered so a status item that attaches after
    /// launch still gets the current glyph; the SwiftUI panel uses the `@Published` projection above
    /// instead. Two surfaces of the one store (design-menubar: glance icon + click-panel).
    nonisolated let presentations: AsyncStream<PresentationState>
    private let presentationsContinuation: AsyncStream<PresentationState>.Continuation

    /// The current glance value for an immediate synchronous read — the status item's seed at attach
    /// (before the first streamed update), and a convenient assertion point in tests.
    var currentPresentation: PresentationState { machine.presentation }

    // MARK: - Internals

    private var machine = HonestStateMachine()
    private var consumeTask: Task<Void, Never>?

    init() {
        (presentations, presentationsContinuation) =
            AsyncStream.makeStream(bufferingPolicy: .bufferingNewest(1))
        // Seed the glance with the initial `.connecting` so a consumer attaching before the first
        // event still renders a coherent starting glyph rather than nothing.
        presentationsContinuation.yield(machine.presentation)
    }

    /// Begin consuming the transport's event stream. Idempotent — a second call is a no-op. The store
    /// is ONE-SHOT (one transport per app run): when the injected stream finishes (the transport's
    /// deliberate `stop()` on app teardown — it never finishes spontaneously), the loop ends and the
    /// glance stream is finished so the status item's `for await` completes cleanly.
    func start(consuming events: AsyncStream<TransportEvent>) {
        guard consumeTask == nil else { return }
        consumeTask = Task { [weak self] in
            for await event in events {
                self?.ingest(event)
            }
            self?.presentationsContinuation.finish()
        }
    }

    private func ingest(_ event: TransportEvent) {
        let outcome = machine.apply(event)
        log(outcome)
        // Mirror the pure core's derived state onto the published surface + the glance stream.
        connectionState = machine.connectionState
        rows = machine.rows
        nextSwap = machine.nextSwap
        refreshEnabled = machine.refreshEnabled
        generatedAt = machine.generatedAt
        presentationsContinuation.yield(machine.presentation)
    }

    /// Log a line's decode outcome. The `watch` stream is redacted at source (no token / email /
    /// fingerprint — issue #15 / C-001), so the structural decode reason is safe to log; the raw line
    /// bytes are never echoed.
    private func log(_ outcome: LineOutcome?) {
        switch outcome {
        case .ignoredUndecodable(let reason):
            storeLog.error("watch: skipped an undecodable line — \(reason, privacy: .public)")
        case .unsupportedSchema:
            storeLog.error("watch: daemon schema major is unsupported — degrading to the unsupported state")
        case .ignoredUnknownFrame:
            storeLog.debug("watch: ignored an unknown frame kind")
        case .appliedSnapshot, .appliedHeartbeat, .none:
            break
        }
    }
}
