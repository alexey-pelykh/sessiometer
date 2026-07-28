// Copyright (c) 2026 Oleksii PELYKH
// SPDX-License-Identifier: MIT

// Hermetic tests for the launchd run-state probe's PARSER (issue #819). `LaunchdJobProbe.classify` is the
// whole decision — `runState` around it is only the spawn — so this is where the signal is pinned.
//
// The fixtures below are REAL `launchctl print gui/<uid>/org.sessiometer.agent` output, captured on a machine
// standing in the exact state issue #819 describes (an agent registered by `smd`, `state = not running`,
// `active count = 0`, while a hand-run `sessiometer run` held the single-instance lock), plus a real running
// job's block. They are trimmed for length but NOT reshaped: the indentation, the key spellings, and the
// decoy lines are verbatim, because those are precisely what the parser discriminates on. Hand-idealised
// fixtures would test the parser against a format that does not exist.
//
// No `launchctl` is spawned here. The spawn wrapper is the untested-OS-wrapper half of the same split
// `LoginItemModel` (tested) vs `SMAppServiceLoginItemService` (app-only) uses.

import Foundation
import XCTest

final class LaunchdJobProbeTests: XCTestCase {

    // MARK: - The two verdicts, from real output

    /// The issue #819 state itself: our agent registered and loaded, but NOT running. This is the reading that
    /// green-lights the repair, so it is the single most load-bearing assertion in the file.
    func testRealNotRunningOutputReadsNotRunning() {
        XCTAssertEqual(LaunchdJobProbe.classify(exitStatus: 0, output: Self.notRunningOutput), .notRunning)
    }

    /// The opposite verdict from a real running job — the reading that makes the repair defer.
    func testRealRunningOutputReadsRunning() {
        XCTAssertEqual(LaunchdJobProbe.classify(exitStatus: 0, output: Self.runningOutput), .running)
    }

    // MARK: - The discriminations the format actually demands

    /// `not running` CONTAINS `running`, so a substring match would read every stopped job as running — and
    /// deferring forever on a stopped job is precisely the bug issue #819 exists to fix, re-introduced one
    /// layer down. The value is matched exactly, and this is what says so.
    func testNotRunningIsNeverMistakenForRunningBySubstring() {
        XCTAssertEqual(LaunchdJobProbe.classify(exitStatus: 0, output: "\tstate = not running\n"), .notRunning)
        XCTAssertEqual(LaunchdJobProbe.classify(exitStatus: 0, output: "\tstate = running\n"), .running)
    }

    /// Real output carries `\t\tstate = active` lines inside nested endpoint dictionaries. A depth-blind match
    /// would read one of those as the job's own state — and `active` is neither verdict, so the probe would go
    /// `.unknown` and the repair would fall back to the any-provenance gate forever. Both real fixtures above
    /// contain these decoys; this pins the discrimination in isolation, including when a decoy comes FIRST.
    func testNestedStateLinesAreNotMistakenForTheJobsOwnState() {
        let decoyFirst = """
        gui/502/org.sessiometer.agent = {
        \tendpoints = {
        \t\t"org.sessiometer.agent" = {
        \t\t\tstate = active
        \t\t}
        \t}
        \tstate = not running
        }
        """
        XCTAssertEqual(LaunchdJobProbe.classify(exitStatus: 0, output: decoyFirst), .notRunning,
                       "the job's own one-tab `state` line is the signal, whatever nested dictionaries say")
    }

    /// `job state = exited` is a DIFFERENT key that also ends in `state = <value>`. It must not be read as the
    /// job's run state — `exited` would classify as unrecognised and mask the real line.
    ///
    /// The decoy comes FIRST, and that ordering is the test. With the real line first the parser returns
    /// before the decoy is ever reached, so the assertion holds under a key-blind `contains("state = ")`
    /// match too — passing for a reason that has nothing to do with what the name claims. Decoy-first is what
    /// actually exercises the discrimination; the real-line-first case follows only to pin order-independence.
    func testTheJobStateKeyIsNotTheStateKey() {
        let decoyFirst = "\tjob state = exited\n\tstate = running\n"
        let realLineFirst = "\tstate = running\n\tjob state = exited\n"
        XCTAssertEqual(LaunchdJobProbe.classify(exitStatus: 0, output: decoyFirst), .running,
                       "a key-blind match would read `exited` here and mask the job's own state line")
        XCTAssertEqual(LaunchdJobProbe.classify(exitStatus: 0, output: realLineFirst), .running,
                       "and the verdict does not depend on which order launchctl emits the two keys")
    }

    // MARK: - Everything ambiguous fails SAFE

    /// A non-zero exit carries no run-state information — "could not find service", a bad domain, and a denied
    /// request all land here. None of them may be read as "not running, therefore safe to unload".
    func testNonZeroExitIsUnknownNeverAVerdict() {
        // 113 is what `launchctl print` returns for an unknown label, measured on macOS 26.
        XCTAssertEqual(LaunchdJobProbe.classify(exitStatus: 113, output: ""), .unknown)
        XCTAssertEqual(LaunchdJobProbe.classify(exitStatus: 1, output: Self.notRunningOutput), .unknown,
                       "a non-zero exit is not trusted even when the output looks parseable")
    }

    /// Output with no job-state line at all — the shape an output-format change would produce. `.unknown` is
    /// the honest answer, and it is what routes the caller back to issue #788's conservative gate.
    func testMissingStateLineIsUnknown() {
        XCTAssertEqual(LaunchdJobProbe.classify(exitStatus: 0, output: ""), .unknown)
        XCTAssertEqual(LaunchdJobProbe.classify(exitStatus: 0, output: "gui/502/x = {\n\tactive count = 0\n}\n"),
                       .unknown)
    }

    /// A recognised key with an UNRECOGNISED value is not guessed in either direction. Reading an unfamiliar
    /// value as `.notRunning` would license an unregister on no evidence; reading it as `.running` would
    /// re-create the permanent deferral. `.unknown` says what is true.
    func testUnrecognisedStateValueIsUnknown() {
        XCTAssertEqual(LaunchdJobProbe.classify(exitStatus: 0, output: "\tstate = waiting\n"), .unknown)
        XCTAssertEqual(LaunchdJobProbe.classify(exitStatus: 0, output: "\tstate = \n"), .unknown)
    }

    /// Indentation is the discriminator, so a format that used spaces instead of tabs must degrade to
    /// `.unknown` — conservative — rather than silently matching nothing and being mistaken for a verdict.
    func testSpaceIndentedOutputDegradesToUnknownRatherThanGuessing() {
        XCTAssertEqual(LaunchdJobProbe.classify(exitStatus: 0, output: "    state = not running\n"), .unknown)
    }

    // MARK: - The production wiring

    /// The probe's own configuration, pinned so the spawn cannot be quietly re-pointed. Named for what it
    /// measures — no spawn happens here: `runState` would query the real launchd, so only its two constant
    /// inputs are read. The equality assertion alone would be a literal-against-literal tautology; the
    /// existence check is the one with an external truth behind it.
    func testProbeTargetsTheSystemLaunchctlWithABoundedTimeout() {
        XCTAssertEqual(LaunchdJobProbe.launchctlPath, "/bin/launchctl",
                       "spawned by absolute path so an inherited PATH cannot redirect the probe")
        XCTAssertTrue(FileManager.default.isExecutableFile(atPath: LaunchdJobProbe.launchctlPath),
                      "the path the probe spawns must actually exist on the platform it ships to")
        XCTAssertGreaterThan(LaunchdJobProbe.timeout, 0, "an unbounded probe could hold the app's launch path")
    }

    // MARK: - Fixtures (real `launchctl print` output, trimmed but not reshaped)

    /// Captured 2026-07-28 from `launchctl print gui/502/org.sessiometer.agent` on a machine in the issue #819
    /// state: our agent registered via `SMAppService` (`submitted by smd`), stopped after a clean stand-down
    /// (`last exit code = 0`), while a hand-run `./target/release/sessiometer run -v` held the lock.
    private static let notRunningOutput = """
    gui/502/org.sessiometer.agent = {
    \tactive count = 0
    \tpath = (submitted by smd.5159)
    \ttype = Submitted
    \tmanaged_by = com.apple.xpc.ServiceManagement
    \tstate = not running

    \tprogram identifier = Contents/Helpers/sessiometer (mode: 2)
    \tparent bundle identifier = org.sessiometer.menubar
    \targuments = {
    \t\tsessiometer
    \t\trun
    \t\t--managed
    \t}

    \tendpoints = {
    \t\t"org.sessiometer.agent" = {
    \t\t\tport = 0xdeadbeef
    \t\t\tactive = 0
    \t\t\tmanaged = 1
    \t\t\tstate = active
    \t\t}
    \t}

    \tdomain = gui/502 [101536]
    \truns = 2
    \tlast exit code = 0
    \tjob state = exited

    \tsemaphores = {
    \t\tsuccessful exit => 0
    \t}
    }
    """

    /// Captured the same day from a real RUNNING user agent, for the opposite verdict. Only the label and pid
    /// are anonymised; the shape is verbatim.
    private static let runningOutput = """
    gui/502/org.example.running = {
    \tactive count = 33
    \tpath = /System/Library/LaunchAgents/org.example.running.plist
    \tstate = running
    \tpid = 28868

    \tendpoints = {
    \t\t"org.example.running" = {
    \t\t\tport = 0xcafebabe
    \t\t\tactive = 1
    \t\t\tstate = active
    \t\t}
    \t}

    \tdomain = gui/502 [101536]
    }
    """
}
