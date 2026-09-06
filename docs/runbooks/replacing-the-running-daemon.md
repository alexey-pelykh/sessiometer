---
title: Replacing the running daemon
created: 2026-09-06
status: draft
source: docs/requirements/daemon-diagnostic-integrity.md
---

# Runbook: replacing the running daemon

**`cargo build` replaces nothing that runs.** It writes `target/<profile>/sessiometer`, and on a
machine where the menu-bar app owns the daemon, nothing loads that path. This runbook is the
procedure that does replace it, and the check that tells you it worked.

**Run every command below from the repository root.** No block changes your working directory —
`release-macos.sh` resolves its own location from `$0`, so it does not need you to `cd` into
`apps/menubar` first. Every path you are asked to *run* is repo-root-relative; the one quoted from
inside the script (§ The hazard) is relative to `apps/menubar`, as that script's own working
directory is.

Written for a **contributor deploying to their own machine by hand**. It is not a release process —
see § What this does not cover.

## Why `cargo build` deploys nothing

The daemon the menu-bar app runs is an **`SMAppService` agent living inside the app bundle**. The
app registers a plist it ships as a bundle resource, whose `BundleProgram` points at the daemon
staged next to it:

| Property | Value |
|---|---|
| Registered by | the app, via `SMAppService.agent` — not `launchctl` |
| launchd label | `org.sessiometer.agent` (`AGENT_LABEL`, `src/service.rs:61`) |
| Bundled plist | `Contents/Library/LaunchAgents/org.sessiometer.agent.plist` (`apps/menubar/project.yml`) |
| Program | `Contents/Helpers/sessiometer`, inside the same bundle |
| Parent bundle | `org.sessiometer.menubar` |

Two consequences, and both are what the `cargo build` mental model gets wrong:

1. **This agent has no `~/Library/LaunchAgents` plist to re-point.** Its plist is sealed into the
   app signature; the only way to change what it points at is to rebuild the bundle.
2. **A Debug build embeds no daemon at all.** `apps/menubar/scripts/embed-daemon.sh` is the
   `postBuildScript` that builds and `lipo`s the daemon into `Contents/Helpers/`, and it is
   **Release-only** — it prints `skip for CONFIGURATION=Debug` and exits 0. So the Debug
   `xcodebuild` run in `CLAUDE.md` § Menu-bar app builds the app and no daemon.

So a `cargo build`, however green, changes nothing that serves.

## Prerequisites

- **Full Xcode plus `xcodegen`** — Command Line Tools alone is not enough (`CLAUDE.md`
  § Menu-bar app).
- **The Developer ID signing identity the script pins.**
  `apps/menubar/scripts/release-macos.sh` hardcodes one certificate SHA-1 and signs
  unconditionally, before its `--sign-only` exit — so a missing identity fails the run *after* the
  destructive step in § The hazard. **Settle it first**, and stop here if the SHA-1 is not listed:

  ```sh
  grep -m1 '^IDENTITY=' apps/menubar/scripts/release-macos.sh
  security find-identity -v -p codesigning
  ```

  There is no alternative deployment path in this repo for a contributor without that identity.
- **A `sessiometer` CLI you can invoke** for `log` and `daemon stop` below. Any build will do,
  including `cargo build --release` and `./target/release/sessiometer`: `log` reads a file, and
  `daemon stop` acts on the launchd job by *label*, never on the binary that issued it. That build
  cannot *serve* — this runbook's whole point — but it can drive both. Note that `daemon stop` is
  not a probe: it terminates the running daemon. Adjust the command names below to however you
  invoke it.

## First: which of the two owners are you replacing?

`org.sessiometer.agent` is **one label with two possible owners**, deliberately — the Rust CLI and
the app can each register it, and the app yields when the CLI already owns it (the two-owner
invariant, `apps/menubar/Sources/LoginItemModel.swift`).

```sh
launchctl print "gui/$(id -u)/org.sessiometer.agent" | head -20
```

Deliberately unfiltered: the fields are in the first dozen lines, and the job's own properties are
one tab in while nested dictionaries are deeper — a filter that loses that distinction reports a
nested `state = active` as the job's. Read `managed_by` and `path`, which name the registrant:

| What you see | Owner | What to do |
|---|---|---|
| `managed_by = com.apple.xpc.ServiceManagement`, `path = (submitted by smd.…)` | The app | Continue below — whether `state` is `running` or `not running`. |
| `path = …/Library/LaunchAgents/org.sessiometer.agent.plist` | CLI (`sessiometer service install`) | **This runbook does not apply.** `service install` renders the plist for *the binary that runs it*, so re-pointing it is a `cargo build` plus a re-`install`. See `README.md` § Running in the background. |
| `Could not find service …` | No job in the domain | Ambiguous — see below. |

**Run this before you start, and read the last row carefully.** *No job in the domain* is also what
step 1 of this procedure produces, so once you are mid-run it stops being diagnostic. From a cold
start it splits three ways, and the `ls` is what separates them:

```sh
ls ~/Library/LaunchAgents/org.sessiometer.agent.plist
```

- **The plist exists** — a CLI-installed agent that someone stopped. Row 2 above.
- **No plist, and the app has never started a daemon** — nothing to replace yet. Build the bundle
  (§ The procedure, skipping step 1), launch the app, and press **Start daemon** in the panel.
  Launching does **not** register a daemon by itself: the repair below only repairs a registration
  the app already holds, and first registration is deliberately the operator's act.
- **`launchctl print` failed for some other reason** — a bad domain or a denied request exits
  non-zero too, and carries no information about the job either way.

## The hazard: the script removes the directory you are probably running from

`release-macos.sh` starts by deleting the Xcode build-products directory:

```
rm -rf .build/Build/Products/Release        # apps/menubar/scripts/release-macos.sh:21
```

That directory is also **where the script leaves the finished `Sessiometer.app`**, so if you
launched the app from a previous run of this script — the normal case — that is the bundle you are
running, and it is about to be removed out from under the live process.

It is also where the **running daemon's** executable lives, at `Contents/Helpers/sessiometer`
inside that same bundle. Quitting the app does not stop that daemon: it is a separate launchd job
with `RunAtLoad` and a conditional `KeepAlive`, and it keeps running perfectly well after its
executable is unlinked. Which is why the first step of the procedure stops it explicitly.

## The procedure

**0. Take the baseline reading.** § Confirm compares against it, and after step 1 you cannot go
back and get it:

```sh
sessiometer log | grep daemon_build | tail -1
```

**1. Quit the app, and stop the daemon.** Both — they are two separate launchd jobs, and each is
needed for a different reason:

```sh
osascript -e 'quit app "Sessiometer"'   # the app; its bundle is about to be deleted
sessiometer daemon stop                 # the daemon; boots the agent out of your login session
```

**2. Rebuild and sign the bundle.**

```sh
./apps/menubar/scripts/release-macos.sh --sign-only
```

`--sign-only` stops after signing, because CI notarizes separately with an ASC API key
(`apps/menubar/scripts/release-macos.sh:4`). It is matched positionally against `$1`
(`:8`), so it **must be the first argument** — anywhere else it is silently ignored and the script
proceeds to notarize.

The steps in between are the script's, not this document's: read
[`apps/menubar/scripts/release-macos.sh`](../../apps/menubar/scripts/release-macos.sh) rather than a
paraphrase of it here. It ends with the rebuilt, signed bundle at
`apps/menubar/.build/Build/Products/Release/Sessiometer.app`.

**3. Launch the rebuilt app.** This is what gets launchd onto the new executable, and it is not
optional: the bundle's `Contents/Helpers/sessiometer` is re-`lipo`ed on every Release build, and
`SMAppService` requires re-registration when the registered executable changes.

```sh
open apps/menubar/.build/Build/Products/Release/Sessiometer.app
```

Launching fires `reconcileDaemonAgentRegistration()`, which unregisters before re-registering —
that is the repair (`apps/menubar/Sources/LoginItemModel.swift`).

**Both halves of step 1 are what make step 3 work**, and each failure is silent:

- **Quitting the app.** The repair runs from `applicationDidFinishLaunching` and nowhere else
  (`apps/menubar/Sources/main.swift`). `open` on an app that is *already running* activates it
  rather than launching it, so the repair never fires at all.
- **Stopping the daemon.** The repair is gated: it *postpones* whenever a daemon is still live,
  because unregistering would terminate it. With the job booted out and the single-instance lock
  free, it proceeds. The lock is any-provenance by design, so a hand-run `sessiometer run` in
  another terminal holds it too, and holds the repair off with it — stop that as well.

Get either wrong and the old daemon keeps serving while § Confirm shows nothing changed and no error
explains it. Only the live-daemon gate says anything at all, and only at `info` in the unified log;
the repair's earlier gates return silently. **If the daemon does not come back, press Start daemon
in the panel** — that registers unconditionally and does not depend on any of this.

### If you did not stop the daemon first

`sessiometer daemon restart` (`kickstart_managed`, `src/service.rs:166`, which runs
`launchctl kickstart -k` against the agent) restarts the job, and because the registration points at a *path* whose
contents you just replaced, it comes back on the new binary. It is the faster route.

Be clear about what it does not do: it does **not** re-register, which is the thing `SMAppService`
asks for when the executable changes. Treat it as a shortcut, and if the daemon does not come back
or § Confirm still shows the old build, fall back to stop-then-relaunch.

Note the asymmetry: `daemon restart` **refuses** after a `daemon stop` on an app-owned agent —
there is no CLI plist for it to bootstrap from, so it reports no managed service. Relaunching the
app is what brings that one back, and only when the executable actually changed: the repair
short-circuits on an unchanged identity, and it also records a new identity *before* waiting for
the daemon to appear, so a registration that succeeded while the spawn failed is not retried on the
next launch either. **Start daemon** in the panel is the recovery for both.

## Confirm the build you just deployed is the one serving

The daemon stamps its own identity into the event log at startup, so this is a read rather than an
inference. Run the same command as step 0 and compare the two:

```sh
sessiometer log | grep daemon_build | tail -1     # the same command as step 0
```

The line's shape (`Event::DaemonBuild`, `src/observability.rs`):

```
ts=<rfc3339> event=daemon_build version=<crate version> exe=<path> exe_size=<bytes> exe_mtime=<rfc3339>
```

**`exe_mtime` is the field that discriminates**, because it is observed from the file at runtime
rather than baked in at compile time — `version` and even `exe` are identical across two builds of
the same source at the same path, and `exe_mtime` is not. A deploy took when the last line is new
*and* its `exe_mtime` equals the bundle you just built:

```sh
exe=apps/menubar/.build/Build/Products/Release/Sessiometer.app/Contents/Helpers/sessiometer
mtime=$(stat -f '%m' "$exe") && date -u -r "$mtime" +%Y-%m-%dT%H:%M:%SZ
```

The `&&` is load-bearing. Collapsed into one `date -u -r "$(stat …)"`, a `stat` that fails leaves
the substitution empty and `date` reads that as the epoch: it prints `1970-01-01T00:00:00Z` and
exits 0. That is a confident answer computed from nothing, at the one moment you are trying to
establish what is true — and `.build/` is gitignored and per-checkout, so a wrong working directory
or a second worktree produces exactly that.

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

**An unchanged line is the informative failure**, and it looks exactly like a good one, which is why
step 0 matters. It means no new daemon reached the stamp, in rough order of likelihood here:

1. **The old daemon never went away** — either half of step 1 skipped. Take the `pid` from
   `launchctl print` and age it with `ps -o lstart= -p <pid>`; `launchctl print` itself carries no
   start time, so the pid alone cannot tell you.
2. **The replacement never started** — registration refused or postponed, or the daemon exited
   before the stamp. The stamp is written early but not first (`src/cli.rs`): creating the support
   directory precedes even the lock, and loading the config, requiring a non-empty roster, creating
   the remaining private directories and opening the event log all precede the stamp. Any of them
   failing produces no line.
3. **Nothing was ever registered** — the third row of § First. Press **Start daemon** in the panel;
   nothing in this procedure registers an agent for the first time.
4. **Something else already held the single-instance lock**, so the new daemon stood down before
   stamping.

## What this does not cover

**There is no release pipeline here, and this runbook does not propose one.** Single operator, one
machine, deploying by hand. Notarization, stapling and distribution are the script's non-`--sign-only`
path and CI's job, not steps an operator runs from this document.

## Known limitation

This runbook documents a script's behaviour, and **nothing in this repo reconciles the two**. No CI
job reads this file against `release-macos.sh`. The citation-rot gate reaches this file, but only
partly: `scripts/check-doc-citation-rot.sh` matches `src/*.rs:NNN` and nothing else, so the two
Rust citations above are held to a symbol while every `release-macos.sh:NN` line number is checked
by nobody.

Citing rather than restating bounds the rot unevenly, and it is worth being exact about which half:
a **renamed flag or a moved file** shows up the moment a reader follows the citation, but a **moved
line number** is the failure mode `CONTRIBUTING.md` § Citing source locations in docs/ documents as
silent — it still resolves, still looks like evidence. Re-derive the line numbers above when you
touch this file rather than carrying them.

Accepted deliberately; adding a reconciliation gate would be a change to the gates themselves,
argued on its own.
