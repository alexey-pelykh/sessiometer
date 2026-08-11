#!/usr/bin/env bash
# Self-contained falsifier + regression test for check-no-security-framework.sh
# (issue #1102, second guard). Proves the keychain-boundary gate goes RED when a
# Security.framework binding is in the dependency graph — the refactor it exists
# to stop (issue #2) — and GREEN on the graph as it stands.
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
# WHAT ISSUE #1233 CHANGED HERE. Three residuals this file measured — the exact
# name match missing `security-framework-sys`, an empty package set passing, and
# the query failing OPEN — were closed in the guard. Each is now an ASSERTION
# below (cases 6 and 10-11), not a deleted line: a residual that silently
# disappears is exactly what the residual block exists to prevent, so the block
# still stands and now carries the ONE hole that remains.
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

# Case 2: that green must not be VACUOUS. Probe the real metadata independently
# and require a non-empty package set, so a green over an empty or reshaped graph
# is distinguishable from a green over a real one. Since #1233 the guard refuses
# an empty set itself (case 11), but this probe is INDEPENDENT of the guard — it
# would still catch a guard whose evaluability check was neutered.
pkg_count=$(cd "$repo_root" && cargo metadata --format-version 1 --locked 2>/dev/null | jq '.packages | length')
if [ "${pkg_count:-0}" -ge 1 ]; then
    printf 'PASS  the real graph the green was measured over holds %s packages, not zero\n' "$pkg_count"
    pass=$((pass + 1))
else
    printf 'FAIL  cargo metadata yielded %s packages — the guard'"'"'s green was over nothing\n' "${pkg_count:-<none>}"
    fail=$((fail + 1))
fi

# Case 2b: and the guard SAYS what it measured. A green that reports its own
# cardinality cannot be mistaken for a green over nothing by someone reading CI
# logs rather than this file.
check_says "the GREEN line reports the package count it checked" "packages checked: $pkg_count" \
    "$(cd "$repo_root" && "$guard" 2>&1)"

# ---------------------------------------------------------------------------
# Case 3: THE FALSIFIER. `security-framework` in the graph is the Security.framework
# SDK write path (issue #2): writing the credential as our own code identity
# re-stamps the keychain item's ACL partition list and evicts `apple-tool:`,
# breaking Claude Code's silent read. The guard must go RED and say which crate.
# ---------------------------------------------------------------------------
hit='{"packages":[{"name":"serde"},{"name":"security-framework"},{"name":"tokio"}]}'
check "security-framework in the graph is RED" 1 "$(run_meta "$hit")"
# The needle must be unique to the RED path. The guard's success line is
# "ok: no Security.framework SDK linkage (packages checked: N)", so a needle that
# also matches it proves nothing; "is in the dependency graph:" appears only on
# the error path, and carrying the crate name after it pins WHICH crate was named
# rather than merely that some error was printed.
check_says "and the error names the crate" "is in the dependency graph: security-framework" "$(say_meta "$hit")"
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
# Restore the fixture-printing stub for the cases below.
cat > "$stub_dir/cargo" <<'STUB'
#!/usr/bin/env bash
cat "$FIXTURE"
STUB
chmod +x "$stub_dir/cargo"

# ---------------------------------------------------------------------------
# Case 6 (issue #1233, AC-1 — was RESIDUAL A). The raw FFI crate ALONE. This is
# the case that used to pass at exit 0: the match was the exact name
# `security-framework`, and a graph carrying only `security-framework-sys` — the
# crate that actually holds `link(name = "Security", kind = "framework")` — sailed
# through. It is a direct Security.framework binding, so passing it was a
# shortfall against issue #2's own wording ("or any direct Security.framework
# binding"), not a missing nicety.
# ---------------------------------------------------------------------------
sys_only='{"packages":[{"name":"serde"},{"name":"security-framework-sys"}]}'
check "security-framework-sys ALONE is RED" 1 "$(run_meta "$sys_only")"
# Naming the crate is the load-bearing half: an error that says only "a binding is
# present" would pass a bare exit-code assertion while telling a reader nothing,
# and the -sys crate is precisely the one a reader would not expect.
check_says "and the error names -sys specifically" "is in the dependency graph: security-framework-sys" \
    "$(say_meta "$sys_only")"

# Case 7: the same shortfall one fork over. `apple-security-framework{,-sys}` is a
# real published pair (x52dev) whose manifests declare "Apple `Security.framework`
# bindings" / "low-level FFI bindings", and whose -sys crate carries the same
# link(name = "Security", kind = "framework") attribute. Same linkage, different
# name — so the guard covers all four, and both halves are pinned here.
check "apple-security-framework-sys is RED" 1 \
    "$(run_meta '{"packages":[{"name":"apple-security-framework-sys"}]}')"
check "apple-security-framework is RED" 1 \
    "$(run_meta '{"packages":[{"name":"apple-security-framework"}]}')"

# Case 8: THE BOUNDARY (issue #1233, AC-1: "must not red on an unrelated crate
# that merely shares a prefix"). The rule is whole-name, so a name that merely
# CONTAINS the token stays GREEN. The first two are not invented: they are real
# crates.io packages, TLS API adapters rather than bindings, and a `contains()`
# match would redden on both. The last three probe each way a name can sit around
# the token — trailing character, suffix, prefix.
#
# These five are individually weak (a neutered guard that always exits 0 would
# pass all of them). What makes them mean something is that they run alongside
# cases 3, 6 and 7, which a neutered guard fails: the suite as a whole cannot be
# satisfied by a matcher that is only permissive, nor by one that is only strict.
for near in tls-api-security-framework tls-api-security-framework-2 \
            security-frameworks security-framework-mock my-security-framework; do
    # Spliced with single quotes rather than escaped double quotes. `\"` inside a
    # nested command substitution is unescaped BEFORE the inner command is parsed,
    # which re-pairs the quotes and leaves the `{...},{...}` exposed to BRACE
    # EXPANSION — run_meta then gets two fragments instead of one document, so the
    # guard is handed malformed JSON. The first draft of this loop did exactly that.
    fixture='{"packages":[{"name":"serde"},{"name":"'"$near"'"}]}'
    check "boundary: '$near' is GREEN (whole-name match, not a substring)" 0 \
        "$(run_meta "$fixture")"
done

# Case 9: both family members present -> the error names BOTH. Proves the guard
# collects its hits rather than reporting the first one and stopping, so a graph
# carrying the wrapper does not hide the -sys crate underneath it.
check_says "an error over two hits names both" \
    "is in the dependency graph: security-framework, security-framework-sys" \
    "$(say_meta '{"packages":[{"name":"security-framework-sys"},{"name":"security-framework"}]}')"

# ---------------------------------------------------------------------------
# Case 10 (issue #1233, AC-2 — was RESIDUAL C). The sharpest of the three, and
# structurally issue #1079. The old guard asked jq a yes/no question inside an
# `if`, which read a jq that ERRORED exactly as it read a jq that found nothing:
# a document with no `.packages` made jq exit 5, the `if` took that as "no match",
# and the guard printed `ok:` and exited 0. The query failing and the property
# holding were indistinguishable to it — a fail-open on a CI-blocking security
# gate. Exit 2 is now "could not evaluate", distinct from 1 "a binding is here"
# and from 0 "checked, clean".
# ---------------------------------------------------------------------------
no_packages='{"workspace_members":["sessiometer"]}'
check "metadata with no .packages key is RED (exit 2, could not evaluate)" 2 "$(run_meta "$no_packages")"
# The distinction is only real if the operator can SEE it. A non-zero exit with a
# message indistinguishable from "a binding is present" would send a reader
# hunting for a dependency that is not there.
check_says "and says the check did not run, not that a binding was found" "linkage check did NOT run" \
    "$(say_meta "$no_packages")"

# The same fail-closed path for every other way the document can stop being one
# the query can read. Grouped rather than argued case by case: all four are the
# same defect — the guard cannot evaluate itself — and all four used to be
# indistinguishable from a clean graph.
check "malformed JSON is RED (exit 2)"                 2 "$(run_meta 'not json at all')"
check "a top-level array is RED (exit 2)"              2 "$(run_meta '[]')"
check "a non-array .packages is RED (exit 2)"          2 "$(run_meta '{"packages":"nope"}')"

# Case 11 (issue #1233, AC-3 — was RESIDUAL B). An empty package set used to pass
# at exit 0. It is closed here BY the evaluability check above rather than by a
# rule of its own: a real graph always contains at least this crate, so zero
# packages is a document the guard cannot judge, not a graph that happens to be
# clean. Unreachable through real cargo on this crate (case 2 measures 70), but it
# is the shape every silent-rot pass takes.
check "an empty package set is RED (exit 2, not a vacuous pass)" 2 "$(run_meta '{"packages":[]}')"

# ---------------------------------------------------------------------------
# Residuals — measured limits of the guard, in each direction it does not reach.
# Not asserted to be correct behaviour; recorded so the gap is legible instead of
# assumed away.
#
# Residuals A, B and C — which this block carried from PR #1227 until issue #1233
# — are CLOSED, and are now cases 6, 11 and 10 above respectively. They are named
# here rather than deleted so that a reader diffing this file can tell a closed
# residual from one that quietly went missing.
#
# One hole remains, and it is the direct consequence of matching by name.
# ---------------------------------------------------------------------------

# Residual D: a direct Security.framework binding published under a name outside
# the four the guard knows still passes. The obvious generalisation does not close
# it, and that was measured rather than assumed: `cargo metadata` exposes a
# `.links` key, but none of the four binding crates declares `links` in its
# manifest — each binds via a source-level `#[link]` attribute — so a
# `.links == "Security"` matcher would match none of them and be inert. The
# fixture below carries `"links":"Security"` precisely to pin that: the guard does
# not look at it, and a matcher that did would still not have caught the four
# crates it is supposed to catch.
residual "a binding under an out-of-family name (the match is by name)" 0 \
    "$(run_meta '{"packages":[{"name":"serde"},{"name":"renamed-sec-bindings","links":"Security"}]}')"

printf '\n%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
