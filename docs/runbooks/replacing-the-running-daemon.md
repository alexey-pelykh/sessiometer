---
title: Replacing the running daemon
created: 2026-09-06
status: accepted
source: docs/requirements/daemon-diagnostic-integrity.md
---

# Runbook: replacing the running daemon

**`cargo build` replaces nothing that runs.** It writes `target/<profile>/sessiometer`, and on a
machine with the menu-bar app installed nothing loads that path. This runbook is the procedure that
does replace it, and the check that tells you it worked.

Written for a **contributor deploying to their own machine by hand**. It is not a release process —
see § What this does not cover.

## Why `cargo build` deploys nothing

The daemon the menu-bar app runs is an **`SMAppService` agent living inside the app bundle**. The
app registers a plist it ships as a bundle resource, whose `BundleProgram` points at the daemon
staged next to it:

| | |
|---|---|
| Registered by | the app, via `SMAppService.agent` — not `launchctl` |
| launchd label | `org.sessiometer.agent` (`AGENT_LABEL`, `src/service.rs:61`) |
| Bundled plist | `Contents/Library/LaunchAgents/org.sessiometer.agent.plist` (`apps/menubar/project.yml`) |
| Program | `Contents/Helpers/sessiometer`, inside the same bundle |
| Parent bundle | `org.sessiometer.menubar` |

Two consequences, and both are what the `cargo build` mental model gets wrong:

1. **There is no `~/Library/LaunchAgents` plist to re-point.** The plist is sealed into the app
   signature; the only way to change what it points at is to rebuild the bundle.
2. **A Debug build embeds no daemon at all.** `apps/menubar/scripts/embed-daemon.sh` is the
   `postBuildScript` that builds and `lipo`s the daemon into `Contents/Helpers/`, and it is
   **Release-only** — it prints `skip for CONFIGURATION=Debug` and exits 0. So the Debug
   `xcodebuild` run in `CLAUDE.md` § Menu-bar app builds the app and no daemon.

So a `cargo build`, however green, changes nothing that serves.

## First: which of the two owners are you replacing?

`org.sessiometer.agent` is **one label with two possible owners**, deliberately — the Rust CLI and
the app can each register it, and the app yields when the CLI already owns it (the two-owner
invariant, `apps/menubar/Sources/LoginItemModel.swift`).

```sh
sessiometer daemon status
ls ~/Library/LaunchAgents/org.sessiometer.agent.plist   # CLI-managed iff this exists
```

- **That plist exists → CLI-managed.** This runbook does not apply. `sessiometer service install`
  renders the plist for *the binary that runs it*, so re-pointing it is a `cargo build` plus a
  re-`install`. See `README.md` § Running in the background.
- **That plist does not exist, and a daemon is running managed → app-managed.** Continue below.

Getting this wrong is cheap but confusing: rebuilding the bundle while the CLI owns the label
replaces a daemon that is not the one serving.

## Prerequisites

- **Full Xcode plus `xcodegen`** — Command Line Tools alone is not enough (`CLAUDE.md`
  § Menu-bar app).
- **The Developer ID signing identity the script pins.** `apps/menubar/scripts/release-macos.sh`
  hardcodes one certificate SHA-1 and signs unconditionally, before its `--sign-only` exit. A
  contributor without that identity in their keychain cannot complete this procedure; `codesign`
  fails and the script stops.

## The hazard: the script removes the directory you are probably running from

`release-macos.sh` starts by deleting the Xcode build-products directory:

```
rm -rf .build/Build/Products/Release        # apps/menubar/scripts/release-macos.sh:21
```

That directory is also **where the script leaves the finished `Sessiometer.app`**, so if you
launched the app from a previous run of this script — the normal case — that is the bundle you are
running, and it is about to be removed out from under the live process.

**Quit the app before running the script.** Not doing so leaves a running app whose bundle no
longer exists on disk, which is not a state worth debugging.

## The procedure

```sh
cd apps/menubar && ./scripts/release-macos.sh --sign-only
```

`--sign-only` stops after signing, because CI notarizes separately with an ASC API key
(`apps/menubar/scripts/release-macos.sh:4`). It is matched positionally against `$1`
(`:8`), so it **must be the first argument** — anywhere else it is silently ignored and the script
proceeds to notarize.

The steps in between are the script's, not this document's: read
[`apps/menubar/scripts/release-macos.sh`](../../apps/menubar/scripts/release-macos.sh) rather than a
paraphrase of it here. It ends with the rebuilt, signed bundle at
`apps/menubar/.build/Build/Products/Release/Sessiometer.app`.

Then get launchd onto the new executable — the registration is stale until you do, because the
bundle's `Contents/Helpers/sessiometer` is re-`lipo`ed on every Release build and `SMAppService`
requires re-registration when the registered executable changes. **Either**:

```sh
open apps/menubar/.build/Build/Products/Release/Sessiometer.app
```

which fires `reconcileDaemonAgentRegistration()` at launch — it unregisters before re-registering,
which is what repairs the stale registration
(`apps/menubar/Sources/LoginItemModel.swift`) — **or**, if the app is already running:

```sh
launchctl kickstart -k gui/$(id -u)/org.sessiometer.agent
```

## Confirm the build you just deployed is the one serving

The daemon stamps its own identity into the event log at startup, so this is a read rather than an
inference:

```sh
sessiometer log | grep daemon_build | tail -1
```

The line's shape (`Event::DaemonBuild`, `src/observability.rs`):

```
ts=<rfc3339> event=daemon_build version=<crate version> exe=<path> exe_size=<bytes> exe_mtime=<rfc3339>
```

**`exe_mtime` is the field that discriminates**, because it is observed from the file at runtime
rather than baked in at compile time — `version` and even `exe` are identical across two builds of
the same source at the same path, and `exe_mtime` is not. Compare it against the bundle you just
built:

```sh
date -u -r "$(stat -f '%m' \
  apps/menubar/.build/Build/Products/Release/Sessiometer.app/Contents/Helpers/sessiometer)" \
  +%Y-%m-%dT%H:%M:%SZ
```

Three things that make a match look like a mismatch:

- **The log is UTC; `ls -l` prints local time.** Compare like for like. Note the shape of the
  command above: `stat` yields a raw epoch and `date -u` converts it. The shorter-looking
  `stat -f '%Sm' -t '%Y-%m-%dT%H:%M:%SZ'` is a **trap** — `-t` formats in the *local* zone, so it
  prints local time wearing a `Z`, and an hours-wide mismatch then reads as a failed deploy.
- **`exe` is percent-encoded**, so the whitespace-free field grammar holds even for a path
  containing a space. Decode before comparing paths by eye.
- **A field the daemon could not resolve renders `unavailable`**, never an omission and never a
  fabricated instant. `exe_mtime=unavailable` means the daemon looked and could not tell — it is
  not evidence that the deploy failed, but it does mean this check cannot answer the question.

**No new line at all** is itself informative. The stamp is emitted on every start **that acquires
the single-instance lock** (`src/cli.rs`), before the socket bind — so a restart that found another
daemon already holding the lock returns before stamping. If the timestamp did not move, suspect a
daemon you did not replace still running, not a missing log line.

A worked example, from the deploy this runbook was written against (2026-09-05): after
`release-macos.sh --sign-only` and a relaunch, the event log carried `exe_mtime=2026-09-05T17:38:18Z`,
matching the rebuilt bundle's own mtime once both were read in UTC.

## What this does not cover

**There is no release pipeline here, and this runbook does not propose one.** Single operator, one
machine, deploying by hand. Notarization, stapling and distribution are the script's non-`--sign-only`
path and CI's job, not steps an operator runs from this document.

## Known limitation

This runbook documents a script's behaviour, and **nothing in this repo reconciles the two**. No CI
job reads this file against `release-macos.sh`. Citing the script rather than restating it bounds
the rot — a moved line number or a renamed flag is visible at the citation — but it does not stop
it. Accepted deliberately; adding a reconciliation gate would be a change to the gates themselves,
argued on its own.
