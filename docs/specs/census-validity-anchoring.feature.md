# Feature: a reading stays valid as long as it carries a guarantee

Issue #1030 · PRD R-18 / R-19 · design D-D

Example Mapping: 🟦 5 rules · 🟩 11 examples · 🟥 0 open

> This is the item that re-sized the appetite, because it is the one that is *algorithmic* rather
> than a render fix. #1029 stops the panel fabricating a zero — necessary, and the honest render.
> But without this, the honest render is `unknown` **permanently**, and a metric that is structurally
> incapable of measuring is not much better than one that lies about it.
>
> The census intersects every rostered account's validity window. `validity_windows()` anchors a
> reading to the poll cadence: `hi = next.min(s.ts + stale_after)`. A saturated peer is polled on the
> *widened* exhausted cadence (`DEFAULT_EXHAUSTED_POLL_SECS = 3600`) while `stale_after_secs` tracks
> `poll_secs` (300) — so it is considered covered roughly 8% of the time. Intersect five such peers
> and the joint coverage collapses toward zero. **Precisely when the roster is saturated — the only
> time the "all accounts high" census has anything to report — it goes blind.**
>
> REQ-STA-B-010 already ratified the fix for the sibling metric: *"A blocked reading's validity window
> SHALL be anchored to its own carried expiry … rather than to the poll cadence. This is what makes
> the metric measurable at all."* `blocked_windows()` implements it. The census does not.

## Rule 1 — a reading carrying an expiry is valid until that expiry

```gherkin
Scenario: a saturated peer stays covered until its reset
  Given a rostered account's reading carries a session_resets_at in the future
    And the account is polled on the widened exhausted cadence
   When the census computes that reading's validity window
   Then the window extends to the carried expiry
    # Mirror blocked_windows: `anchored_hi = cadence_hi.max(relief_at)`. The account told us when it
    # would be back. That statement does not expire because we chose to ask again less often — and
    # we chose to ask less often BECAUSE it was saturated, which is the circularity to break.

Scenario: cadence still governs a reading with no carried expiry
  Given a reading carries no expiry
   When its validity window is computed
   Then it is anchored to the poll cadence exactly as today
    # The anchoring is a max(), not a replacement. A reading with nothing to promise gets no
    # extension — which keeps the honest-degradation floor where it is.

Scenario: the census becomes measurable under saturation
  Given five rostered accounts are at weekly 97-100% and carry resets
   When the census runs over the week
   Then the jointly covered time is a substantial fraction of the window
    # Acceptance signal for the whole issue, and "greater than zero" is too weak to be one. Replayed
    # against a real 6-account store over a trailing 7-day window: joint coverage goes from EXACTLY
    # 0.00% (0s — the intersection is genuinely empty) to ~39.9% (67h of 168h). The target is TENS OF
    # HOURS. A few seconds of coverage means the anchoring did not reach the path that computes it.
```

## Rule 2b — anchor to the session expiry, not the weekly one

```gherkin
Scenario: the weekly reset does not extend a session reading
  Given a reading whose weekly_resets_at is six days away
   When its validity window is computed
   Then the window is not extended to the weekly reset
    # The same replay returns 97.8% instead of 39.9% if weekly_resets_at is also honoured — a 2.5x
    # swing on one implementation choice, in the flattering direction. Take the smaller number: the
    # session percentage moves in minutes, so a weekly reset six days out says nothing about whether
    # a session reading is still true. Honouring it would manufacture coverage across stretches where
    # nothing was observed — which is what Rule 2 forbids. The wrong choice looks better on the
    # dashboard, which is exactly why it is written down here.
```

## Rule 2 — anchoring refines UNKNOWN; it must not repeal it

```gherkin
Scenario: a dead daemon still reads as unmeasurable
  Given the daemon stopped writing readings mid-window
    And no reading carries an expiry covering the silent period
   When the census computes coverage over that period
   Then the period is NOT covered
    # THE discriminating constraint, and the reason the cheaper alternative was rejected. Widening
    # `stale_after` globally would also have fixed the saturated case — by making a genuinely dead
    # daemon read as covered, destroying exactly what REQ-STA-B-008 exists to protect.

Scenario: an expired expiry does not extend anything
  Given a reading carries an expiry that has already passed
   When its validity window is computed
   Then the window does not extend beyond that expiry
    # A carried guarantee bounds validity; it does not confer it retroactively or indefinitely.

Scenario: the extension is bounded by the reporting period
  Given a reading carries an expiry beyond the end of the reporting window
   When its validity window is computed
   Then the window is clipped to the period end
    # validity_windows already clips with `.min(period.end)`. Preserve that; an anchored hi that
    # escapes the period would inflate the denominator and quietly flatter every coverage figure.
```

## Rule 3 — the asymmetry is intended, and is stated

> **Rationale corrected by #1097; the rule is unchanged.** This rule previously justified the
> asymmetry with *"the low peer is the one IN rotation, polled at the normal cadence — its coverage
> was never the problem."* That is false for half the roster in
> `build/fixtures/capacity-replay-corpus.tsv`: `a4`/`a5`/`a6` are session-**low** and polled on the
> **widened** exhausted cadence at the same time, held out of rotation by their weekly dimension —
> 44 / 44 / 43 readings across 172 800 s at session peaks of 0.03 / 0.15 / 0.00, about 7.6 %
> coverage each. A low peer can be exactly as sparsely polled as a saturated one, and that
> population is what pins that corpus's census at UNKNOWN. The asymmetry's real justification is the
> DIRECTION the guarantee runs in, which is what the scenarios below now state.
>
> Whether that class can carry a session guarantee of its own is **settled, not open**:
> **ADR-0033** decides it stays UNKNOWN. The candidate — an account out of rotation on its weekly
> dimension is not being spent against, so its session cannot climb — is contradicted by the same
> corpus, and carriability was never the blocker. Do not reopen it here.

```gherkin
Scenario: only readings that carry a guarantee are extended
  Given a low-utilisation account, whatever cadence it is polled on
   When its validity window is computed
   Then it receives no extension
    # A saturated peer carries session_resets_at; a low peer carries no equivalent. The asymmetry
    # runs in the direction the guarantee does: session utilisation only climbs within a window, so
    # a reading at/above the water stays true until its reset, while a low one can cross the water
    # at any instant. Extending it would assert known-and-not-high over time nobody observed. The
    # asymmetry is not an oversight to be tidied up later, and it is NOT a claim about rotation.

Scenario: a low peer on the widened cadence is left UNKNOWN rather than interpolated
  Given an account held out of rotation by its WEEKLY dimension
    And it is polled on the widened exhausted cadence
    And its session reading is below the water and carries a session_resets_at
   When the census computes that reading's validity window
   Then it receives no extension, and the time beyond the cadence horizon stays UNKNOWN
    # The class #1097 raised, decided by ADR-0033. This is where the census genuinely loses coverage
    # — it is the whole reason the replay corpus reports UNKNOWN — so the refusal is a cost being
    # paid, not a case nobody thought about. The tempting narrow fix is to carry rotation state and
    # extend on the argument that an out-of-rotation peer is not being spent against. Measured, that
    # argument is false: 17 of 274 weekly-pinned consecutive pairs sharing one session window show
    # session CLIMBING, `a5` by 0.05 -> 0.15 across a single 4109 s widened-cadence gap. Asserted by
    # on_the_replay_corpus_the_utilisation_census_is_unknown_because_the_drain_was_weekly.
```

## Rule 4 — the existing invariant survives, extended rather than replaced

```gherkin
Scenario: all_high_secs never exceeds the jointly covered time
  Given the anchoring is in effect
   When prop_all_high_time_never_exceeds_the_jointly_covered_time runs
   Then it still holds
    # The property must be EXTENDED to hold under the new anchoring, not rebaselined away. The
    # code comments in src/usage_stats.rs are explicit that the ⊆ relation is deliberately NOT
    # force-repaired in the aggregation path, precisely so this property can fail on a genuine break.
    # Preserve that non-repair.

Scenario: the HQ parent PRD carries the rule
  Given R-18 changes what "covered" means for the census
   When the change lands
   Then prd-stats.md carries the census-anchoring rule at requirement level
    # R-19. A code change that alters the meaning of a ratified metric, without amending the
    # requirement that ratified it, leaves the next reader deriving the behaviour from the code —
    # which is how the original asymmetry between B-010 and B-005 survived unnoticed.
```

## Not closed by this issue

**#865** — *should the census refuse to report under roster fallback* — is **not** resolved here.
R-18 changes the measurability landscape #865 was raised against, so it must be **re-read afterwards**.
It must not be closed by implication.
