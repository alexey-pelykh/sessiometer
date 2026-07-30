# Feature: a real user can reach the panel's Dynamic Type scaling

Issue #817 · PRD R-2 / R-3 / R-4 / R-11 · design § 5.1 · tiers **T3** (real app) + **T1** (brand lock)

Example Mapping: 🟦 4 rules · 🟩 4 examples · 🟥 2 open (which driver — #971; storage, if in-app)

## Rule 1 — the driver takes effect in the real app, not only the harness

```gherkin
Scenario: changing the driver changes the rendered panel
  Given a driver selected by issue #971
   When the user changes it and opens the panel
   Then the panel renders at the corresponding size class
    # Verified against a running app. The harness already proves the CONSUMER works;
    # what has never been shown is that anything reaches it.
```

## Rule 2 — exactly one injection site

```gherkin
Scenario: the single entry point is preserved
  Given the panel has one \.dynamicTypeSize entry point by design (issue #756)
   When the driver is added
   Then it injects at the StatusPanelView() construction site
    And no second entry point is introduced
```

## Rule 3 — fail-open to today's behaviour

```gherkin
Scenario Outline: a bad preference degrades to k=1.0
  Given the driver value is <state>
   When the panel renders
   Then it renders at k=1.0
    And the panel still opens

Examples:
  | state          |
  | missing        |
  | unreadable     |
  | out-of-range   |
  # k=1.0 IS today's rendering, so the fallback is a no-op rather than a new behaviour
```

## Rule 4 — the menu-bar item never scales

```gherkin
Scenario Outline: the status item is byte-identical at every class
  Given any driver value at <sizeClass>
   When the menu-bar status item renders
   Then it is byte-identical to its k=1.0 rendering

Examples:
  | sizeClass       |
  | large           |
  | accessibility3  |
  # Issue #437 ratified brand lock. StatusItemController.swift keeps its font-lint exemption;
  # the reachability gate (#982) asserts a DIFFERENT property about the same file.
```

## 🟥 Open

- **Which driver** (OS setting / in-app preference / none) — resolved by **#971**. R-3 makes "ship
  none" a live outcome; it must not be pre-empted, and #817's original AC-3 was superseded for
  exactly that reason.
- **Storage**, only live under the in-app option — design § 5.3 recommends client-local on the
  daemon-down argument. Ratification-pending.
