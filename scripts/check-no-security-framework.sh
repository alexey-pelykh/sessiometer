#!/usr/bin/env bash
# Fail the build if a Security.framework SDK binding is linked into the
# dependency graph.
#
# All keychain access must go through the /usr/bin/security CLI (see issue #2).
# Writing the credential via the Security.framework SDK as our own code identity
# re-stamps the keychain item's ACL partition list to our team ID and evicts the
# original `apple-tool:` entry, breaking Claude Code's silent read. The CLI write
# rides `apple-tool:`, preserving it. This guard stops a refactor from silently
# pulling in the SDK write path.
#
# WHAT THE MATCH ACCEPTS. Issue #2 requires the build to fail on
# `security-framework` "or any direct Security.framework binding" — so the match
# is not the crate name alone. It is by crate NAME, compared WHOLE, against the
# four published crates whose own manifest declares them Apple Security.framework
# bindings:
#
#   security-framework            "Security.framework bindings for macOS and iOS"
#   security-framework-sys        "Apple `Security.framework` low-level FFI bindings"
#   apple-security-framework      the x52dev fork of the pair, whose manifests
#   apple-security-framework-sys  declare "Apple `Security.framework` bindings for
#                                 macOS and iOS" / "... low-level FFI bindings",
#                                 the -sys half carrying the same
#                                 link(name = "Security", kind = "framework")
#
# Each pair is a wrapper over its `-sys` crate, and the `-sys` crate is where the
# `#[link]` attribute lives. Both halves are listed because a graph can carry the
# raw FFI crate with no wrapper above it — which is precisely the case that used
# to pass (issue #1233, residual A).
#
# WHAT IT REJECTS, AND WHY IT IS WHOLE-NAME. A name that merely CONTAINS the
# token does not match, and that boundary is not hypothetical: crates.io also
# publishes `tls-api-security-framework` and `tls-api-security-framework-2`,
# which are TLS API adapters rather than bindings. A `contains()` match reddens
# on both. Whole-name matching does not.
#
# Narrowness is not the cost it looks like, because the names are matched over
# the FULL RESOLVED graph: `cargo metadata` lists transitive packages too, so a
# crate reaching Security.framework THROUGH one of the four is caught at the
# binding's own node and never has to match by its own name.
#
# RESIDUAL, measured rather than assumed: a direct binding published under a name
# outside those four still passes. The obvious generalisation does not close it.
# `cargo metadata` does expose a `.links` key, but none of the four declares
# `links` in its manifest — the binding is a source-level `#[link]` attribute — so
# a `.links == "Security"` matcher matches none of them and would be inert. The
# residual is pinned in check-no-security-framework.test.sh, not left to
# inference.
#
# EXIT CODES. 0 = checked, no linkage found. 1 = a binding is in the graph.
# 2 = the check could not be EVALUATED, so no verdict was reached — the same
# "degrade to a safe exit 2 rather than a false pass/fail" convention
# check-cc-version.sh already uses.
set -euo pipefail

meta="$(cargo metadata --format-version 1 --locked)"

# EVALUABILITY FIRST. Silence only means something once the query is known to have
# RUN. The previous form asked jq a yes/no question inside an `if`, which read a jq
# that ERRORED exactly as it read a jq that found nothing: a document without
# `.packages` — a cargo metadata schema change, not a hypothetical — made jq exit
# 5, the `if` took that as "no match", and the guard printed `ok:` and exited 0.
# For a CI-blocking security gate that is a fail-open (issue #1233, residual C;
# structurally issue #1079). Assert the shape the match needs and fail CLOSED when
# it is absent, so "could not check" is never reported as "nothing to find".
#
# This also closes the empty-package-set hole (residual B) rather than leaving it
# to a separate test: a real graph always contains at least this crate, so zero
# packages is a document this guard cannot judge, not a graph that happens to be
# clean.
if ! printf '%s' "$meta" \
    | jq -e 'type == "object" and (.packages | type) == "array" and (.packages | length) > 0' \
        >/dev/null 2>&1; then
    echo "error: cannot evaluate the dependency graph." >&2
    echo "'cargo metadata' yielded no non-empty .packages array, so the Security.framework" >&2
    echo "linkage check did NOT run. Refusing to report a pass that was never measured." >&2
    exit 2
fi

count="$(printf '%s' "$meta" | jq '.packages | length')"

# Collecting the names (rather than asking `jq -e` a yes/no question) is what lets
# the error name what it found. It is also fail-closed a second way: under
# `set -euo pipefail` a jq that errors here fails the assignment and aborts the
# script, so a reshaped document can never reach the `ok:` line below.
hits="$(printf '%s' "$meta" | jq -r '
    [ .packages[].name
      | select(. == "security-framework"       or . == "security-framework-sys"
            or . == "apple-security-framework" or . == "apple-security-framework-sys")
    ] | unique | join(", ")')"

if [ -n "$hits" ]; then
    echo "error: a Security.framework SDK binding is in the dependency graph: $hits" >&2
    echo "All keychain access must go through the /usr/bin/security CLI (see issue #2)." >&2
    exit 1
fi

echo "ok: no Security.framework SDK linkage (packages checked: $count)."
