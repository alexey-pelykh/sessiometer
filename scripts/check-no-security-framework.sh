#!/usr/bin/env bash
# Fail the build if `security-framework` (a Security.framework SDK binding) is
# linked into the dependency graph.
#
# All keychain access must go through the /usr/bin/security CLI (see issue #2).
# Writing the credential via the Security.framework SDK as our own code identity
# re-stamps the keychain item's ACL partition list to our team ID and evicts the
# original `apple-tool:` entry, breaking Claude Code's silent read. The CLI write
# rides `apple-tool:`, preserving it. This guard stops a refactor from silently
# pulling in the SDK write path.
set -euo pipefail

meta="$(cargo metadata --format-version 1 --locked)"

if printf '%s' "$meta" | jq -e '.packages[] | select(.name == "security-framework")' >/dev/null; then
    echo "error: 'security-framework' is in the dependency graph." >&2
    echo "All keychain access must go through the /usr/bin/security CLI (see issue #2)." >&2
    exit 1
fi

echo "ok: no 'security-framework' / Security.framework SDK linkage."
