<!--
SPECIFICATION STUB — not executable.
This repo has no Gherkin runner; the executable gates are the Rust test suite and the
scripts/check-*.sh shell gates. These scenarios pin each acceptance criterion in scenario form and
bind it to an ACC capability from the Master Test Plan
(docs/design/migration-credential-portability-solution-design.md § 11). Do not read a written
scenario as a passing test.
-->

# Feature: roster entries from an artifact are validated before use

Tracked as **issues #1052, #1053**. Requirements: PRD R-15, R-16. Design § 4.9, RSK-11, OQ-5.

**Rule under test**: `account_uuid` arrives from the artifact validated for **non-emptiness and
uniqueness only** (`src/config/validate.rs:281-293`, reached via `apply_import`'s parse at
`src/cli.rs:4735`) and is otherwise interpolated into a keychain service name — its **shape and length
are unchecked**. Under scope selection the roster becomes the payload every import touches.

## Scenario: a malformed uuid is rejected before a stash name is derived  · Cap-11.1

    Given a roster entry whose `account_uuid` is malformed or over-length
    When the artifact is imported
    Then it is rejected before a stash name is derived from it
    And the error names the offending entry without printing credential material
    But not by asserting the empty-uuid case, which already ships

> The empty case is **already rejected** (`src/config/validate.rs:281-284`), so asserting it yields a
> test that is green over unimplemented work. Note the shipped rejection aborts the **whole artifact**
> with `ConfigInvalid` rather than naming one entry; the per-entry wording above is the *target*
> behaviour for #1052, not a description of today.

## Scenario: existing valid rosters still parse  · Cap-11.1

    Given a roster of real, previously-accepted account uuids
    When the new validation is applied
    Then every entry still parses

    # The constraint is checked against real data first. A validation rule that rejects the
    # operator's live roster is a worse defect than the one it fixes.

## Scenario: severity is not overstated  · Cap-11.1

    Given the validation work is described in code comments, commit messages, or docs
    When that description is read
    Then it is framed as input-validation hygiene
    But not as a path-traversal vulnerability

    # Verified: stash() reaches no filesystem path, and keychain service names are opaque strings
    # rather than hierarchical paths. A future reader acting on an inflated framing would hunt for a
    # filesystem bug that does not exist.

## Scenario: the `[credential]` incompatibility is legible  · Cap-11.2

    Given an artifact carrying a non-roster block the current parser does not know
    When it is read by the current binary on the artifact-config parse path
    Then that block is tolerated rather than aborting the import
    And the documented version floor states which releases cannot read a `[credential]`-bearing artifact
    But not by using `[credential]` as the unknown block — the current parser knows it
    But not by asserting anything about an already-shipped binary's message

> **The half you cannot test — do not write a test for it.** An earlier draft's *When* read "by a
> parser **predating** that block", which no test in this tree can realize: that parser is in a
> released binary we cannot patch (design § 4.9, § 14 R-16 — *"the released-binary half is
> unfixable"*). The **current** binary parses `[credential]` fine (`src/config.rs:1395`), and
> forward-tolerance is designed to keep it that way — so neither side of the boundary produces the
> asserted failure. **OQ-5** decides whether the tolerance + documented floor above is the whole
> deliverable; until it closes, assert only what this scenario now names.
