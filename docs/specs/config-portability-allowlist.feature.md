<!--
SPECIFICATION STUB — not executable.
This repo has no Gherkin runner; the executable gates are the Rust test suite and the
scripts/check-*.sh shell gates. These scenarios pin each acceptance criterion in scenario form and
bind it to an ACC capability from the Master Test Plan
(docs/design/migration-credential-portability-solution-design.md § 11). Do not read a written
scenario as a passing test.
-->

# Feature: the system refuses config values that must never cross machines

Tracked as **issues #1045, #1047**. Requirements: PRD R-11, R-11a…R-11e. Design § 4.8, AD-7/8/11.

**Rule under test**: the allowlist binds **regardless of the operator's scope selection**. Scope
selection answers *what was asked for*; this answers *what is permitted*.

## Scenario: `claude_bin` is refused even when settings were requested  · Cap-8.1

    Given an artifact whose config sets `[refresh].claude_bin` to an attacker-chosen path
    When it is imported WITH `--settings`
    Then the target's saved config does not contain that value

    # The flag-enabled path is the one that matters. Asserting only the default path would pass
    # while leaving the actual hazard reachable.

## Scenario: a weaker KDF is refused, a stronger one accepted  · Cap-8.2

    Given a local KDF parameter set
    When an artifact carrying weaker parameters is imported with `--settings`
    Then the weaker values are refused
    But an artifact carrying stronger values is adopted

## Scenario: the target operator's conflict policy survives  · Cap-8.3

    Given a target whose `[migration].conflict_policy` was deliberately set
    When an artifact carrying a different policy is imported with `--settings`
    Then the target's policy is unchanged

    # AD-11, resolved over a recorded dissent (PRD § 9 D-1). Today this cannot happen at all;
    # --settings is what would newly allow it.

## Scenario: an unclassified key fails the build  · Cap-8.4

    Given a new key is added to `Config`
    When it carries no portability classification
    Then the build fails

    # The load-bearing one. An unenforced allowlist is a denylist with extra steps, and denylist-rot
    # is the exact failure the allowlist was chosen to avoid.

## Scenario: refusals are visible  · Cap-8.5

    Given any key is refused during an import
    When the command completes
    Then the refusal is reported on stdout
    But no refusal line contains a token or an email

    # A silently dropped claude_bin is indistinguishable from one that was never present.

## Scenario: default-deny holds for a key nobody carved out  · Cap-8.6

    Given an artifact carrying a non-roster key classified non-portable
    And that key is none of claude_bin, kdf_*, or conflict_policy
    When the operator runs `import --settings`
    Then the key is not adopted
    And the refusal is reported
    But not by relying on the key being one of the three named carve-outs

> R-11's own assertion is **default-deny over a key nobody carved out**, and it is the one the other
> scenarios do not make. Cap-8.1/8.2/8.3 each pin a *named* carve-out and Cap-8.4 pins the add-time
> guard; all four pass while an ordinary non-portable key sails through at runtime, because none of
> them exercises the default branch. Use a **`[jitter]`** field: PRD § 1 calls `[tunables]` and
> `[stats]` *freely portable*, so an implementer building the allowlist from it classifies those
> `Portable` and they cannot satisfy this Given — `[jitter]` is the one of the three that can.
> `--settings` is in the *When* deliberately: the operator's widest flag must still not widen past the
> allowlist — the flag is a ceiling, never a floor (R-9a).
>
> **The *Given* says "classified non-portable", not "unclassified" — that distinction is the
> scenario.** *Corrected 2026-08-04 (fifth pass).* An earlier draft said *unclassified*, which is not
> buildable in either reading: a **known** `Config` key with no classification is a **compile error**
> by R-11d (Cap-8.4 asserts exactly that — there is no binary in which to run this test), and a key
> **not** in `Config` is rejected by `RawConfig`'s `deny_unknown_fields` (`src/config.rs:1378`) on the
> full-parse path that `--settings` uses, before the allowlist is ever consulted — so no refusal line
> could be emitted to observe. The default branch is reached by a key that *is* classified, and
> classified **non-portable**.
