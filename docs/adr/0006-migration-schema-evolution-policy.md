---
type: architecture-decision-record
number: 6
title: "Migration-artifact schema-evolution policy; v1 format frozen as the tested baseline"
date: 2026-07-03
status: accepted
decision_makers: [Oleksii PELYKH (maintainer)]
---

# ADR-0006: Migration-artifact schema-evolution policy; v1 format frozen

## Status

**Accepted** — 2026-07-03. Records the additive-vs-breaking evolution policy for the
migration artifact, **freezes the current v1 format as the tested baseline**, and adds
a byte-frozen cross-version round-trip test to enforce it (issue #198).

Unlike ADR-0002/0003/0004/0005 (which record invariants or choices already in force),
this ADR also lands a **new test** — the golden-fixture gate in `src/migration.rs` — and
takes a decision (freeze v1) that is *free now and expensive after release*. It was
ratified after a maintainer-convened **council** (`security-architect`,
`rust-architect`, `technical-architect`) returned a convergent 3/3 **FREEZE-NOW**
verdict at high confidence; the refinements the panel raised are folded in below (the
promoted BREAKING rules, the recorded "no guard field" decision, the whole-algorithm
agility finding, and key-commitment as the designated first future hardening).

**Grounding.** #146 (format), #147 (envelope), #148 (export), #149 (import), #150
(config/events) are all **closed** — the format and the `export`/`import` verbs are
code-complete in `src/cli.rs`. But there are **no git tags / no releases**, so `export`
has not shipped and **no real field artifact exists yet**. Breaking changes to v1 were
therefore still free at the time of this decision; freezing now pins v1 before that
window closes.

## Context

The encrypted artifact wraps its body in an Argon2id + XChaCha20-Poly1305 envelope and
**binds the whole header as the AEAD associated data (AAD)** so tamper/downgrade fails
closed (`src/migration.rs`, `Header::associated_data`, ~L555):

```rust
// src/migration.rs
fn associated_data(&self) -> Vec<u8> {
    serde_json::to_vec(self).expect("serializing a header cannot fail")
}
```

Used identically on encrypt (`let aad = header.associated_data();`, ~L633) and decrypt
(`let aad = self.header.associated_data();`, ~L683). The AAD is the **entire serialized
`Header`**. Its determinism doc (~L550) states what the binding rests on: fixed field
order, no maps or floats, both parameter blocks always present — so equal headers yield
identical bytes on both sides.

This is a sharp, non-obvious failure surface. A header change that *looks* additive can
**silently break decryption of every already-written artifact**, because it changes the
bytes the AAD is computed over. The existing suite cannot catch this: every crypto
round-trip encrypts and decrypts with the **same struct definition in the same binary**,
so the AAD is self-consistent by construction, and
`the_associated_data_binds_every_header_field` (~L1284) builds `Header` fresh on both
sides. None freeze bytes across a *definition* change — which is exactly the regression
that matters. Before real artifacts exist, we need (1) a written rule for which changes
are additive vs breaking, derived from how this code builds the AAD, and (2) a test that
enforces it against frozen bytes.

## Decision

### The three evolution surfaces

| Surface | What it is | Governed by |
|---|---|---|
| **Preamble** | `magic` + `header.format_version` | Exact-match version gate (~L364); peeked before body interpretation |
| **Header** | the whole `Header` struct | **AAD byte-stability** — the sharp one |
| **Payload / Body** | `Payload`, `Body` (`tag="encoding"`) | serde **deserialize** tolerance (no `deny_unknown_fields`) |

### The load-bearing rule (header surface)

Encrypt-time AAD equals the on-disk header bytes; decrypt-time AAD equals
`serde_json::to_vec(&parse(on_disk_header))`. So for every already-written artifact,
decryption survives a header change **iff**:

> **AAD invariant** — parsing an artifact's on-disk header with the *new* `Header`
> definition and re-serializing it reproduces its **byte-identical** original bytes.

A header change is **ADDITIVE** iff it preserves this invariant for all previously
written artifacts; otherwise it is **BREAKING** and MUST bump `format_version`. The
version gate is **exact-match** (`if version != FORMAT_VERSION`, ~L364) — not `>` — so a
bump makes new binaries reject old artifacts and vice versa, up front, with the typed
`Error::MigrationUnsupportedVersion`. Each `format_version` is an **island**.

### ADDITIVE — permitted with NO `format_version` bump

1. **Change a KDF/cipher parameter VALUE, or introduce a whole new KDF/cipher
   ALGORITHM.** A new Argon2 cost, a different validated-length salt/nonce, a new
   `algorithm` string. Safe by construction: both the KDF and the cipher **dispatch on
   an in-header algorithm string and reject anything unknown *before* key derivation**
   (~L672 cipher, ~L711 KDF; cost read from the header at ~L737), and the value travels
   in the header and reads back byte-identically. An old binary rejects an unknown
   algorithm fail-closed; a new binary reads both. This means even a future **new AEAD
   (e.g. a key-committing cipher)** lands as a new algorithm identifier with **no v1
   wire change** — this is the blessed path, and it is wider than "just parameter
   values." Covered by `an_unsupported_kdf_or_cipher_algorithm_is_rejected_before_derivation`
   (~L1451) and `encrypt_with_cost_records_the_supplied_cost_and_round_trips` (~L1589).
2. **Add a header field ONLY as `Option<T>` with
   `#[serde(default, skip_serializing_if = "Option::is_none")]`.** For every existing
   artifact the field is absent → parses to `None` → re-serializes skipped →
   byte-identical AAD → still decrypts. This is the **only** additive *struct* change to
   the header — and only *backward*-safe. **Populating it is a de-facto version bump: see
   BREAKING (2).**

### BREAKING — MUST bump `format_version`

Anything that alters an existing artifact's re-serialized header bytes or its
parseability:

1. Add a header field that serializes when "empty" — `bool`, integer, `String`→`""`,
   `Vec`→`[]`, or an `Option` **without** `skip_serializing_if`; **reorder**, **rename**
   (or add `rename`/`rename_all`), **retype** (int↔string, the `hex_bytes` encoding, an
   enum tag), or **remove** an existing header field; or change `default` /
   `skip_serializing_if` on an existing one.
2. **Populate a previously-absent AAD-bound header field** — even a correctly-additive
   `Option<T>` one. The moment it carries a value, an older binary parses the artifact
   (version matches, unknown field ignored) but reconstructs the AAD **without** the
   field, computes a different AAD, and **fails to decrypt with the correct passphrase**
   (indistinguishable from corruption). Populating an AAD-bound field is a **de-facto
   `format_version` bump** for forward-compat — treat it as BREAKING. *(This is the
   forward/backward asymmetry, promoted from prose into an enumerated rule so a
   contributor reading only this table cannot miss it.)*
3. **Add a load-bearing / secret-bearing PAYLOAD field.** Because unknown payload fields
   are *ignored* (no `deny_unknown_fields`), an older reader importing a newer artifact
   silently **drops** any field it doesn't know. Silently dropping a new secret-bearing
   field on import is a correctness/security hazard — so adding one MUST bump
   `format_version` (or gate import on a minimum reader version). *(Ordinary, non-load-bearing
   additive payload growth via `Option`/`#[serde(default)]` stays additive.)*
4. Change `MAGIC`, or move the preamble — **nuclear**: old files are unrecognized.
   `MAGIC` is frozen forever.
5. Change `Body`'s `tag` / `content` / `rename_all` — old files fail to parse. (Body is
   not AAD, but it is still a structural break.)

### Explicitly NOT adopted for v1: a forward-compat guard field

We considered adding a header field such as `min_reader_version` to let a writer signal
"an older reader must refuse this artifact rather than silently mis-handle it." **We do
not add it.** The exact-match gate plus BREAKING (2)/(3) already subsume every case: any
change that would matter to an older reader forces a `format_version` bump, and the
exact-match island then makes the older reader refuse the artifact with a typed error.
This is a **recorded "no", not a default-by-omission** — because adding *any* header
field is a breaking change after the freeze, this was the maintainer's last free moment
to decide it, and the council was unanimous it is unnecessary.

## Enforcement: the cross-version round-trip test

**What it freezes.** Two **byte-frozen, committed** fixtures, generated once from the
current tree and thereafter **only read** (`build/fixtures/`, sibling to the
`build/version-compat.md` ledger):

1. `migration-v1-plaintext.json` — a plaintext artifact (`encrypted:false`).
2. `migration-v1-encrypted.json` — an encrypted artifact at **trivial** Argon2 cost with
   a **committed, obviously-synthetic** test passphrase, plus the expected `Payload`.

Freezing at trivial cost keeps CI fast while exercising the **identical** AAD/serde path
— the AAD trap is cost-independent (it is about header *byte-stability*), and the cost
travels in the fixture header so decrypt honours it automatically.

**What it asserts** (inline in the `#[cfg(test)] mod tests` in `src/migration.rs`):
- **Plaintext**: `from_bytes(FIXTURE)?.into_plaintext_payload()?` equals the expected
  synthetic payload — catches magic / version-gate / `Body` / `Payload` parse breaks.
- **Encrypted**: `from_bytes(FIXTURE)?.decrypt(passphrase)?` equals the expected payload
  — catches the **silent header/AAD break** *and* the payload-deserialize break.
- **Version pin**: each fixture's on-disk `format_version` equals
  `EXPECTED_FIXTURE_VERSION`. Bumping `FORMAT_VERSION` then forces a deliberate fixture
  refresh in the same change — you cannot bump the constant and leave a stale fixture.

**Why byte-frozen is the whole point.** The invariant an additive change must preserve
is "parse+reserialize under a possibly-changed struct reproduces the original bytes."
Only an artifact created **once** and thereafter **only read** exercises it. Fixture
regeneration is a documented, manual step (a `#[test] #[ignore]` emitter) run **only**
alongside a deliberate `FORMAT_VERSION` bump — the encrypt path draws a fresh random
salt/nonce each call, so re-emitting produces *different* bytes; regenerating on any
other occasion masks exactly the regression the fixture exists to catch.

**Fixture hygiene.** Only **synthetic** sample material (never a real token) and an
obviously-a-test committed passphrase.

## Alternatives considered

1. **Per-version header types + a version-dispatch reader**
   (`match version { 1 => Header1, 2 => Header2, _ => reject }`) instead of the single
   exact-match gate.
   - **Pros**: a new binary could **read** old-version artifacts (true cross-version
     backward read); breaking header changes stop being "nuclear."
   - **Cons**: every version needs a frozen header type and its own AAD reconstruction;
     more code and fixtures; still does **not** fix forward compat (old binary, new file).
   - **Why not now**: sessiometer moves an install between machines you control, at
     matched or forward versions; the exact-match island is simpler and fail-closed.
     **Freezing v1 does not foreclose this** — a future dispatched reader would pin
     `Header1` = today's exact shape, and the golden fixture *is* that anchor. Building
     dispatch before a second version exists is backwards. This is the escape hatch to
     revisit **if** real cross-version import becomes a requirement.
2. **An AAD-excluded header metadata region** for non-security extensibility.
   - **Cons**: anything outside the AAD is **unauthenticated** — tamperable/removable
     without detection; splitting the header into signed/unsigned halves is a downgrade
     surface.
   - **Why rejected**: whole-header-as-AAD is the safer default; add an unsigned region
     only if a concrete need justifies the tamper surface.
3. **Rely on the existing tests** (same-binary round-trip +
   `the_associated_data_binds_every_header_field`).
   - **Why rejected**: both exercise **one** struct definition on both sides; neither
     freezes bytes across a definition change. They prove the AAD binds fields *now*;
     they cannot prove a *future* definition still reads *today's* bytes. Only a
     byte-frozen fixture witnesses that.
4. **AAD over the raw stored header bytes** (authenticate the exact on-disk header
   byte-string rather than re-serializing the parsed struct).
   - **Pros**: removes the "re-serialization must be byte-identical" fragility.
   - **Cons**: a larger change to the on-disk format and the encrypt/decrypt seam (#147,
     already shipped); is itself a breaking format change needing its own bump; closes no
     attacker-facing gap (the current design already fails closed on every tamper and
     downgrade).
   - **Why deferred**: worth considering as a future v2 hardening, but making a breaking
     change *now* to avoid a hypothetical future one — when the additive-only policy plus
     the byte-frozen fixture already guard the fragility — is not warranted.

## Consequences

### Positive

- A written, **code-derived** rule the `export`/`import` path relies on before it writes
  real artifacts — same "records a decision in force" spirit as ADR-0002/0003/0004/0005.
- A single **loud CI gate** converts the format's sharpest failure mode — a silent, late
  AAD break no same-binary test catches — into an early, deterministic failure.
- Freezing does **not** lock out future evolution: new crypto (including a key-committing
  AEAD) is additive via the algorithm-string dispatch; the `config.toml` payload is
  carried verbatim as text so the config schema evolves independently of the format; and
  the per-version-dispatch escape hatch (Alternative 1) stays open with the fixture as
  its anchor.

### Negative / trade-offs

- **The forward/backward asymmetry is the highest residual risk.** A correctly-classified
  *additive* header field, once **populated**, silently breaks decryption on **older**
  binaries, and it is **not catchable by any in-repo test** (CI has no old binary).
  BREAKING (2) is the sole discipline standing between the sound design and this hazard —
  keep it prominent; reviewers own it.
- The enforcement test can be **defeated by regenerating the fixture**; its teeth depend
  on the version-pin guard + review discipline (regeneration allowed **only** alongside a
  deliberate `FORMAT_VERSION` bump). It is a *process* gate, not a self-enforcing one.
- The exact-match gate gives **no cross-version import**: a newer binary refuses an older
  artifact. Acceptable for a matched-version move-between-machines tool; Alternative 1 is
  the escape hatch if that changes.
- A committed (trivially-encrypted) credential-**shaped** fixture lives in the repo —
  contained by the synthetic-material + test-passphrase hygiene rule.

### Designated first future hardening (not adopted here)

XChaCha20-Poly1305 is **not** a key-committing AEAD, and password-based encryption is the
textbook partitioning-oracle setting. For a single-user artifact decrypted by its own
owner (a human-in-the-loop `import`, not an automated oracle) at 64 MiB / 3-pass Argon2
cost, the practical risk is marginal — so this is **not** a freeze blocker. It is recorded
as the **designated first additive hardening**: a committing variant is added later as a
**new cipher-algorithm identifier** riding the string-dispatch above, with **no change to
frozen v1's wire shape** — old artifacts stay readable, new ones opt in.

## Related

- Issues: #198 (this policy), #147 (envelope — the AAD binding), #146 (format), #148 /
  #149 / #150 (export/import wiring), #15 (redaction hygiene).
- Code: `src/migration.rs` — `Header`, `Header::associated_data` (~L555), version gate
  (~L364), encrypt/decrypt AAD (~L633, ~L683), algorithm-string dispatch (~L672, ~L711),
  cost from header (~L737), `Body` tag, the golden-fixture tests and their `#[ignore]`
  emitter; `build/fixtures/` (the frozen v1 artifacts); `build/version-compat.md`
  (empirical ledger).
- ADR-0002 / ADR-0003 / ADR-0004 / ADR-0005 (house style; minimal-dependency posture).
