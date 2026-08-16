#!/usr/bin/env bash
# Hold every committed surface that SETS one of the menubar test bundle's environment
# switches to the `TEST_RUNNER_`-prefixed spelling (issue #1362). Two spellings set one:
# `NAME=value` on a command line, and `NAME: value` at key position in a YAML `env:`
# mapping. Naming a switch without setting it is unconstrained — that is rule A's whole
# boundary, and it is why this cannot be stated as "every surface".
#
# THE MECHANISM. `xcodebuild` forwards a `TEST_RUNNER_`-prefixed variable into the xctest
# process with the prefix stripped; an un-prefixed one does not arrive at all. So a name
# written without the prefix on a command line stops at `xcodebuild`, leaves the switch
# off, and the run still ends `** TEST SUCCEEDED **` having done nothing. Issue #1332
# established that; issue #1362 re-measured it on `SESSIOMETER_PANEL_MEASURE`
# (un-prefixed: `Executed 1 test, with 1 test skipped`, no calibration printed).
#
# WHY A GATE AND NOT A CONVENTION. The failure is silent in both directions. A document
# naming the un-prefixed form hands the reader a command that prints nothing and exits 0
# — and the sites that did included the suite's own skip messages, so following the
# instruction a skip printed landed the reader back on that same skip. In the other
# direction, RENAMING a switch in Swift leaves every document confidently naming a switch
# nothing reads, with every gate still green. Neither shows up in any test, because no
# test reads prose.
#
# THE TWO RULES, and they are duals:
#
#   A. FORM — a switch the test bundle reads may not appear in a committed file in a
#      spelling that SETS it (`NAME=` on a command line, `NAME:` at YAML key position)
#      without the prefix.
#   B. EXISTENCE — a `TEST_RUNNER_`-prefixed name in a committed file must correspond to
#      a switch the test bundle actually reads.
#
# Rule A alone is blind to a rename (the docs would name a name this script no longer
# derives, so nothing would look for it); rule B alone is blind to the un-prefixed form.
#
# THE SUBJECT IS DERIVED, NOT LISTED — every `SESSIOMETER_`-namespaced
# `ProcessInfo.processInfo.environment["…"]` key under `apps/menubar/Tests/`. Two scoping
# decisions, both narrowing, and each pinned by the peer rather than by this tree — as of
# writing, widening either leaves the guard green here, so they state the CONTRACT, not an
# exclusion the tree currently exercises:
#
#   the TEST tree, because that is exactly what `xcodebuild` forwards into. A switch the
#   APP reads from its own launch environment is correctly written bare —
#   `SESSIOMETER_GLYPH_GALLERY=1 "$BIN"` (design/README.md § the glyph gallery) — and
#   deriving it would forbid that correct line. T18 pins this; T9 pins only that the whole
#   TEST bundle is covered, never that Sources is out. Worth knowing what is doing the
#   work TODAY, because it is not this bound: `SESSIOMETER_GLYPH_GALLERY` is read through
#   a symbolic constant (`environment[glyphGalleryEnvironmentKey]`) and the daemon's
#   `SESSIOMETER_*` test variables through `std::env::var_os(CONST)`, so neither matches
#   the literal-subscript shape this derives from at all. Widen the bound and the derived
#   set does not move — until one of those reads becomes a literal, at which point this
#   bound is the only thing standing between it and a red on a correct line.
#
#   this repo's NAMESPACE, because an ambient variable a test merely saves and restores is
#   not a switch an operator sets. `SocketPathResolverTests` reads `XDG_CONFIG_HOME` to
#   restore it around its own `setenv`; setting that variable in front of the daemon
#   binary is a correct line, and the daemon binary is not an `xcodebuild` command line at
#   all. T6 pins this. (README.md carries such prefixes — `PATH=…` and `CLAUDE_BIN=…` —
#   but names `$XDG_CONFIG_HOME` only as a path, never as an assignment.)
#
# An empty derived set is exit 2, never a pass. And the two rules cross-check the
# derivation itself: a derivation returning names that exist nowhere would leave rule A
# trivially satisfied, but rule B then reds on every real prefixed name in the tree.
#
# THE ONE EXEMPTION, and it is DIRECTIONAL. A line may state the un-prefixed form when the
# word `bare` PRECEDES it: that is how a document WARNS about the trap, and
# design/README.md § Panel golden drift gate has to quote the broken form to explain it.
#
# The direction is the load-bearing half, not a nicety. Every repaired skip message in the
# suite states the CORRECT form and then explains the bare one on the same line
# (`TEST_RUNNER_…=1 — the bare, un-prefixed name reaches xcodebuild and not the test`).
# An undirected exemption reads the word and passes the line, so de-prefixing exactly
# those messages — the sites issue #1362 is about — would be green: the skip would hand
# the reader the broken form while its own next clause called it broken. Directional, the
# text before the name is `+ "` and the line reds.
#
# Stated residual: the exemption is textual, so a line that writes `bare` for another
# reason BEFORE an un-prefixed command spelling would pass. Nothing in the tree does; the
# alternative — a magic comment marker — buys precision at the cost of a convention no
# reader can see in the prose itself, which is the failure this gate exists to end. The
# YAML spelling is not exemptible at all: at key position nothing precedes the name.
#
# SELF-EXCLUSION, exactly one path: this guard's own falsifier peer
# (`scripts/check-test-runner-env-form.test.sh`) builds mutant trees out of literal
# fixture text, and a linter's fixtures are inputs, not documentation. The exclusion is a
# single exact path rather than a glob, so it cannot silently widen, and the peer's own
# cases prove the guard still fires on files INSIDE a fixture tree — which is what makes
# excluding the file that holds them safe. This script itself is scanned like any other —
# it derives its patterns at runtime and names no individual switch, only the namespace,
# which rule B is careful to distinguish from a switch name.
#
# Exit codes:
#   0  every surface that SETS a switch uses the prefixed spelling, and every prefixed
#      name is real
#   1  DRIFT — a switch set under its un-prefixed name (rule A) or an unknown switch
#      (rule B)
#   2  could not determine (not a git tree, no switches derived, nothing scanned)
#
# Run locally:  ./scripts/check-test-runner-env-form.sh
set -euo pipefail

export LC_ALL=C

# Resolved BEFORE the `cd`, and tested: `cd "$(...)"` cannot carry the substitution's
# failure — the exit status is the `cd`'s, and bash's `cd ""` succeeds and stays put — so
# written that way this branch is unreachable and a run outside a git tree falls through
# to the derivation error below, which tells the reader to re-point a script that is
# merely in the wrong directory.
toplevel="$(git rev-parse --show-toplevel 2>/dev/null || true)"
if [ -z "$toplevel" ] || ! cd "$toplevel"; then
    echo "error: not inside a git work tree — nothing to scan, which is a FAILURE" >&2
    echo "       rather than a pass." >&2
    exit 2
fi

prefix="TEST_RUNNER_"
sources="apps/menubar/Tests"
exclude="scripts/check-test-runner-env-form.test.sh"
# The same path as an ERE. Interpolated raw, its dots would match any character.
exclude_re="$(printf '%s' "$exclude" | sed 's/[][^$.*\\]/\\&/g')"

# The subject. `-h` because the keys are wanted, not their locations; `sort -u` because a
# switch read from two tests is one switch.
switches="$(git grep -hoE 'environment\["SESSIOMETER_[A-Z0-9_]*"\]' -- "$sources" 2>/dev/null |
            grep -oE '"SESSIOMETER_[A-Z0-9_]*"' | tr -d '"' | sort -u || true)"

if [ -z "$switches" ]; then
    echo "error: derived NO SESSIOMETER_ environment switches from $sources/ — this gate" >&2
    echo "       would then hold every surface to an empty rule and report green having" >&2
    echo "       compared nothing. Either the suite stopped reading its environment, or the" >&2
    echo "       \`ProcessInfo.processInfo.environment[\"…\"]\` shape this derives from" >&2
    echo "       changed and this script needs re-pointing." >&2
    exit 2
fi
switch_count="$(printf '%s\n' "$switches" | wc -l | tr -d '[:space:]')"

# Everything git tracks, minus binaries (`-I`) and this guard's own fixture holder.
scanned="$(git grep -lI '' -- . | grep -vxF "$exclude" | wc -l | tr -d '[:space:]')"
if [ "$scanned" -eq 0 ]; then
    echo "error: scanned 0 tracked text files — nothing was evaluated, which is a" >&2
    echo "       FAILURE rather than a pass." >&2
    exit 2
fi

violations=0
found_a=0
exempt_a=0
checked_b=0

# RULE A. `[^_A-Za-z0-9]` before the name is what lets the prefixed spelling through: the
# character preceding `SESSIOMETER_…` in `TEST_RUNNER_SESSIOMETER_…` is `_`. What follows
# the name is what distinguishes SETTING the switch from naming it — the un-prefixed name
# on its own is the correct in-process spelling and is unrestricted, which is why
# `environment["NAME"]` reads are not matched here either.
#
# `=` is the command-line setter. `:` at YAML key position is the other one, and it is the
# reason this rule is not a one-pattern rule: `.github/workflows/ci.yml` arms the drift
# gate through an `env:` mapping, which carries no `=` anywhere, so the command pattern
# alone cannot see the single highest-consequence surface in the tree. De-prefixed there,
# `panel-goldens` skips both comparisons on every run — and every step of that job is
# `continue-on-error`, so the job reports pass regardless.
report_a() { # <hit record> <the line's text BEFORE the match>
    found_a=$((found_a + 1))
    case "$2" in
        *[Bb]are*) exempt_a=$((exempt_a + 1)); return 0 ;;
    esac
    if [ "$violations" -eq 0 ]; then
        echo "error: committed surfaces SET a test-bundle switch under its un-prefixed" >&2
        echo "       name. Under \`xcodebuild\` that name never reaches the test process:" >&2
        echo "       it leaves the switch off and the run still ends" >&2
        echo "       \`** TEST SUCCEEDED **\`, so the reader who follows it gets nothing." >&2
        echo >&2
    fi
    printf '  %s\n' "$1" >&2
    violations=$((violations + 1))
    return 0
}

for name in $switches; do
    # The COMMAND spelling, anywhere git tracks text.
    hits="$(git grep -nIE "(^|[^_A-Za-z0-9])${name}=" -- . 2>/dev/null |
            grep -vE "^${exclude_re}:" || true)"
    if [ -n "$hits" ]; then
        while IFS= read -r hit; do
            [ -n "$hit" ] || continue
            # `path:lineno:` off first, so a tracked PATH containing the exemption word
            # cannot exempt every hit in that file; then everything from the match to the
            # end of the line off, leaving only what PRECEDES the name. The pattern here
            # is the rule's own, so a prefixed occurrence earlier on the line is stepped
            # over rather than mistaken for the match.
            line="${hit#*:}"
            line="${line#*:}"
            before="$(printf '%s\n' "$line" | sed -E "s/(^|[^_A-Za-z0-9])${name}=.*/\1/")"
            report_a "$hit" "$before"
        done <<EOF
$hits
EOF
    fi

    # The WORKFLOW spelling. Key position only — a `#` comment or a value quoting the
    # name is not setting it — so nothing can precede the name and nothing is exemptible.
    yaml_hits="$(git grep -nIE "^[[:space:]]*${name}:" -- '*.yml' '*.yaml' 2>/dev/null |
                 grep -vE "^${exclude_re}:" || true)"
    if [ -n "$yaml_hits" ]; then
        while IFS= read -r hit; do
            [ -n "$hit" ] || continue
            report_a "$hit" ""
        done <<EOF
$yaml_hits
EOF
    fi
done

if [ "$violations" -gt 0 ]; then
    echo >&2
    echo "       Write ${prefix}<NAME> instead, in whichever spelling the surface uses." >&2
    echo "       To quote the broken form deliberately — as design/README.md does when" >&2
    echo "       explaining the trap — say so on the same line, in the word this gate" >&2
    echo "       reads for it, BEFORE the name." >&2
    exit 1
fi

# RULE B. Every prefixed name in the tree must be a switch the bundle reads. This is the
# arm a RENAME trips: rule A derives its patterns from the new name and would find the
# stale documents perfectly compliant, because they name something nothing looks for.
#
# `+` rather than `*`, and it is not a nicety: a document explaining this convention writes
# the prefixed NAMESPACE with no switch after it, and `*` matches that with an empty tail,
# which is then compared against the derived set and reported as an unknown switch. This
# script's own rule A comment is such a document, which is how it was found — the guard was
# green while untracked and red the moment it was committed, since `git grep` reads what git
# tracks. A namespace mention names no switch, so it cannot be stale; a stale name has
# characters after the prefix and still matches. Pinned by the peer.
b_hits="$(git grep -nIoE "${prefix}SESSIOMETER_[A-Z0-9_]+" -- . 2>/dev/null |
          grep -vE "^${exclude_re}:" || true)"
unknown=0
if [ -n "$b_hits" ]; then
    while IFS= read -r hit; do
        [ -n "$hit" ] || continue
        checked_b=$((checked_b + 1))
        found="${hit##*:}"
        bare_name="${found#"$prefix"}"
        if ! printf '%s\n' "$switches" | grep -qxF "$bare_name"; then
            if [ "$unknown" -eq 0 ]; then
                echo "error: committed surfaces name ${prefix}-prefixed switches that no test" >&2
                echo "       under $sources/ reads. A switch renamed in Swift leaves every" >&2
                echo "       document naming the old one, with every other gate still green:" >&2
                echo >&2
            fi
            printf '  %s\n' "$hit" >&2
            unknown=$((unknown + 1))
        fi
    done <<EOF
$b_hits
EOF
fi

if [ "$unknown" -gt 0 ]; then
    echo >&2
    echo "       Switches this bundle actually reads:" >&2
    printf '%s\n' "$switches" | sed 's/^/         /' >&2
    echo "       Re-point the surfaces above, or delete them if the switch is gone." >&2
    exit 1
fi

echo "ok: ${switch_count} test-bundle switch(es) derived from $sources/, held across ${scanned} tracked text file(s)"
printf '      %s\n' $switches
echo "    (rule A: ${found_a} un-prefixed switch-setting spelling(s) found, all ${exempt_a} of them"
echo "     exempt as deliberate counter-examples; rule B: ${checked_b} prefixed name(s)"
echo "     checked against the derived set)"
