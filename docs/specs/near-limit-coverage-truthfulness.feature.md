<!--
SPECIFICATION STUB — not executable.
This repo has no Gherkin runner; the executable gates are the Rust test suite and the Swift
XCTest bundle. These scenarios pin each acceptance criterion in scenario form; do not read a
written scenario as a passing test.

No scenario below names a binding test yet — the item is scoped, not built. Add a *Binds* line to
each as its test lands; an unbound scenario is a statement of intent, never evidence.
-->

# Feature: near_limit_poll_coverage reports the interval the scheduler applied

Issue #1487 · PRD R-2 / R-3 · design D-2 / D-2b

Example Mapping: 🟦 3 rules · 🟩 6 examples · 🟥 0 open

> At the live configuration the event reports `sub_interval_secs=60` — the configured cap — while the
> scheduler applies `min(poll_secs / N, near_limit_poll_secs)` = `min(37.5, 60)` = **37.5 s**. The cap
> has never bound here. Emitted 624 times over the investigated corpus, at **band entry** — the exact
> regime an operator inspects during an incident. A reader who trusts it concludes the daemon
> tightened its polling when it did not, and stops looking.
>
> `Event::NearLimitPollCoverage` emits the cap; `next_subinterval` computes the effective value,
> both in `src/daemon.rs`; and the doc comment on `near_limit_poll_secs` already describes the
> `min(...)` form correctly — it even names `next_subinterval` as the applier. The code and its documentation
> are right — only the emitted value is wrong.

## Rule 1 — the reported sub-interval is the one the scheduler applied

```gherkin
Scenario: the cap does not bind, so the effective interval is reported
  Given poll_secs is 300 and the roster size N is 8
    And near_limit_poll_secs is 60
    And the poll-interval jitter is pinned so the drawn interval is exactly 300 seconds
   When the daemon enters the near-limit band and emits near_limit_poll_coverage
   Then the reported sub-interval is 37.5 seconds
    And it is not 60 seconds
    # This is the RED oracle. Against the pre-change tree it asserts 37.5 and observes 60.
    # The pinned-jitter Given is LOAD-BEARING, not scene-setting. poll_secs jitters BY DEFAULT
    # (src/config.rs: "the one tunable that jitters by default", sigma ~20%) and next_subinterval
    # draws a FRESH interval per sub-interval before dividing, so base is a random variable whose
    # MEAN is 37.5. Without the pin this Then is not decidable and the test is flaky.
    # It also requires a sub-second field: integral sub_interval_secs cannot express 37.5 and
    # would truncate a truthful report to 37, passing "not 60" while still lying.

Scenario: the cap does bind, and is reported as the applied value
  Given a configuration in which near_limit_poll_secs is below poll_secs / N
    And the poll-interval jitter is pinned so the drawn interval is exactly poll_secs
   When the event is emitted
   Then the reported sub-interval equals near_limit_poll_secs
    # Same rule, not a special case: the applied value is min(base, cap) either way.
    # The pinned-jitter Given is as LOAD-BEARING here as in the scenario above, and for the same
    # reason: base is a draw. Without the pin, "near_limit_poll_secs below poll_secs / N" only makes
    # the cap binding LIKELY, so the Then is probabilistic -- and the flake would land on the arm
    # that is supposed to prove the cap is not dead code.
```

## Rule 2 — the cap travels beside the applied value, so "did it bind?" is answerable

```gherkin
Scenario: both values are carried
  Given any near-limit band entry
   When the event is emitted
   Then the line carries the applied sub-interval
    And the line carries the configured cap
    # Strictly more informative than either alone. A reader can see the cap did not bind rather
    # than having to infer it from poll_secs and roster size.
```

## Rule 3 — nothing about when the daemon polls changes

```gherkin
Scenario: the schedule is untouched
  Given the change to the emitted value has landed
   When the existing #80 and #366 stagger locks run
   Then they pass unchanged
    And per-tick spacing is unchanged
    And the aggregate request rate is unchanged
    And the value of near_limit_poll_secs is unchanged
    # A regression guard, NOT a RED oracle — it passes pre-change too. Do not present it as
    # evidence the fix works.

Scenario: the event is not gated away
  Given a configuration in which the cap never binds
   When the daemon enters the near-limit band
   Then the event is still emitted
    # Considered and rejected: gating on the cap binding would delete the only durable record
    # that the band was entered — which, at the live configuration, means always.

Scenario: lowering the cap is out of scope
  Given the temptation to make the schedule tighter rather than the report truthful
   Then near_limit_poll_secs is NOT lowered
    # That is #1458's scope, conditional on operator-owned {T}/{D}. The cap is SHARED, so lowering
    # it tightens the whole tick — a rate change ADR-0012 Decision 3 forbids buying silently.
```
