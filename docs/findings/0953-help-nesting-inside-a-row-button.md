# Finding #953 — does a `.help()` on a child inside a row `Button` surface on hover?

**Verdict: NOT ESTABLISHED.** The question is open, and the fix it gates (#953 AC-1, scoping the switch
tooltip to the chip) must not ship until it is answered. Two things *were* settled, and both matter:

1. **There is no deterministic route to the answer.** SwiftUI's `.help()` does not materialize as an
   AppKit `NSView.toolTip` on the view it is attached to, does not register a tooltip owner the hosting
   view responds to, and does not expose a readable accessibility-help node in-process. A live hover is
   the only route — which is what makes this expensive rather than a two-minute check.
2. **One run produced a signal, and it points the wrong way for the fix**: a `.help()` on a child inside
   a `Button` surfaced **nowhere**, while its row-level neighbours surfaced everywhere. That run's
   geometry could not be validated, so it is **suggestive, not measured**. It is recorded here because
   it is the reason the fix is blocked rather than merely unverified — if it holds, scoping the switch
   invitation to the chip makes that copy **unreachable**, which is worse than the defect #953 fixes.

## Why this matters more than it looks

The switch invitation (`switchHelpText`) is today attached to the row-wrapping `Button`, which is why
hovering the **health glyph** answers with the *switch* copy — the #953 defect. The prescribed fix moves
that copy onto the chip. If a nested `.help()` does not surface, that move does not narrow the tooltip's
scope; it **deletes** the tooltip. The failure mode is silent: no crash, no failing test, no golden
moves (a tooltip is a hover affordance and the goldens render at `.idle`), and the panel looks correct
in every static capture. Nothing in CI would catch it.

## What was measured, and what was not

**Not the shipped panel.** This is a minimal SwiftUI harness, as #950's was — the same substitute, for
the same reason: no roster fixture with a populated `blockReason` exists, and reproducing one live needs
specific daemon state. What generalises is the platform behaviour; the roster's own wiring is not
re-proven here.

Every run carried a **negative control** (a row with no `.help()` anywhere) and a **positive control**
(the #953 defect itself — row-level help answering over the health glyph). Per #950's rule, a run whose
positive control reads NONE is discarded rather than reported: it cannot distinguish "this element has
no tooltip" from "this rig cannot see tooltips".

### The one run that produced a signal

`probe3`, macOS 26.5.2 (25F84), bare binary launched from a terminal holding the Accessibility grant:

| Case | body | health glyph | chip |
|---|---|---|---|
| **R** — row-level `.help()` only (today's shape) | SHOWN | **SHOWN** | SHOWN |
| **C** — chip-level `.help()` only | NONE | NONE | **NONE** |
| **B** — both row-level and chip-level | SHOWN | SHOWN | SHOWN |
| **N** — negative control, no `.help()` | NONE | — | NONE |

Read literally: R reproduces the defect (the glyph answers with the row's copy), and C shows a
chip-level `.help()` inside a `Button` surfacing **nowhere, including on the chip itself**.

**Why it is not reported as measured.** The rig computed the hosting view's screen origin from the
window's CONTENT RECT, but `NSHostingView` sized itself to its fitting size inside a larger window, so
where each probe point actually landed was never established. Rows below the third produced results
inconsistent with rows above them, which is the tell. R and C are adjacent rows computed identically, so
the contrast between them is the strongest part of the run — but "strongest part of an unvalidated run"
is not a measurement, and the detection could not read the tooltip's own text, so even the SHOWN cells
are unattributed to a case.

### Five rigs that failed, and why each would corrupt a re-run

Recorded because each looks like it works:

- **Deterministic inspection (`probe1`, `probe2`)** — walking the AppKit tree for `NSView.toolTip` finds
  nothing (SwiftUI installs a single tracking area on the hosting view), and the hosting view does not
  respond to `view:stringForToolTip:point:userData:`, so the tooltip owner cannot be queried per point.
- **Unvalidated geometry (`probe3`)** — above. Size the WINDOW to `hosting.fittingSize` so the hosting
  view's bounds equal the content rect.
- **Hit-test landing validation (`probe4`)** — modern SwiftUI draws without per-row `NSView`s, so every
  hit test returns the hosting view itself. All 17 probes reported INVALID; the validator proved nothing.
  Verify landings by rendered PIXEL instead.
- **Absolute pixel matching (`probe5`)** — captured colours come back roughly halved, and probe points
  that sat on text or on a row's edge read black. Normalise each sample by its own max channel and match
  by HUE, and give each probe region a solid colour rather than text.
- **Main-thread event pumping (`probe7`)** — driving the sequence on the main thread with
  `RunLoop.run(mode:before:)`, or with a manual `nextEvent`/`sendEvent` pump, produced runs where even
  the positive control read NONE. Only the real `NSApp.run()` loop, with the sequence on a background
  thread, was ever observed to surface a tooltip at all.

### Why the instrumented rig could not close the question

The rig that fixed all of the above — the one inlined under § Reproducing this — reports honestly and
reports nothing: run as a bare binary the host never became active or key — the exact state #950 § The
activation gate measured as showing **no** tooltip for **any** case, positive control included — so
every probe correctly self-reported INVALID rather than producing a NONE that looks like an answer.

Launched as a proper `.app` bundle the host **did** become active and key (`active=true key=true
visible=true`), and every probe then read NONE — positive control included. In that configuration the
synthetic cursor was observed parked at its rest position at read time for every probe, so the hover the
result depends on is not confirmed to have happened. Under #950's own discard rule this run is void, not
a measurement that nesting fails.

Console state was checked and is not the cause: `CGSessionCopyCurrentDictionary` reported
`onConsole=1` with no `CGSSessionScreenIsLocked` key.

## What would close this

The rig is inlined below so it survives this worktree. It needs two things this environment could not
supply together: a host that becomes **active and key**, and a **confirmed synthetic cursor landing** on
the probe point. It already asserts the first per probe; add the second before trusting a NONE — assert
`NSEvent.mouseLocation` has reached the target *before* polling, and treat a miss as INVALID rather than
as an absence. On a machine with an interactive session and Accessibility permission this is minutes.

Read the run like #950's: **if the positive control (`R/glyph`) reads NONE, discard the whole run** — the
rig cannot see tooltips, and every other NONE in it is meaningless. Then the answer is `C/chip` and
`E/chip`: a marker string means a nested `.help()` surfaces and #953 AC-1 can proceed; NONE on both, with
the positive control SHOWN, means it does not and the fix needs a different mechanism (restructuring the
row so the glyph sits outside the `Button`'s hit rect — a design decision, not an improvisation).

Failing that, the cheapest honest answer is a **manual hover** against the built app with a chip-level
`.help()` spiked in — one build, one hover, one screenshot.

## Reproducing this

Self-contained: no project build, no daemon, no fixtures. Needs Accessibility permission (to post
synthetic mouse-moved events) for whatever runs it.

Run it as an **`.app` bundle**, not as a bare binary — a bare binary did not become active or key here,
and per #950 § The activation gate an inactive, non-key host shows no tooltip for any case:

```sh
swiftc -O repro.swift -o repro
mkdir -p Repro.app/Contents/MacOS && cp repro Repro.app/Contents/MacOS/
cat > Repro.app/Contents/Info.plist <<'PLIST'
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
  <key>CFBundleExecutable</key><string>repro</string>
  <key>CFBundleIdentifier</key><string>com.example.tooltipprobe</string>
  <key>CFBundleName</key><string>TooltipProbe</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>NSPrincipalClass</key><string>NSApplication</string>
</dict></plist>
PLIST
open Repro.app --stdout "$PWD/run.log" --stderr "$PWD/run.err" && sleep 150 && cat run.log
```

```swift
// Reproduces sessiometer #953: does a `.help()` on a CHILD inside a row `Button` surface on hover,
// and how does it compose with a `.help()` on the Button itself?
//
// swiftc -O repro.swift -o repro && ./repro          # see § Reproducing this for the .app-bundle step
import Cocoa
import SwiftUI

let rowTints: [String: (Double, Double, Double)] = [
    "R": (1, 0, 0), "C": (0, 1, 0), "B": (0, 0, 1), "K": (1, 1, 0),
    "E": (1, 0, 1), "P": (0, 1, 1), "N": (1, 0.5, 0),
]
let glyphTint = (1.0, 1.0, 1.0)
let chipTint  = (0.5, 0.0, 1.0)

func color(_ t: (Double, Double, Double)) -> Color { Color(red: t.0, green: t.1, blue: t.2) }

struct RowStyle: ButtonStyle {
    let tint: Color
    func makeBody(configuration: Configuration) -> some View {
        configuration.label.frame(width: 300, height: 60).background(tint)
    }
}

struct OptionalHelp: ViewModifier {
    let text: String?
    @ViewBuilder func body(content: Content) -> some View {
        if let text { content.help(text) } else { content }
    }
}

/// The swap-chip slot stand-in, mirroring the shipped `switchSlot` composition — `Group` around the
/// glyph, then `.frame`, then (optionally) `.help`, then `.accessibilityHidden(true)` — because SwiftUI
/// modifier ORDER is semantically load-bearing (the same reason #950 carried a reversed-order case).
struct ChipSlot: View {
    var help: String? = nil
    var clear = false
    var body: some View {
        Group {
            if clear { Color.clear } else { Image(systemName: "arrow.left.arrow.right") }
        }
        .frame(width: 40, height: 24, alignment: .trailing)
        .background(clear ? Color.clear : color(chipTint))
        .modifier(OptionalHelp(text: help))
        .accessibilityHidden(true)
    }
}

struct Row: View {
    var chip = ChipSlot()
    var body: some View {
        HStack(spacing: 8) {
            Spacer()
            color(glyphTint).frame(width: 30, height: 24)      // the health-glyph stand-in
            chip
        }
    }
}

struct Cases: View {
    var body: some View {
        VStack(spacing: 20) {
            // R — row-level help only: today's shipped shape, and the #953 defect.
            Button(action: {}) { Row() }
                .buttonStyle(RowStyle(tint: color(rowTints["R"]!))).disabled(true).help("MARKER-ROW-ONLY")
            // C — chip-level help only, DISABLED row.
            Button(action: {}) { Row(chip: ChipSlot(help: "MARKER-CHIP-C")) }
                .buttonStyle(RowStyle(tint: color(rowTints["C"]!))).disabled(true)
            // B — BOTH row-level and chip-level: the composition question.
            Button(action: {}) { Row(chip: ChipSlot(help: "MARKER-CHIP-B")) }
                .buttonStyle(RowStyle(tint: color(rowTints["B"]!))).disabled(true).help("MARKER-ROW-B")
            // K — row-level help over a CLEAR slot: the shipped BLOCKED row post-#959.
            Button(action: {}) { Row(chip: ChipSlot(clear: true)) }
                .buttonStyle(RowStyle(tint: color(rowTints["K"]!))).disabled(true).help("MARKER-ROW-K")
            // E — chip help only on an ENABLED row: isolates "child help inside a Button" from
            //     disabled-ness, so a C failure cannot be blamed on `.disabled()`.
            Button(action: {}) { Row(chip: ChipSlot(help: "MARKER-CHIP-E")) }
                .buttonStyle(RowStyle(tint: color(rowTints["E"]!)))
            // P — POSITIVE control for the mechanism: chip help on a PLAIN (non-Button) row. Without
            //     it, a C/E failure cannot be attributed to the Button wrap rather than to child-help
            //     generally.
            Row(chip: ChipSlot(help: "MARKER-CHIP-P"))
                .frame(width: 300, height: 60).background(color(rowTints["P"]!))
            // N — NEGATIVE control: no .help() anywhere. A tooltip here invalidates the run.
            Button(action: {}) { Row() }
                .buttonStyle(RowStyle(tint: color(rowTints["N"]!)))
        }.padding(20)
    }
}

let screenH = NSScreen.screens[0].frame.height
let source = CGEventSource(stateID: .hidSystemState)

/// Sleep on the CALLING (background) thread. The main thread must stay inside `NSApp.run()` — two
/// substitutes were tried and both produced runs in which even the positive control read NONE:
/// `RunLoop.run(mode:before:)` fires run-loop sources but never dequeues AppKit events, and a manual
/// `nextEvent`/`sendEvent` pump did not surface tooltips either. Only the real `NSApp.run()` loop does.
func spin(_ seconds: Double) { Thread.sleep(forTimeInterval: seconds) }

func hover(_ p: NSPoint) {
    for dx in [CGFloat(0), 3, -2, 1] {
        CGEvent(mouseEventSource: source, mouseType: .mouseMoved,
                mouseCursorPosition: CGPoint(x: p.x + dx, y: screenH - p.y),
                mouseButton: .left)?.post(tap: .cghidEventTap)
        spin(0.06)
    }
}

func tooltipWindows() -> [NSWindow] {
    NSApp.windows.filter { String(describing: type(of: $0)).contains("ToolTip") && $0.isVisible }
}

var dumped = false
func tooltipText() -> String? {
    for w in tooltipWindows() {
        guard let root = w.contentView else { continue }
        if !dumped {
            dumped = true
            var stack = [(root, 0)]
            print("  [tooltip panel = \(String(describing: type(of: w)))]")
            while let (v, d) = stack.popLast() {
                print("  \(String(repeating: "  ", count: d))\(String(describing: type(of: v)))")
                for s in v.subviews { stack.append((s, d + 1)) }
            }
        }
        var stack = [root]
        while let v = stack.popLast() {
            if let f = v as? NSTextField, !f.stringValue.isEmpty { return f.stringValue }
            if let t = v as? NSText, !t.string.isEmpty { return t.string }
            for key in ["stringValue", "string", "displayString", "toolTipString", "title"]
            where v.responds(to: NSSelectorFromString(key)) {
                if let s = v.value(forKey: key) as? String, !s.isEmpty { return s }
                if let a = v.value(forKey: key) as? NSAttributedString, !a.string.isEmpty { return a.string }
            }
            if let s = v.accessibilityLabel(), !s.isEmpty { return s }
            if let s = v.accessibilityValue() as? String, !s.isEmpty { return s }
            stack.append(contentsOf: v.subviews)
        }
    }
    return nil
}

final class Rig {
    let window: NSWindow
    let hosting: NSHostingView<Cases>
    var snapshot: NSBitmapImageRep!
    var invalid = 0

    init() {
        hosting = NSHostingView(rootView: Cases())
        let size = hosting.fittingSize
        window = NSWindow(contentRect: NSRect(x: 200, y: 100, width: size.width, height: size.height),
                          styleMask: [.titled], backing: .buffered, defer: false)
        window.contentView = hosting
        window.acceptsMouseMovedEvents = true
        window.makeKeyAndOrderFront(nil)
        NSApp.activate(ignoringOtherApps: true)
    }

    /// The rendered HUE at a hosting-coordinate point — the sample normalised by its own max channel,
    /// so uniform dimming (from `.disabled()` or the capture's colour space) cannot change attribution.
    func hue(_ p: NSPoint) -> (Double, Double, Double)? {
        let sx = Double(snapshot.pixelsWide) / Double(hosting.bounds.width)
        let sy = Double(snapshot.pixelsHigh) / Double(hosting.bounds.height)
        guard let c = snapshot.colorAt(x: Int(p.x * sx), y: Int(p.y * sy)),
              let rgb = c.usingColorSpace(.deviceRGB) else { return nil }
        let m = max(rgb.redComponent, rgb.greenComponent, rgb.blueComponent)
        guard m > 0.05 else { return nil }
        return (rgb.redComponent / m, rgb.greenComponent / m, rgb.blueComponent / m)
    }

    func matches(_ got: (Double, Double, Double)?, _ want: (Double, Double, Double)) -> Bool {
        guard let got else { return false }
        let m = max(want.0, want.1, want.2)
        let w = (want.0 / m, want.1 / m, want.2 / m)
        let d = (got.0 - w.0) * (got.0 - w.0) + (got.1 - w.1) * (got.1 - w.1) + (got.2 - w.2) * (got.2 - w.2)
        return d.squareRoot() < 0.25
    }

    func probe(_ name: String, expect: (Double, Double, Double), hosting hp: NSPoint, screen p: NSPoint) {
        let window = self.window
        guard matches(hue(hp), expect) else {
            invalid += 1
            let g = hue(hp).map { String(format: "(%.2f,%.2f,%.2f)", $0.0, $0.1, $0.2) } ?? "black/nil"
            print("INVALID \(name) — hue at \(hp) is \(g), expected \(expect)")
            return
        }
        // Re-assert activation before every probe. Tooltip display is gated on the app being active and
        // the window key (docs/findings/0950-help-on-disabled-button.md § The activation gate measured
        // that an inactive, non-key host shows NOTHING — the enabled control included), so a probe run
        // while inactive produces a NONE that says nothing about the element under the cursor.
        DispatchQueue.main.sync {                       // AppKit activation is main-thread only
            window.makeKeyAndOrderFront(nil)
            NSApp.activate(ignoringOtherApps: true)
            NSRunningApplication.current.activate(options: [.activateAllWindows])
        }
        Thread.sleep(forTimeInterval: 0.6)
        var live = false
        DispatchQueue.main.sync { live = NSApp.isActive && window.isKeyWindow }
        guard live else {
            invalid += 1
            var a = false, k = false
            DispatchQueue.main.sync { a = NSApp.isActive; k = window.isKeyWindow }
            print("INVALID \(name) — host not active/key (active=\(a) key=\(k)); "
                  + "a NONE here would be the activation gate, not the element")
            return
        }
        hover(p)
        // Preconditions for a tooltip to be possible AT ALL. Without these a NONE is uninterpretable:
        // a blocked CGEvent post (no Accessibility permission ⇒ the cursor never moves) and an inactive
        // app both produce silence that looks exactly like "this element has no tooltip".
        let loc = NSEvent.mouseLocation
        let moved = abs(loc.x - p.x) < 6 && abs((screenH - loc.y) - p.y) < 6
        print("  [pre] active=\(NSApp.isActive) key=\(window.isKeyWindow) visible=\(window.isVisible) "
              + "cursorAtTarget=\(moved) cursor=(\(Int(loc.x)),\(Int(screenH - loc.y))) "
              + "target=(\(Int(p.x)),\(Int(p.y)))")
        var seen: String? = nil
        var any = false
        let deadline = Date().addingTimeInterval(6)     // generous: a NONE must not be impatience
        while Date() < deadline, !any {
            any = !tooltipWindows().isEmpty
            if any { seen = tooltipText() }
            spin(0.12)
        }
        print("RESULT \(name) -> \(any ? (seen ?? "<SHOWN, text unreadable>") : "NONE")")
        hover(NSPoint(x: 1700, y: 1000))
        spin(2.0)                                       // let the panel dismiss before the next probe
    }
}

setvbuf(stdout, nil, _IONBF, 0)                         // unbuffered: partial results survive a hang

/// The whole probe sequence, run on a background thread while the main thread stays in `NSApp.run()`.
func sequence(_ rig: Rig) {
    spin(2.0)
    DispatchQueue.main.sync {
        rig.hosting.layoutSubtreeIfNeeded()
        let rep = rig.hosting.bitmapImageRepForCachingDisplay(in: rig.hosting.bounds)!
        rig.hosting.cacheDisplay(in: rig.hosting.bounds, to: rep)
        rig.snapshot = rep
    }
    // CLEAN START — a cursor resting on a cell from a previous run contaminates the first probe.
    DispatchQueue.main.sync { hover(NSPoint(x: 1700, y: 1000)) }
    spin(3.0)
    var pre = 0, b = NSRect.zero, f = NSRect.zero, px = 0, py = 0
    DispatchQueue.main.sync {
        pre = tooltipWindows().count; b = rig.hosting.bounds; f = rig.window.frame
        px = rig.snapshot.pixelsWide; py = rig.snapshot.pixelsHigh
    }
    print("PRECONDITION tooltips=\(pre)  (must be 0)")
    print("hosting.bounds=\(b) window.frame=\(f) snapshot=\(px)x\(py)")

    let contentTop = screenH - (f.origin.y + f.size.height) + (f.size.height - b.size.height)
    // Row content spans hosting x 20…320: body/Spacer 20…234, glyph 242…272, chip slot 280…320 (the
    // chip's own glyph is trailing-aligned, so x=286 samples the slot's own background).
    let bodyX = 120.0, glyphX = 257.0, chipX = 286.0
    let order = ["R", "C", "B", "K", "E", "P", "N"]

    func go(_ name: String, _ row: String, _ region: String, expectRowTint: Bool = false) {
        let x = region == "body" ? bodyX : (region == "glyph" ? glyphX : chipX)
        let want = expectRowTint || region == "body" ? rowTints[row]!
                 : (region == "glyph" ? glyphTint : chipTint)
        let i = order.firstIndex(of: row)!
        rig.probe(name, expect: want,
                  hosting: NSPoint(x: x, y: 20 + Double(i) * 80 + 30),
                  screen: NSPoint(x: f.origin.x + x, y: contentTop + 20 + Double(i) * 80 + 30))
    }

    // NEGATIVE control first, so a spuriously-firing detector is caught before any case.
    go("N/chip  (neg. control, no help anywhere — expect NONE)", "N", "chip")
    // POSITIVE control for the rig: the #953 defect itself — the row's copy answering over the glyph.
    // If this reads NONE the rig cannot see tooltips and the whole run must be discarded.
    go("R/glyph (row help only — the #953 DEFECT)", "R", "glyph")
    // POSITIVE control for the mechanism: does a child `.help()` surface OUTSIDE a Button?
    go("P/chip  (child help, NO Button — expect CHIP-P)", "P", "chip")
    // THE QUESTION: does a child `.help()` surface INSIDE a Button?
    go("C/chip  (chip help, DISABLED Button)", "C", "chip")
    go("E/chip  (chip help, ENABLED Button)", "E", "chip")
    go("E/glyph (chip help, ENABLED Button — the glyph must stay silent)", "E", "glyph")
    // COMPOSITION: when both exist, which answers where?
    go("B/chip  (BOTH — CHIP-B, or shadowed by ROW-B?)", "B", "chip")
    go("B/body  (BOTH — expect ROW-B)", "B", "body")
    // The blocked-row shape: the row's help must still reach a `Color.clear` slot.
    go("K/slot  (row help over Color.clear — expect ROW-K)", "K", "chip", expectRowTint: true)

    print("INVALID probes: \(rig.invalid)  (an INVALID voids that probe, not the run)")
    DispatchQueue.main.async { NSApp.terminate(nil) }
}

final class Delegate: NSObject, NSApplicationDelegate {
    var rig: Rig!
    func applicationDidFinishLaunching(_ n: Notification) {
        rig = Rig()
        // Watchdog: never let a wedged rig hold the turn open. A truncated result set is readable;
        // a hang is not.
        DispatchQueue.main.asyncAfter(deadline: .now() + 210) {
            print("WATCHDOG fired — terminating with the results printed so far")
            NSApp.terminate(nil)
        }
        DispatchQueue.global().async { sequence(self.rig) }
    }
}

let app = NSApplication.shared
let delegate = Delegate()
app.delegate = delegate
app.setActivationPolicy(.regular)
app.run()
```

## Consequences for #953

- **AC-1 (scope the switch tooltip to the chip) — BLOCKED.** Not implemented. The mechanism it depends
  on is unverified, its failure mode is silent, and the one available signal points at failure.
- **AC-2 (the health glyph's tooltip-less state) — settled and recorded**, independently of this
  question: #955 decided it deliberately, and the decision now lives at `authView` in
  `apps/menubar/Sources/StatusPanelRoster.swift`. The build reference agrees — `title=` sits on
  `.rowact` and on **zero** of the 78 `.health` spans in `design/menubar-preview.html`.
- **AC-3 (the row body still explains itself) — currently satisfied by the status quo**, since the
  switch copy has not moved off the row. The non-target rows' silence is now a recorded decision at the
  `else` branch. Should AC-1 ever land, AC-3 needs re-reading: scoping the invitation to the chip leaves
  a viable row's *body* silent, and that absence has to be argued or a fallback kept.
- **The #950 residual is inherited and still open.** Whether **any** tooltip surfaces in the shipped
  panel's `panelIsKey=true, appIsActive=false` presentation is capture-pending. Nothing load-bearing may
  ride the tooltip channel — which is why #955 made the blocked-row reason persistent on-screen text.

## Provenance

Method: purpose-built minimal SwiftUI harnesses (12 iterations, `.tmp/tooltip-probe/`), macOS 26.5.2
(25F84), Xcode 26.6, Apple Silicon, single 1920×1080 display, 2026-08-04. Synthetic `CGEvent`
mouse-moved hovers; detection by a visible in-process `NSToolTipPanel`. A negative control (no `.help()`)
and a positive control (the #953 defect) rode in every hover run.

Boundary: **measured** — `.help()` does not materialize as `NSView.toolTip`, and the hosting view does
not respond to the tooltip-owner selector, on macOS 26.5.2. **Not measured** — whether a nested
`.help()` surfaces on hover (the question this finding was opened to answer); macOS 13.0, the deployment
target, on any question here; and the shipped panel's own activation state at hover time (inherited from
#950). The `probe3` table is **suggestive, not measured**, for the reasons above.

No credentials read, no network call, no daemon state consulted. Related: #950 (`.help()` on a disabled
Button), #955 (affordance coverage; the auth-glyph decision), #959 (the blocked row's chip).

macOS 26.5.2 · sessiometer #953.
