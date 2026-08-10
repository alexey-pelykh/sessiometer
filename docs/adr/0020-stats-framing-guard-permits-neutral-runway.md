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

**Amended 2026-08-10 (#1123) — the non-acquisitive REMEDY DIRECTIVE is permitted on operator
advisories; this ADR's Context banned "imperatives" more broadly than its own Decision did.**
The Decision below bans a specific imperative — the *acquisitive* purchase prompt (`buy` / `add`
/ `upgrade` / `purchase` / `need`) — and the vocabulary that carries it never proscribed the
imperative MOOD. The Context paragraph nonetheless generalised that to "no imperative", which is
strictly broader than the rule it introduces, and read as a ban on any directive at all.

Issue #1123 took the `status` operator advisories inside the same firewall and had to settle
what the over-broad sentence left ambiguous: is `degraded — run 'sessiometer poke'` permitted?
**It is.** The discriminator is the OBJECT of the imperative, not its mood: an imperative whose
object is acquisition is a purchase call and stays banned, while one whose object is a free,
local, mechanical operation on the tool's own state is a **remedy** — a fact about what fixes
the state. Measured, `run 'sessiometer poke'` costs zero central tokens, so no vocabulary
changed and none was added.

Two boundaries on that, because this amendment is narrower than it may read:

- **The surface class is new, not inherited.** This ADR governs `stats`' own summary bands, and
  a band has no remedy to direct — it reports a period, it does not diagnose a repairable
  account. The remedy directive is permitted on the **operator-advisory** surfaces issue #1123
  brought in (`status`' AUTH-cell cue and the `[refresh]` advisory). **`stats`' bands are
  unchanged by this amendment**, and nothing here permits a directive there.
- **This is not an extension of the runway permit.** The permit below is explicitly for a fact
  stated "as an observation, not advice", and a remedy directive *is* advice — so it cannot be
  justified by analogy to a head-room number. Its authority is the separate, older and
  independently tested requirement that this tool's operator guidance be **clear and
  FOLLOWABLE** (issues #376 / #397): `src/error.rs`'s `NoManagedService` and
  `UnmanagedDaemonNoRestart` doc comments and the
  `unmanaged_daemon_no_restart_guides_the_operator_with_a_followable_action` test, `src/cli.rs`'s
  "name the followable stop first", `src/log.rs`'s "the refusal must name the followable
  alternatives". A tool required to name a followable action cannot simultaneously be forbidden
  from naming one. The #160 firewall governs whether the tool editorialises; #376 / #397 governs
  whether its guidance is actionable, and they are orthogonal.

## Context

The `stats` verb foots its human views with a neutral summary band (issue #160) and
carries a CI **framing guard** — a central banned vocabulary (`BANNED_TOKENS`) plus a
scanner (`scan_banned`) in `src/framing_vocabulary.rs`, asserted by the
`summary_render_carries_no_banned_token_but_the_guard_bites_on_injection` test — that keeps
every rendered surface descriptive: no value judgement, no **acquisitive** imperative, no
recommendation, no projection framing. The guard exists to stop `stats` drifting into an
**acquisitive / purchase-timeline** framing.

> This sentence read "no imperative" until 2026-08-10 — broader than the Decision it
> introduces, which bans the acquisitive purchase prompt specifically. See § Status →
> Amended 2026-08-10 (#1123), which settles the non-acquisitive **remedy directive** as
> permitted on operator advisories, and leaves `stats`' own bands unchanged.

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

Two mechanical facts carry this in `src/framing_vocabulary.rs`:

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

- Code: `src/framing_vocabulary.rs` — `BANNED_TOKENS`, `BANNED_PHRASES`, `scan_banned`.
  The vocabulary and scanner lived in `src/stats.rs` when this ADR was accepted; issue #918
  hoisted them so `src/cli.rs` could scan `--help` against the same list. That MOVE changed no
  boundary — the lists kept their content and ordering, and `--help` scans a subset derived from
  them rather than a second copy. The boundary itself was later amended: issue #1123 settled the
  non-acquisitive remedy directive as permitted on operator advisories (§ Status → Amended
  2026-08-10), which is a change to what this ADR settles rather than to where the code lives.
  The central lists remain untouched by it — and by issue #1139, which added a fifth audience
  (see below) without amending this ADR.
- Code: `src/stats.rs` — the assertions stayed put:
  `framing_guard_permits_neutral_runway_but_bans_the_acquisitive_call` and
  `summary_render_carries_no_banned_token_but_the_guard_bites_on_injection`.
- Issues: #542 (the 2026-07-15 amendment) · #541 (runway umbrella) · #160 (the framing guard) ·
  #918 (the vocabulary's relocation) · **#1123 (the 2026-08-10 amendment — the non-acquisitive
  remedy directive, permitted on operator advisories)** · #376 / #397 (the clear-and-FOLLOWABLE
  operator-guidance requirement that amendment rests on) · #543 / #544 (the runway surfaces) ·
  #158 / #159 (`--json` / charts) · #1139 (see below) · #1151 (the one violation it found).

### Issue #1139 applied this ADR without amending it — deliberately

Issue #1139 took `Error`'s authored `#[error(...)]` templates inside the firewall as a fifth
audience, and had to judge three tokens the shipped messages already spend. It recorded the
verdicts in `src/error.rs`'s `ERROR_PROSE_LEDGER` and changed **no boundary here**, so this ADR's
status cell in `README.md` is unchanged. That is a decision worth being explicit about, because
#1123 set the opposite precedent and a reader may reasonably expect a second amendment:

- **`add`**, in `ActiveAccountUnresolved`'s "add it to the rotation", is permitted by the 2026-08-10
  amendment **as written**, not by an extension of it. That amendment's discriminator is stated
  generally — the OBJECT of the imperative, a free, local, mechanical operation on the tool's own
  state versus an acquisition — and its own § Status bounds it against `stats`' bands for the
  reason that "a band has no remedy to direct". An error message diagnosing a repairable state is
  the opposite case, and it is the surface the amendment already cites for its authority: the
  clear-and-FOLLOWABLE requirement of #376 / #397, whose named examples are `src/error.rs`'s
  `NoManagedService` and `UnmanagedDaemonNoRestart`. Applying the rule to the surface its authority
  came from does not move it.
- **`must`**, in `ConfigTargetMaxSessionAboveTrigger` and `SharedCredentialMutated`, is a CONSTRAINT
  STATEMENT rather than the recommendation framing the group bans: the modal's subject is a config
  value or this tool's own invariant, never the operator. That is a reading of the existing group,
  not a new permit — nothing about the acquisitive call, the value judgements or the projection
  words changed.
- **`healthy`**, in `ActiveAccountUnresolved`, did **not** survive. It is a value judgement, the
  group this firewall exists for, and the defence that it names the machine-checkable
  `CredentialHealth::Healthy` is false on that code path. It is recorded as a violation under
  issue #1151 rather than excused — so the boundary held here rather than moving.

The one thing #1139 did add is a mechanism, not a boundary: this audience gets **no exemption set**
and scans the central lists whole, because `Error` is dozens of independent messages rather than one
surface, so its carve-outs are per-(variant, token). See `src/framing_vocabulary.rs`'s module doc
§ "The fifth audience has no exemption set".
