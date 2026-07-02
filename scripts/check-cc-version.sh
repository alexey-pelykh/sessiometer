#!/usr/bin/env bash
# Re-verify the installed Claude Code against sessiometer's supported CC range.
#
# `sessiometer` depends on reverse-engineered Claude Code internals (the
# keychain-service derivation #100, the credential-refresh lifecycle #101). Those
# were verified against a specific CC range, recorded as the authoritative source
# of truth in build/version-compat.md. A CC release outside that range may have
# silently changed the internals, and sessiometer would then target the wrong
# keychain item with no other signal — so the supported range is a pre-release
# gate (see build/release-checklist.md), not a hermetic CI check (CI never execs
# a real `claude`).
#
# This script reads the range from build/version-compat.md (the single source of
# truth) and compares the installed `claude --version` against it.
#
# `claude` resolution: $CLAUDE_BIN if set, else the first `claude` on $PATH
# (mirroring the tool's own $CLAUDE_BIN/$PATH layer; the config `claude_bin`
# override is not consulted here).
#
# Exit codes:
#   0  installed CC is within the supported range and the README states it
#   1  installed CC is OUTSIDE the supported range, or the README no longer states the range
#   2  could not determine (no `claude`, unparseable version, or no range in the ledger)
set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ledger="$script_dir/../build/version-compat.md"

if [[ ! -f "$ledger" ]]; then
    echo "error: cannot find the version ledger at $ledger" >&2
    exit 2
fi

# Parse the machine-readable `- CC_SUPPORTED_MIN: x.y.z` / `- CC_SUPPORTED_MAX: x.y.z`
# lines. build/version-compat.md § Supported Claude Code range owns this format.
extract_range() { # $1 = MIN|MAX — only the `- CC_SUPPORTED_x: n.n.n` data line, never a comment
    grep -oE "^-[[:space:]]*CC_SUPPORTED_$1:[[:space:]]*[0-9]+\.[0-9]+\.[0-9]+" "$ledger" |
        grep -oE "[0-9]+\.[0-9]+\.[0-9]+" | head -n1
}
min="$(extract_range MIN || true)"
max="$(extract_range MAX || true)"

if [[ -z "$min" || -z "$max" ]]; then
    echo "error: could not read CC_SUPPORTED_MIN/MAX from $ledger" >&2
    echo "       expected '- CC_SUPPORTED_MIN: x.y.z' and '- CC_SUPPORTED_MAX: x.y.z' lines." >&2
    exit 2
fi

# README drift guard: AC1 requires the range in a user-facing location, so a stale
# README is a release failure, not a silent regression. Assert README states the
# current ledger range (checked at the end so `claude` findings print first).
readme="$script_dir/../README.md"
readme_ok=1
if [[ -f "$readme" ]] && { ! grep -qF "$min" "$readme" || ! grep -qF "$max" "$readme"; }; then
    readme_ok=0
fi

# Resolve the claude binary: $CLAUDE_BIN wins, else $PATH.
claude_bin="${CLAUDE_BIN:-claude}"
if ! command -v "$claude_bin" >/dev/null 2>&1; then
    echo "error: no \`claude\` found (looked for '${claude_bin}'; set \$CLAUDE_BIN or add it to \$PATH)." >&2
    echo "       cannot re-verify against the supported range ${min}-${max}." >&2
    exit 2
fi

raw="$("$claude_bin" --version 2>/dev/null || true)"
# Anchor to a version at the START of a line (optionally `v`-prefixed): `claude
# --version` prints `<x.y.z> (Claude Code)`, so a stray version-like number
# elsewhere in the output can't hijack the parse (weird formats degrade to the
# safe exit-2 below rather than a false pass/fail).
cur="$(printf '%s\n' "$raw" | grep -m1 -oE '^[[:space:]]*v?[0-9]+\.[0-9]+\.[0-9]+' |
    grep -oE '[0-9]+\.[0-9]+\.[0-9]+' | head -n1 || true)"
if [[ -z "$cur" ]]; then
    echo "error: could not parse a version from \`${claude_bin} --version\` (got: '${raw}')." >&2
    exit 2
fi

# `a <= b` via version sort: the lower of {a, b} must be a.
ver_le() { [[ "$(printf '%s\n%s\n' "$1" "$2" | sort -V | head -n1)" == "$1" ]]; }

status=0
if ver_le "$min" "$cur" && ver_le "$cur" "$max"; then
    echo "ok: Claude Code ${cur} is within the supported range ${min}-${max} (build/version-compat.md)."
else
    status=1
    echo "warning: Claude Code ${cur} is OUTSIDE the supported range ${min}-${max}." >&2
    echo "         sessiometer's reverse-engineered CC internals are unverified on this version." >&2
    if ver_le "$cur" "$min"; then
        echo "         ${cur} is BELOW the range — install a supported \`claude\`, or verify the older CC." >&2
    else
        echo "         ${cur} is ABOVE the range — re-verify H3 (fresh-start adoption) and the #100" >&2
        echo "         keychain-service derivation in build/version-compat.md, then widen the range there" >&2
        echo "         and update the README." >&2
    fi
fi

if [[ "$readme_ok" -eq 0 ]]; then
    status=1
    echo "warning: README.md does not state the supported range ${min}-${max}." >&2
    echo "         the user-facing range has drifted from build/version-compat.md — update" >&2
    echo "         the README \`## Prerequisites\` range to match." >&2
fi

exit "$status"
