#!/usr/bin/env bash
# Fail the build if any CI job is missing from `ci-ok.needs`.
#
# `ci-ok` is the single always-running required check; it rolls up every job in
# its `needs:` list via `needs.*.result`, failing on any `failure`/`cancelled`
# (a path-skipped job is a pass). That `needs:` list is the manual source of
# truth for "which jobs the gate covers". Add a job to the workflow and forget to
# add it to `ci-ok.needs`, and that job sits OUTSIDE the rollup — its failure
# would not fail the gate. GitHub exposes no "all jobs in this workflow" context,
# so this guard parses the workflow and asserts the invariant that every job
# other than `ci-ok` appears in `ci-ok.needs` (issue #318). It converts the one
# manual rollup invariant from discipline to enforcement.
#
# The reverse direction — a `needs:` entry that is not a real job — is already
# enforced by GitHub Actions itself ("depends on unknown job"), so this only
# checks the forward, silently-rotting direction.
set -euo pipefail

workflow="${1:-.github/workflows/ci.yml}"

if ! command -v yq >/dev/null 2>&1; then
    echo "error: 'yq' is required to parse ${workflow} but was not found." >&2
    echo "It is preinstalled on GitHub ubuntu runners; locally: https://github.com/mikefarah/yq" >&2
    exit 1
fi

# Every job id except the gate itself — the set that must be covered.
all_jobs="$(yq '.jobs | keys | .[] | select(. != "ci-ok")' "$workflow" | sort)"
# The gate's declared coverage.
declared_needs="$(yq '.jobs["ci-ok"].needs[]' "$workflow" | sort)"

# Jobs present in the workflow but absent from ci-ok.needs (comm column 1 only).
missing="$(comm -23 <(printf '%s\n' "$all_jobs") <(printf '%s\n' "$declared_needs"))"

if [ -n "$missing" ]; then
    echo "error: these jobs are missing from ci-ok.needs — their failure would NOT fail the gate:" >&2
    while IFS= read -r job; do
        printf '  - %s\n' "$job" >&2
    done <<< "$missing"
    echo "Add them to .github/workflows/ci.yml -> jobs.ci-ok.needs (issue #318)." >&2
    exit 1
fi

echo "ok: ci-ok.needs covers every job ($(printf '%s' "$all_jobs" | tr '\n' ' '))."
