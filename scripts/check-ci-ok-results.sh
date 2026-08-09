#!/usr/bin/env bash
# Fail the build unless every job `ci-ok` rolls up either succeeded or was
# legitimately path-skipped.
#
# `ci-ok` is the single required status check, so this classifier is the last
# thing standing between a run and a merge. It reads the rollup as an
# ALLOW-LIST — `success` and `skipped` pass, anything else fails — rather than
# denying a hand-written list of known-bad values.
#
# That inversion is issue #1079, and the reason for it is worth keeping. The
# gate used to be a workflow `if:` expression that fired only on
# `failure`/`cancelled`, written on the assumption that
# `{skipped, failure, cancelled}` was the whole value space. It is not: GitHub
# also emits `abandoned` for a job that died before running a single step — an
# Actions outage during `Set up job`. `abandoned` matched neither term, the
# fail step was skipped, and `ci-ok` reported green on run 31116958205, where
# three gates never executed at all. A deny-list gate fails OPEN on every value
# its author did not foresee. An allow-list fails CLOSED, including on whatever
# GitHub adds next, with no edit here.
#
# Job names deliberately do not appear in this file. The workflow expands
# `needs.*.result` before calling it, so this stays a pure classifier over an
# opaque list and `ci-ok.needs` coverage remains the sole business of
# check-ci-ok-needs.sh (issue #318). Naming jobs here would reopen exactly the
# escape hatch that guard closed: a job could be added to `needs:`, satisfy that
# check, and still sit outside this one.
#
# Peer of scripts/check-ci-ok-needs.sh (#318) and check-gate-change-ack.sh
# (#317): a small guard, runnable locally — with one difference that matters.
# Those sit in `ci-ok.needs`, so the gate can never be skipped past them; this
# one runs INSIDE `ci-ok` as a step. It is not covered by the rollup, it IS the
# rollup. Its falsifier lives in scripts/check-ci-ok-results.test.sh.
#
# Usage:
#   CI_OK_RESULTS='success skipped abandoned' ./scripts/check-ci-ok-results.sh
#
# `CI_OK_RESULTS` is what the workflow sets from `join(needs.*.result, ' ')`.
set -euo pipefail

results="${CI_OK_RESULTS-}"

count=0
bad=""
# Unquoted on purpose: the results arrive as one whitespace-separated string, and
# word-splitting is what turns it into a list.
for result in $results; do
    count=$((count + 1))
    case "$result" in
        # `skipped` is a pass because most jobs here are filtered on the `changes`
        # outputs — a docs-only PR must not be wedged by a skipped `test`.
        # `success` covers both a real pass and a deliberately soft job whose steps
        # are all `continue-on-error` (`panel-goldens`, issue #754); this gate
        # cannot distinguish the two, and by that job's design it should not try.
        success | skipped) ;;
        *) bad="$bad $result" ;;
    esac
done

# Zero results is not a pass. Were `ci-ok.needs` ever emptied, every job would
# leave the rollup and `ci-ok-needs-complete` would no longer be a dependency
# whose failure could surface — so a loop over nothing would report green having
# verified nothing, which is the same silent green in a different disguise.
if [ "$count" -eq 0 ]; then
    echo "::error::ci-ok evaluated ZERO job results, so it verified nothing."
    {
        echo "error: CI_OK_RESULTS is empty or unset."
        echo
        echo "In .github/workflows/ci.yml, jobs.ci-ok sets it from"
        echo "join(needs.*.result, ' '). An empty value means either that env: entry"
        echo "was dropped or renamed, or that jobs.ci-ok.needs itself is empty."
        echo "Restore whichever is missing: a rollup over nothing is not a pass"
        echo "(issue #1079)."
    } >&2
    exit 1
fi

if [ -n "$bad" ]; then
    # A `::error::` line so the failure is annotated in the checks UI rather than
    # buried in the log — this is the one gate whose red state blocks every merge.
    echo "::error::A required job did not pass (results: $results)"
    {
        echo "error: these job results are neither 'success' nor 'skipped':$bad"
        echo "Full rollup: $results"
        echo
        echo "A job reports 'abandoned' if it died before running a step, typically a"
        echo "GitHub Actions outage during 'Set up job' — re-run the workflow. Any other"
        echo "unrecognised value fails closed by design (issue #1079)."
    } >&2
    exit 1
fi

echo "ok: all $count job results are success or skipped ($results)."
