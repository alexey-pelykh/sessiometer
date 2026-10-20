---
type: architecture-decision-record
number: 38
title: "Every canonical write settles Claude Code's plaintext fallback; the default deletes it"
date: 2026-10-20
status: accepted
decision_makers: [Stefano Benatti (contributor, #1540), Oleksii PELYKH (maintainer)]
---

# ADR-0038: Every canonical write settles Claude Code's plaintext fallback — the default deletes it

## Status

**Accepted** — 2026-10-20, on **#1540**. Proposed with the change that implements it, so it is in
force from that merge. The mechanism it answers is recorded in `build/version-compat.md`
§ Issue #1540.

## Context

On macOS, Claude Code stores its login in the keychain and keeps a plaintext copy,
`<config dir>/.credentials.json`, only as a **fallback** (`build/version-compat.md` § AC-6): it
writes the file when a keychain save fails, and deletes it only when a later save lands over an
*empty* keychain. sessiometer writes the keychain item and nothing else.

On CC `2.1.285` the check a live session runs before each request is **file-first**: while the
file exists, the session compares only the file's mtime and never looks at the keychain. So one
transient keychain failure leaves behind a file that:

1. **hides every swap** from every running session, which keeps billing the outgoing account until
   its in-memory token fails — the failure the field report on #1540 measured for ~6 hours;
2. **brings back a stale login** when the keychain item is scrubbed (ADR-0018), since reads fall
   back to the file;
3. **is never cleaned up**, because sessiometer's own writes keep the keychain non-empty, so CC's
   delete-on-empty arm never fires.

Removing the file by hand fixed (1) for the same running processes, with no restart.

## Decision

1. **Settle after every successful canonical write, and only then.** A decorator,
   `FallbackSettlingStore` in `src/plaintext_fallback.rs`, wraps the store at the three production
   construction sites whose stores write the canonical item: the daemon (its swaps and the
   keep-warm promotion), `use` (swap and adopt), and an activating `login`. A source-inspection
   test pins all three. A failed write settles nothing: the keychain was not refreshed, so the file
   may be the only good copy.
2. **The behaviour is a config key, `[credential].plaintext_fallback`**, rendered like every other
   key (ADR-0005): `remove` deletes the file, `touch` bumps its mtime so live sessions re-read
   storage (keychain first) once, `off` leaves it.
3. **The default is `remove`.** sessiometer has just written the item the file shadows; the file's remaining effects are (1)–(3) above, and none of them is wanted.
   CC recreates it if a later keychain save fails again, and the next write removes it again.
4. **macOS only.** On Linux the file *is* CC's credential store (#26), so the wrapper resolves no
   path there and is a pure pass-through. Nothing is touched unless the path is absolute and ASCII,
   mirroring the keychain service-name derivation the file pairs with.
5. **Never follow, never fail.** Only a regular file is acted on (`symlink_metadata`; a symlink or
   directory is `refused`). Every outcome but "absent" is one `event=plaintext_fallback action=…`
   line, and no outcome — including an I/O error — fails the write that triggered it: the swap has
   already landed, and settling is a repair on top of it.

## Alternatives considered

- **`touch` as the default.** It fixes (1) with nothing deleted, but leaves (2) and (3) — the file
  stays, stale, ready to be read the next time the keychain is empty. Kept as an option for an
  operator who would rather never delete a file CC wrote.
- **Rename the file aside instead of deleting it.** It keeps a plaintext copy of a credential on
  disk indefinitely, which is the thing worth removing; and a growing set of `.disabled-*` files is
  its own cleanup problem. The contents are never needed: the keychain holds the current login.
- **Rewrite the file with the swapped-in credential.** It would fix (1) and (2), but makes
  sessiometer a second writer of a plaintext secret that CC only produces on a failure path, and
  the file would then shadow the keychain for good.
- **Warn on `status` / the menubar, act on nothing.** It surfaces the problem without fixing it, and
  touches the status wire and the Swift fixtures. A natural follow-up; not a substitute.
- **Settle on a timer instead of after writes.** Every observed harm needs a write to matter (a swap
  to hide, a scrub to resurrect over), so after-write is enough, and it costs nothing when the file
  is absent.

## Consequences

### Positive

- Live sessions follow swaps again without a restart, whatever left the file behind.
- A scrub can no longer resurrect an expired plaintext login.
- Each settlement is visible in the event log, so a CC change that started recreating the file on
  *every* save would show up as one `removed` line per swap.

### Negative / trade-offs

- sessiometer now deletes a file Claude Code owns. The deletion is limited to one path, derived
  exactly as CC derives it, and only after sessiometer's own write to the item that path shadows.
- The daemon reads the mode once at start-up, like the tunables; changing it takes a restart.
- Adding a rendered key moves the import version floor (`CONFIG_BLOCK_VERSION_FLOOR`,
  ADR-0006's #1053 amendment): an older build refuses an artifact a build with this key exports.
  The floor constant names a commit, so it moves in a follow-up once this change has a `main` SHA.
- The mechanism was decoded from one CC version. If a later release stops being file-first, the
  settling becomes unnecessary but stays harmless; `build/version-compat.md` § Issue #1540 lists
  what would falsify it.
