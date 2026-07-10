// Copyright (c) 2026 Oleksii PELYKH
// SPDX-License-Identifier: MIT

// Pure presentation formatting for the menu-bar status panel (issue #326): every text/glyph the
// SwiftUI panel renders, extracted as PURE functions over the store's already-decoded view state so
// they mirror the `status` verb's renderers in `src/cli.rs` and are unit-testable WITHOUT launching
// the app — exactly the pure-core / thin-shell split `HonestStateMachine` + `WatchStatusStore` use.
// `StatusPanelView` (the SwiftUI layer) is a thin consumer of these; the app never renders a number
// this file did not format, so the parity tests in `StatusPanelFormatTests` gate the whole panel.
//
// Source of truth mirrored — do NOT re-derive (grep the symbols, they move):
//   * `src/cli.rs` `health_glyph`      → `healthGlyph`      (the 4+1-state emoji rollup)
//   * `src/cli.rs` `health_cell`       → `authCell`         (glyph + `claude /login` / `recovering` cue + `disabled`)
//   * `src/cli.rs` `legacy_health_tags`→ `legacyHealthTags` (the pre-#119 auth-nil fallback)
//   * `src/cli.rs` `reset_cell`        → `resetCell`        (one window's "resets in", or `n/a`)
//   * `src/cli.rs` `humanize_until`    → `humanizeUntil`    (two-largest-unit compact duration)
//   * `src/cli.rs` `pct`               → `pct`              (`N%` or `n/a`, never a fabricated 0)
//
// The panel's SINGLE per-row reset-in pick (weekly-exhausted → weekly, else session), the honest-state
// banners, and the `next_swap` footer wording are #326's OWN panel spec (the issue AC), not a
// `src/cli.rs` mirror — the CLI prints both reset columns and phrases the footer differently.

import Foundation

/// Pure formatting for the status panel. A caseless namespace of `static` functions — no state, no
/// I/O, no clock (the caller passes `now`), so every output is a deterministic function of its inputs.
enum StatusPanelFormat {

    /// The `sessiometer capture` CLI subcommand — retained as the equivalent terminal command an operator
    /// may prefer (and the parity anchor for `StatusPanelFormatTests`). As of #360 the panel's PRIMARY
    /// capture path is the in-app "Capture active account" affordance (a real daemon-routed action over the
    /// #358 transport), NOT a clipboard copy of this string — see the capture-affordance copy below.
    static let captureCommand = "sessiometer capture"

    // MARK: - Capture affordance copy (issue #360 — the in-app capture states)

    /// The pending label. Capture is now a REAL daemon-routed action (#360: command → daemon → redacted
    /// ack), so a pending state is HONEST — unlike the superseded copy-command, which never ran and so had
    /// no honest in-flight state (design-menubar's old "no fake spinner" scoped only the never-running
    /// copy-command; a real action earns a real pending).
    static let capturePendingText = "Capturing…"

    /// The success confirmation — "Captured '<label>'" under the label the daemon actually ASSIGNED (the
    /// UUID-derived handle when the operator left the field blank), echoed from the redacted ack so the
    /// operator sees the real handle, never a fabricated one. Curly quotes match the panel's typography.
    static func captureDoneText(label: String) -> String {
        "Captured \u{2018}\(label)\u{2019}"
    }

    /// Human copy for a capture failure — the redacted machine verdict mapped to ONE operator-facing
    /// sentence (never the raw kebab tag or transport jargon), actionable where there is an action. Pure:
    /// a deterministic function of the non-secret `CaptureFailure`, unit-tested in isolation.
    static func captureErrorText(_ failure: CaptureFailure) -> String {
        switch failure {
        case .rejected(let reason):
            switch reason {
            case .noActiveAccount: return "No active account — run claude /login, then capture."
            case .keychainLocked:  return "Keychain is locked — unlock it, then try again."
            case .swapLockBusy:    return "The daemon is busy — try again in a moment."
            case .failed:          return "Capture failed — try again."
            }
        case .daemonError(let reason):
            // The same-user local peer should never be unauthorized; surface it plainly if it ever happens.
            return reason == "unauthorized" ? "Not authorized to capture." : "Capture failed — try again."
        case .transport(let error):
            switch error {
            case .connectionRefused: return "The daemon isn’t running."
            case .timedOut:          return "The daemon didn’t respond — try again."
            case .closedBeforeAck:   return "The daemon closed the connection — try again."
            case .encodeFailed, .io: return "Capture failed — try again."
            }
        case .undecodable:
            return "Unexpected reply from the daemon."
        case .unavailable:
            return "The daemon socket is unreachable."
        }
    }

    // MARK: - Percentage cell (mirror `src/cli.rs` `pct`)

    /// A `0...100` percent as `N%`, or `n/a` when the last poll failed — never a fabricated `0`
    /// (mirrors `src/cli.rs` `pct`).
    static func pct(_ percent: UInt8?) -> String {
        percent.map { "\($0)%" } ?? "n/a"
    }

    // MARK: - Reset-in cell (mirror `src/cli.rs` `humanize_until` / `reset_cell`)

    /// A whole-second remaining time as a compact "resets in" — the two largest non-zero units, e.g.
    /// `12m` / `4h` / `3d4h` — mirroring `src/cli.rs` `humanize_until` EXACTLY: a reset already reached
    /// (`<= 0`) is `now`, and under a minute is `<1m`.
    static func humanizeUntil(_ secs: Int64) -> String {
        if secs <= 0 { return "now" }
        let minute: Int64 = 60
        let hour: Int64 = 60 * minute
        let day: Int64 = 24 * hour
        let days = secs / day
        let hours = (secs % day) / hour
        let mins = (secs % hour) / minute
        if days > 0 {
            return hours > 0 ? "\(days)d\(hours)h" : "\(days)d"
        } else if hours > 0 {
            return mins > 0 ? "\(hours)h\(mins)m" : "\(hours)h"
        } else if mins > 0 {
            return "\(mins)m"
        } else {
            return "<1m"
        }
    }

    /// One window's "resets in" against the client's own clock `now`, or `n/a` when the instant is
    /// unknown (mirrors `src/cli.rs` `reset_cell`) — never a fabricated duration.
    static func resetCell(_ resetAt: Int64?, now: Int64) -> String {
        guard let at = resetAt else { return "n/a" }
        return humanizeUntil(at - now)
    }

    /// The panel's SINGLE per-row reset-in (issue #326 AC): a `weekly_exhausted` account keys off its
    /// WEEKLY reset — it is blocked for the week regardless of the session window — otherwise the
    /// SESSION reset, the sooner and more-actionable window. Humanized like `resetCell`, against the
    /// client's own clock `now`.
    static func resetIn(
        weeklyExhausted: Bool,
        sessionResetsAt: Int64?,
        weeklyResetsAt: Int64?,
        now: Int64
    ) -> String {
        let instant = weeklyExhausted ? weeklyResetsAt : sessionResetsAt
        return resetCell(instant, now: now)
    }

    // MARK: - AUTH cell (mirror `src/cli.rs` `health_glyph` / `health_cell` / `legacy_health_tags`)

    /// The emoji glyph for a 4+1-state credential rollup — self-coloring content, not an overlay —
    /// mirroring `src/cli.rs` `health_glyph` (issue #119; the neutral `⚪` for `unknown` is the anti-#137
    /// "no false green" verdict).
    static func healthGlyph(_ health: CredentialHealth) -> String {
        switch health {
        case .healthy: return "🟢"
        case .unknown: return "⚪"
        case .stale:   return "🟡"
        case .atRisk:  return "🟠"
        case .dead:    return "🔴"
        }
    }

    /// The native SF Symbol + semantic tint for a health state — the PANEL's per-medium render of the
    /// SAME `CredentialHealth` the CLI (and `healthGlyph`, the byte-parity mirror) shows as an emoji. R-2
    /// was re-ratified (2026-07-09) as STATE-parity — the enum + `authSpoken` rendered per-medium — so
    /// the panel draws a native symbol while the CLI keeps its emoji. DISTINCT SHAPES per state (checkmark
    /// / question / clock / triangle / octagon), so health is legible WITHOUT color — the WCAG 1.4.1 fix
    /// the shape-identical emoji ramp lacked. `unknown` stays neutral (the #137 "no false green").
    static func healthSymbol(_ health: CredentialHealth) -> (name: String, tint: HealthTint) {
        switch health {
        case .healthy: return ("checkmark.circle.fill", .green)
        case .unknown: return ("questionmark.circle", .neutral)
        case .stale:   return ("clock.badge.exclamationmark", .yellow)
        case .atRisk:  return ("exclamationmark.triangle.fill", .orange)
        case .dead:    return ("xmark.octagon.fill", .red)
        }
    }

    /// The semantic tint ROLE for a health symbol. This Foundation-only namespace cannot name a SwiftUI
    /// `Color`, so it names the ROLE; the view maps it (via `healthTint`) to a concrete tint — never
    /// `Color.accentColor` (the AUTH glyph is never app-tinted, #84).
    enum HealthTint: Equatable { case green, yellow, orange, red, neutral }

    /// The RESOLVED tint target for a panel role — the Foundation-only handle the SwiftUI view turns into a
    /// concrete `Color`. `.asset` names an asset-catalog color set (#388: a theme-adaptive, contrast-safe
    /// token carrying Any/Dark + Increased-Contrast variants, because a raw system `Color` fails WCAG
    /// non-text/text contrast on the translucent vibrancy — system yellow ≈ 1.2:1 there); `.secondary` /
    /// `.primary` keep the system semantic colors where contrast already passes (neutral / no-data — the
    /// #137 "no false green").
    enum PanelTint: Equatable {
        case asset(String)
        case secondary
        case primary
    }

    /// The AUTH glyph's tint token (#388 token table). The healthy check and the warm warning tints move to
    /// contrast-safe asset tokens (`--ok` / `--ut-a` / `--ut-o` / `--ut-r` from the design mock); `.neutral`
    /// (unknown) stays `Color.secondary` — the #137 "no false green". `.yellow` (stale) and `.orange` (atRisk)
    /// map to DISTINCT tokens (amber vs orange), never one collapsed amber: severity-by-warmth is a second
    /// channel over the distinct shapes, and the `status` CLI keeps its 🟡 / 🟠 apart too (state-parity).
    static func healthTint(_ tint: HealthTint) -> PanelTint {
        switch tint {
        case .green:   return .asset("HealthOK")    // mock --ok  (healthy)
        case .yellow:  return .asset("UtilAmber")   // mock --ut-a (stale)
        case .orange:  return .asset("UtilOrange")  // mock --ut-o (atRisk)
        case .red:     return .asset("UtilRed")     // mock --ut-r (dead)
        case .neutral: return .secondary            // mock --text-3 (unknown)
        }
    }

    /// The full AUTH cell string, mirroring `src/cli.rs` `health_cell` BYTE-FOR-BYTE: the glyph, a DEAD
    /// account's actionable `claude /login` cue (softened to `recovering` for a healing quarantined
    /// account, issue #109), then the independent `disabled` rotation tag (#36). A pre-#119 daemon
    /// (`auth == nil`) falls back to the legacy comma-joined tags. Kept as the parity anchor for the
    /// tests and the row's VoiceOver label; the VIEW draws the glyph and cue as separate elements via
    /// `healthGlyph` + `authCue`.
    static func authCell(
        auth: CredentialHealth?,
        recovering: Bool,
        enabled: Bool,
        quarantined: Bool
    ) -> String {
        guard let health = auth else {
            return legacyHealthTags(enabled: enabled, quarantined: quarantined, recovering: recovering)
        }
        var cell = healthGlyph(health)
        if health == .dead {
            cell += " " + (recovering ? "recovering" : "claude /login")
        }
        if !enabled {
            cell += " disabled"
        }
        return cell
    }

    /// The trailing AUTH cue WITHOUT the glyph — the DEAD account's `claude /login` / `recovering`
    /// action plus a trailing `disabled`, or `nil` when there is no cue — for the modern (`auth != nil`)
    /// path where the view renders the glyph as its own element. The legacy (`auth == nil`) path uses
    /// `legacyHealthTags` as plain text instead.
    static func authCue(auth: CredentialHealth?, recovering: Bool, enabled: Bool) -> String? {
        var parts: [String] = []
        if auth == .dead {
            parts.append(recovering ? "recovering" : "claude /login")
        }
        if !enabled {
            parts.append("disabled")
        }
        return parts.isEmpty ? nil : parts.joined(separator: " ")
    }

    /// The pre-#119 AUTH text for an account whose daemon sent no rollup (`auth == nil`), mirroring
    /// `src/cli.rs` `legacy_health_tags`: comma-joined `disabled` (#36) + `needs re-login` /
    /// `recovering` (#42/#109), so an old daemon degrades gracefully rather than showing a
    /// defaulted-healthy glyph over a dead account.
    static func legacyHealthTags(enabled: Bool, quarantined: Bool, recovering: Bool) -> String {
        var status = ""
        if !enabled {
            status += "disabled"
        }
        if quarantined {
            if !status.isEmpty {
                status += ", "
            }
            status += recovering ? "recovering" : "needs re-login"
        }
        return status
    }

    // MARK: - Honest-state banner (issue #326 AC — panel spec, not a `src/cli.rs` mirror)

    /// The visual weight of a banner — drives the panel's tint (semantic `Color`), never a literal hex.
    enum BannerKind: Equatable {
        /// The one healthy state — a live, schema-supported roster.
        case healthy
        /// A neutral, non-degraded transitional/info state (connecting, empty-roster).
        case info
        /// Degraded but recoverable, last-known data shown MARKED stale (stale).
        case warning
        /// Absent or unreadable — numbers are not trustworthy (disconnected, unsupported).
        case error
    }

    /// A rendered honest-state banner: a headline + one plain sentence + its tint. Every
    /// `ConnectionState` maps to exactly one banner (the AC's connecting / connected / dropped / empty /
    /// stale / unsupported), so the panel ALWAYS states its honest connection status and never renders
    /// healthy on a degraded or absent daemon.
    struct Banner: Equatable {
        let title: String
        let detail: String
        let kind: BannerKind
    }

    /// Derive the honest-state banner for a connection state (`accountCount` speaks the live count).
    /// Pure — the same state always yields the same banner. The `disconnected` reason is deliberately
    /// NOT surfaced verbatim (it is transport jargon, e.g. "connection closed (EOF)"); the banner is a
    /// plain operator-facing sentence.
    ///
    /// `ageText` (from `snapshotAgeText`) folds the snapshot's freshness into the detail for the three
    /// states that RETAIN a reading (connected / stale / disconnected) — so a persistent "Live" never
    /// silently implies the numbers are fresh (the council's "don't let Live imply fresh"). It is
    /// deliberately omitted for `connecting` (no snapshot yet), `emptyRoster` (no reading to age), and
    /// `unsupported` (numbers refused — the banner shows no freshness). `ageStale` (from
    /// `snapshotIsStale`) escalates a Live-but-stale daemon (transport up, data outlived any poll
    /// cadence) from `.healthy` to `.warning` — the connected-but-stale cell of the matrix.
    static func banner(for state: ConnectionState,
                       accountCount: Int,
                       ageText: String? = nil,
                       ageStale: Bool = false) -> Banner {
        switch state {
        case .connecting:
            return Banner(title: "Connecting…",
                          detail: "Reaching the daemon.",
                          kind: .info)
        case .connected:
            let plural = accountCount == 1 ? "" : "s"
            let base = "\(accountCount) account\(plural)"
            return Banner(title: "Live",
                          detail: ageText.map { "\(base) · \($0)." } ?? "\(base).",
                          kind: ageStale ? .warning : .healthy)
        case .emptyRoster:
            return Banner(title: "No accounts yet",
                          detail: "Connected to the daemon — no accounts configured.",
                          kind: .info)
        case .stale:
            let base = "Daemon quiet; showing last-known"
            return Banner(title: "Data may be stale",
                          detail: ageText.map { "\(base) · \($0)." } ?? "\(base).",
                          kind: .warning)
        case .disconnected:
            let base = "Reconnecting; showing last-known"
            return Banner(title: "Daemon not responding",
                          detail: ageText.map { "\(base) · \($0)." } ?? "\(base).",
                          kind: .error)
        case .unsupported:
            return Banner(title: "Update required",
                          detail: "The daemon speaks a newer version this app can't read.",
                          kind: .error)
        }
    }

    // MARK: - Snapshot age (issue #326 / council — the CLI's parity render of the wire `generated_at`)

    /// The age (in seconds) past which a snapshot's data is UNAMBIGUOUSLY stale — the maximum possible
    /// poll cadence (`POLL_SECS_HI` = 3600 in `src/daemon.rs`). A snapshot older than this has outlived
    /// even the slowest legitimate poll interval, so it cannot be dismissed as "just a long cadence."
    /// Deliberately conservative: it NEVER false-alarms a healthy-but-slow daemon (the client does not
    /// know the configured cadence, so a lower bar would cry wolf), and the transport-liveness watchdog
    /// (#344, 32 s) already catches a DROPPED connection far sooner. This is the backstop for the one
    /// gap the watchdog misses — a daemon that keeps HEARTBEATING while its poll loop is wedged (frames
    /// still arrive, so the connection reads live, but `generated_at` stops advancing). Mirrors the Rust
    /// `STALE_AGE_SECS` (`src/cli.rs`); the two thresholds move together.
    static let staleAgeSecs: Int64 = 3600

    /// "updated Ns ago" for a snapshot's freshness, or `nil` when there is no generation instant
    /// (`generatedAt <= 0` — the wire's `0` sentinel for an all-defaults / never-generated snapshot).
    /// The age is `now - generatedAt` against the client's OWN clock, humanized with the SAME
    /// two-largest-unit `humanizeUntil` the reset-in uses (so the vocabulary matches and the panel↔CLI
    /// parity is inherited from that already-byte-mirrored humanizer). Clamped at 0 for a benign
    /// client-ahead clock skew. This is the field the wire contract itself earmarks for exactly this UX
    /// (`snapshot.rs`: "a client compares it against its own clock and greys out once the gap grows").
    static func snapshotAgeText(generatedAt: Int64, now: Int64) -> String? {
        guard generatedAt > 0 else { return nil }
        let age = max(0, now - generatedAt)
        return age == 0 ? "updated just now" : "updated \(humanizeUntil(age)) ago"
    }

    /// Whether a snapshot is unambiguously stale — older than `staleAgeSecs`. `false` for a snapshot
    /// with no generation instant (`generatedAt <= 0`): absent freshness is NOT stale (it is unknown).
    /// Drives the connected-but-stale banner escalation (a `Live` daemon whose data has outlived any
    /// poll cadence is flagged `.warning`).
    static func snapshotIsStale(generatedAt: Int64, now: Int64) -> Bool {
        generatedAt > 0 && (now - generatedAt) > staleAgeSecs
    }

    // MARK: - Usage severity + swap-trigger (mirror `src/cli.rs` `util_severity` / `weekly_cell_severity`)

    /// One utilization urgency band. Mirrors the subset of `src/cli.rs` `Severity` the per-cell
    /// utilization overlay uses — the reset-proximity `Dim` and the account-aggregate's reset-soon
    /// downgrade are CLI-table concerns (the `ACCOUNT` cell), NOT the per-metric panel color, so the
    /// panel mirror is the three utilization bands only.
    enum UsageSeverity: Equatable { case green, yellow, red }

    /// The usage %-TEXT tint token (#388). The meter percent is small text (WCAG 4.5:1), so it uses the
    /// darker `--ut-*` TEXT family — NOT the brighter `--u-*` BAR-FILL family the meter bar keeps (a bar
    /// is a non-text fill, 3:1, and the mock fills it with the system-bright colors). A failed poll (`nil`)
    /// stays `.primary`: an uncolored metric, never a false "healthy" green (#137).
    static func usageTextTint(_ severity: UsageSeverity?) -> PanelTint {
        switch severity {
        case .green:  return .asset("UtilGreen")   // mock --ut-g
        case .yellow: return .asset("UtilAmber")   // mock --ut-a
        case .red:    return .asset("UtilRed")     // mock --ut-r
        case .none:   return .primary
        }
    }

    /// The urgency band for a utilization percent — the panel's mirror of `src/cli.rs` `util_severity`:
    /// `>= 90` Red (at/near the ~95% session swap-away trigger, #41), `>= 75` Yellow (worth watching),
    /// else Green. One shared "how full is too full" definition (issue #84), so the panel's per-metric
    /// threshold color keys off the SAME bands as the CLI's per-cell overlay for the same reading.
    static func utilSeverity(_ pct: UInt8) -> UsageSeverity {
        if pct >= 90 { return .red }
        if pct >= 75 { return .yellow }
        return .green
    }

    /// The SESSION metric's severity — the raw `utilSeverity` of its percent, or `nil` when the poll
    /// failed (the `n/a` text carries the truth; an uncolored metric is not a false "healthy"). Mirrors
    /// the CLI's `session_severity` (`account.session_pct.map(util_severity)`).
    static func sessionSeverity(_ sessionPct: UInt8?) -> UsageSeverity? {
        sessionPct.map(utilSeverity)
    }

    /// The WEEKLY metric's severity — `utilSeverity` of its percent, EXCEPT a weekly-EXHAUSTED account
    /// (the daemon's blocked-for-the-week verdict, #11/#37) reads Red whatever the rounded percent — a
    /// week-blocked account is never painted "healthy", even under a lowered `weekly_trigger`. `nil`
    /// when the weekly poll failed. Mirrors the CLI's `weekly_cell_severity`.
    static func weeklySeverity(weeklyPct: UInt8?, weeklyExhausted: Bool) -> UsageSeverity? {
        weeklyPct.map { weeklyExhausted ? .red : utilSeverity($0) }
    }

    // MARK: - `next_swap` footer (issue #326 AC — renders the FORWARD candidate, not swap history)

    /// The footer line for the daemon's `next_swap` candidate, or `nil` when there is no active anchor
    /// to swap from (the footer is then absent). Renders the FORWARD candidate the `watch` wire carries
    /// — NOT swap history (a true last-swap needs a new daemon source; issue #326 note).
    static func nextSwapFooter(_ nextSwap: NextSwap?) -> String? {
        switch nextSwap {
        case .target(let to): return "Next swap → \(to)"
        case .noViableTarget: return "No viable target"
        case .awaitingData:   return "Awaiting data"
        case nil:             return nil
        }
    }

    // MARK: - Header identity + swap callout (issue #355 — design-reference parity)

    /// The header's identity sub-line — the design reference's `app-sub` ("N accounts · {active}
    /// active"). Honest per connection state: a degraded roster reads "last-known" and a Live-but-wedged
    /// or gone-quiet snapshot appends "· stale", so the always-present identity line NEVER implies the
    /// numbers are live/fresh (the never-healthy-on-degraded discipline, carried into the header).
    static func headerSubtitle(state: ConnectionState,
                               accountCount: Int,
                               activeLabel: String?,
                               ageStale: Bool) -> String {
        let plural = accountCount == 1 ? "" : "s"
        let count = "\(accountCount) account\(plural)"
        switch state {
        case .connecting:   return "Connecting to the daemon…"
        case .emptyRoster:  return "Welcome"
        case .unsupported:  return "Version mismatch"
        case .disconnected: return "\(count) · last-known"
        case .connected, .stale:
            let base = activeLabel.map { "\(count) · \($0) active" } ?? count
            let isStale: Bool = { if case .stale = state { return true } else { return ageStale } }()
            return isStale ? "\(base) · stale" : base
        }
    }

    /// The swap-callout target label (the design reference's hero card), or `nil` when there is no
    /// forward candidate — the card is then absent (same single-cardinality as `nextSwapFooter`; a
    /// `noViableTarget` / `awaitingData` / absent anchor shows no card).
    static func swapCalloutTarget(_ nextSwap: NextSwap?) -> String? {
        if case .target(let to) = nextSwap { return to }
        return nil
    }

    /// The swap-callout's muted "why" line — a CLIENT-derived description of the target row, because
    /// the wire's `next_swap` carries only the label, never a reason (#15). Facts only: the target's
    /// weekly headroom, flagged "lowest weekly" ONLY when it genuinely has the least weekly usage among
    /// the viable swap candidates (computed by the caller) — never an invented rationale.
    static func swapCalloutReason(targetWeeklyPct: UInt8?, isLowestWeekly: Bool) -> String {
        guard let weekly = targetWeeklyPct else {
            return isLowestWeekly ? "most headroom" : "next candidate"
        }
        return isLowestWeekly ? "lowest weekly · \(pct(weekly)) · most headroom" : "weekly \(pct(weekly))"
    }

    // MARK: - Row VoiceOver label (issue #326 AC — VoiceOver-navigable rows)

    /// One spoken, comma-separated sentence for a row's VoiceOver label, so the whole row reads as a
    /// single accessible element rather than a scatter of unlabeled glyphs. Speaks identity, the active
    /// marker, the auth verdict + its cue, both usage percents each with its own reset-in — the same facts
    /// the row shows visually. Next-swap is NOT per-row (R-2 re-ratified 2026-07-09): it is a single-cardinality
    /// fact spoken once by the footer, mirroring the CLI (which has no per-row next marker).
    static func rowAccessibilityLabel(
        label: String,
        isActive: Bool,
        auth: CredentialHealth?,
        recovering: Bool,
        enabled: Bool,
        quarantined: Bool,
        sessionPct: UInt8?,
        weeklyPct: UInt8?,
        sessionReset: String,
        weeklyReset: String
    ) -> String {
        var parts: [String] = [label]
        if isActive { parts.append("active") }
        parts.append(authSpoken(auth: auth, recovering: recovering, enabled: enabled, quarantined: quarantined))
        // Both windows, each with its reset — matching the row's two meters and the CLI's two columns.
        parts.append("session \(pct(sessionPct)) resets in \(sessionReset)")
        parts.append("weekly \(pct(weeklyPct)) resets in \(weeklyReset)")
        // Drop any empty auth phrase (a healthy pre-#119 legacy account speaks no auth verdict).
        return parts.filter { !$0.isEmpty }.joined(separator: ", ")
    }

    /// A spoken auth verdict for VoiceOver — the glyph's meaning in words (the emoji alone is a
    /// color-only signal), plus the DEAD cue and `parked` tag. Empty for a healthy pre-#119 legacy
    /// account that carries no verdict and no tags.
    static func authSpoken(
        auth: CredentialHealth?,
        recovering: Bool,
        enabled: Bool,
        quarantined: Bool
    ) -> String {
        var phrase: String
        if let health = auth {
            switch health {
            case .healthy: phrase = "auth healthy"
            case .unknown: phrase = "auth unknown"
            case .stale:   phrase = "auth stale"
            case .atRisk:  phrase = "auth at risk"
            case .dead:    phrase = recovering ? "recovering" : "credential dead, run claude /login"
            }
        } else {
            // Legacy (auth nil): speak only the tags the CLI would show.
            if quarantined {
                phrase = recovering ? "recovering" : "needs re-login"
            } else {
                phrase = ""
            }
        }
        if !enabled {
            phrase = phrase.isEmpty ? "parked" : "\(phrase), parked"
        }
        return phrase
    }
}
