# Feature: the fleet runway refuses rather than states an impossible duration

Issue #1028 · PRD R-3 / R-4 / R-7 / R-20 · design D-A / D-B

Example Mapping: 🟦 4 rules · 🟩 11 examples · 🟥 0 open

> The reported symptom was `accounts last ~648427 days`. The live wire carried
> `runway_secs: 9223372036854775807` — `i64::MAX`. Two independent defects compose to produce it: a
> guard that tests for *exactly* zero against a value that decays toward zero but never reaches it,
> and a cast that saturates instead of failing. Either one alone is survivable; together they turn
> "no measurable drain" into a confident number.
>
> This is a **keystone compliance breach**, not an inaccuracy. REQ-STA-B-006 and REQ-STA-SUR-001 both
> say the surface SHALL NOT forecast. A figure of that magnitude cannot be read as anything else.

## Rule 1 — a burn rate not *meaningfully* distinguishable from zero yields UNKNOWN

```gherkin
Scenario: an exactly-zero rate refuses, as it does today
  Given every counted account has a combined weekly burn rate of exactly 0.0
   When the fleet runway is computed
   Then it is UNKNOWN
    # This case already works. The guard `total_rate > 0.0` catches it. Preserve the behaviour;
    # the bug is everything the guard lets through.

Scenario: a decayed EMA does not pass as a measurable drain
  Given the combined weekly burn rate is 1e-11
   When the fleet runway is computed
   Then it is UNKNOWN
    # Reproduced: 1e-11 yields 787,037 days. The velocity EMA decays geometrically and never lands
    # on 0.0, so `> 0.0` is not a test for "no measurable drain" — it is a test for "no drain at all",
    # which floating-point arithmetic will essentially never satisfy for an account that once moved.

Scenario: a genuine drain still reports
  Given the combined weekly burn rate reflects an account actually climbing toward its ceiling
   When the fleet runway is computed
   Then a duration is reported
    # The floor must not delete the feature. Premortem P4 is the failure this scenario guards:
    # a fix that makes the metric never report is not a fix.
```

## Rule 2 — no saturating or lossy conversion ever reaches a rendered figure

```gherkin
Scenario: the saturating cast cannot produce a rendered value
  Given a computation whose quotient exceeds i64::MAX
   When the seconds value is converted for the wire
   Then the conversion does not silently saturate
    And the runway is UNKNOWN
    # `f64 as i64` has been a *saturating* cast since Rust 1.45 — it does not panic, does not wrap,
    # and does not signal. It quietly hands back i64::MAX, which every downstream consumer then
    # treats as a known, valid, very large number.

Scenario: NaN and infinity are refusals, not values
  Given the quotient is NaN or infinite
   When the seconds value is converted
   Then the runway is UNKNOWN
    # 0.0/0.0 is NaN; x/0.0 is inf. Both saturate to a plausible-looking integer under `as i64`
    # (NaN casts to 0 — a runway of *zero seconds*, which reads as "the fleet is empty right now").
    # That one is worse than i64::MAX because it is not obviously absurd.
```

## Rule 3 — an implausible *result* is refused, never clamped

```gherkin
Scenario: a runway beyond one weekly window is impossible
  Given the computed runway exceeds one weekly quota window
   When the runway is evaluated for plausibility
   Then it is UNKNOWN
    And the implausible computation is recorded as a fault
    # The bound is derivable, not arbitrary: weekly quotas reset. A runway longer than one weekly
    # window asserts the fleet drains with no reset intervening — which cannot happen. This is why
    # the bound is result-side; an input-side epsilon on the rate would be a magic number in units
    # nobody can reason about.

Scenario: the bound does not scale with roster size
  Given a roster of six accounts
   When the plausibility bound is applied
   Then the bound is still one weekly window
    # Pooling headroom across N accounts does NOT push the horizon past the first reset — the resets
    # are what refill the pool. An earlier draft of this design used roster × window (42 days) and
    # was wrong by exactly a factor of the roster size.

Scenario: an implausible figure is never clamped to the bound
  Given the computed runway exceeds the plausibility bound
   When the runway is reported
   Then no numeric duration is rendered
    # Premortem P7, and it is the whole design stance: a clamped figure is a *credible* lie.
    # "42 days" looks like an answer and would be acted on. "unknown" does not and would not.
    # An obviously-absurd number is safer than a plausibly-wrong one — but refusing beats both.
```

## Rule 4 — the line is printed in every state, on every surface

```gherkin
Scenario: an unknown runway still prints its line
  Given the fleet runway is UNKNOWN for any reason above
   When the roster block is rendered
   Then the runway line is printed, stating that it is unknown
    And the counted set is still stated
    # `fleet_line` today emits the runway only under `Some(FleetRunway { runway_secs: Some(_), .. })`
    # (stats.rs:1729, doc: "Rendered ONLY when the pool has a finite runway"). So Rule 1's floor,
    # shipped alone, would make this line VANISH MORE OFTEN than it does today — the corrective work
    # would make the surface quieter rather than more honest. That is premortem P2 landing in reality.

Scenario: the reason for refusing is distinguishable
  Given the runway is UNKNOWN because no measurable burn was observed
    And separately, a run where it is UNKNOWN because the result was implausible
   When each is rendered
   Then the two renders are distinguishable
    # A reader who sees "unknown" twice for different reasons learns nothing. The no-burn case is
    # benign (a quiet fleet); the implausible case means the computation broke and a fault was
    # recorded (#1036). Collapsing them hides a defect behind a normal state.

Scenario: both surfaces obey this rule
  Given the menubar panel reports the fleet runway
   When the runway is UNKNOWN
   Then the panel prints the line and its state, exactly as the CLI does
    # STATE-parity, not glyph-parity (design-menubar.md R-2). The wording may differ per medium;
    # which state is shown may not. Pinned by #1035.
```
