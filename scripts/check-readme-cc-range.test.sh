#!/usr/bin/env bash
# Self-contained falsifier + regression test for check-readme-cc-range.sh (issue #1279).
# Builds a throwaway tree the guard resolves relative to its own location and exercises
# the cases that define its contract.
#
# The falsifiers below each fail against a specific wrong implementation:
#
#   T2   kills a two-independent-substrings check (issue #712's measured defect)
#   T3   kills a guard that pins the Claude Code range and leaves the HOST range
#   T5   kills a MAX prefix-match (issue #721: `2.1.2179` satisfying `2.1.217`)
#   T7   kills a guard that passes when no claim could be constructed
#   T8   kills a guard that passes on an absent README (nothing evaluated)
#   T9   kills a guard that accepts the two halves stated far apart
#   T10  kills a guard that silently picks the first of several disagreeing claims
#   T11  kills an UNSCOPED guard — one searching the whole README, so a correct copy
#        in an unrelated section stands in for a stale `## Prerequisites` sentence
#   T12  kills the rejected alternative to T11's fix: requiring the claim to occur
#        exactly ONCE in the file, which goes RED on a legitimate second copy
#   T13  kills a scoped guard that passes when the pinned section is absent
#   T14  kills a scoped guard that passes when the pinned section is DUPLICATED —
#        T13's other half, which one `-ne 1` enforces and a `-lt 1` mutant drops
#   T15  kills a CONTAINMENT-ONLY scoped guard — one asking whether the claim is
#        present, so a stale claim beside it in the same section passes (issue #1317)
#   T16  kills T15's rule written as a COUNT of claim-shaped statements rather than a
#        count of DISTINCT ones, which goes RED on a section restating itself correctly
#
# T11/T12 and T15/T16 are two RED/GREEN pairs of the same shape, one section apart: each
# pins a masking defect closed and, beside it, the correct file the over-broad fix would
# have rejected. A suite without them goes green on every defect this gate exists to
# prevent — or red on files with nothing wrong with them.
#
# T17 is not a falsifier but a BOUNDARY: it pins the residual T15's rule leaves open, so
# that widening or narrowing it is a deliberate, measured act rather than a silent one.
#
# MUTATION-VALIDATED. A suite that passes against the correct implementation is no
# evidence it would catch a wrong one, so each falsifier was checked against the mutant
# it targets — the guard was rewritten to the wrong rule and the test confirmed RED. The
# mutation log is in the PR body for issue #1279. T14 was added later (issue #1314), and
# T15-T17 later still (issue #1317); each one's mutation log is in its own commit body.
#
# The fixture ledger carries a range and a host range independent of the repo's real
# ones, so a future range widening never touches this test.
#
# Every fixture README is written through write_readme, which wraps the case's body in
# the `## Prerequisites` heading the guard pins and closes it with an unrelated trailing
# section — the real README's shape. Cases needing a specific multi-section layout
# (T11-T14) build their fixture directly instead.
#
# Run locally:  ./scripts/check-readme-cc-range.test.sh
set -euo pipefail

here="$(cd "$(dirname "$0")" && pwd)"
guard="$here/check-readme-cc-range.sh"

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

# The guard resolves its ledger and README relative to its OWN location
# ($script_dir/../build/version-compat.md, $script_dir/../README.md), so stand up a fake
# tree beside a copy of the guard under test:
#   $work/scripts/check-readme-cc-range.sh  (a copy of the real guard — always current)
#   $work/build/version-compat.md           (a hermetic fixture ledger, NOT the repo's)
#   $work/README.md                         (rewritten per case)
mkdir -p "$work/scripts" "$work/build"
cp "$guard" "$work/scripts/check-readme-cc-range.sh"
chmod +x "$work/scripts/check-readme-cc-range.sh"
sut="$work/scripts/check-readme-cc-range.sh"

# A fixture ledger in the real shape: the machine-readable pair, the HTML comment above
# them the `-` prefix must exclude, and an authoritative prose sentence stating both the
# Claude Code range and the host range.
write_ledger() { # $1=MIN $2=MAX $3=prose-range $4=host-clause
    cat > "$work/build/version-compat.md" <<EOF
# fixture ledger (hermetic — not the repo's real range)

<!-- Machine-readable: keep the \`- CC_SUPPORTED_MIN: x.y.z\` format stable. -->

- CC_SUPPORTED_MIN: $1
- CC_SUPPORTED_MAX: $2

Every assumption recorded in this ledger was verified against Claude Code in $3
$4. A CC release outside this range may silently change those internals.

Elsewhere this file mentions macOS \`9.9.9\` in an unrelated measurement note, which the
claim anchor must not mistake for the authoritative host range.
EOF
}

# The default fixture: ledger range 3.2.100–3.2.140 on macOS 30.1.1–30.1.2 / Darwin 29.x.
default_ledger() {
    write_ledger 3.2.100 3.2.140 '`3.2.100`–`3.2.140`' 'on macOS `30.1.1`–`30.1.2` / Darwin `29.x`'
}

# Wrap a case's body in the section the guard pins. The trailing `## Quickstart` is
# what makes the section BOUNDED — without it a scoped guard reading to EOF would be
# indistinguishable from an unscoped one on every fixture here.
write_readme() { # section body on stdin
    {
        printf -- '# sessiometer (fixture README)\n\n'
        printf -- '## Prerequisites\n\n'
        cat
        printf -- '\n## Quickstart\n\nAn unrelated section stating no range at all.\n'
    } > "$work/README.md"
}

pass=0
fail=0
check() { # <label> <expected-exit> <actual-exit>
    if [ "$2" = "$3" ]; then
        printf 'PASS  %s (exit %s)\n' "$1" "$3"
        pass=$((pass + 1))
    else
        printf 'FAIL  %s (expected exit %s, got %s)\n' "$1" "$2" "$3"
        printf '      output: %s\n' "$(tr '\n' '|' < "$work/out.txt")"
        fail=$((fail + 1))
    fi
}

check_out() { # <label> <needle> — assert the last run's output contained <needle>
    if grep -qF -- "$2" "$work/out.txt"; then
        printf 'PASS  %s (reported "%s")\n' "$1" "$2"
        pass=$((pass + 1))
    else
        printf 'FAIL  %s (output missing "%s")\n' "$1" "$2"
        printf '      got: %s\n' "$(tr '\n' '|' < "$work/out.txt")"
        fail=$((fail + 1))
    fi
}

run() { # capture exit code without tripping set -e; stdout+stderr -> out.txt
    local rc
    set +e
    "$sut" > "$work/out.txt" 2>&1
    rc=$?
    set -e
    echo "$rc"
}

# T1: the REAL README shape — the range bolded, the sentence wrapped mid-claim (the live
# README breaks between "Darwin" and "`29.x`"). GREEN. Guards against the guard
# false-FAILing on emphasis or line wrapping, which is how a correct README would get
# rejected and the gate then weakened to shut it up.
default_ledger
{
    printf -- '- A Claude Code version the internals were **verified against**. The range is\n'
    printf -- '  currently **`3.2.100`–`3.2.140`** on macOS `30.1.1`–`30.1.2` / Darwin\n'
    printf -- '  `29.x`. This is provenance, not a compatibility gate.\n'
} | write_readme
check "real README shape (bold + wrapped mid-claim) passes" 0 "$(run)"

# T2 (#712): README states a STALE range and mentions the new MAX in an unrelated
# sentence. Both bounds appear as substrings, so a two-independent-greps check passes —
# but the stated range is wrong. RED.
default_ledger
{
    printf -- '- verified against **`3.2.100`–`3.2.120`** on macOS `30.1.1`–`30.1.2` / Darwin `29.x`.\n'
    printf -- '- Note: `3.2.140` is not yet verified.\n'
} | write_readme
check "stale range + stray new-version mention is REJECTED" 1 "$(run)"

# T3: THE new-coverage falsifier. The Claude Code range is stated correctly; the HOST
# range is stale. A guard pinning only the CC range passes this. RED, and the diagnosis
# must name the host half specifically.
default_ledger
printf -- '- verified against **`3.2.100`–`3.2.140`** on macOS `30.1.0`–`30.1.1` / Darwin `29.x`.\n' \
    | write_readme
check "correct CC range with a STALE HOST range is REJECTED" 1 "$(run)"
check_out "  and the diagnosis names the host half" "the HOST range is not stated as"

# T4: the mirror — host range correct, Claude Code range stale. RED, naming the CC half.
default_ledger
printf -- '- verified against **`3.2.100`–`3.2.139`** on macOS `30.1.1`–`30.1.2` / Darwin `29.x`.\n' \
    | write_readme
check "correct host range with a STALE CC range is REJECTED" 1 "$(run)"
check_out "  and the diagnosis names the Claude Code half" "the CLAUDE CODE range 3.2.100-3.2.140"

# T5 (#721): MAX trailing-edge. The README states `3.2.1409` while the ledger MAX is
# 3.2.140 — a prefix match. RED.
default_ledger
printf -- '- verified against **`3.2.100`–`3.2.1409`** on macOS `30.1.1`–`30.1.2` / Darwin `29.x`.\n' \
    | write_readme
check "MAX with a trailing digit (prefix-match) is REJECTED" 1 "$(run)"

# T6: ledger with no machine-readable bounds -> cannot determine. Exit 2, never 0.
cat > "$work/build/version-compat.md" <<'EOF'
# fixture ledger with no machine-readable range at all
EOF
printf -- '- verified against **`3.2.100`–`3.2.140`** on macOS `30.1.1`–`30.1.2` / Darwin `29.x`.\n' \
    | write_readme
check "ledger without CC_SUPPORTED lines cannot determine" 2 "$(run)"

# T7: bounds present, but the ledger's own PROSE range disagrees with them, so no claim
# anchors. A guard that shrugged and passed would ship a README checked against nothing.
# Exit 2, and the message must say so rather than reporting drift.
write_ledger 3.2.100 3.2.140 '`3.2.100`–`3.2.120`' 'on macOS `30.1.1`–`30.1.2` / Darwin `29.x`'
printf -- '- verified against **`3.2.100`–`3.2.140`** on macOS `30.1.1`–`30.1.2` / Darwin `29.x`.\n' \
    | write_readme
check "ledger prose disagreeing with its own bounds cannot determine" 2 "$(run)"
check_out "  and it says the claim was not constructible" "no canonical range claim found"

# T8: absent README -> nothing was evaluated. Exit 2, NOT 0. (Deliberate divergence from
# check-cc-version.sh, which tolerates a missing README because it is an advisory run on
# a maintainer's box rather than a gate over a tree where README.md is tracked.)
default_ledger
rm -f "$work/README.md"
check "absent README is a FAILURE, not a silent pass" 2 "$(run)"

# T9: both halves present and individually correct, but stated in SEPARATE sentences. The
# contiguity rule is what stops a distant mention standing in for a stale range, so this
# must be RED even though every token is right.
default_ledger
{
    printf -- '- verified against **`3.2.100`–`3.2.140`**, a range covering seven releases.\n'
    printf -- '- Testing ran on macOS `30.1.1`–`30.1.2` / Darwin `29.x` throughout.\n'
} | write_readme
check "halves stated apart (not one contiguous claim) is REJECTED" 1 "$(run)"
check_out "  and the diagnosis says both are present but split" "not as one contiguous claim"

# T10: the ledger states the claim two DIFFERENT ways. Picking the first silently would
# check the README against an arbitrary one of two disagreeing sources. Exit 2.
write_ledger 3.2.100 3.2.140 '`3.2.100`–`3.2.140`' 'on macOS `30.1.1`–`30.1.2` / Darwin `29.x`'
printf -- '\nA later section restates it as `3.2.100`–`3.2.140` on macOS `30.1.1`–`30.1.9` / Darwin `29.x`.\n' \
    >> "$work/build/version-compat.md"
printf -- '- verified against **`3.2.100`–`3.2.140`** on macOS `30.1.1`–`30.1.2` / Darwin `29.x`.\n' \
    | write_readme
check "ledger stating the claim two ways cannot determine" 2 "$(run)"

# T11: THE round-3 falsifier, and the defect that motivated scoping. The pinned
# `## Prerequisites` section states a STALE range while a CORRECT copy of the whole
# claim sits under an unrelated heading. An UNSCOPED guard — which is what this was —
# finds the claim somewhere in the file and exits 0 while the user-facing sentence is
# wrong. That is issue #712's defect one level up: there a distant BOUND bridged a
# stale range, here a distant WHOLE CLAIM does. RED, naming the CC half.
default_ledger
cat > "$work/README.md" <<'EOF'
# sessiometer (fixture README)

## Prerequisites

- verified against **`3.2.100`–`3.2.120`** on macOS `30.1.1`–`30.1.2` / Darwin `29.x`.

## Support

Verified against `3.2.100`–`3.2.140` on macOS `30.1.1`–`30.1.2` / Darwin `29.x`.
EOF
check "stale claim in the pinned section + a correct copy elsewhere is REJECTED" 1 "$(run)"
check_out "  and the diagnosis names the Claude Code half" "the CLAUDE CODE range 3.2.100-3.2.140"

# T12: the deliberate PERMISSION that distinguishes the chosen fix from the rejected
# one. The pinned section is CORRECT and a second correct copy appears elsewhere. The
# rejected alternative — "the claim must occur exactly once in the file" — goes RED
# here, on a README that is entirely right. Scoping is what makes this GREEN: it
# constrains the surface that is pinned and leaves the rest of the README alone.
default_ledger
cat > "$work/README.md" <<'EOF'
# sessiometer (fixture README)

## Prerequisites

- verified against **`3.2.100`–`3.2.140`** on macOS `30.1.1`–`30.1.2` / Darwin `29.x`.

## Support

As above, verified against `3.2.100`–`3.2.140` on macOS `30.1.1`–`30.1.2` / Darwin `29.x`.
EOF
check "a correct pinned section plus a second correct copy elsewhere PASSES" 0 "$(run)"

# T13: the pinned section is ABSENT (renamed here, which is how a restructure presents).
# The claim is stated correctly elsewhere, so a scoped guard that fell back to the whole
# file — or simply found no section and shrugged — would exit 0 having checked nothing
# the pin names. Exit 2: a guard that evaluated no section is not green, and the pin
# must be re-pointed deliberately.
default_ledger
cat > "$work/README.md" <<'EOF'
# sessiometer (fixture README)

## Requirements

- verified against **`3.2.100`–`3.2.140`** on macOS `30.1.1`–`30.1.2` / Darwin `29.x`.
EOF
check "an absent pinned section cannot determine" 2 "$(run)"
check_out "  and it says the section is what is missing" "'## Prerequisites' heading"

# T14: the pinned section is DUPLICATED — a correct `## Prerequisites` and, further down,
# a stale second one of the kind a restructure leaves behind. The guard enforces both
# halves of "exactly one heading" with a single `-ne 1`, so a `-lt 1` mutant keeps T13's
# missing-section behaviour and drops this one silently: it reads the FIRST section, finds
# the correct claim there and exits 0, having never looked at the stale copy a reader
# scrolling to the second heading would land on. That is the guard's own motivating defect
# — a correct copy masking a stale one — one level in. Exit 2, naming the count: which of
# two sections is the pinned one is not the guard's to guess, so the pin must be re-pointed
# deliberately.
default_ledger
cat > "$work/README.md" <<'EOF'
# sessiometer (fixture README)

## Prerequisites

- verified against **`3.2.100`–`3.2.140`** on macOS `30.1.1`–`30.1.2` / Darwin `29.x`.

## Quickstart

An unrelated section stating no range at all.

## Prerequisites

- Legacy note: verified against **`3.2.100`–`3.2.120`** on macOS `30.1.1`–`30.1.2` / Darwin `29.x`.
EOF
check "a DUPLICATED pinned section cannot determine" 2 "$(run)"
check_out "  and it reports how many headings it found" "found 2"

# T15 (#1317): THE round-4 falsifier, and the defect scoping NARROWED rather than closed.
# The pinned section states the ledger's claim AND, above it, a stale one of the kind a
# range widening leaves behind. Containment — `grep -qF -- "$claim"` — is satisfied by the
# correct sentence, so the containment-only guard this extends exits 0 on this fixture with
# a message byte-identical to the one it prints on a clean one (measured against that guard,
# not inferred): blind to the difference, not merely quiet about it.
# That is T11's defect one section in — there a correct copy under ANOTHER heading masked
# a stale sentence, here a correct copy under the SAME one does. RED, and the diagnosis
# must quote the statement the ledger does not make, since "they disagree" is not
# actionable when the section also contains a sentence that is entirely right.
default_ledger
{
    printf -- '- Legacy note: verified against **`3.2.100`–`3.2.120`** on macOS `30.1.1`–`30.1.2` / Darwin `29.x`.\n'
    printf -- '- verified against **`3.2.100`–`3.2.140`** on macOS `30.1.1`–`30.1.2` / Darwin `29.x`.\n'
} | write_readme
check "a stale claim BESIDE the correct one in the pinned section is REJECTED" 1 "$(run)"
check_out "  and the diagnosis quotes the stale statement" \
    '`3.2.100`–`3.2.120` on macOS `30.1.1`–`30.1.2` / Darwin `29.x`'

# T16 (#1317): the PERMISSION that keeps T15's rule from being the exactly-once rule
# design choice 4 rejected, merely rescoped — T12's role one section in. The pinned
# section states the correct claim TWICE, and is entirely right. A rule counting
# claim-shaped OCCURRENCES goes RED here; counting DISTINCT ones is what makes it GREEN.
# Without this case, tightening the guard to a count would pass the whole suite while
# newly rejecting a correct README, which is the trade #1292 measured and declined.
default_ledger
{
    printf -- '- verified against **`3.2.100`–`3.2.140`** on macOS `30.1.1`–`30.1.2` / Darwin `29.x`.\n'
    printf -- '- Restated for emphasis: `3.2.100`–`3.2.140` on macOS `30.1.1`–`30.1.2` / Darwin `29.x`.\n'
} | write_readme
check "the SAME correct claim stated twice in the section PASSES" 0 "$(run)"

# T17 (#1317): the RESIDUAL, pinned GREEN so it cannot move in either direction
# unmeasured. T15's rule pins the claim's SHAPE, so a stale range inside the section that
# carries no `on macOS … / Darwin …` clause is not claim-shaped and still passes. This is
# a boundary marker, not an endorsement: closing this last step means counting
# version-range PAIRS instead, and that reading is RED on a wholly-correct section — the
# live README's holds two such pairs, the CC range and the host range, both of them inside
# the canonical claim itself. Anyone widening the shape must turn this case RED
# deliberately and re-derive that cost rather than inherit this comment.
default_ledger
{
    printf -- '- verified against **`3.2.100`–`3.2.140`** on macOS `30.1.1`–`30.1.2` / Darwin `29.x`.\n'
    printf -- '- Superseded: an earlier note gave `3.2.100`–`3.2.120`, with no host clause.\n'
} | write_readme
check "a bare non-claim-shaped stale range in the section PASSES (stated residual)" 0 "$(run)"

printf '\n%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
