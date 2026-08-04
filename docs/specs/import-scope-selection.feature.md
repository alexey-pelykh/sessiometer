<!--
SPECIFICATION STUB — not executable.
This repo has no Gherkin runner; the executable gates are the Rust test suite and the
scripts/check-*.sh shell gates. These scenarios pin each acceptance criterion in scenario form and
bind it to an ACC capability from the Master Test Plan
(docs/design/migration-credential-portability-solution-design.md § 11). Do not read a written
scenario as a passing test.
-->

# Feature: `import` applies only the payload classes the operator selected

Tracked as **issue #1046**. Requirements: PRD R-9, R-9a, R-9b, R-9c, R-9d. Design § 4.7, AD-5/6/9/10.

**Rule under test**: scope is a property of the *apply*, never of the artifact. The artifact describes
what it carries; the operator decides what is applied; the operator's decision is a **ceiling, never a
floor**.

## Scenario: `--accounts` applies the roster and no config  · Cap-7.1

    Given an artifact carrying both a roster and non-roster config
    When the operator runs `import --accounts`
    Then the roster and its credentials are applied
    But no non-roster block reaches the target config

## Scenario: the default is unchanged  · Cap-7.2

    Given the same artifact
    When the operator runs `import` with no scope flag
    Then the resulting target state is byte-identical to the pre-change behaviour

    # Regression, not a new assertion. AD-9 keeps the shipped default; the safety case for
    # defaulting narrow is absorbed by the allowlist (#1045).

## Scenario: an artifact cannot widen the operator's selection  · Cap-7.3

    Given an artifact that asserts, by any means, that it should be applied in full
    When the operator runs `import --accounts`
    Then the non-roster blocks are still ignored

    # AD-6: on a --plaintext export nothing is authenticated, so any scope the artifact declares is
    # attacker-controlled. This scenario must stay meaningful even if a scope field is never added.

## Scenario: a roster-only artifact survives an unknown block  · Cap-7.4

    Given a roster-only artifact that also carries a block the parser does not know
    When it is imported with `--accounts`
    Then the import succeeds

    # Narrow-parse, not parse-then-filter: deny_unknown_fields never fires on blocks outside the
    # parse path. This is also the roster-only half of #1053.

## Scenario: `export` gains no narrowing flag  · Cap-7.5

    Given the scope feature has shipped
    When `export --help` is read
    Then it offers no config/roster narrowing flag

    # AD-5. Export scope is disclosure hygiene; import scope is input validation. Only the latter
    # defends against an artifact the attacker minted.

## Scenario: `--settings` on a roster-only artifact is not an error  · Cap-7.6

    Given a roster-only artifact
    When the operator runs `import --settings`
    Then the command reports that the artifact contains no configuration
    But does not fail, and does not silently do nothing

> **Where a roster-only artifact comes from — `export` cannot mint one.** `export` writes
> `config.render()` unconditionally (`src/cli.rs:4532`) and `render()` always emits at least
> `[tunables]` (`src/config/render.rs:370`), so `config_toml` is never empty on an artifact this
> product produced — and AD-5 keeps it that way by giving `export` no narrowing flag (scenario
> above). The precondition is therefore reachable only from a **hand-constructed `Payload`** (which
> is what the Cap-7.6 unit test must build), or from a third-party or future producer. State that in
> the test; do not try to reach it through `export`, and do not "fix" the un-emptiable `config_toml`.
>
> This is an asymmetry in R-9a's presence rule, not a defect in it: the `accounts`-empty half **is**
> export-reachable (`a_config_only_artifact_imports_accounts_as_roster_entries_without_a_stash`
> covers it), and R-9's circuit breaker — *stop if scope needs a declared field* — stays untripped,
> because nothing here requires the artifact to declare anything. The operator's flag remains a
> ceiling, never a floor.
