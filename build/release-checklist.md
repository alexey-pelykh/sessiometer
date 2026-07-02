# Release checklist

Steps to run before tagging a `sessiometer` release.

Its reason for existing is the **Claude Code compatibility gate** (step 1). Because `sessiometer`
depends on reverse-engineered Claude Code internals recorded in
[`build/version-compat.md`](version-compat.md), each release must re-verify the Claude Code version
range it was validated against — so a CC drift becomes a *visible* release-time signal instead of
silent breakage. CI cannot cover this: it runs hermetically and never execs a real `claude`.

## 1. Re-verify Claude Code compatibility (required)

The supported range is the authoritative declaration in
[`build/version-compat.md`](version-compat.md) § *Supported Claude Code range*
(the `CC_SUPPORTED_MIN` / `CC_SUPPORTED_MAX` lines).

- [ ] Run the check against the `claude` you are releasing against:

  ```sh
  ./scripts/check-cc-version.sh
  # or point it at a specific binary:
  CLAUDE_BIN=/path/to/claude ./scripts/check-cc-version.sh
  ```

- [ ] **In range** (`ok:` / exit 0) → proceed.
- [ ] **Above range** (a newer `claude` shipped; `warning:` / exit 1) → do **not** release blind.
  Re-verify the version-sensitive findings in [`build/version-compat.md`](version-compat.md) — at
  minimum **H3** (fresh-start adoption) and the **#100** keychain-service derivation (`n1()`) —
  against the new CC. If they still hold, widen `CC_SUPPORTED_MAX` in the ledger, update the range
  stated in the [README](../README.md#prerequisites) to match, then re-run the check.
- [ ] **Below range** (`warning:` / exit 1) → the `claude` you tested is older than the verified
  floor; install a supported `claude`, or (if releasing for that older CC) verify it and lower
  `CC_SUPPORTED_MIN`.
- [ ] **Could not determine** (`error:` / exit 2) → no `claude` resolved (set `$CLAUDE_BIN` or add it
  to `$PATH`) or its `--version` was unparseable. Fix and re-run — exit 2 is **not** a pass.
- [ ] The script also fails (exit 1) if `README.md` no longer states the ledger range, so the
  user-facing range (AC1) cannot silently drift — keep the README `## Prerequisites` range in sync
  with `CC_SUPPORTED_MIN`–`CC_SUPPORTED_MAX`.

## 2. Standard gates

- [ ] CI is green on the release commit — format, `clippy -D warnings`, docs, build, tests, MSRV, and
  `cargo deny` (see [`.github/workflows/ci.yml`](../.github/workflows/ci.yml)).
- [ ] (Recommended) Run the manual end-to-end [`build/smoke-test.md`](smoke-test.md) against real
  accounts for live-keychain / live-API assurance.
- [ ] Crate version bumped in `Cargo.toml`.
