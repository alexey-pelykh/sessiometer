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
- **A `sessiometer` CLI you can invoke** for `log` and `daemon stop` below. For those two verbs
  any build will do, including `cargo build --release` and `./target/release/sessiometer`: `log`
  reads a file, and `daemon stop` acts on the launchd job by *label*, never on the binary that
  issued it. That is not true of `stats` or `reliability`, which read the sample store: for them,
  follow the next bullet. That build cannot *serve* — this runbook's whole point — but it can drive
  `log` and `daemon stop`. Note that `daemon stop` is not a probe: it terminates the running
  daemon. Step 0 also runs `status --json`, which reads no store but is not one of those two
  verbs either: it decodes the serving daemon's reply into the invoking build's own status types
  and prints them again, so take it with the serving bundle's own helper,
  `apps/menubar/.build/Build/Products/Release/Sessiometer.app/Contents/Helpers/sessiometer`, still
  the serving daemon's own build at step 0. Adjust the command names below to however you invoke
  it.
- **Both halves of the binary change together.** The daemon that polls and the `sessiometer` you
  invoke for `stats` and `reliability` are one executable, so one bundle rebuild replaces both —
  the bundle's own copy is `Contents/Helpers/sessiometer`. Since #1598 a successful poll of an
  account whose plan has no five-hour session window appends a sample line carrying
  `"session":null`, and a poll of an account with no weekly window writes `"weekly":null`. Either
  is a `null` line, and an older binary cannot read it. Its `read_samples` keeps only the
  records that `from_slice::<Sample>` decodes (`src/usage_store.rs` at `fd8640b`, line 460), so
  an older `stats` or `reliability` silently leaves every such reading out of its report. Neither
  of them deletes anything: they only read. An older **daemon** is the binary that erases the
  lines for good: its `compact_and_roll` (`src/daemon.rs` at `fd8640b`, line 871) rewrites the
  raw file from what it could read, and is due on the daemon's first poll after start (`last_roll`
  begins `None`, `src/daemon.rs` at `fd8640b`, lines 863-866). From its first compaction a #1598
  build fences the store against that. It writes `usage-rollup.json` as
  `{"format":2,"rollup":{…}}`, and an older binary's `read_rollup` refuses that file with
  `UsageRollupMalformed` (`src/usage_store.rs` at `fd8640b`, lines 473-484) before it rewrites
  anything. An older daemon then stops compacting, without a word, and an older `stats` fails
  with `malformed usage rollup: JSON error at line … column …`. After #1598 that message is
  expected from any older build and is no reason to touch the file: read its `format` key with
  `jq -r .format "$HOME/Library/Application Support/sessiometer/usage-rollup.json"`, the file
  beside `usage-samples.jsonl`, before treating it as damage. A #1598 build that cannot compact
  says so on its stderr (the `Err` arm of `compact_and_roll`'s call, `src/daemon.rs:897-906`),
  which reaches `daemon.err.log` only under a CLI-installed agent; the app bundle's agent plist
  routes stderr to no file. There a stall on the read side, a newer or malformed rollup, shows as
  `stats unavailable` on the socket verb, as the error the same build's own `stats` prints when
  you run it with statement 5's helper, and in that `format` key; a compaction that fails on the
  write side, after its reads succeed, shows nowhere. Never delete or move `usage-rollup.json` or
  `usage-samples.jsonl`: without the rollup an older build treats the store as new and erases the
  lines, and the rollup's daily tier cannot be rebuilt. The fence does not cover lines written
  before its first write. So:
  1. Deploy the daemon and the CLI you will use in the same procedure, before the first `null`
     line exists, on either axis. That line appears the first time the new daemon polls an account
     that lacks a session window or a weekly window. If the store already holds one, replace both
     halves together all the same, and keep to statements 2 and 3 from now on: any such line an
     older daemon has already compacted away is gone for good, and the new daemon writes the next
     reading of each such account at its next poll.
  2. After that, never run an older `sessiometer stats` or `reliability` against the store: a
     `target/` build from an earlier checkout, a sibling worktree's build, or a Homebrew tap
     install from a release that predates #1598.
  3. Never start an older daemon against the store, by any path: relaunching a kept bundle, the
     hot-fix bundle of #1613 included; rebuilding the served bundle in a checkout that does not
     contain #1598; a `sessiometer service install` or `sessiometer run` from an older build.
     Before any rebuild of the served bundle, run statement 5's ancestry check in the checkout
     that builds it. The rule holds per store, so per machine, from the store's first `null` line
     on either axis.
  4. `log`, `daemon stop` and `daemon restart` are exempt: they read the event log or act on the
     launchd label.
  5. When in doubt, use the bundle's own
     `apps/menubar/.build/Build/Products/Release/Sessiometer.app/Contents/Helpers/sessiometer`
     for `stats` and `reliability`, run from the root of the checkout that holds the registered
     bundle: from another worktree's root that path names that worktree's own bundle, the sibling
     build statement 2 forbids. Or use a `target/` build made at a `HEAD` for which
     `git merge-base --is-ancestor adececc123791f5a0d02673ddfcf76d14656b8e4 HEAD` exits 0 (that
     commit is #1598's squash merge). Build it at that `HEAD` first: a `target/` directory left by
     an earlier build says nothing about the checkout it now sits in. A version string cannot
     settle it: the `daemon_build` line's `version` is the crate version, which most commits leave
     unchanged (§ Confirm).
- **`jq`**, for step 0's `H0` line, which runs before anything is stopped, for the rollup's
  `format` read in the bullet above, and for the readings in § Fleet recovery check. Current macOS
  ships `/usr/bin/jq` (jq 1.7.1 on macOS 27); elsewhere, or on a macOS without it, install it
  (`brew install jq`). The programs were written against jq 1.7.1; no older version has been
  tried.

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
| `path = …/Library/LaunchAgents/org.sessiometer.agent.plist` | CLI (`sessiometer service install`) | **This procedure does not apply, but the both-halves rule of § Prerequisites does.** `service install` renders the plist for *the binary that runs it*, so re-pointing it is a `cargo build` plus a re-`install`. Run statement 5's ancestry check in the checkout before that `cargo build`, and never `service install` or `run` from a build that lacks #1598 (statement 3). See `README.md` § Running in the background. |
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
sessiometer status --json | jq '[.accounts[] | select(.enabled and .auth == "healthy")] | length'
```

The second command prints `H0`, the number of enabled accounts whose credential is healthy. Run it
with the serving bundle's own helper (§ Prerequisites), whose status types match the daemon that
answers. Write it down: after step 1 the daemon no longer answers `status --json`, and § Fleet
recovery check compares against it.

**1. Quit the app, and stop the daemon.** First make sure that step 2 will build, because from
this step on no daemon serves until step 3. Run step 2's ancestry check on its own:

```sh
git merge-base --is-ancestor adececc123791f5a0d02673ddfcf76d14656b8e4 HEAD && echo "contains #1598"
```

If it prints nothing, stop nothing: the daemon still serves. Bring this checkout to a commit that
contains #1598, or, when you may not switch its branch, take § Building in a worktree, whose
step 1 builds before anything is stopped. Then quit the app and stop the daemon. Both — they are
two separate launchd jobs, and each is needed for a different reason:

```sh
osascript -e 'quit app "Sessiometer"'   # the app; its bundle is about to be deleted
sessiometer daemon stop                 # the daemon; boots the agent out of your login session
```

**2. Rebuild and sign the bundle.**

```sh
if git merge-base --is-ancestor adececc123791f5a0d02673ddfcf76d14656b8e4 HEAD; then
  ./apps/menubar/scripts/release-macos.sh --sign-only
else
  echo "not built: this checkout's HEAD does not contain #1598 (adececc), or the clone lacks that commit"
fi
```

The `if` is statement 5's ancestry check, which statement 3 requires before any rebuild of the
served bundle (§ Prerequisites): the script runs only when the checkout contains #1598, because
a bundle built from older source runs a daemon that erases the store's `null` lines while no
#1598 build has fenced the store, and on a fenced store stops compacting without a word; and
nothing in § Confirm reads the commit a bundle was built from. On the
`not built` line nothing has been built or deleted, and no daemon serves until step 3: bring the
checkout to a commit that contains #1598, fetching first if the clone lacks it, and run step 2
again. When you may not switch this checkout's branch, take § Building in a worktree instead.

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
T0=$(date -u +%s)
open apps/menubar/.build/Build/Products/Release/Sessiometer.app
```

The first line records `T0`, the restart instant, in epoch seconds. Write it down: § Fleet
recovery check spaces its takes from it.

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

### Building in a worktree

The procedure above builds in the checkout that holds the registered bundle, and
`release-macos.sh` deletes that checkout's `Release` directory (§ The hazard). When that checkout
is on another branch or holds work that is not yours, do not switch its branch. Build at the
deploy commit in a dedicated worktree instead, and install the result over the registered path.

The route changes where step 2 runs and replaces step 3's block, and nothing else. Step 2's block
runs unchanged in the worktree, as the route's step 1 below, so statement 5's ancestry check,
which statement 3 requires (§ Prerequisites), runs there. Step 3's whole block, its `T0` line and
its `open`, is replaced by the route's steps 3 and 4, and the `T0` of the route's step 4 is the
one the readings use. Step 0 and step 1 still run, in the main checkout, after the route's step 1
has built and before its step 3: that step removes the bundle the app and the daemon run from. In
step 1, skip its ancestry check: the route's step 1 has run it in the worktree, and the main
checkout may be on another branch. Run § Confirm and § Fleet recovery check from the main
checkout's root, because their paths, the `S=` helper path among them, are relative to it and
name the registered bundle.

1. Fetch first (`git fetch origin`), so that `origin/main` is the deploy commit and not the
   clone's last view of it, then create the worktree there
   (`git worktree add --detach <path> origin/main`). In the worktree, run step 2's block, whose
   ancestry check (`git merge-base --is-ancestor`) lets `release-macos.sh --sign-only` run only
   when the worktree contains #1598. On its `not built` line, stop: remove and install nothing
   (step 3 below), because no later check reads the commit the bundle was built from. Otherwise
   the script empties that worktree's own `Release` directory and leaves the signed bundle in it.
2. Before you install anything, read the built helper's mtime in UTC, in the `stat` and
   `date -u -r` form of § Confirm. Call it `want`.
3. In the main checkout, remove the old bundle at the registered path, then copy the built one
   over it with `ditto`, which keeps the signature. The registration points at the path, so it
   keeps serving. Compare the installed helper with the built one byte for byte (`cmp`) before you
   open anything.
4. Record `T0`, then `open` the **main checkout's** bundle by its absolute path. Never open the
   worktree's bundle.
5. Once § Confirm passes and § Fleet recovery check has taken its last take, delete the
   worktree's own bundle and every older bundle that carries a daemon (none has a use, and each
   can be launched by mistake), then run `git worktree remove` on the worktree.

**An `exe_mtime` match alone does not confirm this deploy.** `ditto` keeps the modification time
(`man ditto`: the copy has "the same mode, access time, modification time, owner"), so the helper
the worktree built and the helper now at the registered path carry one mtime, and a lone mtime
comparison passes in two wrong cases. After a skipped install, the registered path still holds
the old helper, and an old daemon relaunched from it reports that old mtime, which matches a
`stat` of the path. And the worktree's own bundle, launched by mistake, reports the built mtime,
which equals the installed one. So check three things on the newest `daemon_build` line: its
`exe=` is the registered path's helper, its `exe_mtime` is `want`, and `want` differs from the
`exe_mtime` of the baseline line you took in step 0. The commands, with their guards, are written
out in #1604 (steps 2 and 3), the first deployment to run this route.

### If you did not stop the daemon first

`sessiometer daemon restart` (`kickstart_managed`, `src/service.rs:166`, which runs
`launchctl kickstart -k` against the agent) restarts the job, and because the registration points at a *path* whose
contents you just replaced, it comes back on the new binary. It is the faster route. Take step 0's
`H0` first, and record `T0=$(date -u +%s)` on the line before the command (§ Fleet recovery check).
A deploy of the optional-quota-windows tranches does not take this route: REQ-DEP-B-001 of
`docs/requirements/optional-quota-windows.md` makes it stop-then-relaunch, so its `T0` is step 3's,
or that of step 4 of § Building in a worktree.

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
*and* its `exe_mtime` equals the bundle you just built. On the worktree route that match alone
proves nothing, because `ditto` keeps the mtime: there the three checks of § Building in a
worktree (`exe=`, `exe_mtime` against `want`, and `want` against the baseline) replace it.

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

### Fleet recovery check

The `daemon_build` match above proves which binary serves. It does not prove that the binary
serves every account, so follow it with these readings. Take all of them with the bundle's own
binary (§ Prerequisites, both halves):

```sh
S=apps/menubar/.build/Build/Products/Release/Sessiometer.app/Contents/Helpers/sessiometer
STORE="$HOME/Library/Application Support/sessiometer/usage-samples.jsonl"
```

If `$HOME` is empty in your shell, write `STORE`, and `CONFIG` and `LOG` below, as absolute
paths. These variables, `T0` (set above, in step 3 or in step 4 of § Building in a worktree) and
`W` (set in (a) below) live only in the shell that set them: set them again from your record if
you change shells. `H0` is no variable: it is the number you wrote down in step 0, and (b)
compares against it by eye. Single-quote every `jq` program, and use zsh or bash, which provide
the `<(…)` process substitution. Paste each block whole, as it stands, except that (e) has you
set its first line before you paste it. No block in this subsection carries a `#` comment,
because a stock interactive zsh does not read `#` as one: it hands a trailing comment to the
command as arguments (`T0=$(date -u +%s) # …` then leaves `T0` unset), and an apostrophe inside
it opens a quote. Two earlier blocks do carry one: step 1's two commands and § Confirm's
`daemon_build` read. In a stock interactive zsh, paste each of those lines without the text from
its `#` on, or run `setopt interactive_comments` first. No block uses `!` in a pattern, because
that zsh expands it from history (`event not found`).

**Reporting rule.** In this deployment every account label is an email address. A reading that
leaves your own terminal — an issue comment, a pull request, a chat message — carries counts,
1-based positions in `status --json`'s `accounts` array, percentages, states and timestamps, and
never a label. Every `jq` program below keeps labels inside the program and prints none, with one
exception: the program in (e) outputs one label, which its block keeps in `L` and never prints,
and unsets at its end. Keep any
list of labels in local scratch only, and never post the raw `.next_swap` object: its `to` field
is a label.

**When a reading is met.** A reading is met only when it meets its goal at take 2 (the PRD's "by
the second cycle"; (a) defines the takes). The PRD here, and for REQ-DEP-B-002 below, is
`docs/requirements/optional-quota-windows.md`, not the one this runbook's `source:` names. One
that first meets its goal at take 3 is `late`: it is not met, and it takes route (e) as a failed
one does (that PRD's § 4 decision gate; #1604, step 9).
One still short at take 3 is FAIL. Record each reading's result as `met`, `late`, `no-anchor` or
FAIL.

**(a) The cycle and the restart instant.** A cycle is one full roster sweep. The daemon polls one
account per `poll_secs / N` sub-interval, where N is the number of distinct accounts in the
rotation (`[tunables]` `poll_secs`, default 300 s), and the active-account interleave makes a
full sweep span about `2·poll_secs` (the `next_subinterval` doc, `src/daemon.rs:5733-5739`). Each
interval is jittered, by default with a standard deviation of 60 s, about 20 % of the default
`poll_secs` (`DEFAULT_POLL_JITTER_STDDEV`, `src/config.rs:89-94`). `T0` is the epoch you recorded
beside step 3's `open`, at step 4 of § Building in a worktree, or before `daemon restart`. The new
`daemon_build` line you matched above is the daemon's start, a few seconds after `T0`. Read
`poll_secs` from the config's `[tunables]` table and set the spacing `W`:

```sh
CONFIG="$HOME/Library/Application Support/sessiometer/config.toml"
POLL=$(awk '
/^[ \t]*[#]/ { next }
/^[ \t]*\[/ { sec = ($0 ~ /^[ \t]*\[[ \t]*tunables[ \t]*\]/) ? "t" : "x"; next }
sec == "" && /^[ \t]*tunables[ \t]*=/ { v = "?"; next }
(sec == "t" && /^[ \t]*poll_secs[ \t]*=/) || (sec == "" && /^[ \t]*tunables[ \t]*[.][ \t]*poll_secs[ \t]*=/) {
  s = $0; sub(/^[^=]*=[ \t]*"?/, "", s)
  if (match(s, /^[0-9]+/) && substr(s, RLENGTH + 1) ~ /^"?([ \t]|$)/) v = substr(s, 1, RLENGTH); else v = "?"
}
END { print v }
' "$CONFIG")
case ${POLL:-300} in
  *[^0-9]*) echo "poll_secs is not a plain number: set POLL by hand, then W" ;;
  *) POLL=${POLL:-300}; W=$(( 2 * POLL + 60 )); echo "poll_secs=$POLL W=$W" ;;
esac
```

The program prints only the number: never print the rest of `config.toml`, which names accounts.
No `poll_secs` key means the default, 300. An `awk` line on stderr saying it `can't open file`
means `CONFIG` names no file, and the default the block printed does not apply. The daemon loads
its roster from that file and stamps no `daemon_build` line without one (§ Confirm), so after a
`daemon_build` match a `can't open file` always means `CONFIG` names the wrong file. Find yours
(the `XDG_CONFIG_HOME` case below), set `CONFIG` to it and paste the block again. The program
reads the key under `[tunables]` (the header may be indented, spaced inside the brackets or
followed by a comment) and the dotted spelling
`tunables.poll_secs` ahead of the first table. A value in plain digits is accepted, with optional
double quotes and a trailing comment after a blank. Any other value stops with the message above:
`1_000`, a float, text, a comment glued to the digits, or `tunables = { … }`. Set `POLL` by hand
then. A table header or key quoted in the TOML way (`["tunables"]`) is not recognised and gives
the default, so set `POLL` by hand if you wrote one. If the agent runs with `XDG_CONFIG_HOME` set,
the config is `$XDG_CONFIG_HOME/sessiometer/config.toml` instead. Do not read the cadence off
`log --channel diag`: that channel is `daemon.err.log`, the stderr file of a launchd job whose
plist sets `StandardErrorPath`, and the bundle's agent plist sets none, so on a bundle install
the file does not exist. Take *k* of a reading is taken at `T0 + k·W` or later: about 11, 22 and
33 minutes after the restart at the default.

**(b) Served accounts, takes 1 to 3.** `H0` is the count of enabled accounts whose credential is
healthy that you recorded in step 0, before `daemon stop`. At each take:

```sh
date -u +%Y-%m-%dT%H:%M:%SZ
"$S" status --json | jq -r '[.accounts[] | select(.enabled and .auth == "healthy")] | "\(map(select(.weekly_pct != null)) | length)/\(length) healthy accounts served"'
"$S" status --json | jq -c '[.accounts | to_entries[] | select(.value.enabled and .value.auth == "healthy" and .value.weekly_pct == null) | .key + 1]'
```

Goal: at take 2 the first command prints `N/N healthy accounts served`, both numbers equal and
`N` at least the `H0` you recorded, and the second prints `[]`. An `H0` of 0 proves nothing. A
denominator above `H0` is met: the daemon keeps credential health in memory, so the restart
clears it, and an account the old daemon did not count can poll healthy under the new one;
record the denominator beside the result. A denominator below `H0` is not met, whatever the
numerator: an account left the healthy set. Repair that credential and read again at the next
take; this check does not judge why it left. The result then follows the take: `late` when take 3
prints `N/N` with `N` at least `H0`, and FAIL when take 3 still shows a denominator below `H0`.
A position the second command lists at take 2 means
the reading is not met: run route (e) for it at take 3 or later. A reading whose second command
first prints `[]` at take 3 is `late`. FAIL: the second command lists any position at take 3.
Why `weekly_pct`: a failed poll nulls both percentages, while a served account without a
session window carries `session_pct: null` beside a present `weekly_pct`, so `weekly_pct` tells
the two apart and `session_pct` does not. Readings (b) and (d) assume every served account has a
weekly window, as REQ-DEP-Q-001's one SCALE does ("`weekly_pct` non-null in `status --json`");
when this was written no account had a five-hour window without a weekly one. Such an account
shows `weekly_pct: null` beside a non-null `session_pct`, a pair no failed poll produces, and (b)
lists it as unserved and (d) as unread. Before you run route (e) for a position either reading
lists, read that row's `session_pct`:
`"$S" status --json | jq --argjson p 7 '.accounts[$p - 1].session_pct'`, with 7 replaced by the
position. If it prints a number, the account is served on its session window only and the
reading's result says nothing about the deploy: record the result the take gives with that
position, run no route for it, file nothing against the deploy, and say on the work item that
REQ-DEP-Q-001's SCALE, not the deploy, is what needs amending.

**(c) Sampling resumed, takes 2 and 3.** Every enabled, healthy account should have written a
sample line since the restart:

```sh
date -u +%Y-%m-%dT%H:%M:%SZ
if [ -r "$STORE" ]; then
  jq -n -c --argjson t "$T0" --slurpfile st <("$S" status --json) '
    ([inputs | select(.ts >= $t) | .acct] | unique) as $seen
      | [$st[0].accounts | to_entries[] | select(.value.enabled and .value.auth == "healthy")] as $h
      | {healthy: ($h | length),
         sampled: ([$h[] | select(.value.label as $l | any($seen[]; . == $l))] | length),
         missing: [$h[] | select(.value.label as $l | any($seen[]; . == $l) | not) | .key + 1]}' "$STORE"
else
  echo "stopped: STORE names no readable file; set it, then paste this block again"
fi
```

The store is `usage_samples` in `src/paths.rs`; each line's `ts` is UTC epoch seconds and its
`acct` is the roster label. A line is appended only after a successful parse, and after a restart
every account is polled at the normal cadence first, because the slow re-poll of an exhausted peer
is armed only by a reading. So a served account has a line by take 2, including any account that
was served but not sampled before the deploy. Goal: at take 2, `missing` is `[]` and `sampled`
equals `healthy`. A position in `missing` at take 2 means the reading is not met: run route (e)
for it at take 3 or later. A reading whose `missing` first reads `[]` at take 3 is `late`. FAIL:
`missing` lists any position at take 3. A `stopped:` line instead of the counts means `STORE`
names no readable file, and every position would otherwise read as missing: set it and read
again.

**(d) The capacity verdict, takes 1 to 3.**

```sh
date -u +%Y-%m-%dT%H:%M:%SZ
"$S" status --json | jq -c '
  .accounts as $a | .next_swap as $n
    | [$a | to_entries[] | select(.value.enabled and .value.auth == "healthy" and (.value.active | not))] as $sp
    | {state: $n.state, reason_kind: $n.reason.kind, cause: $n.cause,
       resets_at: ($n.resets_at // $n.reason.resets_at),
       target_position: (if $n.state == "target" then ($a | map(.label) | index($n.to)) as $i | (if $i == null then "not_in_roster" else $i + 1 end) else null end),
       spares: ($sp | length),
       unread_spares: [$sp[] | select(.value.weekly_pct == null) | .key + 1],
       open_weekly_only_spares: [$sp[] | select(.value.weekly_pct != null and (.value.weekly_exhausted | not) and .value.session_pct == null) | .key + 1],
       open_two_window_spares: [$sp[] | select(.value.weekly_pct != null and (.value.weekly_exhausted | not) and .value.session_pct != null) | {position: (.key + 1), session_pct: .value.session_pct}],
       weekly_blocked_spares: [$sp[] | select(.value.weekly_pct != null and .value.weekly_exhausted) | {position: (.key + 1), weekly_resets_at: .value.weekly_resets_at}]}
    | .verdict = (if $n == null then "no-anchor"
        elif .state == "target" and .target_position != "not_in_roster" then "met"
        elif .state == "awaiting_data" or (.state == "no_viable_target" and (.unread_spares | length) > 0) then "retake"
        elif .state == "no_viable_target" and .cause == "weekly" and (.open_weekly_only_spares | length) == 0 and (.open_two_window_spares | length) == 0 then "met_weekly_blocked"
        else "fail" end)'
```

A spare is an enabled, healthy, non-active row. Like (b), the program counts a spare as read when
its `weekly_pct` is non-null, so a served spare with no weekly window is unread here; (b) states
that assumption and the read that detects it. The program computes the target's 1-based
position and never prints `to`. `verdict` reads `met` when the daemon names a target in the
roster; `retake` while it is `awaiting_data`, or `no_viable_target` with a spare unread;
`met_weekly_blocked` when it is `no_viable_target` with `cause` `weekly`, no spare unread, and
every spare `weekly_exhausted`, with `weekly_blocked_spares` naming each spare by position and
`weekly_resets_at`. That arm reads REQ-DEP-B-002's exception, "every enabled spare has a served
reading and each is at or above the weekly line (none unread)"; the requirement also has the daemon
name "the soonest-returning spare and its weekly reset". Under `no_viable_target` the wire carries
`cause` and `resets_at` and no `to`, so no reading can check which spare the daemon means, and the
daemon takes `resets_at` over every enabled non-active account it holds a reading for, healthy or
not, and over both windows. The reading records `resets_at` beside the list; a non-null
`resets_at` that matches no `weekly_resets_at` in `weekly_blocked_spares` goes on the work item
with the takes, and does not change the verdict. `verdict` reads `no-anchor` when
`next_swap` is `null`, which the daemon writes when there is no active anchor to swap from; and
`fail` otherwise: in particular `cause` `session` or absent, and any spare with weekly headroom. A
weekly-only one passed over is a defect. A two-window one may be held by the daemon's session
reserve, which the wire does not show, so it is listed with its `session_pct`. Goal: `met` or
`met_weekly_blocked` at take 2. Take (d) at all three takes: the verdict of take 1 is recorded with
the others and settles nothing, because the reading's result is read at take 2. A `retake` at
take 2 is not met: a reading first met at take 3 is recorded `late`, and takes the failure route
below as a failed one does (#1604, step 9). FAIL: `fail` at take 2 or later, or `retake` at
take 3. A `no-anchor` verdict is not met and is not a FAIL: it is not a swap defect, so it takes
no failure route below and files no issue. When take 2 reads `no-anchor`, record the reading's
result as `no-anchor`, and report it on the work item with the other takes.

**(e) When a reading is not met.** No route here is a rollback: a reply shape the parser does not
know is a new fixture, not a defect in the deploy. Relaunch no previous bundle, and run no older
binary against the store "to check". The poll outcomes are in the event log, the only log the
bundle's agent writes. The block below runs at take 3 or later, for each position a reading
named at take 2. Before you paste it, replace the 7 on its first line with one such position. The
block counts that position's lines there since `T0`, with the label kept in a variable that is
never printed:

```sh
p=7
case $T0 in ''|*[^0-9]*) bad=T0 ;; *) bad= ;; esac
case $p in ''|0*|*[^0-9]*) bad=${bad:-p} ;; esac
L=$([ -z "$bad" ] && "$S" status --json | jq -r --argjson p "$p" '.accounts[$p - 1].label')
LOG="$HOME/Library/Logs/sessiometer/sessiometer.log"
if [ -n "$bad" ]; then
  echo "stopped: $bad is not set as the text above says; set it, then paste this block again"
elif [ ! -r "$LOG" ]; then
  echo "stopped: LOG names no readable file; set it, then paste this block again"
elif [ -z "$L" ] || [ "$L" = null ]; then
  echo "stopped: status --json names no account at position $p; check p and the daemon"
else
  SINCE="ts=$(date -u -r "$T0" +%Y-%m-%dT%H:%M:%SZ)"
  echo "position $p: usage_gap since T0: $(awk -v t="$SINCE" '$1 >= t' "$LOG" | grep -cF " event=usage_gap acct=$L ")"
  echo "usage_backoff since T0, any account: $(awk -v t="$SINCE" '$1 >= t' "$LOG" | grep -c ' event=usage_backoff ')"
  if [ "$p" = 7 ]; then echo "note: p is still 7, the position this runbook shows; set p unless a reading named 7"; fi
fi
unset L bad
```

The block counts nothing, and prints one `stopped:` line naming the input, in four cases. `T0`
is unset or not epoch seconds: `date` would read an empty `T0` as the epoch, and the counts would
cover the whole log. `p` is not a position of 1 or more: `jq` reads 0 as the last account. `LOG`
names no readable file: both counts would read 0. Or `status --json` names no account at
position `p`: past the end of the roster `jq` prints `null`,
and the `usage_gap` count would read 0. Left at 7 on a roster that has a seventh account, `p`
passes every guard, and the block counts position 7 and adds only a `note:` line, so set it.

A `usage_gap` line is written on the first poll of a streak that yields no reading, whatever the
cause, and carries the label. The new daemon starts with no streak, so an account it fails to read
has a line from its first poll after the restart. A `usage_backoff` line is written only for an
HTTP 429 or a 5xx or network failure, carries `class=rate_limited` or `class=transient`, and
names the account by UUID, which `status --json` does not show. A reply the parser rejects
writes a `usage_gap` and never a `usage_backoff`.

- **Reading (b) or (c): a position listed at take 2.** The reading is not met. Run the block for
  each such position at take 3 or later, and read the reading again.
  - The reading no longer lists the position: the account was read late, not refused. Record the
    counts and file nothing; the reading is `late`.
  - Still listed by reading (c) while reading (b) counts it as served: a store defect, whatever
    the counts, because a served reading comes from a parsed reply, and a parsed reply appends a
    line. File a `(fix) usage-store:` issue on the work item with reading (c)'s output. The
    branches below are for a position reading (b) lists too.
  - Still listed, `usage_gap` 0: the daemon has not polled the account since the restart. Take
    one more `W`; if it is still 0, file a defect on the work item being deployed with every
    take's output and both counts.
  - Still listed, `usage_gap` 1 or more and `usage_backoff` 0: no account was throttled or hit a
    5xx since the restart, so the daemon is refusing this account's reply: an unknown reply
    shape, or a non-429 4xx. The operator then fetches that account's raw `/api/oauth/usage`
    reply with their own tooling: the repository carries no command for it (`sessiometer capture`
    stashes a credential, it does not capture a reply), so an agent session hands this step to
    the operator and never runs it. Compare the reply's quota fields against the frozen
    null-session capture, `CAPTURED_NULL_SESSION_QUOTA_FIELDS` in `src/usage.rs`'s tests (landed
    by #1591, pinned by #1592), and never commit or quote the reply's identity fields. File the
    new shape as a fixture issue under the work item being deployed (#1590 for the
    optional-quota-windows tranches).
  - Still listed, `usage_gap` 1 or more and `usage_backoff` 1 or more: the log cannot say
    whether this account was the throttled one. Take one more `W`; if the position is still
    listed, take the operator-capture route above.
- **Reading (d): `fail` at take 2 or later, or `retake` at take 2.** For a `fail`, file a
  `(fix) swap:` issue there with every take's output. For each position in `unread_spares` at
  take 2, first read its `session_pct` as reading (b)'s last paragraph says, then run the block
  above and the first bullet's branches. A `retake` at take 2 that reads
  `met` or `met_weekly_blocked` at take 3 is `late`: record it and file nothing.

Post every take of (b), (c) and (d), each with its UTC timestamp, then each reading's result
(`met`, `late`, `no-anchor` or FAIL), and `T0`, `W` and `H0`, as a comment on that work item
(#1590 for the optional-quota-windows tranches), under the reporting rule above.

## What this does not cover

**There is no release pipeline here, and this runbook does not propose one.** Single operator, one
machine, deploying by hand. Notarization, stapling and distribution are the script's non-`--sign-only`
path and CI's job, not steps an operator runs from this document.

## Known limitation

This runbook documents a script's behaviour, and **nothing in this repo reconciles the two**. No CI
job reads this file against `release-macos.sh`. The citation-rot gate reaches this file, but only
partly: `scripts/check-doc-citation-rot.sh` matches `src/*.rs:NNN` and nothing else, so the Rust
citations above are held to a symbol while every `release-macos.sh:NN` line number is checked by
nobody. The citations into older code that name commit `fd8640b` are not in that form, so the
gate does not inspect them; they point at a fixed commit, so their line numbers cannot move.

Citing rather than restating bounds the rot unevenly, and it is worth being exact about which half:
a **renamed flag or a moved file** shows up the moment a reader follows the citation, but a **moved
line number** is the failure mode `CONTRIBUTING.md` § Citing source locations in docs/ documents as
silent — it still resolves, still looks like evidence. Re-derive the line numbers above when you
touch this file rather than carrying them.

Accepted deliberately; adding a reconciliation gate would be a change to the gates themselves,
argued on its own.
