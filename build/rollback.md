# Rollback / downgrade procedure

How to undo `sessiometer`'s changes to each piece of state it touches, and what to do if an
older binary meets a migration artifact a newer one wrote.

`sessiometer` rewrites state a live Claude Code session depends on — the canonical
`Claude Code-credentials` keychain item and `~/.claude.json` — and its `export` / `import`
verbs read and write a versioned portable **migration artifact**. This document is the
per-artifact recovery path, so a downgrade or an abandoned rollout is a **documented
procedure, not source-reading**. It is a pre-first-release readiness gate (issue
[#191](https://github.com/alexey-pelykh/sessiometer/issues/191)), kept current by the
[release checklist](release-checklist.md).

## The one thing to know first: live state is not versioned

`sessiometer` touches two *kinds* of state, and only one of them carries a version:

- **Live state** — the canonical `Claude Code-credentials` keychain item and the
  `oauthAccount` block in `~/.claude.json`. These are plain, opaque values that **any**
  version of `sessiometer` *and* Claude Code itself reads and writes; they carry **no
  `sessiometer` version stamp**. Downgrading — or removing — the binary can never make live
  state unreadable: whatever account was active when you stopped stays active, and the
  Claude Code session keeps working.
- **The portable migration artifact** (`.smmig`, written by `export`, read by `import`) —
  the *only* artifact with a `format_version` (`src/migration.rs`). It is a **transport
  file**, never live state, and the only place a version mismatch can bite.

So a binary downgrade needs **no rollback of live state**. The sections below are the
per-artifact detail (§1–§2) plus the one real version-mismatch case (§3).

## 1. Canonical `Claude Code-credentials` keychain item

**What `sessiometer` did.** Each swap rewrote this item in place (via `/usr/bin/security`)
to point at the next account's credential. It also stashed a per-account copy of every
captured account under its own keychain service, `Sessiometer/<account_uuid>`
(`src/stash.rs`) — items **only `sessiometer` reads**, never Claude Code.

**To roll back:**

- **Just stop.** The canonical item holds a *valid* credential for whichever account was
  active at the last swap; the running Claude Code session keeps working as that account. No
  action required.
- **Reset to a clean single-account state.** Run `claude /login`. This is Claude Code's own
  path and fully overwrites the canonical item with a fresh token — `sessiometer` is not
  involved. (If the daemon is still running it picks this re-login up automatically; see the
  README "Edge cases & resilience".)
- **Erase `sessiometer`'s per-account stashes.** Before uninstalling, run
  `sessiometer remove <account>` for each — it drops the roster entry **and deletes the
  account's keychain stash** (README "Removing an account"). If the binary is already gone,
  delete the leftover items directly: they live under the keychain service
  `Sessiometer/<account_uuid>` — remove them in **Keychain Access.app**, or:

  ```sh
  # Two items live under each service (acct = credential, acct = oauthAccount);
  # run once per stored item.
  security delete-generic-password -s "Sessiometer/<account_uuid>"
  ```

  Leftover stash items are inert (Claude Code never reads them); erasing them is hygiene, not
  a functional requirement.

## 2. `~/.claude.json` (the `oauthAccount` block)

**What `sessiometer` did.** A swap co-writes **only** the `oauthAccount` identity block into
`~/.claude.json`; the rest of that (Claude Code-owned) file is untouched
(`src/claude_state.rs`, `src/paths.rs`).

**To roll back.** `claude /login` rewrites `oauthAccount` to match the re-authenticated
account — the same single step as §1. There is nothing else to undo: `sessiometer` only ever
writes a *valid* `oauthAccount` block (the one matching the swapped-in credential), so even
leaving it as-is is consistent — it names whatever account is currently active.

## 3. The migration artifact `format_version` contract

This is the only place a version mismatch can occur, and it is **fail-closed**.

**The contract.** The migration artifact carries a `format_version` (currently **1**;
`src/migration.rs`). `import` peeks it **before** touching any state and rejects a version
this build does not understand — so a mismatch changes **nothing** on disk or in the
keychain.

**What you see when an older binary meets a newer artifact.** Say a future `0.2.0` writes
`format_version = 2`, and you then run an older `0.1.0` `import` on that file. It exits with:

```
sessiometer: unsupported migration format version 2 (this build supports 1)
```

(`Error::MigrationUnsupportedVersion`, `src/error.rs`; the `sessiometer:` prefix is `main.rs`'s
error surface). No credential, roster, or config was read or written — the older binary refused
the file whole.

**What to do about it** — pick whichever fits:

1. **Import with a binary of the artifact's own `format_version`.** The reader gate is
   **exact-match**: `import` rejects any `format_version` that is not this build's own (`!=`,
   `src/migration.rs`) — so a *newer* build rejects an older file just as an older build rejects
   a newer one. An artifact therefore imports only into the release whose `FORMAT_VERSION` equals
   the file's. Keep (or re-download) that release and run the `import` there.
2. **Re-export from live state at the version you need.** If the source machine still has its
   state, run `export` there with a binary whose `format_version` matches the **importing**
   binary, producing an artifact that binary accepts. (There is deliberately **no** "convert an
   existing artifact" path — the body is opaque / encrypted; re-exporting from live state is the
   supported route.)
3. **Skip the artifact entirely.** It is only a transport convenience. Live state is
   version-independent: on the target machine, `claude /login` each account and
   `sessiometer capture` it — no `.smmig` file needed. This always works, whatever binary
   versions are in play.

**The rule that avoids this.** Match the binary to the artifact: **`export` and `import` with
builds of the same `format_version`.** If the two ends are on different releases, align them
first, re-export from live state at the importing binary's version (option 2), or bypass the
artifact and re-authenticate from live state (option 3). *(A future release could widen its
reader to accept a range of older versions — the parameters-in-file design leaves room for it —
but that is a per-release property, not a guarantee of the format; do not assume it.)*

## Removing `sessiometer` entirely (clean revert)

To return a machine to plain single-account Claude Code:

1. Stop any running `sessiometer` daemon.
2. `sessiometer remove <account>` for each managed account (erases each keychain stash; §1).
   *Optional* — leftover stashes are inert.
3. `claude /login` the account you want to keep using — this overwrites the canonical
   keychain item and the `~/.claude.json` `oauthAccount` block (§1–§2).
4. *Optional:* delete `sessiometer`'s own config directory,
   `~/Library/Application Support/sessiometer/` (holds `config.toml`), and its logs,
   `~/Library/Logs/sessiometer/`. Both are `sessiometer` state only; Claude Code never reads
   them.
5. Remove the binary.

Claude Code is now authenticated as the account from step 3, with no `sessiometer`
involvement.
