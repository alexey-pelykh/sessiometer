# Feature: a login is single-flight, fail-closed, and the reaper honours it

Issue #1011 · PRD R-10 / R-11 / AC-2 / AC-3 · design § 5.1 SP-3 · ADR-0032 prerequisite 2

Example Mapping: 🟦 5 rules · 🟩 9 examples · 🟥 0 open

> **Read Rule 4 before touching `src/paths.rs`.** The obvious-looking fix is the wrong one and would
> weaken a credential-bearing safety mechanism.

## Rule 1 — a second login is refused, and the first is undisturbed

```gherkin
Scenario: the second login is refused
  Given a login is in flight
   When a second login is requested
   Then it is refused with a machine-readable reason
    And the first login continues unaffected

Scenario: refusal is fail-closed, not best-effort
  Given the lock cannot be acquired for any reason
   When a login is requested
   Then it is REFUSED
    # Fail-closed: an unacquirable lock must never degrade to "proceed anyway". The failure this
    # guards is two logins deleting each other's isolation dir, which is worse than no login.

Scenario: the second is refused, not queued
  Given a login is in flight
   When a second is requested
   Then it is refused immediately
    And it does NOT wait for the first to finish
    # A queued login would start ~180 s later against a roster the operator has since changed.
```

## Rule 2 — the interactive span never holds `swap.lock`

```gherkin
Scenario: the autonomous swap timer fires during a login
  Given a login is in flight, awaiting the operator's browser
   When the daemon's autonomous swap timer fires
   Then the swap proceeds normally

Scenario: the reconcile write does take the swap lock
  Given a login has completed and is harvesting
   When the roster reconcile writes
   Then it holds swap.lock for that write only
    # swap.lock guards the torn-keychain-write race. Correct for a millisecond write, catastrophic
    # for a ~180 s human-paced flow — the daemon would stop swapping for three minutes.
```

## Rule 3 — the orphan reaper must not sabotage a live login

```gherkin
Scenario: the reaper does not reap a live login's item
  Given a login is in flight and its isolated keychain item exists
   When reap_login_orphan runs
   Then it does NOT remove that item

Scenario: the reaper still reaps a genuine orphan
  Given no login is in flight
    And a crashed prior login left an isolated keychain item
   When reap_login_orphan runs
   Then it removes it
    # Both directions matter. A reaper taught to skip live logins by never reaping at all would
    # pass the first scenario and leave credential-bearing items behind forever.

Scenario: the reaper's existing precision is preserved
  Given a sibling CLAUDE_CONFIG_DIR exists
   When reap_login_orphan runs
   Then it cannot touch that sibling's item
    # src/refresh.rs:952. This property comes from the FIXED path — see Rule 4.
```

## Rule 4 — CONSTRAINT: the fixed isolated-login path must NOT change

```gherkin
Scenario: keying the login dir by an ephemeral id is rejected
  Given a proposal to make isolated_login_dir() ephemeral-id-keyed
   When it is reviewed against the reaper's targeting
   Then it is REJECTED
    # An early design note specified exactly this, and a 2026-07-31 design council judged it the
    # safer side. It is backwards. The path's hash NAMES the suffixed isolated keychain item, which
    # is how the #133 reaper targets precisely rather than by scanning. Ephemeral keying forfeits
    # that on a credential-bearing item. The concurrency defect is real; the remedy is this lock.

Scenario: there is nothing to key on anyway
  Given a fresh login capture
   When the account uuid is sought before the login completes
   Then it does not exist yet
    # src/paths.rs:225-234. The uuid is read from the isolated .claude.json AFTER completion, so an
    # ephemeral id would be synthetic rather than account-derived.
```

## Rule 5 — the lock is falsifiable

```gherkin
Scenario: removing the lock fails the concurrency test
  Given the login.lock acquisition is disabled in memory
   When two concurrent logins are attempted
   Then the test FAILS
    # A single-flight test that never actually races proves nothing. It must be able to observe the
    # unlocked failure.
```
