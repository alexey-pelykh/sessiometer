# Feature: the isolated spawn scrubs a named list of credential-bearing env vars, including the refresh triple

Issue #1009 · PRD R-13 / AC-4 · design § 5.1 SP-1 · ADR-0032 prerequisite 1 · Rule 5: issue #1535

Example Mapping: 🟦 5 rules · 🟩 10 examples · 🟥 0 open

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

## Rule 5 — the three issue #1535 variables are scrubbed too

`build/version-compat.md` § "Issue #1535 — environment inputs Claude Code consults ahead of the
claude.ai login (CC 2.1.294)" records where each input sits, read out of the stock 2.1.294 binary.
It also lists the inputs this scrub leaves out, each with its reason. One of them,
`ANTHROPIC_BASE_URL`, stays inherited. Once the three are scrubbed, the refresh child is expected
to send the seeded token to whatever host it names, while the login gate stays open and the OAuth
builder reaches the keychain item. That is inferred from the shared builder and client, not
measured: the ledger's runtime check used an environment token, and it lists the keychain-token
path under what the read did not settle.

```gherkin
Scenario: the three issue #1535 variables are absent from the child environment
  Given the parent process carries ANTHROPIC_AUTH_TOKEN,
        CLAUDE_CODE_OAUTH_TOKEN_FILE_DESCRIPTOR and CLAUDE_CODE_API_KEY_FILE_DESCRIPTOR
   When the refresh spawn, the login spawn or the issue #783 login-shell PATH harvest
        constructs its child environment
   Then none of the three is present

Scenario: deleting a recorded credential source from the list fails by name
  Given SPAWN_ENV_REMOVE lacks one of ANTHROPIC_API_KEY, ANTHROPIC_AUTH_TOKEN,
        CLAUDE_CODE_OAUTH_TOKEN, CLAUDE_CODE_OAUTH_TOKEN_FILE_DESCRIPTOR or
        CLAUDE_CODE_API_KEY_FILE_DESCRIPTOR
   When the_recorded_env_credential_sources_are_scrubbed_by_name runs
   Then it FAILS, naming the seam and the variable
    And its message is true on every seam it can fire on
    # The five do not share one role. Three supply a credential directly: CLAUDE_CODE_OAUTH_TOKEN
    # is an OAuth token that outranks the keychain item, while ANTHROPIC_API_KEY and
    # ANTHROPIC_AUTH_TOKEN also close the login gate. The OAuth token descriptor variable supplies
    # one through the descriptor it names, a token that outranks the keychain item while that
    # descriptor is readable. The API-key descriptor variable selects and supplies: set, it closes
    # the login gate; with its descriptor readable, it is also where the API key is read from. So
    # "outranks the keychain" would be false for ANTHROPIC_API_KEY, ANTHROPIC_AUTH_TOKEN and the
    # API-key descriptor variable, which close the gate instead, and "no-ops the refresh" would be
    # false for the PATH harvest, which runs a login shell, and for the login spawn, which is not
    # a refresh. So the message says only that the variable would reach the child environment.

Scenario: the API-key descriptor variable closes the gate even with no open descriptor behind it
  Given the scrub does not remove CLAUDE_CODE_API_KEY_FILE_DESCRIPTOR
    And the parent carries it set to a value that is non-empty after trimming
    And the descriptor that value names is not open in the child
    And the parent carries neither CLAUDE_CODE_REMOTE nor a CLAUDE_CODE_ENTRYPOINT of
        claude-desktop, claude-desktop-3p or local-agent
   When a refresh child starts under Claude Code 2.1.294
   Then the login gate closes
    And the child does not use the seeded claude.ai login
    # Recorded from the static read, not from a run. The gate asks whether the variable is set,
    # non-empty after trimming, or whether its descriptor was already consumed. Whether the
    # descriptor it names is open does not enter that test, which is why the variable belongs on
    # the list even when nothing can be read from it. The CLAUDE_CODE_REMOTE and
    # CLAUDE_CODE_ENTRYPOINT conditions above are the gate's own exemption for this row, which the
    # ledger calls vr(), and the scrub removes neither variable.
```
