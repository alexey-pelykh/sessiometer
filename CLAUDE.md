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

These five do **not** cover the other path-filtered jobs in `ci.yml`, and every one of those runs
locally too. Run the matching one when you touch its paths. `.github/workflows/ci.yml` is in all
three filters, so editing it re-runs every one of them.

**`msrv` and `deny` share one trigger, and it is much wider than dependency work.** Both are
`if: needs.changes.outputs.rust == 'true'`, and so is `test`, whose commands are the five above. The
`rust` filter is `src/**`, `Cargo.toml`, `Cargo.lock`, `deny.toml`, `build/fixtures/**`,
`scripts/**`, `.cargo/**`, `build.rs`, `**/build.rs`, `rust-toolchain.toml`, `rust-toolchain`,
`rustfmt.toml`, `clippy.toml`, `.github/workflows/ci.yml`, and `src/**` is the entry that gets
missed. **An ordinary source edit owes both jobs**: any change under `src/**` puts the PR in this
filter, whether or not it touches a dependency. The wording this replaced named only the
dependency-shaped paths under `deny`, so a `src/**`-only diff read as "`deny` not owed" — a false
line that went into commit bodies, where squash-merge makes it permanent.

Every path list in this section is a copy, and `ci.yml` wins wherever they disagree. Print the live
filters block with `awk '/filters: \|/,/^  [a-z-]+:$/' .github/workflows/ci.yml`; it stops at the
next two-space key rather than at a blank line, so it over-prints rather than truncating in silence.

- the `rust` filter → **`msrv`**, the one that surprises people: it re-runs
  `cargo build --verbose` + `cargo test --verbose` on a **different toolchain** (`RUST_MSRV` in
  `ci.yml`), so a newer-std API or a dependency bump passes all five above and still fails here.
  Reproduce with `cargo +<RUST_MSRV> build && cargo +<RUST_MSRV> test`.
- the `rust` filter → **`deny`**, whose two checks differ in what they need locally. The first,
  `./scripts/check-no-security-framework.sh`, runs off `cargo metadata` and `jq`. The second,
  `cargo deny check advisories sources licenses`, needs `cargo-deny`, which is not part of the
  toolchain — `cargo install --locked cargo-deny`, or `brew install cargo-deny`. Run both; if you
  ran only the first, say so rather than reporting the job as run.
- `apps/menubar/**`, `build/fixtures/**` or `.github/workflows/ci.yml` → **`swift`**:
  `./scripts/check-menubar-zero-egress.sh`, then the app build below. The fixtures are in that
  filter because the Swift byte-drift guard pins against the Rust-emitted wire goldens, so a golden
  re-baseline touching no `apps/menubar/**` path still owes this job — and `panel-goldens` with it.
- the same paths as `swift` → **`panel-goldens`**, the panel-appearance drift check, and the one job
  here the row above does **not** get you: the app build below leaves both committed-golden
  comparisons *skipped*, and the CI job is soft (§ Before you merge below), so neither surface can
  tell you the panel drifted. Its armed command is in § Menu-bar app below.
- `Formula/**`, `scripts/check-formula.sh` or `.github/workflows/ci.yml` → **`formula`**:
  `./scripts/check-formula.sh` (`brew style` + `brew audit --strict`). Its own guard is in that
  filter — and `scripts/**` puts that guard in the `rust` filter too, so editing it re-runs `test`,
  `msrv` and `deny` as well.

↳ Convention only — nothing reconciles these lists against `ci.yml`. That is a decision rather than
an oversight: such a check lands in `scripts/**` plus the `doc-gates` job, both gate paths, so it is
a change to the gates themselves and has to be argued on its own rather than ridden in on a docs
fix. Enumerating the filters verbatim, as this section now does, widens the copy and raises what
that check would be worth — the case is stronger than it was, and still not made here. Until it is,
re-derive both halves rather than trusting either: the `awk` above prints the live filters, and
`grep -B2 'if: needs.changes' .github/workflows/ci.yml | grep name:` prints every path-filtered job
(all of them but `test` belong above). The `grep` prints no paths at all, so it cannot tell you a
row's trigger has drifted — which is the half that actually went wrong here.

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

That run does **not** compare the panel against its committed goldens. `PanelGoldenParityTests`'
two golden comparisons `XCTSkipUnless` on `SESSIOMETER_PANEL_GOLDEN_GATE`, so they skip and the run
still ends `** TEST SUCCEEDED **`. Arm them separately — the `TEST_RUNNER_` prefix is what reaches
the test process, and a bare `SESSIOMETER_…` stops at `xcodebuild`, leaving the tests skipped and the
run green having compared nothing:

```sh
cd apps/menubar && xcodegen generate && TEST_RUNNER_SESSIOMETER_PANEL_GOLDEN_GATE=1 xcodebuild test \
  -project Menubar.xcodeproj -scheme Menubar -configuration Debug \
  -destination 'platform=macOS' CODE_SIGNING_ALLOWED=NO \
  -only-testing:MenubarTests/PanelGoldenParityTests/testEveryRenderMatchesItsCommittedGolden \
  -only-testing:MenubarTests/PanelGoldenParityTests/testEachFreshRenderIsNearestToItsOwnGolden
```

Read the result off the drift line, never off `TEST SUCCEEDED`: a real pass prints a
`[panel-goldens] max drift …` line naming the worst cell and the ceiling it was measured against,
and `Executed 2 tests, with 2 tests skipped` means nothing was compared.

↳ `panel-goldens` job in `.github/workflows/ci.yml`. Re-baselining, the ceiling, and what a red
means: `apps/menubar/design/README.md` § Panel golden drift gate.

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
the panel drifted. Its verdict is in the job's step summary, never in its status; the only way to
have that answer before you push is to have armed it yourself (§ Before you push).

## Commit and issue conventions [override]

> **Override rationale**: the global `/git-commit` directs that issue numbers stay out of commit
> messages and that linking happens in the PR body. This repo inverts that, deliberately:
> squash-merge does not preserve the PR body in `main`'s history, so the issue link must live in the
> commit subject to survive. The linking keyword itself is **not** overridden — the body form below
> is the ordinary auto-closing one.

- **Subject**: `(type) scope: imperative summary (issue #NNN)` — the common types are `feat`, `fix`,
  `test`, `refactor`, `docs`, `chore`. GitHub appends the PR number on squash-merge; do not write it
  yourself.
- **Body**, when the commit completes the issue: `Closes #NNN.` It auto-closes on merge, which is
  the intent — acceptance criteria are a precursor to merging, so a merge *is* the verification
  event. The trailing period is safe; GitHub's parser stops at the number.

Through 2026-08-07 this section mandated a deliberately non-auto-close `Closes issue #NNN.` form and
a separate `gh issue close NNN`; that is withdrawn — it defended against unverified merges by
weakening the link rather than the gate. On 2026-08-16 `main`'s history was rewritten so that every
`Closes issue #NNN.` link became the auto-closing form: 64 commits. Every issue they referenced was
already closed, so the conversion closed nothing. The pass ran with `main-protection` lifted and
restored field-for-field afterwards — § Before you merge's *no force-push to `main`* binds
anyone working under the ruleset, not an owner who removes it and puts it back. **You may now
read the `Closes` convention off `main`'s history** — that decision retires both of this
section's earlier prohibitions: the instruction not to infer the convention from history, and
`Do not rewrite either.`

Two things a widened window will hit, and neither is drift. `fda6755` and `715fa57` (both
2026-07-22) carry `Resolves issue #NNN.` — the same keyword-plus-`issue` shape under a different
linking keyword, equally non-auto-closing, and outside what the rewrite converted. And `34f07ba`
and `4cf1adf` quote `Closes issue #` as *prose about the convention*; the rewrite left those alone
while still converting `4cf1adf`'s own link line.

The same pass removed `Co-authored-by: judge <judge@t.invalid>` from 42 commits — a fabricated
co-author, whose mechanism is recorded upstream at `alexey-pelykh/.claude` issue #4251. Both edits
were message-only: the ordered tree-SHA sequence across all 622 commits is identical before and
after, so no content moved. Pre-rewrite history stays recoverable at tag
`backup/pre-attribution-rewrite-20260816` (`e4b118c`).

↳ Convention only — nothing parses the commit message. Check a body link's *shape*, not one literal:
`git log --format='%b' -20 | grep -nE '^(Closes|Fixes|Resolves) issue #'` should return nothing, and
every body link in that window should carry `Closes #NNN.`. Widen it far enough and the grep
reaches `fda6755` and `715fa57` — history, not new drift. It never reaches `34f07ba` or
`4cf1adf`: their `Closes issue #` sits mid-line, and this pattern is anchored.

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
