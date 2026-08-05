<!--
SPECIFICATION STUB — not executable.
This repo has no Gherkin runner; the executable gates are the Rust test suite and the
scripts/check-*.sh shell gates. These scenarios pin each acceptance criterion in scenario form and
bind it to an ACC capability from the Master Test Plan
(docs/design/migration-credential-portability-solution-design.md § 11). Do not read a written
scenario as a passing test.
-->

# Feature: `rotated` is emitted only where it carries information

Tracked as **issue #1004**. Requirements: PRD R-5, R-5a. Design § 4.4, AD-3.

**Rule under test**: `classify()` computes `rotated` as `seeded_rt != after_rt`
(`src/refresh.rs:434-437`) *before* the outcome is known. A `dead` outcome sets
`after_rt = Some("")`, so whenever the seeded blob carries a **parseable, non-empty** refresh token
the comparison is `non-empty != ""` and `rotated=true` is **guaranteed by construction** on that dead
line. The fix is to make that state unrepresentable, not merely unprinted.

> **Not *every* dead line — the two exceptions do not weaken the fix.** `rotated` falls through to
> `_ => false` when `refresh_token(seeded)` is `None` (an unparseable seeded blob), and an empty
> seeded token yields `"" != ""` → false. `Dead` is decided solely from `after_rt`
> (`src/refresh.rs:445`), so those dead lines carry `rotated=false`. An earlier draft said "on every
> dead line", which is the kind of absolute a test author checks and disproves in one case. The
> chosen remedy — move `rotated` into the `refreshed` variant so the state cannot be represented
> (AD-3, Cap-4.1) — is unaffected: it removes the field from *all* dead lines regardless of which
> value they would have carried.

## Scenario: a dead refresh emits no rotation claim  · Cap-4.1

    Given a refresh whose response clears the refresh token in place
    When the outcome is classified
    Then the outcome is dead
    And the emitted event carries no rotated value

    # The live line this comes from:
    #   outcome=dead expires_before=06:50:10Z expires_after=06:50:10Z rotated=true window_secs=0
    # window_secs=0 and expires_before == expires_after prove no exchange occurred, yet the line
    # claimed a rotation.

## Scenario: an errored refresh emits no rotation claim  · Cap-4.1

    Given a refresh that fails before a token is returned
    When the outcome is classified
    Then the emitted event carries no rotated value

## Scenario: a no-change refresh emits no rotation claim  · Cap-4.1

    Given a refresh whose response returns a non-empty token but does not slide the expiry
    When the outcome is classified
    Then the outcome is no_change
    And the emitted event carries no rotated value

> **The variant this file used to omit, and the omission is the failure mode Cap-4.1 names.**
> *Added 2026-08-05 (eighth pass).* The three scenarios around this one enumerated `dead`, `error` and
> `refreshed` — which is precisely the three-of-four shape Cap-4.1 warns against, on the file that
> pins Cap-4.1. `RefreshOutcome` has **four** variants (`src/refresh.rs:225-240`) and `NoChange` is
> live-emitted as `"no_change"` (`src/observability.rs:180`).
>
> It is not benign: `rotated` is decided by the **token** compare (`src/refresh.rs:434-437`) while
> `NoChange` is decided by the **expiry** failing to move past the seeded marker (`:448-452`) — two
> independent comparisons, so a `no_change` line can carry `rotated=true` for exactly the reason a
> `dead` line can. An implementation that suppresses `rotated` on `Dead`/`Error` only passes every
> other scenario here and leaves the field meaningless on a live log path.
>
> Do **not** add a `refreshed_not_restashed` scenario alongside it: that is an *event* outcome mapped
> from `Refreshed` (`src/refresh_tick.rs:843`), where `rotated` is real.

## Scenario: a successful refresh still reports rotation  · Cap-4.1

    Given a refresh that returns a new refresh token differing from the seeded one
    When the outcome is classified
    Then the outcome is refreshed
    And the emitted event reports that the token rotated
    But not by removing the field from the path where it is meaningful

## Scenario: the meaningless combination is unrepresentable  · Cap-4.1

    Given the RefreshOutcome type
    When a non-refreshed outcome is constructed
    Then no rotated value can be attached to it

    # Type-level, not formatting-level: a formatting-layer suppression can be reintroduced by a
    # later emitter change. But the type-level move is NECESSARY AND NOT SUFFICIENT: it passes with
    # every emitted line unchanged, because the field the three renders read is a sibling of outcome,
    # not inside it (src/refresh.rs:284). See the next scenario, which asserts the rendered lines.

## Scenario: all three rotation-emitting lines drop the field, not just `event=refresh`  · Cap-4.2

    Given a non-refreshed outcome on each of the three refresh mechanisms
    When each mechanism's event is rendered, and the status/watch wire is built
    Then no `rotated=` appears on the `refresh` line
    And none appears on the `poll_refresh` line
    And none appears on the `keep_warm` line
    And the versioned `status`/`watch` wire presents no rotation claim either
    But not by asserting only the type
    But not by asserting only `event=refresh`
    But not by treating the wire as a log line — it is versioned, with a Swift consumer

> **`rotated=` is emitted from three modules onto three lines, and reshaping `RefreshOutcome` removes
> it from none of them.** *Added 2026-08-05 (eleventh pass); every R-5 artifact scoped the fix to
> `classify` and this file asserted the type.* The renders read
> **`RefreshReport.refresh_token_rotated`** (`src/refresh.rs:284`) — a **sibling of** `outcome`, not a
> payload inside it — and interpolate it unconditionally at `src/observability.rs:2155`
> (`event=refresh`), `:2173` (`event=poll_refresh`) and `:2191` (`event=keep_warm`), fed from
> `src/refresh_tick.rs`, `src/daemon/refresh_fold.rs` and `src/daemon/keep_warm.rs`. The code states
> the multiplicity in terms: *"three separate refresh mechanisms, three separate event names"*
> (`src/observability.rs:2187`).
>
> Build AD-3 exactly as written — `RefreshOutcome::Refreshed { rotated }` — and Cap-4.1, the
> `classify()` unit tests and R-5's Planguage meter all pass while
> `outcome=dead rotated=false` keeps shipping on every keep-warm and poll-refresh line. **`keep_warm`
> is the worst of the three**: its own doc says it renders `refreshed_not_restashed` on a real mint
> and *"never renders `refreshed`"* (`src/observability.rs:1282-1284`), so there the field is
> meaningless on **every** outcome R-5 targets — while AC-5's carve-out (`refreshed_not_restashed`
> keeps `rotated`) exempts the one outcome where it is real. Assert on the rendered line.
