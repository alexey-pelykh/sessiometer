#!/usr/bin/env bash
# Fail the build if the menu-bar app's own source acquires a network-egress, a
# keychain, or an offline-store-read surface. The app is a pure local-socket client:
# it reaches the daemon over a raw POSIX AF_UNIX control socket ONLY (ADR-0011) — no
# host networking, no analytics, no outbound call of any kind — it never touches the
# keychain (credentials are the daemon's job), and it never reads the daemon's offline
# usage store: everything it needs, including the usage-history series, comes OVER THE
# SOCKET (the daemon `stats` verb, #356), never from the store files directly. This
# guard is the Swift-side peer of scripts/check-no-security-framework.sh (#316): a
# regression gate so a future change can't silently pull the app onto the host network,
# into the keychain, or into a direct store-read (issue #328).
#
# The store-read half closes the #328 gap #356 flagged: the network/keychain checks
# below never forbade a filesystem read of the daemon's store, so a direct store-read
# from the app would have shipped green — precisely the anti-pattern #356's socket verb
# exists to make unnecessary. We forbid the app naming the store ARTIFACTS
# (usage-samples / usage-rollup); the app has no legitimate reason to name them (it
# still references the support-dir SOCKET path, which is allowed and untouched here).
#
# Why source-level, not `otool -L` of the built .app:
# The daemon guard works at the build-INPUT level — it asks whether the
# `security-framework` crate is in `cargo metadata`, not whether the linked binary
# names Security.framework. The faithful Swift analogue is the app's OWN imports.
# `otool -L` is in fact the wrong signal here: Foundation/AppKit transitively pull
# Security.framework AND Network.framework into EVERY Cocoa app's load commands at
# the OS level, so a linked-framework grep would go red on every app regardless of
# whether it uses the keychain or the network. What we actually forbid is the app's
# own code REACHING for those APIs — an `import` of an egress/keychain module, a
# host-networking symbol, or a network entitlement — which is exactly, and only,
# what this inspects. (A binary otool assertion would add nothing here and is a
# deliberate non-goal, not an oversight.)
#
# Scope: apps/menubar/Sources (the compiled app sources — project.yml sources only
# `Sources`, so apps/menubar/spikes, which names these frameworks in prose, is
# correctly excluded) plus the app config (project.yml + any *.entitlements).
#
# Peer of scripts/check-no-security-framework.sh and scripts/check-gate-change-ack.sh:
# a small, fail-closed guard, runnable locally, wired into the `swift` CI job (which
# already sits under the `ci-ok` summary gate, so ci-ok.needs is unchanged).
#
# Usage:
#   check-menubar-zero-egress.sh [<menubar-dir>]
# <menubar-dir> defaults to the repo's apps/menubar; the .test.sh harness passes a
# throwaway tree.
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
menubar="${1:-$root/apps/menubar}"
sources="$menubar/Sources"

if [ ! -d "$sources" ]; then
    echo "error: menu-bar Sources not found at '$sources'." >&2
    exit 1
fi

# Collect the compiled Swift sources up front (avoids an empty-input `xargs` reading
# from stdin, and gives a clear error if the source tree is unexpectedly empty).
swift_files=()
while IFS= read -r -d '' f; do swift_files+=("$f"); done \
    < <(find "$sources" -type f -name '*.swift' -print0)

if [ "${#swift_files[@]}" -eq 0 ]; then
    echo "error: no Swift sources found under '$sources'." >&2
    exit 1
fi

# Two source-level surfaces are forbidden. Both are matched on CODE ONLY: the awk
# strips `//` line comments before testing (preserving line numbers via FNR), so a
# comment that documents the invariant by NAMING a framework — e.g. the existing
# "// No Network.framework, no host" in WatchTransport.swift — is never itself a
# violation. The original line is reported for a legible message.
#
#   1. Forbidden `import`s — modules that ARE the egress/keychain surface. Anchored
#      to the `import` keyword (so, again, prose naming a framework is not a match),
#      and covering the decl-kind form (`import struct Network.NWConnection`) and
#      the `@testable`/`@_exported` attribute forms:
#        Security             keychain / credential APIs — the daemon's job, never the app's
#        Network              NWConnection & friends — host networking (AF_UNIX is raw POSIX)
#        NetworkExtension     VPN / content-filter host networking
#        SystemConfiguration  SCNetworkReachability / dynamic-store host probes
#        CFNetwork            CFURL / CFStream host networking
#
#   2. Forbidden host-networking SYMBOLS that ride in through the always-allowed
#      Foundation (so they carry no forbidden import to anchor on), plus the
#      Network.* types as belt-and-suspenders. Matched as substrings — distinctive
#      enough that this also catches URLSessionConfiguration, NSURLSession, etc.,
#      with no plausible benign identifier containing them:
#        URLSession URLRequest NSURLConnection
#        NWConnection NWListener NWEndpoint NWBrowser NWPath
#
#   3. Forbidden STORE ARTIFACTS — the daemon's offline usage store, which the app must
#      never read directly (it gets the usage series over the socket, #356). Matched as
#      substrings of the store filenames (src/paths.rs), distinctive enough to have no
#      benign use in the app — and, being code-only like the rest, a doc comment that
#      NAMES them (e.g. explaining why the app doesn't read them) is not a violation.
#      The support-dir SOCKET path (Library/Application Support/sessiometer/daemon.sock)
#      is deliberately NOT matched — that reference is the app's legitimate transport:
#        usage-samples   the raw usage-sample log (usage-samples.jsonl)
#        usage-rollup    the rolled usage aggregates (usage-rollup.json)
hits="$(
    awk '
        { code = $0; sub(/\/\/.*/, "", code) }
        code ~ /^[[:space:]]*(@[A-Za-z_]+[[:space:]]+)?import([[:space:]]+[a-z]+)?[[:space:]]+(Security|Network|NetworkExtension|SystemConfiguration|CFNetwork)([[:space:]]|\.|$)/ \
            { printf "%s:%d: forbidden import (keychain/host-networking) -> %s\n", FILENAME, FNR, $0; next }
        code ~ /(URLSession|URLRequest|NSURLConnection|NWConnection|NWListener|NWEndpoint|NWBrowser|NWPath)/ \
            { printf "%s:%d: forbidden host-networking symbol -> %s\n", FILENAME, FNR, $0 }
        code ~ /(usage-samples|usage-rollup)/ \
            { printf "%s:%d: forbidden store-path read (socket client, not a store reader) -> %s\n", FILENAME, FNR, $0 }
    ' "${swift_files[@]}" || true
)"

# Network entitlement — the app declares none (a raw AF_UNIX socket needs no App
# Sandbox network entitlement). Guard project.yml AND any *.entitlements the app
# grows later.
ent_files=()
[ -f "$menubar/project.yml" ] && ent_files+=("$menubar/project.yml")
while IFS= read -r -d '' f; do ent_files+=("$f"); done \
    < <(find "$menubar" -type f -name '*.entitlements' -print0 2>/dev/null)

ent_hits=""
if [ "${#ent_files[@]}" -gt 0 ]; then
    ent_hits="$(
        grep -nE 'com\.apple\.security\.network\.(client|server)' "${ent_files[@]}" 2>/dev/null || true
    )"
fi

if [ -n "$hits" ] || [ -n "$ent_hits" ]; then
    {
        echo "error: the menu-bar app must stay a pure local-socket client (zero network"
        echo "       egress, no keychain, no store-read), but a forbidden surface appeared:"
        echo
        [ -n "$hits" ] && printf '%s\n' "$hits" | sed 's/^/  /'
        [ -n "$ent_hits" ] && printf '%s\n' "$ent_hits" | sed 's/^/  network entitlement -> /'
        echo
        echo "The app talks to the daemon over a raw POSIX AF_UNIX socket ONLY (ADR-0011):"
        echo "no Network.framework/NWConnection, no URLSession/URLRequest, no Security.framework"
        echo "keychain access (credentials are the daemon's job), no network entitlement, and no"
        echo "direct read of the daemon's usage store (the usage series comes over the socket via"
        echo "the daemon 'stats' verb, #356 — never from usage-samples/usage-rollup directly)."
        echo "If this surface is genuinely intended it is an architecture change — reconsider it"
        echo "against ADR-0011, don't relax the gate."
    } >&2
    exit 1
fi

echo "ok: menu-bar app is a pure local-socket client — no Security/Network/URLSession"
echo "    surface in apps/menubar/Sources, no network entitlement, and no store-path read."
