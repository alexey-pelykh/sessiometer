---
type: architecture-decision-record
number: 39
title: "Every canonical write settles Claude Code's plaintext fallback; the default re-stamps it"
date: 2026-10-20
status: accepted
decision_makers: [Stefano Benatti (contributor, #1540), Oleksii PELYKH (maintainer)]
---

# ADR-0039: Every canonical write settles Claude Code's plaintext fallback — the default re-stamps it

## Status

**Accepted** — 2026-10-20, on **#1540**. Proposed with the change that implements it, so it is in
force from that merge. The default changed from `remove` to `touch` in review of #1541 on
2026-10-30, before that merge, for the window recorded in § Context. The mechanism it answers is
recorded in `build/version-compat.md` § Issue #1540.

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

**The file is not always stale, and sessiometer cannot tell when it is.** Claude Code writes it
when a keychain save *fails*, so at that moment the file holds a credential **newer** than the
keychain item; and the endpoint rotates the refresh token on every exchange (ADR-0007), so the
keychain's older copy may already be dead. The account Claude Code was refreshing is the active
one — the account sessiometer's next swap moves *away from*. `swap.rs` re-stashes that account
from a keychain read and never sees the file, so for that window the file can be the only working
copy of the outgoing account's login, and deleting it costs that account a fresh `login`. Telling
the two cases apart would mean reading the file's contents, which sessiometer never does, or
comparing its mtime with the item's modification date, a keychain read this path does not
otherwise make.

## Decision

1. **Settle after every successful canonical write, and only then.** A decorator,
   `FallbackSettlingStore` in `src/plaintext_fallback.rs`, wraps the store at the three production
   construction sites whose stores write the canonical item: the daemon (its swaps and the
   keep-warm promotion), `use` (swap and adopt), and an activating `login`. A source-inspection
   test pins all three, and a second sweeps every other construction site in the crate as
   read-only. A failed write settles nothing: the keychain was not refreshed, so the file may be
   the only good copy.
2. **The behaviour is a config key, `[credential].plaintext_fallback`**, rendered like every other
   key (ADR-0005): `touch` bumps the file's mtime so live sessions re-read storage (keychain first)
   once, `remove` deletes the file, `off` leaves it.
3. **The default is `touch`.** It fixes (1), the harm that was measured, and deletes nothing, so
   the window in § Context costs nothing: a file that was the only good copy of a login is still
   there. It leaves (2) and (3) standing — the file stays, stale or not, and is read again the
   next time the keychain is empty. `remove` is the opt-in for an operator who would rather the
   keychain be the only copy and accepts that window.
4. **macOS only.** On Linux the file *is* CC's credential store (#26), so the wrapper resolves no
   path there and is a pure pass-through. Nothing is touched unless the path is absolute and ASCII,
   mirroring the keychain service-name derivation the file pairs with.
5. **Never follow, never fail.** Only a regular file is acted on (`symlink_metadata`; a symlink or
   directory is `refused`), and the re-stamp opens the path without following a link, so one
   swapped in after the stat is refused by the kernel rather than re-stamped through. There is no
   owner check: re-stamping a file the user does not own already fails at the kernel (`futimens`
   needs ownership), and unlinking one in the user's own config directory is what the operator
   could do by hand. Every outcome but "absent" is one `event=plaintext_fallback action=…` line,
   and no outcome —
   including an I/O error — fails the write that triggered it: the swap has already landed, and
   settling is a repair on top of it.

## Alternatives considered

- **`remove` as the default** — the first draft of this record. It also fixes (2) and (3), which
  `touch` leaves standing, and keeps a plaintext copy of the secret off disk. Rejected as the
  default for the window in § Context: sessiometer cannot tell a stale leftover from the only
  working copy of the outgoing account's rotated login, and deleting the latter costs the operator
  a `login` — the one failure a swap tool should never cause. Kept as the opt-in mode.
- **Remove only a file provably older than the item it shadows.** `security find-generic-password`
  prints the item's modification date without `-w`, so the comparison would read no secret. Not
  taken: it adds a keychain read to every write to make safe a mode that is no longer the default.
  It is the natural route if `remove` is ever wanted as the default again.
- **Rename the file aside instead of deleting it.** It keeps a plaintext copy of a credential on
  disk indefinitely, which is the thing `remove` exists to avoid; and a growing set of
  `.disabled-*` files is its own cleanup problem.
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

- Live sessions follow swaps again without a restart, whatever left the file behind — and under
  the default, with nothing deleted to get there.
- Each settlement is visible in the event log, so a CC change that started recreating the file on
  *every* save would show up as one `touched` line per swap.
- Under `remove`, a scrub can no longer resurrect an expired plaintext login.

### Negative / trade-offs

- Under the default the stale file stays: harms (2) and (3) in § Context are made visible (one
  `touched` line per write), not fixed. An operator who wants them fixed opts into `remove`, and
  with it the window in § Context.
- Under `remove`, sessiometer deletes a file Claude Code owns. The deletion is limited to one path,
  derived exactly as CC derives it, and only after sessiometer's own write to the item that path
  shadows.
- The daemon reads the mode once at start-up, like the tunables; changing it takes a restart.
- Adding a rendered key moves the import version floor (`CONFIG_BLOCK_VERSION_FLOOR`,
  ADR-0006's #1053 amendment): an older build refuses an artifact a build with this key exports.
  The floor constant names a commit, so it moves in issue #1589 once this change has a `main`
  SHA; until then `a_build_at_the_current_floor_commit_still_rejects_a_current_render` in
  `src/config/load.rs` witnesses the gap.
- The mechanism was decoded from one CC version. If a later release stops being file-first, the
  settling becomes unnecessary but stays harmless; `build/version-compat.md` § Issue #1540 lists
  what would falsify it.
