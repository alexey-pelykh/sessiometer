# Feature: an absent config is a first run only when nothing survives to say otherwise

Issue #1440 · PRD R-1 / R-5 / R-6 · design D-1

Example Mapping: 🟦 3 rules · 🟩 9 examples · 🟥 0 open

> On 2026-08-27 `login` found no `config.toml`, concluded first run, wrote one account, and notified
> a daemon still holding six. An absent config is **ambiguous** — "never configured" or
> "configuration disappeared" — and every write path resolves it the same way, unconditionally.
>
> The trap in this feature is that the two obvious fixes each fail on exactly one of the two
> scenarios below, and each passes the other convincingly. A guard that consults the control socket
> is correct while the daemon runs and degrades to *permissive* with it down. A guard that refuses on
> absent-config alone is correct after a loss and blocks a genuine first run. The scenarios are
> written so a fix that trades one for the other is visibly incomplete.

## Rule 1 — a surviving witness makes an absent config a loss, not a first run

```gherkin
Scenario: the incident, with the daemon down
  Given no config.toml
    And Sessiometer/* Keychain items exist for six accounts
    And the daemon is not running
   When an append-only verb is invoked
   Then it refuses and writes nothing
    # This is the case a socket-consulting guard gets wrong. With no daemon to ask, "no daemon" and
    # "no prior roster" are indistinguishable, so that guard falls back to permissive — precisely
    # where nothing else would have noticed either. The witness is read without the socket.

Scenario: the incident, with the daemon up and populated
  Given no config.toml
    And Sessiometer/* Keychain items exist for six accounts
    And the daemon is running and holds six accounts
   When an append-only verb is invoked
   Then it refuses and writes nothing
    And the daemon's roster is still six accounts
    # The live roster is the only surviving copy at this moment. The daemon corroborates the
    # refusal; it must never be what establishes it — see Rule 1's first scenario.

Scenario: a usage store alone is a witness
  Given no config.toml
    And no Sessiometer/* Keychain items
    And a non-empty usage sample store
   When an append-only verb is invoked
   Then it refuses and writes nothing
    # Two independent witnesses, either sufficient. Both demonstrably survived the incident. A
    # single-witness implementation is weaker than the evidence supports.
```

## Rule 2 — a genuine first run pays nothing

```gherkin
Scenario: a fresh machine
  Given no config.toml
    And no Sessiometer/* Keychain items
    And an empty or absent usage sample store
    And the daemon is not running
   When the operator captures their first account
   Then it succeeds and the roster contains one account
    # PRD AC-4, and the case the absent-config-alone route breaks. Note "the daemon is not running":
    # on a fresh machine it has never been started, which is why PRD § 7 row 1 had to be amended
    # (§ 7a) — as written it required refusal here, contradicting AC-4.

Scenario: the witness-absent path is byte-for-byte today's behaviour
  Given no witness of any kind
   When an append-only verb is invoked
   Then the resulting roster is identical to what the unguarded path produces
    # Planguage FirstRunFriction holds at its PAST baseline. If a first run gains a step, a prompt,
    # or a delay, the fix has taken the cost the rejected route was rejected for.

Scenario: a second account still widens the live roster
  Given a config.toml holding one account
    And the daemon is running and holds one account
   When a second account is captured
   Then the roster holds two accounts
    And the daemon holds two without a restart
    # Issue #139's path. notify_daemon_roster_reload exists because of it; a guard that blocks
    # widening breaks the fix it is built on.
```

## Rule 3 — reading the witness costs no privilege and no prompt

```gherkin
Scenario: the Keychain probe does not decrypt
  Given a locked login keychain
   When the witness is read
   Then it returns correctly
    And no password prompt is presented
    # src/keychain.rs:1293 already enumerates via `security dump-keychain` WITHOUT -d — metadata
    # only. A probe that decrypts would make refusal contingent on an unlocked keychain, which is
    # the one moment an operator is least able to supply it.

Scenario: the refusal names no secret
  Given a refusal is emitted
   Then it carries no account label, no path, no count, and no Keychain item name
    # The roster indexes credentials. config.toml carries no secret material (src/config.rs:17-20)
    # but is mode 0o600; a refusal message has a wider audience than the file.

Scenario: the parsed roster is not re-derived
  Given a config.toml holding six accounts
   When an append-only verb runs
   Then the roster used at the write is the one parsed at load
    # capture.rs:264 loads the config and keeps only `c.login`; :689-697 then rebuilds a roster from
    # Vec::new(). Three lines apart. Re-derivation is what made the fall-through invisible.
```
