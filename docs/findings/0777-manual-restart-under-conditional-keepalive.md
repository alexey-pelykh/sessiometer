# Finding #777 — does a manual `Restart…` mean anything under conditional `KeepAlive`? (spike)

The second of the two orphans left by the menubar deferral `// (View log / Restart remain #169/#171
siblings.)` in `apps/menubar/Sources/StatusPanelView.swift`. Its sibling `View log` was re-anchored as
#776 and simply needs building; `Restart…` does not, because issue #742 moved the ground under it. This
spike asks whether the mock's decision still holds, and answers it from measured launchd behavior rather
than from the plist.

**Verdict — (b) AMEND THE MOCK: drop `Restart…`.** Under the conditional `KeepAlive`
(`{SuccessfulExit: false}`) that #742 shipped, launchd **already** restarts the daemon on every
non-clean exit, on a **measured 10.072 s cadence** (n = 16, range 10.055–10.089 s). A manual restart in
the crash-looping state therefore requests exactly what the supervisor is already doing — and measurably
makes it **worse**: a `launchctl kickstart -k` issued mid-throttle **cost one full extra respawn cycle**
(interval **20.068 s / 20.072 s** against the 10.072 s baseline, replicated 2 of 2), i.e. **≈ 10 s of
*additional* daemon downtime per press**, while **blocking the caller ≈ 16.37 s** (16.354 s / 16.384 s).
Pressing the button lengthens the outage it appears to fix, and hangs the popover while doing so.
Independently, the app has **no restart primitive to wire it to**: `SMAppService` exposes only
`register()` / `unregister()` / `status`, and the daemon's control socket has **eleven** verbs, none of
them `restart`. The affordance is redundant, harmful, and unimplementable-as-drawn — three independent
grounds, any one sufficient. `View log` (#776) is the survivor of the pair: it surfaces the *cause* the
crash loop is signalling, which is the thing a restart would mask.

## Data-availability boundary (read first)

The operator's real service was running throughout and is **deliberately untouched** — no `kickstart`,
`bootout`, `stop`, `start`, `restart`, `install`, or `kill` was issued against `org.sessiometer.agent`,
and no process was signalled. What that costs, and how each claim is grounded:

- **Measured, on a throwaway agent** (`org.sessiometer.spike777.throttleprobe`, invented for this spike,
  booted out and verified gone): the respawn cadence, the kickstart-during-throttle penalty, and the
  caller-block duration. The throwaway plist carries the **same supervision shape** as the real agent —
  `RunAtLoad: true` + `KeepAlive {SuccessfulExit: false}` (compare
  `apps/menubar/LaunchAgents/org.sessiometer.agent.plist`) — and launchd reported the same
  `minimum runtime = 10` for both, so the measurements transfer.
- **Measured, read-only, on the real agent**: `minimum runtime = 10`,
  `semaphores = { successful exit => 0 }`, `runs = 2`, `last exit code = 0` — read via
  `launchctl print`, which starts, stops, and signals nothing. Post-experiment re-read was byte-identical
  (`runs = 2`, `last exit code = 0`), confirming no disturbance.
- **NOT observed, and not asserted as if it were** — `SMAppService.register()` / `unregister()` behaviour
  under a live crash loop. The only registrable daemon-agent identity on this machine **is** the real
  `org.sessiometer.agent`, so exercising it would have meant touching the operator's service. That
  branch is reasoned from the API surface in `LoginItemModel.swift` / `SMAppServiceLoginItem.swift`
  (which is *complete* — the protocol enumerates every OS call the app can make) and is marked
  🟡 *API-surface-reasoned* below, never as a measurement.
- **NOT observed**: the real daemon crash-looping. Inducing one on the operator's service is out of
  bounds; the throwaway agent is the stand-in, and it is a faithful one for *supervision* behaviour
  (which is what the question turns on), not for daemon-internal behaviour.

## Results at a glance

| # | Question (from the issue) | Verdict | Basis |
|---|---|---|---|
| **1** | What state does a manual restart change that launchd's respawn does not? | ✅ **Nothing** — in `.crashLooping`, none. launchd is already respawning at 10.072 s | measured cadence, n = 16 |
| **2** | Does `kickstart -k` interact safely with the respawn throttle? | ❌ **No — it worsens it**: +1 full cycle (≈ 10 s more downtime), caller blocks ≈ 16.37 s | measured, replicated 2/2 |
| **2′** | Does `SMAppService` offer a restart at all? | ❌ **No such primitive** — `register` / `unregister` / `status` only | 🟡 API-surface-reasoned |
| **3** | Is `crash-looping` even the right state for it? | ❌ **No** — and the one state where a restart *would* be unique (wedged-but-alive) is a **different** state | code + measurement |
| **4** | If it adds nothing, amend the mock | ✅ **AMEND** — drop the affordance; keep `View log` (#776) | synthesis |

## (1) launchd is already doing it — the measured cadence

A crash-looping daemon is, by definition, exiting non-cleanly; `KeepAlive {SuccessfulExit: false}` is
exactly the clause that respawns on a non-clean exit. So the supervisor is already in the restart
business, and the only question is how fast.

launchd throttles respawns at its `minimum runtime` — reported as **10** for the real agent and for the
throwaway alike. Observed on a job that exits `1` immediately (19 launches, 16 uninterrupted intervals):

| | value |
|---|---|
| mean interval | **10.072 s** |
| range | 10.055 – 10.089 s |
| n | 16 |

The cadence is metronomic. Whatever a crash loop costs the operator, **it self-corrects every ~10 s
without any UI at all** — and the moment the underlying fault is fixed (config corrected, keychain
unlocked, binary replaced), the very next respawn picks it up, ≤ 10.1 s away.

## (2) A manual restart makes it worse — measured, replicated

The interesting case is not the steady state but the *interaction*: what does a restart request do when
issued **inside** a throttle window? Two independent trials, each issuing
`launchctl kickstart -k` ≈ 3.6 s into a cycle (≈ 6.4 s of throttle still owed):

| Trial | Issued at (offset into cycle) | Interval that followed | vs baseline | Caller blocked |
|---|---|---|---|---|
| 1 | +3.687 s | **20.068 s** | **+9.996 s** | **16.354 s** |
| 2 | +3.656 s | **20.072 s** | **+10.000 s** | **16.384 s** |
| — | *(baseline, no kickstart)* | 10.072 s | — | — |

The full series, with the two interventions visible as the only non-10 s intervals:

```
10.068  10.074  10.079  10.058  10.071  10.078  10.063
20.068  ← kickstart trial 1
10.064  10.071  10.068  10.085  10.066
20.072  ← kickstart trial 2
10.055  10.069  10.088  10.089
```

Two consequences, both bad, both reproducible:

1. **It delays the restart it asks for.** The naturally-due launch was *skipped*; the process came up one
   full interval later. Net effect of pressing the button: **≈ 10 s of extra downtime**. The cadence
   returns to 10.06 s immediately afterwards, so this is a per-press cost, not a lasting degradation —
   but a crash-loop panel invites *repeated* pressing, and each press buys another ~10 s of outage.
2. **It blocks the caller for ≈ 16.37 s.** `kickstart` does not return until the job actually spawns —
   in both trials it returned **within ~30 ms of the launch** (0.027 s / 0.032 s before the child's own
   timestamp). A menubar popover is not a place to spend 16 s waiting, and the state in which the button
   is offered is precisely the state in which that wait is guaranteed.

*Mechanism note, stated honestly*: the **observable** is "the due launch is skipped and the job spawns
one interval later." Whether launchd internally re-arms the throttle window or drops the pending launch
is **not** directly observed and is not asserted here; the operator-visible cost is the measured part,
and it is what the design decision turns on.

## (2′) There is nothing to wire the button to

Even setting the throttle aside, the affordance cannot be built as drawn without inventing a capability:

- **`SMAppService` has no restart.** The `LoginItemService` protocol — the *complete* enumeration of OS
  calls the app makes — offers `registerApp` / `unregisterApp` / `registerDaemonAgent` /
  `unregisterDaemonAgent` / status reads / `openLoginItemsSettings`. A "restart" would have to be
  composed as `unregister()` + `register()`, which is not a process restart at all: it mutates the
  **registration**, and re-registering has a known failure mode (a register can throw `EPERM` when the
  user has toggled the item off in System Settings, which `startDaemon()` already has to surface as
  `.failed`). Trading a self-healing 10 s respawn for a registration round-trip that can fail *and* can
  leave the agent unregistered is a strictly worse deal.
- **The control socket has no `restart` verb.** Its verbs are `status`, `manual-swapped`,
  `roster-reload`, `restored`, `shutdown`, `watch`, `stats`, `swap`, `capture`, `config-get`,
  `config-set` (`src/daemon/socket.rs`). Nor could there be a useful one: a daemon cannot restart itself
  over a socket that dies with it, and a crash-looping daemon is not reliably answering anyway.
- **Shelling out to `/bin/launchctl` is the only remaining route**, and it breaks the app's
  pure-IPC-client posture, inherits the 16.37 s block measured above, and *still* fails in the common
  unmanaged configuration: `plan_restart` returns `RestartPlan::RefuseUnmanaged` when a daemon is running
  that launchd does not supervise. That is not a corner case — it is the state of **this** machine right
  now (the live daemon is a foreground `sessiometer run`, and `org.sessiometer.agent` sits registered
  but idle at `last exit code = 0`, the clean stand-down #742 defines).

## (3) `crash-looping` is the wrong home — the right one is a different state

`.crashLooping` is **not** a launchd report and **not** a daemon report. It is a **client-side
inference**: `HonestStateMachine` counts `consecutiveUnstableReconnects` — reconnects whose held
snapshot dropped before surviving the stability window — and at `crashLoopThreshold = 2` renders the
fault shape. The app therefore has **no visibility into launchd's throttle phase**, and could not time a
restart to dodge the penalty in (2) even if it wanted to.

Enumerating where a manual restart could conceivably be unique:

| Panel state | Daemon condition | Does `KeepAlive` already respawn? | Manual restart adds |
|---|---|---|---|
| `.crashLooping` | exiting non-cleanly, repeatedly | **yes**, every ~10.07 s | **nothing** — and costs ~10 s + a 16 s block |
| `.notRunning` | absent / cleanly exited | no | already served by **`Start daemon`** (#170) |
| `.disconnected` / `.stale` | socket dropped; process may be **alive but wedged** | **no** — a hung process never exits, so the clause never fires | 🟡 *plausibly something* — see below |
| `.unsupported` | version skew | n/a | nothing — needs an upgrade, not a restart |

So the mock places `Restart…` in the **one state where it is provably redundant**, and omits it from the
one state where a restart is the only thing that could help. That inversion is the strongest single
argument that the mock's decision was made before #742 changed the supervision model, and has not been
re-examined since.

**This does not license a RESHAPE into `.disconnected`.** That would require building a real capability
against a state the app cannot diagnose: from the client side, "wedged" and "busy" and "socket
transiently dropped" are indistinguishable, and the app would be offering a destructive action on a
guess. It also inherits every problem in (2′) — no primitive, and a hung daemon still holds
`daemon.lock`, so an `unregister`/`register` round-trip yields an instance that loses the lock and
cleanly stands down (exit `0`, #742), changing nothing. A wedged-daemon watchdog is a **separate,
genuinely open question** with its own design work (who detects the hang? the daemon itself, via a
self-watchdog that exits non-cleanly so `KeepAlive` *can* fire? that is a daemon-side fix, not a
button). It should be its own item **if it ever manifests**, not smuggled in as the justification for an
affordance in a different state.

## (4) It also contradicts the panel's own stance

Two design arguments, independent of the measurements, both pointing the same way:

- **The banner says the opposite of what the button does.** The crash-loop copy is *"Restarting
  repeatedly; holding status until it stays up."* The panel is deliberately **refusing to act on unstable
  data** — the crown-jewel anti-#137 debounce. A `Restart…` beside it invites the operator to churn
  precisely the thing the panel just said it is waiting to stabilise.
- **It masks the signal.** A crash loop *means something*: bad config, a keychain it cannot reach, a
  corrupt state file. Restarting is the one action that resets the visible symptom without touching the
  cause — while, per (2), measurably prolonging it. `View log` (#776) is the affordance that answers
  *why*, which is the only question the operator can usefully act on in that state. The mock drew the
  pair together; only one of them survives scrutiny.

## Downstream

**The follow-up this verdict warrants (to be filed by the orchestrator — this spike does not open it):**

- **Amend the mock** (`apps/menubar/design/menubar-preview.html`, crash-looping frame): remove the
  `Restart…` `.btn`, leaving `View log` as the sole action, and drop the trailing *"Restart is behind a
  confirm."* clause from the `msg-hint`. Record the reason inline so the next reader does not
  re-derive it.
- **Re-anchor the deferral comment** (`apps/menubar/Sources/StatusPanelView.swift`, grep
  `View log / Restart remain`): it currently defers both affordances to two closed issues. With
  `View log` re-anchored as #776 and `Restart…` dropped here, the comment should name #776 alone.
  *(Both edits are deliberately out of scope for this item — a verdict files them, it does not execute
  them.)*

**Explicitly NOT filed** — the wedged-but-alive daemon gap (§ 3). It is real but unmanifested, its
natural fix is daemon-side (a self-watchdog that exits non-cleanly so the existing `KeepAlive` clause
fires) rather than a panel button, and filing it off the back of this spike would smuggle speculative
scope through a question that was about a different state. Noted here so the analysis is not lost.

**Unaffected**: `daemon restart` (#376/#397) keeps its full four-outcome dispatch. This verdict is about
a *panel affordance*, not the CLI verb — an operator at a terminal has context, a stable process, and
`RestartPlan`'s honest refusals; a popover button in a crash loop has none of those.

## Provenance

Measurement by throwaway LaunchAgent `org.sessiometer.spike777.throttleprobe` (a label invented for this
spike; plist + probe script session-local under `.tmp/spike777/`, uncommitted), booted out at the end and
verified gone. 19 launches over 2026-07-28 ≈ 19:31–19:34 local; two `launchctl kickstart -k` trials
against **that** label only. The operator's `org.sessiometer.agent` was read **read-only**
(`launchctl print`, `launchctl list`) and re-verified byte-identical afterwards (`runs = 2`,
`last exit code = 0`); its live daemon (PID 33005, an unmanaged `sessiometer run`) was never signalled.
**No credentials read or written; no network call.** Timings are one machine's macOS 25.5.0 launchd,
descriptive rather than a platform-general constant — the *direction* (kickstart costs a cycle and blocks
the caller) is the load-bearing part, not the exact milliseconds. Cross-checks:
`apps/menubar/LaunchAgents/org.sessiometer.agent.plist` (supervision shape), `src/service.rs` (#742
`KeepAlive` rationale, `kickstart_managed`), `src/cli.rs` (`plan_restart` / `RestartPlan`),
`src/daemon/socket.rs` (verb set), `apps/menubar/Sources/HonestStateMachine.swift`
(`crashLoopThreshold`), `apps/menubar/Sources/LoginItemModel.swift` (`LoginItemService` surface),
`apps/menubar/design/menubar-preview.html` (the mock under interrogation).

Throwaway-agent measurement 2026-07-28 · sessiometer issue #777 · siblings #776 (`View log`, under the
#772 log-access umbrella), #742 (conditional `KeepAlive`).
