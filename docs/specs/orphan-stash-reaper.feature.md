# Feature: an orphan stash is deleted only while every guard holds

Issue #1555 · PRD REQ-STSH-B-003 / AC-STSH-003 · design D-12, T-03, SP-2 · ADR-0036

Example Mapping: 🟦 6 rules (2 more held as 🟥) · 🟩 27 examples · 🟥 7 open

> **PROVISIONAL · Tier B.** The PRD's `dor_status` is `failed`, so every requirement this stub rests
> on is unratified. Each scenario waits to be bound in #1555's execution; none is a test yet. The
> feature exists only if the operator's `orphan-stash-policy` decision is *reap*. The design recommends
> *report* (SEC-3, author-chosen), and #1555 is invalidated if the record names *report* or *accept* only.
>
> A scenario marked 🟥 hangs on a decision or a measurement that has not landed. It names that
> blocker and writes no outcome as decided. The seven open questions are listed at the end.
>
> The trap in this feature is that a reaper passes every "never deletes" scenario by deleting nothing.
> The first scenario of Rule 1 is there to fail that reaper. The nearest precedent is a second trap:
> the isolated-keychain reaper's loop (`reap`, src/keychain.rs:1434-1457) attempts every delete after
> one fails, and Rule 6 stops at the first locked keychain instead.
>
> Words. *W* is the grace window and *now* is the clock reading. An *orphan* is a `Sessiometer/<uuid>`
> service that no roster row carries. A *deleting run* is a run of the reaper that is allowed to
> delete, and Rule 7 is 🟥 on what allows it. *Item time* is a service's time as the decision function
> receives it; how a service's time follows from its two items is 🟥 (Rule 3). `config-loads`,
> `no-roster-row`, `older-than-window` and `not-in-flight` are handles for the four guards in this
> stub; the strings the reaper prints are the pull request's. *AC* names are the bold lead-ins of
> #1555's acceptance criteria. Every 🟩 scenario that loads a roster gives it a row, because a roster
> with no rows is 🟥 (Rule 5).

## Rule 1 — nothing is deleted unless `config.toml` loads

```gherkin
Scenario: an orphan is deleted when every guard holds
  Given config.toml loads, its roster carrying u-A
    And the keychain holds stashes for u-A and u-Z
    And u-Z's item time is older than W
    And no stash write is waiting for its roster save
   When a deleting run is made
   Then u-Z's stash is deleted
    And u-A's stash is untouched
    # PRD AC-STSH-003's Then; AC Decision table, the all-true row. The nominal case counts as much
    # as the refusals: a reaper that deletes nothing passes every "never deletes" scenario below,
    # and this is the one scenario that fails it.

Scenario Outline: nothing is deleted when config.toml does not load
  Given the keychain holds a stash for u-Z, older than W
    And config.toml is <state>
   When a deleting run is made
   Then no stash is deleted
    And the decision keeps u-Z, with `config-loads` as its reason

  Examples:
    | state                                                         |
    | absent                                                        |
    | present but malformed                                         |
    | present but unreadable (a read error that is not "not found") |
    # The loader, `load_path` at src/config/load.rs:39, returns a different error for each state.
    # The guard treats them alike. Without a roster every stash would read as an orphan, so the
    # loader that the daemon and the CLI use is the only judge of "loads". Covers AC Decision table
    # (the config-load verdict false alone) and PRD AC-STSH-003's first BUT NOT.

Scenario: the witness reads the same before and after on a machine with no config.toml
  Given config.toml is absent
    And the keychain holds stashes for u-X and u-Y, both older than W
   When a deleting run is made
   Then both stashes are still there
    And the keychain half of the prior-configuration witness answers as it did before the run
    # These stashes may be the only surviving proof that the machine was configured, and that proof
    # is what refuses a first-run write over a live roster (#1440, ADR-0036). The probe is
    # `any_stash_item_present` at src/keychain.rs:814, and the witness's own tests pass unchanged.
    # Covers AC Invariant (witness): the PRD's "witness reads the same before and after".
```

## Rule 2 — a stash that a roster row carries is never deleted, and rows are matched by uuid

```gherkin
Scenario: a rostered stash is kept however old it is
  Given config.toml loads, its roster carrying u-A and u-B
    And the keychain holds stashes for u-A, u-B and u-Z, all older than W
   When a deleting run is made
   Then only u-Z's stash is deleted
    And the decision keeps u-A and u-B, with `no-roster-row` as their reason
    # Covers AC Invariant (no rostered stash is ever deleted) and PRD AC-STSH-003's last BUT NOT.

Scenario: a row is matched by its uuid, never by its label
  Given config.toml loads, its roster carrying one row with uuid u-A and label "work"
    And the keychain holds stashes under u-A and under "work", both older than W
   When a deleting run is made
   Then u-A's stash is kept
    And the stash under "work" is deleted
    # The first Then is the dangerous direction. A comparison on labels finds no row labelled u-A
    # and deletes the one stash the roster does carry. The second Then pins the other half: no row
    # carries the uuid "work", and the item says the comparison is never on the label.

Scenario: a disabled row still carries its uuid
  Given config.toml loads, its roster carrying u-A, disabled
    And the keychain holds a stash for u-A, older than W
   When a deleting run is made
   Then u-A's stash is kept
    # Disabled means out of rotation, not removed. `remove_account` at src/cli.rs:5990 implements
    # the verb that drops a row.

Scenario: two guards false together keep the stash
  Given config.toml loads, its roster carrying u-A
    And u-A's item time is younger than W
   When the reaper decides
   Then the decision keeps u-A
    And its reason names `no-roster-row` or `older-than-window`
    # Covers AC Decision table (two guards false together). Which of the two the reason names is not
    # pinned by the item; see "Not pinned by the item" at the end.

Scenario: a stash written and rostered between the read and the delete is not deleted
  Given config.toml loads, its roster carrying u-A
    And a deleting run has read the keychain and found u-Z, older than W, carried by no roster row
   When a capture writes u-Z's stash and saves a roster row for u-Z before u-Z's delete
   Then u-Z is not deleted
    # Covers AC Invariant (no rostered stash is ever deleted), the clause about a stash written and
    # rostered between the census read and the delete. The item time and the roster both change
    # here. Rule 4 isolates each one alone.
```

## Rule 3 — only a stash strictly older than the grace window is deleted

```gherkin
Scenario Outline: the item time against the window
  Given config.toml loads, its roster carrying u-A
    And the keychain holds stashes for u-A and u-Z
    And the grace window is W
    And u-Z's item time is <item time>
   When the reaper decides
   Then u-Z is <outcome>

  Examples:
    | item time                                  | outcome                                      |
    | just younger than W before now             | kept, with `older-than-window` as the reason |
    | exactly W before now                       | kept, with `older-than-window` as the reason |
    | just older than W before now               | selected for deletion                        |
    | later than now (the clock reads behind it) | kept, with `older-than-window` as the reason |
    | unreadable, or absent from the metadata    | kept, with `older-than-window` as the reason |
    # "Just" is one tick of the item time's resolution, which #1554 measures. The second row is where
    # "strictly older" and "older or equal" differ; the item's mutation check turns on it. The fourth
    # row follows from the same rule: an item that is not older than W is kept. Covers AC Grace window.
```

🟥 Blocked on #1554 (the SP-2 spike). The two scenarios below wait for its findings note.

```gherkin
Scenario: the item time is read from the spike's real dump sample
  Given the secret-free dump sample of one stash's two items, as the findings note records it
   When the census parses it
   Then each service carries the item time that the note records for it
    # The attribute reader `block_attr` at src/keychain.rs:732 matches only "<name>"<blob>= lines,
    # and no canned dump in the repository carries a time attribute. The parser and its fixture come
    # from the note's sample, not from a hand-typed guess. Covers AC Grace window (the parser).

Scenario: a service whose two items carry different times
  Given u-Z's credential item is older than W
    And u-Z's identity item is younger than W
   When the reaper decides
   Then (pending #1554: which time the service has)
    # Reading offered for the note to confirm or refute: a service is as young as its youngest item,
    # and one item with no readable time keeps the whole service. It is the only reading under which a
    # stash caught between its two writes is never deleted. The item states both halves: its decision
    # table keeps a service whose two items straddle the window, and its grace-window criterion keeps an
    # item with no readable time; which time a service has is #1554’s question.
```

## Rule 4 — a stash that a capture, login or import is about to roster is never deleted

> No lock spans a stash write and the roster save after it. Each of the four writers takes the swap
> lock around its stash write and saves the roster once the lock is released:
> - CLI `capture`: `capture_locked` at src/capture.rs:234.
> - CLI `login`: `run_login_locked` in src/capture.rs.
> - socket `capture`: the call to `capture_locked` at src/daemon/commands.rs:349.
> - `import`: the `acquire` inside `apply_import` at src/cli.rs:5324, taken only when the artifact
>   carries credentials.
>
> So the grace window is the main defence, and the lock closes the one case it cannot: a write that
> lands while the reaper runs.

```gherkin
Scenario: a stash written moments ago and not yet rostered is kept
  Given a capture has just written u-N's stash and has not yet saved its roster row
    And config.toml loads, its roster carrying u-A
   When a deleting run is made
   Then u-N's stash is still there
    And the decision keeps u-N, with `older-than-window` as its reason
    # This is the in-flight case itself. The grace window alone tells it from a crash's leftover.
    # Covers PRD AC-STSH-003's second BUT NOT and AC Concurrency property.

Scenario: the item time is read again just before the delete
  Given a deleting run has selected u-Z, whose item time was older than W when the run read it
   When the item time read again just before u-Z's delete is younger than W
   Then u-Z is not deleted
    # A capture of an old orphan's account rewrites that stash in place: `write`, src/stash.rs:83-85,
    # updates an existing item. Whether a real rewrite moves the time that the guard reads is
    # #1554 question 1. If it does not, this re-read cannot see a re-capture.
    # Covers AC Concurrency property (an injected write at each step of the run).

Scenario: a roster row appears with no stash write
  Given a deleting run has selected u-Z, older than W, carried by no roster row
   When config.toml gains a row for u-Z before u-Z's delete
   Then u-Z is not deleted
    # `config_restore` at src/cli.rs:2433 does exactly this and restores no stash, so the item time
    # does not move and only the roster read catches it. The roster is read again through the same
    # loader, so a read that fails is Rule 1's refusal again.

Scenario: the swap lock is held from the first read to the last delete
  Given a deleting run is in progress
   When another writer tries to take the swap lock at any step of the run
   Then it cannot until the run has ended
    # Conditional on the swap lock being the means, as AC Concurrency property says ("if the swap
    # lock is the means"). The config-write lock is not taken with it: the code says the two are
    # never nested (`reconcile_login` in src/capture.rs), so taking both needs a reason in the
    # pull request.

Scenario: a swap lock that cannot be taken stops the run
  Given another holder keeps the swap lock for longer than the wait limit
   When a deleting run is made
   Then nothing is deleted
    And the run reports that it could not take the lock
    # Derived, not stated by the item: every other acquire of this lock fails closed, and its caller
    # writes nothing (`acquire`, src/swap.rs:1001, returns SwapLockBusy). The wait limit is
    # `SWAP_LOCK_MAX_WAIT` at src/swap.rs:964. Conditional on the swap lock being the means.
```

## Rule 5 — 🟥 the cases that the decision record settles

> Blocked on `orphan-stash-policy`. #1552's record must answer the first two cases for the chosen
> option, and the third is the witness question of its item 3 and of #1554 question 3. #1555 states a
> fail-closed default for the first two and marks it pipeline-added and ratification-pending. A default
> is not an outcome that anyone decided, so no `Then` below is written.

```gherkin
Scenario: an orphan whose uuid a retained roster backup still names
  Given config.toml loads, its roster carrying u-A and not u-Z
    And a retained roster backup carries a row for u-Z
    And u-Z's item time is older than W
   When the reaper decides
   Then (pending the record: kept or deleted)
    # The item's default is kept. `config restore` brings a backup's roster back and restores no
    # stash, so a delete here can destroy the only surviving credential for a row that the restore
    # would bring back. Backups are enumerated by `list` at src/roster_backup.rs:282.
    # The ring holds `RING_DEPTH` of them (src/roster_backup.rs:96). Also open: a backup that cannot
    # be read, and a ring that cannot be listed. Covers AC Record-dependent cases.

Scenario: a well-formed config.toml with no roster rows
  Given config.toml loads with no roster rows
    And the keychain holds stashes for u-X and u-Y, both older than W
   When the reaper decides
   Then (pending the record: nothing deleted, or both deleted)
    # The item's default is nothing deleted. `load_path` at src/config/load.rs:39 accepts such a
    # file, so "only while config.toml loads" does not exclude it. A roster emptied by accident has
    # this shape. Covers AC Record-dependent cases.

Scenario: the last Sessiometer item on a machine whose config.toml loads
  Given config.toml loads, its roster carrying u-A, whose stash is absent
    And the only Sessiometer items in the keychain are u-Z's, older than W
   When a deleting run is made
   Then (pending the record and #1554 question 3)
    # Deleting u-Z turns the keychain half of the witness off. The witness is consulted only when
    # config.toml is absent, so this does not breach Rule 1's invariant. Whether it needs a guard, or
    # an ADR-0036 amendment, is the record's item 3. Guards 1 to 4 of the item delete u-Z.
```

## Rule 6 — a selected service is deleted through the stash seam, and a locked keychain stops the run

```gherkin
Scenario: a selected service is deleted, both items with it
  Given the in-memory stash holds u-Z, selected for deletion
   When the run deletes u-Z
   Then the stash no longer holds u-Z
    # `delete` at src/stash.rs:251 removes both items of a service and treats an absent item as
    # success. Covers AC Deletion (both items).

Scenario: a half stash is deleted like a whole one
  Given config.toml loads, its roster carrying u-A
    And the keychain holds u-A's stash, and u-Z with its credential item only, older than W
   When a deleting run is made
   Then u-Z is deleted
    # A write that fails between its two items leaves a half stash. The in-memory fake keeps a whole
    # entry per service (`FakeAccountStash` at src/stash.rs:424), so it cannot hold a half today;
    # AC Fakes allows the addition. Covers AC Deletion (a half stash).

Scenario: an item that vanished before its delete is not an error
  Given a deleting run has selected u-Y and u-Z
    And u-Y's stash disappears before u-Y's delete
   When the run reaches u-Y
   Then no error is raised
    And the run goes on to u-Z
    # Whether u-Y counts as reaped is not pinned. The item counts a service as reaped when the census
    # showed it and the delete returned success, which u-Y meets. `delete_one`, src/keychain.rs:1416,
    # counts a vanished item apart. An oracle must not assert that count.

Scenario: a locked keychain stops the run at once
  Given a deleting run has selected u-X, u-Y and u-Z, to be deleted in that order
    And u-X's delete succeeds
   When u-Y's delete reports a locked keychain
   Then u-Z's delete is not attempted
    And the output reports u-X as deleted
    And the output does not report u-Y as deleted
    # The isolated-keychain reaper does the opposite (see the note at the top). Covers AC Deletion
    # (the locked-keychain assertion) and the item's rule that only a successful delete counts as reaped.
```

## Rule 7 — 🟥 the command shows what it would delete before it deletes

> Blocked on `orphan-stash-policy`: the record names the trigger, an explicit operator command or an
> automatic one, and says whether a preview is the default. #1555's own text, marked pipeline-added,
> is an explicit command that deletes nothing and prints what it would delete unless given its
> confirmation flag. The pull request chooses the spelling (a flag on `remove`, or a new verb) and
> states its cost. No `Then` below is written as decided.

```gherkin
Scenario: a run that was not confirmed deletes nothing and lists what it would delete
  Given config.toml loads, its roster carrying u-A
    And u-Z is an orphan older than W
   When the reaper command is run without its confirmation flag
   Then (pending the record: nothing deleted, and u-Z listed as one a confirmed run would delete)
    # Covers AC Preview. If the item's default stands, no service is reported as reaped either:
    # nothing was deleted, so nothing was reaped.

Scenario: the confirmation flag makes the same run delete
  Given the same keychain and roster
   When the reaper command is run with its confirmation flag
   Then (pending the record: u-Z's stash is deleted)

Scenario: the preview's bytes do not vary
  Given the same keychain and roster
   When the reaper command is run twice without its confirmation flag
   Then (pending the record: both outputs are byte for byte equal)
    # AC Preview says the tests pin the bytes. The wording is the pull request's, so no oracle
    # can pin it first.

Scenario Outline: the argv parser
  When the arguments are <argv>
  Then they parse to <result>

  Examples:
    | argv                                           | result                                   |
    | the chosen spelling                            | the reaper command, without confirmation |
    | the chosen spelling with the confirmation flag | the reaper command, with confirmation    |
    | any form of the verb that parses today         | exactly what it parsed to before         |
    | the chosen spelling with an unknown flag       | rejected                                 |
    # Covers AC Parse. The rows are 🟥 because the command's existence follows from the trigger.
```

## Rule 8 — the report names uuids and facts, and nothing else from the keychain

```gherkin
Scenario: each orphan has its own line, and the counts close the report
  Given config.toml loads, its roster carrying u-A, whose stash is absent
    And the keychain holds stashes for u-Z (older than W) and for u-N (younger than W)
   When a deleting run is made
   Then one line names u-Z as reaped
    And one line names u-N as kept and names `older-than-window`
    And the last line carries the counts: one reaped, one kept
    # Covers the item's Output section. The counts equal the lines above them. The keychain holds no
    # stash for u-A on purpose: whether the report also lists a rostered service is not pinned (see
    # "Not pinned by the item").

Scenario: nothing else from the keychain reaches the output
  Given config.toml loads, its roster carrying u-A
    And a keychain dump whose items carry an account name, a comment and a label besides the service
   When a deleting run is made
   Then none of those values appears in the output
    And the output names no label, no email address, no token and no credential-derived value
    # An orphan has no label, so its uuid is its only name (issue #15). Covers AC Text rules, which
    # asks for a canned dump whose other attribute values must not appear.

Scenario: the report's fixed strings pass the framing scan
  Given every fixed string that the run can print, and the help text of the command that starts it
   When the strings are scanned
   Then the prose passes `scan_advisory_banned` at src/framing_vocabulary.rs:456
    And the help text passes `scan_help_banned` at src/framing_vocabulary.rs:449
    And each string states a fact and gives no instruction
    # Covers AC Text rules. The banned list includes imperative words, so the report cannot tell the
    # operator what to do.
```

## Red cards — the questions that no scenario above can close

| Blocked on | Question | Hangs on it | The item's own default |
|---|---|---|---|
| `orphan-stash-policy` | Is the choice *reap*? | the whole feature | none; *report* or *accept* only invalidates #1555 |
| `orphan-stash-policy` | A uuid that a retained roster backup still names: kept or deleted? Also a backup that cannot be read, and a ring that cannot be listed | Rule 5, first scenario | kept |
| `orphan-stash-policy` | Does a well-formed `config.toml` with no rows count as "loads"? | Rule 5, second scenario | nothing deleted |
| `orphan-stash-policy` | The trigger: an explicit command or an automatic one? Is a preview the default? | Rule 7 | an explicit command, preview by default |
| `orphan-stash-policy` and #1554 question 3 | Deleting the last `Sessiometer/…` item while `config.toml` loads: does it need a guard or an ADR-0036 amendment? | Rule 5, third scenario | nothing deleted, and the run says why: #1555's record-dependent cases say a run that would leave no `Sessiometer/` item deletes nothing, unless the landed record says otherwise |
| #1554 question 1 | Which time attribute, in what format and resolution; on both items; moved by a rewrite; present on a locked keychain; which time a service has when its two items differ. With no usable time the guard cannot be built as designed | Rule 3 (both 🟥 scenarios), the real-world link of Rule 4's re-read | none |
| #1554 question 2 | The recommended value of W | the constant, and AC Grace window source | none |

## Not pinned by the item

An oracle must not assert these. Each is the pull request's choice, or follows from a rule the item
does not spell out.

- Which reason is printed when two guards fail together. The item asks for "a reason".
- Whether the report lists a rostered service. That depends on whether the shell hands the decision
  function every service in the dump or only those that no roster row carries. The decision table
  needs the function to accept rostered services either way.
- The order of the deletes, and what the run does after a delete fails for a reason other than a
  locked keychain.
- The exit statuses.
- The exact bytes of every line, the command's spelling, and the shape of the census-source seam: a
  small trait next to the stash trait, or a listing method on it.
- Whether a vanished item counts as reaped (Rule 6).
- The outcome when the swap lock is busy. Rule 4 derives it from the way every other acquire fails.
- Whether the keychain matches a service name byte for byte or without regard to case when
  `delete` removes it. Nothing in the tree measures this; #1554's question 3 asks it since
  2026-10-30 (a delete with the case of one dummy uuid flipped), and the item reads the answer. It would matter
  only for two uuids that differ by case, and the design's premise that Claude Code never varies a
  uuid's case is itself unmeasured (SP-1, design § 14.4).

## Acceptance criteria that no scenario expresses

- *Fakes*: the existing tests for the stash, the witness and the keychain parsers pass unchanged.
  `cargo test` checks it.
- *Linux*: the change adds no macOS-only call; the pull request quotes the image, the commands run
  as a non-root user (ADR-0029) and each `test result: ok` line.
- *Real path*: a macOS-gated test over a throwaway keychain seeds a rostered stash and an orphan,
  decides with an injected clock past the window, deletes through `RealAccountStash`, and asserts
  by a second dump that only the orphan's two items are gone.
- *Pointers*: the surfaces #1552 marks corrected also name this command's spelling;
  `git grep -n -e '<spelling>'` lists them.
- *Grace window source*: the constant's comment cites #1554's findings note by path
  (`git grep -n 'docs/findings/' -- src/` prints it) and states the import account count N the
  window covers; its value equals the note's recommended window. Blocked on #1554.
- *Local gates*: the five gates in order, the `msrv` check and both `deny` checks.
- *Review*: an independent fresh-context review before merge, because the item deletes keychain
  material; the reviewer, a context other than the author's, posts the verdict on the pull request.

## Where each group is asserted

Taken from the acceptance criteria, for whoever binds the scenarios. A step of the form "the decision
keeps … as its reason" is asserted at the decision function. A step about which stashes are still
there or were deleted is asserted at the shell over the in-memory stash.

- Rules 1 to 3, and the decision half of Rule 4: one pure decision function over the census entries
  with their item times, the roster-load result, the clock and W. It returns the services to delete
  and a reason for each one kept.
- Rule 4's re-reads: the deleting shell, with an injected roster read and an injected per-service
  time read that a test can change between steps. `FakeAccountStash` has no time and no listing.
- Rule 4's lock: the real swap lock on a temporary path, or the shell's lock seam.
- Rule 6: the shell over `AccountStash` (src/stash.rs:82), with the in-memory fake extended for a half stash.
- Rule 7: the pure argv parser and the preview output.
- Rule 8: the pure function that renders the run's result to text.
