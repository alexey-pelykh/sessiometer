// Copyright (c) 2026 Oleksii PELYKH
// SPDX-License-Identifier: MIT

//! The CROSS-SURFACE severity contract (issue #768) — the machinery that makes
//! [ADR-0026](../docs/adr/0026-daemon-fault-severity-rank-is-cross-surface.md) *enforced* rather
//! than merely *recorded*.
//!
//! # What was missing, and why string parity could not catch it
//!
//! `StatusPanelFormat` mirrors `src/cli.rs` extensively — `pct`, `humanizeUntil`, `resetCell`,
//! `authCell`, `legacyHealthTags` — and `StatusPanelFormatTests` asserts that mirroring at the
//! STRING layer. Two surfaces can nevertheless agree on every string and still make the operator
//! draw opposite conclusions, through ORDERING and SEVERITY. Issue #575 is the proof: the `status`
//! CLI and the menubar panel ranked the three daemon-payload faults in OPPOSITE order — the CLI
//! painted the LEAST-blocking fault (`systemic`) its only red while the two act-now vault faults sat
//! plain. Every string was correct. Every string test stayed green.
//!
//! ADR-0026 settled the invariant: **severity is a property of the FAULT, ranked ONCE, rendered by
//! each surface in its own vocabulary — the colour/glyph vocabulary may differ, the RANK may not.**
//! But it left the rank living in two languages, "reconciled by this ADR and by tests, not by a
//! shared compiled source" (§ Consequences). Until this module there were no such tests: each
//! surface asserted its own rank against its OWN hand-written expectation, which is *exactly* the
//! re-derive-independently mechanism that produced #575 in the first place.
//!
//! # The shape of the fix: one committed manifest, two independent conformers
//!
//! ADR-0026 Alternative 3 DEFERRED a shared cross-language abstraction (a wire rank, or codegen)
//! as heavier than the leak warrants. This is the lighter mechanism it left room for, and it
//! deliberately introduces no wire field, no codegen, and no build-time coupling:
//!
//! 1. Rust — the canonical rank home per ADR-0026 § Decision 1 — EMITS
//!    `build/fixtures/cross-surface-severity.json`, a byte-pinned manifest of the rank order, the
//!    per-fault severity band, the arbitration edges, the per-account utilization bands, the
//!    per-account REFRESH-token expiry cells, and the ENUMERATED legitimate divergences.
//! 2. The Rust gate asserts the manifest still describes `DaemonPayloadFault` *and* the text
//!    `render_status` actually prints.
//! 3. The Swift gate (`apps/menubar/Tests/CrossSurfaceSeverityParityTests.swift`) reads the SAME
//!    committed bytes and asserts `StatusPanelFormat` conforms to them.
//!
//! The load-bearing property is that **neither surface can move alone**. Change the Rust rank and
//! the Rust gate reddens until the manifest is re-emitted; re-emit it and the SWIFT gate reddens
//! until the panel is moved to match. A one-sided rank change has nowhere to hide — which is the
//! one thing #575 needed and did not have.
//!
//! # Re-baselining is deliberate, never a side effect
//!
//! Like the CLI render goldens ([`crate::render_golden`]) and the panel goldens, the manifest IS
//! the gate's assertion content. Writing it takes an explicit act — the `#[ignore]`d
//! `emit_cross_surface_severity_manifest` test. There is deliberately no auto-bless-on-missing:
//! `include_str!` makes a missing manifest a BUILD error rather than a silently skipped gate.
//!
//! Compiled only under `cfg(test)` — this is test machinery, and nothing in the shipping binary
//! reads the manifest.

use std::collections::BTreeSet;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// The committed manifest, resolved from `CARGO_MANIFEST_DIR` so it is the same path whatever the
/// test's working directory. The Swift side resolves the same file from its own `#filePath`.
pub(crate) fn manifest_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("build/fixtures/cross-surface-severity.json")
}

/// The committed manifest bytes, as a COMPILE-TIME input — so a deleted or renamed manifest is a
/// build error rather than a gate that quietly skips. (The Swift consumer necessarily reads the
/// same file at runtime; its own missing-file arm fails loudly for the same reason.)
pub(crate) const COMMITTED_MANIFEST: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/build/fixtures/cross-surface-severity.json"
));

/// The committed contract, parsed and self-checked. Every consumer goes through here, so no gate
/// can accidentally use an unvalidated oracle.
pub(crate) fn committed_manifest() -> Manifest {
    let manifest = Manifest::parse(COMMITTED_MANIFEST);
    manifest.assert_internally_consistent();
    manifest
}

/// The manifest's own version. Bumped only when the SHAPE changes (a new section, a renamed
/// field) — never for a content change, which is an ordinary re-baseline. Both consumers assert
/// it, so a shape change that reaches only one language fails loudly instead of decoding to
/// silent defaults.
pub(crate) const MANIFEST_SCHEMA: u32 = 3;

/// A severity BAND in the cross-surface vocabulary — deliberately neither surface's own spelling.
/// The CLI renders these as SGR overlays (`Severity::Red` / `Severity::Yellow` / no escape); the
/// panel renders them as banner tints (`.error` / `.warning` / `.info`). R-2 is rank-parity, not
/// glyph-parity, so the manifest names the BAND and each surface maps it to its own medium.
///
/// Mirrored by `Band` in `apps/menubar/Tests/CrossSurfaceSeverityParityTests.swift` — the spellings
/// are the manifest's, so both sides must agree on them for any comparison to mean anything.
pub(crate) mod band {
    pub(crate) const RED: &str = "red";
    pub(crate) const YELLOW: &str = "yellow";
    /// Deliberately uncoloured on the CLI; `.info` (calm, no tint) on the panel. Not "none" — the
    /// panel does render a banner, it just renders it calm.
    pub(crate) const PLAIN: &str = "plain";
    /// Utilization only. No daemon-payload FAULT is ever green — a fault that did not matter would
    /// not be a fault — so this band appears in [`Manifest::account_severity_cases`] and nowhere
    /// else, which is why the fault-rank spell-check below deliberately rejects it.
    pub(crate) const GREEN: &str = "green";
    /// De-emphasis: the CLI's `Severity::Dim` (SGR 2, faint) and the panel's `.neutral`. Expiry
    /// only — it is the band a deadline BEYOND the operator's horizon gets, where there is nothing
    /// to act on and a coloured cell would be noise. Kept DISTINCT from [`PLAIN`] even though both
    /// render de-emphasised on the panel: they are different verdicts (`beyond` observed something
    /// and said it is far off; `plain` observed nothing), the CLI does render them differently, and
    /// collapsing them here would let a surface swap one for the other unnoticed.
    pub(crate) const DIM: &str = "dim";
}

/// One daemon-payload fault's position in the single cross-surface rank.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct FaultRank {
    /// 1-based, worst-first. Redundant with array position ON PURPOSE: a hand edit that reorders
    /// the array without renumbering (or vice versa) is caught by
    /// [`Manifest::assert_internally_consistent`] rather than silently changing the contract.
    pub(crate) rank: u8,
    /// The cross-surface fault identifier. Not a wire field — the wire carries `keychain_locked`,
    /// `canonical_scrub` and `canary` as separate shapes; these ids name the (fault, VARIANT)
    /// pairs the rank is actually over, which is the load-bearing distinction ADR-0026 and the
    /// panel both insist on ("severity ranks by (fault, VARIANT), never fault identity").
    pub(crate) id: String,
    /// One of [`band`].
    pub(crate) severity: String,
}

/// An arbitration EDGE: two faults that can be set on ONE snapshot, and therefore genuinely
/// compete for prominence. Pinned in the manifest rather than derived independently on each side,
/// so both gates provably evaluate the same universe of edges (a side that derived a smaller
/// universe would pass over fewer comparisons and still report green).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ArbitrationEdge {
    /// The higher-ranked (more urgent) fault id — the one that must win.
    pub(crate) winner: String,
    /// The lower-ranked fault id.
    pub(crate) loser: String,
}

/// A group of fault ids that are MUTUALLY EXCLUSIVE on the wire, so no snapshot can hold two of
/// them and nothing ever arbitrates between them. Recorded explicitly because "these pairs are not
/// tested" must be a declared, reviewable fact rather than an unexplained gap in the edge list.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ExclusiveGroup {
    /// The wire field whose single value picks at most one member.
    pub(crate) wire_field: String,
    pub(crate) members: Vec<String>,
    pub(crate) why: String,
}

/// One per-account utilization case: the inputs both surfaces classify, and the bands they must
/// both produce. The second half of the parity claim — the daemon-payload rank is fleet-wide, this
/// is the per-ACCOUNT severity the issue's AC also names.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct AccountSeverityCase {
    pub(crate) name: String,
    pub(crate) session_pct: Option<u8>,
    pub(crate) weekly_pct: Option<u8>,
    pub(crate) weekly_exhausted: bool,
    /// `None` = no reading, so the cell stays uncoloured on both surfaces (never a fake green).
    pub(crate) session_severity: Option<String>,
    pub(crate) weekly_severity: Option<String>,
}

/// One REFRESH-token expiry case: the wire payload both surfaces classify, and the cell TEXT and
/// tint BAND they must both produce (issues #878/#882/#883/#884, pinned by #886).
///
/// The third half of the parity claim, and the one with the most surface area to drift: the daemon
/// rank is fleet-wide, [`AccountSeverityCase`] is per-account utilization, and this is the
/// per-account FORESIGHT axis. `src/cli.rs`'s `expiry_view` and `StatusPanelFormat.expiryView` are
/// documented as mirroring each other "arm-for-arm, INCLUDING the arm ORDER" — which until now was
/// prose governing runtime, defended on each side by its own hand-written expectations. That is the
/// re-derive-independently shape ADR-0026 exists to refuse, and exactly what produced issue #575.
///
/// Unlike the fault ranks, this pins TEXT as well as band. R-2 is STATE-parity: for expiry the two
/// surfaces deliberately share a vocabulary (`6d21h`, `lapsed`, `—`) rather than each rendering the
/// state in its own idiom, and `StatusPanelFormat.expiryCell` is documented as "byte-identical to
/// `src/cli.rs` `expiry_cell`". A claim of byte-identity is worth asserting as one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ExpiryParityCase {
    pub(crate) name: String,
    /// The observed deadline as an offset in SECONDS from the render instant, or `None` when the
    /// credential carried no deadline at all.
    ///
    /// Relative rather than absolute so both surfaces evaluate the SAME payload against their own
    /// `now` — which is the whole point, since the case set deliberately includes deadlines that
    /// have already passed and the render-time re-check is what must fire on them.
    pub(crate) offset_secs: Option<i64>,
    /// The daemon's cached classification, in its WIRE spelling (`within` / `beyond` / `lapsed` /
    /// `unknown`) — so the manifest names what the daemon actually sends rather than either
    /// language's enum.
    pub(crate) horizon_state: String,
    /// The cell both surfaces must render, byte for byte.
    pub(crate) cell: String,
    /// One of [`band`], or `None` for an uncoloured cell — which is what an unobserved deadline
    /// gets on both surfaces, never a fake calm.
    pub(crate) severity: Option<String>,
}

/// A divergence between the two surfaces that is DELIBERATE — enumerated and justified here rather
/// than asserted away, per issue #768 AC4. Each entry is PINNED by an assertion on both sides, so a
/// documented divergence cannot silently become a different divergence: the register is a gate, not
/// a comment.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct KnownDivergence {
    pub(crate) id: String,
    /// What the CLI does.
    pub(crate) cli: String,
    /// What the panel does.
    pub(crate) panel: String,
    pub(crate) why: String,
    /// Where the decision is ratified — the record a reader should go read, not a restatement.
    pub(crate) record: String,
    /// Whether BOTH gates assert this entry. `false` means the entry is documentation only —
    /// declared here so a reader can tell a pinned divergence from a narrated one at a glance.
    pub(crate) pinned: bool,
}

/// An axis this contract deliberately does NOT cover. An honest scope boundary: a gate that
/// silently omits an axis reads, to the next maintainer, exactly like a gate that covers it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct UncoveredAxis {
    pub(crate) id: String,
    pub(crate) why: String,
}

/// The whole cross-surface contract.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Manifest {
    pub(crate) schema: u32,
    /// A pointer for whoever opens the raw JSON first. Byte-pinned like every other field (the
    /// Rust gate compares the WHOLE serialized manifest), so improving this prose means editing
    /// `manifest_from_source()` and re-emitting — editing the JSON directly reddens the gate.
    pub(crate) about: String,
    pub(crate) daemon_fault_ranks: Vec<FaultRank>,
    /// The `systemic_refresh_source` values BOTH surfaces must rank identically. Provenance picks
    /// the systemic banner's EVIDENCE clause (#787/#813) and — the panel's own resolver comment
    /// says so — "never moves this rank". That was prose governing runtime with nothing asserting
    /// it: both gates previously exercised systemic at ONE provenance each (Rust `Sweep`, Swift the
    /// `nil` default), so neither covered `preflight` and a provenance-GATED rank inversion passed
    /// both. Pinning the set here makes every gate walk every variant.
    pub(crate) systemic_provenance_variants: Vec<String>,
    pub(crate) exclusive_groups: Vec<ExclusiveGroup>,
    pub(crate) arbitration_edges: Vec<ArbitrationEdge>,
    pub(crate) account_severity_cases: Vec<AccountSeverityCase>,
    /// The REFRESH-token expiry cases (issue #886). Added at [`MANIFEST_SCHEMA`] 3.
    pub(crate) expiry_cases: Vec<ExpiryParityCase>,
    pub(crate) known_divergences: Vec<KnownDivergence>,
    pub(crate) uncovered_axes: Vec<UncoveredAxis>,
}

impl Manifest {
    /// Parse the committed bytes. Panics with the re-baseline hint on malformed JSON — a manifest
    /// that will not decode is a broken gate, never a skipped one.
    pub(crate) fn parse(bytes: &str) -> Self {
        serde_json::from_str(bytes).unwrap_or_else(|err| {
            panic!(
                "build/fixtures/cross-surface-severity.json does not decode: {err}{}",
                rebaseline_hint()
            )
        })
    }

    pub(crate) fn ordered_ids(&self) -> Vec<&str> {
        self.daemon_fault_ranks
            .iter()
            .map(|entry| entry.id.as_str())
            .collect()
    }

    /// The contract restricted to the faults a particular snapshot can hold, keeping the manifest's
    /// order and each entry's ORIGINAL rank number (so a failure message says "rank 6", the reader's
    /// coordinate, rather than "rank 3 of this projection"). Panics on an id the manifest does not
    /// pin — a snapshot exercising an unranked fault is a gap in the contract, not in the snapshot.
    pub(crate) fn projection(&self, present: &[&str]) -> Vec<FaultRank> {
        for id in present {
            assert!(
                self.daemon_fault_ranks.iter().any(|entry| entry.id == *id),
                "`{id}` is not pinned in the cross-surface manifest — a fault a snapshot can set \
                 must be ranked, or it is being rendered at a severity nobody agreed on"
            );
        }
        self.daemon_fault_ranks
            .iter()
            .filter(|entry| present.contains(&entry.id.as_str()))
            .cloned()
            .collect()
    }

    /// The manifest's own invariants — checked before it is used as an oracle, because an
    /// oracle nobody validated is just a second opinion. Catches the hand-edit failure modes the
    /// cross-surface comparison itself is blind to: a duplicated id (which would let one fault go
    /// unasserted while every lookup still resolves), a rank column out of step with array order,
    /// an unknown band spelling, an edge naming a fault that does not exist, an edge pointing the
    /// WRONG way, and an edge between two mutually-exclusive faults.
    pub(crate) fn assert_internally_consistent(&self) {
        assert_eq!(
            self.schema, MANIFEST_SCHEMA,
            "cross-surface manifest schema {} != expected {MANIFEST_SCHEMA} — the manifest SHAPE \
             changed; update both consumers, not just this one",
            self.schema
        );
        assert!(
            !self.daemon_fault_ranks.is_empty(),
            "cross-surface manifest has zero ranks — cardinality-zero is an automatic FAIL, never \
             a pass"
        );
        let ids: BTreeSet<&str> = self.ordered_ids().into_iter().collect();
        assert_eq!(
            ids.len(),
            self.daemon_fault_ranks.len(),
            "cross-surface manifest has DUPLICATE fault ids — a duplicate lets every lookup \
             resolve while one fault goes unasserted"
        );
        for (index, entry) in self.daemon_fault_ranks.iter().enumerate() {
            assert_eq!(
                usize::from(entry.rank),
                index + 1,
                "`{}` carries rank {} at array position {} — the rank column and the array order \
                 have drifted apart",
                entry.id,
                entry.rank,
                index + 1
            );
            assert!(
                matches!(
                    entry.severity.as_str(),
                    band::RED | band::YELLOW | band::PLAIN
                ),
                "`{}` has unknown severity band `{}` — expected one of red/yellow/plain",
                entry.id,
                entry.severity
            );
        }
        assert!(
            self.systemic_provenance_variants.len() >= 2,
            "cross-surface manifest pins {} systemic-provenance variant(s) — fewer than two means \
             the provenance axis is not actually being varied, which is how a provenance-gated \
             rank inversion slipped both gates before",
            self.systemic_provenance_variants.len()
        );
        // Exclusive groups must name real faults, and each group must have something to exclude.
        let mut excluded: BTreeSet<(&str, &str)> = BTreeSet::new();
        for group in &self.exclusive_groups {
            assert!(
                group.members.len() >= 2,
                "exclusive group `{}` has fewer than two members — it excludes nothing",
                group.wire_field
            );
            for member in &group.members {
                assert!(
                    ids.contains(member.as_str()),
                    "exclusive group `{}` names unknown fault `{member}`",
                    group.wire_field
                );
            }
            for (i, a) in group.members.iter().enumerate() {
                for b in &group.members[i + 1..] {
                    excluded.insert(unordered(a, b));
                }
            }
        }
        let rank_of = |id: &str| {
            self.daemon_fault_ranks
                .iter()
                .position(|entry| entry.id == id)
                .unwrap_or_else(|| panic!("arbitration edge names unknown fault `{id}`"))
        };
        let mut seen: BTreeSet<(&str, &str)> = BTreeSet::new();
        for edge in &self.arbitration_edges {
            let (winner, loser) = (edge.winner.as_str(), edge.loser.as_str());
            assert!(
                rank_of(winner) < rank_of(loser),
                "arbitration edge `{winner}` > `{loser}` points the WRONG way — `{loser}` ranks \
                 higher (worse) in this same manifest"
            );
            let pair = unordered(winner, loser);
            assert!(
                !excluded.contains(&pair),
                "arbitration edge `{winner}` > `{loser}` is between two MUTUALLY EXCLUSIVE faults \
                 — no snapshot can hold both, so nothing arbitrates between them"
            );
            assert!(
                seen.insert(pair),
                "arbitration edge `{winner}` > `{loser}` is listed twice"
            );
        }
        // …and the edge list is COMPLETE: every co-occurrable pair is an edge. Without this a
        // silently-dropped edge shrinks the tested universe while both gates still report green
        // over whatever remains.
        let expected = expected_edge_count(self.daemon_fault_ranks.len(), excluded.len());
        assert_eq!(
            self.arbitration_edges.len(),
            expected,
            "cross-surface manifest lists {} arbitration edges; {} faults with {} \
             mutually-exclusive pairs make {expected} co-occurrable pairs — the edge list is \
             INCOMPLETE, so some arbitration goes untested",
            self.arbitration_edges.len(),
            self.daemon_fault_ranks.len(),
            excluded.len()
        );
        assert!(
            !self.account_severity_cases.is_empty(),
            "cross-surface manifest has zero account-severity cases — cardinality-zero is an \
             automatic FAIL"
        );
        for case in &self.account_severity_cases {
            // `None` is a legitimate outcome (no reading ⇒ an uncoloured cell on both surfaces,
            // never a fake green), so only the present bands are spell-checked.
            for severity in [&case.session_severity, &case.weekly_severity]
                .into_iter()
                .flatten()
            {
                assert!(
                    matches!(
                        severity.as_str(),
                        band::RED | band::YELLOW | band::GREEN | band::PLAIN
                    ),
                    "account case `{}` has unknown band `{severity}`",
                    case.name
                );
            }
        }
        assert!(
            !self.expiry_cases.is_empty(),
            "cross-surface manifest has zero expiry cases — cardinality-zero is an automatic FAIL"
        );
        // The four wire states must ALL be exercised. A case set that quietly lost one — most
        // consequentially `unknown`, the absent-`refreshTokenExpiresAt` verdict — would still pass
        // every per-case comparison on both sides while covering less, which is exactly the
        // degenerate-subject pass this manifest's other cardinality guards are here to refuse.
        for state in ["within", "beyond", "lapsed", "unknown"] {
            assert!(
                self.expiry_cases
                    .iter()
                    .any(|case| case.horizon_state == state),
                "cross-surface manifest pins no `{state}` expiry case — all four wire states must \
                 be walked, or a surface can diverge on the missing one unobserved"
            );
        }
        for case in &self.expiry_cases {
            assert!(
                matches!(
                    case.horizon_state.as_str(),
                    "within" | "beyond" | "lapsed" | "unknown"
                ),
                "expiry case `{}` has unknown horizon state `{}` — the manifest names the WIRE \
                 spelling, not either language's enum",
                case.name,
                case.horizon_state
            );
            // `None` is legitimate (an unobserved deadline is uncoloured on both surfaces); only a
            // present band is spell-checked. Two spellings are deliberately absent. `GREEN`,
            // because no expiry verdict is ever reassuring-green — the calmest thing this axis can
            // say is "far off", which is `DIM`. And `PLAIN`, because uncoloured is encoded here as
            // `severity: None` rather than as a band token; admitting `"plain"` too would give
            // "uncoloured" two spellings, and one of them would go untested.
            if let Some(severity) = &case.severity {
                assert!(
                    matches!(severity.as_str(), band::RED | band::YELLOW | band::DIM),
                    "expiry case `{}` has unknown band `{severity}`",
                    case.name
                );
            }
            // An unobserved deadline must never carry a band at all, and must render the gap. The
            // #137 invariant, asserted on the ORACLE itself rather than only on the two surfaces
            // that read it — a manifest that pinned `unknown` to a reassuring cell would make both
            // gates enforce the wrong thing in perfect agreement.
            if case.horizon_state == "unknown" {
                assert_eq!(
                    case.severity, None,
                    "expiry case `{}` tints an UNOBSERVED deadline — absence of a reading is not a \
                     verdict, and a colour would read as one",
                    case.name
                );
                assert_eq!(
                    case.cell, "—",
                    "expiry case `{}` narrates a deadline it never observed",
                    case.name
                );
            }
        }
        assert!(
            !self.known_divergences.is_empty(),
            "cross-surface manifest enumerates zero known divergences — issue #768 AC4 requires \
             the legitimate ones be enumerated, and there is at least one (the blind-DEGRADED tint)"
        );
    }
}

/// How many unordered pairs of `n` faults can actually co-occur, given `excluded_pairs` of them
/// that are mutually exclusive on the wire.
fn expected_edge_count(n: usize, excluded_pairs: usize) -> usize {
    n * (n - 1) / 2 - excluded_pairs
}

fn unordered<'a>(a: &'a str, b: &'a str) -> (&'a str, &'a str) {
    if a <= b {
        (a, b)
    } else {
        (b, a)
    }
}

/// One fault as some surface actually ranks and renders it — the OBSERVED side of every
/// comparison. Both Rust observers (the `DaemonPayloadFault` declaration, and the text
/// `render_status` prints) produce this shape, so one predicate judges both.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ObservedFault {
    pub(crate) id: String,
    pub(crate) severity: String,
}

impl ObservedFault {
    pub(crate) fn new(id: &str, severity: &str) -> Self {
        Self {
            id: id.to_owned(),
            severity: severity.to_owned(),
        }
    }
}

/// THE predicate. Compare an observed rank sequence against the manifest and return every way they
/// disagree, in reader-ready prose.
///
/// This is the single judgement both the real gates and the CONSTRAINT-A canaries route through —
/// a canary that bypassed it would prove nothing about the gate that actually runs. It is
/// deliberately sensitive on THREE independent axes, because a predicate blind to any one of them
/// passes a real defect:
///
/// * **cardinality** — a short or empty observation is a FAIL, never a vacuous pass;
/// * **order** — position `i` must name the same fault (this is the #575 defect exactly: the right
///   set of faults, ranked in the opposite order);
/// * **band** — the severity at each position must match (a surface can keep perfect order and
///   still paint the wrong urgency, which an order-only predicate would wave through).
pub(crate) fn rank_divergences(expected: &[FaultRank], observed: &[ObservedFault]) -> Vec<String> {
    let mut findings = Vec::new();
    if observed.is_empty() {
        findings.push(
            "the surface reported ZERO ranked faults — cardinality-zero is an automatic FAIL, \
             never a pass (the observation is degenerate, so a green here would be evidence of \
             nothing)"
                .to_owned(),
        );
        return findings;
    }
    if expected.len() != observed.len() {
        findings.push(format!(
            "rank COUNT diverges: the manifest pins {} fault(s), the surface reported {} — \
             expected [{}], observed [{}]",
            expected.len(),
            observed.len(),
            expected
                .iter()
                .map(|e| e.id.as_str())
                .collect::<Vec<_>>()
                .join(", "),
            observed
                .iter()
                .map(|o| o.id.as_str())
                .collect::<Vec<_>>()
                .join(", "),
        ));
    }
    for (index, expect) in expected.iter().enumerate() {
        let Some(actual) = observed.get(index) else {
            findings.push(format!(
                "rank {} (`{}`) is MISSING from the surface's ordering",
                expect.rank, expect.id
            ));
            continue;
        };
        if expect.id != actual.id {
            findings.push(format!(
                "ORDER diverges at rank {}: the manifest ranks `{}` there, the surface ranks \
                 `{}` — this is the issue #575 shape exactly (the same faults, ranked \
                 differently), and ADR-0026 makes it a defect, not a style difference",
                expect.rank, expect.id, actual.id
            ));
        }
        if expect.severity != actual.severity {
            findings.push(format!(
                "SEVERITY BAND diverges for `{}` (rank {}): the manifest pins `{}`, the surface \
                 renders `{}` — the colour/glyph VOCABULARY may differ per medium (R-2), the \
                 BAND may not",
                actual.id, expect.rank, expect.severity, actual.severity
            ));
        }
    }
    for extra in observed.iter().skip(expected.len()) {
        findings.push(format!(
            "the surface ranks `{}`, which the manifest does not pin at all — a new daemon-payload \
             fault must be added to the cross-surface rank (ADR-0026: \"any FOURTH fault inherits \
             this\"), not ranked locally",
            extra.id
        ));
    }
    findings
}

/// One named corruption of an OBSERVED rank sequence — the vocabulary [`assert_canary`] proves the
/// gate against. Mutating the OBSERVATION (what a surface reports), not the expectation, is the
/// direction that matters: it simulates a surface actually drifting.
pub(crate) struct Mutation {
    pub(crate) name: &'static str,
    /// Corrupt the observation, or return `None` when it has no such shape to corrupt.
    pub(crate) apply: fn(&[ObservedFault]) -> Option<Vec<ObservedFault>>,
}

/// The divergence classes this gate must be able to catch — each one a way a surface could drift
/// that a WEAKER predicate would wave through.
///
/// The list is chosen adversarially rather than for coverage's sake. `invert-the-worst-and-a-
/// milder-fault` is the literal issue #575 defect (a set-comparison or a per-fault-only band check
/// passes it). `flip-a-severity-band` keeps the order perfect and changes only urgency (an
/// order-only predicate passes it). `drop-a-rank` and `duplicate-a-rank` are the cardinality
/// classes a positional walk that stopped at the shorter list would pass. A predicate that survives
/// all four is sensitive on all three axes it claims.
pub(crate) const RANK_MUTATIONS: &[Mutation] = &[
    // Issue #575, reproduced: the least-blocking fault jumps above the most-blocking one. The
    // faults are the same, every string is the same, only the ORDER moved.
    Mutation {
        name: "invert-the-worst-and-a-milder-fault",
        apply: |observed| {
            (observed.len() >= 2).then(|| {
                let mut mutated = observed.to_vec();
                let last = mutated.len() - 1;
                mutated.swap(0, last);
                mutated
            })
        },
    },
    // Perfect order, wrong urgency: one fault is painted a band louder (or, for an already-red
    // one, calmer) than the contract. Invisible to any order-only comparison.
    Mutation {
        name: "flip-a-severity-band",
        apply: |observed| {
            let mut mutated = observed.to_vec();
            let at = mutated.iter().position(|f| f.severity != band::RED)?;
            mutated[at].severity = band::RED.to_owned();
            Some(mutated)
        },
    },
    // A fault stops being ranked at all — the shape a surface takes when a renderer is deleted or
    // its guard inverted.
    Mutation {
        name: "drop-a-rank",
        apply: |observed| {
            (observed.len() >= 2).then(|| {
                let mut mutated = observed.to_vec();
                mutated.remove(mutated.len() / 2);
                mutated
            })
        },
    },
    // A fault is ranked twice — the shape a duplicated render site takes.
    Mutation {
        name: "duplicate-a-rank",
        apply: |observed| {
            (!observed.is_empty()).then(|| {
                let mut mutated = observed.to_vec();
                let at = mutated.len() / 2;
                mutated.insert(at, mutated[at].clone());
                mutated
            })
        },
    },
];

/// The CONSTRAINT-A canary: prove this gate can FAIL, by MUTATION rather than by inspection.
///
/// Runs the clean observation through [`rank_divergences`] first — it must be silent, or the gate
/// is red before any mutation and the canary below would prove nothing (an always-failing predicate
/// "catches" every mutation too). Then every mutation in [`RANK_MUTATIONS`] must be caught. A
/// mutation that no longer APPLIES is a failure in its own right: a canary that quietly stopped
/// perturbing anything is indistinguishable, from the outside, from one that passes.
pub(crate) fn assert_canary(surface: &str, expected: &[FaultRank], observed: &[ObservedFault]) {
    let clean = rank_divergences(expected, observed);
    assert!(
        clean.is_empty(),
        "{surface}: the UNMUTATED observation already diverges — the canary below cannot \
         distinguish a working gate from a permanently-red one:\n  {}",
        clean.join("\n  ")
    );
    for mutation in RANK_MUTATIONS {
        let mutated = (mutation.apply)(observed).unwrap_or_else(|| {
            panic!(
                "{surface}: mutation `{}` did not APPLY to the observation ({} fault(s)) — a \
                 canary that perturbs nothing proves nothing",
                mutation.name,
                observed.len()
            )
        });
        assert_ne!(
            mutated, observed,
            "{surface}: mutation `{}` returned the observation UNCHANGED — it is a no-op, so the \
             `caught` result below would be vacuous",
            mutation.name
        );
        let findings = rank_divergences(expected, &mutated);
        assert!(
            !findings.is_empty(),
            "{surface}: mutation `{}` was NOT caught by `rank_divergences` — the gate is blind to \
             this divergence class, so a green from it is not evidence.\n  mutated ordering: [{}]",
            mutation.name,
            mutated
                .iter()
                .map(|f| format!("{}:{}", f.id, f.severity))
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
}

/// The re-baseline instructions, appended to every manifest failure so the operator never has to go
/// hunting for the command.
pub(crate) fn rebaseline_hint() -> String {
    "\n\nIf this change to the cross-surface severity contract is INTENTIONAL, re-baseline \
     deliberately:\n\
     \x20   cargo test -- --ignored emit_cross_surface_severity_manifest\n\
     then move the OTHER surface to match — the panel's rank lives in \
     `apps/menubar/Sources/StatusPanelFormat.swift` (`daemonFaultBanner`), and \
     `apps/menubar/Tests/CrossSurfaceSeverityParityTests.swift` will stay RED until it does. That \
     is the point: ADR-0026 makes a one-sided rank change a defect, and re-emitting this manifest \
     is the act that hands the change to the other surface."
        .to_owned()
}

/// Serialize a manifest to the committed on-disk form: pretty-printed with a trailing newline, so
/// a re-baseline shows as a readable diff rather than one reflowed line.
pub(crate) fn to_committed_bytes(manifest: &Manifest) -> String {
    let mut json = serde_json::to_string_pretty(manifest).expect("manifest serializes");
    json.push('\n');
    json
}

/// Write the manifest to [`manifest_path`] — the body of the `#[ignore]`d emitter, and the ONLY
/// way it is ever written. No auto-bless-on-missing anywhere in this module.
pub(crate) fn emit(manifest: &Manifest) {
    let path = manifest_path();
    std::fs::create_dir_all(path.parent().expect("fixtures dir has a parent"))
        .expect("create build/fixtures");
    std::fs::write(&path, to_committed_bytes(manifest))
        .unwrap_or_else(|err| panic!("write {}: {err}", path.display()));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ranks() -> Vec<FaultRank> {
        vec![
            FaultRank {
                rank: 1,
                id: "a".to_owned(),
                severity: band::RED.to_owned(),
            },
            FaultRank {
                rank: 2,
                id: "b".to_owned(),
                severity: band::YELLOW.to_owned(),
            },
            FaultRank {
                rank: 3,
                id: "c".to_owned(),
                severity: band::PLAIN.to_owned(),
            },
        ]
    }

    fn observed() -> Vec<ObservedFault> {
        vec![
            ObservedFault::new("a", band::RED),
            ObservedFault::new("b", band::YELLOW),
            ObservedFault::new("c", band::PLAIN),
        ]
    }

    #[test]
    fn a_conforming_observation_produces_no_findings() {
        assert!(rank_divergences(&ranks(), &observed()).is_empty());
    }

    #[test]
    fn an_empty_observation_is_a_failure_not_a_vacuous_pass() {
        // The degenerate-subject guard: a gate that "passes" over nothing is evidence of nothing.
        let findings = rank_divergences(&ranks(), &[]);
        assert!(
            findings.iter().any(|f| f.contains("ZERO ranked faults")),
            "{findings:?}"
        );
    }

    #[test]
    fn an_order_inversion_is_caught_even_though_the_fault_set_is_identical() {
        // Issue #575's shape: same faults, same bands, opposite order. A set comparison passes it.
        let mut mutated = observed();
        mutated.swap(0, 2);
        let findings = rank_divergences(&ranks(), &mutated);
        assert!(
            findings.iter().any(|f| f.contains("ORDER diverges")),
            "{findings:?}"
        );
    }

    #[test]
    fn a_band_flip_is_caught_even_though_the_order_is_perfect() {
        // The complement: an order-only predicate is structurally blind to this.
        let mut mutated = observed();
        mutated[1].severity = band::RED.to_owned();
        let findings = rank_divergences(&ranks(), &mutated);
        assert!(
            findings
                .iter()
                .any(|f| f.contains("SEVERITY BAND diverges")),
            "{findings:?}"
        );
        assert!(
            !findings.iter().any(|f| f.contains("ORDER diverges")),
            "the order was untouched, so no order finding should fire: {findings:?}"
        );
    }

    #[test]
    fn the_canary_vocabulary_is_wholly_caught_by_the_predicate() {
        // The meta-canary: assert_canary is itself exercised over a synthetic contract, so a
        // mutation that stops applying (or stops being caught) reddens here too, not only in the
        // real gates.
        assert_canary("synthetic", &ranks(), &observed());
    }

    #[test]
    #[should_panic(expected = "did not APPLY")]
    fn a_mutation_that_cannot_apply_fails_the_canary_rather_than_passing_it() {
        // A one-fault observation has no two ranks to swap. The canary must treat that as a
        // failure — a perturbation that perturbs nothing proves nothing.
        assert_canary(
            "synthetic-single",
            &ranks()[..1],
            &[ObservedFault::new("a", band::RED)],
        );
    }

    #[test]
    #[should_panic(expected = "already diverges")]
    fn a_permanently_red_predicate_fails_the_canary_rather_than_passing_it() {
        // If the clean observation already diverges, every mutation is "caught" trivially. The
        // canary must refuse to draw evidence from that.
        assert_canary("synthetic-broken", &ranks(), &observed()[..2]);
    }

    #[test]
    fn the_committed_manifest_is_internally_consistent() {
        // The oracle is validated BEFORE any surface is judged against it. An oracle nobody
        // checked is just a second opinion.
        committed_manifest();
    }
}
