# Feature: the reading model says "does not apply" per window, and no site turns that into a number

Issue #1597 · PRD REQ-RDG-B-001 / REQ-RDG-C-001 (§ 10.3; D10 in § 8; § 11.1) · design § 5 B-2, § 4.1, § 4.2, § 4.4, § 5 B-6t, § 11 Cap-2, § 12 AD-1 / AD-4 / AD-6 / AD-23, § 12b, § 14 R-D4

Example Mapping: 🟦 5 rules · 🟩 19 examples · 🟥 0 open

> Since 2026-10-30 a verified Ultra account's `GET /api/oauth/usage` reply carries `"five_hour":null`
> beside a present `seven_day` window. The parser item (#1596, block B-1) serves that reply with its
> session window absent; this item is where "absent" lands. At `fd8640b` the daemon's reading model
> has nowhere to put it: `Usage.session` and `Usage.weekly` are `f64` (`src/usage.rs:87-110`), and so
> are the retained pre-blind anchor `LastGood.session` / `.weekly` (`src/daemon/records.rs:194-198`)
> and the per-slot `BlindAnchor.session` / `.weekly` (`src/daemon/records.rs:222-226`). Every way to
> land the parser fix on an `f64` model fabricates a number, and `0.0` is byte-identical to a measured
> idle window the tree already holds. The six fields become `Option<f64>`; `None` is "does not apply to
> this account", never a fabricated `0`; `#[derive(Debug, Clone, Copy, PartialEq)]` stays on all three
> types; `to_usage` (`src/usage.rs:165-178`), the `LastGood` construction (`src/daemon.rs:2934-2938`)
> and the `BlindAnchor` construction (`src/daemon.rs:3397-3402`) copy each `Option` through; the
> status/watch wire's `session_pct` / `weekly_pct` mapping (`src/daemon/snapshot.rs:996-997`) applies
> the unchanged `to_pct` through `Option::map`, so an absent window rides as `null` under
> the `STATUS_SCHEMA_VERSION` on `origin/main` (`1.14` at fd8640b) with no key added and no golden
> moved. The compiler enumerates every
> other consumer; each is resolved in its owner's commit by one of four named moves — skip, pass,
> exclude, name — never a numeric default. The item lands in one pull request with #1596 and the
> decision-arms item (block B-3): the crate compiles only with all three. Its acceptance is graded at
> its own seams — `to_usage`, the two anchor constructions, the wire mapping — and never waits on the
> arms' tests.
>
> The `# Tier` line under each scenario is this stub's binding line (the `# Binds:` form of
> `docs/specs/first-sight-readout.feature.md`). Tier A names a grader the executor may not
> reinterpret: a test committed before dispatch and left unedited, or a test the issue names verbatim
> with its expected values, which the executor writes from that name and those values. None of
> #1591's four oracles and none of #1592's eight tests reads
> `Usage`, `LastGood` or `BlindAnchor` — they grade `parse_usage` and `dimension` — so Tier A here
> rests on this item's own tests M1–M7 and on the committed wire test
> `status_response_carries_handles_and_percentages_and_never_a_secret`, each named verbatim in #1597
> § 6 or AC-7 together with the mutant AC-7 record against it, and on the committed wire-golden pin
> whose bytes AC-5 forbids moving. None of them carries a RED record — M1 and M3–M7 do not compile
> before the lift, M2 is green at fd8640b already (the three types are `Copy` today), and so is the
> committed wire test — so the recorded mutant row (AC-7 (i)–(viii)) is each one's grader, M2's a
> compile-failure row (the mutant removes `Copy`; the run ends in the compiler's error, not in
> `FAILED`). Tier B
> names what is still unbound. D10 is the operator's open decision; every scenario it reaches is
> written for the recorded default (ship the transitional `1.14` state) and says so in its name.
> Every rule, tier and name here is ratification-pending.

## Rule 1 — a window the reply did not carry is `None` on the reading: "does not apply", distinct by type from a measured `0 %` and from no reading at all

```gherkin
Scenario: the captured null-session reply projects to a reading whose session window is absent
  # Tier A — graded by usage::tests::to_usage_copies_an_absent_window_through_as_none (M1, #1597 § 6;
  # mutant-proven by AC-7 (i): `session: self.session.or(Some(0.0))` in `to_usage` makes it FAIL) ·
  # covers AC-1, AC-2
  Given the served report of the captured reply: session None, weekly 0.90, session reset None,
        weekly reset "2026-11-06T22:59:59.916061+00:00"
   When the report is projected to the reading the decision path acts on
   Then the reading's session fraction is None
    And its weekly fraction is within 1e-9 of 0.90
    And its session reset is None and its weekly reset is present
    # `to_usage` copies each `Option` through (`session: self.session, weekly: self.weekly`,
    # `src/usage.rs:165-168`). The parser said "no session window"; nothing between the parser and the
    # reading may turn that into a number. REQ-RDG-B-001: distinguishable from (i) a measured `0 %`.

Scenario: a session-only reply projects to a reading whose weekly window is absent
  # Tier A — graded by usage::tests::to_usage_copies_an_absent_weekly_window_through_as_none (M6, #1597
  # § 6; mutant-proven by AC-7 (v): `weekly: self.weekly.or(Some(0.0))` in `to_usage` makes it FAIL) ·
  # covers AC-1, AC-2
  Given the served report of the session-only body (#1591's Oracle D fixture): session 0.42,
        weekly None, both resets None
   When the report is projected to the reading
   Then the reading's weekly fraction is None
    And its session fraction is within 1e-9 of 0.42
    And both resets are None
    # The operator's direction is per window and symmetric ("maybe eventually they will make some
    # account w/o weekly limits"), which is why both fractions lift. REQ-RDG-B-001 BUT NOT: expressible
    # for the session window only. No observed instance exists; the type carries the case anyway.

Scenario: no reading at all and an absent window are two different absences
  # Tier B — pending: the inner half is M3 (mutant-proven by AC-7 (iii), see Rule 2) and the outer half
  # is the committed
  # daemon::tests::last_good_retains_the_pre_blind_reading_across_an_active_429 (`src/daemon.rs:8336`),
  # whose reads the executor adapts under AC-9; nothing executor-uneditable grades the pair together —
  # the merge judge reads M3 beside that test · covers AC-2
  Given the active account's poll failed with a 429, so its latest-reading slot holds no reading
    And a weekly-only peer's poll was served, so its slot holds a reading whose session fraction
        is None
   When the two slots are read
   Then the first is the outer None — no reading at all (`last_reading` is `Option<Usage>`,
        `src/daemon.rs:1305`; `DecisionState.last_good` is `Option<LastGood>`, `src/daemon.rs:1607`)
    And the second is Some reading carrying an inner None — a window the account does not have
    And neither absence is a `0.0`
    # REQ-RDG-B-001: distinguishable from (ii) no reading at all (a failed poll, a blind account). The
    # two Nones sit at two depths; a consumer that collapses them has nothing to collapse them into.

Scenario: the three reading types stay `Copy`, and `UsageReport` gains no derive
  # Tier A — graded by usage::tests::the_lifted_reading_types_stay_copy and
  # daemon::records::tests::the_lifted_reading_types_stay_copy (M2, #1597 § 6; green at fd8640b too,
  # since the three types are `Copy` today, and mutant-proven by AC-7 (vii): removing
  # `Copy` from `Usage`'s derive at `src/usage.rs:86` makes it fail to compile) · covers AC-1
  Given `Usage`, `LastGood` and `BlindAnchor` with `session` and `weekly` as `Option<f64>`
   When `fn assert_copy<T: Copy>() {}` is instantiated for each of the three
   Then the crate compiles
    And `#[derive(Debug, Clone, Copy, PartialEq)]` stands byte-identical on all three
        (`src/usage.rs:86`, `src/daemon/records.rs:193`, `src/daemon/records.rs:221`)
    And `UsageReport` (`src/usage.rs`; never `Copy`, it holds four `Option<String>`) keeps its
        derives unchanged
    # `Option<f64>` is `Copy` and `PartialEq`, so the derives need no edit. The by-value copy-outs
    # keep working: `let (prev_usage, next_usage) = (*prev, *next)` (`src/daemon.rs:2809`) and every
    # `if let Some(anchor) = self.state.last_good`. AC-1 BUT NOT: a sentinel, a flag beside an `f64`,
    # or a per-window struct (`Option<Window>` is design § 12b's recorded follow-up).

Scenario: the three types' doc comments say what `None` means and no longer say "both"
  # Tier B — pending: documentation —
  # `git grep -n -i -E 'both quota windows|both fractions|both usage windows|\(both windows\)' -- src apps docs/adr docs/runbooks docs/findings README.md CONTRIBUTING.md`
  # (7 lines at fd8640b; 0 expected on the pull request) and its canary, the same command with only the
  # pattern swapped —
  # `git grep -n -i -E 'compile_error!' -- src apps docs/adr docs/runbooks docs/findings README.md CONTRIBUTING.md`
  # (7 lines at fd8640b and the same 7 on the pull request, run in the same shell) — are the named
  # checks; `git grep -c -E 'across both quota windows' -- src/usage.rs` and
  # `git grep -c -E '\(both windows\)' -- src/usage.rs` (each `src/usage.rs:1` at fd8640b, nothing on
  # the pull request) are separate correction checks, not the canary; the wording is the merge judge's ·
  # covers AC-1, AC-10
  Given the module doc at
        `src/usage.rs:14` ("a [`Usage`] reading (both windows)"), the doc comments at `src/usage.rs:80-87`
        ("across both quota windows", "carries both
        fractions") and `src/daemon/records.rs:218` ("Carries BOTH usage windows")
   When this item lands
   Then each of the three types says a window the served reply did not carry is `None` — "does not
        apply to this account" — never a fabricated `0`, and that `Some(0.0)` is a measured idle window
    And `BlindAnchor`'s opening sentence becomes "Carries every usage window the account has (a
        window the account lacks is `None`)" and keeps its argument that a session-only anchor cannot
        distinguish a mid-blindness reset from a quiet account
    And the module doc at `src/usage.rs:14` no longer says a reading carries both windows
    And the three lines the sweep returns in the decision-arms item's files — `src/observability.rs`
        and `:3181` (its row 15) and `src/daemon.rs` (its AC-17) — are
        propagated, not corrected here
    And the sweep's cardinality is reported on the pull request with the canary beside it
    # Invariant I-1 (a `*_resets_at` is `Some` only when its fraction is `Some`) is pinned at the
    # parser by #1596 and is carried here as a documented contract on `Usage` — no runtime check.
```

## Rule 2 — both anchors copy the reading's absence through; the decision beside the copy belongs to the decision-arms item

```gherkin
Scenario: a weekly-only active retains a pre-blind anchor whose session is absent, and a 429 leaves it standing
  # Tier A — graded by daemon::tests::last_good_retains_a_weekly_only_reading_with_its_session_absent
  # (M3, #1597 § 6; mutant-proven by AC-7 (iii): `session: reading.session.or(Some(0.0))` at the
  # `LastGood` construction (`src/daemon.rs:2934-2938`) makes it FAIL) · covers AC-2
  Given the active account's served reading is session None, weekly 0.90, weekly reset present
   When the tick retains it as the account's last good reading
   Then `last_good` is Some(LastGood { session: None, weekly: Some(0.90), .. })
    And when the active's next poll is a 429, `last_good` is unchanged
    # The anchor copies the reading (`session: reading.session, weekly: reading.weekly`,
    # `src/daemon.rs`); a weekly-only active therefore retains an anchor whose session is
    # None, which is what lets the blind gate read "nothing to protect" honestly. What the gate does
    # with a None anchor (`blind_gate_armed`, `src/daemon.rs:470-472`) is B-3 row 7, not this item.

Scenario: a weekly-only account's live-to-blind entry anchors its reading with the session absent
  # Tier A — graded by
  # daemon::tests::a_blind_entry_anchor_for_a_weekly_only_account_carries_no_session_fraction (M4, #1597
  # § 6; mutant-proven by AC-7 (iv): `session: prev.session.or(Some(0.0))` at the
  # `BlindAnchor` construction (`src/daemon.rs:3397-3402`) makes it FAIL) · covers AC-3
  Given an account whose last live reading was session None, weekly Some(w)
   When its next poll fails — the entry edge of `note_blind_episode` (`src/daemon.rs:3372`): a prior
        live reading, then an Err
   Then `blind_anchor` is Some(BlindAnchor { session: None, weekly: Some(w), .. })
    And the anchor's `near_limit` tag for a None session is asserted by the decision-arms item's
        row-7 test, not here
    # The construction copies the reading (`session: prev.session, weekly: prev.weekly`,
    # `src/daemon.rs:3397-3402`). The line above it, `prev.session >= self.session_ceiling_base`
    # (`src/daemon.rs`), decides something and so belongs to B-3 row 7, in the same pull request.
```

## Rule 3 — the status/watch wire projects an absent window to `null` beside the window that is present, under an unchanged schema minor (`1.14` at fd8640b), with no golden moved (D10 default)

```gherkin
Scenario: a served weekly-only reading rides the wire as session null beside weekly 90 (D10 default)
  # Tier A — graded by
  # daemon::snapshot_build::tests::status_response_maps_an_absent_session_window_to_null_beside_a_present_weekly
  # (M5, #1597 § 6; mutant-proven by AC-7 (ii):
  # `session_pct: account.usage.map(|u| to_pct(u.session.unwrap_or(0.0)))` makes it FAIL) · covers AC-4
  Given a snapshot account whose reading is Some(Usage { session: None, weekly: Some(0.90),
        session_resets_at: None, weekly_resets_at: Some(_) })
   When the status response is serialised
   Then the frame carries "session_pct":null, "weekly_pct":90, "session_resets_at":null and a present
        "weekly_resets_at"
    And the frame's schema minor is unchanged from `origin/main` (`1.14` at fd8640b) — no key is added
    # `session_pct: account.usage.and_then(|u| u.session.map(to_pct))` and its weekly twin
    # (`src/daemon/snapshot.rs:996-997`); `to_pct` stays `f64 -> u8` (`src/daemon/snapshot.rs:1072`).
    # The no-window key is the status-wire item's (block B-6), at the next free minor, never here.

Scenario: a served session-only reading rides the wire as weekly null beside session 42 (D10 default)
  # Tier A — graded by
  # daemon::snapshot_build::tests::status_response_maps_an_absent_weekly_window_to_null_beside_a_present_session
  # (M7, #1597 § 6; mutant-proven by AC-7 (viii):
  # `weekly_pct: account.usage.map(|u| to_pct(u.weekly.unwrap_or(0.0)))` makes it FAIL, and leaves M5
  # green) · covers AC-2, AC-4
  Given a snapshot account whose reading is Some(Usage { session: Some(0.42), weekly: None,
        session_resets_at: None, weekly_resets_at: None })
   When the status response is serialised
   Then the frame carries "session_pct":42, "weekly_pct":null, "session_resets_at":null and
        "weekly_resets_at":null
    And the frame's schema minor is unchanged from `origin/main` (`1.14` at fd8640b) — no key is added
    # M5's weekly-axis sibling: `weekly_pct: account.usage.and_then(|u| u.weekly.map(to_pct))`
    # (`src/daemon/snapshot.rs:997`). Without it, weekly absence is graded at `to_usage` only (M6), and
    # a fabricating weekly mapping passes M5, whose weekly is present.

Scenario: a failed poll still rides as null on both percents — the steady-state meaning of null is not redefined
  # Tier A — graded by the committed
  # daemon::snapshot_build::tests::status_response_carries_handles_and_percentages_and_never_a_secret (`src/daemon/snapshot_build.rs:642`;
  # its present-reading literal gains `Some` under AC-8, its `usage: None` arm and the
  # "session_pct":null assertion at `:701` stay as committed; mutant-proven by AC-7 (vi):
  # `session_pct: match account.usage { None => Some(to_pct(0.0)), Some(u) => u.session.map(to_pct) }`
  # makes that assertion FAIL) · covers AC-4
  Given a snapshot account whose reading is None — its last poll failed
   When the status response is serialised
   Then the frame carries "session_pct":null — the committed assertion at `:701` — and
        "weekly_pct":null, which the same `usage: None` arm produces by construction and no committed
        test asserts, so it is a review read under AC-4; never a fabricated 0
    # Under `1.14` a failed poll and a served weekly-only reading share `session_pct: null`; they
    # differ on `weekly_pct`, present only for the served reading. The steady-state contract — `null`
    # means the last poll failed — is what B-6's key restores. AC-4 BUT NOT: a redefinition of `null`.

Scenario: the five committed wire goldens reproduce byte-identical with no regeneration (D10 default)
  # Tier A — graded by
  # daemon::snapshot_build::tests::the_committed_wire_goldens_still_match_the_daemon_encoders (`src/daemon/snapshot_build.rs:3929`,
  # committed at fd8640b, named verbatim in AC-4 and AC-5; the goldens it reads under `build/fixtures/`
  # are byte-identical to `origin/main` by AC-5, so the executor edits neither side — the status wire's
  # fixture-integrity pin, read as #1592's T1–T8 are; a ratification-pending reading, since the pin's
  # RED is structural rather than recorded) · covers AC-4, AC-5
  Given `STATUS_SCHEMA_VERSION` is unchanged from `origin/main` (`1.14` at fd8640b, `src/daemon/snapshot.rs:538-541`)
    And the five goldens `build/fixtures/wire-heartbeat-basic.json`, `wire-snapshot-basic.json`,
        `wire-snapshot-canonical-scrub.json`, `wire-snapshot-expiry.json`,
        `wire-snapshot-next-swap.json`,
        every row of which carries `session_pct: 60`
   When the daemon encoders re-emit them in-process
   Then the bytes match the committed files without any regeneration
    And `git diff origin/main...HEAD --stat -- build/fixtures/wire-heartbeat-basic.json build/fixtures/wire-snapshot-basic.json build/fixtures/wire-snapshot-canonical-scrub.json build/fixtures/wire-snapshot-expiry.json build/fixtures/wire-snapshot-next-swap.json`
        prints nothing — the three-dot form reads only this branch's changes, so PR #1588's rewrite of
        the same
        goldens never reads as this branch's; the five wire goldens only, because the decision-arms item
        (#1602 AC-19) may
        regenerate a CLI render golden under `build/fixtures/cli-renders/` in the same pull request
    # The lift changes a field's type on the reading, not a key on the frame. Under D10's alternative
    # (B-6 in tranche 1) the minor moves and the goldens with it — this scenario then does not hold on
    # the pull-request head, which is why it is tagged.

Scenario: the session_pct doc comment names the transitional meaning and the item that reverts it (D10 default)
  # Tier B — pending: documentation —
  # `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --document-private-items` decides that it builds,
  # the merge judge the wording; under D10's alternative the sentence is not written and B-6's key lands
  # in the same pull request · covers AC-4
  Given the doc comment at `src/daemon/snapshot.rs:758-760`: "Last-polled session-window usage
        percent (`0..=100`); `null` if the last poll for this account failed (never a fabricated `0`)."
   When this item lands under D10's default
   Then the comment gains, after "(never a fabricated `0`)", one sentence: "Transitionally, until the
        status-wire item #1599 lands its no-window key, also `null` when the served reading
        carried no session window; that item reverts this sentence."
    And the steady-state contract is not redefined
    # Cost of the default, stated: for those days a healthy weekly-only account's session cell reads
    # `n/a` on the CLI and in the 1.14 app exactly like a failed poll, while its weekly figures return.
```

## Rule 4 — CONSTRAINT: no site, in production or in a test, substitutes a number for an absent window

```gherkin
Scenario: the copy-through that fills an absent session with an idle window is caught
  # Tier A — graded by usage::tests::to_usage_copies_an_absent_window_through_as_none (M1) against AC-7
  # mutant (i), recorded in the pull request body by exact name as `running 1 test` then `FAILED` ·
  # covers AC-7
  Given a scratch copy of `to_usage` mutated to `session: self.session.or(Some(0.0))`
   When M1 runs by exact name
   Then it prints `running 1 test` and FAILS — the reading's session is Some(0.0) where None was
        required
    # `Some(0.0)` is byte-identical to a measured idle window — the fabrication the type exists to
    # forbid. AC-7 BUT NOT: a mutant run on the committed branch state; a passing suite offered in
    # place of the run.

Scenario: the wire mapping that fills an absent session with zero is caught
  # Tier A — graded by
  # daemon::snapshot_build::tests::status_response_maps_an_absent_session_window_to_null_beside_a_present_weekly
  # (M5) against AC-7 mutant (ii), recorded the same way · covers AC-7
  Given a scratch copy of the wire mapping mutated to
        `session_pct: account.usage.map(|u| to_pct(u.session.unwrap_or(0.0)))`
   When M5 runs by exact name
   Then it prints `running 1 test` and FAILS — the frame carries "session_pct":0 where null was
        required

Scenario: the three production files this item touches carry no numeric-default idiom
  # Tier B — pending: a named mechanical check, not a test —
  # `git grep -nE 'unwrap_or\(0\.0\)|unwrap_or_default\(\)|or\(Some\(0\.0\)\)' -- src/usage.rs src/daemon/records.rs src/daemon/snapshot.rs`
  # prints nothing at fd8640b, so a hit is this item's doing; its canary, the same command with only the
  # pattern swapped,
  # `git grep -nE 'SPDX-License-Identifier' -- src/usage.rs src/daemon/records.rs src/daemon/snapshot.rs`,
  # prints 3 lines at fd8640b and the same 3 on the pull request, run in the same shell; no test is owed
  # — design AD-4 rejected a source lint over `unwrap_or(0.0)` for its false positives, and the two
  # per-seam mutants above are the instrument · covers AC-7
  Given `src/usage.rs`, `src/daemon/records.rs` and `src/daemon/snapshot.rs` on the pull-request head
   When the grep runs
   Then it prints nothing
    And its canary, run in the same shell, prints 3 lines, one per file
    # REQ-RDG-C-001: a read site meeting the absent state takes one of four named moves — skip, pass,
    # exclude, name — and never a numeric default.

Scenario: a Some(0.0) wrapper stands only for a measured idle window
  # Tier B — pending: review of the diff (design § 14 R-D4: a test "fixed" by `Some(0.0)` asserts
  # nothing about the absent state); `git grep -nE 'Some\(0\.0\)' -- src` prints 10 lines at fd8640b,
  # none a usage-window fraction (a comment, glyph and sparkline inputs, three rates, a spend), so the
  # signal is the lines this item's own commit adds —
  # `git show -U0 --format= <commit found by its `(issue
  # #1597)` subject> -- src | grep -E '^\+.*Some\(0\.0\)'` — each listed in the pull request body with
  # the reading it means; no executor-uneditable grader exists for a test literal's intent · covers AC-8
  Given a test literal that constructed a present reading — `Usage { session: 0.5, .. }`, or
        `LastGood { session: 0.70, .. }` as at `src/daemon/records.rs:581`
   When it is adapted to the lifted type
   Then it becomes `Some(0.5)`, `Some(0.70)` — a measured reading, `Some(0.0)` included where the
        test meant a measured idle window
    And `None` is written only in a test that is about the absent state and asserts something about it
    And no `Some(0.0)` stands for absence, no `unwrap_or(0.0)` or `unwrap_or_default()` touches a
        fraction, and no assertion is weakened
    And the count of adapted literals is reported against the 122 `Usage {` + 9 `LastGood {` +
        5 `BlindAnchor {` sites measured at fd8640b with the word-bounded
        `(^|[^A-Za-z_])(Usage|LastGood|BlindAnchor) \{` (measured at fd8640b; struct declarations and
        the five `-> Usage {` function signatures excluded)
```

## Rule 5 — the lift is a type change the compiler enumerates: copy-throughs need no edit, decisions go to their owner, existing tests adapt reads only

```gherkin
Scenario: the lift-only build enumerates every consumer, and each flagged site has an owner
  # Tier B — pending: `cargo build` on the lift-only tree and the pull request body are the record;
  # nothing executor-uneditable grades a reconciliation table — the merge judge reads it against #1597 §
  # 4 · covers AC-6
  Given the six fields changed to `Option<f64>` and nothing else
   When `cargo build` runs and its full output is saved
   Then the error list is carried in the pull request body
    And every flagged production site is reconciled against the § 4 table with an owner — this item,
        the decision-arms item by design row, or the sample-store item (#1598)
    And every unflagged table row is explained — a copy-through that type-checks, or a site behind an
        inference chain
    And the flagged production count is at least the lower bounds the investigation's lift-only
        `cargo build` reported (design § 4.1; PRD § 2.4): 33 production errors in 6 files for the
        session fields and 17 in 9 for the weekly — or the body says why it is lower
    # AC-6 BUT NOT: a table-only enumeration with no compiler output behind it. The compiler's list is
    # the authoritative enumeration; the lexical list of § 4 is its pre-enumeration.

Scenario: this item's only production resolutions are copy-throughs and one `.map`
  # Tier B — pending: compile plus review (AC-6, AC-11); the three copy-through sites type-check
  # unedited once both sides are `Option<f64>` — a compiler flag on one of them means a sibling type was
  # not lifted yet, so the order is wrong, not the site · covers AC-6, AC-11
  Given `to_usage` (`src/usage.rs:165-168`), the `LastGood` construction (`src/daemon.rs:2934-2938`),
        the `BlindAnchor` construction (`src/daemon.rs:3397-3402`) and the two `to_pct` lines
        (`src/daemon/snapshot.rs:996-997`)
   When this item's commit lands
   Then the three copy-throughs carry no textual change
    And the two wire lines read `account.usage.and_then(|u| u.session.map(to_pct))` and the weekly twin
    And every other flagged site is resolved in its owner's commit by a named move — skip, pass,
        exclude, name
    And no site anywhere in the pull request is resolved by a numeric default
    And this item's production diff touches only `src/usage.rs` (`Usage`; `UsageReport` and
        `parse_usage` are #1596's), `src/daemon/records.rs` and `src/daemon/snapshot.rs`; nothing
        under `build/fixtures/`, `apps/menubar/`, `.github/workflows/`, top-level `scripts/`,
        `.cargo/` or `deny.toml`; no account email, label, uuid, token or credential in the diff

Scenario Outline: an existing reader of the anchors or the reading stays green with its reads adapted, never its assertions
  # Tier B — pending: the adapted reads are executor-written, so nothing executor-uneditable pins that
  # an assertion survives — AC-8's `Some(0.0)` grep and the merge judge's diff read are the signals; the
  # missing grader would be a committed pin on each test's assertion lines, which no item carries and
  # the design did not ask for · covers AC-9, AC-8
  Given the test <test> at <where>, which compares a fraction of the reading or an anchor to a literal
   When the lift lands
   Then its reads compare to `Some(<literal>)` with the file's tolerance idiom, or destructure `Some`
    And it is green under `cargo test --verbose`
    And no assertion is removed or weakened

  Examples:
    | test                                                                   | where                                                                  |
    | last_good_retains_the_pre_blind_reading_across_an_active_429           | src/daemon.rs:8336                                                     |
    | last_good_refreshes_on_each_successful_active_poll                     | src/daemon.rs:8385                                                     |
    | a_swap_away_clears_last_good_so_it_never_describes_the_departed_active | src/daemon.rs:8425                                                     |
    | blind_anchor_high_water_stamps_only_a_stale_low_anchor                 | src/daemon.rs:9408                                                     |
    | reconcile_roster_preserves_an_in_flight_blind_anchor_in_lockstep       | src/daemon.rs:12078                                                    |
    | the LastGood fixture tests of src/daemon/records.rs mod tests          | src/daemon/records.rs:581, :635, :702, :735, :826, :901                |
    | a_success_yields_the_reading                                           | src/usage.rs:1183 (its two fraction reads are assigned here by #1596)  |
```

## Coverage

Each acceptance criterion of #1597, with the scenarios that cover it. A criterion no scenario covers
is named as such, with what grades it instead.

| AC | Scenarios | What grades it |
|---|---|---|
| AC-1 — the six fields are `Option<f64>`, the three derives unchanged, the doc comments say `None` is "does not apply" | Rule 1, all five: the two projection scenarios (the type), "the three reading types stay `Copy`…" (the derives), "the three types' doc comments…" (the prose) | M1, M6 (Tier A); M2 (Tier A, mutant row AC-7 (vii)); the doc build and the merge judge for the prose |
| AC-2 — the reading model expresses "does not apply" per window (REQ-RDG-B-001) | Rule 1 "the captured null-session reply projects…", "a session-only reply projects…", "no reading at all and an absent window are two different absences"; Rule 2 "a weekly-only active retains a pre-blind anchor…"; Rule 3 "a served session-only reading rides the wire as weekly null…" | M1, M6 (Tier A); M3 (Tier A, mutant row AC-7 (iii)); M7 (Tier A, mutant row AC-7 (viii)); the two-absences pair stays the merge judge's read of M3 beside the committed 429 test |
| AC-3 — the blind-entry anchor carries the absence | Rule 2 "a weekly-only account's live-to-blind entry anchors its reading…" | M4 (Tier A, mutant row AC-7 (iv)) |
| AC-4 — the wire maps an absent window to `null` beside the window that is present under an unchanged schema minor, no golden moved, D10 sentence | Rule 3, all five | M5 and M7 (Tier A, mutant rows AC-7 (ii) and (viii)); the committed wire test (Tier A, mutant row AC-7 (vi)); the goldens pin (Tier A); the doc build and the judge |
| AC-5 — conformance to the Build Reference | Rule 3 "the five committed wire goldens reproduce byte-identical…" | `the_committed_wire_goldens_still_match_the_daemon_encoders`; `git diff origin/main...HEAD --stat --` over the five wire-golden paths printing nothing |
| AC-6 — the compile-error enumeration is recorded and reconciled | Rule 5 "the lift-only build enumerates every consumer…", "this item's only production resolutions…" | `cargo build` on the lift-only tree; the pull request body; the merge judge — no test can grade a reconciliation |
| AC-7 — no numeric default, proven by recorded mutants and a grep (REQ-RDG-C-001) | Rule 4, the two mutant scenarios and "the three production files… carry no numeric-default idiom" | M1 and M5 against mutants (i) and (ii) (Tier A); mutants (iii)–(viii) are recorded on the Tier lines of the M3, M4, M6, committed-wire-test, M2 and M7 scenarios (Tier A); the grep, with its `SPDX-License-Identifier` canary, is a named check with no test by design (AD-4) |
| AC-8 — the test-literal adaptation follows the rule | Rule 4 "a Some(0.0) wrapper stands only for a measured idle window"; Rule 5 outline | review of every wrapper (R-D4); the `Some\(0\.0\)` grep as the signal |
| AC-9 — the anchor and reading tests stay green, reads adapted only | Rule 5 outline (seven rows); Rule 1 "no reading at all and an absent window…" (its outer half) | `cargo test --verbose`; the judge's diff read for weakened assertions |
| AC-10 — propagation sweep over the corrected claim | Rule 1 "the three types' doc comments say what `None` means…" | the sweep grep with its same-invocation `compile_error!` canary (7 lines); the pull request body's per-line disposition (four corrected in-band, three propagated to the decision-arms item #1602) |
| AC-11 — this item's share of the diff is bounded | Rule 5 "this item's only production resolutions…" (its last step) | `git diff --stat origin/main...HEAD`; review |
| AC-12 — gates | none | the five `test` steps, `msrv`, both `deny` checks, in CI; never `cargo test -- --include-ignored` |
| AC-13 — the records precede the change | none | the merge order on `main`: #1593 before the pull request that carries this item |
| AC-14 — commit convention | none | review of the squash commit: `(feat) usage: … (issue #1597)` with `Closes #1597.`, the sibling items' `Closes` lines beside it |
