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
row of an aligned, border-less table under a labelled header — greppable, one
record per line:

```text
ACCOUNT  SESSION% RESET  WEEKLY% RESET  AUTH
* work   97%      12m    40%     5d     🟢
  spare  10%      2h     20%     3d     🟢
  dead   n/a      n/a    n/a     n/a    🔴 claude /login

next swap: spare
```

- A **header row** labels the columns: `ACCOUNT`, then the grouped `SESSION%` +
  `RESET`, then the grouped `WEEKLY%` + `RESET`, then `AUTH`. It is plain
  (uncolored) and aligned with the data; each window's reset shares the `RESET`
  label, disambiguated by sitting beside its own `%`.
- `*` marks the **active** account.
- Each account carries **two `% reset` pairs**: a **session** pair (the rolling
  5-hour window — *when work resumes*) then a **weekly** pair (the account-level
  window — *when the account fully frees up*), in that paired order —
  `session% session-reset`, then `weekly% weekly-reset`.
- The percentages are the last-polled usage (`n/a` when the last poll for that
  account failed — never a fabricated `0`).
- Each reset is the compact time until that window refills (e.g. `12m`, `2h`,
  `3d4h`), shown for **every** account, not only an exhausted one — `n/a` when that
  reset instant is unknown.
- A trailing **`AUTH`** column reports each account's **credential-auth state** as one
  self-coloring glyph — **🟢** healthy (a positive liveness signal), **🟡** stale (the
  access token has expired but the refresh token still recovers it), **🟠** at-risk (the
  auto-refresh safety-net is failing), **🔴** dead (needs re-login), **⚪** unknown (no
  liveness signal yet — unverified, not a false 🟢, issue #137). A **🔴** dead credential
  trails the actionable **`claude /login`** cue, softened to `recovering` while a dead
  credential is answering again and climbing back toward health (issue #109); a parked
  account trails `disabled` (issue #36, orthogonal to credential health). The header
  reports **auth** standing, not a vague "health" (rate-limit health lives in the `%`
  columns); the column is omitted only when no account carries a state.

The **`next swap:`** footer names the account the daemon would rotate to next — the
viable target whose weekly quota resets soonest. It reads `none (no viable target)`
when no other account is a sound swap destination — every one is weekly-exhausted, over
the opt-in swap-target session floor, or quarantined and needs a re-login — and
`none (awaiting usage data)` right after the daemon starts, before it has polled the
other accounts. It is **forward-looking** and recomputed every cycle, so —
unlike a remembered "last swap" — it survives a daemon restart and always shows where
the next rotation will land.

On a terminal too narrow for the full table the lowest-priority columns drop in
order — the **weekly pair** (`weekly%` + `weekly-reset`) first and together, then
the health-text column, each taking its header label with it — never wrapping a
row; the `ACCOUNT` label and the **session pair** (the soonest, most actionable
reset) and their labels are always kept. Output that is piped or redirected (not a
TTY) always keeps the full table, so `sessiometer status | grep work` stays complete.

On an interactive terminal each **cell** is **color-coded by its own health** —
**green** / **yellow** / **red**. Each `%` is coloured by its own utilization
(green = plenty of quota, red = heavily used); each reset is coloured by its own
**proximity** — a far reset reads green, an imminent one red — so a far weekly
reset can sit green beside an imminent session reset in red on the same row. The
colour **augments** the row — every percentage and reset is fully readable without
it — and is never the only signal. Color is emitted **only** on an interactive
TTY: it is suppressed when output is piped or redirected, when `--no-color` is
passed, or when `NO_COLOR`, `CLICOLOR=0`, or `TERM=dumb` is set in the environment
— so an escape sequence never reaches a pipe, a redirect, or a log.

For the full data regardless of terminal width — both reset instants as raw
epoch seconds, for scripting — use `--json`:

```sh
sessiometer status --json | jq '.accounts[] | {label, session_resets_at}'
```

The output is sourced solely from non-secret fields (labels, percentages, reset
instants, a next-swap candidate label), so it never prints a token or email (issue #15).

For each account's raw **access-token expiry**, pass `-v` (or `--verbose`):

```text
ACCOUNT  SESSION% RESET  WEEKLY% RESET  AUTH
* work   97%      12m    40%     5d     🟢
  spare  10%      2h     20%     3d     🟢
  dead   n/a      n/a    n/a     n/a    🔴 claude /login

next swap: spare

access token — auto-refreshed by Claude Code, not a re-login deadline:
  work   expires in 3h
  spare  expires in 40m
  dead   unknown
```

The block trails the table with one line per account — `expires in <time>` (the same
compact `2h` / `3d4h` units the resets use), `expired` once that instant has passed, or an
honest `unknown` when no expiry is stored. It is the raw **access-token** TTL: Claude Code
refreshes this token invisibly, so a lapsed clock is **not** a re-login deadline — that is
the 🔴 `claude /login` cue in the `AUTH` column. The raw clock is kept out of the default
table (where it would be misread as a deadline); `--verbose` is the opt-in for it in the
text view, mirroring `--json`, which already carries the raw `access_expires_at` for every
account. Like the table, the block is content (it survives a pipe), never colored, and
sourced only from non-secret fields, so it never prints a token or email (issue #15).

## Listing accounts (offline)

`sessiometer list` prints the captured roster — one `label` + full `account_uuid`
per line — **without a running daemon**. Unlike `status` (which queries the live
`run` loop), `list` reads only `config.toml`, the credential **store**, and the
event log, so it answers *even when the daemon is down* — frequently exactly when a
wedged daemon is itself a credential problem and you most need to look (issue #120).

```text
work    11111111-1111-1111-1111-111111111111  · expires in 2h · last refresh: refreshed
spare   22222222-2222-2222-2222-222222222222  · expired · last refresh: dead — claude /login
backup  33333333-3333-3333-3333-333333333333 · disabled · expires in 3d

3 accounts
```

Each row trails the **static auth subset** the daemon would otherwise surface live:

- **`expires in <time>`** — the stored access token's freshness, derived from its
  `expiresAt` against the wall clock (the same compact `2h` / `3d` units `status`
  uses); **`expired`** once that instant has passed.
- **`last refresh: <outcome>`** — the **last-persisted** outcome of the automatic
  refresh tick (issue #105/#106) for that account, in the same token the event log
  records (`refreshed`, `no_change`, `dead`, …); a **`dead`** credential trails the
  actionable **`claude /login`** cue, matching `status`.

Each tag is **omitted when its datum is unavailable** — an unreadable stash (locked
keychain) drops the expiry, and an account the refresh tick has never touched drops
the refresh tag — so a config-only roster reads as the plain `label` + `uuid` view.
The reads are **daemon-independent and read-only**: no daemon, no `/usage` call, no
live refresh, and — like `status` — only non-secret fields (a timestamp-derived
duration and a bare outcome token), never a token or email (issue #15).

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

## Keeping a parked credential fresh

A parked account's stored credential can go stale while it sits out of the active
session. `poke` keeps it fresh by running Claude Code once for that account in a
dedicated, throwaway `CLAUDE_CONFIG_DIR`: it seeds a copy of the account's stashed
credential into an isolated keychain item, runs `claude -p` pointed at that config
dir so **Claude Code refreshes its own credential** there, reads the refreshed
credential back, re-stashes it, and tears the isolated dir and item down. `poke` is
only the trigger — Claude Code performs the refresh — and the live
`Claude Code-credentials` item the active session reads is never touched.

```sh
# Refresh one parked account (resolves by `list` label OR account-uuid):
sessiometer poke spare

# Refresh every parked account whose stored token is near expiry:
sessiometer poke
```

`poke` refreshes **parked** accounts only: it never touches the active account
(naming it is refused, and the all-accounts sweep skips it), so the live session's
credential is left alone. A cycle reports one redacted line per account —
`refreshed`, `no change`, `dead` (needs re-login), or `error` — naming only the
account's `list` label, never a token. It needs the `claude` binary on your `PATH`
(or `$CLAUDE_BIN` set to its absolute path).

## Refreshing parked credentials automatically

`poke` is the manual trigger; the daemon can also run that same refresh **on a
cadence** so a spare is always ready to swap to without a stale-token round-trip.
The periodic tick is **off by default** (opt-in) and runs entirely in the daemon's
**idle path** — between polls, off the poll → usage → swap seam — so it never
competes with the work that keeps the active session alive. Each refresh happens in
an isolated `CLAUDE_CONFIG_DIR` (exactly as `poke`), so the live
`Claude Code-credentials` item is never touched, and the **active account and the
imminent swap target are always excluded** — it refreshes parked accounts only. A
refresh failure (or a cycle that overruns its timeout) is non-fatal: it is logged,
redacted, and the daemon returns to polling.

Turn it on and tune it in the `[refresh]` table of `config.toml`:

```toml
[refresh]
enabled = true            # opt-in; false (the default) leaves the tick wholly inert
accounts = []             # parked accounts by `list` label or account-uuid; [] = all near-expiry
cadence_secs = 3600       # seconds between ticks AND the near-expiry horizon (60..=86400)
idle_after_secs = 60      # idle seconds (no poll/swap) required before a refresh fires (0..=3600)
timeout_secs = 90         # whole-cycle bound for one account's refresh (10..=600)
# claude_bin = "/absolute/path/to/claude"   # overrides $CLAUDE_BIN/$PATH; omit to resolve normally
```

An account is **due** when its stored token would expire within one `cadence_secs`
of now — i.e. it would not survive until the next tick — so the cadence doubles as
the near-expiry horizon (no second knob). Changes take effect at the next daemon
start. If the `claude` binary cannot be resolved when the tick is enabled, the tick
is disabled with a warning rather than failing the daemon.

`idle_after_secs` is measured from the last poll, and the daemon polls one account
roughly every `poll_secs ÷ (accounts in rotation)` — so keep `idle_after_secs`
**below** that spacing, or the next poll always preempts the refresh and it never
fires. With the defaults (`poll_secs = 300`) that leaves room for rosters up to five
accounts; larger rosters should lower `idle_after_secs` to fit the gap.

> **Defaults are provisional.** The refresh token's durable lifetime is not yet
> pinned, so the shipped cadence/idle defaults are deliberately conservative and may
> change once the engine's own first-run telemetry establishes the real TTL. Pick a
> `cadence_secs` comfortably shorter than your observed token lifetime.

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
