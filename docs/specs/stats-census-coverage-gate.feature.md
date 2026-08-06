# Feature: the census reports UNKNOWN when it could never have measured

Issue #1029 · PRD R-1 / R-2 / R-8 / R-21 · design D-C / D-E

Example Mapping: 🟦 4 rules · 🟩 9 examples · 🟥 0 open

> The panel prints `All accounts ≥95% at once — 0 episodes (0s)` for a week in which the census was
> never once able to observe the whole roster. `all_high_covered_secs` was `0` — the denominator, not
> the numerator. Zero coverage means *unmeasurable*, and the panel renders it as *calm*.
>
> The CLI already gets this right. The gap is one field: `all_high_covered_secs` is not among the
> keys `StatsRoster` decodes, so the Swift side cannot gate on a denominator it never reads. This is
> the identical shape of #804 → #805, which carried `all_high_threshold` over the same boundary
> for the same reason — *"a fabricated threshold is the very defect #805 exists to end"*.

## Rule 1 — a zero coverage denominator renders UNKNOWN, never a number

```gherkin
Scenario: the panel gates on the denominator it must first decode
  Given all_high_covered_secs is 0
   When the Stats tab renders the all-accounts-high census
   Then no episode count is rendered
    And no duration is rendered
    # The wire already carries the field (RosterWire, serialized). No schema bump is needed — only a
    # Swift decode plus the gate. Verify the decode landed before trusting the gate: a gate reading a
    # field that defaults to 0 on a decode miss would pass while measuring nothing.

Scenario: a partially covered week reports what it saw
  Given all_high_covered_secs is a fraction of the window
   When the census is rendered
   Then the episodes and duration are rendered
    And the extent of observation is stated alongside them
    # Partial coverage is still a measurement — REQ-STA-B-008 says low-coverage periods SHALL be
    # annotated, not suppressed. Suppressing them would trade one dishonesty for another.

Scenario: the CLI's existing behaviour is the reference
  Given the same wire payload is rendered by the CLI and by the panel
   When the census is unmeasurable
   Then both report UNKNOWN
    # roster_line already does this (stats.rs). The CLI is the reference implementation FOR COVERAGE
    # GATING ONLY — it is not a clean surface overall; its runway rendering carries four defects of
    # its own (#1028).
```

## Rule 2 — UNKNOWN is distinguishable from a measured zero *and* from a quiet week

```gherkin
Scenario: unmeasurable does not look like quiet
  Given a week in which the census was never able to observe the whole roster
    And separately, a week fully observed in which no episode occurred
   When each is rendered
   Then the two renders are distinguishable
    # This is the entire user-visible complaint. Both weeks currently print "0 episodes (0s)".
    # One means "we never saw"; the other means "we saw, and nothing happened". Conflating them is
    # what makes a daemon-down week read as "underused → cancel a subscription" (D-STA-9's rationale).

Scenario: a bare dash is not sufficient on the panel
  Given the census is unmeasurable
   When the Stats tab renders it
   Then the render states the condition, not merely a sentinel glyph
    # Premortem P1: a bare "—" reads as "all quiet" on a surface with room for words. The CLI's "—"
    # is defensible in a dense line-oriented render; the panel has neither that constraint nor that
    # convention. STATE-parity permits this divergence — it is per-medium vocabulary, same state.
```

## Rule 3 — no implementation vocabulary in a user-facing string

```gherkin
Scenario: the denominator is stated as its meaning, not its field name
  Given the census is rendering its coverage
   When the coverage is expressed to the reader
   Then it is stated as the condition it represents
    And the word "covered" does not appear as a bare qualifier
    # Operator, on reading a draft: "0 covered — covered WHAT?" The word is the field name
    # `all_high_covered_secs` leaking through. What it MEANS is whether the census could see the
    # whole roster at one moment — which is sayable in plain words and is what the reader needs.

Scenario: the CLI has the same defect and is in scope for it
  Given the CLI renders ", 64% covered"
   When this rule is applied
   Then the CLI string is corrected too
    # Found while drafting the panel copy. Fixing only the panel would leave the two surfaces
    # divergent on a rule that applies to both — and would make the CLI the odd one out on a
    # correctness property it otherwise leads on.
```

## Rule 4 — the render survives the states the mock never depicted

```gherkin
Scenario: every reachable census state has a defined render
  Given the census can be unmeasurable, partially observed, or fully observed
   When each state reaches the Stats tab
   Then a defined render exists for it
    # apps/menubar/design/menubar-preview.html depicts ONLY the happy path (3 episodes (1h40m)).
    # The panel is FAITHFUL to a reference that never covered the degraded case — a Reference Defect,
    # not an implementation defect. #1037 adds the frames; this rule is what they must satisfy.

Scenario: the gate does not regress when the roster is empty
  Given the roster is empty
   When the census is rendered
   Then it reports UNKNOWN rather than a measured zero
    # A cardinality-zero subject passing a gate is not evidence the gate works. An empty roster
    # trivially has "no instant where all accounts were high" — which is unmeasurable, not calm.
```
