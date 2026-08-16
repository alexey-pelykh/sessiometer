# Contributing to sessiometer

Thanks for your interest in improving `sessiometer`. For building and running the
tool see the [README](README.md); for the *why* behind load-bearing technical
decisions see the [ADRs](docs/adr/). This guide covers the two things most likely
to surprise a new contributor: **macOS is the only supported build target**, and the
project holds a deliberate **minimal-dependency line** under which several primitives
you might expect to be crates are hand-rolled on purpose.

If you are about to add a dependency, or "helpfully" swap a hand-rolled primitive
for a well-known crate (`clap`, `hex`, `reqwest`, …), please read this first — the
omission is intentional, not an oversight.

## Supported platform: macOS only

**`sessiometer` builds and runs on macOS only.** The crate does not compile for a Linux
or Windows target today, and **no CI job attempts it** — every job that runs `cargo
build` / `test` / `clippy` / `doc` uses a `macos-latest` runner. The `ubuntu-latest` jobs
(`changes`, `deny`, `ci-ok-needs-complete`, `doc-gates`, `gate-change-ack`, `ci-ok`) are
gates and routers; none of them compiles the crate.

Two things follow, and both matter when you write or review a change:

- **A green CI run says nothing about portability.** Introduce a macOS-only call and every
  gate still passes. The most concrete instance is already on `main`:
  [`src/daemon/peer_auth.rs`](src/daemon/peer_auth.rs) calls `libc::getpeereid` with no
  `cfg(target_os)` gate, and `getpeereid(3)` is not in glibc — so a Linux `cargo check`
  exits 101. That is a known, accepted consequence, not a defect to fix in passing.
- **Do not write an acceptance criterion asserting that the Linux build works.** Nothing
  verifies it, so the claim cannot fail — which is worse than not making it. Where a test
  carries a platform assumption (a live `/bin/sh -l` spawn, an absolute passwd entry), say
  so in a comment beside it, as the login-shell harvest tests in
  [`src/paths.rs`](src/paths.rs) do.

None of this closes the door. Cross-platform support is tracked, sequenced future work:
recon first (#40), then the backend-neutral credential-store seam (#25) and the per-OS
mechanisms (#26 Linux, #27 Windows, #28 at-rest hygiene), then productionization including
a per-OS CI matrix (#29). The full rationale — and the alternative that was weighed and
deferred — is in [ADR-0029](docs/adr/0029-macos-is-the-only-supported-build-target.md).

## The minimal-dependency line

`sessiometer` reads and rewrites the credential Claude Code stores in your macOS
login keychain. Every dependency is therefore part of a **credential-adjacent
supply chain** — code that ships in a binary handling live secrets. The goal is to
keep that surface small and auditable: few direct dependencies, shallow transitive
trees, and no dependency pulled in for something the crate can do itself in a few
well-understood lines.

Concretely, the tree deliberately has **no** heavy client stacks:

- **No TLS / HTTP client** (`reqwest`, `hyper`, `rustls`, `native-tls`) — the one
  network call rides the system `curl` (see [the transport rule](#system-clis-not-client-crates-the-transport-rule)).
- **No argument-parsing framework** (`clap`) — a zero-dependency argv lexer plus
  our own routing does the job (see [when a crate is warranted](#when-a-crate-is-warranted)).
- **No date/time crate** (`chrono`, `time`) — the civil-date math is hand-rolled.
- **No keychain FFI binding** (`security-framework`) — enforced by a CI guard (see
  [guards](#guards-and-where-the-rationale-lives)).

The authoritative, always-current picture is the code, not this document:

- [`Cargo.toml`](Cargo.toml) — each direct dependency carries a comment explaining
  why it earns its place.
- [`deny.toml`](deny.toml) + `cargo deny check advisories sources licenses` — gates
  that dependencies come from crates.io and carry an allow-listed license.
- `cargo tree` — the actual graph at any moment.

## Hand-rolled primitives (and why)

These live in the crate instead of as dependencies. Each is small, stable, and
well-specified — the kind of thing where a runtime dependency buys little and costs
supply-chain surface:

| Primitive | Home | Why hand-rolled |
|-----------|------|-----------------|
| SHA-256 (FIPS 180-4) | [`src/sha256.rs`](src/sha256.rs) | Derives the keychain service-name suffix (replicating Claude Code's `sha256(CLAUDE_CONFIG_DIR)[..8]`) and a test-only redaction fingerprint. A cryptographic hash is the wrong thing to pull a runtime dependency in for; verified against the NIST vectors in-module. |
| Lowercase hex codec | [`src/hex.rs`](src/hex.rs) | Secrets must stay pure-ASCII so the keychain round-trip renders them as text, not as their own `0x`-hex blob. A two-digit-per-byte codec is the wrong thing to pull a dependency in for. |
| Civil-date math | `days_from_civil` / `civil_from_days` ([`src/usage.rs`](src/usage.rs), [`src/observability.rs`](src/observability.rs)) | Epoch-seconds ↔ civil-date conversion via Howard Hinnant's algorithms — so there is no date crate in the graph. |
| Jitter PRNG (SplitMix64) | [`src/timing.rs`](src/timing.rs) | Poll-cadence decorrelation noise, **not** a security primitive, so a tiny deterministic generator is exactly right — and it keeps the `cargo deny` advisory surface empty. |

The rule of thumb: **hand-roll a small, well-specified primitive rather than pull a
crate for it — but do not hand-roll something a maintained crate does more
correctly.** `unicode-width` (below) is that second clause in practice.

## System CLIs, not client crates (the transport rule)

Where `sessiometer` talks to the outside world, it **prefers a system CLI at an
absolute path, with secrets fed on stdin, over a client crate**:

- **Keychain access** goes through [`/usr/bin/security`](src/keychain.rs) (also
  [`src/stash.rs`](src/stash.rs)), never the Security.framework SDK. Here the reason
  is more than dependency count: writing the item through the SDK as our own code
  identity re-stamps its ACL and evicts the `apple-tool:` entry, breaking Claude
  Code's silent read — the CLI write rides `apple-tool:` and preserves it. The full
  rationale is [ADR-0002](docs/adr/0002-keychain-via-security-cli-zero-ffi.md).
- **The usage poll** rides [`/usr/bin/curl`](src/usage.rs) — **not** an HTTP client
  crate such as `reqwest`. `curl` is always present on macOS, so no TLS/HTTP stack
  enters the dependency graph for a single read-only `GET`.

Two disciplines both calls share, and any new external call should follow:

1. **Absolute path** (`/usr/bin/security`, `/usr/bin/curl`), never `$PATH`-resolved
   — a hijacked `PATH` cannot substitute a different binary for a
   security-sensitive call.
2. **Secrets on stdin, never argv** — the bearer token / secret never appears in
   this process's command line (`curl --config -`; `security -i`), so it cannot leak
   via `ps` or process listings.

## When a crate is warranted

Sometimes a crate genuinely is the right call. When it is, **prefer a zero-/low-
dependency crate over a heavy tree.** Two crates in the current graph are the model:

- **`lexopt`** (0 transitive dependencies) — an argv lexer that makes the argv layer
  strict (unknown flags and malformed usage become clear errors) **without** the
  ~10-crate weight of `clap`. We still own subcommand routing, help text, and error
  wording on top of it. (See issue #175.)
- **`unicode-width`** (0 transitive dependencies) — the canonical UAX #11
  display-width table, which replaced a hand-rolled `wcwidth` approximation that
  mis-measured emoji, ZWJ sequences, regional-indicator flags, skin-tone modifiers,
  and variation selectors. It is *strictly more correct* and has ~nil
  dependency-count impact — the case where reaching for a solved-and-versioned crate
  beats reinventing it. (See issue #176.)

Before adding a dependency, weigh:

- **Transitive weight** — what does `cargo tree` show it dragging in?
- **Credential adjacency** — does it end up in the binary that touches secrets?
- **License** — is it on the `deny.toml` allow-list? A new license fails
  `cargo deny check licenses` until reviewed and added.
- **Source** — it must resolve from crates.io; a git or alternate-registry source
  fails `cargo deny check sources` until vetted.

## Measurements published in a commit body

Squash-merge concatenates every commit body in a PR into the one commit that lands on `main`,
and `main` carries `non_fast_forward` with no bypass actors — so a figure published in a body
can never be corrected afterwards. Treat every number you put in one as permanent.

**A load-dependent measurement — a failure count, rate, or timing observed under artificial CPU
load, on one host, in one session — must carry one of two things:**

- **A re-runnable carrier**: something committed that re-derives the figure. Env-gated and
  skipped by default, following the idiom in
  [`apps/menubar/Tests/PanelGoldenParityTests.swift`](apps/menubar/Tests/PanelGoldenParityTests.swift)
  (`SESSIOMETER_PANEL_MEASURE=1` + `XCTSkipUnless`), so it costs the required suite nothing and
  never asserts on a host-dependent number.
- **An explicit `one-time attestation, no in-repo witness` label**, in the body, beside the
  figures it covers.

Silence is the failure mode. An unlabelled figure reads as reproducible, and the reader who
tries to reproduce it fails at something that was never on offer. The label is not an apology —
it distinguishes *known but not carryable* from *unknown*, which is a real difference and the
one a later reader needs.

**Do not commit a load generator to close the gap.** A harness that yields its finding only when
hand-fed a CPU load generator is a second unverified artifact in verification costume, not
evidence. That is why PR #1095, which set this convention, wrote such a probe, ran it, and
deliberately did not commit it.

Two claims usually travel together in these commits and only one of them is load-dependent.
Split them, and carry the half that can be carried:

- the **mechanism** ("the poll reads main-actor state from the cooperative pool") is normally
  pinnable *structurally* — assert the property, not the timing — and belongs in a committed
  test that runs in the required suite;
- only the **rate** ("21/250 failures before, 0/250 after, under load") needs the label.

A deterministic measurement needs neither: re-running the test re-derives it exactly, so the
test is its own witness. `bf71ad9`'s fixed-seed LCG iteration counts are that class, not this
one.

Worked example, including the figures that are labelled and the two that turned out to be
carryable after all:
[`apps/menubar/Tests/AccountSwapTests.swift`](apps/menubar/Tests/AccountSwapTests.swift)
§ Calibration, and PR #1095's own body.

## Writing an acceptance criterion

A merge here *is* the verification event — acceptance criteria are its precursor, not a parallel
record. So a criterion that cannot be settled from the artifact does not raise the bar; it moves the
decision somewhere no reviewer can reach.

> A criterion specifies a **property of the artifact**, not an **activity the author performed**.
> Where an activity is genuinely wanted, the criterion names the committed artifact that evidences
> it — or, where nothing committed can, demands the **label** rather than the run.

**An activity has no in-band carrier.** A diff can carry the *count* of a soak as prose; it can never
carry the *performance* of the run. A criterion demanding one is therefore dischargeable only by
author attestation — exactly the class an independent reviewer is structurally barred from accepting.
A clean change then converts into a block by construction, and that block says nothing about the
change.

This guide already forbids one instance of the same defect: [do not write an acceptance criterion
asserting that the Linux build works](#supported-platform-macos-only), because nothing verifies it.
Hold the two together, because the root is identical — the criterion names something no artifact
carries — and only the sign differs:

- **No gate exists**, so the criterion cannot fail. It passes vacuously and reads as verified.
- **Only attestation exists**, so the criterion cannot pass an independent check. A faithful
  implementation is blocked, and the criterion looks vindicated by the blocking.

Neither is a strict bar. Both are a bar pointing at nothing.

**The worked example, because the failure is not obvious in the abstract.** Issue #912 carried four
criteria and three of them were already properties — *the mechanism is confirmed with evidence*, *the
root cause is stated*, *the fix does not weaken the assertion*. Only the fourth was not:

> - [ ] Verified over a run count that could actually catch it, with the count stated. Given the
>       observed rarity, that means a soak substantially larger than the 125 runs it took to see it
>       once.

That names an **instrument** — a soak of size N — and it pre-commits to a statistical epistemology
*before the mechanism is known*. Its own first criterion can make the soak unnecessary, and on PR
#1095 it did. `readAck`'s `defer` closes the connection strictly before `send` returns, and
`CommandFakeConnection.close()` increments `closeCount` inside its lock strictly before it finishes
the `lines` stream — so a read-phase timeout cannot return with `closeCount == 0`, and that reading
implies the connect path *necessarily* rather than probably. The entailment is universally
quantified and re-derivable from the tree in minutes, which makes it strictly stronger than any
finite sample of it — and unreachable by a criterion that demands the sample. Both symbols live in
[`ControlCommandClient.swift`](apps/menubar/Sources/ControlCommandClient.swift) and
[`ControlCommandTransportTests.swift`](apps/menubar/Tests/ControlCommandTransportTests.swift);
the test file records the same argument in a comment beside the test it governs.

The pair that would have worked, and that #1101 proposed:

> - [ ] The repaired assertion is shown insensitive to the mechanism identified above — either
>       **(a)** by construction, with the entailment stated and checkable against the code, or
>       **(b)** failing that, by a stated-count soak under a stated load regime whose pre-fix arm
>       reproduces the reported failure verbatim.
> - [ ] Either way, a **committed test drives the identified mechanism deterministically**, so a
>       teardown regression fails CI rather than needing a soak to rediscover it.

Outcome-specified, artifact-dischargeable everywhere except clause (b) — which the next paragraph
takes up — and it admits the proof route instead of foreclosing it. Both clauses hold on `main` with
no soak at all: the entailment above for the first, and `testSlowConnectIsBoundedByTimeout` for the
second, which drives the connect path deterministically by putting a one-second blocking connect
against a 150 ms budget. The second clause is the load-bearing one — it is what converts a
regression that a soak would have to rediscover into one the required suite catches.

**Clause (b) is still an activity, and that is the point of writing it down.** Where no entailment is
available and only a performed run can settle the question, say so *in the criterion* — and have the
criterion demand the **label** rather than the run: what was done, where its result is published, and
that no committed artifact re-derives it. A reviewer then meets the attestation *as* attestation and
can weigh it, instead of being handed it in verification costume. The defect was never that
attestation exists; it is that an unlabelled demand for one is indistinguishable from a demand for
evidence.

[§ Measurements published in a commit body](#measurements-published-in-a-commit-body) is that label in
its established form — a re-runnable carrier, or the explicit `one-time attestation, no in-repo
witness` label — and it is the discharge to name whenever the activity falls in the class it governs.
Check what it governs before pointing at it — not from its opening line, which is about permanence,
but from the bolded sentence introducing its two discharges, whose class is *a load-dependent
measurement* where this rule's is wider. An empirical walk against a live external system, or a
check performed under the configuration that actually ships rather than in the foreground, is an
activity with no in-band carrier and no load to depend on; both are live in this tracker today.
Those get the same treatment — the criterion names the label — with no section to defer to.

**Do not "fix" #912's criteria.** They stand as written on purpose: PR #1095 is faithful to them, the
defect is in the criterion rather than in the implementation, and rewriting them would delete the
only worked example this section has.

Two things this does **not** say. It does not say *always soak*. Mandating the run lands back on the
second of the two failures above — only attestation exists — which is where #912's fourth criterion
already sat; clause (b) is the fallback, and clause (a) is stronger wherever it is reachable. And it
does not restrict what a commit body may **state**: a soak you actually ran is worth publishing,
§ Measurements published in a commit body governs that and is unchanged. The rule here is narrower,
constraining only what a criterion may **demand as a condition of acceptance**.

Nothing enforces any of this. No CI job reads a *criterion*. The unfiltered `doc-gates` job does
walk every document under `docs/`, the requirements briefs included, and it reads more of them than
their frontmatter: `check-doc-citations.sh` resolves the path-shaped frontmatter values, and
`check-doc-citation-rot.sh` reads the **body**, holding each `src/<file>.rs:NNN` citation the PR's
own diff adds or modifies to a symbol a reader can re-derive the line from — the tree-wide sweep is
its `--audit` mode, which CI does not invoke. Neither reaches a criterion. An issue body is outside
CI altogether, and *names an activity* is a prose judgment rather than a matchable pattern in any
case.

**The committed briefs under `docs/requirements/` are not exempt, and their form is no evidence that
they are.** Where they use the `**AC-N**` block, their criteria are a *Given / When / Then* over the
artifact — but that is a template, and an activity demand fits it exactly. Three that do, each
passing the form:

- `menubar-accessibility-reachability.md` **AC-2** rules the carrier out in its own text — *"verified
  against a running app, not only the harness"* — where the harness is the committed artifact.
- Its **AC-1** wants the effect of a system text-size change *"recorded ... with evidence"* for each
  of four cells against a running signed app. Nothing committed changes a system-wide OS setting.
- `gui-cli-capability-parity.md` **AC-1** wants *"the login completes and the account returns to
  healthy **without the operator typing a command anywhere**"* — an operator act and a real OAuth
  round trip, neither of which a tree can be in the state of having done.

The pattern is worth more than the instances: all three sit in the two briefs whose subject is the
running app or a live external flow, and the briefs whose subject is committed code read far
cleaner.

A fourth instance sits where nothing keyed to the `**AC-N**` form can reach it, and the blind spot
is worth as much as the instance. `session-warmup-on-reset.md` states its acceptance as prose under
`## 7. Success criteria` and carries no AC block at all, so any instrument counting that form passes
over it in silence. Its first criterion discharges through R-1, which requires the measurement be
taken *"on at least three cycles across at least two accounts"* — #912's fourth criterion again, in
a committed brief. Its subject is a live external measurement, so it falls on the same side of the
line as the three above rather than disturbing it.

That is where the rule above has to do its work — name the committed artifact, or demand the label —
and `menubar-accessibility-reachability.md` already works both routes itself. **AC-8** takes the
label: *"the verification tier is recorded as manual-only"*. **AC-4a** takes the artifact, naming
the Appearance-settings manual checklist in `apps/menubar/design/README.md` and requiring it carry a
text-size step. That second route is the established one here:
[ADR-0031](docs/adr/0031-ui-verification-tiers-bounded-by-structural-blindness.md) records a
**Manual pre-release** tier whose landed evidence is the manual checklists in that same file.

## Citing source locations in docs/

Documents under `docs/` cite code as `src/<file>.rs:NNN`. A stale **path** fails
loudly — the file is not there. A stale **line number** fails silently and
plausibly, which is worse: `src/cli.rs:4549-4611`, cited as the `import` body,
came to rest on `write_export`'s doc comment after an unrelated PR shifted the
file. It still resolved, still looked like evidence, and still read as verified.
That happened twice in one week, and the second time only an adversarial review
caught it (issue #1058).

The rule:

> Cite a **symbol** for a file that churns, and one that occurs **at the lines you
> cite**; a bare **line number** only for a file that does not churn. Never carry a
> line number across a rebase without re-deriving it. And once you have named a
> symbol, it must agree with the number **whatever the file's churn** — a quiet
> file excuses you from naming one, never from meaning it.

A line number is fine as a *secondary* locator — `` `apply_import`
(`src/cli.rs:4726-4813`) `` is the preferred form, because the symbol and the
number then check each other. That example is not hypothetical, and it is also
the limit of what the guard sees: the range was authored as `apply_import`
itself, the definition has since moved elsewhere in the file, and it passes
today only because the caller inside those lines names it. Re-derive the range
when you touch a citation rather than carrying a stale one across. What rots is
a number with nothing beside it to re-derive from — and a symbol the range does
not point at is nothing beside it. `login` occurs all over `src/cli.rs` and at
not one of the lines `src/cli.rs:741-765`, which is how that range sat rotted in
an ADR while the same range, cited without the word, was reported (issue #1338).
Both shapes are what the guard below rejects.

Churn governs only whether you are **asked** for a symbol, never whether the one
you wrote is **checked** (issue #1388). The two were one question until a
citation into `src/config.rs` — a file sitting under the threshold, so never
asked anything — quoted the `account_uuid` doc comment's text verbatim and
pointed two lines short of it, after an unrelated two-line insertion ~280 lines
higher up. A quiet file is a bet that a number keeps its meaning, and one commit
inserting one line above a citation is all it takes to lose that bet; so the bet
is a sound basis for not demanding a symbol, and never one for ignoring a symbol
already there.

"Churns" is not a judgment call — measure it:

```sh
# commits touching a file among the last 300 on this branch; >= 15 is churning
git rev-list --count "$(git rev-list --max-count=1 --skip=299 HEAD)..HEAD" -- src/cli.rs
```

Those two numbers are the guard's defaults (`CITATION_CHURN_WINDOW`,
`CITATION_CHURN_THRESHOLD`). The threshold is a **deliberate choice, not a
derived constant**, and it is set stricter than this repo's own past practice
rather than reconstructing it — run the guard over PR #1057
(`./scripts/check-doc-citation-rot.sh 386a6a2^ 386a6a2`) and it goes red, so any
claim that it merely reproduces that judgment is false. It is set low on the
asymmetry instead: naming a symbol costs one word and never rots, so a false
demand costs a word while a false exemption costs a silently-rotted citation.
The window is a commit count rather than a date range on purpose — a calendar
window would make the same commit green today and red next month because the
window slid, not because anything changed.

The guard is scoped to **the PR's own diff**, not the tree. The corpus already
carries a backlog of bare citations into churning files, so a tree-wide gate
would be red the day it landed and clearable only by the bulk conversion this
convention explicitly does not ask for. Working through that backlog is a
separate, optional job; `./scripts/check-doc-citation-rot.sh --audit` counts and
lists it as of whatever ref you pass (default `HEAD`) — the counts move with
every merge, so read them from the tool rather than from prose.

## Guards and where the rationale lives

- [`scripts/check-no-security-framework.sh`](scripts/check-no-security-framework.sh)
  — a CI guard (the `deny` job in
  [`.github/workflows/ci.yml`](.github/workflows/ci.yml)) that fails the build if
  `security-framework` appears anywhere in the dependency graph, so a refactor
  cannot silently reintroduce the SDK write path.
- [`scripts/check-menubar-zero-egress.sh`](scripts/check-menubar-zero-egress.sh)
  — the Swift-side peer of the guard above (the `swift` job in
  [`.github/workflows/ci.yml`](.github/workflows/ci.yml)). The menu-bar app is a
  pure local-socket client — it reaches the daemon over a raw POSIX AF_UNIX socket
  only ([ADR-0011](docs/adr/0011-menubar-transport-raw-posix-af-unix.md)), never the
  host network or the keychain — and this fails the build if `apps/menubar/Sources`
  grows a `Security`/`Network`-framework import, a host-networking symbol
  (`URLSession`, `NWConnection`, …), or a network entitlement. Like the daemon
  guard it works at the source (build-input) level, not on the linked binary (issue
  #328).
- [`scripts/check-gate-change-ack.sh`](scripts/check-gate-change-ack.sh)
  — a CI guard (the `gate-change-ack` job in
  [`.github/workflows/ci.yml`](.github/workflows/ci.yml)) that fails the build
  when a PR touches a gate-definition path (`.github/workflows/**`, `scripts/**`,
  `deny.toml`, `.cargo/**`) without a `Gate-Change-Acknowledged: <reason>` trailer
  on one of its commits, so a change to the merge gate's own definition lands
  deliberately and auditably rather than slipping through green in this solo repo
  (issue #317).
- [`scripts/check-panel-golden-rebaseline.sh`](scripts/check-panel-golden-rebaseline.sh)
  — a CI guard (the `gate-change-ack` job in
  [`.github/workflows/ci.yml`](.github/workflows/ci.yml)) that fails the build when a
  PR adds, changes, or deletes a committed panel golden under
  `apps/menubar/design/renders/panel-goldens/` without a
  `Panel-Goldens-Rebaselined: <reason>` trailer on one of its commits. Those PNGs are
  the reference the panel drift gate compares against, so re-blessing them is the one
  edit that can make the gate agree with a regression — it has to be a deliberate,
  recorded act rather than a side effect of regenerating (issue #754). Deliberately a
  SEPARATE trailer from `Gate-Change-Acknowledged:` above: that one acknowledges
  changing a gate's *definition*, this one acknowledges moving a gate's *baseline*.
  Falsifier peer: [`scripts/check-panel-golden-rebaseline.test.sh`](scripts/check-panel-golden-rebaseline.test.sh).
- [`scripts/check-ci-ok-needs.sh`](scripts/check-ci-ok-needs.sh)
  — a CI guard (the `ci-ok-needs-complete` job in
  [`.github/workflows/ci.yml`](.github/workflows/ci.yml)) that parses the workflow
  and fails the build if any job other than `ci-ok` is missing from `ci-ok.needs`,
  so a newly added job cannot silently escape the `ci-ok` summary gate's rollup
  (issue #318).
- [`scripts/check-ci-ok-results.sh`](scripts/check-ci-ok-results.sh)
  — a CI guard (a step in the `ci-ok` job itself in
  [`.github/workflows/ci.yml`](.github/workflows/ci.yml)) that is the gate's
  verdict: it reads the rolled-up `needs.*.result` list and admits only
  `success` and `skipped`, failing on anything else. An
  allow-list rather than a deny-list, because the deny-list it replaced enumerated
  `failure`/`cancelled` and so waved through `abandoned` — the result GitHub gives
  a job that died in `Set up job` — letting `ci-ok` report green on a run where
  three gates never executed (issue #1079). Its sibling above answers "is every
  job in the rollup?"; this one answers "did every job in it actually pass?".
  Falsifier peer: [`scripts/check-ci-ok-results.test.sh`](scripts/check-ci-ok-results.test.sh),
  which proves the guard goes RED on that run's verbatim results string and on a
  value nobody has enumerated yet.
- [`scripts/check-formula.sh`](scripts/check-formula.sh)
  — a CI guard (the `formula` job in
  [`.github/workflows/ci.yml`](.github/workflows/ci.yml)) that runs `brew style` and
  `brew audit --strict` against the canonical
  [`Formula/sessiometer.rb`](Formula/sessiometer.rb) — the source the published tap
  mirrors ([ADR-0021](docs/adr/0021-homebrew-tap-topology.md)). It stages the formula
  into a throwaway tap first, because `brew audit` refuses a file path outright and
  `brew style` mis-lints a loose `.rb` as ordinary Ruby. Static-only: the install +
  `test do` + bottle build belong to the tap's own CI (issue #560), keeping this cheap
  enough to run on every `Formula/**` touch (issue #567). Its falsifier,
  [`scripts/check-formula.test.sh`](scripts/check-formula.test.sh), proves the guard
  goes RED on the stanza-order defect of issue #566.
- [`scripts/check-doc-citation-rot.sh`](scripts/check-doc-citation-rot.sh)
  — a CI guard (the `doc-gates` job in
  [`.github/workflows/ci.yml`](.github/workflows/ci.yml)) that fails the build when a
  PR ADDS a `src/<file>.rs:NNN` citation under `docs/` that will rot silently: a line
  number into a file the PR's own history shows is churning, with no symbol named
  beside it that occurs AT the cited lines — or, whatever the churn, one whose named
  symbol contradicts its own number (issue #1388), one pointing past the file's EOF,
  or one at a file no clone has. Scoped to the PR's diff rather than the
  tree, because a backlog of such citations already exists and a gate that is red on
  arrival is a gate nobody reads (issue #1058); the convention it enforces is
  § Citing source locations in docs/ above. It refuses to
  run against a shallow clone rather than reporting the green that a depth-1 checkout —
  whose one grafted commit is parentless, collapsing every churn count to 1 and every
  file to "stable" — would otherwise manufacture. Falsifier peer:
  [`scripts/check-doc-citation-rot.test.sh`](scripts/check-doc-citation-rot.test.sh),
  which proves it goes RED on the shape issue #1058 reports (a bare range into a
  churning file) and GREEN on both things it must never block — a range whose symbol
  occurs INSIDE it, and a bare line number into a file that does not move. Anchoring
  is scoped to the range on BOTH sides, and each side is pinned by a pair differing
  in the range alone, so the scoping is falsifiable rather than asserted (issue
  #1338). A third pair differs only in whether the doc line names a symbol at all,
  over one unmoving file, so that churn cannot be what separates the demand from the
  self-check (issue #1388). It exercises those shapes over its own throwaway fixture,
  which has no `src/cli.rs` in it.
- `cargo deny check advisories sources licenses` — the supply-chain gates configured
  in [`deny.toml`](deny.toml).
- [`docs/adr/`](docs/adr/) — Architecture Decision Records for the load-bearing
  choices, including [ADR-0002](docs/adr/0002-keychain-via-security-cli-zero-ffi.md)
  on keychain-via-CLI.
