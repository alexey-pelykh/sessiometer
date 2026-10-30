# Feature: a credential stored under a second roster entry is reported at the write that creates the pair (Tier B)

Issue #1557 · PRD REQ-STSH-B-001 / B-002 / Q-001 / Q-002 / C-001 / C-002 / C-003, REQ-NTC-C-001 / C-002, AC-STSH-001 / 002, AC-NTC-002 · design D-3, D-4, D-9, § 8.4, § 8.5, § 11.5

Example Mapping: 🟦 5 rules · 🟩 52 examples · 🟥 2 open

> **PROVISIONAL · Tier B.** The PRD's `dor_status` is `failed`, so every requirement this stub rests on
> is unratified and its problem framing is unvalidated. Each scenario waits to be bound in #1557's
> execution; none is a test yet. The stub maps the core of F3 only: the comparison, the report and the
> event. The CLI collision and relabel notices on the same writers are #1556, the `shared_with` field of
> the capture acknowledgement is #1559, and the menu-bar line is #1561. An example is a scenario or an
> outline row.
>
> **Oracle.** #1579 holds the RED tests for the examples that have a seam a test can name
> before the fix. The examples without one say "No pre-authored oracle" where they occur: the import
> examples that depend on how `import` derives the set of entries it wrote (the item leaves that shape
> open), the order of lines in the CLI reply and in the import report, the example that judges
> ambiguity on the roster after the write, the template examples (their module is created by whichever
> of #1556 and #1557 lands first), and the swap-lock example (no acceptance criterion owes it a RED).
>
> **Calls this stub makes.** Each waits for ratification and none is the operator's decision.
> - The PRD's questions DQ-5 and DQ-6 stand as design D-4 answers them: no start-up sweep, and both
>   channels, with the event log here and the acknowledgement field in #1559. No example depends on a
>   sweep. The socket examples assume the event channel.
> - On the item's second report line the written entry is `spare`. The line's own bytes and the item's
>   first-side rule need that. The paragraph under the item's three lines says `work`, which they
>   contradict.
> - The other rows of Rule 2's two tables follow from the naming rule. The item shows three report
>   lines and two event lines.
> - No example depends on `duplicate-label-policy` or `login-mismatch-policy`. Every write here keeps
>   the label of the entry it writes, adds an entry under a label no other entry bears, or is an
>   import. `duplicate-label-policy` governs `capture`, `login`, the socket `capture` and `config-set`,
>   and `login-mismatch-policy` governs a login that renames an account. An import is neither.
>
> The traps in this feature:
> - A detector that runs after a refresh passes every fixture that never refreshes. Rule 5's first
>   example refreshes.
> - A comparison that includes the written entry reports every re-capture as a pair. Rule 1 has an
>   example for it.
> - An error from one read that propagates turns a detector failure into a failed write. Rule 5 fails
>   reads on purpose and keeps the write.
> - A read counter that never sees a read passes the cost bound. Rule 5 requires an exact count for a
>   roster where every read succeeds.
> - An import that finds its written entries by label loses the pair whose labels are equal. Rule 1
>   has an example with equal labels.
>
> Words. An *entry* is a roster entry. `A:work` is an entry whose uuid is A and whose label is `work`;
> A, B and C stand for whole uuids, and `<uuid-B>` in a line stands for B's. *A roster* in a Given is
> the roster the write leaves; *a target roster* is the roster an import starts from. K and K2 stand for
> credential bytes that a test builds. No scenario quotes one. The *written entry* is the one entry a
> `capture`, a `login` or a socket `capture` stashed; an import writes one entry per credential it
> stashed. An *other entry* is every roster entry that is not a written entry, parked entries
> included. A *pair* is two entries whose stashes hold byte-identical credentials, at least one of them
> written. *The reply* is what the writing command prints; *the event* is the `credential_shared` line
> in the event log.

## Rule 1 — a write that puts a credential beside an identical one is detected at that write, on every path that writes a stash

> REQ-STSH-B-001 and AC-STSH-001. When a CLI `capture`, a CLI `login`, the daemon's socket `capture` or
> an `import` writes a credential into an entry's stash, the credential just written, still in memory,
> is compared with every other entry's stash. The comparison is a byte comparison. A match is a pair.
> The comparison runs after the roster is saved and the swap lock is released (Rule 5).

```gherkin
Scenario Outline: a CLI write of another entry's credential is reported in its reply and in the log
  Given a roster A:work, B:spare
    And A's stash holds K
   When <write> puts K into B's stash
   Then the reply says that "spare" and "work" hold the same credential
    And the log gains one credential_shared line, with via=<via>, account=spare and matches=work

  Examples:
    | write                   | via |
    | a capture               | capture |
    | a login that lands on B | login |
    # Covers AC `oracle-a-write-of-another-entrys-credential-is-reported` through the capture row. The
    # item suggests the test name `a_write_of_another_entrys_credential_is_reported`.
    # Covers AC `all-write-paths` through the login row.
    # The capture row is asserted at `run_capture` (src/capture.rs:546).
    # The login row is asserted at `run_login` (src/capture.rs:777).

Scenario: the socket capture reports a pair as an event that follows its own
  Given a daemon whose roster is A:work, B:spare
    And A's stash holds K
    And the account signed in to Claude Code is B
   When the daemon's socket capture writes K into B's stash
   Then the handler returns a list of events
    And the capture's own event is first in the list
    And one credential_shared event follows it, with via=socket_capture, account=spare and matches=work
    # Covers AC `all-write-paths` (socket) and AC `socket-handler-returns-a-list`.
    # Asserted at `perform_socket_capture` (src/daemon/commands.rs:280).
    # The handler returns one optional event today. The item widens that to a list, as the swap
    # handler's list already is.

Scenario: the run loop logs every event of the list, in order
  Given the run loop handles a socket capture whose list holds the capture's own event and one credential_shared event
   When the loop emits them
   Then the log holds the capture line first and the credential_shared line second
    # Covers AC `socket-handler-returns-a-list` ("the run loop emits every event best effort").
    # The loop's call is at `perform_socket_capture` (src/daemon/run_loop.rs:773).
    # Its emitter is `emit_best_effort` (src/daemon/run_loop.rs:86).
    # The fixture to extend is `run_loop_performs_an_authenticated_capture_and_writes_a_redacted_ack` (src/daemon.rs:18364).

Scenario: an import that writes a credential another entry already holds
  Given a target roster A:work
    And A's stash holds K
    And an artifact that carries K for a new entry B:spare
   When the import runs
   Then the import report gains the line that "spare" and "work" hold the same credential
    And the log gains one credential_shared line, with via=import, account=spare and matches=work
    # Covers AC `all-write-paths` (import).
    # Asserted at `apply_import` (src/cli.rs:5277) plus the verb's step after the roster save.

Scenario: an import that writes one credential for two entries
  Given an empty target
    And an artifact that carries K for B:spare and for C:extra
   When the import runs
   Then one pair is reported, between "spare" and "extra"
    And neither side is marked "(this one)"
    # Covers AC `all-write-paths` (import). The item compares the credentials the import wrote with
    # each other in memory first, and reads no stash for that.
    # Which of the two comes first is not pinned: both are written, and the item names the written
    # entry as the first side.

Scenario: an import finds its written entries by uuid, not by label
  Given a target roster A:work
    And A's stash holds K
    And an artifact that carries K for B:work
   When the import runs
   Then the pair is reported between B and A, with B as the first side
    And each side carries its uuid, because `work` has two bearers
    # No pre-authored oracle: the item leaves the shape of the written set open ("extend the per-account
    # report or return the written set separately"), so no test can name a seam for it before the fix.
    # Covers AC `all-write-paths` (import): "labels are not unique and must not be used to match."
    # A label cannot tell B from A, so a rule that matches on it misses this pair or reports it against
    # the wrong entry.

Scenario: an entry the import skipped is compared by what its stash holds
  Given a target roster B:spare
    And B's stash holds K
    And the conflict policy skips an entry the target already has
    And an artifact that carries K2 for B and K for a new entry C:extra
   When the import runs
   Then B is skipped and its stash still holds K
    And the pair "extra" and "spare" is reported
    # No pre-authored oracle, for the reason above.
    # The entries an import wrote are the ones whose stash it set. B's credential in the artifact was
    # not written, so it takes no part, and B's stash is read as an entry the import did not write.

Scenario Outline: credentials that differ at all are not a pair
  Given a roster A:work, B:spare
    And A's stash holds K
   When a capture writes <written> into B's stash
   Then no pair is reported
    And no credential_shared line is written

  Examples:
    | written |
    | K with its last byte changed |
    | K with one byte appended |
    # Covers the unslugged mutation AC. With the byte comparison inverted, these rows report a pair and
    # the first scenario of this rule reports none, so both fail.

Scenario: a parked entry is an other entry
  Given a roster A:work parked, B:spare
    And A's stash holds K
   When a capture writes K into B's stash
   Then "spare" and "work" are reported as holding the same credential
    # Covers the Specification's definition of an other entry: every roster entry except the one just
    # written, "parked entries included". `enabled` (src/config.rs:381) is the flag a parked entry clears.

Scenario: a capture of an entry does not pair it with itself
  Given a roster A:work
    And A's stash holds K
   When a capture writes K into A's stash again
   Then nothing is reported
    # The written entry is not an other entry. A capture of an account the roster already holds can
    # write the bytes its stash already holds, and a comparison that included the entry would report
    # every such capture.

Scenario: one write that matches two entries reports two pairs, and not the pair between them
  Given a roster A:work, B:spare, C:extra
    And A's and B's stashes both hold K
   When a capture writes K into C's stash
   Then two lines are reported, one for "extra" with "work" and one for "extra" with "spare"
    And no line is reported for "work" with "spare"
    And two credential_shared lines are written, one per reported pair
    # Covers the Specification ("One line is written per detected pair") and the comparison of the
    # credential just written. A and B already held K together, and that is not this write's to report.
    # The order of the two lines is not pinned.

Scenario: a socket capture that writes nothing returns its own event alone
  Given a daemon with no account signed in to Claude Code
   When the socket capture runs
   Then it is refused
    And the list holds the refusal event alone
    # Covers AC `socket-handler-returns-a-list` for `capture_failure` (src/daemon/commands.rs:437).
    # The fixture is `perform_socket_capture_refuses_when_no_active_account_and_writes_nothing` (src/daemon/commands.rs:2129).
    # Its assertion changes mechanically from one optional event to a one-element list.
```

## Rule 2 — the reply and the event name the same two entries, by label, and by uuid only beside an ambiguous label

> REQ-STSH-B-002, REQ-NTC-C-001, design D-3 and § 8.4 to § 8.5. One line per pair. The written entry is
> the first side. A side carries its uuid when its own label has more than one bearer in the roster
> after the write. When both sides carry one label, a single-entry write marks the entry it touched
> `(this one)` in the reply, and an import marks neither side. The event has no mark. It adds
> `account_uuid=` or `matches_uuid=` only for a side whose label has more than one bearer. The wording
> is the item's draft; the facts are binding. The rows below take the roster as given. Where a row has
> one label on two entries, the write is a refresh of an entry that already bore it, or an import.

```gherkin
Scenario Outline: the reply line when <case>
  Given a roster <roster>
    And <written> is written by <write> with the credential that <other>'s stash holds
   When the pair is rendered for the reply
   Then the line reads <line>

  Examples:
    | case                          | roster                           | write     | written | other | line |
    | both labels are unique        | A:work, B:spare                  | a capture | B       | A     | "spare" and "work" hold the same credential. |
    | only the other side ambiguous | A:work, B:work, C:spare          | a capture | C       | B     | "spare" and "work" <uuid-B> hold the same credential. |
    | both sides carry one label    | A:work, B:work                   | a capture | B       | A     | "work" <uuid-B> (this one) and "work" <uuid-A> hold the same credential. |
    | both ambiguous, labels differ | A:work, B:work, C:spare, D:spare | a capture | C       | A     | "spare" <uuid-C> and "work" <uuid-A> hold the same credential. |
    | an import, one label          | A:work, B:work                   | an import | B       | A     | "work" <uuid-B> and "work" <uuid-A> hold the same credential. |
    # Covers AC `report-wording-by-ambiguity`. The first three rows are the item's three lines, and the
    # second is the roster the item names: `work` on A, `work` on B, and `spare` holding B's credential.
    # The fourth row shows that the mark follows equal labels and not ambiguity: both sides are
    # ambiguous and neither is marked. The fifth shows that an import marks neither side.
    # Seam: the pure rendering function.

Scenario Outline: the event line when <case>
  Given a roster <roster>
    And <written> is written by <write> with the credential that <other>'s stash holds
   When the pair is rendered for the log
   Then the line is its timestamp followed by <line>

  Examples:
    | case                   | roster                  | write            | written | other | line |
    | both labels unique     | A:work, B:spare         | a capture        | B       | A     | event=credential_shared via=capture account=spare matches=work |
    | other side ambiguous   | A:work, B:work, C:spare | a capture        | C       | B     | event=credential_shared via=capture account=spare matches=work matches_uuid=<uuid-B> |
    | both sides one label   | A:work, B:work          | a socket capture | B       | A     | event=credential_shared via=socket_capture account=work matches=work account_uuid=<uuid-B> matches_uuid=<uuid-A> |
    | written side ambiguous | A:work, B:work, C:spare | a login          | B       | C     | event=credential_shared via=login account=work matches=spare account_uuid=<uuid-B> |
    | an import              | A:work, B:spare         | an import        | B       | A     | event=credential_shared via=import account=spare matches=work |
    | a label with a space   | A:work, B:my spare      | a capture        | B       | A     | event=credential_shared via=capture account=my spare matches=work |
    # Covers the unslugged grammar AC: "render the example lines above from fixtures and assert the
    # bytes, including a label that contains a space". The first and third rows are the item's two
    # event lines. The other rows show every `via` token and each placement of a uuid token.
    # A label is written verbatim, as ADR-0034 decides. The renderer is `Event::to_log_line` (src/observability.rs:2592).

Scenario: ambiguity is judged on the roster after the write
  Given a target roster A:work, B:work
    And A's stash holds K
    And the conflict policy overwrites an entry the target already has
    And an artifact that carries K for B, labelled personal
   When the import runs
   Then the line reads "personal" and "work" hold the same credential.
    # Before the import `work` has two bearers and after it one, so A carries no uuid. The rule reads
    # the roster after the write. Covers AC `report-wording-by-ambiguity`, and the item's "(the roster
    # after the write)".
    # No pre-authored oracle: a test hands the saved roster to the renderer itself, so it cannot see
    # which roster the verb hands over.

Scenario: the shared line follows the confirmation line and any collision notice
  Given a capture that writes a pair
   When the reply is printed
   Then the confirmation line is first
    And the shared line comes after it
    And a collision notice, when the same write prints one, sits between the two
    # Covers the Specification's "Each path" line for `capture` and `login`: confirmation, collision
    # notice, shared line. #1556 adds the collision notice, and the third Then applies once it exists.
    # No pre-authored oracle: no function returns a CLI write's whole reply yet, and its shape is the
    # fix's choice.

Scenario: the import report puts the shared line after its existing notices
  Given a target roster A:work
    And A's stash holds K
    And an artifact that carries K for a new entry B:work
   When the import runs
   Then the report lists the per-account lines, then the tally, then the duplicate-label notice, then the shared line
    # Covers the Specification's "Each path" line for `import`: "after its existing notices".
    # `import_report` (src/cli.rs:5650) renders the first three parts.
    # No pre-authored oracle: `import_report` takes the outcomes alone today, and where the pairs enter
    # it is the fix's choice.
```

## Rule 3 — the detector reports and changes nothing else

> AC-STSH-001 and its four "BUT NOT" lines, REQ-STSH-C-003, design D-4. The roster, the write and the
> active account are the same with and without the detector. The write is never refused. No entry is
> chosen, merged, parked or removed. The canary's verdicts do not change. A failed log write does not
> fail the write.

```gherkin
Scenario: a detected pair changes neither the roster, nor either stash, nor the active account
  Given a write that creates a pair
   When it runs with the detector and again without it, from the same starting state
   Then the roster file holds the same bytes both times
    And both stashes hold the same bytes both times
    And `resolve_account_for` answers the same entry both times
    # Covers the unslugged Invariant AC: "the detector never changes what is stored, nor which entry
    # is active". `resolve_account_for` is at src/active.rs:43.

Scenario: the write ends as it ends when no pair exists
  Given a write that creates a pair
   Then its outcome and count are those of the same write with no pair
    And a CLI write exits as it exits with no pair
    And the socket capture's acknowledgement has the same bytes as with no pair
    # Covers AC-STSH-001 ("BUT NOT refusing the write", "BUT NOT choosing, merging, disabling or
    # removing either entry"). #1559 is the item that adds a field to the acknowledgement.

Scenario: the canary still passes a canonical credential that two stashes share
  Given the same credential in two stashes and in the canonical item
   When the canary runs for the displayed account
   Then it passes
    # Covers AC `canary-unedited` and REQ-STSH-C-003.
    # The test is `shared_token_under_both_stashes_passes_via_the_stash_a_first_order` (src/canary.rs:790).
    # It passes with no edit.

Scenario Outline: a reader gives the same result for a log with a credential_shared line and for one without
  Given a log, and the same log with one credential_shared line added
   When <reader> reads both
   Then it returns the same result for both

  Examples:
    | reader |
    | `parse_events` (src/reliability.rs:1332), the reader behind `reliability` |
    | `parse_swap_events` (src/usage_stats.rs:421), the swap reader behind `stats` |
    # Covers AC `readers-ignore-the-new-event`. The item says both build a field map by token name and
    # ignore an event kind they do not know. The example pins that tolerance.

Scenario: `log --json` carries the line verbatim and its schema version stays
  Given a log that holds a credential_shared line
   When `log --json` renders it
   Then the record's `line` is that line, byte for byte
    And the record's `event` is credential_shared
    And `JSON_SCHEMA_VERSION` (src/log.rs:167) holds the value it holds today
    # Covers AC `readers-ignore-the-new-event` ("`log --json` carries the line verbatim").

Scenario: a log that cannot be written does not fail the write
  Given a write that creates a pair
    And the event log cannot be opened or written
   Then the write succeeds
    And the reply still carries the shared line
    # The item writes the CLI event line "best effort like the login audit line": open the log, emit,
    # ignore a failure. `emit_login_event` (src/capture.rs:359) is the model.
    # No acceptance criterion names a seam for this example.
```

## Rule 4 — nothing derived from a credential leaves memory, and every new string is a scanned template

> REQ-STSH-C-001, REQ-NTC-C-001, REQ-NTC-C-002 and AC-NTC-002. The comparison runs in memory. No
> digest, fingerprint, length or byte of a credential appears in a reply, a log line, a file or a wire.
> Every new operator-facing string is a named template in one module, and the module registers its
> templates with the framing scan under the advisory audience. No new function in `src/cli.rs` spells
> operator prose.

```gherkin
Scenario: the redaction meter passes over every new reply line and every new event line
  Given K is the credential the meter's fixture knows
    And the roster labels are the meter's authored-label allow-set
   When each reply line and each event line of Rule 2 is rendered for a pair whose credential is K
   Then `assert_clean` passes over every one
    And the corpus it scanned is not empty
    # Covers AC `no-credential-derived-value`. The meter is test-only.
    # `assert_clean` is at src/redaction.rs:256.
    # `meter_fixture` is at src/redaction.rs:122 and builds the credential this example uses.
    # A hyphenated uuid is not one long alphanumeric run, so the entropy check does not flag it. The
    # fixture uuids must be hyphenated.

Scenario: a digest of the credential in the same position would be flagged
  Given a line of Rule 2 rendered with the sha256 of K in place of a label
   When the meter scans it
   Then it reports a `BlobSha256` finding
    # Covers AC `no-credential-derived-value` ("a digest of the credential in the same position would
    # be flagged"). It shows that the instrument can fail. `BlobSha256` is at src/redaction.rs:158.

Scenario: the bytes of the credential occur in no new output
  Given K is the credential the meter's fixture knows
   When every new output of a write that creates a pair is collected
   Then the bytes of K, and every token inside it, occur in none of it
    # Covers the unslugged Invariant AC: "a test that scans every new output for the fixture
    # credential's bytes". The outputs are the reply lines, the event lines, the import report and the
    # socket capture's events.

Scenario: the length of the credential occurs in no new output
  Given K has a length in bytes that is a number occurring nowhere else in the output
   When every new output of a write that creates a pair is collected
   Then that number occurs in none of it
    # The item says nothing derived from the bytes leaves memory: "no digest, no fingerprint, no
    # length". The meter has no length check, so this example carries it.

Scenario: every new template passes the advisory framing scan
  Given the templates the item adds
   When each is scanned with `scan_advisory_banned` (src/framing_vocabulary.rs:456)
   Then none contains a banned token
    # Covers AC `templates-pass-the-scans`.
    # The advisory audience excuses one token. `ADVISORY_EXEMPT_TOKENS` is at src/framing_vocabulary.rs:296.
    # No pre-authored oracle: the template module does not exist yet.

Scenario: a template declared and not registered fails the module's completeness tripwire
  Given a template added to the module with no registration for the scan
   When the module's tripwire runs
   Then it fails, and it names the template
    # No pre-authored oracle: the module is created by whichever of #1556 and #1557 lands first, so a
    # test cannot name it yet. Covers AC `templates-pass-the-scans`.
    # The tripwire in `src/cli.rs` covers that file only. Its register is `INLINE_PROSE_REGISTER` (src/cli.rs:14321).

Scenario: an injected banned token is caught
  Given a registered template with a banned token appended
   When the scan runs over it
   Then the scan reports the token
    # The scan has to bite, or a green run proves nothing. Covers AC `templates-pass-the-scans`.
    # No pre-authored oracle: the template module does not exist yet.

Scenario: no new function in src/cli.rs spells operator prose
  Given the change is applied
   When `every_function_spelling_inline_prose_is_dispositioned` runs
   Then it passes with no edit to its pinned counts
    # `every_function_spelling_inline_prose_is_dispositioned` is at src/cli.rs:14420.
    # Covers AC `templates-pass-the-scans`. That file's unscanned prose is a gap #1167 tracks, and this
    # item neither widens it nor closes it.
```

## Rule 5 — the pair is found at its creating write, at a bounded read cost, and a failed read is silent

> REQ-STSH-Q-001, REQ-STSH-Q-002, AC-STSH-002, design D-4. The pair is reported at the write that
> creates it, before either entry is refreshed. A refresh rotates the refresh token, so a pair a
> refresh has already split is a miss the item accepts. Each other entry's stash is read at most once
> per write, after the roster is saved and the swap lock is released. No poll and no swap reads a stash
> for this. A stash that cannot be read is skipped, and the absence of a report is never a claim that
> no pair exists.

```gherkin
Scenario: a pair is reported at its creating write and not again when a refresh splits it
  Given a roster A:work, B:spare
    And A's stash holds K
   When a capture writes K into B's stash
   Then the pair "spare" and "work" is reported at that write
   When B's stash is then rewritten with K2, as a token refresh does
   Then nothing further is reported
    And neither entry's roster row or stash is changed by the detector
    # Covers AC `window` and REQ-STSH-Q-001. A mutation that moves the comparison to after a refresh
    # fails the first Then, because K and K2 differ by then.
    # A pair that a refresh splits before any write reports it is a known miss and an accepted residual.

Scenario: a refresh between the write and the comparison does not hide the pair
  Given a roster A:work, B:spare
    And A's stash holds K
    And a capture has written K into B's stash and saved the roster
   When B's stash is rewritten with K2 before the comparison runs
    And the comparison runs on the credential the write held in memory
   Then the pair "spare" and "work" is reported
    # Covers the Specification ("the just-written Credential, still in memory"). The comparison uses
    # the credential in memory and never re-reads the written entry's stash, so a refresh that lands
    # after the write cannot hide the pair. A mutation that moves the comparison after a refresh turns
    # this example red.

Scenario: each other entry is read once per write, and the written entry not at all
  Given a roster A:work, B:spare, C:extra, D:new
    And a stash that counts its reads by service
   When a capture writes K into D's stash
   Then the detector reads A's, B's and C's stashes once each
    And it does not read D's stash
    # Covers AC `cost` and REQ-STSH-Q-002. "At most once" is the bound. Every other entry must still be
    # compared, so with every read succeeding the count is exactly one each. A detector that reads
    # nothing fails here as well as in Rule 1.

Scenario: an import reads each entry it did not write once, however many credentials it wrote
  Given a target roster A:work, B:spare, C:extra
    And a stash that counts its reads by service
   When an import writes new entries D and E
   Then the detector reads A's, B's and C's stashes once each
    And it does not read D's or E's stash
    # Covers AC `cost` (import).
    # `write_and_verify` (src/cli.rs:5451) reads each written stash back once for the import's own check.
    # Those reads are not the detector's, and this example counts the detector's alone.

Scenario: the detector is called from exactly four places, and none runs on a poll or a swap
  Given the production source of the crate
   When the detector's call sites are counted
   Then there are four: CLI capture, CLI login, the socket capture and import
    And in each of those four functions the call comes after the roster save and before the reply is printed
    And the walk finds all four, so it cannot pass over an empty corpus
    # Covers AC `cost` (the source scan), AC-STSH-002, and the Specification's order: after the save.
    # The model is the scan `every_roster_verb_declares_the_intent_its_verb_implies` (src/capture.rs:2330).
    # Its crate-wide walk is `crate_notify_mentions` (src/capture.rs:2452). It cuts each file above its
    # test block and checks the comment strip with a canary.
    # The oracle pins the count and the four host functions. The order clause is checked in review,
    # because the needles it needs depend on the fix's code shape.

Scenario: a fifth call site fails the scan
  Given a call to the detector added to a function that a poll tick runs
   When the scan runs
   Then it fails
    # Covers AC `cost`: "it fails when a fifth call site appears". RED is owed for the scan.

Scenario Outline: a stash that cannot be read is skipped, and the other comparisons still run
  Given a roster A:work, B:spare, C:extra
    And A's stash fails to read with <failure>
    And B's stash holds K
   When a capture writes K into C's stash
   Then the pair "extra" and "spare" is reported
    And nothing is printed or logged about A
    And the write succeeds

  Examples:
    | failure |
    | an absent half of the stash |
    | a locked keychain |
    # Covers AC `failed-read-is-silent`. The two failures are `Error::StashIncomplete` and
    # `Error::KeychainLocked`. A detector that propagates the error with `?` turns a failed read into
    # a failed write. The locked row needs a stash wrapper that fails one read.

Scenario: the detector reads with the swap lock free
  Given a capture whose stash step took the swap lock
   When the detector reads the other stashes
   Then the lock is free while it reads
    # Covers the Specification ("after the roster is saved and the swap lock is released") and the
    # Implementation Note on cost: a concurrent daemon swap never waits on these reads.
    # The lock's bounded wait is `SWAP_LOCK_MAX_WAIT` (src/swap.rs:964).
    # No pre-authored oracle: no acceptance criterion owes a RED for it.
```

## Red cards — the questions that no scenario above can close

Open with `d4-restash-paths-coverage` (design D-4; registered in the scope brief after the scoping pull request's review): whether F3 covers the two stash writes outside the four paths this feature names, the swap engine's re-stash of the outgoing account (`outgoing_stash`, `src/swap.rs:835-846`) and the daemon's re-auth `restash_account` (`src/daemon/canonical.rs:401-422`). Each rewrites an existing entry's own stash under that entry's uuid, so no scenario above runs on either path. The header's count does not include it, because #1557 quotes that count.

🟥 Blocked on `label-normalization-policy`. The scenario below waits for the operator's answer.

```gherkin
Scenario: 🟥 a pair between labels that differ only in letter case
  Given a roster A:Work, B:work, C:spare
    And A's stash holds K
   When a capture writes K into C's stash
   Then (pending `label-normalization-policy`: whether `Work` and `work` are one label with two bearers, so that the line names A's uuid)
    # The project compares labels byte for byte today (#1575 is the item that records the decision). Under that rule
    # `Work` has one bearer and the line is `"spare" and "Work" hold the same credential.` Under case
    # folding `Work` has two, and the line is `"spare" and "Work" <uuid-A> hold the same credential.`
    # The item does not say which. Covers AC `report-wording-by-ambiguity`, which names the rule
    # "more than one bearer" and not the key that counts bearers.
```

🟥 Blocked on `import-duplicate-uuid-policy`. The scenario below waits for the operator's answer.

```gherkin
Scenario: 🟥 an artifact that carries two credentials for one uuid
  Given a target roster A:work
    And A's stash holds K
    And an artifact whose credential list holds K and K2 for a new entry B:spare
   When the import runs
   Then (pending `import-duplicate-uuid-policy`: whether the import writes any credential for B, and which, so whether the detector compares K or K2 with A's)
    # #1565 builds refuse until the operator answers. Under refuse nothing is written and there is
    # nothing to compare. Under a warn that imports B roster-only, B has no credential. Under a warn
    # that keeps today's last-entry-wins, the last entry is the one written.
    # Covers AC `all-write-paths` (import).
```

| Blocked on | Question | Hangs on it | The item's own default |
|---|---|---|---|
| `label-normalization-policy` | Do labels that differ only in letter case count as one label with two bearers, so the line carries the uuid on each side that has one? #1575 records the decision, and its options include doing nothing | Rule 2, the 🟥 scenario | #1557 states none. The project compares labels byte for byte today, so the line carries no uuid |
| `import-duplicate-uuid-policy` | When an artifact's credential list holds two entries for one uuid, which credential does the import write, if any, and so which does the detector compare? #1565 holds the decision | Rule 1, the 🟥 scenario | #1557 states none. #1565 builds refuse until the operator answers, so nothing is written and nothing is compared |

## Not pinned by the item

An oracle must not assert these. Each is the pull request's choice, or follows from something the item
does not spell out.

- The wording beyond the facts. The item calls the `(this one)` mark and the exact wording drafts and
  calls the facts binding. The oracle pins the item's three report lines and two event lines byte for
  byte, because the item's own criterion verifies the grammar by comparing bytes. A later wording
  change edits those strings in the same change as the template.
- The order of the lines when one write finds several pairs, and the order of the two sides when an
  import wrote both.
- Whether a later write that puts the same credential into the same entry reports the pair again. The
  item says only that the pair is reported at the creating write.
- The blank-line layout of the shared lines in the import report.
- A label that embeds `event=` or `=` tokens. ADR-0034 puts the obligation to survive one on each
  reader, and the event module's doc lists readers that do not meet it yet. The item's reader
  criterion covers a plain label.
- The exit status of a write that printed a shared line. The item changes none.
- The names of the module, the functions, the templates and the types. The item names none. #1579
  picks the seam names its stubs need.

## Acceptance criteria that no scenario expresses

- *Own tests*: `own-tests` adds two beside #1579's: a roster-only import (an empty written set) makes no
  `AccountStash::read`, RED owed; and the redaction meter runs over an event line carrying one full
  36-character uuid. No scenario here expresses either.
- *Mutation pass*: invert the byte comparison, then skip the read, and show a test fail each time.
  The pull request records it. Rule 1's first scenario fails under either, and its differing-bytes
  rows fail under the first.
- *Existing tests*: they pass with no edit, apart from the mechanical update the list return forces on
  the handler's own tests.
  `perform_socket_capture_captures_the_active_account_and_reconciles_the_roster` (src/daemon/commands.rs:1991) is one of them.
  The canary test (Rule 3) passes with no edit at all.
- *The credential is held once*: the item moves the credential into the report instead of cloning it,
  so the secret exists once in memory and is zeroized on drop. No scenario can see a clone. Review
  checks it.
- *The event module's doc*: the doc that calls every field a handle (`HANDLE` at src/observability.rs:19)
  is amended in the same change, unless the swap-line uuid tokens of F4 landed first and carried the
  amendment.
- *Local gates*: the pre-push gates in `CLAUDE.md`, plus `msrv` and both `deny` checks, because the
  change is under `src/**`.
- *Linux*: a container build and test run as a non-root user (ADR-0029), with the image, the
  commands and each `test result: ok` line quoted in the pull request, because this change edits
  shared code and no CI job compiles Linux (#964 tracks adding one).
- *Oracles*: #1579 records its RED output on its own issue. The commit that makes the tests
  pass removes their `#[ignore]` attributes.

## Where each group is asserted

Taken from the acceptance criteria, for whoever binds the scenarios.

- Rule 1: the CLI rows at `run_capture` and `run_login`, over `FakeAccountStash` (src/stash.rs:424) and the
  new comparison function. The socket rows at `perform_socket_capture`. The run-loop row by extending
  the existing end-to-end test named in that row. The import rows at `apply_import` plus the verb's
  step after the roster save.
- Rule 2: the pure rendering functions, and `Event::to_log_line` for the event.
- Rule 3: the comparison over the stash fake with and without the detector; the canary's own test; the
  two pure parse functions; the `log --json` renderer.
- Rule 4: the redaction meter over the rendering functions; the template module's own tests; the
  existing prose-register test.
- Rule 5: a wrapper around `FakeAccountStash` that counts reads and can fail one; a source scan
  modelled on the one named in its row. `FakeAccountStash` cannot fail a read with a locked keychain
  today, so that row needs the wrapper.
- No function returns a CLI write's whole reply today. `confirmation` (src/capture.rs:688) returns the
  confirmation line only, and `CaptureReport` (src/capture.rs:81) and `LoginReport` (src/capture.rs:737)
  carry no credential. The item says they gain one. A test written before the change cannot read it
  from them, so the oracle builds the written credential from its own fixture.
