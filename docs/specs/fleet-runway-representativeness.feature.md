# Feature: the runway names who it counted, and refuses when they are the wrong accounts

Issue #1034 · PRD R-5 / R-6 · design D-B · gated by SPIKE #1033

Example Mapping: 🟦 3 rules · 🟩 8 examples · 🟥 0 open

> Observed live: `counted 1–2 of 6`. The five accounts actually pinned at weekly 97–100% were
> **excluded** by the staleness gate, because velocity is unknown when the last reading is older than
> `stale_after_secs`. The one surviving counted account was the idle spare with the large headroom.
>
> So the metric answering *"how long until the fleet runs out"* was computed exclusively from the
> accounts that are **not** running out. The pool rationale assumes the counted set is dominated by
> whichever account is climbing; here it is exactly inverted.
>
> **Two halves, and only one is blocked.** Half 1 (state the counted set) is unconditional honesty and
> ships now. Half 2 (refuse on an unrepresentative subset) waits on #1033, because a naive predicate
> may mean the runway never reports again — a feature deletion wearing a fix's clothing.

## Rule 1 — every surface reporting the runway states the counted set

```gherkin
Scenario: the CLI already satisfies this
  Given the runway is computed from 1 of 6 rostered accounts
   When the CLI renders the fleet line
   Then the counted set is stated alongside the figure
    # Already true — `fleet_line` prints "({counted} of {observed} counted)" (stats.rs:1735). An
    # earlier draft of this scope claimed the CLI left it unstated; that was wrong, and checking it
    # is what surfaced R-20. The open surface is the panel.

Scenario: the panel states it too
  Given the panel reports the fleet runway
   When the runway is computed from a proper subset
   Then the panel states the counted set
    # #1032 ships the panel's runway; this is the property it must carry over. counted/observed are
    # already on the wire ("counted":2,"observed":6) — no schema work.

Scenario: the counted set is stated even when the runway is unknown
  Given the runway is UNKNOWN
   When the roster block is rendered
   Then the counted set is still stated
    # R-20. "We counted 1 of 6 and cannot say" is a materially different statement from silence,
    # and it is the one that would have made the original defect self-evident.
```

## Rule 2 — a subset excluding every near-ceiling account cannot report

```gherkin
Scenario: the inverted case refuses
  Given five accounts sit at or near their weekly ceiling
    And all five are excluded from the counted set
    And the counted set is one idle account with large headroom
   When the runway is computed
   Then no runway figure is reported
    # The observed live case. Reporting here is worse than reporting nothing: the figure is not
    # merely imprecise, it answers the opposite question from the one it appears to answer.

Scenario: "at or near the ceiling" reuses the daemon's own boundary
  Given the predicate must decide what counts as near a ceiling
   When the boundary is chosen
   Then it is the daemon's existing viability boundary, not a new threshold
    # A second, independently-chosen water would drift from the first, and drift between two
    # definitions of the same condition is exactly what REQ-STA-B-010 exists to prevent. One shared
    # helper, one definition.

Scenario: a representative subset still reports
  Given the counted set includes the accounts closest to their ceilings
   When the runway is computed
   Then a figure is reported
    # The refusal is targeted at inversion, not at partiality. A runway from a representative
    # subset is a legitimate bounded statement; the counted set (Rule 1) is what makes it readable.
```

## Rule 3 — half 2 does not ship until the spike says it can

```gherkin
Scenario: the spike measures reporting rate before the predicate lands
  Given the candidate predicate from Rule 2
   When on-disk history is replayed through it over rolling weekly windows
   Then the proportion of windows that would still report is measured
    # #1033. Idle accounts are ALWAYS stale under the current polling behaviour (#80), so a naive
    # predicate plausibly excludes the whole roster forever. This cannot be settled analytically:
    # it depends on the empirical joint distribution of staleness and ceiling-proximity — which is
    # the exact thing that already surprised us once.

Scenario: the decision rule is fixed in advance
  Given the spike reports a reporting rate
   When the rate is at least ~20% of windows
   Then Rule 2 ships as designed
   But when the rate is below that
   Then DG-2 fires and Rule 2 descopes to "honesty shipped, correctness not reachable in appetite"
    # Fixed BEFORE the measurement so the threshold cannot be renegotiated against a disappointing
    # result. Descoping is an acceptable outcome: Rule 1 alone already ends the silent inversion,
    # because a reader who sees "1 of 6 counted" can tell the figure is not about the fleet.
```
