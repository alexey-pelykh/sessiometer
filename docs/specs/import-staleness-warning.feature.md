<!--
SPECIFICATION STUB — not executable.
This repo has no Gherkin runner; the executable gates are the Rust test suite and the
scripts/check-*.sh shell gates. These scenarios pin each acceptance criterion in scenario form and
bind it to an ACC capability from the Master Test Plan
(docs/design/migration-credential-portability-solution-design.md § 11). Do not read a written
scenario as a passing test.
-->

# Feature: import discloses that the artifact has a shelf life

Tracked as **issue #1002**. Requirements: PRD R-4, R-4a. Design § 4.2, AD-2.

**Rule under test**: the warning is **unconditional**. The hazard — the source refreshing after the
export, superseding every credential in the artifact — is *not detectable at the target*, so no
computed check may gate the warning. A derived expiry check is a supplementary symptom, never an
all-clear.

## Scenario: every credential-bearing import warns  · Cap-2.1

    Given a migration artifact carrying at least one credential
    When import runs
    Then the output warns that a source refresh after export invalidates the artifact
    And it states the safe sequence including "never resume the source"
    And the sequence names `use --force <label>`
    But not by naming `use <label>` unqualified
    But not gated on any freshness computation

> **Assert the `--force` token.** *Added 2026-08-05 (tenth pass).* The *And* clause above used to
> enumerate one element of the sequence ("never resume the source"), so a test author could emit
> `… → import → sessiometer use <label> → start` and pass. That string is a provable no-op against the
> already-active account (`src/use_account.rs:325-326`, pinned by
> `already_active_without_force_is_a_noop_success_with_zero_writes` at `:2490-2502`) — the operator
> restarts still holding the **stale** token, with `import` and `use` both reporting success. This is
> the 2026-07-31 incident, reproduced by the warning written to prevent it, on **every**
> credential-bearing import.

## Scenario: the warning fails closed when deadlines are unreadable  · Cap-2.2

    Given a migration artifact whose credential blobs carry no readable expiresAt
    When import runs
    Then the staleness warning is still emitted

## Scenario: an already-expired artifact reports the extra symptom  · Cap-2.3

    Given a migration artifact whose credential expiresAt is in the past
    When import runs
    Then the output additionally reports that the access token has already expired
    And the unconditional warning is still present
    But not phrased as a verdict on whether the artifact is usable

## Scenario: a still-valid artifact is not declared safe  · Cap-2.3

    Given a migration artifact whose credential expiresAt is an hour in the future
    When import runs
    Then the output does not state or imply that the artifact is fresh or valid
    And the unconditional warning is still present

    # This is the incident case: at import time the real artifact had ~55 minutes of access-token
    # validity left. It was not expired — it was superseded, which the blob cannot express.

## Scenario: no warning line leaks a secret  · Cap-6.1

    Given any artifact that triggers any warning above
    When the output is captured
    Then no line contains a token or an email address
