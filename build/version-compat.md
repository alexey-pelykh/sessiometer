# Pre-build empirical checks (H0–H3) — results ledger

The recording ledger for issue [#16](https://github.com/alexey-pelykh/sessiometer/issues/16): a
one-time empirical verification of the macOS credential mechanism, run **before** the swap engine
(#6) is built. **This file holds the results + versions** (the acceptance record). The procedure
lives in **[`keychain-swap-guide.md`](./keychain-swap-guide.md)**, driven by **[`h3-r1.sh`](./h3-r1.sh)**.

## Environment

| Field | Value | How captured |
|---|---|---|
| Claude Code version | `2.1.181` | `claude --version` |
| macOS version | `26.5.1` | `sw_vers -productVersion` |
| macOS build | `25F80` (Darwin 25.x) | `sw_vers -buildVersion` |
| First run | 2026-06-25 | — |

> Re-capture `claude --version` at each run — it is part of the acceptance record and may have advanced.

## Results at a glance

| Check | Hypothesis | Result | Reshapes |
|---|---|---|---|
| **H3** | Running `claude` adopts a mid-session keychain swap | ⚠️ INCONCLUSIVE — confounded; needs R1 rerun | #6 swap mechanism (blocking) |
| **H2** | `~/.claude.json` `oauthAccount` load-bearing vs keychain | 🟡 PARTIAL — `oauthAccount` drives the *displayed* account; precedence-on-mismatch pending | #6 co-write: atomic-critical vs best-effort |
| **H1** | A stashed credential survives a `claude /login` | ⬜ PENDING | #4 capture / #6 stash viability |
| **H0** | Locked-keychain read returns exit code 36 (advisory) | ⬜ PENDING | #13 locked-keychain edge case |

Legend: ⬜ PENDING · ✅ PASS · ❌ FAIL · ⚠️ INCONCLUSIVE/caveat · 🟡 PARTIAL

---

## Findings — run 1 (2026-06-25 · CC 2.1.181 · macOS 26.5.1)

The first run swapped the keychain token but never the matching `oauthAccount`, and both test
accounts share `displayName` "Oleksii" — so the swap *looked* inert. That was a procedure/observation
problem, not an inert system. What it nonetheless established, with direct evidence:

### ✅ Confirmed — `login.keychain-db` IS claude's credential store
The "A" and "B" captures are **different blobs** (`sha256` differs; the item's `mdat` advanced on
every `/login`). So `claude` genuinely reads/writes the legacy file-based login-keychain item, and
`/usr/bin/security` is **not** blind to it. The macOS-26 "is it the Data Protection Keychain?" worry
is resolved: **no, it's the legacy login keychain.**
**⇒ #2** (keychain read/write via the `security` CLI) is viable; the `security-framework` ban stands.

### ✅ Confirmed — the active account is driven by `~/.claude.json` `oauthAccount`
The keychain item's `acct` attribute is just the macOS username (`alexey-pelykh`), never the Claude
account. Which account `claude` shows/uses tracks `~/.claude.json` `oauthAccount`, which the keychain
swaps never touched (it stayed on the primary throughout).
**⇒ #6** must **co-write both** the keychain token *and* `oauthAccount`; swapping the token alone can
never switch accounts. (Half of H2 answered: `oauthAccount` is load-bearing for identity.)
**⇒ #4** capture must stash **token + oauthAccount block** per account.
**⇒ #9 / #17** the roster must key accounts by `emailAddress` / `accountUuid`, never `displayName`.

### ⚠️ Open — caching vs re-read not yet attributable (needs R1)
"The swap did nothing" cannot yet be pinned to "running `claude` caches the token (watch-for **b**)"
vs "it re-reads but identity is `oauthAccount`-driven", because run 1 moved both knobs at once and
the display-name collision masked any token-level change. **R1** (guide Tests 1–3) isolates these:
co-write both sides, observe by email/uuid, and separate fresh-start from running-session adoption.

> **Weak corroboration (not proof):** the *coordinating* `claude` session stayed authenticated on its
> original account across ~90 min of keychain rewrites and several `/login`s during run 1 — consistent
> with start-up caching (watch-for **b**), but token-refresh timing was uncontrolled, so R1 still decides it.

---

## Result slots (fill from the R1 rerun — see the guide)

**H3 — fresh-start adoption (guide Test 1):** ⬜ _does a fresh `claude` honor a `set_state` pair?_
**H3 — running-session adoption (guide Test 2):** ⬜ _watch-for a/b/c; any keychain prompt; /status_
**H2 — precedence on mismatch (guide Test 3):** ⬜ _keychain-wins / oauthAccount-wins / re-auth_
**H1 — stash survives /login (guide Test 4):** ⬜ _exit code · silent? · blob matches A?_
**H0 — locked-keychain read (guide Test 5):** ⬜ _actual exit code (expect 36)_

## Interpretation → downstream design (fill once R1 lands)

- **H3 → #6 / #12:** fresh-start adopts ⇒ swap-on-restart works; running-session result decides whether a
  live swap needs a `claude` restart/signal (the hard mid-turn case, #12).
- **H2 → #6:** keychain-wins ⇒ `oauthAccount` co-write best-effort · oauthAccount-wins ⇒ atomic-critical ·
  re-auth ⇒ both must be written atomically.
- **H1 → #4:** confirms the namespaced token+oauthAccount stash survives `/login`.
- **H0 → #13:** pins the exit code the daemon's "keychain locked → back off" branch keys on.

## Provenance

- Checks defined by issue #16 (`phase:0-smoke`, milestone 0.1.0).
- Keychain confounds (false-proxy of `/usr/bin/security`, partition re-stamp on `kSecValueData` write,
  DPK-vs-legacy backing) from the `macos-keychain-internals` skill — empirically observed on Darwin 25.x,
  under-documented by Apple, version-dependent; re-verify on macOS major bumps.
- Harness + run-1 finding authored 2026-06-25. Run-1 was procedure-confounded; R1 (corrected) is the rerun of record.
