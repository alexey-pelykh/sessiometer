# Feature: `login` routes over the control socket as a start-and-observe pair

Issue #1021 · PRD R-7 / R-8 / R-9 / R-12 / AC-1 / AC-5 · design § 5.2, § 6 · ADR-0032

Example Mapping: 🟦 6 rules · 🟩 11 examples · 🟥 0 open

> The only novel problem here is **duration**. Every other socket command completes in milliseconds;
> a login is ~180 s. Everything else follows the shipped routed-mutation template.

## Rule 1 — `login` starts and returns; it does not block

```gherkin
Scenario: the ack is immediate
  Given no login is in flight
   When an authenticated same-user peer sends {"cmd":"login","label":"work"}
   Then the daemon replies immediately with an accepted ack
    And the login continues in the background

Scenario: the run loop is not blocked for the duration
  Given a login is in flight
   When the daemon's poll tick is due
   Then it fires on schedule
    # ADR-0001: the daemon is a single-threaded current_thread runtime. Blocking work is spawned
    # off the run loop — the shipped stats / config-get precedent. Blocking here would stop
    # polling AND stop swapping for three minutes.
```

## Rule 2 — auth posture matches the other state-affecting commands

```gherkin
Scenario: an unauthenticated peer cannot start a login
  Given an unauthenticated peer
   When it sends {"cmd":"login"}
   Then it receives {"error":"unauthorized"}
    And NO login is started
    # Assert the absence, not just the error string. An error reply with a login started anyway is
    # the failure this scenario exists to catch.

Scenario: login-status is also gated
  Given an unauthenticated peer
   When it sends {"cmd":"login-status"}
   Then it receives {"error":"unauthorized"}
    # Deliberately NOT modelled on the un-gated reads (status / watch / stats). Auth-flow state is
    # readable only by a peer that could start one.
```

## Rule 3 — nothing secret crosses to the client, on any path

```gherkin
Scenario: the success ack is redacted
  Given a login completes
   When the ack is returned
   Then it carries a label and an outcome
    And no credential, token, or authorization code

Scenario: the failure reason is a machine cause, not passthrough text
  Given a login fails inside Claude Code
   When login-status reports it
   Then the reason is a machine-readable cause
    And it is NOT the child's raw error output
    # Passthrough error text is the leak path nobody audits — a token fragment in an upstream
    # message would cross to the client through a field everyone reads as "just a string".

Scenario: the label is non-secret and is not argv
  Given the operator supplies a label
   When the child is spawned
   Then the label does not appear in the child's argv
    # src/isolated_spawn.rs:111 — argv stays &'static [&'static str]. That compile-time
    # no-injection guarantee is preserved precisely because the label travels elsewhere.
```

## Rule 4 — observation without re-issuing

```gherkin
Scenario: phase is readable while in flight
  Given a login is awaiting the operator's browser
   When the panel sends {"cmd":"login-status"}
   Then it receives phase "awaiting-browser"
    And no second login is started
    # This is why the pair exists. Without a separate read, the only way to learn the outcome is to
    # re-issue login — which #1011 refuses. The panel would be unable to observe its own operation.

Scenario: terminal phases are distinguishable
  Given a login has ended
   When login-status is read
   Then the phase is exactly one of completed, cancelled, timed-out, failed
    # The operator's next action differs by cause, so a single "failed" is not sufficient.
```

## Rule 5 — completion reconciles via the shipped signal path

```gherkin
Scenario: success raises roster-reload
  Given a login completes and the credential is harvested
   When the stash write finishes
   Then the existing roster-reload signal (#139) is raised
    And the roster reconciles
    # Reuse, not new code. The reconcile path is shipped machinery.
```

## Rule 6 — CONSTRAINT: the shared credential is untouched

```gherkin
Scenario: the canonical item is byte-for-byte unchanged
  Given the shared "Claude Code-credentials" item is hashed before a routed login
   When the login completes and is harvested
   Then the item hashes identically
    # PRD AC-5. The invariant the entire isolated-login design exists to protect, and routing must
    # not weaken it. Assert by hash, not by inspection.

Scenario: it is unchanged on the failure path too
  Given a routed login fails or is cancelled
   Then the shared item still hashes identically
    # The success path is the one people test. A teardown that clobbers on the error path would
    # pass a success-only assertion.
```
