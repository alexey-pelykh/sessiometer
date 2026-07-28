// Copyright (c) 2026 Oleksii PELYKH
// SPDX-License-Identifier: MIT

// The ONE definition of *which* panel states get rendered offscreen and *how* (issue #754) — the shared
// engine behind two consumers that must not drift apart:
//
//   • `RenderPanelTool` (app target, `--render-panel <dir>`) — the human design-parity oracle. It writes
//     the PNGs `design/build-comparison.py` pairs against the mock's `.pop` frames.
//   • `PanelGoldenParityTests` (MenubarTests) — the automated DRIFT gate. It re-renders the same fixtures
//     in-process and diffs them against the committed goldens under `design/renders/panel-goldens/`.
//
// WHY SHARED, not copied. Issue #504 is the local precedent: `StatusPanelView` grew a new
// `@EnvironmentObject`, the always-run app host was updated, the rarely-run render harness was not, and
// `--render-panel` trapped at render time. The fix was one injection seam (`statusPanelEnvironment`) both
// hosts route through. A fixture list duplicated between the app tool and the test bundle would rot the
// same way — the gate would defend a stale set of states while the oracle rendered a newer one, and the
// green would mean nothing. So the fixture catalog, the render call, and the file-naming all live here
// exactly once.
//
// DETERMINISM — what makes these renders diffable at all. The panel's clock is `TimelineView`'s
// `context.date` (issue #326: reset-in is computed against the client's own wall clock), which no seam
// can pin. So the fixtures are seeded from a caller-supplied `now` and every clock-relative offset is
// deliberately placed AWAY from a `StatusPanelFormat.humanizeUntil` / `snapshotAgeText` rounding
// boundary: the sub-second gap between building a fixture and rasterizing it can shift the rendered
// delta by a second or two, and a boundary-adjacent offset would flip "2h14m" → "2h13m" and redden the
// gate at random. `boundaryGuardSecs` below is that margin, and `PanelGoldenParityTests` asserts the
// stability empirically (two renders taken seconds apart must score exactly 0).
//
// SCALE. Both consumers render at `scale` = 2, matching the committed `panel-healthy-*.png` oracle and
// the Retina surface the panel actually ships on.
//
// HOST-EQUIVALENCE, measured. Because both consumers route through this one `render`, the app tool and the
// in-bundle gate produce PIXEL-IDENTICAL output — verified by rendering all 34 cells through
// `--render-panel` and diffing them against the in-bundle goldens (max drift 0.000000 over 34 cells). The
// PNG *files* still differ byte-wise, because the app tool encodes the renderer's `CGImage` directly while
// the gate encodes from its normalized comparison buffer; only the pixels are the claim. That equivalence
// is not free — it is what `Color.panelAssets` (asset lookup follows the compiled-into bundle, not
// `.main`) and the explicit `.tint` below buy. Do NOT re-bless the goldens from the app tool anyway: the
// gate owns its own regeneration path so the committed bytes are exactly what it reads back.

#if DEBUG
import AppKit
import SwiftUI

/// One named panel state to render, so a single pass emits the whole set the panel supports for a
/// screen-by-screen diff against the mock's `.pop` states.
struct PanelRenderFixture {
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

@MainActor
enum PanelRenderHarness {

    // The naming / scale surface is `nonisolated` on purpose: it is pure value formatting, and callers
    // enumerate filenames from contexts that are not main-actor-isolated (a nested test struct's computed
    // property, for one). Only the RENDER itself needs the main actor.

    /// The Retina raster scale both consumers use (see the file header).
    nonisolated static let scale: CGFloat = 2

    /// The two themes every fixture is rendered in.
    nonisolated static let themes: [ColorScheme] = [.light, .dark]

    /// The `light` / `dark` filename token for a scheme.
    nonisolated static func themeToken(_ scheme: ColorScheme) -> String {
        scheme == .light ? "light" : "dark"
    }

    /// `panel-<state>-<theme>.png` — the filename both the app tool writes and the golden gate reads.
    nonisolated static func fileName(fixture: String, scheme: ColorScheme) -> String {
        "panel-\(fixture)-\(themeToken(scheme)).png"
    }

    /// The margin every clock-relative fixture offset keeps from a `humanizeUntil` unit boundary, so a
    /// sub-second delay between seeding a fixture and rasterizing it cannot change the rendered text.
    /// 30 s is half a minute — the finest unit `humanizeUntil` prints — so a render that lands anywhere in
    /// a ±30 s window around the seed instant formats to the same string.
    nonisolated static let boundaryGuardSecs: Int64 = 30

    // MARK: - Fixture catalog

    /// Every panel state rendered by both consumers, seeded against `now` (seconds since the epoch).
    ///
    /// The roster mirrors the mock's "Healthy · Status" example rows — same percents + layout, so the
    /// render is directly comparable: Work active 42/88, Personal 31/71, Temp 4/18 — next swap → Temp. The
    /// third account is "Temp" where the mock illustrates "Scratch": re-picked so all three labels hash to
    /// DISTINCT #445 palette slots (the mock's "Personal" + "Scratch" both land on slot 5 / ochre under the
    /// shared 8-slot label hash), so the committed oracle shows three visibly-distinct identity colours —
    /// violet / ochre / teal (#709). The provider secondary line (#173) and the "Last swap …" footer (#88)
    /// are the documented Wave-1 reconciliations and correctly do NOT appear.
    static func fixtures(now: Int64) -> [PanelRenderFixture] {
        let day: Int64 = 86_400
        let guardSecs = boundaryGuardSecs

        let rows = [
            AccountRow(label: "Work", isActive: true, isEnabled: true, isQuarantined: false,
                       isRecovering: false, auth: .healthy, sessionPct: 42, weeklyPct: 88,
                       sessionResetsAt: now + 2 * 3600 + 14 * 60 + guardSecs,
                       weeklyResetsAt: now + 3 * day + guardSecs,
                       weeklyExhausted: false, isNextSwapTarget: false, blindActive: nil),
            AccountRow(label: "Personal", isActive: false, isEnabled: true, isQuarantined: false,
                       isRecovering: false, auth: .healthy, sessionPct: 31, weeklyPct: 71,
                       sessionResetsAt: now + 3600 + 2 * 60 + guardSecs,
                       weeklyResetsAt: now + 3 * day + guardSecs,
                       weeklyExhausted: false, isNextSwapTarget: false, blindActive: nil),
            AccountRow(label: "Temp", isActive: false, isEnabled: true, isQuarantined: false,
                       isRecovering: false, auth: .healthy, sessionPct: 4, weeklyPct: 18,
                       sessionResetsAt: now + 5 * 3600 + 20 * 60 + guardSecs,
                       weeklyResetsAt: now + 3 * day + guardSecs,
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
                       weeklyPct: 88, sessionResetsAt: now + 2 * 3600 + 14 * 60 + guardSecs,
                       weeklyResetsAt: now + 3 * day + guardSecs,
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
                                           sessionPct: 14, weeklyPct: 100,
                                           sessionResetsAt: now + 2 * 3600 + guardSecs,
                                           weeklyResetsAt: now + 2 * day + 4 * 3600 + guardSecs,
                                           weeklyExhausted: true,
                                           isNextSwapTarget: false, blindActive: nil)
        let exhaustedTemp = AccountRow(label: "Temp", isActive: false, isEnabled: true,
                                       isQuarantined: false, isRecovering: false, auth: .healthy,
                                       sessionPct: 6, weeklyPct: 97,
                                       sessionResetsAt: now + 4 * 3600 + 50 * 60 + guardSecs,
                                       weeklyResetsAt: now + 3 * day + 3600 + guardSecs,
                                       weeklyExhausted: true,
                                       isNextSwapTarget: false, blindActive: nil)
        let blindCorneredRows = [blindWork(BlindActive(blindSecs: 1080, lastKnownSessionPct: 92,
                                                       autoProtectionDegraded: true)),
                                 exhaustedPersonal, exhaustedTemp]

        // `generatedAt` sits a comfortable 12 s in the past: `snapshotAgeText` humanizes any sub-minute age
        // to the single string "updated <1m ago", so the whole 1…59 s band renders identically — the widest
        // rounding plateau available, hence no `guardSecs` needed on this one.
        let fresh = now - 12
        // The `stale` fixture's age must land inside a stable `humanizeUntil` plateau too: 1h30m + 30 s.
        let staleAge = now - (5400 + guardSecs)

        // The panel-rendered states (the fuller 9-state fidelity's remaining facets are #169 siblings).
        // `stale` and `disconnected` retain the last-good roster (disconnected dims it); the account-less
        // states — including `crashLooping` (#169), which refuses the held snapshot's numbers behind an
        // honest message card — show a banner / onboarding card. Ages chosen so the footer reads live /
        // stale as intended.
        let nextSwap = NextSwap.target(to: "Temp", reason: .soonestReset(resetsAt: now + 3 * day + guardSecs))
        return [
            PanelRenderFixture(name: "healthy", state: .connected, rows: rows,
                               nextSwap: nextSwap, generatedAt: fresh),
            // #704: the healthy roster's STATS tab — the ONE fixture seeded to `.stats`/`.loaded` (every other
            // renders the Status glance). Reuses the healthy roster so the Stats rows join the same
            // Work/Personal/Temp identities (active = Work) the mock's `healthy-stats-*` frames show; the
            // loaded series rides `statsWire`. State stays `.connected` because `StatusPanelView` offers the
            // Stats seg only over a live roster (`.connected`/`.stale`) — a Stats tab on a degraded daemon
            // could only fail. `next_swap` is inert here (the Stats tab renders no footer).
            PanelRenderFixture(name: "stats", state: .connected, rows: rows,
                               nextSwap: nextSwap, generatedAt: fresh,
                               statsWire: PanelStatsModel.loadedPreviewFixture),
            PanelRenderFixture(name: "stale", state: .stale, rows: rows,
                               nextSwap: nextSwap, generatedAt: staleAge),
            PanelRenderFixture(name: "disconnected", state: .disconnected(reason: "the daemon is not responding"),
                               rows: rows, nextSwap: nil, generatedAt: now - (240 + guardSecs)),
            PanelRenderFixture(name: "connecting", state: .connecting, rows: [], nextSwap: nil,
                               generatedAt: nil),
            // #499: the cold-refused daemon-absent states (no reading ever held) — a forming card for
            // starting, and the not-running card. #170 adds the Start-daemon affordance to the not-running
            // card: the harness seeds `canStartDaemon` true (see `render`) so this fixture shows the
            // mock's Start button; the shipped app gates it off until #171 bundles the agent plist.
            PanelRenderFixture(name: "starting", state: .starting, rows: [], nextSwap: nil, generatedAt: nil),
            PanelRenderFixture(name: "not-running", state: .notRunning, rows: [], nextSwap: nil,
                               generatedAt: nil),
            PanelRenderFixture(name: "crash-looping", state: .crashLooping, rows: [], nextSwap: nil,
                               generatedAt: nil),
            PanelRenderFixture(name: "unsupported", state: .unsupported, rows: [], nextSwap: nil,
                               generatedAt: nil),
            PanelRenderFixture(name: "empty-roster", state: .emptyRoster, rows: [], nextSwap: nil,
                               generatedAt: nil),
            // #571: the active-account blind row, OK + DEGRADED — a per-row modifier on a `.connected`
            // snapshot (fresh header/footer), rendered as the held session bar + auto-protection verdict.
            PanelRenderFixture(name: "blind-ok", state: .connected, rows: blindOKRows,
                               nextSwap: nextSwap, generatedAt: fresh),
            PanelRenderFixture(name: "blind-degraded", state: .connected, rows: blindDegradedRows,
                               nextSwap: nextSwap, generatedAt: fresh),
            // #572: the CORNERED blind row — blind + DEGRADED + no viable target. `next_swap` is
            // `.noViableTarget` (every spare weekly-exhausted, capacity back in 2d 4h), the signal the panel
            // composes with the row's `autoProtectionDegraded` into the red "cannot act" verdict + remedy.
            PanelRenderFixture(name: "blind-cornered", state: .connected, rows: blindCorneredRows,
                               nextSwap: .noViableTarget(cause: .weekly,
                                                         resetsAt: now + 2 * day + 4 * 3600 + guardSecs),
                               generatedAt: fresh),
        // …plus the four daemon-level FAULT ranks (#592) — appended rather than inlined because they vary a
        // different axis: same `.connected` state and same healthy roster, differing only in which payload
        // fault is set. See `faultFixtures`.
        ] + faultFixtures(rows: rows, nextSwap: nextSwap, generatedAt: fresh)
    }

    /// The four NON-CANARY daemon-level FAULT fixtures (#592) — the four ranks of
    /// `StatusPanelFormat.daemonFaultBanner`'s worst-first resolver whose banners this harness can render
    /// standalone, so the shipped banner family has a VISUAL oracle to set beside the mock's fault frames
    /// (`menubar-preview.html`). Before these, the harness rendered none of the family, so
    /// `design/build-comparison.py` had nothing to pair against and the severity ranking — a *visual* claim —
    /// was defended by format-layer unit tests alone. The resolver now spans EIGHT ranks over FOUR faults
    /// (#714/#728 added the canary refusal pair at ranks 3-4 and an overridden drift at rank 7; #730/#738 added
    /// the unparseable-canonical refusal at rank 5); the four canary ranks are NOT rendered here yet — their
    /// oracle needs matching canary frames in the mock, tracked as the fault-family visual-oracle follow-up
    /// (#571). So the four fixtures below are ranks 1, 2, 6, and 8.
    ///
    /// All four ride a `.connected` snapshot over the SAME healthy green roster, deliberately: a daemon-level
    /// fault is exactly the one NO per-row `auth` cell reflects, so "full green roster under a loud banner" is
    /// the state these banners exist to contradict — not an inconsistency in the fixture. Header and footer
    /// stay fresh for the same reason: the fault is the DAEMON's, not the snapshot's (never a whole-snapshot
    /// `stale`, #137).
    ///
    /// Rendering the calm rank 8 alongside the louder ranks is the point rather than redundancy: rank 6
    /// (systemic, `.warning`) has to be SEEN to beat rank 8 (`recovering`, `.info`), and an inversion between
    /// those two is precisely the regression `daemonFaultBanner` documents at length. One frame each is what
    /// makes the (fault, VARIANT) ordering reviewable instead of asserted.
    private static func faultFixtures(rows: [AccountRow], nextSwap: NextSwap,
                                      generatedAt: Int64) -> [PanelRenderFixture] {
        func fault(_ name: String, _ apply: (inout PanelRenderFixture) -> Void) -> PanelRenderFixture {
            var fixture = PanelRenderFixture(name: name, state: .connected, rows: rows,
                                             nextSwap: nextSwap, generatedAt: generatedAt)
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
            // Rank 6 — the refresh MECHANISM is down. `.warning`, not `.error`: every account still works, so
            // it is a pre-death "next break" task, ranked deliberately ABOVE the calm scrub below (#523). The
            // count is plural-agreeing, so 3 exercises the "sweeps" arm rather than the n=1 floor. (Ranks 3-5,
            // the canary refusal trio, sit ABOVE this but are not rendered here — see the doc above.)
            fault("fault-systemic-refresh") { $0.systemicRefreshFailure = 3 },
            // Rank 8 — scrubbed but self-healing. `.info`, and the LOWEST claim on the one banner slot
            // precisely because its whole message is "no action needed" — a self-healing state can never
            // outrank one that cannot self-heal. (Rank 7, an overridden canary drift, sits just above but is
            // not rendered here — see the doc above.)
            fault("fault-scrub-recovering") { $0.canonicalScrub = .recovering },
        ]
    }

    // MARK: - Rendering

    /// Rasterize one fixture in one theme, exactly the way both consumers must.
    ///
    /// The environment is injected via the shared `statusPanelEnvironment` modifier — the SAME wiring
    /// `StatusItemController` uses for the live app, so the harness and the app cannot drift and every
    /// `@EnvironmentObject` the panel reads is resolved instead of trapping (issue #504: a missing
    /// `PanelStatsModel` here was exactly that drift). All three transport-backed models take a NIL client,
    /// so nothing here touches a socket:
    ///   • `AccountCaptureModel` renders at `.idle` with `captureSurfaceRequested == false`, so the
    ///     populated fixtures show the roster with NO capture bar (capture is off-panel / empty-roster only
    ///     now, #394) and the empty-roster fixture shows the onboarding card. The nil client renders the
    ///     idle field/button and never touches a socket — the label field itself stays a known
    ///     `ImageRenderer` blank (see design/README.md).
    ///   • `AccountSwapModel` renders at `.idle`, so the fixtures capture the RESTING row (no hover, no
    ///     pending). As of #448 the per-row switch chip is PERSISTENT, so its resting glyph
    ///     (`arrow.left.arrow.right`, or the `nosign` on a non-viable row) IS captured in a static render;
    ///     only the ARMED hover/focus brighten and the in-flight `Switching…` spinner stay a manual-check
    ///     surface (#380).
    ///   • `PanelStatsModel` (#446) renders at its default `.status` tab / `.idle` phase for every fixture
    ///     EXCEPT `stats` (#704), which `loadedPreview` seeds straight to `.stats`/`.loaded` from
    ///     `loadedPreviewFixture` so the render shows the account cards. BOTH stay socket-free: the default
    ///     nil client never fires a `stats` query, and the seeded fixture sets its phase directly rather
    ///     than loading.
    ///
    /// `LoginItemModel` is seeded so `canStartDaemon` is TRUE (#170) — the not-running fixture then renders
    /// the mock's Start-daemon button (the DESIGN-TARGET state). The shipped #170 app gates that button OFF
    /// until #171 bundles the agent plist; this render is a design oracle against the mock (which shows the
    /// button), NOT a capture of the #170 runtime's inert-banner state. Every other fixture carries the
    /// model inert (its state renders no Start card).
    static func render(_ fixture: PanelRenderFixture, scheme: ColorScheme) -> CGImage? {
        warmUpIfNeeded()
        return rasterize(fixture, scheme: scheme)
    }

    /// Discard-renders until the rasterizer reaches its steady state, once per process.
    ///
    /// MEASURED, and the reason committed goldens are byte-reproducible at all. The first renders in a
    /// process do not match the ones that follow: rendering `healthy/light` six times in a row yields
    /// renders 0–1 identical to each other, renders 2–5 identical to each other, and the two groups
    /// differing by ±1/255 on 905 of 2 729 920 bytes — a warm-up artifact (text rasterization caches
    /// populating), not a clock or fixture effect. Renders seeded seconds apart are byte-identical, which
    /// rules the clock out directly.
    ///
    /// Left unhandled it is quietly corrosive rather than dramatic: whichever cells happen to be rasterized
    /// first carry cold pixels, so a re-bless rewrites files that did not change, and the churn buries a
    /// real change in the `git diff` that `Panel-Goldens-Rebaselined:` exists to make readable. The gate
    /// metric's 64/255 threshold hides ±1 either way, so nothing here is load-bearing for a verdict — it is
    /// load-bearing for the AUDIT TRAIL.
    ///
    /// Self-calibrating rather than a tuned constant: renders a throwaway fixture until two consecutive
    /// rasters agree byte-for-byte, so a machine that warms up in a different number of passes is handled
    /// without a magic number. Bounded, and never fatal — if it cannot stabilize it returns anyway, and
    /// `PanelGoldenParityTests.testAnIdenticalRerenderScoresExactlyZero` is the assertion that turns that
    /// into a loud failure instead of a silent one.
    private static var isWarm = false
    private static func warmUpIfNeeded() {
        guard !isWarm else { return }
        isWarm = true
        // Seeded from the real clock, NOT from 0: at epoch 0 every countdown in the fixture is ~56 years in
        // the past, which is not a state the panel is ever asked to format and not what we want to warm.
        let probe = fixtures(now: Int64(Date().timeIntervalSince1970)).first { $0.name == "healthy" }
        guard let probe else { return }
        var previous: [UInt8]?
        for _ in 0..<8 {
            guard let cg = rasterize(probe, scheme: .light), let bytes = rawBytes(cg) else { return }
            if let previous, previous == bytes { return }
            previous = bytes
        }
    }

    /// Tightly-packed RGBA8 bytes for the warm-up comparison. Deliberately its own small routine rather than
    /// a dependency on the test target's `PanelRaster` — the harness ships in the APP too, and the app tool
    /// needs the same warm-up so its renders and the in-bundle goldens agree byte-for-byte.
    private static func rawBytes(_ image: CGImage) -> [UInt8]? {
        let width = image.width, height = image.height
        guard width > 0, height > 0, let space = CGColorSpace(name: CGColorSpace.sRGB) else { return nil }
        var bytes = [UInt8](repeating: 0, count: width * height * 4)
        let ok: Bool = bytes.withUnsafeMutableBytes { raw -> Bool in
            guard let base = raw.baseAddress,
                  let ctx = CGContext(data: base, width: width, height: height, bitsPerComponent: 8,
                                      bytesPerRow: width * 4, space: space,
                                      bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue) else { return false }
            ctx.setBlendMode(.copy)
            ctx.draw(image, in: CGRect(x: 0, y: 0, width: width, height: height))
            return true
        }
        return ok ? bytes : nil
    }

    private static func rasterize(_ fixture: PanelRenderFixture, scheme: ColorScheme) -> CGImage? {
        let store = WatchStatusStore.preview(state: fixture.state, rows: fixture.rows,
                                             nextSwap: fixture.nextSwap, generatedAt: fixture.generatedAt,
                                             canonicalScrub: fixture.canonicalScrub,
                                             keychainLocked: fixture.keychainLocked,
                                             systemicRefreshFailure: fixture.systemicRefreshFailure)
        let stats = fixture.statsWire.map { PanelStatsModel.loadedPreview($0) }
            ?? PanelStatsModel(client: nil)
        let loginItem = LoginItemModel(service: PanelRenderLoginItemService())
        let view = StatusPanelView()
            .statusPanelEnvironment(store: store,
                                    capture: AccountCaptureModel(client: nil),
                                    swap: AccountSwapModel(client: nil),
                                    stats: stats,
                                    loginItem: loginItem)
            .environment(\.colorScheme, scheme)
            // PIN the accent (#754). `Color.accentColor` — and every SwiftUI control that tints itself from
            // it — resolves through `ASSETCATALOG_COMPILER_GLOBAL_ACCENT_COLOR_NAME`, which is set on the
            // APP target only (#391 pins it to the brand-blue `AccentColor` asset). Rendered from the
            // MenubarTests bundle instead, that setting is absent and the accent would fall back to the
            // OPERATOR'S system accent — a machine-dependent hue that would make every golden unusable on a
            // second machine. `.tint` pins it explicitly for the whole hierarchy, from the same asset, so
            // the app tool and the in-bundle gate rasterize identical pixels.
            .tint(Color.panelAccent)
        let renderer = ImageRenderer(content: view)
        renderer.scale = scale
        return renderer.cgImage
    }
}

/// A hermetic `LoginItemService` for the render harness (#170) — NO `SMAppService`, no OS calls. Seeded so
/// `canStartDaemon` is TRUE (a registrable daemon agent — `.notRegistered`, NOT `.notFound` — and no
/// CLI-owned agent), so the `not-running` fixture renders the mock's Start-daemon affordance. Only that
/// fixture reads it; the model rides inert in every other fixture's environment. Register/unregister are
/// no-ops — a design render never mutates real login-item state. Distinct from `MenubarTests`'
/// `FakeLoginItemService` (which drives `LoginItemModelTests` through mutable, assertable state): this one
/// is a fixed design-oracle seed, and it must be identical for both harness consumers, which is why it
/// lives here rather than in either.
private final class PanelRenderLoginItemService: LoginItemService {
    let appStatus: LoginItemStatus = .enabled
    let daemonAgentStatus: LoginItemStatus = .notRegistered
    let cliManagedAgentPresent: Bool = false
    // No daemon running in a design render → the lock is free, so the liveness gate (issue #742)
    // stays open and the not-running fixture keeps rendering the Start-daemon affordance.
    let daemonLockHeld: Bool = false
    func registerApp() throws {}
    func unregisterApp() throws {}
    func registerDaemonAgent() throws {}
    func unregisterDaemonAgent() throws {}
    func openLoginItemsSettings() {}
}
#endif
