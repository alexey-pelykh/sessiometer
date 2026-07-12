// Copyright (c) 2026 Oleksii PELYKH
// SPDX-License-Identifier: MIT

// The SINGLE source of truth for the status panel's SwiftUI environment wiring (issue #504). Both hosts of
// `StatusPanelView` — the live menu-bar app (`StatusItemController`) and the DEBUG offscreen design-parity
// harness (`RenderPanelTool`, the `--render-panel` tool) — inject the panel's `@EnvironmentObject`
// dependencies through THIS one modifier, so the two cannot drift: every environment object the panel and its
// subviews read is listed here exactly once, and both hosts pass the same set.
//
// Why it exists — the drift this guards against: #446 added the Stats tab, making `StatusPanelView` read a
// `PanelStatsModel` via `@EnvironmentObject`. `StatusItemController` (the app, exercised every run) was
// updated to inject it; `RenderPanelTool` (the harness, exercised only when regenerating renders) was not — so
// `--render-panel` rendered a view with an unsatisfied `@EnvironmentObject` and `ImageRenderer` trapped at
// render time (`No ObservableObject of type PanelStatsModel found`), silently breaking the whole design-parity
// workflow. Routing BOTH hosts through one modifier turns that class of drift into a COMPILE error: adding a
// new panel environment object is a change to this signature, which fails BOTH call sites to build until each
// supplies it — the rarely-run harness can no longer fall behind the always-run app.
//
// Completeness caveat (honest scope of the guard): this couples the two INJECTION sites at build time; it does
// not, by itself, prove the list is EXHAUSTIVE against the view's actual `@EnvironmentObject` declarations —
// SwiftUI resolves those at render time, not compile time. Keeping this the ONLY `.environmentObject` path for
// the panel (no inline injection at either host), plus the app's own routine runtime exercise, is what closes
// the residual gap: a newly-required object surfaces the moment the app runs, and the fix — extending this one
// modifier — then forces the harness to keep up at build time. Not DEBUG-gated: the live app uses it too.

import SwiftUI

extension View {
    /// Inject the COMPLETE set of environment objects `StatusPanelView` (and its subviews) require. Every
    /// panel host must route through this modifier rather than calling `.environmentObject` directly — see this
    /// file's header for the #504 drift it exists to make a build error rather than a silent render-time crash.
    func statusPanelEnvironment(store: WatchStatusStore,
                                capture: AccountCaptureModel,
                                swap: AccountSwapModel,
                                stats: PanelStatsModel) -> some View {
        self
            .environmentObject(store)
            .environmentObject(capture)
            .environmentObject(swap)
            .environmentObject(stats)
    }
}
