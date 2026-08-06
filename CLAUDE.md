# CLAUDE.md

Repo-specific directives for `sessiometer`. This **complements** your global configuration: it adds
facts about this repo, it does not change how you work. Exactly one section overrides a global
default, and it is marked `[override]` with a rationale. Every directive cites what enforces it, so
you can re-verify rather than trust — where nothing enforces one, it says so, and you should treat
those differently.

## What this repo is

A Rust daemon + CLI at `src/`, and a SwiftUI macOS menu-bar app at `apps/menubar/` that talks to the
daemon over a local AF_UNIX socket. **macOS is the only supported build target** — no CI job compiles
for Linux or Windows, so a green run says nothing about portability (`CONTRIBUTING.md`).

The two halves version their wire contracts independently and are gated by different CI jobs. Most
mistakes below come from applying one half's rule to the other.

## Where the canonical answers live

| You need | Read |
|---|---|
| *Why* the dependency line, the transport rule, or a guard exists | `CONTRIBUTING.md` |
| *Why* a load-bearing choice was made | `docs/adr/` |
| Measured verdicts that killed a feature | `docs/findings/` |
| How to build and run the daemon | `README.md` |
| Panel design reference + expected mock/Swift divergences | `apps/menubar/design/README.md` |
| *What to run, and when* | this file |

A directive that can't be reduced to a command or a concrete check belongs in one of those, not here.

## Before you push — build, lint, doc, and test gates

Run all five, in this order. They are copied byte-identical from the `test` job in
`.github/workflows/ci.yml`, and `fmt` is the cheapest, so a formatting slip aborts before the
expensive steps. If they ever diverge, `ci.yml` wins and this file is the bug.

```sh
cargo fmt --all --check
cargo clippy --all-targets --all-features -- -D warnings
RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --document-private-items
cargo build --verbose
cargo test --verbose
```

Do not paraphrase the flags. `--all-features` and `--document-private-items` are inert today (the
crate is binary-only with no `[features]` table), so dropping them still passes — until someone adds
a feature or a `[lib]` target, at which point your local gate silently stops predicting CI.

These five do **not** cover four other jobs, and every one of those runs locally too. Run the matching
one when you touch its paths:

- `src/**`, `Cargo.toml`, `Cargo.lock` → **`msrv`**, the one that surprises people: it re-runs
  `cargo build --verbose` + `cargo test --verbose` on a **different toolchain** (`RUST_MSRV` in
  `ci.yml`), so a newer-std API or a dependency bump passes all five above and still fails here.
  Reproduce with `cargo +<RUST_MSRV> build && cargo +<RUST_MSRV> test`.
- a dependency, `Cargo.toml`, `deny.toml` → **`deny`**: `./scripts/check-no-security-framework.sh`,
  then `cargo deny check advisories sources licenses`.
- `apps/menubar/**` → **`swift`**: `./scripts/check-menubar-zero-egress.sh`, then the app build
  below.
- `Formula/**` → **`formula`**: `./scripts/check-formula.sh` (`brew style` + `brew audit --strict`).

The toolchain is **not** pinned in-repo — there is no `rust-toolchain.toml`. CI pins `RUST_STABLE`
and `RUST_MSRV` in `ci.yml`, and `-D warnings` behaviour is toolchain-sensitive, so prefix
`cargo +<RUST_STABLE>` if a lint result surprises you. `Cargo.toml`'s `rust-version` and `ci.yml`'s
`RUST_MSRV` are two hand-maintained copies of the MSRV with **nothing enforcing that they agree**.

### Menu-bar app

Needs **full Xcode plus `xcodegen`** — Command Line Tools alone is not enough, and the failure is
`xcodebuild: command not found` rather than anything that names the real cause. `Menubar.xcodeproj`
is generated and gitignored, so it does not exist in a fresh clone.

```sh
cd apps/menubar && xcodegen generate && xcodebuild test \
  -project Menubar.xcodeproj -scheme Menubar -configuration Debug \
  -destination 'platform=macOS' CODE_SIGNING_ALLOWED=NO
```

↳ `swift` job in `.github/workflows/ci.yml`

## Before you merge

`main` is protected by a **ruleset** named `main-protection`, not by classic branch protection. `gh
api repos/alexey-pelykh/sessiometer/branches/main/protection` returns `404 Branch not protected` —
that is a false negative, do not read it as "unprotected". Read the real rules with:

```sh
gh api "repos/alexey-pelykh/sessiometer/rulesets/$(gh api repos/alexey-pelykh/sessiometer/rulesets \
  --jq '.[] | select(.name=="main-protection") | .id')"
```

What it enforces: `ci-ok` is the **only** required status check; branches must be up to date
(`strict_required_status_checks_policy`); linear history, reinforced by `allow_merge_commit: false`
at the repo level; no force-push to `main`; zero required approvals; and `bypass_actors` is empty, so
`--admin` **cannot** succeed — mechanically impossible here, not merely discouraged. Squash is the
strategy in use, rebase is also permitted, and head branches are auto-deleted on merge.

A concurrent merge leaves your PR BEHIND, and GitHub's "Update branch" button is disabled
(`allow_update_branch: false`), so rebase is the only way out:

```sh
git fetch origin main && git rebase origin/main
git push --force-with-lease          # never plain --force
gh pr merge <N> --squash --auto      # auto-merge is enabled
```

Run that inside `/submit`. It is the **recovery** sequence for a BEHIND PR, not a substitute for your
normal submit pipeline, and not a licence to hand-roll a merge chain.

**`ci-ok` is the only check that must be green.** A path-skipped job reports `skipping`, and skipped
counts as a pass — do not wait for every context to report. `panel-goldens` is a deliberately
**soft** gate: every step is `continue-on-error`, so it always reports pass and can never tell you
the panel drifted.

## Commit and issue conventions [override]

> **Override rationale**: the global `/git-commit` directs that issue/PR numbers stay out of commit
> messages and that linking happens in the PR body via `Closes #N`. This repo inverts both,
> deliberately. Squash-merge means the PR body is not preserved in `main`'s history, so the issue
> link must live in the commit subject to survive. And the interposed literal word `issue` is
> load-bearing: `Closes issue #806` renders as a cross-reference but is **not** a GitHub auto-close
> keyword, so merging never silently closes an issue whose acceptance criteria nobody verified.

- **Subject**: `(type) scope: imperative summary (issue #NNN)` — the common types are `feat`, `fix`,
  `test`, `refactor`, `docs`, `chore`. GitHub appends the PR number on squash-merge; do not write it
  yourself.
- **Body**, when the commit completes the issue: `Closes issue #NNN.` Never `Closes #NNN` or
  `Fixes #NNN` — those auto-close.
- **Closing is a separate, explicit act**: `gh issue close NNN`.

↳ Convention only — not enforced. Verify with `git log --format='%s%n%b' -20`.

## Branch naming

`{type}/{issue-number}-{slug}` — e.g. `fix/828-clear-exhausted-hold-on-leave-edge`,
`docs/833-project-claude-md`. Same type vocabulary as commit subjects.

↳ Convention only — not enforced. Verify with `gh pr list --state merged --limit 15 --json headRefName`.

## If you touch X, you must also do Y

| If you touch | You must also | Enforced by |
|---|---|---|
| `STATUS_SCHEMA_VERSION` (minor bump) | Regenerate the five status/watch goldens, then update the current-daemon Swift fixtures and assertions — leave version-pinned compat fixtures alone. See § Schema versions below. | `swift` + `test` jobs |
| `apps/menubar/design/renders/panel-goldens/**` | `git commit --amend --trailer 'Panel-Goldens-Rebaselined: <what changed and why>'` | `scripts/check-panel-golden-rebaseline.sh` |
| `build/fixtures/cli-renders/**` | `git commit --amend --trailer 'CLI-Goldens-Rebaselined: <what changed and why>'` | `scripts/check-cli-golden-rebaseline.sh` |
| `.github/workflows/**`, top-level `scripts/**`, `.cargo/**`, `deny.toml` | `git commit --amend --trailer 'Gate-Change-Acknowledged: <why this is safe>'` | `scripts/check-gate-change-ack.sh` |
| Add a job to `ci.yml` | Add it to `ci-ok.needs` | `scripts/check-ci-ok-needs.sh` |
| A path-shaped frontmatter value in `docs/**` | Make it a **git-tracked, repo-root-relative** path — or, on `source` / `parent-requirements` only, replace it with a prose note naming the referent. See § Doc citations below. | `scripts/check-doc-citations.sh` |
| The `doc-gates` job | Leave it **unfiltered**. `ci-ok` counts a `skipped` job as a pass, so a path filter converts a filter miss into a silent green. | Not enforced |
| Add a file to `apps/menubar/Sources/` that a **test** compiles against | Add an explicit `- path: Sources/NewFile.swift` under the `MenubarTests` target in `apps/menubar/project.yml`, then re-run `xcodegen generate` | Test-compile failure (`cannot find X in scope`) |
| Panel UI in `apps/menubar/Sources/` | Check it against the design mock — see § The design mock below | Not enforced |
| Add a dependency | Confirm its licence is on the `deny.toml` allow-list and it resolves from crates.io | `cargo deny check advisories sources licenses` |

**Trailer gotcha, and it is the expensive one.** Git parses only the *final contiguous* trailer
block, so your trailer must sit in the same paragraph as `Co-authored-by:` with **no blank line
between** — a blank line makes it invisible and the gate fails while you are looking straight at the
text. The value must also be non-empty. Any commit in the PR may carry it. Use `git commit
--trailer '...'` rather than hand-typing, and do not copy the shape from `main`'s own squashed
history: GitHub inserts the fatal blank line there.

Path scopes are narrower than they look: it is `.github/workflows/**`, not `.github/**`; and
`scripts/**` is anchored at top level, so `apps/menubar/scripts/**` does not trigger it.

### Doc citations

A committed document may only point at something a fresh clone has. The rule, in one line:

> A frontmatter value that is **path-shaped** must be a **git-tracked, repo-root-relative** path.
> A value that is not path-shaped is a provenance note, legal only on `source` and
> `parent-requirements`.

Path-shaped is decided by the value's **first whitespace-delimited token** — so a note may still name
its referent (`parent-requirements: private HQ (prd-stats), REQ-STA-* family`) without the check
demanding that file exist. That is what makes converting to a note a real option rather than a
euphemism for deleting the information.

Three things that look like fixes and are not:

- **Annotating a broken pointer** (`# uncommitted`, `# provenance only`) does not repair it. That
  annotation is what made a fabricated referent indistinguishable from a real one for four months.
- **Correcting the depth** on a pointer into the private HQ. It is a repo-root *sibling*; no `../`
  count reaches it from inside a clone. Write a note.
- **Deleting the pointer and leaving the reasoning that depended on it.** If a committed document's
  argument rests on a value from an unreachable source, replicate that value in-band at the point of
  use. A note names its referent; it does not carry its content.

If a referent genuinely never existed, **delete the key**. A pointer with no referent is not
provenance — it is a claim of provenance.

### Schema versions are four independent wires

Bumping the wrong one is the single most common cross-cutting mistake here. Being on the control
socket does not imply `STATUS_SCHEMA_VERSION` — `{"cmd":"stats"}` is served over that same socket and
carries `stats`' own `schema`.

| Wire | Constant | Home |
|---|---|---|
| `status` reply + **both** `watch` frames (snapshot and heartbeat) | `STATUS_SCHEMA_VERSION` (major/minor) | `src/daemon/snapshot.rs` |
| `log --json` | `JSON_SCHEMA_VERSION` | `src/log.rs` |
| `stats --json` (`StatsWire`) | `JSON_SCHEMA_VERSION` | `src/stats.rs` |
| `reliability --json` (`ReliabilityWire`) | `JSON_SCHEMA_VERSION` | `src/reliability.rs` |

`src/log.rs`'s own doc comment on the constant is the authority for this independence. "Additive
means no bump" is **not** a general rule — it holds only where the new key is omittable via
`skip_serializing_if`; `log` bumped precisely because its new field is always present.

**Two of the four cross into Swift.** `STATUS_SCHEMA_VERSION` does, via the fixtures below. `stats`
does too: `apps/menubar/Sources/WireModel.swift` carries a hand-maintained `StatsWire` mirror
(consumed by `PanelStatsModel`, `StatusPanelStats`, `StatusPanelFormat`, `PanelRenderHarness`), plus
the `statsBasic` / `statsConfigUnreadable` fixtures and `build/fixtures/wire-stats-basic.json` — so
reshaping the Rust `StatsWire` means editing Swift as well. `reliability` and `log` have no Swift
surface at all.

Not a JSON wire, but versioned and more dangerous to touch: `FORMAT_VERSION` in `src/migration.rs`
gates the on-disk migration-artifact container, and its fixtures are **frozen** — its own emitter's
doc comment says freeze once, thereafter only read.

**Telling the two Swift fixture classes apart.** Both live in `apps/menubar/Tests/Fixtures.swift`,
and nothing in their names or section comments reliably distinguishes them — those doc comments have
already gone stale at least once, so do not trust the prose. Select on the JSON literal instead: at
the current minor, `grep -n '"minor":<current>' apps/menubar/Tests/Fixtures.swift` **is** the set to
sweep. Fixtures at any other minor are deliberately pinned — most to prove backward tolerance, some
to exercise forward-compat, major-gating, or malformed-shape decoding — and none of them should move.

For `WireDecoderTests.swift`, grep **both** spellings: `grep -nE 'minor: <current>|"minor":<current>'`.
The assertions use the spaced form, but that file also holds an inline JSON literal one minor *ahead*
as a "future daemon" frame, which the spaced grep misses and which stops being future on the bump. A
bump may also need a *new* pinned fixture at the outgoing minor, so that the tolerance the old
version proved keeps being tested.

Regenerate the byte-pinned goldens rather than hand-editing them:

```sh
cargo test -- --ignored emit_wire_golden_fixtures       # the 5 status/watch wire-*.json
cargo test -- --ignored emit_wire_stats_golden_fixture  # wire-stats-basic.json
cargo test -- --ignored emit_cli_render_goldens         # build/fixtures/cli-renders/
```

Production Swift needs no edit for a minor bump — `WireModel.swift` deliberately reads only the major.

### The design mock is the panel's build reference

`apps/menubar/design/menubar-preview.html` is the canonical **visual** build reference for the panel
(`apps/menubar/design/README.md`) — build against it, not against whatever the extant Swift happens
to do. Two limits, both real:

- It is the oracle **only for what it authors**. Silence is not authority; Dynamic Type, Reduce
  Transparency, and the Settings window are outside it entirely, and its hex/pixel values are
  directional rather than targets except where README says otherwise.
- It can be stale, and measurement outranks it. Known divergences are recorded under **Expected
  reconciliations** in `apps/menubar/design/README.md` — read that before "fixing" a mismatch.

Comparing a built panel against it is a **manual** step, not a CI gate. Pass an explicit
`-derivedDataPath` so the binary lands somewhere you can name, and an explicit capture directory — a
bare `build-comparison.py` run defaults to a directory that lacks some frames and dies:

```sh
cd apps/menubar
xcodegen generate
xcodebuild build -project Menubar.xcodeproj -scheme Menubar -configuration Debug \
  -destination 'platform=macOS' -derivedDataPath .build/xcode CODE_SIGNING_ALLOWED=NO
mkdir -p ../../.tmp/panelcaps
.build/xcode/Build/Products/Debug/Sessiometer.app/Contents/MacOS/Sessiometer \
  --render-panel "$PWD/../../.tmp/panelcaps"
python3 design/build-comparison.py ../../.tmp/panelcaps ../../.tmp/design-vs-capture.html
```

## Deliberate — do not "fix" these

- `src/daemon/peer_auth.rs` calls `libc::getpeereid` with no `cfg(target_os)` gate, so a Linux
  `cargo check` fails. That is an accepted consequence of macOS-only support (`CONTRIBUTING.md`), not
  a defect to clean up in passing.
- Eight Swift files are permanently excluded from the `MenubarTests` target because they touch
  surfaces a headless bundle cannot host (`main.swift`, `StatusItemController`, the Settings pair,
  the login-item and notification presenters, and the two render tools). The exclusions carry
  rationale comments in `apps/menubar/project.yml`. Do not resolve a link error by adding one.
- `apps/menubar/spikes/**` is outside the build graph — `project.yml` never references it. It is not
  live code.
