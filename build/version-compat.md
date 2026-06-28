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
