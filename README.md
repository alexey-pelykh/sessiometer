# sessiometer

Manage multiple Claude Code accounts on macOS. `sessiometer` polls each
account's usage quota and swaps the active credential out-of-band before an
account is exhausted, so a long session keeps running by rotating across
accounts.

> **Status:** early scaffold (`0.1.0`, first workable slice). The subsystems
> behind the CLI are still being implemented — see the
> [open issues](https://github.com/alexey-pelykh/sessiometer/issues).

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

# 3. Check the roster and the last swap at any time:
sessiometer status
```

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
