# Feature: a rejection the daemon can send is a rejection every surface can render

Issue #1441 · PRD R-2 / R-12 · design D-1 / D-5

Example Mapping: 🟦 3 rules · 🟩 8 examples · 🟥 0 open

> `perform_socket_capture`'s roster `save_to` (`src/daemon/commands.rs:300`) reaches the same
> empty-roster
> fall-through as the CLI, so the menu-bar capture button reproduces the roster collapse. Fixing it
> means adding a rejection reason — and `CaptureRejection` in
> `apps/menubar/Sources/CaptureAck.swift` is a **closed four-tag enum** whose decoder throws on an
> unrecognized tag (line 101). A new Rust tag does not degrade in the app; it breaks the button.
>
> **The trap here is the gate, not the code.** The natural place to assert cross-surface agreement
> is the panel-appearance check — and `panel-goldens` is a deliberately soft gate whose every step
> is `continue-on-error`. It always reports pass. An assertion placed there can never fail, so it
> would certify the parity it never checked. Rule 3 exists to make that unavailable.

## Rule 1 — the second entry point refuses on the same rule as the first

```gherkin
Scenario: the menu-bar button cannot reproduce the collapse
  Given no config.toml
    And a prior-configuration witness is present
    And the daemon holds six accounts
   When capture is invoked over the control socket
   Then it is rejected
    And the daemon still holds six accounts
    # Same witness rule as issue #1440, applied at commands.rs:300. One rule, two entry points —
    # not two rules.

Scenario: the menu-bar first run still works
  Given no config.toml
    And no witness of any kind
   When capture is invoked over the control socket
   Then it succeeds and the roster contains one account
    # The GUI first run is a real path and must not be the price of the fix.
```

## Rule 2 — the vocabulary is closed, so it moves as one

```gherkin
Scenario: every Rust tag has a Swift case
  Given the set of rejection reasons the daemon can emit
   Then each has a corresponding CaptureRejection case
    # The four today are no-active-account, keychain-locked, swap-lock-busy, failed.

Scenario: the new tag decodes rather than throws
  Given the daemon sends the new refusal tag
   When CaptureAck decodes the reply
   Then it yields a known rejection
    And DecodeError.unrecognized is not thrown
    # CaptureAck.swift:101 fails closed. That is correct behaviour — it is why the two sides must
    # move in one change rather than being allowed to drift and degrade.

Scenario: the tag carries no operator data
  Given a rejection is sent over the socket
   Then it carries a redacted machine code
    And no path, account label or count
    # The panel is a more public surface than a terminal.
```

## Rule 3 — the assertion lives where it can fail

```gherkin
Scenario: divergence fails a hard gate
  Given a rejection reason exists in Rust with no CaptureRejection case
   When CI runs
   Then the test job fails
    # Not panel-goldens: every step there is continue-on-error, so it always reports pass and can
    # never tell you the vocabulary drifted. `ci-ok` is the only required check, and it counts a
    # skipped job as a pass — a soft or filtered gate is indistinguishable from one that ran clean.

Scenario: the panel state is checked against the design reference
  Given the panel renders the refusal state
   Then it is compared against apps/menubar/design/menubar-preview.html
    And any intended divergence is recorded under Expected reconciliations
    # The mock is the canonical visual reference for the capture states. It is the oracle only for
    # what it authors, and it can be stale — a recorded reconciliation, not silent drift.

Scenario: the golden gate is read off its drift line
  Given the panel goldens are run armed
   Then the result is read from the [panel-goldens] max drift line
    # TEST_RUNNER_SESSIOMETER_PANEL_GOLDEN_GATE=1; a bare SESSIOMETER_ stops at xcodebuild. "Executed
    # 2 tests, with 2 tests skipped" plus ** TEST SUCCEEDED ** means nothing was compared.
```
