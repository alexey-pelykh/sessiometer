#!/usr/bin/env bash
# Self-contained falsifier + regression test for check-doc-citations.sh
# (issue #1064). Builds a throwaway git repo and exercises the guard across the
# cases that define its contract.
#
# Six of these are FALSIFIERS — each one fails against a specific wrong
# implementation, and three of those implementations were live in this gate's own
# design drafts before the corpus falsified them:
#
#   T2   kills a path-existence (`test -e`) reachability check
#   T6   kills a note-only rule for `source`
#   T11  kills a "contains a slash" path-shape test
#   T13  kills a gate that passes because it evaluated nothing
#   T15  kills a "whole value has no whitespace" path-shape test
#   T16  kills a rule that forbids a note from naming its referent
#
# A suite without them goes green on every defect this gate exists to prevent.
#
# MUTATION-VALIDATED. A suite that passes against the correct implementation is
# no evidence it would catch a wrong one, so each falsifier was checked against
# the mutant it targets — the guard was rewritten to the wrong rule and the test
# confirmed to go RED:
#
#   guard uses `test -e` instead of git-tracked      -> T2  RED
#   `source` made note-ONLY (a path there rejected)  -> T6  RED
#   path-shape tests the whole value, not the token  -> T15 RED
#   zero-count guard removed                         -> T13 RED
#
# That pass also corrected a mislabel: an initial mutation made `source`
# pointer-only (rejecting a NOTE) — the opposite error — and T6 stayed green
# while T7/T11 caught it. The mutation was wrong, not the test; but a mutation
# never run would have left that indistinguishable from an inert falsifier.
#
# Run locally:  ./scripts/check-doc-citations.test.sh
set -euo pipefail

here="$(cd "$(dirname "$0")" && pwd)"
guard="$here/check-doc-citations.sh"

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
cd "$work"

git init -q
git config user.email test@test.invalid
git config user.name "citation test"
git config commit.gpgsign false

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

check_out() { # <label> <needle> — assert the last run's stderr contained <needle>
    if grep -qF "$2" "$work/out.txt"; then
        printf 'PASS  %s (reported "%s")\n' "$1" "$2"
        pass=$((pass + 1))
    else
        printf 'FAIL  %s (output missing "%s")\n' "$1" "$2"
        printf '      got: %s\n' "$(tr '\n' '|' < "$work/out.txt")"
        fail=$((fail + 1))
    fi
}

run() { # capture exit code without tripping set -e; stderr+stdout -> out.txt
    local rc
    set +e
    "$guard" docs > "$work/out.txt" 2>&1
    rc=$?
    set -e
    echo "$rc"
}

# A fresh docs/ for each case, so one case's fixture cannot mask another's.
reset_docs() { rm -rf docs; mkdir -p docs/requirements docs/design docs/briefs; }

# Referents that really are tracked — the ground truth the pointer cases resolve to.
mkdir -p docs/design docs/requirements
printf -- '---\ntitle: D\n---\n' > docs/design/real-design.md
printf -- '---\ntitle: R\n---\n' > docs/requirements/real-requirements.md
printf 'ignored/\n.tmp/\n' > .gitignore
git add .gitignore docs/design/real-design.md docs/requirements/real-requirements.md
git commit -qm "tracked referents"

# Re-stage the fixture referents after each reset (reset_docs deletes them).
restore_referents() {
    mkdir -p docs/design docs/requirements docs/briefs
    git checkout -q -- docs/design/real-design.md docs/requirements/real-requirements.md
}

# ---------------------------------------------------------------- T1
reset_docs; restore_referents
printf -- '---\ntitle: A\ndesign-doc: docs/design/real-design.md\n---\n' > docs/requirements/a.md
check "T1  pointer key -> tracked root-relative path" 0 "$(run)"

# ---------------------------------------------------------------- T2  FALSIFIER
# The file EXISTS on disk. Only its tracked status differs. A `test -e`
# implementation passes this case and is thereby proven wrong: it would pass on
# the author's machine and fail the identical tree in CI.
reset_docs; restore_referents
printf -- '---\ntitle: U\n---\n' > docs/design/untracked-design.md
printf -- '---\ntitle: A\ndesign-doc: docs/design/untracked-design.md\n---\n' > docs/requirements/a.md
check "T2  FALSIFIER pointer -> on disk but UNTRACKED" 1 "$(run)"
check_out "T2  reason is not-git-tracked" "not-git-tracked"

# ---------------------------------------------------------------- T3
reset_docs; restore_referents
printf -- '---\ntitle: A\ndesign-doc: docs/design/never-existed.md\n---\n' > docs/requirements/a.md
check "T3  pointer -> nonexistent file" 1 "$(run)"

# ---------------------------------------------------------------- T4
reset_docs; restore_referents
mkdir -p .tmp; printf 'scratch\n' > .tmp/scope.md
printf -- '---\ntitle: A\nscope-working-doc: .tmp/scope.md\n---\n' > docs/requirements/a.md
check "T4  pointer -> gitignored path" 1 "$(run)"
check_out "T4  reason is gitignored" "gitignored"

# ---------------------------------------------------------------- T5
reset_docs; restore_referents
printf -- '---\ntitle: A\ndesign-doc: a design we talked about once\n---\n' > docs/requirements/a.md
check "T5  pointer key holding a prose note" 1 "$(run)"
check_out "T5  reason is note-in-pointer-key" "note-in-pointer-key"

# ---------------------------------------------------------------- T6  FALSIFIER
# `source` is bimodal. In a brief it names the primary document — a tracked path.
# A note-only rule for this key fails here, and would have failed 6 correct
# citations in the real repo on its first run.
reset_docs; restore_referents
printf -- '---\ntype: design-brief\nsource: docs/design/real-design.md\n---\n' > docs/briefs/b.md
check "T6  FALSIFIER source holding a TRACKED PATH" 0 "$(run)"

# ---------------------------------------------------------------- T7
reset_docs; restore_referents
printf -- '---\ntitle: A\nsource: session context, working notes not retained\n---\n' > docs/requirements/a.md
check "T7  source holding a prose note" 0 "$(run)"

# ---------------------------------------------------------------- T8
reset_docs; restore_referents
printf -- '---\ntitle: A\ndesign-doc: ../design/real-design.md\n---\n' > docs/requirements/a.md
check "T8  document-relative path (resolves under the other base)" 1 "$(run)"
check_out "T8  reason is not-root-relative" "not-root-relative"

# ---------------------------------------------------------------- T9
# A path-shaped string in prose and in a fenced block is documentation, not a
# citation. Only frontmatter is parsed.
reset_docs; restore_referents
{
    printf -- '---\ntitle: A\nsource: a note\n---\n\n'
    printf 'See docs/design/never-existed.md for details.\n\n'
    printf '```yaml\ndesign-doc: docs/design/also-never-existed.md\n```\n'
    printf '\n[link](docs/design/third-nonexistent.md)\n'
} > docs/requirements/a.md
check "T9  path-shaped strings in prose / code fence / link" 0 "$(run)"

# ---------------------------------------------------------------- T10
reset_docs; restore_referents
printf -- '---\ntitle: A\ndesign-doc: docs/design/gone-one.md\nrequirements-brief: docs/briefs/gone-two.md\n---\n' > docs/requirements/a.md
check "T10 two bad sites in one file" 1 "$(run)"
check_out "T10 first site reported" "gone-one.md"
check_out "T10 second site reported (no stop-at-first)" "gone-two.md"

# ---------------------------------------------------------------- T11 FALSIFIER
# Prose whose only slash belongs to a slash-command name or an initialism. A
# "contains a slash" detector flags both and is thereby proven wrong.
reset_docs; restore_referents
printf -- '---\ntitle: A\nsource: session /investigate — stats roster aggregate and fleet runway\nparent-requirements: GUI/CLI capability parity, private HQ\n---\n' > docs/requirements/a.md
check "T11 FALSIFIER note containing a slash" 0 "$(run)"

# ---------------------------------------------------------------- T12
reset_docs; restore_referents
printf 'Just a body, no frontmatter at all.\n' > docs/requirements/bare.md
printf -- '---\ntitle: A\nsource: a note\n---\n' > docs/requirements/a.md
check "T12 document with no frontmatter is tolerated" 0 "$(run)"

# ---------------------------------------------------------------- T13 FALSIFIER
# Zero citations is a FAILURE. A gate that goes green because it examined
# nothing is not evidence — the same write-only-field failure, one level up.
reset_docs
check "T13 FALSIFIER zero documents -> FAIL, not pass" 1 "$(run)"
check_out "T13 says it evaluated nothing" "evaluated 0 citations"

# ---------------------------------------------------------------- T14
reset_docs; restore_referents
{
    printf -- '---\ntitle: A\nsource: a note\n---\n\n'
    printf 'Section one.\n\n---\n\nSection two, after a horizontal rule.\n\n'
    printf 'design-doc: docs/design/never-existed.md\n'
} > docs/requirements/a.md
check "T14 --- horizontal rule in prose body" 0 "$(run)"

# ---------------------------------------------------------------- T15 FALSIFIER
# A real gitignored path carrying a trailing parenthetical. A "whole value has no
# whitespace" path-shape rule classifies this as a legal note and lets it
# through — which is how it missed 2 of the 10 real defect sites, silently and in
# the permissive direction.
reset_docs; restore_referents
mkdir -p .tmp; printf 'scratch\n' > .tmp/findings.md
printf -- '---\ntitle: A\nsource: .tmp/findings.md (/investigate 2026-07-30)\n---\n' > docs/requirements/a.md
check "T15 FALSIFIER gitignored path + parenthetical prose" 1 "$(run)"
check_out "T15 reason is gitignored" "gitignored"

# ---------------------------------------------------------------- T16 FALSIFIER
# A note may NAME its referent without that file having to be tracked — the
# filename is not the first token. This is what makes "convert to a note" a real
# option rather than a euphemism for deleting the information.
reset_docs; restore_referents
printf -- '---\ntitle: A\nparent-requirements: private HQ (prd-stats.md), REQ-STA-* family\n---\n' > docs/requirements/a.md
check "T16 FALSIFIER note naming a file in a later token" 0 "$(run)"

# ---------------------------------------------------------------- summary
printf '\n%s passed, %s failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
