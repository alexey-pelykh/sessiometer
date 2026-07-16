---
type: architecture-decision-record
number: 20
title: "The stats framing guard permits a neutral runway, bans the acquisitive call"
date: 2026-07-15
status: accepted
decision_makers: [Oleksii PELYKH (maintainer)]
---

# ADR-0020: The `stats` framing guard permits a neutral runway, bans the acquisitive call

## Status

**Accepted** — 2026-07-15. Records the design behind the **issue #542** amendment to the
`stats` framing guard (issue #160), so a contributor does not re-litigate why a neutrally
framed velocity + runway readout is permitted while an acquisitive purchase prompt stays
banned. Prerequisite for the **issue #541** runway surfaces (issues #543 / #544).

## Context

The `stats` verb foots its human views with a neutral summary band (issue #160) and
carries a CI **framing guard** — a central banned vocabulary (`BANNED_TOKENS`) plus a
scanner (`scan_banned`) in `src/stats.rs`, asserted by the
`summary_render_carries_no_banned_token_but_the_guard_bites_on_injection` test — that keeps
every rendered surface descriptive: no value judgement, no imperative, no recommendation,
no projection framing. The guard exists to stop `stats` drifting into an **acquisitive /
purchase-timeline** framing.

Issue #541 needs `stats` to surface a **runway** — a per-account and fleet head-room
("this account lasts ~Xh", "accounts last ~X days") plus a `%/min` velocity. A runway is
forward-looking, and the guard as originally worded ("no projections / forecasts") reads as
forbidding it. The owner ruled to **amend the guard to permit a neutrally framed runway**
rather than drop the runway.

## Decision

The guard bans the **framing**, not the **fact**. The permit/ban boundary is:

- **Permit** — descriptive head-room phrased as an observation: a `%/min` velocity, an
  approximate time-to-trigger, days-of-runway "at current rate", and the bare "runs out in
  ~Xh" fact. These use none of the banned vocabulary and read as an observation, not advice.
- **Still ban** — the acquisitive **call to acquire**: the imperative purchase prompt
  (`buy` / `add` / `upgrade` / `purchase` / `need`), the imperative-free purchase phrase
  (`top up` / `get more`), the value judgements, the recommendations, and the alarmist
  projection *words* (`forecast` / `imminent` / `soon`). The intent-leak concern is a
  purchase prompt, never a head-room number.

Two mechanical facts carry this in `src/stats.rs`:

1. The token list is unchanged on the permit side — analysis confirmed no neutral
   runway/velocity word (`runway`, `velocity`, `rate`, `%/min`, `to trigger`, `at current
   rate`, `runs out`) collides with it, so the neutral runway already passes. The banned
   vocabulary bans editorialising *words*, and a neutral fact uses none.
2. A short `BANNED_PHRASES` list plus an adjacent-word scan in `scan_banned` closes the one
   real gap: an imperative-free purchase call ("you'll run out — top up", "get more") that a
   single-token scan misses. Matched on word boundaries, not raw substrings, so a neutral
   render never false-trips.

Both sides are pinned by the
`framing_guard_permits_neutral_runway_but_bans_the_acquisitive_call` fixture test: neutral
runway strings pass, acquisitive phrasings fail, and the SAME "runs out" head-room passes as
an observation but fails the instant a purchase call is appended.

## Alternatives considered

1. **Drop the runway (keep the guard verbatim).** Rejected by the owner: #541's head-room is
   worth surfacing, and the guard's real target is a purchase prompt, not a forward-looking
   number.
2. **Relax the guard broadly** (remove the projection words). Rejected: the alarmist
   projection *words* (`imminent`, `soon`, `forecast`) are framing, not facts — keeping them
   banned reinforces "state a number, not an alarm". A neutral runway is numeric, so it loses
   nothing.
3. **Substring phrase matching** for the acquisitive calls. Rejected: a raw substring test
   over-trips (`laptop update` contains `top up`); the adjacent-word test is word-boundary
   safe.

## Consequences

### Positive

- Prerequisite unblocked: #541's per-account (#543) and fleet (#544) runway surfaces can
  render a neutral velocity/runway without tripping the guard.
- The boundary is durable and executable — recorded here and pinned by a fixture — so it is
  not re-litigated per PR.
- The ban side is strengthened: imperative-free purchase calls (`top up`, `get more`) that
  previously slipped through are now caught.

### Negative / trade-offs

- The guard now carries two mechanisms (token + adjacent-phrase), a small maintenance
  surface increase over the single-list original.
- A purchase call that uses neither a banned imperative nor a banned phrase (some novel
  synonym) can still pass; the guard covers the known acquisitive vocabulary, not every
  paraphrase. The runway surfaces (#543 / #544) remain responsible for neutral wording, with
  this guard as the regression net.

## Related

- Code: `src/stats.rs` — `BANNED_TOKENS`, `BANNED_PHRASES`, `scan_banned`, and the
  `framing_guard_permits_neutral_runway_but_bans_the_acquisitive_call` test.
- Issues: #542 (this amendment) · #541 (runway umbrella) · #160 (the framing guard) ·
  #543 / #544 (the runway surfaces) · #158 / #159 (`--json` / charts).
