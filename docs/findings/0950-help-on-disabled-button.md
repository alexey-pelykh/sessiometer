# Finding #950 — does `.help()` surface a tooltip on a `.disabled()` Button?

A blocked roster row is rendered as a `.disabled()` `Button` that also carries the explanatory
tooltip (`apps/menubar/Sources/StatusPanelRoster.swift`):

```swift
.disabled(blockReason != nil || swap.phase.isPending)   // :275
.help(hoverText)                                        // :276
```

`hoverText` (`:310`) resolves to `StatusPanelFormat.switchBlockedText(blockReason)` on a blocked row —
the copy that explains *why* the row cannot be switched to. If `.help()` did not surface on a disabled
control, that copy would be unreachable on **every** blocked row and **every** pending-swap row.

**Verdict: YES — `.disabled()` does not suppress `.help()`.** Measured by hover on macOS **26.5.2**
(build 25F84): a disabled `Button` carrying `.help()` shows its tooltip, with the **same** text,
placement and delay as an identical enabled control. It holds in **both** modifier orders and in
**both** host windows tested — including a faithful replica of the shipped panel's borderless,
non-activating `NSPanel` in an accessory app. The `switchBlockedText` copy **is** reachable on hover.

Because the answer is yes, the issue's conditional branch — *"if no, state where the blocked-reason
copy should live instead"* — **does not fire**, and no relocation is recommended. #959 (render no chip
on a blocked row) therefore does **not** strand the block explanation: hover keeps it, and VoiceOver
already carries it independently via `rowSwitchAccessibilityLabel` (`:279-281`).

**One caveat, and it is not about disabled-ness:** tooltip display is gated by the app's
activation/key state, and that gate is **indifferent to `.disabled()`** — in the fully inactive,
non-key state *nothing* showed a tooltip, the **enabled** control included (§ The activation gate).

## What was measured — and what was not

**This is a minimal SwiftUI harness, not the shipped panel.** The issue explicitly permits this
substitute and requires it be declared: no roster fixture with a populated `blockReason` or a pending
swap exists in `apps/menubar/Tests/Fixtures.swift`, and reproducing one live needs specific daemon
state. The harness reproduces the *modifier composition under test* verbatim — `Button` +
custom `ButtonStyle` + `.disabled(true)` + `.help(...)`, in the shipped order — not the roster's data
flow. What generalises is the **platform behaviour**; the roster's own wiring is not re-proven here.

Every run carried both controls, because either one alone is uninterpretable:

- **Positive control (B, enabled).** Without it, "the disabled button showed nothing" cannot be
  distinguished from "the rig cannot observe tooltips at all".
- **Negative control (D, no `.help()` anywhere).** Without it, "a tooltip window exists" cannot be
  distinguished from a detector firing on something that is not a tooltip.

Detection was **twofold** and had to agree: (a) a visible in-process `NSToolTipPanel` window, and
(b) a screenshot in which the tooltip's **case-unique marker string** is legible under the **correct**
cell. Requiring (b) is what makes the result per-case rather than merely per-run.

## Results

Case **A** is the shipped composition. Case **C** reverses the two modifiers, since SwiftUI modifier
order is semantically load-bearing and "we happened to write it the other way" is a real failure mode.

| Host / app state | D — no `.help()` (neg. control) | A — **disabled**, shipped order | B — enabled (pos. control) | C — **disabled**, reversed order |
|---|---|---|---|---|
| Titled `NSWindow`, `.regular` app, app active | **NONE** ✅ | **SHOWN** `MARKER-A-DISABLED` | **SHOWN** `MARKER-B-ENABLED` | **SHOWN** `MARKER-C-DISABLED` |
| Borderless `.nonactivatingPanel` in accessory app, panel key + app active | **NONE** ✅ | **SHOWN** | **SHOWN** | **SHOWN** |
| Same panel, app **inactive** + panel **not key** | **NONE** | **NONE** | **NONE** ⚠️ | **NONE** |

Reading the table: in **every** state where a tooltip appeared at all, the disabled cases (A, C) were
**indistinguishable** from the enabled case (B). Disabled-ness never once suppressed a tooltip.

The second row is the fidelity-relevant one: the shipped panel is a borderless,
`.nonactivatingPanel` `FloatingPanel` at `.popUpMenu` level inside an `NSVisualEffectView`, hosted by
an accessory app (`StatusItemController.swift:75-110`, `:362-372`). The harness replicates that host
construction verbatim, so the answer is not an artifact of a plain titled test window.

## The activation gate — a separate axis, with an unresolved residual

The third row is the one honest surprise: with the app inactive **and** the panel not key, no tooltip
appeared for **any** case — the **enabled** positive control included. So this is **not** a
disabled-specific hazard; it suppresses viable and blocked rows alike, and it does not change the
#950 answer.

**What stays unresolved.** The shipped panel is shown by `openPanel()` as
`panel.orderFrontRegardless()` then `panel.makeKey()` (`StatusItemController.swift:296-307`), whose
comment asserts the accessory app stays **inactive** while the panel becomes **key**. That
combination — `panelIsKey=true, appIsActive=false` — is the shipped state, and **the harness could not
construct it**: `NSApp.deactivate()` followed by `makeKey()` re-activated the app
(`appIsActive=true`), collapsing into the second row. Two states were reached; the shipped one was
inferred to be equivalent to neither with certainty.

This is recorded as **capture-pending**, not asserted: whether the *shipped* panel runs active or
inactive in an operator's hands — and therefore whether tooltips surface there at all — needs a hover
against the **real** running app, not this harness. It is a question about **all** row tooltips, not
about `.disabled()`, so it belongs with the tooltip-scope work (#953) rather than here. Nothing in
#950's answer depends on it: whatever activation state the panel is in, blocked rows and viable rows
get the same treatment.

## Version boundary — read before reusing this

**Observed on macOS 26.5.2 (25F84) only.** The deployment target is **macOS 13.0**, and no 13.0 host
was reachable from this environment, so **13.0 is NOT measured** and must not be read as measured.

What is observed, and bounds the risk: the tooltip is an AppKit **`NSToolTipPanel`** — a window class
whose presence shows SwiftUI's `.help()` resolving onto AppKit's long-standing `NSView.toolTip`
machinery, which is driven by **tracking areas**, not by a control's enabled state. A 13.0 divergence
is therefore **unlikely** — but that is an inference from an observed mechanism, **not** a measurement
on 13.0. If a 13.0 (or any older-OS) host becomes available, re-run § Reproducing this; it is a
two-minute check, which is the point of recording it here.

## Reproducing this

Self-contained: no project build, no daemon, no fixtures. Requires Accessibility permission (to post
synthetic mouse-moved events) and Screen Recording permission (for the corroborating capture) for the
invoking terminal.

```swift
// help-spike.swift — swiftc -O help-spike.swift -o help-spike && ./help-spike
import Cocoa
import SwiftUI

private struct RowStyle: ButtonStyle {
    func makeBody(configuration: Configuration) -> some View {
        configuration.label.frame(width: 240, height: 70).background(Color(white: 0.85))
    }
}

private struct Cases: View {
    var body: some View {
        HStack(spacing: 30) {
            // A: the shipped composition — .disabled() BEFORE .help()
            Button(action: {}) { Text("A disabled") }
                .buttonStyle(RowStyle()).disabled(true).help("MARKER-A-DISABLED")
            // B: POSITIVE control — proves the rig can see a tooltip at all
            Button(action: {}) { Text("B enabled") }
                .buttonStyle(RowStyle()).disabled(false).help("MARKER-B-ENABLED")
            // C: reversed modifier order
            Button(action: {}) { Text("C disabled") }
                .buttonStyle(RowStyle()).help("MARKER-C-DISABLED").disabled(true)
            // D: NEGATIVE control — no .help() at all
            Text("D no help").frame(width: 240, height: 70).background(Color(white: 0.85))
        }.padding(40)
    }
}

let screenH = NSScreen.screens[0].frame.height
let source = CGEventSource(stateID: .hidSystemState)

func hover(_ p: NSPoint) {                  // jiggle: a tooltip needs movement INSIDE the tracking area
    for dx in [CGFloat(0), 3, -2, 1] {
        CGEvent(mouseEventSource: source, mouseType: .mouseMoved,
                mouseCursorPosition: CGPoint(x: p.x + dx, y: screenH - p.y),
                mouseButton: .left)?.post(tap: .cghidEventTap)
        usleep(60_000)
    }
}

func tooltips() -> [NSWindow] {
    NSApp.windows.filter { String(describing: type(of: $0)).contains("ToolTip") && $0.isVisible }
}

final class Delegate: NSObject, NSApplicationDelegate {
    var window: NSWindow!
    func applicationDidFinishLaunching(_ n: Notification) {
        window = NSWindow(contentRect: NSRect(x: 150, y: 300, width: 1180, height: 200),
                          styleMask: [.titled], backing: .buffered, defer: false)
        window.contentView = NSHostingView(rootView: Cases())
        window.acceptsMouseMovedEvents = true
        window.makeKeyAndOrderFront(nil)
        NSApp.activate(ignoringOtherApps: true)
        DispatchQueue.global().async { self.run() }
    }
    func run() {
        Thread.sleep(forTimeInterval: 2)
        // CLEAN START: a cursor already resting on a cell shows THAT cell's tooltip and
        // contaminates the first probe. Park clear, then assert none is up.
        DispatchQueue.main.sync { hover(NSPoint(x: 1700, y: 1000)) }
        Thread.sleep(forTimeInterval: 3)
        DispatchQueue.main.sync { print("PRECONDITION tooltips=\(tooltips().count)") }
        var f = NSRect.zero
        DispatchQueue.main.sync { f = self.window.frame }
        // Probe D (negative control) FIRST, so a false-positive detector is caught before A.
        for (i, name) in [(3, "D-negative"), (0, "A-disabled"), (1, "B-enabled"), (2, "C-reversed")] {
            let p = NSPoint(x: f.origin.x + 40 + Double(i) * 270 + 120, y: f.midY - 12)
            DispatchQueue.main.sync { hover(p) }
            var seen = false
            let deadline = Date().addingTimeInterval(8)     // generous: a NONE must not be impatience
            while Date() < deadline, !seen {
                DispatchQueue.main.sync { seen = !tooltips().isEmpty }
                usleep(150_000)
            }
            print("RESULT \(name) tooltip=\(seen ? "SHOWN" : "NONE")")
            // Screenshot here and confirm the marker text sits under the RIGHT cell — the
            // window-class check alone does not attribute a tooltip to a case.
            DispatchQueue.main.sync { hover(NSPoint(x: 1700, y: 1000)) }
            Thread.sleep(forTimeInterval: 2.5)             // let the panel dismiss before the next probe
        }
        DispatchQueue.main.async { NSApp.terminate(nil) }
    }
}

let app = NSApplication.shared
let delegate = Delegate()
app.delegate = delegate
app.setActivationPolicy(.regular)
app.run()
```

Expected: `D NONE, A SHOWN, B SHOWN, C SHOWN`. **`D SHOWN` or `B NONE` invalidates the run** — the
detector is firing spuriously, or the rig cannot see tooltips — and the result must be discarded
rather than reported. For the panel-host variant, swap the `NSWindow` for a borderless
`.nonactivatingPanel` `NSPanel` with `canBecomeKey` overridden to `true`, inside an
`NSVisualEffectView`, under `setActivationPolicy(.accessory)`.

## Downstream

- **#959 — unblocked.** Removing the blocked-row chip does not strand the block explanation: the
  hover tooltip carries `switchBlockedText`, and the VoiceOver label carries it independently. #959
  may ship standalone on this axis; #950 imposes no reachable-explanation prerequisite on it.
- **#955 — no consequence to implement.** The conditional relocation branch did not fire; the copy
  stays where it is.
- **#953 (tooltip scope, row-vs-chip) — inherits the residual.** Two items land there, both about
  tooltips generally rather than about `.disabled()`: the unresolved activation-gate state above,
  and the standing fact that a hover tooltip is a **pointer-only** affordance (keyboard-only users
  reach the block reason through the accessibility label, not the tooltip).

## Provenance

Method: a purpose-built minimal SwiftUI harness (§ Reproducing this) run on **macOS 26.5.2 (25F84)**,
Xcode 26.6 / Swift 6.3.3, Apple Silicon, single 1920×1080 display, 2026-07-30. Three runs across two
host configurations and three app-activation states; synthetic `CGEvent` mouse-moved hovers;
detection by visible in-process `NSToolTipPanel` **plus** a `screencapture` frame in which the
case-unique marker string is legible under the correct cell. Both a positive (enabled) and a negative
(no-`.help()`) control were carried in every run and both behaved as required.

Two rig defects were found and corrected before the reported run, and are recorded because they would
silently corrupt a re-run: (1) a cursor left resting on a cell by a previous run showed **that** cell's
tooltip during the **first** probe, producing a false positive on the negative control — two captures
were byte-identical (same SHA) despite different timestamps, which is how it was caught; fixed by
parking the cursor clear and asserting zero tooltips before probing. (2) A KVC text-dump helper raised
`NSUnknownKeyException` on `NSVisualEffectView`; fixed with a `responds(to:)` guard. The
window-class signal alone proved **insufficient** for per-case attribution — the screenshot check is
load-bearing, not decoration.

Boundary: the **measured** claim is that `.disabled()` does not suppress `.help()` on macOS 26.5.2, in
a harness, in two host configurations and both modifier orders. **Not measured**: macOS 13.0 (the
deployment target — no host reachable); the shipped panel's own activation state at hover time
(capture-pending, § The activation gate); and the roster's own data flow, which this harness does not
exercise. The 13.0 low-divergence-risk statement is an **inference** from the observed `NSToolTipPanel`
mechanism, never a measurement.

Code cited: `apps/menubar/Sources/StatusPanelRoster.swift` (`:275` `.disabled`, `:276` `.help`,
`:279-281` accessibility label, `:310` `hoverText`), `apps/menubar/Sources/StatusItemController.swift`
(`:75-110` panel host, `:296-307` `openPanel`, `:362-372` `FloatingPanel`). No credentials read, no
network call, no daemon state consulted. Related: #959 (blocked-row chip), #955 (affordance coverage),
#953 (tooltip scope).

macOS 26.5.2 · sessiometer #950.
