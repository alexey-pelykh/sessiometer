# Feature: the isolated spawn scrubs every credential-bearing env var, including the refresh triple

Issue #1009 · PRD R-13 / AC-4 · design § 5.1 SP-1 · ADR-0032 prerequisite 1

Example Mapping: 🟦 4 rules · 🟩 7 examples · 🟥 0 open

> **Why this spec exists at all.** The defect's refresh-token half is currently **inert** — under
> today's argv (`&["/login"]`) an inherited refresh token changes nothing, though an inherited
> client id already reaches the login child's OAuth requests. That half becomes a wrong-account
> credential write the moment #1020 lands. A spec written only against observable behaviour today
> would be thin, so these scenarios are written against the argv the project is moving to.

## Rule 1 — the three refresh variables are scrubbed

```gherkin
Scenario: the refresh triple is absent from the child environment
  Given the parent process carries CLAUDE_CODE_OAUTH_REFRESH_TOKEN,
        CLAUDE_CODE_OAUTH_SCOPES and CLAUDE_CODE_OAUTH_CLIENT_ID
   When the isolated spawn seam constructs the child environment
   Then none of the three is present

Scenario: the three already-scrubbed variables stay scrubbed
  Given the parent carries CLAUDE_CODE_OAUTH_TOKEN, ANTHROPIC_API_KEY
        and CLAUDE_SECURESTORAGE_CONFIG_DIR
   When the isolated spawn seam constructs the child environment
   Then none of the three is present
    # Regression guard. The change is an addition to SPAWN_ENV_REMOVE (src/isolated_spawn.rs),
    # and an addition is exactly the edit that can drop a neighbour.
```

## Rule 2 — the scrub is asserted at the seam, not per call site

```gherkin
Scenario: both spawn plans share one scrub
  Given the login plan and the refresh plan
   When each constructs its child environment
   Then both have the full SPAWN_ENV_REMOVE set removed
    # SpawnPlan::login's doc comment in src/isolated_spawn.rs records that the login plan is
    # built specifically to prove the scrub applies to it too. One seam, one scrub — a second
    # copy would drift.
```

## Rule 3 — CONSTRAINT: the gate must be falsifiable

```gherkin
Scenario: removing an entry from the scrub list fails the test
  Given CLAUDE_CODE_OAUTH_REFRESH_TOKEN is removed from SPAWN_ENV_REMOVE in memory
   When the scrub test runs
   Then it FAILS, naming the variable
    # A test asserting "these are absent" passes trivially when the parent never set them.
    # The test must set them in the parent first, or it proves nothing.
```

## Rule 4 — the behavioural consequence, under the target argv

```gherkin
Scenario: an inherited refresh token does not short-circuit the browser
  Given the argv is ["auth", "login", "--claudeai"]
    And the daemon's environment carries a valid CLAUDE_CODE_OAUTH_REFRESH_TOKEN
   When a login spawn runs
   Then a browser flow is still required
    And no credential is written from the inherited token

Scenario: the failure this prevents, stated so it is not re-discovered
  Given the scrub does NOT include the refresh triple
    And the argv is ["auth", "login", "--claudeai"]
    And the environment carries a refresh token for account A
   When the operator starts a login intending to add account B
   Then Claude Code short-circuits, writes A's credential and exits 0
    And every existing safety check passes
    And the operator has silently captured the WRONG account
    # This is the whole reason #1009 blocks #1020 rather than shipping alongside it. Nothing is
    # malformed on this path — the capture succeeds, it just captures someone else.

Scenario: ordering is part of the requirement
  Given #1020 has changed the argv
    And #1009 has not landed
   Then the vulnerability window is open
    # Recorded as a scenario deliberately: "land these together" reads as satisfied by one PR, and
    # a reviewer splitting that PR would not know the order was load-bearing.
```
