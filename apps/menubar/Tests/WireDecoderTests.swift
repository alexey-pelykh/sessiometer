// Copyright (c) 2026 Oleksii PELYKH
// SPDX-License-Identifier: MIT

// Golden-fixture tests for the `watch` wire decoder (issue #322). The fixtures live in
// `Fixtures.swift` (shared, no XCTest dependency); each test below maps to an acceptance
// criterion. The decoder is pure `JSONDecoder`, so these run identically under `xcodebuild test`
// (CI) and any plain verifier.

import XCTest

final class WireDecoderTests: XCTestCase {

    // AC: "Decodes real `snapshot` … frames."
    func testDecodesRealSnapshotFrame() throws {
        guard case .snapshot(let v) = try parseWatchFrame(Fixtures.snapshotBasic) else {
            return XCTFail("expected a snapshot frame")
        }
        XCTAssertEqual(v.schemaVersion, SchemaVersion(major: 1, minor: 13))
        XCTAssertEqual(v.generatedAt, 42)
        XCTAssertTrue(v.isSchemaSupported)
        XCTAssertNil(v.nextSwap, "next_swap null decodes to nil")
        XCTAssertEqual(v.refreshEnabled, false)
        XCTAssertNil(v.systemicRefreshFailure, "systemic_refresh_failure null decodes to nil")
        XCTAssertNil(v.systemicRefreshSource, "systemic_refresh_source absent (healthy) decodes to nil")
        XCTAssertNil(v.canonicalScrub, "canonical_scrub absent (healthy) decodes to nil")
        XCTAssertFalse(v.keychainLocked, "keychain_locked absent (unlocked) decodes to false")
        XCTAssertNil(v.canary, "canary absent (no verdict yet) decodes to nil")
        XCTAssertEqual(v.accounts.count, 1)

        let a = v.accounts[0]
        XCTAssertEqual(a.label, "work")
        XCTAssertTrue(a.active)
        XCTAssertTrue(a.enabled)
        XCTAssertFalse(a.quarantined)
        XCTAssertFalse(a.recovering)
        XCTAssertEqual(a.sessionPct, 60)
        XCTAssertEqual(a.weeklyPct, 10)
        XCTAssertNil(a.sessionResetsAt)
        XCTAssertNil(a.weeklyResetsAt)
        XCTAssertFalse(a.weeklyExhausted)
        XCTAssertNil(a.accessExpiresAt)
        XCTAssertNil(a.refreshHealth)
        XCTAssertEqual(a.auth, .healthy)
    }

    // AC: "Decodes real … `heartbeat` frames." + heartbeat carries the freshness envelope.
    func testDecodesRealHeartbeatFrame() throws {
        let frame = try parseWatchFrame(Fixtures.heartbeatBasic)
        XCTAssertEqual(frame, .heartbeat(generatedAt: 42, schemaVersion: SchemaVersion(major: 1, minor: 13)))
        XCTAssertEqual(frame.schemaVersion, SchemaVersion(major: 1, minor: 13))
        XCTAssertTrue(WireContract.isSupported(try XCTUnwrap(frame.schemaVersion)))
    }

    // AC: "All three `next_swap` states … decode" — `target` here (+ null in the basic test).
    // AC: "`auth` → CredentialHealth …; `refresh_health` … tolerated" — present + null both here.
    func testDecodesRichSnapshotWithTargetAndMixedHealth() throws {
        guard case .snapshot(let v) = try parseWatchFrame(Fixtures.snapshotRichTarget) else {
            return XCTFail("expected a snapshot frame")
        }
        XCTAssertEqual(v.generatedAt, 1_893_456_000)
        // The target carries the #393 reason: personal is the lone viable spare → `only_candidate`.
        XCTAssertEqual(v.nextSwap, .target(to: "personal", reason: .onlyCandidate))
        XCTAssertEqual(v.refreshEnabled, true)
        XCTAssertEqual(v.accounts.count, 2)

        let work = v.accounts[0]
        XCTAssertEqual(work.sessionPct, 30)
        XCTAssertEqual(work.weeklyPct, 20)
        XCTAssertEqual(work.sessionResetsAt, 1_893_460_000)
        XCTAssertEqual(work.weeklyResetsAt, 1_893_800_000)
        XCTAssertEqual(work.accessExpiresAt, 1_893_470_000)
        XCTAssertEqual(work.refreshHealth, RefreshHealth(lastOk: true, rotated: true, consecutiveFailures: 0))
        XCTAssertEqual(work.auth, .atRisk)

        let personal = v.accounts[1]
        XCTAssertFalse(personal.active)
        XCTAssertNil(personal.sessionPct)
        XCTAssertNil(personal.refreshHealth, "refresh_health null is tolerated → nil")
        XCTAssertEqual(personal.auth, .unknown)
    }

    // AC (#393): the target's structured reason decodes — `soonest_reset` carries its `resets_at`
    // epoch straight off the wire, so the client renders the daemon's rationale rather than
    // re-deriving any selection heuristic. Byte-pinned to the Rust golden (WireGoldenTests).
    func testDecodesNextSwapTargetWithSoonestResetReason() throws {
        guard case .snapshot(let v) = try parseWatchFrame(Fixtures.snapshotNextSwap) else {
            return XCTFail("expected a snapshot frame")
        }
        XCTAssertEqual(
            v.nextSwap,
            .target(to: "spare", reason: .soonestReset(resetsAt: 1_893_800_000))
        )
    }

    // AC (#393): the `roster_order` reason decodes — ≥2 accounts qualified, none reported a weekly
    // reset, so the earliest roster index won. The client must decode every KNOWN tag the daemon
    // emits to its own case (an UNKNOWN `kind` now degrades to `reason: nil` — issue #412,
    // `testUnknownReasonKindDecodesToNilReasonAndFrameStillDecodes`), and must never render this as
    // "only viable target" — other targets were viable.
    func testDecodesNextSwapTargetWithRosterOrderReason() throws {
        guard case .snapshot(let v) = try parseWatchFrame(Fixtures.snapshotRosterOrderTarget) else {
            return XCTFail("expected a snapshot frame")
        }
        XCTAssertEqual(v.nextSwap, .target(to: "spare", reason: .rosterOrder))
    }

    // AC: "All three `next_swap` states …" — `no_viable_target`. + `auth` stale, failure streak.
    // AC (#405): the no-viable-target carries the fleet-capacity relief — `cause` = weekly (the
    // WEEKLY window gates the soonest-returning spare, #665) with that reset off the wire, so the
    // client renders "out of capacity, resets in ⟨dur⟩" rather than re-deriving it (mirroring the
    // #393 reason path).
    func testDecodesNoViableTargetAndStale() throws {
        guard case .snapshot(let v) = try parseWatchFrame(Fixtures.snapshotNoViable) else {
            return XCTFail("expected a snapshot frame")
        }
        XCTAssertEqual(v.nextSwap, .noViableTarget(cause: .weekly, resetsAt: 1_893_800_500))
        let a = v.accounts[0]
        XCTAssertTrue(a.weeklyExhausted)
        XCTAssertEqual(a.auth, .stale)
        XCTAssertEqual(a.refreshHealth, RefreshHealth(lastOk: false, rotated: false, consecutiveFailures: 2))
    }

    // AC (#405): a pre-#405 daemon omits the additive `cause`/`resets_at` relief keys — a bare
    // no-viable-target must decode to `cause: nil, resetsAt: nil` (the `decodeIfPresent` forward-compat
    // path), NOT a decode error. The additive-minor contract that makes the #405 relief render-safe
    // against an older daemon (mirrors `testPreReasonTargetDecodesWithNilReason` for #393).
    func testDecodesNoViableTargetWithoutReliefIsForwardCompatible() throws {
        guard case .snapshot(let v) = try parseWatchFrame(Fixtures.snapshotNoViableNoRelief) else {
            return XCTFail("expected a snapshot frame")
        }
        XCTAssertEqual(v.nextSwap, .noViableTarget(cause: nil, resetsAt: nil))
    }

    // AC: "All three `next_swap` states …" — `awaiting_data`. + `auth` dead, quarantined.
    func testDecodesAwaitingDataAndDead() throws {
        guard case .snapshot(let v) = try parseWatchFrame(Fixtures.snapshotAwaitingDead) else {
            return XCTFail("expected a snapshot frame")
        }
        XCTAssertEqual(v.nextSwap, .awaitingData)
        let a = v.accounts[0]
        XCTAssertTrue(a.quarantined)
        XCTAssertNil(a.sessionPct)
        XCTAssertEqual(a.auth, .dead)
    }

    // AC (#427): the NON-TERMINAL `"auth":"degraded"` verdict decodes — the menubar reads the new
    // rollup token a daemon emits for a quarantined-but-refreshable account rather than hard-erroring
    // on an unrecognized value. This is the wire half of CLI↔menubar agreement (single source of truth).
    func testDecodesDegraded() throws {
        guard case .snapshot(let v) = try parseWatchFrame(Fixtures.snapshotDegraded) else {
            return XCTFail("expected a snapshot frame")
        }
        let a = v.accounts[0]
        XCTAssertTrue(a.quarantined)
        XCTAssertEqual(a.auth, .degraded)
    }

    // AC (#485, MANDATORY schema-bump lockstep): the active account's `blind_active` object decodes —
    // blind duration + retained last-known session % + the DEGRADED flag — while `session_pct` / `weekly_pct`
    // stay nil (the daemon's `usage: None` during blindness; the retained value lives ONLY in blind_active).
    // A non-blind sibling omits the key → decodes to nil (the additive-optional forward-compat path).
    func testDecodesBlindActiveDegradedOnActiveAccount() throws {
        guard case .snapshot(let v) = try parseWatchFrame(Fixtures.snapshotBlindActiveDegraded) else {
            return XCTFail("expected a snapshot frame")
        }
        XCTAssertEqual(v.schemaVersion, SchemaVersion(major: 1, minor: 13))
        let work = v.accounts[0]
        XCTAssertTrue(work.active)
        XCTAssertNil(work.sessionPct)
        XCTAssertNil(work.weeklyPct)
        XCTAssertEqual(work.blindActive,
                       BlindActive(blindSecs: 1380, lastKnownSessionPct: 87, autoProtectionDegraded: true))
        XCTAssertNil(v.accounts[1].blindActive, "a non-blind account omits the key → nil")
    }

    // The OK projection decodes with `autoProtectionDegraded == false` (the calm, self-resolving state).
    func testDecodesBlindActiveOkIsNotDegraded() throws {
        guard case .snapshot(let v) = try parseWatchFrame(Fixtures.snapshotBlindActiveOK) else {
            return XCTFail("expected a snapshot frame")
        }
        XCTAssertEqual(v.accounts[0].blindActive,
                       BlindActive(blindSecs: 240, lastKnownSessionPct: 64, autoProtectionDegraded: false))
    }

    // AC (#516): the daemon-level `canonical_scrub` = `exhausted` rollup decodes — the fleet-wide
    // scrubbed-AND-recovery-exhausted (un-recoverable) state that no per-account `auth` reflects, which
    // #469 renders with the `claude /login` remedy. Byte-pinned to the Rust golden (WireGoldenTests),
    // so the `{"state":"exhausted"}` discriminant is under the cross-language byte-drift guard.
    func testDecodesCanonicalScrubExhausted() throws {
        guard case .snapshot(let v) = try parseWatchFrame(Fixtures.snapshotCanonicalScrubExhausted) else {
            return XCTFail("expected a snapshot frame")
        }
        XCTAssertEqual(v.schemaVersion, SchemaVersion(major: 1, minor: 13))
        XCTAssertEqual(v.canonicalScrub, .exhausted)
        // The rest of the frame still decodes normally alongside the added rollup.
        XCTAssertEqual(v.accounts.count, 1)
        XCTAssertEqual(v.accounts[0].auth, .healthy)
    }

    // AC (#516): the OTHER known `canonical_scrub` state — `recovering` (scrubbed, adopt in progress,
    // the self-may-heal state) — decodes to its own case. The client must decode every KNOWN state the
    // daemon emits (an UNKNOWN one is a HARD error — `testUnknownCanonicalScrubStateThrows`), which one
    // byte-golden (carrying `exhausted`) cannot cover.
    func testDecodesCanonicalScrubRecovering() throws {
        guard case .snapshot(let v) = try parseWatchFrame(Fixtures.snapshotCanonicalScrubRecovering) else {
            return XCTFail("expected a snapshot frame")
        }
        XCTAssertEqual(v.canonicalScrub, .recovering)
    }

    // AC (#498): the daemon-level `keychain_locked` = `true` flag decodes — the fleet-wide
    // unreadable-credential lockout (the login keychain is LOCKED, so the shared item can't be READ at
    // all) that no per-account `auth` reflects, DISTINCT from `canonical_scrub` (a readable-but-scrubbed
    // item). A bare `bool`: absent (the healthy/unlocked frame, `skip_serializing_if`) → false
    // (`testDecodesRealSnapshotFrame`), present → true here. The wire prerequisite for the menubar #498
    // surface; not byte-pinned to a golden (the goldens cover the unlocked frame, which omits the flag).
    func testDecodesKeychainLocked() throws {
        guard case .snapshot(let v) = try parseWatchFrame(Fixtures.snapshotKeychainLocked) else {
            return XCTFail("expected a snapshot frame")
        }
        XCTAssertEqual(v.schemaVersion, SchemaVersion(major: 1, minor: 13))
        XCTAssertTrue(v.keychainLocked)
        // The flag is independent of `canonical_scrub` (a locked keychain can't be read to know
        // scrubbed-ness), and the rest of the frame still decodes normally alongside it.
        XCTAssertNil(v.canonicalScrub)
        XCTAssertEqual(v.accounts.count, 1)
        XCTAssertEqual(v.accounts[0].auth, .healthy)
    }

    // AC (#520/#523): the daemon-level `systemic_refresh_failure` COUNT decodes when non-null — the refresh
    // MECHANISM being down (#378), the third daemon-level payload fault. `null` → nil is already pinned by
    // `testDecodesRealSnapshotFrame`; this pins the present-and-set case, the wire prerequisite for the
    // menubar glyph + banner surfaces. Note the roster alongside it reads HEALTHY: that combination is the
    // whole reason the signal exists (#378 is visible before any account dies).
    func testDecodesSystemicRefreshFailureCount() throws {
        guard case .snapshot(let v) = try parseWatchFrame(Fixtures.snapshotSystemicRefreshFailure) else {
            return XCTFail("expected a snapshot frame")
        }
        XCTAssertEqual(v.schemaVersion, SchemaVersion(major: 1, minor: 13))
        XCTAssertEqual(v.systemicRefreshFailure, 3)
        XCTAssertEqual(v.systemicRefreshSource, .sweep, "#813: the episode's opening bracket")
        // Independent of the vault pair — the mechanism can be down while the shared item is fine.
        XCTAssertNil(v.canonicalScrub)
        XCTAssertFalse(v.keychainLocked)
        XCTAssertEqual(v.accounts.count, 1)
        XCTAssertEqual(v.accounts[0].auth, .healthy)
    }

    // AC1/AC3 (#813): the episode's PROVENANCE decodes on both arms, and its ABSENCE is tolerated. The
    // count cannot carry this distinction — a preflight-opened episode seeds the count at 1 for pre-#813
    // grammar, so on the wire it is byte-indistinguishable from a genuine one-sweep crossing. This is the
    // decode-level pin that the surfaces' phrasing split rests on.
    func testDecodesSystemicRefreshProvenanceOnBothArmsAndToleratesItsAbsence() throws {
        guard case .snapshot(let pre) = try parseWatchFrame(Fixtures.snapshotSystemicRefreshPreflight) else {
            return XCTFail("expected a snapshot frame")
        }
        XCTAssertEqual(pre.schemaVersion, SchemaVersion(major: 1, minor: 13))
        XCTAssertEqual(pre.systemicRefreshSource, .preflight)
        XCTAssertEqual(pre.systemicRefreshFailure, 1, "the seeded floor, not a sweep count")

        // AC3 — a PRE-#813 daemon (minor 10) sends the bare count. The additive key's absence must decode
        // to nil, never a decode error: this frame is exactly what an older daemon puts on the wire, and
        // refusing it would break a supported-major client over an additive minor.
        guard case .snapshot(let legacy) = try parseWatchFrame(Fixtures.snapshotSystemicRefreshNoSource) else {
            return XCTFail("a pre-#813 frame must still decode, not throw")
        }
        XCTAssertEqual(legacy.schemaVersion, SchemaVersion(major: 1, minor: 10))
        XCTAssertTrue(legacy.isSchemaSupported, "an additive minor stays supported — major gates, minor does not")
        XCTAssertEqual(legacy.systemicRefreshFailure, 3, "the count it does send is unchanged")
        XCTAssertNil(legacy.systemicRefreshSource, "absent provenance decodes to nil")

        // The two #813-era arms differ ONLY in provenance at the same count-carrying position — which is
        // precisely why the count alone could never tell them apart.
        XCTAssertNotEqual(pre.systemicRefreshSource, legacy.systemicRefreshSource)
    }

    // #813: an UNKNOWN provenance token from a FUTURE daemon degrades to nil — it must NOT fail the frame.
    // The tolerated-decoration posture of an unknown `reason.kind`, not the hard-reject posture of an
    // unknown `canonical_scrub` state, because the alarm rides in a SEPARATE field here: the count still
    // decodes, so rejecting would throw away the roster, the vault pair, the canary AND the mechanism-down
    // signal itself over a phrasing selector. It would also be unrecoverable — `isSupported` keys on the
    // MAJOR (pinned two tests above), so a 1.14 daemon is a version this client calls supported while
    // blanking every frame it sends.
    //
    // The frame's minor is deliberately ONE AHEAD of `STATUS_SCHEMA_VERSION` — it models a daemon
    // NEWER than this build. It must be re-pointed on every minor bump, or it silently stops being a
    // future frame and starts asserting the current contract emits a token it does not (last moved
    // 1.13 → 1.14 for issue #879).
    func testUnknownSystemicRefreshSourceIsReadableAsUnrecognizedRatherThanFailingTheFrame() throws {
        let frame = #"""
        {"type":"snapshot","schema_version":{"major":1,"minor":14},"generated_at":42,"accounts":[{"label":"work","active":true,"enabled":true,"quarantined":false,"recovering":false,"session_pct":60,"weekly_pct":10,"session_resets_at":null,"weekly_resets_at":null,"weekly_exhausted":false,"access_expires_at":null,"refresh_health":null,"auth":"healthy"}],"next_swap":null,"refresh_enabled":true,"systemic_refresh_failure":3,"systemic_refresh_source":"future_bracket"}
        """#
        guard case .snapshot(let v) = try parseWatchFrame(frame) else {
            return XCTFail("an unknown provenance token must not cost us the frame")
        }
        XCTAssertEqual(v.systemicRefreshSource, .unrecognized,
                       "an unreadable token is present-but-unreadable — NOT nil, which means a pre-#813 daemon")
        XCTAssertEqual(v.systemicRefreshFailure, 3, "the DOWN verdict itself still decodes — that is the point")
        XCTAssertEqual(v.accounts.count, 1, "and so does the roster")
        XCTAssertTrue(v.isSchemaSupported, "a future MINOR is still a supported contract")

        // AC1's spirit one version further out: with the bracket unreadable, both surfaces claim NO
        // evidence rather than falling back to the sweep phrasing. A third bracket is far likelier to be
        // another non-sweep opener, so guessing "3 consecutive sweeps failed" would re-create exactly the
        // fabrication #813 removes — in a build too old to know better.
        let detail = StatusPanelFormat.systemicRefreshFailureBanner(v.systemicRefreshFailure,
                                                                    source: v.systemicRefreshSource)?.detail
        XCTAssertEqual(detail,
                       "This app cannot read the cause — check the daemon log.")
        XCTAssertFalse(detail?.contains("sweep") ?? true, "must not invent a sweep it cannot know about")

        let label = PresentationState.make(for: .connected,
                                           accountCount: 1,
                                           systemicRefreshFailure: v.systemicRefreshFailure,
                                           systemicRefreshSource: v.systemicRefreshSource).accessibilityLabel
        XCTAssertEqual(label,
                       "Sessiometer: refresh mechanism down — this app cannot read the cause; check the daemon log")
        XCTAssertFalse(label.contains("sweep"), "nor read a fabricated sweep aloud to a VoiceOver user")
    }

    // AC (#728): the behavioral-canary `drift` verdict decodes with its labels + `overridden` flag — the
    // ACT-NOW alarm (writes refused) the panel surfaces at rank 3. `null`/absent → nil is already pinned by
    // `testDecodesRealSnapshotFrame`; this pins the present-and-set `drift` case, the wire prerequisite for
    // the menubar banner. Labels are operator handles, never a token or email (#15).
    func testDecodesCanaryDriftRefusing() throws {
        guard case .snapshot(let v) = try parseWatchFrame(Fixtures.snapshotCanaryDriftRefusing) else {
            return XCTFail("expected a snapshot frame")
        }
        XCTAssertEqual(v.schemaVersion, SchemaVersion(major: 1, minor: 13))
        XCTAssertEqual(v.canary, .drift(displayed: "work", matched: "personal", overridden: false))
        // Independent of its sibling faults — the identity can drift while the vault reads fine.
        XCTAssertNil(v.canonicalScrub)
        XCTAssertFalse(v.keychainLocked)
        XCTAssertEqual(v.accounts.count, 1)
        XCTAssertEqual(v.accounts[0].auth, .healthy)
    }

    // AC (#728): the OTHER drift variant — `overridden: true`, the standing acknowledged alarm (writes
    // proceed) the panel surfaces at rank 7 (`.warning`). The `overridden` flag is what the (fault, VARIANT)
    // banner split reads, so both boolean values must decode to distinct cases.
    func testDecodesCanaryDriftOverridden() throws {
        guard case .snapshot(let v) = try parseWatchFrame(Fixtures.snapshotCanaryDriftOverridden) else {
            return XCTFail("expected a snapshot frame")
        }
        XCTAssertEqual(v.canary, .drift(displayed: "work", matched: "personal", overridden: true))
    }

    // AC (#728): the `ambiguous` verdict decodes with its COUNT — the other ACT-NOW alarm (no unique write
    // target → writes refused) the panel surfaces at rank 4. Carries only the count, never a token (#15).
    func testDecodesCanaryAmbiguous() throws {
        guard case .snapshot(let v) = try parseWatchFrame(Fixtures.snapshotCanaryAmbiguous) else {
            return XCTFail("expected a snapshot frame")
        }
        XCTAssertEqual(v.canary, .ambiguous(count: 2))
    }

    // AC (#738): the `refused_unparseable_canonical` verdict decodes to its own case — the THIRD act-now
    // alarm (an unrecognized item under the derived service → writes refused rather than clobber it), which
    // the panel surfaces at rank 5. A bare tag with no payload, so this pins the tag SPELLING: the daemon
    // serializes the Rust variant under `rename_all = "snake_case"`, and any drift between the two spellings
    // would land as a HARD decode error (`testUnknownCanaryVerdictThrows`) — a dropped frame, not a silent
    // mis-render. Before #738 this state arrived as the quiet `inconclusive`, so the panel stayed silent
    // while the daemon refused; that is the regression this guards.
    func testDecodesCanaryRefusedUnparseableCanonical() throws {
        guard case .snapshot(let v) =
            try parseWatchFrame(Fixtures.snapshotCanaryRefusedUnparseableCanonical) else {
            return XCTFail("expected a snapshot frame")
        }
        XCTAssertEqual(v.schemaVersion, SchemaVersion(major: 1, minor: 13))
        XCTAssertEqual(v.canary, .refusedUnparseableCanonical)
        // Independent of its sibling faults — an unrecognized canonical can sit under a vault that
        // reads fine and a roster that is entirely healthy.
        XCTAssertNil(v.canonicalScrub)
        XCTAssertFalse(v.keychainLocked)
        XCTAssertEqual(v.accounts.count, 1)
        XCTAssertEqual(v.accounts[0].auth, .healthy)
    }

    // AC (#728): the quiet `ok` verdict decodes to its own case (non-nil) but drives NO banner — the client
    // must decode every KNOWN verdict the daemon emits (an UNKNOWN one is a HARD error —
    // `testUnknownCanaryVerdictThrows`); the render-nothing decision lives in `daemonFaultBanner`, not here.
    func testDecodesCanaryOkIsDecodedButQuiet() throws {
        guard case .snapshot(let v) = try parseWatchFrame(Fixtures.snapshotCanaryOk) else {
            return XCTFail("expected a snapshot frame")
        }
        XCTAssertEqual(v.canary, .ok)
    }

    // AC: "`auth` → CredentialHealth including `null`".
    func testAuthNullIsTolerated() throws {
        guard case .snapshot(let v) = try parseWatchFrame(Fixtures.snapshotAuthNull) else {
            return XCTFail("expected a snapshot frame")
        }
        XCTAssertNil(v.accounts[0].auth)
    }

    // AC: additive-default path — a pre-#109/#119 account with only required fields decodes.
    func testLegacyMinimalAccountDecodesWithDefaults() throws {
        guard case .snapshot(let v) = try parseWatchFrame(Fixtures.snapshotLegacyMinimal) else {
            return XCTFail("expected a snapshot frame")
        }
        let a = v.accounts[0]
        XCTAssertEqual(a.label, "work")
        XCTAssertFalse(a.recovering)
        XCTAssertFalse(a.weeklyExhausted)
        XCTAssertNil(a.sessionPct)
        XCTAssertNil(a.weeklyPct)
        XCTAssertNil(a.sessionResetsAt)
        XCTAssertNil(a.weeklyResetsAt)
        XCTAssertNil(a.accessExpiresAt)
        XCTAssertNil(a.refreshHealth)
        XCTAssertNil(a.auth)
        // Back-compat (#498): a pre-#498 (minor 0) frame omits `keychain_locked` → decodes to false —
        // the `decodeIfPresent ?? false` additive-default path (mirrors the Rust `#[serde(default)]`).
        XCTAssertFalse(v.keychainLocked, "an older daemon that never emits keychain_locked → false")
    }

    // AC: forward-compat MINOR — unknown additive keys ignored, still supported.
    func testUnknownAdditiveFieldsAreIgnored() throws {
        guard case .snapshot(let v) = try parseWatchFrame(Fixtures.snapshotUnknownAdditiveFields) else {
            return XCTFail("expected a snapshot frame")
        }
        XCTAssertEqual(v.schemaVersion, SchemaVersion(major: 1, minor: 5))
        XCTAssertTrue(v.isSchemaSupported, "a minor bump stays supported")
        XCTAssertEqual(v.accounts[0].label, "work")
    }

    // AC (#884): a CURRENT 1.12 daemon carries the additive per-account `expiry` modifier, and this
    // build now MIRRORS it — the property landed with the panel surface, exactly as `blind_active`
    // arrived under #485 rather than the #479 wire item. #882 pinned the tolerance (the frame must
    // decode even unmirrored); this now additionally pins the READING, on both the populated and the
    // deadline-less row.
    //
    // Still not redundant with `testUnknownAdditiveFieldsAreIgnored`: that fixture's unknown additive is
    // a scalar (`"future_field":"x"`) on a hand-built 1.5 frame. This one is a nested OBJECT with its own
    // keys, on the real shape a shipped daemon emits — and `WatchStatusStore` DROPS an undecodable
    // line, so getting this wrong blanks the panel on every frame rather than degrading one cell.
    func testCurrentDaemonExpiryModifierDecodesOnBothThePopulatedAndDeadlinelessRow() throws {
        guard case .snapshot(let v) = try parseWatchFrame(Fixtures.snapshotExpiryModifier) else {
            return XCTFail("the #882 expiry modifier must not cost us the frame")
        }
        XCTAssertEqual(v.schemaVersion, SchemaVersion(major: 1, minor: 13))
        XCTAssertTrue(v.isSchemaSupported, "an additive minor stays supported — major gates, minor does not")
        XCTAssertEqual(v.accounts.count, 2, "the roster survives the unknown per-account key")

        // The row carrying a real deadline (`horizon_state: "within"`) decodes unchanged in every
        // field this client knows — the modifier is ORTHOGONAL to the auth rollup, so `work` is
        // `healthy` AND inside its expiry horizon at once. That co-occurrence is the whole reason #878
        // made it a separate axis instead of a new `auth` state, and a client that mis-read one as the
        // other would report a healthy account as failing.
        let work = v.accounts[0]
        XCTAssertEqual(work.label, "work")
        XCTAssertEqual(work.auth, .healthy)
        XCTAssertEqual(work.sessionPct, 30)
        XCTAssertEqual(work.accessExpiresAt, 1_893_470_000,
                       "the ACCESS-token clock is a different field from the REFRESH-token modifier")
        XCTAssertEqual(work.refreshHealth, RefreshHealth(lastOk: true, rotated: false, consecutiveFailures: 0))
        XCTAssertNil(work.blindActive, "the sibling per-account modifier is independent of this one")
        XCTAssertEqual(work.expiry, AccountExpiry(expiresAt: 1_893_800_000, horizonState: .within),
                       "the mirrored modifier carries BOTH facts — the deadline and its classification")

        // And the row whose credential held NO parseable deadline — `expiry` present with a null
        // `expires_at` and `horizon_state: "unknown"`. The object's PRESENCE is itself the observation
        // ("polled, and the credential carried no deadline"), which is why the daemon emits an explicit
        // `null` rather than omitting the key — and why this decodes to a non-nil `expiry` holding a nil
        // deadline, NOT to a nil `expiry`. Those two are different facts (#137: `unknown` is never "not
        // expiring", and an ABSENT modifier is "never observed").
        let spare = v.accounts[1]
        XCTAssertEqual(spare.label, "spare")
        XCTAssertEqual(spare.auth, .healthy)
        XCTAssertEqual(spare.weeklyPct, 10)
        XCTAssertEqual(spare.expiry, AccountExpiry(expiresAt: nil, horizonState: .unknown))
        XCTAssertNotNil(spare.expiry, "a polled-but-deadline-less row is OBSERVED, not absent")

        // The rest of the frame is untouched by the new key.
        XCTAssertEqual(v.nextSwap, .target(to: "spare", reason: .onlyCandidate))
        XCTAssertEqual(v.refreshEnabled, true)
        XCTAssertNil(v.canonicalScrub)
        XCTAssertFalse(v.keychainLocked)
    }

    // AC (#879 forward-compat): a 1.13 daemon carries the synchronized-expiry cohort as TWO new
    // keys — the per-account `expiry.cohort_id` grouping key, and the DAEMON-LEVEL `expiry_cohort`
    // condition beside it. Issue #884 has since mirrored the `expiry` object itself, but neither
    // cohort key: nothing in the panel groups rows, so modelling the relationship would be drawing
    // a surface that does not exist. The contract this defends is that leaving them unmirrored
    // costs the build nothing.
    //
    // Worth its own test rather than folding into the #882 case above, for the top-level half: every
    // unmirrored additive object this client has tolerated so far (`blind_active` under #479,
    // `expiry` under #882) arrived nested inside an account. `expiry_cohort` is the first to arrive
    // at the ROOT of the payload, which is a different decode path — and the cost of getting it
    // wrong is not a blank cell but a blank panel, since `WatchStatusStore` drops a line it cannot
    // decode. A frame carrying BOTH shapes at once is also the realistic one: the daemon populates
    // `cohort_id` and the condition from a single walk, so they always ship together.
    func testCurrentDaemonExpiryCohortIsToleratedWithoutAMirror() throws {
        guard case .snapshot(let v) = try parseWatchFrame(Fixtures.snapshotExpiryCohort) else {
            return XCTFail("the #879 cohort keys must not cost us the frame")
        }
        XCTAssertEqual(v.schemaVersion, SchemaVersion(major: 1, minor: 13))
        XCTAssertTrue(v.isSchemaSupported, "an additive minor stays supported")
        XCTAssertEqual(v.accounts.count, 3, "the roster survives an unknown ROOT-level key")

        // Every field this client knows keeps its value, on a grouped row and an ungrouped one
        // alike. The cohort is ORTHOGONAL to the auth rollup — all three accounts are `healthy`
        // while two of them are about to expire together, which is precisely why the fleet fact
        // cannot be read off any single row.
        for account in v.accounts {
            XCTAssertEqual(account.auth, .healthy)
            XCTAssertNil(account.blindActive)
        }
        XCTAssertEqual(v.accounts[0].label, "work")
        XCTAssertEqual(v.accounts[0].sessionPct, 30)
        XCTAssertEqual(v.accounts[0].accessExpiresAt, 1_893_470_000)
        XCTAssertEqual(v.accounts[1].label, "spare")
        XCTAssertEqual(v.accounts[2].label, "archive", "the ungrouped row decodes like any other")

        // And the rest of the frame is untouched by either new key.
        XCTAssertEqual(v.nextSwap, .target(to: "spare", reason: .onlyCandidate))
        XCTAssertEqual(v.refreshEnabled, true)
        XCTAssertNil(v.canonicalScrub)
        XCTAssertFalse(v.keychainLocked)
        XCTAssertNil(v.systemicRefreshFailure)
    }

    // AC (#393 forward-compat): a pre-#393 daemon's `target` has no `reason` key → decodes to
    // `reason: nil` (the additive `decodeIfPresent` path), never a decode error. This is the
    // contract that lets the client render an older daemon's next-swap without the rationale line.
    func testPreReasonTargetDecodesWithNilReason() throws {
        guard case .snapshot(let v) = try parseWatchFrame(Fixtures.snapshotTargetNoReason) else {
            return XCTFail("expected a snapshot frame")
        }
        XCTAssertEqual(v.schemaVersion, SchemaVersion(major: 1, minor: 1))
        XCTAssertTrue(v.isSchemaSupported, "a pre-#393 minor stays supported")
        XCTAssertEqual(v.nextSwap, .target(to: "spare", reason: nil))
    }

    // AC (#884): a NEWER daemon's unrecognised `expiry.horizon_state` DEGRADES to `.unknown` and the
    // frame STILL decodes — the same forward-compat posture `next_swap.reason.kind` (#412) and
    // `systemic_refresh_source` (#813) take, and for the same reason: the modifier only DECORATES a row
    // that decodes fine, and `WatchStatusStore` DROPS an undecodable line, so throwing would blank the
    // WHOLE panel on every frame to refuse ONE cell.
    //
    // `.unknown` is the right degrade target specifically because it renders as the GAP: a client that
    // cannot classify says "no deadline observed" rather than inventing a reassuring one (#137). The
    // daemon's own `horizon_state` is `#[serde(default)]` to the same fail-safe, so this extends that
    // rule to the token rather than inventing a new one.
    func testUnknownExpiryHorizonTokenDegradesToTheGapRatherThanCostingTheFrame() throws {
        let frame = #"""
        {"type":"snapshot","schema_version":{"major":1,"minor":13},"generated_at":1893456000,"accounts":[{"label":"work","active":true,"enabled":true,"quarantined":false,"recovering":false,"session_pct":30,"weekly_pct":20,"session_resets_at":null,"weekly_resets_at":null,"weekly_exhausted":false,"access_expires_at":null,"refresh_health":null,"auth":"healthy","expiry":{"expires_at":1893800000,"horizon_state":"cohort_pending"}}],"next_swap":{"state":"awaiting_data"},"refresh_enabled":true,"systemic_refresh_failure":null}
        """#
        guard case .snapshot(let v) = try parseWatchFrame(frame) else {
            return XCTFail("an unrecognised horizon token must not cost us the frame")
        }
        XCTAssertEqual(v.accounts.count, 1, "the roster survives the unknown token")
        XCTAssertEqual(v.accounts[0].auth, .healthy, "every other field keeps its value")
        XCTAssertEqual(v.accounts[0].expiry?.horizonState, .unknown)

        // …and the honest consequence: the cell reads as the GAP, NOT as a duration derived from the
        // deadline sitting right beside it. `.unknown` is authoritative — the client will not narrate a
        // classification it does not understand.
        XCTAssertEqual(
            StatusPanelFormat.expiryCell(v.accounts[0].expiry, now: 1_893_456_000),
            StatusPanelFormat.expiryGap)
    }

    // AC (#884): a PARTIAL `expiry` object degrades rather than throwing — deliberately unlike
    // `BlindActive`, whose three fields have no honest default and so throw when one is missing. Both
    // of this object's fields carry `#[serde(default)]` on the daemon side; the mirror matches.
    func testPartialExpiryObjectDegradesToTheFailSafeRatherThanThrowing() throws {
        let frame = #"""
        {"type":"snapshot","schema_version":{"major":1,"minor":13},"generated_at":1893456000,"accounts":[{"label":"work","active":true,"enabled":true,"quarantined":false,"recovering":false,"session_pct":30,"weekly_pct":20,"session_resets_at":null,"weekly_resets_at":null,"weekly_exhausted":false,"access_expires_at":null,"refresh_health":null,"auth":"healthy","expiry":{}}],"next_swap":{"state":"awaiting_data"},"refresh_enabled":true,"systemic_refresh_failure":null}
        """#
        guard case .snapshot(let v) = try parseWatchFrame(frame) else {
            return XCTFail("a partial expiry object must not cost us the frame")
        }
        XCTAssertEqual(v.accounts[0].expiry, AccountExpiry(expiresAt: nil, horizonState: .unknown))
    }

    // AC (#884): an UNPOLLED account omits the key entirely → `expiry == nil`. This is a DIFFERENT fact
    // from a present object holding a null deadline: "never observed" vs "observed, no deadline in the
    // credential". Neither means "not expiring", and the shared golden proves the omission is the real
    // shape a shipped daemon emits (all four wire goldens carry `expiry: None`).
    func testUnpolledAccountOmitsTheExpiryKeyEntirely() throws {
        guard case .snapshot(let v) = try parseWatchFrame(Fixtures.snapshotBasic) else {
            return XCTFail("expected a snapshot frame")
        }
        XCTAssertTrue(v.accounts.allSatisfy { $0.expiry == nil },
                      "an omitted key is 'never observed', never a fabricated reading")
        XCTAssertFalse(
            StatusPanelFormat.rosterShowsExpiry(v.accounts.map(\.expiry), now: 1_893_456_000),
            "and a roster with nothing to say shows no expiry line at all — which is why the committed panel goldens are untouched by #884")
    }

    // AC (#412): a NEWER daemon's unrecognised `next_swap.reason.kind` is a forward-compat DECORATION
    // — it must degrade to `reason: nil` (the bare target label, the SAME path as a pre-#393 omitted
    // reason) and the frame must STILL decode, never be lost. This is the whole fix: one unknown
    // rationale must not silently freeze the panel (`WatchStatusStore` drops an undecodable line, so
    // an unrecognised kind used to take down every account row, every meter, the whole frame).
    func testUnknownReasonKindDecodesToNilReasonAndFrameStillDecodes() throws {
        guard case .snapshot(let v) = try parseWatchFrame(Fixtures.snapshotUnknownReasonKind) else {
            return XCTFail("expected a snapshot frame")
        }
        XCTAssertTrue(v.isSchemaSupported, "a newer minor stays supported")
        // The unrecognised reason degrades to the bare target label (reason == nil)…
        XCTAssertEqual(v.nextSwap, .target(to: "spare", reason: nil))
        // …and the REST of the frame survived — this is the regression the fix prevents.
        XCTAssertEqual(v.accounts.count, 1, "the whole frame decoded, not just next_swap")
        XCTAssertEqual(v.accounts[0].label, "work")
        XCTAssertEqual(v.accounts[0].auth, .healthy)
    }

    // AC (#412): the tolerance is for UNRECOGNISED kinds ONLY. A MALFORMED known kind — here
    // `soonest_reset` without its required `resets_at` — is corruption, not forward-compat, so it
    // stays a HARD decode error; it must NOT be swallowed to `nil` the way an unknown kind is. This
    // pins the tolerate-vs-reject discriminator so a future refactor cannot over-tolerate into it.
    func testMalformedKnownReasonKindStillThrows() {
        XCTAssertThrowsError(try parseWatchFrame(Fixtures.snapshotTargetMalformedReason))
    }

    // AC: "Unknown `type` → ignored (returns an 'unknown' frame, NOT an error)".
    func testUnknownFrameTypesAreIgnoredNotErrors() throws {
        XCTAssertEqual(try parseWatchFrame(Fixtures.unknownFutureType), .unknown)
        XCTAssertEqual(try parseWatchFrame(Fixtures.noTypeTag), .unknown)
        XCTAssertNil(try parseWatchFrame(Fixtures.unknownFutureType).schemaVersion)
    }

    // AC: "malformed line → error".
    func testMalformedLineThrows() {
        XCTAssertThrowsError(try parseWatchFrame(Fixtures.notJSON))
        XCTAssertThrowsError(try parseWatchFrame(""))
    }

    // AC: "`schema_version.major != 1` → flagged unsupported … never mis-rendered".
    func testUnsupportedMajorDecodesButIsFlagged() throws {
        guard case .snapshot(let v) = try parseWatchFrame(Fixtures.snapshotUnsupportedMajor) else {
            return XCTFail("expected a snapshot frame")
        }
        XCTAssertEqual(v.schemaVersion.major, 2)
        XCTAssertFalse(v.isSchemaSupported)
    }

    // AC: "a pre-freeze/absent version decodes to major `0` → unsupported".
    func testPreFreezeVersionDecodesToMajorZeroUnsupported() throws {
        guard case .snapshot(let v) = try parseWatchFrame(Fixtures.snapshotPreFreeze) else {
            return XCTFail("expected a snapshot frame")
        }
        XCTAssertEqual(v.schemaVersion, SchemaVersion(major: 0, minor: 0))
        XCTAssertEqual(v.generatedAt, 0, "absent generated_at defaults to 0")
        XCTAssertFalse(v.isSchemaSupported)

        let beat = try parseWatchFrame(Fixtures.heartbeatPreFreeze)
        XCTAssertEqual(beat, .heartbeat(generatedAt: 7, schemaVersion: SchemaVersion(major: 0, minor: 0)))
        XCTAssertFalse(WireContract.isSupported(try XCTUnwrap(beat.schemaVersion)))
    }

    // Faithful mirror: an unknown internally-tagged `next_swap` state is a hard error.
    func testUnknownNextSwapStateThrows() {
        XCTAssertThrowsError(try parseWatchFrame(Fixtures.snapshotUnknownNextSwap))
    }

    // Faithful mirror (#516): an unknown internally-tagged `canonical_scrub` state is a hard error —
    // the same reject posture as `next_swap.state` (a mis-rendered fleet state is dangerous), NOT the
    // tolerated-decoration posture of an unknown `reason.kind`.
    func testUnknownCanonicalScrubStateThrows() {
        XCTAssertThrowsError(try parseWatchFrame(Fixtures.snapshotUnknownCanonicalScrub))
    }

    // Faithful mirror (#714/#728): an unknown internally-tagged `canary` verdict is a hard error — the same
    // reject posture as `canonical_scrub.state` / `next_swap.state` (a mis-rendered / under-rendered alarm
    // state is dangerous), NOT the tolerated-decoration posture of an unknown `reason.kind`.
    func testUnknownCanaryVerdictThrows() {
        XCTAssertThrowsError(try parseWatchFrame(Fixtures.snapshotUnknownCanary))
    }

    // Faithful mirror: an unknown `auth` value is a hard error.
    func testUnknownAuthValueThrows() {
        XCTAssertThrowsError(try parseWatchFrame(Fixtures.snapshotUnknownAuth))
    }

    // Faithful mirror: a snapshot missing a required field is a hard error. Covers every
    // required (non-Option, no-`serde(default)`) field across the type graph: account `label`,
    // envelope `accounts`, `next_swap` target's `to`, `schema_version.minor`, heartbeat
    // `generated_at`. Each mirrors serde's "missing field" error (verified against the daemon).
    func testMissingRequiredFieldThrows() {
        XCTAssertThrowsError(try parseWatchFrame(Fixtures.snapshotMissingLabel))
        XCTAssertThrowsError(try parseWatchFrame(Fixtures.snapshotMissingAccounts))
        XCTAssertThrowsError(try parseWatchFrame(Fixtures.snapshotTargetMissingTo))
        XCTAssertThrowsError(try parseWatchFrame(Fixtures.snapshotSchemaMissingMinor))
        XCTAssertThrowsError(try parseWatchFrame(Fixtures.heartbeatMissingGeneratedAt))
    }

    // The supported-major constant tracks the daemon's STATUS_SCHEMA_VERSION.major (= 1).
    func testSupportedSchemaMajorMatchesFrozenContract() {
        XCTAssertEqual(WireContract.supportedSchemaMajor, 1)
        XCTAssertTrue(WireContract.isSupported(SchemaVersion(major: 1, minor: 0)))
        XCTAssertTrue(WireContract.isSupported(SchemaVersion(major: 1, minor: 99)))
        XCTAssertFalse(WireContract.isSupported(SchemaVersion(major: 0, minor: 0)))
        XCTAssertFalse(WireContract.isSupported(SchemaVersion(major: 2, minor: 0)))
    }
}
