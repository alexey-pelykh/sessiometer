# Feature: every decision arm names its move on an absent quota window — skip, pass, exclude or name, never a numeric default

Issue #1602 · PRD REQ-RDG-C-001 / REQ-ARM-B-001 … REQ-ARM-B-012 / REQ-ARM-C-001 / REQ-ARM-Q-001 (§ 10.3; D3, D4, D7, D10 in § 8; § 11.1) · design § 5 B-3, § 4.2, § 4.4, § 4.5, § 5 B-6t, § 11 Cap-2 / Cap-10 / MET-R-001, § 12 AD-4 / AD-4b / AD-5 / AD-6 / AD-8 / AD-14 / AD-15 / AD-16 / OAD-2, § 14 R-D4 / R-D5

Example Mapping: 🟦 5 rules · 🟩 30 examples · 🟥 0 open

> Since 2026-10-30 a verified Ultra account's `GET /api/oauth/usage` reply carries `"five_hour":null`
> beside a present `seven_day` window. The parser item (#1596, block B-1) serves that reply with its
> session window absent and the reading-model item (#1597, block B-2) makes `Usage.session`,
> `LastGood.session` and `BlindAnchor.session` an `Option<f64>`. From that moment the compiler flags
> every place the daemon *decides* something on a window fraction — the fifteen arms #1602
> Specification § 1 enumerates — and each has to say what it does when the window does not apply.
> The only way to compile without this item is the fabricating shortcut, `unwrap_or(0.0)`, and `0.0`
> is byte-identical to an idle window: a weekly-only account at weekly 0.99 would read as a spare
> with session headroom, a blind weekly-only active would log `session_pct=0` and `near_limit=false`
> as if measured, and the fleet's 15 weekly-only accounts would keep being misjudged.
>
> After #1596 and #1597 a window fraction is one of three things: `Some(f)`, measured; `None`, the
> window does not apply to this account; or no reading at all — the outer `Option` around the whole
> `Usage` (`AccountRuntime::last_reading`, `last_good`), a failed poll or a blind account, already
> handled at every arm today. This stub is about the middle state only. "Weekly-only" below means
> `session: None, session_resets_at: None` beside a present weekly window; "weekly-less" is the
> mirror. Each arm takes exactly one of four named moves (design § 4.2, the rule `REQ-RDG-C-001`
> names): **skip** — the arm has nothing to evaluate on this axis and does not fire; **pass** — the
> gate is satisfied vacuously (`is_none_or`); **exclude** — the account or sample is left out of a
> figure or a census; **name** — the absence is carried as a typed absence on a surface. The fifth
> move, substitute a number, is forbidden at every site including tests: a test "fixed" by wrapping
> the old literal in `Some(0.0)` asserts nothing about the absent state. The tree's values the arms
> compare against, read at `fd8640b`: session ceiling 0.95 (`DEFAULT_SESSION_CEILING`,
> `src/config.rs:152`), weekly ceiling 0.98 (`src/config.rs:166`) whose effective rotation line is
> 0.97 (`weekly_rotation_line`, `src/daemon.rs:4105-4107`), the operator's floor read as `floor` in
> selection, `BLIND_GATE_SECS = 300` (`src/daemon.rs:370`), `BLIND_GATE_RISK_BAND = 0.60`
> (`src/daemon.rs`). The item lands in one pull request with #1596 and #1597 — the crate compiles
> only with all three — and is graded at its own seams: each arm's pure function or tested method.
>
> The `# Tier` line under each scenario is this stub's binding line (the form of
> `docs/specs/reading-model-optional-window.feature.md`). Tier A names a grader the executor may not
> edit and this item set already pins by name. None of #1591's four oracles and none of #1592's seven
> pins reads a decision arm — they grade `parse_usage` and `dimension` — so Tier A here rests on the
> tests #1602 § 6 names verbatim *and* § 7 records a mutant against — M-a to M-z and M-aa to M-ae, every
> named test — and on the committed wire-golden pin whose bytes AC-19 and AC-20 forbid moving. No named
> test carries a RED record (nothing over an `Option` field compiles before #1597's lift), so each
> one's record is its mutant run: where the fabrication is unobservable at a skip or pass arm (§ 7),
> the mutant is the *opposite* error — an absent window excluding or swapping the account — or the
> **BUT NOT** the test guards (M-x, M-z). Test 8 carries its own mutant, M-aa: the substitution that grades
> test 7 finds no session reset on a weekly-only reading, so by § 7's skip-arm rule test 8 takes
> the opposite error of its own arm. The scenarios a named
> check or the merge judge grades with no test stay Tier B. D3, D4, D7, D10 and
> D14 are the operator's open decisions; every scenario one reaches is written for the recorded
> default and says so in its name. Written headless at Stage 3.5: every rule, tier and name here is
> ratification-pending.

## Rule 1 — a spare, a `use` target and the wire verdict are judged on the windows the account has: the weekly axis alone for a weekly-only account, the session axis alone for a weekly-less one (AD-4b), and one ranking axis for a mixed fleet (D7 default)

```gherkin
Scenario Outline: a spare's viability is decided by the windows it has
  # Tier A — graded by `pick_target_admits_a_weekly_only_spare_on_its_weekly_headroom` (#1602 § 6 test 1, `src/daemon/selection.rs`; mutant-proven by § 7 M-f: the session gate read as `usage.session.is_some_and(|s| s < session_ceiling)` at `src/daemon/selection.rs:252` makes it FAIL — the weekly-only spare is excluded), beside tests 2–3, bound by name: `pick_target_blocks_a_weekly_only_spare_on_the_weekly_line_like_a_two_window_one` (test 2; mutant-proven by § 7 M-x: the weekly line read as `usage.session.is_none() || usage.weekly.is_none_or(|w| w < weekly_ceiling)` at `src/daemon/selection.rs:247`, which exempts a weekly-only spare from it, makes its weekly-only half FAIL — the spare at weekly 0.98 is admitted; the two-window spare carries a session, so the mutant still excludes it, and M-f cannot stand in because it excludes the weekly-only spare at 0.98 as well) and `pick_target_never_weekly_blocks_a_weekly_less_spare` (test 3; mutant-proven by § 7 M-y: the weekly line read as `usage.weekly.is_some_and(|w| w < weekly_ceiling)` at `src/daemon/selection.rs:247` makes its viable half FAIL — the weekly-less spare at session 0.10 is excluded; M-f cannot stand in because that spare passes its session gate as well, and at 0.96 both forms exclude it) · covers AC-1
  Given a warmed-up fleet whose active account is two-window, with the session ceiling 0.95, the
        floor Some(0.80) and the weekly rotation line 0.97
    And a non-active, enabled spare whose served reading is <reading>
   When a target is picked, through `pick_target` (`src/daemon/selection.rs:69`) and
        `pick_target_with_reason` (`:170`) alike
   Then the spare is <verdict>
    And in test 1 a viable spare ranks by its weekly reset against a two-window peer, an absent reset
        sorting after a known one exactly as it does today

  Examples:
    | reading                               | verdict                                                 |
    | weekly-only, weekly 0.90              | viable                                                  |
    | weekly-only, weekly 0.98              | excluded on the weekly line                             |
    | two-window, session 0.50, weekly 0.98 | excluded on the weekly line, exactly like the row above |
    | weekly-less, session 0.10             | viable — no weekly line to block it (AD-4b)             |
    | weekly-less, session 0.96             | excluded by the session ceiling (AD-4b)                 |
    # The session anti-thrash gate becomes `usage.session.is_none_or(|s| s < session_ceiling)` and the
    # floor `floor.is_none_or(|f| usage.session.is_none_or(|s| s < f))` (`src/daemon/selection.rs:252-253`);
    # the weekly line becomes `usage.weekly.is_none_or(|w| w < weekly_ceiling)` (`:247`); `filter_map`
    # at `:246` drops only a failed poll; the `min_by` at `:256-265` is unchanged. REQ-ARM-B-001 BUT
    # NOT admitted because its session read as `0.0`; REQ-ARM-B-002 BUT NOT a second weekly line.
    # AD-4b is author-chosen by symmetry: no weekly-less account exists to measure against.
    # Test 2 asserts the weekly-only half with no other viable spare in the fleet, so the exclusion
    # reads as `None`: beside a viable peer the admitted spare can lose the ranking and M-x stays
    # invisible. Test 3 asserts its viable half with the weekly-less spare as the fleet's only spare:
    # it carries no weekly reset, so beside a viable peer with a known reset it loses the ranking
    # under the correct code and the mutant alike (§ 7).

Scenario: a weekly-only spare and a two-window spare rank on one weekly-reset axis, and an absent EMA reads as the safest landing on a tie (D7 default)
  # Tier A — graded by `pick_target_ranks_a_weekly_only_spare_and_a_two_window_spare_on_the_same_weekly_reset_axis` (#1602 § 6 test 24, `src/daemon/selection.rs`; mutant-proven by § 7 M-t: `velocity_rate` reading an absent EMA as `f64::INFINITY` in `src/daemon/selection.rs` makes its tie arm FAIL, the test ranking with a `Some` seed); under D7's alternative the tie expectation moves with the ratified rule · covers AC-11
  Given two viable spares: a weekly-only one with no retained session-velocity EMA and a two-window
        one with a sustained, climbing EMA
    And the selection carries a per-daemon seed, so the #612 tie-break axes are live
   When their weekly resets differ
   Then the spare with the sooner weekly reset wins, whichever kind it is
   When their weekly resets tie exactly
   Then the weekly-only spare is preferred — its absent EMA reads `0.0`, "the safest
        (lowest-velocity) landing", the rule `velocity_rate` already applies to any untrusted signal
    # REQ-ARM-B-012: a mixed fleet competes on one axis; the session filters apply only to accounts
    # whose session window applies. D7 is the pipeline default, ratification-pending, watched by
    # MET-R-001. BUT NOT a weekly-only spare preferred or penalised by a rule nobody ratified.

Scenario Outline: `use` serves the weekly verdict from a served reading, never an abort
  # Tier A — graded by `poll_viability_reads_a_weekly_less_reading_as_viable` (#1602 § 6 test 20, `src/use_account.rs`; mutant-proven by § 7 M-r: `reading.usage.weekly.unwrap_or(1.0) >= weekly_ceiling` at `src/use_account.rs:416` makes it FAIL — the weekly-less reading reads `WeeklyExhausted`), beside test 19, `poll_viability_serves_the_weekly_verdict_for_a_weekly_only_reading` (mutant-proven by § 7 M-z: the weekly guard read as `reading.usage.session.is_some() && reading.usage.weekly.is_some_and(|w| w >= weekly_ceiling)` at `src/use_account.rs:416`, so a reading without a session window bypasses the weekly gate, makes it FAIL — the weekly-only reading at weekly 0.98 falls through to `Viable`; M-r cannot stand in, since it acts only on an absent weekly window and test 19's reading carries one, and M-z leaves test 20 unchanged, since a weekly-less reading carries a session and the guard is false either way), bound by name · covers AC-9
  Given `sessiometer use <account>` without `--force`, no daemon reachable, and the weekly ceiling 0.97
    And the one-shot live poll of the account serves <reading>
   When the viability gate reads it
   Then the verdict is <verdict>, and no `UsageParse` abort occurs

  Examples:
    | reading                   | verdict                             |
    | weekly-only, weekly 0.98  | WeeklyExhausted                     |
    | weekly-only, weekly 0.50  | Viable                              |
    | weekly-less, session 0.42 | Viable — no line to exhaust (AD-4b) |
    # `poll_viability` (`src/use_account.rs:407-424`) reads
    # `reading.usage.weekly.is_some_and(|w| w >= weekly_ceiling)`; the fallback through
    # `cached_viability` (`:451-462`) is reached only on a cache miss. REQ-ARM-B-009 BUT NOT a bypass
    # of the weekly gate; BUT NOT a new `Unknown` verdict (design AD-15).

Scenario: the daemon-routed `use` gate reads the cached weekly-only reading's weekly axis
  # Tier A — graded by use_gate_reads_the_weekly_verdict_from_a_weekly_only_cached_reading (#1602 § 6 test 21) with mutant M-w (`usage.weekly.unwrap_or(1.0)` at `src/daemon/commands.rs:146-148`), which only the weekly-less arm below discriminates · covers AC-9
  Given a running daemon whose cached reading for the `use` target is weekly-only
   When the operator's `use` is re-validated daemon-side against the weekly rotation line 0.97
   Then `weekly_exhausted` is true for a cached weekly 0.98 and false for 0.50
    And `weekly_exhausted` is false for a cached weekly-less reading (weekly window absent, session 0.40), by AD-4b
    And the cached verdict is served with zero usage-endpoint requests, exactly as for a two-window
        target
    # `last_reading.is_some_and(|usage| usage.weekly.is_some_and(|w| w >= line))`
    # (`src/daemon/commands.rs`) — the rotation line, so an operator `use` never admits a
    # target `decide` would swap away from on the next tick.

Scenario Outline: the status wire's weekly-exhausted verdict comes from the weekly window alone
  # Tier A — graded by `status_response_marks_weekly_exhausted_from_the_weekly_window_alone` (#1602 § 6 test 25, `src/daemon/snapshot_build.rs`; mutant-proven by § 7 M-u: `usage.weekly.unwrap_or(1.0) >= line` at `src/daemon/snapshot_build.rs:439-440` makes its weekly-less arm FAIL); the committed `status_response_carries_handles_and_percentages_and_never_a_secret` (`src/daemon/snapshot_build.rs:642`) must stay green but is executor-adapted under #1597 AC-8, so it is not the grader · covers AC-12
  Given a snapshot account whose served reading is <reading>, against the weekly rotation line 0.97
   When the status response is built
   Then its row carries `weekly_exhausted: <verdict>`
    And under `STATUS_SCHEMA_VERSION` `1.14` a weekly-only row carries `session_pct: null` beside a
        present `weekly_pct` — the transitional state #1597 and design B-6t record; no key is added
        by this item

  Examples:
    | reading                   | verdict                                |
    | weekly-only, weekly 0.98  | true                                   |
    | weekly-only, weekly 0.50  | false                                  |
    | weekly-less, session 0.42 | false — never weekly-exhausted (AD-4b) |
    # `is_some_and(|usage| usage.weekly.is_some_and(|w| w >= line))` at
    # `src/daemon/snapshot_build.rs`. A pass arm: the verdict means exactly "the daemon will
    # not rotate onto this", and a window the account lacks cannot be exhausted.
```

## Rule 2 — the live swap arms skip an absent session axis: only the weekly arm moves a weekly-only active, nothing projects or marks a session it does not have, and a weekly-only blocked spare names a weekly hold

```gherkin
Scenario Outline: `decide` swaps a weekly-only active on its weekly threshold alone
  # Tier A — graded by `decide_swaps_a_weekly_only_active_on_its_weekly_threshold_alone` (#1602 § 6 test 4, `src/swap.rs`; mutant-proven by § 7 M-g: `usage.session.is_none_or(|s| s >= session_threshold)` at `src/swap.rs:130` makes its `0.96` arm FAIL — it swaps — the opposite error, since the fabrication is unobservable at this skip arm) · covers AC-2
  Given the active's served reading is weekly-only at weekly <weekly>, against the thresholds
        session 0.95 and weekly 0.97
   When the pure two-threshold predicate decides
   Then the decision is <decision>

  Examples:
    | weekly | decision |
    | 0.96   | Hold     |
    | 0.98   | Swap     |
    # `decide` (`src/swap.rs:129-134`) becomes `usage.session.is_some_and(|s| s >= session_threshold)
    # || usage.weekly.is_some_and(|w| w >= weekly_threshold)`, signature unchanged. REQ-ARM-B-003:
    # only the weekly arm can swap a weekly-only active away.

Scenario: a weekly-only active at weekly 0.99 swaps with reason weekly, logs no session percent and arms no parked landing
  # Tier A — graded by `a_weekly_only_active_swaps_with_reason_weekly_and_arms_no_parked_landing` (#1602 § 6 test 5, `src/daemon.rs`; mutant-proven by § 7 M-a: `session_pct: Some(to_pct(active_usage.session.unwrap_or(0.0)))` at `src/daemon.rs:4412` makes it FAIL, `running 1 test` then `FAILED`), beside test 28, `a_parked_landing_watch_releases_with_no_record_when_the_departed_account_reads_weekly_only` (mutant-proven by § 7 M-ac: the resolution at `src/daemon.rs:3326-3327` skipping an absent session keeps the watch armed), and test 29, `a_weekly_only_active_within_cooldown_gets_no_raw_ceiling_bypass` (mutant-proven by § 7 M-ad: `at_raw_ceiling` read as `is_none_or` at `src/daemon.rs:4323` bypasses the cooldown), bound by name · covers AC-2, AC-14
  Given a warmed-up fleet whose active's served reading is weekly-only at weekly 0.99, outside
        cooldown, with a viable two-window spare
   When the tick evaluates the reactive arm
   Then a swap fires whose event carries `reason: SwapReason::Weekly` and `session_pct: None`
    And the rendered `swap` line carries `reason=weekly` and no `session_pct=` token, every other
        token unchanged
    And no `ParkedLanding` is armed on the departed account
    And within cooldown of a prior swap the same active gets `TickAction::SkippedCooldown`, never
        the raw-ceiling bypass (`at_raw_ceiling`, `src/daemon.rs:4323`), which needs a `Some` session
        at the ceiling — test 29
    And the departed account's parked-landing watch, had one been armed by a `reason=session` swap,
        releases with no landing record when that account's next reading inside the landing window is
        weekly-only — test 28
    # The reason (`src/daemon.rs:4403-4407`) reads `Session` only from a `Some` session at or over its
    # threshold, else `Weekly`; the landing watch (`:4425-4429`) is armed only on reason `Session`,
    # so its `decision_pct` is always read from the `Some` the reason came from; the parked-landing
    # resolution (`landing_pct`, `:3327`) reads a landing from a `Some` session only and releases the
    # watch with no landing record when the departed account's next reading carries none — a change
    # from `fd8640b`, where a live below-ceiling reading keeps it armed (`:3324-3338`).
    # REQ-ARM-B-003 BUT NOT `reason=Session` for a weekly-only active; BUT NOT a landing record
    # without a session.

Scenario: the velocity projection holds when the active's session window is absent
  # Tier A — graded by `velocity_swap_holds_when_the_active_session_window_is_absent` (#1602 § 6 test 6, `src/daemon.rs`; mutant-proven by § 7 M-h: the early return removed and the projection run on `active_usage.session.unwrap_or(1.0)` at `src/daemon.rs:5317-5331` makes it FAIL — it swaps — over a warmed-up fleet with a viable spare) · covers AC-3
  Given a warmed-up fleet whose active's served reading is weekly-only, with a sustained, climbing
        retained session-velocity EMA and a non-zero horizon
   When the velocity arm evaluates
   Then it returns `TickAction::Held` before any session value is read
    And no `swap` event is emitted from it
    # `let Some(session) = active_usage.session else { return TickAction::Held };` at the top of
    # `velocity_swap` (`src/daemon.rs:5280`), so the projection (`:5317-5331`) and its `swap` line's
    # `session_pct` (`:5390`) read a bound `Some` by construction. REQ-ARM-B-003 BUT NOT a velocity
    # projection over an absent session.

Scenario: exhaustion bookkeeping keys a weekly-only peer on its weekly line only
  # Tier A — graded by `out_of_rotation_for_a_weekly_only_peer_keys_on_its_weekly_line_only` (#1602 § 6 test 7, `src/daemon.rs`; mutant-proven by § 7 M-i: `reading.session.is_none_or(|s| s >= ceiling)` in `out_of_rotation` at `src/daemon.rs:5928` makes its `0.90` arm FAIL — the window is armed), beside test 8, `exhausted_poll_window_ignores_an_absent_session_axis` (mutant-proven by § 7 M-aa: `exhausted_poll_window` bailing to `exhausted_poll_secs` whenever the session is absent makes its 0.98 half FAIL; it discriminates only because test 8 places the weekly reset strictly inside `poll_secs..exhausted_poll_secs` of `now_secs`, so the clamp at `src/daemon.rs:6036-6038` does not decide the answer; M-i's substitution inside `exhausted_poll_window`, `:6028-6030`, is unobservable), bound by name · covers AC-4
  Given a non-active, enabled peer whose served reading is weekly-only
   When the tick books the poll against the weekly rotation line 0.97 and the session ceiling 0.95
   Then at weekly 0.98 the peer is out of rotation and the slow-poll window is armed, keyed off its
        weekly reset
    And at weekly 0.90 it is not out of rotation and no window is armed
    And the pure window (`exhausted_poll_window`, `src/daemon.rs:6008`) over the 0.98 reading
        returns exactly the seconds to the weekly reset, placed strictly inside
        `poll_secs..exhausted_poll_secs` of now; over a weekly-only reading at 0.50 no exhausted dimension
        contributes a reset, so it falls back to the hourly ceiling `exhausted_poll_secs` exactly as
        today's reading with no applicable reset does (`:6038`)
    # `out_of_rotation = active != Some(i) && (reading.weekly.is_some_and(|w| w >= line)
    # || reading.session.is_some_and(|s| s >= ceiling))`; an absent axis contributes no window.
    # #1602 § 6's test 8 now states this no-reset fallback (corrected 2026-11-01) — the function
    # returns a `Duration`, never an `Option`.

Scenario: a weekly-only blocked spare names a weekly hold, never the nameless fallthrough
  # Tier A — graded by `all_exhausted_relief_names_a_weekly_hold_for_a_weekly_only_blocked_spare` (#1602 § 6 test 9, `src/daemon.rs`; mutant-proven by § 7 M-j: `session_blocked = usage.session.is_none_or(|s| s >= line)` at `src/daemon.rs:6128` makes it FAIL — `spare_relief` returns `None` and the relief falls to the hold with its timestamp omitted) · covers AC-5
  Given no viable target and a single enabled spare whose served reading is weekly-only at weekly
        0.98 with a weekly reset
   When the all-exhausted relief is computed against the block lines session 0.95 and weekly 0.97
   Then it returns `(SwapReason::Weekly, that spare, Some(its weekly reset))`
    And never the `(None, None)` fallthrough at `src/daemon.rs:6154`, which names nothing
    # `session_blocked = usage.session.is_some_and(|s| s >= session_block_line)`, `weekly_blocked`
    # likewise (`:6128-6129`); `spare_relief`'s signature — two `bool`s — is unchanged (`:6056`).
    # REQ-ARM-B-005 BUT NOT `cause: session`; the existing relief tests bind two-window spares only.

Scenario: the fleet is not out of capacity while a weekly-only spare has headroom
  # Tier A — graded by `next_swap_is_not_no_viable_target_while_a_weekly_only_spare_has_headroom` (#1602 § 6 test 10, the one test over the symptom the operator reported; mutant-proven by § 7 M-k: Rule 1's selection mutant, `is_some_and` at `src/daemon/selection.rs:252`, makes it FAIL) · covers AC-5
  Given a warmed-up fleet whose active is two-window and whose only other enabled, healthy account
        has a served weekly-only reading at weekly 0.50
   When the `status` reply is built
   Then `next_swap` names that spare as the target
    And it is never `no_viable_target` and never `AwaitingData` — `src/daemon.rs:5481-5494` is
        unchanged: a served reading is not an unpolled one
    # REQ-ARM-B-006, origin the operator's "crippled": the fleet of 17 accounts, 15 weekly-only since
    # the lift, reported out of capacity because their readings failed to parse. BUT NOT
    # `AwaitingData` while served readings exist.

Scenario: the near-limit fast poll reads false for a weekly-only active, stated and pinned (D4 default)
  # Tier A — graded by `active_near_limit_reads_false_for_a_weekly_only_active` (#1602 § 6 test 16, `src/daemon.rs`; mutant-proven by § 7 M-o: the floor comparison read as `active_usage.session.unwrap_or(1.0) >= floor` at `src/daemon.rs:5617` makes it FAIL — the band engages); under D4's alternative the expectation moves and the band gains a weekly predicate in a separate item · covers AC-7
  Given a warmed-up daemon with the near-limit path enabled, whose active's served reading is
        weekly-only at weekly 0.96, with a sustained, climbing EMA and a non-zero horizon
   When the near-limit band is evaluated
   Then `active_near_limit` is false on both branches — the in-band floor comparison and the
        sustained-EMA projection
    And `near_limit_fast_poll_engaged` stays false
    # The band is session-only (`src/daemon.rs:5605-5641`); `plausible_active_usage` (`:5614-5619`)
    # corrects the session through `plausible_session`, whose signature stays
    # `(Option<SessionHighWater>, &Usage) -> Usage` (`src/swap.rs:703`) and which still window-matches
    # (`:1755`); the `session` field of the passed and returned `Usage` is `Option` in, `Option` out; the floor
    # comparison at `:5631` reads false on `None`. REQ-ARM-B-008 BUT NOT an accident of a
    # session-only predicate: this scenario and its test are the statement.

Scenario: the stale-low correction never fabricates a session for an absent window
  # Tier A — graded by `stale_low_correction_never_fabricates_a_session_for_an_absent_window` (#1602 § 6 test 17, `src/swap.rs`; mutant-proven by § 7 M-p: `plausible_session` returning the mark's session for an absent session at `src/swap.rs:703-710` makes it FAIL) · covers AC-7, AC-11
  Given a retained session high-water mark from the active's last two-window window
    And a fresh served reading whose session window is absent — session None, session reset None
   When the plausibility correction is applied to the fresh reading
   Then the corrected session is None — no mark is applied to a window the reading does not have
    And `is_stale_low(Some(mark), &reading)` is false
    # `plausible_session` (`src/swap.rs:703-710`) and `is_stale_low` (`:690-692`) consult the mark
    # only through `applies_to`, which needs the reading's own stamp; invariant I-1 (a stamp is
    # `Some` only when its fraction is) makes an absent window carry no stamp, so no correction can
    # apply. REQ-ARM-B-004 BUT NOT a stale-reading guard that fires because the fraction vanished.

Scenario: the high-water fold yields no mark without a session and seeds a fresh one when a window appears
  # Tier A — graded by `session_high_water_fold_yields_no_mark_for_an_absent_session_and_reseeds_when_one_appears` (#1602 § 6 test 18, `src/swap.rs`; mutant-proven by § 7 M-q: the stamp read as `let Some(window) = usage.session_resets_at else { return mark };` at `src/swap.rs:652`, so an unstamped reading keeps the previous mark, makes its first half FAIL); the committed `session_high_water_needs_a_window_stamp` (`src/swap.rs:1603`) must stay green but its `reading(0.90, None)` helper is executor-adapted, so it is not the grader · covers AC-8
  Given a retained mark from the previous session window
   When a served reading whose session window is absent is folded
   Then the fold yields no mark — the previous mark is released exactly as a vanished stamp
        releases it today
   When a later reading carries session Some(0.30) with a fresh window stamp
   Then the fold seeds a new mark at 0.30 in that window
    And nothing durable remembered the absence in between: no persisted flag, no config key, no new
        field in the daemon's state file — `git diff origin/main -- src | grep -nE
        '^\+.*(weekly_only|session_absent|no_session)'` prints only added lines inside `#[cfg(test)]`
        modules: test names, test-local bindings such as the `weekly_only_usage` of tests 17 and 18,
        and comments
    And removed lines are not read: retiring #1613's `UsageReport.session_absent` in this same pull
        request (#1596 AC-17) deletes production lines that name it
    # `let window = usage.session_resets_at?; let session = usage.session?;` (`src/swap.rs:651-665`);
    # I-1 makes the second `?` unreachable alone — it stands for the type. REQ-ARM-B-004, and both
    # directions of REQ-ARM-B-011: a window that disappears stops acting as a current reading, a
    # window that reappears is a fresh window with every session gate applying again.
```

## Rule 3 — the blind gate's surfacing half: every pure predicate reads false on an absent anchor session, no `blind_active` block is fabricated under the transitional `1.14` wire (D10 default), and the server-directed arm is unchanged (D3 default) but retains no `status` notice off a session-less anchor under that wire (D10's analogue)

```gherkin
Scenario: the pure gate predicates and the anchor correction read false, and `None`, on an absent anchor session
  # Tier A — graded by `blind_gate_armed_reads_false_on_an_absent_anchor_session` (#1602 § 6 test 11, `src/daemon.rs`; mutant-proven by § 7 M-l: `anchor_session.unwrap_or(1.0) >= BLIND_GATE_RISK_BAND` at `src/daemon.rs:471` makes it FAIL — armed) and `plausible_anchor_session_passes_an_absent_anchor_through` (test 12, `src/swap.rs`; mutant-proven by § 7 M-m: `plausible_anchor_session` returning `Some(m.session)` for a `None` anchor at `src/swap.rs:745-750` makes it FAIL) · covers AC-6
  Given an anchor whose session window is absent, blind for `BLIND_GATE_SECS + 1` seconds
   When the single-source predicate `blind_gate_armed(blind_secs, None)` is read
   Then it is false
    And `blind_velocity_projected_armed(BLIND_GATE_SECS + 1, None, Some(a sustained climbing EMA),
        0.95)` is false
    And `plausible_anchor_session(Some(mark), None)` is None, while with `Some(0.40)` and a higher
        mark in the same window it is the mark, as today
    And `blind_anchor_high_water` (`src/daemon.rs:5236-5241`) yields no high-water percent for a
        `None` anchor
    # The predicates take `anchor_session: Option<f64>` (design AD-5): the eligibility SLI
    # (`note_blind_gate_eligibility`, `src/daemon.rs`), `blind_swap`'s anchor arm
    # (`:4924-4927`) and `blind_active_view` (`src/daemon/records.rs`) all express this one
    # predicate, so none can drift — the single-source rule the gate states. ADR-0017's "Never fires
    # on a `None`/missing reading" (`docs/adr/0017-bounded-blindness-preemptive-swap-not-header-observation.md:189`)
    # is extended by #1593 to an anchor whose session window does not apply. The arming half —
    # whether a weekly anchor arms a preemptive swap — is OAD-2 (D3), outside this item.

Scenario: a weekly-only active blind past the gate fires no blind swap and no eligibility event (D3 default)
  # Tier A — graded by `a_weekly_only_blind_active_past_the_gate_fires_no_blind_swap_and_no_eligibility_event` (#1602 § 6 test 13, `src/daemon.rs`; mutant-proven by § 7 M-n: `blind_swap`'s anchor arm read as `is_none_or(|s| s >= self.session_blind_risk_band)` at `src/daemon.rs:4924-4927` makes it FAIL — a blind swap fires — over a warmed-up fleet with a viable spare; test 11's mutant cannot stand in, since this arm never calls `blind_gate_armed`) · covers AC-6
  Given a warmed-up daemon whose active is blind for `BLIND_GATE_SECS + 1` seconds behind a
        weekly-only anchor, with no `Retry-After` holding it and a viable spare
   When the blind arm and the eligibility SLI evaluate
   Then no blind swap fires — the tick takes the historical skip, never a swap on an absent anchor
    And no `BlindGateEligible` event is emitted
    And because that event is only ever emitted past `blind_gate_armed`, its `session_pct` stays
        `u8` — an `Option` that is always `Some` would misdescribe the seam
    # REQ-ARM-B-007 BUT NOT a swap fired on an absent anchor; AC-6 BUT NOT a weekly arming rule
    # (OAD-2). The `u8` is a ratification-pending deviation from design B-3 row 15, which lists
    # `src/observability.rs` among the fields that move; under the uniform-`Option` alternative
    # the emitter writes `Some(to_pct(s))` from the bound `Some` and no assertion here changes.

Scenario: no `blind_active` block is emitted for a weekly-only blind active until the wire can say "none" (D10 default)
  # Tier A — graded by `blind_active_view_emits_no_block_for_a_weekly_only_blind_active_until_the_wire_can_say_so` (#1602 § 6 test 15, `src/daemon/records.rs`; mutant-proven by § 7 M-c: emitting the block with `last_known_session_pct: to_pct(anchor.session.unwrap_or(0.0))` at `src/daemon/records.rs:515` makes it FAIL under the D10 default) · covers AC-6
  Given an anchor whose session window is absent, the active blind past `BLIND_GATE_SECS`, not
        quarantined
   When `blind_active_view` projects the status block under `STATUS_SCHEMA_VERSION` `1.14`
   Then it returns None — no `blind_active` block, because `last_known_session_pct` is a `u8` there
        (`src/daemon/snapshot.rs:167`) and a `0` would be the fabrication this item forbids
    And `status` therefore makes no auto-protection claim on the strength of the session gate
    # The cost, stated (design B-6t row 5): until B-6 lands, such an account reads as a failed-poll
    # row on the CLI and the panel, its blind duration and percent are not shown, and the
    # `Retry-After` and velocity report arms are silent for it — no regression against the running
    # bundle, which cannot read such an account at all. Under D10's alternative (B-6 in the same pull
    # request) the block is emitted with `last_known_session_pct: None`, and `auto_protection_degraded`
    # is computed from four arms: the session gate false, the `Retry-After` arm unchanged, the velocity
    # arm false, and #1599's fourth, status-only arm — `blind_secs > BLIND_GATE_SECS &&
    # anchor.session.is_none()` reads `auto_protection_degraded: true` and arms no swap — so test 15
    # under the alternative pins `true` past the gate, not `false`, because a plain `false` would read
    # as the auto-protection claim REQ-ARM-B-007 forbids. The representation is #1599's (#1599 § 2a).
    # BUT NOT a block whose percent is fabricated.

Scenario: the server-directed arm still swaps a weekly-only active away, and its swap line omits the session percent
  # Tier A — graded by `the_swap_line_omits_session_pct_for_a_blind_preempt_from_a_weekly_only_anchor` (#1602 § 6 test 27, `src/daemon.rs`; mutant-proven by § 7 M-e: `to_pct(anchor.session.unwrap_or(0.0))` at `src/daemon.rs:5044` makes it FAIL) · covers AC-6, AC-14
  Given a warmed-up daemon whose active is blind behind a weekly-only anchor, held by a
        `Retry-After` past `session_blind_swap_secs`, outside cooldown, with two viable spares
  # Two, not one: a session-less `Retry-After` swap is speculative (`src/daemon.rs`), and the
  # #582 reserve breaker's `OnlyCandidate` hold (`:5019-5030`) keeps it unless at least two viable
  # targets exist, so with one spare the swap never fires. The fixture is
  # `blind_retry_after_daemon(0.29, 0.10, …)` with both peers at 0.10; the held case uses 0.90 (`:10655`).
   When the blind arm evaluates
   Then the #582 server-directed arm fires on its own authority — unchanged by this item
    And the `swap` event carries `reason: SwapReason::BlindPreempt` and `session_pct: None`, and its
        line carries no `session_pct=` token
    # `blind_swap`'s `Retry-After` arm (`src/daemon.rs`) is reached whatever the anchor's
    # session; the `swap` line's `session_pct` there is the anchor's `Option` (`:5044`). Its condition
    # (`:4933-4937`) never reads the session, so a session-less anchor still swaps here; what the
    # swap retains for `status` is the next scenario's. The anchor
    # arm itself read false (the first scenario of this rule). PR #1538's velocity arm on this seam
    # composes with it through `plausible_anchor_session`; whichever lands second rebases.

Scenario: a `Retry-After` swap off a session-less anchor retains no `status` notice under `1.14` (D10's analogue)
  # Tier A — graded by `a_retry_after_blind_preempt_from_a_weekly_only_anchor_retains_no_status_notice` (#1602 § 6 test 30, `src/daemon.rs`; mutant-proven by § 7 M-ae: the record built whatever the anchor carries, with `last_known_session_pct: last_known_session_pct.unwrap_or(0)` at `src/daemon.rs:5063-5068`, makes it FAIL — the notice is retained and the wire serves `"last_known_session_pct":0`), beside the committed `blind_swap_fires_past_t_with_a_viable_target` (`src/daemon.rs:9919`), which keeps a two-window anchor's notice with its `68` · covers AC-6
  Given a warmed-up daemon whose active is blind behind a weekly-only anchor, held by a
        `Retry-After` past `session_blind_swap_secs`, outside cooldown, with two viable spares
  # Two, for the reason test 27's scenario gives: one spare is the `OnlyCandidate` hold, not a swap.
   When the server-directed arm swaps the active away
   Then the swap fires and the #582 walk record `retry_after_swaps` gains it
    And no `BlindPreemptSwapRecord` is retained, so the snapshot's `recent_blind_preempt_swap` is
        None and the `status` frame carries no such key
    # Under `STATUS_SCHEMA_VERSION` `1.14` the record's `last_known_session_pct` is a `u8`
    # (`src/daemon/records.rs`) that `recent_blind_preempt_swap_view` (`src/daemon.rs:597-614`)
    # copies onto the wire's `BlindPreemptSwap.last_known_session_pct: u8`
    # (`src/daemon/snapshot.rs:204`); a `0` there would print "last-known 0%" in the CLI narration
    # (`src/cli.rs:3259-3270`). So the record is built as `last_known_session_pct.map(…)`, None here,
    # and `record_swap` (`src/daemon.rs`) has already cleared any earlier notice. The cost
    # (design B-6t): until #1599 lands, the CLI prints no narration and no `use <from>` undo hint
    # for such a swap; the menu-bar app does not decode the notice; the swap and its log line are
    # unchanged. #1599 § 2b restores the notice with the percent omitted and retires test 30.
    # Rejected: an `Option<u8>` record here, which would make the wire key omittable under `1.14`.
    # BUT NOT a notice whose percent is fabricated.
```

## Rule 4 — every per-window event field is a typed absence rendered as an omitted token; `reliability` decodes the omission; a served weekly-only reply is a live poll and the velocity fold skips the axis it lacks (D14 default)

```gherkin
Scenario: a weekly-only active entering blindness logs `near_limit=false` and no session percent
  # Tier A — graded by `a_weekly_only_active_entering_blindness_logs_near_limit_false_and_no_session_percent` (#1602 § 6 test 14, `src/daemon.rs`; mutant-proven by § 7 M-b: `session_pct: Some(to_pct(prev.session.unwrap_or(0.0)))` at `src/daemon.rs:3406` makes it FAIL) · covers AC-6, AC-14
  Given an active account whose last live reading was weekly-only
   When its next poll fails — the live-to-blind entry edge
   Then the `BlindEnter` event carries `near_limit: false`, `session_pct: None` and
        `weekly_pct: Some(_)`
    And its line carries `near_limit=false` and `weekly_pct=` and no `session_pct=` token
    # `near_limit` at `src/daemon.rs:3396` reads `prev.session.is_some_and(|s| s >= ceiling)` — the
    # anchor's tag for the whole episode, so the exit line agrees by construction; the `BlindWindow`
    # site at `:2864` reads the same way, and `near_limit` stays `bool` on all three events. The
    # anchor's own copy-through (`:3397-3402`) is #1597's. Rendered by the `session_high_water_pct`
    # precedent (`src/observability.rs`): an absent token means "none", never "unknown".

Scenario: a served weekly-only reading ends a blind episode, and the exit line carries the weekly burn alone
  # Tier A — graded by `a_weekly_only_reading_ends_a_blind_episode_and_the_exit_event_carries_no_session_percent` (#1602 § 6 test 22, `src/daemon.rs`; mutant-proven by § 7 M-s: `session_at_recovery: Some(to_pct(fresh.session.unwrap_or(0.0)))` at `src/daemon.rs:3419` makes it FAIL) · covers AC-10, AC-14
  Given an episode opened on a two-window anchor — session Some, weekly Some — for the active
        account
   When the next poll is served as a weekly-only reading
   Then the poll classifies `Live` — `classify_poll` (`src/daemon/classify.rs:55-62`) is unchanged,
        `Ok(_)` is `Live` — so the gap and blind streaks end and sampling resumes
    And the `BlindExit` event carries `session_at_recovery: None` and `weekly_at_recovery: Some(_)`
    And its line carries `weekly_burn_pct=` and omits `session_burn_pct=` — a burn is derived only
        when both the anchor and the recovery carry that window
    And the `BlindWindow` line for the same episode omits `session_at_recovery=`
    # REQ-ARM-B-010 BUT NOT a classifier edit; BUT NOT a cadence change. Rendered per axis:
    # `blind_exit` (`src/observability.rs:3193`), `blind_window` (`:3149`), `blind_enter` (`:3165`).
    # Every other token on those lines is unchanged.

Scenario: the velocity event skips an absent axis and the session EMA is untouched
  # Tier A — graded by `usage_velocity_skips_an_absent_axis` (#1602 § 6 test 23, `src/daemon/classify.rs`; mutant-proven by § 7 M-d: computing the session delta from `unwrap_or(0.0)` on each side at `src/daemon/classify.rs:121-125` makes it FAIL) · covers AC-11, AC-14
  Given two consecutive served readings of one account: the previous with session Some(0.40), the
        next with session None, both with a weekly fraction
   When the velocity is computed
   Then `usage_velocity` returns `(None, Some(weekly delta))` — each axis `prev.zip(next).map(…)`
    And the `UsageVelocity` event fires because the weekly axis is `Some` and non-zero, and its line
        carries `weekly_pct_per_min=` and `weekly_delta_pct=`, and neither `session_pct_per_min=` nor
        `session_delta_pct=` — `reliability` reads only the second (`src/reliability.rs:1722`)
    And the retained session-velocity EMA is not folded — `note_session_velocity` runs only when both
        endpoints carry a session, and the retained EMA is untouched
    # The emission and the fold at `src/daemon.rs:2805-2816`; `UsageVelocity`'s deltas
    # (`src/observability.rs:2039`) become `Option<i16>`. REQ-ARM-B-011: a window that disappears
    # stops acting as a current reading. AD-16.

Scenario: a `blind_window` line without a session axis is censused as "no session window", never `malformed` (D14 default)
  # Tier A — graded by `a_blind_window_line_without_a_session_axis_is_censused_as_no_session_window_not_malformed` (#1602 § 6 test 26, `src/reliability.rs`; mutant-proven by § 7 M-v: `record_blind_projection`'s `fd8640b` rule restored at `src/reliability.rs:1251-1262` makes it FAIL — the line counts as `malformed` — and by § 7 M-ab: the withdrawn no-bucket arm, the line counted in `total` and in no bucket, makes it FAIL — the partition sum is short by one) · covers AC-14, AC-15
  Given a log holding a `blind_window` line with `duration_secs`, `near_limit=false` and no
        `session_pct=` / `session_at_recovery=`; the two half-axis lines (one with `session_pct=` and
        no `session_at_recovery=`, as test 22 makes the daemon emit, and its mirror); a line with
        `session_pct=xx`; a line without `duration_secs`; a `blind_enter` / `blind_exit` pair without
        session tokens; and a `usage_velocity` line without `session_pct_per_min=` and without
        `session_delta_pct=`
   When `reliability` parses it
   Then the three session-less lines each increment `total` and `without_session_window`, and none
        of them is `malformed`
    And the `session_pct=xx` line and the line without `duration_secs` are `malformed`
    And `n_blind_windows == n_without_session_window + n_projectable + n_below_arm_gate +
        n_without_velocity + n_malformed` — every line in exactly one disclosed bucket, the committed
        partition (`src/reliability.rs`, `:2276-2277`, `:3492-3493`, pinned at `:5674-5678`,
        issue #484) kept
    And `render_json` carries `"schema": 14` and `n_without_session_window` beside `n_malformed`,
        and the human render's "outside the arm's domain" line shows the count
    And the pair parses into `blind_entries` / `blind_exits` — those parsers read `ts`, `acct`,
        `near_limit`, `duration_secs` and `swapped_away` only (`src/reliability.rs:1544-1551`,
        `:1559-1571`)
    And the `usage_velocity` line contributes no session rate (`:1720-1735` reads
        `session_delta_pct=` only)
    # The bucket sits ahead of the arm's gates: a line without a session axis on both ends has nothing
    # to project at any duration. The version moves `13` → `14` because the key is always present
    # (the bump rule on `JSON_SCHEMA_VERSION` in `src/log.rs:159-166`); `RELIABILITY_USAGE`
    # (`src/cli.rs:1260`) advertises `schema:14`, and the three `build/fixtures/cli-renders/reliability-*.txt`
    # goldens regenerate with the `CLI-Goldens-Rebaselined:` trailer. Decided by the scoping party,
    # ratification-pending; the no-bucket arm is rejected because it breaks the #484 partition.
    # REQ-RDG-C-001 BUT NOT an event-field change that moves `reliability`'s `JSON_SCHEMA_VERSION`
    # without saying so; BUT NOT `n_malformed` changed by a well-formed no-session line. The
    # blind-window SLIs gated on `near_limit=true` (`:1513`) exclude a weekly-only account's windows
    # by that gate. `log --json` (`LogWire`, `src/log.rs:865`) carries the line's own tokens, so
    # `JSON_SCHEMA_VERSION` at `src/log.rs:167` stays `2`.

Scenario: the two event percents that never meet the absent state keep their shape
  # Tier B — pending: no test in #1602 § 6 is named for this; the manual swap's sentinel is pinned by the committed tests at `src/daemon/commands.rs:1408`, `:1499`, `src/observability.rs:8526` and `src/use_account.rs:938`, which the executor adapts (`session_pct: 0` → `Some(0)`), and `BlindGateEligible.session_pct` by the second scenario of Rule 3; the merge judge reads both adaptations · covers AC-14
  Given the manual / forced swap at `src/daemon/commands.rs:224`, whose `session_pct=0` is a
        pre-existing, documented "not a window read" marker, and the `BlindGateEligible` event, only
        ever emitted past an armed gate
   When the event fields lift
   Then the manual swap's sentinel becomes `Some(0)` and its line still prints `session_pct=0` —
        replacing it with an omitted token is a separate decision, out of scope here
    And `BlindGateEligible.session_pct` stays `u8`
    # An `Option` that is always `Some` would misdescribe a seam; the sentinel is a pre-existing
    # meaning, not an absent window. AC-14 BUT NOT an event skipped because its session axis is
    # absent; BUT NOT `session_pct=0` emitted for an absent window.
```

## Rule 5 — CONSTRAINT: no site substitutes a number for an absent window, in production or in a test; the unchanged arms are unchanged; the compiler's enumeration is reconciled; the diff and the `1.14` wire are bounded

```gherkin
Scenario Outline: each fabrication on a name surface is caught by its recorded mutant
  # Tier A — graded by the six tests named in the table, each run by exact name against its mutant on a scratch copy and printing `running 1 test` then `FAILED` — #1602 § 7's record, carried in the pull request body · covers AC-16, and the RED-owed halves of AC-2, AC-6, AC-11, AC-14
  Given a scratch copy of the pull-request head with <site> mutated to <mutant>
   When <test> runs by exact name
   Then it prints `running 1 test` and FAILS — the <id> record

  Examples:
    | id  | site                           | mutant                                                                          | test                                                                                           |
    | M-a | `src/daemon.rs:4412`           | `session_pct: Some(to_pct(active_usage.session.unwrap_or(0.0)))`                 | test 5 `a_weekly_only_active_swaps_with_reason_weekly_and_arms_no_parked_landing`               |
    | M-b | `src/daemon.rs:3406`           | `session_pct: Some(to_pct(prev.session.unwrap_or(0.0)))`                         | test 14 `a_weekly_only_active_entering_blindness_logs_near_limit_false_and_no_session_percent` |
    | M-c | `src/daemon/records.rs:515`    | the block emitted with `last_known_session_pct: to_pct(anchor.session.unwrap_or(0.0))` | test 15 `blind_active_view_emits_no_block_for_a_weekly_only_blind_active_until_the_wire_can_say_so` (D10 default) |
    | M-d | `src/daemon/classify.rs:121-125` | the session delta computed from `unwrap_or(0.0)` on each side                  | test 23 `usage_velocity_skips_an_absent_axis`                                                   |
    | M-e | `src/daemon.rs:5044`           | `to_pct(anchor.session.unwrap_or(0.0))`                                          | test 27 `the_swap_line_omits_session_pct_for_a_blind_preempt_from_a_weekly_only_anchor`         |
    | M-ae | `src/daemon.rs:5063-5068`     | the record built whatever the anchor carries, with `last_known_session_pct: last_known_session_pct.unwrap_or(0)` | test 30 `a_retry_after_blind_preempt_from_a_weekly_only_anchor_retains_no_status_notice` |
    # A `None` test over a skip or pass arm is observationally identical under `unwrap_or(0.0)` (an
    # absent session reading as `0.0` passes every below-the-line gate as `None` does), so those
    # tests pin the opposite error, and the fabrication there is caught by the enumeration below,
    # the grep below and the review of every `Some(` wrapper the pull request adds. AC-16 BUT NOT a
    # passing suite offered in place of the six runs; BUT NOT a run on the committed branch state.

Scenario: the twelve production files carry no numeric default over a window fraction
  # Tier B — pending: a named mechanical check, not a test — over the twelve production files AC-19 names, the AC-16 float grep prints exactly the twelve lines it prints at fd8640b, compared by content and count and never by line number because this item's own edits shift the numbers (re-measured this run): ten `unwrap_or_default()` lines over text, collection or config values (`src/daemon/commands.rs:5586`, `src/daemon/snapshot_build.rs:83`, `src/use_account.rs:1637`, `:4955`, `:6903`, `:7101`, `:7274`, `:7678`, `src/cli.rs:5127`, `:5744`) and two `map_or(0.0, |v| v.rate)` lines over the velocity EMA rate (`src/daemon.rs:4204` and `src/daemon/selection.rs:148`, row 12's D7 default), none over a window fraction, and nothing else; AC-16's integer grep (`unwrap_or\(0\)|map_or\(0,|unwrap_or\(100\)|map_or\(100,`) prints exactly its 17 lines at fd8640b (re-measured this run: six wall-clock fallbacks, five column widths, a hash seed, two bearer counts, two comments, and `src/cli.rs:4112`'s `weekly_pct.unwrap_or(100)`, unreachable behind `weekly_exhausted`; 7 of them over the earlier eleven-file pathspec, none over a window percent); design AD-4 rejected a source lint for its false positives, so the per-arm `None` tests and the six mutants are the instrument and the review of each `Some(` wrapper the signal (`git grep -nE 'Some\(0\.0\)' -- src` is 10 lines at fd8640b, re-measured this run) · covers AC-16
  Given the pull-request head
   When `git grep -nE 'unwrap_or\(0\.0\)|unwrap_or\(1\.0\)|unwrap_or_default\(\)|or\(Some\(0\.0\)\)|map_or\(0\.0|map_or\(1\.0'`
        runs over the twelve production files this item touches
   Then it prints the same twelve lines as at fd8640b — ten `unwrap_or_default()` and two
        `map_or(0.0, |v| v.rate)` — compared by content and count, never by line number, and
        nothing else
    And `git grep -nE 'unwrap_or\(0\)|map_or\(0,|unwrap_or\(100\)|map_or\(100,'` over the same files
        prints the same 17 lines as at fd8640b, so no integer default over an `Option<u8>` window
        percent was added
    And every `Some(` wrapper this item adds in a test wraps a value the test means as a measured
        reading (#1597 AC-8's rule), and every `Some(0.0)` in `src` asserts or constructs a measured
        idle window
    And a weekly-only reading in any new test is constructed as `session: None,
        session_resets_at: None`, never by zeroing
    # REQ-RDG-C-001: a read site meeting the absent state takes one of four named moves — skip, pass,
    # exclude, name — and never a numeric default; "including tests" (design § 4.2, premortem P-11).

Scenario: the propagation sweep rewords the two-window premise where it is stated as a rule
  # Tier B — pending: documentation — the AC-17 grep over the twelve files AC-19 names (25 lines at fd8640b, re-measured this run; the 6 recorded-unchanged lines expected on the pull request) and its canary (the same command with only the pattern swapped, `git grep -n -i -E 'SPDX-License-Identifier' -- src/daemon.rs src/daemon/selection.rs src/daemon/classify.rs src/daemon/records.rs src/swap.rs src/observability.rs src/use_account.rs src/daemon/commands.rs src/daemon/snapshot_build.rs src/reliability.rs src/cli.rs src/config/validate.rs`: 12 lines, one per file, at fd8640b and the same 12 on the pull request, run in the same shell; the old `git grep -c -E 'predicate on BOTH dimensions' -- src/daemon/selection.rs`, `2` at fd8640b and `0` on the pull request, stays as a separate correction check, not the canary) are the named checks, with the wrapped-phrase query AC-17 adds because the first cannot see a phrase split across a line end — `git grep -n -i -A1 -E '(^|[^a-z])both$' -- <the twelve files> | grep -i -E -- '-[0-9]+-[[:space:]]*(//[/!]?)?[[:space:]]*(dimensions|windows|usage windows|quota|fractions)([^a-z]|$)'`, run exactly as AC-17 gives it, which prints the continuation line of each wrapped instance: 3 lines at fd8640b (re-measured this run); each line's disposition is the merge judge's read against AC-17's list · covers AC-17
  Given the corrected proposition "every decision arm reads both dimensions of every account"
   When the sweep runs on the pull request and its cardinality is reported with the canary beside it
   Then the lines that state the premise as a rule read "every dimension the account has" — the nine
        #1596's sweep routed here, the further seven AC-17 lists in this item's files
        (`src/daemon.rs`, `:3413`, `:4354`, `src/daemon/selection.rs:44`,
        `src/observability.rs`, `:3161`, `src/swap.rs:175`) and the two propagated from #1597
        AC-10 (`src/observability.rs`, `:3181`)
    And the six lines AC-17 records as unchanged stay — `src/daemon.rs`, `:9622`, `:9655`,
        `src/observability.rs`, `src/cli.rs`, `:11246`: window semantics, a failed poll's
        render, or a two-window fixture's own narrative
    And `src/daemon/records.rs:218` is corrected by #1597, not here
    And the wrapped-phrase query's three lines take these dispositions: `src/observability.rs`
        (`SwapReason`'s doc, "when BOTH / dimensions are at/over their triggers") is reworded, because
        row 3 changes the attribution it states (`Session` only from a `Some` session at or over its
        threshold); `src/reliability.rs` (a test comment, "billing BOTH / windows") is
        reworded, the same committed-tail claim as `src/swap.rs:175`; `src/observability.rs`
        (a test comment, "both / windows untouched") stays, a two-window fixture's own narrative
    And any line the re-run returns that AC-17 does not list takes its own disposition with the reason
    # AC-17 BUT NOT a sweep keyed on citations to the types; BUT NOT a sweep count reported without the
    # canary's count beside it, from the same shell.

Scenario: the three unchanged arms are unchanged, and the poll cadence with them
  # Tier B — pending: `git diff origin/main --stat -- src/daemon/keep_warm.rs` printing nothing and `git diff origin/main -- src/daemon/classify.rs` showing hunks inside `usage_velocity` and its tests only are the named checks; the committed #80/#366 stagger-lock tests are the cadence pin (REQ-ARM-Q-001, goal delta 0) and the pull request body states both; no new test is owed (#1602 § 6: "Row 14 adds no test") · covers AC-10, AC-13, and the unchanged half of AC-1
  Given the pull-request head
   When the diff against `origin/main` is read
   Then `src/daemon/keep_warm.rs` carries no change — `should_keep_warm_retry` (`:44`) reads
        liveness only, so a weekly-only account's keep-warm schedule equals a two-window account's
        with the same credential state (REQ-ARM-C-001)
    And `classify_poll` (`src/daemon/classify.rs:55-62`) is unchanged
    And selection's `filter_map` (`src/daemon/selection.rs:246`) and `min_by` (`:256-265`) are
        unchanged
    And the #80/#366 stagger locks are untouched: aggregate `/usage` requests per hour and per-tick
        spacing are the same before and after — the fix ends requests that yield nothing, it does not
        change cadence
    # Design § 4.5, "What does not move". REQ-ARM-B-010 BUT NOT a cadence change.

Scenario: the compiler's enumeration is reconciled for this item's share, and the share is bounded
  # Tier B — pending: `cargo build` on the lift-only tree (#1597 AC-6) and the pull request body are the record; nothing executor-uneditable grades a reconciliation table — the merge judge reads it against Specification § 1's fifteen rows · covers AC-18, AC-19
  Given the compiler's error list from the lift-only build, with every flagged site's owner
   When this item's commit lands
   Then every site the list assigns to B-3 is resolved by the move its row in #1602 Specification § 1
        names — skip, pass, exclude or name — and no site by a move its row does not name
    And every row of Specification § 1 is matched to its sites in the pull request body
    And this item's production changes touch only `src/daemon.rs`, `src/daemon/selection.rs`,
        `src/daemon/classify.rs`, `src/daemon/records.rs` (`blind_active_view` only; the structs are
        #1597's), `src/swap.rs`, `src/observability.rs`, `src/use_account.rs`,
        `src/daemon/commands.rs`, `src/daemon/snapshot_build.rs`, `src/reliability.rs`, `src/cli.rs`
        (`RELIABILITY_USAGE`'s `schema:` token only) and `src/config/validate.rs` (the floor comments
        and the zero-floor message only)
    And nothing under `src/usage.rs`, `src/daemon/snapshot.rs`, `src/daemon/keep_warm.rs`,
        `src/usage_store.rs`, `src/stats.rs`, `src/usage_stats.rs`, `apps/menubar/`,
        `.github/workflows/`, top-level `scripts/`, `.cargo/` or `deny.toml`; under
        `build/fixtures/` only the three `cli-renders/reliability-*.txt` goldens the census bucket
        moves, regenerated through `cargo test -- --ignored emit_cli_render_goldens` with the
        `CLI-Goldens-Rebaselined:` trailer, every other golden that command rewrites byte-identical
    And no account email, label, uuid, token or credential appears in the diff
    # The compile unit: #1596's parser commit, #1597's model commit, then this one. A red site that
    # is not a row of Specification § 1 is a copy-through #1597 missed — then the order is wrong,
    # not the site.

Scenario: the five committed wire goldens reproduce byte-identical under `1.14` (D10 default)
  # Tier A — graded by `the_committed_wire_goldens_still_match_the_daemon_encoders` (`src/daemon/snapshot_build.rs:3929`, committed at fd8640b, named verbatim in AC-20; the goldens it reads under `build/fixtures/` stay byte-identical to `origin/main` by AC-19 and AC-20, so the executor edits neither side — the status wire's fixture-integrity pin, read as the B-2 stub reads it; a ratification-pending reading, since the pin's RED is structural rather than recorded) · covers AC-20
  Given `STATUS_SCHEMA_VERSION` is `1.14` (`src/daemon/snapshot.rs:538-541`) and the five goldens
        `build/fixtures/wire-heartbeat-basic.json`, `wire-snapshot-basic.json`,
        `wire-snapshot-canonical-scrub.json`, `wire-snapshot-expiry.json`,
        `wire-snapshot-next-swap.json`, every row of which is a two-window account
   When the daemon encoders re-emit them in-process
   Then the bytes match the committed files without regeneration — this item's `weekly_exhausted`
        verdict (row 13), `next_swap` relief (row 6) and `blind_active_view` move (row 7) leave them
        untouched
    And a new omittable key, the next minor and the `last_known_session_pct` reshape are the
        status-wire item's (block B-6), never this item's
    # Under D10's alternative (B-6 in the same pull request) the minor moves and the goldens with
    # it — this scenario then does not hold on the pull-request head, which is why it is tagged.
```

## Coverage

Each acceptance criterion of #1602, with the scenarios that cover it. A criterion no scenario covers
is named as such, with what grades it instead.

| AC | Scenarios | What grades it |
|---|---|---|
| AC-1 — selection (rows 1, 2; REQ-ARM-B-001, B-002, AD-4b) | Rule 1 "a spare's viability is decided by the windows it has" (five rows); Rule 5 "the three unchanged arms are unchanged" (the `filter_map` and `min_by` half) | tests 1, 2, 3 against M-f, M-x, M-y (Tier A) |
| AC-2 — reactive swap, reason, parked landing (row 3; REQ-ARM-B-003) | Rule 2 "`decide` swaps a weekly-only active on its weekly threshold alone"; Rule 2 "a weekly-only active at weekly 0.99 swaps with reason weekly…" | test 4 against M-g, test 5 against M-a, test 28 against M-ac, test 29 against M-ad (Tier A) |
| AC-3 — velocity projection (row 4; REQ-ARM-B-003) | Rule 2 "the velocity projection holds when the active's session window is absent" | test 6 against M-h (Tier A) |
| AC-4 — exhaustion bookkeeping (row 5; REQ-ARM-B-003) | Rule 2 "exhaustion bookkeeping keys a weekly-only peer on its weekly line only" | tests 7 and 8 against M-i and M-aa (Tier A) |
| AC-5 — all-exhausted relief (row 6; REQ-ARM-B-005, B-006) | Rule 2 "a weekly-only blocked spare names a weekly hold…"; Rule 2 "the fleet is not out of capacity while a weekly-only spare has headroom" | tests 9–10 against M-j, M-k (Tier A) |
| AC-6 — the blind gate's surfacing half (row 7; REQ-ARM-B-007; D3, D10) | Rule 3, all five; Rule 4 "a weekly-only active entering blindness logs `near_limit=false`…" | tests 11–13 against M-l, M-m, M-n; tests 14, 15, 27, 30 against M-b, M-c, M-e, M-ae (Tier A); the committed `blind_swap_fires_past_t_with_a_viable_target` staying green |
| AC-7 — the near-limit band (row 8; REQ-ARM-B-008; D4) | Rule 2 "the near-limit fast poll reads false for a weekly-only active…"; Rule 2 "the stale-low correction never fabricates a session…" | tests 16–17 against M-o, M-p (Tier A) |
| AC-8 — the high-water fold and nothing durable (row 9; REQ-ARM-B-004, B-011) | Rule 2 "the high-water fold yields no mark without a session and seeds a fresh one…" | test 18 against M-q (Tier A); the committed `session_high_water_needs_a_window_stamp` staying green; the `'^\+.*(weekly_only\|session_absent\|no_session)'` grep over added lines, judged against the `#[cfg(test)]` modules |
| AC-9 — the `use` verb (row 10; REQ-ARM-B-009, AD-4b) | Rule 1 "`use` serves the weekly verdict from a served reading…" (three rows); Rule 1 "the daemon-routed `use` gate reads the cached weekly-only reading's weekly axis" | tests 19, 20, 21 against M-z, M-r, M-w (Tier A) |
| AC-10 — liveness and the exit edge (row 11; REQ-ARM-B-010) | Rule 4 "a served weekly-only reading ends a blind episode…"; Rule 5 "the three unchanged arms are unchanged" | test 22 against M-s (Tier A); the `classify.rs` diff check |
| AC-11 — the velocity EMA and the tie-break (row 12; REQ-ARM-B-012; D7) | Rule 4 "the velocity event skips an absent axis…"; Rule 1 "a weekly-only spare and a two-window spare rank on one weekly-reset axis…"; Rule 2 "the stale-low correction never fabricates a session…" (`is_stale_low`) | tests 23, 24, 17 against M-d, M-t, M-p (Tier A) |
| AC-12 — the weekly-exhausted wire verdict (row 13; AD-4b) | Rule 1 "the status wire's weekly-exhausted verdict comes from the weekly window alone" (three rows) | test 25 against M-u (Tier A) |
| AC-13 — keep-warm and rate neutrality (row 14; REQ-ARM-C-001, Q-001) | Rule 5 "the three unchanged arms are unchanged, and the poll cadence with them" | the `keep_warm.rs` diff check; the committed stagger-lock tests; the pull request body |
| AC-14 — event fields are typed absences with omitted tokens (row 15; REQ-RDG-C-001; D14) | Rule 2 "a weekly-only active at weekly 0.99 swaps…"; Rule 3 "the server-directed arm still swaps…"; Rule 4, all five | tests 5, 14, 22, 23, 26, 27 against M-a, M-b, M-s, M-d, M-v, M-e (Tier A); the sentinel's committed tests (executor-adapted) |
| AC-15 — `reliability` decodes the emitted shape (row 15; D14) | Rule 4 "a `blind_window` line without a session axis is censused…" | test 26 against M-v and M-ab (Tier A); the `schema:14` move in `src/reliability.rs` and `src/cli.rs:1260`; the three regenerated `reliability-*.txt` goldens with their trailer |
| AC-16 — no numeric default, by grep and by review | Rule 5 "each fabrication on a name surface is caught by its recorded mutant" (six rows); Rule 5 "the twelve production files carry no numeric default…" | the six mutant runs (Tier A); the float and integer greps as named checks with no test by design (AD-4) |
| AC-17 — propagation sweep over the corrected claim | Rule 5 "the propagation sweep rewords the two-window premise…" | the sweep grep with its same-invocation `SPDX-License-Identifier` canary (12 lines); the pull request body's per-line disposition |
| AC-18 — the enumeration is reconciled for this item's share | Rule 5 "the compiler's enumeration is reconciled…" | `cargo build` on the lift-only tree; the pull request body; the merge judge |
| AC-19 — this item's share of the diff is bounded | Rule 5 "the compiler's enumeration is reconciled…" (its last three steps) | `git diff --stat origin/main`; review |
| AC-20 — conformance to the Build Reference | Rule 5 "the five committed wire goldens reproduce byte-identical under `1.14`" | `the_committed_wire_goldens_still_match_the_daemon_encoders` (Tier A, ratification-pending reading); `git diff origin/main -- build/fixtures/wire-*.json` printing nothing (the three `reliability-*.txt` render goldens move under AC-15, not here) |
| AC-21 — gates | none | the five `test` steps, `msrv`, both `deny` checks, and, because the three `reliability` render goldens under `build/fixtures/**` move, the `swift` job and an armed `panel-goldens` run (read off its `[panel-goldens] max drift` line), in CI; never `cargo test -- --include-ignored` |
| AC-22 — the records precede the change | none | the merge order on `main`: #1593 before the pull request that carries this item |
| AC-23 — commit convention | none | review of the squash commit: `(feat) daemon: … (issue #1602)` with `Closes #1602.`, #1596's and #1597's `Closes` lines beside it |
