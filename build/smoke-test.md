# Manual end-to-end smoke test — the 0.1.0 "done-when"

The **manual** acceptance procedure for issue
[#14](https://github.com/alexey-pelykh/sessiometer/issues/14): the same end-to-end behaviour the
hermetic test asserts, run once against **real** Claude Code accounts to confirm the loop works
against the live keychain, the live usage API, and a real `~/.claude.json`.

The **automated** counterpart runs on every CI build and burns no quota — it drives the whole loop
through the `UsageSource` / `CredentialStore` / `Clock` seams with injected values:

> `src/daemon.rs` → `e2e_acceptance_full_loop_swaps_propagates_and_reconciles_without_oscillation_or_leak`

This document is the human-run complement; it is **documented, not required to pass CI**. Run it once
per environment when you want real-account assurance (e.g. before a release, or after a macOS / Claude
Code upgrade).

## Gate: the #16 pre-build checks must have passed

This smoke test assumes the credential mechanism itself is sound — the one-time empirical verification
recorded in [`build/version-compat.md`](version-compat.md) (issue
[#16](https://github.com/alexey-pelykh/sessiometer/issues/16), checks **H0–H3**). That gate is
**satisfied** (H1/H2/H3 PASS/RESOLVED; H0 caveat is the non-interactive-read design, handled by #13).
If you are on a materially newer macOS or Claude Code build than the version-compat ledger records,
re-confirm H3 (fresh-start adoption) there first — a regression in credential adoption would surface
here as a swap that "succeeds" but leaves Claude Code unable to authenticate.

## What it confirms (the acceptance criteria)

| Done-when (issue #14) | Observation in this procedure |
|---|---|
| Auto-swaps **at the trigger**, no manual step | Step 4 — a swap event appears with no operator action |
| Lands on a **viable** target | Step 4 — `status` shows the swapped-to account active; it is not an exhausted one |
| **No oscillation** (cooldown respected) | Step 5 — no second swap inside the cooldown window |
| Each event **surfaced** | Steps 4–6 — `sessiometer.log` + `sessiometer status` reflect every transition |
| Crash-mid-swap / drift **reconciles** on start | Step 7 — a deliberately stale `~/.claude.json` is healed at the next `run` |
| **No secret** on any channel | Step 8 — `status`, the log, and any error output carry handles + percentages only |

## The synthetic-threshold trick — force a swap without exhausting an account

Waiting for a real account to hit ~95% session usage would burn most of a quota window. Instead, lower
the **swap-away trigger** so a *lightly*-used account already sits "over" it: the daemon then swaps on
the first poll, exercising the entire real path (poll → decide → out-of-band swap → propagate) on
accounts that still have plenty of quota left. This is the real-account analogue of the injected
utilization values the hermetic test uses.

Config lives at `~/Library/Application Support/sessiometer/config.toml` (or
`$XDG_CONFIG_HOME/sessiometer/config.toml`). Set, for the duration of the test:

```toml
poll_secs       = 5     # poll briskly so the run is quick (min 5)
session_trigger = 50    # the minimum: a half-used session already trips a swap-away
cooldown_secs   = 60    # a window long enough to watch the no-oscillation hold
# leave session_floor commented out (the #10 default: cooldown alone bounds oscillation)
```

Restore your normal values when finished (Step 9).

## Prerequisites

- A macOS login keychain with a current `Claude Code-credentials` item (you are signed in to Claude
  Code), per the README **Prerequisites**.
- **Two** accounts captured into the rotation, both with real remaining quota (so each is a viable
  target for the other). Sign in to the first account, `sessiometer capture`, then sign in to the
  second and `sessiometer capture` again.
- A terminal you can leave the foreground daemon running in, plus a second terminal for `status`.

## Procedure

1. **Confirm the roster.** `sessiometer list` shows both accounts with their labels (referred to below
   as **A** and **B**), neither marked `disabled`.

2. **Apply the synthetic threshold.** Edit `config.toml` as above (`session_trigger = 50`,
   `poll_secs = 5`, `cooldown_secs = 60`).

3. **Start the daemon (foreground).** `sessiometer run` — or `sessiometer run -v` to also watch the
   per-cycle diagnostic channel (issue #77) on stderr: a `diag=start` config summary, then each poll's
   outcome and the per-tick decision. It reconciles on start, then begins polling.

4. **Observe the swap (threshold → viable → swap → propagate).** Within a poll interval or two, with
   the active account's real session usage ≥ 50%, the daemon swaps to the other account. Confirm, with
   **no manual step**:
   - `sessiometer status` (second terminal) now shows the **other** account active. (The `next swap:`
     footer is forward-looking — it names the *next* candidate, not the swap that just happened; the
     swap itself is confirmed by the event log below.)
   - `~/Library/Logs/sessiometer/sessiometer.log` has an `event=swap from=… to=…` line.
   - Claude Code itself continues working as the swapped-to account (the canonical credential and
     `~/.claude.json` both moved — propagation).
   - If the active account's session is below 50%, use it briefly (any Claude Code request) until it
     crosses, or lower `session_trigger` further toward your actual usage.

5. **Observe no oscillation.** Keep watching for the length of `cooldown_secs` (60 s). Even though the
   now-active account may also be over the trigger, **no second swap occurs inside the window** — the
   log shows no new `event=swap`, and `status` keeps the same active account. Past the window a
   swap-back is allowed (bounded, not frozen) — the real-account B→A→B cycle.

6. **Confirm each event surfaced.** Cross-check `sessiometer.log` against `sessiometer status`: every
   swap is both logged and reflected live. Quota-rejection events (a `401`/`403`, a locked keychain)
   would likewise each log one line — you need not provoke these here; the hermetic test covers them.

7. **Confirm reconcile-on-start (canonical≠oauth).** Stop the daemon (`Ctrl-C`). Hand-edit
   `~/.claude.json` so its `oauthAccount.accountUuid` shows the **other** account than the one the
   keychain currently holds (simulating a crash mid-swap). Run `sessiometer run` again: on start it
   **heals** the display back to the account the canonical credential actually holds. Confirm with
   `sessiometer status` / by re-reading `~/.claude.json`.

8. **Confirm no secret leaks.** Skim every operator surface — `sessiometer status`, the full
   `sessiometer.log`, the `-v` diagnostic channel on stderr (issue #77), and any error output (e.g. run
   a bogus `sessiometer status` with the daemon stopped). They carry account **labels**, **percentages**,
   and relative ages only — **never** a token (`sk-ant-…`), a credential blob, or an account email.
   (This is exactly what the redaction METER asserts mechanically over the same channels in CI, issues
   #15 and #77.) On an interactive terminal `status` color-codes each row by urgency (green/yellow/red,
   issue #73); the color only **augments** the same non-secret text and adds nothing but ANSI escapes —
   pipe it (`sessiometer status | cat -v`) to confirm the escapes vanish and not a single secret appears.

9. **Teardown.** `Ctrl-C` the daemon. Restore `config.toml` to your normal `session_trigger` /
   `poll_secs` / `cooldown_secs`. The rotation and both stashes are unchanged.

## Pass criteria

The smoke test **passes** when Steps 4–8 are all observed: an unattended swap at the lowered trigger
onto a viable account with the credential propagated to both the keychain and `~/.claude.json`; no
second swap inside the cooldown window; every transition visible in both the log and live `status`; the
stale `~/.claude.json` healed on the next start; and no token / blob / email on any channel. Any
deviation is a regression — capture the `sessiometer.log` excerpt and the `status` output and file it
against the relevant subsystem issue.
