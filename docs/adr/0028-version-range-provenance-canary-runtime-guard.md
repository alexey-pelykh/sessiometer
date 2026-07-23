---
type: architecture-decision-record
number: 28
title: "CC version range is provenance, not a runtime gate; the #714 behavioral canary is the runtime compatibility guard"
date: 2026-07-23
status: accepted
decision_makers: [Oleksii PELYKH (maintainer)]
---

# ADR-0028: Claude Code version range is provenance, not a runtime gate — the #714 behavioral canary is the runtime compatibility guard

## Status

**Accepted** — 2026-07-23. Records that Sessiometer does **not** gate on the Claude
Code version at runtime (it never did), consolidating the settled *"support all
versions until behavior diverges"* model. It makes explicit the framing left
implicit once the #714 behavioral canary shipped and the #715 runtime version
advisory was demoted to provenance (**#716**). Reached via a `/council` (SRE /
security / product lenses, 2026-07-23, CONVERGENT).

## Context

Sessiometer depends on reverse-engineered Claude Code internals — the **#100**
keychain-service derivation and the **#101** credential-refresh lifecycle — verified
only within a range recorded in `build/version-compat.md`. A natural but wrong
instinct is to treat the installed CC version as a runtime compatibility gate.

Two facts make the version number a poor control:

- A **matching** version does not prove the reverse-engineered internals are
  unchanged, and a **mismatched** one does not prove they changed. The version is a
  proxy, not the property that matters.
- CC **auto-updates** on the user's machine, routinely past the verified MAX
  (already at 2.1.218+ against a verified MAX of 2.1.217), so a runtime "outside
  verified range" advisory (**#715**) fires for effectively the entire userbase —
  cry-wolf that trains operators to ignore the one channel they should heed.

The compatibility risk is real, but it lands at **runtime on the user's machine** —
exactly where a static version comparison cannot see it.

## Decision

**The Claude Code version range is provenance; the #714 behavioral canary is the
sole runtime compatibility guard.** Three parts:

1. **No runtime version gate** (and there never was one). Sessiometer supports all
   CC versions and reacts only when observed behavior diverges — the
   "support-all-until-drift" model.
2. **The #714 behavioral canary is the runtime detector.** It re-derives the
   keychain service (#100) at boot and before every swap, resolves the canonical
   credential, and compares it exact-byte against the per-account credential
   stashes: a match to a *different* account than the active one is drift → it
   **refuses the credential write** (fail-closed); a match to the active account is
   OK. It catches the drift that matters — a changed derivation or a mis-resolved
   canonical — on the machine where the risk lands, not by proxy.
3. **The range is provenance a user can pull, not an alarm pushed at them.** #716
   removed the #715 runtime advisory and surfaces the verified range as a neutral,
   unconditional line in `sessiometer --version`
   (`cc_version::supported_range_provenance()` → `verified against Claude Code
   MIN-MAX`), alongside the `build/version-compat.md` ledger and this ADR.
   `scripts/check-cc-version.sh` remains an advisory *release-time* provenance
   check, not a runtime gate.

This **amends ADR-0002** (keychain access via the `security` CLI): that ADR
established the reverse-engineered keychain model; this one records how its
version-compatibility risk is actually governed.

## Alternatives considered

1. **A runtime version gate / advisory (#715).** Rejected and removed (#716): the
   version both false-positives (auto-update past MAX) and false-negatives (a
   same-version derivation change). Cry-wolf that degrades the signal operators
   should act on.
2. **Refuse to run outside the verified range.** Rejected: it would strand the
   userbase on every CC auto-update while proving nothing about actual
   compatibility, and the behavioral canary already covers the real failure at the
   point of risk.
3. **Online identity verification as the primary runtime signal** (the #714
   Option B, `/api/oauth/profile`). Deferred, not adopted: the #714 re-council chose
   the **offline** stash-token cross-check (Option C) as the foundation — no network
   dependency on the swap path, no added failure mode — with online identity
   reserved as an operator-enablable escalation only if the shared/duplicate-token
   falsifier fires.

## Consequences

### Positive

- The runtime guard sits where the risk is (the user's keychain derivation), not on
  a proxy that a version comparison can only guess at.
- **No cry-wolf.** Operators keep no habit of ignoring a version banner, so the
  canary's refuse verdicts land.
- The range still travels as durable, citable provenance (`--version` + ledger +
  this ADR) for anyone verifying what was tested.
- The decision now travels — it previously lived only across the #714 / #715 / #716
  issue threads.

### Negative / trade-offs / residual gap (disclosed)

- **The canary fails OPEN on its INCONCLUSIVE cases.** It fails closed on positive
  drift, but when it cannot determine identity — notably `Inconclusive(NoStashMatch)`,
  the resolved canonical matches no stash — the swap currently proceeds. The sharpest
  residual (a *future CC storage-format change* → `NoStashMatch` on the **active**
  account → an unrelated secret clobbered by the atomic `-U` upsert) is being
  hardened to fail-closed, shape-gated, by **#730**.
- **Layer-3 same-account silent relocation** (named in #714) remains a residual: an
  offline cross-check cannot detect a same-account credential relocated in place. The
  online liveness/identity probes that would close it are deferred (#714 options).
- **No independent online signal today.** The offline cross-check is the foundation;
  if roster accounts ever share credential blobs, its false-positive profile degrades
  and the deferred online identity fetch becomes warranted (the #714 falsifier).

## Related

- Issues: **#714** (behavioral canary — the runtime guard + its INCONCLUSIVE
  residual), **#716** (version-advisory demotion — the counterpart "the version proxy
  is not the control" decision), **#730** (hardening the INCONCLUSIVE→fail-open
  residual, shape-gated — **open**), **#715** (the removed runtime advisory).
- Code: `src/canary.rs` (the behavioral canary), `src/cc_version.rs`
  (`supported_range_provenance()` — the `--version` line; the baked
  `CC_SUPPORTED_MIN`/`CC_SUPPORTED_MAX` kept honest by the ledger-drift test),
  `scripts/check-cc-version.sh` (advisory release-time provenance check),
  `build/version-compat.md` (the authoritative range ledger).
- Prior art: **ADR-0002** (keychain access via the `security` CLI, zero FFI — the
  reverse-engineered-internals model this ADR amends).
