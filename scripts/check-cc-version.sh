#!/usr/bin/env bash
# Advisory provenance check: compare the installed Claude Code against the range
# sessiometer's reverse-engineered internals were last verified against.
#
# `sessiometer` depends on reverse-engineered Claude Code internals (the
# keychain-service derivation #100, the credential-refresh lifecycle #101). Those
# were verified against a specific CC range, recorded as the authoritative
# provenance in build/version-compat.md. A CC release outside that range may have
# silently changed the internals — but that drift is caught at RUNTIME, where the
# risk actually lands: the #714 behavioral canary refuses credential writes when
# the keychain derivation drifts, and the #715 in-binary advisory names an
# out-of-range `claude` to the operator. This check is therefore ADVISORY
# provenance for the maintainer — it keeps the recorded "verified against" range
# honest — not a release-blocking gate (demoted in #716; see
# build/release-checklist.md). It is still not a hermetic CI check: CI never
# execs a real `claude`.
#
# This script reads the range from build/version-compat.md (the single source of
# truth) and compares the installed `claude --version` against it.
#
# `claude` resolution: $CLAUDE_BIN if set, else the first `claude` on $PATH
# (mirroring the tool's own $CLAUDE_BIN/$PATH layer; the config `claude_bin`
# override is not consulted here).
#
# Exit codes (unchanged by the #716 demotion — advisory status lives in how the
# release checklist treats a non-zero exit, not in the codes themselves):
#   0  installed CC is within the verified range and the README states it
#   1  installed CC is OUTSIDE the verified range, or the README no longer states the range
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

# README drift guard: the range must live in a user-facing location, so a stale
# README means the PUBLISHED provenance is wrong — reported here (exit 1) so the
# maintainer fixes the README rather than shipping copy that misstates what was
# verified. Assert the README states the current ledger range (checked at the end
# so `claude` findings print first).
#
# #712: check the range as a UNIT, not two independent substrings. Two loose
# `grep -qF` (MIN present AND MAX present, anywhere) pass on a README that states a
# STALE range while mentioning the new bound in an unrelated sentence — exactly the
# drift this guard exists to catch. Require MIN and MAX to appear ADJACENT, separated
# only by markup/dash with no intervening digit, so a stray version elsewhere cannot
# bridge them. The README states the range compactly, e.g. `2.1.181`–`2.1.217`.
readme="$script_dir/../README.md"
readme_ok=1
if [[ -f "$readme" ]]; then
    if [[ "$min" == "$max" ]]; then
        # Degenerate single-version range: assert the one version is present.
        grep -qF "$min" "$readme" || readme_ok=0
    else
        # Escape the dots so grep -E reads them as literal `.`, not any-char.
        min_re="${min//./\\.}"
        max_re="${max//./\\.}"
        grep -qE "${min_re}[^0-9]{1,12}${max_re}" "$readme" || readme_ok=0
    fi
fi

# Resolve the claude binary: $CLAUDE_BIN wins, else $PATH.
claude_bin="${CLAUDE_BIN:-claude}"
if ! command -v "$claude_bin" >/dev/null 2>&1; then
    echo "error: no \`claude\` found (looked for '${claude_bin}'; set \$CLAUDE_BIN or add it to \$PATH)." >&2
    echo "       cannot re-verify against the verified range ${min}-${max}." >&2
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
    echo "ok: Claude Code ${cur} is within the verified range ${min}-${max} (build/version-compat.md)."
else
    status=1
    echo "warning: Claude Code ${cur} is OUTSIDE the verified range ${min}-${max} (advisory — see #716)." >&2
    echo "         sessiometer's reverse-engineered CC internals are unverified on this version;" >&2
    echo "         runtime drift protection: the #714 canary refuses drifted credential writes." >&2
    if ver_le "$cur" "$min"; then
        echo "         ${cur} is BELOW the range — install a verified \`claude\`, or verify the older CC." >&2
    else
        echo "         ${cur} is ABOVE the range — to refresh the provenance, re-verify H3 (fresh-start" >&2
        echo "         adoption) and the #100 keychain-service derivation in build/version-compat.md," >&2
        echo "         then widen the range there and update the README." >&2
    fi
fi

if [[ "$readme_ok" -eq 0 ]]; then
    status=1
    echo "warning: README.md does not state the verified range ${min}-${max}." >&2
    echo "         the user-facing range has drifted from build/version-compat.md — update" >&2
    echo "         the README \`## Prerequisites\` range so the published provenance is accurate." >&2
fi

exit "$status"
