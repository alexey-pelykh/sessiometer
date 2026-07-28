#!/usr/bin/env bash
# Self-contained falsifier + regression test for check-panel-golden-rebaseline.sh
# (issue #754). Builds a throwaway git repo and exercises the guard across the
# cases that define its contract — in particular proving the gate goes RED on a
# golden re-baseline with no recorded reason, and GREEN once the trailer is
# present. A gate that cannot fail is not evidence; this is where that claim is
# demonstrated rather than assumed.
#
# Peer of check-gate-change-ack.test.sh, whose shape this follows deliberately —
# same throwaway-repo harness, same case table, so the two trailer guards read as
# one family. Run locally:  ./scripts/check-panel-golden-rebaseline.test.sh
set -euo pipefail

here="$(cd "$(dirname "$0")" && pwd)"
guard="$here/check-panel-golden-rebaseline.sh"
goldens="apps/menubar/design/renders/panel-goldens"

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
cd "$work"

git init -q
git config user.email test@test.invalid
git config user.name "panel golden test"
git config commit.gpgsign false

mkdir -p "$goldens" apps/menubar/Tests apps/menubar/design/renders src
printf 'seed\n' > src/lib.rs
printf 'not-really-a-png\n' > "$goldens/panel-healthy-light.png"
git add src/lib.rs "$goldens/panel-healthy-light.png"
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
    mkdir -p "$goldens" apps/menubar/Tests apps/menubar/design/renders src
}

# Case 1: touch nothing under the goldens dir -> PASS. Ordinary PRs, including
# ones that change the gate's own TEST code, are never blocked by this guard.
from_base non-golden
printf 'func testSomething() {}\n' > apps/menubar/Tests/PanelGoldenParityTests.swift
git add apps/menubar/Tests/PanelGoldenParityTests.swift
git commit -qm "edit the gate's test code only"
check "non-golden change needs no justification" 0 "$(run)"

# Case 2: MODIFY a golden with NO trailer -> RED. This is the falsifier: the
# silent-re-baseline path the whole issue exists to close.
from_base rebaseline-noreason
printf 'silently-different\n' > "$goldens/panel-healthy-light.png"
git add "$goldens/panel-healthy-light.png"
git commit -qm "regenerate goldens"
check "golden re-baseline WITHOUT a reason is RED" 1 "$(run)"

# Case 3: the same change WITH the trailer -> GREEN.
from_base rebaseline-reason
printf 'deliberately-different\n' > "$goldens/panel-healthy-light.png"
git add "$goldens/panel-healthy-light.png"
git commit -qm "regenerate goldens

Panel-Goldens-Rebaselined: footer copy changed in #999, re-blessed after review"
check "golden re-baseline WITH a reason is GREEN" 0 "$(run)"

# Case 4: ADDING a new golden is a baseline change too — a new fixture's first
# blessing is exactly as consequential as re-blessing an old one.
from_base rebaseline-add
printf 'new\n' > "$goldens/panel-brand-new-state-dark.png"
git add "$goldens/panel-brand-new-state-dark.png"
git commit -qm "add a golden for a new fixture"
check "ADDING a golden WITHOUT a reason is RED" 1 "$(run)"

# Case 5: DELETING a golden likewise — dropping a reference silently shrinks what
# the gate covers, which is a weakening the diff alone would not announce.
from_base rebaseline-delete
git rm -q "$goldens/panel-healthy-light.png"
git commit -qm "drop a golden"
check "DELETING a golden WITHOUT a reason is RED" 1 "$(run)"

# Case 6: the trailer may ride ANY commit in the PR, not only the one that touched
# the goldens — the guard scans the whole range, as `git log` audit would.
from_base rebaseline-trailer-elsewhere
printf 'changed\n' > "$goldens/panel-healthy-light.png"
git add "$goldens/panel-healthy-light.png"
git commit -qm "regenerate goldens"
printf 'more\n' > src/lib.rs
git add src/lib.rs
git commit -qm "unrelated follow-up

Panel-Goldens-Rebaselined: blessed alongside the layout change in this PR"
check "trailer on a LATER commit in the PR is GREEN" 0 "$(run)"

# Case 7: an EMPTY trailer value is not a reason. "Panel-Goldens-Rebaselined:"
# with nothing after it would otherwise satisfy a naive presence check.
from_base rebaseline-empty-reason
printf 'changed\n' > "$goldens/panel-healthy-light.png"
git add "$goldens/panel-healthy-light.png"
git commit -qm "regenerate goldens

Panel-Goldens-Rebaselined:"
check "EMPTY trailer value is RED" 1 "$(run)"

# Case 8: no base/head args (not a PR context, e.g. push to main) -> PASS.
set +e
"$guard" >/dev/null 2>&1
noargs=$?
set -e
check "no PR range passes" 0 "$noargs"

# Case 9: a SIBLING directory under design/renders/ is NOT the goldens directory (path-coverage
# breadth, the negative half). Case 1 only proves a FAR-AWAY path passes; this proves the match is
# tight enough that re-rendering the bar-glyph references — which live one directory over, and have
# their own gate — does not demand a `Panel-Goldens-Rebaselined:` trailer. A guard that fires on
# unrelated renders trains contributors to add the trailer reflexively, and a reflexive trailer is
# exactly the "recorded reason" this guard exists to keep meaningful.
from_base sibling-renders
printf 'bar-glyph\n' > apps/menubar/design/renders/bar-glyph-healthy-light@2x.png
git add apps/menubar/design/renders/bar-glyph-healthy-light@2x.png
git commit -qm "regenerate the bar-glyph references"
check "a SIBLING render dir needs no panel-golden reason" 0 "$(run)"

printf '\n%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
