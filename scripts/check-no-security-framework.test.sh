#!/usr/bin/env bash
# Self-contained falsifier + regression test for check-no-security-framework.sh
# (issue #1102, second guard). Proves the keychain-boundary gate goes RED when
# `security-framework` is in the dependency graph — the refactor it exists to
# stop (issue #2) — and GREEN on the graph as it stands.
#
# Peer of check-ci-ok-results.test.sh and check-gate-change-ack.test.sh, whose
# shape this follows deliberately so the guards read as one family. Added
# alongside check-ci-ok-needs.test.sh: those two were the repo's only `check-*.sh`
# gates with no `.test.sh` companion.
#
# HOW THE FALSIFIER IS BUILT. The guard takes no arguments and reads the real
# graph via `cargo metadata`, so a corpse cannot be passed in. Adding a real
# dependency on `security-framework` to build one would need the network and a
# lockfile resolve, and would leave this test unable to run offline. Instead a
# stub `cargo` on PATH emits a canned metadata document. That exercises the part
# of the guard that can actually rot — the jq predicate over `.packages` — while
# case 1 runs the guard for real, so the stub never becomes the only subject.
#
# `jq` and `cargo` are dependencies of the guard, so they are dependencies of
# this test. When either is absent this test EXITS NON-ZERO (2) with a loud
# banner rather than reporting green: a test that passes because its dependency
# was missing is the same defect class as #1079. Exit 2 is "did not run",
# distinct from exit 1 "assertions failed".
#
# Run locally:  ./scripts/check-no-security-framework.test.sh
set -euo pipefail

here="$(cd "$(dirname "$0")" && pwd)"
guard="$here/check-no-security-framework.sh"
repo_root="$(cd "$here/.." && pwd)"

missing_dep=""
command -v jq    >/dev/null 2>&1 || missing_dep="jq"
command -v cargo >/dev/null 2>&1 || missing_dep="${missing_dep:+$missing_dep and }cargo"
if [ -n "$missing_dep" ]; then
    echo "=======================================================================" >&2
    echo "SKIPPED (NOT A PASS): $missing_dep not installed, so"                     >&2
    echo "check-no-security-framework.sh cannot be exercised and nothing here"      >&2
    echo "was verified. Exiting 2 on purpose: a green here would mean the guard"    >&2
    echo "is untested, not sound."                                                  >&2
    echo "=======================================================================" >&2
    exit 2
fi

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
check_says() { # <label> <needle> <text>
    if printf '%s' "$3" | grep -qF -- "$2"; then
        printf 'PASS  %s (output names %s)\n' "$1" "$2"
        pass=$((pass + 1))
    else
        printf 'FAIL  %s (output does not name %s)\n' "$1" "$2"
        printf '      got: %s\n' "$3"
        fail=$((fail + 1))
    fi
}
# A RESIDUAL is a case the guard does NOT cover, measured rather than asserted to
# be correct. It is pinned here so the hole cannot silently widen, and so that
# closing it is a deliberate edit to this file rather than a surprise red. A
# residual that moves is reported as a failure precisely because it means the
# guard's coverage changed — re-measure and update, do not just flip the number.
residual() { # <label> <measured-exit> <actual-exit>
    if [ "$2" = "$3" ]; then
        printf 'RESIDUAL  %s (exit %s — NOT covered by the guard)\n' "$1" "$3"
        pass=$((pass + 1))
    else
        printf 'FAIL  %s: residual moved (was exit %s, now %s) — the guard'"'"'s coverage changed; re-measure\n' "$1" "$2" "$3"
        fail=$((fail + 1))
    fi
}

# A stub `cargo` that prints the fixture named by $FIXTURE, shadowing the real one.
stub_dir="$work/stub"
mkdir -p "$stub_dir"
cat > "$stub_dir/cargo" <<'STUB'
#!/usr/bin/env bash
# Stands in for `cargo metadata --format-version 1 --locked`.
cat "$FIXTURE"
STUB
chmod +x "$stub_dir/cargo"

# Run the guard over a canned metadata document, capturing its exit code.
run_meta() { # <json>
    local rc f="$work/meta.json"
    printf '%s' "$1" > "$f"
    set +e
    FIXTURE="$f" PATH="$stub_dir:$PATH" "$guard" >/dev/null 2>&1
    rc=$?
    set -e
    echo "$rc"
}
say_meta() { # <json>
    local f="$work/meta.json"
    printf '%s' "$1" > "$f"
    set +e
    FIXTURE="$f" PATH="$stub_dir:$PATH" "$guard" 2>&1
    set -e
}

# ---------------------------------------------------------------------------
# Case 1: the real graph, real cargo -> GREEN. The gate must not be red on
# arrival, and this is the one case where nothing is stubbed.
# ---------------------------------------------------------------------------
rc_real=$(
    set +e
    cd "$repo_root" && "$guard" >/dev/null 2>&1
    echo $?
)
check "the committed dependency graph is GREEN" 0 "$rc_real"

# Case 2: that green must not be VACUOUS. The guard reports "ok" whenever its jq
# query yields nothing — including when the query yields nothing because it
# stopped matching the document at all. Probe the real metadata independently and
# require a non-empty package set, so a green over an empty or reshaped graph is
# distinguishable from a green over a real one.
pkg_count=$(cd "$repo_root" && cargo metadata --format-version 1 --locked 2>/dev/null | jq '.packages | length')
if [ "${pkg_count:-0}" -ge 1 ]; then
    printf 'PASS  the real graph the green was measured over holds %s packages, not zero\n' "$pkg_count"
    pass=$((pass + 1))
else
    printf 'FAIL  cargo metadata yielded %s packages — the guard'"'"'s green was over nothing\n' "${pkg_count:-<none>}"
    fail=$((fail + 1))
fi

# ---------------------------------------------------------------------------
# Case 3: THE FALSIFIER. `security-framework` in the graph is the Security.framework
# SDK write path (issue #2): writing the credential as our own code identity
# re-stamps the keychain item's ACL partition list and evicts `apple-tool:`,
# breaking Claude Code's silent read. The guard must go RED and say which crate.
# ---------------------------------------------------------------------------
hit='{"packages":[{"name":"serde"},{"name":"security-framework"},{"name":"tokio"}]}'
check "security-framework in the graph is RED" 1 "$(run_meta "$hit")"
# The needle must be unique to the red path: the guard's SUCCESS line is
# "ok: no 'security-framework' ... linkage", so grepping the bare crate name
# matches a GREEN run too and the assertion proves nothing.
check_says "and the error names the crate" "'security-framework' is in the dependency graph" "$(say_meta "$hit")"
check_says "and points at the CLI rule" "/usr/bin/security" "$(say_meta "$hit")"

# Case 4: the same document with only that entry removed -> GREEN. Without this,
# case 3's red could be coming from the stub rather than from the crate, and the
# pair would prove nothing about the predicate.
check "the same graph without it is GREEN" 0 \
    "$(run_meta '{"packages":[{"name":"serde"},{"name":"tokio"}]}')"

# Case 5: a `cargo` that fails must not be read as "no linkage found". `set -e`
# on the command substitution is what carries this; it is asserted here so a
# future refactor of that line cannot quietly turn a broken cargo into a pass.
cat > "$stub_dir/cargo" <<'STUB'
#!/usr/bin/env bash
echo "cargo: simulated failure" >&2
exit 101
STUB
chmod +x "$stub_dir/cargo"
rc_cargo_fail=$(
    set +e
    PATH="$stub_dir:$PATH" "$guard" >/dev/null 2>&1
    echo $?
)
if [ "$rc_cargo_fail" -ne 0 ]; then
    printf 'PASS  a failing cargo is RED, not a silent pass (exit %s)\n' "$rc_cargo_fail"
    pass=$((pass + 1))
else
    printf 'FAIL  a failing cargo passed (exit 0) — the guard green-lit a graph it never read\n'
    fail=$((fail + 1))
fi
# Restore the fixture-printing stub for the residual cases below.
cat > "$stub_dir/cargo" <<'STUB'
#!/usr/bin/env bash
cat "$FIXTURE"
STUB
chmod +x "$stub_dir/cargo"

# ---------------------------------------------------------------------------
# Residuals — measured limits of the guard, in each direction it does not reach.
# None of these is asserted to be correct behaviour; each is recorded so the gap
# is legible instead of assumed away. All three are worth their own issue.
# ---------------------------------------------------------------------------

# Residual A: the match is the EXACT name. `security-framework-sys` is the raw
# FFI crate the wrapper sits on and links Security.framework directly, so a
# dependency on it alone is the same linkage by a different name — and passes.
residual "security-framework-sys alone (exact-name match only)" 0 \
    "$(run_meta '{"packages":[{"name":"serde"},{"name":"security-framework-sys"}]}')"

# Residual B: an empty package set passes. Unreachable through real cargo on this
# crate (case 2 measures 70+ packages), but it is the shape every silent-rot pass
# takes, so it is pinned rather than left to inference.
residual "an empty package set (nothing to find, so nothing found)" 0 \
    "$(run_meta '{"packages":[]}')"

# Residual C: the sharpest one, and structurally issue #1079. If the document no
# longer has `.packages` — a cargo metadata schema change, not a hypothetical —
# jq errors, the `if` reads that as "no match", and the guard prints ok and exits
# 0. The query failing and the property holding are indistinguishable to it.
residual "metadata with no .packages key (the query fails OPEN)" 0 \
    "$(run_meta '{"workspace_members":["sessiometer"]}')"

printf '\n%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
