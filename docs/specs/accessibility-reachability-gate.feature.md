# Feature: the reachability gate detects an unreachable affordance

Issue #982 · PRD R-5 / R-5a / R-5b (**T2**, source-as-data) + **R-5c** (**T3**, manual) ·
design § 5.2, § 11

Example Mapping: 🟦 5 rules · 🟩 7 examples · 🟥 1 open (polarity — resolved by #971)

## Rule 1 — the predicate distinguishes driver-present from driver-absent

```gherkin
Scenario: a variable-sourced injection satisfies the positive gate
  Given issue #971 selected a driver
    And StatusItemController injects a size class from a variable source
   When the reachability gate runs
   Then it passes

Scenario: a missing injection fails the positive gate
  Given issue #971 selected a driver
    And the construction site injects no size class
   When the reachability gate runs
   Then it FAILS, naming the construction site
```

## Rule 2 — a semantically-dead literal does NOT satisfy it

```gherkin
Scenario: a hardcoded .large is rejected
  Given the construction site injects `.dynamicTypeSize(.large)`
   When the reachability gate runs
   Then it FAILS
    # .large maps to factor exactly 1.0 (StatusPanelTypeScale.swift:132), so the panel renders
    # identically to today — green here would be green over the very defect the gate exists to catch
```

## Rule 3 — CONSTRAINT-A: falsifiable by mutation, both directions in one run

```gherkin
Scenario: the canary trips at the mutated site
  Given real panel sources are read from disk
    And the driver injection is reverted in memory
   When the gate runs over the mutated text
   Then it FAILS at that line

Scenario: the unmutated tree stays clean in the same run
  Given the same run as the canary above
   When the gate runs over the unmutated sources
   Then it passes
    # Both directions matter: a canary alone could pass while every real file was unreadable;
    # a green run alone could mean the predicate cannot fail at all
```

## Rule 4 — headless, and no bundle change

```gherkin
Scenario: the gate reads an excluded source as data
  Given StatusItemController.swift is excluded from the MenubarTests target
   When the gate discovers and reads it
   Then it succeeds without compiling it into the bundle
    And no window, screen, or TCC permission is required
    # Precedent: PanelDynamicTypeLintTests already reads this exact file — it must, to exempt it
```

## Rule 5 — the T2 gate does not close Matrix row 3 by itself

```gherkin
Scenario: a green T2 gate is not evidence of delivery
  Given the reachability gate passes
   When Matrix row 3 ("built, gated, unreachable") is claimed closed
   Then the claim is REJECTED unless a manual text-size step has also been run
    # R-5c. The T2 gate proves the driver is WIRED — which is exactly the evidence row 3 already
    # has. Only a real app under a real OS setting observes it DELIVERED.

Scenario: the manual step must be authored before it can be run
  Given apps/menubar/design/README.md:497-516 is the Appearance-settings checklist
   When it is inspected for a text-size step
   Then none exists — its four steps are Increase contrast, Reduce transparency,
        Reduce motion, and a Light/Dark repeat
    And closing row 3 therefore requires WRITING that step, not merely running the checklist
    # Written as a scenario, not a note: an executor building #982 would otherwise ship the T2
    # gate, see row 3 go green, and never discover the T3 half was never owned by anyone.
```

## 🟥 Open

- **Polarity** — under a "ship no driver" outcome the gate inverts to a **defect pin** (green while
  unreachable, red when fixed). Resolved by #971; both shapes are specified in design § 5.2, so the
  item is writable now.
