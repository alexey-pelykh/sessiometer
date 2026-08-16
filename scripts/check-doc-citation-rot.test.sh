#!/usr/bin/env bash
# Self-contained falsifier + regression test for check-doc-citation-rot.sh
# (issue #1058). Builds a throwaway git repo and exercises the guard across the
# cases that define its contract — in particular proving it goes RED on the exact
# defect the issue reports (a bare line number into a churning file) and GREEN on
# the two things it must never block: a citation whose symbol occurs AT the lines
# it cites, or cites none at all, and a bare line number into a file that does
# not move.
#
# The second contract half is issue #1338: anchoring is scoped to the CITED
# RANGE, not the whole file — and on BOTH sides of it, since the symbol must sit
# at or after the range start AND at or before its end. Each side is pinned by
# its own pair differing in the range alone: `stable_anchor_symbol` cited `1-9`
# against `3-9` for the lower bound, `JSONWire` cited `1-4` against `1-3` for the
# upper. The upper pair is not decoration — it was added because neutering ONLY
# the upper bound was measured to leave this entire suite green, so the bound the
# rule half rests on was asserted by nothing. The same scoping governs the
# DECLINED-acronym report, and its pair — `CLI` cited `1-9` against `3-9` — was
# found by the same measurement and added for the same reason.
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
# All four sit on lines 1-4, ABOVE the churn, and that placement is load-bearing
# twice over since issue #1338: the shape cases cite `1-9` so that a token's
# SHAPE is the only thing under test, and `3-9` — the same file, the same symbol,
# a range that excludes line 1 — is what makes the misplaced-anchor pair differ
# in the range alone. Appending churn below them keeps both stable.
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
# `quiet_helper` sits on line 1 and NOWHERE else, which is what lets a citation
# into this file name a symbol that is real, present, and not where the number
# points — the issue #1388 shape, in a file whose churn exempts it from being
# ASKED for one. The trailing `churny` mention is the other half: it is not a
# symbol here at all, it is what the path `src/churny.rs` shreds to, and a doc
# line naming that neighbouring file must not thereby "disagree" with a citation
# into this one.
printf 'fn quiet_helper() {}\n%s\n' "$(for i in $(seq 1 40); do echo "// line $i"; done)" > src/quiet.rs
printf '// The moving counterpart lives in churny; see it for the churn cases.\n' >> src/quiet.rs

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

run 'Given the roster merge (`stable_anchor_symbol`, `src/churny.rs:1-9`)'
check "range whose symbol occurs INSIDE it is ACCEPTED (number stays a locator)" 0 "$rc"

# --- The anchor must be AT the citation, not merely in the file (issue #1338) -
# The pair above and below differs in THE RANGE ALONE: `stable_anchor_symbol` is
# on line 1, so `1-9` contains it and `3-9` does not. The symbol is equally real,
# equally present in the file and equally backticked in both. Widening the
# exemption back to the file — drop the range arguments from `occurs` in
# `anchored()` — turns the REJECT below green, and NOT it alone. The blast
# radius was MEASURED rather than reasoned about, because it is wider than it
# looks: the same mutation also greens the `1-3` upper-bound rejection below and
# the single-line `:2` rejection further down, and it reds the two POSITIVE
# message assertions here, since a case that stops failing prints no message left
# to assert on. (The negative one — that the refusal picks no side — passes
# VACUOUSLY for that same reason, which is exactly why it is not the only one.)
# Performing the same widening the other way, by deleting the ranged arm inside
# `occurs()`, reds that set AND one more — the declined-token message assertion
# below — because `occurs()` serves the acceptance loop and the acronym loop
# both, while `anchored()`'s acceptance call serves only the first. That extra
# RED is the only thing that tells the two widenings apart. And that is how the
# defect shipped: an ADR cited the verb table as `src/cli.rs:741-765` while the
# range sat on `parse_import`'s tail, `parse`'s doc comment and `fn parse`, and
# rode through on the backticked word `login`.
run 'Given the roster merge (`stable_anchor_symbol`, `src/churny.rs:3-9`)'
check "symbol occurring in the file but OUTSIDE the cited range is REJECTED" 1 "$rc"
# The author has already done what "name a symbol" asks, so that instruction would
# read as nonsense here. What the guard has MEASURED is that the two disagree;
# which of them is wrong it cannot see, and the corpus carries both readings — a
# citation landing on an item's doc comment, with the item's own name a line past
# the cited end, is a RIGHT range whose author must not be told to move it. So the
# refusal names the symbol and the lines, prescribes the remedy for BOTH readings,
# and asserts neither. Three separate assertions because they fail separately: a
# message can name the symbol while still picking a side.
names=1; both=1; asserted=0
case "$out" in *stable_anchor_symbol*"NOT within lines 3-9"*) names=0 ;; esac
case "$out" in *"re-derive the range from the symbol"*"widen the range"*) both=0 ;; esac
case "$out" in *"so the number has drifted"*) asserted=1 ;; esac
check "refusal names the symbol and the lines it is absent from" 0 "$names"
check "refusal prescribes the remedy for BOTH readings" 0 "$both"
check "refusal does NOT assert which of the two is wrong" 0 "$asserted"

# Containment is TWO-SIDED and everything else here exercises only its LOWER
# half: every other case that puts the symbol OUTSIDE the range puts it before
# the range START, so the upper bound could be deleted outright without
# reddening one of them (it was, and not one moved). `JSONWire` is on line 4, so
# `1-4` ends ON it and `1-3` stops one line short: the mirror image. The REJECT
# half below is the ONLY assertion in this file that reds when the upper bound
# alone is neutered — print `${first},$p` in place of `${first},${last}` and it
# is the one line that changes. It is also the direction the real corpus leans:
# swept with `--audit`, most misplaced anchors sit PAST the cited end, not
# before its start.
run 'The `JSONWire` payload is built at src/churny.rs:1-4'
check "range ENDING on the symbol is ACCEPTED (the upper bound is inclusive)" 0 "$rc"

run 'The `JSONWire` payload is built at src/churny.rs:1-3'
check "symbol one line PAST the cited range end is REJECTED" 1 "$rc"

# A citation with no `-` is a one-line range, which is the only place the `first`
# extraction runs without a suffix to strip. Off by ONE line is the whole
# difference between these two, so the boundary is proven rather than assumed.
run 'The entry point is `stable_anchor_symbol` at src/churny.rs:1.'
check "single-line citation ON the symbol is ACCEPTED" 0 "$rc"

run 'The entry point is `stable_anchor_symbol` at src/churny.rs:2.'
check "single-line citation one line OFF the symbol is REJECTED" 1 "$rc"

# --- A prose ACRONYM is not a symbol (issue #1319) ---------------------------
# The reported defect. `CLI` OCCURS at the cited lines — the seed puts it there on
# purpose — so this pair is not "the token is missing"; it is the guard declining
# a token whose only qualification is that the CamelCase shape test cannot tell an
# acronym apart from an identifier. The two cases differ in BACKTICKS ALONE, which
# is what makes the rule falsifiable: refusing both would be a ban on the word,
# and accepting both is the defect. Delete the subtraction in `anchored()` and the
# first goes green.
run 'The CLI parses 18 verbs at src/churny.rs:1-9'
check "unbackticked ALL-CAPS prose token is REFUSED as an anchor" 1 "$rc"
case "$out" in
    *CLI*prose*) check "refusal NAMES the declined token, so 'name a symbol' is legible" 0 0 ;;
    *) check "refusal NAMES the declined token, so 'name a symbol' is legible" 0 1 ;;
esac

run 'The `CLI` parses 18 verbs at src/churny.rs:1-9'
check "backticking the SAME token accepts it — the escape hatch is real" 0 "$rc"

# The DECLINED report is range-scoped too (issue #1338), and only its MESSAGE can
# say so: the citation is unanchored either way, so this case and the `1-9` one
# above BOTH exit 1 and an exit-code assertion would pass straight over the
# regression. `CLI` is on line 2, so `1-9` contains it and `3-9` does not — the
# range is the only variable, exactly as in the misplaced-anchor pair above.
# Un-range-scoping that loop — `occurs "$tok" "$file"` in place of the ranged
# call — was MEASURED before this pair was written: it left every assertion in
# this file green, while making the guard report that `CLI` occurs at lines it
# has zero hits in and prescribe backticking it, a remedy that would not anchor
# the citation. Under that mutation the message assertion below is the one and
# only line in this file that reds; the exit-code assertion above does not move.
run 'The CLI parses 18 verbs at src/churny.rs:3-9'
check "acronym OUTSIDE the cited range is still REJECTED" 1 "$rc"
case "$out" in
    *"CLI occurs at those lines"*)
        check "refusal does NOT claim the declined token occurs at those lines" 0 1 ;;
    *)
        check "refusal does NOT claim the declined token occurs at those lines" 0 0 ;;
esac

# The subtraction is "all-caps", not "starts with caps" and not "shouty". Both of
# these are real Rust identifiers and both must survive it; a subtraction written
# over `^[A-Z0-9_]+$` kills the first, and one written over a stricter camel rule
# kills the second.
run 'Retries are capped by MAX_RETRIES at src/churny.rs:1-9'
check "SCREAMING_SNAKE constant still anchors (underscore is not an acronym)" 0 "$rc"

run 'The JSONWire payload is built at src/churny.rs:1-9'
check "caps-prefixed CamelCase type still anchors" 0 "$rc"

# --- Line numbers stay legitimate for stable files ---------------------------
run 'The fallback path is at src/quiet.rs:12 in the parser.'
check "bare line number into a STABLE file is ACCEPTED" 0 "$rc"

# --- The two questions, separated (issue #1388) ------------------------------
# The pair above and below differ in ONE thing: whether the doc line names a
# symbol. Neither file moved, so churn cannot be what tells them apart — which is
# the whole claim. A citation that OWES an anchor is exempted by a quiet file
# (above); a citation that already CARRIES one is checked against its own number
# anyway (below), because nothing is being demanded and there is nothing to
# exempt. Collapsing the two is what passed `src/config.rs:341` while its own
# line said `account_uuid`, and the collapsed form is not recoverable by moving
# the threshold, so no `run_windowed` variant of this can exist.
run 'The fallback path is at src/quiet.rs:12, reached from `quiet_helper`.'
check "symbol disagreeing with the number is REJECTED even in a STABLE file" 1 "$rc"

# Two assertions, because the single one they replace named two things and
# pinned one (issue #1406). Its body greps only for the phrase, so restoring the
# churn parenthetical PR #1401 took out of that message — `(${c} commits in the
# last ${WINDOW})`, after `disagree`, in `check_line`'s `elsewhere` branch —
# left this whole suite green, measured by doing it. The second clause now has a
# body of its own, and each name states exactly what its own test pins.
#
# They are a PAIR, not independents: the negative one passes VACUOUSLY if the
# disagreement message goes away entirely, so it carries the claim only beside
# the positive one above it. It reads that message's OWN line rather than the
# whole output, so it says what its name says — the DEMAND branch's two messages
# carry the churn count legitimately, and this fixture cites once today but need
# not forever. `case` rather than `grep -q`, so that a SIGPIPE under `pipefail`
# cannot turn a NEGATIVE assertion green — the hazard `occurs()` in the guard
# documents and dodges with process substitution, where the same shape changes
# the violation count silently instead of failing loudly.
printf '%s\n' "$out" | grep -q 'the symbol and the number disagree' && rc=0 || rc=1
check "refusal reports the disagreement" 0 "$rc"

disagreement="$(printf '%s\n' "$out" | grep 'the symbol and the number disagree' || true)"
case "$disagreement" in *"commits in the last"*) rc=1 ;; *) rc=0 ;; esac
check "the disagreement refusal carries no churn count" 0 "$rc"

# A backticked PATH is one token to a reader, so it anchors nothing and — now
# that the check above runs at any churn — must not manufacture a disagreement
# either. `src/churny.rs` shreds to `src`, `churny`, `rs`; `churny` really does
# occur in `src/quiet.rs`, on its last line, nowhere near 12.
run 'The fallback at src/quiet.rs:12 mirrors `stable_anchor_symbol` in `src/churny.rs`.'
check "a backticked PATH neither anchors nor manufactures a disagreement" 0 "$rc"

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
