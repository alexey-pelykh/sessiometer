---
type: architecture-decision-record
number: 33
title: "The weekly-saturated / session-low peer stays UNKNOWN; rotation state is not carried into the census"
date: 2026-08-10
status: accepted
decision_makers: [Oleksii PELYKH (maintainer)]
---

# ADR-0033: The weekly-saturated / session-low peer stays UNKNOWN; rotation state is not carried into the census

## Status

**Accepted** — 2026-08-10. Settles the open question issue **#1097** raised against **#1030**'s
census anchoring: *is there a reading-carried guarantee for a weekly-saturated peer's **session**
dimension?* The answer is **no**, and the reason is a measurement rather than a preference, which is
why it is recorded here instead of as a note on the spec. Companion to **ADR-0019** (the widened
cadence that creates the blind spot in the first place).

## Context

The all-accounts-high census intersects every rostered account's validity window. Issue #1030
anchored a **high** reading to its carried `session_resets_at`, because session utilisation only
climbs within a window: a reading at/above the water is a statement that stays true until the reset.
A **low** reading carries no such guarantee, so it is not extended. That asymmetry is
`high_windows` in `src/usage_stats.rs`, and it is correct.

The rationale ratified alongside it was not. Three documents justified the asymmetry with *"the low
peer is the one IN rotation, polled at the normal cadence — its coverage was never the problem."*
Measured against `build/fixtures/capacity-replay-corpus.tsv` — the frozen 48 h (172 800 s), 6-account,
1734-row slice — that sentence is false for half the roster:

| account | readings | session peak | weekly | median poll gap | coverage |
|---|---|---|---|---|---|
| `a4` | 44 | 0.03 | 0.97–0.98 | 3948 s | 7.6 % |
| `a5` | 44 | 0.15 | 0.97–1.00 | 3964 s | 7.6 % |
| `a6` | 43 | 0.00 | 0.97 | 3975 s | 7.5 % |

These peers are session-**low** and polled on the **widened** `exhausted_poll_secs` cadence at the
same time, held out of rotation by their **weekly** dimension. A low peer can therefore be exactly as
sparsely polled as a saturated one, and this population is what pins the corpus's census at UNKNOWN:
joint coverage is **0.00 %** before #1030 and **0.00 %** after it.

So the question is whether the asymmetry can be *narrowed* — not relaxed wholesale, but extended to
this one class on a guarantee of its own. #1097 stated the candidate: an account out of rotation
because its weekly window is exhausted is not being spent against, so its session utilisation cannot
climb; a low reading would then be safe to extend for that class specifically. #1097 also stated the
apparent blocker: the sample does not carry rotation state.

Both halves were tested. The blocker turns out not to be the binding one.

## Decision

**The weekly-saturated / session-low class stays UNKNOWN. Rotation state is not carried into the
census, and no low reading is anchored.** The asymmetry in `high_windows` is unchanged.

The census reports what it observed and refuses to interpolate across what it did not. On a
weekly-drained roster it therefore reports UNKNOWN, and
`on_the_replay_corpus_the_utilisation_census_is_unknown_because_the_drain_was_weekly`
(`src/usage_stats.rs`) is the standing record of why, with the falsifier below asserted against the
frozen corpus so the refutation lives in the fixture rather than in prose.

Rule 3's rationale is corrected in place at all three sites that carried it — the spec
(`docs/specs/census-validity-anchoring.feature.md`), the design
(`docs/design/stats-honesty-cross-surface-solution-design.md` § D-I) and the requirement
(`docs/requirements/stats-honesty-cross-surface.md` § R-18). The asymmetry's sound justification is
the **direction the guarantee runs in** — utilisation climbs within a window, so only a high reading
carries a statement that survives to its reset — never the peer's rotation state. R-18's normative
text is untouched; only the prose arguing for it changed.

### Why the guarantee does not exist

**The candidate is falsified on this repo's own frozen corpus.** Restricting to consecutive readings
that are weekly-pinned at both endpoints — at or above the daemon's own rotation line,
`weekly_ceiling − swap::WEEKLY_TAIL_MARGIN`, which is **0.97** at the shipped defaults — and carry
the **same** `session_resets_at`, so no reset intervened and the pair provably sits inside one
session window: **17 of 274** such pairs show session utilisation *climbing*. Both counts are stated
against that line and the frozen fixture; the replay test derives the line from
`config::Tunables::default()` rather than a literal, so a change to the ceiling moves the basis and
the test reports the new counts in its failure message. The sharpest case is on the very population
in question:

> `a5`, corpus offset 116 018 → 120 127: session **0.05 → 0.15** across a **4109 s** gap, while
> weekly went 0.99 → 1.00 and `session_resets_at` did not move.

A 4109 s gap is the widened cadence, so `a5` was demonstrably not being polled at `poll_secs` — it
was in exactly the state the candidate guarantee describes, and its session utilisation tripled
anyway. Two more of the 17 are `a5` in the same regime; the rest are `a1` and `a2` at weekly 0.97–1.00.

The mechanism is **not determinable** from this corpus, which carries no active-account or host
attribution, and it does not need to be: "the daemon will not rotate onto this account" is not
"nothing is spending against this account". The daemon's own environment already supplies candidates
— the canonical credential is shared with external `claude` sessions the daemon neither controls nor
observes (finding **#465**), and cross-machine coordination is an explicit non-goal, stated as
crate doctrine in `src/swap.rs` § Single-machine-sync boundary (**#613**): *"Nothing coordinates
ACROSS machines — Sessiometer has no shared backend … **Co-consumption.** Both machines bill one
account's session/weekly quota at the same time."* That clause describes this observation exactly.
What is settled is the observation, and the observation contradicts the premise.

**The headroom is not comfortable either.** A low anchor asserts *"stayed under the 0.80 water"* for
every second of the extension. On this corpus the highest session reading that is both weekly-pinned
and followed by a widened-cadence gap is `a1` at **0.66** (offset 53 806, 4019 s gap, weekly 0.97),
and the largest same-window climb across such a gap is the **+0.10** above. Those two do not co-occur
on one account in these 48 h, so the corpus holds no instance where the assertion would be outright
**false** — 0.66 + 0.10 = 0.76 against a water of 0.80. That is the honest statement of the result:
the corpus refutes the *reason* offered for believing the extension safe, and leaves it four
hundredths short of refuting the extension itself. Neither margin is a guarantee.

### Carriability was not the blocker

#1097 supposed the class could not be addressed because the sample does not carry rotation state.
That is true of the sample and false of the system, so it is recorded rather than left implied — a
future reader must not reopen this on the belief that a new field would settle it.

The daemon computes the verdict at poll time. `note_exhausted_poll` (`src/daemon.rs`) derives
`out_of_rotation = active != Some(i) && (reading.weekly >= weekly_rotation_line() || reading.session
>= session_ceiling_base)` and acts on it, arming `exhausted_poll_until`. Of its three inputs, two are
already persisted — `usage_store::Sample` carries `session` and `weekly`, and both lines are config —
and the third, *which account was active*, is reconstructible in the aggregation path itself:
`aggregate_with_roster` already receives the swap stream and already rebuilds an active-account
timeline from it via `active_at`, for `contribution_counts`. The daemon additionally emits durable
edge events, `Event::ExhaustedSlowPoll` / `ExhaustedSlowPollCleared`, that bracket each slow-poll
episode by account.

So at least three routes exist to carry the state. All are moot: they would deliver a fact that does
not license the inference anyone wanted from it.

## Alternatives considered

1. **Anchor low readings too — extend the asymmetry to every reading that carries a
   `session_resets_at`.**
   - **Pros**: the single smallest change; it is what "make the census measurable" naively suggests,
     and it lifts the three starved peers dramatically — per-account coverage on `a4`/`a5`/`a6` goes
     from 7.6 / 7.6 / 7.5 % to **62.4 / 55.3 / 64.4 %**.
   - **Cons**: measured joint census coverage rises from 0.00 % to **1.61 %** (2790 s of 172 800 s).
     That is the whole yield. Set against it, each of those per-account jumps is an assertion of
     *known-and-not-high* across roughly 55 percentage points of window that nothing observed —
     precisely the fabricated calm **REQ-STA-B-008** exists to forbid, and the same objection that
     already sank the globally-widened `stale_after_secs`.
   - **Why rejected**: it trades the requirement's core guarantee for 1.61 %. The ratio is the
     argument; neither half of it was known before this measurement.

2. **Honour `weekly_resets_at` in the census as well as `session_resets_at`.**
   - **Pros**: a weekly-drained roster is exactly where the census goes blind, so consulting the
     weekly reset looks like the dimension-appropriate fix.
   - **Cons**: measured joint coverage stays at **0.00 %** — it buys literally nothing here — and
     Rule 2b already forbids it for a reason that outlives this corpus: a weekly reset six days out
     says nothing about whether a session reading, which moves in minutes, is still true.
   - **Why rejected**: no benefit, and it would manufacture coverage across unobserved stretches. The
     measurement removes the last reason to revisit it.

3. **Carry rotation state (a new `Sample` field, or derive it from the swap log, or consume the
   `ExhaustedSlowPoll` episode brackets) and anchor a weekly-saturated peer's low session reading on
   the strength of it.**
   - **Pros**: it is the narrow fix #1097 pointed at — it would touch only the class that is actually
     losing coverage, leaving every other low reading un-extended, and all three carriage routes are
     mechanically available.
   - **Cons**: the guarantee it would license is contradicted by the frozen corpus (17 of 274
     same-window weekly-pinned pairs climb; `a5` climbs +0.10 across one widened-cadence gap). The
     state is carriable; the inference from it is not sound. Building the carriage would produce a
     census that is confidently wrong in exactly the direction REQ-STA-B-008 forbids, and it would
     look more principled than alternative 1 while being less honest — its extension is justified by
     a premise the data rejects, where alternative 1 at least wears its assumption openly.
   - **Why rejected**: on measurement, not on cost. This is the alternative worth writing down, since
     it is the one a future reader will re-derive.

4. **Leave the rationale as it stood and treat #1097 as a documentation nit.**
   - **Pros**: no change to three ratified documents.
   - **Cons**: the sentence is load-bearing. "The low peer was never blind" is the entire argument
     that the asymmetry costs nothing, and it is false for half of a real roster. Left standing, the
     next reader either believes the census has no blind spot, or discovers it does and concludes the
     asymmetry itself is a bug.
   - **Why rejected**: a wrong reason attached to a right rule is how right rules get removed.

## Consequences

### Positive

- **The census keeps its meaning.** UNKNOWN on a weekly-drained roster is a true report of what was
  observed. Nothing here weakens REQ-STA-B-008, and #1030's anchoring stays exactly as narrow as it
  was ratified to be.
- **The rationale now survives contact with the corpus.** All three documents argue the asymmetry
  from the direction of the guarantee, which is what `high_windows` actually implements, rather than
  from a rotation claim the fixture contradicts.
- **The refutation is asserted, not narrated.** The replay test now pins the weekly-pinned session
  climb against the frozen corpus, so a future attempt at alternative 1 or 3 meets a failing test
  rather than a paragraph.
- **The dead ends are priced.** 1.61 %, 0.00 % and the three carriage routes are recorded, so the
  next person to have these ideas can skip the measurement.

### Negative / trade-offs

- **The census stays UNKNOWN on precisely the roster shape an operator most wants a readout for.**
  This is the cost, and it is real: a fleet drained on the weekly dimension gets no utilisation
  census at all. Accepted, because the alternative on offer is a number that would be fabricated over
  unobserved time. The capacity-holds census remains measurable on the same corpus and is the readout
  that survives.
- **This is settled against one 48 h corpus.** It is real data and it is frozen, but it is one slice
  of one operator's fleet, and no weekly-pinned reading in it ever approaches the water. A future
  capture in which a weekly-saturated peer *does* run hot on the session dimension would sharpen the
  question rather than reopen it — the falsifier already found climbs, so more data can only add
  them.
- **Three routes to carry rotation state now exist and are documented as unused.** A reader who finds
  `active_at` and the `ExhaustedSlowPoll` events may reasonably ask why the census ignores them. The
  answer is here rather than at each site.

## Related

- Issues: **#1097** (this ADR). Prior art: **#1030** (the census anchoring whose rationale this
  corrects; PR #1096 flagged the contradiction rather than absorbing it), **#803 / #804**
  (REQ-STA-B-008 gap honesty and the capacity-holds census), **#537 / ADR-0019** (the widened
  out-of-rotation cadence that creates the blind spot), **#806** (the frozen replay corpus),
  **#465** (the shared canonical credential external sessions consume), **#613** (the
  single-machine-sync boundary: cross-machine coordination as a non-goal — `src/swap.rs` § of the
  same name is the canonical statement, mirrored at `src/reliability.rs`).
- Still open, deliberately not closed here: **#865** — whether the census should refuse to report
  under roster fallback. R-18 changed the measurability landscape it was raised against; this ADR
  does not re-read it.
- Code: `src/usage_stats.rs` — `high_windows` (the asymmetry), `all_high`, `active_at`,
  `contribution_counts`, and the replay test
  `on_the_replay_corpus_the_utilisation_census_is_unknown_because_the_drain_was_weekly`.
  `src/daemon.rs` — `note_exhausted_poll`, `exhausted_poll_window`, `weekly_rotation_line`.
  `src/usage_store.rs` — `Sample` (the fields a reading does and does not carry).
  `src/observability.rs` — `Event::ExhaustedSlowPoll` / `ExhaustedSlowPollCleared`.
- Documents: `docs/specs/census-validity-anchoring.feature.md` Rule 3;
  `docs/design/stats-honesty-cross-surface-solution-design.md` § D-I;
  `docs/requirements/stats-honesty-cross-surface.md` § R-18.
- ADR-0019 (the widened cadence); ADR-0009 (the per-account skip it layers on).
