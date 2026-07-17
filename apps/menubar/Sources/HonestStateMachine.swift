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
// the grace elapses still refused. The WARM drop gets the SAME grace-then-escalate discipline (#526,
// the mirror of the cold split): a live connection held then lost reads as the transient `.reconnecting`
// within a bounded warm-dwell window (so a routine daemon restart / wake-from-sleep socket blip rides the
// calm `…` self-resolving glance), then escalates to the durable `.disconnected` once the dwell elapses
// still dropped (the loud `!`, for a genuinely-dead daemon). The dwell timer SUSPENDS across system sleep
// (`systemWillSleep` / `systemDidWake`) so a lid-closed-overnight disconnect never opens on a false
// Attention. The REMAINING degraded-state-map facets are tracked siblings, NOT this file: the
// rich version-skew upgrade UX, plus the daemon-level PAYLOAD faults that ride alongside a `.connected`
// roster — which now ALL project to the glance via `PresentationState.make`: the two "act now" vault
// faults (keychain-locked, canonical-scrub-`exhausted`) to `.noRunway` `⊘`, and the "next break"
// mechanism fault (`systemic_refresh_failure`, issue #378) to `.attention` `!` (issue #520).
// Scrub-`recovering` stays deliberately unmapped — it may self-heal with no operator action, so
// alarming the glance would cry wolf.
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

    /// A WARM drop (a live connection WAS held, then the socket dropped) that is still WITHIN a bounded
    /// warm-dwell window (issue #526) — the transient "reconnecting" state that reads as self-resolving,
    /// because a routine daemon restart / wake-from-sleep socket blip passes THROUGH here on its way back
    /// up. The warm sibling of `.starting` (same benign forming glance, the `…` connecting glyph) and the
    /// transient half of the warm-drop split: it escalates to `.disconnected` once the dwell elapses still
    /// dropped, reserving the loud `!` for a genuinely-dead daemon. Distinct from `.starting` (a COLD
    /// connect-refused — no live connection EVER held) and `.connecting` (a bare reconnect whose socket is
    /// already back, awaiting a fresh snapshot). Last-good data is retained, shown dimmed. NEVER healthy.
    case reconnecting(reason: String)

    /// A WARM drop that has PERSISTED past the warm-dwell window (issue #526) — the durable "daemon not
    /// responding" state, reached from `.reconnecting` once the dwell elapses still dropped (a warm drop
    /// ALWAYS enters `.reconnecting` first; it lands here only by escalation). Last-good data is shown
    /// MARKED stale, never as live; the transport keeps reconnecting with backoff on its own, but the drop
    /// has outlived the dwell so the honest glance is the loud `!` — a hand-launched daemon that dies
    /// mid-session and never returns parks here indefinitely. NEVER healthy.
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

/// The abstract glance glyph — the menu bar's **4-state ATTENTION axis** (issue #524), SHAPE-coded and
/// decoupled from any concrete SF Symbol (that mapping is `StatusGauge`; the bespoke artwork is #437).
///
/// This is deliberately NOT one-to-one with `ConnectionState`. The pre-#524 set was — and was therefore
/// **mis-axed**: it answered *"what is the socket doing?"*, enumerating nine connection topologies onto
/// nine silhouettes, when the only question a ~16 pt monochrome glance can afford to answer is the
/// operator's — **"do I need to act, and can I trust what it shows?"**. The nine `ConnectionState` cases
/// are now INPUTS to this axis, not glyphs of their own (#524 AC).
///
/// | Case | Meaning | Ratified interior mark |
/// |---|---|---|
/// | `.healthy` | alive ∧ fresh — ignore me | a low check `✓` |
/// | `.connecting` | can't vouch yet — self-resolving | an ellipsis `…` |
/// | `.attention` | act at your next break | an exclamation `!` |
/// | `.noRunway` | the tool can't keep you working — act now | a slash `⊘` |
///
/// The taxonomy is operator-ratified and LOCKED (two design councils, 2026-07-14): a FIFTH state is an
/// operator call, not a code call. `.attention` is a deliberate COLLAPSE BUCKET — the glyph does not
/// disambiguate *which* fault; that is one click away in the panel (and, for VoiceOver, not even that:
/// `PresentationState.accessibilityLabel` keeps the per-input sentence the glyph collapses).
enum StatusGlyph: Equatable, Sendable, CaseIterable {
    /// The ONE healthy glyph. GATED: emitted only when BOTH "alive" AND "fresh" are positively verified
    /// AND the fleet has runway — i.e. `ConnectionState.connected` (which structurally already implies a
    /// live connection, a fresh schema-supported snapshot, and ≥ 1 account) with no `noViableTarget`.
    /// Never a default, never a fallback — the glance-surface analogue of the anti-#137 discipline.
    case healthy
    /// Honest uncertainty: we cannot vouch for the data YET, and the state is expected to resolve with no
    /// operator action. Load-bearing property: only states whose self-resolution is BOUNDED belong here
    /// (`.connecting` is superseded by the next frame; `.starting` is bounded by the start grace, which
    /// escalates to `.notRunning`; `.reconnecting` is bounded by the warm dwell, which escalates to
    /// `.disconnected` — #526). An UNBOUNDED "…" would be a promise the app cannot keep.
    case connecting
    /// The collapse bucket: the tool needs something from the operator, and it is not urgent. Ratified
    /// members reachable from this axis: version-skew (`.unsupported`) and crash-loop (`.crashLooping`),
    /// plus the daemon-liveness faults that cannot self-resolve (`.stale`, `.disconnected`, `.notRunning`)
    /// and the un-configured tool (`.emptyRoster`). Daemon-level PAYLOAD faults do NOT flow through
    /// `ConnectionState` — they ride alongside a `.connected` roster — so they are projected in `make` off
    /// the vouched snapshot: the two that mean "act now" (keychain-locked and canonical-scrub-`exhausted`)
    /// map to `.noRunway`, while `systemic_refresh_failure` (the refresh MECHANISM down, issue #378) maps
    /// HERE — it is PRE-DEATH by construction (#378 fires while every account is still alive), so the tool
    /// IS still keeping the operator working and `⊘`'s "act NOW" would over-state it; but it cannot
    /// self-heal (the mechanism keeps failing until an operator intervenes), so `…` would be a promise the
    /// app cannot keep. That is exactly "act at your next break" — and it sits with `.crashLooping`, this
    /// bucket's other "the daemon's own machinery is broken" member (issue #520).
    /// Canonical-scrub-`recovering` stays deliberately unmapped: it MAY self-heal, so it stays healthy-`✓`
    /// (the panel still shows its calm banner) — alarming the glance would cry wolf.
    case attention
    /// The no-runway state (issue #524): the tool cannot keep the operator working, so act now. THREE
    /// vouched inputs converge here — issue #520 added the daemon-level vault pair to #524's fleet verdict:
    /// the fleet has no viable swap target left (quota), the login keychain is LOCKED (the shared
    /// credential is unreadable → unlock it), or the shared canonical is scrubbed-`exhausted` (→ `claude
    /// /login`). All three are GATED exactly as `.healthy` is — read only off a fresh, vouched `.connected`
    /// snapshot, so a retained fault under a `.stale` / `.disconnected` render never shouts here (see `make`).
    /// (Quota reaches the bar at exactly this one point; a resting quota level is the daemon's job, never a glyph.)
    case noRunway
}

/// What the status item renders: the glance `glyph` plus a VoiceOver `accessibilityLabel`. The label
/// is a plain, factual sentence (design-menubar a11y: shape-coded glyph + a spoken label per state).
///
/// The two channels carry DIFFERENT resolutions, by design: the `glyph` collapses to the ratified 4
/// (a ~16 pt monochrome silhouette can carry no more), while the `accessibilityLabel` stays specific to
/// the originating `ConnectionState` — VoiceOver is not shape-constrained, so collapsing it too would
/// discard honesty the surface can afford to keep.
struct PresentationState: Equatable, Sendable {
    let glyph: StatusGlyph
    let accessibilityLabel: String

    /// Project a connection state onto the attention axis (issue #524).
    ///
    /// The rule is two-tier, and it is what makes gated-Healthy STRUCTURAL rather than conventional:
    ///
    ///   1. **Vouched?** (`.connected` — live ∧ fresh ∧ schema-supported ∧ ≥ 1 account) → the FLEET/VAULT
    ///      speaks, worst-first: `.noRunway` when the operator must act NOW — keychain LOCKED, shared
    ///      canonical scrubbed-`exhausted`, or no viable swap target left; then `.attention` when the tool
    ///      still works but needs the operator at their next break — the refresh MECHANISM down (#378), or
    ///      the active account's auto-protection DEGRADED (#485); else `.healthy`. Within a glyph the order
    ///      only picks which root cause the a11y label names (the silhouette is one `⊘` / one `!`
    ///      regardless); ACROSS glyphs it is the severity rank itself. Healthy and every fault here share
    ///      one evidence bar.
    ///   2. **Not vouched?** → the CONNECTION speaks, and it may only claim what it can observe:
    ///      `.connecting` while self-resolution is BOUNDED, `.attention` otherwise.
    ///
    /// Two consequences worth naming, because they are the reason the rule is shaped this way:
    ///
    ///   * A fleet/vault verdict is never rendered off data we cannot vouch for. Retained `noViableTarget`
    ///     — and, identically, a retained `keychainLocked` / `canonicalScrub` — on a `.stale` /
    ///     `.disconnected` roster does NOT reach the bar: these bits ride alongside the roster and are
    ///     retained across a drop, so `make` reads them ONLY in the `.connected` arm; on a dropped socket
    ///     the actionable problem is always the socket, not a stale vault bit (quota, moreover, moves UP
    ///     while we are not looking — so `⊘` off it would shout about a problem most likely already
    ///     resolved). The panel still shows the retained value, marked stale — bar = vouched verdict, panel
    ///     = attributed record. This mirrors `AccountEventNotifier`, which derives `.allExhausted` ONLY from `.connected`.
    ///   * `.emptyRoster` cannot be `.healthy`, even though it IS alive ∧ fresh. "Zero accounts are fine"
    ///     is a gate passing on a degenerate subject — vacuously true, not meaningfully true — and
    ///     `.healthy` means "ignore me", which is false for a tool that is doing nothing. It fails tier 1
    ///     on cardinality (structurally: `.connected` requires a non-empty roster) and falls to tier 2,
    ///     where it cannot self-resolve without the operator → `.attention` ("add your first account" is
    ///     precisely a next-break task). The same cardinality argument kills a vacuous `.noRunway` there.
    ///
    /// Pure and total — the same input always yields the same presentation, and the exhaustive `switch`
    /// (no `default:`) makes the compiler, not a reviewer, the check that every input has a home.
    static func make(for state: ConnectionState,
                     accountCount: Int,
                     hasNoViableTarget: Bool = false,
                     keychainLocked: Bool = false,
                     canonicalScrub: CanonicalScrub? = nil,
                     activeBlindDegraded: Bool = false,
                     systemicRefreshFailure: UInt32? = nil) -> PresentationState {
        switch state {
        case .connected:
            // TIER 1 — vouched: the fleet/vault speaks. Every `⊘` no-runway path first (worst-first:
            // keychain-locked ▸ scrub-`exhausted` ▸ no-viable-target — one `⊘` glyph regardless, the order
            // only picks which root cause the label names), then the sole healthy path. The two surfaces
            // share this severity ordering but NOT their input sets, so neither doc may claim the other's
            // rank verbatim: quota is a glance-only input (`daemonFaultBanner` never sees it), and the
            // calm scrub-`recovering` is a banner-only rank (it is no input at all here — see below).
            // `.recovering` scrub is NOT here: it may self-heal with no operator action,
            // so alarming would cry wolf (issue #520 defers the recovering-glyph call).
            if keychainLocked {
                return PresentationState(glyph: .noRunway,
                                         accessibilityLabel: "Sessiometer: keychain locked — unlock it to keep working")
            }
            if case .exhausted = canonicalScrub {
                return PresentationState(glyph: .noRunway,
                                         accessibilityLabel: "Sessiometer: signed out of the shared login — run claude /login")
            }
            if hasNoViableTarget {
                return PresentationState(glyph: .noRunway,
                                         accessibilityLabel: "Sessiometer: no account has capacity right now — action needed")
            }
            // TIER 1 — the `!` next-break rung, worst-first: the FLEET-wide mechanism verdict (#520, the
            // refresh mechanism down) before the per-account modifier (#485), mirroring the panel, which
            // rides its daemon-level banner ABOVE the roster. The glyph is one `!` either way, so the order
            // only picks which root cause the a11y label names. Why a down refresh mechanism belongs on `!`
            // rather than the `⊘` above or the `…` of a self-resolving state: see `StatusGlyph.attention`.
            if let consecutive = systemicRefreshFailure {
                let sweeps = consecutive == 1 ? "sweep" : "sweeps"
                return PresentationState(glyph: .attention,
                                         accessibilityLabel: "Sessiometer: refresh mechanism down — \(consecutive) consecutive \(sweeps) failed for every eligible account")
            }
            // #485: a blind ACTIVE account whose ADR-0017 auto-protection is DEGRADED (armed but acting on a
            // STALE anchor) is a next-break the operator should see — ratified 2026-07-16 as the honest "!"
            // attention glance. It rides ONE rung below `⊘` no-runway: a cornered blind account (blind +
            // degraded + no viable target) already took the `hasNoViableTarget` ⊘ above, so reaching here
            // means a viable target still exists. A blind-but-OK account is deliberately NOT escalated — the
            // daemon is self-resolving within the bounded window, so it stays on the calm healthy path below.
            if activeBlindDegraded {
                return PresentationState(glyph: .attention,
                                         accessibilityLabel: "Sessiometer: auto-protection degraded — acting on a stale anchor")
            }
            let plural = accountCount == 1 ? "" : "s"
            return PresentationState(glyph: .healthy,
                                     accessibilityLabel: "Sessiometer: live — \(accountCount) account\(plural)")

        // TIER 2 — not vouched: the connection speaks. BOUNDED self-resolution → the honest-unknown "…".
        case .connecting:
            return PresentationState(glyph: .connecting,
                                     accessibilityLabel: "Sessiometer: connecting to the daemon…")
        case .starting:
            return PresentationState(glyph: .connecting,
                                     accessibilityLabel: "Sessiometer: the daemon is starting…")
        case .reconnecting:
            // A WARM drop still within the warm dwell (#526): bounded self-resolution — the dwell escalates
            // it to `.disconnected` — so it rides the calm "…", letting a routine daemon restart / wake blip
            // pass through quietly rather than flashing the loud "!" it would have before #526.
            return PresentationState(glyph: .connecting,
                                     accessibilityLabel: "Sessiometer: reconnecting to the daemon…")

        // TIER 2 — not vouched, and NOT self-resolving: the operator is needed → the collapse bucket.
        // `.stale` and `.disconnected` land here rather than under "…" because neither is pre-verdict:
        // `.stale` is reached only AFTER the 32 s liveness window has already elapsed with no valid frame
        // (the debounce has run — it is a verdict, not a wait), and a `.disconnected` drop has already
        // outlived the warm dwell (#526: the in-window transient rides `.reconnecting` above; only the
        // ESCALATED drop reaches here), so an unbounded "…" would misreport a genuinely-dead daemon as
        // "hold on, self-resolving" forever. Since this daemon is launched by hand (no launchd relaunch),
        // that dead-forever case is ordinary, not exotic — so the honest failure mode here is LOUD, not
        // silent. The warm dwell is what buys the transient its calm "…" WITHOUT softening this durable "!".
        case .stale:
            return PresentationState(glyph: .attention,
                                     accessibilityLabel: "Sessiometer: data may be stale — the daemon has gone quiet")
        case .disconnected:
            return PresentationState(glyph: .attention,
                                     accessibilityLabel: "Sessiometer: disconnected — the daemon is not responding")
        case .notRunning:
            return PresentationState(glyph: .attention,
                                     accessibilityLabel: "Sessiometer: the daemon is not running")
        case .emptyRoster:
            return PresentationState(glyph: .attention,
                                     accessibilityLabel: "Sessiometer: connected — no accounts configured")
        case .unsupported:
            return PresentationState(glyph: .attention,
                                     accessibilityLabel: "Sessiometer: daemon version unsupported — update required")
        case .crashLooping:
            return PresentationState(glyph: .attention,
                                     accessibilityLabel: "Sessiometer: the daemon is restarting repeatedly — holding status until it stays up")
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
    /// The active account's bounded-blindness projection (issues #479/#485) — present ONLY when THIS
    /// (active) account's usage poll is blind. Drives the per-row blind treatment (a SEMANTIC held-state
    /// row in place of a false-healthy one, #137) and — when auto-protection is DEGRADED — escalates the
    /// menu-bar glance to `.attention`. A pass-through of the wire field; the daemon owns the verdict.
    let blindActive: BlindActive?

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
                isNextSwapTarget: account.label == targetLabel,
                blindActive: account.blindActive)
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
        case reconnecting(reason: String) // a warm drop (a live connection held, then lost) within the warm dwell (#526)
        case disconnected(reason: String) // a warm drop escalated past the warm dwell — the durable "not responding" state (#526)
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
    /// track, #499) from a WARM drop (a connection WAS held, then lost → the `.reconnecting`/`.disconnected`
    /// socket-dropped track, #526). Set once, never cleared: a session that has ever reached the daemon is
    /// past the cold-start question for good, so a later refused reconnect reads as a drop, not "never running".
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

    // MARK: - Warm dwell: split reconnecting (transient) from disconnected (durable) (#526)

    /// The warm-dwell timer token (#526), mirroring `graceGeneration`: bumped whenever a WARM drop first
    /// enters `.reconnecting` (ARM the dwell), whenever that dwell is LEFT — the daemon reconnected
    /// (`apply(.connected)`), or the dwell elapsed to the durable `.disconnected` (`dwellElapsed`) — and
    /// whenever system sleep SUSPENDS or wake RESUMES it (`systemWillSleep` / `systemDidWake`). A fired
    /// `dwellElapsed` whose `generation` ≠ this is a superseded timer and is ignored. The shell re-arms its
    /// real `Task.sleep` timer whenever this value changes across a mutation.
    private(set) var dwellGeneration = 0

    /// Whether the warm dwell is SUSPENDED because the system is asleep (#526). Set by `systemWillSleep`,
    /// cleared by `systemDidWake`, so it is `true` only for the sleep interval. It gates `isAwaitingWarmDwell`
    /// to `false` while asleep — the BLOCKING sleep/wake falsifier: a lid closed overnight is a very long,
    /// 100%-benign disconnect that resolves in ~1 s on wake, so if the dwell ran during sleep the app would
    /// open on a FALSE Attention every morning (the tool's most-seen moment). Suspending the dwell across
    /// sleep, and RESETTING it on wake (`systemDidWake` re-arms a fresh window — "treat wake as a fresh
    /// connect"), keeps a genuinely-benign wake blip on the calm "…" it deserves.
    private var dwellSuspended = false

    /// Whether a WARM drop is currently within the warm dwell (liveness `.reconnecting`) AND not suspended by
    /// sleep: the shell runs the real dwell `Task.sleep` exactly while this is `true`. Every connected / stale
    /// / cold / already-`.disconnected` state — and a `.reconnecting` state while the system is asleep — is
    /// NOT awaiting, so the shell cancels (not re-arms) its dwell timer when this is `false`. The sleep guard
    /// is what makes the timer suspend across a lid-close without any clock-type trickery.
    var isAwaitingWarmDwell: Bool {
        if case .reconnecting = liveness, !dwellSuspended { return true }
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

    /// The daemon-level KEYCHAIN-LOCKED flag (#498, wire #521), carried from the last applied snapshot
    /// exactly as `canonicalScrub` is: `true` while the macOS login keychain is LOCKED, so the daemon
    /// cannot READ the shared `Claude Code-credentials` item at ALL — the daemon-LEVEL sibling of
    /// `canonicalScrub`, but for an UNREADABLE item (access denied), so the remedy is UNLOCK the keychain,
    /// not `claude /login`. RETAINED across a drop (shown under the dimmed last-known render, like
    /// `canonicalScrub`) and REFUSED with the other numbers on an unsupported-major frame. `false` when
    /// the keychain is unlocked — or a pre-#498 daemon omits the wire key (`decodeIfPresent ?? false`), so
    /// a healthy/legacy daemon never renders a keychain-locked banner.
    private(set) var keychainLocked: Bool = false

    /// The daemon-level SYSTEMIC REFRESH-FAILURE count (#378, wire since schema 1.1), carried from the last
    /// applied snapshot exactly as `canonicalScrub` / `keychainLocked` are: the number of consecutive refresh
    /// SWEEPS in which EVERY eligible account's cycle failed with `outcome=error` — the refresh MECHANISM is
    /// down (a stale pinned `claude` path, a wedged spawn), not one account's credentials. The third
    /// daemon-level payload fault, and the one NO per-account `auth` cell reflects even in principle: it is
    /// visible BEFORE any account dies (that is the whole point of #378 — the #375 incident kept a total
    /// refresh outage invisible for ~4.5 h until a token finally expired). RETAINED across a drop (shown
    /// under the dimmed last-known render, like its siblings) and REFUSED with the other numbers on an
    /// unsupported-major frame. `nil` when the mechanism is healthy — or a pre-#378 daemon omits the wire key
    /// (`decodeIfPresent`), so a healthy/legacy daemon never alarms. A COUNT only, never a token, path, or
    /// email (issue #15).
    private(set) var systemicRefreshFailure: UInt32?

    /// The honest connection state — a PURE function of `(liveness, snapshotClass)`. This is where the
    /// never-healthy-when-dead invariant lives: `.connected` is returned on exactly one combination.
    var connectionState: ConnectionState {
        switch liveness {
        case .reconnecting(let reason):
            return .reconnecting(reason: reason)
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

    /// The glance presentation derived from the current state (issue #524 + #520). `hasNoViableTarget`,
    /// `keychainLocked`, and `canonicalScrub` are read from the retained snapshot, but `PresentationState.make`
    /// only lets them reach the `.noRunway` glyph on a vouched `.connected` snapshot — so a fault retained
    /// under a `.stale` / `.disconnected` render is carried here yet correctly ignored by the projection
    /// (the panel still shows it, marked stale). Mirrors `AccountEventNotifier.isNoViableTarget`.
    var presentation: PresentationState {
        PresentationState.make(for: connectionState,
                               accountCount: rows.count,
                               hasNoViableTarget: hasNoViableTarget,
                               keychainLocked: keychainLocked,
                               canonicalScrub: canonicalScrub,
                               activeBlindDegraded: activeBlindDegraded,
                               systemicRefreshFailure: systemicRefreshFailure)
    }

    /// Whether the retained `nextSwap` reports the fleet has no viable swap target left — the
    /// `.noRunway` input (issue #524). A pure read of the last applied snapshot's `nextSwap`; the
    /// vouched-data gate lives in `PresentationState.make`, not here.
    private var hasNoViableTarget: Bool {
        if case .noViableTarget = nextSwap { return true }
        return false
    }

    /// Whether the ACTIVE account is blind AND its ADR-0017 auto-protection is DEGRADED (#485) — the
    /// glance-escalation input. A pure read of the retained snapshot's active row; the vouched-data gate
    /// (only a `.connected` snapshot lets it reach `.attention`) lives in `PresentationState.make`, exactly
    /// as it does for `hasNoViableTarget` / `keychainLocked`.
    private var activeBlindDegraded: Bool {
        rows.contains { $0.isActive && ($0.blindActive?.autoProtectionDegraded ?? false) }
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
        // And the pre-event warm-dwell state, on the SAME transition-only discipline (#526): the false→true
        // first warm drop arms the dwell; a true→false reconnect / escalation cancels it; a repeat drop
        // within one dwell leaves it counting so the dwell alone owns reconnecting → disconnected.
        let wasAwaitingWarmDwell = isAwaitingWarmDwell
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
                // WARM: a live connection was held, then lost. Split the in-window transient (`.reconnecting`,
                // the calm "…") from the escalated durable drop (`.disconnected`, the loud "!") — the warm
                // mirror of the cold `.starting` → `.notRunning` split (#526). Enter `.reconnecting` on the
                // FIRST drop; STAY put on repeat drops within the backoff loop (already `.reconnecting`, or
                // already escalated to `.disconnected`) so the dwell timer ALONE owns the escalation — a
                // repeat drop must not reset the window (mirrors the cold path staying in `.starting`).
                switch liveness {
                case .reconnecting, .disconnected:
                    break                      // already in the warm-drop track — the dwell owns reconnecting → disconnected
                default:
                    // A genuine drop of an established (or gone-quiet) connection. A drop while a held snapshot
                    // was still stabilizing = an UNSTABLE reconnect: the clock-free crash-loop signal (#169).
                    // Count it BEFORE mutating liveness (which flips `isStabilizing`).
                    if wasStabilizing { consecutiveUnstableReconnects += 1 }
                    // Last-good rows/nextSwap/generatedAt are RETAINED but the state is now non-live. Also reset
                    // the snapshot classification: the roster is no longer confirmed, so healthy must be re-earned
                    // by a FRESH snapshot — this makes the never-healthy invariant hold STRUCTURALLY even if a
                    // heartbeat were somehow to arrive before the reconnect `.connected` (the transport orders
                    // `.connected` first, but the invariant must not depend on that).
                    liveness = .reconnecting(reason: reason)
                    snapshotClass = .none
                    hasEverDisconnected = true     // ARM the debounce for every subsequent reconnect (#169)
                    watchdogGeneration += 1        // INVALIDATE: already non-live, no watchdog needed (#344)
                }
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
        // And the warm dwell (#526), same transition-only arm/cancel: false→true on the first warm drop arms
        // it; true→false when the daemon reconnects (`.connected` → `.live`) cancels it; a repeat drop within
        // one dwell leaves it counting toward the `.disconnected` escalation.
        if isAwaitingWarmDwell != wasAwaitingWarmDwell { dwellGeneration += 1 }
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

    /// Fold in an elapsed warm dwell (#526): a WARM drop has stayed dropped for the WHOLE dwell with the
    /// daemon never reconnecting — so it is a durable outage, not a routine restart / wake blip. Escalate the
    /// transient `.reconnecting` to the durable `.disconnected` (`.connecting` "…" → `.attention` "!"),
    /// carrying the original drop reason forward. Generation-guarded exactly like `graceElapsed` (a token
    /// superseded by the daemon reconnecting, by any later re-arm, or by a sleep suspend / wake reset is
    /// ignored) and gated on actually still reconnecting, so it can never manufacture `.disconnected` from a
    /// connected / cold / already-disconnected state, nor fire twice. The clock lives in the `WatchStatusStore`
    /// shell, which performs the real `Task.sleep` and feeds the elapse back — the pure core stays clock-free.
    mutating func dwellElapsed(generation: Int) {
        guard generation == dwellGeneration else { return }             // superseded (e.g. the daemon reconnected) → ignore
        guard case .reconnecting(let reason) = liveness else { return } // only a still-reconnecting drop escalates
        liveness = .disconnected(reason: reason)
        dwellGeneration += 1     // consume: the dwell is over (`isAwaitingWarmDwell` is now false)
    }

    // MARK: - Sleep/wake gating of the warm dwell (#526)

    /// The system is about to sleep: SUSPEND the warm dwell so it cannot escalate a benign sleep-time
    /// disconnect. This is the BLOCKING falsifier — a lid closed overnight is a long, 100%-benign disconnect
    /// that resolves in ~1 s on wake; if the dwell ran during sleep the app would open on a FALSE Attention
    /// every morning. Setting `dwellSuspended` flips `isAwaitingWarmDwell` to `false` (a true→false
    /// transition), so the shell cancels the in-flight dwell timer; the liveness stays `.reconnecting`, so the
    /// dwell merely pauses rather than losing the drop. A no-op when no warm drop is dwelling. The shell wires
    /// this to `NSWorkspace.willSleepNotification`; tests call it directly (hermetic, no real sleep).
    mutating func systemWillSleep() {
        let wasAwaiting = isAwaitingWarmDwell
        dwellSuspended = true
        if isAwaitingWarmDwell != wasAwaiting { dwellGeneration += 1 }  // suspend: cancel the in-flight dwell timer
    }

    /// The system just woke: RESUME the warm dwell, RESET afresh — "treat wake as a fresh connect". Clearing
    /// `dwellSuspended` flips `isAwaitingWarmDwell` back to `true` if the drop is still dwelling (a false→true
    /// transition), so the shell re-arms a FULL fresh dwell window from wake — a genuinely-benign wake blip
    /// (the socket returns in ~1 s) resolves well inside it and never escalates, while a truly-dead daemon
    /// reaches Attention one dwell after wake. If the daemon already reconnected across the sleep boundary
    /// (liveness left `.reconnecting`), this is a no-op. The shell wires this to `NSWorkspace.didWakeNotification`;
    /// tests call it directly (hermetic, no real wake).
    mutating func systemDidWake() {
        let wasAwaiting = isAwaitingWarmDwell
        dwellSuspended = false
        if isAwaitingWarmDwell != wasAwaiting { dwellGeneration += 1 }  // resume: re-arm a FRESH dwell if still reconnecting
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
            keychainLocked = false
            systemicRefreshFailure = nil
            return .unsupportedSchema
        }
        snapshotClass = status.accounts.isEmpty ? .empty : .healthy
        rows = AccountRow.rows(from: status)
        nextSwap = status.nextSwap
        refreshEnabled = status.refreshEnabled
        generatedAt = status.generatedAt
        canonicalScrub = status.canonicalScrub
        keychainLocked = status.keychainLocked
        systemicRefreshFailure = status.systemicRefreshFailure
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
            keychainLocked = false
            systemicRefreshFailure = nil
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
