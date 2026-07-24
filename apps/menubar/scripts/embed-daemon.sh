#!/bin/bash
# Build the universal `sessiometer` daemon and embed it at Contents/Helpers/sessiometer.
# Runs as a Menubar postBuildScript. RELEASE-ONLY: the Debug `xcodebuild test
# CODE_SIGNING_ALLOWED=NO` path (the CI `swift` job) must never build Rust.
#
# Contents/Helpers/ (NOT Contents/MacOS/) because the app executable is `Sessiometer`
# (PRODUCT_NAME) and the daemon is `sessiometer` (lowercase) — those collide on
# case-insensitive APFS. The bundled LaunchAgent plist's BundleProgram points here.
set -euo pipefail

if [ "${CONFIGURATION}" != "Release" ]; then
  echo "note: [embed-daemon] skip for CONFIGURATION=${CONFIGURATION} (Release-only)"
  exit 0
fi

REPO_ROOT="$(cd "${SRCROOT}/../.." && pwd)"

# Stable shared cargo target dir → incremental across a normal `cargo build` and this phase.
# NOT Xcode DerivedData (avoids churn, keeps the cache warm).
export CARGO_TARGET_DIR="${REPO_ROOT}/target"

# Xcode's Run Script PATH is minimal — make rustup/cargo + Homebrew reachable.
export PATH="${HOME}/.cargo/bin:/opt/homebrew/bin:/usr/local/bin:${PATH}"
if [ -f "${HOME}/.cargo/env" ]; then . "${HOME}/.cargo/env"; fi

# Idempotent (fast no-op when present).
rustup target add aarch64-apple-darwin x86_64-apple-darwin

cargo build --release --locked --manifest-path "${REPO_ROOT}/Cargo.toml" --target aarch64-apple-darwin
cargo build --release --locked --manifest-path "${REPO_ROOT}/Cargo.toml" --target x86_64-apple-darwin

DEST="${BUILT_PRODUCTS_DIR}/${WRAPPER_NAME}/Contents/Helpers"
mkdir -p "${DEST}"
lipo -create \
  "${CARGO_TARGET_DIR}/aarch64-apple-darwin/release/sessiometer" \
  "${CARGO_TARGET_DIR}/x86_64-apple-darwin/release/sessiometer" \
  -output "${DEST}/sessiometer"

/usr/bin/xattr -c "${DEST}/sessiometer" 2>/dev/null || true   # pre-empt later codesign xattr errors
echo "note: [embed-daemon] $(lipo -info "${DEST}/sessiometer")"
