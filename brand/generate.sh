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
