---
type: architecture-decision-record
number: 36
title: "An absent `config.toml` is a first run only when no prior-configuration witness survives"
date: 2026-09-03
status: accepted
decision_makers: [Oleksii PELYKH (maintainer)]
---

# ADR-0036: An absent `config.toml` is a first run only when no prior-configuration witness survives

## Status

**Accepted** — 2026-09-03. Settles decision **D-1** of
`docs/design/roster-loss-prevention-solution-design.md`, which that document records as
`proposed` and as warranting a committed ADR on merge, "because it is the load-bearing choice and
it overturns a fork a ratified PRD posed". Landed by issue **#1440**; issue **#1441** carries the
same rule to the control-socket entry point.

## Context

On 2026-08-27 `sessiometer login` found no `config.toml`, concluded first run, wrote a one-account
roster, and told a daemon that still held six to reload. Five accounts were destroyed.

Two lines carried it, a file apart and separated by one multi-minute interactive login. `login` read the
config and kept only `c.login`, dropping the roster three lines later; the reconcile then read the
config a *second* time and, on `None`, rebuilt one from `Vec::new()`. `plan_capture` has exactly
two arms — update-in-place and push — so on an empty roster the only reachable outcome is a
one-account file. The running count is taken *after* that, which is why the run reported
`(now 1 in rotation)` and nothing looked wrong.

Underneath the double read sits the real question. **An absent config is ambiguous**: it means
either *"this machine was never configured"* or *"this machine's configuration disappeared"*, and
every write path resolved it to the first, unconditionally.

`docs/requirements/roster-loss-prevention.md` R-6 states the outcome — the two cases must be told
apart before any append-only write commits — and deliberately leaves the mechanism to the design
stage, posing two routes.

## Decision

**A write verb that finds no `config.toml` consults a prior-configuration witness: durable local
state independent of both the config file and the control socket. Witness present → refuse.
Witness absent → allow.**

Two sources, either sufficient, both *observed survivors* of the incident this exists to prevent:

| Witness | Read by |
|---|---|
| any `Sessiometer/…` keychain item | `security dump-keychain` **without `-d`** — metadata only, so no prompt, no decryption, and correct against a **locked** keychain (`keychain::any_stash_item_present`) |
| a non-empty `usage-samples.jsonl` / `usage-rollup.json` | `std::fs::metadata` — polling requires rostered accounts, so a populated store implies a roster existed |

The rule and the usage-store probe live in `src/witness.rs`; the keychain probe is
`keychain::any_stash_item_present`, beside the rest of this crate's `security` handling. The
append-only CLI verbs `capture` and `login` consult the rule before any prompt, credential read or
write, and — in `capture` — ahead of `ensure_private_dir` as well, so a refusal leaves the
filesystem untouched (`src/capture.rs`). Only the `config.toml` read precedes it, because its result
is the rule's own input.

Two properties are load-bearing rather than incidental, and each has a test that fails when it
stops holding:

- **The metadata read is correct through a locked keychain.** The witness is consulted at the exact
  moment an operator has lost their `config.toml` — the moment they are least able to supply a
  keychain password. A probe that needed one would withhold the refusal precisely then, and would
  do so *permissively*.
- **The module cannot reach the control socket.** The rule has to hold with the daemon down; a
  reachable daemon may corroborate a refusal, and must never be what establishes one.

### The ratified PRD had to be amended, and was

PRD § 7 row 1 (config absent, daemon **not running**) required refusal unconditionally, while AC-4
requires a genuine first run to succeed and forbids requiring a hand-created config file. **On a
fresh machine before the daemon is ever started, both preconditions hold at once**, so row 1
forbade exactly what AC-4 mandates and no implementation could satisfy both. The witness dissolves
it: row 1 becomes *refuse if the witness is present, allow if absent*. Recorded as
`docs/requirements/roster-loss-prevention.md` § 7a — surfaced as a design-stage correction to a
ratified artifact, not applied silently.

Row 1's original rationale is preserved rather than weakened. It refused because permissiveness
there is what a socket-consulting guard would silently fall back to; the witness is read without
the socket, so the refusal holds precisely where such a guard would have degraded.

## Alternatives considered

- **Consult the control socket.** The strongest signal — the daemon held the surviving copy. But
  with no daemon running, *"no daemon"* and *"no prior roster"* are indistinguishable, so the guard
  falls back to permissive **exactly where nothing else would have noticed either**. Rejected: it
  fails PRD § 7 row 1 outright, and its failure mode is silence.
- **Refuse on an absent config alone.** No socket dependency, and already the ratified shape for a
  mutating verb (`ConfigSetRejection::NoConfig`, `src/daemon/commands.rs`). Rejected: it puts a
  refusal in front of a genuine first run, which must keep working (AC-4). The witness makes that
  tax unnecessary rather than merely acceptable.
- **Absent-config-alone plus an explicit `--first-run` affordance.** Correct, but taxes every
  genuine first run to buy nothing the witness does not already supply.
- **A sentinel file written at first configure.** A new artifact that would have to survive whatever
  removed `config.toml` — precisely the property that cannot be assumed.
- **A third *"cannot tell"* verdict** for a failed probe. Rejected: it has to resolve to refuse or
  allow at some call site, and naming it invites each caller to resolve it differently. The
  resolution is made once, in `WitnessSources::observe`, with its reasoning written down.

## Consequences

**Positive.**

- The incident is prevented at its source, with the daemon up or down.
- `FirstRunFriction` holds at its PAST baseline: with no witness there is no prompt, no added
  step, and a roster identical to what the unguarded path produces. The path is not unchanged —
  it pays the observation itself, priced under *Negative* below — but nothing an operator does or
  sees differs.
- Both probes are built on mechanisms already in the codebase: the metadata `dump-keychain` pass
  that `IsolatedService::enumerate` uses, and `std::fs::metadata`. The witness itself is new code —
  `any_stash_item_present`, `WitnessSources`, and the rule — and is tested as such.
- The double read is gone as a *shape*, not just as a symptom: `reconcile_login` now takes the
  caller's parsed config as a parameter, so there is no second read left to disagree with the
  first. `run_login`'s empty-roster constructor is now reachable only when the CALLER saw no
  config and the witness did not contradict it — a genuine first run, or the accepted false
  negative below. What it is no longer reachable from is a disagreement between two reads.

**Negative / accepted trade-offs.**

- **A loss that also takes both witnesses is a false negative.** Accepted: the roster backup ring
  (issue #1439) is the second line and does not depend on this rule.
- **A keychain probe that ERRORS counts as a witness, with a diagnostic on stderr.** It fails
  CLOSED, because only *absent* permits the write and *"cannot tell"* must not be spelled *absent*.
  The arm is reached only after the usage store has already answered negative, so there is no
  second witness left to catch what the other direction would let through. The cost is bounded by
  what else a broken probe breaks: the same `dump-keychain` backs `RealCredentialStore::resolve`,
  so an activating verb fails on such a machine either way, and a false refusal there is a less
  accurate message on a run that was already going to fail. The reverse is not symmetric —
  `login <other>` with `activate == false` never reaches `store.write`, leaving this gate as the
  only thing between a broken probe and the incident.
- **The keychain probe degrades silently rather than erroring on an unreadable keychain.** Measured
  on macOS 26.5.2 / 25F84: `security dump-keychain` exits 0 with empty output for a nonexistent
  path, a junk file, an empty file and a directory alike, and does not fall back to the login
  keychain. So that case arrives as *no items found*, indistinguishable from an empty keychain, and
  no error handling can close it. This is the sharpest form of the false negative above, and the
  usage-store half is what covers it.
- **A genuine first run pays one `security dump-keychain` and two `stat` calls.** No prompt and no
  interaction; the same call the daemon already makes at start.
- **`login` now holds the parsed config across the interactive login.** Threading the caller's
  roster through `reconcile_login` is what removes the double read, but it also widens the window
  between reading `config.toml` and writing it to include the login itself. A concurrent writer in
  that window is lost, as it was before — the window is longer, not newly unsafe. Write
  serialization is issue #1445's, and this record does not claim to have addressed it.
- **The refusal names an ambient prior configuration and nothing that indexes it** — no account
  label, no path, no count, no keychain item name. An operator therefore learns *that* the machine
  was configured, never *what* survived; the roster indexes credentials, and a refusal on stderr
  has a wider audience than the `0600` file it is about.
