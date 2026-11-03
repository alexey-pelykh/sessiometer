# Feature: a save that would make a label name a second account is refused and the Settings client is told which labels, and a save that would not is applied as today

Issue #1558 · PRD account-label-integrity § 10.1 (REQ-ROS-B-001, REQ-ROS-B-002, REQ-ROS-I-001, REQ-ROS-C-001, REQ-ROS-Q-001; AC-ROS-002, AC-ROS-003), § 10.11 (REQ-NTC-C-001, REQ-NTC-C-002) and the § 8.1 ruling on `duplicate-label-policy` · design D-1 / D-10 / § 8.1 / § 8.2 / § 11.5 / § 12.2 / § 12.3 (Terms, C-7, C-9, C-12) · parent R-6 / AC-6, as amended 2026-11-03

Example Mapping: 🟦 5 rules · 🟩 43 examples · 🟥 1 open

> **Tier B, and provisional.** Scenarios only; none is bound to a test yet. The PRD's `dor_status` is
> `failed`, so every requirement behind this file is unratified and its problem framing unvalidated. The
> refuse form is the operator's ruling of 2026-11-03 on `duplicate-label-policy` (PRD § 8.1). These are
> readings by the `/scope` orchestrator, recorded in design § 12.3 and confirmed or changed at
> ratification: the ruling reaches `config-set`; a collision is counted per account, not by
> REQ-ROS-B-002's count (Rule 1); `config-set` carries no override; the existing reasons
> keep their precedence; `collisions` rides on `rejected` alone; and the rename ruling of the same day
> does not reach Settings. The names
> `label-in-use`, `collisions` and `{label, bearers}` are pipeline defaults, not operator decisions. A
> line `binds (existing)` names a test in the tree today; `binds (suggested)` names one that does not exist
> yet. Accounts are written by uuid (A, B, C), listed in roster order; labels are in quotes.
>
> **Story.** One Settings save can relabel an account onto another account's label, and today the daemon
> saves it: nothing compares the new label with the other entries. The operator ruled on 2026-11-03 that a
> write which would make a label name a second account is refused. This item makes the daemon work out
> which labels a save would make name a second account, refuse that save whole before anything is written,
> and say which labels and which accounts; and it makes the app read the refusal and say it in Settings.
>
> **What this file does not write.** The order of `collisions` when one save collides on two labels, which
> is Red 2. Which Settings rows the refusal marks, and the note for a duplicate that already exists, belong
> to #1560. The rule that computes a collision (Rule 1) counts per account: design § 12.3's Terms replace
> REQ-ROS-B-002's count on the write paths the rulings govern, and `import` keeps the count. The refusal (Rule 2),
> today's acknowledgements (Rule 3), the app's closed vocabulary (Rule 4) and the gates against drift
> (Rule 5) are written in the refuse form.

## Rule 1 — a save creates a collision when it gives an account a label it did not carry, and another account carries that label after the save

> The rule is design § 12.3's per-account rule (Terms, C-7). It replaces REQ-ROS-B-002's count, the rule
> import follows (R-6): a label above one bearer and above its count before the write. The count lets a
> save move one account off a shared label and another onto it, because the label's count does not rise.
> #1556 adds the per-account rule beside `Account` (`src/config.rs:360`) as `label_collisions`, so that
> every writer the rulings govern calls one copy; `import` keeps its count (`label_bearers` and
> `created_ambiguity`). Here `apply_settings` (`src/config/settings.rs:37`) calls it over the roster it
> parsed (`before`) and the finished roster (`after`). A collision's bearers are every account that
> carries its label after the save, in roster order.
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
    # A is given "c", which it did not carry, but no other account carries "c" after the save. This is the
    # example that fails if the rule drops "another account carries it after the save".

Scenario: an unrelated save on a roster that already holds a duplicate is not a collision
  # covers: AC quiet-saves-stay-quiet (a tunable-only save on a duplicate roster)
  # binds (suggested): apply_settings_reports_no_collision_for_a_save_that_leaves_a_duplicate_alone
  Given account A and account B, both labelled "work"
   When a save changes a tunable and no label
   Then the save creates no collision
    # "work" ends with two bearers, but the save gives no account a label it did not carry. This is the
    # example that fails if the rule drops "a label it did not carry". That half is what keeps a
    # duplicate that already exists, from `import`, `config restore`, a hand edit or a derived label,
    # from refusing every later save. Import's count is quiet here too: "work" has two bearers before
    # and after (`created_ambiguity`, `src/cli.rs:5425`).

Scenario: renaming one of two same-label accounts to a fresh label ends the duplicate and is not a collision
  # covers: Invariant "a save that creates no collision is applied as today" · design TF-2
  # binds (suggested): apply_settings_reports_no_collision_when_a_save_resolves_a_duplicate
  Given account A and account B, both labelled "work"
   When a save sends B the label "spare"
   Then the save creates no collision
    # The remedy for a duplicate that already exists. B is given "spare", which no other account carries,
    # so the save is applied (Rule 3), and "work" falls from two bearers to one.

Scenario: a relabel onto another account's label is a collision
  # covers: AC refusal-oracle (the rule's half; the refusal is Rule 2)
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
    # C is given "work", which it did not carry, and A and B carry it after the save. The rule asks whether
    # the save gives an account a label another account carries, not whether the duplicate is new. A rule
    # that asked the second would swallow a save that made a duplicate worse.

Scenario: a save that moves one account off a shared label and another onto it is a collision
  # covers: AC refusal-oracle (the rule's half, for a save that keeps every label's count) · AC "Output conforms to import-duplicate-label" (the substitution that file accepts as a known limit of `import`) · design § 12.3, Terms
  # binds (suggested): apply_settings_reports_the_collision_when_a_save_substitutes_one_bearer_for_another
  Given account A and account B, both labelled "work", and account C labelled "c"
   When one save sends B the label "c" and C the label "work"
   Then the save creates a collision on "work"
    And the collision's bearers are A and C, in roster order
    # "work" has two bearers before the save and two after, and "c" one and one, so import's count lets
    # this save through, although C joins A's label. C is given "work", which it did not carry, and A
    # carries it after the save. This is the example that fails if the rule counts bearers. B, given "c",
    # collides with nobody: C no longer carries it.

Scenario: two accounts sent to one fresh label in a single save are a collision
  # covers: AC refusal-oracle (the rule's half, for two accounts sent to one label) · AC "Output conforms to import-duplicate-label" (its own-duplicate scenario)
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
  # binds (suggested): #1556's writer scan, once its "pending" entry for `perform_config_set` is removed
  Given the scan that requires every function writing the roster to call the collision rule
   When it reaches `perform_config_set`, which saves what `Config::apply_settings` returns
   Then it finds the call, reached through `apply_settings`
    And `perform_config_set` carries no "pending" mark
    # #1556's writer table lists `perform_config_set`, the saver, as pending until this item lands, and
    # names `apply_settings` as where the rule is computed. From then on the scan fails the day
    # `apply_settings` stops calling the rule.

Scenario: a save and an import that would leave the same roster say the same thing about the label
  # covers: AC "Output conforms to import-duplicate-label" (concretize-and-compare)
  # binds (suggested): a comparison test over `apply_import` and `apply_settings`
  Given a roster before a write, and a write that would leave "work" on account A and on account B
   When the write is a save in one case and an import in the other
   Then the save's collision and the import's warning both name the label "work"
    And the save lists the bearers A and B, as many as import counts for "work" after the write
    # Two rules, two outcomes: the save asks the shared rule and is refused (Rule 2), and the import keeps
    # its own count and still warns and writes, which AC-6 as amended on 2026-11-03 keeps for `import`.
    # Import's notice (`duplicate_label_notice`, `src/cli.rs:5628`) names the label only, so the count
    # comes from `label_bearers` (`src/cli.rs:5245`), which import keeps beside `created_ambiguity`
    # (design § 12.3, Terms). The two rules part only on a save that moves a label between accounts
    # without raising its count and leaves it on more than one, as the scenario above on
    # `{B: "c", C: "work"}` does.
```

## Rule 2 — a save that would create a collision is refused whole, before anything is written, with `label-in-use` and the labels it would collide on

> The ruling refuses a write that would make a label name a second account (PRD § 8.1).
> `perform_config_set` (`src/daemon/commands.rs:478`) asks the rule of Rule 1 once `Config::apply_settings`
> has accepted the whole edited config, and refuses before the save, the roster backup the save takes and
> the live reconcile. The
> acknowledgement is the existing `rejected` arm with a new reason, `label-in-use`, and beside it
> `collisions`: one `{label, bearers}` for each label the save would collide on, where `bearers` holds the
> uuid of every account that would carry the label, in roster order.
>
> **No override.** Replacing the holder of a label means removing an account and deleting its stored login,
> and `config-set` can neither remove a roster entry nor touch a credential (issue #268). The way through is
> to rename the holder first, in the same save or an earlier one (Rule 3), or to remove it with the CLI.

```gherkin
Scenario: a relabel onto another account's label is refused, and nothing is written
  # covers: AC refusal-oracle · AC wire-shape · AC refused-save-writes-nothing · PRD REQ-ROS-B-001
  # binds (suggested): a_config_set_that_relabels_onto_another_accounts_label_is_refused
  Given the daemon holds account A labelled "a" and account B labelled "work", and has a wired config path
   When one save sends A the label "work"
   Then the acknowledgement is "rejected" with the reason "label-in-use"
    And its "collisions" holds one element, the label "work" with the bearers A and B, in roster order
    And it carries no "detail" and no other key
    And the config file is byte-for-byte what it was
    And the daemon's live roster still has A labelled "a"
    # The refusal precedes the save, so a colliding save is never reported "save-failed": the write that
    # could have failed is never attempted, and no roster backup is taken. The line is compared as a JSON
    # value here; the golden of Rule 5 pins the bytes the crate writes.

Scenario: a relabel that would add a third bearer to a duplicate is refused
  # covers: AC refusal-oracle (the deepening) · AC "Output conforms to import-duplicate-label" (its deepening scenario)
  # binds (suggested): a_config_set_that_deepens_a_duplicate_is_refused
  Given the daemon holds account A and account B, both labelled "work", and account C labelled "c"
   When one save sends C the label "work"
   Then the acknowledgement is "rejected" with the reason "label-in-use"
    And its "collisions" holds the label "work" with the bearers A, B and C, in roster order
    And nothing is written
    # "work" already named two accounts, which this save did not cause; the third is what it would cause.

Scenario: a colliding save is refused whole, its tunable edits included
  # covers: AC refused-save-writes-nothing (refused whole)
  # binds (suggested): a_colliding_config_set_saves_none_of_its_tunables
  Given the daemon holds account A labelled "a" and account B labelled "work", with poll_secs 300 on disk
   When one save sends A the label "work" and poll_secs 120
   Then the acknowledgement is "rejected" with the reason "label-in-use"
    And the config file still holds poll_secs 300, and is byte-for-byte what it was
    # One save is one decision. Saving the tunable and refusing the label would be a partial write, and
    # `config-set` saves the whole change or none of it.

Scenario: a colliding save on a file the live roster has diverged from is refused before the reconcile
  # covers: AC refused-save-writes-nothing (no reconcile)
  # binds (suggested): a_colliding_config_set_on_a_diverged_file_reconciles_nothing, built like a_label_edit_cannot_narrow_the_live_roster_onto_a_diverged_file (`src/daemon/commands.rs:2812`)
  Given the config file holds account A labelled "a" and account B labelled "work"
    And the daemon's live roster holds A, B and an account C that the file does not
   When one save sends A the label "work"
   Then the acknowledgement is "rejected" with the reason "label-in-use"
    And the config file is byte-for-byte what it was
    And the live roster still holds A, B and C, with A labelled "a"
    # The rule reads the file the save would write. Refused, the save reaches neither the save nor the
    # reconcile, so the divergence that #1443 detects is left exactly as it was.

Scenario: the refusal names accounts by uuid and by the operator's own label, and nothing else
  # covers: AC no-email-no-token · PRD REQ-NTC-C-001
  # binds (suggested): the_label_in_use_refusal_names_no_email_and_no_token, over the issue #15 meter (`crate::redaction::meter`, test-only)
  Given the daemon holds account A labelled "a" and account B under an email-shaped label the operator authored
   When one save sends A that label
   Then the refusal line names that label and the uuids of A and B
    And the issue #15 meter finds in it no email but the authored label, and no token
    # The bearers are uuids, which `config-get` already sends to the same client, so the refusal adds no
    # exposure class (design § 8.1).

Scenario: a save cannot ask to override the refusal
  # covers: AC no-override
  # binds (existing): serve_control_rejects_an_authenticated_config_set_with_a_forbidden_key (`src/daemon/commands.rs:3077`), extended with an "overwrite" key
  Given an authenticated config-set line that carries a top-level key "overwrite" beside its labels
   When the daemon reads it
   Then the reply is a "malformed request" error whose detail names "overwrite"
    And the line never reaches the run loop, so nothing is written
    # `ConfigSetRequest` (`src/daemon/socket.rs:561`) re-parses under `deny_unknown_fields`, so no override
    # can ride in as a field. The ruling's override, `--overwrite`, belongs to CLI `capture` and `login`.
```

## Rule 3 — every acknowledgement a daemon sends today is byte-for-byte what it is today

> **Bytes, not values.** If the acknowledgement gains a list field, a comparison of Rust values is green
> whether or not serde writes that list when it is empty. An empty list on the wire is a line an older
> app ignores in silence and a byte-pinned test rejects. Each scenario here asserts the serialized line.
>
> **The existing reasons come first.** The rule runs only once `apply_settings` has accepted the whole
> edited config. A save that would also collide but fails earlier, on no wired path, no file, an unreadable
> file, an unknown account or an invalid value, is answered exactly as today, with no `collisions`. A save
> that would not collide is applied exactly as today.

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

Scenario: a save refused for an unknown account is answered as today, though it would also collide
  # covers: AC existing-reasons-keep-their-precedence (an unknown account) · AC wire-shape ("collisions" only with "label-in-use")
  # binds (existing): perform_config_set_rejects_an_unknown_account_uuid_and_writes_nothing (`src/daemon/commands.rs:2959`), extended with a second, colliding entry
  Given the daemon holds account A labelled "a" and account B labelled "work"
   When one save sends A the label "work" and sends the label "x" to an account the roster does not hold
   Then the acknowledgement line is exactly {"result":"rejected","reason":"unknown-account"}
    And nothing is written
    # An unknown account fails the whole save while the labels are laid down, before the rule is asked. So
    # the existing reason keeps its place and its bytes, though half of the save would collide.

Scenario: a save refused for an invalid value is answered as today, though it would also collide
  # covers: AC existing-reasons-keep-their-precedence (an invalid value)
  # binds (existing): perform_config_set_rejects_an_out_of_range_tunable_and_writes_nothing (`src/daemon/commands.rs:2920`), extended with a colliding label
  Given the daemon holds account A labelled "a" and account B labelled "work"
   When one save sends A the label "work" and a session_ceiling of 200
   Then the acknowledgement line carries the reason "invalid" and its field-naming detail, as today
    And it carries no "collisions" key
    And nothing is written
    # `Config::validate` refuses the whole edited config before the rule is asked, whatever the labels in
    # the same save would have done.

Scenario: moving a label off its holder and onto another account in one save is applied
  # covers: AC quiet-saves-stay-quiet (a save that moves a label off its holder) · AC no-override (the way through)
  # binds (suggested): perform_config_set_applies_a_save_that_moves_a_label_off_its_holder
  Given the daemon holds account A labelled "a" and account B labelled "work"
   When one save sends B the label "spare" and A the label "work"
   Then the acknowledgement line is exactly {"result":"applied","effect":"live"}
    # A is given "work", and B, which carried it, leaves it for "spare" in the same save, so no account
    # ends on a label another account carries. This is the refusal's remedy, made in the very save that
    # would otherwise have been refused: the swap's shape, with a fresh label in place of "a".

Scenario: a rename in Settings onto a label nobody carries needs no consent
  # covers: AC quiet-saves-stay-quiet (a relabel to a fresh label) · PRD § 8.1, the `login-mismatch-policy` ruling
  # binds (existing): perform_config_set_adopts_a_label_live_and_reconciles_the_roster (`src/daemon/commands.rs:2780`), extended to assert the line
  Given the daemon holds account A labelled "work"
   When a save sends A the label "day-job"
   Then the acknowledgement line is exactly {"result":"applied","effect":"live"}
    # The rename ruling refuses a `login` that would rename an account unless `--rename` is given. On the
    # orchestrator's reading in the note above, it does not reach Settings, the deliberate rename path.

Scenario: renaming one of two same-label accounts is applied
  # covers: AC quiet-saves-stay-quiet (a relabel that resolves a duplicate)
  # binds (suggested): perform_config_set_applies_a_save_that_resolves_a_duplicate
  Given the daemon holds account A and account B, both labelled "work"
   When a save sends B the label "spare"
   Then the acknowledgement line is exactly {"result":"applied","effect":"live"}
    # The save touches an account in a duplicate that already exists and gives it a label no other account
    # carries, so nothing is refused. Such a duplicate comes from `import`, `config restore`, a hand edit or
    # a derived label.

Scenario: no new effect value is added
  # covers: AC effects-unchanged-and-one-reason-added (the effect half)
  # binds (existing): testEveryAppliedEffectRendersItsOwnOutcome in SettingsTextMetricsTests, which must pass without edits
  Given the acknowledgement's effects are "live", "restart_required" and "unchanged"
   When this feature is built
   Then those remain the only effects
    # Swift mirrors `ConfigSetEffect` and hard-errors on an unknown value, so a fourth effect breaks every
    # shipped app until all surfaces land together (`ConfigSetEffect`, `src/daemon/commands.rs:563-567`).
    # A new optional field is safe; a new value is not.
```

## Rule 4 — the app's vocabulary stays closed and gains `label-in-use`, and every acknowledgement a daemon sends today decodes as it does today

> `ConfigSetAck.init(from:)` (`apps/menubar/Sources/ConfigWire.swift:264`) throws on an unknown `result`,
> `effect` or `reason` and silently drops an unknown sibling key. That is why a new optional field is safe
> and a new value is not, and why a decode that merely does not throw proves nothing about a field. The
> refusal adds one of each, so the two move together: the value `label-in-use`, whose Swift case lands in
> the same change as the Rust tag, and the field `collisions`, decoded by value. An app older than this
> change cannot decode the new reason and says "Not saved — the daemon sent an unexpected reply.", which is
> true; a new effect has no such fallback, because it would report a save that did happen. Settings shows
> the refusal through its existing rejected-save state, and
> `SettingsFormat.rejectionText` (`apps/menubar/Sources/SettingsModel.swift:640`) switches over every
> reason, so the new case does not compile without its sentence.

```gherkin
Scenario: an unknown result, effect or reason is still a hard decode error
  # covers: AC effects-unchanged-and-one-reason-added (the closed vocabulary)
  # binds (existing): testConfigSetUnknownTagsThrow in ConfigWireTests, which must pass without edits
  Given a line whose result, effect or reason the app does not model
   When the app decodes it
   Then the decode fails
    # A drifted daemon degrades loudly. It never silently mis-renders an outcome.

Scenario: the acknowledgements a daemon sends today decode to the outcomes they decode to today
  # covers: AC swift-decodes-values (the app still reads a line that lacks any new key)
  # binds (existing): testDecodesConfigSetAppliedRestartRequired, testDecodesConfigSetAppliedLive and testDecodesConfigSetAppliedUnchanged in ConfigWireTests, which need no edit, and testDecodesEveryRejectionReason, changed only by the shape of the decoded rejection and by the new reason's row
  Given each acknowledgement line a daemon sends today for a save that created no collision
   When the app decodes it
   Then each decodes to the same effect or rejection as before
    # Every quiet acknowledgement lacks any key this feature adds. The app has to read such a key with its
    # absence tolerated, or the next quiet save becomes a decode error.

Scenario: a label-in-use refusal decodes by value, and a rejection without the list decodes to an empty one
  # covers: AC swift-decodes-values
  # binds (suggested): a Swift test over the Rule 5 golden with `decodeConfigSetAck`, for example testDecodesTheLabelInUseGoldenByValue
  Given the golden line of a "label-in-use" refusal whose "collisions" names "work" with the bearers A and B
   When the app decodes it
   Then it decodes to a rejection with the reason "label-in-use", no detail, and one collision: "work", A then B
    And every key of the golden is a key the decoder reads
    And {"result":"rejected","reason":"unknown-account"} decodes to a rejection with an empty collision list
    # A decode that merely does not throw proves nothing, because an unknown key is dropped in silence. The
    # decoder's keys come from its own `CodingKeys`, made `CaseIterable` for the purpose, never a hand copy.

Scenario: a malformed "collisions" drops the list and keeps the refusal
  # covers: AC swift-decodes-values (a malformed list) · scope brief register `malformed-key-drops-the-fact`
  # binds (suggested): a Swift decode test beside the one above, for example testAMalformedCollisionsListKeepsTheRefusal
  Given a "label-in-use" refusal whose "collisions" is present but is not a list of {label, bearers}
   When the app decodes it
   Then it decodes to a rejection with the reason "label-in-use" and an empty collision list
    # A throw would turn a refusal the daemon explained into "the daemon sent an unexpected reply". The
    # refusal is the outcome; the list is a fact about it.

Scenario: Settings says a refused save was not made, in a sentence of its own
  # covers: AC settings-sentence · PRD REQ-NTC-C-001, REQ-NTC-C-002 · design UX-7
  # binds (existing): testEveryRejectionReasonRendersADistinctSentence and the width and tooltip loops over every reason in SettingsTextMetricsTests, which take the new reason with no edit but the detail-free count
  # binds (suggested): a framing test over every reason's sentence that reads `src/framing_vocabulary.rs` at test time through `FramingVocabularyScan`, as `NotificationDeliveryTests` does, for example testEveryRejectionSentencePassesTheFramingScan (design § 12.3, C-9)
  Given the daemon refuses a save with "label-in-use"
   When Settings renders the outcome
   Then it shows the existing rejected-save state, with a sentence no other reason uses
    And the sentence fits the footer's two-line clamp, and its tooltip is the sentence itself
    And it names no account, no email and no token, and passes the #160 framing screen
    And the operator's edits stay in the form, unsaved
    # Which rows collide is #1560's: it marks them from "collisions". The sentence is fixed, because
    # `rejectionText` receives only the reason and the detail, and the daemon sends no detail with this one.

Scenario: the rejection vocabulary gains exactly one reason, on both sides
  # covers: AC effects-unchanged-and-one-reason-added (the rejection half)
  # binds (existing): testDecodesEveryRejectionReason in ConfigWireTests, which gains the row ("label-in-use", .labelInUse)
  Given the reasons a daemon sends today: "invalid", "unknown-account", "no-config", "config-unreadable", "save-failed" and "unavailable"
   When this feature is built
   Then the daemon and the app share those six and "label-in-use", and no other
    And each of the six keeps its wire tag
    # The Swift decoder throws on a reason it does not model, so the two sides move in one change, or the
    # Settings save fails to decode its first refusal. Rule 5's reason gate is what fails when they do not.
```

## Rule 5 — the two sides cannot drift unseen: the `test` job fails, and `panel-goldens` never stands in for it

> The natural home for a cross-surface check is `panel-goldens`, and that job is soft by design: every step
> is `continue-on-error`, so it always reports pass and could never tell the two sides drifted
> (`docs/specs/capture-rejection-parity.feature.md`, Rule 3). So there are two gates, both Rust tests in the
> `test` job, modelled on `every_rust_rejection_tag_has_a_swift_case` (`src/daemon/socket.rs:1979`): the
> Rust side is proven against serde, the Swift side is parsed from source text, and a shape either cannot
> parse is a panic, not a guess. Both read `apps/menubar/Sources/ConfigWire.swift` at test time, and the
> `rust` path filter does not list that file, while `ci-ok` counts a skipped job as a pass.
>
> The field gate's subject is the fields this work adds, in a named list. Whole-body equality is not its
> subject. Whichever of #1558 and #1559 lands first creates the field gate and its helpers. The reason
> gate's subject is the `ConfigSetRejection` vocabulary, compared both ways, as the capture gate compares
> `CaptureRejection`.

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
  Given the gate lists a field the daemon sends on "rejected"
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
    # Today the `rust` filter lists the capture decoder by hand and not this file, so both gates would be
    # skipped and `ci-ok` would count the skip as a pass. The entry is the one file, not `apps/menubar/**`,
    # so that a panel edit does not pay for `test`, `msrv` and `deny`.

Scenario: the field gate's list for this item is "collisions" on "rejected", and it is red without the field
  # covers: AC field-gate-bites (the list)
  # binds (suggested): the gate's named list, and its first recorded run
  Given the gate's list names "collisions" on the "rejected" acknowledgement
   When it runs over a daemon and a decoder that do not carry the field, as they stand before this change
   Then the gate fails, naming "collisions"
    And once the daemon writes the key with "label-in-use" and the decoder reads it, the gate passes
    # Nothing on "applied" is listed: that arm carries no "collisions", because no save that would create
    # one is ever applied.

Scenario: the golden holds the refusal line the crate writes, and a drifted golden fails its pin
  # covers: AC golden-and-pin
  # binds (suggested): the named ignored emitter, for example emit_wire_ack_golden_fixtures, and its `include_str!` pin
  Given a golden under `build/fixtures/` that holds the line the daemon writes for a "label-in-use" refusal
   When the pin test compares it with what the crate produces now
   Then the two are equal byte for byte
    And a golden that differs by one byte fails the pin
    And only the named ignored emitter writes the file
    # The emitted value names every field with no `..` rest pattern, so a field added later is a compile
    # error until the builder fills it. The Swift test of Rule 4 decodes this file.

Scenario: a config-set reason that one side has and the other does not fails the test job
  # covers: AC reason-parity-gate-bites · `docs/specs/capture-rejection-parity.feature.md` Rule 2 and Rule 3, applied to `ConfigWire.swift`
  # binds (suggested): a reason gate modelled on every_rust_rejection_tag_has_a_swift_case (`src/daemon/socket.rs:1979`), for example every_rust_config_set_rejection_tag_has_a_swift_case, and its recorded mutations
  Given the gate compares the `ConfigSetRejection` tags serde writes with the cases of the Swift `enum ConfigSetRejection`
   When a Rust tag has no Swift case, or a Swift case has no Rust tag
   Then the `test` job fails, naming the tag
    # Set equality, not one-way coverage: a Swift case the daemon never sends is copy for a state that
    # cannot occur. Not `panel-goldens`, which reports pass on every run.

Scenario: the reason gate reads the right declarations, strictly, and never an empty one
  # covers: AC reason-parity-gate-bites (the extraction)
  # binds (suggested): the reason gate's canary and refusal tests, after the_rejection_vocabulary_is_completely_enumerated (`src/daemon/socket.rs:1961`) and swift_rejection_tags (`src/daemon/socket.rs:1882`)
  Given `ConfigWire.swift` as it stands, and `enum ConfigSetRejection` in `src/daemon/socket.rs`
   When the gate extracts the two vocabularies
   Then the Swift set is not empty, and comes from `enum ConfigSetRejection` matched on an identifier boundary
    And a case inside a block comment is not counted, and a case under `#if` fails the gate
    And a Rust variant missing from the gate's own list fails it, read against the declaration
    # Each of these is a way to report green over a vocabulary that differs. The capture gate refuses the
    # same ones for `CaptureRejection`.
```

## Open — one Red card, and the example it keeps unwritten

This pass re-authored the Red cards to the operator's refuse ruling of 2026-11-03: Red 1 (`duplicate-label-policy`) is written as Rules 2, 4 and 5, and Red 2 is unchanged.

### Red 2 — the order of `collisions` when one save collides on two labels

The issue fixes the order of a collision's bearers (roster order) and says nothing about the order of the
collisions themselves. Import's `label_bearers` (`src/cli.rs:5245`) counts into a `HashMap`, so a rule that
groups the same way would write them in an unstable order, which a test that pins the line, and #1560
when it marks the rows the list names, would both read. Suggested, not decided: the roster order of each
label's first bearer, which is the order #1560's note already uses. Unwritten: one save that sends A and B
to "x" and C and D to "y".

Not scenario-shaped, and kept in the issue's criteria: the repository's pre-push gates, and the recorded
mutation evidence.
