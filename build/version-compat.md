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

---

# Issue #101 — isolated-`CLAUDE_CONFIG_DIR` credential-refresh lifecycle (spike)

The knowledge gate for the isolated-refresh **engine** ([#102](https://github.com/alexey-pelykh/sessiometer/issues/102)):
the engine seeds a managed account's credential into an isolated `CLAUDE_CONFIG_DIR`, spawns `claude -p`
so Claude Code performs its **own** credential refresh in that dir's isolated keychain item, and reads
the refreshed token back. This validates the six lifecycle facts that gate the build, decoded from the
stock **CC 2.1.181** binary (`~/.local/share/claude/versions/2.1.181`, 2026-06-30) — the method #100
used — plus **safe isolated live probes**. Provenance / redaction: every probe used **FAKE non-secret
tokens** (clearly labelled); no real token was ever read or logged; keychain inspection was metadata-only
(`dump-keychain`, no `-d`). The machine ran a live sessiometer daemon (pid-confirmed) managing 3 real
accounts; a **real** credential refresh was therefore **deliberately not performed** (a real refresh
exchanges a real refresh token — rotation could invalidate a managed account and race the daemon's swaps,
violating the issue's zero-impact mandate). After the probes the login keychain was byte-identical to
its starting state and the daemon untouched.

## Results at a glance

| AC | Question | Verdict | Evidence |
|---|---|---|---|
| **1** | CC refreshes in the isolated dir's own keychain item, seeded beforehand | ✅ **CONFIRMED** | binary (read path = `n1()`-resolved service) + live (seeded fake → CC used it) |
| **2** [BUILD-BLOCKER] | apple-tool: read-back succeeds **silently** after CC's own save | ✅ **FAVOURABLE — no heal-write** | binary (CC saves via apple-tool: `security`) + live (silent read-back of a CC-written item) + CI (`real_cli`) |
| **3** | `expiresAt` across repeated runs: sliding vs capped | ⚠️ **CC-side SLIDING (proven); server-side residual** | binary (store = `now+expires_in*1000`, no cap). Multi-day server datum → #102 telemetry / operator probe |
| **4** | Exact path-normalization before hashing the suffix | ✅ **CONFIRMED = #100** | binary (#100 `n1()`) + live (sha256/node cross-check + 401 at the suffixed item) |
| **5** | Minimal isolated `.claude.json`; proactive vs near-expiry | ✅ **CONFIRMED** | binary (`pQ` 5-min window + proactive scheduler) + live (print-mode = no onboarding; CC auto-writes `.claude.json`) |
| **6** | Simplification: honor a seeded plaintext `.credentials.json` + write back there | ✅ **REJECTED on macOS** | binary (`uc()` keychain-PRIMARY, plaintext-FALLBACK, migrate-to-keychain on save) |

Legend: ✅ resolved · ⚠️ partially resolved (residual named)

> Symbol names below (`n1`, `uc`, `pQ`, `E$s`, `HRr`, `kun`, `ZE`, `vO`, `Bn`/`Fb`, `Doe`) are the
> minified identifiers in CC 2.1.181; quoted expressions are decoded verbatim from the binary.

## AC-1 — CC refreshes in the isolated item, seeded beforehand ✅

CC's credential **read** resolves the service through `n1()` (the #100 config-dir-suffix function) and
reads the macOS-username `acct` with `-w`:

```js
await Bn("security",["find-generic-password","-a",vO()/*=$(whoami)*/,"-w","-s",n1(Doe)],…)
```

So under `CLAUDE_CONFIG_DIR=<dir>` CC reads exactly `Claude Code-credentials-<sha256(NFC(<dir>))[:8]>`,
acct = macOS username — the item the engine seeds. **Live**: a fake credential seeded at the computed
suffixed service (`…-e9e8e7bd` for the probe dir) made `claude -p` return `Failed to authenticate. API
Error: 401 Invalid bearer token` — distinct from the empty-dir `Not logged in · Please run /login`,
proving CC located, read, and *used* the seeded isolated item. (The default config dir does **not**
fall back to the suffixed item, and vice-versa — confirmed in #100.)

## AC-2 — apple-tool: read-back is silent after CC's own save ✅ (BUILD-BLOCKER cleared)

**This was the gating risk: if CC saved the refreshed token via the Security.framework SDK as its own
team identity, that write would re-stamp the item's partition list to CC's `teamid:` and evict
`apple-tool:` — and sessiometer's `security -w` read-back would then raise a SecurityAgent prompt /
hang.** (`macos-keychain-internals`: a `kSecValueData` write re-stamps the partition to the *writer's*
identity.)

Decoded, CC's save (`update(e)`) writes through the **Apple-signed `security` CLI** — the *same*
identity sessiometer uses, not the SDK:

```js
async update(e){ … let r=Oe(e), o=Buffer.from(r,"utf-8").toString("hex"),
  s=`add-generic-password -U -a "${vO()}" -s "${n1(Doe)}" -X "${o}"\n`;
  if(s.length<=CDu/*4032*/) await Fb("security",["-i"],{input:s,…})        // PRIMARY: security -i on stdin
  else await Fb("security",["add-generic-password","-U","-a",n,"-s",t,"-X",o],…) // argv fallback (large payload)
}
```

`add-generic-password -U` is an atomic in-place update; the blob is the credential JSON hex-encoded via
`-X` (sessiometer seeds raw via `-w`; both store identical bytes, both read back byte-exact with `-w`).
The native Security.framework symbols present in the binary (`SecItemAdd`, `kSecValueData`, …) are
dynamic-link symbols in a CoreFoundation/Security symbol cluster, **not** the credential code path —
every credential read/write/delete site spawns `security`.

⇒ Writer identity is `apple-tool:` throughout (CC saves, sessiometer seeds, sessiometer reads — all via
`/usr/bin/security`). The partition list is never re-stamped away from `apple-tool:`, so the read-back
**succeeds silently. No apple-tool: heal-write is required before reading.**

**Live**: a CC instance, on a 401, rewrote the login-keychain item in place via its apple-tool: path
(see the dead-token bonus below); a subsequent `security find-generic-password -w` read-back returned
**exit 0, silent** — a direct observation of "CC wrote the item → apple-tool: read-back is silent." **CI**
corroboration: `keychain::tests::real_cli` seeds via `security`, rewrites via sessiometer's `security
-i`, and reads via `security -w`, all green in non-interactive CI (a partition mismatch would surface
as exit 36 / hang).

## AC-3 — `expiresAt` evolution: CC-side sliding (proven); server datum residual ⚠️

CC stores the refreshed expiry **verbatim from the server, with no client-side cap**:

```js
return {accessToken:e.access_token, refreshToken:e.refresh_token, expiresAt:Date.now()+e.expires_in*1000, …}
// refresh-success handler: {access_token:l, refresh_token:c=e/*keep old RT if response omits one*/, expires_in:u}=a, d=Date.now()+u*1000
```

So **CC-side the window SLIDES** — each successful refresh sets `expiresAt = now + expires_in`; CC
never caps it. Whether the *feature* delivers durable value over many days therefore reduces to two
**server** properties, both carried in the `/v1/oauth/token` response (which includes both `expires_in`
**and** `refresh_token_expires_in`):

1. Does `expires_in` stay constant across refreshes (slide) or shrink toward a deadline (cap)?
2. Does the server **rotate** the refresh token, and does the new RT get a fresh `refresh_token_expires_in`?
   - rotates + fresh RT life ⇒ indefinitely sliding (durable).
   - no rotation (CC keeps the old RT via `c=e`) + fixed RT life ⇒ hard ceiling = the RT's original
     lifetime (capped ⇒ reassess).

This server datum is observable from a **single** real refresh (the integer TTLs + whether a new
`refresh_token` is returned), but obtaining it requires a real account's refresh token — **not** run
here (zero-impact mandate; live daemon + 3 real accounts), and the AC's "over several days" is
inherently longitudinal. **Resolution path** (either suffices):

- **Preferred — #102 telemetry**: the engine reads before/after `expiresAt` every cycle; emitting that
  delta (redacted) across its first days of operation *is* the multi-day observation, gathered safely
  through the engine's own CAS-protected flow. Wire an `expiresAt`-delta + RT-rotation breadcrumb into
  #102 and AC-3 settles itself in production.
- **Or — operator-gated probe**: pause the daemon, pick a disposable account, do one refresh, record the
  integer `expires_in` / `refresh_token_expires_in` / rotation flag (redacted), re-stash.

**Build is not blocked** (AC-2, the true blocker, is favourable). AC-3 gates *value*, not feasibility;
CC-side sliding + the forced-refresh lever (AC-5) make the engine able to drive refreshes deterministically.
Recommend **proceed**, carrying "fixed-cap ⇒ reassess" as the documented contingency.

## AC-4 — path-normalization before hashing ✅ (= #100, byte-for-byte)

No new normalization: the suffix is `sha256(NFC(raw CLAUDE_CONFIG_DIR value))[:8]`, **no path expansion**
(no `~`, realpath, relative→absolute, or trailing-slash) — the issue prose's "expandedPath" framing is
wrong; #100 decoded `n1()` from the binary and pinned it. **Live cross-check**: `printf %s '/abs/path' |
shasum -a256 | cut -c1-8` and `node -e 'sha256(NFC).slice(0,8)'` both yield `6d80187b`; `/opt/cc` →
`34fd9c6e` — matching the #100 `keychain::tests` assertions. The AC-1 live probe independently confirms
targeting: CC hit the item at `…-e9e8e7bd` = `sha256(<probe dir>)[:8]`.

## AC-5 — minimal `.claude.json`; proactive vs near-expiry ✅

**Refresh trigger.** The first-party getter refreshes iff a 5-minute pre-expiry predicate holds:

```js
function pQ(e){ if(e===null) return !1; let t=300000; return Date.now()+t>=e }  // refresh when ≤5 min to expiry, or past
async function zOr(e,t,n){ … let o=await h0(); if(!t){ if(o&&!pQ(o.expiresAt)) return "not_needed"; … } /* else POST /v1/oauth/token */ }
```

CC also *proactively* schedules a background refresh (`setTimeout` at ≈`expires_in − buffer`, 30 s floor)
in a long-lived session, and refreshes on-demand at request time. For the engine's short-lived `claude
-p`, the **on-demand** path governs: **CC refreshes only if the seeded `expiresAt ≤ now + 5 min`** (else
it returns `not_needed`). ⇒ **Engine lever**: seed the isolated copy with a **back-dated** `expiresAt`
(≤ now+5 min) to force a deterministic refresh on every run — safe, because real validity is server-side
and the still-valid refresh token does the work. Refreshes are serialized by a cross-process OAuth lock
(`tengu_oauth_token_refresh_lock_*`) scoped to the config dir, so the isolated instance never contends
with a live session.

**Minimal `.claude.json`.** None required for headless `claude -p`. **Live**: an isolated empty dir,
no `.claude.json`, no creds → `claude -p` printed `Not logged in · Please run /login` (exit 1) with **no
onboarding / theme / trust prompt** (print mode is non-interactive), and CC **auto-wrote** a minimal
`.claude.json` (0600): `firstStartTime`, `machineID`, `userID`, migration flags, `seenNotifications` —
**no** `hasCompletedOnboarding` / `theme` / `hasTrustDialogAccepted` needed. The engine may seed an
empty/minimal `.claude.json` (belt-and-suspenders) but onboarding keys are unnecessary.

## AC-6 — simplification probe (seeded plaintext `.credentials.json`) — REJECTED on macOS ✅

CC *does* have a plaintext backend — `<config_dir>/.credentials.json`, written 0600 with a `"Warning:
Storing credentials in plaintext."` — but it is only a **fallback**. The store is built unconditionally
keychain-primary:

```js
function uc(){ return E$s(HRr/*name:"keychain"*/, kun/*name:"plaintext"*/) }  // "keychain-with-plaintext-fallback"
// combinator E$s(e=primary, t=fallback):
//   read:   primary first; fall back to plaintext only if primary returns null
//   update: write primary; fall back to plaintext only if the primary write FAILS non-transiently;
//           on primary-write success with a previously-EMPTY primary →  await t.delete()  ← deletes the seeded file
```

There is **no platform branch and no force-plaintext knob** — `uc()` is keychain-primary everywhere (on
Linux the keychain primary simply fails → plaintext fallback). Consequence on macOS: seeding **only**
`.credentials.json` (keychain empty) makes CC *read* it once (primary null → fallback), but the first
refresh-**save** writes the new token to the **keychain** (`add-generic-password -U` succeeds on the
empty keychain) and, because the primary was previously empty, **deletes the seeded `.credentials.json`**.
The refreshed token ends up in the keychain regardless.

⇒ **The simplification does not hold on macOS**: teardown cannot be `rm -rf <dir>` — CC migrates the
credential into the suffixed keychain item, which must be removed with `security delete-generic-password`.
The engine must use the isolated keychain item (the #100/#102 approach). This costs nothing: AC-2 proved
the keychain path is silent (apple-tool: throughout), so the partition-handling the simplification aimed
to avoid was never a problem.

## Downstream design impact (propagated to #102)

- **Seed + read-back via apple-tool: `security` only** (sessiometer already does — `keychain.rs`). CC's
  own save uses the identical `security -i` apple-tool: path, so the partition stays apple-tool: and the
  read-back is silent. **No heal-write step needed** (drops a step from #102's plan).
- **Force the refresh deterministically**: seed the isolated copy with `expiresAt ≤ now + 5 min`
  (back-dated). A fresh seed left at its true (far) expiry yields `not_needed` and a wasted spawn.
- **Classify by `refreshToken`, not only `expiresAt`**: on a dead/invalid token CC rewrites the item in
  place setting `claudeAiOauth.refreshToken = ""` (siblings preserved) — see bonus below. #102 step 6
  (classify refreshed / no-change / dead / error): treat `refreshToken == ""` (or RT changed) as the
  **dead** signal; `expiresAt` alone is unreliable (CC may leave it untouched on failure). Dead ⇒ no
  re-stash, leave as-is, surface (#102 step 7).
- **Keychain item, not a plaintext file** (AC-6): teardown deletes the suffixed item via `security
  delete-generic-password` **and** `rm -rf <dir>`.
- **No keychain pinned by CC**: CC's `security` calls do **not** pin a keychain file (default search
  list); sessiometer pins `login.keychain-db`. Both resolve to the same item as long as the login
  keychain is on the default search list (it is, by default) — the engine should not assume CC and
  sessiometer pin identically, only that they converge on the login keychain.
- **Refresh is lock-serialized** per config dir; the isolated instance won't contend with a live session.

### Bonus — CC's dead-token handling (live)

Seeding a fake far-future-expiry credential (so `pQ` = false, **no** OAuth call) and running `claude -p`
produced `401 Invalid bearer token`; CC then **rewrote the keychain item in place** — `claudeAiOauth.
refreshToken` set to `""`, `accessToken` / `expiresAt` / `scopes` and an unrelated sibling top-level key
all preserved — and the post-write `security -w` read-back was silent (exit 0). This is the #102
dead-signal and a second live witness for AC-2.

## Provenance

- Decoded from the stock **CC 2.1.181** binary (`~/.local/share/claude/versions/2.1.181`); the user's
  `claude` is a byte-patched wrapper, so inspection targeted the stock binary (the engine should pin the
  binary it spawns). Live probes used the stock binary directly.
- Live probes: isolated `CLAUDE_CONFIG_DIR` under `.tmp/`, **fake non-secret tokens only**, far-future
  expiry (no OAuth call) for the targeting/round-trip probes; full cleanup verified (keychain identical
  to start, daemon untouched). No real token read or logged; keychain inspection metadata-only.
- AC-2 partition mechanics per the `macos-keychain-internals` skill (apple-tool: vs teamid: re-stamp on
  `kSecValueData` write); re-verify on macOS major bumps.
- Real-refresh-dependent residual (AC-3 server sliding-vs-cap) deferred to #102 telemetry / an
  operator-gated probe — see AC-3.

CC 2.1.181 · macOS 26.5.1 / 25F80 · sessiometer #101.

---

# #130 — isolated-`CLAUDE_CONFIG_DIR` **interactive**-login lifecycle ✅ PASS

Acceptance record for issue [#130](https://github.com/alexey-pelykh/sessiometer/issues/130): a
one-time empirical check that an **interactive** `claude /login` (browser-OAuth handoff), run under an
isolated `CLAUDE_CONFIG_DIR`, writes its credential to the **suffixed** keychain item (#100) and leaves
the shared `Claude Code-credentials` item a live session reads **byte-for-byte unchanged**. The
front-loaded gate for the interactive-login capture engine (#132). Delta over #101: interactive
`/login` (real browser handoff + credential-write target) vs #101's non-interactive `claude -p`.

## Environment

| Field | Value |
|---|---|
| Claude Code version | `2.1.197` (re-validated on the bump from `2.1.181` at #101) |
| macOS | `26.5.1` / `25F80` |
| Run | 2026-07-01 (two operator-driven interactive runs) |
| acct | macOS username (`whoami`) |

## Result — ✅ PASS (two independent runs)

Suffix derived byte-exactly by sessiometer's own #100 derivation (`printf %s <config-dir> | shasum
-a256 | cut -c1-8`, cross-checked live against the committed `keychain::tests` `/abs/path → 6d80187b`
vector). Shared-item integrity checked two ways **without surfacing the secret**: the attribute dump
(incl. `mdat`) and a `security -w | shasum` blob hash, captured before each run and re-verified after
teardown.

| AC | Check | Run 1 (`claude /login`) | Run 2 (bare `claude`, seeded) |
|---|---|---|---|
| 1a | interactive login → **suffixed** item gets a fresh blob | ✅ blob `de2a3701…` | ✅ blob `45066956…` |
| 1b | shared `Claude Code-credentials` **byte-for-byte unchanged** | ✅ attr `125cc3f0…` + blob `447f5211…` == baseline | ✅ attr `125cc3f0…` == baseline |
| 2  | browser OAuth completes under isolation | ✅ both handoffs completed; CC authenticated in the isolated dir |

Teardown (`security delete-generic-password` on the suffixed item + `rm -rf` the dir) ran clean each
time; the shared item was intact after both.

⇒ **The isolation premise holds for interactive `/login` on CC 2.1.197.** #132 (capture engine) is
**UNBLOCKED**.

## Onboarding / single-login mechanics (probe) → propagated to #132

Run 1 surfaced an operator-UX wrinkle: a **fresh** isolated dir made `claude /login` run first-start
onboarding (trust-folder + theme prompts) **and** a **double login** — an onboarding auto-login **then**
the explicit `/login`. Run 2 was a controlled probe to isolate the cause: a **seeded** `.claude.json`
(`{"hasCompletedOnboarding":true,"theme":"dark","hasTrustDialogAccepted":true}`) + a **bare** `claude`
(no `/login`). Observed:

- **No theme prompt** (seed `theme` honored); **no onboarding auto-login** — CC sat idle at `Not logged
  in · Run /login` until the operator typed `/login` once.
- ⇒ the run-1 auto-login was **onboarding's own login step**, removed by `hasCompletedOnboarding:true` —
  **not** a generic auth-gate. **Config-seeding alone removes the extra login; no auth pre-seed is
  needed** (which would defeat capturing a fresh login anyway).
- **Trust-folder dialog STILL appeared** — top-level `hasTrustDialogAccepted` does **not** cover it;
  trust is **CWD-scoped** (the accessed workspace was the operator's terminal cwd), stored per-project.

**#132 design impact (evidence-based):**

1. **Single login while keeping the AC-mandated `/login`:** seed the isolated `.claude.json` with
   `hasCompletedOnboarding:true` (+ `theme`), then pass `/login`. Onboarding login skipped (0) +
   explicit `/login` (1) = **one** login; the operator only completes the browser OAuth. (Directly
   observed: seeded ⇒ no onboarding auto-login; `/login` ⇒ exactly one login. #132's tests should
   confirm the composed `seeded + /login` case.)
2. **Seed content differs from the refresh path.** #101 AC-5 seeds `{}` (`MINIMAL_CLAUDE_JSON`) because
   headless `claude -p` skips onboarding; the **interactive** login path needs the onboarding keys
   above. If #131's shared seam parametrizes only argv/stdio/cancel-arm, #132 must supply this seed
   itself (or #131 adds seed-content as a seam parameter).
3. **Trust dialog** is a separate, CWD-scoped friction — suppress by seeding the per-project trust entry
   for the launch cwd (or launch from a pre-trusted/neutral dir). Minor (one keystroke); polish, not a
   blocker.

## Provenance

- Live, operator-driven, on the operator's Mac; **real** interactive OAuth (no fake tokens — this is the
  interactive-flow gate #101's `claude -p` probes could not cover). Two runs.
- Shared-item baseline captured before each run and re-verified after teardown; the secret blob was
  hashed, never printed. Isolation held both times.
- Suffix derivation identical to `keychain::service_for_config_dir` (#100); bash `shasum` cross-checked
  against the committed `/abs/path → 6d80187b` test vector.

CC 2.1.197 · macOS 26.5.1 / 25F80 · sessiometer #130.
