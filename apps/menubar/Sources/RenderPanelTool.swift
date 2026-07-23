// Copyright (c) 2026 Oleksii PELYKH
// SPDX-License-Identifier: MIT

// Offscreen design-parity renderer — TOOLING ONLY, compiled in DEBUG. Invoked as
// `Sessiometer.app/Contents/MacOS/Sessiometer --render-panel <dir>` (see `AppDelegate`): it renders
// `StatusPanelView` for the mock's "Healthy · Status" fixture to `panel-healthy-{light,dark}.png` via
// SwiftUI `ImageRenderer`, then exits WITHOUT starting the menu-bar app.
//
// Why it exists: the panel is an `NSPopover`-hosted view that can't be opened programmatically and
// can't be screen-captured without Screen-Recording TCC, so design-parity against the canonical mock
// (`apps/menubar/design/menubar-preview.html`) had no self-service path. `ImageRenderer` draws the view
// straight to a bitmap — no popover, no screen capture, no permission — giving a committable render to
// diff against the mock. It seeds a `WatchStatusStore.preview` (no transport), so it renders the SAME
// `@Published` state the panel reads, only pinned rather than machine-derived.

#if DEBUG
import AppKit
import SwiftUI

@MainActor
enum RenderPanelTool {
    /// A named panel state to render, so one run emits the whole set the panel supports for a
    /// screen-by-screen diff against the mock's `.pop` states.
    private struct Fixture {
        let name: String
        let state: ConnectionState
        let rows: [AccountRow]
        let nextSwap: NextSwap?
        let generatedAt: Int64?
        // Three of the four daemon-level payload faults `StatusPanelFormat.daemonFaultBanner` ranks
        // worst-first (#592). The fourth — the #714/#728 behavioral canary — is deliberately NOT modeled here
        // yet: its visual oracle needs matching canary fault frames in the mock (`menubar-preview.html`) to
        // pair against, which is design-SSOT work tracked as the fault-family visual-oracle follow-up (#571).
        // `var` with a default, not `let` — Swift's memberwise init defaults `var` properties but EXCLUDES
        // defaulted `let`s, so `let` here would make these unreachable from the fixture list.
        var keychainLocked: Bool = false
        var canonicalScrub: CanonicalScrub?
        var systemicRefreshFailure: UInt32?
        // The loaded Stats-tab series (#704). Non-nil ONLY on the `stats` fixture: it seeds a `PanelStatsModel`
        // to `.stats`/`.loaded` so the render shows the account cards, not the Status glance. `var` with a
        // default for the same memberwise-init reason as the payload faults above.
        var statsWire: StatsWire?
    }

    /// Render every panel-supported state (light + dark) into `outputDir` as `panel-<state>-<theme>.png`.
    /// Any failure is written to stderr; the caller (`AppDelegate`) exits after this returns.
    static func run(outputDir: String) {
        let now = Int64(Date().timeIntervalSince1970)
        let day: Int64 = 86_400

        // The mock's "Healthy · Status" example rows — same percents + layout, so the render is directly
        // comparable: Work active 42/88, Personal 31/71, Temp 4/18 — next swap → Temp. The third account is
        // "Temp" where the mock illustrates "Scratch": re-picked so all three labels hash to DISTINCT #445
        // palette slots (the mock's "Personal" + "Scratch" both land on slot 5 / ochre under the shared 8-slot
        // label hash), so the committed oracle shows three visibly-distinct identity colours — violet / ochre /
        // teal (#709). The provider secondary line (#173) and the "Last swap …" footer (#88) are the documented
        // Wave-1 reconciliations and correctly do NOT appear.
        let rows = [
            AccountRow(label: "Work", isActive: true, isEnabled: true, isQuarantined: false,
                       isRecovering: false, auth: .healthy, sessionPct: 42, weeklyPct: 88,
                       sessionResetsAt: now + 2 * 3600 + 14 * 60, weeklyResetsAt: now + 3 * day,
                       weeklyExhausted: false, isNextSwapTarget: false, blindActive: nil),
            AccountRow(label: "Personal", isActive: false, isEnabled: true, isQuarantined: false,
                       isRecovering: false, auth: .healthy, sessionPct: 31, weeklyPct: 71,
                       sessionResetsAt: now + 3600 + 2 * 60, weeklyResetsAt: now + 3 * day,
                       weeklyExhausted: false, isNextSwapTarget: false, blindActive: nil),
            AccountRow(label: "Temp", isActive: false, isEnabled: true, isQuarantined: false,
                       isRecovering: false, auth: .healthy, sessionPct: 4, weeklyPct: 18,
                       sessionResetsAt: now + 5 * 3600 + 20 * 60, weeklyResetsAt: now + 3 * day,
                       weeklyExhausted: false, isNextSwapTarget: true, blindActive: nil),
        ]

        // The active-account bounded-blindness rosters (#479/#485) — the ACTIVE "Work" row carries a
        // `blind_active` projection (its live meters are replaced by the SEMANTIC held-state block); the
        // siblings stay healthy. These give the mock's blind frames (`menubar-preview.html`, #571) a matching
        // built-panel capture, so the design-vs-capture harness can cover the blind row. The whole-snapshot
        // stays `.connected` — blindness is a per-row modifier, NOT a 10th daemon-state, and the header +
        // footer stay fresh (the locality that distinguishes it from a whole-snapshot `stale`, #137).
        // Only `blind.lastKnownSessionPct` drives the render (the held bar) — while blind, BOTH live meters
        // are replaced by the held block, so the row's own `sessionPct` / `weeklyPct` are inert. `sessionPct`
        // mirrors the blind anchor (so a non-blind read of the row agrees with the held bar instead of
        // contradicting it); `weeklyPct` stays at the healthy-Work value.
        func blindWork(_ blind: BlindActive) -> AccountRow {
            AccountRow(label: "Work", isActive: true, isEnabled: true, isQuarantined: false,
                       isRecovering: false, auth: .healthy, sessionPct: blind.lastKnownSessionPct,
                       weeklyPct: 88, sessionResetsAt: now + 2 * 3600 + 14 * 60, weeklyResetsAt: now + 3 * day,
                       weeklyExhausted: false, isNextSwapTarget: false, blindActive: blind)
        }
        // OK: last-known session 58% (green band), blind 3m, auto-protection self-resolving.
        let blindOKRows = [blindWork(BlindActive(blindSecs: 180, lastKnownSessionPct: 58,
                                                 autoProtectionDegraded: false)), rows[1], rows[2]]
        // DEGRADED: last-known session 88% (amber band), blind 11m, auto-protection acting on a stale anchor
        // → orange eye-slash + orange leading rule + orange verdict.
        let blindDegradedRows = [blindWork(BlindActive(blindSecs: 660, lastKnownSessionPct: 88,
                                                       autoProtectionDegraded: true)), rows[1], rows[2]]
        // CORNERED (#572): blind + DEGRADED + no viable target — last-known session 92% (red band), blind
        // 18m → RED eye-slash + red leading rule + red "CANNOT ACT" verdict + the "add or free an account"
        // remedy. The siblings are BOTH weekly-exhausted (WHY there is no target), so each renders the
        // `nosign` switch chip. The cornered-ness is composed at render from `blind_active` (degraded) +
        // the fixture's `next_swap == .noViableTarget` — no new wire field.
        let exhaustedPersonal = AccountRow(label: "Personal", isActive: false, isEnabled: true,
                                           isQuarantined: false, isRecovering: false, auth: .healthy,
                                           sessionPct: 14, weeklyPct: 100, sessionResetsAt: now + 2 * 3600,
                                           weeklyResetsAt: now + 2 * day + 4 * 3600, weeklyExhausted: true,
                                           isNextSwapTarget: false, blindActive: nil)
        let exhaustedTemp = AccountRow(label: "Temp", isActive: false, isEnabled: true,
                                       isQuarantined: false, isRecovering: false, auth: .healthy,
                                       sessionPct: 6, weeklyPct: 97, sessionResetsAt: now + 4 * 3600 + 50 * 60,
                                       weeklyResetsAt: now + 3 * day + 3600, weeklyExhausted: true,
                                       isNextSwapTarget: false, blindActive: nil)
        let blindCorneredRows = [blindWork(BlindActive(blindSecs: 1080, lastKnownSessionPct: 92,
                                                       autoProtectionDegraded: true)),
                                 exhaustedPersonal, exhaustedTemp]

        // The panel-rendered states (the fuller 9-state fidelity's remaining facets are #169 siblings).
        // `stale` and `disconnected` retain the last-good roster (disconnected dims it); the account-less
        // states — including `crashLooping` (#169), which refuses the held snapshot's numbers behind an
        // honest message card — show a banner / onboarding card. Ages chosen so the footer reads live /
        // stale as intended.
        let fixtures = [
            Fixture(name: "healthy", state: .connected, rows: rows,
                    nextSwap: .target(to: "Temp", reason: .soonestReset(resetsAt: now + 3 * day)),
                    generatedAt: now - 12),
            // #704: the healthy roster's STATS tab — the ONE fixture seeded to `.stats`/`.loaded` (every other
            // renders the Status glance). Reuses the healthy roster so the Stats rows join the same
            // Work/Personal/Temp identities (active = Work) the mock's `healthy-stats-*` frames show; the
            // loaded series rides `statsWire`. State stays `.connected` because `StatusPanelView` offers the
            // Stats seg only over a live roster (`.connected`/`.stale`) — a Stats tab on a degraded daemon
            // could only fail. `next_swap` is inert here (the Stats tab renders no footer).
            Fixture(name: "stats", state: .connected, rows: rows,
                    nextSwap: .target(to: "Temp", reason: .soonestReset(resetsAt: now + 3 * day)),
                    generatedAt: now - 12, statsWire: PanelStatsModel.loadedPreviewFixture),
            Fixture(name: "stale", state: .stale, rows: rows,
                    nextSwap: .target(to: "Temp", reason: .soonestReset(resetsAt: now + 3 * day)),
                    generatedAt: now - 5400),
            Fixture(name: "disconnected", state: .disconnected(reason: "the daemon is not responding"),
                    rows: rows, nextSwap: nil, generatedAt: now - 240),
            Fixture(name: "connecting", state: .connecting, rows: [], nextSwap: nil, generatedAt: nil),
            // #499: the cold-refused daemon-absent states (no reading ever held) — a forming card for
            // starting, and the not-running card. #170 adds the Start-daemon affordance to the not-running
            // card: the harness seeds `canStartDaemon` true (see the render loop) so this fixture shows the
            // mock's Start button; the shipped app gates it off until #171 bundles the agent plist.
            Fixture(name: "starting", state: .starting, rows: [], nextSwap: nil, generatedAt: nil),
            Fixture(name: "not-running", state: .notRunning, rows: [], nextSwap: nil, generatedAt: nil),
            Fixture(name: "crash-looping", state: .crashLooping, rows: [], nextSwap: nil, generatedAt: nil),
            Fixture(name: "unsupported", state: .unsupported, rows: [], nextSwap: nil, generatedAt: nil),
            Fixture(name: "empty-roster", state: .emptyRoster, rows: [], nextSwap: nil, generatedAt: nil),
            // #571: the active-account blind row, OK + DEGRADED — a per-row modifier on a `.connected`
            // snapshot (fresh header/footer), rendered as the held session bar + auto-protection verdict.
            Fixture(name: "blind-ok", state: .connected, rows: blindOKRows,
                    nextSwap: .target(to: "Temp", reason: .soonestReset(resetsAt: now + 3 * day)),
                    generatedAt: now - 12),
            Fixture(name: "blind-degraded", state: .connected, rows: blindDegradedRows,
                    nextSwap: .target(to: "Temp", reason: .soonestReset(resetsAt: now + 3 * day)),
                    generatedAt: now - 12),
            // #572: the CORNERED blind row — blind + DEGRADED + no viable target. `next_swap` is
            // `.noViableTarget` (every spare weekly-exhausted, capacity back in 2d 4h), the signal the panel
            // composes with the row's `autoProtectionDegraded` into the red "cannot act" verdict + remedy.
            Fixture(name: "blind-cornered", state: .connected, rows: blindCorneredRows,
                    nextSwap: .noViableTarget(cause: .weekly, resetsAt: now + 2 * day + 4 * 3600),
                    generatedAt: now - 12),
        // …plus the four daemon-level FAULT ranks (#592) — appended rather than inlined because they vary a
        // different axis: same `.connected` state and same healthy roster, differing only in which payload
        // fault is set. See `faultFixtures`.
        ] + faultFixtures(rows: rows, now: now, day: day)

        for fixture in fixtures {
            let store = WatchStatusStore.preview(state: fixture.state, rows: fixture.rows,
                                                 nextSwap: fixture.nextSwap, generatedAt: fixture.generatedAt,
                                                 canonicalScrub: fixture.canonicalScrub,
                                                 keychainLocked: fixture.keychainLocked,
                                                 systemicRefreshFailure: fixture.systemicRefreshFailure)
            for scheme in [ColorScheme.light, .dark] {
                let theme = scheme == .light ? "light" : "dark"
                let name = "panel-\(fixture.name)-\(theme).png"
                // Inject the COMPLETE panel environment via the shared `statusPanelEnvironment` modifier — the
                // SAME wiring `StatusItemController` uses for the live app, so the harness and the app cannot
                // drift and every `@EnvironmentObject` the panel reads is resolved instead of trapping (issue
                // #504: missing `PanelStatsModel` here was exactly that drift). All three take a NIL client, so
                // nothing here touches a socket:
                //   • `AccountCaptureModel` renders at `.idle` with `captureSurfaceRequested == false`, so the
                //     populated fixtures show the roster with NO capture bar (capture is off-panel / empty-
                //     roster only now, #394) and the empty-roster fixture shows the onboarding card. The nil
                //     client renders the idle field/button and never touches a socket — the label field itself
                //     stays a known ImageRenderer blank (see design/README.md).
                //   • `AccountSwapModel` renders at `.idle`, so the fixtures capture the RESTING row (no hover,
                //     no pending). As of #448 the per-row switch chip is PERSISTENT, so its resting glyph
                //     (`arrow.left.arrow.right`, or the `nosign` on a non-viable row) IS captured in a static
                //     render; only the ARMED hover/focus brighten and the in-flight `Switching…` spinner stay a
                //     manual-check surface (#380).
                //   • `PanelStatsModel` (#446) renders at its default `.status` tab / `.idle` phase for every
                //     fixture EXCEPT `stats` (#704), which `loadedPreview` seeds straight to `.stats`/`.loaded`
                //     from `loadedPreviewFixture` so the render shows the account cards. BOTH stay socket-free:
                //     the default nil client never fires a `stats` query, and the seeded fixture sets its phase
                //     directly rather than loading — so `--render-panel` stays as offline as the rest.
                let stats = fixture.statsWire.map { PanelStatsModel.loadedPreview($0) }
                    ?? PanelStatsModel(client: nil)
                // #170: a hermetic login-item model seeded so `canStartDaemon` is TRUE — the not-running
                // fixture then renders the mock's Start-daemon button (the DESIGN-TARGET state). The shipped
                // #170 app gates that button OFF until #171 bundles the agent plist; this render is a design
                // oracle against the mock (which shows the button), NOT a capture of the #170 runtime's inert-
                // banner state. Every other fixture carries the model inert (its state renders no Start card).
                let loginItem = LoginItemModel(service: PreviewLoginItemService())
                let view = StatusPanelView()
                    .statusPanelEnvironment(store: store,
                                            capture: AccountCaptureModel(client: nil),
                                            swap: AccountSwapModel(client: nil),
                                            stats: stats,
                                            loginItem: loginItem)
                    .environment(\.colorScheme, scheme)
                let renderer = ImageRenderer(content: view)
                renderer.scale = 2
                guard let cg = renderer.cgImage else {
                    FileHandle.standardError.write(Data("render failed: \(name)\n".utf8))
                    continue
                }
                write(cg, to: outputDir + "/" + name)
            }
        }
    }

    /// The four NON-CANARY daemon-level FAULT fixtures (#592) — the four ranks of
    /// `StatusPanelFormat.daemonFaultBanner`'s worst-first resolver whose banners this harness can render
    /// standalone, so the shipped banner family has a VISUAL oracle to set beside the mock's fault frames
    /// (`menubar-preview.html`). Until these, `RenderPanelTool` rendered none of the family, so
    /// `design/build-comparison.py` had nothing to pair against and the severity ranking — a *visual* claim —
    /// was defended by format-layer unit tests alone. The resolver now spans SEVEN ranks over FOUR faults
    /// (#714/#728 added the canary refusal pair at ranks 3-4 and an overridden drift at rank 6); the three
    /// canary ranks are NOT rendered here yet — their oracle needs matching canary frames in the mock, tracked
    /// as the fault-family visual-oracle follow-up (#571). So the four fixtures below are ranks 1, 2, 5, and 7.
    ///
    /// All four ride a `.connected` snapshot over the SAME healthy green roster, deliberately: a daemon-level
    /// fault is exactly the one NO per-row `auth` cell reflects, so "full green roster under a loud banner" is
    /// the state these banners exist to contradict — not an inconsistency in the fixture. Header and footer
    /// stay fresh for the same reason: the fault is the DAEMON's, not the snapshot's (never a whole-snapshot
    /// `stale`, #137).
    ///
    /// Rendering the calm rank 7 alongside the louder ranks is the point rather than redundancy: rank 5
    /// (systemic, `.warning`) has to be SEEN to beat rank 7 (`recovering`, `.info`), and an inversion between
    /// those two is precisely the regression `daemonFaultBanner` documents at length. One frame each is what
    /// makes the (fault, VARIANT) ordering reviewable instead of asserted.
    private static func faultFixtures(rows: [AccountRow], now: Int64, day: Int64) -> [Fixture] {
        // The healthy next-swap the roster would carry regardless — ranks 3-4 leave swapping alive, and even
        // where the daemon is blocked the panel still states its recommendation.
        let nextSwap = NextSwap.target(to: "Temp", reason: .soonestReset(resetsAt: now + 3 * day))
        func fault(_ name: String, _ apply: (inout Fixture) -> Void) -> Fixture {
            var fixture = Fixture(name: name, state: .connected, rows: rows,
                                  nextSwap: nextSwap, generatedAt: now - 12)
            apply(&fixture)
            return fixture
        }
        return [
            // Rank 1 — the login keychain is LOCKED, so the shared item is unreadable. `.error`; remedy is
            // UNLOCK, never `claude /login` (#498).
            fault("fault-keychain-locked") { $0.keychainLocked = true },
            // Rank 2 — the shared canonical is scrubbed AND recovery is exhausted: an act-now lockout whose
            // remedy is `claude /login` (#469). `.error`.
            fault("fault-scrub-exhausted") { $0.canonicalScrub = .exhausted },
            // Rank 5 — the refresh MECHANISM is down. `.warning`, not `.error`: every account still works, so
            // it is a pre-death "next break" task, ranked deliberately ABOVE the calm scrub below (#523). The
            // count is plural-agreeing, so 3 exercises the "sweeps" arm rather than the n=1 floor. (Ranks 3-4,
            // the #714 canary refusal pair, sit ABOVE this but are not rendered here — see the doc above.)
            fault("fault-systemic-refresh") { $0.systemicRefreshFailure = 3 },
            // Rank 7 — scrubbed but self-healing. `.info`, and the LOWEST claim on the one banner slot
            // precisely because its whole message is "no action needed" — a self-healing state can never
            // outrank one that cannot self-heal. (Rank 6, an overridden canary drift, sits just above but is
            // not rendered here — see the doc above.)
            fault("fault-scrub-recovering") { $0.canonicalScrub = .recovering },
        ]
    }

    private static func write(_ cg: CGImage, to path: String) {
        let rep = NSBitmapImageRep(cgImage: cg)
        guard let png = rep.representation(using: .png, properties: [:]) else {
            FileHandle.standardError.write(Data("PNG encode failed: \(path)\n".utf8))
            return
        }
        do {
            try png.write(to: URL(fileURLWithPath: path))
            FileHandle.standardOutput.write(Data("wrote \(path) (\(cg.width)x\(cg.height))\n".utf8))
        } catch {
            FileHandle.standardError.write(Data("write failed \(path): \(error)\n".utf8))
        }
    }
}

/// A hermetic `LoginItemService` for the render harness (#170) — NO `SMAppService`, no OS calls. Seeded so
/// `canStartDaemon` is TRUE (a registrable daemon agent — `.notRegistered`, NOT `.notFound` — and no
/// CLI-owned agent), so the `not-running` fixture renders the mock's Start-daemon affordance. Only that
/// fixture reads it; the model rides inert in every other fixture's environment. Register/unregister are
/// no-ops — a design render never mutates real login-item state. Tooling-only (DEBUG, app target), so it
/// never reaches `MenubarTests` (whose own `FakeLoginItemService` drives `LoginItemModelTests`).
private final class PreviewLoginItemService: LoginItemService {
    let appStatus: LoginItemStatus = .enabled
    let daemonAgentStatus: LoginItemStatus = .notRegistered
    let cliManagedAgentPresent: Bool = false
    func registerApp() throws {}
    func unregisterApp() throws {}
    func registerDaemonAgent() throws {}
    func unregisterDaemonAgent() throws {}
    func openLoginItemsSettings() {}
}
#endif
