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
#   VERBATIM, as one contiguous unit, modulo markdown emphasis and line wrapping —
#   and must state no OTHER claim in that same shape. The ledger is held to that same
#   exclusivity rule, over the whole file (issue #1354).
#
# Six design choices carry the weight:
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
#      Scoping NARROWED the masking defect; it did not close it, and this paragraph
#      read as though it had until issue #1317. Containment is not exclusivity: a
#      `## Prerequisites` stating the correct claim AND a stale range satisfied the
#      containment test below, so the substitution survived one level further in —
#      inside the pinned section rather than elsewhere in the file. Choice 5 closes
#      that; what remains open after it is stated there rather than left to be found.
#
#      A MISSING or duplicated `## Prerequisites` heading is exit 2, never a pass:
#      the pin is expressed as a section NAME, so a restructure that renames or
#      moves it must be re-pointed deliberately rather than silently unpinned. A
#      guard that evaluated no section is not green.
#
#   5. EXCLUSIVE within that section, not merely contained (issue #1317). The section
#      must state the ledger's claim and NO OTHER statement in that claim's shape.
#      Containment alone asks only whether the right sentence is PRESENT, so the drift
#      shape this surface actually takes — a widening that adds a sentence and leaves
#      the old one behind — passed. Measured on the real tree before choosing: guard,
#      ledger and README verbatim with one stale bullet prepended INSIDE the section,
#      exit 0 with a message identical to the unmutated control's, so the guard was
#      blind to the difference rather than quiet about it.
#
#      This is NOT the exactly-once rule choice 4 rejected, and the difference is a
#      measurement rather than an argument — the unit being counted decides it. Counting
#      version-range PAIRS finds TWO in the live, wholly-correct section: the CC range
#      and the host range, both of them inside the canonical claim itself. That reading
#      is RED on a file with nothing wrong with it, which is choice 4's rejection
#      re-derived here rather than inherited. Counting whole CLAIM-shaped statements
#      finds one there and two on the mutant above. That is the rule below.
#
#      This paragraph used to add that the rule was "not a new invention: the ledger-side
#      check this script already runs (`claim_count > 1`) turned on the other document."
#      As an argument about principle that is sound, and the principle still holds — a
#      document that states the range two ways cannot be trusted about which way it means.
#      As a description of the two MECHANISMS it was wrong, and issue #1354 measured the
#      gap: `claim_count > 1` counts matches of `claim_re`, which PINS the ledger's own
#      bounds, so it caught only the same bounds restated with a different HOST half. The
#      README rule below has always used FREE bounds. The two were never the same check,
#      and the difference ran the wrong way — see choice 6.
#
#      DISTINCT statements, so a section legitimately restating the SAME correct claim
#      twice stays green; counting occurrences instead would re-introduce the false RED
#      that made the wrong unit unusable. T12's permission is untouched — it pins a
#      second correct copy OUTSIDE the section, and this rule reads only within it.
#
#      That SCOPE is pinned by T21 (issue #1353), and it needs a case of its own because
#      T12 cannot carry it: T12's outside copy is CORRECT, so the subtraction below drops
#      it at either scope and it passes a whole-file reading too. What separates them is
#      an outside claim carrying DIFFERENT bounds — legal, since this rule reads only
#      within the section, and rejected by the whole-file reading on a correct README.
#
#      Exit 1, where the ledger's own two-ways case is exit 2. The asymmetry is the two
#      documents' ROLES: an inconsistent AUTHORITY means nothing can be compared, while
#      an inconsistent COPY is simply wrong, and deleting the stale sentence is a fix a
#      maintainer can act on.
#
#      RESIDUAL, stated rather than left to be discovered: this pins the claim's SHAPE,
#      so a stale range inside the section that is NOT claim-shaped — a bare `X`–`Y`
#      carrying no `on macOS … / Darwin …` clause — still passes. Widening the shape to
#      any version-range pair is exactly the reading measured RED above, so closing this
#      last step costs the correct file. Pinned green by T17 so it cannot move in either
#      direction unmeasured.
#
#   6. The LEDGER is held to choice 5's rule too (issue #1354). Until it was, the
#      authority was held to a WEAKER exclusivity standard than the copy it authorises:
#      a ledger stating its canonical claim beside a stale one carrying different bounds
#      matched `claim_re` exactly once and passed, while the same shape of drift in the
#      README was caught by the free-bounds rule of choice 5. Measured against this guard
#      both as PR #1343 left it and as the commit before it left it — exit 0 in both, so
#      the gap is pre-existing rather than introduced by the change that added choice 5.
#
#      Same shape, same unit, two deliberate differences. WHOLE-FILE rather than scoped:
#      the README may legitimately restate the range under another heading (T12), whereas
#      a second authoritative-shaped claim anywhere in the ledger leaves a reader unable
#      to tell which one the constants mean. And exit 2 rather than 1, which is choice 5's
#      own role argument unchanged. Measured green on the real ledger before choosing — it
#      holds exactly ONE claim-shaped statement, because it records superseded ranges as
#      bare pairs with no host clause and reserves the full shape for the authoritative
#      sentence. That is now self-enforcing: if it ever stops being one, this arm says so.
#      The ledger-side residual is therefore the same one choice 5 states, and the
#      ledger's existing convention already lives inside it; T20 pins it green.
#
# A claim that cannot be CONSTRUCTED is a FAILURE (exit 2), never a pass. A gate that
# goes green because it found nothing to compare is the same write-only-copy failure
# this script exists to end, one level up — hence also the absent-README exit 2, which
# deliberately diverges from check-cc-version.sh's tolerance of a missing README (that
# script is advisory and runs on a maintainer's box; this one gates a tree in which
# README.md is tracked and its absence means nothing was evaluated).
#
# Exit codes:
#   0  the README states the ledger's canonical claim, and no other one
#   1  DRIFT — the README's copy disagrees with the ledger (its claim is absent from
#      the pinned section, or that section ALSO states a claim the ledger does not)
#   2  could not determine (README or ledger missing, no claim constructible, the ledger
#      states a claim the ledger's own constants do not support, or the README has no
#      single `## Prerequisites` section to check)
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

# The SAME sentence with FREE bounds. Both documents are checked for exclusivity against
# this one, and it is defined here — beside `claim_re`, not at either use site — because the
# relationship between the two patterns is the whole design: `claim_re` is this shape with
# the ledger's own bounds substituted in, so its matches are a strict SUBSET of this one's.
# That is what makes "every claim-shaped statement, minus the canonical one" a well-formed
# question on either document (issue #1354).
shape_re="\`[0-9]+\.[0-9]+\.[0-9]+\`–\`[0-9]+\.[0-9]+\.[0-9]+\` on macOS \`[^\`]+\`–\`[^\`]+\` / Darwin \`[^\`]+\`"

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

# EXCLUSIVE within the ledger too, not merely self-consistent about its own bounds
# (issue #1354). The two checks above are anchored on `claim_re`, which PINS the bounds
# read from CC_SUPPORTED_{MIN,MAX} — so a ledger stating its canonical claim beside a
# stale one carrying DIFFERENT bounds matches that pattern exactly once and passes both.
# Measured on the fixture tree before choosing, against this guard as PR #1343 left it and
# as the commit before it left it: exit 0 either way, and the pass text byte-identical to a
# clean run's. What `claim_count > 1` actually catches is narrower than the rationale it
# was given — the SAME bounds stated with different HOST halves (T10, exit 2).
#
# That left the AUTHORITY held to a weaker exclusivity standard than the COPY it
# authorises: the README arm below has always used the free-bounds shape, so the document
# that merely REPEATS the range could not carry a contradictory second claim while the
# document that DEFINES it could. This closes that direction with the same rule, and the
# unit is the same one design choice 5 settled on — whole CLAIM-shaped statements, not
# version-range PAIRS, which is the reading measured RED on a wholly-correct file.
#
# WHOLE-FILE, where the README arm is scoped to one section, and the asymmetry that
# remains is deliberate rather than an unfinished symmetry: the README may legitimately
# restate the range under another heading (T12 pins that permission), whereas a second
# authoritative-shaped claim ANYWHERE in the ledger leaves a reader unable to tell which
# one the constants mean. Measured on the real ledger before choosing: exactly ONE
# claim-shaped statement. Its widening history is written as bare pairs
# (`2.1.197` → `2.1.217`) with no `on macOS … / Darwin …` clause, so the ledger's own
# convention for recording superseded ranges already lives inside what this rule permits.
#
# Exit 2, matching the two checks above rather than the README arm's exit 1: the role
# asymmetry those document is unchanged — an inconsistent AUTHORITY means nothing can be
# compared, while an inconsistent COPY is simply wrong.
#
# RESIDUAL, the ledger-side twin of the one design choice 5 states: this pins the claim's
# SHAPE, so a stale range in the ledger carrying no host clause still passes — which is
# precisely what its widening history relies on. Pinned green by T20 so it cannot move
# unmeasured in either direction.
#
# Built exactly as the README arm's `others` is, and the SUBTRACTION is what carries it:
# expressed as "more than one claim-shaped OCCURRENCE" this rule would go RED on a ledger
# restating its own claim verbatim (T19). `sort -u` decides no exit code here — measured,
# dropping it alone leaves the whole suite green, because `grep -vxF` drops every line
# equal to the claim rather than the first; what it owns is the diagnostic below, where a
# statement repeated twice is listed once. Same finding as the README arm's.
ledger_others="$(normalize < "$ledger" | grep -oE "$shape_re" | sort -u | grep -vxF -- "$claim" || true)"
if [[ -n "$ledger_others" ]]; then
    other_count="$(printf '%s\n' "$ledger_others" | wc -l | tr -d '[:space:]')"
    echo "error: $ledger states its canonical range claim AND ${other_count} other claim(s) in the" >&2
    echo "       same shape, carrying different bounds:" >&2
    printf '%s\n' "$ledger_others" | sed 's/^/         /' >&2
    echo "       its CC_SUPPORTED_MIN/MAX lines (${min}-${max}) support only: ${claim}" >&2
    echo "       the ledger is the AUTHORITY for this range, so it must state it exactly one way" >&2
    echo "       before the README can be checked against it — a reader landing on the statement(s)" >&2
    echo "       above reads a range the constants do not support. Delete or update them, or, if" >&2
    echo "       one is a superseded range being recorded deliberately, state it without the" >&2
    echo "       \`on macOS ... / Darwin ...\` clause, as this ledger's other widening records do." >&2
    exit 2
fi

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

# Design choice 5: containment is not exclusivity. Every statement in the CLAIM'S SHAPE,
# with FREE bounds — `claim_re` pins the ledger's specific bounds, and a stale sentence by
# definition carries the OLD ones, so it is invisible to that pattern and needs `shape_re`
# (defined above, shared with the ledger arm). `[^\`]+` cannot cross a backtick, so a
# match cannot span two adjacent statements.
# SUBTRACTING the claim is what keeps a section restating the SAME correct claim twice
# green (T16): `grep -v` drops EVERY line equal to the claim, not merely the first, so no
# copy survives into `others`. Not `sort -u` — measured, removing that stage alone leaves
# the whole suite green, T16 included, and it can only shorten a non-empty list rather than
# empty one, so it decides no exit code here. What it does own is the DIAGNOSTIC below: a
# stale statement repeated twice is listed and counted once rather than twice. `-x` is
# belt-and-braces — without it a line merely CONTAINING the claim would be dropped, which
# this shape cannot produce (`grep -o` returns non-overlapping matches and the shape cannot
# nest inside itself), so no fixture exercises it either.
others="$(printf '%s' "$section_norm" | grep -oE "$shape_re" | sort -u | grep -vxF -- "$claim" || true)"

if printf '%s' "$section_norm" | grep -qF -- "$claim"; then
    if [[ -z "$others" ]]; then
        echo "ok: README.md \`## Prerequisites\` states the ledger's verified range — ${claim}"
        echo "    (1 claim compared: the Claude Code range and the host range, as one unit;"
        echo "     and that section states no other claim in the same shape)"
        # Names the ledger arm too, so the pass text reports everything that was actually
        # evaluated rather than half of it. A gate whose green understates its own coverage
        # is how the ledger arm's absence went unnoticed for as long as it did (issue #1354).
        echo "    (and the ledger itself states that claim exactly one way, whole-file)"
        exit 0
    fi

    # The claim is present AND so is a range claim the ledger does not make. A reader who
    # lands on the stale sentence reads a range that was never verified, so the correct
    # copy beside it does not redeem the section.
    other_count="$(printf '%s\n' "$others" | wc -l | tr -d '[:space:]')"
    echo "error: README.md \`## Prerequisites\` states the ledger's claim AND ${other_count} other range" >&2
    echo "       claim(s) the ledger does not make:" >&2
    printf '%s\n' "$others" | sed 's/^/         /' >&2
    echo "       ledger states: ${claim}" >&2
    echo "       this is the shape a range widening leaves behind — a new sentence added and" >&2
    echo "       the old one left in place. A correct copy in the same section does not redeem" >&2
    echo "       a stale one a reader can land on: delete or update the statement(s) above." >&2
    exit 1
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
