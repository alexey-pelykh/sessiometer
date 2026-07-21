#!/usr/bin/env python3
"""Build a self-contained design-vs-capture comparison page, screen by screen.

  Design side  = the mock's LIVE `.pop` blocks (menubar-preview.html, this directory), reused with the
                 mock's own <style> so each panel renders pixel-accurate.
  Capture side = the built SwiftUI panel PNGs from `RenderPanelTool` (`Sessiometer --render-panel <dir>`),
                 base64-embedded so the page opens anywhere with no external files.

Usage:
  # 1. render the built panel states (from apps/menubar, after a Debug build):
  BIN=.build/xcode/Build/Products/Debug/Sessiometer.app/Contents/MacOS/Sessiometer
  "$BIN" --render-panel /tmp/panelcaps        # writes panel-<state>-<theme>.png
  # 2. build + open the comparison:
  python3 design/build-comparison.py /tmp/panelcaps design/design-vs-capture.html
  open design/design-vs-capture.html

Args: <captures-dir> [output.html]   (output defaults to <captures-dir>/design-vs-capture.html)
"""
import base64
import pathlib
import re
import sys

HERE = pathlib.Path(__file__).resolve().parent
MOCK = HERE / "menubar-preview.html"

caps_dir = pathlib.Path(sys.argv[1]) if len(sys.argv) > 1 else HERE / "renders"
out = pathlib.Path(sys.argv[2]) if len(sys.argv) > 2 else caps_dir / "design-vs-capture.html"

html = MOCK.read_text()
style = re.search(r"<style>(.*?)</style>", html, re.S).group(1)

# Every `.pop` block, keyed by the `data-frame` name the mock gives it, sliced by balanced <div>.
# Frames are selected BY NAME, never by position: inserting, removing, or reordering a frame in the
# mock cannot silently re-pair this page, so a mock frame and its Swift fixture no longer have to land
# in one commit. Every failure below is loud — this is a verification tool, and one that quietly
# reports the wrong answer is worse than one that fails (#581, superseding #571's count tripwire).
def line_of(pos):
    """The mock's 1-based line number at `pos` — so a failure can point at the offending block."""
    return html.count("\n", 0, pos) + 1


pops = {}
for m in re.finditer(r'<div class="pop theme-(?:light|dark)"[^>]*>', html):
    tag = m.group()
    named = re.search(r'\bdata-frame="([^"]+)"', tag)
    if not named:
        raise SystemExit(
            f"{MOCK.name}: the `.pop` block on line {line_of(m.start())} has no `data-frame` name — "
            f"every frame needs one, it is how this page selects them. Name it after its `fcap` "
            f"caption:\n  → {tag}"
        )
    name = named.group(1)
    if name in pops:
        raise SystemExit(
            f'{MOCK.name}: duplicate `data-frame="{name}"` on line {line_of(m.start())} — names are '
            f"this page's selector, so a duplicate would silently pair a state against the wrong "
            f"panel. Give each frame a unique name."
        )
    start, depth = m.start(), 0
    end = start
    for tok in re.finditer(r"<div\b|</div>", html[start:]):
        depth += 1 if tok.group().startswith("<div") else -1
        if depth == 0:
            end = start + tok.end()
            break
    pops[name] = html[start:end]


def design(name):
    """The mock's live `.pop` block for `name` — or a loud failure listing what the mock does carry."""
    if name not in pops:
        raise SystemExit(
            f'STATES design="{name}": {MOCK.name} carries no such frame — re-point this entry, or '
            f"name the frame in the mock. Frames it does carry:\n"
            f"  {', '.join(pops)}"
        )
    return pops[name]


def cap(name):
    """The built panel PNG for `name`, base64-embedded — or a loud failure naming the missing file."""
    png = caps_dir / name
    if not png.is_file():
        raise SystemExit(
            f'STATES capture="{name}": {png} — no such capture. Render the built panel states '
            f'first:\n  "$BIN" --render-panel {caps_dir}'
        )
    return "data:image/png;base64," + base64.b64encode(png.read_bytes()).decode()


STATES = [
    dict(title="1 · Healthy — Status", theme="light", design="healthy-status-light", capture="panel-healthy-light.png",
         note="The steady state. Mock adds four things the panel intentionally reconciles away: the "
              "Status/Stats toggle (Stats has no socket data path — spike #356), the provider line under "
              "each name (no <code>provider</code> wire field yet — #173), the “Last swap …” footer "
              "(dropped from the wire → event log, #88), and an enabled blue Swap button (its action is "
              "#169, so the panel ships it disabled — never a dead-click). The header glyph diverges too — the mock now carries the locked Cycle-Gauge "
              "mark (#437/#524), the built panel the neutral system <code>gauge.medium</code>. "
              "The panel’s “updated &lt;1m "
              "ago” footer mirrors the <code>status</code> CLI (R-2), not the mock’s illustrative "
              "“snapshot 12s old”; resets no longer diverge either — the mock now uses the CLI’s "
              "duration form too (“2h14m” / “3d”), and its usage meters carry the CLI’s 75/90 bands. "
              "Capture is now reconciled too: the populated panel carries no capture bar, matching the "
              "mock — capture is empty-roster / first-run only, with Add account in the status-item menu (#394)."),
    dict(title="1 · Healthy — Status (dark)", theme="dark", design="healthy-status-dark", capture="panel-healthy-dark.png",
         note="Same state, dark appearance — system semantic colours, not the mock’s hex."),
    dict(title="2 · Connecting / daemon-starting", theme="light", design="daemon-starting-light", capture="panel-connecting-light.png",
         note="Awaiting the first snapshot: an honest banner, no roster — never a false “healthy”. The panel "
              "draws this, <b>not-running</b>, and <b>crash-looping</b> (both below) on ONE shared “no "
              "trustworthy reading” card — a single <code>StatusPanelView</code> arm. The card is shared; the "
              "<i>message</i> is not, and the message is the card’s whole content — which is why the next two "
              "pair separately instead of folding into this one (#593)."),
    dict(title="3 · Daemon not running", theme="light", design="not-running-light", capture="panel-not-running-light.png",
         note="The daemon is absent and — unlike a drop — never held a reading to age (#499): no roster to dim, "
              "no “updated Nm ago” footer to amber. Mock↔panel, the whole comparison is the string. The header "
              "sub-line diverges (mock “Daemon offline”, panel “Daemon not running”, so the panel’s sub-line and "
              "card title read the same words twice). The body loses its second half — mock “The background "
              "service isn’t running. Start it to resume live status.” against the panel’s “The daemon isn’t "
              "running.” — because the mock’s <b>Start daemon</b> button has no panel counterpart: launch-at-login "
              "is #170 (deferred, signing-blocked), so the card ships an inert explanatory line rather than a "
              "dead click."),
    dict(title="3 · Daemon not running (dark)", theme="dark", design="not-running-dark", capture="panel-not-running-dark.png",
         note="Same state, dark appearance."),
    dict(title="4 · Daemon crash-looping", theme="light", design="crash-looping-light", capture="panel-crash-looping-light.png",
         note="The daemon served a snapshot but keeps dropping before it stabilises, so its numbers are REFUSED "
              "rather than flickered as live — the anti-#137 healthy-flash debounce (#169). Same shared card as "
              "not-running above, so again the message is the comparison — and here it largely lands: sub-line "
              "(“Daemon fault”) and card title (“Daemon crash-looping”) both agree with the mock. The body "
              "diverges only in its count — the mock’s clock-based “Restarted 5× in the last minute.” becomes "
              "the panel’s clock-free “Restarting repeatedly”, both closing on “holding status until it stays "
              "up.” — because the client counts consecutive unstable reconnects, not wall-clock restarts, and "
              "will not quote a number it cannot source. The mock’s <b>View log</b> / <b>Restart…</b> "
              "affordances (Restart behind a confirm) are #169 siblings, so the panel’s card carries the "
              "message alone. Light-only: the mock has no <code>crash-looping-dark</code> frame."),
    dict(title="5 · Disconnected (UDS drop)", theme="light", design="disconnected-light", capture="panel-disconnected-light.png",
         note="Dropped connection: a loud strip over the <b>dimmed last-known</b> roster — retained, "
              "never frozen-as-live (#137) — and an amber “updated Nm ago” footer."),
    dict(title="6 · Stale snapshot", theme="light", design="stale-snapshot-light", capture="panel-stale-light.png",
         note="Connection open but the daemon went quiet: the roster stays full-strength, the header "
              "sub-line and footer carry the “stale” mark (amber), so numbers are never read as live."),
    dict(title="7 · Version skew / unsupported", theme="light", design="version-skew-light", capture="panel-unsupported-light.png",
         note="The daemon speaks a wire contract this client can’t safely read → numbers refused, a plain "
              "honest message. The mock’s richer “brew upgrade” affordance is #169."),
    dict(title="8 · Empty roster / first run", theme="light", design="empty-roster-light", capture="panel-empty-roster-light.png",
         note="Connected, zero accounts — an onboarding card distinct from daemon-down. First run "
              "captures the active account <b>in-app</b> (operator-label field + button over the #358 "
              "control socket, honest pending → done → error) — not a copied command (#360)."),
    dict(title="8 · Empty roster / first run (dark)", theme="dark", design="empty-roster-dark", capture="panel-empty-roster-dark.png",
         note="Onboarding, dark appearance."),
    dict(title="Modifier · Active blind — OK", theme="light", design="blind-ok-light", capture="panel-blind-ok-light.png",
         note="The ACTIVE account’s /usage poll is rate-limited (429): the daemon holds a bounded anchor and "
              "pushes <code>blind_active</code>. The two live meters are replaced by a HELD session bar "
              "(<b>dashed</b> = last-known, never a live fill — #137), the <code>blind {dur}</code> chip, the "
              "LAST-KNOWN·RATE-LIMITED caption, and the auto-protection verdict. Health slot → an eye-slash "
              "(usage visibility lost — a distinct shape). OK stays calm; the menu-bar glance reads healthy "
              "(bounded, self-resolving — no cry-wolf). Header + footer stay FRESH: the fault is THIS row’s "
              "(locality), the tell that it is not a whole-snapshot “stale”. Per #479/#485. The mock hand-draws "
              "the panel’s SF Symbols (<code>eye.slash</code>, <code>checkmark.shield.fill</code>) as OUTLINE "
              "approximations of filled marks — a mock-fidelity limit, not a parity claim: mock↔panel is ONE "
              "medium, so R-2 (the CLI↔panel STATE-parity anchor) does not license the difference."),
    dict(title="Modifier · Active blind — OK (dark)", theme="dark", design="blind-ok-dark", capture="panel-blind-ok-dark.png",
         note="Same state, dark appearance."),
    dict(title="Modifier · Active blind — DEGRADED", theme="light", design="blind-degraded-light", capture="panel-blind-degraded-light.png",
         note="Auto-protection DEGRADED — the ADR-0017 gate is armed but acting on a STALE anchor (last-known "
              "88%, amber). The row gains an at-risk ORANGE leading rule + orange eye-slash + orange verdict — a "
              "non-colour-redundant locality tell. The menu-bar glance escalates to attention “!” (one rung "
              "below no-runway). The CLI emphasises this line in red; the panel uses orange by design — a "
              "per-medium colour choice under R-2 STATE-parity (the shared STATE is DEGRADED)."),
    dict(title="Modifier · Active blind — DEGRADED (dark)", theme="dark", design="blind-degraded-dark", capture="panel-blind-degraded-dark.png",
         note="Degraded, dark appearance."),
    # The four DAEMON-FAULT ranks (#592), in the worst-first order `StatusPanelFormat.daemonFaultBanner`
    # resolves them. They pair CONSECUTIVELY on purpose: the ranking is a *visual* claim — rank 3 has to be
    # seen to beat rank 4 — and a severity inversion is only legible when the frames sit next to each other.
    dict(title="Fault 1 · Keychain locked", theme="light", design="keychain-locked-light", capture="panel-fault-keychain-locked-light.png",
         note="Rank 1, the worst: the login keychain is LOCKED, so the daemon cannot READ the shared "
              "<code>Claude Code-credentials</code> item at all. Fleet-wide, and NO per-row "
              "<code>auth</code> cell reflects it — which is why both sides show a full green roster under "
              "the fault. That is the state this banner exists to contradict, not an inconsistency. Remedy "
              "is UNLOCK, never <code>claude /login</code> (a re-login can’t help while the keychain storing "
              "the credential is locked). <b>One row-level asymmetry to read past</b>: this pre-existing frame "
              "alone marks the active row with a per-row lock glyph — the three fault frames below, and the "
              "SHIPPED panel in all four captures, keep every row healthy-green, matching the banner’s own "
              "premise that no per-row <code>auth</code> cell reflects a daemon-level fault. The lock predates "
              "this pairing (#498) and #592 left it untouched by design. Structural divergence, uniform across all four of these: the mock "
              "expresses a daemon fault as a <b>strip under the header</b> (plus a paused swap-row and an "
              "explanatory footer line), the panel as a single <b>banner between dividers</b> above the "
              "roster — dot + headline + one sentence. The mock’s richer affordances (pausing the swap row, "
              "the “only waits, never prompts” footer) have no panel counterpart; the panel carries the "
              "whole message in the banner. Per #498."),
    dict(title="Fault 1 · Keychain locked (dark)", theme="dark", design="keychain-locked-dark", capture="panel-fault-keychain-locked-dark.png",
         note="Same state, dark appearance."),
    dict(title="Fault 2 · Shared login scrubbed — exhausted", theme="light", design="scrub-exhausted-light", capture="panel-fault-scrub-exhausted-light.png",
         note="Rank 2: the shared canonical was scrubbed AND the daemon’s adopt-recovery is exhausted, so "
              "every <code>claude</code> session is logged out until an operator acts. Ranked directly under "
              "keychain-locked — the other half of the “act now” vault pair — and like it, an "
              "<code>.error</code> banner carrying the remedy (<code>claude /login</code>). Content-parity "
              "with the CLI’s <code>shared login: scrubbed …</code> line under R-2. Per #469."),
    dict(title="Fault 3 · Refresh mechanism down", theme="light", design="systemic-refresh-light", capture="panel-fault-systemic-refresh-light.png",
         note="Rank 3, and the load-bearing one: N consecutive refresh sweeps failed for EVERY eligible "
              "account, so the refresh <i>mechanism</i> is down. <code>.warning</code>, not "
              "<code>.error</code> — every account still works, so this is a PRE-DEATH “next break” task "
              "(#375 kept a total refresh outage invisible ~4.5 h until a token finally expired). The visual "
              "tell separating it from ranks 1–2 is that <b>swapping stays live</b> on both sides — nothing "
              "is blocked yet. It is deliberately ranked ABOVE the calm scrub below, which is the pairing "
              "the next entry exists to make visible. Per #523/#378."),
    dict(title="Fault 4 · Shared login scrubbed — recovering", theme="light", design="scrub-recovering-light", capture="panel-fault-scrub-recovering-light.png",
         note="Rank 4, the calmest — and the reason the whole family needed an oracle. Same scrub as rank 2, "
              "but the daemon is still self-healing, so its whole message is “no action needed”: an "
              "<code>.info</code> banner in the panel, and in the mock a <code>calm</code> strip whose icon "
              "drops the amber attention tint. Read this frame AGAINST rank 3 above: severity ranks by "
              "(fault, VARIANT), never by fault identity, so a self-healing state can never outrank one that "
              "cannot self-heal. Treating canonical-scrub as ONE slot silently promoted this variant over "
              "systemic — and a <code>recovering</code> scrub coinciding with a down refresh mechanism then "
              "made the two surfaces CONTRADICT each other: the glance correctly shouted <code>!</code> at "
              "the systemic fault while the panel answered the click with a grey “no action needed” over a "
              "green roster. That inversion is a purely visual regression, which is exactly what this pair "
              "is here to catch. Per #469/#523."),
]

sections = "".join(f"""
    <section class="cmp">
      <h2>{s['title']}</h2>
      <p class="note">{s['note']}</p>
      <div class="pair">
        <figure class="side">
          <figcaption>Design — mock (live)</figcaption>
          <div class="stage-bg {s['theme']}">{design(s['design'])}</div>
        </figure>
        <figure class="side">
          <figcaption>Capture — built panel</figcaption>
          <img src="{cap(s['capture'])}" alt="{s['title']} capture">
        </figure>
      </div>
    </section>""" for s in STATES)

page = f"""<!doctype html>
<html lang="en"><head><meta charset="utf-8">
<title>Sessiometer menubar — design vs. capture</title>
<style>{style}</style>
<style>
  html, body {{ margin:0; padding:0; background:#f2f2f4; color:#1d1d1f;
    font:14px/1.5 -apple-system,"SF Pro Text",system-ui,sans-serif; background-image:none !important; }}
  .wrap {{ max-width:920px; margin:0 auto; padding:28px 24px 80px }}
  header.top h1 {{ font-size:22px; margin:0 0 6px }}
  header.top p {{ margin:0 0 4px; color:#555; max-width:74ch }}
  code {{ font-family:ui-monospace,"SF Mono",Menlo,monospace; font-size:.88em;
    background:rgba(120,120,128,.15); padding:1px 5px; border-radius:4px }}
  section.cmp {{ margin-top:34px; border-top:1px solid rgba(0,0,0,.1); padding-top:18px }}
  section.cmp h2 {{ font-size:16px; margin:0 0 6px }}
  .note {{ margin:0 0 16px; color:#555; max-width:82ch }}
  .pair {{ display:grid; grid-template-columns:1fr 1fr; gap:22px; align-items:start }}
  figure.side {{ margin:0 }}
  figcaption {{ font-size:11px; font-weight:600; text-transform:uppercase; letter-spacing:.04em;
    color:#888; margin-bottom:8px }}
  .side img {{ width:380px; max-width:100%; height:auto; display:block;
    border:.5px solid rgba(0,0,0,.12); border-radius:13px }}
  .stage-bg {{ width:380px; max-width:100%; border-radius:13px; overflow:hidden; display:inline-block }}
  .stage-bg.light {{ background:linear-gradient(145deg,#dfe6f2,#c9d3e6) }}
  .stage-bg.dark  {{ background:linear-gradient(145deg,#2a2f3a,#1c2029) }}
  .stage-bg .pop {{ margin:0 }}
  @media (max-width:820px) {{ .pair {{ grid-template-columns:1fr }} }}
</style>
</head><body>
<div class="wrap">
  <header class="top">
    <h1>Sessiometer menubar — design vs. capture</h1>
    <p><b>Design</b> = the canonical mock (<code>menubar-preview.html</code>), reused live so each panel
       is pixel-accurate. <b>Capture</b> = the <i>built</i> SwiftUI panel, drawn to PNG by
       <code>RenderPanelTool</code> (<code>--render-panel</code>, SwiftUI <code>ImageRenderer</code> — no
       screen capture).</p>
    <p>These are the <b>8 connection-states the panel implements</b>, plus the active-account
       <b>blind</b> modifier (OK / DEGRADED, #479/#485) — a per-row modifier on a connected snapshot,
       not a 10th daemon-state — and the four <b>daemon-fault</b> ranks (#592), which are likewise not
       states but a banner resolved over a <i>connected</i> snapshot. Those four close on the ranking
       itself: they run in the worst-first order the panel resolves them, so a severity inversion — a
       visual claim no format-layer unit test can catch — is legible by reading them in sequence. The
       mock uses <code>backdrop-filter</code> vibrancy (semi-transparent); the app uses an opaque window
       background for contrast — a deliberate native translation.</p>
  </header>{sections}
</div>
</body></html>"""

out.write_text(page)
print(f"wrote {out} ({len(page)//1024} KB, {len(STATES)} states from {len(pops)} frames)")
