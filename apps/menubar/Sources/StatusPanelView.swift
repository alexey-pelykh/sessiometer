// Copyright (c) 2026 Oleksii PELYKH
// SPDX-License-Identifier: MIT

// The menu-bar popover's content view (issue #325): a MINIMAL, neutral placeholder panel the
// `NSStatusItem` click hosts via `NSPopover` + `NSHostingController`. It observes the same
// `WatchStatusStore` (#324) the status-item glyph does, so the popover is HONEST — it renders the
// current connection state and the (retained, possibly-stale) roster, and never claims "live" on a
// degraded or absent daemon. The RICH panel — per-account health, copy-command affordances, the full
// nine-state treatment in `design-menubar` — is #168 / #169; this is D4 chrome only, enough to prove
// the popover hosting + click-to-toggle path end to end.

import SwiftUI

/// The popover body: a header, a plain-language state line, and a lean roster. Neutral chrome (D1) —
/// system materials + semantic colors, no provider brand mark or color.
struct StatusPanelView: View {
    @ObservedObject var store: WatchStatusStore

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            Text("Sessiometer")
                .font(.headline)
            Text(stateSummary)
                .font(.subheadline)
                .foregroundStyle(.secondary)
            if !store.rows.isEmpty {
                Divider()
                ForEach(store.rows) { row in
                    HStack {
                        Text(row.label)
                        Spacer()
                        if row.isActive {
                            Text("active").foregroundStyle(.secondary)
                        }
                    }
                    .font(.callout)
                }
            }
        }
        .padding(12)
        .frame(width: 260, alignment: .leading)
    }

    /// A plain-language line for the current honest state — the panel analogue of the glyph's spoken
    /// label. Never claims "live" on a degraded / absent daemon (the never-healthy-when-dead invariant
    /// is enforced in the store; this view only renders what the store reports).
    private var stateSummary: String {
        switch store.connectionState {
        case .connecting:   return "Connecting to the daemon…"
        case .connected:    return "Live"
        case .emptyRoster:  return "Connected — no accounts configured"
        case .stale:        return "Data may be stale — the daemon has gone quiet"
        case .disconnected: return "Disconnected — the daemon is not responding"
        case .unsupported:  return "Daemon version unsupported — update required"
        }
    }
}
