# Feature: an unclassified CLI verb fails the build

Issue #1012 · PRD R-2 / R-3 / AC-8 / § 5b · design § 5.5

Example Mapping: 🟦 4 rules · 🟩 8 examples · 🟥 0 open

> This is the durable half of #1008. The rest of the umbrella closes today's gap; this keeps it
> closed. The `capture`/`login` split went unrecorded for the project's life because **nothing forced
> the question** — not because anyone decided wrongly.

## Rule 1 — every parsed verb carries exactly one classification

```gherkin
Scenario: the table covers the parser
  Given the verbs parsed by `parse_subcommand` in `src/cli.rs`
   When the routability table is compared against them
   Then every parsed verb has exactly one row

Scenario: a new verb without a row fails
  Given a new verb is added to the parser
    And no routability row is added
   When the parity test runs
   Then it FAILS, naming the unclassified verb
    # This single scenario is the item's entire reason for existing. It forces the question once,
    # per new verb, at the cheapest possible moment.

Scenario: a stale row without a verb also fails
  Given a verb is removed from the parser
    And its routability row remains
   When the parity test runs
   Then it FAILS
    # Set equality, not containment. A one-directional check lets the table accumulate rows for
    # verbs that no longer exist, and a table nobody trusts is a table nobody maintains.
```

## Rule 2 — `structurally-unroutable` requires a reason, not a status

```gherkin
Scenario: a structural claim without a reason is rejected
  Given a verb classified structurally-unroutable
    And no structural reason is stated
   When the row is reviewed
   Then it is REJECTED

Scenario: "not yet built" is not a structural reason
  Given a verb classified structurally-unroutable because it has not been implemented
   Then the classification is wrong — it is `routable`
    # This is the exact failure the classification exists to prevent. "Can't" and "haven't" look
    # identical in a status column and are opposite facts. `login` sat in the second category while
    # everyone read it as the first, for the life of the project.

Scenario: the three genuine cases share one reason
  Given run, service install/uninstall, and daemon restart (unmanaged)
   When each is classified
   Then each is structurally-unroutable
    And the reason is that a socket cannot serve a verb managing the lifetime of the process
        serving it
    # A managed agent is stopped with launchctl bootout, not over the socket
    # (src/daemon/socket.rs:19-23).
```

## Rule 3 — `routable`-but-unrouted is a tracked gap, not a resting state

```gherkin
Scenario: a routable verb with no command is a gap
  Given a verb classified routable
    And no socket command serves it
   Then it is a tracked parity gap
    And a tracked item exists for it
    # PRD R-3. Without this, `routable` becomes a comfortable place for a verb to sit forever —
    # which is functionally where `login` was, minus the label.
```

## Rule 4 — CONSTRAINT: derive the counts, never hardcode them

```gherkin
Scenario: the test derives both sets
  Given the parser currently exposes 18 verbs
    And the socket currently serves 11 commands
   When the parity test runs
   Then it derives both sets from source
    And neither 18 nor 11 appears as a literal in the test
    # A hardcoded count turns the gate into a second thing to update, and a test that must be
    # edited whenever the subject changes is one that gets edited to pass.

Scenario: the gate is falsifiable
  Given a verb is removed from the routability table in memory
   When the parity test runs
   Then it FAILS
    # Demonstrate the failure. A completeness gate that has never been observed to fail is not
    # evidence of completeness — it is evidence a test ran.
```
