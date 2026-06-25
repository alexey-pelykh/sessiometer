# Keychain swap-test guide (R1) — issue #16

The corrected, de-confounded runbook for the pre-build credential checks. It replaces the first-run
procedure (which moved several variables at once and was masked by a display-name collision — see
`version-compat.md` § Findings). Results go into **`version-compat.md`**; this file is the **how**.

Driven by **`build/h3-r1.sh`** so you can't repeat run 1's mistake of changing the keychain token
without the matching `~/.claude.json` `oauthAccount`.

> ⚠️ Mutates live Claude Code auth. Run on a dev Mac with **two accounts**. **Observe a SEPARATE
> `claude` you launch in its own terminal — never the session you're coordinating from.** Cleanup +
> resync at the end. The helper stores live-token snapshots under `~/.sessiometer-r1` (0600, outside the repo).

## The golden rule

**Tell accounts apart by `emailAddress` / `accountUuid` / `organizationName` — NEVER `displayName`.**
Both test accounts here display as "Oleksii" and differ only by email (`A = …consulting.fr`,
`B = …pelykh.com`). The `active` helper prints the unambiguous fields; in a live `claude`, read
`/status` for org/email, not the name.

## Setup

```sh
cd /path/to/sessiometer
source build/h3-r1.sh          # loads: snapshot_account / set_state / set_token / set_oauth / active / kc_locate / r1_status / r1_cleanup
r1_status                      # baseline: where the item lives + which account config resolves
```

`kc_locate` must show `…/login.keychain-db`. (Run 1 confirmed it does — if it ever shows a Data
Protection Keychain instead, STOP and record it; the swap design changes.)

## Step 0 — snapshot both accounts cleanly (consistent token+config pairs)

```sh
claude /login                  # as A (…consulting.fr)
snapshot_account A             # → ~/.sessiometer-r1/A.{cred,oauth.json}

claude /login                  # as B (…pelykh.com)
snapshot_account B             # → ~/.sessiometer-r1/B.{cred,oauth.json}
```

Now `set_state A` / `set_state B` can reconstruct either account's full state on demand.

---

## Test 1 — fresh-start adoption (baseline: does a security-written pair take at all?)

```sh
# claude NOT running:
set_state A ; active           # expect: A
claude                         # launch fresh, watch a watched terminal → /status = A?  then quit
set_state B ; active           # expect: B
claude                         # fresh → /status = B?  then quit
```

**Record (H3, fresh-start):** does a fresh `claude` honor the `set_state` account both times?
✅ yes ⇒ a consistent keychain+oauthAccount pair written by `security` is adopted on launch (so #6’s
swap works **across a restart**). ❌ no / re-auth ⇒ note exactly.

## Test 2 — running-session adoption  (watch-for **b**; the real #12 "mid-turn swap" question)

```sh
set_state A ; claude           # launch as A and LEAVE IT RUNNING; confirm /status = A
# …in a SECOND terminal (helper sourced there too):
set_state B                    # swap BOTH sides to B out-of-band
# …back in the still-running claude: issue a NEW request, then check /status
active                         # did config get rewritten? (claude may reconcile on its own)
```

**Record (H3, running):** on the new request, does the live session
**(a)** switch to B (re-reads per request) · **(b)** stay A (caches at startup — the swap engine must
restart/signal claude) · **(c)** prompt re-auth / error. Note any keychain prompt and `/status`.

## Test 3 — precedence on mismatch  (this **is** H2: keychain vs oauthAccount)

```sh
# 3a — token=B but config=A:
set_token B ; set_oauth A ; r1_status      # keychain←B, oauthAccount←A
claude                                      # fresh; watch: re-auth? error? /status account?
active                                      # AFTER it settles: did claude rewrite oauthAccount→B (keychain won) or keep A (oauthAccount won)?
# quit claude, then 3b — token=A but config=B:
set_token A ; set_oauth B ; r1_status
claude ; active
```

**Record (H2):** which side wins / whether it re-auths.
keychain-wins ⇒ #6’s `oauthAccount` co-write is **best-effort** · oauthAccount-wins ⇒ **atomic-critical**
· re-auth-loop or refusal ⇒ #6 must write **both atomically** (neither alone is valid).

## Test 4 — stash survives `/login`  (H1; the #4 capture primitive)

```sh
# stash A under our namespace, then log in as B, then read the stash back:
security add-generic-password -U -s 'Sessiometer/acct-A' -a "$(id -un)" \
  -w "$(cat ~/.sessiometer-r1/A.cred)" ~/Library/Keychains/login.keychain-db
claude /login                  # as B
security find-generic-password -w -s 'Sessiometer/acct-A' ~/Library/Keychains/login.keychain-db >/tmp/stash-rt 2>/dev/null; echo "exit=$?"
cmp -s /tmp/stash-rt ~/.sessiometer-r1/A.cred && echo "blob matches A ✅" || echo "blob differs ❌"; rm -f /tmp/stash-rt
```

**Record (H1):** `exit=0`, silent (no prompt), blob matches A ⇒ ✅ stash survives `/login` (so #4 can
stash token + oauthAccount per account safely).

## Test 5 — locked-keychain read code  (H0, advisory; the #13 edge case)

```sh
security lock-keychain ~/Library/Keychains/login.keychain-db
security find-generic-password -w -s 'Sessiometer/acct-A' ~/Library/Keychains/login.keychain-db >/dev/null 2>&1; echo "exit=$?"
security unlock-keychain ~/Library/Keychains/login.keychain-db
```

**Record (H0):** actual exit code (expected **36** = `errSecInteractionNotAllowed`). A prompt instead of a clean failure is itself the finding.

---

## Restore + cleanup

```sh
security delete-generic-password -s 'Sessiometer/acct-A' ~/Library/Keychains/login.keychain-db 2>/dev/null
r1_cleanup                     # deletes ~/.sessiometer-r1 (live-token snapshots + the config backup)
claude /login                  # as your PRIMARY (…consulting.fr) — resyncs token + oauthAccount to a clean state
r1_status                      # sanity: config shows your primary again
```

## Report back

Paste the **Record** line from each test (or just the terminal output + what you saw in the watched
`claude` session). I’ll write the verdicts into `version-compat.md`, propagate H3/H2 to #6 & #14,
then finish the PR.
