# sessiometer

Manage multiple Claude Code accounts on macOS. `sessiometer` polls each
account's usage quota and swaps the active credential out-of-band before an
account is exhausted, so a long session keeps running by rotating across
accounts.

> **Unofficial.** `sessiometer` is not affiliated with, endorsed by, or supported
> by Anthropic. "Claude" and "Claude Code" are trademarks of Anthropic, referenced
> here only nominatively to describe what `sessiometer` works with — no third-party
> logos or marks are used. The project is [MIT-licensed](LICENSE).

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
- A Claude Code version the internals were **verified against**. `sessiometer`
  depends on reverse-engineered Claude Code internals (the keychain-service
  derivation and credential-refresh behaviour); the range they were last verified
  against is currently **`2.1.181`–`2.1.217`** on macOS `26.5.1`–`26.5.2` / Darwin
  `25.x`. This is provenance, not a compatibility gate: a `claude` outside the
  range is unverified and handled best-effort, never refused on version alone.
  `sessiometer` does not check your installed `claude` version at runtime — the
  version string was never a control. It records the verified range as a neutral
  line in `sessiometer --version` (baked in, printed always). The one runtime
  guard is the behavioral canary (see "Edge cases & resilience"): it re-verifies
  the keychain-service derivation and refuses credential writes rather than let
  them land on the wrong item — the drift that actually matters, caught on the
  user's machine where a version number never could. The
  authoritative range lives in [`build/version-compat.md`](build/version-compat.md);
  `scripts/check-cc-version.sh` compares the installed `claude` against that
  range for maintainers at release time as an advisory provenance check.

## Quickstart

```sh
# 1. Capture each account's credential. Sign in to the account in Claude Code,
#    then stash its current credential. At a terminal, `capture` offers the
#    account's email as an editable label default — press Enter to keep it, or
#    type a shorter handle like `work`. Pass `capture <label>` to skip the prompt.
sessiometer capture

# 2. Run the foreground daemon. It polls usage and swaps the active credential
#    to the next account before the current one is exhausted:
sessiometer run

# 3. Check the roster and the next swap candidate at any time:
sessiometer status
```

## Running in the background

`sessiometer run` in the Quickstart is a **foreground** daemon: it stops when you
close its terminal. To keep the rotation going across the whole login session — with
no terminal open, and an always-present daemon for a future UI to attach to — install
it as a per-user launchd **LaunchAgent**:

```sh
# Install + start the background agent (runs `sessiometer run` at login, kept up):
sessiometer service install

# Stop + remove it:
sessiometer service uninstall
```

`service install` writes a LaunchAgent plist to
`~/Library/LaunchAgents/org.sessiometer.agent.plist` and loads it into
your login session (`launchctl bootstrap`). It is a LaunchAgent, not a system-wide
LaunchDaemon, because the swap loop needs your **login keychain**, which only exists
inside the per-user session. The agent is `RunAtLoad` + `KeepAlive`, so it starts at
login and is brought back up if it ever exits. Its stdout/stderr land in
`~/Library/Logs/sessiometer/daemon.out.log` and `daemon.err.log`.

### Two nouns: `service` (persistence) and `daemon` (process)

The lifecycle surface is split by concern — the same way systemd separates
`enable`/`disable` from `start`/`stop`:

| Noun | Concern | Verbs |
|------|---------|-------|
| `service` | **Persistence** — does the daemon auto-start at login? | `install`, `uninstall`, `status` |
| `daemon` | **Process** — the running daemon itself | `status`, `stop`, `restart` |

`service status` answers *"is a managed service installed?"*; `daemon status` answers
*"is a daemon running, and how is it managed?"*. You never *start* a daemon with the
`daemon` noun — you start one with `service install` (managed) or `sessiometer run`
(unmanaged) — which is why there is no `daemon start`.

### The three daemon states

| State | What it means | `daemon status` reports |
|-------|---------------|-------------------------|
| **Managed** | installed via `service install`; launchd starts it at login and keeps it up | running (managed by launchd) |
| **Foreground `run`** (unmanaged) | started by `sessiometer run`, in a terminal or detached; nothing supervises it | running (unmanaged) |
| **Stopped** | no daemon is running — none was started, or it was stopped | not running |

```sh
# Which state am I in?
sessiometer daemon status

# Stop the running daemon (either state; a no-op if none is running):
sessiometer daemon stop

# Restart it — the recovery verb after a stuck/stale daemon or a config change:
sessiometer daemon restart
```

`daemon stop` reaches both running states, but by different means, because the means are
what make "stopped" stick. A **managed** daemon is booted out of your login session
(`launchctl bootout`), which also stops `KeepAlive` from respawning it — it returns at
next login, and `service uninstall` removes it for good. An **unmanaged** daemon is asked
to shut down gracefully over its control socket, so an in-flight swap always completes
before it exits. Either way the post-condition is the same: not running. No lifecycle path
ever discovers a PID in order to signal it.

"Managed" here means **launchd is supervising the daemon right now** — not merely that a
service is installed. The two differ after a `daemon stop`, which boots the agent out but
leaves it registered: a `sessiometer run` started in that window is unmanaged, even though
`service status` still reports an installed service. The `daemon` verbs follow the running
process, so they treat it as exactly what it is.

`daemon restart` works only on a **managed** daemon, because restart means *kill and
relaunch* (`launchctl kickstart -k`, atomic — a bare kill would just be respawned by
`KeepAlive`), and only launchd supervises the daemon well enough to relaunch it. A
foreground `sessiometer run` has no supervisor, so `daemon restart` is a clear, actionable
error rather than a half-restart: install a managed service (`sessiometer service install`)
for a supervised daemon with restart, or `sessiometer daemon stop` it and start a new
`sessiometer run`. On an installed service that is currently stopped, with nothing else
running, `daemon restart` simply starts it — no login cycle needed.

> **Recovering a wedged daemon.** The managed path never needs the daemon to be
> responsive: `daemon stop` and `daemon restart` both go through launchd. The unmanaged
> path talks to the daemon's control socket, so it needs a daemon that still answers —
> there is no clean automated recovery for a wedged foreground `run`.

**One owner at a time — a safety guard.** Whatever launchd starts is the ordinary
lock-guarded `sessiometer run`: it takes a single-owner lock on the roster before it
polls or swaps. So the background agent and a foreground `sessiometer run` can never
both drive the swap loop — whichever starts **second** refuses immediately with

```text
sessiometer: another sessiometer daemon is already running (the single-instance lock is held)
```

and exits `3`, performing **no** swap. This guard is deliberate and has **no**
`--force`-style bypass: two processes rewriting the active credential on the same
roster would fight over which account is canonical. If you want to run in the
foreground temporarily, `sessiometer service uninstall` first (or `sessiometer daemon
stop` to stop it just for this login session), then `sessiometer run`.

**Two machines, one roster — the single-machine-sync boundary.** That single-owner
lock is a *per-machine* guard: it stops two `sessiometer` processes on the **same** Mac
from fighting over the roster, but it cannot see — let alone coordinate with — a
`sessiometer` running on a **second** machine against the same accounts. Sessiometer has
no shared backend, so running the same roster on two machines at once is possible, and
each daemon is **blind to the other's consumption**. Two consequences follow:

- **Co-consumption.** Both machines can bill the same account's session and weekly quota
  at the same time. The swap loop's safety margin is calibrated for a single machine's
  post-swap tail; two machines' tails stacked on one parked account can exceed it, so an
  account can land past its ceiling even when each machine swapped on target.
- **Per-machine visibility.** The landing check — both the offline `sessiometer
  reliability` readout and the live `status` overshoot signal — only sees what *this*
  machine observed. Another machine pushing a parked account past the ceiling is
  invisible to it.

**The mitigation, and its limits.** Velocity-spike detection is the one guard that
crosses the boundary: it reads each account's usage from the account-global
`/oauth/usage` endpoint, which already reflects **both** machines' combined burn, so a
co-consumption spike shows up as a faster-than-expected climb and can trigger an earlier
swap. It **reduces** the exposure — it does not remove it: the post-swap committed tail
and the shared per-account rate limits still apply, and two machines can still briefly
stack usage between polls. Running one roster per machine avoids the boundary entirely;
if you must span two, treat velocity spikes as the safety net, not a guarantee.

## Checking status

`sessiometer status` queries the running daemon and prints each account as one
row of an aligned, border-less table under a labelled header — greppable, one
record per line:

```text
ACCOUNT  SESSION% RESET  WEEKLY% RESET  EXPIRY   AUTH
* work   97%      12m    40%     5d     21d3h    🟢
  spare  10%      2h     20%     3d     [6d21h]  🟢
  idle   n/a      n/a    n/a     n/a    —        🟠 degraded — run 'sessiometer poke'
  gone   n/a      n/a    n/a     n/a    lapsed   🔴 claude /login

next swap: spare
```

- A **header row** labels the columns: `ACCOUNT`, then the grouped `SESSION%` +
  `RESET`, then the grouped `WEEKLY%` + `RESET`, then `EXPIRY`, then `AUTH`. It is
  plain (uncolored) and aligned with the data; each window's reset shares the `RESET`
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
- An **`EXPIRY`** column carries each account's **refresh-token deadline** — the
  instant its stored credential stops being renewable — as a compact time-until
  (`6d21h`), the word **`lapsed`** once that instant has passed, or **`—`** when no
  deadline was observed. A time-until in **brackets** (`[6d21h]`) sits **inside** the
  [`[credential]`](#credential) foresight horizon; a bare one (`21d3h`) sits beyond it.
  The brackets are what make that distinction survive `--no-color`, `NO_COLOR`, a pipe,
  and a log capture — they matter because the horizon is **yours to configure**, so the
  bare duration alone cannot tell you which side of it you are on. It is a cell of **its
  own**, never folded into `AUTH`: the two axes are independent, so an account can read
  **🟢** in `AUTH` and still sit days from its refresh-token deadline. On an interactive
  terminal the cell is additionally tinted by its own band — red once `lapsed`, yellow
  inside the horizon, dimmed beyond it, and left uncolored when nothing was observed.
  Like the health-text column it is **conditional**: a roster whose credentials carry no
  deadline renders exactly as it did before the column existed, rather than growing a column
  of em dashes. See [The refresh-token deadline](#the-refresh-token-deadline-expiry) below.
- A trailing **`AUTH`** column reports each account's **credential-auth state** as one
  self-coloring glyph — **🟢** healthy (a positive liveness signal), **🟡** stale (the
  access token has expired but the refresh token still recovers it), **🟠** at-risk (the
  auto-refresh safety-net is failing), **🟠** degraded (the access token was rejected and
  the account quarantined out of rotation, but its **refresh token is still good** — it
  needs a *refresh*, not a re-login; issue #427), **🔴** dead (a refresh **proved** the
  refresh token itself is dead — the one state that genuinely needs re-login, recover with
  [`sessiometer login`](#logging-in--re-authenticating)), **⚪** unknown (no liveness signal
  yet — unverified, not a false 🟢, issue #137). A **🟠** degraded credential trails a
  **needs-refresh** cue (`run 'sessiometer poke'`, or enable
  [`[refresh]`](#refreshing-parked-credentials-automatically)); only a **🔴** dead credential
  trails the re-login **`claude /login`** cue — each softened to `recovering` while the
  credential is answering again and climbing back toward health (issue #109); a parked
  account trails `disabled` (issue #36, orthogonal to credential health). The header
  reports **auth** standing, not a vague "health" (rate-limit health lives in the `%`
  columns); the column is omitted only when no account carries a state.

The **`next swap:`** footer names the account the daemon would rotate to next — the
viable target whose weekly quota resets soonest. When no other account is a sound swap
destination — every one is weekly-exhausted, session-saturated (over its swap-away
session trigger), over the swap-target `target_max_session_usage` reserve, or quarantined (out
of rotation until it recovers) — it reads `none — out of capacity; resets in ⟨when⟩`,
naming the soonest moment *any* spare returns to viability across both the session
and weekly windows (issue #665), so a stranded operator sees the real blocker and
when it lifts rather than a content-free "none". When that wait exceeds one session
window — or is unknown — the block is a structural shortage rather than a transient
one, and the footer appends the `— add an account` remedy; the nudge keys off the
actual wait, never the session/weekly cause label, which on a mixed fleet names only
the soonest-returning spare's gating dimension, not a fleet-wide property (issue
#666). A relief-less dead end falls back to the bare `none (no viable target)`. Right
after the daemon starts, before it has polled the other accounts, it reads `none
(awaiting usage data)`. It is **forward-looking** and recomputed every
cycle, so — unlike a remembered "last swap" — it survives a daemon restart and always
shows where the next rotation will land.

On a terminal too narrow for the full table the lowest-priority columns drop in
order — **`EXPIRY`** first (the **slowest-moving** fact on the row: a server-issued
deadline in days that no tick moves, where every other column can flip inside the
current session), then the **weekly pair** (`weekly%` + `weekly-reset`) together, then
the health-text column, each taking its header label with it — never wrapping a row;
the `ACCOUNT` label and the **session pair** (the soonest, most actionable reset) and
their labels are always kept. Output that is piped or redirected (not a TTY) always
keeps the full table, so `sessiometer status | grep work` stays complete.

On an interactive terminal each **cell** is **color-coded by its own health** —
**green** / **yellow** / **red**. Each `%` is coloured by its own utilization
(green = plenty of quota, red = heavily used); each reset is coloured by its own
**proximity** — a far reset reads green, an imminent one red — so a far weekly
reset can sit green beside an imminent session reset in red on the same row. The
colour **augments** the row — every cell is fully readable without it — and is
never the only signal: a percentage and a reset each state their own number, and
the `EXPIRY` cell brackets a within-horizon deadline, so the one distinction that
lived only in the tint now survives the colour being stripped. Color is emitted
**only** on an interactive TTY: it is suppressed when output is piped or
redirected, when `--no-color` is passed, or when `NO_COLOR`, `CLICOLOR=0`, or
`TERM=dumb` is set in the environment — so an escape sequence never reaches a
pipe, a redirect, or a log.

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
instants, a next-swap candidate label), so it never prints a token or an *unauthored*
email (issue #15; an operator-authored email label may appear — #444).

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
sourced only from non-secret fields, so it never prints a token or an *unauthored* email
(issue #15; an operator-authored email label may appear — #444).

### The refresh-token deadline (`EXPIRY`)

Each stored credential carries **two** clocks, and only one of them slides:

| Clock | Where it shows | Does it move? |
|-------|----------------|---------------|
| **Access token** — the short-lived token a session actually spends | the `-v` block above, `access_expires_at` in `--json` | **Yes.** Claude Code and the daemon refresh it invisibly; each refresh slides it forward. |
| **Refresh token** — the credential's own lifetime | the `EXPIRY` column of `sessiometer status` | **No.** It is a fixed instant the server issued at login. |

**Refreshing does not extend the refresh-token deadline.** Every *other* expiry in this
tool slides forward on use, which is what makes this one easy to guess wrong. The daemon
keeps access tokens alive indefinitely — that is what `[refresh]` and `sessiometer poke`
are for — but it cannot move this wall.

Two independent lines of evidence, against Claude Code v2.1.218–220. **From the client
code**: the login path computes `refreshTokenExpiresAt` with a fallback, the refresh path
computes the same field *without* it so the value resolves undefined, and the merge step
keeps whichever deadline was already stored — an omitted field preserves the old one.
**From observation**: across six accounts whose access tokens had all refreshed within
hours, the refresh-token deadlines sat unmoved at fixed absolute instants. A refresh does
rotate the refresh token itself, but a new token is not a new deadline.

So an account can be **🟢** in `AUTH`, refreshing on schedule, and still be counting down
to a deadline nothing in that loop will move. That gap is what the `EXPIRY` column exists
to close: the deadline is reported **ahead** of the lapse rather than after it, so it is
visible while there is still a whole horizon of room.

A credential past that deadline is replaced by
[`sessiometer login`](#logging-in--re-authenticating) — the same verb that onboards a new
account, and the only path that writes a fresh credential rather than renewing the stored
one. It lands that credential in the rotation **without disturbing the active session**:
logging in an account other than the active one adds or revives it and performs no swap.

> **What a fresh login does, measured:** it lands a *new* deadline rather than carrying the
> old one forward — observed 2026-07-29, when a re-login moved one account from a
> 2026-07-31 deadline to a 2026-08-26 one. What it does *not* do is land a **predictable**
> one. That grant was not the "thirty days from login" figure sometimes quoted, and the
> `now + 30 days` in the client is its fallback for a field the server omits rather than the
> value the server sends. A single observation fixes no constant, so `sessiometer` assumes
> none — it reads whatever deadline the credential carries and reports that. Recorded in
> [issue #877](https://github.com/alexey-pelykh/sessiometer/issues/877).

**Brackets mean *inside the horizon*.** A time-until rendered `[6d21h]` sits within the
[`[credential]`](#credential) foresight horizon; a bare `21d3h` sits beyond it. That is the
only thing the brackets say — they are a descriptor, not a prompt, and they ask for nothing.
They exist because the horizon is **operator-configurable**: unlike `SESSION%`, where `98%`
describes itself, a bare deadline cannot tell you which side of *your* window it falls on.
Before them the distinction lived only in the colour band, so it vanished under `--no-color`,
`NO_COLOR`, a pipe, a log capture, or colour-blindness — a first-class supported mode losing
the whole per-account signal. `lapsed` and `—` are never bracketed: neither is *within* a
forward-looking window, and both are already words among durations. Since the brackets are
literal characters, match one with `grep -F` — bare `grep` would read `[6d21h]` as a
character class.

**`—` means not observed — it does not mean "not expiring."** The daemon found no usable
`refreshTokenExpiresAt` in the credential: an older Claude Code, a changed upstream policy,
or a non-first-party credential all produce it, as does an account the daemon has not
polled yet. Absence of a deadline in the blob is not evidence that no deadline exists — the
daemon cannot vouch for one it never saw, so it reports the absence rather than the
false-reassuring "fine" that would let an account lapse quietly. This is the same invariant
the **⚪** `unknown` auth glyph follows (issue #137), applied to foresight: an unverified
account is unverified, not well. It is also why the whole feature degrades safely if
upstream ever drops the field — every cell reads `—`, the column elides, and nothing claims
an all-clear it cannot support.

The same cell is defined for the `expiry` column of `sessiometer stats` — right-aligned and
uncolored there, since that surface's colors are the neutral utilization band. The brackets
carry over, and there they are the *only* channel for the horizon: that column never tints.

**It does not appear there yet.** `stats` is a structurally offline reader: it is a pure
function of the sample store and the daemon's own event log, and never queries a running
daemon. The daemon does write the deadline to that log, but the step that folds those lines
into one deadline per account is not built, so the overlay is empty on every path and the
column elides. Its absence in `stats` therefore means *that missing step* — not an account
without an observed deadline, which is what the same absence means in `status`. Tracking in
[issue #917](https://github.com/alexey-pelykh/sessiometer/issues/917); until it lands,
`sessiometer status` is where the deadline is reported.

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
duration and a bare outcome token), never a token or an *unauthored* email (issue #15;
an operator-authored email label may appear — #444).

## Watching the daemon (diagnostics)

`run` writes to two operator-facing channels, neither of which ever carries a
token or an *unauthored* email (issue #15; an operator-authored email label may
appear — see below):

- **The event log** — durable, edge-triggered STATE CHANGES (a swap, a re-stash, a
  dead credential, entering the all-exhausted state, …), one `key=val` line each,
  appended to `~/Library/Logs/sessiometer/sessiometer.log` (surfaced in Console.app).
  Always on.
- **The diagnostic channel** — per-cycle DETAIL for debugging a live `run`, on
  **stderr**, **off by default**. For an interactive `run` that is `-v`; for a
  **background (launchd-managed) daemon**, which gets no `-v`, it is the
  `[tunables].verbose` knob — see
  [Turning diagnostics on for a background daemon](#turning-diagnostics-on-for-a-background-daemon).

The event log — and the `-v` diagnostic channel — identify accounts by their
**`label`**, written **verbatim** as the account handle (e.g. `event=swap from=…
to=…`, `diag=poll account=…`). It is the one operator-chosen, free-form field on
the durable surface. The label may be the account's **email** — `capture` pre-fills
the harvested address as an editable default (#447), and an operator-authored email
is permitted verbatim (a *provenance-scoped* waiver: you chose it, so it is not a
leak — #444) — or any **nickname** you prefer (`work`, `spare`); a non-PII nickname
remains a fine choice, no longer a requirement. What still holds unconditionally: no
token, and no *unauthored* email (a stranger's address, a credential spill) ever
reaches this surface (issue #15). Labels are set at capture time or
[`sessiometer login <label>`](#logging-in--re-authenticating).

Pass `-v` (or `--verbose`) to opt into the diagnostic channel:

```sh
sessiometer run -v
```

It then prints, every cycle, the outcome of each account's poll — including the
`rate_limited` / `transient` outcomes the event log records no event for — the
per-tick decision and any back-off, plus the daemon's start (with the effective
config), its stop, and the moment it **leaves** the all-exhausted state:

```text
ts=2026-06-30T00:00:00Z diag=start accounts=2 poll_secs=30 target_max_session_usage=80 session_ceiling=90 weekly_ceiling=98 monitor_401_n=5 monitor_recovery_m=4
ts=2026-06-30T00:00:00Z diag=poll account=work outcome=rate_limited
ts=2026-06-30T00:00:00Z diag=tick decision=skip_active_unavailable backoff_secs=120
ts=2026-06-30T00:00:30Z diag=poll account=work outcome=live
ts=2026-06-30T00:00:30Z diag=tick decision=hold
```

When a `429` carries a `Retry-After`, the `tick` line adds `retry_after_secs=<n>`
— the raw server-advised wait (delta-seconds), **before** any daemon cap — so you
can **place the back-off's source** (issue #295) by comparing it to `backoff_secs`:

- **no `retry_after_secs`** — the server advised nothing; the wait is the daemon's
  **self-capped** exponential (as in the `backoff_secs=120` line above).
- **`retry_after_secs` == `backoff_secs`** — the **server-advised** wait governed.
- **`retry_after_secs` < `backoff_secs`** — the server advised a smaller floor, but
  the daemon's larger **self-capped** exponential governed the wait.
- **`retry_after_secs` > `backoff_secs`** — a **non-active** account only: the server
  advised more than the wait, so the ~1 h cap clamped a pathological value, which a
  bare `backoff_secs=3600` alone could never tell you:

```text
ts=2026-06-30T00:02:00Z diag=poll account=spare outcome=rate_limited
ts=2026-06-30T00:02:00Z diag=tick decision=hold backoff_secs=3600 retry_after_secs=86400
```

The **active** account is the exception (issue #453): its `Retry-After` is an
**un-clamped floor** — the daemon never re-polls before it — and its self-backoff
caps far tighter (**120 s**, not the ~1 h peer ceiling), recovering observability
fast after a throttle. So `retry_after_secs > backoff_secs` never appears for the
active account; a large server value governs it in full (the `==` case above).

Both channels carry handles, enums, percentages, and timestamps only — and a CI
redaction meter scans every rendered line of each (issues #9, #15, #77).

That guarantee is about the **lines these two channels emit**. The *file* a managed
daemon's stderr lands in is a wider surface than the diagnostics written to it: being
raw stderr, `daemon.err.log` also collects anything else the process printed there,
**panic output included**, which never passed that meter. So `sessiometer log` treats
it as an ungoverned channel and keeps it strictly opt-in — see
[The two channels](#the-two-channels---channel).

### Reading the event log (`sessiometer log`)

`log` prints the event log itself, **offline** — it reads the file directly and
makes no live call, so it works with the daemon down:

```sh
# The whole log.
sessiometer log

# Just the last day, and just the swaps.
sessiometer log --since 24h
sessiometer log --event swap

# Both, as JSON records (schema:2) for a script.
sessiometer log --since 7d --event swap --json

# Watch a running daemon: print the log, then keep printing what arrives.
sessiometer log --follow
sessiometer log -f --event swap

# The daemon's diagnostics instead — see "The two channels" below.
sessiometer log --channel diag
```

`--since` takes a non-negative integer and a unit — `s`, `m`, `h`, `d`, `w` (e.g.
`30m`, `24h`, `7d`, `2w`) — the same grammar as `sessiometer reliability --since`; a
malformed value is an error, never a silent whole-log fallback. `--event` matches the
line's kind token **exactly**, so `--event swap` will not also match a longer name that
starts with it. (That token is `event=` on the event log and `diag=` on the diagnostic
channel, so `--event tick --channel diag` selects `diag=tick`.)

The two streams are split, so a pipe stays clean:

- **stdout** carries the data. In the default text view that is the matched log lines
  **verbatim and nothing else** — every byte already existed in the file, so
  `sessiometer log | grep …`, `| wc -l` and `| head` stay honest, and with no flags
  stdout reproduces the log byte for byte. With `--json` it is instead a single JSON
  document, which is what a script parses — there `| wc -l` counts lines of JSON, not
  events, so read `n_matched`. (Under `--follow` the `--json` shape differs; see
  [below](#following-a-running-daemon--f---follow).)
- **stderr** carries the operator notice: the resolved window, the active filter, the
  match count, and — when nothing came back — *which* empty state it was. An empty
  stdout is never an ambiguous silence: it tells you whether there is no log file yet,
  an empty one, or simply no matching event. A missing log is a normal cold state on a
  fresh install, not an error, so the verb says so and exits `0`.

Because the window is stated on stderr, a redirect like `sessiometer log --since 1h >
audit.txt` keeps the lines but not the record of which window produced them. Use
`--json`, whose `window` object travels with the data, when that provenance matters.

`log` is the raw-lines counterpart to `sessiometer reliability`, which reads the same
file but only to fold it into SLIs. Run `sessiometer log --help` for the full usage.

> **Piping moves it somewhere less private.** The log is `0600` on disk and, as described
> above, identifies accounts by the label you chose — which may be your email.
> `sessiometer log` does not redact (that is the point: what it prints is what the file
> says), so treat what you pipe, paste, or attach accordingly.

#### Following a running daemon (`-f`, `--follow`)

Watching a daemon that is *currently running* is the case a reader exists for, and a
one-shot render does not serve it. `--follow` prints the log as usual and then keeps
printing lines as they are appended, until you interrupt it with Ctrl-C:

```sh
sessiometer log --follow
```

The two filters deliberately do **not** behave the same way here:

- **`--since` bounds the initial catch-up only.** It is a statement about the log's
  *history*, and a line that arrives while you are watching is recent by definition.
  So `--follow --since 1h` backfills the last hour and then streams everything that
  follows.
- **`--event` keeps filtering every streamed line**, because it is a content filter with
  no time in it. `--follow --event swap` shows you swaps and only swaps, live.

The log has no rotation of its own, but an operator or an external tool (`newsyslog`)
can still rotate it — and the follower survives that, a truncation, and a rewrite in
place. If the file is truncated, rewritten, or moved aside and replaced, it says which
one happened on stderr and resumes from the new content's start rather than stalling or
reprinting what it already showed you. If the log does not exist yet, it waits for the
daemon to create it instead of exiting: a follow started before the first write is a
normal cold start, not an error.

One caveat on rotation: a running daemon holds its own log open for its whole run, so it
keeps appending to the **moved-aside** file until it restarts. The follower reattaches to
the *path* — which is where the events will be once the daemon does restart — so expect
the stream to go quiet until then.

With `--json`, a follow is **JSON Lines** — one complete record per line, each carrying
its own `schema` — rather than the single document the one-shot form prints. A stream has
no last record, so its `records` array could never be closed; reading it line by line is
what lets a consumer act on an event the moment it arrives:

```sh
sessiometer log --follow --json | jq --unbuffered -r .line
```

#### The two channels (`--channel`)

The daemon writes two streams, and they are **not** the same kind of thing:

| `--channel` | File | What it is |
| --- | --- | --- |
| `event` *(default)* | `~/Library/Logs/sessiometer/sessiometer.log` | The durable event log. Every field is a handle, an enum, a number or a timestamp by construction, and the whole channel is redaction-checked in CI. |
| `diag` | `~/Library/Logs/sessiometer/daemon.err.log` | A launchd-managed daemon's raw stderr, where the per-poll / per-tick / lifecycle diagnostics land. |
| `all` | both | Interleaved in timestamp order. |

> **The diagnostic channel is not redaction-checked.** It is raw process stderr, so
> besides the diagnostics it can carry anything the daemon printed there — including
> **panic output**, which never passed the checks the event log's every field passes by
> construction. That is why it is strictly opt-in: a bare `sessiometer log` never reads
> it, `--channel all` is never the default, and the verb says so on stderr whenever you
> do ask for it.

Under `--channel all`, each file keeps its own internal order — so a panic backtrace
stays contiguous — and ties put the event line first. A diagnostic line with no
timestamp of its own (raw stderr, a panic payload) is placed at the timestamp of the
nearest line before it, so it lands where it actually happened rather than being dropped
as unplaceable. `--channel all` is not available with `--follow`: ordering a *live* merge
would mean holding each new line back until the other channel produced one at least as
late, which on a quiet channel never happens. Follow one at a time.

##### Turning diagnostics on for a background daemon

A launchd-managed daemon runs `run --managed` with no `-v`, so **by default it writes no
diagnostics at all** — `--channel diag` on a fresh install correctly reports that none
exist. To turn them on, set `verbose` under `[tunables]` in the config
(`sessiometer config path`) and restart the daemon:

```toml
[tunables]
verbose = true
```

```sh
sessiometer daemon restart
sessiometer log --channel diag
```

No plist editing — `sessiometer service install` would overwrite it anyway. Like every
other tunable this is **not hot-reloaded**: it takes effect at the *next* daemon start,
which is what the restart above is for. The knob is scoped to the managed daemon, so an
interactive `sessiometer run` is unaffected; use `-v` there. `-v` still wins over the
knob on either.

## Switching the active account

Switch the active account **on demand**, without waiting for the daemon to swap
on a usage trigger — the same out-of-band swap, run once by you:

```sh
# Switch to `spare` now (resolves by list label OR account-uuid):
sessiometer use spare

# Force the switch, overriding the pre-swap checks below:
sessiometer use spare --force

# Advance to the next account in the swap chain without naming it:
sessiometer use --next
```

By default `use` runs a **pre-swap gate** and refuses — with a specific reason
and **without writing anything** — when the target is not a sound destination:
its weekly window is exhausted, it is quarantined (out of rotation), or a swap
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

`--next` swaps you along the chain **without naming a target**: it takes the account
the daemon has already chosen as its next swap candidate — the very one
[`status`](#checking-status) prints on its `next swap:` line, picked by the
same rules the automatic rotation uses. It prints which account that turned out to be,
and why, before it swaps. Pass `--force` alongside it and the two compose exactly as
they do for a named target: `--next` only supplies the handle, and adds no gate of its
own. `--next` and an explicit `<account>` are mutually exclusive — naming one
contradicts the flag, so the pair is rejected rather than silently resolved one way.

Because the candidate is the **daemon's** choice, `--next` needs a daemon running: it
is not something the CLI can work out for itself, so with none reachable it says so and
exits without writing, rather than guessing a target. It also stands down — again
writing nothing — when the daemon reports **no** viable candidate, and then tells you
why the fleet is blocked and when capacity returns, the same relief hint `status`
shows. To override that, name a target and force it: `sessiometer use <account>
--force`.

Otherwise `use` works whether or not the daemon is running: when one is up, the
pre-swap gate reads the **cached** usage the daemon already polled — so `use` makes no
usage request of its own and won't trip a rate limit — and with no daemon it falls back
to a single live check.

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

Accounts resolve by their `list` label **or** their account-uuid, exactly as `use` and
`poke` do. Labels are your own handles and are not required to be unique — if two
accounts share one, `disable`/`enable` refuse rather than guess which you meant, and you
disambiguate with the account-uuid `list` shows. The state is stored in `config.toml`, so
it persists across daemon restarts; `list` and `status` mark a parked account as
`disabled`. A running daemon picks up the change in its live rotation right away — no
restart needed.

## Removing an account

Delete an account from the rotation **and erase its stashed credential** — the
destructive counterpart to `disable`. Where parking keeps the entry and its
stash, removal drops the roster entry and deletes the account's keychain stash,
so it is gone for good:

```sh
# Drop `work` from the rotation and erase its stash:
sessiometer remove work
```

Accounts resolve by their `list` label **or** their account-uuid, exactly as `use` and
`poke` do. If two accounts share a label, `remove` **refuses** rather than guessing which
you meant — pass the account-uuid `list` shows for the one you want. That refusal matters
most here: removing the wrong account costs a re-login, and nothing in `sessiometer` puts
it back. The roster entry is removed from `config.toml` **first**, then the stash is
deleted — so an interrupted removal leaves at most an unreferenced (harmless) keychain
item, never a roster entry pointing at a missing stash. A running daemon picks up the
removal in its live rotation right away — no restart needed.

Removing the **active** account is allowed: it touches only `sessiometer`'s
roster entry and stash, never the live `Claude Code-credentials` item, so the
running Claude Code session keeps working. The daemon then simply resolves no
active account (polling only, never swapping) until you `capture` another account
or sign in again.

## Logging in / re-authenticating

Revive a **`dead`** account — or one whose [refresh-token deadline has
`lapsed`](#the-refresh-token-deadline-expiry) — by re-authenticating it; the same verb
onboards a new account. Those are the two ways a credential stops being recoverable by
refresh: a refresh that **failed** (🔴 `dead`), and a deadline that **passed** (`lapsed`
in `EXPIRY`) — the same fact, observed after a refresh fails and known before one is
tried. Neither is recoverable by `poke` or `[refresh]`; both are re-established here.

`sessiometer login` runs `claude /login` inside an **isolated, throwaway
`CLAUDE_CONFIG_DIR`** (the same isolation `poke` uses), so the browser OAuth
handoff never touches the live `Claude Code-credentials` item a running session
reads. It harvests the credential Claude Code writes there and lands it in the
rotation — stashing it and adding or refreshing its roster entry. Whether that
re-login also becomes **active** is gated to preserve whichever account is
currently live: it re-points the canonical `Claude Code-credentials` item to the
fresh credential under the swap lock **only** when you re-authenticate the account
that is already active (re-auth in place), or when no account is active yet
(bootstrap). Logging in a **different** account while one is active adds or revives
it **without** touching the active slot — the live session keeps working. If that
account was **quarantined** (🟠 `degraded`, or 🔴 `dead` if a refresh proved it
unrecoverable), the re-login also **clears the quarantine at once** — `login` signals
the running daemon to return it to the rotation the moment the fresh credential lands,
instead of waiting on the daemon's slower periodic recovery sweep. There is still no
swap; the active account stays live.
Switch to it with [`sessiometer use`](#switching-the-active-account) when you're ready:

```sh
# Re-authenticate (or onboard) an account; the label is optional:
sessiometer login spare
```

The optional `<label>` names a **new** account — omit it and the label is
auto-derived from the account's `account_uuid` (exactly as non-interactive
`capture`); a re-login of an already-rostered account keeps its existing label
unless you pass a new one. Name it with the account's **email** or any **nickname**
you prefer (`work`, `spare`): the label is written verbatim into the daemon's durable
[event log](#watching-the-daemon-diagnostics), so an operator-authored email is
permitted (a *provenance-scoped* waiver — #444) while an unauthored one never appears
(issue #15). A non-PII nickname stays a fine option, no longer a requirement.
(`login` takes the label as an explicit argument; the editable email pre-fill is
`capture`'s interactive prompt — #447.)
`login` needs a real terminal and the `claude` binary on your **login shell's** `PATH` —
not the `PATH` of the shell you run it from, which is ignored (or `$CLAUDE_BIN` /
[`[login].claude_bin`](#login), which override both; see
[Which `PATH` the CLI resolves `claude` on](#which-path-the-cli-resolves-claude-on));
tune its timeout in the [`[login]`](#login) block. On success it
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
account's `list` label, never a token. It needs the `claude` binary on your **login
shell's** `PATH` — not the `PATH` of the shell you run it from, which is ignored (or
`$CLAUDE_BIN` set to its absolute path, which overrides both). See
[Which `PATH` the CLI resolves `claude` on](#which-path-the-cli-resolves-claude-on).

## Refreshing parked credentials automatically

`poke` is the manual trigger; the daemon can also run that same refresh **on a
cadence** so a spare is always ready to swap to without a stale-token round-trip.
The periodic tick is **on by default** and runs entirely in the daemon's
**idle path** — between polls, off the poll → usage → swap seam — so it never
competes with the work that keeps the active session alive. Each refresh happens in
an isolated `CLAUDE_CONFIG_DIR` (exactly as `poke`), so the live
`Claude Code-credentials` item is never touched, and the **active account and the
imminent swap target are always excluded** — it refreshes parked accounts only (the
active account is instead kept warm **in place**, see
[below](#keeping-the-active-credential-warm-in-place)). A refresh failure (or a cycle
that overruns its timeout) is non-fatal: it is logged, redacted, and the daemon
returns to polling.

Tune it (or turn it off) in the `[refresh]` table of `config.toml`:

```toml
[refresh]
enabled = true            # on by default; set false to leave the tick wholly inert
accounts = []             # parked accounts by `list` label or account-uuid; [] = all near-expiry
cadence_secs = 3600       # seconds between ticks AND the near-expiry horizon (60..=86400)
idle_after_secs = 60      # idle seconds (no poll/swap) required before a refresh fires (0..=3600)
timeout_secs = 90         # whole-cycle bound for one account's refresh (10..=600)
proactive_keep_warm = false  # pre-emptively refresh the ACTIVE token before expiry; off by default
                             # (it rotates the live shared credential each cadence) — the active
                             # account is instead kept warm reactively on a 401. See below.
# claude_bin = "/absolute/path/to/claude"   # overrides $CLAUDE_BIN + your login shell's PATH;
                                            # omit (or leave empty) to resolve normally
```

An account is **due** when its stored token would expire within one `cadence_secs`
of now — i.e. it would not survive until the next tick — so the cadence doubles as
the near-expiry horizon (no second knob). `[refresh]` config changes take effect at
the next daemon start. The `claude` binary, however, is resolved **per refresh cycle**
(honoring `claude_bin` → `$CLAUDE_BIN` → your **login shell's** `PATH`), not frozen at
start-up — so a Claude Code auto-update that re-points the binary is picked up on the next
cycle with no daemon restart. A cycle that cannot resolve `claude` records a non-fatal
error and retries next cycle; it never disables the tick.

That third tier is the user-level `PATH`, not the daemon's own. Started by `launchd` the
daemon inherits a bare `PATH=/usr/bin:/bin:/usr/sbin:/sbin` — no `~/.local/bin`, and so no
`claude` at all — so it reconstructs the PATH your terminal would have by running your
login shell once and reading its environment. That PATH is scanned **in your own order**
and the **first** `claude` wins, so a binary you deliberately shadow earlier on your `PATH`
is the one the daemon spawns, exactly as your shell would resolve it. The harvested value
is reused for 60 s (so a sweep runs one login shell, not one per account) while the
directory scan still runs every cycle. If the harvest fails, resolution falls back to the
daemon's own `$PATH` rather than erroring — a failure is never cached, so a shell that is
persistently broken is retried per account rather than reused, which costs time but keeps a
transient hiccup from turning into a minute-long outage. Setting `claude_bin` or
`$CLAUDE_BIN` skips the harvest entirely.

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

### Which `PATH` the CLI resolves `claude` on

The `claude_bin` → `$CLAUDE_BIN` → **login shell's `PATH`** ladder above is not the
daemon's alone. `sessiometer poke` and `sessiometer login` share one resolver with it, so
they resolve `claude` on your **login shell's** `PATH` too — **not** on the `PATH` of the
shell you typed the command into. In a terminal the harvest normally succeeds, and a
successful harvest *replaces* the inherited `PATH` (it never unions with it), so a
shell-local prefix is never consulted:

```sh
# Does NOT pick up /custom/bin/claude — the prefix is ignored, and the `claude` found
# on your LOGIN SHELL's PATH is spawned instead:
PATH=/custom/bin:$PATH sessiometer poke

# Do this instead. $CLAUDE_BIN is the per-invocation override, and it wins outright
# (it also skips the login-shell harvest entirely):
CLAUDE_BIN=/custom/bin/claude sessiometer poke
```

This is deliberate, and it is the reason a union is refused: unioning would let an entry
from the daemon's bare `launchd` `PATH` outrank one of your own, which is exactly the
shadowing the scan order exists to honor. The payoff is that **`poke` predicts the
daemon** — it resolves the same binary the next refresh cycle will, so a working `poke`
alongside a failing daemon can no longer mean "the two found different binaries". That
divergence is what once turned an environment problem into hours of credential
debugging. The trade — a CLI that ignores its own `$PATH` — is recorded in
[ADR-0030](docs/adr/0030-one-resolution-policy-cli-included.md), with the alternatives
and why each was rejected.

`claude_bin` in `[login]` (for `login`) or `[refresh]` (for the daemon), and
`$CLAUDE_BIN` for any of them, all take precedence and all skip the harvest. `poke` has
no `claude_bin` key of its own — `$CLAUDE_BIN` is its only override.

### Keeping the active credential warm in place

The parked sweep above deliberately skips the **active** account — its refresh writes
each account's *stash*, and rotating the active token there would strand the fresh
value where no live session reads it. But an idle machine left overnight can let the
active token lapse and, on the next poll, a `401` is mistaken for a dead credential —
starting a false-death logout cascade. To close that gap, whenever `[refresh]` is
enabled the daemon **also keeps the active account's canonical token warm in place** —
**reactively by default, and proactively when you opt in**:

- **As a reactive backstop (default)** — if the active account returns a `401` with a
  still-live refresh token, the daemon refreshes it in place and re-polls **before** the
  `401` counts toward the dead-credential streak. Only a genuinely dead credential (an
  empty refresh token, or a refresh that reports `dead`) advances the streak — so a
  truly-dead active account still quarantines and the emergency swap to a live spare is
  preserved. This is the layer that prevents active-token expiry mid-use.
- **Proactively (opt-in, `proactive_keep_warm = true`, off by default)** — before the
  active token nears expiry, the daemon mints a fresh token (the same isolated spawn the
  parked sweep uses, on a *copy* of the canonical blob) and **promotes it to the canonical
  `Claude Code-credentials` item** a live session reads — refreshing it *ahead* of any
  `401`. This is **off by default** because it rotates the live shared credential on every
  cadence, and that churn is a window for a rare multi-writer credential scrub; the reactive
  backstop above (plus the daemon's autonomous recovery of a scrubbed credential) covers an
  actively-used account without it. Enable it if you want the active token refreshed ahead
  of expiry rather than on the first `401`. (See
  [`docs/findings/0476`](docs/findings/0476-keep-warm-scrub-risk-tradeoff.md) for the
  churn-vs-scrub tradeoff behind this default.)

The in-place canonical write is serialized against account swaps (the same single-
writer lock, an atomic keychain update, re-checked each cycle), so it can never tear a
swap. Each account's keep-warm timing is **staggered** by a stable per-account offset,
so a roster that logged in together does not all reach expiry — and refresh — in
lockstep. The reactive backstop needs no extra configuration — it rides the
`[refresh].enabled` switch; the proactive layer is the one opt-in
(`proactive_keep_warm = true`) and reuses `cadence_secs` as its near-expiry horizon.
Every keep-warm firing is logged as a redacted `event=keep_warm` line
(`trigger=proactive`/`reactive`, the classified outcome, and whether the refresh token
rotated), never a token or an *unauthored* email (an operator-authored email label may
appear — #444).

## Configuration

`sessiometer` keeps all of its state in one TOML file,
`~/Library/Application Support/sessiometer/config.toml` (or
`$XDG_CONFIG_HOME/sessiometer/config.toml` when `$XDG_CONFIG_HOME` is set). The
**roster** — the `[[account]]` entries — is managed for you by `capture`, `login`,
`remove`, and `disable`/`enable`; don't hand-edit it. The tuning blocks below **are**
safe to hand-edit: every key is optional and falls back to the default shown, an
out-of-range value is rejected at load with a message naming the key, and a hand-edited
change takes effect on the next daemon restart (`sessiometer daemon restart`) — the tuning
blocks are read once at start-up and frozen for the process lifetime, so a running daemon
does not pick them up live (unlike the managed roster, which hot-reloads). The generated
file also carries an inline comment on every key, so your own `config.toml` doubles as a
reference.

### `[tunables]`

The primary hand-editable block — the poll cadence and the swap thresholds.

| Key | Meaning | Range | Default |
|-----|---------|-------|---------|
| `poll_secs` | Seconds between re-polling a given account — the per-account cadence and the base of the rate-limit back-off. | `5..=3600` | `300` |
| `exhausted_poll_secs` | Widened re-poll cadence for an **out-of-rotation** peer — one that is weekly- or session-exhausted. Its usage can only change when its server-side window resets (a time the daemon already knows) or on a rare out-of-band reset, so re-polling it every `poll_secs` wastes a request; this is the **ceiling** of its slow-poll window, pulled **earlier** when a known reset lands sooner. The **active** account is never slow-polled. | `poll_secs..=86400` | `3600` |
| `cooldown_secs` | Seconds to wait after a swap before another is allowed — the swap-pacing floor. Tunable **above** a non-zero minimum but never down to zero, so rapid-fire account flapping can't be configured on. | `5..=3600` | `60` |
| `session_ceiling` | The session-usage **ceiling** (%) the active account must not cross — *not* a fire-at level. Both swap estimators (reactive `observed`, projected `observed + velocity × H` — one predicate, **not two knobs**) derive their fire point **backward** from it, covering the larger unseen window (`observed ≥ ceiling − tail_margin − velocity × max(poll_gap, H)`), so the account *lands below* the ceiling even after its post-swap committed tail (the `tail_margin` is 6 pp, chosen just above the measured max tail of +5 pp — in-flight work keeps billing the parked account). The reactive arm looks ahead over the **measured p90 re-observation gap (313 s floor)**, so the default `95` is a conservative lever — **99 is reachable** (raise it to spend the margin as runway). See [ADR-0023](docs/adr/0023-session-trigger-ceiling-semantics.md) and [ADR-0024](docs/adr/0024-reactive-lookahead-gap-percentile-max-window-coverage.md). | `50..=99` | `95` |
| `weekly_ceiling` | The weekly-usage **ceiling** (%) the active account must not cross — *not* a fire-at level. The swap fires **backward** from it at `ceiling − 1 pp`, so the parked account *lands below* the ceiling after its post-swap committed tail; the margin is 1 pp rather than the session dimension's 6 pp because the same committed tail is a far smaller fraction of the weekly budget. Independent of `session_ceiling` (no cross-field constraint, typically higher); a swap fires when *either* dimension reaches its own fire point. See [ADR-0025](docs/adr/0025-weekly-trigger-ceiling-semantics.md). | `50..=99` | `98` |
| `target_max_session_usage` | Swap-target **reserve**: only swap **to** an account whose session usage is below this percent — the most-full a target may be to receive the active session, so it keeps runway. *Raising* it toward `session_ceiling` admits busier targets (equal is inert); `0` admits nothing (proactive swaps off). Not a swap-away level; that is `session_ceiling`. A **dead** active ignores it entirely and escapes to any live account. When nothing sits below it the daemon holds and logs `all_exhausted cause=session`. | `0..=session_ceiling` | `80` |
| `monitor_401_n` | Consecutive non-scope `401`s before an account is treated as dead and quarantined. | `1..=20` | `3` |
| `monitor_recovery_m` | Consecutive recovery-probe successes before a quarantined account whose own token recovers (without a re-login) is returned to the rotation. | `1..=20` | `2` |

The ranges and defaults above are exactly the ones enforced in
[`src/config.rs`](src/config.rs) (`Config::validate` and the `DEFAULT_*` constants) —
the single source of truth this table is drawn from, so it stays in step with the code.

### `[jitter]`

Per-cycle randomization added to a tunable, drawn fresh each cycle and clamped back to
the tunable's range, so polls and swaps decorrelate across accounts and cycles. One
optional entry per tunable — `poll`, `session_ceiling`, `weekly_ceiling`, `cooldown` —
each an inline table whose `kind` is `"none"`, `"uniform"` (with a `spread`), or
`"normal"` (with a `stddev`); magnitudes are TOML floats. Each key is named for the
tunable it jitters. Only `poll` jitters by default:

```toml
[jitter]
poll = { kind = "normal", stddev = 60.0 }   # default: normal, ~20% of poll_secs
session_ceiling = { kind = "none" }         # session_ceiling / weekly_ceiling / cooldown default to none
```

### `[login]`

Settings for `sessiometer login`, the interactive re-auth verb.

| Key | Meaning | Range | Default |
|-----|---------|-------|---------|
| `timeout_secs` | Seconds bounding one whole interactive login capture — longer than a refresh, since it waits on a human completing a browser OAuth handoff. | `60..=600` | `180` |
| `claude_bin` | Absolute path to the `claude` binary to spawn, overriding `$CLAUDE_BIN` and your login shell's `PATH`. Omit (or leave empty) to resolve normally. Note that `login` resolves on your **login shell's** `PATH`, not the one you invoke it from — see [Which `PATH` the CLI resolves `claude` on](#which-path-the-cli-resolves-claude-on). | — | unset |

### `[credential]`

How far ahead the daemon looks at each account's **refresh-token deadline** when
classifying it for the [`EXPIRY` column](#the-refresh-token-deadline-expiry).

| Key | Meaning | Range | Default |
|-----|---------|-------|---------|
| `expiry_horizon_secs` | Seconds of lookahead over `refreshTokenExpiresAt`. A deadline falling inside this window classifies as *within* the horizon (bracketed, `[6d21h]`, and tinted yellow); one further out classifies as *beyond* it (bare, and dimmed). | `86_400..=7_776_000` (one day to ninety days) | `604800` (seven days) |

This bounds the **lookahead only** — it is emphatically not an assumed refresh-token
lifetime. The daemon always reads the deadline from the credential and never infers one
from a constant, which is why an account with no observable deadline reads `—` instead of a
computed guess.

The knob has no "off" setting, and the one-day floor is deliberate: a zero horizon would
quietly reduce the column to reporting only *already-lapsed* credentials — the
after-the-fact signal the foresight exists to get ahead of. Shorten it for less lookahead;
the axis itself stays on. It governs **classification** only, and never feeds a swap or
poll decision — expiry is surfaced, not acted upon. It is also independent of
[`[refresh]`](#refreshing-parked-credentials-automatically), and keeps classifying whether
or not that periodic refresh is enabled — an account with refresh off is if anything more
exposed to a quiet lapse.

### Other blocks

- **`[refresh]`** — the daemon's periodic parked-credential refresh; documented under
  [Refreshing parked credentials automatically](#refreshing-parked-credentials-automatically).
- **`[stats]`** — retention horizons for the usage-stats store.
- **`[migration]`** — the KDF cost and conflict-policy defaults for `export` / `import`.

`[stats]` and `[migration]` are hand-editable too; their keys, ranges, and defaults
are documented by the inline comments in the generated `config.toml`.

### Inspecting the config (`config path` / `validate` / `show`)

Because the tuning blocks are read once at start-up and frozen for the process lifetime
(and the daemon only ever echoes its effective config once, to stderr), there is
otherwise no way to see what a running daemon actually loaded — a hand-deleted `[tunables]`
block, for instance, silently falls back to defaults with nothing to show for it. Three
**read-only** `config` verbs make it observable (none of them writes the file, touches the
daemon, or changes any state):

```console
$ sessiometer config path
/Users/you/Library/Application Support/sessiometer/config.toml
```

`config path` prints the resolved `config.toml` location (honouring `$XDG_CONFIG_HOME`),
so you always edit — or `cat` — the exact file the daemon reads.

```console
$ sessiometer config validate
/Users/you/Library/Application Support/sessiometer/config.toml is valid (2 accounts)
```

`config validate` parses and validates the file **without running** — the same checks the
daemon applies at load. It reports the documented error classes and exits non-zero on any
of them, so it drops into a pre-flight check: a typo'd/unknown key (e.g. `poll_secss`), an
out-of-range value (`poll_secs must be in 5..=3600`), or `target_max_session_usage > session_ceiling`.

A **valid** file may still print a non-fatal **advisory** (it does not change the exit code):
if `target_max_session_usage` sits above the *peak-velocity runway bound* — the highest reserve
that still leaves a swapped-to account runway when it is climbing at the assumed peak session
velocity over the swap lookahead (`near_limit_poll_secs` / `session_velocity_horizon_secs`
together) — `config validate` names the bound and suggests lowering the reserve or narrowing the
lookahead. The shipped defaults sit in this band deliberately (the tail margin and the sub-SLO
default ceiling are the guard), so it is a tuning note, not an error. Only the *unsatisfiable*
extreme — a lookahead so wide that **no** reserve keeps runway at peak velocity — is a hard load
error.

```console
$ sessiometer config show --origin
# effective configuration
# /Users/you/Library/Application Support/sessiometer/config.toml

[tunables]  (absent — all defaults)
  poll_secs          = 300  default
  session_ceiling    = 95   default
  …
[refresh]
  enabled            = true  from-file
  …
```

`config show` prints the **effective** config — every value the daemon would use, defaults
filled in. With `--origin`, each value is tagged `from-file` or `default`, and a whole
absent `[section]` is flagged, so a silently-defaulted block (the drift above) is visible
at a glance rather than buried in start-up stderr.

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

An artifact embeds the config **text**, so a tunable that was absent when the artifact
was written is absent on import, and takes **today's** default — not the default that
was in force at export time. This is the same absent-key rule the config file follows
everywhere. It is worth knowing for artifacts exported before `target_max_session_usage` became a
default-on `80` (issue #398): they import with the reserve **on**, where the original
machine ran with it off.

**Import version floor.** Because the config travels as text, the *importing* build re-parses it
with its own parser — and that parser refuses any config **key** it does not know, at any nesting
level, not merely a whole unfamiliar section. So an artifact is only as portable as the **oldest**
build that has to read it, and the artifact's own format version does not warn you: it reads `1`
on both sides either way.

There are no releases yet, so this is a floor on **builds**, not on versions — and it moves every
time a rendered config key is added, so it is not a fixed number to memorize. As of 2026-08-09 it
sits at commit `81bd4f2` (`[credential].expiry_cohort_window_secs`). Do not reach for the commit
that added a whole *section*: `[credential]` arrived 14 commits earlier in `6fe3457`, and a build
pinned there still refuses a current artifact over the later key.

**So the rule to follow is the one that cannot go stale: import on a build at least as new as the
one that wrote the artifact.** The two ways of breaking it read differently, which is worth
knowing before you diagnose one. A build older than the floor stops on a bare parser complaint
naming an unexpected key, and no change to `sessiometer` can improve that message — it is the old
build printing it, and it has already been built. A build that is merely older than *the artifact*
stops and states this floor instead (issue #1053,
[ADR-0006](docs/adr/0006-migration-schema-evolution-policy.md)).

## Privacy: no telemetry

`sessiometer` phones home for nothing. It sends **no** analytics, usage
telemetry, crash reports, update pings, or beacons of any kind. The **only**
outbound network request it makes is the read-only per-account usage poll the
swap logic runs — `GET https://api.anthropic.com/api/oauth/usage`, under your own
account's bearer token — so the daemon can tell when an account is near its quota
and swap before exhaustion. Nothing else leaves the machine during normal
operation (start, poll, swap, idle).

This is an architectural guarantee, not just a policy:

- **No HTTP or TLS client is linked.** `sessiometer` pulls in no `reqwest` /
  `hyper` / `rustls` / `native-tls` (the
  [transport rule](CONTRIBUTING.md#system-clis-not-client-crates-the-transport-rule));
  the one usage `GET` rides the system `/usr/bin/curl` at an absolute path. With
  no in-process HTTP/TLS stack and no raw TCP/UDP socket — the daemon's control
  socket is a local Unix-domain socket that never leaves the machine — the process
  has no way to open a second connection.
- **A test enforces it.** The no-other-egress invariant is a capability guard in
  the test suite ([`src/usage.rs`](src/usage.rs), run by `cargo test` in CI): it
  fails the build if an HTTP/TLS/telemetry crate ever enters the dependency graph,
  if a raw TCP/UDP socket appears, or if any network binary other than the
  sanctioned usage-endpoint `curl` is referenced — so a future change cannot
  silently open a telemetry channel.

`sessiometer` also *drives* the official Claude Code CLI (`claude`) to refresh a
parked account's token and to log in; that separate program makes its own network
calls to Anthropic under your credential, exactly as it would if you ran it
yourself. That is the official client's traffic, not `sessiometer` reporting on
you.

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
| `~/Library/Logs/sessiometer/sessiometer.log` | The event log — durable state changes | No — every line passes a CI redaction check; never a token or an *unauthored* email (an operator-authored email label may appear — #444) |

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

1. **Stop the daemon.** If you installed the background LaunchAgent, remove it with
   `sessiometer service uninstall` (this unloads it and deletes its plist). Also stop
   any running foreground `sessiometer run` with `sessiometer daemon stop` (or Ctrl-C
   in its terminal). After that, nothing runs in the background.

2. **Erase the per-account stashes.** The cleanest way is to `remove` each captured
   account, which deletes its `Sessiometer/<account_uuid>` keychain stash:

   ```sh
   sessiometer list               # see the captured accounts
   sessiometer remove <account>   # repeat for each; erases that account's stash
   ```

   `<account>` is a label or an account-uuid. If two accounts happen to share a label,
   `remove` refuses rather than guessing which you meant — pass the account-uuid `list`
   shows for the one you want.

   `remove` never touches the live `Claude Code-credentials` item — even for the
   active account — so your Claude Code session keeps working throughout.

3. **Delete the on-disk state:**

   ```sh
   rm -rf ~/"Library/Application Support/sessiometer"
   rm -rf ~/"Library/Logs/sessiometer"
   # only if `service uninstall` in step 1 did not already remove it:
   rm -f ~/"Library/LaunchAgents/org.sessiometer.agent.plist"
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
- **Rate-limiting and transient errors back off per-account.** When the usage
  endpoint returns `429` (rate-limited) or a `5xx` / network error for an account,
  the daemon widens **that account's** poll spacing instead of re-polling it at the
  fixed interval — an exponential back-off that doubles each consecutive throttled
  cycle (capped at ~1 h) and honours any `Retry-After` the server sends as a minimum
  wait — itself clamped to that same ~1 h ceiling, so a pathological server value
  can't dark an account for longer; a clean poll resets it. The back-off is
  **scoped to the throttled account, not the whole roster** — each account's `429`
  bucket is independent (its token resolves to its own Anthropic org) — so the active
  account and every other account keep polling on their normal cadence, and one
  rate-limited spare never silences the active account's monitoring. The default
  cadence also carries normal jitter so concurrent accounts decorrelate, and on
  start-up the daemon waits a small jittered delay before its first poll so repeated
  restarts don't synchronise a burst of requests.
- **Sustained refresh failures back off per-account.** A parked-credential refresh
  that keeps erroring — a broken `claude` binary, a dead network — is **not** retried
  at the idle floor forever. The daemon widens **that account's** refresh spacing on
  each consecutive error (an exponential back-off from the idle floor to the same
  ~1 h ceiling), skipping the account **whole** while it backs off — no `claude -p`
  spawn, no keychain read — so a persistent failure can no longer storm the machine
  with a refresh every idle floor. The streak is **per-account** and **clears on the
  first clean refresh**, and every other account keeps refreshing on its normal
  cadence. The armed wait is surfaced as `backoff_secs=<n>` on the account's
  `event=refresh` error line.
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
- **A drifted keychain derivation refuses writes (behavioral canary).** Before
  every swap — the daemon before its own swaps, and `use` itself on the
  daemon-down path, `--force` included — sessiometer re-verifies, fresh rather
  than from a boot-time cache, that the keychain item it
  resolves still points at the credential Claude Code is actually using: the
  resolution must be **unique** (exactly one matching item), and the resolved
  credential must not byte-match a **different** account's stash than the account
  Claude Code's own state names active. On such **drift**, credential writes
  (swaps and auto-protection) are refused *before any mutation* — an in-place
  overwrite of a wrong target would clobber an unrelated secret unrecoverably —
  while reads, polling, and `status` stay live; `status` names both accounts in a
  dedicated line. If you diagnose a **false alarm**, set
  `canary_drift_override = true` under `[tunables]` in `config.toml` and restart
  the daemon: swaps then proceed despite the standing drift, and every overridden
  write is logged with `overridden=true`. Zero or multiple matching keychain
  items refuse the same way (there is no unique, safe write target) — with no
  override, since ambiguity has no false-positive story. (The `use --force`
  adopt-target recovery is deliberately outside this gate: it runs only when the
  credential is confirmed gone, so there is no resolved item to cross-check.)

  The same gate also refuses when the resolved item matches **no** stashed
  account *and* is **not in Claude Code's own credential format** — that
  combination means the item is most likely an unrelated secret that happens to
  sit under the derived service name, and an in-place overwrite would destroy it
  unrecoverably. `status` and the menu-bar panel both name this as an
  *unrecognized credential*. Once you have checked what that item actually is and
  confirmed it is safe to replace (for example, a legitimately **new** Claude
  Code credential format that you will re-stash), set
  `canary_nostashmatch_override = true` under `[tunables]` in `config.toml` and
  restart the daemon; every overridden write is logged with `overridden=true`.
  This is a **separate** switch from `canary_drift_override` and neither covers
  the other. An item that matches no stash but *does* parse as a Claude Code
  credential is the ordinary case of a token refreshed in place since it was last
  stashed, so it is never refused and needs no override.

  Both of those checks are **offline**, which leaves one case they cannot see: the
  same account, with Claude Code having quietly moved its credential elsewhere and
  the item sessiometer manages left behind. Everything still lines up — right
  account, right format, matching stash — while the two have gone their separate
  ways. Setting `canary_online_probe = true` under `[tunables]` adds one short
  online check right before a swap writes: it asks the usage endpoint whether the
  credential it is about to overwrite *still works*. Off by default, and while it
  is off no request is made at all. Note what it can and cannot tell you: the
  endpoint does not say **which** account a credential belongs to, only that it
  works — so this catches a left-behind credential that has since **stopped**
  working, and not one that still works. It narrows the gap rather than closing it.

  By default a check that fails — no answer, a timeout, a rejection — is written to
  the log and the swap goes ahead anyway, so a patchy network never turns into a
  stuck rotation. Setting `canary_online_probe_strict = true` instead **refuses** the
  swap (before any write) unless the check comes back clean, **including** when it
  simply could not reach the endpoint — that is the point of the setting. Understand
  what you are buying: this is not a short delay, it is a refusal that repeats for as
  long as the check keeps failing. Weigh it knowing a rejection on its own is weak
  evidence — Claude Code refreshes its access token in place, so a token that has
  merely gone briefly stale is rejected too while being perfectly healthy. That is
  why refusing is something you opt into rather than the default.

  Two escapes keep strict mode from trapping you on a credential that has genuinely
  died. The daemon **skips the check** whenever it has no current reading for the
  account it is swapping away from — its last poll came back empty, or it has not
  polled that account yet (right after a restart, say). The first case is precisely
  the condition its automatic escape swaps fire on, so a check that fails for the same
  reason must not block them; the second is a state it simply cannot vouch for. It
  likewise skips while that account is inside a rate-limit hold
  (`verdict=uninformative` in the log records any of these). And
  `sessiometer use --force <account>` skips the check outright, whether or not the
  daemon is running (`verdict=overridden` in the log records that you did). The
  forced escape is the one that always applies: the skip above cannot help when the
  daemon's last poll *succeeded* and the credential died in the window since. Note
  that `--force` skips **only** this online check: the offline checks above still
  refuse, with or without it, because those protect against destroying an unrelated
  secret.
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

**macOS is the only supported build target.** The crate does not compile for Linux or
Windows today, and no CI job attempts it — every job that builds, tests, or lints the
crate runs on a macOS runner. That is a stated position, not an oversight: the daemon is
built on `launchd`, the login keychain, and the passwd database, and the menu-bar app on
SwiftUI, TCC, and Developer ID notarization. Cross-platform support is tracked future
work — recon (#40) first, then the credential-store seam (#25), the per-OS mechanisms
(#26/#27/#28), and packaging plus CI (#29). See
[ADR-0029](docs/adr/0029-macos-is-the-only-supported-build-target.md).

```sh
cargo build --release
./target/release/sessiometer --help
```

### Install with Homebrew (CLI / headless channel)

The [`Formula/sessiometer.rb`](Formula/sessiometer.rb) Homebrew formula builds the
crate from source and installs the `sessiometer` CLI + daemon — the headless,
scripting channel for terminal and automation use. It is locally compiled, with no
notarization or code-signing; those belong to the parallel GUI channel — the notarized
`.app` (#171: sign + notarize + staple) delivered as a Homebrew cask (#172), not yet
shipped. This channel is off that critical path.

The crate is still pre-release (no tagged release yet), so tap the formula repo and
install from `HEAD` (the `main` branch):

```sh
brew tap sessiometer/tap
brew install --HEAD sessiometer/tap/sessiometer
```

That compiles the crate — with the committed `Cargo.lock`, via `--locked`, for a
reproducible build — and puts `sessiometer` on your `PATH`. From there the
swap/monitor loop runs headless: `sessiometer run` for a foreground daemon,
or `sessiometer service install` to keep one running at login (see
[Quickstart](#quickstart)). Once a stable release is tagged, the formula gains a
`url` + `sha256` stanza and also installs without `--HEAD`.

> **Unofficial.** `sessiometer` is not affiliated with or endorsed by Anthropic (see the
> [notice at the top of this README](#sessiometer)) and is distributed under the
> [MIT license](LICENSE). "Claude Code" is referenced only nominatively.

The macOS menu-bar app lives in [`apps/menubar/`](apps/menubar/) — a Swift/XcodeGen
sibling to the Rust crate at the repo root, not a Cargo workspace member (see
[ADR-0010](docs/adr/0010-macos-app-repo-topology.md)).

## Support

`sessiometer` is free and MIT-licensed. If you find it useful, you can support
its continued development through GitHub Sponsors —
**[github.com/sponsors/alexey-pelykh](https://github.com/sponsors/alexey-pelykh)**.
Sponsorship is entirely optional and never gates any functionality; every
feature remains available under the [MIT license](LICENSE).

## License

[MIT](LICENSE) © 2026 Oleksii PELYKH
