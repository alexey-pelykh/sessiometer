// Copyright (c) 2026 Oleksii PELYKH
// SPDX-License-Identifier: MIT

// The panel's Stats-tab model (issue #446): the thin `@MainActor` shell that owns the panel's Status|Stats
// tab selection AND the ONE-SHOT `stats` query over the #358 control-command transport, exposing its
// idle → loading → loaded → failed phase to the SwiftUI panel. It is the READ-ONLY sibling of
// `AccountSwapModel` (#169) / `AccountCaptureModel` (#360) — the same short-lived-client shape over the same
// transport, but it only QUERIES and renders (no swap, no capture, no write of any kind). The Stats tab is a
// pure display surface: the crown-jewel (`.connected` sole healthy state) and the footer-`next_swap`
// invariants belong to the Status tab and are untouched here.
//
// The `stats` verb (issue #356) is a request→response: connect, send one `{"cmd":"stats","period":"week"}`
// line, read exactly ONE reply object (the `StatsWire` document — the SAME `sessiometer stats --json`
// emits, R-2 parity), close. Unlike the long-lived `watch` STREAM the read-side `WatchStatusStore` drives,
// this is a discrete one-shot, so it mirrors the write-side clients' send shape (via `ControlCommandClient`),
// only it DECODES a full document rather than a redacted ack.
//
// AppKit-free BY DESIGN (Foundation + Combine + os only) so it compiles into the headless `MenubarTests`
// bundle and its phase transitions are driven hermetically against a fake connector — no socket, no live
// daemon (the same testability split `AccountSwapModel` / `AccountCaptureModel` / the store use).
//
// NO credential handling of any kind (C-001 / issue #15): the `stats` channel carries redacted roster LABELS
// and neutral usage MAGNITUDES only — no token, email, or oauth blob — and it never reads the daemon's
// offline usage store (the series comes OVER THE SOCKET, the whole point of #356; the zero-egress guard
// enforces it).

import Combine
import Foundation
import os

private let statsLog = Logger(subsystem: "org.sessiometer.menubar", category: "stats")

/// The `stats` control-command request (issue #356 wire): `{"cmd":"stats","period":"week"}`. The panel reads
/// the DEFAULT 7-day daily-bucket window (the mock's "last 7 days"), which is `period` = `week`: the CLI has
/// no `7d` period (that is `--since` grammar), and `week` IS the 7-day daily-bucket window (`src/stats.rs`).
/// `period` is sent explicitly (not omitted) so the wire line is self-describing, exactly as `SwapCommand`
/// sends its `force` explicitly.
struct StatsCommand: Encodable, Sendable {
    let cmd = "stats"
    let period = "week"
}

/// Why a stats query did not yield a series — the READ-side sibling of `SwapFailure`. Non-secret: the whole
/// stats channel is redacted (labels + magnitudes only, issue #15), so each case carries a plain reason.
enum StatsFailure: Equatable {
    /// A bounded transport failure (#358 `ControlCommandError`): no daemon (refused), a wedged daemon (timed
    /// out / closed before the reply), or an I/O / encode fault.
    case transport(ControlCommandError)
    /// The daemon returned a redacted `{"error":…}` envelope (e.g. an invalid period — never on the panel's
    /// always-`week` path, but surfaced honestly rather than swallowed).
    case daemonError(String)
    /// The reply did not match the `StatsWire` contract (a buggy / drifted daemon) — degrade loudly.
    case undecodable
    /// No control client — the daemon control-socket path would not resolve (sandboxed / home unresolved), so
    /// stats are unavailable from this app instance (honest, never a blank tab).
    case unavailable
}

@MainActor
final class PanelStatsModel: ObservableObject {
    /// The panel's two tabs (issue #446, the mock's `.seg` Status | Stats control). `status` is the default —
    /// the panel opens on the honest-state roster glance, never on a not-yet-loaded Stats tab.
    enum Tab: Equatable {
        case status
        case stats
    }

    /// The Stats query's phase the panel observes. `loading` is shown only on a FIRST load / a retry; a
    /// refetch over already-`loaded` data keeps the prior series visible (no blank flash on a re-select).
    enum Phase: Equatable {
        case idle
        case loading
        case loaded(StatsWire)
        case failed(StatsFailure)

        /// The loaded series, or `nil` in any other phase — the "keep prior data while refetching" gate.
        var wire: StatsWire? {
            if case .loaded(let wire) = self { return wire }
            return nil
        }
    }

    /// The current tab the panel renders and the header seg highlights.
    @Published private(set) var tab: Tab = .status

    /// The current Stats query phase the panel observes.
    @Published private(set) var phase: Phase = .idle

    /// The short-lived control-command client, or `nil` when the socket path would not resolve — in which case
    /// a stats query short-circuits to `.failed(.unavailable)` (honest, never a dead tab).
    private let client: ControlCommandClient?

    init(client: ControlCommandClient?) {
        self.client = client
    }

    /// Select a tab. Switching TO `stats` triggers a one-shot `stats` query (the tab is data-backed);
    /// switching back to `status` leaves the last series in place (a later re-select shows it instantly, then
    /// refreshes). A no-op when already on `tab`, so the seg is idempotent.
    func select(_ tab: Tab) {
        guard tab != self.tab else { return }
        self.tab = tab
        if tab == .stats {
            Task { await load() }
        }
    }

    /// Reset to the Status tab and drop any loaded series — the panel's default glance. Called when the panel
    /// closes, so each fresh open starts on Status and the Stats tab re-queries live rather than showing a
    /// stale window from a prior open.
    func reset() {
        tab = .status
        phase = .idle
    }

    /// Run the one-shot `stats` query and render loading → loaded / failed. Keeps prior `loaded` data visible
    /// while refetching (only a first load / retry shows `loading`), so a re-select never blanks the panel. A
    /// missing client short-circuits to `.failed(.unavailable)`.
    func load() async {
        guard let client else {
            phase = .failed(.unavailable)
            return
        }
        if phase.wire == nil {
            phase = .loading
        }

        let result = await client.send(StatsCommand())
        switch result {
        case .failure(let error):
            statsLog.error("stats: transport failure — \(String(describing: error), privacy: .public)")
            phase = .failed(.transport(error))
        case .success(let line):
            do {
                switch try decodeStatsReply(line) {
                case .ok(let wire):
                    phase = .loaded(wire)
                case .error(let reason):
                    statsLog.error("stats: daemon error — \(reason, privacy: .public)")
                    phase = .failed(.daemonError(reason))
                }
            } catch {
                statsLog.error("stats: undecodable reply — \(String(describing: error), privacy: .public)")
                phase = .failed(.undecodable)
            }
        }
    }
}

#if DEBUG
extension PanelStatsModel {
    /// Tooling / preview only (`--render-panel`, SwiftUI previews): a model pinned straight to the Stats tab's
    /// `.loaded` phase WITHOUT a transport, so `StatusPanelView`'s Stats tab can be rendered offscreen
    /// (`ImageRenderer`) for design-parity review against the mock. NOT a production path — the real phase is
    /// machine-derived from a socket reply, never set directly. SOCKET-FREE by construction: the client is nil
    /// and neither `select` nor `load` is called, so no `stats` query is ever fired — it stays as offline as
    /// every other `--render-panel` fixture (issue #704). Same-file so it can set the `private(set)` projection,
    /// mirroring `WatchStatusStore.preview`.
    static func loadedPreview(_ wire: StatsWire) -> PanelStatsModel {
        let model = PanelStatsModel(client: nil)
        model.tab = .stats
        model.phase = .loaded(wire)
        return model
    }

    /// The loaded 3-card series the `--render-panel` `stats` fixture (issue #704) renders, pinned to the mock's
    /// `healthy-stats-{light,dark}` frames (`design/menubar-preview.html`):
    ///   • Work (active, saturated) — session 42 / 100%, weekly 88%, 42 cap-hits
    ///   • Personal (balanced)      — session 31 /  96%, weekly 71%, 11 cap-hits
    ///   • Scratch (underused)      — session  4 /  22%, weekly 18%,  0 cap-hits
    ///   • aggregate — "All accounts ≥90% at once — 3 episodes (1h40m) · swaps 28 · last 7 days"
    ///
    /// `StatsWire` is Decodable-only (it mirrors the Rust `src/stats.rs` serializer), so the fixture is the
    /// wire's OWN JSON, DECODED — the exact path the live tab takes, only the bytes are pinned rather than
    /// socketed. Handles are the roster's CAPITALISED labels ("Work"/"Personal"/"Scratch") so the Stats rows
    /// join `RenderPanelTool`'s roster identities — `StatusPanelFormat.orderedStatHandles` matches by exact
    /// label. `StatsTests` guards the decode + the card values, so a fixture that drifts off the wire contract
    /// fails a headless test rather than silently rendering the loading placeholder.
    static var loadedPreviewFixture: StatsWire {
        guard let reply = try? decodeStatsReply(statsPreviewFixtureJSON), case .ok(let wire) = reply else {
            fatalError("stats preview fixture failed to decode — see StatsTests.testStatsPreviewFixtureDecodesToTheMockCards")
        }
        return wire
    }

    /// The raw JSON `loadedPreviewFixture` decodes (see it for the card values). One `stats` reply document:
    /// the `week` window (→ "last 7 days"), seven daily buckets whose per-account `session.peak` IS the
    /// sparkline series (the mock's three shapes; each series' max equals the summary peak, so a window peak is
    /// never below its own buckets), then the summary carrying the displayed numbers. Laid out one bucket per
    /// line for reviewability — inter-token whitespace is legal JSON, so the decoder reads it identically.
    static let statsPreviewFixtureJSON = #"""
    {"schema":1,"window":{"start":1783200000,"end":1783804800,"label":"last 7 days","period":"week"},"accounts":[],"series":[
    {"start":1783200000,"end":1783286400,"roster":{"swap_count":0,"swaps":{"session":0,"weekly":0,"manual":0,"forced":0,"emergency":0},"all_high_episodes":0,"all_high_secs":0},"accounts":{"Work":{"seen":1,"coverage":1.0,"coverage_class":"complete","session":{"mean":0.25,"peak":0.25,"p95":0.25},"weekly":{"mean":0.0,"peak":0.0,"p95":0.0},"cap_hits":0,"time_at_cap_secs":0,"contribution_share":0.0,"band":"low"},"Personal":{"seen":1,"coverage":1.0,"coverage_class":"complete","session":{"mean":0.12,"peak":0.12,"p95":0.12},"weekly":{"mean":0.0,"peak":0.0,"p95":0.0},"cap_hits":0,"time_at_cap_secs":0,"contribution_share":0.0,"band":"low"},"Scratch":{"seen":1,"coverage":1.0,"coverage_class":"complete","session":{"mean":0.1,"peak":0.1,"p95":0.1},"weekly":{"mean":0.0,"peak":0.0,"p95":0.0},"cap_hits":0,"time_at_cap_secs":0,"contribution_share":0.0,"band":"low"}}},
    {"start":1783286400,"end":1783372800,"roster":{"swap_count":0,"swaps":{"session":0,"weekly":0,"manual":0,"forced":0,"emergency":0},"all_high_episodes":0,"all_high_secs":0},"accounts":{"Work":{"seen":1,"coverage":1.0,"coverage_class":"complete","session":{"mean":0.5,"peak":0.5,"p95":0.5},"weekly":{"mean":0.0,"peak":0.0,"p95":0.0},"cap_hits":0,"time_at_cap_secs":0,"contribution_share":0.0,"band":"low"},"Personal":{"seen":1,"coverage":1.0,"coverage_class":"complete","session":{"mean":0.37,"peak":0.37,"p95":0.37},"weekly":{"mean":0.0,"peak":0.0,"p95":0.0},"cap_hits":0,"time_at_cap_secs":0,"contribution_share":0.0,"band":"low"},"Scratch":{"seen":1,"coverage":1.0,"coverage_class":"complete","session":{"mean":0.12,"peak":0.12,"p95":0.12},"weekly":{"mean":0.0,"peak":0.0,"p95":0.0},"cap_hits":0,"time_at_cap_secs":0,"contribution_share":0.0,"band":"low"}}},
    {"start":1783372800,"end":1783459200,"roster":{"swap_count":0,"swaps":{"session":0,"weekly":0,"manual":0,"forced":0,"emergency":0},"all_high_episodes":0,"all_high_secs":0},"accounts":{"Work":{"seen":1,"coverage":1.0,"coverage_class":"complete","session":{"mean":0.75,"peak":0.75,"p95":0.75},"weekly":{"mean":0.0,"peak":0.0,"p95":0.0},"cap_hits":0,"time_at_cap_secs":0,"contribution_share":0.0,"band":"low"},"Personal":{"seen":1,"coverage":1.0,"coverage_class":"complete","session":{"mean":0.55,"peak":0.55,"p95":0.55},"weekly":{"mean":0.0,"peak":0.0,"p95":0.0},"cap_hits":0,"time_at_cap_secs":0,"contribution_share":0.0,"band":"low"},"Scratch":{"seen":1,"coverage":1.0,"coverage_class":"complete","session":{"mean":0.22,"peak":0.22,"p95":0.22},"weekly":{"mean":0.0,"peak":0.0,"p95":0.0},"cap_hits":0,"time_at_cap_secs":0,"contribution_share":0.0,"band":"low"}}},
    {"start":1783459200,"end":1783545600,"roster":{"swap_count":0,"swaps":{"session":0,"weekly":0,"manual":0,"forced":0,"emergency":0},"all_high_episodes":0,"all_high_secs":0},"accounts":{"Work":{"seen":1,"coverage":1.0,"coverage_class":"complete","session":{"mean":1.0,"peak":1.0,"p95":1.0},"weekly":{"mean":0.0,"peak":0.0,"p95":0.0},"cap_hits":0,"time_at_cap_secs":0,"contribution_share":0.0,"band":"low"},"Personal":{"seen":1,"coverage":1.0,"coverage_class":"complete","session":{"mean":0.96,"peak":0.96,"p95":0.96},"weekly":{"mean":0.0,"peak":0.0,"p95":0.0},"cap_hits":0,"time_at_cap_secs":0,"contribution_share":0.0,"band":"low"},"Scratch":{"seen":1,"coverage":1.0,"coverage_class":"complete","session":{"mean":0.14,"peak":0.14,"p95":0.14},"weekly":{"mean":0.0,"peak":0.0,"p95":0.0},"cap_hits":0,"time_at_cap_secs":0,"contribution_share":0.0,"band":"low"}}},
    {"start":1783545600,"end":1783632000,"roster":{"swap_count":0,"swaps":{"session":0,"weekly":0,"manual":0,"forced":0,"emergency":0},"all_high_episodes":0,"all_high_secs":0},"accounts":{"Work":{"seen":1,"coverage":1.0,"coverage_class":"complete","session":{"mean":1.0,"peak":1.0,"p95":1.0},"weekly":{"mean":0.0,"peak":0.0,"p95":0.0},"cap_hits":0,"time_at_cap_secs":0,"contribution_share":0.0,"band":"low"},"Personal":{"seen":1,"coverage":1.0,"coverage_class":"complete","session":{"mean":0.7,"peak":0.7,"p95":0.7},"weekly":{"mean":0.0,"peak":0.0,"p95":0.0},"cap_hits":0,"time_at_cap_secs":0,"contribution_share":0.0,"band":"low"},"Scratch":{"seen":1,"coverage":1.0,"coverage_class":"complete","session":{"mean":0.12,"peak":0.12,"p95":0.12},"weekly":{"mean":0.0,"peak":0.0,"p95":0.0},"cap_hits":0,"time_at_cap_secs":0,"contribution_share":0.0,"band":"low"}}},
    {"start":1783632000,"end":1783718400,"roster":{"swap_count":0,"swaps":{"session":0,"weekly":0,"manual":0,"forced":0,"emergency":0},"all_high_episodes":0,"all_high_secs":0},"accounts":{"Work":{"seen":1,"coverage":1.0,"coverage_class":"complete","session":{"mean":0.87,"peak":0.87,"p95":0.87},"weekly":{"mean":0.0,"peak":0.0,"p95":0.0},"cap_hits":0,"time_at_cap_secs":0,"contribution_share":0.0,"band":"low"},"Personal":{"seen":1,"coverage":1.0,"coverage_class":"complete","session":{"mean":0.55,"peak":0.55,"p95":0.55},"weekly":{"mean":0.0,"peak":0.0,"p95":0.0},"cap_hits":0,"time_at_cap_secs":0,"contribution_share":0.0,"band":"low"},"Scratch":{"seen":1,"coverage":1.0,"coverage_class":"complete","session":{"mean":0.11,"peak":0.11,"p95":0.11},"weekly":{"mean":0.0,"peak":0.0,"p95":0.0},"cap_hits":0,"time_at_cap_secs":0,"contribution_share":0.0,"band":"low"}}},
    {"start":1783718400,"end":1783804800,"roster":{"swap_count":0,"swaps":{"session":0,"weekly":0,"manual":0,"forced":0,"emergency":0},"all_high_episodes":0,"all_high_secs":0},"accounts":{"Work":{"seen":1,"coverage":1.0,"coverage_class":"complete","session":{"mean":0.75,"peak":0.75,"p95":0.75},"weekly":{"mean":0.0,"peak":0.0,"p95":0.0},"cap_hits":0,"time_at_cap_secs":0,"contribution_share":0.0,"band":"low"},"Personal":{"seen":1,"coverage":1.0,"coverage_class":"complete","session":{"mean":0.4,"peak":0.4,"p95":0.4},"weekly":{"mean":0.0,"peak":0.0,"p95":0.0},"cap_hits":0,"time_at_cap_secs":0,"contribution_share":0.0,"band":"low"},"Scratch":{"seen":1,"coverage":1.0,"coverage_class":"complete","session":{"mean":0.12,"peak":0.12,"p95":0.12},"weekly":{"mean":0.0,"peak":0.0,"p95":0.0},"cap_hits":0,"time_at_cap_secs":0,"contribution_share":0.0,"band":"low"}}}
    ],"summary":{"roster":{"swap_count":28,"swaps":{"session":19,"weekly":5,"manual":3,"forced":1,"emergency":0},"all_high_episodes":3,"all_high_secs":6000},"accounts":{"Work":{"seen":7,"coverage":1.0,"coverage_class":"complete","session":{"mean":0.42,"peak":1.0,"p95":0.95},"weekly":{"mean":0.6,"peak":0.88,"p95":0.85},"cap_hits":42,"time_at_cap_secs":5400,"contribution_share":0.62,"band":"at_cap"},"Personal":{"seen":7,"coverage":1.0,"coverage_class":"complete","session":{"mean":0.31,"peak":0.96,"p95":0.9},"weekly":{"mean":0.5,"peak":0.71,"p95":0.68},"cap_hits":11,"time_at_cap_secs":900,"contribution_share":0.28,"band":"moderate"},"Scratch":{"seen":7,"coverage":1.0,"coverage_class":"complete","session":{"mean":0.04,"peak":0.22,"p95":0.2},"weekly":{"mean":0.1,"peak":0.18,"p95":0.16},"cap_hits":0,"time_at_cap_secs":0,"contribution_share":0.1,"band":"low"}}}}
    """#
}
#endif
