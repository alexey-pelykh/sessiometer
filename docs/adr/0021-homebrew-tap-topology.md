---
type: architecture-decision-record
number: 21
title: "Homebrew tap topology — an org-owned distribution repo, not a second product repo"
date: 2026-07-16
status: accepted
decision_makers: [Oleksii PELYKH (maintainer)]
---

# ADR-0021: Homebrew tap topology — an org-owned distribution repo, not a second product repo

## Status

**Accepted** — 2026-07-16. Records why **#558** stands up a *second repository* —
the Homebrew tap **`sessiometer/homebrew-tap`** — and why that does **not**
contradict **ADR-0010**'s "one repo, one notarized artifact". Governs where the
**#269** formula is published and where the **#172** cask will land.

Like ADR-0010, this is a **topology decision**: it records where a distribution
surface lives and why, so a contributor does not re-litigate the second repo on
sight of it.

## Context

The **#269** Homebrew formula already lives **in this repo** at
`Formula/sessiometer.rb` — but as it stands it installs for **nobody**. Homebrew
refuses to load a formula from a file path by default:

```console
$ brew install --HEAD --build-from-source ./Formula/sessiometer.rb
Error: Homebrew requires formulae to be in a tap, rejecting:
  ./Formula/sessiometer.rb
```

The guard is on unless `HOMEBREW_DEVELOPER` is set, so the in-repo formula is a
*build recipe a maintainer can exercise* (`HOMEBREW_DEVELOPER=1 brew install …`),
not *distribution*: it gives an ordinary user **no** install path at all. (The
same guard rejects path-loaded **casks**, so **#172** cannot ship this way
either.)

Distribution requires a **tap**: a Git repository from which `brew` reads
formulas and casks. To be tappable by the short one-argument form the repo name
must start with `homebrew-` — Homebrew's own wording is that the prefix "is not
optional" for that form — and `brew` **strips** it, so a repo named
`homebrew-tap` is tapped as `sessiometer/tap`. (A two-argument
`brew tap <user>/<repo> <url>` can tap an arbitrarily-named repo, but it forfeits
exactly the short, memorable UX a tap exists to provide.)

That requirement collides — on its face — with **ADR-0010**, which decided
**monorepo**: the Swift menubar app lives in *this* repo precisely because
**#171** notarizes the app and the embedded daemon **as one unit**, and "one
shipped artifact wants one repo". A second repository looks like a reversal.

Two questions fell out, and this ADR answers both:

1. **Does a tap repo violate ADR-0010's one-repo rule?**
2. **Who owns the tap** — the `sessiometer` org, or the maintainer's personal
   namespace (where the source repo lives)?

## Decision

1. **The tap is its own repository — `sessiometer/homebrew-tap` — because
   Homebrew's design requires it.** A tap *is* a repo; there is no in-repo tap.
   The resulting UX:

   ```sh
   brew tap sessiometer/tap

   # CLI + daemon — today:
   brew install --HEAD sessiometer/tap/sessiometer

   # once #172 lands, if the cask shares the token (see Decision 3):
   brew install --formula --HEAD sessiometer/tap/sessiometer   # the CLI + daemon
   brew install --cask sessiometer/tap/sessiometer             # the .app
   ```

2. **Org-owned, not personal.** The tap belongs to the **`sessiometer` GitHub
   org**, aligned to the **`org.sessiometer`** reverse-DNS namespace already used
   by the sessiometer.org domain and the `org.sessiometer.*` app bundle IDs. The
   tap name is the most user-visible string in the install flow; it carries the
   *project's* identity, not the maintainer's handle.

3. **One tap, both channels.** The CLI/daemon **formula** and the later **#172**
   `.app` **cask** share this one tap: Homebrew reads `Casks/` alongside
   `Formula/` in the same repo, so a second tap would be ceremony.

   **Open for #172 — the cask's token.** This ADR settles the *tap*, **not** the
   cask's name, and deliberately does not freeze it. If the cask takes the same
   `sessiometer` token as the formula, the two rub against each other inside the
   tap: an un-flagged `brew install sessiometer/tap/sessiometer` still resolves to
   the **formula** (Homebrew prioritises a formula over a non-core-tap cask) but
   warns — *"Treating … as a formula … specify the `--cask` flag"* — and
   `brew trust sessiometer/tap/sessiometer` fails outright with *"Ambiguous trust
   target"*. Note `--HEAD` does **not** disambiguate; only `--formula` / `--cask`
   do. Either resolution is open to **#172**: keep one token and require the flags,
   or give the cask a distinct token (e.g. `sessiometer-app`) and sidestep the
   friction. That choice is #172's to make; it does not change this ADR's one-tap
   decision either way.

4. **The in-repo formula stays canonical; the tap copy is a one-way mirror.**
   `Formula/sessiometer.rb` *here* is authoritative and reviewed here; the copy in
   the tap is **never hand-edited**. A release-CI job will sync it on tagged
   release (**#559**, not yet built — the mirror is seeded and maintained by hand
   until it lands).

5. **HEAD-only for now.** The crate is pre-release (0.1.0, no tag), so the formula
   carries `head` and no `url`/`sha256`. `brew install --HEAD` works today; a bare
   `brew install` waits on the first tag (owner-gated, out of scope).

### Reconciliation with ADR-0010

ADR-0010's rule is about **product source** — the code that compiles into the
shipped artifact. Its reasoning is that **#171** notarizes the app *and* the
embedded daemon as **one version-locked unit**, so splitting that unit's *source*
across repos would force cross-repo pinning and release choreography on every
build.

A tap holds **distribution metadata**, not product source:

- The tap contains a Ruby **formula** that *points at* the source repo
  (`head "https://github.com/alexey-pelykh/sessiometer.git", branch: "main"`),
  a README, and a LICENSE. **No product source, no build graph, nothing compiled
  from the tap.**
- `brew install --HEAD` clones **this** repo and builds **this** tree. The tap is
  a *package-index entry*, the same role a homebrew-core formula file would play
  if the project were in core — and nobody would call homebrew-core a second
  sessiometer repo.
- The notarized `.app` (#171) is still built, signed, and notarized from the
  **single** source tree. The cask (#172) will merely *reference* that released
  artifact.

So "one repo, one notarized artifact" is **untouched**: there is still exactly one
product repo and exactly one notarized artifact. The tap adds a second
*distribution* repo, which ADR-0010 never spoke to. The two ADRs are orthogonal —
ADR-0010 governs where **source** lives; this ADR governs where **package
metadata** lives.

## Alternatives considered

1. **Personal-namespace tap (`alexey-pelykh/homebrew-tap`).**
   - **Pros**: sits beside the source repo; no org indirection; one less place to
     hold permissions.
   - **Cons**: diverges from the `org.sessiometer` reverse-DNS namespace the
     domain and bundle IDs already use; bakes a personal handle into the
     project's most user-visible install string (`brew tap alexey-pelykh/tap`),
     which ages badly if ownership or maintainership ever moves.
   - **Why rejected**: the tap name *is* product-facing UX. It should say
     `sessiometer`, matching every other identity surface the project owns.

2. **No tap — keep the in-repo formula as the only path.**
   - **Pros**: zero new repos; ADR-0010 trivially intact; nothing to sync.
   - **Cons**: it installs for **nobody**. Homebrew rejects path-loaded formulae
     unless `HOMEBREW_DEVELOPER` is set (§ Context), so this is not a worse
     channel — it is *no* channel, let alone install-by-name, `brew upgrade`, or
     discovery. The same guard rejects path-loaded casks, so **#172** could not
     ship either.
   - **Why rejected**: it is a build recipe, not a distribution channel. The
     formula existing in-repo (#269) and the tap publishing it are complementary,
     not alternatives.

3. **Submit to homebrew-core (`brew install sessiometer`, no tap).**
   - **Pros**: the best possible UX — no `brew tap` step; bottles built by
     BrewTestBot.
   - **Cons**: core requires a **stable tagged release** (none yet — the tag is
     owner-gated and out of scope here); core does not take HEAD-only formulas;
     and core requires **notability** — the documented bar is ">=30 forks, >=30
     watchers **or** >=75 stars", **tripled for self-submitted software** (">=90
     forks, >=90 watchers, or >=225 stars"), plus use by someone other than the
     author. Self-submitted sessiometer sits at 0 / 0 / 0 today.
   - **Why rejected (for now)**: gated on a tag and notability the project has not
     reached. The tap is the correct **pre-release** channel and remains correct
     afterward; core stays a future option that this decision does not foreclose.

4. **A tap per channel** — one repo for the formula, another for the cask.
   - **Pros**: superficial separation of the headless and GUI channels.
   - **Cons**: two repos to create, sync, and secure; two `brew tap` commands for
     users; no benefit, since Homebrew already separates formulas and casks
     *within* a tap by directory and namespace.
   - **Why rejected**: pure ceremony — the same "no unnecessary structure" ethos
     ADR-0010 applied to the deferred Cargo workspace.

## Consequences

### Positive

- **Install-by-name works today, pre-tag.** `brew tap sessiometer/tap && brew
  install --HEAD sessiometer/tap/sessiometer` builds and installs the CLI +
  daemon with no tag and no local checkout.
- **One tap for both channels.** The #172 cask lands in the same tap, so users run
  `brew tap sessiometer/tap` once for the headless *and* GUI channels.
- **Project-identity UX.** The install string carries `sessiometer`, consistent
  with sessiometer.org and the `org.sessiometer.*` bundle IDs.
- **ADR-0010 stays intact and now has a stated boundary**: source lives in one
  repo; package metadata may live in a tap. Future distribution surfaces
  (a cask, a core formula) inherit that boundary instead of re-opening it.
- **The canonical formula stays reviewed here.** The mirror rule keeps the
  formula's review surface in this repo, where CI and history already are.

### Negative / trade-offs

- **A second repository to maintain.** Accepted: Homebrew's design requires it,
  and it holds no source — its blast radius is packaging metadata only.
- **The mirror can drift** from the canonical in-repo formula until the release-CI
  sync job (**#559**) lands — until then the mirror is hand-maintained, which is
  exactly the failure mode #559 removes. Accepted, and bounded: the copy is
  byte-identical today, and the "never hand-edit the mirror" rule is recorded here
  and in the tap's README.
- **Users must `brew tap` first**, unlike a homebrew-core formula. Accepted: the
  price of shipping pre-release; the core path stays open once a tag and
  notability exist.
- **No CI covers the formula — on either side.** The tap has none (**#560**), and
  *this* repo's CI runs no `brew audit` / `brew style` / `brew test` either:
  `Formula/**` sits in no path filter, so a formula-only change triggers nothing
  substantive. Accepted for now, but bounded by what is actually true rather than by
  a CI claim — the formula is **human-reviewed here** (#269), and the mirror is a
  **byte-identical mechanical copy**, not a hand-written second source, so the copy
  cannot silently diverge in content from what was reviewed.
- **The mirror inherits the canonical formula's audit posture, warts included.**
  Byte-identity is a two-way street: `brew audit --strict` currently flags the
  canonical stanza order (`depends_on :macos` before `depends_on "rust" => :build`),
  and the tap reproduces that faithfully — so **#560** will trip on it the day it
  lands. It is also the standing proof of the bullet above: that problem is live in
  canonical *right now*, while CI is green. Accepted: the fix belongs **upstream** in
  `Formula/sessiometer.rb`, then re-mirrors down. Hand-patching the tap to green its
  own CI is exactly what the one-way-mirror rule forbids, because it would fork the
  copy from canonical.

## Related

- Issues: **#558** (this ADR — stand up the tap) · **#269** (the canonical
  in-repo formula — **closed**) · **#172** (the `.app` cask, lands in this same
  tap — **open**) · **#171** (sign + notarize + staple the `.app` — **open**) ·
  **#273** (the "unofficial / not affiliated" neutrality disclaimer, carried by
  the tap's README — **open**) · **#559** (the release-CI one-way sync that will
  keep the mirror current — **open**) · **#560** (tap-repo CI — `brew test-bot` /
  formula audit on PRs to the tap — **open**) · **#15** (redaction — the tap holds
  packaging metadata only: no account emails, tokens, or UUIDs).
- ADRs: **ADR-0010** (macOS app repo topology — "one repo, one notarized
  artifact"; reconciled above, **not** superseded).
- Tap: <https://github.com/sessiometer/homebrew-tap> — `Formula/sessiometer.rb`
  (one-way mirror of the canonical copy), `README.md`, `LICENSE` (MIT).
- Code: `Formula/sessiometer.rb` (canonical, #269) — the `head` stanza the tap
  mirror points at; `README.md` § *Install with Homebrew (CLI / headless channel)*.
