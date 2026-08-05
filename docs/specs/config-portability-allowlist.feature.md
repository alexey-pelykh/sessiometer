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

## Scenario: `[login].claude_bin` is refused on the same grounds  · Cap-8.1

    Given an artifact whose config sets `[login].claude_bin` to an attacker-chosen path
    When it is imported WITH `--settings`
    Then the target's saved config does not contain that value

    # PRD R-11a names BOTH keys; an earlier revision of this spec asserted only [refresh]. Both
    # funnel into claude_binary_from in src/paths.rs and both are spawned, so guarding one leaves
    # the capability transferable through the other.

## Scenario: a FRESH target adopts nothing the allowlist forbids  · Cap-8.1

    Given a target machine with NO local config at all
    And an artifact whose config sets a capability-granting key
    When it is imported WITH `--settings`
    Then the target's saved config does not contain that value

    # The fresh target IS the migration scenario, and it is the only path where the allowlist
    # CHANGES behaviour rather than formalising it: with no local config, apply_import's
    # fresh-target branch adopts the artifact's config WHOLESALE (PRD AC-3's BUT-NOT clause). Every
    # other scenario here can pass against a target that already has a config, so without this one
    # the capability the allowlist exists to block stays reachable on the path that matters most.

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
