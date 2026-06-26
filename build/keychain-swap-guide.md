# Keychain swap-test guide (R1) — issue #16

The de-confounded runbook for the pre-build credential checks. Results go into
**[`version-compat.md`](./version-compat.md)** (the acceptance record); this file is the **how**, and
it is driven by **[`h3-r1.sh`](./h3-r1.sh)** so you can't repeat run 1's mistake of changing the
keychain token without the matching `~/.claude.json` `oauthAccount`.

> ⚠️ Mutates live Claude Code auth. Run on a dev Mac with **two accounts** (here **A** = primary,
> **B** = secondary). **Observe a SEPARATE `claude` you launch in its own terminal — never the session
> you're coordinating from.** Cleanup + resync at the end. The helper stores live-token snapshots
> under `~/.sessiometer-r1` (0600, outside the repo).

## The golden rule

**Tell accounts apart by `emailAddress` / `accountUuid` / `organizationName` — NEVER `displayName`.**
Distinct accounts can share a display name; the `active` helper prints the unambiguous fields, and in a
live `claude` you read `/status` for org/email, not the name.

## Setup

```sh
cd /path/to/sessiometer
source build/h3-r1.sh          # loads: snapshot_account / set_state / set_token / set_oauth / active / kc_locate / r1_status / r1_cleanup
r1_status                      # baseline: where the item lives + which account config resolves
```

`kc_locate` must show `…/login.keychain-db`. (Confirmed it does — if it ever shows a Data Protection
Keychain instead, STOP and record it; the swap design changes.)

## Step 0 — snapshot both accounts cleanly (consistent token+config pairs)

```sh
claude /login                  # as A
snapshot_account A             # → ~/.sessiometer-r1/A.{cred,oauth.json}
claude /login                  # as B
snapshot_account B             # → ~/.sessiometer-r1/B.{cred,oauth.json}
```

Now `set_state A` / `set_state B` can reconstruct either account's full state on demand.

---

## Test 1 — fresh-start adoption  (does a CLI-written pair take at startup?)

```sh
set_state A ; active           # expect: A
claude                         # fresh launch in a watched terminal → /status email/org = A?  then quit
set_state B ; active           # expect: B
claude                         # fresh → /status = B?  then quit
```

**Record (H3 fresh-start):** does a fresh `claude` honor the `set_state` account both times (by
email/org)? ✅ yes ⇒ a `security`-written token+`oauthAccount` pair is adopted on launch (#6’s swap
works across a restart). ❌ / re-auth ⇒ note exactly.

## Test 2 — running-session adoption  (#12 mid-turn swap)

```sh
set_state A ; claude           # launch as A and LEAVE IT RUNNING; confirm /status = A
# …in a SECOND terminal (helper sourced there too):
set_state B                    # swap BOTH sides to B out-of-band
# …back in the still-running claude: issue a NEW request, then /status
```

**Record (H3 running):** does the live session reflect B with no restart?
**(a)** switches to B (re-reads) · **(b)** stays A until restart (caches) · **(c)** re-auth/error.

> **Display confound:** `/status` re-reads the config file, so it can show B even if requests still use
> the cached old token. To judge *request* routing (not display), use the token-validity test below.

### Optional: garbage-token confirm  (settles request-routing without the display confound)

```sh
set_state A ; claude           # running as A
# …second terminal: replace ONLY the token with garbage, leave config = A:
security add-generic-password -U -s 'Claude Code-credentials' -a "$(id -un)" -w 'not-a-real-token' ~/Library/Keychains/login.keychain-db
# …in the running claude: issue a NEW request.
```

**Record:** request **fails (401/auth error)** ⇒ the running session re-reads the token per request →
**hot-swap viable**. Request **succeeds** ⇒ the token was cached at startup → **swap needs a restart**.
(Restore afterward with `set_state A`.)

## Test 3 — precedence on mismatch  (H2: keychain token vs `oauthAccount`)

```sh
set_token B ; set_oauth A ; r1_status      # token=B, config=A
claude ; active                             # fresh; watch: re-auth? which account? did config get rewritten?
set_token A ; set_oauth B ; r1_status      # token=A, config=B
claude ; active
```

**Record (H2):** does it re-auth, and does `/status` follow the token or the config? (Run of record:
config drives display, token drives auth, mismatch tolerated — they're orthogonal.)

## Test 4 — stash survives `/login`  (H1; the #4 capture primitive)

```sh
security add-generic-password -U -s 'Sessiometer/acct-A' -a "$(id -un)" \
  -w "$(cat ~/.sessiometer-r1/A.cred)" ~/Library/Keychains/login.keychain-db
claude /login                  # as B
security find-generic-password -w -s 'Sessiometer/acct-A' ~/Library/Keychains/login.keychain-db >/tmp/stash-rt 2>/dev/null; echo "exit=$?"
cmp -s /tmp/stash-rt ~/.sessiometer-r1/A.cred && echo "blob matches A ✅" || echo "blob differs ❌"; rm -f /tmp/stash-rt
```

**Record (H1):** `exit=0`, silent, blob matches A ⇒ ✅ stash survives `/login`.

## Test 5 — locked-keychain read code  (H0, advisory; #13)

```sh
security lock-keychain ~/Library/Keychains/login.keychain-db
security find-generic-password -w -s 'Sessiometer/acct-A' ~/Library/Keychains/login.keychain-db >/dev/null 2>&1; echo "exit=$?"
security unlock-keychain ~/Library/Keychains/login.keychain-db
```

**Record (H0):** exit code, and whether a UI prompt appeared. In an interactive TTY the CLI is allowed
to prompt for unlock (so you get a prompt + `exit=0`, not `36`); a non-interactive daemon is the context
that yields `36`. #13 must read non-interactively / pre-check lock state, never raise a prompt.

---

## Restore + cleanup

```sh
security delete-generic-password -s 'Sessiometer/acct-A' ~/Library/Keychains/login.keychain-db 2>/dev/null
r1_cleanup                     # deletes ~/.sessiometer-r1 (live-token snapshots + the config backup)
claude /login                  # as your PRIMARY — resyncs token + oauthAccount to a clean state
r1_status                      # sanity: config shows your primary again
```

## Report back

Paste each test's **Record** line (or the terminal output + what you saw in the watched `claude`).
The verdicts land in [`version-compat.md`](./version-compat.md).
