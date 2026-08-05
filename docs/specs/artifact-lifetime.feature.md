<!--
SPECIFICATION STUB — not executable.
This repo has no Gherkin runner; the executable gates are the Rust test suite and the
scripts/check-*.sh shell gates. These scenarios pin each acceptance criterion in scenario form and
bind it to an ACC capability from the Master Test Plan
(docs/design/migration-credential-portability-solution-design.md § 11). Do not read a written
scenario as a passing test.
-->

# Feature: the operator can destroy a consumed migration artifact

Tracked as **issues #1049, #1048**. Requirements: PRD R-12, R-10b. Design § 4.9, RSK-9.

**Rule under test**: `import` reads the artifact and leaves it, while the plaintext warning advises
deleting it with no mechanism. Scope selection narrows what an import *applies*, not what the file
*contains* — `export` gains no narrowing flag (R-9c/AD-5) — and the artifact carries live credentials
regardless, so the gap does not narrow.

## Scenario: a successful import can destroy its source  · Cap-9.1

    Given an artifact that imports successfully
    When the operator ran `import --shred`
    Then the source artifact no longer exists on disk

## Scenario: a failed import does not destroy its source  · Cap-9.1

    Given an artifact whose import fails partway
    When `--shred` was requested
    Then the source artifact still exists

    # The artifact may be the only copy, and a failed import is exactly when it is needed again.

## Scenario: shred does not claim more than it delivers  · Cap-9.2

    Given the shred feature has shipped
    When its help text and documentation are read
    Then neither claims secure or forensic erasure

    # On APFS, overwrite-in-place does not reliably destroy the prior extent. Claiming erasure we do
    # not deliver is the same false-assurance failure AD-2 declines for staleness.

## Scenario: the plaintext warning matches what the tool can do  · Cap-7.8

    Given `--no-secrets` has been removed, so every artifact with a non-empty roster carries credentials
    When PLAINTEXT_WARNING is read
    Then its advice corresponds to a mechanism the tool actually provides

## Scenario: an empty-roster export does not claim to carry credentials  · Cap-7.8

    Given a config whose roster is empty
    When the operator runs `export --plaintext`
    Then the plaintext warning is not printed
    But not by removing the warning for the ordinary non-empty case

> **R-10 deletes the condition, not the hazard.** `export` prints the warning today as
> `if !no_secrets { … }` (`src/cli.rs:4475`), whose in-code reason is *"nothing to protect, so the
> warning would misinform"*. R-10 removes `no_secrets` — but an **empty roster** also yields zero
> credentials: `gather_payload`'s `else` branch builds one entry per roster account
> (`src/cli.rs:4533-4534`). A roster-less config is first-class (`require_roster()` binds only at
> `run`, `src/config.rs:1145-1158`; test `accepts_a_roster_less_config_and_preserves_tunables`,
> `src/config/validate.rs:1089`) and `export` calls no roster guard — reachable by removing the last
> account. Re-express the guard over the artifact's credential count; do not let it vanish with the
> flag.
