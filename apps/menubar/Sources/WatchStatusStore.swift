// Copyright (c) 2026 Oleksii PELYKH
// SPDX-License-Identifier: MIT

// The menu-bar status store (issue #324): the single `@MainActor ObservableObject` source of truth
// the UI renders. It is the thin IMPERATIVE SHELL over the pure `HonestStateMachine` decision core —
// it consumes the transport's `AsyncStream<TransportEvent>` (#323), folds each event into the machine
// (decoding `.line`s via `parseWatchFrame`, #322), and mirrors the machine's derived state out on two
// surfaces:
//
//   * the `@Published` projection (`connectionState` + `rows` + `nextSwap` + `refreshEnabled` +
//     `generatedAt` + `canonicalScrub`) that the SwiftUI detail panel observes; and
//   * the `presentations` stream (glyph + a11y label) that the AppKit `NSStatusItem` glance consumes.
//
// ALL honest-state logic — and the crown-jewel "never healthy on a degraded/absent daemon" invariant
// (ADR-0003 UI analogue) — lives in the pure core, so this shell has no branching state logic to get
// wrong: it pumps events in and copies derived values out. The one piece of MECHANISM it owns is the
// store-side valid-frame watchdog's real timer (#344): the pure core decides WHEN the watchdog should
// (re)arm (it bumps a generation token) and WHAT elapsing means (`watchdogElapsed` → `.stale`); this
// shell only performs the `Task.sleep(for:)` and feeds the elapse back — exactly as `WatchTransport`
// performs `WatchStateMachine`'s `armLiveness` effect. Because the store consumes an INJECTED stream
// (not a `WatchTransport` it builds), it is unit-testable against a synthetic
// `AsyncStream<TransportEvent>` — no socket, and independent of #328's mock-socket harness; the
// watchdog window is injected too, so the timer path is driven deterministically in tests.
//
// macOS 13 floor → `ObservableObject` + `@Published` (Combine), not the 14+ `@Observable` macro. The
// consume loop runs ON the MainActor (the `Task` inherits `@MainActor`), so every `@Published`
// mutation and the small pure decode happen on main — exactly where SwiftUI expects them
// (design-menubar "bg reader → hop to MainActor → ObservableObject → SwiftUI re-renders").

import Combine
import Foundation
import os

private let storeLog = Logger(subsystem: "org.sessiometer.menubar", category: "watch-store")

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
    /// The daemon-level shared-canonical scrub rollup (#469, wire #516) — a fleet-wide `claude`-login
    /// lockout NO per-account `auth` cell reflects (each row can read healthy while the shared item sits
    /// emptied). The panel renders it as an honest banner above the roster; `nil` when the shared
    /// canonical is healthy (or a pre-#516 daemon omits the wire key).
    @Published private(set) var canonicalScrub: CanonicalScrub?
    /// The daemon-level KEYCHAIN-LOCKED flag (#498, wire #521) — a fleet-wide unreadable-credential
    /// lockout NO per-account `auth` cell reflects (each row can read healthy while the shared item sits
    /// UNREADABLE because the login keychain is locked). The panel renders it as an honest banner above
    /// the roster (worst-first over `canonicalScrub`); `false` when the keychain is unlocked (or a
    /// pre-#498 daemon omits the wire key). Its remedy is UNLOCK the keychain, not `claude /login`.
    @Published private(set) var keychainLocked: Bool = false
    /// The daemon-level SYSTEMIC refresh-failure count (#378, wire since schema 1.1) — the refresh
    /// MECHANISM is down (N consecutive sweeps in which EVERY eligible account's cycle errored: a stale
    /// pinned `claude` path, a wedged spawn), which no per-account `auth` cell reflects even in principle
    /// because it is visible BEFORE any account dies. The panel renders it as an honest banner above the
    /// roster, ranked UNDER the two "act now" faults (`keychainLocked`, `canonicalScrub` = `exhausted`)
    /// but OVER the calm self-healing `canonicalScrub` = `recovering` — severity ranks by (fault, VARIANT),
    /// never by fault identity, or a "no action needed" banner would answer a glance that is shouting
    /// (see `StatusPanelFormat.daemonFaultBanner`). `nil` when the mechanism is healthy (or a pre-#378
    /// daemon omits the wire key). A COUNT only, never a token or path (issue #15).
    @Published private(set) var systemicRefreshFailure: UInt32?

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

    /// The default store-side valid-frame watchdog window (#344): 32 s — identical to the transport's
    /// `livenessWindow`. A healthy daemon emits a snapshot or heartbeat every ≤ 15 s
    /// (`WATCH_HEARTBEAT`), so 32 s (> 2× that, plus scheduling grace) means TWO consecutive expected
    /// valid frames were missed = unambiguously degraded, while a single late/dropped beat never
    /// false-trips. Matching the transport's window keeps the two staleness paths coherent: the store
    /// never declares stale SOONER than the transport for a genuinely silent daemon; it only ADDS a
    /// path for a byte-live-but-frame-dead daemon (the #344 hole).
    ///
    /// `nonisolated` so it is referenceable from the `init` default-argument expression (a nonisolated
    /// context) — safe because it is an immutable `Sendable` `Duration` constant.
    nonisolated static let defaultValidFrameWindow: Duration = .seconds(32)

    /// How long the store tolerates a live connection with NO valid decodable frame before it drives
    /// itself `.stale` — injected so tests drive the timer deterministically (as `WatchTransport`
    /// injects `livenessWindow`).
    private let validFrameWindow: Duration
    /// The in-flight watchdog timer, re-armed whenever the pure core bumps its watchdog generation.
    private var watchdogTask: Task<Void, Never>?

    /// The default crash-loop stability window (#169): how long a POST-RECONNECT connection must stay
    /// up (holding a fresh snapshot) before it is promoted to healthy. 8 s — comfortably longer than a
    /// fast crash-loop's up-period (a daemon that dies on startup within a second or two never survives
    /// it, so it is held `.crashLooping` instead of flickering healthy) yet short enough that a genuine
    /// single reconnect settles quickly. The FIRST connect is EXEMPT (`HonestStateMachine.isStabilizing`
    /// is false until a drop), so a cold clean start is immediate. Injected so tests drive it
    /// deterministically. `nonisolated` for the `init` default-argument expression, as
    /// `defaultValidFrameWindow`.
    nonisolated static let defaultStabilityWindow: Duration = .seconds(8)

    /// How long the store holds a post-reconnect snapshot before promoting it to healthy — injected so
    /// tests drive the timer deterministically (as `validFrameWindow`).
    private let stabilityWindow: Duration
    /// The in-flight stability timer, re-armed whenever the pure core bumps its stability generation.
    private var stabilityTask: Task<Void, Never>?

    /// The default start grace (#499): how long a COLD connect-refused shows as the transient `.starting`
    /// before it escalates to the durable `.notRunning`. 3 s — a "short grace" that comfortably covers a
    /// daemon coming up (a launchd (re)start binds its socket within a second or two, so a genuinely-
    /// starting daemon connects INSIDE the grace and never flickers "not running") yet is short enough
    /// that a truly-absent daemon reaches the actionable not-running state promptly. Injected so tests
    /// drive it deterministically. `nonisolated` for the `init` default-argument
    /// expression, as the other windows.
    nonisolated static let defaultStartGraceWindow: Duration = .seconds(3)

    /// How long the store holds a cold connect-refused as `.starting` before escalating to `.notRunning`
    /// — injected so tests drive the grace timer deterministically (as `validFrameWindow`).
    private let startGraceWindow: Duration
    /// The in-flight start-grace timer, re-armed whenever the pure core bumps its grace generation (#499).
    private var graceTask: Task<Void, Never>?

    /// The default warm dwell (#526): how long a WARM drop shows as the transient `.reconnecting` (the calm
    /// "…") before escalating to the durable `.disconnected` (the loud "!"). 8 s — chosen to CLEAR the
    /// transport's exponential-backoff reconnect schedule rather than fall inside it. `WatchStateMachine`'s
    /// `ExponentialBackoff.default` (base 1 s, ×2) lands reconnect ATTEMPTS at cumulative t ≈ 1 s, 3 s, and
    /// 7 s after the drop (delays 1 s → 2 s → 4 s). A shorter dwell (e.g. 6 s) falls in the DEAD-ZONE between
    /// the t ≈ 3 s and t ≈ 7 s attempts: a daemon that rebinds in that gap is not re-probed until t ≈ 7 s, so
    /// the dwell would elapse first and briefly flash Attention on a restart that was already back. 8 s sits
    /// just PAST the t ≈ 7 s attempt, so a routine daemon restart caught by any of the first three attempts
    /// reconnects INSIDE the window and does not flash Attention — the t ≈ 7 s attempt being the tight case,
    /// leaving ~1 s for the reconnect + first snapshot, which is precisely what the on-device calibration
    /// below settles — yet it stays short enough that a genuinely-dead daemon reaches the actionable
    /// Attention promptly (Attention is a next-break state, not urgent). It is longer
    /// than #499's 3 s cold start grace precisely because the warm path rides this backoff whereas a cold
    /// socket-bind does not. Injected so tests drive it deterministically; the on-device p99-restart
    /// calibration is an owner verification step (#526).
    /// `nonisolated` for the `init` default-argument expression, as the other windows.
    nonisolated static let defaultWarmDwellWindow: Duration = .seconds(8)

    /// How long the store holds a warm drop as `.reconnecting` before escalating to `.disconnected` —
    /// injected so tests drive the dwell timer deterministically (as `startGraceWindow`).
    private let warmDwellWindow: Duration
    /// The in-flight warm-dwell timer, re-armed whenever the pure core bumps its dwell generation (#526).
    private var dwellTask: Task<Void, Never>?

    init(validFrameWindow: Duration = WatchStatusStore.defaultValidFrameWindow,
         stabilityWindow: Duration = WatchStatusStore.defaultStabilityWindow,
         startGraceWindow: Duration = WatchStatusStore.defaultStartGraceWindow,
         warmDwellWindow: Duration = WatchStatusStore.defaultWarmDwellWindow) {
        self.validFrameWindow = validFrameWindow
        self.stabilityWindow = stabilityWindow
        self.startGraceWindow = startGraceWindow
        self.warmDwellWindow = warmDwellWindow
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
            self?.watchdogTask?.cancel()          // teardown: drop the in-flight valid-frame watchdog
            self?.stabilityTask?.cancel()         // teardown: drop the in-flight stability timer (#169)
            self?.graceTask?.cancel()             // teardown: drop the in-flight start-grace timer (#499)
            self?.dwellTask?.cancel()             // teardown: drop the in-flight warm-dwell timer (#526)
        }
    }

    private func ingest(_ event: TransportEvent) {
        let watchdogBefore = machine.watchdogGeneration
        let stabilityBefore = machine.stabilityGeneration
        let graceBefore = machine.graceGeneration
        let dwellBefore = machine.dwellGeneration
        let outcome = machine.apply(event)
        log(outcome)
        resyncTimers(watchdogBefore: watchdogBefore, stabilityBefore: stabilityBefore, graceBefore: graceBefore, dwellBefore: dwellBefore)
        publish()
    }

    /// Re-arm / cancel each real timer to match the pure core's intent, but ONLY when its generation
    /// token changed across the mutation — the token changes exactly when the timer should (re)start or
    /// stop, so a routine event that touches none leaves all four counting down untouched. Shared by
    /// event ingestion, every timer-fired path, AND the sleep/wake handlers so a cross-timer effect (e.g.
    /// the valid-frame watchdog tripping `.stale`, which also ends a stabilization hold) re-syncs ALL
    /// (#344, #169, #499, #526).
    private func resyncTimers(watchdogBefore: Int, stabilityBefore: Int, graceBefore: Int, dwellBefore: Int) {
        if machine.watchdogGeneration != watchdogBefore { rearmValidFrameWatchdog() }
        if machine.stabilityGeneration != stabilityBefore { rearmStabilityTimer() }
        if machine.graceGeneration != graceBefore { rearmGraceTimer() }
        if machine.dwellGeneration != dwellBefore { rearmDwellTimer() }
    }

    /// Mirror the pure core's derived state onto the published surface + the glance stream. Shared by
    /// event ingestion and the watchdog-fired path so both surfaces stay in lock-step with the core.
    private func publish() {
        connectionState = machine.connectionState
        rows = machine.rows
        nextSwap = machine.nextSwap
        refreshEnabled = machine.refreshEnabled
        generatedAt = machine.generatedAt
        canonicalScrub = machine.canonicalScrub
        keychainLocked = machine.keychainLocked
        systemicRefreshFailure = machine.systemicRefreshFailure
        presentationsContinuation.yield(machine.presentation)
    }

    // MARK: - Store-side valid-frame watchdog (#344)

    /// (Re)arm — or cancel — the store-side valid-frame watchdog to match the pure core's current
    /// intent. Real-timer analogue of `WatchTransport.armLiveness`: cancel any prior timer, and if the
    /// core is still watching for valid frames, sleep the window then feed the (generation-guarded)
    /// elapse back. The generation guard makes a superseded timer that still fires a harmless no-op,
    /// so a late cancellation never mis-fires `.stale`.
    private func rearmValidFrameWatchdog() {
        watchdogTask?.cancel()
        guard machine.isWatchingForValidFrames else { watchdogTask = nil; return }
        let generation = machine.watchdogGeneration
        let window = validFrameWindow
        // Created in this `@MainActor` context, so the closure is MainActor-isolated: the elapse hop
        // back to the machine + published surface runs on main, where SwiftUI expects mutations.
        watchdogTask = Task { [weak self] in
            do { try await Task.sleep(for: window) } catch { return }   // cancelled → drop
            self?.validFrameWatchdogElapsed(generation: generation)
        }
    }

    /// The watchdog fired: fold the elapse into the machine (a superseded token is ignored there) and
    /// re-publish in case it downgraded a live connection to `.stale`. Re-syncs BOTH timers because
    /// going stale also ends an in-flight stabilization hold (#169), invalidating that timer too.
    private func validFrameWatchdogElapsed(generation: Int) {
        let watchdogBefore = machine.watchdogGeneration
        let stabilityBefore = machine.stabilityGeneration
        let graceBefore = machine.graceGeneration
        let dwellBefore = machine.dwellGeneration
        machine.watchdogElapsed(generation: generation)
        resyncTimers(watchdogBefore: watchdogBefore, stabilityBefore: stabilityBefore, graceBefore: graceBefore, dwellBefore: dwellBefore)
        publish()
    }

    // MARK: - Store-side crash-loop stability timer (#169)

    /// (Re)arm — or cancel — the store-side stability timer to match the pure core's current intent, the
    /// crash-loop-debounce analogue of `rearmValidFrameWatchdog`: while a post-reconnect snapshot is
    /// held (`machine.isStabilizing`), sleep the window then feed the (generation-guarded) elapse back,
    /// which promotes the held snapshot to healthy. A drop / stale before the window elapses supersedes
    /// the token, so a crash-looping daemon never reaches the promote — it stays held, never healthy.
    private func rearmStabilityTimer() {
        stabilityTask?.cancel()
        guard machine.isStabilizing else { stabilityTask = nil; return }
        let generation = machine.stabilityGeneration
        let window = stabilityWindow
        stabilityTask = Task { [weak self] in
            do { try await Task.sleep(for: window) } catch { return }   // cancelled → drop
            self?.stabilityWindowElapsed(generation: generation)
        }
    }

    /// The stability window fired: fold the elapse into the machine (a superseded token — a drop / stale
    /// mid-window — is ignored there) and re-publish, in case it promoted the held snapshot to healthy.
    private func stabilityWindowElapsed(generation: Int) {
        let watchdogBefore = machine.watchdogGeneration
        let stabilityBefore = machine.stabilityGeneration
        let graceBefore = machine.graceGeneration
        let dwellBefore = machine.dwellGeneration
        machine.stabilityElapsed(generation: generation)
        resyncTimers(watchdogBefore: watchdogBefore, stabilityBefore: stabilityBefore, graceBefore: graceBefore, dwellBefore: dwellBefore)
        publish()
    }

    // MARK: - Store-side start-grace timer (#499)

    /// (Re)arm — or cancel — the store-side start-grace timer to match the pure core's current intent, the
    /// not-running-split analogue of `rearmStabilityTimer`: while a COLD connect-refused is held as
    /// `.starting` (`machine.isAwaitingStartGrace`), sleep the grace then feed the (generation-guarded)
    /// elapse back, which escalates the held `.starting` to the durable `.notRunning`. The daemon
    /// connecting before the grace elapses supersedes the token (the pure core bumps `graceGeneration` on
    /// the `.starting`→`.live` transition), so a daemon that was merely coming up never reaches the
    /// escalate — it goes straight to connected.
    private func rearmGraceTimer() {
        graceTask?.cancel()
        guard machine.isAwaitingStartGrace else { graceTask = nil; return }
        let generation = machine.graceGeneration
        let window = startGraceWindow
        graceTask = Task { [weak self] in
            do { try await Task.sleep(for: window) } catch { return }   // cancelled → drop
            self?.startGraceElapsed(generation: generation)
        }
    }

    /// The start grace fired: fold the elapse into the machine (a superseded token — the daemon connected
    /// mid-grace — is ignored there) and re-publish, in case it escalated `.starting` to `.notRunning`.
    private func startGraceElapsed(generation: Int) {
        let watchdogBefore = machine.watchdogGeneration
        let stabilityBefore = machine.stabilityGeneration
        let graceBefore = machine.graceGeneration
        let dwellBefore = machine.dwellGeneration
        machine.graceElapsed(generation: generation)
        resyncTimers(watchdogBefore: watchdogBefore, stabilityBefore: stabilityBefore, graceBefore: graceBefore, dwellBefore: dwellBefore)
        publish()
    }

    // MARK: - Store-side warm-dwell timer (#526)

    /// (Re)arm — or cancel — the store-side warm-dwell timer to match the pure core's current intent, the
    /// reconnecting-split analogue of `rearmGraceTimer`: while a WARM drop is held as `.reconnecting`
    /// (`machine.isAwaitingWarmDwell`), sleep the dwell then feed the (generation-guarded) elapse back, which
    /// escalates the held `.reconnecting` to the durable `.disconnected`. The daemon reconnecting before the
    /// dwell elapses supersedes the token (the pure core bumps `dwellGeneration` on the `.reconnecting`→`.live`
    /// transition), so a routine restart / wake blip that comes back inside the window never reaches the
    /// escalate — it goes straight back to connected. System sleep also supersedes it (`systemWillSleep`
    /// suspends the dwell → `isAwaitingWarmDwell` false → this cancels), so a lid-close never escalates.
    private func rearmDwellTimer() {
        dwellTask?.cancel()
        guard machine.isAwaitingWarmDwell else { dwellTask = nil; return }
        let generation = machine.dwellGeneration
        let window = warmDwellWindow
        dwellTask = Task { [weak self] in
            do { try await Task.sleep(for: window) } catch { return }   // cancelled → drop
            self?.warmDwellElapsed(generation: generation)
        }
    }

    /// The warm dwell fired: fold the elapse into the machine (a superseded token — the daemon reconnected,
    /// or the system slept, mid-dwell — is ignored there) and re-publish, in case it escalated `.reconnecting`
    /// to `.disconnected` (the calm "…" → the loud "!").
    private func warmDwellElapsed(generation: Int) {
        let watchdogBefore = machine.watchdogGeneration
        let stabilityBefore = machine.stabilityGeneration
        let graceBefore = machine.graceGeneration
        let dwellBefore = machine.dwellGeneration
        machine.dwellElapsed(generation: generation)
        resyncTimers(watchdogBefore: watchdogBefore, stabilityBefore: stabilityBefore, graceBefore: graceBefore, dwellBefore: dwellBefore)
        publish()
    }

    // MARK: - Sleep/wake gating of the warm dwell (#526)

    /// The system is about to sleep — SUSPEND the warm dwell so a benign sleep-time disconnect cannot
    /// escalate to Attention (the BLOCKING falsifier: a lid closed overnight is a long, 100%-benign
    /// disconnect resolving in ~1 s on wake — running the dwell through sleep would open on a FALSE
    /// Attention every morning). Folds `machine.systemWillSleep()` in and re-syncs so the in-flight dwell
    /// timer is cancelled. Wired to `NSWorkspace.willSleepNotification` in `main.swift`; callable directly
    /// from tests (hermetic, no real sleep). `internal` (not `private`) for that test access.
    func systemWillSleep() {
        let watchdogBefore = machine.watchdogGeneration
        let stabilityBefore = machine.stabilityGeneration
        let graceBefore = machine.graceGeneration
        let dwellBefore = machine.dwellGeneration
        machine.systemWillSleep()
        resyncTimers(watchdogBefore: watchdogBefore, stabilityBefore: stabilityBefore, graceBefore: graceBefore, dwellBefore: dwellBefore)
        publish()
    }

    /// The system just woke — RESUME the warm dwell, RESET afresh (treat wake as a fresh connect). Folds
    /// `machine.systemDidWake()` in and re-syncs so a still-`.reconnecting` drop re-arms a FULL fresh dwell
    /// window from wake (a genuinely-benign wake blip resolves inside it; a truly-dead daemon reaches
    /// Attention one dwell after wake). Wired to `NSWorkspace.didWakeNotification` in `main.swift`; callable
    /// directly from tests (hermetic, no real wake). `internal` (not `private`) for that test access.
    func systemDidWake() {
        let watchdogBefore = machine.watchdogGeneration
        let stabilityBefore = machine.stabilityGeneration
        let graceBefore = machine.graceGeneration
        let dwellBefore = machine.dwellGeneration
        machine.systemDidWake()
        resyncTimers(watchdogBefore: watchdogBefore, stabilityBefore: stabilityBefore, graceBefore: graceBefore, dwellBefore: dwellBefore)
        publish()
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

#if DEBUG
extension WatchStatusStore {
    /// Tooling / preview only (`--render-panel`, SwiftUI previews): a store pinned to a fixed derived
    /// state WITHOUT the transport, so `StatusPanelView` can be rendered offscreen (`ImageRenderer`) for
    /// design-parity review against the mock. NOT a production path — the real state is machine-derived
    /// from the wire, never set directly. Same-file so it can set the `private(set)` projection.
    static func preview(state: ConnectionState, rows: [AccountRow],
                        nextSwap: NextSwap?, generatedAt: Int64?,
                        canonicalScrub: CanonicalScrub? = nil,
                        keychainLocked: Bool = false,
                        systemicRefreshFailure: UInt32? = nil) -> WatchStatusStore {
        let store = WatchStatusStore()
        store.connectionState = state
        store.rows = rows
        store.nextSwap = nextSwap
        store.generatedAt = generatedAt
        store.canonicalScrub = canonicalScrub
        store.keychainLocked = keychainLocked
        store.systemicRefreshFailure = systemicRefreshFailure
        return store
    }
}
#endif
