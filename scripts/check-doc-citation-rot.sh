#!/usr/bin/env bash
# Fail the build when a PR introduces a source citation that will rot silently
# (issue #1058).
#
# Documents under docs/ cite source locations as `src/<file>.rs:NNN`. A stale
# *path* fails loudly — the file is not there. A stale *line number* fails
# silently and plausibly: `src/cli.rs:4549-4611`, cited as the `import` body,
# came to rest on `write_export`'s doc comment after an unrelated PR shifted the
# file. It still resolves, still looks like evidence, and still reads as
# verified. That has happened twice in this repo, and the second time only an
# adversarial review caught it.
#
# THE RULE
#   A `src/<file>.rs:NNN` citation must leave behind something a reader can
#   RE-DERIVE the location from — a SYMBOL named on the same line that actually
#   occurs in the cited file. A bare number as the sole referent is only
#   admissible when the cited file is STABLE, because then the number keeps
#   meaning what it meant.
#
#   A SYMBOL, not a word that merely occurs. An ALL-CAPS token in running prose
#   — `CLI`, `MUST`, `NEVER`, `ACL` — is not an identifier, and it satisfied the
#   shape test below only by accident: `[A-Za-z]+[A-Z]...` cannot tell an acronym
#   apart from CamelCase. `docs/requirements/gui-cli-capability-parity.md` cited
#   the verb table as `src/cli.rs:741-765` and passed on the token `CLI`, while
#   the range had already drifted off the table onto `parse`'s doc comment — the
#   exact silent rot this file exists to stop, waved through by its own check
#   (issue #1319). Backticks remain the escape hatch: an author who means a real
#   all-caps constant writes `MAX_RETRIES`-style prose as code, and it counts.
#
#   THE BAR IS "IS IT AN IDENTIFIER", NOT "HOW MANY LINES DOES IT HIT". The
#   tempting rule — refuse an anchor that does not narrow the file enough — was
#   measured against this corpus and INVERTS. Re-derive it and see:
#
#       for t in apply_import MUST; do
#           printf '%-14s %s\n' "$t" "$(git grep -cE \
#             "(^|[^A-Za-z0-9_])${t}([^A-Za-z0-9_]|$)" HEAD -- src/cli.rs | cut -d: -f3)"
#       done
#
#   `apply_import` — this file's own worked example of a GOOD anchor, quoted in
#   the failure message below — hits many times more lines than the prose word
#   `MUST` does. Any ceiling that refuses the prose refuses the symbol first. The
#   two populations overlap on line count and separate on shape, so shape is what
#   is tested.
#
# Three design choices carry the weight.
#
#   1. SCOPE IS THE PR'S OWN DIFF, not the tree. The corpus already carries a
#      substantial backlog of bare citations into churning files. `--audit`
#      counts it; the count is deliberately NOT transcribed here, because it
#      mirrors a base that moves on every merge and a number written into a
#      comment is stale the day after it lands. A tree-scoped gate is therefore
#      RED on the day it lands, and the only way to clear it is the bulk
#      conversion issue #1058 explicitly rules out (line numbers stay legitimate
#      for stable files). A gate that is red on arrival is a gate nobody reads.
#      Diff scope makes it clearable by construction and still serves the stated
#      goal — stopping the NEXT pipeline run from re-introducing the pattern.
#      `--audit` sweeps the whole tree for a human working through that backlog;
#      CI does not run it, precisely because it is red. Its resolvability half is
#      already clean tree-wide — no citation points past its file's EOF and none
#      names a file the tree does not carry — so check (a) below costs nothing
#      today and catches the case that needs no judgment.
#
#   2. CHURN IS COUNTED OVER A COMMIT WINDOW, NOT A CALENDAR ONE. A calendar
#      window is non-deterministic — the same commit is green today and red next
#      month, because the window slid, not because anything changed. A window of
#      the last N commits *of the head's own history* is a property of the commit
#      under test, so a re-run months later returns the same verdict. It is also
#      free of `date` arithmetic, which is not portable between GNU and BSD.
#
#   3. A SHALLOW CLONE IS FATAL, NOT QUIET. `actions/checkout` defaults to
#      `fetch-depth: 1`, whose single grafted commit is parentless — so every
#      tracked file reads as introduced by it and every churn count collapses to
#      1, under any threshold worth setting. Every file then looks stable and
#      this script becomes a rubber stamp that reports success having measured
#      nothing. That failure is invisible in the log unless the script says so,
#      so it refuses to run instead.
#
# THE THRESHOLD IS A DELIBERATE CHOICE, NOT A DERIVED CONSTANT, and it is set
# STRICTER than this repo's own past practice rather than reproducing it. The
# tempting calibration — "reproduce the judgment PR #1057 already made" — does
# not survive being run. #1057 is a pure-addition commit: it converted no
# citation at all, and the ones it did author include bare line numbers into
# files this threshold calls churning. Point this gate at it and it goes red:
#
#     ./scripts/check-doc-citation-rot.sh 386a6a2^ 386a6a2
#
# That is a fixed pair of commits, so unlike a churn distribution the result
# does not drift — re-run it whenever you want to re-audit this paragraph.
# Clearing it would take a threshold so high that almost nothing in `src/` would
# meet it, which is a gate that gates nothing.
#
# 15 is chosen on the asymmetry instead: naming a symbol costs one word and
# never rots, so the bar for DEMANDING one belongs low, and a false demand costs
# a word while a false exemption costs a silently-rotted citation. The
# distribution it sits in is a moving base — recomputed from the last $WINDOW
# commits of whatever history is under test — so it is deliberately not
# transcribed here either. Re-derive it when you want it:
#
#     git rev-list --count "$(git rev-list --max-count=1 --skip=299 HEAD)..HEAD" -- <file>
#
# Usage:
#   check-doc-citation-rot.sh [<base-ref> <head-ref>]   # diff-scoped (CI default)
#   check-doc-citation-rot.sh --audit [<ref>]           # whole-tree report
#
# `--audit` reports on the corpus AS OF <ref> (default HEAD) — the doc set, the
# cited files and the churn counts are all read from that ref, not from the
# working tree, so a figure quoted from it belongs to a nameable commit.
#
# base/head default to $BASE_SHA/$HEAD_SHA (set from the pull_request event in
# CI), then to `merge-base(origin/main, HEAD)`..HEAD. That last fallback is why
# a bare local run on a feature branch still inspects that branch's own work
# rather than short-circuiting to a green that examined nothing.
set -euo pipefail

cd "$(git rev-parse --show-toplevel)"

# Tunable, and named so a failure message can quote them. Overridable for
# experiments; CI uses the defaults.
WINDOW="${CITATION_CHURN_WINDOW:-300}"   # commits of head history to look back over
THRESHOLD="${CITATION_CHURN_THRESHOLD:-15}"  # commits touching the file => "churning"

# A citation: a repo-relative Rust path, a colon, a line, optionally a range end.
CITATION_RE='src/[A-Za-z0-9_/-]+\.rs:[0-9]+(-[0-9]+)?'

violations=0
inspected=0
lines_scanned=0

fail() { printf '%s\n' "$*" >&2; violations=$((violations + 1)); }

# --- Fatal preconditions -----------------------------------------------------
# A shallow or grafted clone collapses every churn count to 1 — its one commit is
# parentless, so every tracked file reads as introduced by it. Refuse rather than
# report a green that measured nothing.
if [ "$(git rev-parse --is-shallow-repository)" = "true" ]; then
    {
        echo "error: refusing to run against a SHALLOW clone."
        echo "       Churn is counted from history; with depth-1 every file reads as stable and"
        echo "       this check degrades into a rubber stamp. Check out with 'fetch-depth: 0'."
    } >&2
    exit 1
fi

mode=diff
if [ "${1:-}" = "--audit" ]; then
    mode=audit
    shift
fi

if [ "$mode" = audit ]; then
    head="${1:-HEAD}"
else
    base="${1:-${BASE_SHA:-}}"
    head="${2:-${HEAD_SHA:-}}"
    if [ -z "$head" ]; then head="HEAD"; fi
    if [ -z "$base" ]; then
        # No PR range supplied: compare this branch against where it left main.
        base="$(git merge-base origin/main "$head" 2>/dev/null || true)"
    fi
    if [ -z "$base" ]; then
        {
            echo "error: no base revision. Pass one explicitly, set BASE_SHA/HEAD_SHA, or make"
            echo "       'origin/main' available so the branch point can be derived."
        } >&2
        exit 1
    fi
    base="$(git merge-base "$base" "$head")"
fi

head_sha="$(git rev-parse "$head")"

# --- Churn ------------------------------------------------------------------
# Commits touching <file> among the last $WINDOW commits of $head's history.
# Deterministic for a given head: the window is defined by the commit, not by
# the wall clock.
window_start="$(git rev-list --max-count=1 --skip=$((WINDOW - 1)) "$head_sha" 2>/dev/null || true)"

churn() { # <path> -> commit count
    if [ -n "$window_start" ]; then
        git rev-list --count "${window_start}..${head_sha}" -- "$1"
    else
        # History shorter than the window: the whole history IS the window.
        git rev-list --count "$head_sha" -- "$1"
    fi
}

# --- Anchor detection --------------------------------------------------------
# Does the doc line name a symbol that actually occurs in the cited file? The
# citation text itself is stripped first, or `cli` from `src/cli.rs` would anchor
# a citation to itself. A candidate anchor is any token carrying `_`, any
# CamelCase token, or any backticked token — the three shapes a Rust identifier
# takes in this corpus's prose. "Occurs in the file" (rather than "is defined
# there") is the deliberate bar: the point is to leave a reader something to
# grep for, and a type or field name serves that as well as a `fn` does.
# The shape only an identifier takes, and the subset of it that is an acronym
# rather than an identifier. Written as one shape and a subtraction, so the two
# can never drift apart: what the check declines is exactly what the hint names.
IDENT_SHAPE='^([A-Za-z0-9]*_[A-Za-z0-9_]*|[A-Za-z]+[A-Z][A-Za-z0-9]*)$'
ACRONYM_SHAPE='^[A-Z0-9]+$'

# Set by anchored() when it refused an ALL-CAPS prose token that DOES occur in
# the cited file — i.e. the tokens that would have anchored the citation before
# issue #1319. Named in the failure so "name a symbol" cannot read as nonsense to
# an author looking straight at a word they believe is one.
declined=""

occurs() { # <token> <file> -> 0 if the token appears in the cited file
    # Read the cited file AT THE REF, like every other read here. Process
    # substitution rather than `git show ... | grep -q`: under this script's
    # `pipefail` that pipeline reports FAILURE on a hit, because `grep -q` exits
    # at the first match and SIGPIPEs `git show`. Every anchored citation then
    # reads as unanchored, so the run does not fail loudly — it inflates the
    # violation count. Measured rather than assumed, by swapping the two forms
    # and diffing `--audit HEAD`; the size of the gap is a corpus figure, so by
    # choice 1 above it is not written down here.
    grep -qE "(^|[^A-Za-z0-9_])${1}([^A-Za-z0-9_]|\$)" \
        <(git show "${head_sha}:${2}" 2>/dev/null)
}

anchored() { # <doc-line> <cited-file>  -> 0 if anchored
    local line="$1" file="$2" stripped tok
    declined=""
    stripped="$(printf '%s' "$line" | sed -E "s#${CITATION_RE}##g")"
    for tok in $(
        {
            # Inside backticks the author has already declared "this is code", so
            # any identifier counts — `classify`, `export` and `stash` are real
            # single-word `fn` names and would fail a shape test. That declaration
            # is also the escape hatch for a genuine all-caps constant, which the
            # subtraction below would otherwise refuse.
            printf '%s' "$stripped" \
                | grep -oE '`[^`]*`' \
                | tr -c 'A-Za-z0-9_' '\n' \
                | grep -E '^[A-Za-z_][A-Za-z0-9_]*$'
            # Outside them, require a shape only an identifier takes, or every
            # English word on the line becomes a candidate anchor — MINUS the
            # acronyms that shape admits by accident (issue #1319). A screaming
            # constant keeps its underscore, so it is not in the subtraction.
            printf '%s' "$stripped" \
                | tr -c 'A-Za-z0-9_' '\n' \
                | grep -E "$IDENT_SHAPE" \
                | grep -vE "$ACRONYM_SHAPE"
        } | sort -u
    ); do
        [ "${#tok}" -ge 3 ] || continue
        if occurs "$tok" "$file"; then
            return 0
        fi
    done
    # Unanchored. Report which prose acronyms were declined, if any, so the
    # remedy the failure prescribes is the one this line actually needs.
    for tok in $(
        printf '%s' "$stripped" \
            | tr -c 'A-Za-z0-9_' '\n' \
            | grep -E "$IDENT_SHAPE" \
            | grep -E "$ACRONYM_SHAPE" \
            | sort -u
    ); do
        [ "${#tok}" -ge 3 ] || continue
        if occurs "$tok" "$file"; then
            declined="${declined:+${declined}, }${tok}"
        fi
    done
    return 1
}

# --- The check ---------------------------------------------------------------
check_line() { # <doc> <lineno> <line>
    local doc="$1" lineno="$2" line="$3" cite path last n c

    for cite in $(printf '%s' "$line" | grep -oE "$CITATION_RE" || true); do
        inspected=$((inspected + 1))
        path="${cite%%:*}"
        last="${cite##*:}"
        last="${last##*-}"

        # (a) Resolvability. Deterministic, and applies whatever the churn:
        #     a citation past EOF, or into a file no clone has, is rot outright.
        if ! git cat-file -e "${head_sha}:${path}" 2>/dev/null; then
            fail "  ${doc}:${lineno}  ${cite}  -> cited file is not tracked at ${head_sha:0:12}"
            continue
        fi
        n="$(git show "${head_sha}:${path}" | wc -l | tr -d ' ')"
        if [ "$last" -gt "$n" ]; then
            fail "  ${doc}:${lineno}  ${cite}  -> line ${last} is past EOF (${path} has ${n} lines)"
            continue
        fi

        # (b) Anchoring, required only where the number cannot be trusted to keep
        #     its meaning.
        c="$(churn "$path")"
        if [ "$c" -lt "$THRESHOLD" ]; then
            continue
        fi
        if ! anchored "$line" "$path"; then
            if [ -n "$declined" ]; then
                fail "  ${doc}:${lineno}  ${cite}  -> bare line number into a churning file (${c} commits in the last ${WINDOW}); ${declined} occurs in ${path} but is prose, not a symbol — name a real one, or backtick it if it is code"
            else
                fail "  ${doc}:${lineno}  ${cite}  -> bare line number into a churning file (${c} commits in the last ${WINDOW}); name a symbol"
            fi
        fi
    done
}

if [ "$mode" = audit ]; then
    while IFS= read -r doc; do
        lineno=0
        while IFS= read -r line; do
            lineno=$((lineno + 1))
            case "$line" in
                *src/*.rs:*) ;;
                *) continue ;;
            esac
            lines_scanned=$((lines_scanned + 1))
            check_line "$doc" "$lineno" "$line"
        done < <(git show "${head_sha}:${doc}")
    done < <(git ls-tree -r --name-only "$head_sha" -- docs/ | grep -E '\.md$' | sort)
else
    # Added / modified lines only. `-U0` so context lines are never mistaken for
    # the PR's own work; the `@@` header carries the new-file line number, which
    # each following `+` line advances.
    while IFS=$'\t' read -r doc lineno line; do
        lines_scanned=$((lines_scanned + 1))
        check_line "$doc" "$lineno" "$line"
    done < <(
        git diff -U0 "$base" "$head_sha" -- 'docs/*.md' 'docs/**/*.md' \
            | awk '
                /^\+\+\+ b\// { doc = substr($0, 7); next }
                /^@@ / {
                    match($0, /\+[0-9]+/)
                    n = substr($0, RSTART + 1, RLENGTH - 1) + 0
                    next
                }
                /^\+/ {
                    body = substr($0, 2)
                    if (body ~ /src\/[A-Za-z0-9_\/-]+\.rs:[0-9]+/)
                        printf "%s\t%d\t%s\n", doc, n, body
                    n++
                    next
                }
            '
    )
fi

if [ "$violations" -gt 0 ]; then
    {
        echo
        echo "error: $violations citation(s) will rot silently (issue #1058)."
        echo
        echo "A line number into a file that moves is worse than a dangling reference: it still"
        echo "resolves, lands on a plausible neighbour, and reads as verified. Cite the SYMBOL so"
        echo "the location can be re-derived:"
        echo
        echo "    the verbs parsed at src/cli.rs:741-765"
        echo "    the verbs parsed by \`parse_subcommand\` in \`src/cli.rs\`"
        echo
        echo "Keeping the range as a secondary locator is fine — \`apply_import\` (\`src/cli.rs:4726-4813\`)"
        echo "passes, because the symbol survives the drift the number does not."
        echo
        echo "An ALL-CAPS word in running prose is not a symbol — \`CLI\`, \`MUST\` and \`ACL\` all occur"
        echo "in src/, and none of them re-derives anything (issue #1319). If the token really is a"
        echo "constant, backtick it and it counts."
        echo
        echo "Is the file churning? Threshold is ${THRESHOLD} commits over the last ${WINDOW}:"
        echo "    git rev-list --count \"\$(git rev-list --max-count=1 --skip=$((WINDOW - 1)) HEAD)..HEAD\" -- <file>"
    } >&2
    exit 1
fi

# Cardinality is always printed, and the two ways of inspecting nothing are named
# apart. "No doc lines changed" is an ordinary pass; it must not read as "the
# corpus was swept and is clean", which is the overclaim this whole check exists
# to stop.
if [ "$mode" = audit ]; then
    echo "ok: audited $inspected citation(s) over $lines_scanned citing line(s) across the tree."
elif [ "$inspected" -eq 0 ]; then
    echo "ok: no source citations added or modified in docs/ between ${base:0:12} and ${head_sha:0:12} — nothing to check (this is NOT a tree-wide audit; use --audit for that)."
else
    echo "ok: inspected $inspected added/modified citation(s) over $lines_scanned line(s); none will rot silently."
fi
