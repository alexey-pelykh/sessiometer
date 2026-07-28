// Copyright (c) 2026 Oleksii PELYKH
// SPDX-License-Identifier: MIT

// Client-side RUN-STATE probe for the bundled daemon LaunchAgent's launchd job (issue #819) — the
// provenance-bearing sibling of `DaemonLockProbe`, which is deliberately provenance-BLIND.
//
// WHY A SECOND PROBE, when `DaemonLockProbe` already answers "is a daemon alive?". Because that is
// not the question the stale-registration repair needs. `DaemonLockProbe` is any-provenance BY
// DESIGN (issue #742): it reports a hand-run `sessiometer run` exactly as it reports a
// launchd-started one, which is right for the Start affordance (don't offer to start a daemon when
// one is already serving) and wrong for the repair (issue #788), whose invariant is narrower:
// `SMAppService.unregister()` unloads OUR launchd job, and unloading a job can only terminate a
// daemon IF THAT JOB IS RUNNING. So the repair asks launchd about OUR label — not the lock about
// anyone — and `DaemonLockProbe`'s any-provenance contract is left untouched at its other call sites.
//
// WHY `launchctl`, and what that costs. `SMAppService.Status` reports REGISTRATION (`.enabled` vs
// `.notRegistered`), never run state — a registered-but-not-running agent is `.enabled`, which is
// precisely the state issue #819 was observed in. There is no public API for a launchd job's run
// state, so `launchctl print gui/<uid>/<label>` — the documented human interface, and the exact
// command the issue's evidence was captured with — is the signal. That means spawning a process:
// a LOCAL, read-only query of a system binary, in the same tier as `DaemonLockProbe`'s filesystem
// `flock`. No network, no keychain, no daemon command; the app stays a pure IPC client (ADR-0011).
// The cost is that the query is synchronous on a `@MainActor` caller — see `timeout` for the bound
// and what it does and does not promise.
//
// PARSING IS THE RISK, so it fails SAFE. Every ambiguity — a non-zero exit, a missing `state` line,
// a value we do not recognise, a timed-out spawn — lands `.unknown`, which the caller reads as "I
// cannot tell" and falls back to the pre-#819 any-provenance gate. A format drift therefore makes
// the repair conservative again; it can never make it destructive.
//
// SPLIT: `classify` is pure over its two arguments and carries the whole decision, so it is
// unit-tested against captured real `launchctl print` output (`LaunchdJobProbeTests`). `runState`
// is the thin, untestable spawn wrapper around it — the same tested-shell / untested-OS-wrapper
// split `LoginItemModel` vs `SMAppServiceLoginItemService` uses.

import Foundation
import os

private let launchdProbeLog = Logger(subsystem: "org.sessiometer.menubar", category: "login-item")

enum LaunchdJobProbe {

    /// `launchctl`'s absolute path — spawned by path rather than via `PATH` lookup, so the probe
    /// cannot be redirected by an inherited environment.
    static let launchctlPath = "/bin/launchctl"

    /// How long the spawn may take before it is abandoned, and a trip lands `.unknown` (conservative),
    /// never a fabricated verdict.
    ///
    /// Stated precisely, because the caller is `@MainActor`: this CAPS the hold on the app's launch
    /// path, it does not prevent one. `runState` is synchronous, so a wedged `launchctl` blocks the
    /// main thread for up to this long. Measured cost of a real query is ~0 ms, and it runs only on an
    /// actual identity change (gate 3), so the bound is a tripwire rather than a budget.
    static let timeout: TimeInterval = 2.0

    /// The one-tab-indented `state` line of the job's OWN property block in `launchctl print`
    /// output. The indentation is load-bearing, not incidental: real output also carries
    /// `\t\tstate = active` lines inside nested endpoint dictionaries, which a depth-blind match
    /// would read as the job's state. Matching exactly one leading tab selects the job's own line
    /// and nothing else. (`\tjob state = exited` is a different key and does not match this prefix.)
    private static let jobStatePrefix = "\tstate = "

    /// Classify `launchctl print`'s result for one job. PURE over its arguments — this is the whole
    /// decision, so it is the thing under test.
    ///
    /// - `exitStatus != 0` ⇒ `.unknown`. `launchctl` exits non-zero for "could not find service"
    ///   (113 as measured) but also for a bad domain or a denied request, and those carry no run-state
    ///   information. Rather than read one of them as "not loaded, therefore safe to unload", every
    ///   non-zero lands `.unknown` and the caller stays conservative.
    /// - `state = running` ⇒ `.running`; `state = not running` ⇒ `.notRunning`.
    /// - anything else — no job-state line, or a value this does not recognise ⇒ `.unknown`. Never
    ///   guessed: an unrecognised value is reported as unknown, not defaulted to either verdict.
    static func classify(exitStatus: Int32, output: String) -> DaemonAgentRunState {
        guard exitStatus == 0 else { return .unknown }
        for line in output.split(separator: "\n", omittingEmptySubsequences: false) {
            guard line.hasPrefix(jobStatePrefix) else { continue }
            switch line.dropFirst(jobStatePrefix.count).trimmingCharacters(in: .whitespaces) {
            case "running": return .running
            case "not running": return .notRunning
            default: return .unknown
            }
        }
        return .unknown
    }

    /// Ask launchd what `label` is doing in the calling user's GUI domain. Bounded, read-only, and
    /// `.unknown` on every failure path — see the file header.
    static func runState(label: String, uid: uid_t = getuid()) -> DaemonAgentRunState {
        let process = Process()
        process.executableURL = URL(fileURLWithPath: launchctlPath)
        process.arguments = ["print", "gui/\(uid)/\(label)"]
        let stdout = Pipe()
        process.standardOutput = stdout
        // Discarded: the "Could not find service" message goes here, and the exit status already
        // carries everything `classify` acts on. Redirecting it (rather than inheriting) keeps the
        // message out of the app's own stderr — to `nullDevice` rather than a `Pipe`, because nothing
        // below ever drains it and an undrained pipe is a buffer that can only fill.
        process.standardError = FileHandle.nullDevice

        do {
            try process.run()
        } catch {
            launchdProbeLog.error(
                "launchd run-state probe could not spawn: \(String(describing: error), privacy: .public)")
            return .unknown
        }

        // A wedged child must not hold the launch path. Terminating it closes the pipe, which is
        // what releases the blocking read below.
        let watchdog = DispatchWorkItem { process.terminate() }
        DispatchQueue.global(qos: .userInitiated).asyncAfter(deadline: .now() + timeout, execute: watchdog)

        // Drain to EOF BEFORE waiting for exit. `launchctl print` emits multiple KB for one job, so
        // waiting first would deadlock the moment the pipe buffer filled with the child still writing.
        let data = stdout.fileHandleForReading.readDataToEndOfFile()
        process.waitUntilExit()
        watchdog.cancel()

        // A watchdog kill exits by SIGNAL, and its termination status is NOT an exit code — feeding
        // it to `classify` would classify a timeout as a launchctl verdict. Report the timeout as
        // what it is: no answer.
        guard process.terminationReason == .exit else {
            launchdProbeLog.error("launchd run-state probe timed out after \(Self.timeout, privacy: .public)s")
            return .unknown
        }
        return classify(exitStatus: process.terminationStatus, output: String(decoding: data, as: UTF8.self))
    }
}
