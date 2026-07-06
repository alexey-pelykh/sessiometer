#!/usr/bin/env bash
# Require an explicit, in-band acknowledgment when a PR changes the merge gate's
# own definition (issue #317).
#
# `ci-ok` is the single required status check, and the invariants it rolls up all
# live in PR-mutable files:
#   .github/workflows/**  the gate wiring itself (`if: always()`, `ci-ok.needs`)
#   scripts/**            the guard scripts the gate runs
#   deny.toml             the cargo-deny supply-chain gate config
#   .cargo/**             linker flags / source replacement that can disarm the
#                         Rust jobs before the gate ever sees them
# With `required_approving_review_count: 0` (solo repo) nothing forces a second
# look, so a single PR could quietly weaken the gate and still merge green.
#
# A *required human review* on these paths cannot be the fix: `main-protection`
# has no bypass actors and GitHub forbids approving your own PR, so it would
# deadlock every gate-touching PR forever (why the issue's literal CODEOWNERS ask
# was rejected). Instead this guard makes a gate change LOUD rather than silent:
# if the PR's diff touches any gate-definition path, at least one commit in the PR
# must carry a
#     Gate-Change-Acknowledged: <reason>
# trailer. The trailer travels with the commit (immutable, no GitHub-API
# dependency, survives PR-label edits) and is auditable with `git log`. A PR that
# touches no gate path needs no acknowledgment and passes untouched, so ordinary
# PRs are never blocked.
#
# Peer of scripts/check-ci-ok-needs.sh (issue #318): a small git/jq/yq guard,
# runnable locally, wired into ci-ok.needs so it can never be skipped past.
#
# Usage:
#   check-gate-change-ack.sh [<base-ref> <head-ref>]
#
# base/head default to $BASE_SHA/$HEAD_SHA (set from the pull_request event in
# CI). When neither is available there is no PR range to inspect — e.g. a push to
# main, where the gate already ran on the originating PR — and the check passes.
set -euo pipefail

base="${1:-${BASE_SHA:-}}"
head="${2:-${HEAD_SHA:-}}"

if [ -z "$base" ] || [ -z "$head" ]; then
    echo "ok: no PR base/head range to inspect — gate-change acknowledgment not required."
    exit 0
fi

# Where head diverged from base: gives the PR's own diff and commit set even if
# base has since moved on independently (identical to `base...head` three-dot).
mergebase="$(git merge-base "$base" "$head")"

# Gate-definition paths this PR touches. Keep the pattern in sync with the issue
# #317 scope and the rust path-filter in .github/workflows/ci.yml. (A `grep -E`
# filter rather than a `case`/`while` loop: bash 3.2 — still the system bash on
# macOS — mis-parses a `case` inside `$(...)`, reading the pattern's `)` as the
# end of the command substitution.)
changed="$(git diff --name-only "$mergebase" "$head")"
touched="$(
    printf '%s\n' "$changed" \
        | grep -E '^(\.github/workflows/.*|scripts/.*|\.cargo/.*|deny\.toml)$' || true
)"

if [ -z "$touched" ]; then
    echo "ok: PR touches no gate-definition paths — no acknowledgment required."
    exit 0
fi

# First non-empty Gate-Change-Acknowledged trailer value across the PR's commits.
# No early `exit` in awk: closing the pipe early would SIGPIPE `git log` and trip
# `set -o pipefail`.
reason="$(
    git log --pretty='tformat:%(trailers:key=Gate-Change-Acknowledged,valueonly)' \
        "$mergebase..$head" | awk 'NF && !seen { print; seen = 1 }'
)"

if [ -z "$reason" ]; then
    {
        echo "error: this PR changes gate-definition files but no commit in it carries a"
        echo "       'Gate-Change-Acknowledged: <reason>' trailer."
        echo
        echo "Gate-definition files touched:"
        printf '%s\n' "$touched" | sed 's/^/  - /'
        echo
        echo "'ci-ok' and the scripts / deny.toml / .cargo config it depends on are the sole"
        echo "automated merge guardrail; changing them must be deliberate and auditable"
        echo "(issue #317). A required human review would deadlock this solo repo, so instead"
        echo "acknowledge the change in-band by adding a trailer to a commit in this PR:"
        echo
        echo "    git commit --amend --trailer 'Gate-Change-Acknowledged: <why this change is safe>'"
        echo
        echo "then force-push. The reason is recorded in history for audit."
    } >&2
    exit 1
fi

echo "ok: gate-definition change acknowledged — \"$reason\""
echo "touched:"
printf '%s\n' "$touched" | sed 's/^/  - /'
