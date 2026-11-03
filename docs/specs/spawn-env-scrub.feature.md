# Feature: the isolated spawn scrubs a named list of env vars that decide the child's credential or where it goes, including the refresh triple

Issue #1009 · PRD R-13 / AC-4 · design § 5.1 SP-1 · ADR-0032 prerequisite 1 · Rule 5: issue #1535 · Rule 6: issue #1625

Example Mapping: 🟦 6 rules · 🟩 13 examples · 🟥 0 open

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

## Rule 5 — the issue #1535 variables are scrubbed too

```gherkin
# build/version-compat.md § "Issue #1535 — environment inputs Claude Code consults ahead of the
# claude.ai login (CC 2.1.294)" records where each input sits, read out of the stock 2.1.294
# binary, and the inputs this scrub leaves out, each with its reason.
#
# In Rules 5 and 6, a seam is a scrub seam: a place that builds a child environment through
# SPAWN_ENV_REMOVE. The scrub seams are the refresh spawn, the login spawn and the issue #783
# login-shell PATH harvest. Rule 2's seam is the one shared spawn scaffold instead.

Scenario: the issue #1535 variables are absent from the child environment
  Given the parent process carries ANTHROPIC_AUTH_TOKEN,
        CLAUDE_CODE_OAUTH_TOKEN_FILE_DESCRIPTOR and CLAUDE_CODE_API_KEY_FILE_DESCRIPTOR
   When the refresh spawn, the login spawn or the issue #783 login-shell PATH harvest
        constructs its child environment
   Then none of them is present
    # Automated: the_recorded_env_credential_sources_are_scrubbed_by_name,
    # all_parametrizations_apply_the_full_scrub_set and refresh_plan_builds_the_legacy_command.
    # They assert the removal recorded on each built command, not on a spawned child.

Scenario: deleting a recorded credential source from the list fails by name
  Given SPAWN_ENV_REMOVE lacks one of ANTHROPIC_API_KEY, ANTHROPIC_AUTH_TOKEN,
        CLAUDE_CODE_OAUTH_TOKEN, CLAUDE_CODE_OAUTH_TOKEN_FILE_DESCRIPTOR or
        CLAUDE_CODE_API_KEY_FILE_DESCRIPTOR
   When the_recorded_env_credential_sources_are_scrubbed_by_name runs
   Then it FAILS, naming the seam and the variable
    And its message is true on every seam it can fire on
    # These inputs do not share one role. CLAUDE_CODE_OAUTH_TOKEN supplies an OAuth token that
    # outranks the keychain item. ANTHROPIC_API_KEY and ANTHROPIC_AUTH_TOKEN supply a credential
    # and can also close the login gate. CLAUDE_CODE_OAUTH_TOKEN_FILE_DESCRIPTOR supplies a token
    # through the descriptor it names, and that token outranks the keychain item while the
    # descriptor is readable and no access token from the CLAUDE_BG_AUTH_SNAPSHOT_PATH snapshot
    # was consumed first.
    # CLAUDE_CODE_API_KEY_FILE_DESCRIPTOR selects and supplies: set, it can close the login gate;
    # with its descriptor readable, it is also where the API key is read from.
    # So "outranks the keychain" would be false for ANTHROPIC_API_KEY, ANTHROPIC_AUTH_TOKEN
    # and CLAUDE_CODE_API_KEY_FILE_DESCRIPTOR, which can close the gate instead.
    # "Skips the refresh" would be false at the PATH harvest, which runs a login shell, and at the
    # login spawn, which is not a refresh. At the refresh spawn, the ledger's 2026-11-02T05:46:18Z
    # run saw a token-host CONNECT still attempted under each of ANTHROPIC_API_KEY,
    # ANTHROPIC_AUTH_TOKEN and an unopened CLAUDE_CODE_API_KEY_FILE_DESCRIPTOR. So the message says
    # only that the variable is not recorded as removed from the environment the seam builds.

Scenario: CLAUDE_CODE_API_KEY_FILE_DESCRIPTOR closes the gate even with no open descriptor behind it
  Given the scrub does not remove CLAUDE_CODE_API_KEY_FILE_DESCRIPTOR
    And the parent carries it set to a value that is non-empty after trimming
    And the descriptor that value names is not open in the child
    And the parent carries neither CLAUDE_CODE_REMOTE nor a CLAUDE_CODE_ENTRYPOINT of
        claude-desktop, claude-desktop-3p or local-agent
   When a refresh child starts under Claude Code 2.1.294
   Then the login gate closes
    And the API clients Claude Code's client builder makes do not carry the seeded claude.ai login
    # The gate reading is static. The ledger's 2026-11-02T05:46:18Z run matches it: with nothing
    # open on the descriptor, the child exited 1 printing "Not logged in · Please run /login",
    # where the control arm ran until its timeout. That run recorded hosts, not headers. The gate
    # asks whether the variable is set to a value non-empty after trimming, or whether its
    # descriptor was already consumed. Whether the descriptor it names is open does not enter
    # that test, which is why the variable belongs on the list even when nothing can be read
    # from it. The CLAUDE_CODE_REMOTE and CLAUDE_CODE_ENTRYPOINT conditions above are the
    # gate's own exemption for this row. The ledger calls that exemption vr(). That is a minified
    # 2.1.294 name, and the ledger's Names row records that such names change between builds. The
    # scrub removes neither variable.
```

## Rule 6 — the API base URL and its companion custom headers are scrubbed

```gherkin
# build/version-compat.md § "Issue #1535 — environment inputs Claude Code consults ahead of the
# claude.ai login (CC 2.1.294)" has a subsection, "`ANTHROPIC_BASE_URL` and
# `ANTHROPIC_CUSTOM_HEADERS`, added under issue #1625". It records why each is on the list, the
# 2026-11-02 measurements, and why _CLAUDE_CODE_ASSUME_FIRST_PARTY_BASE_URL stays off it.

Scenario: the base URL and the custom headers are absent from the child environment
  Given the parent process carries ANTHROPIC_BASE_URL and ANTHROPIC_CUSTOM_HEADERS
   When the refresh spawn, the login spawn or the issue #783 login-shell PATH harvest
        constructs its child environment
   Then neither is present
    # Automated: the_base_url_and_its_companion_headers_are_scrubbed_by_name,
    # all_parametrizations_apply_the_full_scrub_set and refresh_plan_builds_the_legacy_command.
    # They assert the removal recorded on each built command, not on a spawned child. The rest of
    # this comment is a static read of Claude Code 2.1.294, except the usage-poll sentence, which
    # reads src/usage.rs. No test checks those reads.
    # Claude Code lists two companions for the base URL. Only one of them, ANTHROPIC_CUSTOM_HEADERS,
    # is scrubbed; the other, _CLAUDE_CODE_ASSUME_FIRST_PARTY_BASE_URL, stays inherited. The custom
    # headers are also a supplier: a header the variable names replaces an earlier header of the
    # same name, so on Claude Code's first-party API client a custom Authorization header replaces
    # the bearer that client built. The ledger records when Claude Code writes its own
    # Authorization over it, and which other client branches drop or replace it.
    # Removing the base URL alone would send a gateway's headers, an Authorization value or a key
    # header, to api.anthropic.com, so the base URL and the custom headers are scrubbed together.
    # Removing the base URL changes where the refresh child's API requests go. It leaves
    # sessiometer's usage poll alone: the poll calls its own fixed URL (src/usage.rs). It does not
    # move the token endpoint the refresh posts to. In one inherited setup it can change whether
    # that refresh runs:
    # CLAUDE_CODE_USE_GATEWAY with a gateway token from CLAUDE_CODE_GATEWAY_TOKEN_FILE_DESCRIPTOR
    # or the snapshot. There an inherited base URL can make Claude Code pick its gateway provider
    # and skip the refresh as not needed. The ledger records that chain and the parts it did not
    # trace.
    # Whether the token-refresh request carries the headers ANTHROPIC_CUSTOM_HEADERS names was not
    # settled. The ledger lists it as open.

Scenario: deleting either from the list fails by name
  Given SPAWN_ENV_REMOVE lacks ANTHROPIC_BASE_URL or ANTHROPIC_CUSTOM_HEADERS
   When the_base_url_and_its_companion_headers_are_scrubbed_by_name runs
   Then it FAILS, naming the seam and the variable
    And its message is true on every seam it can fire on
    # The message says only that the variable is not recorded as removed from the environment
    # the seam builds. The PATH harvest runs a login shell, not claude, so a message naming the
    # host a credential reaches would be false there.

Scenario: the refresh child sends its stored token as the bearer to an inherited base URL
  Given the scrub does not remove ANTHROPIC_BASE_URL
    And the parent carries ANTHROPIC_BASE_URL pointing at another host
    And the parent carries no credential input
    And the parent carries no gate-closer from the ledger's gate table
    And the parent does not carry CLAUDE_CODE_PROVIDER_MANAGED_BY_HOST
    And the child's isolated config dir holds a stored token and no stored gateway credential
    And that token is unexpired, carries the user:inference scope and has no refresh token
   When a refresh child starts under Claude Code 2.1.294
   Then the API clients Claude Code's client builder makes send their requests to that host
    And each carries the stored token as its Authorization: Bearer header
    # The ledger records why the outcome needs no inherited gate-closer and no inherited
    # CLAUDE_CODE_PROVIDER_MANAGED_BY_HOST. A gate-closer stops the API clients Claude Code's
    # client builder makes from using the login. That variable stops the builder before it reads
    # the keychain item.
    # The outcome also assumes no managed provider policy, and a host without the well-known token
    # files. The ledger lists those files under § Left out by the ratified scope.
    # Measured at 2026-11-02T05:14:47Z with a dummy stored token, the refresh seam's environment
    # and the base URL at a loopback listener. The token's expiresAt was ten years ahead, its
    # scopes were user:inference and user:profile, and it had no refresh token. Two
    # POST /v1/messages?beta=true reached the listener, each with the dummy as the bearer and no
    # x-api-key. The store was the plaintext credentials file; the macOS keychain read was not
    # exercised. The static read agrees. On its first-party client, the client builder takes one
    # bearer, whatever its source. Without a managed provider policy, no host test sits between
    # choosing it and the base URL.
    # The refresh seam seeds a back-dated expiresAt instead, and Claude Code runs its refresh
    # check before it reads the bearer. After a refresh that lands, the bearer would be the newly
    # issued access token. That case is a static read; no run measured it.
```
