#!/usr/bin/env bash
# Self-contained falsifier + regression test for check-doc-citation-rot.sh
# (issue #1058). Builds a throwaway git repo and exercises the guard across the
# cases that define its contract — in particular proving it goes RED on the exact
# defect the issue reports (a bare line number into a churning file) and GREEN on
# the two things it must never block: a symbol-anchored citation, and a bare line
# number into a file that does not move.
#
# Three of the assertions this file EXECUTES exist because a green suite
# is not by itself evidence that the code CI runs was executed. (Assertions, not
# top-level cases: the two units differ here, and the third of the three shares a
# guard invocation with an assertion that was already present.) Two of them reach
# `churn()`'s windowed arm — the one production always takes, which a fixture
# this size can never enter at the shipped window — and differ only in that
# window, so it is shown to bound the count rather than merely asserted to. The
# third asserts on the BASE_SHA/HEAD_SHA case's MESSAGE rather than its exit
# code, because the fallback exits 1 too and an exit-code-only assertion passes
# just as happily over dead env wiring.
#
# The load-bearing case is `pre-existing bare citation is not flagged`. The gate
# is diff-scoped precisely so it is clearable on the day it lands, with a backlog
# of such citations already in the tree; if that case ever fails, the gate has
# become the unclearable tree-wide check its design rejected.
#
# Peer of check-panel-golden-rebaseline.test.sh, whose harness shape this follows
# deliberately. Run locally:  ./scripts/check-doc-citation-rot.test.sh
set -euo pipefail

here="$(cd "$(dirname "$0")" && pwd)"
guard="$here/check-doc-citation-rot.sh"

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
cd "$work"

git init -q
git config user.email test@test.invalid
git config user.name "citation rot test"
git config commit.gpgsign false

mkdir -p docs/specs src

# A file that moves. 20 commits clears the shipped threshold of 15 without the
# test having to override it — the default is what CI runs, so the default is
# what gets proven here.
# The seed carries three tokens beyond the plain symbol, each load-bearing for a
# case below: an ACRONYM that genuinely OCCURS in the file (so refusing it is a
# statement about its shape in prose, not about it being absent — the whole point
# of issue #1319), a SCREAMING_SNAKE constant and a caps-prefixed TYPE, both of
# which are real identifiers the acronym subtraction must not swallow.
cat > src/churny.rs <<'RUST'
fn stable_anchor_symbol() {}
// CLI entry point. Named here so the acronym is present in the cited file.
const MAX_RETRIES: u32 = 3;
struct JSONWire;
RUST
for i in $(seq 1 20); do
    printf 'fn churn_%s() {}\n' "$i" >> src/churny.rs
    git add src/churny.rs
    git commit -qm "churn $i"
done

# A file that does not. One commit, ever.
printf 'fn quiet_helper() {}\n%s\n' "$(for i in $(seq 1 40); do echo "// line $i"; done)" > src/quiet.rs

# A pre-existing bare citation into the churning file: already in the tree at
# base, therefore outside every PR's diff, therefore never this gate's business.
printf 'Legacy prose citing src/churny.rs:3 with no symbol at all.\n' > docs/specs/legacy.md

git add src/quiet.rs docs/specs/legacy.md
git commit -qm "base"
base="$(git rev-parse HEAD)"

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

# Runs the guard over a one-line doc added on top of `base`, leaving the verdict
# in $rc and the output in $out. Never propagates the guard's exit status as its
# own — a non-zero return here is an expected outcome under test, not an error,
# and letting it escape would abort the run at the first RED case.
run() { # <doc-content>
    git checkout -q -B probe "$base"
    printf '%s\n' "$1" > docs/specs/probe.md
    git add docs/specs/probe.md
    git commit -qm "probe"
    out="$("$guard" "$base" HEAD 2>&1)" && rc=0 || rc=$?
    printf '%s\n' "$out" | sed 's/^/      | /'
}

# Same, with $CITATION_CHURN_WINDOW overridden. The shipped window is 300
# commits and no fixture this size can reach it, so at the default EVERY case
# here takes churn()'s short-history FALLBACK arm — and the windowed arm, which
# is the one production always takes, would be exercised by nothing. Only the
# window is overridden; the threshold stays at the shipped default.
run_windowed() { # <window> <doc-content>
    git checkout -q -B probe "$base"
    printf '%s\n' "$2" > docs/specs/probe.md
    git add docs/specs/probe.md
    git commit -qm "probe"
    out="$(CITATION_CHURN_WINDOW="$1" "$guard" "$base" HEAD 2>&1)" && rc=0 || rc=$?
    printf '%s\n' "$out" | sed 's/^/      | /'
}

# --- The reported defect: a bare range into a file that moves ----------------
run 'Given the verbs parsed at src/churny.rs:3-9'
check "bare line number into a churning file is REJECTED" 1 "$rc"

# --- The remedy the failure message prescribes -------------------------------
run 'Given the verbs parsed by `stable_anchor_symbol` in `src/churny.rs`'
check "symbol citation with no line number is ACCEPTED" 0 "$rc"

run 'Given the roster merge (`stable_anchor_symbol`, `src/churny.rs:3-9`)'
check "symbol-anchored range is ACCEPTED (the number may stay as a locator)" 0 "$rc"

# --- A prose ACRONYM is not a symbol (issue #1319) ---------------------------
# The reported defect. `CLI` OCCURS in the cited file — the seed puts it there on
# purpose — so this pair is not "the token is missing"; it is the guard declining
# a token whose only qualification is that the CamelCase shape test cannot tell an
# acronym apart from an identifier. The two cases differ in BACKTICKS ALONE, which
# is what makes the rule falsifiable: refusing both would be a ban on the word,
# and accepting both is the defect. Delete the subtraction in `anchored()` and the
# first goes green.
run 'The CLI parses 18 verbs at src/churny.rs:3-9'
check "unbackticked ALL-CAPS prose token is REFUSED as an anchor" 1 "$rc"
case "$out" in
    *CLI*prose*) check "refusal NAMES the declined token, so 'name a symbol' is legible" 0 0 ;;
    *) check "refusal NAMES the declined token, so 'name a symbol' is legible" 0 1 ;;
esac

run 'The `CLI` parses 18 verbs at src/churny.rs:3-9'
check "backticking the SAME token accepts it — the escape hatch is real" 0 "$rc"

# The subtraction is "all-caps", not "starts with caps" and not "shouty". Both of
# these are real Rust identifiers and both must survive it; a subtraction written
# over `^[A-Z0-9_]+$` kills the first, and one written over a stricter camel rule
# kills the second.
run 'Retries are capped by MAX_RETRIES at src/churny.rs:3-9'
check "SCREAMING_SNAKE constant still anchors (underscore is not an acronym)" 0 "$rc"

run 'The JSONWire payload is built at src/churny.rs:3-9'
check "caps-prefixed CamelCase type still anchors" 0 "$rc"

# --- Line numbers stay legitimate for stable files ---------------------------
run 'The fallback path is at src/quiet.rs:12 in the parser.'
check "bare line number into a STABLE file is ACCEPTED" 0 "$rc"

# --- churn()'s windowed arm, and that the window really BOUNDS the count -----
# Every case above runs on ~22 commits of fixture history, so every case above
# takes the fallback arm. These two take the windowed one. The pair is chosen so
# that only the window differs: at 22 `churny.rs` moved inside it and the bare
# citation is REJECTED; at 2 the same file, the same citation and the same
# threshold are ACCEPTED, because the churn fell out of the window. That makes
# design choice #2 — churn is a property of the commit under test, not of the
# day it is re-run — falsifiable rather than merely asserted. It is also what
# goes red if the windowed arm is ever neutered: without it the whole suite
# stays green while the real corpus drops to zero violations.
run_windowed 22 'Given the verbs parsed at src/churny.rs:3-9'
check "windowed arm REJECTS a file that churned INSIDE the window" 1 "$rc"

run_windowed 2 'Given the verbs parsed at src/churny.rs:3-9'
check "windowed arm ACCEPTS the same citation once churn falls OUTSIDE it" 0 "$rc"

# --- Resolvability, which needs no judgment about churn ----------------------
run 'The fallback path is at src/quiet.rs:9999 in the parser.'
check "citation past EOF is REJECTED even in a stable file" 1 "$rc"

run 'See `deleted_symbol` at src/does-not-exist.rs:4.'
check "citation into an untracked file is REJECTED" 1 "$rc"

# --- Diff scope: the property that makes this clearable on day one -----------
git checkout -q -B probe "$base"
printf 'Unrelated edit, no citations.\n' > docs/specs/probe.md
git add docs/specs/probe.md
git commit -qm "probe"
out="$("$guard" "$base" HEAD 2>&1)" && rc=0 || rc=$?
printf '%s\n' "$out" | sed 's/^/      | /'
check "pre-existing bare citation in the tree is NOT flagged" 0 "$rc"
case "$out" in
    *"nothing to check"*) check "empty range says 'nothing to check', not 'clean'" 0 0 ;;
    *) check "empty range says 'nothing to check', not 'clean'" 0 1 ;;
esac

# The same tree, swept whole, IS red — proving the case above passed on scope
# rather than because the guard cannot see the legacy citation at all.
"$guard" --audit HEAD >/dev/null 2>&1 && rc=0 || rc=$?
check "--audit DOES see the pre-existing citation the diff scope skipped" 1 "$rc"

# --- BASE_SHA/HEAD_SHA (the wiring CI uses) ----------------------------------
git checkout -q -B probe "$base"
printf 'Given the verbs parsed at src/churny.rs:3-9\n' > docs/specs/probe.md
git add docs/specs/probe.md
git commit -qm "probe"
out="$(BASE_SHA="$base" HEAD_SHA="$(git rev-parse HEAD)" "$guard" 2>&1)" && rc=0 || rc=$?
printf '%s\n' "$out" | sed 's/^/      | /'
check "BASE_SHA/HEAD_SHA env wiring reaches the same verdict" 1 "$rc"
# The exit code ALONE cannot tell that verdict apart from a guard that ignored
# the env entirely: with no range passed and no `origin/main` in this throwaway
# repo, the fallback also exits 1 — on `error: no base revision`. So a total
# failure of this wiring would read as a pass. Assert on what the message says.
# This is the only case covering the path CI actually takes on a `pull_request`.
case "$out" in
    *"no base revision"*) check "env wiring is USED, not silently ignored" 0 1 ;;
    *"will rot silently"*) check "env wiring is USED, not silently ignored" 0 0 ;;
    *) check "env wiring is USED, not silently ignored" 0 1 ;;
esac

# --- A shallow clone must be fatal, never a quiet pass -----------------------
# Under fetch-depth:1 the one grafted commit is parentless, so every churn count
# collapses to 1 — under the threshold, so the bare citation above would sail
# through. That is the failure this refuses rather than reports.
git checkout -q -B probe "$base" 2>/dev/null || true
shallow="$work/shallow"
git clone -q --depth 1 "file://$work" "$shallow" 2>/dev/null
out="$(cd "$shallow" && "$guard" --audit 2>&1)" && rc=0 || rc=$?
printf '%s\n' "$out" | sed 's/^/      | /'
check "shallow clone is REFUSED rather than silently passed" 1 "$rc"
case "$out" in
    *SHALLOW*) check "refusal names the shallow clone as the cause" 0 0 ;;
    *) check "refusal names the shallow clone as the cause" 0 1 ;;
esac

printf '\n%s passed, %s failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
