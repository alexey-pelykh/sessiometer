#!/usr/bin/env bash
# Self-contained falsifier + regression test for check-cc-version.sh (issue #712).
# Builds a throwaway tree the guard resolves relative to its own location and
# exercises the README drift guard across the cases that define its contract — in
# particular proving the guard goes RED on a README that states a STALE range while
# mentioning the new bound elsewhere (the two-independent-substring-grep gap #712
# closes), and GREEN on a correctly-stated adjacent range. Run locally:
#   ./scripts/check-cc-version.test.sh
#
# check-cc-version.sh is a pre-release gate (build/release-checklist.md), not a CI
# job — CI never execs a real `claude`. This peer is the same: a local falsifier,
# runnable by hand, like scripts/check-formula.test.sh and check-gate-change-ack.test.sh.
set -euo pipefail

here="$(cd "$(dirname "$0")" && pwd)"
guard="$here/check-cc-version.sh"

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

# The guard resolves its ledger and README relative to its OWN location
# ($script_dir/../build/version-compat.md, $script_dir/../README.md), so stand up a
# fake tree beside a copy of the guard under test:
#   $work/scripts/check-cc-version.sh   (a copy of the real guard — always current)
#   $work/build/version-compat.md       (a hermetic fixture ledger, NOT the repo's)
#   $work/README.md                     (rewritten per case)
#   $work/claude                        (a fake `claude` printing a chosen version)
mkdir -p "$work/scripts" "$work/build"
cp "$guard" "$work/scripts/check-cc-version.sh"
chmod +x "$work/scripts/check-cc-version.sh"
sut="$work/scripts/check-cc-version.sh"

# Fixture ledger with a known range, independent of the repo's real range so a future
# range bump never touches this test.
write_ledger() { # $1=MIN $2=MAX
    local min="$1" max="$2"
    cat > "$work/build/version-compat.md" <<EOF
# fixture ledger (hermetic — not the repo's real range)
- CC_SUPPORTED_MIN: $min
- CC_SUPPORTED_MAX: $max
EOF
}

# A fake `claude` printing a chosen version in the real `x.y.z (Claude Code)` shape,
# so the version arm passes (or fails, by choice) and the README arm's verdict is what
# reaches the exit status. The guard resolves it via $CLAUDE_BIN.
fake_claude() { # $1=version
    local ver="$1"
    cat > "$work/claude" <<EOF
#!/usr/bin/env bash
echo "$ver (Claude Code)"
EOF
    chmod +x "$work/claude"
}

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

# Run the copied guard with the fake claude injected, capturing its exit code without
# tripping set -e. cwd is irrelevant (the guard anchors on BASH_SOURCE, not $PWD).
run() {
    local rc
    set +e
    CLAUDE_BIN="$work/claude" "$sut" >/dev/null 2>&1
    rc=$?
    set -e
    echo "$rc"
}

# Case 1: correct README states the range adjacently (real format, en-dash) with an
# in-range claude -> GREEN. Guards against the tightened guard false-FAILing on the
# actual README format.
write_ledger 2.1.181 2.1.217
printf -- '- supported **`2.1.181`–`2.1.217`** on macOS `26.5.1`\n' > "$work/README.md"
fake_claude 2.1.190
check "correct adjacent range passes" 0 "$(run)"

# Case 2: THE falsifier (#712). README states a STALE range (`2.1.181`–`2.1.197`) and
# mentions the new MAX (2.1.217) in an unrelated sentence. Both bounds appear as
# substrings, so the OLD two-independent-greps check passed — but the stated range is
# wrong. The adjacency check must go RED.
write_ledger 2.1.181 2.1.217
{
    printf -- '- supported **`2.1.181`–`2.1.197`** on macOS\n'
    printf -- '- Note: `2.1.217` is not yet verified.\n'
} > "$work/README.md"
fake_claude 2.1.190
check "stale range + stray new-version mention is REJECTED" 1 "$(run)"

# Case 3: README omits the MAX entirely (states only the MIN) -> RED. The common case
# the original guard already caught; proves the tightening did not lose it.
write_ledger 2.1.181 2.1.217
printf -- '- supported only `2.1.181` on macOS\n' > "$work/README.md"
fake_claude 2.1.190
check "README missing MAX is REJECTED (common-case catch preserved)" 1 "$(run)"

# Case 4: no README at all -> the guard flags drift only when the file exists, so an
# absent README with an in-range claude passes (behavior preserved from the original).
write_ledger 2.1.181 2.1.217
rm -f "$work/README.md"
fake_claude 2.1.190
check "absent README does not flag drift (in-range claude)" 0 "$(run)"

# Case 5: the version arm is independent of the README arm. An OUT-of-range claude
# fails even with a perfectly correct README -> RED. Proves the #712 edit to the README
# arm did not disturb the version comparison.
write_ledger 2.1.181 2.1.217
printf -- '- supported **`2.1.181`–`2.1.217`** on macOS\n' > "$work/README.md"
fake_claude 2.2.0
check "out-of-range claude fails even with a correct README" 1 "$(run)"

# Case 6: degenerate single-version range (MIN == MAX). The adjacency check would
# demand the version stated twice; the min==max fallback instead asserts the single
# version is present, so a normal single-version README passes -> GREEN.
write_ledger 2.1.181 2.1.181
printf -- '- supported **`2.1.181`** on macOS\n' > "$work/README.md"
fake_claude 2.1.181
check "degenerate min==max range accepts a single-version README" 0 "$(run)"

printf '\n%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
