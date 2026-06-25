#!/usr/bin/env bash
# h3-r1.sh — de-confounded keychain ⇄ oauthAccount swap-test helper for issue #16.
#
#   SOURCE it (the functions must run in YOUR interactive shell — zsh or bash):
#       source build/h3-r1.sh
#   Running it as `./build/h3-r1.sh` does nothing useful (functions vanish on exit).
#
# Snapshots hold LIVE OAuth tokens. They are written 0600 under ~/.sessiometer-r1
# (OUTSIDE the repo, never committed) and removed by `r1_cleanup`. Run on a dev Mac with
# two accounts and observe a SEPARATE `claude` you launch yourself — never the session
# you are coordinating from. See build/keychain-swap-guide.md for the procedure.

R1_DIR="${R1_DIR:-$HOME/.sessiometer-r1}"
R1_SVC='Claude Code-credentials'
R1_KC="$HOME/Library/Keychains/login.keychain-db"
R1_JSON="$HOME/.claude.json"

# account claude RESOLVES from config — keyed by EMAIL+uuid+org, never the ambiguous displayName
active() {
  python3 - "$R1_JSON" <<'PY'
import json, sys
try:
    a = (json.load(open(sys.argv[1])).get("oauthAccount") or {})
    print("oauthAccount =", a.get("emailAddress", "<none>"),
          "·", (a.get("accountUuid") or "")[:8],
          "· org:", a.get("organizationName", "<none>"))
except Exception as e:
    print("oauthAccount = <error>", e)
PY
}

# read-only: which keychain holds the item + its acct / created / modified stamps
kc_locate() {
  security find-generic-password -s "$R1_SVC" 2>&1 | grep -E 'keychain:|"acct"|"svce"|"cdat"|"mdat"'
}

# one-shot snapshot of the full current state (keychain side + config side)
r1_status() { echo "— keychain —"; kc_locate; echo "— config —"; active; }

# snapshot_account <label> : capture the CURRENT token blob + oauthAccount block.
# Run this immediately after `claude /login` as the account you want labelled.
snapshot_account() {
  L="$1"; [ -z "$L" ] && { echo "usage: snapshot_account <label>"; return 2; }
  ( umask 077; mkdir -p "$R1_DIR" )
  security find-generic-password -w -s "$R1_SVC" > "$R1_DIR/$L.cred" 2>/dev/null \
    || { echo "FAIL: cannot read '$R1_SVC' (keychain locked / item absent?)"; rm -f "$R1_DIR/$L.cred"; return 1; }
  python3 - "$R1_JSON" "$R1_DIR/$L.oauth.json" <<'PY'
import json, sys
a = (json.load(open(sys.argv[1])).get("oauthAccount") or {})
json.dump(a, open(sys.argv[2], "w"), indent=2, ensure_ascii=False)
print("snapshot:", a.get("emailAddress", "<none>"), (a.get("accountUuid") or "")[:8])
PY
  chmod 600 "$R1_DIR/$L".* 2>/dev/null
  echo "saved '$L' → $R1_DIR/$L.{cred,oauth.json}"
}

# set_token <label> : write ONLY the keychain token (leaves oauthAccount untouched) — for mismatch tests
set_token() {
  L="$1"; f="$R1_DIR/$L.cred"
  [ -f "$f" ] || { echo "no snapshot '$L' (run snapshot_account $L first)"; return 1; }
  security add-generic-password -U -s "$R1_SVC" -a "$(id -un)" -w "$(cat "$f")" "$R1_KC" \
    && echo "keychain token ← $L"
}

# set_oauth <label> : write ONLY ~/.claude.json oauthAccount (leaves the keychain untouched) — for mismatch tests
set_oauth() {
  L="$1"; f="$R1_DIR/$L.oauth.json"
  [ -f "$f" ] || { echo "no snapshot '$L'"; return 1; }
  ( umask 077; mkdir -p "$R1_DIR" ); [ -f "$R1_DIR/claude.json.orig" ] || cp "$R1_JSON" "$R1_DIR/claude.json.orig"
  python3 - "$R1_JSON" "$f" <<'PY'
import json, sys
p, src = sys.argv[1], sys.argv[2]
d = json.load(open(p)); d["oauthAccount"] = json.load(open(src))
json.dump(d, open(p, "w"), indent=2, ensure_ascii=False)
print("oauthAccount ←", d["oauthAccount"].get("emailAddress"))
PY
}

# set_state <label> : co-write BOTH sides consistently — this is what swap engine #6 will do
set_state() { set_token "$1" && set_oauth "$1"; }

# delete the snapshot dir (live secrets). Does NOT fix your auth — resync with: claude /login
r1_cleanup() { rm -rf "$R1_DIR" && echo "removed $R1_DIR — now run: claude /login (as your primary) to resync"; }

echo "R1 helper loaded · snapshots dir: $R1_DIR"
echo "  snapshot_account <L> · set_state <L> · set_token <L> · set_oauth <L> · active · kc_locate · r1_status · r1_cleanup"
echo "  (source me; observe a SEPARATE claude you launch — see build/keychain-swap-guide.md)"
