#!/usr/bin/env bash
# Require an explicit, in-band justification when a PR re-baselines the CLI
# full-output render goldens (issue #767).
#
# `build/fixtures/cli-renders/**` holds the committed whole-output renders of
# `status`, `stats` and `reliability` that the in-module golden tests compare
# fresh renders against byte-for-byte. A golden IS the gate's assertion content:
# changing one changes what the gate asserts, exactly as changing a threshold
# would. The failure this guards is a baseline moving as a SIDE EFFECT — a
# render tweak, an "obviously cosmetic" alignment change, a re-run of the
# emitter to make a red suite green — leaving no artifact whose change tells a
# reviewer that the CLI now looks different.
#
# So re-baselining is deliberately a two-part act:
#   1. an explicit command — `cargo test -- --ignored emit_cli_render_goldens`
#      (the emitters are `#[ignore]`d, so nothing blesses a render implicitly,
#      and there is no auto-bless-on-missing anywhere in the suite);
#   2. a recorded reason — a
#          CLI-Goldens-Rebaselined: <reason>
#      trailer on a commit in the PR, which this guard enforces.
# The trailer travels with the commit (immutable, no GitHub-API dependency,
# survives PR-label edits) and is auditable with `git log`, so "why did `stats`
# print differently that week?" has an answer in history rather than in a
# reviewer's memory.
#
# Deliberately SEPARATE from both sibling trailers rather than folded into
# either:
#   * `Gate-Change-Acknowledged:` (scripts/check-gate-change-ack.sh, issue #317)
#     answers "is this weakening of the merge gate safe?";
#   * `Panel-Goldens-Rebaselined:` (scripts/check-panel-golden-rebaseline.sh,
#     issue #754) answers "what changed in the PANEL's appearance?";
#   * this one answers "what changed in the CLI's rendered TEXT?".
# Three different questions, three audit trails — a `git log --grep` for any one
# of them stays meaningful. The shape is copied from the panel guard on purpose,
# including running from the same always-on `gate-change-ack` CI job (same
# fetch-depth-0 checkout, same PR range), so no new job is needed and it cannot
# be path-skipped past.
#
# Unlike the panel guard, the comparison this protects is a REQUIRED gate from
# day one: it is a byte comparison of deterministic text with no rendering,
# no antialiasing and no cross-machine variance, so it cannot be flaky.
#
# Usage:
#   check-cli-golden-rebaseline.sh [<base-ref> <head-ref>]
#
# base/head default to $BASE_SHA/$HEAD_SHA (set from the pull_request event in
# CI). When neither is available there is no PR range to inspect — e.g. a push to
# main, where the guard already ran on the originating PR — and the check passes.
set -euo pipefail

base="${1:-${BASE_SHA:-}}"
head="${2:-${HEAD_SHA:-}}"

goldens_path='build/fixtures/cli-renders/'
trailer='CLI-Goldens-Rebaselined'

if [ -z "$base" ] || [ -z "$head" ]; then
    echo "ok: no PR base/head range to inspect — CLI-golden re-baseline justification not required."
    exit 0
fi

# Where head diverged from base: gives the PR's own diff and commit set even if
# base has since moved on independently (identical to `base...head` three-dot).
mergebase="$(git merge-base "$base" "$head")"

changed="$(git diff --name-only "$mergebase" "$head")"
touched="$(printf '%s\n' "$changed" | grep -F "$goldens_path" || true)"

if [ -z "$touched" ]; then
    echo "ok: PR does not touch the CLI render goldens — no re-baseline justification required."
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
        echo "error: this PR changes the committed CLI render goldens but no commit in it carries a"
        echo "       '${trailer}: <reason>' trailer."
        echo
        echo "CLI render goldens touched:"
        printf '%s\n' "$touched" | sed 's/^/  - /'
        echo
        echo "These files are the baseline the in-module golden tests assert against, so changing one"
        echo "changes what the gate asserts (issue #767). A baseline must never move as a side effect."
        echo "Re-bless deliberately:"
        echo
        echo "    cargo test -- --ignored emit_cli_render_goldens"
        echo
        echo "then LOOK at the regenerated files (a reference you have not looked at is not a"
        echo "reference) and record why they changed:"
        echo
        echo "    git commit --amend --trailer '${trailer}: <what changed in the render and why>'"
        echo
        echo "See src/render_golden.rs for the golden machinery and the re-baseline contract."
    } >&2
    exit 1
fi

echo "ok: CLI-golden re-baseline justified — \"$reason\""
echo "goldens touched:"
printf '%s\n' "$touched" | sed 's/^/  - /'
