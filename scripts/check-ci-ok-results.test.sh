#!/usr/bin/env bash
# Self-contained falsifier + regression test for check-ci-ok-results.sh (issue
# #1079). Proves the rollup classifier goes RED on the `abandoned` result that
# used to slip through as a pass, GREEN on the results a healthy run really
# produces, and — the property the allow-list shape exists for — RED on a value
# nobody has enumerated yet.
#
# Peer of check-gate-change-ack.test.sh, whose shape this follows deliberately
# so the guards read as one family. It needs no fixture repo: the guard is a
# pure classifier over a results string, so each case is one invocation.
# Run locally:  ./scripts/check-ci-ok-results.test.sh
set -euo pipefail

here="$(cd "$(dirname "$0")" && pwd)"
guard="$here/check-ci-ok-results.sh"

pass=0
fail=0
check() { # <label> <expected-exit> <actual-exit>
    if [ "$2" = "$3" ]; then
        printf 'PASS  %s (exit %s)\n' "$1" "$3"
        pass=$((pass + 1))
    else
        printf 'FAIL  %s (expected exit %s, got %s)\n' "$1" "$2" "$3"
        fail=$((fail + 1))
    fi
}

# Run the guard over one results string, capturing its exit code without tripping set -e.
run() { # <results-string>
    local rc
    set +e
    CI_OK_RESULTS="$1" "$guard" >/dev/null 2>&1
    rc=$?
    set -e
    echo "$rc"
}

# Case 1: an all-green run passes.
check "every job succeeded is GREEN" 0 "$(run "success success success")"

# Case 2: path-skipped jobs are a legitimate pass. Most jobs here are filtered on
# `changes` outputs, so a docs-only PR must not be wedged by a skipped `test`.
check "path-skipped jobs are GREEN" 0 "$(run "success skipped skipped success skipped")"

# Case 3: THE FALSIFIER. The verbatim results string that run 31116958205
# attempt 1 printed, on which `gate-change-ack`, `doc-gates` and `panel-goldens`
# died in `Set up job` and `ci-ok` went green anyway. This is issue #1079.
check "issue #1079's observed run is RED" 1 \
    "$(run "success skipped skipped skipped success skipped abandoned success abandoned abandoned")"

# Case 4: the same defect reduced to one job.
check "a lone abandoned is RED" 1 "$(run "abandoned")"

# Case 5 + 6: the two values the old deny-list did catch must stay caught. Without
# these, a fix for #1079 could silently drop the behaviour it was extending.
check "failure is still RED" 1 "$(run "success failure success")"
check "cancelled is still RED" 1 "$(run "success cancelled success")"

# Case 7: the whole point of inverting to an allow-list. `abandoned` was a value
# GitHub emitted that this repo had never enumerated; there is no reason to think
# it is the last one. An unknown value must fail CLOSED, with no edit here.
check "an unknown future value is RED (fails closed)" 1 "$(run "success neutral")"

# Case 8-10: a gate that evaluates nothing is not a gate. If `ci-ok.needs` were
# emptied, `needs.*.result` would expand to nothing, every job would leave the
# rollup, and a naive loop would report green having checked zero jobs.
check "zero results is RED" 1 "$(run "")"
check "whitespace-only results is RED" 1 "$(run "   ")"
rc_unset=$(
    set +e
    env -u CI_OK_RESULTS "$guard" >/dev/null 2>&1
    echo $?
)
check "unset CI_OK_RESULTS is RED" 1 "$rc_unset"

# Case 11 + 12: re-confirming what issue #1079 asked to re-confirm. `panel-goldens` is
# deliberately soft (issue #754) — every one of its steps is `continue-on-error`,
# so the job's conclusion is `success` whatever its drift verdict says, and it
# reaches the rollup indistinguishable from a real pass. The allow-list must not
# change that: a soft job's `success`, and its path-skipped `skipped`, both pass.
check "a soft job's success still lands GREEN" 0 \
    "$(run "success success success success success success success success success success")"
check "a soft job that was path-skipped still lands GREEN" 0 \
    "$(run "success skipped skipped skipped success skipped skipped success success success")"

# Case 13: the separator is part of the contract. The workflow passes
# `join(needs.*.result, ' ')`; joining with ', ' instead would yield tokens like
# `success,` that match nothing. That must fail closed and loudly rather than be
# quietly tolerated, so a mis-wired separator cannot decay into a silent pass.
check "a comma-joined results string is RED" 1 "$(run "success, skipped, abandoned")"

printf '\n%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
