#!/usr/bin/env bash
# Regenerate every Sessiometer brand asset from the masters in brand/src/.
#
#   ./brand/generate.sh
#
# Never hand-edit a generated file — edit the master in brand/src/ and re-run.
#
# Requires: rsvg-convert (brew install librsvg), Google Chrome (banner text),
#           iconutil + sips (macOS, for .icns).

set -euo pipefail

BRAND_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${BRAND_DIR}/.." && pwd)"
SRC="${BRAND_DIR}/src"
DIST="${BRAND_DIR}/dist"
APPICON="${REPO_ROOT}/apps/menubar/Sources/Assets.xcassets/AppIcon.appiconset"
ASSETS="${REPO_ROOT}/apps/menubar/Sources/Assets.xcassets"

CHROME="/Applications/Google Chrome.app/Contents/MacOS/Google Chrome"

# --- Locked design tokens (see brand/README.md) -----------------------------
MARK_BONE="#EDE8DF"          # resting mark on the warm-graphite body

NEEDLE_RESTING="M12 12 14.75 7.24"
NEEDLE_HEALTHY="M12 12 8.18 8.18"     # full
NEEDLE_WARNING="M12 12 12 6.6"        # half
NEEDLE_CRITICAL="M12 12 15.82 8.18"   # redline

# Vivid set — controlled tile (Dock, README, DMG, Homebrew) + the colour app icon.
# There is no contrast-darkened companion set any more: it existed SOLELY for the
# free-standing COLOUR menu-bar glyph, retired in #439. The bar is a monochrome
# template now (the bespoke `.symbolset` family below, #437/#524), and a template
# is system-tinted — so it needs no contrast set of its own.
V_HEALTHY="#30D158"; V_WARNING="#FF9F0A"; V_CRITICAL="#FF453A"; V_SWAP="#0A84FF"

# --- Bespoke bar-glyph .symbolset set (issue #437) --------------------------
# The menu-bar status item's four attention-state glyphs (#524: healthy / connecting /
# attention / no-runway), shipped as CUSTOM SF SYMBOL `.symbolset`s (NOT raster) so Apple's
# SF-Symbols engine optically-sizes + pixel-aligns them at bar size. Each glyph is the SHARED
# Cycle-Gauge chassis (open arc + rotation arrowhead; the needle + pivot dot from icon.svg are
# DROPPED) PLUS one bold interior mark — the family LOCKED in hq brand-identity.md (2588c9ea).
# Authored straight into Assets.xcassets as a generated artifact (like AppIcon.appiconset) —
# PURE bash, no rsvg (a .symbolset is SVG + Contents.json, never rasterized), so the `symbolset`
# subcommand below runs on ANY machine. Never hand-edit an emitted file — edit here and re-run.
#
# ⚠ SF SYMBOLS FILLS ARTWORK — IT NEVER STROKES IT. This bit the project hard: the first cut of
# this emitter authored the glyphs as STROKES (`fill="none" stroke-width="2.4"`), which actool
# silently reinterprets as FILLED paths. The open arc filled into a solid DISC, and every open
# stroked line (needle, X, "!") enclosed zero area and vanished outright — so all four states
# rendered as one identical white blob. That artifact was then measured on-device and misread as
# "the glyph DESIGN fails shape-distinctness", nearly costing a re-ratification of a locked brand
# mark. Every shape below is therefore authored as a FILLED OUTLINE — the stroke already expanded
# — using exact SVG arc commands (not polygonized): `fill`, never `stroke`. If you add a mark
# here, expand it to an outline yourself; a bare `stroke-width` will silently render as a blob.
#
# The structure below is the MINIMUM actool accepts (empirically verified): a Guides layer with
# Capline/Baseline at all three optical scales S/M/L, plus a Symbols layer carrying Regular-{S,M,L}.
# The design is fixed-weight (no Ultralight/Black anchors — a menu-bar item renders one weight),
# so the same artwork fills all three scales. The 2.4 arc weight is the shared app-icon master
# weight (src/icon.svg) and must NOT be re-weighted here.
#
# GAUGE_ARC — the invariant carrier (brand-identity.md: "the invariant carrier is the open gauge
# arc"). Centreline r=8 about (12,12), 2.4 wide → outer r=9.2 / inner r=6.8, round caps of r=1.2
# centred on the mouth edges (7.76,18.78)=122.03° and (16.24,18.78)=57.97°. Outline order: outer
# arc the long way round → end cap → inner arc back → start cap. The mouth (57.97°..122.03°, at
# the bottom) is what keeps this an OPEN arc, so the outline is one simple closed contour.
GAUGE_ARC='<path d="M 7.12 19.80 A 9.2 9.2 0 1 1 16.88 19.80 A 1.2 1.2 0 0 1 15.61 17.77 A 6.8 6.8 0 1 0 8.39 17.77 A 1.2 1.2 0 0 1 7.12 19.80 Z"/>'
# The rotation arrowhead — already a filled triangle in the master, so it needs no expansion. Part
# of the SHARED chassis (all four states): brand-identity.md calls it "the invariant 'cycle' cue".
# ⚠ VERTEX ORDER IS LOAD-BEARING (issue #532). The arrowhead overlaps GAUGE_ARC's outer edge at the
# arc mouth, and actool unions the two into ONE compound path under NONZERO winding — so the triangle
# must wind the SAME way as the arc's outer contour, or the overlap cancels to a WHITE HOLE. The
# vertices below run A→C→B (matching the arc); swapping the last two back to A→B→C reintroduces the
# notch. A browser can't catch this — it unions regardless of winding — see the "simply union" note below.
GAUGE_ARROWHEAD='<path d="M14.04 20.16 15.18 17.08 17.30 20.48 Z"/>'

# The shared chassis every state is built on: open arc + rotation arrowhead. Per the lock, the
# needle + pivot dot from src/icon.svg are DROPPED here (they stay in the colour app-icon master).
GAUGE_CHASSIS="${GAUGE_ARC}${GAUGE_ARROWHEAD}"

# emit_gauge_symbol <AssetName> <svg-filename> <interior-mark-svg>: write one bespoke .symbolset
# (shared chassis + the given interior mark) into Assets.xcassets. Idempotent — rewrites the dir.
emit_gauge_symbol() {
  local name="$1" file="$2" mark="$3"
  local dir="${ASSETS}/${name}.symbolset"
  local art="${GAUGE_CHASSIS}${mark}"
  rm -rf "${dir}"; mkdir -p "${dir}"
  cat > "${dir}/Contents.json" <<JSON
{
  "info" : { "author" : "xcode", "version" : 1 },
  "symbols" : [ { "idiom" : "universal", "filename" : "${file}" } ]
}
JSON
  cat > "${dir}/${file}" <<SVG
<?xml version="1.0" encoding="UTF-8"?>
<svg xmlns="http://www.w3.org/2000/svg" width="24" height="24" viewBox="0 0 24 24">
 <g id="Notes"/>
 <g id="Guides">
  <line id="Capline-S" x1="2" y1="3" x2="22" y2="3" stroke="#27AAE1" stroke-width="0.05"/>
  <line id="Baseline-S" x1="2" y1="21" x2="22" y2="21" stroke="#27AAE1" stroke-width="0.05"/>
  <line id="Capline-M" x1="2" y1="3" x2="22" y2="3" stroke="#27AAE1" stroke-width="0.05"/>
  <line id="Baseline-M" x1="2" y1="21" x2="22" y2="21" stroke="#27AAE1" stroke-width="0.05"/>
  <line id="Capline-L" x1="2" y1="3" x2="22" y2="3" stroke="#27AAE1" stroke-width="0.05"/>
  <line id="Baseline-L" x1="2" y1="21" x2="22" y2="21" stroke="#27AAE1" stroke-width="0.05"/>
 </g>
 <g id="Symbols" fill="#000000">
  <g id="Regular-S">${art}</g>
  <g id="Regular-M">${art}</g>
  <g id="Regular-L">${art}</g>
 </g>
</svg>
SVG
}

# The four #524 attention states = the LOCKED family from hq brand-identity.md (commit 2588c9ea,
# "Second refinement (2026-07-14) — the 4-glyph family + render pipeline are LOCKED"; creative
# council Direction B, operator-chosen):
#
#   "Locked family (shared arc+arrowhead chassis, each + one interior mark):
#    Healthy ✓ · Connecting … · Attention ! · No-runway ⊘"
#
# So the arrowhead is part of the SHARED chassis on ALL FOUR ("the arrowhead is the invariant
# 'cycle' cue"), and the needle stays DROPPED ("Drop the thin needle" — the hairline was predicted
# to die at bar size). Both were briefly violated in this branch (arrowhead demoted to healthy-only,
# healthy re-drawn as a needle) while the glyphs were mis-diagnosed as failing their distinctness
# falsifier — that verdict was a RENDER bug (see the fills-never-strokes warning above), not a
# design failure, so the deviation had no basis and is reverted here. Any future divergence from the
# family above must amend the hq lock FIRST, judged against a real on-device render — not a blob.
#
# Capsule outlines below are expanded stroke centrelines: two offset edges + a round cap (r = half
# the weight) at each end. Given centreline p1→p2 of weight w, the four corners are p{1,2} ± (w/2)·n
# where n is the unit normal — the literals are those corners, so re-deriving a mark means
# re-deriving its corners (the stroke centreline is named in each comment so that stays possible).
# A multi-segment stroke (the check) is its per-segment capsules PLUS a round-join disc at the
# vertex — overlapping filled paths union ONLY when they wind the SAME way. actool merges every
# subpath into one NONZERO-winding compound path, so two overlapping fills wound OPPOSITE ways CANCEL
# to a hole (issue #532 — the arrowhead-vs-arc white notch). Same ink is necessary, not sufficient:
# author every solid mark to wind like GAUGE_ARC's outer edge. (The check's capsules happen to agree,
# so they union fine; the arrowhead did not, and holed.)
#
# ⚠ AND actool HONOURS ONLY <path> — IT SILENTLY DROPS <circle>. The second half of the same trap:
# an earlier cut drew the dots/pivot as <circle fill=...>, which renders correctly in every browser
# but vanishes entirely on-device (connecting came out a BARE RING). So every dot below is a <path>
# of two half-arcs: a circle (cx,cy,r) is `M cx-r cy A r r 0 1 0 cx+r cy A r r 0 1 0 cx-r cy Z`.
# Corollary for anyone verifying a change: rendering the SVG in a browser CANNOT catch either trap
# (browsers stroke, and browsers draw <circle>) — only the on-device SESSIOMETER_GLYPH_GALLERY
# capture is a valid check. See apps/menubar/Sources/main.swift.
#
#   .healthy check "✓": (8.8,12.2)→(10.8,14.4)→(15.2,9.4) w=2.4, round join at the vertex.
#   .connecting dots "…": r=1.5, centres 7.5/12/16.5 — gap ≥ radius so they never merge at Small
#     (the lock's one geometry tweak: r1.35→1.5, "→ 4 px @2x"); their 16 px survival is the
#     convergent falsifier all three council lenses named.
#   .attention "!": bar (12,7.8)→(12,13.0) w=2.4, + dot r=1.4 at (12,16.1).
#   .noRunway slash "⊘": (8.6,15.4)→(15.4,8.6) w=2.6 — a FORWARD slash. The lock names the mark
#     "⊘" (U+2298 CIRCLED DIVISION SLASH), which is a forward slash; the first cut drew the
#     centreline (8.6,8.6)→(15.4,15.4), i.e. a BACKslash, so this is a fidelity fix to the locked
#     glyph, not a divergence from it. (Operator-confirmed on-device 2026-07-14.)
emit_status_symbolset() {
  echo "==> status-item bar glyphs (bespoke Cycle-Gauge .symbolset — #437)"
  emit_gauge_symbol GaugeHealthy    gauge-healthy.svg    '<path d="M 7.91 13.01 L 9.91 15.21 A 1.2 1.2 0 0 0 11.69 13.59 L 9.69 11.39 A 1.2 1.2 0 0 0 7.91 13.01 Z"/><path d="M 11.70 15.19 L 16.10 10.19 A 1.2 1.2 0 0 0 14.30 8.61 L 9.90 13.61 A 1.2 1.2 0 0 0 11.70 15.19 Z"/><path d="M 9.6 14.4 A 1.2 1.2 0 1 0 12 14.4 A 1.2 1.2 0 1 0 9.6 14.4 Z"/>'
  emit_gauge_symbol GaugeConnecting gauge-connecting.svg '<path d="M 6 12 A 1.5 1.5 0 1 0 9 12 A 1.5 1.5 0 1 0 6 12 Z"/><path d="M 10.5 12 A 1.5 1.5 0 1 0 13.5 12 A 1.5 1.5 0 1 0 10.5 12 Z"/><path d="M 15 12 A 1.5 1.5 0 1 0 18 12 A 1.5 1.5 0 1 0 15 12 Z"/>'
  emit_gauge_symbol GaugeAttention  gauge-attention.svg  '<path d="M 10.8 7.8 L 10.8 13.0 A 1.2 1.2 0 0 0 13.2 13.0 L 13.2 7.8 A 1.2 1.2 0 0 0 10.8 7.8 Z"/><path d="M 10.6 16.1 A 1.4 1.4 0 1 0 13.4 16.1 A 1.4 1.4 0 1 0 10.6 16.1 Z"/>'
  emit_gauge_symbol GaugeNoRunway   gauge-norunway.svg   '<path d="M 9.52 16.32 L 16.32 9.52 A 1.3 1.3 0 0 0 14.48 7.68 L 7.68 14.48 A 1.3 1.3 0 0 0 9.52 16.32 Z"/>'
}

# `generate.sh symbolset` emits ONLY the bar-glyph set and exits — the one path that needs NO
# rsvg-convert (it authors SVG + Contents.json directly), so it runs anywhere (incl. this repo's
# CI / a Linux box). A bare `generate.sh` (below) still emits it as part of the full brand regen.
if [[ "${1:-}" == "symbolset" ]]; then
  emit_status_symbolset
  echo "done → 4 status-item .symbolset(s) in ${ASSETS}"
  exit 0
fi

need() { command -v "$1" >/dev/null 2>&1 || { echo "missing: $1" >&2; exit 1; }; }
need rsvg-convert

rm -rf "${DIST}"; mkdir -p "${DIST}"

# derive(master, out.svg, new_color, new_needle, old_color)
derive() {
  sed -e "s|${5}|${3}|g" -e "s|M12 12 14.75 7.24|${4}|" "$1" > "$2"
}

echo "==> app icon (resting, warm graphite + bone)"
for s in 16 32 64 128 256 512 1024; do
  rsvg-convert -w $s -h $s "${SRC}/icon.svg" -o "${DIST}/icon_${s}.png"
done
cp "${DIST}/icon_512.png" "${DIST}/logo.png"          # dot-github profile logo (512²)

echo "==> AppIcon.appiconset"
mkdir -p "${APPICON}"
cp "${DIST}/icon_16.png"   "${APPICON}/icon_16x16.png"
cp "${DIST}/icon_32.png"   "${APPICON}/icon_16x16@2x.png"
cp "${DIST}/icon_32.png"   "${APPICON}/icon_32x32.png"
cp "${DIST}/icon_64.png"   "${APPICON}/icon_32x32@2x.png"
cp "${DIST}/icon_128.png"  "${APPICON}/icon_128x128.png"
cp "${DIST}/icon_256.png"  "${APPICON}/icon_128x128@2x.png"
cp "${DIST}/icon_256.png"  "${APPICON}/icon_256x256.png"
cp "${DIST}/icon_512.png"  "${APPICON}/icon_256x256@2x.png"
cp "${DIST}/icon_512.png"  "${APPICON}/icon_512x512.png"
cp "${DIST}/icon_1024.png" "${APPICON}/icon_512x512@2x.png"
cat > "${APPICON}/Contents.json" <<'JSON'
{
  "images" : [
    { "filename" : "icon_16x16.png",      "idiom" : "mac", "scale" : "1x", "size" : "16x16" },
    { "filename" : "icon_16x16@2x.png",   "idiom" : "mac", "scale" : "2x", "size" : "16x16" },
    { "filename" : "icon_32x32.png",      "idiom" : "mac", "scale" : "1x", "size" : "32x32" },
    { "filename" : "icon_32x32@2x.png",   "idiom" : "mac", "scale" : "2x", "size" : "32x32" },
    { "filename" : "icon_128x128.png",    "idiom" : "mac", "scale" : "1x", "size" : "128x128" },
    { "filename" : "icon_128x128@2x.png", "idiom" : "mac", "scale" : "2x", "size" : "128x128" },
    { "filename" : "icon_256x256.png",    "idiom" : "mac", "scale" : "1x", "size" : "256x256" },
    { "filename" : "icon_256x256@2x.png", "idiom" : "mac", "scale" : "2x", "size" : "256x256" },
    { "filename" : "icon_512x512.png",    "idiom" : "mac", "scale" : "1x", "size" : "512x512" },
    { "filename" : "icon_512x512@2x.png", "idiom" : "mac", "scale" : "2x", "size" : "512x512" }
  ],
  "info" : { "author" : "xcode", "version" : 1 }
}
JSON

emit_status_symbolset

echo "==> .icns (DMG / Finder)"
if command -v iconutil >/dev/null 2>&1; then
  ICONSET="${DIST}/Sessiometer.iconset"; mkdir -p "${ICONSET}"
  for pair in "16:icon_16x16" "32:icon_16x16@2x" "32:icon_32x32" "64:icon_32x32@2x" \
              "128:icon_128x128" "256:icon_128x128@2x" "256:icon_256x256" \
              "512:icon_256x256@2x" "512:icon_512x512" "1024:icon_512x512@2x"; do
    cp "${DIST}/icon_${pair%%:*}.png" "${ICONSET}/${pair##*:}.png"
  done
  iconutil -c icns "${ICONSET}" -o "${DIST}/Sessiometer.icns"
  rm -rf "${ICONSET}"
fi

echo "==> living icon — status variants on the tile (vivid set)"
derive "${SRC}/icon.svg" "${DIST}/_ih.svg" "${V_HEALTHY}"  "${NEEDLE_HEALTHY}"  "${MARK_BONE}"
derive "${SRC}/icon.svg" "${DIST}/_iw.svg" "${V_WARNING}"  "${NEEDLE_WARNING}"  "${MARK_BONE}"
derive "${SRC}/icon.svg" "${DIST}/_ic.svg" "${V_CRITICAL}" "${NEEDLE_CRITICAL}" "${MARK_BONE}"
derive "${SRC}/icon.svg" "${DIST}/_is.svg" "${V_SWAP}"     "${NEEDLE_RESTING}"  "${MARK_BONE}"
for pair in "_ih:icon-healthy" "_iw:icon-warning" "_ic:icon-critical" "_is:icon-swap"; do
  for s in 256 512; do
    rsvg-convert -w $s -h $s "${DIST}/${pair%%:*}.svg" -o "${DIST}/${pair##*:}_${s}.png"
  done
done
cp "${DIST}/icon_512.png" "${DIST}/icon-resting_512.png"

# --- No colour menu-bar glyph emission (issue #439) -------------------------
# The menu-bar status item is a MONOCHROME TEMPLATE: state is carried by the glyph
# SHAPE, not colour — a menu-bar image is system-tinted, so colour cannot encode
# health there at all (#325). Its four bespoke attention-state glyphs are the
# `.symbolset` family emitted above (#437 artwork / #524 taxonomy).
#
# This block previously emitted a free-standing COLOUR bar glyph in two contrast
# sets (`-lightbar` darkened / `-darkbar` vivid, needle tracking the reading). That
# targeted a colour bar the app does not — and cannot — use, so it is retired here.
# The colour "living instrument" still governs the app icon, Dock, and in-panel
# surfaces (emitted above); only the MENU BAR is monochrome.
#
# `src/glyph.svg` is kept as the archived colour-glyph master — no longer emitted by
# this pipeline, and no longer consumed by the app.

echo "==> favicon"
cp "${SRC}/favicon.svg" "${DIST}/favicon.svg"
rsvg-convert -w 32 -h 32 "${SRC}/favicon.svg" -o "${DIST}/favicon-32.png"
rsvg-convert -w 180 -h 180 "${SRC}/icon.svg"  -o "${DIST}/apple-touch-icon.png"

echo "==> social preview / og-image (1200x630)"
if [ -x "${CHROME}" ]; then
  "${CHROME}" --headless=new --hide-scrollbars --force-device-scale-factor=1 \
    --window-size=1200,630 --screenshot="${DIST}/social-preview.png" \
    "file://${SRC}/social-preview.html" >/dev/null 2>&1
  cp "${DIST}/social-preview.png" "${DIST}/og-image.png"
else
  echo "   ! Chrome not found — skipping banner" >&2
fi

rm -f "${DIST}"/_*.svg
echo
echo "done → ${DIST}"
ls -1 "${DIST}"
