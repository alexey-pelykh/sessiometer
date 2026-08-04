# Finding #953 — does a `.help()` on a child inside a row `Button` surface on hover?

**Verdict: ANSWERED — YES**, superseding this document's original *NOT ESTABLISHED*. A `.help()` on a
child inside a row `Button` **does** surface on hover, and it is correctly scoped to that child's rect:
the child answers, and its siblings inside the same `Button` stay silent. The fix it gated (issue #953
AC-1) shipped on that measurement.

**But the result that actually shaped the fix is the second one: a row-level `.help()` WINS over a
child's.** With both attached, the ROW's copy answers everywhere — *including over the chip*. So the
prescribed fix could never have been "scope one to the chip and keep a row-level fallback": that is not a
belt-and-braces version of #953, it is the #953 defect rebuilt, with the chip's tooltip dead and the row
answering for every element again. The invitation had to **move**, not be added. `switchChipHelp` /
`switchRowHelp` return `String?` and are asserted mutually exclusive for exactly this reason.

Three further behaviours were measured because the fix rests on each, and each fails silently if assumed
wrong:

| Assumption | Measured | Consequence for the fix |
|---|---|---|
| `.help("")` is an inert way to say "no tooltip" | **False** — an empty string still registers an owner and still wins, killing the chip's tooltip | the modifier must be applied **conditionally**, never with `""` |
| `.disabled()` on the `Button` suppresses a nested child's tooltip | **False** — the chip still answers | a swap-pending row keeps its chip tooltip |
| `.accessibilityHidden(true)` on the enclosing group suppresses the tooltip (`.help()` also sets an AX attribute) | **False** — the tooltip is unaffected | the chip stays a11y-hidden; the row remains ONE VoiceOver element |

## Why this mattered more than it looked

The switch invitation (`switchHelpText`) was attached to the row-wrapping `Button`, which is why hovering
the **health glyph** answered with the *switch* copy — the #953 defect. The prescribed fix moved that copy
onto the chip. Had a nested `.help()` not surfaced, that move would not have narrowed the tooltip's scope,
it would have **deleted** the tooltip — and the failure mode is silent: no crash, no failing test, no
golden moves (a tooltip is a hover affordance and the goldens render at `.idle`), and the panel looks
correct in every static capture. Nothing in CI would have caught it. That is why the question gated the
fix rather than being resolved by trying it.

## The answer, and the rig that produced it

Measured 2026-08-04, macOS 26.5.2, Xcode 26.6 (17F113), Swift 6.3.3. Eight runs across three harnesses
(`probe8` ×3, `probe9` ×3 incl. two panel variants, `probe10` ×2); the load-bearing rows were identical in
every one. Cases: **R** row-level help only (the shipped shape, and the positive control), **C** chip-level
only (the question), **B** both, **N** none (negative control).

| Case | body | health glyph | chip |
|---|---|---|---|
| **R** — row help only | SHOWN (row copy) | **SHOWN (row copy)** — the defect | SHOWN (row copy) |
| **C** — chip help only | NONE | **NONE** | **SHOWN (chip copy)** |
| **B** — both | SHOWN (row copy) | SHOWN (row copy) | **SHOWN (ROW copy)** — the chip loses |
| **N** — neither | NONE | NONE | NONE |

**What made this rig work where five earlier ones failed** — two changes, both structural:

1. **Detection is not pixels.** A macOS tooltip is a real `NSWindow` owned by the app, so the probe reads
   `CGWindowListCopyWindowInfo` for an on-screen window belonging to its own PID other than the main
   window. That is deterministic, and it also returns the tooltip's **bounds** — which is what let the
   *precedence* question be answered at all: the two help strings were given very different lengths, so
   the tooltip's width names its owner (239 pt for the long row copy vs 38 pt for the short chip copy).
   Every earlier rig tried to read a translucent tooltip out of a screen capture and drowned in
   colour-space and hit-test problems.
2. **Geometry that cannot be wrong.** The window is sized to `hosting.fittingSize`, so the hosting view's
   bounds *equal* the content rect — this is `probe3`'s recorded failure, fixed at the source rather than
   compensated for. Each landing is still verified against the live cursor position (`CGEvent(source:)`)
   before its reading is trusted.

The host reported `active=true key=true visible=true` on every reported run, and the negative control read
NONE on every one, so no run needed discarding under #950's rule.

**This refutes the `probe3` signal recorded below** (which found a nested `.help()` surfacing nowhere).
That is the expected outcome, not a surprise: this document already recorded `probe3`'s geometry as
unvalidated and its table as *suggestive, not measured*. The suggestive signal was wrong.

**One residual stands, unchanged.** The shipped panel's `panelIsKey=true, appIsActive=false` combination
(`StatusItemController.swift:296-307`) still could not be constructed: an `.accessory` activation policy
with a `.nonactivatingPanel`, ordered front regardless and then sent `makeKey()`, reported
`appIsActive=true` — including when activation was handed to Finder first. Three attempts, all reporting
the same. So whether *any* tooltip surfaces in the shipped presentation remains capture-pending, exactly
as `docs/findings/0950-help-on-disabled-button.md` left it. It does **not** gate this fix: the residual
applies identically to the row-level tooltip that shipped before and the chip-level one that shipped
after, so it cannot discriminate between them. Nothing load-bearing rides the tooltip either way — the
blocked row's reason is on-screen at rest (`switchBlockedCue`, #955) and spoken
(`rowSwitchAccessibilityLabel`).

## What was measured, and what was not

**Not the shipped panel.** This is a minimal SwiftUI harness, as #950's was — the same substitute, for
the same reason: no roster fixture with a populated `blockReason` exists, and reproducing one live needs
specific daemon state. What generalises is the platform behaviour; the roster's own wiring is not
re-proven here.

Every run carried a **negative control** (a row with no `.help()` anywhere) and a **positive control**
(the #953 defect itself — row-level help answering over the health glyph). Per #950's rule, a run whose
positive control reads NONE is discarded rather than reported: it cannot distinguish "this element has
no tooltip" from "this rig cannot see tooltips".

### The one run that produced a signal — since REFUTED

> **Superseded.** This table was the reason the fix was blocked, and it was **wrong**: `C/chip` reads
> SHOWN in the validated runs (§ The answer). It is kept because it is the record of *how* an unvalidated
> rig produces a confident, wrong, decision-shaping answer — the failure this document exists to describe.
> Do not cite it as evidence.

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

## What closed this

Both things this section previously asked for, plus one it did not think to ask for.

1. **A host that becomes active and key** — supplied by running as an `.app` bundle, as this section
   already prescribed.
2. **A confirmed synthetic cursor landing** — supplied, and it is now asserted per probe from
   `CGEvent(source:)?.location` before any reading is trusted. Every reported landing read LANDED. This is
   what the earlier `.app`-bundle run could not show (its cursor sat parked at rest), and the difference
   was environmental rather than a code fix: the session running this is an **Aqua session with
   `HasGraphicAccess` but `IsOnConsole: False`** (switched out via fast user switching), so it owns a
   window server and a cursor *independent of the physical mouse*. Warping that cursor is unobstructed and
   does not disturb the console user.
3. **A detection channel that is not pixels** — the one this section did not anticipate needing, and the
   change that made the *precedence* question answerable at all. See § The answer above.

The rig inlined below is the one that worked, replacing the earlier non-working one; the narrative of the
five rigs that failed is kept above, because knowing why each *looks* like it works is the expensive part.

## Reproducing this

Self-contained: no project build, no daemon, no fixtures. This is the rig that produced § The answer —
`probe10`, which carries every case including the three silent-failure assumptions. Needs Accessibility
permission (to post synthetic mouse-moved events) for whatever runs it.

Run it as an **`.app` bundle**, not as a bare binary — a bare binary does not become active or key here,
and per #950 § The activation gate an inactive, non-key host shows no tooltip for any case:

```sh
swiftc -O probe10.swift -o probe10
mkdir -p Probe10.app/Contents/MacOS && cp probe10 Probe10.app/Contents/MacOS/Probe10
cat > Probe10.app/Contents/Info.plist <<'PLIST'
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
  <key>CFBundleExecutable</key><string>Probe10</string>
  <key>CFBundleIdentifier</key><string>com.example.tooltipprobe</string>
  <key>CFBundleName</key><string>Probe10</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>NSPrincipalClass</key><string>NSApplication</string>
</dict></plist>
PLIST
./Probe10.app/Contents/MacOS/Probe10 "$PWD/out.txt"      # ~60s; results also stream to stderr
```

**How to read a run.** Per #950's rule, check the controls FIRST and discard the whole run if either
fails — a rig that cannot see tooltips reports NONE for everything, which is indistinguishable from a
real absence:

- `HOST` must read `appIsActive=true key=true visible=true`.
- Every line must read `cursor=LANDED`. A `PARKED` line is INVALID, not an absence.
- **Positive control**: `R/glyph` must be SHOWN (that is the #953 defect itself).
- **Negative control**: all three `N/` lines must be NONE.

Then the answers are `C/chip` (does a nested `.help()` surface — SHOWN means yes), `B/chip` (precedence —
the tooltip WIDTH names the winner, wide=row, narrow=chip), `E/chip` (is `.help("")` inert — NONE means
no, it still wins), `D/chip` (does `.disabled()` suppress a nested help), and `H`/`G` (does an
accessibility-hidden enclosing group suppress the tooltip, for either placement).

```swift
// probe10 — the two assumptions the #953 fix would otherwise rest on untested.
//
// E: row `.help("")` + chip `.help("CHIP")`. Applying `.help` unconditionally with an empty string is
//    the tidiest way to write "row help only when blocked". But probe9 proved a ROW-level help WINS
//    over a chip's, so if an EMPTY row help still registers an owner it wins and shows nothing — the
//    chip tooltip would vanish, silently. If E/chip reads CHIP-help, `.help("")` is inert and the tidy
//    form is safe; if it reads NONE, the modifier must be applied conditionally.
//
// D: `.disabled(true)` on the Button + chip `.help("CHIP")`, no row help. A swap-pending row IS
//    disabled and DOES still render a chip, so whether a nested help survives disabling is load-bearing.
//    (#950 answered this for ROW-level help; a nested child is a different question.)
//
// R / C / B / N carry over from probe8-9 unchanged as controls.

import AppKit
import SwiftUI

let ROW_HELP  = "ROW-tooltip-deliberately-very-long-so-its-width-is-unmistakable"
let CHIP_HELP = "CHIP"

// H / G mirror the REAL switchSlot: the chip lives in a Group carrying `.frame(width: 28)` and
// `.accessibilityHidden(true)`. `.help()` also sets an AX help attribute, so whether a11y-hiding the
// enclosing Group also kills the TOOLTIP is a third assumption worth measuring, not asserting.
//   H = .help() on the inner glyph (hit rect = the glyph, most faithful to the mock's chip span)
//   G = .help() on the outer Group (hit rect = the full 28pt slot)
let CASES = ["R", "C", "B", "N", "E", "D", "H", "G"]
let rowH: CGFloat = 60

struct RowView: View {
    let kase: String
    var rowHelp: String? {
        switch kase {
        case "R", "B": return ROW_HELP
        case "E":      return ""          // the assumption under test
        default:       return nil
        }
    }
    var chipHelp: String? { (kase == "C" || kase == "B" || kase == "E" || kase == "D") ? CHIP_HELP : nil }
    var disabled: Bool { kase == "D" }

    var body: some View {
        let btn = Button(action: {}) {
            HStack(spacing: 0) {
                Color(nsColor: bodyColor).frame(width: 200, height: rowH)
                Color(nsColor: glyphColor).frame(width: 100, height: rowH)
                chip
            }
        }
        .buttonStyle(.plain)
        .disabled(disabled)
        if let rowHelp { btn.help(rowHelp) } else { btn }
    }

    @ViewBuilder var chip: some View {
        let c = Color(nsColor: chipColor).frame(width: 120, height: rowH)
        switch kase {
        case "H":
            // .help() INSIDE an accessibility-hidden, width-framed Group — the real switchSlot shape.
            Group { c.help(CHIP_HELP) }
                .frame(width: 120, alignment: .trailing)
                .accessibilityHidden(true)
        case "G":
            Group { c }
                .frame(width: 120, alignment: .trailing)
                .accessibilityHidden(true)
                .help(CHIP_HELP)
        default:
            if let chipHelp { c.help(chipHelp) } else { c }
        }
    }

    var idx: Int { CASES.firstIndex(of: kase)! }
    var bodyColor: NSColor  { NSColor(calibratedHue: CGFloat(idx) * 0.16 + 0.00, saturation: 0.95, brightness: 0.95, alpha: 1) }
    var glyphColor: NSColor { NSColor(calibratedHue: CGFloat(idx) * 0.16 + 0.04, saturation: 0.95, brightness: 0.95, alpha: 1) }
    var chipColor: NSColor  { NSColor(calibratedHue: CGFloat(idx) * 0.16 + 0.08, saturation: 0.95, brightness: 0.95, alpha: 1) }
}

struct Root: View {
    var body: some View { VStack(spacing: 0) { ForEach(CASES, id: \.self) { RowView(kase: $0) } }.frame(width: 420) }
}

final class Probe: NSObject, NSApplicationDelegate {
    var window: NSWindow!
    var mainWindowNumber: CGWindowID = 0
    var results: [String] = []

    func applicationDidFinishLaunching(_ n: Notification) {
        UserDefaults.standard.register(defaults: ["NSInitialToolTipDelay": 150])
        let hosting = NSHostingView(rootView: Root())
        window = NSWindow(contentRect: NSRect(origin: .zero, size: hosting.fittingSize),
                          styleMask: [.titled], backing: .buffered, defer: false)
        window.contentView = hosting
        window.setFrameOrigin(NSPoint(x: 200, y: 150))
        window.makeKeyAndOrderFront(nil)
        NSApp.activate(ignoringOtherApps: true)
        mainWindowNumber = CGWindowID(window.windowNumber)
        Thread.detachNewThread { [weak self] in self?.run() }
    }

    func auxWindows() -> [[String: Any]] {
        let pid = ProcessInfo.processInfo.processIdentifier
        guard let list = CGWindowListCopyWindowInfo([.optionOnScreenOnly, .excludeDesktopElements],
                                                    kCGNullWindowID) as? [[String: Any]] else { return [] }
        return list.filter {
            ($0[kCGWindowOwnerPID as String] as? Int32) == pid &&
            ($0[kCGWindowNumber as String] as? CGWindowID) != mainWindowNumber
        }
    }

    func moveCursor(to p: CGPoint) {
        for pt in [CGPoint(x: p.x - 6, y: p.y - 6), p] {
            CGWarpMouseCursorPosition(pt)
            CGAssociateMouseAndMouseCursorPosition(1)
            CGEvent(mouseEventSource: nil, mouseType: .mouseMoved, mouseCursorPosition: pt, mouseButton: .left)?
                .post(tap: .cghidEventTap)
            usleep(60_000)
        }
    }

    func emit(_ s: String) { FileHandle.standardError.write((s + "\n").data(using: .utf8)!); results.append(s) }

    func run() {
        usleep(900_000)
        var state = "?"
        DispatchQueue.main.sync {
            state = "appIsActive=\(NSApp.isActive) key=\(self.window.isKeyWindow) visible=\(self.window.isVisible)"
        }
        emit("HOST \(state)")

        var frame = NSRect.zero
        DispatchQueue.main.sync { frame = self.window.frame }
        let screenH = CGDisplayBounds(CGMainDisplayID()).height

        for (i, kase) in CASES.enumerated() {
            for (regionName, xOff) in [("body", CGFloat(100)), ("glyph", CGFloat(250)), ("chip", CGFloat(360))] {
                let cocoaY = frame.maxY - (CGFloat(i) * rowH + rowH / 2) - 28
                let pt = CGPoint(x: frame.minX + xOff, y: screenH - cocoaY)
                moveCursor(to: CGPoint(x: frame.minX - 120, y: screenH - frame.maxY - 60))
                usleep(450_000)
                moveCursor(to: pt)
                usleep(1_300_000)

                var cursorNow = CGPoint.zero
                var aux: [[String: Any]] = []
                DispatchQueue.main.sync {
                    cursorNow = CGEvent(source: nil)?.location ?? .zero
                    aux = self.auxWindows()
                }
                let landed = abs(cursorNow.x - pt.x) < 3 && abs(cursorNow.y - pt.y) < 3
                let w = (aux.first?[kCGWindowBounds as String] as? [String: Any])
                    .map { Int($0["Width"] as? Double ?? 0) } ?? 0
                let whose = aux.isEmpty ? "—" : (w > 150 ? "ROW-help" : "CHIP-help")
                emit("\(kase)/\(regionName)\tcursor=\(landed ? "LANDED" : "PARKED")\ttooltip=\(aux.isEmpty ? "NONE" : "SHOWN")\twidth=\(w)\twinner=\(whose)")
            }
        }
        try? (results.joined(separator: "\n") + "\n")
            .write(toFile: CommandLine.arguments[1], atomically: true, encoding: .utf8)
        DispatchQueue.main.async { NSApp.terminate(nil) }
    }
}

let app = NSApplication.shared
app.setActivationPolicy(.regular)
let d = Probe()
app.delegate = d
app.run()
```


## Consequences for #953

- **AC-1 (scope the switch tooltip to the chip) — DELIVERED**, on the measurement above. The invitation
  MOVED from the row-wrapping `Button` to the chip's slot (`switchSlot`); the row keeps a tooltip only
  when blocked, where since #959 there is no chip to carry one. The routing is
  `StatusPanelFormat.switchChipHelp` / `switchRowHelp`, and their mutual exclusivity — the platform fact,
  not a preference — is asserted by `testTheChipAndTheRowNeverBothClaimTheTooltip`.
- **AC-2 (the health glyph's tooltip-less state) — settled and recorded**, independently of this
  question: #955 decided it deliberately, and the decision now lives at `authView` in
  `apps/menubar/Sources/StatusPanelRoster.swift`. The build reference agrees — `title=` sits on
  `.rowact` and on **zero** of the 78 `.health` spans in `design/menubar-preview.html`. AC-1 landing
  *strengthens* this: the glyph no longer answers with the switch copy either, so it is now silent in
  fact and not merely un-annotated.
- **AC-3 (the row body still explains itself) — satisfied as a RECORDED DECISION, not as a fallback.**
  A viable row's body is now silent, and that is deliberate: the measurement forecloses the alternative,
  because a row-level fallback would win over the chip and re-create the defect. The spec's bar is "some
  tooltip appears, or the absence is a recorded decision; but not silently nothing"
  (`docs/specs/tooltip-scope.feature.md`, Cap-3.1), and the absence is argued at `switchButton`: the row
  is fully self-describing at rest, the chip is the affordance being described, the mock authors exactly
  this scoping, and the spoken channel is *unchanged* — `.accessibilityHint` still carries the invitation
  on the row, so VoiceOver loses nothing.
- **The #950 residual is inherited and still open**, and three further attempts to construct
  `panelIsKey=true, appIsActive=false` failed (§ The answer). Nothing load-bearing may ride the tooltip
  channel — which is why #955 made the blocked-row reason persistent on-screen text. The residual does
  not discriminate between the old shape and the new one, so it did not gate this fix.

## Provenance

Method: purpose-built minimal SwiftUI harnesses, macOS 26.5.2 (25F84), Xcode 26.6 (17F113), Swift 6.3.3,
Apple Silicon, single 1920×1080 display, 2026-08-04. Twelve early iterations under `.tmp/tooltip-probe/`
established only that the question needed a live hover; the answer came from `probe8` / `probe9` /
`probe10` (eight runs), which detect the tooltip as an **`NSWindow` via `CGWindowListCopyWindowInfo`**
rather than by reading pixels, and validate every cursor landing before trusting its reading. A negative
control (no `.help()`) and a positive control (the #953 defect) rode in every hover run.

Boundary: **measured** — that a nested `.help()` surfaces and is scoped to its child; that a row-level
`.help()` takes precedence over a child's; that `.help("")` still registers an owner; that `.disabled()`
and `.accessibilityHidden(true)` suppress neither; and that `.help()` does not materialize as
`NSView.toolTip` nor make the hosting view respond to the tooltip-owner selector. All on macOS 26.5.2,
in a **switched-out Aqua session** (`HasGraphicAccess` true, `IsOnConsole` false) — a caveat worth
carrying, since that session owns a window server and cursor of its own.

**Not measured** — macOS 13.0, the deployment target, on any question here (every reading is from 26.5.2,
and `.help()` precedence is not a documented API contract, so a future OS may differ); the shipped
panel's own activation state at hover time (inherited from #950, three further attempts failed); and the
roster's own wiring, which is asserted by unit test rather than by hover. The `probe3` table below is
**suggestive, not measured**, and has been **refuted** by the runs above.

No credentials read, no network call, no daemon state consulted. Related: #950 (`.help()` on a disabled
Button), #955 (affordance coverage; the auth-glyph decision), #959 (the blocked row's chip).

macOS 26.5.2 · sessiometer #953.
