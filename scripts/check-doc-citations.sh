#!/usr/bin/env bash
# Fail the build if a committed document cites a path that will not resolve in a
# fresh clone (issue #1060).
#
# Documents under docs/ carry path-valued frontmatter keys — `design-doc`,
# `requirements-brief`, `source` and friends. Nothing dereferences them, so
# nothing notices when one goes bad, and a field with no consumer decays
# monotonically. This is measured, not assumed: a correct pointer became a
# fabricated one inside an hour, through a reviewed and CI-green PR whose own
# last commit was about restoring provenance. A pipeline mints these pointers
# mechanically, so their count grows on its own; a one-time repair is
# insufficient by construction.
#
# THE RULE
#   A frontmatter value that is PATH-SHAPED must be a git-tracked,
#   repo-root-relative path. A value that is not path-shaped is a provenance
#   note, legal only on the pointer-or-note keys below.
#
# Two design choices carry the weight, and a naive implementation gets both wrong:
#
#   1. Reachability is `git ls-files --error-unmatch`, NEVER `test -e`.
#      Path-existence is machine-dependent: it passes an untracked local file on
#      the author's disk and fails the same file in CI. That non-determinism is
#      the defect, not a check for it. Git-tracked answers the question a reader
#      actually has — does this resolve in a fresh clone?
#
#   2. Path-shape is decided by the value's FIRST whitespace-delimited token.
#      Not "contains a slash" (`source: session /investigate — …` is prose), and
#      not "the whole value has no whitespace" — that rule missed 2 of the 10
#      real defect sites, both being a genuine gitignored path carrying a
#      trailing parenthetical. First-token also lets a note NAME its referent
#      (`private HQ (prd-stats)`) without the check demanding that file exist,
#      which is what makes "convert to a note" a real option.
#
# Scope is the allowlist below, derived from a sweep of every frontmatter key in
# all of docs/ — not from the keys a defect report happened to name. An allowlist
# built from a partial survey has a blind spot exactly the size of what the
# survey missed, and this one did: `scope-working-doc` was absent from an earlier
# draft, and the defect it guards went unreported.
#
# A count of ZERO evaluated citations is a FAILURE, not a pass. A gate that goes
# green because it looked at nothing is the same write-only-field failure this
# script exists to end, one level up.
#
# Run locally:  ./scripts/check-doc-citations.sh
set -euo pipefail

root="${1:-docs}"

cd "$(git rev-parse --show-toplevel)"

# Keys whose value must be a tracked, root-relative path. A prose note here is a
# pointer that points nowhere.
pointer_keys=" design-doc design-brief requirements-brief scope-brief prd design scope-working-doc "
# Keys that may hold EITHER a tracked path or a prose note. `source` is genuinely
# bimodal: in a brief it names the primary document (a path); in a requirements
# doc it names the session or scratch it came from (a note). `parent-requirements`
# targets a private repo-root sibling that no clone contains, so no depth
# correction makes it resolvable — a note is the only honest form.
note_ok_keys=" source parent-requirements "

# A first token ending in one of these is a path claim.
path_extensions=" md yml yaml toml json sh rs swift "

evaluated=0
documents=0
violations=0

report() { # <file> <line> <key> <value> <reason>
    printf '  %s:%s  %s: %s  -> %s\n' "$1" "$2" "$3" "$4" "$5" >&2
    violations=$((violations + 1))
}

if [ ! -d "$root" ]; then
    echo "error: '$root' is not a directory." >&2
    exit 1
fi

while IFS= read -r file; do
    documents=$((documents + 1))

    # Frontmatter only: the block between the first `---` and the next one. Prose,
    # code fences and inline Markdown links are never parsed — a path-shaped
    # string in a code sample is documentation, not a citation.
    [ "$(head -n 1 "$file")" = "---" ] || continue
    end="$(awk 'NR > 1 && /^---$/ { print NR; exit }' "$file")"
    [ -n "$end" ] || continue

    # lineno starts at 0 and increments BEFORE use, so `line` and `lineno` refer to
    # the same physical line. Getting this wrong silently drops the last
    # frontmatter line — which is exactly where an `artifacts:` block's last
    # citation sits.
    lineno=0
    while IFS= read -r line; do
        lineno=$((lineno + 1))
        [ "$lineno" -lt "$end" ] || break

        # `key: value`, at any indent so nested blocks (artifacts:) are covered.
        case "$line" in
            *:*) ;;
            *) continue ;;
        esac
        key="${line%%:*}"
        key="$(printf '%s' "$key" | tr -d '[:space:]')"
        case "$key" in
            ''|*[!a-z-]*) continue ;;
        esac

        case "$pointer_keys$note_ok_keys" in
            *" $key "*) ;;
            *) continue ;;
        esac

        value="${line#*:}"
        # Strip a trailing `# …` comment, then outer whitespace.
        value="${value%%#*}"
        value="$(printf '%s' "$value" | sed 's/^[[:space:]]*//; s/[[:space:]]*$//')"
        [ -n "$value" ] || continue

        evaluated=$((evaluated + 1))

        token="${value%%[[:space:]]*}"
        ext="${token##*.}"
        shaped=no
        case "$path_extensions" in
            *" $ext "*) [ "$token" != "$ext" ] && shaped=yes ;;
        esac

        if [ "$shaped" = no ]; then
            case "$pointer_keys" in
                *" $key "*) report "$file" "$lineno" "$key" "$value" "note-in-pointer-key" ;;
            esac
            continue
        fi

        case "$token" in
            ../*|/*|*/../*)
                report "$file" "$lineno" "$key" "$token" "not-root-relative"
                continue
                ;;
        esac

        if git ls-files --error-unmatch "$token" >/dev/null 2>&1; then
            continue
        fi

        if git check-ignore -q "$token" 2>/dev/null; then
            report "$file" "$lineno" "$key" "$token" "gitignored"
        else
            report "$file" "$lineno" "$key" "$token" "not-git-tracked"
        fi
    done < "$file"
done < <(find "$root" -type f -name '*.md' | sort)

# Every failing site is reported before exiting — no stop-at-first. A reader
# fixing one citation should not have to re-run to discover the next.
if [ "$violations" -gt 0 ]; then
    echo "error: $violations citation(s) will not resolve in a fresh clone (see above; issue #1060)." >&2
    echo "Fix: make the value a git-tracked, repo-root-relative path — or, on 'source' /" >&2
    echo "'parent-requirements' only, replace it with a prose note naming the referent." >&2
    exit 1
fi

# Cardinality is printed, and zero is fatal. Without this, an empty or
# mis-scoped run is indistinguishable from a clean one.
if [ "$evaluated" -eq 0 ]; then
    echo "error: evaluated 0 citations across $documents document(s) under '$root'." >&2
    echo "A gate that passes because it examined nothing is not evidence. Check the scan root." >&2
    exit 1
fi

echo "ok: evaluated $evaluated citations across $documents documents under '$root'; all resolve."
