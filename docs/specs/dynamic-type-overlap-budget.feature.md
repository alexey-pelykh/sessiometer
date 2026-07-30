# Feature: content-sized elements stay within their allowance at every size class

Issue #896 · PRD R-6 / R-6a · tier **T1** (in-process, driveable)

Example Mapping: 🟦 3 rules · 🟩 5 examples · 🟥 0 open

## Rule 1 — every content-sized element stays inside the allowance its budget assumes

```gherkin
Scenario Outline: content-sized elements fit their allowance
  Given a roster row rendered at <sizeClass>
   When authColumn and statsSignalPill are measured
   Then each stays within its allowance
    And the test reports required-vs-allowance, not just pass/fail

Examples:
  | sizeClass       |
  | large           |
  | xLarge          |
  | xxLarge         |
  | xxxLarge        |
  | accessibility1  |
  | accessibility2  |
  | accessibility3  |
  # allowances: authColumnAllowance 60, statsSignalPillAllowance 85
  # derived budgets resting on them: rosterLabelBudget 171, statsHandleBudget 198
  #
  # WHY SEVEN ROWS, NOT TWELVE — and why the sweep is still twelve.
  # `PanelTypeScale.factor` clamps to [floor .large, ceiling .accessibility3], so only these seven
  # of the twelve `DynamicTypeSize` cases yield DISTINCT factors. The other five are aliases of an
  # endpoint: .xSmall/.small/.medium → 1.0 (the floor), .accessibility4/.accessibility5 → 40/17
  # (the ceiling). Measuring them as if independent would report five duplicate results as five
  # real ones — the degenerate-certification shape CONSTRAINT-A exists to reject.
  # The sweep still visits all twelve (PRD AC-5, design Cap-2.1, and the in-tree precedent
  # `PanelTextMetricsTests`): the five alias cases assert the CLAMP holds, which is a different
  # assertion from the seven above and would otherwise go untested.
```

```gherkin
Scenario Outline: the five clamp-alias classes render at their endpoint's factor
  Given a roster row rendered at <sizeClass>
   When PanelTypeScale.factor is evaluated
   Then it equals <endpointFactor>
    And the row measures identically to its endpoint class

Examples:
  | sizeClass       | endpointFactor        |
  | xSmall          | 1.0     (floor .large)|
  | small           | 1.0     (floor .large)|
  | medium          | 1.0     (floor .large)|
  | accessibility4  | 40/17   (ceiling a3)  |
  | accessibility5  | 40/17   (ceiling a3)  |
  # These five complete the twelve-class sweep PRD AC-5 and design Cap-2.1 require. They are a
  # DIFFERENT assertion from Rule 1's seven — they assert the CLAMP, not the budget — so they are
  # written as executable scenarios rather than left in a comment, where an executor building the
  # suite would build seven and silently omit five.
```

## Rule 2 — the predicate must NOT be frame arithmetic

```gherkin
Scenario: a summed-frames predicate is rejected as tautological
  Given the panel scales UNIFORMLY by one factor k
   When a predicate compares summed row frames to panel width
   Then that predicate is invalid for this gate
    # It computes k·(default arithmetic) at every class: it cannot fail at class N if it passes at
    # .large. Signal lives only in elements sizing to CONTENT, whose width grows non-linearly with
    # point size (glyph advance, hinting, device rounding) while the allowance grows linearly.
```

## Rule 3 — CONSTRAINT-A, and no green-by-collapse

```gherkin
Scenario: the canary trips through the same predicate
  Given the real measurement path
   When an element's demand is mutated past its allowance
   Then the gate FAILS for that element
    And the unmutated case passes in the same run

Scenario: a zero-vs-zero collapse is rejected
  Given both measured demand and allowance could degrade to zero
   When the assertion would read 0 == 0
   Then the gate treats that as NOT-EVIDENCE and fails
    # The exact shape issue #755 only caught by re-running the matrix adversarially
```
