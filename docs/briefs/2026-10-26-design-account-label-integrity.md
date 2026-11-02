---
type: design-brief
date: 2026-10-26
source: docs/design/account-label-integrity-solution-design.md
workflow: /design-solution
status: draft   # PROVISIONAL: the PRD is unratified (dor_status failed) and load-bearing open questions stand
---

# Design Brief: Account Label Integrity

## Problem

A roster label is the only handle most of sessiometer's surfaces use, and labels are not unique by
ratified design. The CLI's `capture` and `login`, the socket `capture` and `config-set` can each make one
label name a second account without saying so, a `login` can rename a different account than the
operator meant, one credential can sit under two entries unnoticed, and nothing downstream — the status
wire, the panel, usage history, swap lines — can tell two same-label accounts apart. This design makes
each of those writes audible where it happens and gives the downstream surfaces an identity a relabel
does not change, without enforcing uniqueness.

## Key Decisions

Every decision below is pipeline-decided and ratification-pending; defaults the pipeline picked among
real alternatives are marked **(default)**.

1. **One collision rule, at the roster.** Import's before/after bearer count moves beside `Account`, and
   every roster writer calls it, so a writer added later cannot ship without it (D-1).
2. **The capture plan carries what the reply needs.** `plan_capture` returns the previous label and the
   collisions before any write, so any refusal a policy chooses happens before the stash is written (D-2).
3. **Notices are named templates; acknowledgements carry facts.** Every new string is scanned by the #160
   framing guard, and the app renders its own text from structured fields (D-3).
4. **"In rotation" counts enabled entries (default).** It is the codebase's own meaning of the phrase, and a
   roster with no parked entry prints exactly what it prints today (D-3, DQ-10).
5. **Shared credentials are detected at the creating write, after the lock.** A refresh rotates the token,
   so later detection is mostly blind; reading after the lock keeps swaps unaffected (D-4).
6. **Local stores and the event log key on `account_uuid`.** Only the status/watch wire waits on the open
   identifier-form question, so that question stays costly-to-reverse rather than one-way (D-5).
7. **The status/watch identifier and the next-swap target's identifier share one minor bump**, whose number
   is read at merge time because #1457 claims the next minor too (D-6).
8. **The usage store expands, then backfills once.** New samples carry the uuid; one pass attributes older
   samples whose label had exactly one bearer, guarded by a sibling sentinel file **(default placement)** that
   an older binary cannot erase (D-7).
9. **`stats`, `reliability` and `log` read what they read today (default).** Using the key there would change
   output after a relabel or reshape `stats`' wire (D-8); whether F4's invariant reaches `stats` is an
   interpretation the operator weighs, not a fact (the `stats-consumes-the-key` default below).
10. **Swap lines carry uuid tokens only for an ambiguous label**, so lines for ordinary rosters stay
    byte-identical; a new `credential_shared` line records each detected pair (D-9).
11. **Acknowledgements gain optional fields only, and a Swift test asserts each decoded value**, because the
    app silently ignores unknown keys and throws on unknown `result` values (D-10).
12. **Import checks the payload before the swap lock and each entry before its write**; the artifact format
    stays frozen (D-11).
13. **Orphan stashes get a metadata-only census; a reaper only with a grace window.** The pipeline recommends
    `report` over `reap` **(default)**, because no lock spans a capture's stash write and roster save (D-12).
14. **An ordinal, "work 2 of 2" by roster order, is a duplicate set's one handle on every surface (default)**:
    panel rows, the next-swap callout, the capture card, Settings and the header. Rows also get
    colour slots no other row uses and a spoken cue; the identifier is never displayed on a row, and rosters
    without a duplicate render unchanged (UX-3, UX-5).
15. **Notices are inline on the surface that made the write and persist until the next action there**; a
    card with a notice skips its 1.8-second reset, and the Settings note follows the roster's state, so a
    duplicate made anywhere is explained there (UX-1, UX-2, UX-11).
16. **A notice names an entry by uuid only beside an ambiguous label, next to its ordinal, in the CLI reply,
    the capture card and the Settings note (default).** Acknowledgements carry the uuids as facts; the F4
    panel rows never show an identifier, and a Settings row shows its own uuid only if the operator asks
    for it (UX-9, § 8.1). The PRD-internal conflict this settles provisionally is Open Questions 8 and 9.
17. **Switching to an account whose label is shared is shown blocked before the click (default)**, with the
    reason and the remedy; today the daemon refuses it after the click. The daemon's own rotation still
    reaches either account. Switching by identifier instead is Open Question 11 (UX-10).

## Design Tracks

| Track | Approach | Key trade-off |
|---|---|---|
| Technical Architecture | modular monolith kept; shared kernel for the rule; one plan seam; expand-then-backfill | no new component, at the cost of touching several modules |
| Data Architecture | no new store; optional `account_uuid` on samples; sentinel-guarded backfill | attribution happens against the roster at upgrade time |
| UX/IA (menu-bar only) | no new screen; inline persistent notices; the ordinal as a duplicate set's one handle; a shared-label switch shown blocked | the identifier stays off the rows, and an ordinal moves if the roster is reordered |
| API Design | additive fields; one minor wire bump; tags only for chosen refusals; trailing event tokens | the parity gate covers new fields, not the existing `count` drift |
| Security Architecture | delta threat model; nothing derived from credential bytes leaves memory | the uuid appears on more local surfaces, never a secret |
| Testing Architecture | master test plan with capability ids binding every requirement; RED-first gates | some checks stay manual: F7's walk, the armed panel goldens, Linux |

## Open Questions

Every question below except question 9 holds the design at `draft`; question 9 builds nothing until it is
answered. Each says what was checked, the options and what they cost, what the pipeline recommends, and what
happens if nobody answers. The recommendations are the pipeline's reading of the evidence. The design
document records questions 2 to 7 neutrally, as open stubs (§ 12.2), and question 1 in § 14.1, deciding
none of them; questions 8 to 11 return to the PRD with the design's provisional answer (§ 14.6).

1. **Will the operator ratify the PRD?** The PRD's readiness gate failed on approval gaps alone: the problem
   statement is unvalidated and every requirement is pipeline-authored. The `/scope` orchestrator
   acknowledged the verdict on the operator's behalf so this design could run; that acknowledgment is
   pipeline-supplied and ratification-pending, not the operator's. Options: ratify as written; ratify with
   edits, then re-run the readiness gate; reject. If unanswered, nothing here is scoped into work items. Stage 3 of
   the same run scoped the items anyway, under the scope brief's `proceed-past-failed-dor` row; design § 14.1
   records the reconciliation.
2. **`account-key-form` — what identifier does the status and watch wire carry?** Checked: the tree applies
   issue #15 in conflicting ways (the canary docs say "never … account-uuid", the usage events carry the
   uuid); the app already receives uuids from `config-get`; `status --json` prints the raw wire, so its
   content can be pasted publicly; a hyphenated uuid passes the redaction meter. Options: the uuid itself
   (simplest; one identity everywhere; pastable); a value derived from it (keeps the raw uuid out of pasted
   status output only, needs a derivation contract, and a long unseparated digest trips the meter); a
   daemon-local id (dominated: unstable across restarts). Recommendation: the evidence leans to the uuid,
   because the uuid already sits in `config.toml`, in `list`, in the usage-family events and in the
   `config-get` view on the same socket. If unanswered, F4's wire and panel half waits; the store, the
   event log and every other feature proceed.
3. **`duplicate-label-policy` — warn or refuse?** Warn extends ratified R-6 and needs no re-ratification;
   the duplicate stays until the operator renames one. Refuse contradicts ratified AC-6, spends an
   interactive `login`, and adds a rejection tag the app must model. Recommendation: warn. Unanswered, F1's
   oracles cannot fix their assertion form.
4. **`login-mismatch-policy` — what does `login <label>` do when it lands on another account?** The reply
   names both labels under every option. Options: keep the rename (disclosed); warn on every renaming
   revive; refuse it (blocks the deliberate rename path the tests pin); compare an email-shaped label with
   the captured address (needs #846's rule and F7's premise 3). Recommendation: keep the rename, disclosed.
5. **`label-normalization-policy` — what makes two labels the same?** Options: none; case folding;
   punctuation folding (wrong as a default: it merges real hyphen pairs); NFC; report-only confusable
   detection (a dependency or a table). The design's rule has a hook for any of them. Recommendation: none,
   as today, because no look-alike pair has been reported and the operator's real labels differ by hyphens
   that folding would merge; NFC is the cheap step if one is ever wanted.
6. **`import-duplicate-uuid-policy` — refuse or warn on a malformed payload?** Only a hand-built or
   corrupted artifact reaches it. Options: refuse the payload; warn and import that account roster-only;
   warn and keep last-wins. Recommendation: refuse, the safest for a path the exporter never produces.
7. **`orphan-stash-policy` — reap, report or accept?** Reaping deletes keychain material irreversibly and
   must avoid an in-flight capture and the witness; reporting is metadata-only; accepting writes the record
   and no code. Recommendation: report.
8. **`panel-notices-name-the-uuid` — returns to the PRD.** The PRD points two ways: § 3 defers "an operator
   affordance to see the uuid behind each label" and AC-WIRE-001 keeps F4's identifier off the display;
   REQ-NTC-C-001 (a uuid "where the label alone is ambiguous"), REQ-STSH-B-002 (a Must: both entries "by
   account uuid where their labels are equal, on the surface that performed the write") and § 9.4 name it.
   The design names an entry's uuid in a notice beside an ambiguous label, next to its ordinal. Under the
   first reading the notices keep their ordinals and drop the uuids, and both requirements' uuid clauses
   narrow for the panel; "Rename one in Settings" stays either way, because the ordinal tells the rows apart.
   Recommendation: the second reading, which meets the Musts as written. If unanswered, the requirements as
   written govern.
9. **`settings-row-shows-uuid` — returns to the PRD.** Should an ambiguous Settings row also show its own
   uuid? It is the narrowest form of the affordance § 3 defers until "The operator adds it", so it is not
   built until the operator answers yes. Yes lets a row be matched with `sessiometer list` and the event
   log; no leaves that match to the note's uuids and to the CLI. Recommendation: no answer is needed to ship;
   the ordinal tells the rows apart without it.
10. **`backfill-reading-of-ac-usg-001` — returns to the PRD.** Under decision 9 nothing reads a backfilled
   key before the raw tier ages it out, so the one-time backfill pays off only if `stats` consumes the key
   inside that window. If "re-keyed" means the migration moment, the backfill is owed; if it means the
   moment a reader switches to the key, turnover re-keys the tier and the backfill item is dropped.
   Recommendation: answer it together with `stats-consumes-the-key`; the backfill is F4's last item either
   way.
11. **`panel-switches-by-identifier` — returns to the PRD.** No requirement names the panel's switch, and
   today the daemon refuses a shared label after the click. The design shows that refusal before the click,
   so neither account can be switched to from the panel until one is renamed, as the CLI's `use work`
   refuses too, although `sessiometer use <uuid>` reaches either from the CLI today. The alternative sends
   the identifier as the swap target: a uuid needs no daemon change, a derived key needs one, so it is
   weighed with question 2. The operator's parity ruling of 2026-07-31 favours the alternative once the wire
   carries the uuid. Recommendation: switch by identifier if question 2 picks the uuid, keeping the blocked
   switch for an older daemon; otherwise the blocked switch. If unanswered, the blocked switch stands.

Also returned to the PRD by the product lens, not blocking: MET-T-004 reads 0 once the store is keyed while
`stats`, `reliability` and the panel's stats view still merge and split by label (a Stats-tab cue is recorded
as an option, not built); the Serves line "the operator can always tell which account a label names" is
delivered on the CLI and, on the panel, by the ordinal; and REQ-DOC-B-003's wording assumes the
keep-rename answer to question 4.

Defaults to ratify, which do not block: **`stats-consumes-the-key`** (default no: continuity changes output
after a relabel, separation reshapes `stats`' wire) and **`colour-seed-from-identifier`** (default no: it
would repaint every panel once), and **`stored-key-name-and-form`** (design § 14.6 item 16; default: the
per-sample key is named `account_uuid`, holds the uuid verbatim and is left out of a line that has none).

Design lock: both lenses were dispatched on the full design; their verdicts are in the design's § 14.9,
verbatim, and summarised here.

- Product lens (`product-strategist`), first reading: BLOCKING — the uuid-free panel dropped REQ-STSH-B-002's
  uuid clause while § 16 reported it covered; it raised non-blocking concerns as well.
- Product lens (`product-strategist`), re-grade of the second revision: CONFIRM — the baseline blocking
  concern is resolved; its non-blocking concerns were answered in the third revision, which this lens has not
  graded.
- UX lens (`ux-architect`), first reading: BLOCKING — the next-swap callout was ambiguous and the panel's
  switch refused on a shared label, because the design leaned on a per-row marker R-2 had removed; "this
  one" was lost on the way to Settings; duplicate Settings rows sounded identical. Answered by decisions
  14, 15 and 17 and question 11.
- UX lens (`ux-architect`), re-grade of the third revision: CONFIRM — every baseline blocking concern
  resolved; it raised non-blocking concerns as well.
- A fourth revision took up the UX re-grade's non-blocking concerns with the remedies it proposed, after
  both final verdicts; neither lens has graded it, and the design's § 14.9.5 lists every change.

## Full Design

[`docs/design/account-label-integrity-solution-design.md`](../design/account-label-integrity-solution-design.md)
— the full solution design, with the coverage matrices (§ 16, § 16b), the open decision stubs (§ 12.2)
and the risk register (§ 14).
