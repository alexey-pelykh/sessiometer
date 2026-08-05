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

## Scenario: the default narrows nothing  · Cap-7.2

    Given the same artifact
    When the operator runs `import` with no scope flag
    Then the same payload classes are applied as before the scope flags existed
    But not by asserting byte-identity with the pre-change target state

> **Scope-equivalence, NOT byte-identity — the distinction is the scenario.** AD-9 keeps the shipped
> default, so *scope selection* adds no narrowing here. But R-11's allowlist binds **regardless of the
> flag** (`design § 4.8`), and on a **fresh target** today's `import` adopts the artifact's config
> wholesale (`src/cli.rs:4744-4750`) — so a non-portable key that used to be adopted is now refused
> and reported. The design records this as a behaviour change on exactly this path (§ 8, `config
> adoption` row), and RSK-6 ships R-9 and R-11 as one unit, so a reader never sees the flags without
> the allowlist. A verbatim byte-identity assertion therefore goes **red**, and the cheapest repair is
> to exempt the no-flag path from the allowlist — which silently reinstates the unattended
> code-execution path PRD § 1 opens on. Assert the payload classes; leave the config outcome to
> Cap-8.1 and Cap-8.7.

## Scenario: `--settings` applies no roster entry and no credential  · Cap-7.9

    Given a target with NO existing config
    And an artifact carrying both a roster and non-roster config
    When the operator runs `import --settings`
    Then the allowlist-filtered non-roster config is applied
    But no roster entry is created or modified
    And no credential is written to the keychain
    But not by leaving the target's config state unstated

> **The *Given* must pin the target state, and OQ-7 is why.** *Added 2026-08-05 (eleventh pass); it
> read "an artifact carrying both a roster and non-roster config" with no target state.* On a target
> that **already has a config**, `apply_import` keeps the local config and discards the incoming
> non-roster blocks entirely (`src/cli.rs:4744-4750`), so the first *Then* — "the filtered config is
> applied" — is **unsatisfiable** there under OQ-7(a) and would make this scenario a red test over
> correct behaviour. The fresh-target path is where adoption happens. **OQ-7 decides whether a second
> scenario is owed on the existing-config target**; do not write one, or decide the question by
> implementing one, until it closes.

> **The mirror of Cap-7.1, and nothing else asserted it.** *Added 2026-08-05 (ninth pass).* R-9
> defines `--settings` as "(non-roster config)" and § 4.7 makes the flag a lattice meet that clears
> the accounts axis — but every capability asserted only the `--accounts` direction. The one
> `--settings`-plus-roster capability was **Cap-7.6**, which asserts the *empty-artifact report* and
> is **OQ-6-gated**, so under OQ-6(a) it may not be built at all.
>
> Without this scenario, `--settings` can ship as "apply everything, then allowlist-filter the config"
> with the roster branch untouched, and Cap-7.1, Cap-7.2, Cap-7.3 and every Cap-8.x still pass. An
> operator running `import --settings` to pick up tunables would get every credential in the artifact
> written to their keychain and every roster entry appended — under `--overwrite`, replacing live
> stashes. § 16b's orphan-capability check cannot see a *missing* assertion; only the forward read can.

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

## Scenario: an artifact whose roster is the payload of interest survives an unknown block  · Cap-7.4

    Given an artifact carrying `[[account]]` entries plus a non-roster block the parser does not know
    When it is imported with `--accounts`
    Then the import succeeds
    But not by calling this a roster-only artifact

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

## Scenario: `--accounts` against a roster-less artifact reports rather than no-ops  · Cap-7.10

    Given an artifact carrying non-roster config and NO `[[account]]` entries
    When the operator runs `import --accounts`
    Then the command reports that the artifact contains no accounts
    But not by erroring
    But not by silently reporting success having applied nothing

> **The mirror of the one case § 4.7 does specify, on the axis that is actually derivable.**
> *Added 2026-08-05 (eleventh pass); no requirement, criterion, capability, scenario or issue said
> what this does.* § 4.7 fixes the settings direction by name — *`import --settings` against a
> roster-only artifact reports "artifact contains no configuration" **rather than erroring or
> silently no-op'ing**"* — and that case is **OQ-6-gated**, because the settings axis may not be
> presence-derivable at all. The accounts axis is: OQ-6 itself says so (*"`[[account]]` entries are
> present or they are not"*), so this scenario is **not** OQ-6-gated and can be written today.
>
> The subject is reachable and first-class, not hypothetical: `require_roster()` binds only where it
> is called (`src/config.rs:1152-1158`), the committed
> `accepts_a_roster_less_config_and_preserves_tunables` (`src/config/validate.rs:1089`) pins the
> roster-less config as valid, `export` calls no roster guard, and `gather_payload`'s roster loop then
> yields an empty `accounts` (`src/cli.rs:4536-4546`). R-10b's own guard argument relies on all of
> this. An operator who removes their last account, exports, and runs `import --accounts` on the
> target to be conservative gets the exact "silently no-op" outcome § 4.7 forbids for the sibling axis.
