# Findings Notes

This directory holds **findings notes** — the evidence artifacts produced by
**spike** work items: a measurement, a characterization, or a probe result that
*feeds* a decision but is not itself one.

A findings note is the sibling of an [ADR](../adr/README.md), split by role:

- An **ADR** records a load-bearing **decision** (in force; immutable once
  accepted).
- A **findings note** records the **evidence** a spike gathered — real data,
  its measured/modeled boundary, and what it implies for the fixes — at a
  point in time. It is a snapshot, not a standing rule, and may be superseded
  by a later capture.

Keeping them apart stops spike evidence from being mistaken for a settled
decision, and gives the fix items a stable citation for *why*.

## Index

| Finding | Title | Spike | Umbrella |
|---------|-------|-------|----------|
| [0465](0465-multi-session-rotation-interference.md) | Multi-session refresh-rotation interference on the shared credential | #465 | #463 |
| [0476](0476-keep-warm-scrub-risk-tradeoff.md) | The scrub-risk cost of gating proactive keep-warm | #476 | #463 |
| [0777](0777-manual-restart-under-conditional-keepalive.md) | Does a manual `Restart…` mean anything under conditional `KeepAlive`? | #777 | — |
| [0950](0950-help-on-disabled-button.md) | Does `.help()` surface a tooltip on a `.disabled()` Button? | #950 | — |
| [0953](0953-help-nesting-inside-a-row-button.md) | Does a `.help()` on a child inside a row `Button` surface on hover? (**not established**) | #953 | — |
| [0981](0981-reachability-predicate.md) | Can a source-as-data predicate tell a variable-sourced size-class injection from a literal one? (**yes**, within three bounds, after four traps) | #981 | #748 |
| [1177](1177-scrollview-raster-route.md) | Can anything rasterize the panel's scroll boundary, and is it worth switching to? (**yes** — route 1 taken, execution deferred to #1261) | #1177 | — |

## Conventions

- **Filename**: `NNNN-kebab-case-title.md`, where `NNNN` is the originating
  spike **issue number** (findings trace to the issue that commissioned them,
  unlike ADRs which are sequentially numbered).
- **Verdict first**: lead with the answer, then the evidence — house style
  (`build/version-compat.md`'s per-issue findings).
- **Measured vs modeled**: state the data-availability boundary explicitly;
  never present a modeled expectation as a measured value. Mark capture-pending
  items so a reader knows what real data still owes.
- **Public-safety** (per umbrella #463): characterize Sessiometer's own
  behavior from its own logs; cite observable Claude Code behavior from the
  relevant record rather than re-deriving it. Redact operator-chosen labels.
- **Provenance**: cite the data source, the analysis method, and cross-checks
  (ADRs, sibling issues), matching this repo's house style.
