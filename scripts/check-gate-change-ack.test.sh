#!/usr/bin/env bash
# Self-contained falsifier + regression test for check-gate-change-ack.sh
# (issue #317). Builds a throwaway git repo and exercises the guard across the
# cases that define its contract — in particular proving the gate goes RED on a
# gate-path change without an acknowledgment, and GREEN once the trailer is
# present. Run locally:  ./scripts/check-gate-change-ack.test.sh
set -euo pipefail

here="$(cd "$(dirname "$0")" && pwd)"
guard="$here/check-gate-change-ack.sh"

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
cd "$work"

git init -q
git config user.email test@test.invalid
git config user.name "gate test"
git config commit.gpgsign false

mkdir -p scripts .github/workflows src
printf 'seed\n' > src/lib.rs
git add src/lib.rs
git commit -qm "base"
base="$(git rev-parse HEAD)"

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

# Run the guard against base..HEAD, capturing its exit code without tripping set -e.
run() {
    local rc
    set +e
    "$guard" "$base" "$(git rev-parse HEAD)" >/dev/null 2>&1
    rc=$?
    set -e
    echo "$rc"
}

# Reset to base on a fresh branch. Recreate the dirs: checking out base drops the
# files a prior case committed, and git prunes the emptied directories with them.
from_base() { git checkout -q "$base"; git checkout -q -B "$1"; mkdir -p scripts .github/workflows src; }

# Case 1: change only a non-gate path, no ack -> PASS (ordinary PRs never blocked).
from_base non-gate
printf 'change\n' > src/lib.rs
git add src/lib.rs
git commit -qm "touch src only"
check "non-gate change needs no ack" 0 "$(run)"

# Case 2: change a gate path with NO ack -> RED. This is the falsifier.
from_base gate-noack
printf 'weakened\n' > scripts/check-ci-ok-needs.sh
git add scripts/check-ci-ok-needs.sh
git commit -qm "weaken a gate script"
check "gate change WITHOUT ack is RED" 1 "$(run)"

# Case 3: same gate-path change WITH the ack trailer -> GREEN.
from_base gate-ack
printf 'weakened\n' > scripts/check-ci-ok-needs.sh
git add scripts/check-ci-ok-needs.sh
git commit -qm "weaken a gate script

Gate-Change-Acknowledged: intentional, reviewed gate change"
check "gate change WITH ack is GREEN" 0 "$(run)"

# Case 4: no base/head args (not a PR context, e.g. push to main) -> PASS.
set +e
"$guard" >/dev/null 2>&1
noargs=$?
set -e
check "no PR range passes" 0 "$noargs"

# Case 5: a workflow-file change is a gate change too (path-coverage breadth).
from_base gate-workflow
printf 'name: x\n' > .github/workflows/ci.yml
git add .github/workflows/ci.yml
git commit -qm "edit the workflow without ack"
check ".github/workflows change WITHOUT ack is RED" 1 "$(run)"

# Case 6: deny.toml is an exact-match gate path.
from_base gate-deny
printf 'x\n' > deny.toml
git add deny.toml
git commit -qm "edit deny.toml without ack"
check "deny.toml change WITHOUT ack is RED" 1 "$(run)"

printf '\n%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
