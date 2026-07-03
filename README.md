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
- A **supported Claude Code version**. `sessiometer` depends on reverse-engineered
  Claude Code internals (the keychain-service derivation and credential-refresh
  behaviour) that were verified against a specific range — currently
  **`2.1.181`–`2.1.197`** on macOS `26.5.1` / Darwin `25.x`. A `claude` outside
  this range may have changed those internals and is unverified: `sessiometer`
  could target the wrong keychain item with no other signal. The authoritative
  range lives in [`build/version-compat.md`](build/version-compat.md), and
  `scripts/check-cc-version.sh` checks your installed `claude` against it.

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
  auto-refresh safety-net is failing), **🔴** dead (needs re-login — recover with
  [`sessiometer login`](#logging-in--re-authenticating)), **⚪** unknown (no
  liveness signal yet — unverified, not a false 🟢, issue #137). A **🔴** dead credential
  trails the actionable **`claude /login`** cue, softened to `recovering` while a dead
  credential is answering again and climbing back toward health (issue #109); a parked
  account trails `disabled` (issue #36, orthogonal to credential health). The header
  reports **auth** standing, not a vague "health" (rate-limit health lives in the `%`
  columns); the column is omitted only when no account carries a state.

The **`next swap:`** footer names the account the daemon would rotate to next — the
viable target whose weekly quota resets soonest. It reads `none (no viable target)`
when no other account is a sound swap destination — every one is weekly-exhausted,
session-saturated (over its swap-away session trigger), over the opt-in swap-target
session floor, or quarantined and needs a re-login — and
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

When the periodic refresh (**`[refresh]`**) is **off** and at least one **non-active**
account is unverified or going stale (**⚪**/**🟡**/**🟠**/**🔴** in `AUTH`), a single
**advisory** line trails the footer, naming the one-shot remedy:

```text
next swap: spare

advisory: [refresh] is off and non-active accounts are going stale — run 'sessiometer poke' or enable [refresh] to maintain them
```

With the tick off, non-active credentials get no maintenance and can lapse silently —
the advisory surfaces that gap up front instead of leaving it to the eventual
`none (no viable target)`, by which point the fallback set is already dead. Run
[`sessiometer poke`](#keeping-a-parked-credential-fresh) once, or enable
[`[refresh]`](#refreshing-parked-credentials-automatically) for ongoing upkeep. Like the
colour overlay it is **advisory chrome** — shown only on an interactive TTY (suppressed
when piped, redirected, `--no-color`, or `NO_COLOR`/`CLICOLOR=0`/`TERM=dumb`) and
**never** emitted into `--json`, so scripts and `status | grep` are unaffected.

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
keychain is locked the switch still aborts at once, writing nothing.

`--force` also **recovers** the session when the active credential itself is **gone
or rotated** — for example a forced Claude logout that scrubbed or replaced the
keychain token, leaving nothing to swap *away* from. With no sound outgoing account
to preserve, `use --force <account>` **adopts** the target directly: it writes the
target's credential to the keychain and `~/.claude.json` without re-stashing the
departing account (there is no valid token to re-stash, so nothing is stapled under
the wrong identity). Only a **confirmed-absent** or **rotated** canonical is adopted:
a credential that merely *could not be read* — a **locked** keychain (transient:
unlock and retry), or any other read failure — still aborts here, writing nothing.
*Could not read* is not *gone*, so a swap is never written blind over a credential
that could not be read.

`use` works whether or not the daemon is running: when one is up, the pre-swap gate
reads the **cached** usage the daemon already polled — so `use` makes no usage
request of its own and won't trip a rate limit — and with no daemon it falls back to
a single live check.

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
`disabled`. A running daemon picks up the change in its live rotation right away —
no restart needed.

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
pointing at a missing stash. A running daemon picks up the removal in its live
rotation right away — no restart needed.

Removing the **active** account is allowed: it touches only `sessiometer`'s
roster entry and stash, never the live `Claude Code-credentials` item, so the
running Claude Code session keeps working. The daemon then simply resolves no
active account (polling only, never swapping) until you `capture` another account
or sign in again.

## Logging in / re-authenticating

Revive a **`dead`** account — or onboard a new one — by re-authenticating it.
`sessiometer login` runs `claude /login` inside an **isolated, throwaway
`CLAUDE_CONFIG_DIR`** (the same isolation `poke` uses), so the browser OAuth
handoff never touches the live `Claude Code-credentials` item a running session
reads. It harvests the credential Claude Code writes there and lands it in the
rotation, re-pointing the canonical credential to it under the swap lock — so the
re-login also takes effect:

```sh
# Re-authenticate (or onboard) an account; the label is optional:
sessiometer login spare
```

The optional `<label>` names a **new** account — omit it and the label is
auto-derived from the account's `account_uuid` (exactly as `capture`); a re-login of
an already-rostered account keeps its existing label unless you pass a new one.
`login` needs a real terminal and the `claude` binary on your `PATH` (or
`$CLAUDE_BIN`); tune its timeout in the [`[login]`](#login) block. On success it
prints one redacted line — `Onboarded` (new) or `Revived` (existing); an unfinished
login prints `login cancelled, nothing captured` and still exits `0`. Unlike the
daemon, a **locked keychain aborts the login at once** (one-shot, no back-off,
nothing written), exiting **`4`**.

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

`idle_after_secs` sets how long the daemon must idle before the **first** refresh
sweep after start-up. Since issue #260 the idle floor is anchored to an absolute
instant, so neither the usage poll nor the 15 s internal login-watch resets it — it
accumulates across idle gaps and the sweep fires once it elapses, after which sweeps
recur on `cadence_secs` alone. Keep it comfortably below `cadence_secs`; the default
60 s suits any roster size.

> **Defaults are provisional.** The refresh token's durable lifetime is not yet
> pinned, so the shipped cadence/idle defaults are deliberately conservative and may
> change once the engine's own first-run telemetry establishes the real TTL. Pick a
> `cadence_secs` comfortably shorter than your observed token lifetime.

## Configuration

`sessiometer` keeps all of its state in one TOML file,
`~/Library/Application Support/sessiometer/config.toml` (or
`$XDG_CONFIG_HOME/sessiometer/config.toml` when `$XDG_CONFIG_HOME` is set). The
**roster** — the `[[account]]` entries — is managed for you by `capture`, `login`,
`remove`, and `disable`/`enable`; don't hand-edit it. The tuning blocks below **are**
safe to hand-edit: every key is optional and falls back to the default shown, an
out-of-range value is rejected at load with a message naming the key, and a running
daemon reloads the file on change (no restart). The generated file also carries an
inline comment on every key, so your own `config.toml` doubles as a reference.

### `[tunables]`

The primary hand-editable block — the poll cadence and the swap thresholds.

| Key | Meaning | Range | Default |
|-----|---------|-------|---------|
| `poll_secs` | Seconds between re-polling a given account — the per-account cadence and the base of the rate-limit back-off. | `5..=3600` | `300` |
| `cooldown_secs` | Seconds to wait after a swap before another swap is allowed. | `0..=3600` | `60` |
| `session_trigger` | Swap **away** from the active account at or above this session-usage percent. | `50..=99` | `95` |
| `weekly_trigger` | Swap **away** at or above this **weekly**-usage percent — independent of `session_trigger` (typically higher); a swap fires when *either* dimension trips. | `50..=99` | `98` |
| `session_floor` | Opt-in guard: only swap **to** an account whose session usage is below this percent. Off unless set. | `0..=session_trigger` | off |
| `monitor_401_n` | Consecutive non-scope `401`s before an account is treated as dead and quarantined. | `1..=20` | `3` |
| `monitor_recovery_m` | Consecutive recovery-probe successes before a quarantined account whose own token recovers (without a re-login) is returned to the rotation. | `1..=20` | `2` |

The ranges and defaults above are exactly the ones enforced in
[`src/config.rs`](src/config.rs) (`Config::validate` and the `DEFAULT_*` constants) —
the single source of truth this table is drawn from, so it stays in step with the code.

### `[jitter]`

Per-cycle randomization added to a tunable, drawn fresh each cycle and clamped back to
the tunable's range, so polls and swaps decorrelate across accounts and cycles. One
optional entry per tunable — `poll`, `trigger`, `weekly_trigger`, `cooldown` — each an
inline table whose `kind` is `"none"`, `"uniform"` (with a `spread`), or `"normal"`
(with a `stddev`); magnitudes are TOML floats. Only `poll` jitters by default:

```toml
[jitter]
poll = { kind = "normal", stddev = 60.0 }   # default: normal, ~20% of poll_secs
trigger = { kind = "none" }                 # trigger / weekly_trigger / cooldown default to none
```

### `[login]`

Settings for `sessiometer login`, the interactive re-auth verb.

| Key | Meaning | Range | Default |
|-----|---------|-------|---------|
| `timeout_secs` | Seconds bounding one whole interactive login capture — longer than a refresh, since it waits on a human completing a browser OAuth handoff. | `60..=600` | `180` |
| `claude_bin` | Absolute path to the `claude` binary to spawn, overriding `$CLAUDE_BIN`/`$PATH`. Omit (or leave empty) to resolve normally. | — | unset |

### Other blocks

- **`[refresh]`** — the daemon's periodic parked-credential refresh; documented under
  [Refreshing parked credentials automatically](#refreshing-parked-credentials-automatically).
- **`[stats]`** — retention horizons for the usage-stats store.
- **`[migration]`** — the KDF cost and conflict-policy defaults for `export` / `import`.

`[stats]` and `[migration]` are hand-editable too; their keys, ranges, and defaults
are documented by the inline comments in the generated `config.toml`.

## Exporting state (offline)

`sessiometer export` serializes your local state — the roster and tunables plus each
account's stashed credential and `oauthAccount` identity — into a single **migration
artifact**, so you can move a whole setup to another Mac. It is **read-only**: it
never mutates the keychain or the roster.

```bash
# Encrypted by default — prompts for a passphrase (no echo), writes a 0600 file:
sessiometer export ~/sessiometer-state.smmig

# Or stream the artifact to stdout (still prompts on the terminal for the passphrase):
sessiometer export > state.smmig

# Config-only — the roster + tunables, with NO credential material:
sessiometer export --no-secrets ~/sessiometer-config.smmig
```

The passphrase is **never** taken from the command line (it would leak into the
process table and shell history). Supply it interactively, or non-interactively for
automation via `--passphrase-stdin` / `--passphrase-file <path>`:

```bash
sessiometer export --passphrase-stdin state.smmig < passphrase.txt
```

Flags:

- **(default)** — encrypt the artifact with a passphrase (Argon2id + XChaCha20-Poly1305).
- **`--plaintext`** — skip encryption. The artifact then holds usable credentials **in
  the clear**; `export` prints a warning, and you should treat and delete the file like
  a password. Legitimately paired with `--no-secrets` (nothing to protect).
- **`--no-secrets`** — export a config-only artifact (roster + tunables), omitting every
  credential blob — handy to share a configuration without secrets.

A `PATH` argument is written atomically (a same-directory temp, then `rename(2)`) at
mode `0600`; with no `PATH` the artifact goes to standard output. Cross-machine
credential portability on macOS is verified (build spike #145), so an exported artifact
restores on another Mac.

## What it stores

`sessiometer` takes custody of Claude Code credentials, so it is worth knowing
exactly what it keeps and where. Everything lives under your own user account — in
the **login keychain** and under `~/Library` — and nothing leaves the machine
(the only outbound traffic is the read-only usage poll; the one file that can carry
credentials off-machine is an [`export`](#exporting-state-offline) artifact you
create explicitly).

### In the login keychain

All credential material lives in the macOS **login keychain**
(`~/Library/Keychains/login.keychain-db`), reached only through the
`/usr/bin/security` CLI:

- **The active Claude Code credential** — the generic-password item whose service
  is `Claude Code-credentials` (Claude Code suffixes it with a hash under a
  non-default `CLAUDE_CONFIG_DIR`). This item is **Claude Code's own**;
  `sessiometer` reads and rewrites it in place to swap the active account, but it
  is the same item plain Claude Code created and reads, so removing `sessiometer`
  leaves it intact.
- **A per-account stash**, one per captured account, under the service
  `Sessiometer/<account_uuid>` — two items each: the raw credential blob
  (`acct = "credential"`) and the account's `oauthAccount` identity block
  (`acct = "oauthAccount"`). Written by `capture` and `login`, erased by `remove`.
  This is what lets `sessiometer` restore any account as the active one.
- **Short-lived isolated items** created during `poke`, `login`, and the periodic
  refresh so Claude Code can refresh a parked account's token without touching the
  live credential. Each is seeded and **torn down within the same cycle**; a crash
  can leave one behind, and the next run's reaper clears it.

### On disk

Under `~/Library`, every file `0600` and every directory `0700`, each checked to be
owned by you:

| Location | Holds | Secrets? |
|----------|-------|----------|
| `~/Library/Application Support/sessiometer/config.toml` (or `$XDG_CONFIG_HOME/sessiometer/config.toml`) | The **roster** — `[[account]]` labels and `account_uuid`s pointing at the keychain stashes — plus the tunables | **No** — the roster references stashes; the credential blobs stay in the keychain |
| `~/Library/Application Support/sessiometer/` | The daemon's runtime files: `daemon.lock`, `daemon.sock` (control socket), `swap.lock`, the usage store (`usage-samples.jsonl`, `usage-rollup.json`), and the ephemeral `refresh/` and `login/` isolation directories | No |
| `~/Library/Logs/sessiometer/sessiometer.log` | The event log — durable state changes | No — every line passes a CI redaction check; never a token or email |

The config directory is `$XDG_CONFIG_HOME/sessiometer` when `$XDG_CONFIG_HOME` is
set, otherwise `~/Library/Application Support/sessiometer`; the daemon's runtime
files always live in the native `~/Library/Application Support/sessiometer`
regardless. `sessiometer` also co-writes the active account's `oauthAccount` block
into Claude Code's own `~/.claude.json` during a swap — that file belongs to Claude
Code, not `sessiometer`.

The security posture behind all of this — keychain-via-CLI, secrets off `argv`,
in-memory zeroization, redacted diagnostics — is stated in
[`SECURITY.md`](SECURITY.md).

## Uninstalling / recovery

To remove `sessiometer` completely and hand credential custody back to plain
Claude Code:

1. **Stop the daemon.** Quit any running `sessiometer run` (Ctrl-C in its terminal,
   or kill the process). Nothing else runs in the background.

2. **Erase the per-account stashes.** The cleanest way is to `remove` each captured
   account, which deletes its `Sessiometer/<account_uuid>` keychain stash:

   ```sh
   sessiometer list               # see the captured accounts
   sessiometer remove <label>     # repeat for each; erases that account's stash
   ```

   `remove` never touches the live `Claude Code-credentials` item — even for the
   active account — so your Claude Code session keeps working throughout.

3. **Delete the on-disk state:**

   ```sh
   rm -rf ~/"Library/Application Support/sessiometer"
   rm -rf ~/"Library/Logs/sessiometer"
   # only if you set $XDG_CONFIG_HOME:
   rm -rf "$XDG_CONFIG_HOME/sessiometer"
   ```

4. **Remove the binary** — delete the `sessiometer` executable you built or
   installed (e.g. `target/release/sessiometer`, or wherever you copied it).

**Returning custody to plain Claude Code.** `sessiometer` never takes the
`Claude Code-credentials` item away from Claude Code — it only rewrites it in
place — so once `sessiometer` is gone, Claude Code simply keeps using **whichever
account was active last**. If that is not the account you want, switch to it before
uninstalling (`sessiometer use <account>`), or run `claude /login` afterwards to
re-authenticate directly.

If you skipped step 2, any leftover `Sessiometer/<account_uuid>` items are inert —
plain Claude Code never reads them — but you can still delete them by hand: in
**Keychain Access**, search `Sessiometer/` and delete the matching items (two per
account); or scripted, `security delete-generic-password -s "Sessiometer/<account_uuid>"`,
repeated until it reports the service is gone.

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
- **On-disk roster changes are picked up at runtime.** After `capture`, `login`,
  `remove`, or `disable`/`enable` writes `config.toml`, a running daemon **reloads
  its roster** and reflects the change in the live rotation — and in `status` —
  **without a restart**. Persisting accounts keep their in-flight health and usage
  readings; a newly-onboarded account joins the rotation and is polled on the next
  cycles. Best-effort: with no daemon running there is nothing to update, and the
  next start loads the current roster anyway.
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
raise [`poll_secs`](#configuration) or keep the roster smaller by choice.

## Build from source

```sh
cargo build --release
./target/release/sessiometer --help
```

## Support

`sessiometer` is free and MIT-licensed. If you find it useful, you can support
its continued development through GitHub Sponsors —
**[github.com/sponsors/alexey-pelykh](https://github.com/sponsors/alexey-pelykh)**.
Sponsorship is entirely optional and never gates any functionality; every
feature remains available under the [MIT license](LICENSE).

## License

[MIT](LICENSE) © 2026 Oleksii PELYKH
