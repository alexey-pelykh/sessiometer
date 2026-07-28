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
    enum StartPhase: Equatable {
        case idle
        case registering
        case failed(reason: String)
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
    /// `LoginItemService` member, deliberately: the protocol's three conformances (the real `SMAppService`
    /// adapter, the render-harness stub, the test double) are unchanged by this feature — the seam it needed
    /// already existed in `unregisterDaemonAgent()`.
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

        startPhase = .registering
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
                : .failed(reason: Self.notStartedReason)
        } catch {
            loginItemLog.error("daemon agent register failed: \(String(describing: error), privacy: .public)")
            startPhase = .failed(reason: Self.startFailureReason(error))
        }
    }

    /// Poll for a daemon to take the single-instance lock after a successful register (issue #745). Reuses the
    /// same `daemonLockHeld` flock probe `canStartDaemon` gates on (issue #742): `canStartDaemon` guaranteed the
    /// lock was FREE at the button press, so a lock that becomes held within the window is the daemon our
    /// register just started (via `RunAtLoad`). Returns `true` as soon as the lock is held, `false` if the
    /// bounded window elapses with no daemon — the honest "registered but never started" signal. Bounded and
    /// cancellation-aware so it never spins the UI.
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
    /// The gate-4 deferral is self-healing for the case the header actually warns about — a stale registration
    /// that "MAY not launch" leaves the lock free, so the next launch finds it and repairs — but NOT in
    /// general: `daemonLockHeld` is ANY-provenance by design (issue #742), so a hand-run `sessiometer run`
    /// holds the lock while OUR agent may be registered-but-not-running, deferring the repair indefinitely.
    /// That is the conservative direction — a working setup is never disturbed — at the cost of this reconcile
    /// being inert on a machine whose daemon is run by hand. A provenance-aware gate would need a signal the
    /// `LoginItemService` seam does not carry.
    ///
    /// Failures surface on the existing not-running card (issue #745's pattern, issue #15's redaction), which
    /// is visible in exactly the state this runs in: no daemon holds the lock, so the panel is `.notRunning`
    /// and `canStartDaemon` is true. Should the unregister land but the register throw, the agent is left
    /// honestly unregistered with the reason shown and Start offered — recoverable, never a silent half-state.
    /// No new UI is introduced.
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

        // Gate 4 — never displace a LIVE daemon: `unregister()` makes launchd unload the job, terminating the
        // daemon running the OLD executable, and doing that unannounced mid-launch is a silent kill. So defer
        // and retry next launch — logged, never painted `.failed`, which would show an error card over a
        // perfectly healthy state.
        guard !service.daemonLockHeld else {
            loginItemLog.info("daemon agent re-registration deferred: a live daemon holds the single-instance lock")
            return
        }

        // Captured so the re-probe deferral below can put the phase back EXACTLY as it found it, matching
        // gate 4, which defers without touching it at all. Forcing `.idle` there instead would make the two
        // deferral paths asymmetric and silently erase a pre-existing `.failed` reason — inert today (one call
        // site, at launch, where the phase is always `.idle`) but live the moment a second call site is added.
        let phaseBeforeRepair = startPhase
        startPhase = .registering
        await Task.yield()  // let the "Starting…" beat paint before the synchronous unregister/register
        // Re-probe across the yield. At login the app (a login item) and the agent (`RunAtLoad`) start
        // CONCURRENTLY — which is exactly the first launch after an update, when the identity has changed — so
        // a daemon can take the lock between gate 4 and the unregister below. Re-probing collapses the
        // check-then-act window to the two synchronous calls that follow; it cannot close it (the lock belongs
        // to another process), but it keeps the widest part of the window out of the race.
        guard !service.daemonLockHeld else {
            loginItemLog.info("daemon agent re-registration deferred: a daemon took the lock while starting up")
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
            // Issue #820 tracks where that reason can go unread.
            registrationStore.lastRegisteredIdentity = identity
            // Same #745 honesty as `startDaemon()`: a register that launchd ACCEPTS still has to be SPAWNED by
            // `RunAtLoad`, and that spawn can fail silently. The lock was free at gate 4, so a lock taken
            // within the window is the daemon this re-registration started.
            startPhase = await daemonBecameLive() ? .idle : .failed(reason: Self.notStartedReason)
        } catch {
            loginItemLog.error(
                "daemon agent re-registration failed: \(String(describing: error), privacy: .public)")
            daemonStatus = service.daemonAgentStatus
            startPhase = .failed(reason: Self.reregisterFailureReason(error))
        }
    }

    /// Re-read both statuses from the OS — called when the Settings window (re)opens and when the app becomes
    /// active, so a login-item change the user made directly in System Settings is reflected without a relaunch.
    func refreshStatus() {
        appStatus = service.appStatus
        daemonStatus = service.daemonAgentStatus
    }

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
    /// (issue #15), only the empty-message fallback differs, naming the situation the operator is actually in.
    private static func reregisterFailureReason(_ error: Error) -> String {
        let message = (error as NSError).localizedDescription
        return message.isEmpty ? "The daemon couldn’t be re-registered after the app update." : message
    }
}
