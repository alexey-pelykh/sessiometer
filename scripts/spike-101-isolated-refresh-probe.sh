#!/usr/bin/env bash
# Reproduce the SAFE empirical probes from the issue #101 spike (the knowledge
# gate for the isolated-CLAUDE_CONFIG_DIR refresh engine, #102). Findings of
# record live in build/version-compat.md `# Issue #101`; this script lets a
# reviewer / the #102 author re-run the live half and seeds #102's integration
# test.
#
# SAFETY — this script NEVER touches a real credential:
#   * isolated CLAUDE_CONFIG_DIR under <repo>/.tmp (the suffixed keychain service
#     it derives is unique to that throwaway dir; it is NEVER the bare
#     `Claude Code-credentials` the live session uses, nor a `Sessiometer/<uuid>`
#     stash);
#   * the seeded credential is a literal FAKE non-secret blob;
#   * `expiresAt` is far-future, so CC's 5-minute refresh predicate is false and
#     NO OAuth network call is made (no refresh-token exchange, no rotation);
#   * a trap removes the throwaway keychain item + dir on every exit path.
# It is a MANUAL diagnostic (needs a live Claude Code + login keychain) and is
# deliberately NOT wired into CI. macOS only.
#
# What it checks:
#   AC-4  service-name derivation = sha256(NFC(raw CLAUDE_CONFIG_DIR))[:8] (#100)
#   AC-5  headless `claude -p` shows no onboarding/trust prompt; CC auto-writes
#         a minimal .claude.json
#   AC-1/4 CC reads the seeded item AT the suffixed service name (401 on the fake
#         token, vs "Not logged in" with no item)
#   AC-2  apple-tool: `security -w` read-back of the (CC-touched) item is silent
set -euo pipefail

if [[ "$(uname -s)" != "Darwin" ]]; then
    echo "skip: macOS only (keychain + Claude Code)." >&2
    exit 0
fi

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
claude_bin="${CLAUDE_BIN:-$(command -v claude || true)}"
keychain="$HOME/Library/Keychains/login.keychain-db"
acct="$(id -un)"               # CC's `acct` is the macOS username, never the account
iso_dir="$repo_root/.tmp/spike-101-probe.$$"
base="Claude Code-credentials" # the bare service the LIVE session uses — must never be our target

suffix="$(printf '%s' "$iso_dir" | shasum -a 256 | cut -c1-8)"
service="$base-$suffix"

# Defensive: our target must be the suffixed item, never the bare canonical.
if [[ "$service" == "$base" ]]; then
    echo "error: refusing to operate on the bare canonical service." >&2
    exit 1
fi

cleanup() {
    security delete-generic-password -a "$acct" -s "$service" "$keychain" >/dev/null 2>&1 || true
    rm -rf "$iso_dir"
}
trap cleanup EXIT

mkdir -p "$iso_dir"
echo "isolated CLAUDE_CONFIG_DIR : $iso_dir"
echo "derived service (n1)       : $service"
echo "acct (vO = macOS username) : $acct"
echo

# --- AC-4: derivation cross-check against the #100 pinned vectors ----------------
echo "== AC-4: sha256(NFC(raw value))[:8], no path expansion =="
for pair in "/abs/path 6d80187b" "/opt/cc 34fd9c6e"; do
    p="${pair% *}"; want="${pair#* }"
    got="$(printf '%s' "$p" | shasum -a 256 | cut -c1-8)"
    if [[ "$got" == "$want" ]]; then
        echo "  ok   $p -> $base-$got"
    else
        echo "  FAIL $p -> $got (want $want)" >&2
        exit 1
    fi
done
echo

if [[ -z "$claude_bin" ]]; then
    echo "note: no \`claude\` on PATH (set CLAUDE_BIN) — skipping the live half." >&2
    echo "      The derivation (AC-4) above stands on its own."
    exit 0
fi
echo "claude binary              : $claude_bin"
echo "  (a wrapper may exec a patched copy; the stock binary under"
echo "   ~/.local/share/claude/versions/<v> is the credential-logic reference.)"
echo

run_claude() {
    env -u CLAUDE_CODE_OAUTH_TOKEN -u ANTHROPIC_API_KEY \
        CLAUDE_CONFIG_DIR="$iso_dir" DISABLE_AUTOUPDATER=1 DISABLE_TELEMETRY=1 \
        DISABLE_ERROR_REPORTING=1 DISABLE_BUG_COMMAND=1 \
        timeout 40 "$claude_bin" -p "say pong" </dev/null 2>&1 || true
}

# --- AC-5: headless run with NO credential — no onboarding prompt -----------------
echo "== AC-5: headless \`claude -p\` in an empty isolated dir =="
out_empty="$(run_claude)"
echo "  output: ${out_empty:-<empty>}"
if [[ -f "$iso_dir/.claude.json" ]]; then
    bytes="$(wc -c <"$iso_dir/.claude.json" | tr -d ' ')"
    echo "  ok   CC auto-wrote a minimal .claude.json ($bytes bytes) — no onboarding/trust keys needed"
fi
echo

# --- AC-1/4/2: seed a FAKE item at the suffixed service, far-future expiry -------
echo "== AC-1/AC-4/AC-2: seed a FAKE far-future credential at the suffixed item =="
blob='{"claudeAiOauth":{"accessToken":"FAKE-AT","refreshToken":"FAKE-RT","expiresAt":9999999999999,"scopes":["user:inference"]}}'
security add-generic-password -U -a "$acct" -s "$service" -w "$blob" "$keychain"
out_seeded="$(run_claude)"
echo "  output: ${out_seeded:-<empty>}"
if printf '%s' "$out_seeded" | grep -qi '401\|bearer\|authenticate'; then
    echo "  ok   AC-1/AC-4: CC read+used the seeded item at the suffixed service (token rejected, not 'Not logged in')"
else
    echo "  note AC-1/AC-4: expected a token-rejected error; got the above (CC behaviour/version may differ)"
fi
# AC-2: apple-tool: read-back must be silent (exit 0) after CC has touched the item.
if security find-generic-password -a "$acct" -s "$service" -w "$keychain" >/dev/null 2>&1; then
    echo "  ok   AC-2: apple-tool: \`security -w\` read-back is SILENT (exit 0) — no partition re-stamp"
else
    echo "  FAIL AC-2: read-back did not return cleanly (exit $?)" >&2
    exit 1
fi

echo
echo "done. (throwaway item + dir removed on exit; login keychain unchanged.)"
