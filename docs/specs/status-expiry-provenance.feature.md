<!--
SPECIFICATION STUB — not executable.
This repo has no Gherkin runner; the executable gates are the Rust test suite and the
scripts/check-*.sh shell gates. These scenarios pin each acceptance criterion in scenario form and
bind it to an ACC capability from the Master Test Plan
(docs/design/migration-credential-portability-solution-design.md § 11). Do not read a written
scenario as a passing test.
-->

# Feature: `status` makes the EXPIRY value's provenance legible

Tracked as **issue #1006**. Requirements: PRD R-7. Design § 4.5, AD-4.

**Rule under test**: `read_poll_clocks` (`src/daemon/snapshot_build.rs:45-53`) reads the **canonical**
item for the active account and the account's **stash** for parked ones. That asymmetry is *correct*
and stays. The defect is that both render identically, so a staged-but-unadopted import is
indistinguishable from a no-op.

## Scenario: the active account's value is marked as canonical-sourced  · Cap-5.1

    Given a status render with one active account and at least one parked account
    When EXPIRY is displayed
    Then an operator can tell which slot each value was read from

## Scenario: a stash/canonical disagreement is surfaced  · Cap-5.1

    Given an account whose stash and canonical item carry different expiry deadlines
    When status renders that account
    Then the disagreement is visible rather than silent

    But not by presenting the disagreement itself as evidence of non-adoption

> **A migration that went perfectly renders the same disagreement as one that did not.** *Corrected
> 2026-08-05 (twelfth pass); this comment read "This is exactly the post-import staged-not-adopted
> state", which is true and incomplete — the runbook's own adoption step produces it on SUCCESS.*
> `swap()` writes the pre-swap canonical into the outgoing stash (`src/swap.rs:838`) and the incoming
> stash into canonical (`:849`); on `use --force A` where A is already the active account, outgoing
> and incoming are the same account, so A's stash is left holding the target's **pre-import**
> credential while canonical holds the imported one. The daemon heals this only on an observed
> canonical *change*, and a daemon started **after** adoption — which is exactly runbook step 5 —
> primes its baseline instead (`src/daemon/canonical.rs:240-242`, `CanonicalChange::Primed` ⇒ prime
> the baseline, detect nothing).
>
> So the signal persists for the life of that daemon run on a healthy target. Surfacing the
> disagreement is still right — it converts the 2026-07-31 symptom into something visible — but the
> rendering must not assert *why*, or a successful migration reads as a failed one and the operator
> re-runs `use --force` or opens an incident against a target that is fine.

## Scenario: authority is unchanged  · Cap-5.1

    Given the active account
    When its EXPIRY is resolved
    Then the canonical item remains the authoritative source
    But not changed to the stash by this feature

    # Which slot is authoritative belongs to issue #1001, not to a display change.
