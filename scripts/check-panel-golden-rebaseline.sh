#!/usr/bin/env bash
# Require an explicit, in-band justification when a PR re-baselines the panel
# golden renders (issue #754).
#
# `apps/menubar/design/renders/panel-goldens/**` is the committed baseline
# `Tests/PanelGoldenParityTests` diffs fresh renders against. A golden IS the
# gate's assertion content: changing one changes what the gate asserts, exactly
# as changing a threshold would. The defect this guard closes is specifically a
# baseline that could move as a SIDE EFFECT — before #754 the only panel
# comparison that existed (`design/build-comparison.py`) sliced the mock LIVE at
# comparison time, so editing `menubar-preview.html` silently re-baselined it and
# left no artifact whose change would show in a diff.
#
# So re-baselining is deliberately a two-part act:
#   1. an explicit command — `TEST_RUNNER_SESSIOMETER_PANEL_GOLDENS=update` (nothing
#      blesses a render implicitly, and there is no auto-bless-on-missing). The prefix is
#      what reaches the test process; an un-prefixed name stops at `xcodebuild` and leaves
#      the regenerate test skipped, which is why the remedy printed below carries it;
#   2. a recorded reason — a
#          Panel-Goldens-Rebaselined: <reason>
#      trailer on a commit in the PR, which this guard enforces.
# The trailer travels with the commit (immutable, no GitHub-API dependency,
# survives PR-label edits) and is auditable with `git log`, so "why did the panel
# look different that week?" has an answer in history rather than in a reviewer's
# memory.
#
# Deliberately SEPARATE from `Gate-Change-Acknowledged:`
# (scripts/check-gate-change-ack.sh, issue #317) rather than folded into it: that
# trailer answers "is this weakening of the merge gate safe?", this one answers
# "what changed in the panel's appearance, and was it intended?". Two different
# questions, two different audit trails — a `git log --grep` for either stays
# meaningful. The shape is copied on purpose, including running from the same
# always-on `gate-change-ack` CI job (same fetch-depth-0 checkout, same PR range),
# so no new job is needed and it cannot be path-skipped past.
#
# Note this guard is REQUIRED from day one, while the drift COMPARISON it protects
# lands non-required (the `panel-goldens` job's soft step, per the issue's RISK-2
# mitigation). That asymmetry is intended: the comparison can be cross-machine
# flaky, so it soft-lands to be measured; this guard is pure git and cannot be
# flaky, and the discipline it enforces is worth nothing if it waits.
#
# Usage:
#   check-panel-golden-rebaseline.sh [<base-ref> <head-ref>]
#
# base/head default to $BASE_SHA/$HEAD_SHA (set from the pull_request event in
# CI). When neither is available there is no PR range to inspect — e.g. a push to
# main, where the guard already ran on the originating PR — and the check passes.
set -euo pipefail

base="${1:-${BASE_SHA:-}}"
head="${2:-${HEAD_SHA:-}}"

goldens_path='apps/menubar/design/renders/panel-goldens/'
trailer='Panel-Goldens-Rebaselined'

if [ -z "$base" ] || [ -z "$head" ]; then
    echo "ok: no PR base/head range to inspect — panel-golden re-baseline justification not required."
    exit 0
fi

# Where head diverged from base: gives the PR's own diff and commit set even if
# base has since moved on independently (identical to `base...head` three-dot).
mergebase="$(git merge-base "$base" "$head")"

changed="$(git diff --name-only "$mergebase" "$head")"
touched="$(printf '%s\n' "$changed" | grep -F "$goldens_path" || true)"

if [ -z "$touched" ]; then
    echo "ok: PR does not touch the panel goldens — no re-baseline justification required."
    exit 0
fi

# First non-empty trailer value across the PR's commits. No early `exit` in awk:
# closing the pipe early would SIGPIPE `git log` and trip `set -o pipefail`.
reason="$(
    git log --pretty="tformat:%(trailers:key=${trailer},valueonly)" \
        "$mergebase..$head" | awk 'NF && !seen { print; seen = 1 }'
)"

if [ -z "$reason" ]; then
    {
        echo "error: this PR changes the committed panel goldens but no commit in it carries a"
        echo "       '${trailer}: <reason>' trailer."
        echo
        echo "Panel goldens touched:"
        printf '%s\n' "$touched" | sed 's/^/  - /'
        echo
        echo "These PNGs are the baseline Tests/PanelGoldenParityTests asserts against, so changing one"
        echo "changes what the gate asserts (issue #754). A baseline must never move as a side effect —"
        echo "that is the exact defect this gate was built to end. Re-bless deliberately:"
        echo
        echo "    cd apps/menubar && xcodegen generate"
        echo "    TEST_RUNNER_SESSIOMETER_PANEL_GOLDENS=update xcodebuild test \\"
        echo "      -project Menubar.xcodeproj -scheme Menubar -configuration Debug \\"
        echo "      -destination 'platform=macOS' CODE_SIGNING_ALLOWED=NO \\"
        echo "      -only-testing:MenubarTests/PanelGoldenParityTests/testRegenerateGoldensWhenExplicitlyRequested"
        echo
        echo "then LOOK at the new renders (a reference you have not looked at is not a reference) and"
        echo "record why they changed:"
        echo
        echo "    git commit --amend --trailer '${trailer}: <what changed in the panel and why>'"
        echo
        echo "See apps/menubar/design/README.md § Panel golden drift gate."
    } >&2
    exit 1
fi

echo "ok: panel-golden re-baseline justified — \"$reason\""
echo "goldens touched:"
printf '%s\n' "$touched" | sed 's/^/  - /'
