#!/usr/bin/env bash
# Self-contained falsifier + regression test for check-test-runner-env-form.sh
# (issue #1362). Builds a throwaway git repo shaped like this one — a menubar test
# source that reads its switches, plus documents that name them — and exercises the
# cases that define the guard's contract.
#
# The falsifiers each fail against a specific wrong implementation:
#
#   T2  kills a guard that checks only that prefixed names are REAL (rule B) and never
#       that a setting spelling carries the prefix — the defect issue #1362 found, where
#       committed surfaces named a form that reaches `xcodebuild` and not the test
#   T4  kills a guard that checks only the FORM (rule A). A switch renamed in Swift
#       leaves every document naming the old one; rule A derives from the NEW name, so
#       it finds those documents perfectly compliant
#   T5  kills a guard that reports green having derived no switches — an empty rule
#       holds every surface to nothing and passes
#   T8  kills a guard scoped to prose. Some of the original sites were the suite's own
#       SKIP MESSAGES, so a reader who followed the instruction the skip printed landed
#       back on the same skip
#
# T3, T6, T7, T9 and T14 are the GREEN half, and each pins a correct file that an
# over-broad rule would reject:
#
#   T3  the exemption — a document must be able to QUOTE the broken form to warn about
#       it, which design/README.md § Panel golden drift gate does. T12 is its other half:
#       the exemption is DIRECTIONAL, and undirected it would swallow the very sites this
#       gate exists for
#   T6  an ambient variable a test saves and restores (`XDG_CONFIG_HOME`) is not a
#       switch, and setting it in front of the daemon binary — which is not an
#       `xcodebuild` command line — is a correct line a guard deriving every environment
#       key would forbid
#   T7  the un-prefixed name written as a NAME rather than as a command is the correct
#       in-process spelling — it is what the source itself reads
#   T9  a switch read by a DIFFERENT test file is derived too, so one suite's document
#       cannot be red merely because another suite owns the switch
#   T18 a switch read by the APP's own Sources is NOT derived, so its bare command form
#       stays legal. `xcodebuild` forwards into the test bundle, not into the app the
#       operator launches by hand — `SESSIOMETER_GLYPH_GALLERY=1 "$BIN"` in
#       design/README.md is a correct line. This is what pins the TEST-tree scoping;
#       T9 pins only that the whole bundle is covered, never that Sources is out
#
# T10 is not a falsifier but a stated BOUNDARY: the guard reads what git tracks, so an
# untracked file is not scanned. That is deliberate — it governs committed surfaces, the
# same scope check-doc-citations.sh takes — and pinning it means narrowing or widening it
# is a measured act. It is also how T11's defect hid: the guard was green while untracked
# and red the moment it was committed, because it had not been scanning itself.
#
#   T11 kills a rule B written with `*` where it needs `+` — one that reads a mention of
#        the prefixed NAMESPACE, with no switch name after it, as a switch nothing reads.
#        A document explaining this convention writes exactly that, and the guard's own
#        rule A comment does
#
#   T12 kills an UNDIRECTED exemption. Every repaired skip message in the real suite
#        states the correct form and then explains the bare one on the same line, so a
#        guard that exempts on the word alone passes those messages de-prefixed — the
#        exact sites issue #1362 is about, silently
#   T13 kills a rule A that keys only on `NAME=`. `.github/workflows/ci.yml` arms the
#        drift gate through a YAML `env:` mapping, which carries no `=` at all; de-prefixed
#        there, `panel-goldens` skips both comparisons on every run and still reports pass
#   T14 pins T13's green half: the same mapping, prefixed, is correct and must not red
#   T15 kills an exemption matched against `path:lineno:content` rather than the line. A
#        tracked path carrying the word would otherwise exempt every hit in that file
#   T16 kills a guard that treats "not a git work tree" as a pass — nothing scanned is a
#        failure, not a green
#   T17 kills the `cd "$(git rev-parse …)" || …` spelling of that branch, which is
#        UNREACHABLE: the exit status is the `cd`'s, and bash's `cd ""` succeeds, so the
#        run falls through to the DERIVATION error and tells the reader to re-point a
#        script that is merely in the wrong directory. Measured: T16 alone does not kill
#        that spelling, because the derivation error exits 2 as well — the code is
#        fail-safe and only the MESSAGE is wrong, which is exactly why the message is
#        pinned separately
#
# MUTATION-VALIDATED against the real tree as well as here: reverting CONTRIBUTING.md's
# repaired sentence to the un-prefixed form reddens rule A on that exact line; de-prefixing
# the `env:` mapping that arms the drift gate in `.github/workflows/ci.yml` reddens rule A
# there; de-prefixing one repaired skip message reddens rule A on it; and renaming
# `SESSIOMETER_PANEL_MEASURE` in the Swift source alone reddens rule B on every surface
# still naming it, enumerating each. All restored byte-identical from a pristine copy after.
#
# The fixture switches are named independently of the repo's real ones, so a future
# rename of a real switch never touches this test.
#
# Run locally:  ./scripts/check-test-runner-env-form.test.sh
set -euo pipefail

here="$(cd "$(dirname "$0")" && pwd)"
guard="$here/check-test-runner-env-form.sh"

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
cd "$work"

git init -q
git config user.email test@test.invalid
git config user.name "env form test"
git config commit.gpgsign false

mkdir -p apps/menubar/Tests scripts docs
cp "$guard" scripts/check-test-runner-env-form.sh
chmod +x scripts/check-test-runner-env-form.sh
sut="$work/scripts/check-test-runner-env-form.sh"

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

run() {
    local rc
    set +e
    "$sut" >/dev/null 2>&1
    rc=$?
    set -e
    echo "$rc"
}

# The fixture's test source: two switches an operator sets, in this repo's namespace.
write_suite() {
    cat > apps/menubar/Tests/FixtureTests.swift <<'SWIFT'
private var isMeasuring: Bool {
    ProcessInfo.processInfo.environment["SESSIOMETER_FIXTURE_MEASURE"] == "1"
}
private var isGated: Bool {
    ProcessInfo.processInfo.environment["SESSIOMETER_FIXTURE_GATE"] == "1"
}
SWIFT
    git add apps/menubar/Tests/FixtureTests.swift
}

# Rewrite the one document each case turns on. Tracked, because the guard reads what
# git tracks (T10 pins that).
write_doc() { # doc body on stdin
    cat > docs/guide.md
    git add docs/guide.md
}

reset_tree() {
    rm -rf apps/menubar/Tests/*.swift docs .github
    git rm -q -r --cached --ignore-unmatch apps/menubar/Tests/FixtureTests.swift docs .github >/dev/null
    mkdir -p docs
    write_suite
}

reset_tree
printf 'Re-derive with `TEST_RUNNER_SESSIOMETER_FIXTURE_MEASURE=1 xcodebuild test`.\n' | write_doc
check "T1  the prefixed command spelling passes" 0 "$(run)"

# T2: the defect itself.
reset_tree
printf 'Re-derive with `SESSIOMETER_FIXTURE_MEASURE=1 xcodebuild test`.\n' | write_doc
check "T2  an un-prefixed command spelling is RED (rule A)" 1 "$(run)"

# T3: the exemption. A document explaining the trap has to state the broken form, and the
# word comes FIRST — which is what makes it a warning rather than an instruction (T12).
reset_tree
printf 'A bare `SESSIOMETER_FIXTURE_MEASURE=1` reaches xcodebuild and not the test.\n' | write_doc
check "T3  the same spelling on a line that calls it bare FIRST passes (the warning form)" 0 "$(run)"

# T4: the rename. The document is correct in FORM and names a switch nothing reads.
reset_tree
printf 'Re-derive with `TEST_RUNNER_SESSIOMETER_FIXTURE_CALIBRATE=1 xcodebuild test`.\n' | write_doc
check "T4  a prefixed name no test reads is RED (rule B — the rename case)" 1 "$(run)"

# T5: nothing derivable. A guard that passes here holds every surface to an empty rule.
rm -f apps/menubar/Tests/*.swift docs/*.md
git rm -q --cached --ignore-unmatch apps/menubar/Tests/FixtureTests.swift docs/guide.md >/dev/null
printf 'func testNothing() {}\n' > apps/menubar/Tests/FixtureTests.swift
git add apps/menubar/Tests/FixtureTests.swift
printf 'Re-derive with `SESSIOMETER_FIXTURE_MEASURE=1 xcodebuild test`.\n' | write_doc
check "T5  deriving NO switches is exit 2, not a pass" 2 "$(run)"

# T6: an ambient variable read to save and restore it is not a switch, and the bare
# spelling in front of a non-xcodebuild binary is correct.
reset_tree
cat >> apps/menubar/Tests/FixtureTests.swift <<'SWIFT'
func testIgnoresXdg() {
    let previous = ProcessInfo.processInfo.environment["XDG_CONFIG_HOME"]
    _ = previous
}
SWIFT
git add apps/menubar/Tests/FixtureTests.swift
printf 'Run `XDG_CONFIG_HOME=/tmp/x sessiometer config path` against the daemon binary.\n' | write_doc
check "T6  an ambient env read is not derived, so its bare command form passes" 0 "$(run)"

# T7: the name without a value is the in-process spelling — what the source reads.
reset_tree
printf 'The suite gates on `SESSIOMETER_FIXTURE_GATE`, off by default.\n' | write_doc
check "T7  the un-prefixed NAME (no value) is the correct in-process spelling" 0 "$(run)"

# T8: the shape the original defect took — inside the suite's own skip message.
reset_tree
cat >> apps/menubar/Tests/FixtureTests.swift <<'SWIFT'
func testMeasure() throws {
    try XCTSkipUnless(isMeasuring, "calibration run only: SESSIOMETER_FIXTURE_MEASURE=1")
}
SWIFT
git add apps/menubar/Tests/FixtureTests.swift
printf 'See the suite.\n' | write_doc
check "T8  an un-prefixed spelling in a SWIFT skip message is RED (not prose-only)" 1 "$(run)"

# T9: a switch owned by another test file is derived too.
reset_tree
cat > apps/menubar/Tests/OtherTests.swift <<'SWIFT'
private var isSwapMeasuring: Bool {
    ProcessInfo.processInfo.environment["SESSIOMETER_FIXTURE_SWAP"] == "1"
}
SWIFT
git add apps/menubar/Tests/OtherTests.swift
printf 'Re-derive with `TEST_RUNNER_SESSIOMETER_FIXTURE_SWAP=1 xcodebuild test`.\n' | write_doc
check "T9  a switch read by another test file is derived (one guard, whole bundle)" 0 "$(run)"
rm -f apps/menubar/Tests/OtherTests.swift
git rm -q --cached --ignore-unmatch apps/menubar/Tests/OtherTests.swift >/dev/null

# T10: stated boundary — the guard governs COMMITTED surfaces.
reset_tree
printf 'See the suite.\n' | write_doc
printf 'Re-derive with `SESSIOMETER_FIXTURE_MEASURE=1 xcodebuild test`.\n' > docs/untracked.md
check "T10 an UNTRACKED file is not scanned (stated boundary)" 0 "$(run)"
rm -f docs/untracked.md

# T11: the namespace, not a switch. A mention with nothing after the prefix names no
# switch, so it cannot be a stale one.
reset_tree
printf 'Set TEST_RUNNER_SESSIOMETER_<NAME> on the command line, never the un-prefixed form.\n' | write_doc
check "T11 a mention of the prefixed NAMESPACE alone is not an unknown switch" 0 "$(run)"

# T12: the exemption is DIRECTIONAL. This is the shape every repaired skip message in the
# real suite has — the correct form, then the bare one explained on the same line — so a
# guard exempting on the word alone would pass exactly those messages de-prefixed.
reset_tree
printf 'Set `SESSIOMETER_FIXTURE_MEASURE=1` — the bare, un-prefixed name reaches xcodebuild.\n' | write_doc
check "T12 the word AFTER the name does not exempt (the regressed-instruction shape)" 1 "$(run)"

# T13: the workflow spelling. A YAML `env:` mapping sets the switch and carries no `=`.
reset_tree
mkdir -p .github/workflows
cat > .github/workflows/gate.yml <<'YAML'
jobs:
  gate:
    steps:
      - env:
          SESSIOMETER_FIXTURE_MEASURE: "1"
        run: xcodebuild test
YAML
git add .github/workflows/gate.yml
check "T13 an un-prefixed YAML env: mapping is RED (the CI arming spelling)" 1 "$(run)"

# T14: T13's green half — the same mapping, correctly prefixed.
reset_tree
mkdir -p .github/workflows
cat > .github/workflows/gate.yml <<'YAML'
jobs:
  gate:
    steps:
      - env:
          TEST_RUNNER_SESSIOMETER_FIXTURE_MEASURE: "1"
        run: xcodebuild test
YAML
git add .github/workflows/gate.yml
check "T14 the prefixed YAML env: mapping passes" 0 "$(run)"

# T15: the exemption reads the LINE. Matched against the whole `path:lineno:content`
# record, a tracked path carrying the word would exempt every hit in that file.
reset_tree
printf 'Re-derive with `SESSIOMETER_FIXTURE_MEASURE=1 xcodebuild test`.\n' > docs/bare-metal-notes.md
git add docs/bare-metal-notes.md
check "T15 a PATH carrying the exemption word does not exempt the file's hits" 1 "$(run)"
reset_tree

# T18: the TEST-tree scoping. The app reads its own launch switches from its own
# environment, and those are correctly written bare — deriving them would forbid a
# correct line. Same literal-subscript read shape as the suite's, so only the directory
# bound separates them.
reset_tree
mkdir -p apps/menubar/Sources
cat > apps/menubar/Sources/AppLaunch.swift <<'SWIFT'
private var isGalleryMode: Bool {
    ProcessInfo.processInfo.environment["SESSIOMETER_FIXTURE_GALLERY"] == "1"
}
SWIFT
git add apps/menubar/Sources/AppLaunch.swift
printf 'Launch the app by hand with `SESSIOMETER_FIXTURE_GALLERY=1 "$BIN"` to screenshot.\n' | write_doc
check "T18 an APP-side switch is not derived, so its bare command form passes" 0 "$(run)"
rm -rf apps/menubar/Sources
git rm -q -r --cached --ignore-unmatch apps/menubar/Sources >/dev/null

# T16: the not-a-git-tree branch. Outside a work tree there is nothing to scan, which is
# exit 2 — and the reader must be told THAT, not that the derivation shape changed.
outside="$(mktemp -d)"
if ( cd "$outside" && git rev-parse --show-toplevel >/dev/null 2>&1 ); then
    # A tree here would make the case vacuous, so it fails rather than skipping.
    printf 'FAIL  T16 fixture directory %s is inside a git work tree\n' "$outside"
    fail=$((fail + 1))
else
    set +e
    outside_rc="$(cd "$outside" && "$sut" >/dev/null 2>&1; echo $?)"
    outside_msg="$(cd "$outside" && "$sut" 2>&1 >/dev/null || true)"
    set -e
    check "T16 outside a git work tree is exit 2 (nothing to scan is a FAILURE)" 2 "$outside_rc"
    case "$outside_msg" in
        *"not inside a git work tree"*)
            check "T17 …and says so, rather than blaming the derivation shape" 0 0 ;;
        *)
            check "T17 …and says so, rather than blaming the derivation shape" 0 1 ;;
    esac
fi
rm -rf "$outside"

printf '\n%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
