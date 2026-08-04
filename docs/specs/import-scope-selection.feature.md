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

    Given an artifact whose config carries every non-roster block populated
    When the operator runs `import --accounts`
    Then the non-roster blocks are still ignored

    # AD-6: on a --plaintext export nothing is authenticated, so any scope the artifact declares is
    # attacker-controlled. This scenario must stay meaningful even if a scope field is never added.

> **The *Given* names a buildable artifact, not an unrealizable one.** *Corrected 2026-08-04 (fourth
> pass).* It used to read *"an artifact that asserts, **by any means**, that it should be applied in
> full"* — which has no realization: `Payload` has exactly two fields with nowhere to assert scope
> (`src/migration.rs:199-210`); an invented `[scope]` block is rejected by `deny_unknown_fields`
> (`src/config.rs:1378`) on the default path and ignored by narrow-parse under `--accounts`. Any test
> written to it collapses into Cap-7.1. The assertion that *is* meaningful and buildable is the one
> above: a **maximally-populated** artifact still cannot widen `--accounts`. The ceiling-never-floor
> property is what Cap-7.3 exists to pin, and it does not need a declarable scope field to be real.

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

> **"Roster-only" means a `config_toml` carrying only `[[account]]` blocks — not an empty one, and
> not an empty `Payload.accounts`.** *Corrected 2026-08-04 (fourth pass); an earlier note here had
> this wrong in both directions.* The roster lives **inside** `config_toml`
> (`src/migration.rs:199-210`); `Payload.accounts` carries only per-account secrets keyed by uuid
> (`ManagedAccount` = `{account_uuid, credential, oauth_account}`, `:220-232` — no label). So:
>
> - an **empty `config_toml`** is roster-*less*, not roster-only — it withholds both axes;
> - an **empty `Payload.accounts`** withholds only the *credentials*: the committed test
>   `a_config_only_artifact_imports_accounts_as_roster_entries_without_a_stash`
>   (`src/cli.rs:10619-10638`) builds exactly that and still imports **two roster entries**.
>
> **What the Cap-7.6 test must build** is a `Payload` whose `config_toml` contains `[[account]]`
> entries and **no non-roster block**. That parses — every `RawConfig` field is `#[serde(default)]`
> including `account` (`src/config.rs:1377-1396`) — so it is constructible by hand. `export` cannot
> mint one, because it writes `config.render()` unconditionally (`src/cli.rs:4532`) and `render()`
> always emits `[tunables]` (`src/config/render.rs:370`), and AD-5 keeps it that way by giving
> `export` no narrowing flag (scenario above). Do not try to reach this precondition through
> `export`, and do not "fix" `render()` to make it reachable.
>
> **Open (OQ-6):** because a defaulted block is indistinguishable from a withheld one, the *settings*
> axis cannot be reliably presence-derived at all — `available(artifact).settings` is effectively
> always true for a self-minted artifact. That is a precision question, not R-9's circuit breaker:
> nothing here requires the artifact to declare anything, and the operator's flag remains a ceiling,
> never a floor. It must be settled before this scenario's sibling behaviour is implemented.
