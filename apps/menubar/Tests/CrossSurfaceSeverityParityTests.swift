// Copyright (c) 2026 Oleksii PELYKH
// SPDX-License-Identifier: MIT

// The PANEL half of the CROSS-SURFACE severity gate (issue #768) — what makes ADR-0026 enforced
// rather than merely recorded.
//
// WHAT WAS MISSING. Parity between the `status` CLI and this panel was asserted only at the STRING
// layer: `StatusPanelFormat` mirrors `src/cli.rs`'s `pct` / `humanizeUntil` / `resetCell` /
// `authCell` / `legacyHealthTags`, and `StatusPanelFormatTests` asserts that mirroring at length.
// Two surfaces can agree on every string and still lead the operator to opposite conclusions —
// through ORDERING and SEVERITY. Issue #575 is the proof: the CLI painted the LEAST-blocking fault
// (`systemic`) its only red while the two act-now vault faults sat plain, ranking them in the
// OPPOSITE order to this panel. Every string was right. Every string test stayed green. It took a
// human noticing, and an ADR to settle.
//
// ADR-0026 settled the invariant — severity is a property of the FAULT, ranked ONCE, rendered by
// each surface in its own vocabulary — and left it "reconciled by this ADR and by tests, not by a
// shared compiled source" (§ Consequences). This file and `src/cli.rs`'s `mod
// cross_surface_parity` ARE those tests. Before them, each surface asserted its rank against its
// OWN hand-written expectation, which is precisely the re-derive-independently mechanism that
// produced #575.
//
// THE MECHANISM. `build/fixtures/cross-surface-severity.json` is EMITTED by the Rust gate from
// `DaemonPayloadFault` (ADR-0026 § Decision 1 makes that the single home of the rank) and read
// HERE, from the same committed bytes — the same shape as `WireGoldenTests` reading the Rust-emitted
// wire goldens, and deliberately NOT the shared cross-language abstraction ADR-0026 Alternative 3
// deferred: no wire field, no codegen, no build-time coupling. The load-bearing property is that
// NEITHER SURFACE CAN MOVE ALONE. Change the panel's rank and this file reddens. Change the CLI's
// and the Rust gate reddens until the manifest is re-emitted — at which point THIS file reddens
// until the panel follows. A one-sided rank change has nowhere to hide.
//
// SCOPE — a RANK gate, not a render gate. It asserts WHICH fault the panel calls worst and at WHICH
// band, never how the banner looks; `PanelGoldenParityTests` (issue #754) owns appearance. And it
// deliberately does NOT force byte-parity with the CLI: the CLI prints every applicable fault line
// while the panel shows exactly ONE banner, which is a legitimate, enumerated divergence
// (`fault-render-medium` in the manifest's register). A gate that flattened deliberate differences
// would be a wrong gate — it would be silenced or deleted by the next person, and the contract
// would die with it. The manifest's `known_divergences` and `uncovered_axes` sections are the
// honest enumeration of what this gate does and does not cover.
//
// THE BASELINE TRAP (issue #437, at the cost of a near brand re-ratification). A gate authored
// against a broken surface blesses the break and thereafter DEFENDS it, reporting green. So every
// predicate here is proven by MUTATION, never by inspection: `testTheGateCatchesEveryDivergenceClass`
// drives DELIBERATELY WRONG resolvers through the SAME `divergences(...)` predicate the real
// assertions use, and each must be caught. A mutation that stops applying is itself a failure — a
// canary that perturbs nothing proves nothing. The mutations are chosen adversarially rather than
// for coverage: the order inversion is the literal #575 defect (a set comparison or a per-fault-only
// band check waves it through), the band flip keeps the order perfect (an order-only predicate waves
// IT through), and the drop is the cardinality class a positional walk that stopped at the shorter
// list would pass.

#if DEBUG
import Foundation
import XCTest

// MARK: - The committed manifest (decoded from the SAME bytes the Rust gate emits)

/// One daemon-payload fault's position in the single cross-surface rank.
private struct FaultRank: Decodable, Equatable {
    let rank: Int
    let id: String
    let severity: String
}

/// Two faults that can be set on ONE snapshot, and therefore genuinely compete for prominence.
/// Read from the manifest rather than re-derived here, so this gate provably walks the same
/// universe of edges the Rust gate walks — a side that derived a smaller universe would compare
/// less and still report green.
private struct ArbitrationEdge: Decodable, Equatable {
    let winner: String
    let loser: String
}

private struct ExclusiveGroup: Decodable, Equatable {
    let wireField: String
    let members: [String]
    let why: String

    private enum CodingKeys: String, CodingKey {
        case wireField = "wire_field"
        case members
        case why
    }
}

private struct AccountSeverityCase: Decodable, Equatable {
    let name: String
    let sessionPct: UInt8?
    let weeklyPct: UInt8?
    let weeklyExhausted: Bool
    let sessionSeverity: String?
    let weeklySeverity: String?

    private enum CodingKeys: String, CodingKey {
        case name
        case sessionPct = "session_pct"
        case weeklyPct = "weekly_pct"
        case weeklyExhausted = "weekly_exhausted"
        case sessionSeverity = "session_severity"
        case weeklySeverity = "weekly_severity"
    }
}

private struct KnownDivergence: Decodable, Equatable {
    let id: String
    let cli: String
    let panel: String
    let why: String
    let record: String
    let pinned: Bool
}

private struct UncoveredAxis: Decodable, Equatable {
    let id: String
    let why: String
}

private struct CrossSurfaceManifest: Decodable {
    let schema: Int
    let about: String
    let daemonFaultRanks: [FaultRank]
    /// The `systemic_refresh_source` values BOTH surfaces must rank identically. Provenance picks
    /// the systemic banner's EVIDENCE clause (#787/#813) and — `daemonFaultBanner`'s own comment
    /// says so — "never moves this rank". That was prose governing runtime with nothing asserting
    /// it: this gate previously exercised systemic only at the `nil` parameter default while
    /// production passes `store.systemicRefreshSource`, so `.preflight` went untested and a
    /// provenance-GATED rank inversion passed every assertion here.
    let systemicProvenanceVariants: [String]
    let exclusiveGroups: [ExclusiveGroup]
    let arbitrationEdges: [ArbitrationEdge]
    let accountSeverityCases: [AccountSeverityCase]
    let knownDivergences: [KnownDivergence]
    let uncoveredAxes: [UncoveredAxis]

    private enum CodingKeys: String, CodingKey {
        case schema
        case about
        case daemonFaultRanks = "daemon_fault_ranks"
        case systemicProvenanceVariants = "systemic_provenance_variants"
        case exclusiveGroups = "exclusive_groups"
        case arbitrationEdges = "arbitration_edges"
        case accountSeverityCases = "account_severity_cases"
        case knownDivergences = "known_divergences"
        case uncoveredAxes = "uncovered_axes"
    }

    /// The contract restricted to the faults one snapshot can hold, keeping the manifest's order
    /// and each entry's ORIGINAL rank number (so a message says "rank 6", the reader's coordinate).
    /// Fails on an id the manifest does not pin — mirroring `Manifest::projection`'s assertion,
    /// because a snapshot exercising an unranked fault is a gap in the CONTRACT, not in the
    /// snapshot, and silently projecting it away would shrink every comparison downstream.
    func projection(_ present: [String],
                    file: StaticString = #filePath,
                    line: UInt = #line) -> [FaultRank] {
        for id in present where !daemonFaultRanks.contains(where: { $0.id == id }) {
            XCTFail("`\(id)` is not pinned in the cross-surface manifest — a fault a snapshot can "
                    + "set must be ranked, or it is being rendered at a severity nobody agreed on",
                    file: file, line: line)
        }
        return daemonFaultRanks.filter { present.contains($0.id) }
    }
}

/// The manifest's medium-neutral band vocabulary, deliberately neither surface's own spelling.
private enum Band {
    static let red = "red"
    static let yellow = "yellow"
    static let plain = "plain"
    static let green = "green"
}

// MARK: - The observed side

/// One fault as the PANEL actually ranks and bands it — the observed side of every comparison,
/// mirroring the Rust gate's `ObservedFault` so one predicate shape judges both surfaces.
private struct ObservedFault: Equatable {
    let id: String
    let severity: String
}

/// The wire inputs one snapshot can carry. Built from manifest fault ids, so a fault added to the
/// contract must also be constructible here or the panel observer silently stops covering it.
private struct FaultSet {
    var keychainLocked = false
    var scrub: CanonicalScrub?
    var canary: CanaryStatus?
    var systemic: UInt32?
    /// The `init` parameter below takes NO default value on purpose, so every construction site
    /// must name a provenance: an implicit `nil` default is exactly what left `.preflight`
    /// untested until issue #768.
    var systemicSource: SystemicRefreshSource?

    init(_ ids: [String],
         systemicSource: SystemicRefreshSource?,
         file: StaticString = #filePath,
         line: UInt = #line) {
        self.systemicSource = systemicSource
        for id in ids {
            switch id {
            case "keychain_locked": keychainLocked = true
            case "canonical_scrub_exhausted": scrub = .exhausted
            case "canonical_scrub_recovering": scrub = .recovering
            case "canary_drift_refusing":
                canary = .drift(displayed: "work", matched: "spare", overridden: false)
            case "canary_drift_overridden":
                canary = .drift(displayed: "work", matched: "spare", overridden: true)
            case "canary_ambiguous": canary = .ambiguous(count: 2)
            case "canary_refused_unparseable_canonical": canary = .refusedUnparseableCanonical
            case "systemic_refresh_failure": systemic = 3
            default:
                XCTFail("no wire mapping for cross-surface fault `\(id)` — a fault added to the "
                        + "manifest must also be constructible here, or this gate silently stops "
                        + "covering it", file: file, line: line)
            }
        }
    }
}

/// How a snapshot's faults resolve to the ONE banner the panel shows. A parameter rather than a
/// direct call so the canary can substitute a DELIBERATELY WRONG panel and drive it through the
/// very same derivation and the very same predicate — mutating the surface, not the expectation.
private typealias BannerResolver = (FaultSet) -> StatusPanelFormat.Banner?

/// The real panel.
private let panelResolver: BannerResolver = { faults in
    StatusPanelFormat.daemonFaultBanner(keychainLocked: faults.keychainLocked,
                                        scrub: faults.scrub,
                                        systemicRefreshFailure: faults.systemic,
                                        systemicRefreshSource: faults.systemicSource,
                                        canary: faults.canary)
}

final class CrossSurfaceSeverityParityTests: XCTestCase {

    // MARK: - Manifest loading

    /// Repo-root `build/fixtures/` resolved from this test file's own location — the same idiom
    /// `WireGoldenTests` and `BarGlyphParityTests` use; CI checks the tree out at the path it
    /// compiled from.
    private static func manifestURL(file: StaticString = #filePath) -> URL {
        URL(fileURLWithPath: "\(file)")                // .../apps/menubar/Tests/<this file>.swift
            .deletingLastPathComponent()               // .../apps/menubar/Tests
            .deletingLastPathComponent()               // .../apps/menubar
            .deletingLastPathComponent()               // .../apps
            .deletingLastPathComponent()               // repo root
            .appendingPathComponent("build/fixtures/cross-surface-severity.json")
    }

    /// Thrown when the committed manifest cannot be read. A plain error rather than `XCTSkip` on
    /// purpose: a gate that cannot run must FAIL, and a skip is precisely the "quietly not run"
    /// outcome this whole contract is built to rule out.
    private struct ManifestUnreadable: Error {
        let path: String
    }

    /// The manifest shape this consumer was written against — the Swift mirror of
    /// `cross_surface::MANIFEST_SCHEMA`. Bumped only when a section is added or renamed, never for
    /// a content re-baseline.
    private static let expectedSchema = 2

    private func manifest(file: StaticString = #filePath, line: UInt = #line) throws
        -> CrossSurfaceManifest {
        let url = Self.manifestURL()
        guard let bytes = try? Data(contentsOf: url) else {
            XCTFail("the committed cross-surface manifest is MISSING at \(url.path) — this gate "
                    + "cannot run, which is a failure and never a skip. Regenerate it with "
                    + "`cargo test -- --ignored emit_cross_surface_severity_manifest`.",
                    file: file, line: line)
            throw ManifestUnreadable(path: url.path)
        }
        let decoded = try JSONDecoder().decode(CrossSurfaceManifest.self, from: bytes)
        XCTAssertEqual(decoded.schema, Self.expectedSchema,
            "the cross-surface manifest SHAPE changed (schema \(decoded.schema)) — update this "
            + "consumer too, rather than letting new sections decode to silent defaults",
            file: file, line: line)
        XCTAssertFalse(decoded.daemonFaultRanks.isEmpty,
            "the manifest pins zero faults — cardinality-zero is an automatic FAIL, never a pass",
            file: file, line: line)
        return decoded
    }

    // MARK: - THE predicate (the one judgement both the real gates and the canaries route through)

    /// Compare an observed rank sequence against the manifest and return every way they disagree.
    ///
    /// Sensitive on THREE independent axes, because a predicate blind to any one of them waves a
    /// real defect through: CARDINALITY (a short or empty observation is a FAIL, never a vacuous
    /// pass), ORDER (position `i` must name the same fault — the #575 defect exactly), and BAND
    /// (a surface can keep perfect order and still paint the wrong urgency).
    ///
    /// Mirrors `cross_surface::rank_divergences` in `src/cross_surface.rs`. The two are deliberately
    /// separate implementations in separate languages — a shared one would be the cross-language
    /// abstraction ADR-0026 Alternative 3 deferred. What they SHARE is the manifest, which is the
    /// coupling that matters.
    private func divergences(_ expected: [FaultRank], _ observed: [ObservedFault]) -> [String] {
        var findings: [String] = []
        guard !observed.isEmpty else {
            return ["the panel reported ZERO ranked faults — cardinality-zero is an automatic "
                    + "FAIL, never a pass (the observation is degenerate, so a green here would be "
                    + "evidence of nothing)"]
        }
        if expected.count != observed.count {
            findings.append("rank COUNT diverges: the manifest pins \(expected.count) fault(s), "
                + "the panel reported \(observed.count) — expected "
                + "[\(expected.map(\.id).joined(separator: ", "))], observed "
                + "[\(observed.map(\.id).joined(separator: ", "))]")
        }
        for (index, expect) in expected.enumerated() {
            guard index < observed.count else {
                findings.append("rank \(expect.rank) (`\(expect.id)`) is MISSING from the panel's "
                    + "ordering")
                continue
            }
            let actual = observed[index]
            if expect.id != actual.id {
                findings.append("ORDER diverges at rank \(expect.rank): the manifest ranks "
                    + "`\(expect.id)` there, the panel ranks `\(actual.id)` — this is the issue "
                    + "#575 shape exactly (the same faults, ranked differently), and ADR-0026 "
                    + "makes it a defect, not a style difference")
            }
            if expect.severity != actual.severity {
                findings.append("SEVERITY BAND diverges for `\(actual.id)` (rank \(expect.rank)): "
                    + "the manifest pins `\(expect.severity)`, the panel renders "
                    + "`\(actual.severity)` — the colour/glyph VOCABULARY may differ per medium "
                    + "(R-2), the BAND may not")
            }
        }
        for extra in observed.dropFirst(expected.count) {
            findings.append("the panel ranks `\(extra.id)`, which the manifest does not pin at all "
                + "— a new daemon-payload fault must be added to the cross-surface rank (ADR-0026: "
                + "\"any FOURTH fault inherits this\"), not ranked locally")
        }
        return findings
    }

    /// The manifest's band for a panel banner kind. `.healthy` has no daemon-fault meaning, so it
    /// is reported rather than silently coerced.
    private func band(_ kind: StatusPanelFormat.BannerKind) -> String {
        switch kind {
        case .error:   return Band.red
        case .warning: return Band.yellow
        case .info:    return Band.plain
        case .healthy: return "healthy(unexpected-for-a-fault)"
        }
    }

    // MARK: - Deriving the panel's own worst-first ordering (structurally, not by eyeballing)

    /// The banner the panel shows for ONE fault set alone — a fault's identity, for the peel below.
    private func solo(_ id: String,
                      _ source: SystemicRefreshSource?,
                      _ resolve: BannerResolver) -> StatusPanelFormat.Banner? {
        resolve(FaultSet([id], systemicSource: source))
    }

    /// Read the panel's worst-first ORDER for a snapshot by PEELING: resolve the whole set, match
    /// the winning banner to whichever fault produces it alone, remove that fault, repeat. Nothing
    /// about the expected order is fed in — the sequence is derived from the panel's own
    /// arbitration, which is what makes the comparison evidence rather than a restatement.
    ///
    /// The peel is only sound while the solo banners are pairwise DISTINCT and non-nil; both are
    /// asserted separately by `testEveryFaultProducesADistinctBannerSoTheObserverCanIdentifyIt`,
    /// which is where a real panel that stopped voicing a fault reddens loudly.
    ///
    /// When a still-unpeeled set produces NO banner, the observer records a SHORTER sequence and
    /// stops rather than failing here. That is the honest model — a fault the panel does not voice
    /// is a fault it does not rank — and it keeps the judgement in `divergences` (which reports it
    /// as a cardinality divergence) instead of in a side-assert the canary cannot exercise.
    private func observedOrder(_ ids: [String],
                               _ source: SystemicRefreshSource?,
                               _ resolve: BannerResolver) -> [ObservedFault] {
        var remaining = ids
        var order: [ObservedFault] = []
        while !remaining.isEmpty {
            guard let winner = resolve(FaultSet(remaining, systemicSource: source)) else {
                return order
            }
            // An UNATTRIBUTABLE winner is different in kind: the panel is voicing something, but
            // the observer cannot say which fault it is, so no ordering claim can be made at all.
            // That is a broken observer, not a divergent surface, so it fails here rather than
            // being laundered into a divergence finding.
            guard let matched = remaining.first(where: { solo($0, source, resolve) == winner })
            else {
                XCTFail("the panel's banner for \(remaining) matches NO single fault's own banner "
                        + "(\(winner.title)) — the observer cannot attribute it, so no ordering "
                        + "claim can be made")
                return order
            }
            order.append(ObservedFault(id: matched, severity: band(winner.kind)))
            remaining.removeAll { $0 == matched }
        }
        return order
    }

    /// The manifest's provenance tokens, resolved to the panel's own enum. Doubly optional, and
    /// both levels carry meaning: the OUTER `nil` is "this gate cannot construct that token" (the
    /// caller fails loudly), while `.some(nil)` is the `none` token — a pre-#813 daemon that sends
    /// no discriminant at all.
    ///
    /// `.unrecognized` has NO manifest token on purpose: no daemon ever sends it —
    /// `SystemicRefreshSource.init(wireToken:)` produces it locally for a bracket a NEWER daemon
    /// introduced — so it has no CLI counterpart to be in a cross-surface contract with. It is
    /// walked anyway, below, as a panel-only case.
    private func provenance(_ token: String) -> SystemicRefreshSource?? {
        switch token {
        case "none":      return .some(nil)
        case "sweep":     return .some(.sweep)
        case "preflight": return .some(.preflight)
        default:          return nil
        }
    }

    /// Every provenance this gate walks: the manifest's shared set, plus the panel-only
    /// `.unrecognized` forward-compat bracket. Fails loudly on a token it cannot map, rather than
    /// silently walking fewer variants than the contract pins.
    private func allProvenances(_ manifest: CrossSurfaceManifest,
                                file: StaticString = #filePath,
                                line: UInt = #line) -> [SystemicRefreshSource?] {
        var resolved: [SystemicRefreshSource?] = []
        for token in manifest.systemicProvenanceVariants {
            guard let mapped = provenance(token) else {
                XCTFail("the manifest pins systemic provenance `\(token)`, which this gate cannot "
                        + "construct — it would silently walk fewer variants than the contract "
                        + "claims", file: file, line: line)
                continue
            }
            resolved.append(mapped)
        }
        XCTAssertEqual(resolved.count, manifest.systemicProvenanceVariants.count,
                       "resolved \(resolved.count) of "
                       + "\(manifest.systemicProvenanceVariants.count) pinned provenances")
        XCTAssertGreaterThanOrEqual(resolved.count, 2,
            "fewer than two provenances means the axis is not actually being varied — which is how "
            + "a provenance-gated rank inversion slipped this gate before")
        // The panel-only forward-compat bracket, appended AFTER the shared set so a failure message
        // makes clear which class it belongs to.
        return resolved + [.unrecognized]
    }

    // MARK: - Guard: the panel's banners are distinguishable at all

    func testEveryFaultProducesADistinctBannerSoTheObserverCanIdentifyIt() throws {
        // Degenerate-subject guard, and the direct analogue of the Rust gate's distinct-fault-line
        // check. The peel above identifies a winner by matching it to a solo banner, so two faults
        // sharing a banner would make every attribution ambiguous and every ordering claim below
        // meaningless. It also catches a fault that has stopped producing a banner entirely.
        let manifest = try manifest()
        for source in allProvenances(manifest) {
            var banners: [(String, StatusPanelFormat.Banner)] = []
            for entry in manifest.daemonFaultRanks {
                let banner = try XCTUnwrap(solo(entry.id, source, panelResolver),
                    "`\(entry.id)` produces NO banner on its own — it is a ranked daemon-payload "
                    + "fault the panel has stopped voicing, so it is unranked on this surface")
                banners.append((entry.id, banner))
            }
            // Cardinality, pinned to a literal rather than to `manifest.daemonFaultRanks.count`:
            // `banners` is built one-per-entry from that very array, so comparing the two would be
            // an assertion that cannot fail. The literal is what actually notices the fault set
            // changing shape.
            XCTAssertEqual(banners.count, 8,
                           "expected 8 ranked daemon-payload faults, observed \(banners.count) — "
                           + "the fault set changed shape, so re-check both surfaces AND this gate")
            for i in 0..<banners.count {
                for j in (i + 1)..<banners.count {
                    XCTAssertNotEqual(banners[i].1, banners[j].1,
                        "`\(banners[i].0)` and `\(banners[j].0)` produce the SAME banner at "
                        + "provenance \(String(describing: source)) — the panel cannot tell the two "
                        + "faults apart, and neither can this gate")
                }
            }
        }
    }

    // MARK: - AC1/AC2: the panel's worst-first ordering + bands match the committed contract

    func testTheMaximalSnapshotIsRankedWorstFirstAtThePinnedBands() throws {
        // Issue #768 AC1 on this surface: ONE fixture snapshot, resolved by the panel, its fault
        // ordering read back out of the panel's own arbitration. Four faults is the maximum a
        // single snapshot can hold — `canonical_scrub` and `canary` are each one wire value — so
        // this is the widest real co-occurrence, and it spans all three bands. The Rust gate
        // asserts the SAME four ids against the SAME manifest entries.
        let manifest = try manifest()
        let present = ["keychain_locked", "canonical_scrub_exhausted", "canary_drift_refusing",
                       "systemic_refresh_failure"]
        let expected = manifest.projection(present)

        // Once per systemic PROVENANCE. Provenance picks the systemic banner's evidence clause
        // (#787/#813) and must never move the rank — an invariant `daemonFaultBanner` states in
        // prose and, before this walk, nothing asserted on either surface.
        for source in allProvenances(manifest) {
            let observed = observedOrder(present, source, panelResolver)
            let findings = divergences(expected, observed)
            XCTAssertTrue(findings.isEmpty,
                "the panel diverges from the committed cross-surface contract at provenance "
                + "\(String(describing: source)):\n  "
                + findings.joined(separator: "\n  ") + "\n\n" + Self.rebaselineHint)
        }
    }

    func testEveryArbitrationEdgeIsWonByTheWorseFault() throws {
        // The total order, edge by edge — the same 21 co-occurrable pairs the Rust gate walks, read
        // from the same manifest. A maximal snapshot only holds four faults at once, so this
        // pairwise walk is what actually establishes the full worst-first rank.
        let manifest = try manifest()
        let provenances = allProvenances(manifest)
        var checked = 0
        for edge in manifest.arbitrationEdges {
            let present = [edge.winner, edge.loser]
            // An edge that involves systemic is walked once per provenance; the rest have no
            // provenance to vary, so one pass covers them.
            let involvesSystemic = present.contains("systemic_refresh_failure")
            let sources: [SystemicRefreshSource?] = involvesSystemic ? provenances : [nil]
            for source in sources {
                let observed = observedOrder(present, source, panelResolver)
                let findings = divergences(manifest.projection(present), observed)
                XCTAssertTrue(findings.isEmpty,
                    "`\(edge.winner)` vs `\(edge.loser)` diverges from the contract at provenance "
                    + "\(String(describing: source)):\n  "
                    + findings.joined(separator: "\n  ") + "\n\n" + Self.rebaselineHint)
                checked += 1
            }
        }
        // Cardinality: a pass over a shrunken edge list is not evidence. 21 co-occurrable pairs
        // over 8 faults (28 total minus 1 scrub pair and 6 canary pairs); the 7 that involve
        // systemic are walked once per provenance.
        let systemicEdges = manifest.arbitrationEdges.filter {
            $0.winner == "systemic_refresh_failure" || $0.loser == "systemic_refresh_failure"
        }.count
        XCTAssertEqual(manifest.arbitrationEdges.count, 21, "the edge universe changed shape")
        XCTAssertEqual(systemicEdges, 7, "systemic should meet all 7 other faults")
        let expectedComparisons = (21 - systemicEdges) + systemicEdges * provenances.count
        XCTAssertEqual(checked, expectedComparisons,
            "walked \(checked) comparisons — expected \(expectedComparisons) "
            + "(\(21 - systemicEdges) provenance-free edges + \(systemicEdges) systemic edges × "
            + "\(provenances.count) provenance variants)")
    }

    func testTheMutuallyExclusiveGroupsAreDeclaredRatherThanQuietlyUntested() throws {
        // The complement of the edge walk: the pairs NOT walked are exactly the ones no snapshot
        // can hold, and they are DECLARED as such rather than merely absent. An unexplained gap in
        // an edge list reads, to the next maintainer, exactly like coverage.
        let manifest = try manifest()
        XCTAssertFalse(manifest.exclusiveGroups.isEmpty, "no exclusive groups declared")
        var excludedPairs = 0
        for group in manifest.exclusiveGroups {
            XCTAssertGreaterThanOrEqual(group.members.count, 2,
                "exclusive group `\(group.wireField)` excludes nothing")
            excludedPairs += group.members.count * (group.members.count - 1) / 2
            // Every member must be a ranked fault, and the wire really must hold only one at a
            // time — which is why `FaultSet` assigns rather than accumulates for these fields.
            for member in group.members {
                XCTAssertTrue(manifest.daemonFaultRanks.contains { $0.id == member },
                              "exclusive group `\(group.wireField)` names unranked `\(member)`")
            }
        }
        let n = manifest.daemonFaultRanks.count
        XCTAssertEqual(manifest.arbitrationEdges.count, n * (n - 1) / 2 - excludedPairs,
            "the arbitration edges plus the declared-exclusive pairs do not account for every "
            + "pair of ranked faults — some pair is neither tested nor explained")
    }

    // MARK: - AC2, per account: the utilization bands agree with the CLI's classifier

    func testThePerAccountUtilizationBandsMatchTheCommittedContract() throws {
        // The per-account half of AC2. The committed cases were classified by the CLI's own
        // `StatusRow`; here the panel's `sessionSeverity` / `weeklySeverity` classify the SAME
        // inputs. `StatusPanelFormatTests` already asserts these bands — against hand-written
        // expectations, which is exactly the re-derive-independently shape #575 came from. This
        // asserts them against the CLI's actual output.
        let manifest = try manifest()
        XCTAssertFalse(manifest.accountSeverityCases.isEmpty,
                       "zero account-severity cases — cardinality-zero is an automatic FAIL")
        for entry in manifest.accountSeverityCases {
            let session = StatusPanelFormat.sessionSeverity(entry.sessionPct)
            let weekly = StatusPanelFormat.weeklySeverity(weeklyPct: entry.weeklyPct,
                                                          weeklyExhausted: entry.weeklyExhausted)
            XCTAssertEqual(name(session), entry.sessionSeverity,
                "`\(entry.name)`: session \(String(describing: entry.sessionPct))% — the CLI bands "
                + "it `\(String(describing: entry.sessionSeverity))`, the panel bands it "
                + "`\(name(session) ?? "nil")`\n\n" + Self.rebaselineHint)
            XCTAssertEqual(name(weekly), entry.weeklySeverity,
                "`\(entry.name)`: weekly \(String(describing: entry.weeklyPct))% "
                + "(exhausted: \(entry.weeklyExhausted)) — the CLI bands it "
                + "`\(String(describing: entry.weeklySeverity))`, the panel bands it "
                + "`\(name(weekly) ?? "nil")`\n\n" + Self.rebaselineHint)
        }
        // Non-degeneracy: an all-`nil` case set would compare equal while asserting nothing about
        // the bands, so require the committed cases to span every outcome.
        let sessions = Set(manifest.accountSeverityCases.map { $0.sessionSeverity ?? "nil" })
        XCTAssertTrue(sessions.isSuperset(of: [Band.green, Band.yellow, Band.red, "nil"]),
            "the committed cases do not span green/yellow/red/no-reading (\(sessions.sorted())), "
            + "so a band mistake could hide in an uncovered arm")
    }

    private func name(_ severity: StatusPanelFormat.UsageSeverity?) -> String? {
        switch severity {
        case .green:  return Band.green
        case .yellow: return Band.yellow
        case .red:    return Band.red
        case nil:     return nil
        }
    }

    // MARK: - AC4: the enumerated legitimate divergences, PINNED so they cannot silently drift

    func testThePanelSideOfEachPinnedDivergenceIsStillTrue() throws {
        // A divergence that is merely DOCUMENTED drifts. Each pinned entry is asserted on the CLI
        // there and on the panel here, so the register cannot quietly come to describe a divergence
        // that no longer exists — or stop describing one that does. This is what "enumerated and
        // justified rather than asserted away" has to mean if it is to survive contact with time.
        let manifest = try manifest()
        let pinned = manifest.knownDivergences.filter(\.pinned)
        XCTAssertGreaterThanOrEqual(pinned.count, 2,
                                    "expected at least two PINNED divergences, found \(pinned.count)")
        for entry in pinned {
            switch entry.id {
            case "blind-degraded-tint":
                // The panel's half: a DEGRADED blind-active row is ORANGE, not the CLI's red,
                // because its GLANCE is `.attention` — one rung below `.noRunway` — so red would
                // over-signal past the glance. The CLI's half (red) is asserted in `src/cli.rs`.
                XCTAssertEqual(entry.panel, "orange",
                               "the register claims the panel paints blind-DEGRADED `\(entry.panel)`")
                XCTAssertEqual(StatusPanelFormat.blindSymbol(.degraded).tint, .orange,
                    "the panel's DEGRADED blind tint is no longer orange, so the enumerated "
                    + "divergence no longer describes reality — either the divergence was closed "
                    + "(re-emit the manifest without it) or the panel regressed")
                // …and the boundary of the divergence: CORNERED is red on BOTH surfaces, because
                // its glance IS `.noRunway`. Without this the entry would read as "the panel never
                // goes red", which is false and would mislead the next editor.
                XCTAssertEqual(StatusPanelFormat.blindSymbol(.cornered).tint, .red,
                    "CORNERED is red on both surfaces — the divergence is scoped to DEGRADED")
                XCTAssertEqual(StatusPanelFormat.blindSymbol(.ok).tint, .neutral,
                               "an OK blind row is calm on both surfaces")

            case "fault-render-medium":
                // The panel's half: exactly ONE banner even when several faults are set. This is
                // what makes byte-parity with the CLI's multi-line output a WRONG gate, and why
                // this contract compares rank rather than bytes.
                let several = FaultSet(["keychain_locked", "canonical_scrub_exhausted",
                                        "canary_drift_refusing", "systemic_refresh_failure"],
                                       systemicSource: .sweep)
                let banner = try XCTUnwrap(panelResolver(several))
                XCTAssertEqual(banner, solo("keychain_locked", .sweep, panelResolver),
                    "with four faults set the panel must show the WORST one's single banner — "
                    + "that single-banner behaviour is the divergence this entry records")

            default:
                XCTFail("divergence `\(entry.id)` is marked pinned but nothing on this surface "
                        + "asserts it — either assert it here or set `pinned: false` and re-emit")
            }
        }
    }

    func testTheUncoveredAxesAreDeclaredRatherThanImplied() throws {
        // Honest scope. This gate covers the daemon-payload rank and the per-account utilization
        // bands; everything it deliberately does not touch is named in the manifest, so a reader
        // can tell "not covered" from "covered and green".
        let manifest = try manifest()
        XCTAssertFalse(manifest.uncoveredAxes.isEmpty,
            "no uncovered axes declared — there are some, and an undeclared gap reads exactly like "
            + "coverage")
        for axis in manifest.uncoveredAxes {
            XCTAssertGreaterThan(axis.why.count, 40,
                "uncovered axis `\(axis.id)` has no real rationale — a bare id is not an enumeration")
        }
    }

    // MARK: - CONSTRAINT-A: the gate PROVES it can fail (issue #768 AC3), by MUTATION

    func testTheGateCatchesEveryDivergenceClass() throws {
        // The canary. Each mutation is a DELIBERATELY WRONG panel, driven through the SAME
        // `observedOrder` derivation and the SAME `divergences` predicate the real assertions above
        // use — never a hand-built wrong list fed straight to the comparison, which would prove
        // something about the comparison and nothing about the gate.
        let manifest = try manifest()
        let present = ["keychain_locked", "canonical_scrub_exhausted", "canary_drift_refusing",
                       "systemic_refresh_failure"]
        let expected = manifest.projection(present)

        // Guard first: the UNMUTATED panel must be clean, or every mutation below is "caught"
        // trivially and the canary distinguishes nothing.
        // The canary runs at `.preflight` deliberately: it is the provenance that went untested
        // until issue #768's own validation caught it, and a provenance-gated inversion is exactly
        // the divergence class that slipped through then.
        let clean = observedOrder(present, .preflight, panelResolver)
        XCTAssertTrue(divergences(expected, clean).isEmpty,
            "the unmutated panel already diverges — the canary cannot distinguish a working gate "
            + "from a permanently-red one")

        for mutation in Self.mutations {
            let observed = observedOrder(present, .preflight, mutation.resolver)
            // A mutation that no longer PERTURBS anything is a failure in its own right: a canary
            // that perturbs nothing is indistinguishable, from the outside, from one that passes.
            XCTAssertNotEqual(observed, clean,
                "mutation `\(mutation.name)` left the observation UNCHANGED — it is a no-op, so "
                + "the `caught` check below would be vacuous")
            let findings = divergences(expected, observed)
            XCTAssertFalse(findings.isEmpty,
                "mutation `\(mutation.name)` was NOT caught — the gate is blind to this divergence "
                + "class, so a green from it is not evidence.\n  mutated ordering: ["
                + observed.map { "\($0.id):\($0.severity)" }.joined(separator: ", ") + "]")
            // …and it was caught for the RIGHT reason. A predicate that reported "count diverges"
            // for an order inversion would technically be non-empty while being blind to order.
            XCTAssertTrue(findings.contains { $0.contains(mutation.expectedFinding) },
                "mutation `\(mutation.name)` was caught, but not as a "
                + "`\(mutation.expectedFinding)` — the gate reddened for the wrong reason, which "
                + "means it is not actually sensitive on that axis:\n  "
                + findings.joined(separator: "\n  "))
        }
        XCTAssertEqual(Self.mutations.count, 3,
                       "expected 3 divergence classes in the canary vocabulary")
    }

    /// One deliberately-wrong panel, and the finding class it must produce.
    private struct Mutation {
        let name: String
        let resolver: BannerResolver
        /// A substring of the finding this mutation must produce — so "caught" cannot be satisfied
        /// by reddening on an unrelated axis.
        let expectedFinding: String
    }

    /// The divergence classes chosen adversarially: each is one a WEAKER predicate would wave
    /// through, and together they pin all three axes `divergences` claims to be sensitive on.
    private static let mutations: [Mutation] = [
        // Issue #575, reproduced literally: `systemic` — the LEAST-blocking fault — is promoted
        // above the act-now vault pair. Every banner is correct; only the arbitration moved. A set
        // comparison, or a per-fault band check with no ordering, passes this.
        Mutation(
            name: "systemic-outranks-the-vault-pair (the literal #575 inversion)",
            resolver: { faults in
                if faults.systemic != nil {
                    return StatusPanelFormat.daemonFaultBanner(
                        keychainLocked: false,
                        scrub: nil,
                        systemicRefreshFailure: faults.systemic,
                        systemicRefreshSource: faults.systemicSource,
                        canary: nil)
                }
                return panelResolver(faults)
            },
            expectedFinding: "ORDER diverges"),

        // Perfect order, wrong urgency: `systemic` keeps rank 6 but is painted the act-now band.
        // An order-only predicate is structurally blind to this.
        Mutation(
            name: "systemic-painted-act-now (band flip, order untouched)",
            resolver: { faults in
                guard let banner = panelResolver(faults) else { return nil }
                // AT THE SAME PROVENANCE — the systemic banner's evidence clause varies with it
                // (#787/#813), so a source-less comparison here never matches and the mutation
                // silently degrades into a no-op.
                let systemicAlone = StatusPanelFormat.daemonFaultBanner(
                    keychainLocked: false, scrub: nil,
                    systemicRefreshFailure: faults.systemic,
                    systemicRefreshSource: faults.systemicSource, canary: nil)
                guard banner == systemicAlone else { return banner }
                return StatusPanelFormat.Banner(title: banner.title,
                                                detail: banner.detail,
                                                kind: .error)
            },
            expectedFinding: "SEVERITY BAND diverges"),

        // A fault stops being ranked at all — the shape the panel takes when a resolver arm is
        // deleted or its guard inverted. The cardinality class.
        Mutation(
            name: "the-worst-fault-stops-being-ranked (cardinality)",
            resolver: { faults in
                var demoted = faults
                demoted.keychainLocked = false
                return panelResolver(demoted)
            },
            expectedFinding: "rank COUNT diverges"),
    ]

    private static let rebaselineHint =
        "If this change to the cross-surface severity contract is INTENTIONAL, it starts on the "
        + "RUST side — `src/cli.rs`'s `DaemonPayloadFault` is ADR-0026's single home of the rank:\n"
        + "    cargo test -- --ignored emit_cross_surface_severity_manifest\n"
        + "then move THIS surface to match. If instead the panel is what should change, change it "
        + "here and re-emit so the CLI is handed the new rank. What you must not do is change one "
        + "surface and silence the other — ADR-0026 makes a one-sided rank change a defect, and "
        + "this gate is that decision's regression test (issue #575, issue #768)."
}
#endif
