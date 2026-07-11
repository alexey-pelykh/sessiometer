#!/usr/bin/env bash
# Drift canary for the issue #466 spike: "is there a Claude Code knob to disable
# the invalid_grant canonical-scrub?" — answered NO as of CC 2.1.207. Findings of
# record live in build/version-compat.md `# Issue #466`; this script re-asserts
# the core claim, OFFLINE, against the stock CC binary, so a reviewer can confirm
# it and a future CC bump that ADDS a knob (or moves/removes the scrub) trips a
# loud, cheap signal instead of silently invalidating #467/#468's premise.
#
# SAFETY — this script is a pure read of the shipped binary:
#   * it NEVER reads or writes a credential, spawns `claude`, or makes a network
#     call — it greps the on-disk Bun-compiled binary (embedded JS is text);
#   * no keychain access, no isolated dir, no fake token — nothing to clean up.
# It is a MANUAL diagnostic (needs a stock CC binary on disk) and is deliberately
# NOT wired into CI (CI never ships a real `claude`; the supported-range gate in
# build/release-checklist.md is the release-time home for CC-version drift).
#
# What it asserts against the stock binary (keyed on the STABLE `tengu_oauth_*`
# analytics event strings, which survive minification across CC versions — the
# minified symbol names do NOT, so they are not used as anchors):
#   1  the invalid_grant scrub is still REACHED   (marked-dead event present)
#   2  the scrub still CLEARS the shared item      (cleared-on-disk event present)
#   3  the scrub still empties BOTH tokens          (refreshToken:""+accessToken:"" shape)
#   4  the scrub is still UN-GATED                  (no process.env / feature-gate in its window)
# Any FAIL = CC drift → re-run the #466 decode and update build/version-compat.md.
set -euo pipefail

# --- resolve the stock binary (never a byte-patched wrapper) ---------------------
# $CC_BIN wins; else the version dir matching `claude --version`; else the
# lexically-highest installed version. Skips cleanly if none is found.
versions_dir="${CLAUDE_VERSIONS_DIR:-$HOME/.local/share/claude/versions}"
bin="${CC_BIN:-}"
if [[ -z "$bin" ]]; then
    ver=""
    if command -v claude >/dev/null 2>&1; then
        ver="$(claude --version 2>/dev/null | grep -m1 -oE '[0-9]+\.[0-9]+\.[0-9]+' | head -n1 || true)"
    fi
    if [[ -n "$ver" && -x "$versions_dir/$ver" ]]; then
        bin="$versions_dir/$ver"
    elif [[ -d "$versions_dir" ]]; then
        bin="$(find "$versions_dir" -maxdepth 1 -type f -name '[0-9]*' 2>/dev/null | sort -V | tail -n1 || true)"
    fi
fi

if [[ -z "$bin" || ! -f "$bin" ]]; then
    echo "skip: no stock Claude Code binary found (looked in $versions_dir; set \$CC_BIN)." >&2
    echo "      #466 finding of record stands on build/version-compat.md \`# Issue #466\`." >&2
    exit 0
fi

# perl drives assertions 3+4 (the scrub-shape and gating regexes); absent, it would
# otherwise misreport as a FAIL/DRIFT — a missing tool read as CC drift. Make the
# dependency explicit and skip cleanly, exactly like the binary-absent case above.
if ! command -v perl >/dev/null 2>&1; then
    echo "skip: perl not found — assertions 3+4 (scrub shape + gating) require it." >&2
    echo "      #466 finding of record stands on build/version-compat.md \`# Issue #466\`." >&2
    exit 0
fi
echo "stock binary : $bin"
echo "cc version   : $(basename "$bin")"
echo

fail=0
has() { grep -a -q -F -- "$1" "$bin"; }   # fixed-string presence in the binary

# --- 1 + 2: the scrub is still reached and still clears the shared item ----------
echo "== 1/2: invalid_grant scrub reached + clears the shared credential =="
if has 'tengu_oauth_refresh_token_marked_dead_invalid_grant'; then
    echo "  ok   marked-dead event present (invalid_grant → mark dead RT)"
else
    echo "  FAIL marked-dead event GONE — the invalid_grant path moved/changed; re-decode #466" >&2; fail=1
fi
if has 'tengu_oauth_refresh_token_cleared_on_disk'; then
    echo "  ok   cleared-on-disk event present (scrub writes the emptied item)"
else
    echo "  FAIL cleared-on-disk event GONE — the scrub write moved/changed; re-decode #466" >&2; fail=1
fi
echo

# --- 3: the scrub still empties BOTH tokens (the 2.1.207 both-cleared shape) -----
echo "== 3: scrub empties BOTH accessToken and refreshToken =="
if perl -0777 -ne 'exit(!(/claudeAiOauth:\{[^}]{0,160}refreshToken:""[^}]{0,40}accessToken:""/s))' "$bin" 2>/dev/null; then
    echo "  ok   claudeAiOauth scrub clears refreshToken:\"\" + accessToken:\"\""
else
    echo "  FAIL both-cleared scrub shape not found — the emptying changed shape; re-decode #466" >&2; fail=1
fi
echo

# --- 4: the scrub is still UN-GATED (no knob) -----------------------------------
# Anchor on the CODE occurrence of the scrub (NOT just the event name — that
# string also lives in the binary's pooled-string/atom table, where a
# process.env read structurally cannot appear, so anchoring on the name alone
# passes vacuously and never watches the scrub). The code site is the one place
# the marked-dead event is followed within a few hundred chars by the
# `claudeAiOauth:{...refreshToken:""` scrub. Take a window spanning from before
# the event through the scrub and assert it holds NO process.env read and NO
# feature-gate call — a hit is the canary: a knob (or gate) now wraps the scrub.
echo "== 4: scrub is un-gated (no process.env / feature-gate knob at the scrub site) =="
gate4="$(perl -0777 -ne '
    my $anchor="tengu_oauth_refresh_token_marked_dead_invalid_grant";
    my $found=0; my %hits;
    while(/\Q$anchor\E/g){
        my $ev_end=pos();
        # CODE occurrence only: the scrub mutate follows the event here; the
        # atom-table copy is NOT followed by claudeAiOauth:{...refreshToken:"".
        next unless substr($_,$ev_end,600)=~/claudeAiOauth:\{[^}]{0,160}refreshToken:""/;
        $found=1;
        my $start=$ev_end-length($anchor)-1400; $start=0 if $start<0;
        my $win=substr($_,$start,1400+length($anchor)+600);
        $hits{$_}=1 for $win=~/(process\.env\.[A-Z0-9_]+|checkGate\(|getFeatureGate\(|Statsig|featureFlag\()/g;
    }
    if(!$found){print "NO_SCRUB_SITE"} elsif(%hits){print join(",",sort keys %hits)} else{print "CLEAN"}
' "$bin" 2>/dev/null || true)"
case "$gate4" in
    CLEAN)         echo "  ok   no env/gate at the scrub site — scrub is unconditional (NO knob), as #466 records" ;;
    NO_SCRUB_SITE) echo "  FAIL could not locate the scrub CODE site to check gating — the scrub moved; re-decode #466" >&2; fail=1 ;;
    *)             echo "  FAIL a gate/env appeared at the scrub site: [$gate4] — a knob may now exist; RE-CHECK #466" >&2; fail=1 ;;
esac
echo

if [[ "$fail" -ne 0 ]]; then
    echo "DRIFT: Claude Code's invalid_grant scrub behaviour changed vs the #466 record." >&2
    echo "       Re-run the decode and update build/version-compat.md \`# Issue #466\`." >&2
    exit 1
fi
echo "ok: CC $(basename "$bin") matches the #466 record — invalid_grant scrub present, un-gated, no knob."
