---
type: architecture-decision-record
number: 34
title: "The log handle charset is unconstrained; surviving one is each reader's obligation"
date: 2026-08-11
status: accepted
decision_makers: [Oleksii PELYKH (maintainer)]
---

# ADR-0034: The log handle charset is unconstrained; surviving one is each reader's obligation

## Status

**Accepted** — 2026-08-11. Settles the open question issue **#1185** raised: the module doc of
`src/observability.rs` stated that enforcing the handle charset "is the meter's job (#15)", and no
component does it. Companion to **ADR-0006**-style freeze reasoning on the durable surface; the
record-splitting half of the same grammar was closed separately by **#1092** / PR #1183.

## Context

The event log is a flat space-separated `key=val` grammar. Every value on it is a handle, an enum,
a number or a timestamp — except two, both free-form:

- the resolved `claude` path (issue #786), which `path_value` percent-encodes, and
- the account **`label`**, which `README.md` documents as written **verbatim** as the account
  handle and calls "the one operator-chosen, free-form field on the durable surface".

A label containing a space or `=` therefore splits one field into several. Three components could
have prevented that, and none does:

| Component | What it actually constrains |
|---|---|
| `config::validate` | `label` is rejected only when empty after `trim` (`src/config/validate.rs:312`) |
| `config::validate::account_uuid_violation` | `[A-Za-z0-9_-]{1,128}` on the **`account_uuid`** alone (issue #1052) |
| `redaction::meter` | token prefixes, credential-blob fingerprints, email SHAPES, high-entropy runs — no charset |

The login-**failure** path widens this further: `capture::login`'s `Err` arm logs an `account_uuid`
harvested from `~/.claude.json` *before* the roster gate that would have constrained it (PR #1183
states that ordering), so even a fully-constrained roster leaves a free-form value reaching a line.

Two of this repo's readers were demonstrably wrong under such a value, each proven by a test that
failed before this change:

- `observability::last_swap_at` selected a line by `line.contains(" event=swap ")`. A handle
  spelled `a b event=swap c` on a `monitor_401` line made that line answer the `use` verb's swap
  cooldown query (#63/#10) — observed: `Some(epoch 40)` where the correct answer was `None`.
- `observability::last_refresh_outcomes` split the account field at the **first** ` outcome=`. A
  handle spelled `my work outcome=refreshed x` truncated to `my work` — its own text up to that
  first separator, the whole leading run and not some shorter word inside it — and read the rest as
  the outcome, so an account genuinely labelled `my work` had its real outcome OVERWRITTEN in the
  offline `list` view (#120) by one it never had. Its doc asserted the opposite — that such a
  handle "truncates to no
  recognized outcome (skipped) rather than mis-attributing" — which held only while the text after
  the handle's own ` outcome=` was not a valid token.

## Decision

**The handle charset stays unconstrained, and the label keeps being written verbatim. Surviving a
free-form handle is the obligation of each READER of the durable log, not of the writer, the config
gate, or the meter.**

Concretely:

1. `Event::to_log_line` continues to encode **control characters only** (`single_line`, issue
   #1092). Whitespace and `=` are not encoded.
2. `config::validate` continues to accept any non-empty `label`.
3. The `#15` meter is **not** the owner of this obligation, and `src/observability.rs`' module doc
   no longer says it is.
4. A reader of the event log must not locate a field by substring search where a free-form value
   could spell it. The two readers this module owns now read the event key by **field position**
   (`event=` is always the second whitespace-delimited field, and an RFC 3339 stamp is space-free,
   so no later value can occupy that slot), and `last_refresh_outcomes` takes the **last**
   ` outcome=` on the line rather than the first — the writer emits `outcome=` once and every field
   after it is a number or an enum token, so the last one is always the writer's and everything
   before it is the handle, whole.

## Alternatives considered

**Encode whitespace at the writer, and teach the readers to decode.** Rejected. `single_line`'s own
doc argues the exclusion: encoding whitespace would REFORMAT well-formed lines, and the event log's
grammar is frozen (`src/log.rs`) with durable records — every space-bearing label ever written would
render differently from that point on. It would also break `last_refresh_outcomes`' verbatim
round-trip, which is currently correct and pinned by
`last_refresh_outcomes_matches_a_handle_with_a_space_verbatim`. Decoding would then have to land at
every reader, making them vintage-aware — precisely what PR #1183 pinned against
(`a_mixed_vintage_log_reads_old_split_records_exactly_as_it_always_did`). Broadest blast radius of
the three, against a harm that is mis-attribution within one record, never a forged record.

**Constrain `label` at config-validate time.** Rejected, and not only on compatibility grounds.
`README.md` documents the label as free-form and verbatim, and #1092 is explicit that it must not be
narrowed; an ordinary roster (`label = "Work Account"`) would start failing validation, which is a
breaking change to a file the operator already owns. Decisively, it would **not close the hole**: the
login-failure path logs a harvested `account_uuid` before any roster gate applies, so a value that
never passed through `config::validate` still reaches a line. A gate that neither preserves the
documented contract nor achieves the property is the worst of the three.

**Accept the per-reader anchoring as it stood, and only correct the module doc.** Rejected as
stated, and adopted only in amended form. The premise — that the anchoring already handles this —
is false: both readers were shown wrong above. Correcting the module doc while leaving the readers
mis-attributing would have replaced one overclaim with another. Choosing "the reader owns it" as
the answer requires the readers to actually own it, which is what this ADR does.

## Consequences

### Positive

- The obligation now has a named owner and a mechanical test for each half, instead of a doc note
  pointing at a component that never performed it.
- No change to the writer, so no already-written record renders or reads differently, and no reader
  becomes vintage-aware. The property holds identically on pre-#1183 and post-#1183 lines, because
  neither a space nor an `=` is a control character and `single_line` never touched either.
- The operator keeps a verbatim label, which is what `README.md` promises and what `capture`'s
  email-prefill default (#447, #444) depends on.
- Field position is a stronger invariant than any substring anchor: it cannot be spelled by a value,
  whereas every anchor this module used could be. It is also mechanically held rather than argued:
  `every_event_line_carries_its_event_key_as_the_second_field` sweeps the exhaustive
  `every_event_variant` corpus, so a future variant that renders `event=` anywhere but field 1 fails
  a test instead of silently un-anchoring both readers.

### Negative / trade-offs

- **The residue is named, not closed.** Readers outside `src/observability.rs` still tokenize on
  whitespace and justify it with *"handles/values are whitespace-free by the log's grammar"* — a
  premise `label` does not satisfy:
  - `log::field` (`src/log.rs`) — splits a line on whitespace, then at the first `=`.
  - `reliability::parse_events` (`src/reliability.rs`) — builds a field MAP, so a later token
    OVERWRITES an earlier key; `path_value`'s doc already records that field position cannot help a
    field-map reader, which is why the path is encoded and the label is not.
  - `usage_stats::parse_swap_events` (`src/usage_stats.rs`) — the same field-MAP shape, carrying
    the same premise in its own words, and reached by the `stats` verb on its default path
    (`stats::build_report` → `parse_swap_events`). A value spelling ` event=swap ` overwrites the
    real `event` key, so the line runs through the swap arm and a fabricated swap enters
    `contribution_counts`, `swap_breakdown` and the `active_at` attribution timeline.

  The count is deliberately carried by the list rather than stated: an earlier revision of this
  ADR said "two" and omitted the third, which `src/observability.rs` had already been naming
  alongside `reliability::parse_events` before this change. All three are outside this ADR's
  change; each is a separate reader with its own contract, and none is reachable from the `use`
  cooldown or the offline `list` view this ADR hardened.
- A handle that spells this grammar still renders confusingly to a human reading the log or to an
  ad-hoc `grep`. This ADR makes the repo's own readers correct; it does not make such a label a good
  idea.
- Choosing the reader as the owner means the obligation must be re-honoured by every FUTURE reader
  of the durable log. That is a standing cost, paid per reader, in exchange for never reformatting a
  durable record.
