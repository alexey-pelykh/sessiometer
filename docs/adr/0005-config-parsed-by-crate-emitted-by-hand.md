---
type: architecture-decision-record
number: 5
title: "Config parsed with the toml crate, emitted by hand (by design)"
date: 2026-07-02
status: accepted
decision_makers: [Oleksii PELYKH (maintainer)]
---

# ADR-0005: Config parsed with the `toml` crate, emitted by hand (by design)

## Status

**Accepted** — 2026-07-02. Records the parse-with-`toml` / emit-by-hand asymmetry
already in force in `src/config.rs`, and the reasoning for it (issue #181, a
reinvented-wheels / library-usage audit finding; "low-priority consistency
observation, not a bug"). A decision in force, not a code change — same posture as
ADR-0002/0003.

> **Update (#415, #443):** the `session_floor` field used as a rendering illustration below
> was renamed to **`target_max_usage`** (#415) and then **`target_max_session_usage`** (#443,
> the unqualified `usage` hid the session axis); the examples are preserved as the historical
> behavior they describe. Both `session_floor` and `target_max_usage` still parse as
> deprecated aliases.

## Context

`config.toml` is the daemon's source of truth (the captured roster plus the poll/swap
tunables; issue #4). Its read and write paths are deliberately **asymmetric**:

- **Read — the `toml` crate.** `Config::parse` runs `toml::from_str` into a permissive
  `RawConfig` (`#[serde(deny_unknown_fields)]`), then validates into the typed
  `Config`. A user may hand-edit the file, so the reader must accept **any** valid TOML
  — exactly what a full parser is for.
- **Write — by hand.** `Config::render` builds the file with `push_str`; roster string
  fields go through a hand-rolled TOML basic-string escaper, `basic_string`. `render`
  is the production write path (`Config::save`) and also feeds the `export` migration
  artifact (issue #148).

The write path is hand-written because the emitted file is a **curated,
self-documenting artifact** (issue #3 N2), not a plain dump — and no TOML serializer
reproduces its shape:

1. **Interleaved doc-comments.** A tailored `#` comment precedes nearly every key and
   block (`poll_secs`, the `[jitter]` / `[refresh]` / `[login]` / `[stats]` /
   `[migration]` tables, …). The rendered file *is* the tunables' primary documentation.
2. **Conditional OFF-state rendering.** Opt-in tunables render as a *commented-out
   example line* when disabled, not an absent key — e.g. `session_floor` at `None`
   emits `# session_floor = <session_trigger>`, embedding another field's value as the
   suggested default; `claude_bin` at `None` emits a commented example path. The comment
   *is* the OFF representation.
3. **Selective omission.** The derived `stash` field is never emitted (issue #70).

`basic_string` is correct — a complete TOML 1.0 basic-string escaper (quote, backslash,
the `\b\t\n\f\r` short forms, C0 controls + DEL via `\uXXXX`, non-ASCII literal) — and
is pinned by `basic_string_escapes_specials` and exercised by the render→parse
round-trip tests, which feed the hand-written output back through the real
`toml::from_str` on every change.

**Dependency posture (load-bearing here).** The crate keeps a documented minimal-dep
stance (see ADR-0004, ADR-0002, and the `Cargo.toml` rationale). A new direct
dependency must clear that bar. `toml_edit` is **not** in the tree; `toml` builds on
`toml_parser` / `toml_writer` / `winnow`. Adopting the format-preserving editor would
add a genuinely new direct crate.

## Decision

**Keep the write path hand-written, by design.** The asymmetry is intentional: consume
with a full parser (accept any valid TOML a human edits), emit a single controlled,
documented artifact no serializer can produce. Do **not** adopt `toml_edit`, and do not
route the emitter through `toml::Serialize`.

Satisfy issue #181's acceptance criterion by naming the asymmetry as a choice at both
the emitter and the escaper, so a future contributor does not read it as an oversight.

## Alternatives considered

Framed as the issue's options (a) / (b) / (c).

1. **(a) Keep hand-written + name the intent — the decision (above).**
   - **Pros**: zero behaviour change; keeps the self-documenting config and its
     OFF-state semantics; no new dependency; leaves the correct, round-trip-tested
     emitter and escaper (and the `export` path) untouched. Cost is two doc-comments.
   - **Cons**: `basic_string` stays hand-maintained against the TOML basic-string
     grammar — bounded (TOML 1.0 is stable; the test pins it).
2. **(b) Emit via `toml::Serialize`, accept loss of the interleaved comments.**
   - **Cons**: `toml::Serialize` **cannot emit comments** — this deletes the entire
     self-documenting config (issue #3 N2) and the commented-out OFF representations.
     And `Config` holds validated/derived forms, so a faithful round-trip needs a
     separate serializable shadow type. A large user-facing regression plus new code to
     tidy an acknowledged non-bug.
   - **Why rejected**: trades away a real feature for a "consistency" that costs more
     than it saves.
3. **(c) Emit via `toml_edit`, preserving the comments as node decor.**
   - **Cons**: adds a **new direct dependency** absent from the tree, against the
     documented posture. It does **not** author the comments — every `#` line stays a
     hand-written string, now set as node decor; the commented-out OFF toggles are not
     DOM nodes, so they must still be string-injected. Net: strictly more machinery and
     a new dependency for no behavioural gain.
   - **Why rejected**: does not remove the hand-authoring that is the actual reason the
     path exists.

## Consequences

### Positive

- The rendered `config.toml` stays a **self-documenting artifact** — curated per-key
  guidance plus the commented-out opt-in lines that make `session_floor` / `[refresh]`
  / `claude_bin` discoverable while OFF.
- **Minimal-dependency posture preserved** (no `toml_edit`), consistent with ADR-0004
  and ADR-0002.
- The correct, round-trip-tested emitter/escaper and the `export` artifact are
  untouched — **zero blast radius**.

### Negative / trade-offs

- ~~**`basic_string` is maintained by hand** against the TOML basic-string escape rules.
  Bounded: TOML 1.0 is stable and `basic_string_escapes_specials` pins the behaviour; a
  spec change would be a visible, test-caught edit.~~ **Retired** by issue #403 — the
  escaping is delegated to `toml_writer`; see the Update note below.
- **Two write paths coexist by design** (`toml` in, hand-rolled out). Mitigated by the
  render→parse round-trip tests, which assert the hand-written output is re-readable by
  the `toml` parser on every change.
- The parse/emit split must be **re-affirmed, not drifted**: a future key added to
  `render` without its documenting comment quietly erodes the artifact's value. The
  round-trip tests catch *correctness*, not *documentation completeness* — reviewers
  own that.

### A considered, optional refinement (not adopted here)

The one honestly-reinventable sub-component is the *escaper*, and its canonical
value-escaper — `toml_writer` — is **already present transitively** (a child of
`toml`). Delegating **only** `basic_string`'s escaping to `toml_writer` (keeping every
comment and the layout as `push_str`) would satisfy "don't reinvent the wheel" for the
one injection-sensitive primitive at **zero new-crate cost**. It is left as an optional
future refinement, not part of this decision: the current escaper is verified correct
and test-pinned, and promoting a transitive dep to a direct one plus re-baselining the
exact-bytes round-trip tests is churn without a correctness gain today.

> **Update (2026-07-10, issue #403).** This refinement **was subsequently adopted**.
> `basic_string` now delegates to `toml_writer::TomlStringBuilder::…as_basic()`;
> `toml_writer` is named directly in `Cargo.toml` and `Cargo.lock` gained a single
> dependency *edge* — no new `[[package]]`, so the zero-new-crate cost held exactly.
> The anticipated re-baselining did not materialize: the escape surface was pinned
> against the hand-rolled implementation first, then the identical tests passed unchanged
> against the delegated one — the two emitters are byte-for-byte equivalent.
>
> **The decision recorded above is unchanged**: the *emitter* stays hand-written
> (`push_str`, interleaved comments, selective omission); only the *escaper* moved. This
> ADR keeps status `Accepted`, and is not superseded — a trade-off it listed (`basic_string`
> is maintained by hand) is simply retired. Two illustrations in the Context above are now
> historical: `session_floor` no longer renders as a commented-out OFF-state line (it is a
> default-on live value since issue #398 — see **ADR-0013**), and `[refresh]`'s `enabled`
> has always rendered live. `claude_bin`'s commented example still stands.

## Related

- Issue #181 (this decision); issue #3 N2 (self-documenting config); issue #148
  (`export` reuses `render`); issue #70 (derived `stash`, never persisted).
- Issue #403 (the optional escaper refinement above, subsequently **adopted** — see the
  Update note); issue #398 / ADR-0013 (`session_floor` is default-on, so the
  commented-out OFF-state illustration in the Context is historical).
- ADR-0004 / ADR-0002: the same minimal-dependency posture that weighs against adopting
  `toml_edit`.
- Code: `src/config/load.rs` — `Config::parse`; `src/config.rs` — `RawConfig`;
  `src/config/render.rs` — `Config::render`,
  `basic_string` (now a `toml_writer` delegation) and its `basic_string_escapes_specials`
  + `rendered_strings_round_trip_through_the_parser` tests; `Cargo.toml` dependency
  rationale; `Cargo.lock` (`toml`, `toml_writer`, no `toml_edit`).
