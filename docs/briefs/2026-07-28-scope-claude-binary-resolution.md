# Scope brief — `claude` binary resolution under launchd

**Date**: 2026-07-28
**Source**: `/investigate` → `/council` → deep-dive verification → `/scope`
**Items tracked**: 6 (issues #783–#788)

---

## The defect

The daemon has resolved **zero** `claude` binaries since it moved under launchd (`da329f7`, issue #171, activated 2026-07-27).

Under launchd the daemon inherits `PATH=/usr/bin:/bin:/usr/sbin:/sbin`, which excludes `~/.local/bin/claude`. With `[refresh].claude_bin` unset and `$CLAUDE_BIN` absent, all three resolution tiers in `paths::claude_binary_from` miss, and `Error::ClaudeBinaryNotFound` is returned **before any spawn**.

**Evidence:**

| Signal | Value |
|---|---|
| Refresh events after cutover | **83/83** `outcome=error window_secs=0` |
| Successful cycles, for contrast | ~25 000 s windows |
| Jul 6–26 | 100% refreshed, ~13–18/day |
| Jul 27 | 7 refreshed, then 53 error |
| Jul 28 | 30 error, 0 refreshed |
| Last success / first error | `09:49:25Z` / `14:02:59Z` |
| Daemon start under launchd | `13:01:47Z` |

**Cascade**: stale credentials → usage-endpoint 401s → quarantine → dropped from the poll schedule → `n/a` percentages → the `🟠 degraded — run 'sessiometer poke'` cue. The manual `poke` works only because the CLI runs in an interactive shell with the correct PATH — which is what made the fault look like a per-account credential problem rather than an environmental one.

---

## The fix

```
1. [refresh].claude_bin      explicit operator pin          (unchanged)
2. $CLAUDE_BIN               explicit env override          (unchanged)
3. harvested user PATH       <pw_shell> -l -c /usr/bin/env  (NEW — replaces raw $PATH)
   ↳ harvest fails → fall back to the daemon's inherited $PATH
not found → ClaudeBinaryNotFound
```

The daemon behaves **as if executed from the user's terminal**. No well-known-fallback list.

Four mechanism details, each settled empirically rather than assumed:

| Detail | Basis |
|---|---|
| `/usr/bin/env`, not `echo $PATH` | fish and nu print `$PATH` as a space-separated *list* — a naive parse corrupts silently |
| Shell from passwd `pw_shell`, not `$SHELL` | The module's existing discipline; `getpwuid` verified working with `HOME` entirely absent |
| `-l -c`, not `-lic` | Measured **identical** 21-entry PATH; **~38 ms vs ~284 ms** |
| Per-cycle, no canonicalization | Preserves #375 and #101 |

**Shadowing is a binding constraint** (user-stated): the harvested PATH is scanned in the **user's own order**, first match wins. No re-ranking, no preference list — if the user shadows `claude`, the daemon must pick the same shadowing binary.

### Rejected, with reasons

- **Bake `EnvironmentVariables` into the plist** — the bundled plist is build-time static and **sealed in `Contents/_CodeSignature/CodeResources`**, so it cannot carry a per-user path; and launchd performs **zero** variable expansion (`$HOME`/`~` arrive verbatim). It would also re-freeze what #375 unfroze.
- **Well-known fallback locations** — **no discovery contract exists** for `claude` (no `claude --where`, no env var, no receipt file; verified against official docs), and npm paths vary by node version manager. A static list is structurally incomplete.
- **Writability / code-signature gate on the resolved binary** — dropped: anyone who can write a PATH directory can already hijack the user's interactive `claude`. A daemon-only gate would be stricter than the terminal it imitates. (Measured anyway: `/opt/homebrew/bin`, `/opt/homebrew/sbin`, and a Sublime Text `bin` are group-writable by another admin account.)

---

## Tracked items

Grouped by root cause. **These are three distinct root causes, not one** — flagged during scoping and accepted.

### A — binary resolution under launchd

| # | Item | Depends on | Status |
|---|---|---|---|
| [#783](https://github.com/alexey-pelykh/sessiometer/issues/783) | Harvest the user-level PATH from the login shell | — | **MERGED** `6db659f` (PR #796) |
| [#784](https://github.com/alexey-pelykh/sessiometer/issues/784) | Wire the harvest into the resolution chain, honoring shadowing order | #783 | **MERGED** `5222fdc` (PR #799) |
| [#785](https://github.com/alexey-pelykh/sessiometer/issues/785) | Regression guard under the literal launchd environment | #784 | **MERGED** `45ebea2` (PR #809) |

### B — diagnosability

| # | Item | Depends on | Status |
|---|---|---|---|
| [#786](https://github.com/alexey-pelykh/sessiometer/issues/786) | Classify the unresolved-binary failure as `reason=unresolved` | — | **MERGED** `a2729de` (PR #812) |
| [#787](https://github.com/alexey-pelykh/sessiometer/issues/787) | A restart erases the systemic refresh-failure signal | — | **MERGED** `b21934a` (PR #816) |

### C — app-managed daemon lifecycle

| # | Item | Depends on | Status |
|---|---|---|---|
| [#788](https://github.com/alexey-pelykh/sessiometer/issues/788) | `unregisterDaemonAgent()` has no call site | — | **MERGED** `13224b0` (PR #825) |

---

## Two findings that reshaped the scope

Both emerged from grounding the design against the code rather than from the original investigation.

### The blind spot was a documented decision, not an oversight

`src/observability.rs:206` states that a hard engine `Err` — explicitly including **"an unresolved binary"** — "has no such class, so it stays a bare `outcome=error` with no `reason=`."

Yet the same doc comment names its own motivation: making a wholesale failure diagnosable, citing "the #375 incident — every account a bare `error` for hours." And `systemic_refresh.rs` opens by naming "an unresolvable binary" as a cause it exists to catch.

**The enum was built so a #375-class outage would be diagnosable, and the cause that produced this #375-class outage is the one excluded from it.** Across the whole log, **0 of 411** refresh events carry a `reason=` field — while `status` tells the operator to "check the daemon log `reason=`."

The stated rationale is secret-safety, but `unresolved` is secret-free by construction. The real reason is structural: the case arrives as a hard `Err` and `refresh_tick.rs:397` discards the variant. #786 reverses the exclusion and amends the doc comment in the same change.

### The detector worked — a restart erased it

`refresh_systemic_failure` fired correctly at `2026-07-27T14:31:27Z` with `consecutive=3`. But across the entire log:

| Event | Count |
|---|---|
| `refresh_systemic_failure` | 1 |
| `refresh_systemic_recovered` | **0** |

The episode opened and never closed. `SystemicRefreshHealth` is a pure in-memory state machine in `DecisionState` — no serde, no persistence — so the restart reset it. The board then showed all six accounts `🟢` over a completely unfixed fault.

This is **worse under launchd**: `KeepAlive { SuccessfulExit: false }` restarts the daemon on abnormal exit, and each restart buys a fresh false-green window of at least N sweeps. The detector built to make a #375-class outage visible is periodically blinded by its own supervisor. → #787.

---

## Test suite

The headline requirement. **~93 specified cases** across the six items, each tied to a named AC.

Coverage dimensions: precedence · ordering and shadowing · parse robustness (`=`-in-value, newline-in-value, `XPATH=` substring, noise lines) · spawn failure modes (missing shell, non-zero exit, hang, `nologin`) · credential-scrub parity · fallback and degradation · edge-trigger preservation · secret-safety (#15) · regression baselines.

> **Correction — the "cross-platform (Linux)" dimension was withdrawn.** I originally scoped a Linux coverage axis on the stated constraint *"the resolver is not `cfg(target_os = "macos")`-gated and CI runs Linux jobs."* The first clause is true; **the second was an inference I never verified, and it is false.** The `#783` executor challenged the premise instead of satisfying it, and it checks out: `test` and `msrv` are both `runs-on: macos-latest`, every ubuntu job (`changes`, `deny`, `ci-ok*`, `gate-change-ack`) is a non-compiling gate, and `main` **already** fails to build for Linux via an un-gated `libc::getpeereid` at `src/daemon/peer_auth.rs:26`. Nothing enforces a Linux guarantee today. What survives as a real requirement is *introduce no new `cfg(target_os)` gate*; the Linux **verification** claim is unenforceable and has been struck from #784's AC9/T19. Whether Linux is supported at all is now [#797](https://github.com/alexey-pelykh/sessiometer/issues/797).

Two disciplines applied throughout:

- **Negative controls.** Every "it now resolves" test is paired with a test that fails if the mechanism is stubbed out — a green suite must not be able to mean "the test did nothing."
- **The launchd PATH as a named constant.** `/usr/bin:/bin:/usr/sbin:/sbin` appears literally in test names and comments, so today's bug is legible to a future reader without this brief.

### Gaps found by the coverage gate

Adversarially re-reading the spec against the code surfaced four items the original formulation missed. All were folded back into the issues.

| Gap | Where | Why it matters |
|---|---|---|
| **`getpw*` sole-caller SAFETY invariant** | #783 | `paths.rs:94` asserts `getpwuid` "is the crate's **ONLY** `getpw*` caller" — the memory-safety argument for the shared static buffer rests on it. A `pw_shell` reader is a third caller; both existing SAFETY comments must be updated or the argument becomes false-by-documentation. |
| **Harvest called per-account, not per-sweep** | #784 | `resolve_binary()` sits inside `refresh(&self, account)` — a 6-account sweep would spawn 6 login shells (~230 ms) every sweep. Now requires per-sweep memoization that does **not** span sweeps (or it becomes a startup freeze by another name). |
| **`nologin` / `false` login shells + timeout attribution** | #783 | A hung harvest inside `[refresh].timeout_secs` would misreport as `reason=timeout`, pointing the operator at the spawn instead of the harvest. |
| **Resolved-path log spam** | #786 | Per-account-per-cycle logging would bury the signal it was added to provide; now edge-triggered on change. |

---

## Decisions settled

Every open choice across the six items is now decided and recorded on the issue itself. Nothing is left to the implementer's discretion except what genuinely belongs there.

| # | Question | Decision |
|---|---|---|
| #783 | Harvest timeout bound | **5 s** — ~130× the measured ~38 ms, 18× below the 90 s `[refresh].timeout_secs`, so a hung harvest can never be misattributed as `reason=timeout` |
| #783 | One `getpwuid` call or three? | **Three** — add the shell accessor as a third function on the established immediate-copy pattern. Folding the existing two into one multi-field call is a refactor this issue has no reason to carry. The **SAFETY comments must still be updated** to enumerate all three callers |
| #784 | What is memoized, and for how long? | **The harvested PATH, 60 s TTL, engine-scoped.** Cache the stable/expensive thing (the user's PATH); never the volatile/cheap thing (the scan). The scan still runs every cycle, so #375 is fully preserved. Failures are never cached. 60 s = the existing `DEFAULT_REFRESH_IDLE_AFTER_SECS`, not an invented constant |
| #786 | Rewrite the `cli.rs` hint? | **No.** It was correct in intent; `reason=` was simply never populated. Once `reason=unresolved` lands the hint becomes truthful as written. Confirm, don't churn |
| #787 | Preflight or persistence? | **Preflight.** No new persistence surface, and self-healing — a persisted episode can be stale in the *false-positive* direction, showing `DOWN` to an operator who just fixed it. Persistence branch withdrawn |
| #788 | Stop affordance or delete? | **Neither — the fork was false.** See below |

### #788: the fork dissolved

`unregisterDaemonAgent()` gets its call site from the **re-registration path**, which was never optional — the SDK header's own remedy for a changed executable *is* an unregister call. So the method is **needed** (deletion is wrong), and it is reached from version-change handling (no Stop button needed to justify it).

The item is now a single-purpose correctness fix with **no open product decision**. A stop affordance, if ever wanted, is a new issue.

Settling it surfaced two things the original framing missed:

- **A live-daemon displacement hazard.** Re-registration may need to displace a daemon still running the *old* executable. Deferring to next launch or surfacing an honest beat are both fine; silently killing it is not. Now an explicit AC5 decision the implementer must record.
- **The two-owner guard outranks the SDK.** When `cliManagedAgentPresent` is true, the app must perform **no** re-registration at all — that agent is not the app's to manage. T9 flipped from covering the delete branch to guarding this.

## Readiness

| # | Verdict |
|---|---|
| #783 | **READY** |
| #784 | **READY when #783 lands** |
| #785 | **READY when #784 lands** |
| #786 | **READY** |
| #787 | **READY** |
| #788 | **READY** |

All six are ready. #784 and #785 are sequenced behind their dependencies; the other four can start immediately and in parallel.

---

## Current state — read before acting

The running daemon is a **shell-launched** `./target/release/sessiometer run -v` (PID 33005, PPID 32212, started `06:11:12Z`). `org.sessiometer.agent` remains **registered and enabled** but stood down on the held lock (`launchctl` shows PID `-`, last exit `0`).

That difference is the whole defect, and today's log is a clean natural A/B — same binary, same config, only the launch environment differs:

| `2026-07-28` refresh outcomes | Count |
|---|---|
| `outcome=error` (under launchd, `02:20Z`–`05:34Z`) | **30** |
| `outcome=refreshed` (shell-launched, `07:12:18Z`) | **1** |

The single success is the active account at `07:12:18Z` — `rotated=true`, `window_secs=26373`. Under launchd every cycle failed with `window_secs=0`; from a shell with the user's PATH, the very next cycle rotated a credential normally.

The board reads healthy, but that is a manual `poke` at `05:43–05:44Z` restoring credentials plus a restart clearing in-memory failure state — not a fix. The other five accounts are still quarantined and backed off (`backoff_secs=3600`), which is why only one account refreshed at all.

**The launchd path is armed. The bug returns the moment it takes over.**


---

## Delivery — batch `20260728-2342`

All six items merged 2026-07-28, sequentially, each independently verified before merge.

| # | Commit | PR |
|---|---|---|
| #783 | `6db659f` | #796 |
| #784 | `5222fdc` | #799 |
| #785 | `45ebea2` | #809 |
| #786 | `a2729de` | #812 |
| #787 | `b21934a` | #816 |
| #788 | `13224b0` | #825 |

**Follow-ups filed** (none of them riders on the fixes): [#797](https://github.com/alexey-pelykh/sessiometer/issues/797) Linux is uncompiled by CI and `main` already fails to build there · [#802](https://github.com/alexey-pelykh/sessiometer/issues/802) `poke`/`login` now resolve via the login-shell PATH · [#813](https://github.com/alexey-pelykh/sessiometer/issues/813) preflight-opened episode renders as a sweep that never ran · [#819](https://github.com/alexey-pelykh/sessiometer/issues/819) re-registration defers behind an any-provenance lock holder · [#820](https://github.com/alexey-pelykh/sessiometer/issues/820) launch-time failure reason can go unread.

### ⚠ Merged is not deployed

The running daemon (PID 33005, started 08:11:12 local) **predates every merge in this batch** and does not contain the fix. Verified by symbol inspection against `target/release/sessiometer`, canaried against a known-present symbol so the absence is real rather than a broken query:

| Symbol | Present |
|---|---|
| `refresh_systemic_failure` (pre-existing — the canary) | 1 |
| `refresh_binary_resolved` (#786) | 0 |
| `refresh_preflight_unresolved` (#787) | 0 |
| `LoginShellPathUnharvested` (#783) | 0 |

`org.sessiometer.agent` remains registered and armed, stood down on the held lock. **Deploying requires a rebuild from `main` plus a daemon restart** — a production action deliberately not taken as part of this batch.
