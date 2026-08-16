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
#   occurs AT THE CITED LINES. A bare number as the sole referent is admissible
#   only when the cited file is STABLE — where "stable" is a BET that the number
#   keeps meaning what it meant, not a guarantee that it does; see § The two
#   questions.
#
# § THE TWO QUESTIONS, AND ONLY ONE OF THEM IS CHURN-GATED (issue #1388)
#   Asking them as one question is what let a citation through whose own line
#   contradicted it.
#
#     DEMAND — the line names no symbol at all: does this citation OWE one?
#     SELF-CHECK — the line already names one: does it AGREE with the number?
#
#   Only the DEMAND is churn-gated. It is a cost imposed on an author, so the
#   threshold decides when it is worth imposing, and issue #1058's ruling that
#   line numbers stay legitimate for stable files governs it unchanged. The
#   SELF-CHECK asks nothing of anybody — the author already wrote both halves —
#   so a "stable" file has nothing to exempt, and it runs at any churn.
#
#   That distinction is not a refinement; the collapsed form was measurably
#   wrong. `docs/requirements/migration-credential-portability.md` cited the
#   `account_uuid` field's doc comment as `src/config.rs:341` and quoted its
#   text verbatim. The quote was right and the number was right on the day it
#   was written; a later commit inserted a two-line `pub(crate) use` re-export
#   near the top of the file, ~280 lines above, and the cited line became
#   `#[allow(dead_code)]`. The line said `account_uuid`, `account_uuid` did not
#   occur at 341, and the gate certified it anyway — because `src/config.rs` sat
#   BELOW the threshold, so the anchoring question was never asked at all.
#   Re-derive both halves rather than trusting them, against the very PR that
#   rewrote that line — a pinned pair of commits, so by design choice 2 below
#   the churn count is a property of the commit and not of the day you ask:
#
#     git fetch origin f286778e51a06bf409d0e3e9e18214e7a41d0b18
#     git rev-list --count "$(git rev-list --max-count=1 --skip=299 f286778)..f286778" \
#       -- src/config.rs                                   # under $THRESHOLD
#     ./scripts/check-doc-citation-rot.sh 9b39061 f286778  # red, on this rule
#
#   The old collapsed rule is NOT reproducible by raising the threshold, and
#   that is the point rather than an inconvenience: the SELF-CHECK no longer has
#   a churn knob to be switched off by.
#
#   The general fact the instance illustrates: ONE commit inserting ONE line
#   anywhere above a citation moves it, whatever the file's churn. Churn bounds
#   how OFTEN that bet is taken, never whether a single commit can lose it — so
#   a threshold is a sound basis for demanding an anchor and never a sound basis
#   for ignoring one that is already there.
#
#   AT THE CITED LINES, not merely somewhere in the file (issue #1338). A symbol
#   the range does not point at re-derives nothing, so it cannot be what earns
#   the exemption; see § Anchor detection below for the rotted ADR citation that
#   rode through on one, and for the ways an author clears it.
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
# Does the doc line name a symbol that actually occurs AT THE CITED LINES? The
# citation text itself is stripped first, or `cli` from `src/cli.rs` would anchor
# a citation to itself. A candidate anchor is any token carrying `_`, any
# CamelCase token, or any backticked token — the three shapes a Rust identifier
# takes in this corpus's prose. "Occurs" (rather than "is defined") is the
# deliberate bar: the point is to leave a reader something to grep for, and a
# type or field name serves that as well as a `fn` does.
#
# WITHIN THE RANGE, not merely somewhere in the file (issue #1338). Anchoring
# exempts a citation from the bare-number rule on the theory that the symbol
# lets a reader RE-DERIVE the location — but a symbol the range does not point
# at re-derives nothing, and the exemption then buys permanent immunity for
# whatever numbers sit beside it. That is not hypothetical:
# `docs/adr/0032-login-is-daemon-routable-tty-gate-is-ours.md` cited the verb
# table as `src/cli.rs:741-765` while the range sat on the tail of `parse_import`,
# `parse`'s doc comment and `fn parse` itself, and rode through on the backticked
# word `login` —
# a real identifier occurring throughout `src/cli.rs`, and nowhere at all inside
# the cited range. Prose about a file names identifiers from that file almost by
# construction, so the file-scoped exemption was weakest exactly where the file
# is largest and churns most. Scoping it to the range makes the citation
# SELF-CHECKING: the symbol and the number have to agree, or one of them is
# wrong. WHICH one is not something this check can see — it measures the
# disagreement and nothing else — so the failure it raises says so, and
# prescribes the remedy for both readings rather than picking one.
#
# The remedies this leaves are all cheap, and the failure below prescribes each
# of them. Widen or correct the range so it covers the symbol; name a symbol that
# is already inside it; or drop the range and cite the symbol alone, which is not
# a `path:NNN` citation at all and is never inspected. None of them is the bulk
# conversion issue #1058 rules out.
#
# The shape only an identifier takes, and the subset of it that is an acronym
# rather than an identifier. Written as one shape and a subtraction, so the two
# can never drift apart: what the check declines is exactly what the hint names.
IDENT_SHAPE='^([A-Za-z0-9]*_[A-Za-z0-9_]*|[A-Za-z]+[A-Z][A-Za-z0-9]*)$'
ACRONYM_SHAPE='^[A-Z0-9]+$'

# Set by anchored() when it refused an ALL-CAPS prose token that DOES occur in
# the cited RANGE — i.e. the tokens that would have anchored the citation before
# issue #1319. Named in the failure so "name a symbol" cannot read as nonsense to
# an author looking straight at a word they believe is one. Range-scoped like the
# acceptance test, so its prescribed remedy — backtick it — is one that would
# actually work; a token outside the range would not anchor even backticked.
declined=""

# Set by anchored() when the line DOES name a real symbol that occurs in the
# cited file but NOT inside the cited range (issue #1338). It gets its own
# message because the author has already done what "name a symbol" asks, so
# repeating that instruction would read as nonsense.
#
# WHAT IS OBSERVED IS THE DISAGREEMENT, NOT ITS CAUSE. That the symbol and the
# number contradict each other is measured. Which of the TWO is wrong is an
# inference this check has no evidence for, and the corpus carries both
# readings: sweep it with `--audit` and most of these reports have their named
# symbol sitting PAST the cited end rather than before its start, and a large
# share of those cite a range made entirely of comment lines — the shape of a
# citation aimed at an item's doc comment, whose own name is just below it. At
# least one such range is RIGHT — `export`'s doc comment in `src/cli.rs` is
# cited for a sentence it literally contains ("with no `path`, to standard
# output"), and `async fn export` sits just past the cited end. Tell that author
# the number drifted and to re-derive the range from the symbol, and they move a
# correct citation onto a signature that does not say it. So the message states
# the disagreement, gives the remedy for BOTH readings, and asserts neither.
# (Named by symbol rather than by line, deliberately: a comment about citation
# rot that carries a rot-able citation is its own counter-example.)
elsewhere=""

occurs() { # <token> <file> [<first> <last>] -> 0 if the token appears there
    # Read the cited file AT THE REF, like every other read here.
    #
    # With <first>/<last> the search is confined to those lines — the range-scoped
    # question, "is the symbol AT the citation". Two loops in `anchored()` ask it
    # and only the first accepts on a hit: the acceptance loop returns 0, taking
    # the doc line outright, while the declined-acronym loop below it records
    # `declined` and accepts nothing. Without <first>/<last> it is the whole-file
    # question, asked only to tell a MISPLACED anchor apart from an ABSENT one: its
    # hit accepts nothing either, it sets `elsewhere`, and that is what routes the
    # refusal to the SELF-CHECK message instead of to the churn-gated DEMAND.
    #
    # Process substitution rather than `git show ... | grep -q`, on BOTH arms.
    # `grep -q` exits at the first match and SIGPIPEs whatever is still writing
    # behind it, so under this script's `pipefail` that pipeline reports FAILURE on
    # a HIT — whenever the writer has not already finished. Process substitution is
    # not a pipeline, so `pipefail` has no writer status to read. Both halves
    # reproduce without a corpus:
    #
    #     set -o pipefail
    #     git show HEAD:src/cli.rs | grep -q parse_subcommand; echo $?    # 141
    #     grep -q parse_subcommand <(git show HEAD:src/cli.rs); echo $?   # 0
    #
    # NEITHER ARM WOULD FAIL LOUDLY IF IT REGRESSED TO THE PIPE FORM. Nothing
    # errors and no citation reports as unreadable; the run just answers a
    # different question, quietly — which is the whole reason to dodge the hazard
    # rather than count on noticing it. Wherever a spurious failure DOES land, this
    # is what each one answers instead:
    #
    #   WHOLE-FILE — `elsewhere` is never set, so a citation whose symbol and
    #   number contradict each other stops being reported as a disagreement and
    #   falls through to the DEMAND, which the churn gate CAN exempt.
    #
    #   RANGED — a doc line the range does anchor is refused; the whole-file arm
    #   then finds the symbol elsewhere, so it is reported as a disagreement
    #   instead, and the churn gate CANNOT exempt that one. The acronym loop reads
    #   through the same arm, so a line that goes on to the DEMAND also loses its
    #   `declined` record and gets the generic wording rather than the one naming
    #   the prose word it refused.
    #
    # Those two are opposite-signed, so the net is a corpus figure like any other:
    # by choice 1 above the SIGN of the gap stays out of this file for the same
    # reason its size does. Measured rather than assumed either way — swap the
    # forms one arm at a time to attribute the change, and both at once for the
    # net, diffing `--audit HEAD` against this form.
    #
    # "Wherever it lands" is the operative clause: NEITHER arm's exposure is
    # structural. Both turn on the same condition — the writer must still have
    # output pending when `grep -q` quits — so a read small enough to sit in the
    # pipe buffer, or whose first match sits late enough to have drained the
    # writer, exits 0 and that citation is decided exactly as before. The arms
    # differ in how often the condition is met, not in kind. A whole source file
    # is routinely large enough to meet it. A cited range is not: `sed -n` prints
    # its whole selection rather than stopping at the range end, but a handful of
    # lines is already buffered before the match is found, which is why converting
    # the ranged arm alone looks harmless. Widen the selection past a pipe buffer
    # and it SIGPIPEs after all, so that is a fact about the ranges this corpus
    # cites, not a property to build on:
    #
    #     set -o pipefail
    #     git show HEAD:src/cli.rs | sed -n 1,200p    | grep -q parse; echo $?  # 0
    #     git show HEAD:src/cli.rs | sed -n 1,100000p | grep -q parse; echo $?  # 141
    if [ "$#" -ge 4 ]; then
        grep -qE "(^|[^A-Za-z0-9_])${1}([^A-Za-z0-9_]|\$)" \
            <(git show "${head_sha}:${2}" 2>/dev/null | sed -n "${3},${4}p")
    else
        grep -qE "(^|[^A-Za-z0-9_])${1}([^A-Za-z0-9_]|\$)" \
            <(git show "${head_sha}:${2}" 2>/dev/null)
    fi
}

anchored() { # <doc-line> <cited-file> <first-line> <last-line>  -> 0 if anchored
    local line="$1" file="$2" first="$3" last="$4" stripped tok
    declined=""
    elsewhere=""
    stripped="$(printf '%s' "$line" | sed -E "s#${CITATION_RE}##g")"
    for tok in $(
        {
            # Inside backticks the author has already declared "this is code", so
            # any identifier counts — `classify`, `export` and `stash` are real
            # single-word `fn` names and would fail a shape test. That declaration
            # is also the escape hatch for a genuine all-caps constant, which the
            # subtraction below would otherwise refuse.
            #
            # A span carrying a `/` is a PATH, not an identifier, and is dropped
            # whole rather than shredded into components (issue #1388). Prose
            # about a citation names neighbouring files almost by construction,
            # and `src/cli.rs` shreds to `src`, `cli`, `rs` — none a symbol. That
            # is the #1319 class one step over, and left alone it MANUFACTURES
            # disagreements now that the self-check is churn-independent:
            # `docs/requirements/gui-cli-capability-parity.md` cites the control
            # socket's verb inventory as `src/daemon/socket.rs:7-46`, which is
            # correct, and names `parse_subcommand` (`src/cli.rs`) alongside it —
            # whereupon `cli` "occurs" in socket.rs, as a bare word in prose,
            # outside the range. Dropping the span whole, rather than filtering
            # the components, is what keeps the rule statable: a path is one
            # token to a reader, so it is one token here.
            printf '%s' "$stripped" \
                | grep -oE '`[^`]*`' \
                | grep -vE '/' \
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
        if occurs "$tok" "$file" "$first" "$last"; then
            return 0
        fi
        # A real symbol, present in the file, absent from the cited lines: the
        # citation contradicts itself. Collected rather than returned on, because
        # a later token may still anchor the line properly.
        if occurs "$tok" "$file"; then
            elsewhere="${elsewhere:+${elsewhere}, }${tok}"
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
        if occurs "$tok" "$file" "$first" "$last"; then
            declined="${declined:+${declined}, }${tok}"
        fi
    done
    return 1
}

# --- The check ---------------------------------------------------------------
check_line() { # <doc> <lineno> <line>
    local doc="$1" lineno="$2" line="$3" cite path first last n c

    for cite in $(printf '%s' "$line" | grep -oE "$CITATION_RE" || true); do
        inspected=$((inspected + 1))
        path="${cite%%:*}"
        # `first` and `last` coincide for a single-line citation, which carries no
        # `-`: both suffix strips are then no-ops and the range is that one line.
        first="${cite#*:}"
        first="${first%%-*}"
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

        # (b) Anchoring. TWO questions, and only the second is churn-gated
        #     (issue #1388). Asking them in one breath is what let a citation
        #     through whose own line contradicted it; see § The two questions.
        c="$(churn "$path")"
        if anchored "$line" "$path" "$first" "$last"; then
            continue
        fi
        # SELF-CHECK: the line already names a symbol, and it disagrees with the
        # number beside it. Nothing is being demanded of the author, so the
        # stable-file exemption has nothing to exempt — checked at any churn.
        if [ -n "$elsewhere" ]; then
            fail "  ${doc}:${lineno}  ${cite}  -> the line names ${elsewhere}, which occurs in ${path} but NOT within lines ${first}-${last} — the symbol and the number disagree, and which of them is wrong is not something this check can see: if the number drifted, re-derive the range from the symbol; if the range is right and the symbol merely sits outside it, widen the range to cover the symbol or name one that is inside. Dropping the range and citing the symbol alone clears it either way"
            continue
        fi
        # DEMAND: the line names no symbol at all. This is what issue #1058
        # exempts for a file whose numbers keep their meaning often enough to
        # bet on, so it stays behind the threshold.
        if [ "$c" -lt "$THRESHOLD" ]; then
            continue
        fi
        if [ -n "$declined" ]; then
            fail "  ${doc}:${lineno}  ${cite}  -> bare line number into a churning file (${c} commits in the last ${WINDOW}); ${declined} occurs at those lines but is prose, not a symbol — name a real one, or backtick it if it is code"
        else
            fail "  ${doc}:${lineno}  ${cite}  -> bare line number into a churning file (${c} commits in the last ${WINDOW}); name a symbol that occurs at those lines"
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
        echo "passes, because \`apply_import\` occurs INSIDE those lines, so the symbol and the number"
        echo "agree — which is what this check measures, and all it measures."
        echo
        echo "That is also the whole of the anchoring rule (issue #1338): a symbol somewhere ELSE in"
        echo "the file exempts nothing. \`login\` occurs all over src/cli.rs and at not one of the"
        echo "lines src/cli.rs:741-765, which is how that range sat rotted in an ADR while the"
        echo "same range, cited without the word, was reported. Widen the range to cover the symbol,"
        echo "or drop the range — a symbol with no line number is not a citation and is never checked."
        echo
        echo "An ALL-CAPS word in running prose is not a symbol — \`CLI\`, \`MUST\` and \`ACL\` all occur"
        echo "in src/, and none of them re-derives anything (issue #1319). If the token really is a"
        echo "constant, backtick it and it counts."
        echo
        echo "Churn gates only whether a citation is ASKED for a symbol. A citation that already"
        echo "names one is checked against its own number whatever the file does (issue #1388), so"
        echo "a report on a quiet file is not a bug. Threshold is ${THRESHOLD} commits over the last ${WINDOW}:"
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
