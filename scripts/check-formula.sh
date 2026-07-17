#!/usr/bin/env bash
# Fail the build if the canonical Homebrew formula does not pass Homebrew's own
# static checks — `brew style` and `brew audit --strict`.
#
# This repo ships `Formula/sessiometer.rb`, the canonical source of the formula
# mirrored to the published tap `sessiometer/homebrew-tap` (ADR-0021) — but until
# issue #567 nothing here ever checked it. ADR-0021 records the gap as "No CI
# covers the formula — on either side", and that gap is the root cause of #566: a
# `depends_on` stanza-order defect reached the published tap precisely because
# nothing checked the canonical formula first. `brew style` + `brew audit
# --strict` catch exactly that class of defect (FormulaAudit/DependencyOrder).
#
# Static-only by design: no `brew install` / `brew test` / `brew test-bot`, all of
# which compile the Rust crate. The tap's own CI (sessiometer/homebrew-tap#1,
# tracked here as #560) owns the full install + `test do` + bottle build. This
# side stays fast enough to run on every `Formula/**` touch. `--online` (network)
# and `--new` (homebrew-core submission rules) are deliberately not passed.
#
# WHY THE SCRATCH TAP: neither check can be aimed at a loose file path.
#   * `brew audit` refuses outright —
#         Error: Calling `brew audit [path ...]` is disabled! Use `brew audit [name ...]` instead.
#     and still refuses under HOMEBREW_DEVELOPER=1. This is unconditional, and is
#     NOT the developer-mode guard ADR-0021 describes for `brew install <path>`.
#   * `brew style <path>` does run, and does apply the FormulaAudit cops — but it
#     lints a loose `.rb` as ordinary Ruby, so it also reports a false
#     `Style/FrozenStringLiteralComment` offense that a formula inside a tap is
#     exempt from. Aimed at the current, correct formula it is RED on arrival.
# Homebrew resolves both checks by *name*, so the formula is staged into a
# throwaway tap and checked as `<scratch-tap>/sessiometer`. The scratch tap is
# created and removed here; the real sessiometer/homebrew-tap is never touched —
# its name is deliberately distinct so this cannot resolve to, or clobber, the
# published tap even on a maintainer's laptop where that tap is installed.
#
# Peer of scripts/check-ci-ok-needs.sh (#318) and check-gate-change-ack.sh
# (#317): a small guard, runnable locally, wired into ci-ok.needs so it can never
# be skipped past. Its falsifier lives in scripts/check-formula.test.sh.
#
# Usage:
#   check-formula.sh [<formula-path>]     # default: Formula/sessiometer.rb
#
# The path argument exists so the falsifier test can aim the guard at a known-bad
# formula and prove it goes RED.
set -euo pipefail

formula="${1:-Formula/sessiometer.rb}"

if [ ! -f "$formula" ]; then
    echo "error: formula not found: $formula" >&2
    exit 1
fi

if ! command -v brew >/dev/null 2>&1; then
    echo "error: 'brew' is required to check ${formula} but was not found." >&2
    echo "It is preinstalled on GitHub macos runners; locally: https://brew.sh" >&2
    exit 1
fi

# Keep the run fast and quiet: no auto-update on every invocation, no analytics
# ping, no environment hints appended to output.
export HOMEBREW_NO_AUTO_UPDATE=1
export HOMEBREW_NO_ANALYTICS=1
export HOMEBREW_NO_ENV_HINTS=1

# Homebrew taps root; `mkdir -p` below creates it if a tap-less Homebrew has none
# yet (it's .gitignored, created on first tap), so there's deliberately no
# pre-existence check — an absent taps root is a normal state, not an error.
taps_root="$(brew --repository)/Library/Taps"

# Scratch tap coordinates. All components are literals, so the paths below can
# never expand to something unintended.
tap_user="sessiometer-canonical-ci"
tap_qualified="$tap_user/canonical/sessiometer"
tap_user_dir="$taps_root/$tap_user"
tap_path="$tap_user_dir/homebrew-canonical"

# Remove the scratch tap (and its now-empty user dir) on any exit path. Also run
# up-front so a previous interrupted run cannot leave a stale formula behind.
cleanup() { rm -rf "$tap_path"; rmdir "$tap_user_dir" 2>/dev/null || true; }
trap cleanup EXIT

cleanup
mkdir -p "$tap_path/Formula"
cp "$formula" "$tap_path/Formula/sessiometer.rb"

# Run both checks even if the first fails, so one pass reports every offense.
rc=0

echo "==> brew style $tap_qualified   (source: $formula)"
brew style "$tap_qualified" || rc=1

echo "==> brew audit --strict --formula $tap_qualified"
brew audit --strict --formula "$tap_qualified" || rc=1

if [ "$rc" -ne 0 ]; then
    {
        echo
        echo "error: $formula failed Homebrew's static checks (see above)."
        echo
        echo "This is the gate that #566's stanza-order defect would have tripped."
        echo "Fix the offenses listed above by hand, then re-run:"
        echo
        echo "    ./scripts/check-formula.sh"
        echo
        echo "Do NOT reach for 'brew style --fix $formula'. Offenses are reported as"
        echo "[Correctable], but autocorrecting the formula OUTSIDE a tap rewrites it as if"
        echo "it were ordinary Ruby: it prepends a '# frozen_string_literal: true' magic"
        echo "comment that a formula in a tap is exempt from — silently corrupting the file"
        echo "this guard exists to protect, and the mirror that copies it (ADR-0021)."
    } >&2
    exit 1
fi

echo "ok: $formula passes brew style + brew audit --strict."
