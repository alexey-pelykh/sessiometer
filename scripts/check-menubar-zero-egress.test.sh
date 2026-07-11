#!/usr/bin/env bash
# Self-contained falsifier + regression test for check-menubar-zero-egress.sh
# (issue #328). Builds throwaway menu-bar source trees and exercises the guard
# across the cases that define its contract — in particular proving it goes RED on
# a forbidden import / host-networking symbol / network entitlement, and GREEN on
# clean sources (including sources whose COMMENTS name a forbidden framework, which
# must NOT be a violation). Run locally:  ./scripts/check-menubar-zero-egress.test.sh
set -euo pipefail

here="$(cd "$(dirname "$0")" && pwd)"
guard="$here/check-menubar-zero-egress.sh"

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

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

# Run the guard against a throwaway menubar dir, capturing its exit code without
# tripping set -e.
run() { # <menubar-dir>
    local rc
    set +e
    "$guard" "$1" >/dev/null 2>&1
    rc=$?
    set -e
    echo "$rc"
}

# Build a fresh menubar tree under $work/<name> with a clean, representative
# Sources/ (the real app's imports) and a clean project.yml. Returns the dir path.
new_tree() { # <name>
    local dir="$work/$1"
    mkdir -p "$dir/Sources"
    cat > "$dir/Sources/WatchTransport.swift" <<'EOF'
// same-user local UDS. No Network.framework, no host, no analytics, no outbound call.
import Foundation
import os
import Darwin

let url = URL(fileURLWithPath: "/tmp/sock")  // a plain file URL is fine, not URLSession
EOF
    cat > "$dir/Sources/main.swift" <<'EOF'
import AppKit
EOF
    cat > "$dir/project.yml" <<'EOF'
name: Menubar
targets:
  Menubar:
    type: application
    sources:
      - Sources
EOF
    echo "$dir"
}

# Case 1: clean sources (real-app imports + a comment naming Network.framework +
# a bare file URL) -> GREEN. This is the by-construction baseline AND the
# comment-safety / URL-vs-URLSession falsifier in one.
clean="$(new_tree clean)"
check "clean pure-client sources are GREEN" 0 "$(run "$clean")"

# Case 2: `import Network` -> RED. The primary regression this gate exists to catch.
netimp="$(new_tree netimp)"
printf 'import Network\n' >> "$netimp/Sources/main.swift"
check "import Network is RED" 1 "$(run "$netimp")"

# Case 3: `import Security` (keychain) -> RED.
secimp="$(new_tree secimp)"
printf 'import Security\n' >> "$secimp/Sources/main.swift"
check "import Security is RED" 1 "$(run "$secimp")"

# Case 4: `import SystemConfiguration` (host reachability) -> RED.
scimp="$(new_tree scimp)"
printf 'import SystemConfiguration\n' >> "$scimp/Sources/main.swift"
check "import SystemConfiguration is RED" 1 "$(run "$scimp")"

# Case 5: a URLSession usage reachable via the allowed Foundation -> RED.
urlsess="$(new_tree urlsess)"
printf 'let t = URLSession.shared.dataTask(with: req)\n' >> "$urlsess/Sources/WatchTransport.swift"
check "URLSession symbol is RED" 1 "$(run "$urlsess")"

# Case 6: an NWConnection usage -> RED.
nwconn="$(new_tree nwconn)"
printf 'let c = NWConnection(host: h, port: p, using: .tcp)\n' >> "$nwconn/Sources/WatchTransport.swift"
check "NWConnection symbol is RED" 1 "$(run "$nwconn")"

# Case 7: the decl-kind import form `import struct Network.NWConnection` -> RED
# (proves the import anchor covers more than the bare `import Module` form).
declimp="$(new_tree declimp)"
printf 'import struct Network.NWConnection\n' >> "$declimp/Sources/main.swift"
check "decl-kind import (import struct Network.X) is RED" 1 "$(run "$declimp")"

# Case 8: a comment that MENTIONS a forbidden symbol is NOT a violation -> GREEN
# (the strongest comment-safety falsifier: even the symbol matcher is code-only).
commentsym="$(new_tree commentsym)"
printf '// we deliberately never use URLSession or NWConnection here\n' >> "$commentsym/Sources/WatchTransport.swift"
check "comment naming a forbidden symbol is GREEN" 0 "$(run "$commentsym")"

# Case 9: a network entitlement in project.yml -> RED.
entyml="$(new_tree entyml)"
printf '        com.apple.security.network.client: true\n' >> "$entyml/project.yml"
check "network entitlement in project.yml is RED" 1 "$(run "$entyml")"

# Case 10: a network entitlement in a *.entitlements file -> RED.
entfile="$(new_tree entfile)"
cat > "$entfile/Menubar.entitlements" <<'EOF'
<?xml version="1.0" encoding="UTF-8"?>
<plist version="1.0"><dict>
  <key>com.apple.security.network.client</key><true/>
</dict></plist>
EOF
check "network entitlement in *.entitlements is RED" 1 "$(run "$entfile")"

# Case 11: the app naming a store artifact (a direct store-read) -> RED. This is the
# #328 gap #356 closes: the app must get the usage series OVER THE SOCKET, never by
# reading the daemon's usage-samples / usage-rollup store files itself.
storeread="$(new_tree storeread)"
printf 'let s = try String(contentsOfFile: dir + "usage-samples.jsonl")\n' >> "$storeread/Sources/WatchTransport.swift"
check "store-path read (names usage-samples) is RED" 1 "$(run "$storeread")"

# Case 12: a comment naming a store artifact is NOT a violation -> GREEN (the same
# code-only discipline as the network-symbol comment-safety case above — the awk strips
# // comments before matching, so documenting the invariant by naming the file is safe).
storecomment="$(new_tree storecomment)"
printf '// the series comes over the socket; the app never reads usage-samples/usage-rollup\n' >> "$storecomment/Sources/WatchTransport.swift"
check "comment naming a store artifact is GREEN" 0 "$(run "$storecomment")"

# Case 13: missing Sources dir -> RED (fail-closed: never silently pass on a
# degenerate/empty subject).
empty="$work/empty"
mkdir -p "$empty"
check "missing Sources dir is RED (fail-closed)" 1 "$(run "$empty")"

printf '\n%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
