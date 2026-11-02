# Feature: a swap line names an account by its uuid when its label has more than one bearer (Tier B)

Issue #1563 · PRD REQ-EVT-B-001, REQ-NTC-C-001, REQ-NTC-C-003, AC-EVT-001, AC-NTC-002 · design D-5, D-9, § 8.4, § 11.5 (CAP-14, CAP-22, CAP-23) · ADR-0034

Example Mapping: 🟦 5 rules · 🟩 30 examples · 🟥 1 open

> **PROVISIONAL · Tier B.** The PRD's `dor_status` is `failed`, so every requirement this stub rests on
> is unratified and its problem framing is unvalidated. Each scenario waits to be bound in #1563's
> execution; none is a test yet. An example is a scenario or an outline row. The stub maps the
> `event=swap` and `event=emergency_swap` lines and nothing else: the `credential_shared` event is
> #1557, the usage sample's uuid is #1562, and the status wire's identifier is #1571. REQ-EVT-B-001 is a
> Should in the PRD.
>
> **Oracle.** #1581 holds the RED tests for the examples that have a seam a test can name
> before the fix: the rendered line at each of the six sites, the parked bearer, the sentinel side and
> the meter corpus. It also holds companion tests that pass before and after the fix, so a fix that
> appends too much goes red. An example marked "No pre-authored oracle" is bound in #1563's execution.
> The oracle asserts rendered line text. It builds no `Event::Swap` or `Event::EmergencySwap` literal,
> because the fix adds fields to both and a literal would stop compiling.
>
> **Calls this stub makes.** Each waits for ratification and none is the operator's decision.
> - The token names `from_uuid` and `to_uuid`, their trailing position and the condition "only for a
>   side whose label has more than one bearer" are design D-9. The design rates D-9 one-way for lines
>   written (§ 12.1). The examples carry the names verbatim, so a rename changes their literals only.
> - No example starts from a write that created the shared label. Each starts from the roster as it
>   stands, so none depends on `duplicate-label-policy`, `login-mismatch-policy`,
>   `import-duplicate-uuid-policy`, `orphan-stash-policy` or `account-key-form`. Design D-5 confines
>   `account-key-form` to the status wire. The one example that does hang on an open decision is the 🟥
>   one, on `label-normalization-policy`.
> - The Blue Card Gate passes at five rules. It passes because Rule 3 takes byte-identical lines and
>   unchanged readers as one constraint, ADR-0034's "additive, trailing and ignorable". Counted apart
>   they are six rules.
> - The item's criterion `readers-unchanged` says `log --json` produces "the same output". `log --json`
>   carries each line verbatim, so a tokened line differs from its tokenless twin in the `line` text of
>   its record. The stub reads the criterion as: the same schema, the same records, and the same `ts`,
>   `event` and `channel` for each, with the tokens only inside `line`.
> - The adopt-recovery example in Rule 4 comes from the item's Context paragraph "Who writes them". No
>   acceptance criterion names it.
>
> The traps in this feature:
> - A formatter that appends both tokens whenever the roster holds any shared label passes the
>   both-sides examples and fails Rule 1's one-side outline and Rule 3's elsewhere example.
> - A count over enabled entries only passes every example that has no parked entry. Rule 2 has one on
>   each side.
> - A site nobody listed passes every formatter-level example. Rule 4 has one row per site and a
>   mutation that must turn each of them red.
> - Tokens placed before `late=true` or before the projection pass every plain example. Rule 1's
>   position outline carries both.
> - A meter corpus that never holds the token passes the meter and proves nothing. Rule 5 asserts the
>   token first.
> - A fix that names the active account for the sentinel side passes every roster-only example. Rule 4's
>   adopt example fails it.
>
> Words. An *entry* is a roster entry. `A:work` is an entry whose uuid is A and whose label is `work`;
> A, B, C and D stand for whole uuids, and `<uuid-B>` in a line stands for B's. An entry is *parked*
> when it is not enabled; it stays in the roster. A *bearer* of a label is an entry that carries it, and
> a side is *shared* when its label has more than one bearer. *The line* is the `event=swap` or
> `event=emergency_swap` line that the daemon, the socket `swap` handler or the standalone `use` writes
> to the event log; `ts=…` stands for its timestamp. Every fixture label is a nickname, so "no email
> address" means none beyond what a label itself carries. *The PRD* is
> `docs/requirements/account-label-integrity.md` and *the design* is
> `docs/design/account-label-integrity-solution-design.md`.

## Rule 1 — a side is named by its uuid, in a trailing token, exactly when its label has more than one bearer when the line is written

> REQ-EVT-B-001, AC-EVT-001 and design D-9. After every token the line already carries, including
> `late=true` and the projection tokens, the line gains `from_uuid=<uuid>` when the outgoing side is
> shared, then `to_uuid=<uuid>` when the incoming side is shared. Each value is that side's entry's uuid.
> A side whose label has one bearer gets no token.

```gherkin
Scenario: a swap between two accounts that share a label names each by its uuid
  Given a roster A:work, B:work, C:spare
   When the daemon writes the line for a swap from A to B
   Then the line is
        ts=… event=swap from=work to=work reason=session session_pct=97 from_uuid=<uuid-A> to_uuid=<uuid-B>
    And the line names no email address and no token
    # Today the line reads `from=work to=work`, a swap from a label to itself. Covers AC
    # `oracle-same-label-swap-names-both-accounts`.

Scenario: an emergency swap between two accounts that share a label names each by its uuid
  Given a roster A:work, B:work, C:spare
   When the daemon writes the line for an emergency swap from A to B
   Then the line is
        ts=… event=emergency_swap from=work to=work from_uuid=<uuid-A> to_uuid=<uuid-B>
    # Covers AC `oracle-emergency-swap-names-both-accounts`.

Scenario Outline: only the side whose label is shared is named
  Given a roster A:work, B:work, C:spare
   When the daemon writes the line for a swap from <out> to <in>
   Then the line is <line>

  Examples:
    | out | in | line |
    | A | C | ts=… event=swap from=work to=spare reason=session session_pct=97 from_uuid=<uuid-A> |
    | C | A | ts=… event=swap from=spare to=work reason=session session_pct=97 to_uuid=<uuid-A> |
    # The first row carries no `to_uuid` and the second no `from_uuid`. Covers AC
    # `one-ambiguous-side-only`.

Scenario Outline: the uuid tokens come after every token the line already carries
  Given a roster A:work, B:work
   When the line for a swap from A to B already carries <already>
   Then the line is <line>

  Examples:
    | already | line |
    | late=true | ts=… event=swap from=work to=work reason=session session_pct=100 late=true from_uuid=<uuid-A> to_uuid=<uuid-B> |
    | the velocity projection | ts=… event=swap from=work to=work reason=velocity_preempt session_pct=70 projected=96.30 rate=0.218333 horizon=120 ceiling=89.00 from_uuid=<uuid-A> to_uuid=<uuid-B> |
    | late=true and the projection | ts=… event=swap from=work to=work reason=velocity_preempt session_pct=100 late=true projected=96.30 rate=0.218333 horizon=120 ceiling=89.00 from_uuid=<uuid-A> to_uuid=<uuid-B> |
    # Covers AC `one-ambiguous-side-only`, its last sentence. The first two rows are reachable at a
    # site: a reactive swap at a reading of 100, and a velocity-preemptive swap. At a site the
    # projection's floats come from a moving average that no test pins, so the oracle pins the line
    # from `horizon=` on. No pre-authored oracle for the third row: no site writes both groups, and
    # only the formatter can be fed that combination.

Scenario: a label with three bearers names the two accounts the swap moved between
  Given a roster A:work, B:work, D:work
   When the daemon writes the line for a swap from A to D
   Then the line ends with from_uuid=<uuid-A> to_uuid=<uuid-D>
    And <uuid-B> appears nowhere on the line
    # "More than one" is not "exactly two", and the line names the two accounts it moved, not every
    # bearer. No pre-authored oracle: it is a key edge, bound in #1563's execution.

Scenario: a relabel does not change the uuid a token carries
  Given a roster A:work, C:spare
    And the operator relabels C as work
   When the daemon writes the line for a swap from A to C
   Then the line ends with from_uuid=<uuid-A> to_uuid=<uuid-C>
    And <uuid-C> is the uuid C held while it was labelled spare
    # Covers the Invariant: a relabel never changes the uuid written in a token. No pre-authored
    # oracle: a harness cannot tell a relabelled roster from one built with the labels already equal,
    # and the oracle's fixtures build the roster the second way.

Scenario: a relabel that ends the sharing ends the tokens
  Given a roster A:work, B:work
    And the operator relabels B as home
   When the daemon writes the line for a swap from A to B
   Then the line is ts=… event=swap from=work to=home reason=session session_pct=97
    # The count is taken when the line is written, over the roster as it then stands. No
    # pre-authored oracle, for the reason given under the previous scenario; the oracle's unique-label
    # companion covers the roster that results.
```

## Rule 2 — every entry counts as a bearer, parked or not, and the count is the one import takes

> The item's Specification, "The bearer count": one pure function counts a label's bearers in a roster
> slice, over the whole roster with parked entries included, and agrees with import's count for the same
> roster. Every site calls it with the roster it holds when it writes.

```gherkin
Scenario Outline: a parked entry counts as a bearer
  Given a roster <roster>
   When the daemon writes the line for a swap from A to C
   Then the line is <line>

  Examples:
    | roster | line |
    | A:work, B:work (parked), C:spare | ts=… event=swap from=work to=spare reason=session session_pct=97 from_uuid=<uuid-A> |
    | A:work, C:spare, D:spare (parked) | ts=… event=swap from=work to=spare reason=session session_pct=97 to_uuid=<uuid-C> |
    # The daemon never swaps to a parked entry, but two entries carry the label, and an operator who
    # reads `from=work` still needs to know which of them it was. Covers AC `one-pure-bearer-count`,
    # "parked entries included".

Scenario: the count is the count import takes
  Given a roster A:work, B:work (parked), C:spare
   When the bearer count is taken for work, for spare and for a label no entry bears
   Then the counts are 2, 1 and 0
    And for each label the roster bears, the count equals the one import takes for the same roster
    # Import's count is `label_bearers`, a map from label to bearer count.
    # (src/cli.rs:5245, `label_bearers`.) Covers AC `one-pure-bearer-count`, "agrees with import's
    # count". No pre-authored oracle: the function does not exist yet and the item leaves its name open.
    # The rendered-line examples above and the parked outline exercise the count through every site.
```

🟥 Blocked on `label-normalization-policy`. The scenario below waits for the operator's answer.

```gherkin
Scenario: 🟥 two labels that differ only in case
  Given a roster A:Work, B:work
   When the daemon writes the line for a swap from A to B
   Then (pending `label-normalization-policy`: whether Work and work are one label for the bearer count)
    # Today's comparison is byte for byte: import's count keys a map on the label as written
    # (`label_bearers`, src/cli.rs:5245), so the item's default is two labels with one bearer each and
    # no token. The design's collision rule has a comparison-key hook for this policy (D-1), and the
    # hook decides it. The item says it does not touch import's count.
```

## Rule 3 — the change is additive: a line without a shared side is byte-identical, and a reader reads a tokened line as it reads the same line without the tokens

> The item's Specification, "Nothing else changes", and ADR-0034 (accepted): the label is written
> verbatim and each reader survives it, so the new tokens are additive, trailing and ignorable. The
> readers, `log`, `stats`, `reliability` and the status wire read what they read today.

```gherkin
Scenario Outline: a roster with no shared label writes the lines it writes today
  Given a roster A:work, B:spare
   When the daemon writes the line for <event> from A to B
   Then the line is <line>

  Examples:
    | event | line |
    | a swap | ts=… event=swap from=work to=spare reason=session session_pct=97 |
    | an emergency swap | ts=… event=emergency_swap from=work to=spare |
    # The first row is the string `swap_line_carries_handles_reason_and_session_pct` pins today, and
    # that test passes with its expected string unedited.
    # (src/observability.rs:4194, `swap_line_carries_handles_reason_and_session_pct`.) Covers AC
    # `unambiguous-lines-byte-identical`; the oracle holds these rows as companions, green before and
    # after.

Scenario: a shared label elsewhere in the roster names no side of this swap
  Given a roster A:work, B:spare, C:team, D:team
   When the daemon writes the line for a swap from A to B
   Then the line is ts=… event=swap from=work to=spare reason=session session_pct=97
    # The roster holds a shared label and this line is not ambiguous. The test is per side, not per
    # roster. Covers the Specification's "a side whose label has one bearer gets no token" with AC
    # `one-ambiguous-side-only`.

Scenario Outline: a reader reads a tokened log as it reads the same log without the tokens
  Given an event log whose swap and emergency-swap lines carry from_uuid and to_uuid
    And the same log with those tokens removed
   When <reader> reads each
   Then <same>

  Examples:
    | reader | same |
    | `parse_swap_events` (src/usage_stats.rs:421) | it returns the same swap events |
    | `parse_events` (src/reliability.rs:1332) | it folds the same inputs |
    | `log --json` | it returns the same schema and the same records, with the same ts, event and channel for each, and the tokens appear only inside each record's line |
    # Covers AC `readers-unchanged`, first sentence. A reader builds a map from token names, so an
    # unknown trailing token is ignored. The oracle holds these rows as companions, green before and
    # after.

Scenario: no reader's version moves
  Given the change that adds the tokens
   When its diff is read
   Then JSON_SCHEMA_VERSION is untouched in src/log.rs, src/stats.rs and src/reliability.rs
    And STATUS_SCHEMA_VERSION is untouched
    # `log --json` carries each line verbatim under its own version.
    # (src/log.rs:167, `JSON_SCHEMA_VERSION`.) Covers AC `readers-unchanged`, second sentence. No
    # pre-authored oracle: it is a property of the diff, read in review.
```

## Rule 4 — no site writes the line without the tokens, and a side that is not a label gets none

> The item's Context, "Who writes them", and its criterion `every-production-site-fills-the-tokens`.
> The sites are the non-test hits of `grep -rnE 'Event::Swap \{|Event::EmergencySwap \{' src`. At
> d9d75ba that grep also hits the two arms of `Event::to_log_line`, which render the events and build
> neither, and its other hits are in test modules.

```gherkin
Scenario Outline: every site that writes the line fills the tokens
  Given a roster A:work, B:work, C:spare
   When <site> writes the line for a move from A to B, <fired>
   Then the line is <line>

  Examples:
    | site | fired | line |
    | the swap decision, `Event::Swap` (src/daemon.rs:4593) | A is over the session trigger and B is viable | ts=… event=swap from=work to=work reason=session session_pct=97 from_uuid=<uuid-A> to_uuid=<uuid-B> |
    | the emergency swap, `Event::EmergencySwap` (src/daemon.rs:4994) | A is quarantined and B is live | ts=… event=emergency_swap from=work to=work from_uuid=<uuid-A> to_uuid=<uuid-B> |
    | the blind-active swap, `Event::Swap` (src/daemon.rs:5231) | A is blind past the limit and its anchor stands at 68 | ts=… event=swap from=work to=work reason=blind_preempt session_pct=68 from_uuid=<uuid-A> to_uuid=<uuid-B> |
    | the velocity-preemptive swap, `Event::Swap` (src/daemon.rs:5564) | A climbs to 92 and its projection crosses the ceiling | ts=… event=swap from=work to=work reason=velocity_preempt session_pct=92 projected=… rate=… horizon=150 ceiling=93.00 from_uuid=<uuid-A> to_uuid=<uuid-B> |
    | the socket swap handler, `Event::Swap` (src/daemon/commands.rs:220) | swap is asked for B's uuid | ts=… event=swap from=work to=work reason=manual session_pct=0 from_uuid=<uuid-A> to_uuid=<uuid-B> |
    | the standalone use verb, `Event::Swap` (src/use_account.rs:934) | use is run for B's uuid with A active | ts=… event=swap from=work to=work reason=manual session_pct=0 from_uuid=<uuid-A> to_uuid=<uuid-B> |
    # Covers AC `every-production-site-fills-the-tokens`. The last two rows ask for B by uuid because
    # the label `work` is ambiguous and resolution refuses it. The numbers 97, 68 and 92 are the
    # readings of the existing harness tests for each site; the velocity floats are not pinned.

Scenario: an adopt recovery with no known outgoing account names no uuid for that side
  Given a roster A:work, B:work
    And the canonical credential is gone and the display is cleared, so no outgoing account resolves
   When use --force for B's uuid adopts B directly
   Then the line is ts=… event=swap from=(unknown) to=work reason=forced session_pct=0 to_uuid=<uuid-B>
    And the line has no from_uuid
    # `(unknown)` is the sentinel `ADOPT_UNKNOWN_FROM` (src/use_account.rs:536), not a label. No entry
    # bears it, and its own doc says it is never an account-uuid. Covers the item's Context; no
    # acceptance criterion names this case. The harness exists:
    # `force_adopts_the_target_when_the_canonical_is_absent_and_display_is_cleared`.

Scenario: each site's test is able to fail
  Given the bearer count is made to report one bearer for every label
   When the duplicate-label test of each of the six sites runs
   Then each fails
    # Restore the count afterwards and record the failing run. Covers AC
    # `every-production-site-fills-the-tokens`, its recorded mutation. A site that goes through one
    # shared builder is reached by the builder's own test.
```

## Rule 5 — the tokens add a uuid and nothing else

> REQ-NTC-C-001 and AC-NTC-002, meter half. A uuid is an identifier, not a secret: it already appears
> in the usage-family event lines, in `config.toml`, in `list` and in the `config-get` reply. The tokens
> add no email address, no token and no digest of a credential.

```gherkin
Scenario: the tokens add a uuid and nothing else
  Given a roster A:work, B:work whose stashes hold an email address and a credential
   When the daemon writes the line for a swap from A to B
   Then the line is the line the same swap writes today, followed by from_uuid=<uuid-A> to_uuid=<uuid-B>
    And the line holds no email address, no token and no digest of a credential
    # Covers AC `oracle-same-label-swap-names-both-accounts`, its "names no email and no token", and
    # AC `no-email-no-token-no-digest`.

Scenario: the redaction meter's corpus holds a swap between two accounts that share a label
  Given the corpus the meter harvests gains a swap from A:work to B:work, with uuids in hyphenated form
   When the meter test runs
   Then the corpus contains from_uuid=<uuid-A> and to_uuid=<uuid-B>
    And the meter finds no secret in the corpus
    # The corpus is harvested by `harvest_channels` (src/daemon.rs:18748) and the meter test is
    # `redaction_meter_emits_no_secret_on_any_channel_across_the_full_loop` (src/daemon.rs:19069).
    # The token is asserted first. A corpus without it passes the meter and proves nothing, and the
    # item rules that case out. A hyphenated uuid's longest alphanumeric run is 12 characters, under
    # the 20 that the entropy backstop flags (`ENTROPY_MIN_RUN`, src/redaction.rs:177). An unbroken
    # 32-character uuid passes validation and can reach the threshold, so fixtures use the hyphenated
    # form, as the item says. Covers AC `no-email-no-token-no-digest`.
```

## Red cards — the questions that no scenario above can close

| Blocked on | Question | Hangs on it | The item's own default |
|---|---|---|---|
| `label-normalization-policy` | Do labels that differ only in case or in Unicode normalization count as one label for the bearer count? | Rule 2's 🟥 scenario. No decided example depends on it. | Byte for byte, as import's count compares today |

The item adds no question of its own. Its token names are design D-9, ratification-pending, and are
listed under the calls above rather than here, because the stub can write a decided outcome under them.

## Acceptance criteria that no scenario expresses

- *`docs-reconciled`* (prose). The event module's doc, the doc of `Event` and the variant's doc stop
  calling every field a label, and the README paragraph that says the event log identifies accounts by
  label also says an ambiguous label's uuid follows in a trailing token. Two checks already decide part
  of it: `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --document-private-items` fails a doc comment
  whose link no longer resolves, and `grep -n 'HANDLE' src/observability.rs` lists some of the
  sentences to read. Neither says whether a sentence is still true. That is judgment, for an
  independent reviewer who reads the sources.
  - The item's locator is now case-insensitive and also reaches `src/log.rs`, `src/cli.rs` and the
    README's channel table (`git grep -n -i -E 'field is a handle|identify accounts by their' -- src README.md`,
    ten lines at the baseline); the sweep below still applies inside `src/observability.rs`. The
    doc of `Event` itself says "Every field is a handle / enum / number / timestamp", and the same
    claim recurs in a variant doc that contrasts the log with `daemon.err.log`, in the `path`
    variant's doc, in the doc of `path_value` and in the module doc's paragraph on diagnostics. Sweep
    by the claim's own words, for example `grep -niE 'every (other )?(field|value)'
    src/observability.rs`, and read each hit; most hits are about other variants.
  - The README sentence at `README.md:484` ("identify accounts by their") covers the event log and
    the `-v` diagnostic channel. This item changes the event log only, so the added clause must name
    the event log and not the diagnostics.
  - #1557 amends the same module-doc sentence for its own tokens. Whichever change lands first carries
    the amendment and the second extends it.
- *Existing tests*: the line-format tests of the swap and emergency-swap lines pass with their
  expected strings unedited. The struct literals around them gain the two new fields, because the item
  leaves the representation to the implementer.
- *Local gates*: the pre-push gates in `CLAUDE.md`, the `msrv` job and both `deny` checks, because the
  change is under `src/**`. The `swift` job is not owed: no path under `apps/menubar/` or
  `build/fixtures/` changes.

## Not pinned by the item

An oracle must not assert these.

- The names of the new fields on `Event::Swap` and `Event::EmergencySwap`, and the name and place of
  the bearer-count function. The item names neither.
- The wording of the amended docs.
- The floats on a velocity line.
- A label that spells a token, such as `from_uuid=x`. That is ADR-0034's accepted case, and no reader
  consumes the tokens in this item. A reader that does owes the ADR-0034 obligations.

## Where each group is asserted

Taken from the existing harnesses, for whoever binds the scenarios.

- Rules 1, 2, 4 and 5 at the daemon's sites: the rendered line of an event a tick or a `decide_action`
  call emits, through `Event::to_log_line`. The fixtures are `meter_daemon` with `warmed_tick` for the
  swap decision, the emergency swap and the meter corpus; `three_account_daemon` for the blind-active
  swap; and `warmed_velocity_daemon` for the velocity-preemptive swap.
- The socket swap handler: the `perform_socket_swap` tests in `src/daemon/commands.rs`, with the
  roster's labels made equal and the target given by uuid.
- The standalone `use`: `run_use_over_cache` in `src/use_account.rs`, which returns the log text, with
  a config whose labels are equal. The adopt example reuses the adopt-recovery tests there.
- Rule 3's readers: `parse_swap_events` and `parse_events` over text, and the `log --json` renderer
  over a temporary log.
- Rule 5's meter: the existing meter test's fixtures with two same-label accounts.
