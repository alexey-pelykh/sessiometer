# menubar spikes — throwaway reference code

Code here is **throwaway spike output** (knowledge acquisition), **not** part of the
app. `project.yml` lists only `Sources` and `Tests` as build sources, so `xcodegen`
/ `xcodebuild` never compile anything under `spikes/` — it will not enter the app or
the CI `swift` job's build graph.

## `watch_spike.swift` + `stub_daemon.py` — issue #321

De-risks the Swift↔daemon **AF_UNIX transport** before WI-2 (**#323**,
`WatchTransport`). The decision it produced is durable in
[`docs/adr/0011-menubar-transport-raw-posix-af-unix.md`](../../../docs/adr/0011-menubar-transport-raw-posix-af-unix.md)
— **read the ADR first**; this code is the reference `WatchTransport` adapts.

It proves, on pure `Darwin`/`Foundation`/`os` Swift (no Network.framework, no Rust
FFI):

1. raw POSIX `socket(AF_UNIX, SOCK_STREAM)` / `connect()` to the daemon's control
   socket, `{"cmd":"watch"}\n` subscribe;
2. an `EINTR`- and partial-read-safe newline line-reader → decode via the **real**
   `Sources/WireModel.swift` (#322 decoder, reused verbatim);
3. bridging the blocking `read()` loop into an `AsyncStream` on a dedicated `Thread`;
4. socket-path resolution — `getpwuid(getuid())->pw_dir` == `NSHomeDirectory()` ==
   the daemon's `src/paths.rs::control_socket()` (the non-sandboxed invariant).

### Build & run (standalone `swiftc` — no Xcode needed)

```sh
# from the repo root
swiftc -O apps/menubar/spikes/watch_spike.swift apps/menubar/Sources/WireModel.swift \
  -o .tmp/spike-run/watch_spike

# 1) path-resolver cross-check only (no socket)
.tmp/spike-run/watch_spike --self-check

# 2) against the golden-fixture stub, forcing a partial read across two read()s
python3 apps/menubar/spikes/stub_daemon.py --socket "$PWD/.tmp/k.sock" --serve 2 --chunked &
.tmp/spike-run/watch_spike --socket "$PWD/.tmp/k.sock"

# 3) force + observe an EINTR retry (stub delays; a SIGALRM interrupts the read)
python3 apps/menubar/spikes/stub_daemon.py --socket "$PWD/.tmp/k.sock" --serve 2 --delay 0.2 &
.tmp/spike-run/watch_spike --socket "$PWD/.tmp/k.sock" --eintr

# 4) against the LIVE daemon (path derived by the resolver; redact real labels)
.tmp/spike-run/watch_spike --redact
```

`stub_daemon.py` stands in for `src/daemon/socket.rs::serve_watch`, vending the
**byte-exact** `snapshotBasic` / `heartbeatBasic` frames from
[`../Tests/Fixtures.swift`](../Tests/Fixtures.swift). It also documents how #323 can
test `WatchTransport` **without** a live daemon (and without touching any credential —
the #209 boundary). Use a scratch socket under the repo `.tmp/` — **never** `/tmp`.

### Findings (full rationale in ADR-0011)

- **Transport**: raw POSIX **confirmed** over `NWConnection` — ~180 lines, zero
  network egress, egress-provably socket-only.
- **Path**: the non-sandboxed app resolves the **same** native-local path the daemon
  binds; App Sandbox would diverge (`NSHomeDirectory()` → container) — the app **must
  stay non-sandboxed**.
- **⚠ Surprise**: the daemon build running at spike time **predated #164/#165** (no
  `watch`, no `schema_version`) — so #323 must treat `{"error":…}` / unknown-only
  streams as **watch-unavailable → degrade**, not hang. `watch` is **not** auth-gated.

## `uitest/StatusItemUISpike.swift` — issue #761

Answers one time-boxed question: **can a minimal XCUITest reliably open the panel and
assert one element, on a developer machine and on a `macos-latest` runner?**

**Measured answer on `macos-latest`: yes — 20/20 runs, 0 % flake, no TCC grant needed.** The
developer-machine half is **unanswered**, not answered negatively — no valid local run was
obtained (below). The go/no-go that this measurement feeds — including the parts XCUITest
still cannot see — is recorded on **issue #761**. This section is the evidence, and the
record of two wrong turns taken to reach it.

### What was measured

Run [`30350129101`](https://github.com/alexey-pelykh/sessiometer/actions/runs/30350129101)
— `macos-latest` = macOS 26.4 / Xcode 26.5, **20 consecutive runs**, with a stub daemon on
the watch socket so the panel had a populated roster:

| Probe | passed | failed |
|---|---:|---:|
| A — the `LSUIElement` app launches under XCUITest | **20** | 0 |
| B — the app's OWN `statusItems` is reachable and correctly labelled | **20** | 0 |
| C — reachable via `XCUIApplication(bundleIdentifier: "com.apple.systemuiserver")` | 0 | **20** |
| D — click the item, then assert one element of the panel | **20** | 0 |
| E — with a populated roster, inspect the interaction surfaces | **20** | 0 |

**Flake rate: 0 %.** Every probe returned the same verdict on all 20 runs, twice over
(rounds 2 and 3). Whatever else is true, this lane is not flaky — which is the opposite of
what the issue expected to find.

The job's own headline reads `SPIKE761_RUNS=20 SPIKE761_PASS=0 SPIKE761_FAIL=20`, which
looks like it contradicts the table. It does not: probe C is an *intentional* expected-fail
documenting that the issue's predicted SystemUIServer path does not work, and it shares a
bundle with the rest, so every `xcodebuild test` invocation exits non-zero. The per-probe
tally is the meaningful one; the run-level tally is an artifact of keeping a known-negative
probe in the suite.

Two controls make the tally readable:

- `SPIKE761_CONTROL=green` — the existing headless `MenubarTests` suite passes on the
  **same runner in the same job**, so a red probe is specific to XCUITest.
- `SPIKE761_CANARY=OK` — a known-failing and a known-passing command driven through the
  *exact* shell predicate the attempts use.

### The panel is richly addressable

The full tree, verbatim, with a two-account roster on screen (probe E, **run 1** — the
job dumps one run's tree, and a `pid` / pointer-bearing dump could not be identical across
20 fresh launches anyway). What *was* identical on all 20 runs is the element-count
signature: `staticTexts=2 buttons=3 checkBoxes=0 images=0 cells=0 groups=1`.

```
 →Application, 0x7a6da75c0, pid: 2933, title: 'Sessiometer', Disabled
    Dialog, 0x7a6da7700, {{587.0, 36.0}, {380.0, 293.0}}, Keyboard Focused, Disabled
      Group, 0x7a6da7840, {{587.0, 36.0}, {380.0, 293.0}}, Disabled
        StaticText, 0x7a6da7e80, {{601.0, 48.0}, {200.0, 31.0}}, value: Sessiometer. 2 acc...
        Button, 0x7a6da57c0, {{849.0, 54.0}, {54.0, 19.0}}, label: 'Status', Selected
        Button, 0x7a6da5180, {{905.0, 54.0}, {46.0, 19.0}}, label: 'Stats'
        Other, 0x7a69b4000, {{595.0, 97.0}, {364.0, 94.0}}, label: 'work, active, auth healthy, session 60% resets in n/a, weekly 10% resets in n/a'
        Button, 0x7a69b4140, {{595.0, 193.0}, {364.0, 94.0}}, label: 'personal, auth healthy, session 20% resets in n/a, weekly 10% resets in n/a'
        StaticText, 0x7a69b4280, {{587.0, 294.0}, {380.0, 24.0}}, value: updated 20662d10h ...
    TouchBar, 0x7a69b43c0, {{80.0, 0.0}, {685.0, 30.0}}, Disabled
    MenuBar, 0x7a69b4500, {{757.0, 3.0}, {40.0, 24.0}}
      StatusItem, 0x7a69b4640, {{757.0, 3.0}, {40.0, 24.0}}, label: 'Sessiometer: live — 2 accounts'
```

What that buys, concretely:

- **The Status / Stats tabs are addressable and carry selection state** (`Selected` on
  `Status`) — so a tab-switch flow is assertable.
- **The switchable row is a `Button`; the ACTIVE row is an `Other`.** So the
  interactive-vs-non-interactive binary is directly assertable, and `.disabled()` +
  `rowSwitchAccessibilityLabel(base:block:)` carry the block reason with it.

  Be precise about what this is *not*, because it is tempting to over-read. Issue #766's
  mis-click guard is a **width** axis — "a too-narrow row must become non-interactive rather
  than degrade into an invisible whole-row hot zone", verified "at the interaction layer, not
  only as a predicate". What was measured here is the **active/non-active** axis, with short
  labels in a 380 pt panel; the narrow-row case was never exercised. And
  `RowSwitchButtonStyle`'s own comment says the guard is delivered by *arming* — the hit rect
  is the whole row, made safe only once hover adds the wash and cursor — which is exactly the
  half XCUITest cannot see (below). Related, not identical.
- Header and footer publish their combined strings, and the status item's own label tracks
  state (`live — 2 accounts`).

Note the snapshot the stub vends carries `generated_at: 42`, so the panel renders it
**stale** ("updated 20662d10h ago"). The roster still renders in full, so this does not
affect the reachability findings — but nothing here was measured against a *fresh* snapshot.

### What XCUITest still cannot see — and this is the load-bearing limit

**The armed / hover state is invisible to it.** Source-verified, not inferred:
`StatusPanelRoster.swift:216` sets `@State isHovering` from `.onHover`, and it drives
**three** signals — the row background wash (`RowSwitchButtonStyle`, 0 at rest → 0.08
hovered), the chip tint (`StatusPanelFormat.switchChipEmphasis` → `.resting` / `.armed`, which
`StatusPanelRoster.swift:367`/`:369` render as `.tertiary` → `.secondary`), and the
`pointingHand` cursor (`:244`, `setCursor(pushed: isHovering && isLiveSwitch)`).

Not one of the three is an accessibility attribute. The row's `accessibilityLabel` is
`rowSwitchAccessibilityLabel(base:block:)` and its `accessibilityHint` keys off
`blockReason`; neither reads `isHovering`, and `RowSwitchButtonStyle.makeBody` carries no
accessibility modifier at all. So the surface issue #766 leads with — *"the armed brighten on
hover / focus … nothing verifies the rendered difference"* — is exactly the one XCUITest
cannot verify either, and that holds for all three of its signals, not just the tint.

By contrast the **in-flight** state *is* reachable: `StatusPanelChrome.swift:236` swaps the
label to `"Switching to \(target)"`, so it is assertable — though reaching it means driving
a real `swap`, which is only safe against a stub.

Also still out of reach, and already covered better elsewhere: **truncation and overflow**
(XCUITest reads the semantic string, so a visually-elided `Wor…rk-2` still reports in full —
issue #750's `PanelTextMetricsTests` measures this directly, headless, with no oracle), and
**appearance drift** (issue #754's 34 panel goldens).

### What the real suite would need (the issue's fourth Method item)

The issue asks this because its GO branch files a suite "with the identifier work as a
prerequisite". The measurement makes the answer concrete, and it is not "none":

`Sources/` still has **zero** `accessibilityIdentifier`s against 22 applications of
`.accessibilityLabel(` / `.accessibilityElement(`, so **every query above keys off a display
string** — and the strings are long, composed, and content-derived. Probe D had to match
`value BEGINSWITH 'Sessiometer.'`, and a roster row publishes a whole sentence:
`'work, active, auth healthy, session 60% resets in n/a, weekly 10% resets in n/a'`. That is
copy-coupling of the worst kind: the assertions are pinned to the exact prose the panel
renders, so a wording change in `StatusPanelFormat` breaks tests that were not testing
wording, while a *real* regression that preserves the sentence slips through.

Note also the trap round 2 fell into: SwiftUI publishes a combined `Text` element's string as
**`value`**, not `label`, while `NSStatusItem` and `Button` use `label`. A suite keying off
display strings has to get that right per element type, with nothing to catch it but a red
test.

So the identifier work is a genuine prerequisite, and a bounded one: stable
`accessibilityIdentifier`s on the surfaces a suite would actually drive — the Status/Stats
tabs, each roster row, the swap/Start buttons, and the Settings controls — not the repo-wide
sweep issue #761 lists as out of scope. Everything else (glyphs, meters, sparklines) is
already `accessibilityHidden` by design and needs none.

### Two of the issue's premises did not survive measurement

1. **"Needs Accessibility TCC granted to the test runner… this repo has already hit the TCC
   wall."** On `macos-latest`, **TCC is not the blocker**: SIP is *disabled*,
   `AXIsProcessTrustedWithOptions` returns `true`, the session is present, unlocked and
   on-console, and the runner executed every test on all 20 runs.
2. **"Reached via `XCUIApplication(bundleIdentifier: "com.apple.systemuiserver")`."** Wrong
   path — 0/20. A modern `NSStatusItem`'s button lives in a window owned by the creating
   process, so it comes through the app's own tree (probe B, 20/20).

A third, **"`NSPopover` auto-dismisses on focus loss — a classic flake source"**, is stale
twice over: the panel has been a borderless `.nonactivatingPanel` `NSPanel` dismissed by an
`NSEvent` global monitor since `StatusItemController` moved off `NSPopover`, and no flake
appeared at all. `design/README.md:38` repeats the same stale claim — tracked as issue #808,
deliberately not fixed here.

### Two wrong turns, recorded because they are the transferable part

**Round 1 shipped a gate that could not fail.** The probe reported `PASS PASS PASS` in ~20 ms
on a subject that never ran: macOS has no GNU `timeout`, and without `set -o pipefail` the
`if … | tee` tested `tee`'s exit status. Fixed with a perl `alarm` bound and a canary that
drives a known-failing and known-passing command through the same predicate.

**Round 2 then shipped a NO-GO built on three of my own errors** — the more instructive failure:

- The "decisive artifact" was an artifact of my own `grep`. The tree was reconstructed with a
  filter that did not include `StaticText`, and the result was published as *"the entire
  accessibility tree"*. The header was in the real tree the whole time. A filtered corpus
  reported as a complete one is not evidence of absence.
- The probe asked for the header by `label`, but SwiftUI publishes a combined `Text`
  element's string as **`value`**. One wrong attribute produced a confident "the panel
  exposes nothing".
- Compounding both, the panel was measured with **no daemon**, so it sat in `.notRunning` —
  header plus a Start card, no roster at all. `buttons=0` there was the product's own
  emptiness, and every claim about roster rows and swap chips came from a state that renders
  none of them.

The round-1 canary did not catch any of this, and the reason is worth keeping: **it validates
the SHELL predicate, not the QUERY**. A measurement harness needs a canary at every layer it
can be wrong in. Probe D now asks four ways (`value`, `label`, `staticTexts`, dialog
presence) and reports each independently, so a single bad predicate can no longer carry a
verdict.

### The local half: 0 valid runs, and why

**No valid local measurement was obtained.** Two runs were attempted; both failed with
`The test runner hung before establishing connection` after the full ~330 s handshake
timeout, and **both are invalid as evidence**: the machine's screen was locked throughout
(`CGSSessionScreenIsLocked=1`, secure input held by another process), and XCUITest cannot
drive a locked session. Ad-hoc re-signing changed nothing, ruling signing out. The screen did
not unlock during the spike, so the count stands at **0 of 20 locally** and the developer-
machine half of the question is **unanswered**, not answered negatively.

That dead end is itself a finding: **an XCUITest lane cannot run while the screen is locked**,
so it can never be an unattended nightly or pre-commit check on a developer machine — only a
foreground one, on an unlocked session, borrowing the operator's menu bar for the duration.

### Re-running it

The probe is throwaway, so — exactly like `watch_spike.swift` above — `project.yml` carries no
UI-test target and `xcodegen` / `xcodebuild` never compile anything under `spikes/`. To
re-take the measurement, re-add the target and **its own scheme** (never to the `Menubar`
scheme's test action — the required `swift` CI job runs that scheme, and a hanging UI-test
runner there would block every merge):

```yaml
# apps/menubar/project.yml — under `targets:`
  MenubarUITests:
    type: bundle.ui-testing
    platform: macOS
    sources: [{path: spikes/uitest}]
    dependencies: [{target: Menubar}]
    settings:
      base:
        PRODUCT_BUNDLE_IDENTIFIER: org.sessiometer.menubar.uitests
        GENERATE_INFOPLIST_FILE: "YES"
        TEST_TARGET_NAME: Menubar
# …and under `schemes:`
  MenubarUISpike:
    build: {targets: {Menubar: all, MenubarUITests: [test]}}
    test: {config: Debug, targets: [MenubarUITests]}
```

```sh
# from apps/menubar, on an UNLOCKED session
xcodegen generate
xcodebuild test -project Menubar.xcodeproj -scheme MenubarUISpike \
  -configuration Debug -destination 'platform=macOS' CODE_SIGNING_ALLOWED=NO
```

Probe E is only meaningful with something serving the watch socket — `stub_daemon.py` above
is the starting point (the CI probe used a threaded variant that keeps the stream open and
vends two accounts). Launching the app adds a second status item to the menu bar for the
run's duration and opens a `watch` subscription; the probes never send `swap` or `capture`,
and there is no single-instance guard, so it coexists with a running app rather than
replacing it.
