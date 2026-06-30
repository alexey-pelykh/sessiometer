# Pre-build empirical checks (H0–H3) — results ledger

The acceptance record for issue [#16](https://github.com/alexey-pelykh/sessiometer/issues/16): a
one-time empirical verification of the macOS credential mechanism, run **before** the swap engine
(#6) is built. The verification was originally driven by a bash harness + runbook; now that the
read/write primitives are ported to Rust and test-covered
([#2](https://github.com/alexey-pelykh/sessiometer/issues/2), `src/keychain.rs`), that harness has
been **retired** — its findings are permanently captured here, and the automated re-verification
path is the `security`-CLI round-trip test in `src/keychain.rs`. Two accounts used below as **A**
(primary) and **B** (secondary).

## Environment

| Field | Value | How captured |
|---|---|---|
| Claude Code version | `2.1.181` | `claude --version` |
| macOS version | `26.5.1` | `sw_vers -productVersion` |
| macOS build | `25F80` (Darwin 25.x) | `sw_vers -buildVersion` |
| Run | 2026-06-25 → 2026-06-26 (R1) | — |

## Results at a glance

| Check | Result | Reshapes |
|---|---|---|
| **H3 — fresh-start adoption** | ✅ **PASS** — a fresh `claude` fully adopts a CLI-written (token + `oauthAccount`) pair | #6 swap is feasible (restart-based, proven) |
| **H3 — running-session (mid-swap)** | 🟡 **Very likely** (strong inference via `/login` equivalence; restart fallback proven) | #6/#12 hot-swap path |
| **H2 — keychain vs `oauthAccount`** | ✅ **RESOLVED** — orthogonal: **token = auth/quota**, **`oauthAccount` = display**; mismatch tolerated, no re-auth | #6 co-write is **best-effort** (display), not atomic-critical |
| **H1 — stash survives `/login`** | ✅ **PASS** — namespaced stash intact after a login as B | #4 capture |
| **H0 — locked-keychain read** | ⚠️ **TTY prompts, not exit 36** — interactive context artifact | #13 (daemon must read non-interactively) |

Legend: ✅ PASS · ⚠️ caveat · 🟡 strong-inference

---

## The model (what the run established)

**The keychain token and `~/.claude.json` `oauthAccount` govern different things — they are orthogonal.**

- **Keychain `Claude Code-credentials` token = the bearer credential.** It is the *only* token present, so
  in H2 test 3a (token=B, `oauthAccount`=A) the request that succeeded was authenticated as **B** while
  `/status` *displayed* A. ⇒ **the token is what reroutes auth/quota** — this is the functional swap.
- **`~/.claude.json` `oauthAccount` = identity/display only** (it contains account metadata —
  `emailAddress`/`accountUuid`/`organizationName` — **no bearer token**). It drives what `/status` shows,
  is re-read live, and a token/`oauthAccount` mismatch is **tolerated** (no re-auth, request still served).
- **`login.keychain-db` is the store** (run 1: A-vs-B blobs differ by sha256; `mdat` bumps per `/login`).
  The Data-Protection-Keychain worry is dead; `/usr/bin/security` reads/writes it. The item's `acct`
  attribute is the **macOS username**, never the Claude account.

This fully explains run 1's "the swap did nothing": swapping the token alone *did* reroute quota to B,
but the display stayed A (config untouched) and both accounts share `displayName` "Oleksii" → invisible.

## Per-check detail

**H3 — fresh-start adoption — ✅ PASS.** `set_state A` (co-write token+`oauthAccount`) → fresh `claude`
`/status` resolved to A (email + org); `set_state B` → resolved to B. A `security`-written consistent
pair is honored on launch. ⇒ #6 can perform a swap and a (re)started `claude` uses the new account.

**H3 — running-session (mid-swap) — 🟡 very likely hot-swappable (strong inference).** Not bench-isolated
here — `/status` re-reads config and masks whether a *running* session re-reads the *token*. But the owner
reports, from daily use, that running `claude /login` in one terminal makes **all** running `claude`
sessions adopt the new credentials with no restart. `/login` ends with the same two writes our swap makes
(keychain token + `~/.claude.json`); the cross-instance propagation is almost certainly driven by the
`~/.claude.json` change (a shared file every instance watches), which our `set_state` reproduces.
**Residual assumption:** the reload re-reads the keychain *token*, not only config — strongly implied by
"all claudes use new *credentials*." **Confirm** (optional, ~2 min): guide § "Optional: garbage-token
confirm." **Fallback:** restart-based swap is proven (above), so #6 is unblocked regardless.

**H2 — keychain vs `oauthAccount` — ✅ RESOLVED.** Both mismatches (token=B/cfg=A and token=A/cfg=B) ran a
request successfully, with **no re-auth**, and `/status` followed **config**, never reconciling. ⇒ the two
are orthogonal (see model). #6's `oauthAccount` co-write is **best-effort for display honesty (#9)**, not
atomic-critical for function.

**H1 — stash survives `/login` — ✅ PASS.** Stashed A under `Sessiometer/acct-A`, ran `claude /login` as B,
read the stash back: blob byte-matches A. ⇒ #4 can stash per-account credentials under a namespaced item
that survives logins.

**H0 — locked-keychain read — ⚠️ TTY-prompt artifact, not exit 36.** A locked-keychain read in an
interactive terminal produced a **UI unlock prompt** + `exit=0`, not `errSecInteractionNotAllowed` (36).
That is the *interactive* context (the CLI is allowed to prompt). A background daemon (no UI) is the
context that yields 36. ⇒ #13: the daemon must read with **interaction disabled** (so a locked keychain
fails deterministically with 36) and/or **pre-check lock state** — it must **never** trigger an
interactive prompt. This run validated the interactive path, not the daemon path.

## Downstream design impact (propagated to the issues)

- **#6 (swap engine):** swap = write the keychain token (functional reroute) **and** co-write
  `~/.claude.json` `oauthAccount` (display) — i.e. **replicate `/login`'s writes** to inherit its
  proven cross-session propagation. Co-write is best-effort, not atomic-critical. Restart-based swap is
  the proven fallback. If propagation ever fails, `/login` may emit an extra signal we must reproduce.
- **#12 (mid-turn swap):** the credential-cut core — a concurrent reader across a forced swap sees a
  clean A→B cut and never a torn blob — is now demonstrated in CI against the real `security` CLI
  (`src/swap.rs` `tests::mid_turn_live`), resting on the atomic `-U` write. The fully-live tail (a
  running session adopts B on its next request; the in-flight request absorbs ≤1 transparently-retried
  401) still needs a live Claude token: hot-swap (no restart) is very likely viable via the
  `/login`-style propagation above — confirm with the garbage-token test before relying on it;
  otherwise restart on swap (the proven fallback).
- **#4 (account capture):** capture must stash **both** the keychain token **and** the `oauthAccount`
  block per account (H1 confirms the stash survives `/login`).
- **#13 (edge cases):** read the keychain **non-interactively** (expect 36 on lock); pre-check lock state;
  never let the daemon raise an interactive unlock prompt.
- **#9 / #17 (status / roster):** key accounts by `emailAddress` / `accountUuid`, **never** `displayName`
  (two distinct accounts shared the display name "Oleksii").

## Provenance

- Checks defined by issue #16 (`phase:0-smoke`, milestone 0.1.0). R1 (corrected, de-confounded) is the run of record.
- Keychain mechanics (`/usr/bin/security` false-proxy, partition re-stamp on `kSecValueData` write, DPK-vs-legacy
  backing) from the `macos-keychain-internals` skill — empirically observed on Darwin 25.x, under-documented by
  Apple, version-dependent; re-verify on macOS major bumps.
- CC 2.1.181 · macOS 26.5.1 / 25F80.

---

# Issue #39 — off-argv credential write (the swap-write argv exposure)

The follow-up to the #2 residual risk: the swap write passed the credential blob as a
`security add-generic-password … -w <blob>` **argument**, briefly visible in the process table
(`ps`). On a multi-account / shared host that is a real exposure. This records the empirical
resolution, verified against `/usr/bin/security` on the same platform (macOS 26.5.1 / 25F80,
Darwin 25.x), 2026-06-29.

## Finding — `security -i` accepts the write off-argv ✅

`security -i` reads commands from **stdin** and dispatches them internally, so the spawned
process's argv is only `/usr/bin/security -i` — the blob never appears on argv. A full
`add-generic-password -U -s <svc> -a <acct> -w <blob> <keychain>` command (the keychain pinned
**inside** the stdin command) writes correctly and round-trips byte-exact. Adopted: both write
paths exercised during a swap — `keychain::RealCredentialStore::write` (the canonical token) and
`stash::…add_item` (the two stash halves) — feed the command on stdin. A swap performs three such
writes; all three are now off-argv.

## Escaping — double-quote, backslash-escape `\` and `"`

The interactive tokenizer is **not a shell**: inside `"…"`, whitespace and `$`, backticks, `;`,
`|`, `&`, `(` … are all literal. Wrapping each field in `"…"` and escaping `\` → `\\` and
`"` → `\"` carries an arbitrary single-line byte string as exactly one argument. Validated
byte-exact across 10 adversarial payloads (spaces, quotes, backslashes, shell metacharacters,
realistic OAuth JSON, leading-dash, hex) and re-asserted in CI by the `real_cli` metacharacter
round-trip tests in both modules.

## Exit-code contract — preserved

`security -i` propagates the inner command's exit status **unchanged** — the load-bearing property
here. Observed interactive-vs-direct, byte-for-byte: not-found `44 == 44`; a usage/parse error
`2 == 2`; a locked-keychain write surfaced the same raw exit either way (`152` in this probe
context). The specific locked-write code is immaterial to #39: `finish_write` / `keychain_error` /
`stash_error` are untouched, so whatever code `security` returns maps exactly as it did on the old
argv path. (The `36 → KeychainLocked` mapping and the broader daemon lock-handling remain #13's
scope — see the H0 row above for why a lock can surface differently by context.) The move to stdin
changes the input channel, not the status.

## Identity / partition list — unchanged

Still `/usr/bin/security` (the `apple-tool:` code identity), so the partition-list and ACL
guarantees recorded for #16/#2 (and in the `macos-keychain-internals` skill) are untouched — `-i`
changes the **input channel**, not the writer's identity.

## Verification — the blob is absent from argv during a write

Holding the child's stdin open keeps `security -i` alive *after* it runs the write, so its argv
can be read deterministically (not racily): `ps -o command= -p <pid>` shows only
`/usr/bin/security -i`; a sentinel blob is absent, and the item is confirmed written. Encoded as
the CI test `keychain::tests::real_cli::the_blob_never_appears_in_the_process_argv`.

## Rejected alternative — `-w` as the last option (interactive prompt)

`add-generic-password … -w` with no value *does* prompt for the secret, but via `getpass` on
`/dev/tty` **with confirmation** (`password data for new item:` / `retype password for new item:`),
not stdin — unusable for a TTY-less daemon. It also cannot pin an explicit keychain alongside the
prompt (BSD getopt stops permuting at the first positional, so a trailing `-w` after the keychain
path is rejected). Not adopted.

## Residual

- **Embedded newline**: the interactive reader is line-based, so a payload containing `\n` would
  break the command. Real payloads never do (single-line OAuth JSON; the stash `oauthAccount` half
  is pure-ASCII hex), and the failure is **loud, not silent** — the command exits non-zero, so the
  write is reported failed rather than writing a truncated item. Guarded by a `debug_assert!`.
- **Kernel pipe buffer**: the blob transits a pipe to the child. That is process-private (unlike
  world-readable argv) and inherent to any off-argv hand-off; the in-process heap copy of the
  escaped command is `Zeroizing` (wiped on drop).

CC 2.1.181 · macOS 26.5.1 / 25F80 · sessiometer #39.

---

# Issue #100 — keychain service name under a non-default `CLAUDE_CONFIG_DIR`

sessiometer hard-coded the bare service `Claude Code-credentials`. Claude Code suffixes that name
under a non-default `CLAUDE_CONFIG_DIR`, so a CC instance in an isolated config dir was invisible
to every keychain site (read/poll, swap-write, resolve). This records the **exact** derivation, read
from the CC 2.1.181 binary itself — `~/.local/share/claude/versions/2.1.181`, 2026-06-30 — because
the issue's prose described a different (wrong) normalization.

## Finding — the derivation is `sha256(raw env value)[..8]`, NOT an expanded path ✅

The service name is built by CC's `n1("-credentials")` (decoded from the binary):

```js
function n1(e=""){                                      // e = "-credentials" for the credential item
  let t=process.env.CLAUDE_SECURESTORAGE_CONFIG_DIR,
      n=t!==void 0?!t:!process.env.CLAUDE_CONFIG_DIR,    // suffix-ABSENT gate
      r=t!==void 0?t.normalize("NFC"):sr(),              // value HASHED
      o=n?"":`-${createHash("sha256").update(r).digest("hex").substring(0,8)}`;
  return `Claude Code${OAUTH_FILE_SUFFIX}${e}${o}`       // OAUTH_FILE_SUFFIX="" for standard OAuth
}
sr = ()=>(process.env.CLAUDE_CONFIG_DIR ?? join(homedir(),".claude")).normalize("NFC")
```

So `service = "Claude Code-credentials" + suffix`, where:

- suffix = `""` when `CLAUDE_CONFIG_DIR` is **unset or empty** (and `CLAUDE_SECURESTORAGE_CONFIG_DIR`
  unset) — the default config dir, unchanged from prior usage (no regression).
- else suffix = `-` + lowercase-hex `sha256( NFC(value) )`, **first 8 chars**.
- The hashed `value` is the **raw env-var string**, NFC-normalized — **no path expansion of any
  kind**: no `~` expansion, no relative→absolute, no trailing-slash strip, no realpath/symlink. (The
  issue's "AC sketch" claimed all of those; the binary disproves it. The hash is over the literal
  `CLAUDE_CONFIG_DIR` bytes, NFC-normalized, nothing more.)
- `CLAUDE_SECURESTORAGE_CONFIG_DIR` takes **precedence** when defined: a non-empty value is the
  hashed value (and `CLAUDE_CONFIG_DIR` is not consulted); a **defined-empty** value forces the bare
  name. Replicated faithfully — a CC instance with it set would otherwise be mis-targeted.
- `OAUTH_FILE_SUFFIX` is non-empty (`-local-oauth` / `-custom-oauth`) **only** for a custom OAuth
  client id (`CLAUDE_CODE_OAUTH_CLIENT_ID`) — out of scope here, as in the issue.

## Ground truth (CC's exact expression, via `node`)

`sha256(value.normalize("NFC")).digest("hex").slice(0,8)` — NFC is the identity for ASCII paths:

| `CLAUDE_CONFIG_DIR` | service name |
|---|---|
| unset / empty | `Claude Code-credentials` |
| `/abs/path` | `Claude Code-credentials-6d80187b` |
| `/opt/cc` | `Claude Code-credentials-34fd9c6e` |

Pinned as `keychain::tests` assertions, so the Rust derivation is proven byte-for-byte against CC,
not merely self-consistent.

## NFC — hash raw bytes, refuse non-ASCII (no Unicode-normalizer dependency)

CC hashes the **NFC** form. For an ASCII config-dir path NFC is the identity, so hashing the raw
bytes is byte-exact — and the crate hand-rolls its primitives (`sha256`, hex) to keep the dependency
graph minimal, so pulling a Unicode-normalizer crate for the rare non-ASCII tail is disproportionate.
A non-ASCII value could differ between its NFC form and its raw bytes, so rather than silently
address the **wrong** keychain item, resolution **refuses** with `Error::NonAsciiConfigDir`. The value
is read as bytes (`OsStrExt`), never `to_string_lossy` — a lossy decode would hash different bytes
than CC sees.

## Verification

`canonical_service_from` is pure (the env read lives in `canonical_service`), so every arm — default,
empty, suffixed (`/abs/path`, `/opt/cc`), `CLAUDE_SECURESTORAGE_CONFIG_DIR` precedence, defined-empty,
non-ASCII refusal — is unit-tested without mutating process env. The `security`-CLI round-trip test
(`keychain::tests::real_cli`) still passes against the bare base name (its `for_keychain` store pins
the base, hermetic against ambient `CLAUDE_CONFIG_DIR`).

CC 2.1.181 · macOS 26.5.1 / 25F80 · sessiometer #100.
