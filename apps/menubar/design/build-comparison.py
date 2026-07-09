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

# Every `.pop` block, in document order, by balanced-<div> slicing. Source order is stable:
#  0 healthy-status-L · 1 healthy-status-D · 2/3 stats(skip) · 4 daemon-starting · 5/6/7 not-running(skip)
#  · 8 disconnected · 9 stale · 10/11 keychain(skip) · 12 version-skew · 13 empty-L · 14 empty-D
pops = []
for m in re.finditer(r'<div class="pop theme-(?:light|dark)">', html):
    start, depth = m.start(), 0
    end = start
    for tok in re.finditer(r"<div\b|</div>", html[start:]):
        depth += 1 if tok.group().startswith("<div") else -1
        if depth == 0:
            end = start + tok.end()
            break
    pops.append(html[start:end])


def cap(name):
    return "data:image/png;base64," + base64.b64encode((caps_dir / name).read_bytes()).decode()


STATES = [
    dict(title="1 · Healthy — Status", theme="light", design=0, capture="panel-healthy-light.png",
         note="The steady state. Mock adds four things the panel intentionally reconciles away: the "
              "Status/Stats toggle (Stats has no socket data path — spike #356), the provider line under "
              "each name (no <code>provider</code> wire field yet — #173), the “Last swap …” footer "
              "(dropped from the wire → event log, #88), and an enabled blue Swap button (its action is "
              "#169, so the panel ships it disabled — never a dead-click). The panel’s “updated &lt;1m "
              "ago” footer and duration resets (“3d”) mirror the <code>status</code> CLI (R-2), not the "
              "mock’s illustrative “snapshot 12s old” / “Sun”."),
    dict(title="1 · Healthy — Status (dark)", theme="dark", design=1, capture="panel-healthy-dark.png",
         note="Same state, dark appearance — system semantic colours, not the mock’s hex."),
    dict(title="2 · Connecting / daemon-starting", theme="light", design=4, capture="panel-connecting-light.png",
         note="Awaiting the first snapshot: an honest banner, no roster — never a false “healthy”. The "
              "mock’s separate <b>not-running</b> and <b>crash-looping</b> shapes are the fuller 9-state "
              "map (#169); the panel currently folds them into this / disconnected."),
    dict(title="3 · Disconnected (UDS drop)", theme="light", design=8, capture="panel-disconnected-light.png",
         note="Dropped connection: a loud strip over the <b>dimmed last-known</b> roster — retained, "
              "never frozen-as-live (#137) — and an amber “updated Nm ago” footer."),
    dict(title="4 · Stale snapshot", theme="light", design=9, capture="panel-stale-light.png",
         note="Connection open but the daemon went quiet: the roster stays full-strength, the header "
              "sub-line and footer carry the “stale” mark (amber), so numbers are never read as live."),
    dict(title="5 · Version skew / unsupported", theme="light", design=12, capture="panel-unsupported-light.png",
         note="The daemon speaks a wire contract this client can’t safely read → numbers refused, a plain "
              "honest message. The mock’s richer “brew upgrade” affordance is #169."),
    dict(title="6 · Empty roster / first run", theme="light", design=13, capture="panel-empty-roster-light.png",
         note="Connected, zero accounts — an onboarding card distinct from daemon-down. The "
              "<code>sessiometer capture</code> command is COPIED, never run (C-005)."),
    dict(title="6 · Empty roster / first run (dark)", theme="dark", design=14, capture="panel-empty-roster-dark.png",
         note="Onboarding, dark appearance."),
]

sections = "".join(f"""
    <section class="cmp">
      <h2>{s['title']}</h2>
      <p class="note">{s['note']}</p>
      <div class="pair">
        <figure class="side">
          <figcaption>Design — mock (live)</figcaption>
          <div class="stage-bg {s['theme']}">{pops[s['design']]}</div>
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
    <p>These are the <b>6 states the panel implements</b>. The mock’s <code>not-running</code>,
       <code>crash-looping</code>, and <code>keychain-locked</code> shapes are the fuller 9-state map
       (#169). The mock uses <code>backdrop-filter</code> vibrancy (semi-transparent); the app uses an
       opaque window background for contrast — a deliberate native translation.</p>
  </header>{sections}
</div>
</body></html>"""

out.write_text(page)
print(f"wrote {out} ({len(page)//1024} KB, {len(pops)} pop blocks)")
