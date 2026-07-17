#!/usr/bin/env bash
# Self-contained falsifier + regression test for check-formula.sh (issue #567).
# Exercises the guard across the cases that define its contract — in particular
# proving the gate goes RED on the exact known-bad formula that #566 fixed
# (`depends_on :macos` ordered before the `rust` build dep), and GREEN on the
# canonical formula as it stands. A gate that cannot fail the known-bad artifact
# is not a gate; this is where that claim is demonstrated rather than assumed.
# Run locally:  ./scripts/check-formula.test.sh
set -euo pipefail

here="$(cd "$(dirname "$0")" && pwd)"
guard="$here/check-formula.sh"
canonical="$here/../Formula/sessiometer.rb"

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

# Run the guard against a formula, capturing its exit code without tripping set -e.
run() { # <formula-path>
    local rc
    set +e
    "$guard" "$1" >/dev/null 2>&1
    rc=$?
    set -e
    echo "$rc"
}

# Case 1: the canonical formula, as committed -> GREEN. The gate must not be red
# on arrival; #566's fix is already in the tree.
check "canonical formula is GREEN" 0 "$(run "$canonical")"

# Case 2: THE FALSIFIER. #566's fix reverted — `depends_on :macos` placed before
# the `rust` build dep — is the precise defect that reached the published tap
# while no CI checked the canonical formula. The guard must go RED.
good='  depends_on "rust" => :build
  depends_on :macos'
bad='  depends_on :macos
  depends_on "rust" => :build'
corpse="$work/corpse.rb"
python3 - "$canonical" "$corpse" "$good" "$bad" <<'PY'
import sys
src_path, out_path, good, bad = sys.argv[1:5]
src = open(src_path).read()
if good not in src:
    sys.exit("fixture drift: the canonical formula no longer contains the "
             "expected depends_on block; update check-formula.test.sh")
open(out_path, "w").write(src.replace(good, bad, 1))
PY
check "#566 corpse (depends_on order reverted) is RED" 1 "$(run "$corpse")"

# Case 3: a second, independent defect class — a bogus stanza the audit rejects —
# so the gate is not merely a one-trick DependencyOrder check.
bogus="$work/bogus.rb"
sed 's|^  homepage .*$|  homepage "not-a-url"|' "$canonical" > "$bogus"
check "malformed homepage is RED" 1 "$(run "$bogus")"

# Case 4: a missing formula path is an error, not a silent pass. A guard that
# green-lights a subject it never read is worse than no guard.
check "missing formula path is RED" 1 "$(run "$work/does-not-exist.rb")"

printf '\n%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
