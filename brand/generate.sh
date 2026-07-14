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
GLYPH_INK="#1D1D1F"          # resting menu-bar glyph (template; macOS tints it)

NEEDLE_RESTING="M12 12 14.75 7.24"
NEEDLE_HEALTHY="M12 12 8.18 8.18"     # full
NEEDLE_WARNING="M12 12 12 6.6"        # half
NEEDLE_CRITICAL="M12 12 15.82 8.18"   # redline

# Vivid set — controlled tile (Dock, README, DMG, Homebrew)
V_HEALTHY="#30D158"; V_WARNING="#FF9F0A"; V_CRITICAL="#FF453A"; V_SWAP="#0A84FF"
# Contrast-darkened set — free-standing menu-bar glyph (clears 3:1 on any bar)
D_HEALTHY="#248A3D"; D_WARNING="#9A6A00"; D_CRITICAL="#FF3B30"; D_SWAP="#007AFF"

# --- Bespoke bar-glyph .symbolset set (issue #437) --------------------------
# The menu-bar status item's four attention-state glyphs (#524: healthy / connecting /
# attention / no-runway), shipped as CUSTOM SF SYMBOL `.symbolset`s (NOT raster) so Apple's
# SF-Symbols engine optically-sizes + pixel-aligns them at bar size. Each glyph is the SHARED
# Cycle-Gauge chassis (open arc; + the rotation arrowhead on healthy only — the arrowhead reads
# as "actively cycling", so it is wrong on the states that are NOT cycling) PLUS one mark.
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
# The rotation arrowhead — already a filled triangle in the master, so it needs no expansion.
GAUGE_ARROWHEAD='<path d="M14.04 20.16 17.30 20.48 15.18 17.08 Z"/>'

# emit_gauge_symbol <AssetName> <svg-filename> <interior-mark-svg>: write one bespoke .symbolset
# (shared chassis + the given interior mark) into Assets.xcassets. Idempotent — rewrites the dir.
emit_gauge_symbol() {
  local name="$1" file="$2" mark="$3"
  local dir="${ASSETS}/${name}.symbolset"
  local art="${GAUGE_ARC}${mark}"
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

# The four #524 attention states, each = GAUGE_ARC + the mark below. All marks are FILLED OUTLINES
# (see the SF-Symbols-fills warning above). Operator-ratified 2026-07-14, on-device:
#   .healthy    → arrowhead + needle + pivot   .attention  → exclamation "!"
#   .connecting → three mid-line dots "…"      .noRunway   → an X, vertices on the ring
# Only .healthy carries the arrowhead (= actively cycling); the rest are a bare arc + mark.
#
# Capsule outlines below are expanded stroke centrelines: two offset edges + a round cap (r = half
# the weight) at each end. Given centreline p1→p2 of weight w, the four corners are p{1,2} ± (w/2)·n
# where n is the unit normal — the literals are those corners, so re-deriving a mark means
# re-deriving its corners (the stroke centreline is named in each comment so that stays possible).
#
# ⚠ AND actool HONOURS ONLY <path> — IT SILENTLY DROPS <circle>. The second half of the same trap:
# an earlier cut drew the dots/pivot as <circle fill=...>, which renders correctly in every browser
# but vanishes entirely on-device (connecting came out a BARE RING). So every dot below is a <path>
# of two half-arcs: a circle (cx,cy,r) is `M cx-r cy A r r 0 1 0 cx+r cy A r r 0 1 0 cx-r cy Z`.
# Corollary for anyone verifying a change: rendering the SVG in a browser CANNOT catch either trap
# (browsers stroke, and browsers draw <circle>) — only the on-device SESSIOMETER_GLYPH_GALLERY
# capture is a valid check. See apps/menubar/Sources/main.swift.
#
#   .healthy needle: (12,12)→(14.75,7.24) w=2.2 — the resting pose from src/icon.svg. NOTE: this
#   contradicts brand-identity.md "Drop the thin needle" (predicted to die at bar size); the
#   operator picked the needle mark on-device 2026-07-14 and it renders. Flagged, not silently kept.
#   .connecting dots: r=1.5, centres 7.5/12/16.5 — gap ≥ radius so they never merge at Small.
#   .attention bar: (12,5.2)→(12,13.0) w=2.7, + dot r=1.6 at (12,16.6) — gap holds the "!" apart.
#   .noRunway X: (6.34,6.34)→(17.66,17.66) and (17.66,6.34)→(6.34,17.66), w=2.4 — the four vertices
#   sit on the arc's r=8 centreline (the 45° diagonals), so the X reads ring-to-ring, not centred.
emit_status_symbolset() {
  echo "==> status-item bar glyphs (bespoke Cycle-Gauge .symbolset — #437)"
  emit_gauge_symbol GaugeHealthy    gauge-healthy.svg    "${GAUGE_ARROWHEAD}"'<path d="M 12.95 12.55 L 15.70 7.79 A 1.1 1.1 0 0 0 13.80 6.69 L 11.05 11.45 A 1.1 1.1 0 0 0 12.95 12.55 Z"/><path d="M 10.5 12 A 1.5 1.5 0 1 0 13.5 12 A 1.5 1.5 0 1 0 10.5 12 Z"/>'
  emit_gauge_symbol GaugeConnecting gauge-connecting.svg '<path d="M 6 12 A 1.5 1.5 0 1 0 9 12 A 1.5 1.5 0 1 0 6 12 Z"/><path d="M 10.5 12 A 1.5 1.5 0 1 0 13.5 12 A 1.5 1.5 0 1 0 10.5 12 Z"/><path d="M 15 12 A 1.5 1.5 0 1 0 18 12 A 1.5 1.5 0 1 0 15 12 Z"/>'
  emit_gauge_symbol GaugeAttention  gauge-attention.svg  '<path d="M 10.65 5.20 L 10.65 13.00 A 1.35 1.35 0 0 0 13.35 13.00 L 13.35 5.20 A 1.35 1.35 0 0 0 10.65 5.20 Z"/><path d="M 10.4 16.6 A 1.6 1.6 0 1 0 13.6 16.6 A 1.6 1.6 0 1 0 10.4 16.6 Z"/>'
  emit_gauge_symbol GaugeNoRunway   gauge-norunway.svg   '<path d="M 5.49 7.19 L 16.81 18.51 A 1.2 1.2 0 0 0 18.51 16.81 L 7.19 5.49 A 1.2 1.2 0 0 0 5.49 7.19 Z"/><path d="M 16.81 5.49 L 5.49 16.81 A 1.2 1.2 0 0 0 7.19 18.51 L 18.51 7.19 A 1.2 1.2 0 0 0 16.81 5.49 Z"/>'
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

echo "==> menu-bar glyphs (appearance-aware; needle tracks the reading)"
# The glyph is a free-standing colored (non-template) image, so it must clear
# contrast on the bar it sits on. Raw system green/amber measure ~2.0-2.2:1 on a
# LIGHT bar and dissolve, so light bars get the contrast-darkened set. On a DARK
# bar the darkened amber reads muddy-brown, so dark bars get the vivid set.
#   -lightbar  = darkened set   -darkbar = vivid set
# Fallback: if menu-bar appearance can't be detected (wallpaper bleed on a
# translucent bar), ship the -lightbar set universally — it clears 3:1 at BOTH
# luminance extremes. The RESTING glyph ships as a template image instead
# (isTemplate = true) and macOS tints it; the two rendered variants below exist
# for mockups/screenshots only.
R_LIGHTBAR="${GLYPH_INK}"   # macOS renders the template near-black on a light bar
R_DARKBAR="#E9E9EC"         #             ...and near-white on a dark bar

emit_glyph() { # emit_glyph <suffix> <color> <needle>
  derive "${SRC}/glyph.svg" "${DIST}/_g.svg" "$2" "$3" "${GLYPH_INK}"
  rsvg-convert -w 18 -h 18 "${DIST}/_g.svg" -o "${DIST}/glyph-$1.png"
  rsvg-convert -w 36 -h 36 "${DIST}/_g.svg" -o "${DIST}/glyph-$1@2x.png"
}
emit_glyph "resting-lightbar"  "${R_LIGHTBAR}" "${NEEDLE_RESTING}"
emit_glyph "resting-darkbar"   "${R_DARKBAR}"  "${NEEDLE_RESTING}"
emit_glyph "healthy-lightbar"  "${D_HEALTHY}"  "${NEEDLE_HEALTHY}"
emit_glyph "healthy-darkbar"   "${V_HEALTHY}"  "${NEEDLE_HEALTHY}"
emit_glyph "warning-lightbar"  "${D_WARNING}"  "${NEEDLE_WARNING}"
emit_glyph "warning-darkbar"   "${V_WARNING}"  "${NEEDLE_WARNING}"
emit_glyph "critical-lightbar" "${D_CRITICAL}" "${NEEDLE_CRITICAL}"
emit_glyph "critical-darkbar"  "${V_CRITICAL}" "${NEEDLE_CRITICAL}"
emit_glyph "swap-lightbar"     "${D_SWAP}"     "${NEEDLE_RESTING}"
emit_glyph "swap-darkbar"      "${V_SWAP}"     "${NEEDLE_RESTING}"
# resting template master (ship this one; macOS tints it)
cp "${SRC}/glyph.svg" "${DIST}/glyph-resting-template.svg"

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
