# sessiometer

Manage multiple Claude Code accounts on macOS. `sessiometer` polls each
account's usage quota and swaps the active credential out-of-band before an
account is exhausted, so a long session keeps running by rotating across
accounts.

> **Status:** early scaffold (`0.1.0`, first workable slice). The subsystems
> behind the CLI are still being implemented — see the
> [open issues](https://github.com/alexey-pelykh/sessiometer/issues).

## Responsibilities

`sessiometer` operates on credentials for provider accounts that you own and
configure. You are responsible for complying with each provider's terms —
including the Terms of Service that govern the accounts you configure with
`sessiometer`. Review those terms and make sure your own use of those
accounts is permitted under them.

## Prerequisites

- **macOS**, using the **login keychain**.
- A Claude Code credential already present in the login keychain — the
  generic-password item whose service is `Claude Code-credentials` (created when
  you sign in to Claude Code). `sessiometer` reads and rewrites this item in
  place through the `/usr/bin/security` CLI; it never uses the
  Security.framework SDK (a CI guard enforces this, so the original silent-read
  access is preserved).

## Quickstart

```sh
# 1. Capture each account's credential. Sign in to the account in Claude Code,
#    then stash its current credential:
sessiometer capture

# 2. Run the foreground daemon. It polls usage and swaps the active credential
#    to the next account before the current one is exhausted:
sessiometer run

# 3. Check the roster and the next swap candidate at any time:
sessiometer status
```

## Checking status

`sessiometer status` queries the running daemon and prints each account as one
row of an aligned, border-less table — greppable, one record per line:

```text
ACCOUNT  SESSION  WEEKLY  RESETS  STATUS
* work   97%      40%     4h
  spare  10%      20%     1h12m
  dead   n/a      n/a     n/a     needs re-login

next swap: spare
```

- `*` marks the **active** account.
- `SESSION` / `WEEKLY` are the last-polled usage percentages (`n/a` when the last
  poll for that account failed — never a fabricated `0`).
- **`RESETS`** is the compact time until the account next regains capacity —
  shown for **every** account, not only an exhausted one. Normally this is the
  rolling 5-hour **session** window's reset (e.g. `12m`, `4h`); when an account's
  **weekly** window is exhausted it is blocked for longer, so `RESETS` shows the
  **weekly** reset instead (e.g. `3d4h`). `n/a` when the governing reset is
  unknown.
- `STATUS` carries inline tags — `disabled` (parked, issue #36) and
  `needs re-login` (a dead credential, issue #42); the column is omitted when no
  account carries a tag.

The **`next swap:`** footer names the account the daemon would rotate to next — the
viable target whose weekly quota resets soonest. It reads `none (no viable target)`
when every other account is weekly-exhausted (or over the opt-in swap-target session
floor), and `none (awaiting usage data)` right after the daemon starts, before it has
polled the other accounts. It is **forward-looking** and recomputed every cycle, so —
unlike a remembered "last swap" — it survives a daemon restart and always shows where
the next rotation will land.

On a terminal too narrow for the full table the lowest-priority columns drop in
order — `WEEKLY` first, then `STATUS` — never wrapping a row; `ACCOUNT`,
`SESSION`, and `RESETS` are always kept. Output that is piped or redirected (not
a TTY) always keeps the full table, so `sessiometer status | grep work` stays
complete.

On an interactive terminal each account row is **color-coded by urgency** —
**green** (healthy, plenty of quota), **yellow** (getting depleted, or heavily
used but about to reset), **red** (heavily used and not about to reset). The
color reflects both how *much* is used and how *soon* the account resets; it
**augments** the row — every percentage and `RESETS` value is fully readable
without it — and is never the only signal. Color is emitted **only** on an
interactive TTY: it is suppressed when output is piped or redirected, when
`--no-color` is passed, or when `NO_COLOR`, `CLICOLOR=0`, or `TERM=dumb` is set
in the environment — so an escape sequence never reaches a pipe, a redirect, or a
log.

For the full data regardless of terminal width — both reset instants as raw
epoch seconds, for scripting — use `--json`:

```sh
sessiometer status --json | jq '.accounts[] | {label, session_resets_at}'
```

The output is sourced solely from non-secret fields (labels, percentages, reset
instants, a next-swap candidate label), so it never prints a token or email (issue #15).

## Watching the daemon (diagnostics)

`run` writes to two operator-facing channels, neither of which ever carries a
token or email (issue #15):

- **The event log** — durable, edge-triggered STATE CHANGES (a swap, a re-stash, a
  dead credential, entering the all-exhausted state, …), one `key=val` line each,
  appended to `~/Library/Logs/sessiometer/sessiometer.log` (surfaced in Console.app).
  Always on.
- **The diagnostic channel** — per-cycle DETAIL for debugging a live `run`, on
  **stderr**, **off by default**.

Pass `-v` (or `--verbose`) to opt into the diagnostic channel:

```sh
sessiometer run -v
```

It then prints, every cycle, the outcome of each account's poll — including the
`rate_limited` / `transient` outcomes the event log records no event for — the
per-tick decision and any back-off, plus the daemon's start (with the effective
config), its stop, and the moment it **leaves** the all-exhausted state:

```text
ts=2026-06-30T00:00:00Z diag=start accounts=2 poll_secs=30 session_floor=off session_trigger=90 weekly_trigger=98 monitor_401_n=5 monitor_recovery_m=4
ts=2026-06-30T00:00:00Z diag=poll account=work outcome=rate_limited
ts=2026-06-30T00:00:00Z diag=tick decision=skip_active_unavailable backoff_secs=120
ts=2026-06-30T00:00:30Z diag=poll account=work outcome=live
ts=2026-06-30T00:00:30Z diag=tick decision=hold
```

Both channels carry handles, enums, percentages, and timestamps only — and a CI
redaction meter scans every rendered line of each (issues #9, #15, #77).

## Switching the active account

Switch the active account **on demand**, without waiting for the daemon to swap
on a usage trigger — the same out-of-band swap, run once by you:

```sh
# Switch to `spare` now (resolves by list label OR account-uuid):
sessiometer use spare

# Force the switch, overriding the pre-swap checks below:
sessiometer use spare --force
```

By default `use` runs a **pre-swap gate** and refuses — with a specific reason
and **without writing anything** — when the target is not a sound destination:
its weekly window is exhausted, it is quarantined and needs a re-login, or a swap
cooldown is still active. Switching to the account that is **already active** is a
no-op success. Each refusal exits with its own status code, so a script can tell
them apart.

`--force` overrides those **policy** checks (and warns when you force onto an
exhausted or quarantined account), but it never bypasses **safety**: if the login
keychain is locked the switch still aborts at once, writing nothing. `use` works
whether or not the daemon is running: when one is up, the pre-swap gate reads the
**cached** usage the daemon already polled — so `use` makes no usage request of its
own and won't trip a rate limit — and with no daemon it falls back to a single live
check.

## Parking an account

Take an account out of the rotation without losing its captured credential — a
reversible **park**, distinct from removing it. A disabled account keeps its
roster entry and its stash, but the daemon never swaps **to** it and does not
poll it:

```sh
# Take `work` out of the rotation (kept, but skipped):
sessiometer disable work

# Return it to the candidate pool:
sessiometer enable work
```

Accounts resolve by their `list` label. The state is stored in `config.toml`, so
it persists across daemon restarts; `list` and `status` mark a parked account as
`disabled`. The change takes effect at the next daemon start (a running daemon
loads the roster once).

## Removing an account

Delete an account from the rotation **and erase its stashed credential** — the
destructive counterpart to `disable`. Where parking keeps the entry and its
stash, removal drops the roster entry and deletes the account's keychain stash,
so it is gone for good:

```sh
# Drop `work` from the rotation and erase its stash:
sessiometer remove work
```

Accounts resolve by their `list` label. The roster entry is removed from
`config.toml` **first**, then the stash is deleted — so an interrupted removal
leaves at most an unreferenced (harmless) keychain item, never a roster entry
pointing at a missing stash. The change takes effect at the next daemon start.

Removing the **active** account is allowed: it touches only `sessiometer`'s
roster entry and stash, never the live `Claude Code-credentials` item, so the
running Claude Code session keeps working. The daemon then simply resolves no
active account (polling only, never swapping) until you `capture` another account
or sign in again.

## Edge cases & resilience

`sessiometer` is built to ride out the keychain and credential edge cases a
long-running rotation hits:

- **Locked keychain.** While the login keychain is locked, the daemon cannot read
  the canonical credential, so it **defers** polling and swapping and **backs
  off** — the wait between retries grows to at most ~60 s — logging the wait once.
  It never tries to unlock the keychain or prompt for a password; unlock it
  yourself and the daemon resumes on its next retry.
- **Rate-limiting and transient errors back off.** When the usage endpoint returns
  `429` (rate-limited) or a `5xx` / network error, the daemon **widens its poll
  spacing** instead of re-polling at the fixed interval — an exponential back-off
  that doubles each consecutive throttled cycle (capped at ~1 h) and honours any
  `Retry-After` the server sends as a minimum wait; a clean poll resets it. The
  default cadence also carries normal jitter so concurrent accounts decorrelate,
  and on start-up the daemon waits a small jittered delay before its first poll so
  repeated restarts don't synchronise a burst of requests.
- **Re-authentication is picked up automatically.** If you `claude /login` an
  account again (refreshing its token, or switching the active account), the
  daemon detects the changed canonical credential and **re-stashes** the affected
  account, so the rotation always tracks the live token rather than a stale one.
- **Crash mid-swap self-heals.** A swap writes the credential before updating the
  display, and the daemon reconciles the two on its next start — so a process
  death partway through a swap leaves the keychain authoritative and is repaired
  automatically when you run it again.
- **Concurrent swap + re-login race (known limitation).** If you run
  `claude /login` at the exact moment the daemon is mid-swap, the two writers race
  on the canonical credential. Last-writer-wins, and the daemon reconciles on its
  next start (the keychain is authoritative); in the worst case one swap is
  effectively undone by the concurrent login and re-running heals the state. This
  is an accepted `0.1.0` limitation.

These behaviours, and the full threshold → swap → propagate loop, are verified
end-to-end: a hermetic acceptance test runs on every CI build (driving the loop
through injected usage / credential / clock seams, no real quota), and a
documented manual smoke test against real accounts —
[`build/smoke-test.md`](build/smoke-test.md) — is the human-run complement.

## Roster size and poll cost

There is **no fixed limit** on how many accounts the roster holds — capture as
many as you want to rotate across. Be aware of the cost, though: the daemon polls
each account independently, issuing **one `curl` usage request per roster account
every `poll_secs`**. Per-tick work and outbound request volume therefore grow
linearly with the roster size. `sessiometer` enforces no ceiling — size the
roster to what your usage warrants, and if request volume becomes a concern,
raise `poll_secs` or keep the roster smaller by choice.

## Build from source

```sh
cargo build --release
./target/release/sessiometer --help
```

## License

[MIT](LICENSE) © 2026 Oleksii PELYKH
