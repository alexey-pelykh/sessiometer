# Feature: the orphan-stash census lists what no roster row names, from metadata alone (Tier B)

Issue #1553 · PRD REQ-STSH-B-004 / AC-STSH-004 / REQ-NTC-C-001 / REQ-NTC-C-002 / REQ-NTC-C-003 · design D-12

Example Mapping: 🟦 5 rules · 🟩 28 examples · 🟥 1 open

> **Provisional.** The PRD behind this file has `dor_status: failed`: every requirement it cites is
> unratified, and its problem framing is unvalidated. Nothing here is the operator's decision.
> The scenarios are Tier B at scope time: stated, and pending binding to any test.
>
> **Applies only if** the operator's `orphan-stash-policy` decision (#1552) is *report*, or *reap*
> with this command kept as the reaper's preview. Under *accept*, #1553 closes as not planned and
> none of this is built (see the open question at the end).
>
> **Oracle owed, not yet filed.** The deliverable is deterministic output, so an independent party
> owes it a pre-authored RED oracle before dispatch. That item waits on the open question and is
> filed once #1552 closes.
>
> A *stash* is two generic-password keychain items, `credential` and `oauthAccount`, under one
> service, `Sessiometer/<account_uuid>`. An *orphan* is a stash whose uuid no roster row carries.
> The census reads the metadata dump that the prior-configuration witness already runs, and no
> secret. The five rules are behaviours; #1553 pins them at seven seams, each with its own RED test.
>
> Three traps. A bare prefix test, which the witness uses, also collects names the roster could
> never hold. `list` reads an absent config as an empty roster, and a census that did the same
> would list every stash as an orphan. And reading the roster before the keychain lists a stash
> whose row was saved a moment earlier.

## Rule 1 — a service counts as a stash only if the roster could hold its uuid

```gherkin
Scenario: stashes, a half stash and a hex-rendered stash are collected; the rest are ignored
  Given a keychain dump holding these items
    | class | service                         | acct         |
    | genp  | Sessiometer/u-A                 | credential   |
    | genp  | Sessiometer/u-A                 | oauthAccount |
    | genp  | Sessiometer/u-B                 | credential   |
    | genp  | Sessiometer/u-C                 | credential   |
    | inet  | Sessiometer/u-D (as the server) |              |
    | genp  | Claude Code-credentials         | user-a       |
    | genp  | Sessiometer/u.E                 | credential   |
    And the dump renders the service of u-C as hex, not as quoted text
   When the stash services are collected from the dump
   Then the collected uuids are exactly u-A, u-B and u-C
    # AC: Census parse. u-A holds both items and u-B holds one, so the census counts services,
    # and a half stash is one entry.
    # The dump renders u-C's service as 0x53657373696F6D657465722F752D43.
    # `decode_attr_value` (`src/keychain.rs:709`) reads that form like a quoted one; a reader of
    # the quoted form alone would miss u-C.
    # The block split and the generic-password filter are the witness's own, so the two paths
    # cannot disagree about what an item in the dump is.
    # `any_service_with_prefix` (`src/keychain.rs:779`) is where the witness applies them.
    # The two differ in one place, on purpose. The witness asks whether any service starts with
    # the prefix, so it also says yes to u.E. The census asks for a uuid the roster could hold.

Scenario Outline: the part after the prefix must be non-empty and inside the roster's uuid rule
  Given one generic-password item whose service is `Sessiometer/` followed by <suffix>
   When the stash services are collected from the dump
   Then the item is <outcome>
    # AC: Census parse. The rule is `account_uuid_violation` (`src/config/validate.rs:525`):
    # ASCII letters, digits, `-` and `_`, up to a length limit.
    # The limit is `ACCOUNT_UUID_MAX_LEN` (`src/config/validate.rs:487`), which is 128 bytes.
    # `account_uuid_violation` returns no violation for the empty string. The roster rejects an
    # empty uuid in a separate check, so the census needs its own non-empty test.
    # A dot is outside the alphabet on purpose: it makes the `../x` shape of issue #1052
    # expressible. An email-shaped name needs `@` and `.`, and both are outside it, so none can
    # reach the output.

  Examples:
    | suffix         | outcome       |
    | nothing at all | not collected |
    | u.E            | not collected |
    | u/F            | not collected |
    | u@G            | not collected |
    | 129 letters a  | not collected |
    | 128 letters a  | collected     |
```

## Rule 2 — an orphan is a collected stash that no roster row names, in ascending byte order

```gherkin
Scenario: the roster's uuids are subtracted from the collected ones
  Given the collected uuids u-A, u-B and u-C
    And a roster whose rows carry u-A and u-C
   When the orphans are computed
   Then the orphans are exactly u-B
    # AC: Subtraction. A rostered uuid is never returned.

Scenario: a disabled row still names its stash
  Given the collected uuids u-A and u-B
    And a roster whose rows carry u-A, enabled, and u-B, disabled
   When the orphans are computed
   Then there are no orphans
    # AC: Subtraction. A row carries its uuid whether or not it is enabled. A filter on the
    # enabled flag would list u-B.

Scenario: a roster row with no stash is not reported
  Given the collected uuid u-A
    And a roster whose rows carry u-A and u-Z
   When the orphans are computed
   Then there are no orphans
    # AC: Subtraction. The inverse orphan, a row whose stash is gone, is out of scope for #1553.
    # This also keeps the subtraction from becoming a symmetric difference, which would list u-Z.

Scenario: a roster that loads with no rows names nothing
  Given the collected uuids u-A and u-B
    And a config.toml that loads with no roster rows
   When the orphans are computed
   Then the orphans are u-A and u-B
    # AC: Subtraction. Loaded and empty is a roster; absent is not (Rule 4, the census says what
    # it cannot know).
    # A tunables-only file loads with an empty roster: see `resolve_roster` (`src/cli.rs:5697`).

Scenario: orphans come out in ascending byte order
  Given the collected uuids u-b, u-A and u-C
    And a roster with a row for none of them
   When the orphans are computed
   Then they are u-A, u-C and u-b, in that order
    # AC: Subtraction. Byte order puts every capital letter before every lowercase one.
    # A case-insensitive sort would give u-A, u-b, u-C.
```

## Rule 3 — the report states facts, one uuid per line, and nothing else from the keychain

```gherkin
Scenario: two orphans are listed one per line under a fixed header, with the in-progress line
  Given the orphans u-B and u-C
   When the report is rendered
   Then the text has one fixed header line
    And it lists u-B and u-C, each on a line of its own, in that order
    And it has one fixed in-progress line
    # AC: Render. The in-progress line says that an item written moments ago can belong to a
    # capture or login still in progress.
    # The wording of both fixed lines, and where the in-progress line sits, are the pull
    # request's. Its tests pin the bytes.
    # The in-progress line appears once, however many uuids are listed. The census cannot tell a
    # stash written an instant before its row from a true orphan, and this item reads no time
    # (spike #1554).
    # Fixtures stay clear of issue #15's meter. It flags an unbroken run of 20 or more letters
    # and digits at 3.5 bits a character or more.
    # The run length is `ENTROPY_MIN_RUN` (`src/redaction.rs:177`).
    # A hyphenated uuid's longest run is 12 (PRD A-6), so fixtures use hyphenated or short ids.
    # A roster-valid uuid that is one such run is still listed like any other.

Scenario: no orphan is said plainly
  Given no orphans
   When the report is rendered
   Then the text says that no orphan stash item was found
    And no line of it is a uuid
    # AC: Render. Whether the header and the in-progress line also appear here is the pull
    # request's to pin.

Scenario: nothing but the uuids comes out of the keychain
  Given a dump whose stash items also carry an account name, a comment and a label, each a marker
    And one orphan stash among them
   When the report is rendered
   Then the text holds the orphan's uuid and the fixed lines
    And none of the marker strings appears in it
    # AC: Metadata only, nothing else leaks; Text rules (replies name uuids only).
    # The `acct` values `credential` and `oauthAccount` are among the values that must not appear.

Scenario: the report's fixed prose states facts and gives no instruction
  Given the fixed strings of the report
   When each is scanned with the advisory scan (`scan_advisory_banned`, `src/framing_vocabulary.rs:456`)
   Then none contains a banned token or phrase
    # AC: Text rules. The banned list holds imperative words such as "remove", "fix", "avoid" and
    # "need".
    # `BANNED_TOKENS` (`src/framing_vocabulary.rs:182`) is the list, so the report may state what
    # is true and may not tell the operator what to do.
```

## Rule 4 — the census says what it cannot know

```gherkin
Scenario Outline: without a usable roster nothing is classified
  Given a keychain dump holding the stash `Sessiometer/u-A`
    And config.toml is <state>
   When the census runs
   Then it lists nothing
    And it says it cannot classify, and why
    And it does not say the roster is empty
    And it does not say that no orphan stash item was found
    And it exits non-zero
    # AC: Cannot classify. Without the roster every stash would read as an orphan. The item
    # itself marks this rule as pipeline-added.
    # `list` reads an absent config as its friendly empty state, so the census cannot reuse that
    # mapping: see `resolve_roster` (`src/cli.rs:5697`).
    # The exit status is non-zero. Which code, and the wording, are the pull request's.

  Examples:
    | state                                  |
    | absent                                 |
    | unreadable                             |
    | malformed (fails to parse or validate) |

Scenario: a failed dump is an error, not an empty report
  Given a config.toml that loads
    And the metadata dump fails
   When the census runs
   Then it reports the dump's error
    And it lists nothing
    And it does not say that no orphan stash item was found
    And it exits non-zero
    # AC: Cannot classify. A failure is never an empty report.

Scenario: the dump is read before the roster
  Given the dump read and the roster read each record when they ran
   When the census runs
   Then the dump read ran before the roster read
    # AC: Order. Both reads sit behind injectable functions, so a test can record which ran first.

Scenario: a stash whose row is saved between the two reads is not listed
  Given the dump holds `Sessiometer/u-N`
    And u-N's roster row is saved after the dump is read and before the roster is read
   When the census runs
   Then u-N is not listed
    # AC: Order. This is the reason for the previous scenario. Reading the roster first would
    # list u-N, because that read would predate its row.
    # The opposite instant is the in-flight case: a stash whose row is not yet saved is listed,
    # and the fixed in-progress line (Rule 3, the report states facts) says so.
```

## Rule 5 — the census is read-only and additive: nothing else changes

```gherkin
Scenario: the only keychain call is the metadata dump
  Given a locked login keychain
   When the census runs
   Then the only `security` call is `dump-keychain` on the login keychain, with no `-d`
    And no password prompt is raised
    And no keychain item is read for its secret, written or deleted
    # AC: Metadata only, nothing else leaks; Invariant: no rostered stash is ever deleted.
    # Graded at the argv, not against a real locked keychain.
    # `the_metadata_dump_never_asks_security_to_decrypt` (`src/keychain.rs:2145`) passes unchanged.
    # The argv stays single-homed in `dump_keychain_args` (`src/keychain.rs:799`), so a second
    # copy of it cannot drift.
    # The census takes no `AccountStash` (`src/stash.rs:82`), the trait that carries `delete`, so
    # it has no way to delete anything.

Scenario: the witness reads the same, and its refusal still names nothing
  Given no config.toml
    And a `Sessiometer/<uuid>` stash in the keychain
   When the prior-configuration witness is read
   Then it answers as it did before
    And its refusal names no item, no count and no uuid
    And the census is not reachable from the refusal
    # AC: Invariant: the witness reads the same. Graded by tests that already exist and must
    # pass unchanged, even after a refactor of the shared parser.
    # They are `the_witness_decodes_a_hex_rendered_service` (`src/keychain.rs:2129`) and the
    # tests in `src/witness.rs`.
    # The witness keeps returning one yes-or-no answer: `any_stash_item_present` (`src/keychain.rs:814`).

Scenario: the census is asked for in one new spelling and every old spelling parses as before
  Given the spelling the pull request chose, a flag on `list` unless its own comparison weighs otherwise
   When the operator's arguments are parsed
   Then they parse to the census command
    And plain `list`, and every other verb, parse to what they parsed to before
    # AC: Parse. The spelling is the pull request's to pick and justify (#1553, The command's
    # surface).
    # A new verb adds a 20th help constant and moves the pinned count in
    # `every_help_constant_is_scanned` (`src/cli.rs:13639`). A flag on `list` adds none.

Scenario: an unknown flag on `list` is still rejected
  Given `list` with a flag it does not know
   When the operator's arguments are parsed
   Then they are rejected as a usage error
    # AC: Parse. `list_takes_no_flags_but_help` (`src/cli.rs:16236`) pins "no flags but -h" today.
    # A flag on `list` changes that premise, so the unknown-flag assertion has to survive its
    # rewrite.
    # The parser is `parse_list` (`src/cli.rs:358`).

Scenario: plain `list` on a machine with no config.toml stays offline and best-effort
  Given no config.toml
   When plain `list` runs
   Then it shows its friendly empty-roster message
    And it makes no keychain read
    # AC: Parse (plain `list` exactly as before; #1553, The command's surface).
    # `resolve_roster_maps_an_absent_config_to_the_friendly_empty_state` (`src/cli.rs:6514`)
    # pins today's mapping.
    # The honest-failure rule (Rule 4, the census says what it cannot know) must not leak into
    # plain `list`: the two read an absent config differently on purpose.

Scenario: the census's help text states facts
  Given the help text that describes the census spelling
   When it is scanned with the help scan (`scan_help_banned`, `src/framing_vocabulary.rs:449`)
   Then it contains no banned token or phrase
    # AC: Text rules. The existing scan covers a new flag or verb on its own.
    # `every_help_constant_is_scanned` (`src/cli.rs:13639`) walks every `*_USAGE` constant.
    # `LIST_USAGE` (`src/cli.rs:1166`) is one of them.
```

## Criteria with no scenario, and the check that decides each

- **No wire or golden change.** The byte-pinned wire and CLI-render goldens already run in
  `cargo test`. A golden edit already needs its re-baseline trailer, which
  `scripts/check-cli-golden-rebaseline.sh` and `scripts/check-panel-golden-rebaseline.sh` enforce.
- **Linux.** A container build and test run as a non-root user (ADR-0029), with the image, the
  commands and each `test result: ok` line quoted in the pull request. No CI job compiles for
  Linux until #964.
- **Real path.** A macOS-gated test over a throwaway keychain seeds one rostered stash and one
  orphan and asserts that the census over the real `dump-keychain` text lists exactly the orphan.
- **Pointers.** The surfaces #1552 marks corrected also name this command's spelling;
  `git grep -n -e '<spelling>'` lists them.
- **Local gates.** The five commands in `CLAUDE.md` § Before you push, plus the `msrv` and `deny`
  jobs, because the change is under `src/**`.

## Open — 🟥 1

**`orphan-stash-policy` (#1552) — the operator decides: reap, report or accept.** The PRD, the
design and the code do not settle it, and #1552 is open.

- *report*: every rule above applies.
- *reap*: the census (Rules 1 and 2) is the reaper's input, as #1553 says. Whether Rules 3 to 5
  stay with this command as its preview is the record's to say. #1553 holds that only "unless
  the record says otherwise".
- *accept*: nothing is built, and #1553 closes as not planned.

No expected outcome above is guessed. Each is stated for the case where this command is built.
The design recommends *report* (SEC-3), which is advice and not the decision.

Left to the pull request, as #1553 says, and so not open: the spelling of the command, the
wording of the fixed lines, and the exit codes. The scenarios hold for any choice among them.
