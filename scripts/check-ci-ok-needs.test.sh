#!/usr/bin/env bash
# Self-contained falsifier + regression test for check-ci-ok-needs.sh (issue
# #1102). Proves the rollup-coverage guard goes RED on the case it exists for —
# a job present in the workflow but absent from `ci-ok.needs`, which would sit
# outside the merge gate and fail without failing it (issue #318) — and GREEN on
# the workflow as committed.
#
# Peer of check-ci-ok-results.test.sh and check-gate-change-ack.test.sh, whose
# shape this follows deliberately so the guards read as one family. The two
# ci-ok guards are complementary halves of one trust chain: this one keeps a job
# INSIDE the rollup, check-ci-ok-results.sh keeps a job inside the rollup from
# escaping its VERDICT.
#
# `yq` is a hard dependency of the guard, so it is a hard dependency of this test.
# When it is absent this test EXITS NON-ZERO (2) with a loud banner rather than
# reporting green — a test that passes because its dependency was missing is the
# same defect class as #1079, where a gate reported green having evaluated
# nothing. Exit 2 is "did not run", distinct from exit 1 "assertions failed".
#
# Run locally:  ./scripts/check-ci-ok-needs.test.sh
set -euo pipefail

here="$(cd "$(dirname "$0")" && pwd)"
guard="$here/check-ci-ok-needs.sh"
real_workflow="$here/../.github/workflows/ci.yml"

if ! command -v yq >/dev/null 2>&1; then
    echo "=======================================================================" >&2
    echo "SKIPPED (NOT A PASS): 'yq' is not installed, so check-ci-ok-needs.sh"    >&2
    echo "cannot be exercised and nothing here was verified."                      >&2
    echo "Install it (https://github.com/mikefarah/yq) and re-run. Exiting 2 on"   >&2
    echo "purpose: a green here would mean the guard is untested, not sound."      >&2
    echo "=======================================================================" >&2
    exit 2
fi

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

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
# Assert on the guard's OUTPUT, not only its exit code. An exit code alone cannot
# tell a gate that reddened for the stated reason from one that reddened by
# accident, and cannot tell a real pass from a pass over an empty job set.
#
# Needles are chosen to appear ONLY on the red path. A bare job name is not one:
# the success line prints the whole job set, so `grep changes` matches a GREEN run
# too, and the assertion passes while proving nothing. The error listing's bullet
# (`  - changes`) is unique to the failure, so the cases below use that form.
check_says() { # <label> <needle> <text>
    if printf '%s' "$3" | grep -qF -- "$2"; then
        printf 'PASS  %s (output names %s)\n' "$1" "$2"
        pass=$((pass + 1))
    else
        printf 'FAIL  %s (output does not name %s)\n' "$1" "$2"
        printf '      got: %s\n' "$3"
        fail=$((fail + 1))
    fi
}

# Run the guard against a workflow, capturing its exit code without tripping set -e.
run() { # <workflow-path>
    local rc
    set +e
    "$guard" "$1" >/dev/null 2>&1
    rc=$?
    set -e
    echo "$rc"
}
# Same, but capturing merged output instead of the exit code.
say() { # <workflow-path>
    set +e
    "$guard" "$1" 2>&1
    set -e
}

# ---------------------------------------------------------------------------
# Case 1: the committed workflow -> GREEN. The gate must not be red on arrival.
# ---------------------------------------------------------------------------
check "committed ci.yml is GREEN" 0 "$(run "$real_workflow")"

# Case 2: that green must not be VACUOUS. `comm -23` over an empty job set is
# empty, so a workflow whose job enumeration collapsed to nothing would print
# `covers every job ()` and exit 0 having compared nothing — cardinality zero is
# an automatic fail, not a pass. Assert the guard reports a real, plural job set.
ok_line="$(say "$real_workflow")"
covered="$(printf '%s' "$ok_line" | sed -n 's/.*covers every job (\(.*\))\..*/\1/p')"
covered_n="$(printf '%s' "$covered" | wc -w | tr -d ' ')"
if [ "$covered_n" -ge 2 ]; then
    printf 'PASS  committed ci.yml green covers %s jobs, not zero (non-vacuous)\n' "$covered_n"
    pass=$((pass + 1))
else
    printf 'FAIL  committed ci.yml green covered %s jobs — a gate that evaluates nothing is not a gate\n' "$covered_n"
    printf '      got: %s\n' "$ok_line"
    fail=$((fail + 1))
fi

# ---------------------------------------------------------------------------
# Case 3: THE FALSIFIER, built from the real workflow. Drop the FIRST entry of
# `ci-ok.needs` — whichever job that is today — and the guard must go RED and
# name exactly that job. This is issue #318's defect: the job still runs, still
# reports, and its failure no longer fails the gate.
#
# The victim is read out of the file rather than hardcoded, so this case cannot
# rot into a no-op when the job list changes.
# ---------------------------------------------------------------------------
victim="$(yq '.jobs["ci-ok"].needs[0]' "$real_workflow")"
if [ -z "$victim" ] || [ "$victim" = "null" ]; then
    printf 'FAIL  fixture drift: ci-ok.needs is empty in %s; this test cannot build its corpse\n' "$real_workflow"
    fail=$((fail + 1))
else
    corpse="$work/corpse.yml"
    yq "del(.jobs[\"ci-ok\"].needs[] | select(. == \"$victim\"))" "$real_workflow" > "$corpse"
    corpse_out="$(say "$corpse")"
    check "job dropped from ci-ok.needs is RED" 1 "$(run "$corpse")"
    check_says "and it is the missing-from-needs error" "missing from ci-ok.needs" "$corpse_out"
    check_says "and the error names the escaped job" "- $victim" "$corpse_out"
fi

# ---------------------------------------------------------------------------
# Synthetic fixtures. Small enough to read at a glance, so a case's intent is
# visible without cross-referencing the real workflow.
# ---------------------------------------------------------------------------
mk() { # <name> <heredoc on stdin> -> prints the path
    cat > "$work/$1.yml"
    echo "$work/$1.yml"
}

# Case 4: a minimal workflow whose ci-ok.needs names every other job -> GREEN.
covered_wf="$(mk covered <<'EOF'
jobs:
  alpha:
    runs-on: ubuntu-latest
  beta:
    runs-on: ubuntu-latest
  ci-ok:
    needs: [alpha, beta]
    runs-on: ubuntu-latest
EOF
)"
check "needs naming every job is GREEN" 0 "$(run "$covered_wf")"

# Case 5: `ci-ok` is excluded from the set it must cover — it cannot depend on
# itself, and requiring it would make every correct workflow fail. Case 4 already
# passes with ci-ok absent from its own needs; this states the property directly
# so a future "just add every job" simplification of the guard reddens here.
self_wf="$(mk self <<'EOF'
jobs:
  ci-ok:
    needs: [alpha]
    runs-on: ubuntu-latest
  alpha:
    runs-on: ubuntu-latest
EOF
)"
check "ci-ok is not required to name itself" 0 "$(run "$self_wf")"

# Case 6: one job missing from an otherwise-complete needs list -> RED, named.
partial_wf="$(mk partial <<'EOF'
jobs:
  alpha:
    runs-on: ubuntu-latest
  beta:
    runs-on: ubuntu-latest
  ci-ok:
    needs: [alpha]
    runs-on: ubuntu-latest
EOF
)"
check "a job absent from ci-ok.needs is RED" 1 "$(run "$partial_wf")"
check_says "and the error names it" "- beta" "$(say "$partial_wf")"

# Case 7: EVERY missing job is reported, not just the first. A guard that names
# one of three sends the reader round the loop three times.
none_wf="$(mk none <<'EOF'
jobs:
  alpha:
    runs-on: ubuntu-latest
  beta:
    runs-on: ubuntu-latest
  gamma:
    runs-on: ubuntu-latest
  ci-ok:
    needs: [alpha]
    runs-on: ubuntu-latest
EOF
)"
missing_out="$(say "$none_wf")"
check "two jobs absent is RED" 1 "$(run "$none_wf")"
check_says "and beta is reported" "- beta" "$missing_out"
check_says "and gamma is reported too" "- gamma" "$missing_out"

# Case 8: `needs: []` -> RED. This is the shape that would empty `needs.*.result`
# and let every job leave the rollup at once — the escape hatch #318 closed, and
# the input check-ci-ok-results.sh independently treats as fatal on its own side.
empty_wf="$(mk empty-needs <<'EOF'
jobs:
  alpha:
    runs-on: ubuntu-latest
  ci-ok:
    needs: []
    runs-on: ubuntu-latest
EOF
)"
check "an empty ci-ok.needs is RED" 1 "$(run "$empty_wf")"

# Case 9: no `needs:` key at all -> RED. Deleting the line must not read as
# "nothing to cover".
missing_wf="$(mk missing-needs <<'EOF'
jobs:
  alpha:
    runs-on: ubuntu-latest
  ci-ok:
    runs-on: ubuntu-latest
EOF
)"
check "a missing ci-ok.needs is RED" 1 "$(run "$missing_wf")"

# Case 10: no `ci-ok` job at all -> RED. Renaming or deleting the gate must not
# leave the guard with nothing to complain about.
nogate_wf="$(mk no-gate <<'EOF'
jobs:
  alpha:
    runs-on: ubuntu-latest
EOF
)"
check "no ci-ok job at all is RED" 1 "$(run "$nogate_wf")"

# Case 11: no `jobs:` key -> RED. Fails closed on a workflow it cannot parse
# rather than green-lighting a subject it never read.
nojobs_wf="$(mk no-jobs <<'EOF'
name: nothing here
EOF
)"
check "a workflow with no jobs is RED" 1 "$(run "$nojobs_wf")"

# Case 12: a path that does not exist -> RED, for the same reason.
check "a missing workflow path is RED" 1 "$(run "$work/does-not-exist.yml")"

# Case 13: the guard checks only the FORWARD direction, by design — a `needs:`
# entry naming a job that does not exist is caught by GitHub Actions itself
# ("depends on unknown job"), so the guard ignores it. Stated as a case because
# it is a deliberate non-goal recorded in the guard's header, not an oversight,
# and a future `comm -13` addition here would be a behaviour change to argue.
extra_wf="$(mk extra-needs <<'EOF'
jobs:
  alpha:
    runs-on: ubuntu-latest
  ci-ok:
    needs: [alpha, ghost]
    runs-on: ubuntu-latest
EOF
)"
check "an unknown extra needs entry is left to GitHub (GREEN)" 0 "$(run "$extra_wf")"

# Case 14: the guard's own dependency check. With `yq` off PATH it must error and
# exit non-zero, never fall through to a pass — the guard-side half of this
# test's own skip-loudly rule above. Everything the guard touches before that
# check is a bash builtin, so an unusable PATH still reaches it.
#
# The interpreter is named by absolute path on purpose: the guard's `#!/usr/bin/env
# bash` shebang resolves `bash` through PATH, so running it directly here would die
# 127 in `env` and never reach the check — an exit code that looks like a red for
# entirely the wrong reason.
rc_noyq=$(
    set +e
    PATH=/nonexistent "${BASH:-/bin/bash}" "$guard" "$real_workflow" >/dev/null 2>&1
    echo $?
)
check "guard with yq off PATH is RED, not a silent pass" 1 "$rc_noyq"

printf '\n%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
