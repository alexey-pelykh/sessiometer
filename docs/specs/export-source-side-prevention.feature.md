<!--
SPECIFICATION STUB — not executable.
This repo has no Gherkin runner; the executable gates are the Rust test suite and the
scripts/check-*.sh shell gates. These scenarios pin each acceptance criterion in scenario form and
bind it to an ACC capability from the Master Test Plan
(docs/design/migration-credential-portability-solution-design.md § 11). Do not read a written
scenario as a passing test.
-->

# Feature: the source machine warns before it mints an artifact it will invalidate

Tracked as **issues #1050, #1051**. Requirements: PRD R-13, R-14, R-14a. Design § 4.9, RSK-10.

**Rule under test**: the design's position is that the staleness hazard is *not detectable at the
target — only preventable at the source*. There is currently zero source-side implementation of it.

**The predicate is "will this machine refresh?", not "is a daemon up?"** — liveness
(`cli::daemon_liveness`) **×** supervision (`service::AgentSupervision`, plus `service::is_managed`
for a plist that outlives its job). Scenarios below that say *warns* are the fail-closed majority;
exactly one stays quiet, and it needs all three factors to agree.

## Scenario: export warns when this machine's daemon is live  · Cap-10.1

    Given the local daemon is running
    When the operator runs `export`
    Then the command warns on STDERR that the daemon's next refresh will invalidate the artifact
    But does not block the export
    But not on stdout, which may carry the artifact itself

> **The stream is the assertion, not a detail.** *Added 2026-08-05 (twelfth pass); no scenario, no
> criterion and no issue named a stream, and the design's Interface-Change table said `export`
> **stdout**.* With `PATH` omitted, `export` writes the artifact to stdout
> (`src/cli.rs:4559-4565`; `EXPORT_USAGE` documents *"stdout if omitted"* at `:1282`). The existing
> `PLAINTEXT_WARNING` already takes this rule, with the reason in the code: *"Warn on stderr — never
> stdout, which may carry the artifact"* (`src/cli.rs:4472-4474`). A warning on stdout prepends its
> bytes to the artifact, which then fails `preamble.magic != MAGIC` on import
> (`src/migration.rs:360`). Assert the stream, or the warning written to save the migration destroys
> it — on the branch where it fires, so every no-daemon test stays green.

## Scenario: export is quiet when no daemon runs and none is due to start  · Cap-10.1

    Given the local daemon is not running
    And launchd holds no job for the agent
    And no agent plist is on disk to start one at next login
    When the operator runs `export`
    Then no warning is emitted
    But not by reading the liveness probe alone

    # Warning unconditionally trains dismissal — RSK-1's failure mode reproduced on a second
    # surface, and it would destroy the signal exactly where it matters. The three Givens are the
    # price of keeping it quiet only where quiet is TRUE — see the axis note below.

> **The quiet branch is a conjunction, not `NotRunning`.** *Corrected 2026-08-11 (thirteenth pass,
> issue #1062); this scenario's sole Given was "the local daemon is not running", which is
> `daemon_liveness()` read as if it answered the question this feature asks.* It does not.
> `daemon_liveness` is documented as *"The daemon **process** liveness"* — a point-in-time read of
> the control socket with the single-instance lock as fallback. The question Cap-10.1 asks is
> *will this machine refresh and invalidate the artifact?*, which is liveness **×**
> `service::AgentSupervision`, and only the first factor was ever enumerated. Deepening the
> liveness axis (the sixth pass's tri-state, the eighth pass's `Err`) could not reach this: those
> passes made the probe answer more honestly, and a confident, correct `NotRunning` is exactly what
> the two states below produce.
>
> **The codebase never treats liveness as sufficient, and says so at every call site.** `daemon_status`
> and `daemon_restart` each pair `daemon_liveness` with `service::agent_supervision`, and `daemon_stop`
> dispatches on supervision alone (`src/cli.rs`). This feature was the only surface reading liveness
> on its own.

## Scenario: a daemon between respawns still warns  · Cap-10.1

    Given launchd holds a job for the agent with no running process behind it
    And the control socket does not answer and the single-instance lock is not held
    When the operator runs `export`
    Then the command warns, exactly as it does for a responsive daemon
    But not by reporting that no daemon will refresh

> `AgentSupervision::RegisteredIdle`, whose own doc comment contains *"or it is simply between
> respawns"* (`src/service.rs`). The agent plist is written `RunAtLoad` true with a conditional
> `KeepAlive` of `{SuccessfulExit: false}` (`service::render_plist`), so a daemon that exited
> non-zero is respawned — and while launchd throttles that respawn there is no process, no socket
> and no lock. Every liveness probe is therefore correct and the artifact is still doomed: launchd
> brings the daemon back, the refresh tick rotates the refresh token, and the credentials are dead
> on arrival. This is the hazard in the feature title, reached through the branch built to stay
> quiet.

## Scenario: a booted-out agent that returns at login still warns  · Cap-10.1

    Given no job for the agent is in the launchd domain
    And the agent plist is still on disk
    When the operator runs `export`
    Then the command warns, exactly as it does for a responsive daemon
    But not by treating an absent launchd job as an absent daemon

> The second reachable state, and the one an operator walks into deliberately: `daemon stop` boots
> the agent out of the domain **but leaves the plist registered for next login** — stated in
> `AgentSupervision::Unregistered`'s own doc comment (`src/service.rs`), and the reason
> `service::is_managed` (plist existence) and `agent_supervision` (domain membership) are separate
> questions rather than one. An operator who stops the daemon precisely so the export is safe gets
> silence, exports, and is invalidated at their next login by `RunAtLoad`. Supervision alone does
> not catch this one either — it reads `Unregistered`, the same as a machine that never installed
> the service — so the predicate needs plist existence as well.
>
> **The product already tells the operator this, on the stop that creates the state.**
> `service::stop_managed` prints *"It returns at next login; `sessiometer service uninstall` removes
> it for good."* So a machine can carry a shipped, accurate promise that the daemon is coming back
> while `export` on that same machine says nothing. The two surfaces are not merely inconsistent —
> the one that knows is the one that already spoke.

## Scenario: a live-but-unresponsive daemon still warns  · Cap-10.1

    Given the control socket does not answer but the single-instance lock is held
    When the operator runs `export`
    Then the command warns, exactly as it does for a responsive daemon
    But not by reporting the daemon as not running

> The middle state of `daemon_liveness()`'s tri-state (`src/cli.rs:1870-1878`), and the one an
> implementer must not invent an answer for. A daemon that is starting up or wedged **holds the lock
> and will still refresh**, so it invalidates the artifact just as a responsive one does — fail
> **closed** and warn. The variant's own doc comment says it is *"Reported honestly, NOT as 'not
> running'"*. A two-state test that maps this to the quiet branch ships silence at the moment the
> warning matters.

## Scenario: a probe failure warns, and does not fail the export  · Cap-10.1

    Given the liveness probe returns an error rather than a liveness verdict
    When the operator runs `export`
    Then the command warns, exactly as it does for a responsive daemon
    And the export still completes
    But not by leaving the warn-or-quiet choice to the implementer

> **`daemon_liveness()` has four outcomes, not three.** *Added 2026-08-05 (eighth pass).* Its
> signature is `Result<DaemonLiveness>` (`src/cli.rs:1885`), so the `Err` branch sits alongside the
> three `Ok` variants. The sixth pass corrected every AC and scenario here from two-state to the
> tri-state of `DaemonLiveness` — and assigned no behaviour to `Err`, leaving the same gap one level
> down. An unassigned branch is decided by whoever implements it, at the point where being wrong is
> silent.
>
> **Fail closed, as the tri-state does.** A probe that errors has not established the daemon is
> absent; if it is in fact running, it will refresh and invalidate the artifact. Warning on an
> inconclusive probe costs a redundant line; staying quiet costs the artifact. This does **not** make
> the warning unconditional — the quiet branch above still exists, which is what keeps RSK-1's
> dismissal-training failure closed.
>
> *Amended 2026-08-11 (issue #1062): this read "`NotRunning` remains the quiet branch". `NotRunning`
> is now necessary for quiet and no longer sufficient — the same fail-closed reasoning, applied to
> the supervision axis this note could not see.*

## Scenario: an export and its import can be correlated  · Cap-10.2

    Given an artifact exported on one machine and imported on another
    When both events are read
    Then a common artifact digest identifies them as the same artifact
    And the import event carries the scope the operator requested
    But neither carries a label, token, or email
    But not by requiring a requested-scope field on the export event

    # The requested scope, never the artifact's claimed scope (AD-6).
    # Import-only, and deliberately: `export` takes no narrowing flag (R-9c, AD-5, Cap-7.5), so it has
    # no operator-requested scope to log. Asserting one there forces either an inert constant or an
    # export scope flag that violates R-9c in the same change. Export correlates on the digest alone.
