// Copyright (c) 2026 Oleksii PELYKH
// SPDX-License-Identifier: MIT

// The status panel's Stats tab (issue #446), split out of `StatusPanelView` by #640: the design reference's
// per-account 7-day sparklines and numeric body, the aggregate callout, and the signal legend — fed by the socket
// `stats` verb, never a store read. READ-ONLY by construction: it queries and states magnitudes, it never acts and
// never advises (the crown-jewel and footer-`next_swap` invariants belong to the Status tab). The sparkline
// geometry, the signal banding, and every string are the unit-tested `StatusPanelFormat`; these views only stroke
// and lay out. Provider-neutral (#173), exactly like the Status roster.

import SwiftUI

// MARK: - Stats tab (issue #446 — the mock's `.stats` view over the socket `stats` verb)

/// The Stats tab body (issue #446): the mock's per-account 7-day sparklines + numeric body, aggregate
/// callout, and signal legend — fed by the socket `stats` verb (never a store read). Renders the stats
/// model's phase honestly: a loading placeholder, a failure message (never a blank tab), or the loaded
/// content. READ-ONLY — it queries and renders, it never acts (the crown-jewel + footer-`next_swap`
/// invariants belong to the Status tab).
struct StatsView: View {
    @EnvironmentObject private var store: WatchStatusStore
    @EnvironmentObject private var stats: PanelStatsModel

    var body: some View {
        switch stats.phase {
        case .idle, .loading:
            StatsMessage(text: "Loading usage stats…")
        case .failed(let failure):
            StatsMessage(text: StatusPanelFormat.statsFailureText(failure))
        case .loaded(let wire):
            StatsContent(wire: wire,
                         activeLabel: store.rows.first(where: \.isActive)?.label,
                         rosterOrder: store.rows.map(\.label))
        }
    }
}

/// A centered one-line Stats-tab message — the loading placeholder and the honest failure surface. Keeps the
/// tab from ever rendering blank (or a fabricated number) when there is no series to show.
private struct StatsMessage: View {
    let text: String

    var body: some View {
        Text(text)
            .font(.system(size: 12))
            .foregroundStyle(.secondary)
            .multilineTextAlignment(.center)
            .fixedSize(horizontal: false, vertical: true)
            .frame(maxWidth: .infinity, alignment: .center)
            .padding(.horizontal, 20).padding(.vertical, 22)
    }
}

/// The loaded Stats view: the per-account rows (ordered to match the Status roster), then the aggregate
/// callout + signal legend. Identity (name + monogram + active marker) joins the stats handles with the
/// live roster the panel already holds — provider-neutral (#173), exactly like the Status roster.
private struct StatsContent: View {
    let wire: StatsWire
    /// The active account's handle (from the watch snapshot the panel already renders) — marks the active
    /// stats row, the only roster fact the Stats tab reads. `nil` when none is active.
    let activeLabel: String?
    /// The roster's handle order, so the Stats rows list accounts identically to the Status tab.
    let rosterOrder: [String]

    var body: some View {
        let handles = StatusPanelFormat.orderedStatHandles(
            summaryHandles: Set(wire.summary.accounts.keys), rosterOrder: rosterOrder)
        // Resolve monograms over the SAME handle set the Stats tab lists (issue #445) — the disambiguation
        // kit applies identically on both tabs, since they render the same accounts.
        let monograms = StatusPanelFormat.accountMonograms(handles)
        VStack(alignment: .leading, spacing: 0) {
            // The daemon told us its config is unreadable (issue #642) — say so ABOVE the numbers, so the
            // caveat is read before the figures it qualifies, never as a footnote after they've landed.
            if let detail = wire.configUnreadable {
                StatsCaveat(text: StatusPanelFormat.statsConfigUnreadableNote(detail))
            }
            VStack(alignment: .leading, spacing: 2) {
                ForEach(handles, id: \.self) { handle in
                    if let account = wire.summary.accounts[handle] {
                        StatStripRow(handle: handle,
                                     monogram: monograms[handle] ?? "?",
                                     account: account,
                                     series: StatusPanelFormat.sparkSeries(wire.series, handle: handle),
                                     isActive: handle == activeLabel)
                    }
                }
            }
            // Mock `.stats { padding:6px 8px 2px }` — inset to align with the roster + aggregate below.
            .padding(.horizontal, PanelMetrics.rosterInset).padding(.top, 6).padding(.bottom, 2)

            StatsAggregate(roster: wire.summary.roster, window: wire.window)
            SignalLegend()
        }
    }
}

/// The Stats-tab caveat strip (issue #642): a dot-plus-sentence line telling the operator that the figures
/// below rest on DEFAULT tunables because the daemon could not read `config.toml`. Deliberately the same
/// dot + tint + secondary-text vocabulary as `BannerView` (the panel's established degraded idiom) rather
/// than a new affordance — and deliberately `.orange`/`.warning` weight, not `.error`: the series IS real
/// data, only its ceiling-dependent framing is off. Panel-surface only; it never escalates the menu-bar
/// glyph (a new glyph fault class is design-gated, and a stale tunable is not a fleet fault).
///
/// Part of this text is DAEMON-AUTHORED, so the panel does not own its length. The daemon keeps it short by
/// construction — it sends one of a small set of STATIC reasons, never the parser's own message (which
/// re-prints the operator's config file) — and `lineLimit` below is the panel-side suspenders to that belt:
/// the popover is fixed in WIDTH (380pt) but INTRINSIC in height, with no `ScrollView`, so an unbounded
/// reason from a drifted daemon would not clip, it would grow the whole panel arbitrarily tall.
private struct StatsCaveat: View {
    let text: String

    var body: some View {
        HStack(alignment: .firstTextBaseline, spacing: 8) {
            Circle()
                .fill(Color.orange)
                .frame(width: 6, height: 6)
                .accessibilityHidden(true)
            Text(text)
                .font(.caption)
                .foregroundStyle(.secondary)
                .fixedSize(horizontal: false, vertical: true)
                .lineLimit(5)
                .truncationMode(.tail)
            Spacer(minLength: 0)
        }
        // `rosterInset` is the stats block's own inset; the `+ 8` is the row card's internal padding
        // (mock `.stat { padding:10px 8px }`), so this dot lines up with a row's status dot, not its card edge.
        .padding(.horizontal, PanelMetrics.rosterInset + 8)
        .padding(.top, 8)
        .accessibilityElement(children: .combine)
        .accessibilityLabel(text)
    }
}

/// One account's Stats row (mock `.stat`): identity + a 7-day session-peak sparkline + the neutral signal
/// pill, over a three-cell numeric body (session mean/peak, weekly peak, cap-hits). The active account wears
/// the accent-tint card fill (mock `.stat.active`, the SAME `--active-bg` token the Status roster uses).
private struct StatStripRow: View {
    let handle: String
    /// The roster-resolved 2-char monogram for this handle (issue #445), computed once by `StatsContent`.
    let monogram: String
    let account: StatsAccountStats
    /// The per-bucket session-peak series (0…1, fixed-scale) — the sparkline source.
    let series: [Double]
    let isActive: Bool
    @Environment(\.colorScheme) private var colorScheme

    var body: some View {
        let signal = StatusPanelFormat.statsSignal(account.band)
        VStack(alignment: .leading, spacing: 9) {
            HStack(spacing: 9) {
                StatusDot(isActive: isActive)
                MonogramBadge(label: handle, monogram: monogram)
                // Provider-neutral name (#15): the redacted handle, exactly as the Status roster shows it —
                // the mock's `.s-prov` provider brand is intentionally dropped (the shipped app names no provider).
                Text(handle)
                    .font(.system(size: 13, weight: .semibold))
                    // MIDDLE-truncation (issue #445) — same as the Status roster, so a same-local-part handle's
                    // distinguishing suffix survives elision on the Stats tab too.
                    .lineLimit(1).truncationMode(.middle)
                Spacer(minLength: 6)
                Sparkline(values: series)
                SignalPill(signal: signal)
            }
            HStack(alignment: .top, spacing: 8) {
                StatCell(label: "Session m/pk", value: StatusPanelFormat.statsSessionMeanPeak(account))
                StatCell(label: "Weekly pk", value: StatusPanelFormat.statsWeeklyPeak(account))
                StatCell(label: "Cap-hits", value: "\(account.capHits)")
            }
            .padding(.leading, 17)  // mock `.stat-body { margin-left:17px }` — aligns the body under the name
        }
        .padding(.vertical, 10).padding(.horizontal, 8)  // mock `.stat { padding:10px 8px }`
        .background(
            RoundedRectangle(cornerRadius: 9)
                .fill(isActive ? Color.accentEmphasis(.activeRowFill, dark: colorScheme == .dark) : Color.clear)
        )
        .accessibilityElement(children: .ignore)
        .accessibilityLabel(accessibilityLabel)
    }

    /// The spoken row summary — identity + signal + the numeric body, so the sparkline (accessibility-hidden,
    /// a purely visual trend) is still conveyed as facts.
    private var accessibilityLabel: String {
        let signal = StatusPanelFormat.statsSignal(account.band)
        let sessionMean = StatusPanelFormat.statsPercent(account.session.mean)
        let sessionPeak = StatusPanelFormat.statsPercent(account.session.peak)
        let weeklyPeak = StatusPanelFormat.statsPercent(account.weekly.peak)
        let active = isActive ? ", active" : ""
        return "\(handle)\(active). \(signal.label). Session mean \(sessionMean) percent, "
            + "peak \(sessionPeak) percent. Weekly peak \(weeklyPeak) percent. \(account.capHits) cap hits."
    }
}

/// One numeric cell of the Stats row's three-column body (mock `.sc`): an uppercase micro-label over a
/// tabular-figure value. Equal-width (`maxWidth: .infinity`), mirroring the mock's `repeat(3, 1fr)` grid.
private struct StatCell: View {
    let label: String
    let value: String

    var body: some View {
        VStack(alignment: .leading, spacing: 2) {
            Text(label.uppercased())
                .font(.system(size: 9, weight: .semibold)).tracking(0.5)
                .foregroundStyle(.tertiary)
                .lineLimit(1)
            Text(value)
                .font(.system(size: 13, weight: .semibold)).monospacedDigit()
                .lineLimit(1)
        }
        .frame(maxWidth: .infinity, alignment: .leading)
    }
}

/// The per-account 7-day sparkline (mock `.spark`): a drawn area + line + end-dot over the per-bucket session
/// peaks, on the FIXED [0, 1] (0–100% of cap) scale — R-2 parity with the CLI trend sparkline (`src/stats.rs`),
/// NOT auto-normalised. The 96 × 28 box + inset 3 reproduce the mock's `.spark` viewBox exactly. The geometry
/// is the pure, unit-tested `StatusPanelFormat.sparkPoints`; this view only strokes/fills it.
private struct Sparkline: View {
    let values: [Double]
    @Environment(\.colorScheme) private var colorScheme

    private let boxWidth = 96.0
    private let boxHeight = 28.0
    private let inset = 3.0

    var body: some View {
        Canvas { context, _ in
            let points = StatusPanelFormat.sparkPoints(values, width: boxWidth, height: boxHeight, inset: inset)
            guard points.count >= 2 else { return }
            let color = Color.spark(dark: colorScheme == .dark)

            var line = Path()
            line.move(to: CGPoint(x: points[0].x, y: points[0].y))
            for point in points.dropFirst() {
                line.addLine(to: CGPoint(x: point.x, y: point.y))
            }

            // Area = the line closed down to the plot baseline (mock `.sp-area` closes to y = height − inset),
            // filled at a fraction of the stroke alpha (mock `.sp-area { fill-opacity:.2 }`).
            let baseline = boxHeight - inset
            var area = line
            area.addLine(to: CGPoint(x: points[points.count - 1].x, y: baseline))
            area.addLine(to: CGPoint(x: points[0].x, y: baseline))
            area.closeSubpath()
            context.fill(area, with: .color(color.opacity(0.2)))

            context.stroke(line, with: .color(color),
                           style: StrokeStyle(lineWidth: 1.75, lineCap: .round, lineJoin: .round))

            // The end dot marks the latest bucket (mock `.sp-dot`, r 1.7).
            let last = points[points.count - 1]
            let dot = Path(ellipseIn: CGRect(x: last.x - 1.7, y: last.y - 1.7, width: 3.4, height: 3.4))
            context.fill(dot, with: .color(color))
        }
        .frame(width: boxWidth, height: boxHeight)
        .accessibilityHidden(true)  // a purely visual trend; the row label speaks the numeric values
    }
}

/// The neutral utilisation signal pill (mock `.signal`): a colored dot + descriptor word (underused /
/// balanced / saturated), tinted by the mock's `--sig-*` tokens. A DESCRIPTOR of the session-peak band, never
/// a recommendation — the read-only Stats tab states the magnitude, it does not advise.
private struct SignalPill: View {
    let signal: StatusPanelFormat.StatSignal
    @Environment(\.colorScheme) private var colorScheme

    var body: some View {
        let dark = colorScheme == .dark
        HStack(spacing: 5) {
            Circle()
                .fill(Color.statsSignalText(signal, dark: dark))
                .frame(width: 6, height: 6)
            Text(signal.label)
                .font(.system(size: 11, weight: .semibold))
                .foregroundStyle(Color.statsSignalText(signal, dark: dark))
                .lineLimit(1)
        }
        .padding(.horizontal, 8).padding(.vertical, 3)
        .background(Capsule().fill(Color.statsSignalFill(signal, dark: dark)))
        .fixedSize()
        .accessibilityHidden(true)  // spoken via the row's accessibility label
    }
}

/// The aggregate callout under the Stats rows (mock `.agg`): the roster-wide all-accounts-high water + swap
/// count over the window, in a neutral card. Facts only (magnitudes + the span), never a recommendation.
private struct StatsAggregate: View {
    let roster: StatsRoster
    let window: StatsWindow
    @Environment(\.colorScheme) private var colorScheme

    var body: some View {
        HStack(alignment: .top, spacing: 8) {
            Image(systemName: "chart.line.uptrend.xyaxis")
                .font(.system(size: 12))
                .foregroundStyle(.secondary)
            Text(StatusPanelFormat.statsAggregateText(roster: roster, window: window))
                .font(.system(size: 11.5))
                .foregroundStyle(.secondary)
                .fixedSize(horizontal: false, vertical: true)
            Spacer(minLength: 0)
        }
        .padding(.vertical, 9).padding(.horizontal, 11)
        .background(RoundedRectangle(cornerRadius: 9).fill(Color.panelFill(.card, dark: colorScheme == .dark)))
        .padding(.horizontal, 12).padding(.vertical, 6)  // mock `.agg { margin:6px 12px }`
    }
}

/// The signal legend (mock `.sig-legend`): the three descriptor pills + the neutrality note. Reinforces the
/// read-only ethos — "descriptive · equal weight · no action implied" — so the pills never read as an alarm.
private struct SignalLegend: View {
    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            HStack(spacing: 7) {
                Text("SIGNAL")
                    .font(.system(size: 9.5, weight: .semibold)).tracking(0.6)
                    .foregroundStyle(.tertiary)
                SignalPill(signal: .underused)
                SignalPill(signal: .balanced)
                SignalPill(signal: .saturated)
                Spacer(minLength: 0)
            }
            Text("descriptive · equal weight · no action implied")
                .font(.system(size: 10)).italic()
                .foregroundStyle(.tertiary)
        }
        // Mock `.sig-legend { margin:2px 12px 13px; border-top }` — a hairline over the note.
        .padding(.horizontal, 12).padding(.top, 10).padding(.bottom, 13)
        .overlay(alignment: .top) {
            Divider().padding(.horizontal, 12)
        }
        .accessibilityHidden(true)  // static explanatory chrome; the per-row labels speak each signal
    }
}
