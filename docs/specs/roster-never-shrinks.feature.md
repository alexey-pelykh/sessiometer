# Feature: a reload may widen a live roster, never narrow one it did not mean to

Issue #1442 · PRD R-3 / R-3a / R-4 / R-7 / R-18 · design D-2 / D-4

Example Mapping: 🟦 3 rules · 🟩 10 examples · 🟥 0 open

> `reconcile_roster` (`src/daemon/commands.rs:505`) applies no floor: whatever a reload presents
> becomes the roster. On 2026-08-27 that took a live six-account roster to one.
>
> **The obvious guard is inert, and the first scenario below is written to prove it.** A
> "refuse an empty reload" floor never fires on this incident, which was 6 → 1 — and cannot fire on
> any append-only path at all, because `plan_capture` (`src/capture.rs:521-571`) has only
> update-in-place and push arms and therefore *always* leaves at least one account in the file it
> saves. An empty floor fires only on a legitimate `remove`-to-zero: the one case that should be
> allowed. The invariant has to be **shrink**-scoped and partitioned by **intent**.

## Rule 1 — the shape of the guard, stated so the inert version fails

```gherkin
Scenario: the incident
  Given the daemon holds six accounts
   When a reload carrying append-only intent presents one account
   Then the reload is refused
    And the daemon still holds six accounts
    # 6 → 1. An empty-roster floor passes this scenario by doing nothing, and the daemon still
    # loses five accounts. This scenario is the reason the invariant is shrink-scoped.

Scenario: an empty floor would never have fired
  Given the daemon holds six accounts
   When a reload carrying append-only intent presents zero accounts
   Then the reload is refused
    # Kept as a control, not as the mechanism. Reachable only if the file is emptied by something
    # other than an append-only verb — an append-only verb cannot produce it.

Scenario: widening is untouched
  Given the daemon holds one account
   When a reload carrying append-only intent presents two accounts
   Then the reload is adopted
    And the daemon holds two accounts
    # Issue #139 is why notify_daemon_roster_reload exists at all. A guard that narrows this
    # direction breaks the fix it is built on. PRD R-18.

Scenario: an equal-count reload is adopted
  Given the daemon holds three accounts
   When a reload carrying append-only intent presents three accounts
   Then the reload is adopted
    # A relabelled or re-ordered account is not a shrink. `<` not `<=`.
```

## Rule 2 — intent travels, and its absence is the refusing case

```gherkin
Scenario: a mutating verb may shrink
  Given the daemon holds six accounts
   When a reload carrying mutating intent presents five accounts
   Then the reload is adopted
    # `remove` is not a defect. Without intent the daemon can only refuse every shrink — blocking
    # this — or none, which is today.

Scenario: a mutating verb may shrink to zero
  Given the daemon holds one account
   When a reload carrying mutating intent presents zero accounts
   Then the reload is adopted
    # Removing the last account is legitimate. This is the case the empty floor would have broken.

Scenario: omitted intent is refused, not trusted
  Given the daemon holds six accounts
   When a reload arrives with no intent supplied and presents one account
   Then the reload is refused
    # notify_daemon_roster_reload() takes no arguments across five call sites: it is a bare
    # notify_daemon_roster_reload at src/capture.rs:123, :859 and at src/cli.rs:4778, :5572,
    # :5674, so a rollout will transiently have callers that send
    # none. The omitted value IS the fail-closed default: a partial rollout refuses rather than
    # permits, and an old caller is safe by construction.
```

## Rule 3 — one enforcement point, and the record can express what happened

```gherkin
Scenario: every caller is covered by the same check
  Given the three reconcile_roster call sites at commands.rs:313, :436 and :470
   When any of them presents a shrinking append-only reload
   Then it is refused
    # The invariant lives in reconcile_roster, not in its callers. The investigation initially found
    # two of the three call sites, which is the argument: a per-caller check is one missed caller
    # away from the original defect.

Scenario: a refusal is legible after the fact
  Given a reload is refused
   Then a roster-reload event records outcome refused with previous count and incoming count
    # "Refused 1" says nothing. "Refused 1, was 6" is the whole event. A single count cannot express
    # a shrink — see issue #1438.

Scenario: the degenerate-but-valid test no longer says that
  Given reconcile_roster_to_an_empty_roster_clears_active_and_state
   Then an empty reconcile is valid only under mutating intent
    # commands.rs:2577-2591 currently blesses the empty reconcile as "a degenerate-but-valid runtime
    # state" — the codified form of the assumption this feature refutes. The comment goes with the
    # behaviour.
```
