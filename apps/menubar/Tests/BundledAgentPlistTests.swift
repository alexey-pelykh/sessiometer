import XCTest

/// Validates the committed LaunchAgent plist that #171 ships into the app bundle at
/// `Contents/Library/LaunchAgents/org.sessiometer.agent.plist` (via the project.yml copyFiles
/// phase). Pins the two-owner parity invariant: the bundled agent and the CLI-generated agent
/// (`src/service.rs` `render_plist`) must describe an identical launchd job except for the
/// executable reference (bundle-relative `BundleProgram` here vs the CLI's absolute `Program`)
/// and the log paths. Any future drift in Label / argv / RunAtLoad / KeepAlive should touch both
/// owners — this test fails loudly if the bundled side drifts.
final class BundledAgentPlistTests: XCTestCase {
    /// Load the committed source plist (the artifact copyFiles bundles). `#filePath` is the
    /// compile-time absolute path of this test file; navigate up to the sibling LaunchAgents dir.
    private func loadAgentPlist(file: StaticString = #filePath) throws -> [String: Any] {
        let testFile = URL(fileURLWithPath: "\(file)")
        let plistURL = testFile
            .deletingLastPathComponent()          // apps/menubar/Tests/
            .deletingLastPathComponent()          // apps/menubar/
            .appendingPathComponent("LaunchAgents/org.sessiometer.agent.plist")
        let data = try Data(contentsOf: plistURL)
        let obj = try PropertyListSerialization.propertyList(from: data, format: nil)
        return try XCTUnwrap(obj as? [String: Any], "agent plist did not parse as a dictionary")
    }

    func testAgentPlistParsesAsValidPropertyList() throws {
        _ = try loadAgentPlist()   // throws → fails if the committed plist is malformed
    }

    func testLabelMatchesTheSharedTwoOwnerIdentity() throws {
        let plist = try loadAgentPlist()
        XCTAssertEqual(plist["Label"] as? String, "org.sessiometer.agent",
                       "Label must equal AGENT_LABEL (service.rs) and the plist filename stem")
    }

    func testProgramArgumentsInvokeTheLockGuardedManagedRunVerb() throws {
        let plist = try loadAgentPlist()
        let argv = try XCTUnwrap(plist["ProgramArguments"] as? [String], "ProgramArguments missing")
        // Full-array pin (issue #742): the daemon is launched via the single-instance-lock-guarded
        // `run` verb plus the `--managed` marker that turns a lost-lock contention into a clean
        // exit-0 stand-down (not the human exit-3). Assert the WHOLE argv, not just `.last` — a
        // `.last`-only pin would miss an inserted arg (`[…, "--x", "run"]`) or a wrong argv[0]. This
        // is the two-owner parity anchor with `src/service.rs render_plist` (which passes the same
        // `["run", "--managed"]` after its own absolute-`Program` argv[0]).
        XCTAssertEqual(argv, ["sessiometer", "run", "--managed"],
                       "argv must be exactly [sessiometer, run, --managed] — parity with render_plist")
    }

    func testRunAtLoadIsEnabledAndKeepAliveIsConditionalOnUnsuccessfulExit() throws {
        let plist = try loadAgentPlist()
        XCTAssertEqual(plist["RunAtLoad"] as? Bool, true, "RunAtLoad must be true (parity with service.rs)")
        // KeepAlive is a CONDITIONAL dict, not a bare Bool (issue #742): {SuccessfulExit: false}
        // respawns only on a NON-clean exit, so a `--managed` agent that cleanly stands down on lock
        // contention (exit 0) is NOT relaunched into a throttled loop. Parity with service.rs.
        let keepAlive = try XCTUnwrap(plist["KeepAlive"] as? [String: Any],
                                      "KeepAlive must be a conditional dict, not a Bool (issue #742)")
        XCTAssertEqual(keepAlive["SuccessfulExit"] as? Bool, false,
                       "KeepAlive.SuccessfulExit=false: respawn on crash, not on a clean stand-down")
        XCTAssertEqual(keepAlive.count, 1, "SuccessfulExit is the only KeepAlive condition")
    }

    func testUsesBundleRelativeProgramNotAbsoluteProgram() throws {
        let plist = try loadAgentPlist()
        XCTAssertEqual(plist["BundleProgram"] as? String, "Contents/Helpers/sessiometer",
                       "SMAppService requires a bundle-relative BundleProgram")
        XCTAssertNil(plist["Program"],
                     "an absolute Program won't survive the bundle moving — use BundleProgram only")
    }
}
