# Feature: a write that gives an account a label the recorded rule deems confusable with another account's says so once, and changes nothing else

Issue #1577 · PRD REQ-LBL-B-001 / REQ-LBL-C-002, REQ-NTC-C-001 / C-002 / C-003, AC-LBL-001 / AC-LBL-003, AC-NTC-002 / AC-NTC-003 · design D-1 (the F8 hook), D-3, § 8.5, § 11.5 CAP-19, § 12.2 `label-normalization-policy`, § 14.3 RK-10, § 14.4 SP-3

Example Mapping: 🟦 5 rules · 🟩 45 examples · 🟥 6 open

> **PROVISIONAL · Tier B.** The PRD's `dor_status` is `failed`, so every requirement this stub rests
> on is unratified and its problem framing is unvalidated. Each scenario waits to be bound in #1577's
> execution; none is a test yet. An example is a scenario or an outline row.
>
> **Every scenario holds only where the decision record adopts confusable detection.** That record is
> #1575's. It answers `label-normalization-policy`, and the question is open. If the record adopts
> nothing, #1577 closes as not planned (its own *Invalidated if*) and this file is withdrawn with it.
> The stub does not guess the answer. Adoption is the Red card
> `label-normalization-policy/adopt-detection`, stated once here; the scenarios say what the item says
> happens *where* it is adopted. A scenario marked 🟥 hangs on a further question inside that record.
> It names the question's handle and writes no outcome as decided; the item's own default sits in the
> comment beside it, and the 🟩 count leaves those examples out.
>
> The item is also blocked by #1576 (the source of skeletons) and #1556 (the collision rule with its
> key parameter, the plan seam, the template module, the exact-collision notice and the writer scan).
> This stub assumes #1556 has landed. It names what #1556 adds and does not restate it.
>
> The Blue Card Gate passes at its limit: five rules, which are the item's five Specification
> headings. PRD § 10.11's cross-cutting constraints (scanned templates, redaction, RED first) bind
> every feature, and sit here under the rule they constrain. A cut that gave them rules of their own,
> as #1556's stub does for its templates and its writer scan, would make seven and trip the gate. This
> stub does not make that cut, and reports the difference so that the call is visible.
>
> Tags in the comments. `RED-owed`: the item says the test must fail on the unmodified tree and the
> failure is recorded; its grader waits for the oracle (last section). `passes today`: a guard that
> holds on the unmodified tree. `no RED`: the item owes none, because the code under test does not
> exist before the change, or because the check is a gate.
>
> The traps in this feature:
> - A key that drops punctuation reports the operator's real roster. The operator holds email-shaped
>   labels that differ by one hyphen, each a separate account (PRD § 2.2). A report on every capture of
>   such an account trains dismissal of the report that matters (RK-10). Rule 2's hyphen rows and
>   Rule 3's quiet table exist to fail it.
> - The rule counts distinct labels per key, not bearers. A count of the key's bearers also rises on an
>   exact duplicate, which #1556's notice already reports, so it would say the same thing twice.
> - "More than before" earns its place. Without it, every refresh of an entry already in a pair says
>   it again.
> - The identity key hides all of it. Under the identity key every key group holds one label, so no
>   fixture that #1556 or `import` already has can fail here. These scenarios run with the project's key.
> - A fixture table written by the author of the key confirms only that the code matches the author's
>   belief about the standard. Rule 1's last scenario checks the key against the standard's own data.
> - The key must not leak into resolution. `alice` and `\u{430}lice` each name one account, and
>   neither is refused as ambiguous.
>
> Words. A *label* is the string an operator gives an account. A *bearer* of a label is a roster entry
> that carries it. The *key* of a label is what `label_key` returns for it, the skeleton under the
> definition the record states. It groups labels and changes none. A *confusable pair* is two entries
> whose labels differ in bytes and share a key. An *exact collision* is two bearers of one byte-equal
> label; the notice for it is #1556's. `A:alice` is an entry whose uuid is A and whose label is
> `alice`; A, B and C stand for whole uuids. `\u{430}lice` is the label whose first character is
> U+0430 CYRILLIC SMALL LETTER A, followed by `lice`: it looks like `alice` and differs in bytes.
> `alic\u{435}` ends with U+0435 CYRILLIC SMALL LETTER IE. `a-b@x.com` and `ab@x.com` are synthetic
> labels in the shape of the operator's real email-shaped labels; they name no real address. *The rule*
> is `label_collisions`, the pure function #1556 adds beside `Account` (src/config.rs:360) and this
> item extends. *The written entry* is the one entry a `capture` or `login` creates or relabels. The
> *harvested* account is the one a `capture` snapshots or a `login` lands on. A *notice* is a line a
> write prints about a collision.

## Rule 1 — the key joins look-alikes, leaves real punctuation alone, and never fails

> REQ-LBL-B-001, with REQ-LBL-C-001 (the record states the definition). `label_key` is the item's
> suggested name for a pure function from a label to its key: the skeleton under the definition the
> record states (`internalSkeleton`, or `bidiSkeleton`), from the source #1576 names. It does no I/O,
> gives the same key for the same label on every call, never fails on any input, and has a pinned data
> version. It groups labels and does nothing else.
>
> The standard's data, `confusables.txt` version 18.0.0 dated 2026-08-06 (read on 2026-10-27), has the
> lines `0430 ; 0061 ; MA` (CYRILLIC SMALL LETTER A to LATIN SMALL LETTER A) and `0435 ; 0065 ; MA`
> (CYRILLIC SMALL LETTER IE to LATIN SMALL LETTER E). It has no line whose source is the hyphen-minus,
> the full stop, the low line, the commercial at or the plus sign, and none whose source is `a`, `l`,
> `i`, `c` or `e`. So `alice`, `\u{430}lice` and `alic\u{435}` share the skeleton `alice`, and
> `a-b@x.com` and `ab@x.com` keep two.

```gherkin
Scenario: a Latin label and its Cyrillic look-alike share a key
  Given the labels alice and \u{430}lice
   When the key is computed for each
   Then the two keys are equal
    And the two labels are different byte strings
    And computing either key again, on another thread, gives the same key
    # Covers AC `key-joins-the-lookalike`, first half. no RED: the function does not exist before
    # the change. The skeleton of both is alice: the data maps U+0430 to a and has no line for l, i,
    # c or e.

Scenario: 🟥 the hyphen pair keeps two keys
  Given the labels a-b@x.com and ab@x.com
   When the key is computed for each
   Then the two keys differ
    # 🟥 `label-normalization-policy/punctuation-significant`. The item's default is "no": the
    # expected result flips if the record makes punctuation insignificant (AC-LBL-001's BUT NOT).
    # The data has no line for the hyphen-minus, the full stop or the commercial at, so a plain
    # skeleton keeps all three. Covers AC `key-joins-the-lookalike`, second half. no RED.

Scenario Outline: the key is total
  Given the label <input>
   When the key is computed
   Then a key is returned
    And nothing panics

  Examples:
    | input                                                          |
    | the empty string                                               |
    | three combining marks only, U+0301 U+0301 U+0301               |
    | default-ignorable characters only, U+200B U+FEFF               |
    # Covers AC `key-is-total`. no RED. No value is asserted: the key of the last row is whatever
    # the definition gives, which the record states. U+200B..U+200F and U+FEFF have the property
    # Default_Ignorable_Code_Point in DerivedCoreProperties-18.0.0 (dated 2026-08-07, read on
    # 2026-10-27).

Scenario: an invisible character does not make a label different
  Given the labels alice and alice\u{200B}
   When the key is computed for each
   Then the two keys are equal
    # Follows the standard's definition as the item quotes it: "Remove any characters in X that have
    # the property Default_Ignorable_Code_Point". Not a numbered AC. It matters because the CLI keeps
    # a trailing U+200B (#1575 measured that Rust's `str::trim` leaves it in place), so such a label
    # can be stored beside alice and prints identically. The source's fidelity to this step is what
    # the next scenario measures. no RED.

Scenario: the key agrees with the standard's own data on every character the data lists
  Given a pinned copy of the standard's data file
    And the reference skeleton the spike's note names
   When the key is computed for every source character the data lists
   Then it equals the reference's skeleton for each of them
    And the run reports how many characters it compared, and a run that compared none fails
    # Covers AC `key-matches-the-pinned-data`: a differential test compares `label_key` with the pinned
    # data of the source the spike named, and fails on one planted disagreement. It is here for independence. A fixture written from the same
    # belief as the key (a hand-made table mapping U+0430 to a) is green while the key is wrong about
    # the standard: it confirms that the code matches the assumption, not that the assumption matches
    # the data. Mismatches the spike's note explains are listed, not hidden. Whether the repository
    # may hold a pinned copy of the data is the Red card `skeleton-source-policy`. no RED.
```

## Rule 2 — a write creates a confusable collision only when it adds a distinct label to a key

> REQ-LBL-B-001 with REQ-ROS-B-002 and REQ-ROS-Q-001. REQ-ROS-B-002 counts a write as creating a
> collision on a label when the label's bearers after the write are above one and above the number
> before. The item applies that before-and-after form to *distinct labels per key*: a write creates a
> confusable collision on a key when the number of distinct labels among the key's bearers is above one
> after the write and above its number before. The unit is the item's choice and is
> ratification-pending. REQ-ROS-B-002's text counts entries, and a count of the key's bearers rises on
> an exact duplicate as well. Under the identity key every key group holds one label, so this rule
> cannot fire for a writer that passes the identity key.

```gherkin
Scenario Outline: the rule with the project's key on <write>
  Given the project's key
    And a roster <before>
   When a write leaves the roster <after>
   Then the rule reports <collisions>

  Examples:
    | write                                                       | before                  | after                                  | collisions                                     |
    | a new uuid under a label whose key another entry bears      | A:alice                 | A:alice, B:\u{430}lice                 | one: alice and \u{430}lice, bearers A, B       |
    | a relabel onto such a label                                 | A:alice, B:spare        | A:alice, B:\u{430}lice                 | one: alice and \u{430}lice, bearers A, B       |
    | a third distinct look-alike joining a pair                  | A:alice, B:\u{430}lice  | A:alice, B:\u{430}lice, C:alic\u{435}  | one: three labels, bearers A, B, C             |
    | a pair arriving on an empty roster                          |                         | A:alice, B:\u{430}lice                 | one: alice and \u{430}lice, bearers A, B       |
    | an ordinary new account under a fresh label                 | A:alice                 | A:alice, B:bob                         | none                                           |
    | a swap of two labels between the pair's entries             | A:alice, B:\u{430}lice  | A:\u{430}lice, B:alice                 | none                                           |
    | a refresh of an entry already in a pair                     | A:alice, B:\u{430}lice  | A:alice, B:\u{430}lice                 | none                                           |
    | B relabelled to the label it already carries                | A:alice, B:\u{430}lice  | A:alice, B:\u{430}lice                 | none                                           |
    | the only bearer relabelled to a look-alike of its label     | A:alice                 | A:\u{430}lice                          | none                                           |
    | a new uuid under a label another entry bears byte for byte  | A:alice                 | A:alice, B:alice                       | none: an exact collision is #1556's            |
    | another bearer of a label that already has a look-alike     | A:alice, B:\u{430}lice  | A:alice, B:\u{430}lice, C:alice        | none: the exact collision on alice is #1556's  |
    | 🟥 the hyphen pair, a new uuid                              | A:a-b@x.com             | A:a-b@x.com, B:ab@x.com                | none (the item's default)                      |
    # Covers AC `calibration`: the item lists nine cases and this table has all nine, plus the
    # arrival on an empty roster, the only-bearer relabel and the byte-equal duplicate that the
    # mutation outline below and the import parity scenario need. The first four rows are the loud
    # ones: RED-owed. The rest are quiet: the rule does not exist before the change, so no RED.
    # The refresh row and the same-label row are one pure input; they differ at the plan seam,
    # where Rule 3 tells them apart. The 🟥 row hangs on
    # `label-normalization-policy/punctuation-significant`, and its default is quiet. The first
    # loud row is the shape of AC-LBL-001.

Scenario Outline: removing a clause, or swapping the unit, turns one fixture red
  Given the rule with <mutation>
   When the calibration fixtures above run
   Then the fixture <fixture> reports a collision, and the test fails

  Examples:
    | mutation                                              | fixture                                                    |
    | bearers counted in place of distinct labels           | a new uuid under a label another entry bears byte for byte |
    | the "more than before" clause removed                 | a refresh of an entry already in a pair                    |
    | 🟥 the key replaced by one that drops punctuation     | the hyphen pair, a new uuid                                |
    # Covers the mutation pass the item asks the pull request to record. The first row is also
    # detected by "another bearer of a label that already has a look-alike" (its bearers go 2 to 3).
    # The second is also detected by the swap row, because a swap inside a pair leaves two distinct
    # labels before and after, so "more than before" alone keeps it quiet. The third row presumes the
    # item's default for `label-normalization-policy/punctuation-significant`; if the record makes
    # punctuation insignificant, that key is the project's key and the row is withdrawn.

Scenario: every creation scenario in the import stub has a look-alike counterpart in the table above
  Given the scenarios of docs/specs/import-duplicate-label.feature.md that create or avoid a duplicate
   When each is rebuilt with look-alike labels in place of byte-equal ones
   Then each quiet one is quiet and each loud one reports exactly one confusable collision
    # Covers AC "Output conforms to import-duplicate-label.feature.md", verified by
    # concretize-and-compare and not by prose inspection. The pairs:
    #   a same-label, different-uuid import warns                      -> the first loud row
    #   a same-label, same-uuid import does not warn                   -> the only-bearer row
    #   an artifact carrying its own duplicate warns on a fresh target -> the empty-roster row
    #   an import that swaps two labels between accounts does not warn -> the swap row
    #   overwriting a duplicate the target already had does not warn   -> the refresh row
    #   deepening a duplicate the target already had DOES warn         -> the third-look-alike row
    #   an ordinary import of distinct labels                          -> the fresh-label row
    #     (a code test: `apply_import_stays_quiet_on_an_ordinary_import_of_distinct_labels`)
    # Import itself keeps the identity key, so the counterparts run at the rule seam with the
    # project's key and not through `apply_import` (src/cli.rs:5277). Import's own rule is
    # `label_bearers` (src/cli.rs:5245) with `created_ambiguity` (src/cli.rs:5425); the item must
    # keep its calibration: a swap and a refresh stay quiet, a write that creates speaks once,
    # deepening speaks. no RED.
```

## Rule 3 — the notice states the facts, once, in scanned templates, and names no email or token

> REQ-LBL-B-001, REQ-NTC-C-001 and REQ-NTC-C-002, design D-3 and § 8.5. CLI `capture` and `login`
> print one line per confusable collision, on stdout, after the confirmation line. The line shows each
> distinct label as given, names every bearer of the key by uuid, marks the written entry `(this one)`
> and lists it first, says the labels look alike and differ in bytes, and says nothing was merged,
> renamed or refused. It names no email address, token or credential-derived value, and it never
> contains the key. The item calls its wording a draft that copy review may refine inside the scan;
> the facts are binding. The item's draft, whose first label starts with U+0430 and which the real
> line carries as the characters themselves:
>
> `"\u{430}lice" and "alice" look alike and differ in bytes: <uuid-B> (this one) and <uuid-A>. Nothing was merged, renamed or refused.`
>
> At the baseline `confirmation` (src/capture.rs:688) and `login_confirmation` (src/capture.rs:508)
> return the confirmation line only, and no function returns a capture's or login's whole reply. The
> "pure reply function" that the item names as the seam is #1556's to add.

```gherkin
Scenario: a capture of a new uuid under a look-alike label prints the notice
  Given a roster A:alice
    And the signed-in account is B, which the roster does not hold
   When the operator runs capture with the label \u{430}lice
   Then the reply opens with the confirmation line it has today
    And one more line follows it, which shows alice and \u{430}lice, each as given
    And that line names A and B by uuid, B first and marked (this one)
    And that line says the labels look alike and differ in bytes
    And that line says nothing was merged, renamed or refused
   But the reply carries no second notice line
    # Covers AC `oracle-new-account-under-a-lookalike-label`, whose test name is
    # `capturing_under_a_label_that_looks_like_another_accounts_is_not_silent`, and AC
    # `notice-facts`. RED-owed. The verbs, arms and print sites: `run_capture` (src/capture.rs:546) and
    # `run_login` (src/capture.rs:777) plan through `plan_capture` (src/capture.rs:635), and the two
    # oracles cover both of its arms (append, relabel) and both print sites. A write that sets one
    # label creates at most one confusable collision, so "one line per collision" is one line here.

Scenario: a login that lands on a rostered account and gives it a look-alike label prints the notice
  Given a roster A:alice, B:spare
    And the login lands on B
   When the operator runs login with the label \u{430}lice
   Then the reply opens with the confirmation line it has today
    And one more line follows it, which shows alice and \u{430}lice, each as given
    And that line names A and B by uuid, B first and marked (this one)
    And that line carries the other facts of the previous scenario
    # Covers AC `oracle-revive-relabels-onto-a-lookalike`, whose test name is
    # `a_revive_that_relabels_onto_a_lookalike_label_is_not_silent`, and AC `notice-facts`. RED-owed.
    # Whether the confirmation also names the label B replaced belongs to #1556 and to
    # `login-mismatch-policy`; this scenario asserts neither way.

Scenario Outline: a write that adds no distinct label to a key prints exactly the reply it prints today
  Given a roster <before>
    And the harvested account is <account>
   When the operator runs <verb> with <label>
   Then the reply equals the reply the same write gives under the identity key
    And it carries no confusable notice

  Examples:
    | verb    | before                  | account                      | label            |
    | capture | A:alice                 | A                            | \u{430}lice      |
    | capture | A:alice, B:\u{430}lice  | B                            | no label         |
    | login   | A:alice, B:\u{430}lice  | B                            | \u{430}lice      |
    | capture | 🟥 A:a-b@x.com          | B, new to the roster         | ab@x.com         |
    | login   | 🟥 A:a-b@x.com          | B, new to the roster         | ab@x.com         |
    # Covers AC `oracle-new-account-under-a-lookalike-label` ("It must not fire when the harvested
    # uuid is A itself": row 1, where A's relabel leaves one bearer), AC `calibration` at the reply
    # seam (rows 2 and 3: a refresh of an entry already in a pair, and a relabel to the label it
    # carries), and AC `hyphen-pair-stays-quiet` through `capture` and through `login` (rows 4 and 5).
    # Comparing with the identity-key reply keeps these true whatever #1556 adds to the reply. The two
    # 🟥 rows hang on `label-normalization-policy/punctuation-significant`; the item's default is
    # quiet, and the item says the hyphen pair under a key that drops punctuation must redden a test.
    # Rows 1 to 3: passes today. Rows 4 and 5: passes today under the default.

Scenario: a third look-alike names all three bearers
  Given a roster A:alice, B:\u{430}lice
    And the signed-in account is C, which the roster does not hold
   When the operator runs capture with the label alic\u{435}
   Then one notice line follows the confirmation line
    And it shows alice, \u{430}lice and alic\u{435}, each as given
    And it names A, B and C by uuid, C first and marked (this one)
    # Covers AC `notice-facts` ("names every bearer of the key") and the calibration's third
    # look-alike at the reply seam. The order of A and B after C is not pinned. RED-owed.

Scenario: the notice shows each label as given and never the key
  Given a roster A:\u{430}lic\u{435}
    And the signed-in account is B, which the roster does not hold
   When the operator runs capture with the label \u{430}lice
   Then the notice shows \u{430}lic\u{435} and \u{430}lice, each in its own bytes
    And the ASCII string alice, which is the key of both, appears nowhere in the reply
    # Covers AC `notice-facts` ("never contains the key"). The item's own fixture pair cannot test
    # it: the key of alice and \u{430}lice is the string alice, which is also the first label, so a
    # notice that printed the key would pass. Both labels here differ from their key. The fixture's
    # uuids must be hexadecimal, as Claude's are: validation admits any ASCII letters, digits, `-`
    # and `_` (`account_uuid_violation`, src/config/validate.rs:525), so a uuid is not barred from
    # containing the string. RED-owed.

Scenario: the confusable templates pass the advisory framing scan
  Given the confusable notice templates, each a named template in the module #1556 adds
   When each is scanned with `scan_advisory_banned` (src/framing_vocabulary.rs:456)
   Then none contains a banned token
    # Covers AC `notices-pass-the-scans`. no RED. The banned list is
    # `BANNED_TOKENS` (src/framing_vocabulary.rs:182): imperatives such as add, fix, avoid, need
    # and remove, value words such as warn, risk and safe, and recommendation words such as should
    # and must. A notice states facts and advises nothing. None of the item's draft words is on it.

Scenario: a template that is declared and not registered fails the tripwire
  Given a confusable template added to the module with no registration for the scan
   When the module's completeness tripwire runs
   Then it fails, and it names the template
    # Covers AC `notices-pass-the-scans`. RED-owed: the item asks for the failure on an unregistered
    # template. #1556 creates the tripwire, and this item's templates join its register.

Scenario: an injected banned token is caught
  Given a registered confusable template with a banned token appended
   When the scan runs over it
   Then the scan reports the token
    # The scan has to bite, or a green run proves nothing. Covers AC `notices-pass-the-scans`.
    # no RED.

Scenario: no confusable reply line names a harvested email, a token or a digest
  Given a fixture whose harvested email, token and credential digest the meter knows
    And the operator's own labels passed as the authored-label allow-set
   When each confusable reply line is rendered for it
   Then `assert_clean` (src/redaction.rs:256) passes over every line
   But a harvested address that is not in the allow-set fails the meter
    # Covers AC `no-email-no-token-no-digest` and AC-NTC-002. no RED. One label in the fixture is
    # shaped like an email address and was authored by the operator: it passes through the
    # authored-label allow-set (issue #444), because it is a label and not the harvested address.
    # The meter is the issue #15 redaction meter, test-only: `meter_fixture` (src/redaction.rs:122)
    # builds the fixture and `scan` (src/redaction.rs:190) takes the allow-set as a separate input.
```

## Rule 4 — the report changes nothing else

> The item adds a report and nothing more. A label resolves by byte-equal match as it does today
> (`resolve_target`, src/use_account.rs:486), no entry is merged, renamed, removed or refused, every
> other writer passes the identity key and prints what it prints today, an exact collision keeps
> #1556's notice, and `import`'s report is byte-identical. The PRD's invariant: normalization never
> changes an entry's uuid and never merges two entries. ADR-0034 fixes that the label is written
> verbatim and that validation accepts any non-empty label.

```gherkin
Scenario: a write never merges, renames, removes or rewrites anything but the label it was given
  Given generated rosters, including ones that hold look-alike labels, and any capture or login write
   When the plan is made
   Then every entry keeps its account_uuid, and none is removed
    And every label other than the written entry's is byte-identical to what it was
    And the label the operator gave is stored exactly as given, after the trim `plan_capture` already applies
    # Covers AC `never-merges-or-renames` and the Invariant. passes today: the property holds on
    # the unmodified tree and must hold after. It runs over generated rosters at
    # `plan_capture` (src/capture.rs:635). The key adds no rewrite; it appears in no stored value.

Scenario: look-alike labels each resolve to their own account
  Given a roster A:alice, B:\u{430}lice
   When a label is resolved
   Then alice names A alone, and \u{430}lice names B alone
    And neither is refused as ambiguous
    And an account uuid still names its account
    # Covers the Out of Scope line "`resolve_target` stays byte-exact" and keeps the key out of
    # the resolver. passes today. `resolve_target` (src/use_account.rs:486) matches
    # `account.label == query || account.account_uuid == query` and refuses on zero or many. It is
    # why the notice names uuids: typing alice never reaches B.

Scenario Outline: an exact collision and a confusable pair each get their own notice
  Given a roster A:alice
    And the signed-in account is B, which the roster does not hold
   When the operator runs capture with the label <label>
   Then the reply carries <notices>

  Examples:
    | label        | notices                                                                              |
    | alice        | the reply #1556 gives for an exact collision, byte for byte, and no confusable notice |
    | \u{430}lice  | the confusable notice, and nothing of #1556's exact-collision reply                  |
    # Covers AC `exact-duplicates-unchanged` (row 1: no RED, a change that must preserve behaviour)
    # and its converse (row 2: RED-owed), which the item numbers as `confusable-pair-gets-no-exact-reply`. It says the key
    # only groups, resolution stays byte-exact, and "the exact collision, if any, is #1556's".
    # Whether #1556's reply is a notice or a refusal is `duplicate-label-policy`'s; row 1 pins only
    # that this item changes nothing in it and adds nothing to it. A writer that handed the project's
    # key straight to #1556's rule would call alice and \u{430}lice one label and tell the operator
    # that alice now names two accounts, which is false: alice names A alone (the previous
    # scenario). By construction a write that sets one label cannot create both an exact and a
    # confusable collision: the new label either equals a rostered label byte for byte, which adds no
    # distinct label to its key, or it does not, which adds no exact bearer. So the item's "after
    # #1556's exact-collision notice if there is one" has no CLI fixture.

Scenario: import's report is byte-identical for every existing fixture
  Given the import fixtures `apply_import` already has
   When the report is rendered with the project's key in the tree
   Then each report is byte-identical to today's
    # Covers AC `exact-duplicates-unchanged`, second half. passes today. The report is
    # `import_report` (src/cli.rs:5650); its notice is
    # `duplicate_label_notice` (src/cli.rs:5628). The fixtures are the apply_import_* tests that
    # docs/specs/import-duplicate-label.feature.md names. They pass with no edit.

Scenario Outline: the other writers keep the identity key and their output does not change
  Given a roster A:alice, B:spare
   When <writer> gives B the label \u{430}lice
   Then its output carries no confusable report
    And its output equals what it would be under the identity key

  Examples:
    | writer                         |
    | the socket capture command     |
    | the socket config-set command  |
    | import                         |
    # Covers the item's "The writers": `import`, the socket `capture`, the socket `config-set`, the
    # menu-bar panel and the Settings app keep the identity key. The panel and the Settings app show
    # what the two socket replies carry, so they do not change either. The design specifies no
    # acknowledgement field, panel frame or Settings line for a confusable report: the Red card
    # `confusable-report-surfaces`. passes today. These rows are the ones that go red if a later item
    # hands a writer the project's key without a design.

Scenario: the writer scan fails when a writer passes a key other than the one listed for it
  Given the writers #1556's scan enumerates, each listed with the key it passes
    And `plan_capture` listed with the project's key
    And `apply_import` (src/cli.rs:5277) and `apply_settings` (src/config/settings.rs:37) listed with the identity key
   When a writer passes a different key
   Then the scan fails, and it names the writer
    # Covers AC `writer-scan-pins-the-key`: a writer that does not pass the project's key is a
    # listed fact and not a silent gap. RED-owed. The scan is #1556's, modelled on
    # `every_reconcile_roster_call_site_reaches_the_one_shared_floor` (src/daemon/commands.rs:4723);
    # this item extends its listing.

Scenario: the scan finds what it claims
  Given a copy of the source in which `plan_capture` passes the identity key
   When the scan runs over the copy
   Then it fails, and it names `plan_capture`
    # Covers AC `writer-scan-pins-the-key` ("show it failing when `plan_capture` passes the identity
    # key"). The canary runs through the same predicate the real assertion uses, over a deliberately
    # broken subject: a scan that matched nothing would return the same pass as a clean tree.
    # RED-owed.

Scenario: the handle-read tripwire stays green
  Given the change is applied
   When `every_handle_read_is_dispositioned` (src/use_account.rs:3362) runs
   Then it passes
    And any row this item adds to `HANDLE_READ_REGISTER` (src/use_account.rs:2888) carries its reason
    # Covers AC `handle-read-tripwire`. passes today. The tripwire reddens on a function that reads
    # a label and is not in the register. A function this item adds that reads a label from the
    # roster joins the register with a reason that says it groups labels and never resolves one.
```

## Rule 5 — a new source for the key passes the repository's licence and provenance gates

> REQ-LBL-C-002 ("Must, where it applies"), AC-LBL-003. `deny.toml` allows `MIT`, `Apache-2.0` and
> `BSD-3-Clause`, one exception (`unicode-ident` may carry `Unicode-3.0`), and only the crates.io
> index as a registry, with unknown registries and git sources denied. The `deny` job runs
> `./scripts/check-no-security-framework.sh` and `cargo deny check advisories sources licenses`. The
> source is #1576's finding. If it is a crate, this item adds one direct dependency; if it is a
> table, the item adds the generated table, its generator and a drift test.

```gherkin
Scenario: a crate source passes the deny job's checks
  Given the source #1576 names is a crate
   When `cargo deny check advisories sources licenses` and `./scripts/check-no-security-framework.sh` run over the change
   Then both pass
    And the crate's Cargo.lock entry names the crates.io registry as its source
    And its licence and its dependencies' licences are on the allow-list
   But not a dependency resolved from outside crates.io
   But not a licence the allow-list lacks
    # Covers AC `licence-and-provenance` and AC-LBL-003. no RED: it is a gate whose property is
    # that it passes. Decided mechanically by the existing `deny` job; no test is authored. The
    # check is also owed by any edit under src/** or to Cargo.toml and Cargo.lock.

Scenario: a table source fails when it disagrees with the pinned data
  Given the source #1576 names is a table generated from the data file
   When the table and a pinned copy of the data file disagree
   Then the drift test fails
    # Covers AC `licence-and-provenance`, table branch: "the drift test exists and fails on a table
    # that disagrees with the pinned data". no RED. The deny job polices crates and not vendored
    # data, so admitting the data is the Red card `skeleton-source-policy`.

Scenario: this item does not edit deny.toml
  Given the spike found that the source needs a licence the allow-list lacks
   When #1577 is picked up
   Then it waits for the operator's decision
    And its change touches no line of deny.toml
    # Covers "This item does not edit `deny.toml`". Mechanical aspect: `git diff --name-only` shows
    # no `deny.toml`, and `scripts/check-gate-change-ack.sh` demands a Gate-Change-Acknowledged
    # trailer for any edit to it. The decision is the operator's: the Red card
    # `skeleton-source-policy`. no RED.
```

## Red cards — the questions that no scenario above can close

| Blocked on | Question | Hangs on it | The item's own default |
|---|---|---|---|
| `label-normalization-policy/adopt-detection` | Does the #1575 record adopt confusable detection at all? | Every scenario. If not, #1577 closes as not planned and this file is withdrawn | none: the item is invalid without it |
| `label-normalization-policy/punctuation-significant` | Is punctuation insignificant to the key, so that `a-b@x.com` and `ab@x.com` are reported? | Rule 1's hyphen scenario; Rule 2's hyphen row and the drop-punctuation mutation row; Rule 3's two hyphen rows | no (AC-LBL-001's BUT NOT; PRD § 8.1 calls punctuation folding wrong as a default) |
| `label-normalization-policy/ascii-lookalikes-count` | Do ASCII-only look-alikes count? A plain skeleton joins the synthetic labels `bob1@x.com` and `bobl@x.com`, `bob0@x.com` and `bobO@x.com`, `tom@x.com` and `torn@x.com`, because the data maps 1 to l, 0 to O and m to rn | No scenario asserts an ASCII-only pair, so none is written. A "no" makes the key a custom rule and not the standard's skeleton, which changes what Rule 1's last scenario compares against | none ("the record's call") |
| `label-normalization-policy/skeleton-definition` | `internalSkeleton`, or `bidiSkeleton`, which first reorders a string for display? | The key's definition and Rule 1's last scenario. No scenario here uses mixed-direction text | none |
| `skeleton-source-policy` | If no crate passes the checks as the repository stands, does the operator grant a `deny.toml` licence exception, or admit the Unicode data into the repository (a generated table, or a pinned copy of the data file for the drift test and Rule 1's last scenario)? No design stub names this decision | Rule 5; Rule 1's last scenario | the item waits (#1576 leaves the exception to the operator) |
| `confusable-report-surfaces` | Do the socket `capture` and `config-set` acknowledgements, the panel capture card, the Settings app and `import` also report a confusable pair? Neither the PRD nor the design specifies a field, a frame or a line, and `docs/requirements/gui-cli-capability-parity.md` R-1 stands as a constraint on CLI reports | Nothing in this file: Rule 4 pins today's silence on those surfaces, and a later design changes it | out of scope; a follow-up needs a design first |

The four `label-normalization-policy/…` handles are the answers #1575's Specification item 6 asks the
record to give if it adopts detection, plus adoption itself. `skeleton-source-policy` and
`confusable-report-surfaces` are handles coined here: design § 12.2 has no decision stub for either.

## Not pinned by the item

An oracle must not assert these. Each is the pull request's choice, or follows from something the
item does not spell out.

- The notice's bytes beyond the facts the item calls binding, its list punctuation for three or more
  labels, and the order of the other bearers after the written entry.
- The name of the key function (`label_key` is a suggestion), the template module, and the template
  names.
- The exit status of a write that printed the notice. No criterion states it.
- The order of this notice against #1556's previous-label text and #1557's shared-credential notice.
- Whether labels that differ only in case (`Alice@x.com` and `alice@x.com`) are reported. The data has
  no line for `A` (U+0041), so a skeleton alone keeps them apart; folding case is another rule of the
  record, with its own follow-up item.
- Locale and platform independence of the key. The item states it. A single run cannot vary the
  platform, and the Linux build is checked by hand (CLAUDE.md, #963 and #964).
- The names of tests other than the two oracles the item names.

## Acceptance criteria that no scenario expresses

- *Mutation pass*: replace the key by one that drops punctuation, count bearers in place of distinct
  labels, and remove the "more than before" clause, and show a test fail each time, recorded in the
  pull request. Rule 2's mutation outline names the fixture that fails for each.
- *Local gates*: `cargo fmt --all --check`, `cargo clippy --all-targets --all-features -- -D warnings`,
  `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --document-private-items`, `cargo build --verbose`
  and `cargo test --verbose`; plus the `msrv` job and both `deny` checks, because the change is under
  `src/**` and, for a crate source, edits `Cargo.toml` and `Cargo.lock`.
- *Recorded failures*: for each `RED-owed` scenario the failing output is posted on #1577 before the
  fix lands (REQ-NTC-C-003, AC-NTC-003).

## Where each group is asserted

Taken from the acceptance criteria, for whoever binds the scenarios.

- Rule 1: pure tests over `label_key`, and a differential test over the pinned data.
- Rule 2: one pure test over the collision rule with the project's key. The item's note applies: if
  #1556's rule returns only a label and its bearers, its result is extended, or a sibling function is
  added, so that each bearer's own label is visible under a non-identity key; the identity key never
  exercises that case.
- Rule 3: `run_capture` and `run_login` over the in-memory stash, `FakeAccountStash` (src/stash.rs:424),
  then the pure reply function that #1556 adds; the framing scan and its tripwire in the template
  module; the redaction meter.
- Rule 4: a property test over `plan_capture`; `resolve_target`; the `apply_import_*` tests; a source
  scan modelled on #1556's; `every_handle_read_is_dispositioned`.
- Rule 5: the `deny` job's two checks, or the table's drift test.
- Seam gap. No function returns a capture's or login's whole reply at the baseline, and neither the
  key parameter nor the writer scan exists. A test for Rules 2 to 4 written before #1556 lands cannot
  compile against them.

## Work class and the oracle

Work class: deterministic-output. The primary deliverable is a pure function and the reply lines it
feeds, which a mechanical grader can falsify. The dependency criterion is a gate that an existing
check decides.

Oracle: blocked on `label-normalization-policy`. No oracle item is filed. The scenarios marked
`RED-owed` assert that a notice exists, and whether one exists is what the record decides
(`label-normalization-policy/adopt-detection`). The record's other answers (punctuation, ASCII
look-alikes, skeleton definition) change none of their expected outputs: their fixtures are
cross-script pairs that every variant of the standard's skeleton joins, and none contains
punctuation. The oracle also needs seams that #1556 adds and that do not exist at the baseline: the
key parameter on the rule, the pure reply function, and the writer scan. It is filed once the record
adopts detection and #1556 has landed. The scenarios it blocks: Rule 2's four loud rows; Rule 3's
capture, login, third-look-alike, never-the-key and unregistered-template scenarios; Rule 4's
look-alike row of the exact-collision outline and both writer-scan scenarios.
