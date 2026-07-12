// Copyright (c) 2026 Oleksii PELYKH
// SPDX-License-Identifier: MIT

// The pure, synchronous decision core of the menu-bar status store (issue #324): the "honest-state
// machine" (D2). It folds the transport's `AsyncStream<TransportEvent>` (#323) plus the wire decoder
// (#322) into the single view state the UI renders — a `ConnectionState`, the `[AccountRow]` roster,
// `nextSwap` / `refreshEnabled` / `generatedAt`, and a `PresentationState` (glyph + a11y label) for
// the status item.
//
// It is the UI-side analogue of the daemon's no-torn-swap / no-false-healthy discipline (ADR-0003):
// the crown-jewel invariant is **never render healthy on a degraded or absent daemon** (anti-#137,
// `design-menubar.md` D-UX-state). That invariant is enforced STRUCTURALLY here, not by convention:
// `ConnectionState` is a PURE function of two internal axes —
//
//   * `liveness`      — is the connection currently delivering VALID data? (from transport events)
//   * `snapshotClass` — what did the last decoded snapshot say? (none / healthy / empty / unsupported)
//
// and the healthy verdict (`.connected`, the only healthy glyph) is reachable on EXACTLY ONE
// combination: `liveness == .live` AND `snapshotClass == .healthy`. Every degraded or absent path
// (initial, awaiting-first-snapshot, stale, disconnected, empty, unsupported) resolves to a
// non-healthy state by construction — there is no assignment that sets "healthy" from a drop, a
// silence, an undecodable line, or a bare reconnect.
//
// Functional-core / imperative-shell, mirroring `WatchStateMachine` + `WatchTransport`: ALL the
// honest-state logic lives here as a value type with no I/O, no clock, and no concurrency, so every
// transition is exhaustively unit-testable synchronously. `WatchStatusStore` is the thin `@MainActor`
// `ObservableObject` shell that pumps this machine from the injected event stream and mirrors its
// derived state into `@Published` properties + the presentation stream.
//
// SCOPE: the #324 MINIMAL honest-state baseline — connecting / connected / empty-roster / stale /
// disconnected / unsupported — PLUS the #169 crash-loop healthy-flash debounce (the crown-jewel
// anti-#137 mitigation: a crash-looping daemon is held in `.crashLooping`, never flickered healthy)
// PLUS the #499 not-running / daemon-starting split: a COLD connect-refused (no live connection EVER
// held this session — discriminated by `hasEverConnected`, NOT by transport enrichment) reads as the
// transient `.starting` within a short start grace, then escalates to the durable `.notRunning` once
// the grace elapses still refused — both distinct from a WARM `.disconnected` drop (a live connection
// held, then lost). The REMAINING degraded-state-map facets are tracked siblings, NOT this file:
// keychain-locked (needs a daemon-side wire signal — the watch stream carries none) and the rich
// version-skew upgrade UX.
//
// STORE-SIDE STALENESS WATCHDOG (#344): staleness must NOT depend solely on the transport's
// byte-level liveness timer. The transport re-arms that timer on ANY non-empty line — garbage,
// `{"error":…}`, unknown frames included — so a daemon holding the connection open while streaming
// only UNDECODABLE frames (spaced < the transport's window) after one healthy snapshot would keep
// the transport perpetually live → it never emits `.stale` → the last healthy snapshot is retained
// → the menubar renders healthy on a garbage-emitting daemon. This machine therefore runs its OWN
// generation-guarded watchdog keyed on VALID DECODABLE FRAMES (a `snapshot` or `heartbeat`), NOT raw
// bytes: `watchdogElapsed(generation:)` downgrades a live connection to `.stale` once the window has
// passed with no valid frame. It is an ADDITIONAL, independent path to `.stale` that fires even while
// the transport still thinks the connection is live; the transport's own `.stale` continues to work
// unchanged. The pure core stays clock-free (mirroring `WatchStateMachine`) — the `WatchStatusStore`
// shell performs the real `Task.sleep(for:)` and feeds the elapse back.

// MARK: - The honest connection states (the UI's single source of truth)

/// The connection state the UI renders — the D2 minimal honest-state set. Distinct cases the UI must
/// never conflate: "connected with zero accounts" (`emptyRoster`, an onboarding state) is NOT
/// "daemon absent" (`disconnected`); a silent-but-open connection (`stale`) is NOT a drop.
enum ConnectionState: Equatable, Sendable {
    /// (Re)connecting: no VALID snapshot yet — either before the first connect, or connected at the
    /// socket level but still awaiting a fresh snapshot (a bare reconnect re-enters here so a stale
    /// pre-drop roster can never be shown as live). Never healthy.
    case connecting

    /// Live: a fresh, schema-supported snapshot with ≥ 1 account is in hand. THE ONLY healthy state.
    case connected

    /// Live, schema-supported, but the daemon reports ZERO accounts — the first-run / empty-roster
    /// state (B-014). Distinct from `disconnected`: the daemon is present and answering.
    case emptyRoster

    /// The connection is still open but the daemon has gone quiet past the liveness window (transport
    /// `.stale`). Last-good data is shown MARKED stale, never as live.
    case stale

    /// The connection dropped (transport `.disconnected`). Last-good data is shown MARKED stale,
    /// never as live; the transport reconnects with backoff on its own.
    case disconnected(reason: String)

    /// The daemon speaks a wire contract this client cannot safely read (`schema_version.major`
    /// mismatch — `!WireContract.isSupported`). Numbers are refused rather than mis-rendered. Minimal
    /// only; the rich version-skew UX is a #169 sibling.
    case unsupported

    /// The daemon is crash-looping (issue #169): it (re)connected and served a fresh snapshot but keeps
    /// DROPPING before the connection survives the stability window — the repeated-launchd-restart
    /// fault. A DISTINCT state that persists a fault shape and NEVER renders healthy — the crown-jewel
    /// anti-#137 "debounce the healthy-flash between launchd restarts" (design-menubar D-UX-state). The
    /// held snapshot's numbers are refused until the daemon stays up. Distinct from `.disconnected` (a
    /// single drop) and `.connecting` (a benign first/single (re)connect still awaiting stabilization).
    case crashLooping

    /// The daemon is being (re)connected but the socket has REFUSED the connect and NO live connection has
    /// ever been held this session, WITHIN a short start grace (issue #499) — a transient "coming up" state
    /// that self-resolves the moment the daemon accepts the socket. Distinct from `.connecting` (the first
    /// connect still IN FLIGHT, no refusal observed yet) — they share the benign forming glance but carry
    /// different panel banners. NEVER healthy. Escalates to `.notRunning` if the grace elapses still refused.
    case starting

    /// A connect-refused that has PERSISTED past the start grace with no live connection ever held (issue
    /// #499) — the durable "no daemon" state: the one that WOULD host a Start-daemon affordance (launch-at-
    /// login is #170, deferred → degrades to an inert explanatory banner, no button). Distinct from
    /// `.disconnected` (a socket that WAS connected and then dropped — a warm loss, not an absent daemon)
    /// and from `.starting` (still within the hopeful grace). NEVER healthy.
    case notRunning

    /// Whether this is the one healthy state. The never-healthy-when-dead invariant is exactly
    /// "`isHealthy` is false on every degraded or absent path".
    var isHealthy: Bool {
        if case .connected = self { return true }
        return false
    }
}

// MARK: - Presentation (the glance surface the status item consumes)

/// The abstract glance glyph — a SHAPE-coded health signal, decoupled from any concrete SF Symbol
/// (that mapping is status-item rendering, later UI). One-to-one with `ConnectionState` for the D2
/// baseline; kept a separate type so #169 can grow per-account health/attention glyphs without
/// touching the connection semantics.
enum StatusGlyph: Equatable, Sendable {
    case connecting
    /// The ONE healthy glyph — emitted only for `ConnectionState.connected`.
    case healthy
    case empty
    case stale
    case disconnected
    case unsupported
    /// The crash-loop FAULT glyph (issue #169) — a persistent fault shape, emitted for
    /// `ConnectionState.crashLooping`; never the healthy glyph.
    case crashLooping
    /// The daemon-starting forming glyph (issue #499) — a STATIC "coming up" shape emitted for
    /// `ConnectionState.starting` (never a fake spinner; the app fakes no progress). A benign forming
    /// glance, never healthy.
    case starting
    /// The daemon-not-running glyph (issue #499) — a distinct "no daemon" shape emitted for
    /// `ConnectionState.notRunning`; never healthy, and distinct from `.disconnected` (a dropped socket).
    case notRunning
}

/// What the status item renders: the glance `glyph` plus a VoiceOver `accessibilityLabel`. The label
/// is a plain, factual sentence (design-menubar a11y: shape-coded glyph + a spoken label per state).
struct PresentationState: Equatable, Sendable {
    let glyph: StatusGlyph
    let accessibilityLabel: String

    /// Derive the glance from a connection state (+ the account count, which the `.connected` label
    /// speaks). Pure — the same input always yields the same presentation.
    static func make(for state: ConnectionState, accountCount: Int) -> PresentationState {
        switch state {
        case .connecting:
            return PresentationState(glyph: .connecting,
                                     accessibilityLabel: "Sessiometer: connecting to the daemon…")
        case .connected:
            let plural = accountCount == 1 ? "" : "s"
            return PresentationState(glyph: .healthy,
                                     accessibilityLabel: "Sessiometer: live — \(accountCount) account\(plural)")
        case .emptyRoster:
            return PresentationState(glyph: .empty,
                                     accessibilityLabel: "Sessiometer: connected — no accounts configured")
        case .stale:
            return PresentationState(glyph: .stale,
                                     accessibilityLabel: "Sessiometer: data may be stale — the daemon has gone quiet")
        case .disconnected:
            return PresentationState(glyph: .disconnected,
                                     accessibilityLabel: "Sessiometer: disconnected — the daemon is not responding")
        case .unsupported:
            return PresentationState(glyph: .unsupported,
                                     accessibilityLabel: "Sessiometer: daemon version unsupported — update required")
        case .crashLooping:
            return PresentationState(glyph: .crashLooping,
                                     accessibilityLabel: "Sessiometer: the daemon is restarting repeatedly — holding status until it stays up")
        case .starting:
            return PresentationState(glyph: .starting,
                                     accessibilityLabel: "Sessiometer: the daemon is starting…")
        case .notRunning:
            return PresentationState(glyph: .notRunning,
                                     accessibilityLabel: "Sessiometer: the daemon is not running")
        }
    }
}

// MARK: - The per-account presentation row

/// One account, projected from the wire `AccountStatusLine` into a stable, view-facing row — the
/// store is the single source of truth, so cross-field derivation (e.g. the next-swap-target marker)
/// happens HERE, not in the view. A lean D2 projection: identity, the swap-relevant flags, and usage;
/// the deeper refresh-health internals (rotation, access-expiry) belong to #169's rich health map.
struct AccountRow: Identifiable, Equatable, Sendable {
    /// The redacted operator handle doubles as the SwiftUI list identity (issue #15 — never an email).
    var id: String { label }
    let label: String
    let isActive: Bool
    let isEnabled: Bool
    let isQuarantined: Bool
    /// Whether a quarantined account is mid-recovery (issue #109) — the panel softens a DEAD account's
    /// `claude /login` cue to `recovering` so the operator holds rather than re-authing (issue #326 AC:
    /// "recovering distinct from dead"). Orthogonal to `auth`, exactly as `src/cli.rs` `health_cell`
    /// reads `account.recovering` alongside the rollup.
    let isRecovering: Bool
    let auth: CredentialHealth?
    let sessionPct: UInt8?
    let weeklyPct: UInt8?
    let sessionResetsAt: Int64?
    let weeklyResetsAt: Int64?
    let weeklyExhausted: Bool
    /// Whether the daemon's `next_swap` names THIS account as the swap candidate — a store-level
    /// cross-field derivation the panel reads to mark the row.
    let isNextSwapTarget: Bool

    /// Project a whole snapshot's accounts into rows, resolving each account's next-swap-target flag
    /// against the snapshot's `next_swap` candidate.
    static func rows(from status: VersionedStatus) -> [AccountRow] {
        let targetLabel: String?
        if case .target(let to, _) = status.nextSwap { targetLabel = to } else { targetLabel = nil }
        return status.accounts.map { account in
            AccountRow(
                label: account.label,
                isActive: account.active,
                isEnabled: account.enabled,
                isQuarantined: account.quarantined,
                isRecovering: account.recovering,
                auth: account.auth,
                sessionPct: account.sessionPct,
                weeklyPct: account.weeklyPct,
                sessionResetsAt: account.sessionResetsAt,
                weeklyResetsAt: account.weeklyResetsAt,
                weeklyExhausted: account.weeklyExhausted,
                isNextSwapTarget: account.label == targetLabel)
        }
    }
}

// MARK: - The outcome of decoding one line (for the shell to log; also a test hook)

/// What happened when a `.line` was folded in — returned so the imperative shell can `os_log`
/// appropriately (and so tests can assert defensive handling explicitly). Non-`.line` events return
/// `nil` from `apply`.
enum LineOutcome: Equatable, Sendable {
    /// A schema-supported snapshot was applied (roster refreshed).
    case appliedSnapshot
    /// A schema-supported heartbeat refreshed freshness/liveness only (no roster change).
    case appliedHeartbeat
    /// A snapshot OR heartbeat carried an unsupported `schema_version.major` — degraded to
    /// `.unsupported`, numbers refused.
    case unsupportedSchema
    /// A decoded-but-unrecognized frame (future `type`, or a `type`-less line such as a pre-#164
    /// `{"error":…}`): ignored by a forward-compatible client. Does NOT prove valid liveness.
    case ignoredUnknownFrame
    /// The line failed to decode (`parseWatchFrame` threw — a non-JSON line, or a malformed body):
    /// non-fatal, logged + skipped. Does NOT prove valid liveness, so it never clears staleness.
    case ignoredUndecodable(String)
}

extension LineOutcome {
    /// Whether this line was a VALID DECODABLE FRAME — a `snapshot` or `heartbeat` (schema-supported
    /// or not; an unsupported-major frame still decoded as a real protocol frame, proving the daemon
    /// is speaking the wire contract) — and therefore proves liveness and RESETS the store-side
    /// valid-frame watchdog (#344). An undecodable line or an unknown/`type`-less frame does NOT: that
    /// is precisely the honesty the watchdog enforces — raw bytes that re-arm the transport's timer
    /// must not masquerade as valid daemon liveness in the store.
    var resetsValidFrameWatchdog: Bool {
        switch self {
        case .appliedSnapshot, .appliedHeartbeat, .unsupportedSchema: return true
        case .ignoredUnknownFrame, .ignoredUndecodable: return false
        }
    }
}

// MARK: - The machine

/// The pure honest-state reducer. Fold transport events in with `apply`; read the derived
/// `connectionState`, `rows`, `nextSwap`, `refreshEnabled`, `generatedAt`, and `presentation` out.
struct HonestStateMachine {

    /// Is the connection currently delivering VALID data? Set to `.live` only by a successful connect
    /// or a VALID decoded frame — never by an undecodable/unknown line, so the store's honesty tracks
    /// valid DATA, not raw bytes.
    private enum Liveness: Equatable {
        case initial                      // before the first connect
        case starting                     // cold connect-refused, no live connection ever held, within the start grace (#499)
        case notRunning                   // cold connect-refused past the start grace, no live connection ever held (#499)
        case live                         // connected and delivering valid frames
        case stale                        // connection open, daemon silent past the liveness window
        case disconnected(reason: String) // the socket dropped (a live connection was held, then lost)
    }

    /// What the last decoded SNAPSHOT said. Reset to `.none` on every (re)connect so a healthy verdict
    /// is only ever earned by a FRESH supported snapshot — never resurrected from a pre-drop roster.
    private enum SnapshotClass: Equatable {
        case none          // no snapshot applied on the current connection yet
        case healthy       // supported snapshot, ≥ 1 account
        case empty         // supported snapshot, zero accounts
        case unsupported   // breaking-major snapshot/heartbeat — numbers refused
    }

    private var liveness: Liveness = .initial
    private var snapshotClass: SnapshotClass = .none

    /// Whether a LIVE connection has ever been held this session (any transition of `liveness` to `.live`).
    /// It discriminates a COLD connect-refused (never connected → the daemon-absent `.starting`/`.notRunning`
    /// track, #499) from a WARM drop (a connection WAS held, then lost → `.disconnected`, the socket-dropped
    /// track). Set once, never cleared: a session that has ever reached the daemon is past the cold-start
    /// question for good, so a later refused reconnect reads as a drop, not "never running".
    private var hasEverConnected = false

    /// The store-side valid-frame watchdog token (#344), mirroring `WatchStateMachine`'s
    /// `livenessGeneration`: bumped every time the watchdog is (re)armed by a valid decodable frame
    /// or a (re)connect, and every time it is invalidated by a drop / transport-stale. A fired
    /// `watchdogElapsed` whose `generation` ≠ this is a superseded timer and is ignored. The shell
    /// re-arms its real `Task.sleep` timer whenever this value changes across an `apply`.
    private(set) var watchdogGeneration = 0

    /// Whether a valid-frame watchdog should currently be running: only on a LIVE connection, where a
    /// valid frame is expected within the window. `.initial` / `.stale` / `.disconnected` are already
    /// non-live, so the shell cancels (not re-arms) its timer when this is `false`.
    var isWatchingForValidFrames: Bool { liveness == .live }

    // MARK: - Crash-loop healthy-flash debounce (#169)

    /// At/above this many consecutive UNSTABLE reconnects (a held snapshot that dropped BEFORE surviving
    /// the stability window), a held snapshot reads as `.crashLooping` rather than a benign `.connecting`.
    /// 2 = "dropped-before-stable twice running" — past a single clean restart, which must NOT cry
    /// crash-loop. The honest floor is "> 1 unstable reconnect"; tunable.
    static let crashLoopThreshold = 2

    /// Whether the connection has dropped at least once this session. The debounce is ARMED only after
    /// the first drop: a cold clean first connect promotes to healthy IMMEDIATELY (the happy path — and
    /// every existing immediate-healthy test — is unchanged), and only a RECONNECT (a "launchd restart"
    /// in the design's words) must earn healthy by surviving the stability window.
    private var hasEverDisconnected = false

    /// Whether the CURRENT live connection has survived the stability window since it (re)connected.
    /// Reset to `false` on every `.connected` (a (re)connect must re-earn stability); set `true` by
    /// `stabilityElapsed`. Only load-bearing post-reconnect (once `hasEverDisconnected`).
    private var stabilizedThisConnection = false

    /// Consecutive reconnects whose held healthy snapshot DROPPED before surviving the stability window
    /// — the clock-free crash-loop signal ("restarted N× … holding until it stays up"). Reset to 0 once
    /// a connection stabilizes (the daemon stayed up). At/above `crashLoopThreshold` a held snapshot is
    /// `.crashLooping`; below it, a benign `.connecting`.
    private(set) var consecutiveUnstableReconnects = 0

    /// The stability-window timer token (#169), mirroring `watchdogGeneration`: bumped whenever the
    /// stabilizing condition is ENTERED (arm), invalidated by a drop / stale (cancel), or consumed by
    /// `stabilityElapsed`. A fired `stabilityElapsed` whose `generation` ≠ this is superseded → ignored.
    /// The shell re-arms its real stability timer whenever this value changes across a mutation.
    private(set) var stabilityGeneration = 0

    /// Whether a held healthy snapshot is currently awaiting the stability window (the post-reconnect
    /// debounce is active). The shell runs the real stability `Task.sleep` exactly while this is `true`.
    /// FALSE on the first connect (`!hasEverDisconnected`) — so the clean-start happy path is immediate.
    var isStabilizing: Bool {
        liveness == .live && snapshotClass == .healthy && hasEverDisconnected && !stabilizedThisConnection
    }

    // MARK: - Start-grace: split daemon-starting (transient) from not-running (durable) (#169/#499)

    /// The start-grace timer token (#499), mirroring `watchdogGeneration` / `stabilityGeneration`: bumped
    /// whenever a COLD connect-refused first enters `.starting` (ARM the grace) and whenever that grace is
    /// LEFT — the daemon finally connected (`apply`), or the grace elapsed to the durable `.notRunning`
    /// (`graceElapsed`). A fired `graceElapsed` whose `generation` ≠ this is a superseded timer and is
    /// ignored. The shell re-arms its real `Task.sleep` timer whenever this value changes across a mutation.
    private(set) var graceGeneration = 0

    /// Whether a COLD connect-refused is currently within the start grace (liveness `.starting`): the shell
    /// runs the real grace `Task.sleep` exactly while this is `true`. `.initial` (first connect in flight),
    /// `.notRunning` (grace already elapsed), and every connected/stale/dropped state are NOT awaiting, so
    /// the shell cancels (not re-arms) its grace timer when this is `false`.
    var isAwaitingStartGrace: Bool {
        if case .starting = liveness { return true }
        return false
    }

    /// The derived view outputs (mirrored into the store's `@Published` surface).
    private(set) var rows: [AccountRow] = []
    private(set) var nextSwap: NextSwap?
    private(set) var refreshEnabled: Bool?
    private(set) var generatedAt: Int64?
    /// The daemon-level shared-canonical scrub rollup (#469, wire #516), carried from the last applied
    /// snapshot exactly as `nextSwap` is: a fleet-wide lockout NO per-account `auth` reflects (each row
    /// can read healthy while the shared `Claude Code-credentials` item sits emptied). RETAINED across a
    /// drop (shown under the dimmed last-known render, like `nextSwap`) and REFUSED with the other numbers
    /// on an unsupported-major frame. `nil` when the shared canonical is healthy — or a pre-#516 daemon
    /// omits the wire key (`decodeIfPresent`), so a healthy/legacy daemon never renders a scrub banner.
    private(set) var canonicalScrub: CanonicalScrub?

    /// The honest connection state — a PURE function of `(liveness, snapshotClass)`. This is where the
    /// never-healthy-when-dead invariant lives: `.connected` is returned on exactly one combination.
    var connectionState: ConnectionState {
        switch liveness {
        case .disconnected(let reason):
            return .disconnected(reason: reason)
        case .starting:
            return .starting
        case .notRunning:
            return .notRunning
        case .stale:
            return .stale
        case .initial:
            return .connecting
        case .live:
            switch snapshotClass {
            case .none:        return .connecting     // connected, but no fresh snapshot yet
            case .unsupported: return .unsupported
            case .empty:       return .emptyRoster
            case .healthy:
                // The crash-loop healthy-flash debounce (#169): a post-reconnect snapshot is HELD until
                // its connection survives the stability window (`isStabilizing`). During the hold it is
                // NEVER healthy — repeated unstable reconnects read as the `.crashLooping` fault shape,
                // a first/single restart as a benign `.connecting`. A cold first connect (or a
                // stabilized reconnect) is not stabilizing → the sole healthy path.
                guard !isStabilizing else {
                    return consecutiveUnstableReconnects >= Self.crashLoopThreshold ? .crashLooping : .connecting
                }
                return .connected      // ← the sole healthy path
            }
        }
    }

    /// The glance presentation derived from the current state.
    var presentation: PresentationState {
        PresentationState.make(for: connectionState, accountCount: rows.count)
    }

    /// Fold one transport event into the state. Returns the `LineOutcome` for a `.line` event (so the
    /// shell can log it), `nil` otherwise.
    mutating func apply(_ event: TransportEvent) -> LineOutcome? {
        // Capture the pre-event stabilizing state so we can (a) count an UNSTABLE reconnect when a held
        // snapshot drops and (b) arm/invalidate the stability timer on a TRANSITION only — never on a
        // repeat frame within one hold, which must not reset the window (#169).
        let wasStabilizing = isStabilizing
        // Capture the pre-event start-grace state too, so the grace timer is armed on the false→true
        // transition (first cold refusal) and cancelled on true→false (connected), never re-armed on a
        // repeat refusal within one grace — mirroring the `wasStabilizing` transition-guard (#499).
        let wasAwaitingStartGrace = isAwaitingStartGrace
        let outcome: LineOutcome?
        switch event {
        case .connected:
            // Socket up + subscribed, but no FRESH snapshot yet. Reset the snapshot classification so
            // a reconnect re-enters `.connecting` and can never resurrect a healthy glyph from the
            // pre-drop roster — the roster rows are RETAINED (shown dimmed under `.connecting`, not
            // blanked) until a fresh snapshot confirms them. A (re)connect must also RE-EARN stability
            // (#169): clear `stabilizedThisConnection` so a post-drop connection is debounced afresh.
            liveness = .live
            hasEverConnected = true        // a live connection was held → past the cold-start question (#499)
            snapshotClass = .none
            stabilizedThisConnection = false
            watchdogGeneration += 1        // ARM: expect a valid frame within the window (#344)
            outcome = nil
        case .disconnected(let reason):
            // The transport emits `.disconnected` for BOTH a connect-refused (daemon absent / coming up)
            // AND a drop of an established connection. Split them on lineage (#499): a live connection ever
            // held ⇒ WARM drop; never held ⇒ COLD connect-refused, the daemon-absent track.
            if hasEverConnected {
                // WARM: a live connection was held, then lost — the socket-dropped state (unchanged). A
                // drop while a held snapshot was still stabilizing = an UNSTABLE reconnect: the clock-free
                // crash-loop signal (#169). Count it BEFORE mutating liveness (which flips `isStabilizing`).
                if wasStabilizing { consecutiveUnstableReconnects += 1 }
                // Last-good rows/nextSwap/generatedAt are RETAINED but the state is now `.disconnected`
                // (never live). The transport reconnects with backoff on its own. Also reset the snapshot
                // classification: the roster is no longer confirmed, so healthy must be re-earned by a
                // FRESH snapshot — this makes the never-healthy invariant hold STRUCTURALLY even if a
                // heartbeat were somehow to arrive before the reconnect `.connected` (the transport orders
                // `.connected` first, but the invariant must not depend on that).
                liveness = .disconnected(reason: reason)
                snapshotClass = .none
                hasEverDisconnected = true     // ARM the debounce for every subsequent reconnect (#169)
                watchdogGeneration += 1        // INVALIDATE: already non-live, no watchdog needed (#344)
            } else {
                // COLD: no live connection has EVER been held this session — the connect is being REFUSED
                // (daemon absent, or still coming up), NOT a drop. Enter `.starting` on the FIRST refusal
                // (the apply-level start-grace delta below arms the grace); STAY on repeat refusals within
                // the backoff loop, so the grace timer alone owns the escalation to `.notRunning` — a
                // repeat refusal must not keep resetting the window. Deliberately does NOT touch
                // `hasEverDisconnected`: a daemon we are merely WAITING for must promote to healthy
                // IMMEDIATELY when it finally connects (a clean cold start), never be debounced as a
                // crash-loop reconnect (the load-bearing #499 ↔ #169 interaction). There are no rows to
                // retain (none were ever shown) and no valid-frame watchdog to invalidate (never was live).
                switch liveness {
                case .starting, .notRunning:
                    break                      // already in the cold-refused track — the grace owns starting → not-running
                default:                       // .initial — the first refusal
                    liveness = .starting
                }
            }
            outcome = nil
        case .stale:
            // Connection still open, daemon silent: last-good data retained but MARKED stale.
            liveness = .stale
            watchdogGeneration += 1        // INVALIDATE: transport already declared stale (#344)
            outcome = nil
        case .line(let line):
            let lineOutcome = applyLine(line)
            // RE-ARM the watchdog ONLY for a valid decodable frame; an undecodable/unknown line does
            // not advance the token, so the timer armed by the last valid frame keeps counting down —
            // that is how continuous garbage after a healthy snapshot still trips `.stale` (#344).
            if lineOutcome.resetsValidFrameWatchdog { watchdogGeneration += 1 }
            outcome = lineOutcome
        }
        // Arm (false→true) or invalidate (true→false) the stability timer ONLY on a transition of the
        // stabilizing condition — mirroring the watchdog's generation bump. Staying stabilizing (a
        // repeat snapshot/heartbeat within one hold) does NOT bump, so the window keeps counting (#169).
        if isStabilizing != wasStabilizing { stabilityGeneration += 1 }
        // Likewise arm (false→true, first cold refusal) or cancel (true→false, the daemon connected) the
        // start grace ONLY on a transition — a repeat refusal within one grace leaves it counting (#499).
        if isAwaitingStartGrace != wasAwaitingStartGrace { graceGeneration += 1 }
        return outcome
    }

    /// Fold in an elapsed store-side valid-frame watchdog (#344): "no VALID decodable frame in the
    /// window → `.stale`", the store's own staleness path, independent of the transport's byte-level
    /// liveness timer. Generation-guarded exactly like `WatchStateMachine`'s liveness timer — a token
    /// superseded by a later valid frame (or a connect / drop / transport-stale) is ignored — and it
    /// only downgrades a currently-LIVE connection, so it can never override an already-`.disconnected`
    /// / `.stale` / `.initial` state, nor fire twice. This closes the honest-state hole where a daemon
    /// holding the connection open while streaming only undecodable/unknown frames (which re-arm the
    /// transport's byte timer but are not valid liveness here) would otherwise hold the last healthy
    /// snapshot forever. A later valid frame (`snapshot`/`heartbeat`) re-arms and un-stales as before.
    mutating func watchdogElapsed(generation: Int) {
        guard generation == watchdogGeneration else { return }  // superseded by a later frame → ignore
        guard liveness == .live else { return }                 // only a live connection can go stale
        let wasStabilizing = isStabilizing
        liveness = .stale
        // Going stale ENDS any in-flight stabilization hold — invalidate its timer so the shell cancels
        // it, mirroring the transitions in `apply` (#169).
        if isStabilizing != wasStabilizing { stabilityGeneration += 1 }
    }

    /// Fold in an elapsed stability window (#169): the held snapshot's connection SURVIVED the window,
    /// so the daemon stayed up — promote it. Mark this connection stabilized (`connectionState` →
    /// `.connected`) and clear the crash-loop churn. Generation-guarded exactly like `watchdogElapsed`
    /// (a token superseded by a later drop / re-arm is ignored) and gated on actually stabilizing, so it
    /// can never manufacture a healthy view from a dropped / stale / already-stabilized state, nor fire
    /// twice. The clock lives in the `WatchStatusStore` shell, which performs the real `Task.sleep` and
    /// feeds the elapse back — the pure core stays clock-free (mirroring `watchdogElapsed`).
    mutating func stabilityElapsed(generation: Int) {
        guard generation == stabilityGeneration else { return }  // superseded → ignore
        guard isStabilizing else { return }                      // only a held snapshot can stabilize
        stabilizedThisConnection = true
        consecutiveUnstableReconnects = 0
        stabilityGeneration += 1     // consume: the hold is over (`isStabilizing` is now false)
    }

    /// Fold in an elapsed start grace (#499): a COLD connect-refused has stayed refused for the WHOLE grace
    /// with no live connection ever held — so the daemon is absent, not merely coming up. Promote the
    /// transient `.starting` to the durable `.notRunning`. Generation-guarded exactly like `watchdogElapsed`
    /// / `stabilityElapsed` (a token superseded by the daemon connecting, or by any later re-arm, is
    /// ignored) and gated on actually still being within the grace, so it can never manufacture
    /// `.notRunning` from a connected / dropped / already-not-running state, nor fire twice. The clock lives
    /// in the `WatchStatusStore` shell, which performs the real `Task.sleep` and feeds the elapse back —
    /// the pure core stays clock-free (mirroring `watchdogElapsed`).
    mutating func graceElapsed(generation: Int) {
        guard generation == graceGeneration else { return }         // superseded (e.g. the daemon connected) → ignore
        guard case .starting = liveness else { return } // only a still-starting connection escalates
        liveness = .notRunning
        graceGeneration += 1     // consume: the grace is over (`isAwaitingStartGrace` is now false)
    }

    // MARK: - Line handling (decode-defensive)

    private mutating func applyLine(_ line: String) -> LineOutcome {
        let frame: WatchFrame
        do {
            frame = try parseWatchFrame(line)
        } catch {
            // A pre-#164 daemon streams `{"error":"unknown command"}` (valid JSON, no `type` → an
            // unknown frame, handled below) — but a genuinely malformed / non-JSON line throws HERE.
            // Non-fatal: skip it, and crucially do NOT mark liveness `.live` — an undecodable line is
            // not proof of valid data, so it must not clear an earlier `.stale` into a healthy view.
            return .ignoredUndecodable(String(describing: error))
        }
        switch frame {
        case .snapshot(let status):
            return applySnapshot(status)
        case .heartbeat(let generatedAt, let schemaVersion):
            return applyHeartbeat(generatedAt: generatedAt, schemaVersion: schemaVersion)
        case .unknown:
            // A future frame kind, or a `type`-less line (e.g. the pre-#164 `{"error":…}` payload):
            // ignored by a forward-compatible client (#164 additive ethos). Like an undecodable line,
            // it is NOT valid data → it does not advance liveness or clear staleness.
            return .ignoredUnknownFrame
        }
    }

    private mutating func applySnapshot(_ status: VersionedStatus) -> LineOutcome {
        liveness = .live
        hasEverConnected = true            // a valid frame proves a live connection was held (#499)
        guard status.isSchemaSupported else {
            // A breaking-major snapshot: reach `.unsupported` and REFUSE its numbers (do not render a
            // roster read through a contract we cannot safely parse). generatedAt is left at its
            // last-known value — the unsupported banner shows no freshness. The scrub rollup is refused
            // with the rest (a fault read through an unreadable contract is not trustworthy either).
            snapshotClass = .unsupported
            rows = []
            nextSwap = nil
            refreshEnabled = nil
            canonicalScrub = nil
            return .unsupportedSchema
        }
        snapshotClass = status.accounts.isEmpty ? .empty : .healthy
        rows = AccountRow.rows(from: status)
        nextSwap = status.nextSwap
        refreshEnabled = status.refreshEnabled
        generatedAt = status.generatedAt
        canonicalScrub = status.canonicalScrub
        return .appliedSnapshot
    }

    private mutating func applyHeartbeat(generatedAt: Int64, schemaVersion: SchemaVersion) -> LineOutcome {
        liveness = .live
        hasEverConnected = true            // a valid frame proves a live connection was held (#499)
        guard WireContract.isSupported(schemaVersion) else {
            snapshotClass = .unsupported
            rows = []
            nextSwap = nil
            refreshEnabled = nil
            canonicalScrub = nil
            return .unsupportedSchema
        }
        // Liveness/keepalive ONLY — a heartbeat carries no roster, so it must NOT be treated as a
        // snapshot (it never touches `snapshotClass` or `rows`). It refreshes the freshness stamp and,
        // via `liveness = .live` above, clears an earlier `.stale` on the SAME still-open connection:
        // the beat proves the last snapshot is still current. It can never, on its own, produce a
        // healthy view — with `snapshotClass == .none` a heartbeat resolves to `.connecting`.
        self.generatedAt = generatedAt
        return .appliedHeartbeat
    }
}
