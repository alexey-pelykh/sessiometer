#!/bin/bash
# Local Dev-ID release for the Sessiometer menu-bar app (#171):
#   generate -> build (unsigned, universal) -> inside-out sign -> notarize -> staple -> verify.
# Pass --sign-only to stop after signing (CI notarizes separately with an ASC API key).
set -euo pipefail

SIGN_ONLY=0
[ "${1:-}" = "--sign-only" ] && SIGN_ONLY=1

IDENTITY="A85B67D61EF9F66750916FC78EEF5D35CF7A8C63"   # Developer ID Application: Oleksii PELYKH SARL (7KNN3SX5TJ)
NOTARY_PROFILE="sessiometer-notary"
MENUBAR="$(cd "$(dirname "$0")/.." && pwd)"           # apps/menubar
ENTITLEMENTS="${MENUBAR}/Sessiometer.entitlements"
APP="${MENUBAR}/.build/Build/Products/Release/Sessiometer.app"

cd "${MENUBAR}"
xcodegen generate                                     # Menubar.xcodeproj is git-ignored

# Universal Release build, UNSIGNED. The embed-daemon postBuildScript builds+lipos the daemon
# into Contents/Helpers/. We sign manually below for full control of order/flags/timestamp.
rm -rf .build/Build/Products/Release
xcodebuild build \
  -project Menubar.xcodeproj -scheme Menubar -configuration Release \
  -derivedDataPath .build -destination 'generic/platform=macOS' \
  ARCHS="arm64 x86_64" ONLY_ACTIVE_ARCH=NO \
  CODE_SIGNING_ALLOWED=NO

DAEMON="${APP}/Contents/Helpers/sessiometer"
[ -f "${DAEMON}" ] || { echo "FATAL: daemon not embedded at ${DAEMON}"; exit 1; }

/usr/bin/xattr -cr "${APP}"                           # strip quarantine/resource forks that break codesign

# INSIDE-OUT: daemon FIRST (hardened runtime + secure timestamp, NO entitlements)...
codesign --force --options runtime --timestamp --sign "${IDENTITY}" "${DAEMON}"
# ...then the .app LAST. NO --deep (the daemon is already signed; the outer seal records its cdhash).
codesign --force --options runtime --timestamp \
  --entitlements "${ENTITLEMENTS}" --sign "${IDENTITY}" "${APP}"

# Verify (--deep is CORRECT for VERIFY; it is only an anti-pattern for SIGN).
codesign --verify --deep --strict --verbose=2 "${APP}"
echo "--- daemon entitlements (expect: none) ---";              codesign -d --entitlements - "${DAEMON}" 2>&1 || true
echo "--- app entitlements (expect: get-task-allow=false) ---"; codesign -d --entitlements - "${APP}" 2>&1 || true
spctl -a -t exec -vv "${APP}" 2>&1 || true            # pre-notarization: "rejected" but source must be Developer ID

if [ "${SIGN_ONLY}" = "1" ]; then
  echo "signed (--sign-only); skipping notarize."
  exit 0
fi

# --- Notarize + staple ---
ZIP="${MENUBAR}/.build/Sessiometer.zip"
/usr/bin/ditto -c -k --keepParent "${APP}" "${ZIP}"   # ditto (not zip) preserves symlinks/xattrs for upload
xcrun notarytool submit "${ZIP}" --keychain-profile "${NOTARY_PROFILE}" --wait
xcrun stapler staple "${APP}"                          # staple the ticket to the .app (never the zip)
xcrun stapler validate "${APP}"
spctl -a -t exec -vv "${APP}"                          # expect: accepted, source=Notarized Developer ID
echo "DONE: signed + notarized + stapled -> ${APP}"
