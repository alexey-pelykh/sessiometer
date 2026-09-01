# Feature: a bad write can never displace a good backup

Issue #1439 · PRD R-8 / R-9 · design D-3

Example Mapping: 🟦 3 rules · 🟩 9 examples · 🟥 0 open

> No backup of `config.toml` exists. On 2026-08-27 the roster went from six accounts to one with
> nothing to restore from, and the original deletion is still **unattributed** — the investigation
> abstained rather than guess. Every other item in this scope guards amplification; this one guards
> the cause, without needing to know what it was.
>
> **The naive implementation makes the incident worse, and Rule 1's first scenario is written to
> catch it.** "Back up the file before overwriting it" runs against this sequence — delete, then
> `login`, then save — where the previous contents at save time were *nothing*. It would faithfully
> record the empty state and, on a ring, evict the last good copy to do it: a recoverable loss
> converted into an unrecoverable one.

## Rule 1 — only a qualifying write backs up, and only a qualifying write evicts

```gherkin
Scenario: the incident's own sequence leaves the good copy intact
  Given a config.toml holding six accounts has been backed up
    And the file is then deleted by something outside sessiometer
   When login writes a fresh one-account config.toml
   Then the six-account backup is still present
    And it has not been evicted
    # This is the point of the feature. A back-up-what-was-there rule passes every other scenario
    # here and fails this one, which is the only sequence that actually happened.

Scenario: a valid populated file qualifies
  Given a config.toml that parses and holds three accounts
   When it is replaced
   Then it is backed up first
    # The ordinary path. Qualifying means: parses as a valid config AND the roster is non-empty.

Scenario: an unreadable file neither backs up nor evicts
  Given a config.toml that cannot be read
   When a write replaces it
   Then no backup is written
    And no existing backup is evicted
    # Same for malformed, and for zero-account. Three shapes, one rule: a file that cannot be
    # vouched for is not evidence of anything and must not displace evidence.

Scenario: repeated bad writes cannot drain the ring
  Given a ring holding one good backup
   When five non-qualifying writes occur in succession
   Then the good backup is still present
    # The eviction predicate is the qualifying write, not the write. A ring that evicts per-write
    # is a fixed-size countdown to losing everything.
```

## Rule 2 — the backup is not a new disclosure and not a new corruption path

```gherkin
Scenario: mode is inherited, not defaulted
  Given a backup is written
   Then its mode is 0o600
    # FILE_MODE, src/paths.rs:56. config.toml carries no secret material (src/config.rs:17-20) but
    # the roster indexes credentials. A 0o644 backup of a 0o600 file is a disclosure the original
    # deliberately prevented. Assert the actual mode; do not assert it by construction.

Scenario: a backup is never torn
  Given a backup is written
   Then it is created via the same atomic temp-and-rename path as the live file
    # write_private_file, src/paths.rs:1185-1206. A partially-written backup is worse than none: it
    # looks restorable.

Scenario: a stale backup cannot be loaded as the live config
  Given backups exist alongside config.toml
   When the config is loaded normally
   Then no backup file is a candidate
    # Backups must sit outside the roster's own parse path. A backup silently adopted as live
    # config is a second unguarded write path into the roster.
```

## Rule 3 — restoring is a roster write like any other

```gherkin
Scenario: the operator can see what is retained
  Given three backups are retained
   When the operator lists them
   Then each is identified by timestamp and account count
    And no account label appears in the listing
    # Enough to choose; not a roster dump. The listing is a more public surface than the file.

Scenario: the ring holds at most three, oldest-first
  Given three backups are retained
   When a fourth qualifying write occurs
   Then the oldest is evicted and three remain
    # Small on purpose. The value is surviving one bad write, not keeping history.
```
