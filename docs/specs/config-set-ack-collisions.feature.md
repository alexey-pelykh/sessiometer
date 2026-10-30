# Feature: a save that makes a label name a second account is told to the Settings client, and a save that did not stays silent

Issue #1558 · PRD account-label-integrity § 10.1 (REQ-ROS-B-002, REQ-ROS-I-001, REQ-ROS-C-001, REQ-ROS-Q-001; AC-ROS-002, AC-ROS-003) · design D-1 / D-10 / § 8.1 / § 11.5 · parent R-6 / AC-6

Example Mapping: 🟦 4 rules · 🟩 24 examples · 🟥 2 open

> **Tier B, and provisional.** Scenarios only; none is bound to a test yet. The PRD's `dor_status` is
> `failed`, so every requirement behind this file is unratified and its problem framing unvalidated. The
> names `collisions` and `{label, bearers}` are pipeline defaults, not operator decisions. A line
> `binds (existing)` names a test in the tree today; `binds (suggested)` names one that does not exist yet.
> Accounts are written by uuid (A, B, C), listed in roster order; labels are in quotes.
>
> **Story.** One Settings save can relabel an account onto another account's label. The daemon's
> `config-set` acknowledgement says only `applied` and an effect, so the Settings window cannot tell the
> operator what the CLI tells them: that a label now names two accounts. This item makes the daemon work
> out which labels a save made name a second account, and the app read what the daemon says about it.
>
> **What this file does not write.** What the daemon does with a collision once it has computed one —
> carry it in the acknowledgement of a saved write (warn) or refuse before saving (refuse) — is
> `duplicate-label-policy`, and no operator has decided it. The rule that computes a collision (Rule 1),
> the acknowledgement of a save that created none (Rule 2), the app's closed vocabulary (Rule 3) and the
> gate against drift (Rule 4) hold under both answers, so they are written. Every example whose outcome
> differs is listed under *Open*, with its Given and When settled and its Then left blank.

## Rule 1 — a save creates a collision when a label ends with more than one bearer, and with more than it started with

> The rule is the one import follows (R-6). #1556 moves it beside `Account` (`src/config.rs:360`) so that
> every roster writer calls one copy. Here `apply_settings` (`src/config/settings.rs:37`) calls it over the
> roster it parsed (`before`) and the finished roster (`after`).
>
> **The trap is where the rule reads.** `overlay_labels` (`src/config/settings.rs:159-169`) assigns the new
> labels one at a time, in uuid order, so a swap passes through a state in which two accounts bear one
> label. A check made as each label is assigned reports that state. A check that compares each new label
> with the roster as it stood before the save misses two accounts sent to one new label. Comparing `before`
> with the finished `after` gets both right.
>
> The comparison is byte equality of the stored label. What else would make two labels "the same" is
> `label-normalization-policy`; no example below depends on it.

```gherkin
Scenario: a swap of two labels is not a collision
  # covers: AC quiet-saves-stay-quiet (a swap) · PRD AC-ROS-002
  # binds (suggested): apply_settings_reports_no_collision_for_a_label_swap
  Given account A labelled "a" and account B labelled "b"
   When one save sends A the label "b" and B the label "a"
   Then the save creates no collision
    # Mid-overlay the roster carries one label twice. That state is never reported: the rule reads the
    # finished roster, where each label has one bearer.

Scenario: a relabel to the label an account already carries is not a collision
  # covers: AC quiet-saves-stay-quiet (a relabel to the label it carries)
  # binds (suggested): apply_settings_reports_no_collision_for_a_relabel_to_the_same_label
  Given account A labelled "a"
   When a save sends A the label "a"
   Then the save creates no collision
    And the save changes nothing
    # `SettingsChange` (`src/config.rs:1317`) records no change, so nothing is written and the effect is
    # "unchanged".

Scenario: a relabel to a label nobody bears is not a collision
  # covers: AC quiet-saves-stay-quiet (a relabel to a fresh label)
  # binds (suggested): apply_settings_reports_no_collision_for_a_relabel_to_a_fresh_label
  Given account A labelled "a" and account B labelled "b"
   When a save sends A the label "c"
   Then the save creates no collision
    # "c" ends with one bearer and started with none: "more than it started with" holds, "more than one"
    # does not. This is the example that fails if the "more than one" half is dropped.

Scenario: an unrelated save on a roster that already holds a duplicate is not a collision
  # covers: AC quiet-saves-stay-quiet (a tunable-only save on a duplicate roster)
  # binds (suggested): apply_settings_reports_no_collision_for_a_save_that_leaves_a_duplicate_alone
  Given account A and account B, both labelled "work"
   When a save changes a tunable and no label
   Then the save creates no collision
    # "work" ends with two bearers and started with two: "more than one" holds, "more than it started
    # with" does not. This is the example that fails if that second half is dropped. The operator was
    # told when the duplicate was made; repeating it on every later save trains dismissal, which is
    # import's own reason (`created_ambiguity`, `src/cli.rs:5425`).

Scenario: renaming one of two same-label accounts to a fresh label ends the duplicate and reports nothing
  # covers: Invariant "a write that creates no collision reports nothing" · design TF-2
  # binds (suggested): apply_settings_reports_no_collision_when_a_save_resolves_a_duplicate
  Given account A and account B, both labelled "work"
   When a save sends B the label "spare"
   Then the save creates no collision
    # The remedy the notice tells the operator to take. "work" falls from two bearers to one.

Scenario: a relabel onto another account's label is a collision
  # covers: AC collision-oracle (the rule's half; the acknowledgement's half is Open, Red 1)
  # binds (suggested): apply_settings_reports_the_collision_when_a_relabel_lands_on_another_accounts_label
  Given account A labelled "a" and account B labelled "work"
   When a save sends A the label "work"
   Then the save creates a collision on "work"
    And the collision's bearers are A and B, in roster order

Scenario: a relabel that adds a third bearer to a duplicate is a collision
  # covers: AC "Output conforms to import-duplicate-label" (its deepening scenario)
  # binds (suggested): apply_settings_reports_the_collision_when_a_relabel_deepens_a_duplicate
  Given account A and account B, both labelled "work", and account C labelled "c"
   When a save sends C the label "work"
   Then the save creates a collision on "work"
    And the collision's bearers are A, B and C, in roster order
    # The rule is "more bearers than before", not "the duplicate is new". Without it, "do not repeat a
    # duplicate" would swallow a save that made one worse.

Scenario: two accounts sent to one fresh label in a single save are a collision
  # covers: AC "Output conforms to import-duplicate-label" (its own-duplicate scenario)
  # binds (suggested): apply_settings_reports_the_collision_when_one_save_sends_two_accounts_to_one_label
  Given account A labelled "a" and account B labelled "b"
   When one save sends both A and B the label "x"
   Then the save creates a collision on "x"
    And the collision's bearers are A and B, in roster order
    # Neither account bore "x" before the save, so a check against the roster as it stood misses this.

Scenario: a label save never changes which accounts exist or in what order
  # covers: Invariant "a label never changes an entry's account_uuid"
  # binds (suggested): a_label_save_never_changes_the_roster_uuids_or_their_order
  Given any roster, and any label save that applies
   When the save is applied
   Then the roster holds the same uuids in the same order as before
    # `overlay_labels` writes only `label`. A collision is never resolved by touching a uuid.

Scenario: the config-set writer is one of the writers that call the rule
  # covers: AC writer-scan-completed
  # binds (suggested): #1556's writer scan, once its "pending" entry for `apply_settings` is removed
  Given the scan that requires every function writing the roster to call the collision rule
   When it reaches `Config::apply_settings`
   Then it finds the call
    And `apply_settings` carries no "pending" mark
    # #1556 lists `apply_settings` as pending until this item lands. From then on the scan fails the day
    # `apply_settings` stops calling the rule.

Scenario: a save and an import that end in the same roster say the same thing about the label
  # covers: AC "Output conforms to import-duplicate-label" (concretize-and-compare)
  # binds (suggested): a comparison test over `apply_import` and `apply_settings`
  Given a roster that ends with "work" on account A and on account B
   When one case reaches it by a save and another by an import
   Then the rule's verdict on both names the label "work"
    And both list the bearers A and B
    # Import's notice (`duplicate_label_notice`, `src/cli.rs:5628`) names the label only, so the count has
    # to come from the rule. The comparator the criterion names (`label_bearers`, `src/cli.rs:5245`) may be
    # gone by then: #1556 has import call the shared rule in place of it and of `created_ambiguity`.
```

## Rule 2 — the acknowledgement of a save that created no collision is byte-for-byte what it is today

> **Bytes, not values.** If the acknowledgement gains a list field, a comparison of Rust values is green
> whether or not serde writes that list when it is empty. An empty list on the wire is a line an older
> app ignores in silence and a byte-pinned test rejects. Each scenario here asserts the serialized line.

```gherkin
Scenario: a quiet label save is acknowledged with today's exact line
  # covers: AC quiet-saves-stay-quiet (at the daemon) · AC wire-shape (nothing is added when there is nothing to say)
  # binds (suggested): perform_config_set_acknowledges_a_label_swap_with_todays_exact_line
  Given the daemon holds account A labelled "a" and account B labelled "b", and has a wired config path
   When one save sends A the label "b" and B the label "a"
   Then the acknowledgement line is exactly {"result":"applied","effect":"live"}

Scenario: an unrelated save on a roster that holds a duplicate is acknowledged with today's exact line
  # covers: AC quiet-saves-stay-quiet (a tunable-only save on a duplicate roster)
  # binds (suggested): perform_config_set_acknowledges_an_unrelated_save_on_a_duplicate_roster_with_todays_exact_line
  Given the daemon holds account A and account B, both labelled "work"
   When a save changes a tunable and no label
   Then the acknowledgement line is exactly {"result":"applied","effect":"restart_required"}
    # `config_set_ack_serializes_to_the_non_secret_wire_shape` (`src/daemon/commands.rs:3227`) pins this
    # line for a value built by hand; this pins it through `perform_config_set` (`src/daemon/commands.rs:478`).

Scenario: a save that changes nothing is acknowledged "unchanged" and writes nothing
  # covers: AC quiet-saves-stay-quiet (a relabel to the label it carries)
  # binds (existing): perform_config_set_reports_unchanged_for_a_noop (`src/daemon/commands.rs:2886`), extended to assert the line
  Given the daemon holds account A labelled "work"
   When a save sends A the label "work" and the tunable value it already holds
   Then the acknowledgement line is exactly {"result":"applied","effect":"unchanged"}
    And the config file is byte-for-byte what it was

Scenario: a refused save carries no collisions
  # covers: AC wire-shape ("the Unchanged and rejection paths carry none")
  # binds (existing): perform_config_set_rejects_an_unknown_account_uuid_and_writes_nothing (`src/daemon/commands.rs:2959`), extended with a second, colliding entry
  Given the daemon holds account A labelled "a" and account B labelled "work"
   When one save sends A the label "work" and sends the label "x" to an account the roster does not hold
   Then the acknowledgement line is exactly {"result":"rejected","reason":"unknown-account"}
    And nothing is written
    # An unknown account fails the whole save while the labels are laid down. A refused save reports no
    # collision, though half of it would have made one.

Scenario: no new effect value is added
  # covers: AC effect-and-rejection-unchanged (the effect half)
  # binds (existing): testEveryAppliedEffectRendersItsOwnOutcome in SettingsTextMetricsTests, which must pass without edits
  Given the acknowledgement's effects are "live", "restart_required" and "unchanged"
   When this feature is built
   Then those remain the only effects
    # Swift mirrors `ConfigSetEffect` and hard-errors on an unknown value, so a fourth effect breaks every
    # shipped app until all surfaces land together (`ConfigSetEffect`, `src/daemon/commands.rs:563-567`).
    # A new optional field is safe; a new value is not.
```

## Rule 3 — the app's vocabulary stays closed, and every acknowledgement a daemon sends today decodes as it does today

> `ConfigSetAck.init(from:)` (`apps/menubar/Sources/ConfigWire.swift:264`) throws on an unknown `result`,
> `effect` or `reason` and silently drops an unknown sibling key. That is why a new optional field is safe
> and a new value is not, and why a decode that merely does not throw proves nothing about a field. The
> decode of a filled acknowledgement, by value, hangs on Red 1; what is written here is the half that
> does not.

```gherkin
Scenario: an unknown result, effect or reason is still a hard decode error
  # covers: AC effect-and-rejection-unchanged (the closed vocabulary)
  # binds (existing): testConfigSetUnknownTagsThrow in ConfigWireTests, which must pass without edits
  Given a line whose result, effect or reason the app does not model
   When the app decodes it
   Then the decode fails
    # A drifted daemon degrades loudly. It never silently mis-renders an outcome.

Scenario: the acknowledgements a daemon sends today decode to the outcomes they decode to today
  # covers: AC swift-decodes-values (the app still reads a line that lacks any new key)
  # binds (existing): testDecodesConfigSetAppliedRestartRequired, testDecodesConfigSetAppliedLive, testDecodesConfigSetAppliedUnchanged and testDecodesEveryRejectionReason in ConfigWireTests, changed only by the shape of the decoded value
  Given each acknowledgement line a daemon sends today for a save that created no collision
   When the app decodes it
   Then each decodes to the same effect or rejection as before
    # Every quiet acknowledgement lacks any key this feature adds. The app has to read such a key with its
    # absence tolerated, or the next quiet save becomes a decode error.
```

## Rule 4 — the two sides cannot drift unseen: the `test` job fails, and `panel-goldens` never stands in for it

> The natural home for a cross-surface check is `panel-goldens`, and that job is soft by design: every step
> is `continue-on-error`, so it always reports pass and could never tell the two sides drifted
> (`docs/specs/capture-rejection-parity.feature.md`, Rule 3). So the gate is a Rust test in the `test` job,
> modelled on `every_rust_rejection_tag_has_a_swift_case` (`src/daemon/socket.rs:1979`): the Rust side is
> proven against serde, the Swift side is parsed from source text, and a shape it cannot parse is a panic,
> not a guess. It reads `apps/menubar/Sources/ConfigWire.swift` at test time, and the `rust` path filter
> does not list that file, while `ci-ok` counts a skipped job as a pass.
>
> The gate's subject is the fields this work adds, in a named list. Whole-body equality is not its subject.
> Whichever of #1558 and #1559 lands first creates the gate and its helpers.

```gherkin
Scenario: the gate's reading of the Swift decoder is proven non-empty
  # covers: AC field-gate-bites (the canary)
  # binds (suggested): the gate's canary, for example the_config_set_gate_finds_the_keys_the_swift_decoder_reads
  Given `ConfigWire.swift` as it stands
   When the gate extracts the keys the acknowledgement's decoder reads
   Then the set holds "result", "effect", "reason" and "detail"
    # A moved delimiter would yield an empty set, and an empty set compares equal to nothing.

Scenario: the gate reads the acknowledgement's decoder and no other type's keys
  # covers: AC field-gate-bites (the extraction)
  # binds (suggested): the gate's scoping test
  Given `ConfigWire.swift` holds four `CodingKeys` declarations, for TunablesView, AccountView, SetTunables and the ConfigSetAck decoder
   When the gate extracts the keys the acknowledgement's decoder reads
   Then "enabled", which only AccountView reads, is not among them
    # The wrong declaration parses cleanly and reports green, which is the failure this gate cannot
    # afford. The capture gate matches its enum on an identifier boundary for the same reason.

Scenario: a decoder shape the gate cannot parse fails the gate
  # covers: AC field-gate-bites ("panics on a shape it cannot parse")
  # binds (suggested): the gate's refusal test
  Given the acknowledgement decoder's keys are declared under conditional compilation
   When the gate reads `ConfigWire.swift`
   Then the gate fails instead of choosing a branch
    # Which branch ships depends on a build configuration the test cannot see. A guess reports green
    # over a vocabulary that may not exist.

Scenario: a listed field the Swift decoder does not read fails the gate
  # covers: AC field-gate-bites (the first mutation)
  # binds (suggested): the recorded mutation — rename the key the decoder reads for a listed field, run the gate, show it fail, restore it
  Given the gate lists a field the daemon sends on "applied"
    And the Swift decoder reads that field under another key
   When the `test` job runs
   Then the gate fails, naming the field

Scenario: a listed field the daemon does not send fails the gate
  # covers: AC field-gate-bites (the second mutation)
  # binds (suggested): the recorded mutation — list a field Rust does not emit, run the gate, show it fail
  Given the gate lists a field the daemon never writes for the filled acknowledgement
   When the `test` job runs
   Then the gate fails, naming the field
    # The Rust side is proven against serde, not asserted from the list.

Scenario: a Swift-only edit to ConfigWire.swift still runs the test job
  # covers: AC ci-filter
  # binds: a read of the `rust` filter in `.github/workflows/ci.yml` (no check reconciles it today), and `scripts/check-gate-change-ack.sh` for the trailer
  Given a change that touches only `apps/menubar/Sources/ConfigWire.swift`
   When CI decides which jobs run
   Then the `test` job runs
    And the commit that adds the filter entry carries a non-empty "Gate-Change-Acknowledged" trailer
    # Today the `rust` filter lists the capture decoder by hand and not this file, so the gate would be
    # skipped and `ci-ok` would count the skip as a pass. The entry is the one file, not `apps/menubar/**`,
    # so that a panel edit does not pay for `test`, `msrv` and `deny`.
```

## Open — two Red cards, and the examples they keep unwritten

### Red 1 — `duplicate-label-policy`: warn or refuse

When a `config-set` makes a label name a second account, does the daemon save it and say so in the
`applied` acknowledgement (warn), or refuse before saving (refuse)? Neither the PRD (§ 8.1) nor the design
(§ 12.2) decides it. The issue builds warn, the form that needs no re-ratification. Refuse contradicts the
ratified AC-6 ("BUT NOT by enforcing label uniqueness"), adds a `label-in-use` rejection and its Swift
case, and so needs AC-6 re-ratified first. PRD § 8.1 says the answer fixes the assertion form of the
oracles, so none of them can be authored before it.

Unwritten, each with its Given and When settled and only its Then open:

1. A on "a", B on "work"; one save sends A the label "work". Warn: `applied`, carrying a collision on
   "work" with bearers A and B. Refuse: `rejected`, nothing saved. (AC collision-oracle, the
   acknowledgement's half; the issue suggests the name
   `a_config_set_that_relabels_onto_another_accounts_label_reports_the_collision`.)
2. A and B on "work", C on "c"; one save sends C the label "work". (The deepening, at the acknowledgement.)
3. Example 1 with a tunable change in the same save. Warn: effect `restart_required`, with the collision.
4. Example 1 where the live roster holds more accounts than the file, so the live reconcile is refused;
   `a_label_edit_cannot_narrow_the_live_roster_onto_a_diverged_file` (`src/daemon/commands.rs:2812`) is the
   fixture. Warn: `restart_required`, with the collision, because the labels are on disk.
5. The wire shape of a non-empty list: the key present only when non-empty, the element `{label,
   bearers}`, bearers in roster order. (AC wire-shape)
6. The golden holds the exact line for an acknowledgement with every optional field filled, and a pin fails
   when the committed bytes differ from what the crate produces. (AC golden-and-pin)
7. The Swift test decodes that golden by value — the label, each bearer in order — asserts its keys equal
   the keys the model reads, and decodes `{"result":"applied","effect":"live"}` to an empty list. (AC
   swift-decodes-values)
8. The serialized acknowledgement passes the issue #15 meter for a label the operator authored in an email
   shape. (AC no-email-no-token)
9. The rejection vocabulary stays at six reasons; refuse adds a seventh. (AC effect-and-rejection-unchanged,
   the rejection half)
10. The gate's list for this item is `collisions` on `applied`: the gate's red before the change (a listed
    field neither side carries) and the recorded mutation on that field. Under refuse the list is empty.
    (AC field-gate-bites, the list; Rule 4 writes the mechanism, which holds either way.)

### Red 2 — the order of `collisions` when one save collides on two labels

The issue fixes the order of a collision's bearers (roster order) and says nothing about the order of the
collisions themselves. Import's `label_bearers` (`src/cli.rs:5245`) counts into a `HashMap`, so a rule that
groups the same way would write them in an unstable order, and #1560 announces each collision once.
Suggested, not decided: the roster order of each label's first bearer, which is the order #1560's note
already uses. Unwritten: one save that sends A and B to "x" and C and D to "y".

Not scenario-shaped, and kept in the issue's criteria: the repository's pre-push gates, and the recorded
mutation evidence.
