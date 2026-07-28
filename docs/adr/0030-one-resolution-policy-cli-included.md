---
type: architecture-decision-record
number: 30
title: "One `claude`-resolution policy for every caller — `poke` / `login` resolve via the login-shell PATH too"
date: 2026-07-28
status: accepted
decision_makers: [Oleksii PELYKH (maintainer)]
---

# ADR-0030: One `claude`-resolution policy for every caller — `poke` / `login` resolve via the login-shell PATH too

## Status

**Accepted** — 2026-07-28. Records a decision to **keep the current behaviour** and document
it where a reader meets it (issue #802, filed deliberately as a question rather than a bug).
Same posture as ADR-0029: a decision in force plus documentation, not a code change.

## Context

Issue #784 (PR #799, commit `5222fdc`) rewired resolution tier 3 from *the process's
inherited `$PATH`* to *the harvested login-shell `PATH`*. It was scoped as a **daemon** fix —
under `launchd` the daemon inherits a bare `PATH=/usr/bin:/bin:/usr/sbin:/sbin`, which holds
no `claude` at all, so every automatic refresh failed to resolve one.

But `poke` and `login` share that resolver. `poke` calls `paths::claude_binary()`
(`src/poke.rs`); `login` calls `paths::claude_binary_with_override()` (`src/login.rs`); both
funnel into the same `claude_binary_ambient` → `claude_binary_tiered` ladder as the daemon's
refresh tick and keep-warm engine. So the change applied to them too, and nobody wrote that
down.

The observable consequence, in a terminal:

```sh
PATH=/custom/bin:$PATH sessiometer poke   # /custom/bin/claude is IGNORED
```

A successful harvest **replaces** the inherited `$PATH` rather than unioning with it — a
union would let a `launchd`-inherited entry outrank the user's own, defeating the shadowing
guarantee #784 AC2 exists to provide (`tier3_path`, `src/paths.rs`). In a terminal the
harvest always succeeds, so a shell-local `PATH` prefix is never consulted.

The #784 executor surfaced this rather than absorbing it silently, and it was merged as-is
because the severity is low: `CLAUDE_BIN=/custom/bin/claude sessiometer poke` is a
documented, trivially-available escape hatch with identical effect. What remained open was
not a defect but an **unrecorded decision** — and an undocumented surprise.

Two properties are in genuine tension here, which is why this warranted a decision rather
than a fix:

**For the current behaviour** — `poke` becomes a *faithful probe of what the daemon will
resolve*. That has real diagnostic value, and its absence is precisely what made the
originating outage confusing: `poke` succeeded while the daemon failed, for hours, because
the two resolved different binaries from different environments. The
`🟠 degraded — run 'sessiometer poke'` cue worked *only* because the CLI had a `PATH` the
daemon lacked — which pointed every diagnosis at credentials instead of at the environment.
Under the current behaviour that divergence cannot recur.

**Against it** — the harvest exists to *reconstruct what the daemon lacks*. A CLI invoked
from an interactive shell already has the real thing, so for that case the change substitutes
reconstructed information for authentic information, which is strictly worse. It is also
surprising: a CLI that ignores its own `$PATH` violates near-universal UNIX expectation.

## Decision

**One resolver, one answer. `poke` and `login` keep resolving `claude` through the harvested
login-shell `PATH`, exactly as the daemon does — and the delta is documented rather than
left to be discovered.** Three parts:

1. **The behaviour is unchanged.** No tier is added, removed or reordered; no tier is scoped
   by caller. The ladder stays `[refresh]`/`[login].claude_bin` → `$CLAUDE_BIN` → harvested
   login-shell `PATH`, with a successful harvest replacing (never unioning with) the
   inherited `$PATH` and a failed harvest degrading to it.
2. **The delta is documented where the question arises** — `README.md`, next to the existing
   `claude_bin` documentation, with the worked `PATH=/custom/bin:$PATH` example and
   `$CLAUDE_BIN` named as the per-invocation override. The two places that previously said
   `poke` / `login` need `claude` "on your `PATH`" now say *which* `PATH` and point at the
   full explanation, because those are the sentences a surprised reader reaches first.
3. **The behaviour is pinned by a discriminating test**, so it cannot drift back unnoticed
   (`src/paths.rs`, the issue-#802 section). A test that merely asserted "resolution works"
   would pass under every alternative below and would therefore pin nothing.

Deliberately **not** done: nothing in the resolution chain is touched. Any change to which
binary gets resolved, in any environment, would be Alternative 1 or 2 below.

Issue #802's **AC3 does not apply** under this decision, and is recorded as such rather than
left unmentioned: it is conditioned on Alternative 1 or 2 being chosen ("*If (b) or (c): the
shadowing guarantee and the daemon/CLI parity property are each re-stated with whatever
weakening the choice implies*"). Neither was, so neither property is weakened — and both are
re-stated **unweakened** under § Consequences → Positive rather than silently dropped.

## Alternatives considered

1. **Try the inherited `$PATH` first; harvest only on a miss.** In a terminal the inherited
   `PATH` resolves immediately, so a shell-local override is honoured and no shell is spawned
   (also faster); under `launchd` the stub `PATH` misses and the harvest runs.
   - **Pros**: restores the universal UNIX expectation for the CLI; strictly faster on the
     interactive path; uses authentic information where authentic information exists.
   - **Cons**: it trades away the shadowing guarantee #784 exists to provide. Under
     `launchd`, `/usr/bin:/bin:/usr/sbin:/sbin` is consulted *first* — so a `claude` in
     `/usr/bin` would outrank the user's own `~/.local/bin/claude`, which is exactly the
     ranking the harvest was built to prevent. It also reintroduces the daemon-vs-CLI
     divergence that made the original outage hard to see: `poke` would once again answer a
     different question than the daemon.
   - **Why rejected**: it gives up a guarantee just built, for an edge case `$CLAUDE_BIN`
     already covers completely.
2. **Scope tier 3 by caller** — daemon paths harvest; CLI verbs use the inherited `$PATH`.
   - **Pros**: most faithful to each context's intent — the daemon reconstructs what it
     lacks, the CLI uses what it genuinely has. It is the only option that removes the
     surprise without weakening the daemon's shadowing guarantee at all.
   - **Cons**: it deliberately makes `poke` stop predicting daemon behaviour, which is the
     property that gives `poke` its diagnostic value and whose absence caused the original
     multi-hour misdiagnosis. It also splits one resolution policy into two — the thing
     #784's design notes explicitly set out to avoid — so every future change to resolution
     has to be made, reasoned about, and tested twice, with the two halves free to drift.
   - **Why rejected**: the divergence it re-opens is the incident, and a second policy is a
     standing maintenance cost paid on every subsequent change to this ladder.
3. **Change nothing and document nothing.**
   - **Why rejected**: the surprise is real and cheap to remove. Leaving it undocumented
     means the next operator re-derives it the hard way — and the README actively said
     `poke` needs `claude` "on your `PATH`", which had become misleading.

## Consequences

### Positive

- **`poke` predicts the daemon.** One resolver means `sessiometer poke` resolves the same
  binary the daemon's next refresh cycle will. The CLI-succeeds-while-daemon-fails
  divergence that made the originating outage a multi-hour misdiagnosis cannot recur.
- **The shadowing guarantee holds everywhere.** A `claude` the user deliberately shadows
  earlier on their own `PATH` is the one spawned, in every caller, in every environment
  (#784 AC2).
- **One policy to change, one policy to test.** A future change to the ladder is made once
  and verified once; there is no second half to keep in step.
- **The surprise is discoverable rather than latent.** A reader hits the explanation at the
  point the question occurs to them, with a worked example and the override spelled out.
- **Zero regression risk.** No production code changed — a record, documentation, and tests.

### Negative / trade-offs

- **A CLI that ignores its own `$PATH` remains surprising**, and no amount of documentation
  makes it unsurprising to someone who does not read it first. This is accepted, not solved.
  `$CLAUDE_BIN=/custom/bin/claude` is the escape hatch, and it is now documented at both
  places the question arises.
- **`poke` and `login` pay a login-shell spawn (~38 ms, memoized 60 s) that an interactive
  invocation does not strictly need**, and they substitute reconstructed information for the
  authentic `$PATH` they already had. Accepted deliberately: fidelity to what the daemon
  will do is the property being bought, and the cost is bounded by
  `LOGIN_SHELL_HARVEST_TIMEOUT` and skipped entirely when either override is set.
- **A user whose login shell is slow, prompts, or is broken pays that cost on every CLI
  invocation** where they have not set an override — the harvest failure degrades to the
  inherited `$PATH` rather than erroring, so the verb still works, but it works *after* the
  bound elapses. `claude_bin` / `$CLAUDE_BIN` skips the harvest entirely and is the
  documented remedy.
- **The tests that pin the CLI half are partly source-anchored, and deliberately layered.**
  The behavioural cases (`cli1` / `cli2`) inject at `claude_binary_ambient`, which sits
  *below* `claude_binary` / `claude_binary_with_override`; the call-site guard binds on
  `poke.rs` / `login.rs`, *above* them. That brackets the entry points without covering
  them — so a caller-scoped tier planted *inside* `claude_binary` (one function, one
  production caller: `poke`) is the shortest route to Alternative 2 and leaves both legs
  green. It was reachable by mutation, not hypothetically. A third guard therefore pins each
  entry point as a **bare delegation** — its body must be nothing but the delegating
  expression — closing the bracket at the layer the other two leave open.
  Source-anchored rather than behavioural for the reason
  `t19_the_resolver_introduces_no_platform_conditional` is: reaching those entry points
  behaviourally means spawning the running developer's own login shell and scanning their
  real `~/.local/bin/claude`, precisely the machine-specific state the suite stages away.
  The residual limitation is real — a refactor that legitimately restructures either entry
  point must re-anchor the guard, and an equality assertion is stricter than the property it
  stands in for.

## Related

- **Issue #802**: this decision (branch **(a)**, "leave it, document the delta"), filed as a
  question with all three branches costed.
- **Issue #784** (PR #799, commit `5222fdc`): the change that produced the delta — tier 3
  rewired from the inherited `$PATH` to the harvested login-shell `PATH`. Its **AC2** is the
  shadowing guarantee this ADR declines to weaken.
- **Issue #783** (PR #796): built the harvest capability (`harvest_login_shell_path`) that
  #784 wired in as tier 3.
- **Issue #785** (PR #809): the launchd re-exec regression harness in `src/paths.rs` that the
  issue-#802 CLI cases extend — same mechanism, opposite environment (a rich terminal `PATH`
  rather than the bare `launchd` one).
- **Issue #375** (per-cycle binary resolution) and **issue #101** (no symlink
  canonicalization): both preserved, and both re-verified as preserved by this change —
  `t15` and the 60 s memo/TTL split (the `PATH` string is memoized, never the resolution).
- **ADR-0029**: the immediately prior decision, and the posture this one follows — a
  `question` issue resolved by recording the existing behaviour and stating it plainly
  rather than by changing code.
- **Code**: `src/paths.rs` (`claude_binary`, `claude_binary_with_override`,
  `claude_binary_ambient`, `claude_binary_tiered`, `tier3_path`, and the issue-#802 test
  section), `src/poke.rs` (`poke` — the `paths::claude_binary()` call site), `src/login.rs`
  (`login` — the `paths::claude_binary_with_override()` call site), `README.md`.
