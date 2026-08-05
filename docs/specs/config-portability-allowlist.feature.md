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

    Given a target with NO existing config, so the artifact's config would otherwise be adopted
    And an artifact whose config sets `[refresh].claude_bin` to an attacker-chosen path
    When it is imported WITH `--settings`
    Then the target's saved config does not contain that value
    And the refusal is reported
    But not by running against a target that already has a config

    # `--settings` is the widest flag: the operator's explicit "yes, take the config" must still not
    # widen past the allowlist. This is the ceiling case, not the shipped one — Cap-8.7 below covers
    # the no-flag path that is the default.
    # The fresh-target Given is load-bearing: with an existing local config, apply_import discards
    # the incoming non-roster blocks anyway (src/cli.rs:4744-4750), so the Then passes with nothing
    # implemented. See Cap-8.3's note.

## Scenario: the allowlist binds with no flag at all, on a fresh target  · Cap-8.7

    Given a target with no existing config
    And an artifact whose config sets `[refresh].claude_bin` to an attacker-chosen path
    When the operator runs `import` with NO scope flag
    Then the target's saved config does not contain that value
    And the refusal is reported
    But not by requiring `--settings` to reach the allowlist

> **This is the shipped hazard, and no other scenario asserts it.** Today a fresh target adopts the
> artifact's config wholesale (`src/cli.rs:4744-4750`), and AD-9 keeps the no-flag path the
> **default** — so this is the path a real operator takes. R-11 binds the allowlist "regardless of the
> operator's scope selection", which includes selecting nothing. Cap-8.1/8.2/8.3 all put `--settings`
> in their *When*; an implementation that hangs the allowlist off the `--settings` branch passes every
> one of them while leaving PRD § 1's code-execution path reachable by default. This scenario is also
> what makes Cap-7.2's "no byte-identity" caveat enforceable rather than advisory.

## Scenario: a weaker KDF is refused, a stronger one accepted  · Cap-8.2

    Given a target with NO existing config, so the artifact's config would otherwise be adopted
    And a local KDF parameter set
    When an artifact carrying weaker parameters is imported with `--settings`
    Then the weaker values are refused
    But an artifact carrying stronger values is adopted
    And the refusal is reported
    But an artifact stronger on one knob and weaker on the other is refused as a block
    But not by comparing `kdf_memory_kib` alone
    But not by running against a target that already has a config

> **Cap-8.2 was the one allowlist scenario the tenth pass's fresh-target fix skipped.** *Added
> 2026-08-05 (twelfth pass).* That pass pinned Cap-8.1 and Cap-8.3 and its note named only "Cap-8.1
> carries the same defect" — Cap-8.2 sits between them and got neither the target-state *Given* nor a
> reported-refusal *Then*. On a target that already has a config, `apply_import` discards the incoming
> non-roster blocks entirely (`src/cli.rs:4744-4750`), so "the weaker values are refused" is true with
> no KDF comparison written at all.

> **"Stronger" is a partial order over two knobs, and only the incomparable case discriminates.**
> *Added 2026-08-05 (eleventh pass); this fed uniformly-weaker and uniformly-stronger pairs only.*
> `kdf_memory_kib` (`8..=1_048_576`) and `kdf_iterations` (`1..=16`) are independent `u32`s
> (`src/config.rs:985`, `:988`), so `1_048_576 / 1` against the shipped defaults `65536 / 3`
> (`:998-999`) is neither. A comparator written on the memory knob alone — the knob the requirement
> prose foregrounds, since it is what kills the 8 KiB downgrade path — passes both clauses above and
> then adopts that block, landing `kdf_iterations = 1`. That is a downgrade through R-11b, the clause
> written to forbid downgrades. Refuse the block and report it (R-11e); do not adopt the half that
> improved.

## Scenario: the target operator's conflict policy survives  · Cap-8.3

    Given a target with NO existing config, so the artifact's config would otherwise be adopted
    And an artifact carrying a `[migration].conflict_policy` different from the default
    When it is imported with `--settings`
    Then the target's saved policy is not the artifact's
    And the refusal is reported
    But not by running against a target that already has a config

> **The *Given* must be a fresh target, or every *Then* is already true with nothing built.**
> *Corrected 2026-08-05 (tenth pass); the Given read "a target whose `conflict_policy` was
> deliberately set".* A deliberately-set policy means `local` is `Some`, and `apply_import` then keeps
> the local config and **discards the incoming non-roster blocks entirely**
> (`src/cli.rs:4744-4750`) — so "the target's policy is unchanged" passes with **no allowlist code
> written at all**. The old comment stated the premise ("Today this cannot happen at all") and never
> drew the conclusion, and there was no report clause or `BUT NOT` to bite.
>
> The fresh-target path is where adoption actually happens and therefore where a refusal is
> observable. **Cap-8.1 carries the same defect and the same fix**; only Cap-8.7 pinned the target
> state before this pass. AD-11 and the D-1 dissent are unaffected — what changes is that the
> scenario can now fail.

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
> them exercises the default branch.
>
> **The subject is chosen when the classification table is built — this spec deliberately does not
> name one.** § 4.8 fixes three carve-out *keys* and leaves the rest of the classification to
> implementation, so at design time no concrete block is yet classified non-portable. Two blocks are
> candidates (`RawConfig` has eight: `account`, `tunables`, `jitter`, `refresh`, `login`, `stats`,
> `migration`, `credential`, `src/config.rs:1379-1396`): **`[jitter]`** and **`[credential]`** are the
> two PRD § 1 neither calls freely portable nor carves out. Neither is *decided* — do not read this
> scenario as deciding either. Take whichever the built table classifies `NonPortable`.
>
> **If the built table leaves no such block, this scenario has no subject — escalate to the ADR
> (#1003); do not invent one.** *Corrected 2026-08-05 (ninth pass); this offered "add a purpose-built
> fixture key" as the fallback.* That does not work, for the reason stated ten lines below: a key
> **not** in `Config` is rejected by `RawConfig`'s `deny_unknown_fields` (`src/config.rs:1378`) on the
> very `--settings` full-parse path this scenario uses, **before the allowlist is consulted** — so it
> yields `ConfigInvalid`, not the refusal line the *Then* observes. And adding a *real* `Config` field
> to serve a test is the same failure as reclassifying one. **Deciding a portability question by test
> pressure is the failure this scenario exists to prevent** — R-11f requires that call to be made in
> the ADR, not in a red test. `--settings` is in the *When* deliberately: the operator's widest flag
> must still not widen past the allowlist — the flag is a ceiling, never a floor (R-9a).
>
> **The *Given* says "classified non-portable", not "unclassified" — that distinction is the
> scenario.** *Corrected 2026-08-04 (fifth pass).* An earlier draft said *unclassified*, which is not
> buildable in either reading: a **known** `Config` key with no classification is a **compile error**
> by R-11d (Cap-8.4 asserts exactly that — there is no binary in which to run this test), and a key
> **not** in `Config` is rejected by `RawConfig`'s `deny_unknown_fields` (`src/config.rs:1378`) on the
> full-parse path that `--settings` uses, before the allowlist is ever consulted — so no refusal line
> could be emitted to observe. The default branch is reached by a key that *is* classified, and
> classified **non-portable**.
