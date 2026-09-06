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

**Run every command below from the repository root**, except where a block says otherwise. The one
`cd` in the procedure is inside its own command and does not carry over.

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
- **A `sessiometer` CLI you can invoke** for the read-only verbs below (`daemon stop`, `log`). Any
  build reads the same event log and the same launchd domain, so `cargo build --release` and
  `./target/release/sessiometer` is fine here — that build cannot *serve*, which is this runbook's
  whole point, but it can *ask*. Adjust the command names below to however you invoke it.

## First: which of the two owners are you replacing?

`org.sessiometer.agent` is **one label with two possible owners**, deliberately — the Rust CLI and
the app can each register it, and the app yields when the CLI already owns it (the two-owner
invariant, `apps/menubar/Sources/LoginItemModel.swift`).

```sh
ls ~/Library/LaunchAgents/org.sessiometer.agent.plist    # CLI-owned iff this exists
launchctl print "gui/$(id -u)/org.sessiometer.agent" | grep -E '^	(state|pid) '
```

| What you see | Owner | What to do |
|---|---|---|
| The plist exists | CLI (`sessiometer service install`) | **This runbook does not apply.** `service install` renders the plist for *the binary that runs it*, so re-pointing it is a `cargo build` plus a re-`install`. See `README.md` § Running in the background. |
| No plist; `launchctl print` shows `state = running` | The app | Continue below. |
| No plist; `launchctl print` fails or shows no running state | Nobody, yet | There is nothing to replace. Build the bundle (§ The procedure, skipping the stop) and launch the app once; registering is what the first launch does. |

`launchctl print` is keyed on the label alone and cannot tell you *who* registered the job — the
plist check is what distinguishes them, so run both.

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

**1. Stop the app and the daemon.** Both, and the daemon is the one that is easy to miss:

```sh
sessiometer daemon stop      # boots the agent out of your login session
```

**2. Rebuild and sign the bundle.**

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

**3. Launch the rebuilt app.** This is what gets launchd onto the new executable, and it is not
optional: the bundle's `Contents/Helpers/sessiometer` is re-`lipo`ed on every Release build, and
`SMAppService` requires re-registration when the registered executable changes.

```sh
open apps/menubar/.build/Build/Products/Release/Sessiometer.app
```

Launching fires `reconcileDaemonAgentRegistration()`, which unregisters before re-registering —
that is the repair (`apps/menubar/Sources/LoginItemModel.swift`).

**Step 1 is what makes step 3 work.** That repair is gated: it *postpones* whenever our own agent's
launchd job is still running, because unregistering would terminate a live daemon. Skip the stop and
the repair silently defers — logged at `info` and nowhere else — the old daemon keeps serving, and
you land on § Confirm with nothing changed and no error to explain it. With the daemon stopped the
job is gone and the lock is free, so the repair proceeds.

### If you did not stop the daemon first

`sessiometer daemon restart` (which is `launchctl kickstart -k gui/<uid>/org.sessiometer.agent`,
`src/service.rs:166`) restarts the job, and because the registration points at a *path* whose
contents you just replaced, it comes back on the new binary. It is the faster route.

Be clear about what it does not do: it does **not** re-register, which is the thing `SMAppService`
asks for when the executable changes. Treat it as a shortcut, and if the daemon does not come back
or § Confirm still shows the old build, fall back to stop-then-relaunch.

Note the asymmetry: `daemon restart` **refuses** after a `daemon stop` on an app-owned agent —
there is no CLI plist for it to bootstrap from, so it reports no managed service. Relaunching the
app is what brings that one back.

## Confirm the build you just deployed is the one serving

The daemon stamps its own identity into the event log at startup, so this is a read rather than an
inference. **Take the reading before you start**, or you have one line and nothing to compare it to:

```sh
sessiometer log | grep daemon_build | tail -1     # run this BEFORE step 1, and again after step 3
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
the before-reading matters. It means no new daemon reached the stamp. Three ways that happens, in
rough order of likelihood here:

1. **The old daemon never went away** — the deferred-repair case above. Check
   `launchctl print "gui/$(id -u)/org.sessiometer.agent"` for a `pid` older than your rebuild.
2. **The replacement never started** — registration refused, or the daemon exited before the stamp.
   The stamp is written early but not first: it sits after the config load, the roster check and
   opening the event log (`src/cli.rs`), so a missing config or an empty roster also produces no
   line.
3. **Something else already held the single-instance lock**, so the new daemon stood down before
   stamping.

## What this does not cover

**There is no release pipeline here, and this runbook does not propose one.** Single operator, one
machine, deploying by hand. Notarization, stapling and distribution are the script's non-`--sign-only`
path and CI's job, not steps an operator runs from this document.

## Known limitation

This runbook documents a script's behaviour, and **nothing in this repo reconciles the two**. No CI
job reads this file against `release-macos.sh`, and the citation-rot gate does not reach it either —
`scripts/check-doc-citation-rot.sh` only inspects `src/*.rs:NNN` citations, so the
`release-macos.sh:NN` line numbers above are checked by nobody.

Citing rather than restating bounds the rot unevenly, and it is worth being exact about which half:
a **renamed flag or a moved file** shows up the moment a reader follows the citation, but a **moved
line number** is the failure mode `CONTRIBUTING.md` § Citing source locations in docs/ documents as
silent — it still resolves, still looks like evidence. Re-derive the line numbers above when you
touch this file rather than carrying them.

Accepted deliberately; adding a reconciliation gate would be a change to the gates themselves,
argued on its own.
