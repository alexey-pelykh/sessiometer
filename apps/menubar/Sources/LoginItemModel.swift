// Copyright (c) 2026 Oleksii PELYKH
// SPDX-License-Identifier: MIT

// The launch-at-login model (issue #170): the pure `@MainActor` decision layer over `SMAppService` login-item
// + LaunchAgent registration, exposing the "Launch at login" toggle intent and the "Start daemon" affordance
// to the Settings form and the not-running panel card. It is the login-item SIBLING of `SettingsModel` (#268)
// and `AccountSwapModel` (#169) — the same tested-shell / untested-OS-wrapper split, only the surface differs.
//
// AppKit- AND ServiceManagement-FREE by design (Foundation + Combine + os only) so it compiles into the
// headless `MenubarTests` bundle and its toggle derivation, idempotent first-launch registration,
// `.requiresApproval` handling, the two-owner guard, and the Start-daemon phase machine are all driven
// hermetically against a fake `LoginItemService` — NO real `SMAppService` registration, no login item ever
// written to the operator's account by a test run. The concrete `SMAppServiceLoginItemService` (which imports
// `ServiceManagement` and touches the OS) stays in the app target, the same split `SettingsModel` (tested) vs
// `SettingsView`/`SMAppServiceLoginItem` (app-only) uses.
//
// NO credential handling of any kind (C-001 / issue #15): the whole surface is registration state (login item
// + LaunchAgent), which carries no token, email, or oauth blob. A `.failed` reason is a redacted registration
// message, never a secret.
//
// TWO-OWNER INVARIANT (issue #170 / #329, load-bearing): the `org.sessiometer.agent` LaunchAgent can be
// registered by the Rust CLI (`sessiometer service install`, `~/Library/LaunchAgents`, `src/service.rs`) AND
// by this app (a bundled `SMAppService.agent` plist) — ONE identity, deliberately shared. Two plists on one
// label collide. The app is the newcomer, so it YIELDS: when the CLI already owns the label
// (`cliManagedAgentPresent`), the app never registers its bundled agent and the Start affordance stands down.
//
// STALE-REGISTRATION REPAIR (issue #788): registering once is not enough. `SMAppService.h` states, verbatim:
// "If an app updates either the plist or the executable for a LaunchAgent or LaunchDaemon, the SMAppService
// must be re-registered or it may not launch. It is recommended to also call unregister before re-registering
// if the executable has been changed." `embed-daemon.sh` re-`lipo`s the daemon into `Contents/Helpers/` on
// EVERY Release build (issue #171), so an app update always changes the registered executable — and the Start
// affordance cannot repair it (it stands down once a daemon holds the lock, issue #742). So the model
// reconciles at launch (`reconcileDaemonAgentRegistration()`), unregistering BEFORE re-registering. The
// two-owner invariant above OUTRANKS the header: a CLI-managed agent is never unregistered by the app.
//
// The repair's live-daemon gate asks about OUR OWN launchd job, not about the lock (issue #819): the lock
// probe is any-provenance by design (issue #742) and reported a hand-run `sessiometer run` as a reason to
// defer, which made the repair permanently inert on exactly the machines that run their daemon by hand.
// `daemonAgentRunState` is the narrower signal the seam grew for it.

import Combine
import Foundation
import os

private let loginItemLog = Logger(subsystem: "org.sessiometer.menubar", category: "login-item")

// MARK: - The OS seam

/// One `SMAppService` registration status, mirrored into an AppKit-free enum so the pure model can reason over
/// it without importing `ServiceManagement`. The concrete `SMAppServiceLoginItemService` maps
/// `SMAppService.Status` (`.notRegistered` / `.enabled` / `.requiresApproval` / `.notFound`) onto these cases.
enum LoginItemStatus: Equatable {
    /// Not registered — the toggle reads OFF.
    case notRegistered
    /// Registered and active — the toggle reads ON.
    case enabled
    /// Registered but the user must approve it in System Settings › General › Login Items. This is a SUCCESS
    /// (the register call worked) with a pending approval gate — the toggle reads ON, never a failure.
    case requiresApproval
    /// No such registrable item — for the daemon agent in #170 this is the expected state until #171 embeds
    /// the daemon binary + ships the bundled `Contents/Library/LaunchAgents` plist.
    case notFound
}

/// What launchd reports about the RUN state of the bundled daemon agent's job (issue #819) — the
/// dimension `LoginItemStatus` above does NOT carry. The two are orthogonal: a `.enabled` registration
/// can be `.notRunning` (registered, loaded, no process behind it), which is exactly the state issue
/// #819 was observed in. `LaunchdJobProbe` produces it; `SMAppServiceLoginItemService` exposes it.
enum DaemonAgentRunState: Equatable {
    /// launchd reports the job is running — a process exists that unloading the job would terminate.
    case running
    /// launchd reports the job is loaded but NOT running — unloading it terminates nothing.
    case notRunning
    /// The probe could not answer (spawn failed, timed out, non-zero exit, or output this does not
    /// recognise). Deliberately NOT collapsed into either verdict: the caller reads it as "I cannot
    /// tell" and falls back to the any-provenance lock gate, which is issue #788's behaviour.
    case unknown
}

/// WHICH writer painted the current `StartPhase` beat (issue #820). Issue #788 made `startPhase` a
/// TWO-writer channel — the Start button and the launch-time registration repair — and the card had no way
/// to say which, so a repair the operator never asked for read exactly like a press they had just made.
///
/// Top-level rather than nested in the `@MainActor` model, alongside `LoginItemStatus` and
/// `DaemonAgentRunState` above and for the same reason: `StatusPanelFormat` (a non-isolated namespace, with
/// non-isolated tests) selects the card's copy from it.
enum StartOrigin: Equatable {
    /// The operator pressed **Start daemon** and is watching the card. Needs no attribution — they know
    /// what they just did, which is why this path's copy is left exactly as issue #170 shipped it.
    case operatorStart
    /// `reconcileDaemonAgentRegistration()` repaired a stale registration at launch. NOTHING the operator
    /// did stands behind this beat, so its copy has to say so.
    case launchRepair
}

/// The OS surface the `LoginItemModel` drives, behind a protocol so the model's decisions are tested against a
/// fake. `SMAppService` exposes no injectable state, so this seam is the ONLY testability boundary; the concrete
/// implementation wraps `SMAppService.mainApp` / `SMAppService.agent(plistName:)` and the CLI-owner probe.
protocol LoginItemService: AnyObject {
    /// The app's own login-item (`SMAppService.mainApp`) status.
    var appStatus: LoginItemStatus { get }
    /// Register the app as a login item (idempotent at the OS layer — a re-register of an enabled item is a
    /// no-op). Throws the `SMAppService` error on failure.
    func registerApp() throws
    /// Unregister the app login item. Throws on failure.
    func unregisterApp() throws

    /// The bundled daemon LaunchAgent (`SMAppService.agent`) status. `.notFound` until #171 ships the plist.
    var daemonAgentStatus: LoginItemStatus { get }
    /// Whether the Rust CLI already owns the `org.sessiometer.agent` LaunchAgent (a plist at the CLI's
    /// `~/Library/LaunchAgents` path). When true, the app defers — the two-owner guard (issue #170 / #329).
    var cliManagedAgentPresent: Bool { get }
    /// Whether ANY daemon currently holds the single-instance lock (`daemon.lock`) — a fresh
    /// client-side `flock` liveness probe (issue #742). Unlike `cliManagedAgentPresent` (which only
    /// checks whether the CLI's LaunchAgent PLIST FILE exists), this detects a LIVE daemon of any
    /// provenance — including a developer's manually-run `sessiometer run` that has no LaunchAgent —
    /// so the Start affordance can stand down honestly rather than register an agent that would just
    /// lose the lock.
    var daemonLockHeld: Bool { get }
    /// What launchd reports about OUR bundled agent's job — running, loaded-but-not-running, or
    /// unknown (issue #819). The provenance-BEARING complement of `daemonLockHeld` above, which is
    /// any-provenance by design (issue #742) and stays that way: this is a NEW signal, not a
    /// redefinition of that one. It answers the only question the stale-registration repair needs —
    /// "would unloading OUR job terminate a live daemon?" — which the lock cannot answer, because the
    /// lock's holder may be a daemon we never started and would never unload.
    ///
    /// What it does NOT answer: it does not identify the lock's holder. When our job is `.notRunning`
    /// and the lock IS held, the holder is some other daemon by ELIMINATION (it cannot be ours — ours
    /// is not running), not by direct evidence of who owns that fd.
    var daemonAgentRunState: DaemonAgentRunState { get }
    /// Register (and, via the plist's `RunAtLoad`, start) the embedded daemon LaunchAgent. Throws on failure.
    func registerDaemonAgent() throws
    /// Unregister the bundled daemon LaunchAgent. Throws on failure.
    func unregisterDaemonAgent() throws

    /// Open System Settings › General › Login Items (`SMAppService.openSystemSettingsLoginItems()`), for the
    /// `.requiresApproval` deep-link.
    func openLoginItemsSettings()
}

// MARK: - The registered-executable identity (issue #788)

/// An opaque identity for the daemon EXECUTABLE this app bundle would register — the signal
/// `reconcileDaemonAgentRegistration()` compares against the last one it recorded, to detect the
/// changed-executable condition the header's STALE-REGISTRATION REPAIR note names. (Only the executable: a
/// bundled-plist edit that neither bumps the version nor re-`lipo`s the daemon is NOT detected.)
///
/// TWO halves, deliberately. The app VERSION is the shipped-update signal — every Release build re-embeds the
/// daemon (issue #171), so a shipped update always carries a new executable. The embedded executable's
/// SIZE + MTIME is the direct measure of the thing the header actually names, and additionally catches a
/// rebuild that did not bump the version — without it a local re-install at the same version would go
/// undetected. A missing helper (a bundle that predates #171, or a Debug build whose embed step no-ops)
/// degrades to the version half rather than failing: the reconcile still works, just on the coarser signal.
///
/// OPAQUE BY CONTRACT: only equality across launches is meaningful. Nothing may parse its shape.
enum DaemonAgentIdentity {
    /// The embedded daemon binary's bundle-relative path — `embed-daemon.sh`'s destination (issue #171).
    static let helperRelativePath = "Contents/Helpers/sessiometer"

    /// The identity of the currently-running app bundle. Every input is injectable so the composition is
    /// tested against a temp bundle rather than the test runner's own `Bundle.main` — otherwise the detector's
    /// only coverage would be model tests feeding it hand-written strings, which cannot show that a CHANGED
    /// executable actually produces a changed identity.
    static func current(bundleURL: URL = Bundle.main.bundleURL,
                        infoDictionary: [String: Any]? = Bundle.main.infoDictionary,
                        fileManager: FileManager = .default) -> String {
        let shortVersion = infoDictionary?["CFBundleShortVersionString"] as? String ?? "-"
        let build = infoDictionary?["CFBundleVersion"] as? String ?? "-"

        let helper = bundleURL.appendingPathComponent(helperRelativePath)
        let attributes = try? fileManager.attributesOfItem(atPath: helper.path)
        let size = (attributes?[.size] as? NSNumber).map { String($0.int64Value) } ?? "-"
        // Whole seconds: a `lipo` rewrite always moves the mtime by far more, and a coarser unit keeps the
        // identity stable against the sub-second jitter a copy or a restore can introduce.
        let modified = (attributes?[.modificationDate] as? Date)
            .map { String(format: "%.0f", $0.timeIntervalSinceReferenceDate) } ?? "-"

        return "\(shortVersion)/\(build)/\(size)/\(modified)"
    }
}

/// The persisted home for the identity of the daemon executable the app LAST successfully registered
/// (issue #788). One `UserDefaults` string with an injectable domain — the same shape as
/// `NotificationPreferences` (issue #267), so a test never writes the operator's real defaults.
///
/// It records a REGISTRATION, not a version: it is written only after a `registerDaemonAgent()` that
/// succeeded, so a deferred or failed repair leaves the old value in place and is retried on the next launch
/// rather than being silently marked done.
@MainActor
final class DaemonAgentRegistrationStore {
    /// The `UserDefaults` key holding the last successfully-registered `DaemonAgentIdentity`.
    static let lastRegisteredIdentityKey = "loginItem.daemonAgent.lastRegisteredIdentity"

    private let defaults: UserDefaults

    init(defaults: UserDefaults = .standard) {
        self.defaults = defaults
    }

    /// The identity recorded by the last register that succeeded, or nil when the app has never registered the
    /// bundled agent — or registered it from a build that predates this bookkeeping. Nil is deliberately read
    /// as CHANGED by the reconcile: an agent registered by an unknown executable is exactly the stale
    /// registration issue #788 exists to repair.
    var lastRegisteredIdentity: String? {
        get { defaults.string(forKey: Self.lastRegisteredIdentityKey) }
        set {
            if let newValue {
                defaults.set(newValue, forKey: Self.lastRegisteredIdentityKey)
            } else {
                defaults.removeObject(forKey: Self.lastRegisteredIdentityKey)
            }
        }
    }
}

// MARK: - LoginItemModel

@MainActor
final class LoginItemModel: ObservableObject {

    /// The Start-daemon affordance's interaction phase. `registering` is the transient beat that persists from
    /// the button press THROUGH the register AND the post-register liveness wait (issue #745). `failed` carries a
    /// card-rendered reason — either a REDACTED register error (no credential surface — issue #15) OR the
    /// "registered but never started" liveness timeout. `idle` is the resting state and TRUE success: register
    /// succeeded AND a daemon took the single-instance lock, so the panel will leave `.notRunning` on the next
    /// `watch` snapshot, exactly as a swap's new active row arrives.
    ///
    /// Both non-resting beats carry their `StartOrigin` (issue #820). The reason ALONE cannot identify its
    /// writer — `notStartedReason` is emitted byte-identically by `startDaemon()` and by the launch-time
    /// repair — so the attribution has to be recorded HERE, where the writer is known, rather than
    /// reconstructed downstream from copy that does not distinguish them.
    enum StartPhase: Equatable {
        case idle
        case registering(StartOrigin)
        case failed(reason: String, origin: StartOrigin)
    }

    // MARK: Published state

    /// The app login-item status — drives the "Launch at login" toggle. `private(set)`: it only changes via a
    /// register/unregister or an explicit refresh, never a direct write.
    @Published private(set) var appStatus: LoginItemStatus
    /// The bundled daemon LaunchAgent status — gates the Start affordance. `.notFound` until #171.
    @Published private(set) var daemonStatus: LoginItemStatus
    /// The Start-daemon interaction phase the not-running card observes.
    @Published private(set) var startPhase: StartPhase = .idle

    private let service: LoginItemService

    /// Where the identity of the last successfully-registered daemon executable is remembered across launches
    /// (issue #788) — the other half of the change detector.
    private let registrationStore: DaemonAgentRegistrationStore

    /// The identity of the executable this bundle would register right now (issue #788). A closure, not a
    /// `LoginItemService` member, deliberately: the protocol's four conformances (the real `SMAppService`
    /// adapter, the render-harness stub, the test double, and the accessibility-tree stub) were unchanged by
    /// that feature — the seam it needed already existed in `unregisterDaemonAgent()`. Issue #819 DID need a
    /// new member, and updating all four is what it cost.
    private let agentIdentity: () -> String

    /// The post-register liveness wait (issue #745): how often to re-probe the single-instance lock, and how
    /// long to wait for a daemon to take it, before declaring the start failed. Injectable so tests drive the
    /// poll deterministically without real-time sleeps; the production defaults give a slow launchd `RunAtLoad`
    /// spawn ample room while still surfacing a dead start within a few seconds.
    private let livenessPollInterval: Duration
    private let livenessTimeout: Duration

    /// `registrationStore` defaults to the real `UserDefaults`-backed store — passed as nil rather than as a
    /// `= DaemonAgentRegistrationStore()` default-argument expression because a default argument is evaluated
    /// in the CALLER's isolation, not the callee's, and the store's init is `@MainActor`.
    init(service: LoginItemService,
         registrationStore: DaemonAgentRegistrationStore? = nil,
         agentIdentity: @escaping () -> String = { DaemonAgentIdentity.current() },
         livenessPollInterval: Duration = .milliseconds(500),
         livenessTimeout: Duration = .seconds(8)) {
        self.service = service
        self.registrationStore = registrationStore ?? DaemonAgentRegistrationStore()
        self.agentIdentity = agentIdentity
        self.livenessPollInterval = livenessPollInterval
        self.livenessTimeout = livenessTimeout
        self.appStatus = service.appStatus
        self.daemonStatus = service.daemonAgentStatus
    }

    // MARK: App login item (fully shippable in #170 — no embedded binary needed)

    /// Whether the app is registered to launch at login — the toggle's ON state. BOTH `.enabled` and
    /// `.requiresApproval` read ON: the register succeeded in each; `.requiresApproval` is a separate,
    /// non-failure approval gate the view surfaces (never a reason to show the toggle off).
    var launchAtLoginEnabled: Bool { appStatus == .enabled || appStatus == .requiresApproval }

    /// Whether launch-at-login is enabled but the user must still approve it in System Settings › Login Items.
    /// The view shows an inline hint + a deep-link when true; the toggle stays ON.
    var needsApproval: Bool { appStatus == .requiresApproval }

    /// The toggle intent (bound by the Settings form). Register when turning ON and not already on; unregister
    /// when turning OFF and currently on. Idempotent — a set to the current state is a no-op, so it never
    /// double-registers — and it re-reads the true status afterwards, so a `.requiresApproval` result (or a
    /// failed register that left the item off) is reflected HONESTLY rather than optimistically.
    func setLaunchAtLogin(_ desired: Bool) {
        if desired {
            guard !launchAtLoginEnabled else { return }
            do {
                try service.registerApp()
            } catch {
                loginItemLog.error("login item register failed: \(String(describing: error), privacy: .public)")
            }
        } else {
            guard launchAtLoginEnabled else { return }
            do {
                try service.unregisterApp()
            } catch {
                loginItemLog.error("login item unregister failed: \(String(describing: error), privacy: .public)")
            }
        }
        appStatus = service.appStatus
    }

    /// Idempotent first-launch registration, called from `main.swift` on every launch. A no-op when the app is
    /// already a login item (`.enabled` / `.requiresApproval`) — so re-launches never re-register — otherwise it
    /// registers the app login item. Non-fatal: a register failure is logged and the app carries on (the toggle
    /// still reflects the true, un-registered status). This registers ONLY the app login item, never the daemon
    /// agent (that stays user-initiated via the Start affordance — issue #170 keystone).
    func registerAppLoginItemOnLaunch() {
        guard !launchAtLoginEnabled else { return }
        do {
            try service.registerApp()
        } catch {
            loginItemLog.error("first-launch login item register failed: \(String(describing: error), privacy: .public)")
        }
        appStatus = service.appStatus
    }

    /// Deep-link to System Settings › General › Login Items — the action behind the `.requiresApproval` hint.
    func openLoginItemsSettings() { service.openLoginItemsSettings() }

    // MARK: Daemon agent (Start affordance; #171 activates the bundled plist)

    /// Whether the Start-daemon affordance can act, as the conjunction of three gates: the bundled
    /// agent is registrable (NOT `.notFound` — i.e. #171 has shipped the plist + embedded binary),
    /// the Rust CLI is not already the LaunchAgent owner (the two-owner guard), AND no daemon
    /// currently holds the single-instance lock (the liveness gate, issue #742). The liveness gate
    /// catches what the two-owner check cannot: a manually-run `sessiometer run` has no LaunchAgent
    /// plist, but it DOES hold `daemon.lock`. While any gate fails — the #170 no-plist state, a
    /// CLI-managed daemon, or ANY live daemon — the Start affordance is withheld rather than
    /// offering a misleading action that would register an agent and silently win nothing.
    var canStartDaemon: Bool {
        daemonStatus != .notFound && !service.cliManagedAgentPresent && !service.daemonLockHeld
    }

    /// The "Start daemon" action: register (and, via `RunAtLoad`, start) the embedded daemon LaunchAgent. A
    /// no-op when `canStartDaemon` is false (the button is only offered when it is) or a start is already in
    /// flight. After register succeeds it WAITS for a daemon to actually take the single-instance lock
    /// (issue #745) — landing `.idle` (the panel then leaves `.notRunning` on the next `watch` snapshot) only if
    /// one comes up, else `.failed`, so a dead start is never silent; a register throw surfaces a redacted reason
    /// inline. The `registering` beat is painted before the synchronous register (the `Task.yield()`) and HELD
    /// across the liveness wait, mirroring the swap/capture affordances' pending state.
    func startDaemon() async {
        guard canStartDaemon else { return }
        if case .registering = startPhase { return }

        startPhase = .registering(.operatorStart)
        await Task.yield()  // let the "Starting…" beat paint before the synchronous register
        do {
            try service.registerDaemonAgent()
            daemonStatus = service.daemonAgentStatus
            // Record WHAT was registered (issue #788), on the register succeeding and BEFORE the liveness wait
            // below: launchd holds the registration from that moment, and whether `RunAtLoad` then spawned it
            // is the separate #745 question. Without this, the very next launch would see no recorded identity,
            // read that as "changed", and immediately unregister→re-register an agent registered seconds ago.
            registrationStore.lastRegisteredIdentity = agentIdentity()
            // register() SUCCEEDING only means launchd accepted the plist — the plist's `RunAtLoad` still has to
            // SPAWN the daemon, and that spawn can fail (a bad launchd config, a crash-loop, a sandbox denial)
            // while register() reports success (issue #745). So don't assume `.idle`: wait for a daemon to
            // actually take the single-instance lock, and surface a failure if none does — otherwise the card
            // sits silent forever, since the panel only leaves `.notRunning` on a `watch` snapshot that, with no
            // daemon, never arrives.
            startPhase = await daemonBecameLive()
                ? .idle
                : .failed(reason: Self.notStartedReason, origin: .operatorStart)
        } catch {
            loginItemLog.error("daemon agent register failed: \(String(describing: error), privacy: .public)")
            startPhase = .failed(reason: Self.startFailureReason(error), origin: .operatorStart)
        }
    }

    /// Poll for a daemon to take the single-instance lock after a successful register (issue #745). Reuses the
    /// same `daemonLockHeld` flock probe `canStartDaemon` gates on (issue #742).
    ///
    /// ITS PREMISE, which both callers must establish before calling: the lock was FREE at the decision point,
    /// so a lock that becomes held within the window is the daemon our register just started (via `RunAtLoad`).
    /// `startDaemon()` gets that from `canStartDaemon`, which includes `!daemonLockHeld`. The reconcile gets it
    /// from `foreignLockHolder` being false — and where a foreign holder IS present it does not call this at
    /// all (issue #819), because the poll would then answer on its first read about somebody else's daemon.
    ///
    /// Returns `true` as soon as the lock is held, `false` if the bounded window elapses with no daemon — the
    /// honest "registered but never started" signal. Bounded and cancellation-aware so it never spins the UI.
    private func daemonBecameLive() async -> Bool {
        if service.daemonLockHeld { return true }
        let clock = ContinuousClock()
        let deadline = clock.now.advanced(by: livenessTimeout)
        while clock.now < deadline {
            do { try await Task.sleep(for: livenessPollInterval) }
            catch { return service.daemonLockHeld }  // cancelled — report last-known state, stop polling
            if service.daemonLockHeld { return true }
        }
        return false
    }

    /// Repair a daemon-agent registration left stale by an app update (issue #788) — called from `main.swift`
    /// on every launch, the sibling of `registerAppLoginItemOnLaunch()`. The header's STALE-REGISTRATION
    /// REPAIR note carries the `SMAppService.h` requirement this honors; nothing else can honor it, because
    /// the only other register call site is the Start affordance, which stands down as soon as any daemon
    /// holds the lock (issue #742).
    ///
    /// It REPAIRS, it never initiates: an agent the operator never registered stays unregistered — first
    /// registration is the Start affordance's job, deliberately (the #170 keystone: the app does not enroll a
    /// daemon nobody asked for). Four gates below guard the four ways a repair could do harm, each annotated
    /// at its guard; the first is where the two-owner invariant OUTRANKS the header's "must be re-registered".
    ///
    /// Gate 4 asks whether unloading OUR job would terminate a live daemon, NOT whether any daemon is alive
    /// (issue #819 narrowed it from the latter to the former — see `repairDisplacementCheck()` for the full
    /// table and for what the narrowed signal cannot distinguish). A hand-run `sessiometer run` holding the
    /// lock therefore no longer defers the repair forever, and is not displaced by it either: it is not our
    /// job, so we never unload it.
    ///
    /// Failures surface on the existing not-running card (issue #745's pattern, issue #15's redaction), and —
    /// since issue #820 — they surface there whatever `canStartDaemon` later becomes: the card's reason line is
    /// no longer nested inside the Start affordance's gate, because a daemon taking the lock afterwards used to
    /// erase the reason silently. They are also ATTRIBUTED, via the `.launchRepair` origin every `.failed` below
    /// carries: no press stands behind this repair, so its copy must not read like a failed button. Should the
    /// unregister land but the register throw, the agent is left honestly unregistered with the reason shown and
    /// Start offered — recoverable, never a silent half-state. No new UI is introduced.
    func reconcileDaemonAgentRegistration() async {
        // A repair already in flight is never re-entered. `startDaemon()` carries the mirror guard, so the two
        // paths cannot interleave into a double unregister→register.
        if case .registering = startPhase { return }

        // Gate 1 — a CLI-managed agent is not the app's to touch (issue #170 / #329): re-pointing that
        // LaunchAgent is `sessiometer service install`'s job. The two-owner invariant outranks the SDK.
        guard !service.cliManagedAgentPresent else { return }

        // Gate 2 — only a registration we HOLD can go stale. `.notFound` (no bundled plist) and
        // `.notRegistered` (never started) both mean launchd holds nothing of ours to repair.
        daemonStatus = service.daemonAgentStatus
        guard daemonStatus == .enabled || daemonStatus == .requiresApproval else { return }

        // Gate 3 — only on an ACTUAL change, so repeated launches at one version repair at most once. A nil
        // recorded identity counts as changed: an agent registered by a build that predates this bookkeeping
        // runs an unknown executable, precisely the stale case.
        let identity = agentIdentity()
        guard registrationStore.lastRegisteredIdentity != identity else { return }

        // Gate 4 — never displace a LIVE daemon OF OURS: `unregister()` makes launchd unload the job,
        // terminating the daemon running the OLD executable, and doing that unannounced mid-launch is a
        // silent kill. So defer and retry next launch — logged, never painted `.failed`, which would show
        // an error card over a perfectly healthy state. See `repairDisplacementCheck()` for what "of ours"
        // narrowed to in issue #819, and what the narrowed signal can and cannot distinguish.
        guard case .proceed = repairDisplacementCheck() else { return }

        // Captured so the re-probe deferral below can put the phase back EXACTLY as it found it, matching
        // gate 4, which defers without touching it at all. Forcing `.idle` there instead would make the two
        // deferral paths asymmetric and silently erase a pre-existing `.failed` reason — inert today (one call
        // site, at launch, where the phase is always `.idle`) but live the moment a second call site is added.
        let phaseBeforeRepair = startPhase
        startPhase = .registering(.launchRepair)
        await Task.yield()  // let the "Repairing…" beat paint before the synchronous unregister/register
        // Re-probe across the yield. At login the app (a login item) and the agent (`RunAtLoad`) start
        // CONCURRENTLY — which is exactly the first launch after an update, when the identity has changed — so
        // OUR agent can come up between gate 4 and the unregister below. Re-probing collapses the
        // check-then-act window to the two synchronous calls that follow; it cannot close it (the job belongs
        // to launchd), but it keeps the widest part of the window out of the race. THIS reading of
        // `foreignLockHolder` — not gate 4's — is the one carried forward, because it is the later and
        // therefore closer-to-the-act one.
        guard case .proceed(let foreignLockHolder) = repairDisplacementCheck() else {
            startPhase = phaseBeforeRepair
            return
        }
        do {
            // Unregister BEFORE re-registering, per the header's explicit recommendation for a changed
            // executable — the whole reason this method exists.
            try service.unregisterDaemonAgent()
            try service.registerDaemonAgent()
            daemonStatus = service.daemonAgentStatus
            // Recorded on the REGISTRATION, before the liveness wait, at parity with `startDaemon()`: this
            // store tracks what launchd holds, and launchd holds it from here. The consequence: a repair that
            // registers but whose daemon never SPAWNS lands `.failed` below with the new identity already
            // recorded, so gate 3 short-circuits every later launch and the repair is not retried. Recording
            // only on a confirmed spawn would be worse — a daemon that cannot start (bad config, crash-loop)
            // would then be unregistered and re-registered on EVERY launch, the storm gate 3 exists to
            // prevent. So recovery is deliberately the operator's: the reason below, and the Start affordance.
            // Issue #820 is what makes that reason actually READABLE — it is no longer erased by a daemon
            // taking the lock after the fact, which is the only thing that made this trade survivable.
            registrationStore.lastRegisteredIdentity = identity
            if foreignLockHolder {
                // The liveness wait CANNOT answer here, so it is not asked: a foreign holder falsifies the
                // premise `daemonBecameLive()` states, and the poll would attribute that daemon to this
                // repair. `.failed` would be wrong on the facts too — the registration this method exists to
                // repair SUCCEEDED, and a daemon IS serving — and since issue #820 decoupled the card's
                // reason from `canStartDaemon`, a wrong `.failed` here is no longer hidden by the held lock
                // but plainly visible. So land `.idle`, and say in the log what was NOT established.
                loginItemLog.info("daemon agent re-registered: \(Self.foreignHolderLivenessNote, privacy: .public)")
                startPhase = .idle
            } else {
                // Same #745 honesty as `startDaemon()`: a register that launchd ACCEPTS still has to be
                // SPAWNED by `RunAtLoad`, and that spawn can fail silently. The lock was free at the re-probe,
                // so a lock taken within the window is the daemon this re-registration started.
                startPhase = await daemonBecameLive()
                    ? .idle
                    : .failed(reason: Self.notStartedReason, origin: .launchRepair)
            }
        } catch {
            loginItemLog.error(
                "daemon agent re-registration failed: \(String(describing: error), privacy: .public)")
            daemonStatus = service.daemonAgentStatus
            startPhase = .failed(reason: Self.reregisterFailureReason(error), origin: .launchRepair)
        }
    }

    /// Whether unregistering the bundled agent right now could terminate a live daemon (issue #819) — asked
    /// at gate 4 and again after the yield, so the two checks cannot drift apart.
    private enum RepairDisplacement: Equatable {
        /// Unloading our job terminates nothing. `foreignLockHolder` reports whether some OTHER daemon holds
        /// the single-instance lock — not a reason to stop (it is not ours to unload) but load-bearing
        /// afterwards, because the post-register liveness wait must not misattribute that daemon to us.
        case proceed(foreignLockHolder: Bool)
        /// Unloading our job could terminate a live daemon, or the probe could not tell. Every postponement
        /// is logged with its reason at the point of decision (`postpone(because:)`), so this case carries
        /// no payload.
        case postpone
    }

    /// Gate 4's question, narrowed by issue #819 — and the narrowing is the whole fix.
    ///
    /// Issue #788 asked "does ANY daemon hold the single-instance lock?" via `daemonLockHeld`, which is
    /// any-provenance BY DESIGN (issue #742). On a machine whose daemon is run by hand — a provenance #742
    /// explicitly supports — that answer is permanently yes, so the repair deferred forever and #788 was
    /// inert exactly where it was needed.
    ///
    /// The question gate 4 actually needs is narrower: `unregister()` unloads OUR launchd job, and unloading
    /// a job can only terminate a daemon IF THAT JOB IS RUNNING. A hand-run `sessiometer run` is not our job
    /// and is not unloaded by us, whether or not it holds the lock. So ask launchd about our own label
    /// (`daemonAgentRunState`) instead of asking the lock about everyone.
    ///
    /// The resulting table, against #788:
    ///
    /// | our job       | lock | verdict  | vs. issue #788                                   |
    /// |---------------|------|----------|--------------------------------------------------|
    /// | `.running`    | any  | postpone | STRICTER (#788 proceeded when the lock was free) |
    /// | `.notRunning` | free | proceed  | same                                             |
    /// | `.notRunning` | held | proceed  | **the issue #819 fix** — #788 deferred forever   |
    /// | `.unknown`    | free | proceed  | same                                             |
    /// | `.unknown`    | held | postpone | same                                             |
    ///
    /// So it is never more destructive than #788 in any cell, and strictly less inert in one. The `.running`
    /// row is deliberately stricter than #788: our job running while the lock is free is a transient (the
    /// daemon is starting up, or standing down and about to exit), and unloading it there WOULD be the silent
    /// kill gate 4 exists to prevent. The next launch repairs.
    ///
    /// WHAT THIS CANNOT DISTINGUISH, stated rather than implied: it does not identify the lock's holder.
    /// `foreignLockHolder` is an inference by ELIMINATION — our job is not running, so the daemon holding the
    /// lock is not one we started — not direct evidence about that process. And `.unknown` is a genuine
    /// "cannot tell", which is why it falls back to #788's gate rather than guessing either way.
    private func repairDisplacementCheck() -> RepairDisplacement {
        switch service.daemonAgentRunState {
        case .running:
            return postpone(because: Self.ownJobRunningDeferral)
        case .notRunning:
            // Our job has no process behind it, so the unregister below terminates nothing — whoever holds
            // the lock. Reading the lock here is not a gate; it records what the liveness wait must not
            // misattribute afterwards.
            return .proceed(foreignLockHolder: service.daemonLockHeld)
        case .unknown:
            // No answer about our own job ⇒ fall back EXACTLY to issue #788's any-provenance gate. Never
            // better than #788 here, and never worse.
            guard service.daemonLockHeld else { return .proceed(foreignLockHolder: false) }
            return postpone(because: Self.unknownRunStateDeferral)
        }
    }

    /// Log a gate-4 postponement and report it. Both deferral paths share one message prefix, and `Logger`
    /// needs that prefix as a LITERAL (`OSLogMessage`), so it cannot be folded into the reason constants —
    /// here is the one place it can live once. Returning the verdict rather than only logging is what keeps
    /// "logged" and "postponed" impossible to do apart.
    private func postpone(because reason: String) -> RepairDisplacement {
        loginItemLog.info("daemon agent re-registration deferred: \(reason, privacy: .public)")
        return .postpone
    }

    /// Re-read both statuses from the OS — called when the Settings window (re)opens and when the app becomes
    /// active, so a login-item change the user made directly in System Settings is reflected without a relaunch.
    func refreshStatus() {
        appStatus = service.appStatus
        daemonStatus = service.daemonAgentStatus
    }

    // The three issue #819 log reasons. Constants rather than inline literals because `Logger` takes an
    // `OSLogMessage` (a literal with interpolations), which a `"…" + "…"` concatenation is not — and each of
    // these is too long for one line. They are LOG copy, not UI copy: no `.failed` card is painted on any of
    // these paths, so none of them reaches the operator's screen.

    /// Gate 4's `.running` deferral — the invariant, stated as the reason.
    private static let ownJobRunningDeferral =
        "our LaunchAgent's job is running, so unloading it would terminate that daemon"

    /// Gate 4's `.unknown` deferral — the honest "I could not tell, so I fell back" note.
    private static let unknownRunStateDeferral =
        "launchd's run state for our job is unknown and a daemon holds the single-instance lock"

    /// The foreign-holder repair's note. It says what was NOT established, so a reader of the log is never
    /// left to infer that this repair's own daemon was seen to come up.
    ///
    /// It says NEXT LOGIN, not "when that daemon exits", and the distinction is load-bearing. `register()`
    /// fires the plist's `RunAtLoad`; `run --managed` finds the lock held and exits 0 — a CLEAN stand-down,
    /// which the bundled plist's conditional `KeepAlive` (`SuccessfulExit: false`) deliberately does NOT
    /// respawn (`src/service.rs`, issue #742). So nothing re-triggers our job when the other daemon later
    /// exits; launchd starts it at the next login. Promising the sooner recovery would send an operator to
    /// kill the daemon they have and be left with none.
    private static let foreignHolderLivenessNote =
        "another daemon holds the single-instance lock, so this registration's own startup is not observable "
        + "from here; it will start at the next login"

    /// The #745 silent-failure copy: register succeeded but no daemon took the lock within the liveness window.
    /// There is no OS error to surface (register did NOT throw), so this is a plain, actionable statement rather
    /// than a redacted message — and, like every `.failed` reason, it carries no credential (issue #15).
    private static let notStartedReason =
        "The daemon was registered but didn’t start. Check Console for details."

    /// A redacted, non-secret reason for a failed daemon start (issue #15) — a registration error carries no
    /// credential, so the OS message is safe to surface, with a plain fallback when it is empty.
    private static func startFailureReason(_ error: Error) -> String {
        let message = (error as NSError).localizedDescription
        return message.isEmpty ? "The daemon couldn’t be started." : message
    }

    /// The re-registration counterpart of `startFailureReason` (issue #788) — same redaction discipline
    /// (issue #15), only the empty-message fallback differs, naming the operation that failed.
    ///
    /// The fallback deliberately does NOT say "after the app update" (issue #820): every call site passes
    /// `origin: .launchRepair`, so the card already prefixes it with `startDaemonRepairAttribution`, and
    /// carrying the occasion here as well rendered "…after an app update — …after the app update."
    private static func reregisterFailureReason(_ error: Error) -> String {
        let message = (error as NSError).localizedDescription
        return message.isEmpty ? "The daemon couldn’t be re-registered." : message
    }
}
