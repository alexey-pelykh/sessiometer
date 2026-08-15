#!/usr/bin/env bash
# Fail the build if the README's user-facing "verified against" copy has drifted
# from build/version-compat.md, the authoritative range ledger (issue #1279).
#
# Three surfaces carry this range. Two were pinned; the third was not:
#
#   src/cc_version.rs      pinned  — `the_baked_range_matches_the_ledger` makes the
#                                    ledger an `include_str!` input of the TEST build
#   sessiometer --version  pinned  — formats those same baked constants
#   README.md              UNPINNED — a third copy nothing compared against anything
#
# So the README stayed correct only for as long as nobody widened the other two. The
# ledger says `scripts/check-cc-version.sh` catches a stale README, and it does own
# that rule — but it is a RELEASE-TIME advisory wired into no workflow, and its README
# verdict is computed before a `claude --version` probe and only REPORTED after it, so
# on a machine with no `claude` (every CI runner: "CI never execs a real `claude`",
# its own header) the script exits 2 and the verdict never prints. A guard that cannot
# run where the drift lands is not a backstop. This one is hermetic — it reads two
# committed files and execs nothing — so it runs in CI on every PR.
#
# THE RULE
#   The README's `## Prerequisites` section must state the ledger's canonical claim
#   VERBATIM, as one contiguous unit, modulo markdown emphasis and line wrapping.
#
# Four design choices carry the weight:
#
#   1. ONE UNIT, not two loose substrings. Issue #712 measured the alternative: a
#      README stating a STALE range while mentioning the new bound in an unrelated
#      sentence satisfies "MIN appears AND MAX appears" and passes. The claim is
#      therefore matched as a single contiguous string, so no distant mention can
#      bridge a stale range. This also anchors MAX's trailing edge for free — MAX is
#      followed by a literal backtick, so a README stating `2.1.2179` cannot satisfy
#      a claim ending `2.1.217`, which is issue #721's prefix-match defect.
#
#   2. The claim is BUILT FROM the machine-readable `- CC_SUPPORTED_{MIN,MAX}` lines,
#      then LOCATED in the ledger's prose by that anchor. The indirection is the point:
#      it is what lets this guard cover the host range (`on macOS ... / Darwin ...`),
#      which has no machine-readable form anywhere, without guessing which of the
#      ledger's ~40 macOS mentions is the authoritative one. It also means the ledger's
#      own prose copy of the range is checked against its own constants as a side
#      effect — if those two disagree the anchor finds nothing and this exits 2.
#
#   3. Both data or neither. README.md:37 states the CC range and the host range in
#      one sentence; both are copies, both drift the same way. Pinning the first and
#      leaving the second would be a guard that reads as covering the sentence while
#      covering half of it.
#
#   4. SCOPED to `## Prerequisites` — the surface this guard's own failure text names
#      and the ledger's consumer bullet pins. Searching the WHOLE README lets a
#      correct copy anywhere stand in for a stale sentence in the pinned section:
#      issue #712's defect one level up, a distant whole CLAIM bridging a stale one
#      rather than a distant BOUND. Measured before choosing, not assumed — with the
#      section stating a stale range and a correct claim appended under an unrelated
#      heading, the unscoped rule exited 0.
#
#      The alternative considered was requiring the claim to occur exactly ONCE in
#      the file. Measured on that same mutant it ALSO exits 0 — the stale copy does
#      not match the claim, so the claim genuinely does occur once — so it does not
#      fix this defect at all. Reinterpreted as counting range-SHAPED statements it
#      does fire, but then it forbids the README from ever restating the range
#      legitimately and goes RED on a correct file. Scoping permits a second correct
#      copy outside the section and fails only on the surface actually pinned, which
#      is the failure a maintainer can act on.
#
#      A MISSING or duplicated `## Prerequisites` heading is exit 2, never a pass:
#      the pin is expressed as a section NAME, so a restructure that renames or
#      moves it must be re-pointed deliberately rather than silently unpinned. A
#      guard that evaluated no section is not green.
#
# A claim that cannot be CONSTRUCTED is a FAILURE (exit 2), never a pass. A gate that
# goes green because it found nothing to compare is the same write-only-copy failure
# this script exists to end, one level up — hence also the absent-README exit 2, which
# deliberately diverges from check-cc-version.sh's tolerance of a missing README (that
# script is advisory and runs on a maintainer's box; this one gates a tree in which
# README.md is tracked and its absence means nothing was evaluated).
#
# Exit codes:
#   0  the README states the ledger's canonical claim
#   1  DRIFT — the README's copy disagrees with the ledger
#   2  could not determine (README or ledger missing, no claim constructible, or the
#      README has no single `## Prerequisites` section to check)
#
# Run locally:  ./scripts/check-readme-cc-range.sh
set -euo pipefail

# Byte-deterministic matching, so the macOS dev box (BSD grep/tr) and the ubuntu CI
# runner (GNU) agree on the EN DASH (U+2013) both documents use between the bounds.
export LC_ALL=C

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ledger="$script_dir/../build/version-compat.md"
readme="$script_dir/../README.md"

if [[ ! -f "$ledger" ]]; then
    echo "error: cannot find the range ledger at $ledger" >&2
    exit 2
fi
if [[ ! -f "$readme" ]]; then
    echo "error: cannot find the README at $readme — nothing to compare, so this is a" >&2
    echo "       FAILURE rather than a pass (a gate that evaluated nothing is not green)." >&2
    exit 2
fi

# Markdown emphasis and line wrapping are formatting, not content: the ledger states the
# claim unemphasised and mid-paragraph, the README bolds the range and wraps the sentence
# across two lines. Strip `*` and collapse every whitespace run to one space so the two
# render to the same bytes. (This also strips literal asterisks; neither document has one
# inside the claim.)
normalize() { tr -d '*' | tr '\n' ' ' | tr -s '[:space:]' ' '; }

# The machine-readable data lines, read with the SAME anchored grep check-cc-version.sh
# uses — the `-` list-item prefix is what excludes the HTML comment above them that
# documents the format. build/version-compat.md § Supported Claude Code range owns it.
extract_bound() { # $1 = MIN|MAX
    grep -oE "^-[[:space:]]*CC_SUPPORTED_$1:[[:space:]]*[0-9]+\.[0-9]+\.[0-9]+" "$ledger" |
        grep -oE "[0-9]+\.[0-9]+\.[0-9]+" | head -n1
}
min="$(extract_bound MIN || true)"
max="$(extract_bound MAX || true)"

if [[ -z "$min" || -z "$max" ]]; then
    echo "error: could not read CC_SUPPORTED_MIN/MAX from $ledger" >&2
    echo "       expected '- CC_SUPPORTED_MIN: x.y.z' and '- CC_SUPPORTED_MAX: x.y.z' lines." >&2
    exit 2
fi

# Escape the dots so grep -E reads them as literal `.` rather than any-char.
min_re="${min//./\\.}"
max_re="${max//./\\.}"

# The canonical claim, as the ledger states it:
#   `MIN`–`MAX` on macOS `X`–`Y` / Darwin `Z`
# Anchored on the two bounds above, so this locates the ONE authoritative sentence rather
# than any of the ledger's other macOS mentions. The host half is shape-matched (`X`–`Y` /
# Darwin `Z`) rather than free-form: if the ledger ever restates the host range in another
# shape this finds nothing and exits 2, which is the correct loud failure — a reformatted
# ledger needs this pattern revisited, not silently skipped.
claim_re="\`${min_re}\`–\`${max_re}\` on macOS \`[^\`]+\`–\`[^\`]+\` / Darwin \`[^\`]+\`"

# Newline-separated rather than an array: macOS ships bash 3.2, where `mapfile` does not
# exist, and this must run on a maintainer's Mac as readily as on the ubuntu CI runner.
claims="$(normalize < "$ledger" | grep -oE "$claim_re" | sort -u || true)"
claim_count=0
if [[ -n "$claims" ]]; then
    claim_count="$(printf '%s\n' "$claims" | wc -l | tr -d '[:space:]')"
fi

if [[ "$claim_count" -eq 0 ]]; then
    echo "error: no canonical range claim found in $ledger" >&2
    echo "       looked for: \`${min}\`–\`${max}\` on macOS \`X\`–\`Y\` / Darwin \`Z\`" >&2
    echo "       either the ledger's PROSE range has drifted from its own" >&2
    echo "       CC_SUPPORTED_MIN/MAX lines (${min}-${max}), or the sentence was" >&2
    echo "       reshaped and this guard's pattern needs updating. Not a pass either way." >&2
    exit 2
fi
if [[ "$claim_count" -gt 1 ]]; then
    echo "error: $ledger states the range claim ${claim_count} different ways:" >&2
    printf '         %s\n' "$claims" >&2
    echo "       the ledger must be self-consistent before the README can be checked against it." >&2
    exit 2
fi
claim="$(printf '%s\n' "$claims" | head -n1)"

# The PINNED SURFACE is the README's `## Prerequisites` section, not the whole file.
# Scoping here is what makes the failure text below true; see design choice 4.
heading_count="$(grep -cE '^## Prerequisites[[:space:]]*$' "$readme" || true)"
if [[ "$heading_count" -ne 1 ]]; then
    echo "error: expected exactly one '## Prerequisites' heading in $readme, found ${heading_count}." >&2
    echo "       this guard pins that SECTION, so it cannot check a README that has no" >&2
    echo "       single one — nothing was evaluated, which is a FAILURE, not a pass. If" >&2
    echo "       the section was renamed or moved, re-point this guard deliberately." >&2
    exit 2
fi

# From the heading to the next `## ` heading (or EOF).
section="$(awk '
    /^## Prerequisites[ \t]*$/ { inside = 1; next }
    /^## / { if (inside) exit }
    inside { print }
' "$readme")"

section_norm="$(printf '%s' "$section" | normalize)"

if printf '%s' "$section_norm" | grep -qF -- "$claim"; then
    echo "ok: README.md \`## Prerequisites\` states the ledger's verified range — ${claim}"
    echo "    (1 claim compared: the Claude Code range and the host range, as one unit)"
    exit 0
fi

# Drift. Report WHICH half moved: the sentence carries two independently-copied data and
# "they disagree" is not actionable on its own.
host="$(printf '%s' "$claim" | grep -oE "on macOS \`[^\`]+\`–\`[^\`]+\` / Darwin \`[^\`]+\`" || true)"

cc_ok=0
host_ok=0
# Markup-agnostic adjacency for the CC half, mirroring check-cc-version.sh's rule, so the
# diagnosis distinguishes "stated differently" from "stated wrongly".
printf '%s' "$section_norm" | grep -qE "${min_re}[^0-9]{1,12}${max_re}([^0-9]|$)" && cc_ok=1
[[ -n "$host" ]] && printf '%s' "$section_norm" | grep -qF -- "$host" && host_ok=1

echo "error: README.md \`## Prerequisites\` has drifted from the range ledger ($ledger)." >&2
echo "       ledger states: ${claim}" >&2
if [[ "$cc_ok" -eq 0 ]]; then
    echo "       -> the CLAUDE CODE range ${min}-${max} is not stated adjacently in that section." >&2
fi
if [[ "$host_ok" -eq 0 && -n "$host" ]]; then
    echo "       -> the HOST range is not stated as: ${host}" >&2
fi
if [[ "$cc_ok" -eq 1 && "$host_ok" -eq 1 ]]; then
    echo "       -> both halves are present in that section but not as one contiguous claim;" >&2
    echo "          they must be stated together, so a distant mention cannot stand in for a" >&2
    echo "          stale range." >&2
fi
echo "       fix that section's sentence so the PUBLISHED provenance matches what was" >&2
echo "       actually verified — the ledger is authoritative, the README is a copy." >&2
exit 1
