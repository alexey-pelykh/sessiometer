---
type: design-brief
date: 2026-08-15
source: docs/design/credential-grant-survival-solution-design.md
workflow: /design-solution
status: draft
---

# Design Brief: Credential Grant Survival

## Problem

Since 2026-08-11 the daemon has been producing about one manual `claude /login` per day — the exact
toil it exists to remove. Accounts refreshing unaided fell from 6/6 to 2/6, and a credential relogged
on 08-13 was dead 16 h later with 8 h of access-token life still on it. **The root cause is not
known**: two hypotheses have already died under evidence, and a third was weakened by evidence
gathered during this design.

## Key Decisions

1. **The first increment is an instrument, not a fix** — because a design that encodes an unproven
   cause becomes the fourth casualty. R-1 (capture the failure's real sub-reason) and R-4 (measure
   contention) change no behaviour and buy the knowledge everything else needs.
2. **Measure the race directly, not by counting foreign writes** — the intuitive meter is
   *falsified*: foreign-write counts are equal across the healthy and the collapsing machine
   (restash 70 vs 82), because the daemon *samples* the canonical item and N rapid writes coalesce
   into one observation. Instead, read the refresh-token bytes before and after each refresh: a
   change inside that window **is** the hazard, and coalescing cannot hide it.
3. **The two 401s are different events and must stay apart.** A usage-API 401 means the access token
   was rejected; a token-endpoint `invalid_grant` means the grant is gone. The codebase already says
   so (`Viability::Quarantined` in `src/use_account.rs`). Conflating them is why shapes A and B are indistinguishable today.
4. **Pipe and classify `claude -p` stderr** — currently nulled at the spawn site, which is why the
   token-endpoint error is invisible. Classified at the boundary against an allowlist, reduced to an
   enum; the raw buffer is never returned, stored, or logged.
5. **`Unobservable` is a first-class value.** If the sub-reason cannot be seen, the system says so
   rather than inferring one. That is R-3's prohibition made structural.
6. **Throttle recovery, never terminate it** — `.eu` was rescued by the daemon's own retry 10 h after
   being marked unrecoverable. A hard circuit-breaker would have stranded it.
7. **Increment 1 needs no schema bump and never crosses into Swift** — new log fields are omittable
   via `skip_serializing_if`, the one condition under which additive means no bump. That is what
   makes it genuinely small.
8. **Increment 2 does not wait.** Retry throttling, health truthfulness, the fleet SLI, and the
   runbook are correct under every surviving hypothesis and ship in parallel.

## Design Tracks

| Track | Approach | Key Trade-off |
|---|---|---|
| Technical | Classified-cause type + two capture sites + direct contention detection | Piping stderr widens what the daemon sees |
| Security | Classify-at-boundary, allowlist not denylist, negative test fixture | Containment by construction rather than by procedure |
| Wire/Schema | Omittable fields only in Increment 1 | Defers the `STATUS_SCHEMA_VERSION` bump (and Swift sweep) to Increment 2 |
| UX | Reuse notification presenter and #923's surface | Additive; no panel re-layout |
| Testing | Unit-dominant; classifier is a pure function over fixtures | The token-non-appearance fixture is the security oracle |

## Open Questions

- **Does Claude Code surface a distinguishable token-endpoint error on stderr?**
  Context: `claude -p`'s output is nulled today, so this has never been observed. It decides whether
  R-1(b) is buildable or collapses into R-3's negative finding.
  Cheapest test: force one failed refresh with stderr piped, inspect once.
  Impact if deferred: Increment 1 ships at roughly half its value — the usage-API half still lands.
  **Load-bearing → this brief is `status: draft`, not `final`.**

## Lock status

**Not locked, deliberately.** Two conditions are unmet: the load-bearing open question above, and
dual-lens ratification (product + UX) which has not been performed. Both coverage matrices are green
— no UNCOVERED requirements, no PHANTOM elements.

## Full Design

See [credential-grant-survival-solution-design.md](../design/credential-grant-survival-solution-design.md).
