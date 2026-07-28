#!/usr/bin/env bash
# Self-contained falsifier + regression test for check-cli-golden-rebaseline.sh
# (issue #767). Builds a throwaway git repo and exercises the guard across the
# cases that define its contract — in particular proving the gate goes RED on a
# CLI-golden re-baseline with no recorded reason, and GREEN once the trailer is
# present. A gate that cannot fail is not evidence; this is where that claim is
# demonstrated rather than assumed.
#
# Peer of check-panel-golden-rebaseline.test.sh, whose shape this follows
# deliberately — same throwaway-repo harness, same case table, so the trailer
# guards read as one family. Run locally:  ./scripts/check-cli-golden-rebaseline.test.sh
set -euo pipefail

here="$(cd "$(dirname "$0")" && pwd)"
guard="$here/check-cli-golden-rebaseline.sh"
goldens="build/fixtures/cli-renders"

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
cd "$work"

git init -q
git config user.email test@test.invalid
git config user.name "cli golden test"
git config commit.gpgsign false

mkdir -p "$goldens" build/fixtures src
printf 'seed\n' > src/lib.rs
printf 'ACCOUNT  SESSION%%\n* work   97%%\n' > "$goldens/status-wide-plain.txt"
git add src/lib.rs "$goldens/status-wide-plain.txt"
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
from_base() {
    git checkout -q "$base"
    git checkout -q -B "$1"
    mkdir -p "$goldens" build/fixtures src
}

# Case 1: touch nothing under the goldens dir -> PASS. Ordinary PRs, including
# ones that change the gate's own machinery, are never blocked by this guard.
from_base non-golden
printf 'fn matches() {}\n' > src/render_golden.rs
git add src/render_golden.rs
git commit -qm "edit the golden machinery only"
check "non-golden change needs no justification" 0 "$(run)"

# Case 2: MODIFY a golden with NO trailer -> RED. This is the falsifier: the
# silent-re-baseline path the whole discipline exists to close.
from_base rebaseline-noreason
printf 'ACCOUNT  SESSION%%\n* work   96%%\n' > "$goldens/status-wide-plain.txt"
git add "$goldens/status-wide-plain.txt"
git commit -qm "regenerate goldens"
check "golden re-baseline WITHOUT a reason is RED" 1 "$(run)"

# Case 3: the same change WITH the trailer -> GREEN.
from_base rebaseline-reason
printf 'ACCOUNT  SESSION%%\n* work   96%%\n' > "$goldens/status-wide-plain.txt"
git add "$goldens/status-wide-plain.txt"
git commit -qm "regenerate goldens

CLI-Goldens-Rebaselined: session column widened in #999, re-blessed after review"
check "golden re-baseline WITH a reason is GREEN" 0 "$(run)"

# Case 4: ADDING a new golden is a baseline change too — a new case's first
# blessing is exactly as consequential as re-blessing an old one.
from_base rebaseline-add
printf 'usage — last 24h\n' > "$goldens/stats-wide-unicode-plain.txt"
git add "$goldens/stats-wide-unicode-plain.txt"
git commit -qm "add a golden for a new case"
check "ADDING a golden WITHOUT a reason is RED" 1 "$(run)"

# Case 5: DELETING a golden likewise — dropping a reference silently shrinks what
# the gate covers, which is a weakening the diff alone would not announce.
from_base rebaseline-delete
git rm -q "$goldens/status-wide-plain.txt"
git commit -qm "drop a golden"
check "DELETING a golden WITHOUT a reason is RED" 1 "$(run)"

# Case 6: the trailer may ride ANY commit in the PR, not only the one that touched
# the goldens — the guard scans the whole range, as `git log` audit would.
from_base rebaseline-trailer-elsewhere
printf 'changed\n' > "$goldens/status-wide-plain.txt"
git add "$goldens/status-wide-plain.txt"
git commit -qm "regenerate goldens"
printf 'more\n' > src/lib.rs
git add src/lib.rs
git commit -qm "unrelated follow-up

CLI-Goldens-Rebaselined: blessed alongside the alignment change in this PR"
check "trailer on a LATER commit in the PR is GREEN" 0 "$(run)"

# Case 7: an EMPTY trailer value is not a reason. "CLI-Goldens-Rebaselined:" with
# nothing after it would otherwise satisfy a naive presence check.
from_base rebaseline-empty-reason
printf 'changed\n' > "$goldens/status-wide-plain.txt"
git add "$goldens/status-wide-plain.txt"
git commit -qm "regenerate goldens

CLI-Goldens-Rebaselined:"
check "EMPTY trailer value is RED" 1 "$(run)"

# Case 8: no base/head args (not a PR context, e.g. push to main) -> PASS.
set +e
"$guard" >/dev/null 2>&1
noargs=$?
set -e
check "no PR range passes" 0 "$noargs"

# Case 9: a SIBLING fixture directory is NOT the CLI-renders directory (path-coverage breadth, the
# negative half). Case 1 only proves a far-away path passes; this proves the match is tight enough
# that re-emitting the WIRE goldens — which live one directory up, are a different gate, and have
# their own emitter — does not demand a `CLI-Goldens-Rebaselined:` trailer. A guard that fires on
# unrelated fixtures trains contributors to add the trailer reflexively, and a reflexive trailer is
# exactly the "recorded reason" this guard exists to keep meaningful.
from_base sibling-fixtures
printf '{"schema":"1.4"}\n' > build/fixtures/wire-snapshot-basic.json
git add build/fixtures/wire-snapshot-basic.json
git commit -qm "regenerate the wire goldens"
check "a SIBLING fixture dir needs no CLI-golden reason" 0 "$(run)"

# Case 10: the PANEL goldens are a DIFFERENT surface with their own trailer. A
# panel re-baseline must not be able to satisfy this guard, nor this one that —
# the two audit trails only stay separately greppable if the paths do not overlap.
from_base panel-goldens
mkdir -p apps/menubar/design/renders/panel-goldens
printf 'png\n' > apps/menubar/design/renders/panel-goldens/panel-healthy-light.png
git add apps/menubar/design/renders/panel-goldens/panel-healthy-light.png
git commit -qm "regenerate the panel goldens

Panel-Goldens-Rebaselined: unrelated panel change"
check "a PANEL re-baseline needs no CLI-golden reason" 0 "$(run)"

# Case 11: …and the converse — the CLI trailer does NOT satisfy a CLI golden change
# by name alone; the guard reads THIS trailer, not any trailer. A commit carrying
# only the panel trailer while touching CLI goldens must still go RED.
from_base wrong-trailer
printf 'changed\n' > "$goldens/status-wide-plain.txt"
git add "$goldens/status-wide-plain.txt"
git commit -qm "regenerate goldens

Panel-Goldens-Rebaselined: wrong surface's trailer"
check "the PANEL trailer does NOT justify a CLI re-baseline" 1 "$(run)"

printf '\n%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
