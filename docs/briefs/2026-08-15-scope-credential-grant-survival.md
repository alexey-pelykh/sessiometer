---
type: scope-brief
date: 2026-08-15
source: docs/requirements/credential-grant-survival.md
workflow: /scope
status: final
---

# Scope Brief: Credential Grant Survival

## What was scoped

A 13-item operator-ratified set covering the credential failures that began 2026-08-11, turned into
**11 new tracked items plus one existing issue**, organised into three gated increments.

## The problem, in one paragraph

Sessiometer exists so the operator never thinks about credentials. Between 08-11 and 08-14 it
produced about one manual `claude /login` per day: accounts refreshing unaided fell 6/6 → 2/6, and
`credential_unrecoverable` fired three times in four days on three accounts having never fired once
in the preceding 41 days. An account relogged on 08-13 was dead 16 h 07 m later **with 8 hours of
access-token life and 15 days of refresh-token life still on it** — a grant invalidated while both
tokens were live.

## Tracked items

| # | Issue | Increment |
|---|---|---|
| #1299 | capture the real sub-reason on a credential failure | **1 — instrument** |
| #1300 | measure refresh contention directly, not by counting foreign writes | **1 — instrument** |
| #1301 | the recovery loop replays with no measured bound | 2 |
| #1302 | credential health is computed from usage-poll reachability | 2 |
| #1303 | a fleet credential SLI with a push surface | 2 |
| #1304 | menubar: a revoked grant has no glance-level cue | 2 |
| #1305 | runbook: an account died — what now | 2 |
| #1306 | re-verify the refresh lifecycle and widen `CC_SUPPORTED_MAX` | 2 |
| #1307 | refresh-binary resolution churned 08-11..08-13 | 2 |
| #1308 | reconcile finding #465's verdict against death shape A | **3 — gated on #1300** |
| #1309 | is the usage-poll cadence a material 429 driver? | 3 |
| #1000 | record the #262 reuse observations (pre-existing; commented) | 2 |

## Key decisions

1. **The root cause is not known, and the scope is built to survive that.** Three hypotheses have now
   died or weakened under evidence. Increment 1 buys knowledge and changes no behaviour; Increment 2
   is correct under every surviving hypothesis and does not wait; only Increment 3 is gated.
2. **Two hypotheses were falsified during scoping, and both are recorded rather than deleted** —
   they are seductive enough to be re-derived otherwise. Claude Code's version (a second machine runs
   2.1.231, also out of range, and is healthy). The cross-machine writer (0 of 77 foreign-write
   observations within ±60 s of any machine-B write; ±300 s matches a 9% chance baseline).
3. **The intuitive contention meter is wrong, and the design says why.** Foreign-write counts are
   equal across the collapsed and healthy machines (restash 70 vs 82 — the healthier one higher),
   because sampled observation *coalesces* N rapid writes into one. #1300 measures the race directly
   by byte-comparing before and after each refresh.
4. **Throttle recovery, never terminate it.** An account rescued itself 10 h after being marked
   `unrecoverable`, via the daemon's own retry. A circuit-breaker would have stranded it.
5. **Increment 1 needs no schema bump and never crosses into Swift** — omittable log fields only.

## Readiness

**11 of 12 READY. #1308 is BLOCKED on #1300** by design — running it first would re-derive the same
untestable inference. #1309 is READY but best sequenced after #1300's data.

## Known gap

The PRD, design doc, and briefs are **staged but not committed**. Each issue's `## Build Reference`
points at them, so those pointers do not resolve on `origin/main` until they land. **Mitigated
deliberately**: every issue carries its load-bearing evidence in-band — the measurements, the
`file:line` citations, the quoted invariants — so each item is executable without dereferencing the
design. Committing closes the gap.

## Artifacts

- `docs/requirements/credential-grant-survival.md` — 22 EARS requirements, DoR passed-with-findings
- `docs/design/credential-grant-survival-solution-design.md` — 3 increments, 7 ADRs, both coverage
  matrices green, `status: draft` (one load-bearing open question)
- `docs/briefs/2026-08-14-requirements-credential-grant-survival.md`
- `docs/briefs/2026-08-15-design-credential-grant-survival.md`
