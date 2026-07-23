# Release checklist

Steps to run before tagging a `sessiometer` release.

It began as the home of the **Claude Code compatibility gate** (step 1). That step is now an
**advisory provenance check** (demoted in #716): since #714 the daemon carries a behavioral canary
that refuses credential writes at runtime when the reverse-engineered keychain derivation drifts —
so drift is caught on the user's machine, where the risk actually lands, instead of being policed at
release time. (There is no runtime version advisory: the version string was never a control, so the
#715 startup/`status` advisory was removed in #716; the range now records provenance only, surfaced
in `sessiometer --version`.) Step 1 keeps the *published* "verified against" range honest; it no
longer blocks a release. CI still cannot cover it: CI runs hermetically and never execs a real
`claude`.

## 1. Record Claude Code provenance (advisory)

The verified range is the authoritative provenance in
[`build/version-compat.md`](version-compat.md) § *Supported Claude Code range*
(the `CC_SUPPORTED_MIN` / `CC_SUPPORTED_MAX` lines) — the record of which CC builds the
reverse-engineered internals were last verified against.

- [ ] Run the check against the `claude` you are releasing against:

  ```sh
  ./scripts/check-cc-version.sh
  # or point it at a specific binary:
  CLAUDE_BIN=/path/to/claude ./scripts/check-cc-version.sh
  ```

- [ ] **In range** (`ok:` / exit 0) → provenance is current; nothing to record.
- [ ] **Above range** (a newer `claude` shipped; `warning:` / exit 1) → **advisory, not a
  blocker**: the release may ship as-is — its provenance then honestly states the older verified
  range, and the #714 canary refuses credential writes on derivation drift. Prefer refreshing the
  provenance when practical:
  re-verify the version-sensitive findings in [`build/version-compat.md`](version-compat.md) — at
  minimum **H3** (fresh-start adoption) and the **#100** keychain-service derivation (`n1()`) —
  against the new CC; if they hold, widen `CC_SUPPORTED_MAX` in the ledger, update the range
  stated in the [README](../README.md#prerequisites) to match, then re-run the check.
- [ ] **Below range** (`warning:` / exit 1) → same advisory footing: the `claude` you tested is
  older than the verified floor. If releasing for that older CC matters, verify it and lower
  `CC_SUPPORTED_MIN`; otherwise ship with the provenance as recorded.
- [ ] **Could not determine** (`error:` / exit 2) → no `claude` resolved (set `$CLAUDE_BIN` or add it
  to `$PATH`) or its `--version` was unparseable. Fix and re-run — exit 2 means the provenance was
  not checked at all this release, which is worth one retry even for an advisory.
- [ ] The script also reports (exit 1) when `README.md` no longer states the ledger range. Unlike
  the version arms above, fix this one before tagging: it means the *published* provenance is
  wrong — the README misstates what was verified — not merely that a newer `claude` exists. Keep
  the README `## Prerequisites` range in sync with `CC_SUPPORTED_MIN`–`CC_SUPPORTED_MAX` (the
  range-as-a-unit assertion from #712/#722 catches a partially-updated README too).

## 2. Rollback/downgrade procedure current (required)

`sessiometer` rewrites live Claude Code state (the `Claude Code-credentials` keychain item and
`~/.claude.json`) and reads/writes a versioned migration artifact, so a downgrade or an
abandoned rollout must have a documented recovery path — [`build/rollback.md`](rollback.md)
(issue #191). Confirm it still matches what this release ships:

- [ ] The mutated artifacts documented in [`build/rollback.md`](rollback.md) still match
  reality — the canonical `Claude Code-credentials` keychain item, `~/.claude.json`, and the
  migration `format_version` contract. No release adds a new mutated state artifact without a
  rollback note for it.
- [ ] **If `FORMAT_VERSION` was bumped this release** (`src/migration.rs`): the `format_version`
  section (§3) of [`build/rollback.md`](rollback.md) states the new version and its
  cross-version compatibility (does this release's reader still exact-match its own version, or
  was it widened to accept older ones?), and its "older binary meets newer artifact" error text
  still matches `Error::MigrationUnsupportedVersion` (`src/error.rs`).

## 3. Standard gates

- [ ] CI is green on the release commit — format, `clippy -D warnings`, docs, build, tests, MSRV, and
  `cargo deny` (see [`.github/workflows/ci.yml`](../.github/workflows/ci.yml)).
- [ ] (Recommended) Run the manual end-to-end [`build/smoke-test.md`](smoke-test.md) against real
  accounts for live-keychain / live-API assurance.
- [ ] Crate version bumped in `Cargo.toml`.
