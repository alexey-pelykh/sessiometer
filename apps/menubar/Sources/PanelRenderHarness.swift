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
// in-bundle gate produce PIXEL-IDENTICAL output — verified by rendering all 44 cells through
// `--render-panel` and diffing them against the in-bundle goldens (max drift 0.000000 over 44 cells). The
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
    // The systemic episode's opening bracket (#813) — it selects the banner's EVIDENCE clause, so each arm
    // is a distinct render (three of them: sweep/absent, preflight, and the newer-daemon `.unrecognized`
    // fallback that cites no evidence). Plumbed through to `WatchStatusStore.preview` so the preflight arm is
    // renderable, but no fixture below sets it yet: a `fault-systemic-refresh-preflight` capture needs a
    // matching `.pop` state in the mock to pair against, the same design-SSOT prerequisite that keeps the
    // canary faults unmodeled here (#571). The rendered fixture is therefore the `.sweep`/absent arm, which
    // is what shipped before #813.
    var systemicRefreshSource: SystemicRefreshSource?
    // The loaded Stats-tab series (#704). Non-nil ONLY on the `stats` fixture: it seeds a `PanelStatsModel`
    // to `.stats`/`.loaded` so the render shows the account cards, not the Status glance. `var` with a
    // default for the same memberwise-init reason as the payload faults above.
    var statsWire: StatsWire?
    // The `View log` affordance's honest-affordance gate (#776), pinned per fixture rather than probed.
    // Non-nil ONLY on the two states the mock gives the action — it seeds `DaemonLogProbe.fixed`, so the
    // render shows the button the reference specifies. The VALUE is a fixed literal and is never read from
    // disk: a real filesystem probe would render differently on a machine that has run the daemon than on a
    // fresh CI runner, which is exactly the machine-dependence the goldens cannot tolerate (the same trap the
    // `.tint` pin below guards for the accent). `var` with a default, memberwise-init reason as above.
    var daemonLogPath: String?
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

    /// The `View log` fixture's stand-in log path (#776) — a FIXED literal, never a resolved one.
    ///
    /// The affordance's honest-affordance gate is "does the daemon's log exist", and answering it from the
    /// real filesystem here would make every render depend on whether the rendering machine has ever run the
    /// daemon. That is precisely the machine-dependence the committed goldens cannot survive (the sibling of
    /// the `.tint` accent pin in `render`). Only the button's PRESENCE is rendered — nothing draws the path —
    /// so a literal is a complete stand-in, and its shape still mirrors `DaemonLogLocation.logTail` so a
    /// reader can see what the real value looks like.
    nonisolated static let fixtureLogPath = "/Users/sessiometer/Library/Logs/sessiometer/sessiometer.log"

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
        // remedy. The siblings are BOTH weekly-exhausted (WHY there is no target), so each renders an EMPTY
        // switch slot plus its #955 reason line — since #959 a blocked row carries no chip at all (it used
        // to draw a `nosign` indistinguishable from the swap arrow it negated); the 28 pt slot stays
        // reserved, so nothing reflows. This fixture renders the ONLY two goldens containing blocked rows
        // (one fixture across light and dark), which makes them the pair that moves on any blocked-row
        // presentation change. The cornered-ness is
        // composed at render from `blind_active` (degraded) + the fixture's `next_swap == .noViableTarget`
        // — no new wire field.
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

        // The four-state expiry roster (#886) — the healthy rows above, each given one horizon verdict,
        // plus a fourth account carrying the UNMEASURED one. Deadlines are `now`-relative and land on a
        // `humanizeUntil` plateau via `guardSecs`, exactly as the reset instants above do, so the rendered
        // durations are stable bytes rather than a value that flips mid-suite.
        func expiring(_ row: AccountRow, _ expiry: AccountExpiry) -> AccountRow {
            AccountRow(label: row.label, isActive: row.isActive, isEnabled: row.isEnabled,
                       isQuarantined: row.isQuarantined, isRecovering: row.isRecovering, auth: row.auth,
                       sessionPct: row.sessionPct, weeklyPct: row.weeklyPct,
                       sessionResetsAt: row.sessionResetsAt, weeklyResetsAt: row.weeklyResetsAt,
                       weeklyExhausted: row.weeklyExhausted, isNextSwapTarget: row.isNextSwapTarget,
                       blindActive: row.blindActive, expiry: expiry)
        }
        let expiryRows = [
            // WITHIN the operator's horizon — still working, but act before it lapses.
            expiring(rows[0], AccountExpiry(expiresAt: now + 5 * day + 18 * 3600 + guardSecs,
                                            horizonState: .within)),
            // Already LAPSED — only a `sessiometer login` recovers it.
            expiring(rows[1], AccountExpiry(expiresAt: now - day, horizonState: .lapsed)),
            // BEYOND it: the one verdict that legitimately means "not expiring soon".
            expiring(rows[2], AccountExpiry(expiresAt: now + 29 * day + guardSecs, horizonState: .beyond)),
            // POLLED, and the credential carried NO deadline — UNKNOWN, rendered as the gap. The row
            // this fixture exists for, and it sits directly under the calm `29d` above on purpose.
            AccountRow(label: "Unmeasured", isActive: false, isEnabled: true, isQuarantined: false,
                       isRecovering: false, auth: .healthy, sessionPct: 12, weeklyPct: 24,
                       sessionResetsAt: now + 4 * 3600 + 30 * 60 + guardSecs,
                       weeklyResetsAt: now + 3 * day + guardSecs,
                       weeklyExhausted: false, isNextSwapTarget: false, blindActive: nil,
                       expiry: AccountExpiry(expiresAt: nil, horizonState: .unknown)),
        ]

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
            //
            // #776 does the same for `View log`: `starting` and `crash-looping` — and ONLY those two, which
            // is the whole set the mock gives the action — seed a `daemonLogPath` so the render shows it.
            // Seeding it is what keeps this harness usable as the design oracle: a fixture rendered without
            // the affordance would read as a permanent mismatch against the mock in `build-comparison.py`,
            // for a button that is present in the shipped app whenever there is a log to open.
            PanelRenderFixture(name: "starting", state: .starting, rows: [], nextSwap: nil, generatedAt: nil,
                               daemonLogPath: fixtureLogPath),
            PanelRenderFixture(name: "not-running", state: .notRunning, rows: [], nextSwap: nil,
                               generatedAt: nil),
            PanelRenderFixture(name: "crash-looping", state: .crashLooping, rows: [], nextSwap: nil,
                               generatedAt: nil, daemonLogPath: fixtureLogPath),
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
            // #886: the per-row REFRESH-token expiry line (#884) in ALL FOUR of its states at once.
            // Every other fixture leaves `expiry` nil, so `rosterShowsExpiry` is false and the line is
            // absent from the OTHER 42 committed cells — those pin the ELISION (a fleet whose credentials
            // carry no deadline shows no line rather than a column of `—`); this one is the only frame
            // in which the line ships.
            //
            // FOUR rows rather than the shared three, because the fourth state is the one that has to
            // be SEEN: `Unmeasured` was polled and its credential held no deadline, so it renders the
            // gap `—` DIRECTLY BELOW `Temp`'s calm `29d`. That those two do not look alike is a purely
            // visual claim — the exact class a unit test cannot make and a golden can — and mistaking
            // one for the other is the silent false-calm the whole foresight feature exists to refuse
            // (#137, and #876 for how that assumption already rotted once). A format-layer test can
            // assert the two STRINGS differ; only a render shows an operator they do not read alike.
            //
            // The roster is otherwise the healthy one, and the snapshot stays `.connected` with a fresh
            // header and footer: expiry is a per-row MODIFIER on a working daemon, never a
            // whole-snapshot degrade (#137) — an account is routinely healthy AND inside its horizon at
            // once, which is why #878 made it an orthogonal axis instead of a new `auth` state. There is
            // deliberately NO banner and no glyph escalation: #884 settled the both-or-neither invariant
            // as NEITHER, so a frame showing the line under a calm header is the CORRECT render, not an
            // omission (see `StatusPanelFormat`'s expiry section for the rationale).
            //
            // This fixture is PAIRED as of #957: the mock now authors the expiry line
            // (`expiry-{light,dark}`) and `design/build-comparison.py` carries the matching `STATES`
            // rows, so the render finally has a design oracle to be compared against.
            //
            // It previously had none — the mock authored no expiry surface at all, so the capture was
            // rendered and never fetched. That gap is not a footnote: it is why #951 (the expiry value
            // landing in the bar column instead of the right-hand gutter) could ship. A visual claim
            // with no visual oracle is only ever checked by whoever happens to look.
            PanelRenderFixture(name: "expiry", state: .connected, rows: expiryRows,
                               nextSwap: nextSwap, generatedAt: fresh),
        // …plus the four daemon-level FAULT ranks (#592) — appended rather than inlined because they vary a
        // different axis: same `.connected` state and same healthy roster, differing only in which payload
        // fault is set. See `faultFixtures`.
        ] + faultFixtures(rows: rows, nextSwap: nextSwap, generatedAt: fresh)
          // …plus the four PATHOLOGICAL-CONTENT rosters (#753). A third axis again: the state is
          // `.connected` and no payload fault is set, only the CONTENT is hostile. See `stressFixtures`.
          + stressFixtures(now: now, generatedAt: fresh)
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

    // MARK: - Pathological content (#753)

    /// The four PATHOLOGICAL-CONTENT rosters (#753) — hostile LABELS, PERCENTS and DURATIONS on an
    /// otherwise-ordinary `.connected` snapshot, so the panel's whole-frame behaviour under them is
    /// captured rather than assumed.
    ///
    /// WHY A RENDER AND NOT A MEASUREMENT. `Tests/PanelTextMetricsTests` (#750) already answers "does this
    /// label fit its cell", per cell, through CoreText. It cannot answer what happens to the FRAME —
    /// whether a row grows, whether a meter and its label start competing for the same width, whether the
    /// callout or the footer collapses. Those are whole-panel claims, and only a render makes them.
    ///
    /// PAIRED BY NAME, and that is the point. Each fixture's name is a `data-frame` base in the mock
    /// (`design/menubar-preview.html` group 7), so `design/build-comparison.py` sets each capture beside the
    /// frame that AUTHORS what it should look like. Without that the stress renders could only
    /// self-baseline — a golden blessing whatever the renderer emits, which then DEFENDS a broken renderer
    /// (the trap `Tests/BarGlyphParityTests.swift:38` documents, and the one issue #437 was misread through
    /// five times). Renaming a fixture here silently unpairs it, so keep these four names in step with the
    /// mock's.
    ///
    /// FOUR fixtures for the SIX pathological concepts issue #753 lists — #752 folded them when it authored
    /// the frames, because a frame is a whole roster and several concepts sit in one without interfering.
    /// Expect four names, not six; the fold, its rationale and the concept-per-frame table live in
    /// `design/README.md` § Pathological content (#752).
    ///
    /// EVERY VALUE HERE IS TRANSCRIBED FROM THE MOCK, never chosen. The mock's numbers are themselves
    /// measured — through the same CoreText primitives the shipped #750 gate drives, against the shipped
    /// budgets — so re-picking one here would silently replace a measured oracle with an eyeballed one.
    /// Three of those measurements contradict issue #753's own prose and the frames follow the MEASUREMENT:
    ///   • CJK / RTL labels do NOT elide at the 171 pt roster budget (119.32 / 116.30 / 123.72 pt) — they
    ///     render whole. Only the 40-char row (273.65 pt) elides.
    ///   • `365d23h` does NOT overflow the 52 pt reset cell (48.32 pt), nor does any three-digit day count.
    ///     Overflow begins at FOUR digits (`1000d23h` = 55.32 pt), which is issue #927 and is deliberately
    ///     NOT rendered here — `999d23h` below is the widest form that still fits.
    ///   • `255%` is rendered HONESTLY, with only the meter GEOMETRY clamped
    ///     (`StatusPanelFormat.meterFillWidth`) — the shipped split on both surfaces, ratified in hq
    ///     `strategy/design-menubar.md` § D-UX-PATHOLOGICAL. Nothing here clamps the number.
    ///
    /// Clock-relative instants take `boundaryGuardSecs` exactly as the ordinary fixtures do, so every
    /// duration below renders the mock's literal string rather than a value that flips mid-suite.
    private static func stressFixtures(now: Int64, generatedAt: Int64) -> [PanelRenderFixture] {
        let minute: Int64 = 60
        let hour: Int64 = 60 * minute
        let day: Int64 = 24 * hour
        let guardSecs = boundaryGuardSecs

        /// One roster row, seeded from the mock's two meter cells — hence one call-site line per cell
        /// below. `sessionIn` / `weeklyIn` are the DURATIONS the mock prints, converted to instants here so
        /// the render re-derives the same string.
        func row(_ label: String, active: Bool = false, isTarget: Bool = false,
                 session: UInt8, sessionIn: Int64, weekly: UInt8, weeklyIn: Int64) -> AccountRow {
            AccountRow(label: label, isActive: active, isEnabled: true, isQuarantined: false,
                       isRecovering: false, auth: .healthy, sessionPct: session, weeklyPct: weekly,
                       sessionResetsAt: now + sessionIn + guardSecs,
                       weeklyResetsAt: now + weeklyIn + guardSecs,
                       weeklyExhausted: false, isNextSwapTarget: isTarget, blindActive: nil)
        }

        func stress(_ name: String, rows: [AccountRow], swapTo: String,
                    swapResetIn: Int64) -> PanelRenderFixture {
            let reason = NextSwapReason.soonestReset(resetsAt: now + swapResetIn + guardSecs)
            return PanelRenderFixture(name: name, state: .connected, rows: rows,
                                      nextSwap: .target(to: swapTo, reason: reason),
                                      generatedAt: generatedAt)
        }

        return [
            // LONG · CJK · RTL, one roster. The active row's label is 273.65 pt against the 171 pt budget,
            // so the panel MIDDLE-truncates it — the elision the mock authors as a literal because CSS
            // `text-overflow` is tail-only. The other three are the scripts that measurement cleared: they
            // fit whole, and the claim under test is that the row's LTR layout (badge leads, health trails)
            // survives a bidi-shaped text run, not that anything elides.
            stress("pathological-label",
                   rows: [
                       row("continuous-integration-runner@example.io", active: true,
                           session: 92, sessionIn: 2 * hour + 14 * minute,
                           weekly: 61, weeklyIn: 2 * day + 4 * hour),
                       row("用户@例子公司.中国", isTarget: true,
                           session: 31, sessionIn: hour + 2 * minute,
                           weekly: 44, weeklyIn: 4 * day),
                       row("مستخدم@شركة.مصر",
                           session: 12, sessionIn: 5 * hour + 20 * minute,
                           weekly: 27, weeklyIn: 6 * day + 3 * hour),
                       row("משתמש@חברה.co.il",
                           session: 7, sessionIn: 3 * hour + 40 * minute,
                           weekly: 19, weeklyIn: 5 * day + 2 * hour),
                   ],
                   swapTo: "用户@例子公司.中国", swapResetIn: 4 * day),
            // The issue #445 invariant, visually: which substring survives elision. The short pair holds by
            // HEADROOM (81.16 / 81.80 pt against 171) and renders whole — its disambiguation rests on the
            // monogram pair the shipped collision-escalation resolves (WC / WB, first⋅last then
            // first⋅second once the pair is taken). The long pair is the case MIDDLE-truncation exists for:
            // at 216.37 / 215.94 pt the distinguishing `-one` / `-two` survives in the kept TAIL, where
            // tail-truncation would collapse both rows to the same `oleksii.pelykh@company`.
            stress("same-local-part",
                   rows: [
                       row("work@a.com", active: true,
                           session: 42, sessionIn: 2 * hour + 14 * minute,
                           weekly: 66, weeklyIn: 2 * day + 4 * hour),
                       row("work@b.com", isTarget: true,
                           session: 18, sessionIn: 3 * hour + 5 * minute,
                           weekly: 51, weeklyIn: 3 * day),
                       row("oleksii.pelykh@company-one.com",
                           session: 24, sessionIn: hour + 48 * minute,
                           weekly: 39, weeklyIn: 4 * day + 6 * hour),
                       row("oleksii.pelykh@company-two.com",
                           session: 9, sessionIn: 5 * hour,
                           weekly: 22, weeklyIn: 5 * day + 1 * hour),
                   ],
                   swapTo: "work@b.com", swapResetIn: 3 * day),
            // DEGENERATE labels — an empty string and a whitespace-only one, between two ordinary rows so
            // the departure is legible. The name line must stay genuinely BLANK: no placeholder, no quoted
            // empty string, no synthesised identity. Identity then rests entirely on #445's other two cues,
            // and the `?` / `?2` monogram sentinel is the NON-colour one that keeps WCAG 1.4.1 satisfied —
            // load-bearing here because the colour hash TRIMS its input, so these two labels hash to the
            // SAME badge colour and the monogram is the only thing separating them.
            //
            // The swap target is deliberately the ORDINARY `Personal`: what the callout should render for a
            // DEGENERATE target is an open question in the design record (#930), not one to guess at here.
            stress("degenerate-label",
                   rows: [
                       row("Work", active: true,
                           session: 42, sessionIn: 2 * hour + 14 * minute,
                           weekly: 88, weeklyIn: 2 * day + 4 * hour),
                       row("",
                           session: 31, sessionIn: hour + 2 * minute,
                           weekly: 71, weeklyIn: 4 * day),
                       row("     ",
                           session: 4, sessionIn: 5 * hour + 20 * minute,
                           weekly: 18, weeklyIn: 6 * day + 3 * hour),
                       row("Personal", isTarget: true,
                           session: 16, sessionIn: 4 * hour + 30 * minute,
                           weekly: 34, weeklyIn: 5 * day),
                   ],
                   swapTo: "Personal", swapResetIn: 5 * day),
            // WIRE-HOSTILE NUMERICS — a percent the wire should never carry and durations at the reset
            // cell's measured edge, over THREE rows (the mock's roster; the two hostile rows read as
            // departures from the ordinary third). `255%` takes the red band and prints verbatim while the
            // bar stops at its own track; `365d23h` / `999d23h` are the three-digit day counts that still
            // FIT (48.32 pt of 52), and `23h59m` is the widest sub-day form, the ordinary counterpart.
            stress("wire-hostile-numerics",
                   rows: [
                       row("Work", active: true,
                           session: 255, sessionIn: 2 * hour + 14 * minute,
                           weekly: 100, weeklyIn: 2 * day + 4 * hour),
                       row("Personal",
                           session: 42, sessionIn: 365 * day + 23 * hour,
                           weekly: 88, weeklyIn: 999 * day + 23 * hour),
                       row("Scratch", isTarget: true,
                           session: 4, sessionIn: 23 * hour + 59 * minute,
                           weekly: 18, weeklyIn: 3 * day),
                   ],
                   swapTo: "Scratch", swapResetIn: 3 * day),
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
    ///     (`arrow.left.arrow.right`) IS captured in a static render. Since #959 a wire-BLOCKED row renders
    ///     NO chip — so what these fixtures capture on such a row is the empty slot itself, and its absence
    ///     is as much a pinned property here as the glyph's presence is on a viable row.
    ///     This previously said "only the ARMED brighten and the in-flight `Switching…` spinner stay a
    ///     manual-check surface (#380)" — corrected per issue #766: those states are un-captured HERE
    ///     because these fixtures supply no input, not because the renderer cannot reach them. Given a
    ///     seam that does (`AccountRowView.armed`, `AccountSwapModel.pendingPreview`) the same
    ///     `ImageRenderer` renders both, and `PanelInteractionStateTests` measures them every run. What
    ///     stays manual is narrower: the pressed wash, the `pointingHand` cursor, the hover tooltip, and
    ///     the real-popover round-trip.
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
    /// `dynamicTypeSize` defaults to `.large` — the DEFAULT class, whose `PanelTypeScale` factor is exactly
    /// 1.0 — so every existing consumer (the app's `--render-panel` tool, the committed golden gate) renders
    /// byte-for-byte what it rendered before issue #756. It is a parameter at all because AC-3 asks what the
    /// panel looks like "given each Dynamic Type size class", and that question needs a seam; issue #757's
    /// gate is the intended consumer of it.
    static func render(_ fixture: PanelRenderFixture, scheme: ColorScheme,
                       dynamicTypeSize: DynamicTypeSize = .large) -> CGImage? {
        warmUpIfNeeded()
        return rasterize(fixture, scheme: scheme, dynamicTypeSize: dynamicTypeSize)
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
            guard let cg = rasterize(probe, scheme: .light, dynamicTypeSize: .large),
                  let bytes = rawBytes(cg) else { return }
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

    private static func rasterize(_ fixture: PanelRenderFixture, scheme: ColorScheme,
                                  dynamicTypeSize: DynamicTypeSize) -> CGImage? {
        let store = WatchStatusStore.preview(state: fixture.state, rows: fixture.rows,
                                             nextSwap: fixture.nextSwap, generatedAt: fixture.generatedAt,
                                             canonicalScrub: fixture.canonicalScrub,
                                             keychainLocked: fixture.keychainLocked,
                                             systemicRefreshFailure: fixture.systemicRefreshFailure,
                                             systemicRefreshSource: fixture.systemicRefreshSource)
        let stats = fixture.statsWire.map { PanelStatsModel.loadedPreview($0) }
            ?? PanelStatsModel(client: nil)
        let loginItem = LoginItemModel(service: PanelRenderLoginItemService())
        let view = StatusPanelView()
            .statusPanelEnvironment(store: store,
                                    capture: AccountCaptureModel(client: nil),
                                    swap: AccountSwapModel(client: nil),
                                    stats: stats,
                                    loginItem: loginItem,
                                    daemonLog: .fixed(fixture.daemonLogPath))
            .environment(\.colorScheme, scheme)
            // PIN the accent (#754). `Color.accentColor` — and every SwiftUI control that tints itself from
            // it — resolves through `ASSETCATALOG_COMPILER_GLOBAL_ACCENT_COLOR_NAME`, which is set on the
            // APP target only (#391 pins it to the brand-blue `AccentColor` asset). Rendered from the
            // MenubarTests bundle instead, that setting is absent and the accent would fall back to the
            // OPERATOR'S system accent — a machine-dependent hue that would make every golden unusable on a
            // second machine. `.tint` pins it explicitly for the whole hierarchy, from the same asset, so
            // the app tool and the in-bundle gate rasterize identical pixels.
            .tint(Color.panelAccent)
            // The Dynamic Type size class (issue #756). `StatusPanelView` clamps it to
            // `PanelTypeScale.ceiling` and derives the panel's uniform scale factor from it.
            .dynamicTypeSize(dynamicTypeSize)
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
    // Nothing of ours is running either (issue #819) — consistent with the free lock above, and with the
    // `.notRegistered` agent status: there is no job to be running. A design render never reaches the
    // reconcile, so this seed only has to be COHERENT, never exercised.
    let daemonAgentRunState: DaemonAgentRunState = .notRunning
    func registerApp() throws {}
    func unregisterApp() throws {}
    func registerDaemonAgent() throws {}
    func unregisterDaemonAgent() throws {}
    func openLoginItemsSettings() {}
}
#endif
