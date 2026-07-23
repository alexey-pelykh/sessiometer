---
type: architecture-decision-record
number: 27
title: "macOS app bundle identity — freeze org.sessiometer.menubar on the <surface> axis"
date: 2026-07-23
status: accepted
decision_makers: [Oleksii PELYKH (maintainer)]
---

# ADR-0027: macOS app bundle identity — freeze `org.sessiometer.menubar` on the `<surface>` axis

## Status

**Accepted** — 2026-07-23. Records the frozen `CFBundleIdentifier` of the macOS
menu-bar app (**#168**) and the family-namespace model it belongs to, so a
contributor does not re-litigate the id — or, worse, change it after the first
signed release. It resolves the `PROVISIONAL / #170-spike-gated` marker that had
sat in `apps/menubar/project.yml` since the bundle-identity reconcile (**#327**).

Unlike ADR-0010 (repo topology) and ADR-0021 (tap topology), which record
*structural* decisions that precede code, this ADR records a **one-way-door
identity freeze**: the id must be fixed **before #171** cuts the first signed,
notarized release, because Sparkle auto-update refuses to update across a changed
`CFBundleIdentifier` (and the softer identity couplings below make a post-release
change user-hostile regardless). The decision was reached across an **empirical
spike** (#170) and a **`/council`** (2026-07-23).

## Context

The macOS app's identity sits inside the **`org.sessiometer`** reverse-DNS
family, ratified 2026-07-06 (grounded by the owned `sessiometer.org` domain; #327,
#329) as a **product-FAMILY namespace — never itself a concrete bundle id**. Every
artifact is a **peer child** on a `<surface>` axis:

- the menu-bar app → `org.sessiometer.menubar`
- the embedded daemon LaunchAgent → `org.sessiometer.agent` (**#329**, `src/service.rs`)
- future siblings → `.cli` / `.web` / `.tui` (per `../hq/strategy/design-menubar.md`)

The app id was **provisional** pending **#170**, which registers the daemon's
LaunchAgent from the app via `SMAppService.agent(plistName:)`. The open question
(a `/council` 2026-07-06 resolution-test): **does SMAppService require the agent's
launchd `Label` to be prefixed by the app's `CFBundleIdentifier`?** If yes, the app
id would be *mechanically forced* to bare `org.sessiometer` (so it prefixes
`org.sessiometer.agent`). The provisional marker named that fallback.

**The #170 spike (2026-07-23) settled it empirically.** A real
Developer-ID-signed `.app` with `CFBundleIdentifier = org.sessiometer.menubar`
(a *peer* of, not a prefix-ancestor of, `org.sessiometer.agent`) registered the
agent via `SMAppService.agent().register()` → status `notFound → enabled`; launchd
showed `gui/<uid>/org.sessiometer.agent` with `parent bundle identifier =
org.sessiometer.menubar`, `managed_by = com.apple.xpc.ServiceManagement`; clean
`unregister()`. **Verdict: NO prefix-coupling** — any peer id works; the bare-`org`
fallback is not needed.

With the mechanical constraint removed, the id is a pure convention/coherence
choice, so a `/council` (2026-07-23) deliberated `.menubar` vs `.app`.

## Decision

**Freeze `PRODUCT_BUNDLE_IDENTIFIER = org.sessiometer.menubar`.**

The suffix names the app's **surface** (`menubar`), consistent with the ratified
`org.sessiometer.<surface>` family axis and the frozen peer `org.sessiometer.agent`.
It is the incumbent value, the exact id the #170 spike validated, and it does not
change here — this ADR removes the `PROVISIONAL` marker and records *why* the id is
now fixed.

**Council synthesis** (3 heterogeneous panelists, parallel-independent; DiscoUQ:
DIVERGENT-DEEP 2–1, all HIGH confidence, **FALSIFIER-CONVERGENT**):

- **macOS-platform lens** → `.menubar`. The suffix is mechanically cosmetic (TCC,
  UserNotifications identity, Sparkle appcast, notarization ticket all key on the
  *whole* id; no keychain-access-group coupling — the app links no
  `Security.framework`, #327 / ADR-0002). Given pure convention, coherence with the
  dated-ratified `<surface>` axis decides it.
- **release / regret lens** → `.menubar`. Asymmetric payoff: switching buys zero
  distribution benefit and costs churn (re-touch closed #327/#329 + hq docs) plus
  invalidating the empirical spike; nothing bearing `.app` is released, so
  `.menubar` is unambiguously the incumbent.
- **naming / durability lens** → `.app` (dissent). A frozen id should name the
  durable **role** (the GUI channel, peer of the `.agent` daemon) not today's
  **form factor**; `menubar` was scaffold-inherited (#311) and could age into a
  misnomer if the app grows a windowed/Dock presence.

The panel **converged on the single fact that decides it**: the platform lens's own
falsifier (*"if the app becomes a windowed/Dock general app → `.app`"*) is exactly
the premise the dissent rests on. The discriminating question — **is the app
permanently menu-bar-only?** — was resolved **YES** by the maintainer: the product's
design SSOT commits to menu-bar form (`design-menubar.md` IA scope guard "display +
manual-swap only"; `LSUIElement=YES`; the Cycle-Gauge brand identity is
menu-bar-centric). Under that commitment `.menubar` is both accurate and durable.

**This id is decoupled from the #172 Homebrew cask token.** The cask token
(`sessiometer` shared-with-formula vs distinct `sessiometer-app`) is a user-facing
install-name decision; the bundle id is a reverse-DNS identity users never see.
They must be chosen on their own merits — do not "align" them and reopen this door.

## Alternatives considered

1. **Bare `org.sessiometer`** (the provisional fallback). Would have been *forced*
   only if the #170 spike showed SMAppService prefix-coupling. **Empirically
   excluded** — the spike proved a peer id registers the agent fine. Also wrong on
   principle: `org.sessiometer` is the family *namespace*, never a concrete id.
2. **`org.sessiometer.app`** (the council dissent). Names the durable role; immune
   to form-factor drift. **Rejected** because (a) it breaks the ratified `<surface>`
   axis where every peer names a surface/channel, (b) the maintainer committed the
   app to permanent menu-bar-only form (removing the drift the dissent guards
   against), (c) it reads as `Sessiometer.app → org.sessiometer.app` ("app.app") and
   monopolizes the generic slot a genuinely distinct second GUI app might want, and
   (d) it needlessly churns a ratified, empirically-validated incumbent. The dissent
   remains the correct choice **if** that menu-bar-only commitment is ever reversed —
   see the falsifier in Consequences.
3. **A placement/platform suffix (`.desktop`, `.mac`, `.gui`)**. Rejected: `.desktop`
   / `.mac` reintroduce the same aging problem as `menubar` in a larger box; `.gui`
   is jargon, and "app vs agent" is Apple's own idiom.

## Consequences

### Positive

- **The one-way door is closed on the record**, before #171 signs anything. No
  post-release `CFBundleIdentifier` change can strand existing installs' Sparkle
  updates, TCC grants (Screen Recording + Accessibility), or `UserDefaults`.
- **Family coherence holds.** `org.sessiometer.menubar` + `org.sessiometer.agent`
  read as an idiomatic `<surface>` pair, extensible to `.cli` / `.web` / `.tui`.
- **Zero churn.** The incumbent value is unchanged; the spike's validation stands.
- **The decision now travels.** Previously it lived only in issue comments (#327,
  #170, #329) + `project.yml` prose — the "constraint carried as prose" anti-pattern
  on a textbook one-way door. This ADR is the durable, citable home.

### Negative / trade-offs

- **`menubar` names a form factor, not a role.** Accepted: the product is
  committed to menu-bar-only form (design SSOT + brand), so the descriptor stays
  accurate. The #268 Settings window and #438 Dock/Finder/About AppIcon are standard
  menu-bar-utility surfaces, **not** a pivot to a windowed app.
- **Reversal falsifier.** This decision's correctness rests on the menu-bar-only
  commitment. **If** a future roadmap ships the app as a windowed/Dock-present
  general application (dropping `LSUIElement`) **or** a *second, distinct native GUI
  app* joins the family, `.menubar` becomes either a frozen misnomer or an
  under-specific id — but by then it is frozen (Sparkle), so the cost is a forced,
  user-hostile migration. Accepted as low-probability given the product's identity.

## Related

- Issues: **#170** (the SMAppService spike that gated this id — **open**; this ADR
  records its spike verdict), **#171** (first signed/notarized release — the freeze
  deadline — **open**), **#329** (`org.sessiometer.agent` peer label — **closed**),
  **#327** (bundle-identity reconcile — **closed**), **#172** (Homebrew cask token,
  explicitly **decoupled** — **open**), **#311** (the scaffold that seeded the
  `menubar` token — **closed**).
- Code / config: `apps/menubar/project.yml` (`PRODUCT_BUNDLE_IDENTIFIER`, and the
  frozen-identity comment this ADR de-provisionalizes); `src/service.rs`
  (`AGENT_LABEL = "org.sessiometer.agent"`).
- Prior art: **ADR-0010** (repo topology — one repo, one notarized artifact),
  **ADR-0021** (tap topology), **ADR-0002** (keychain via CLI, zero FFI — why no
  keychain-access-group couples to the id). Design SSOT: `../hq/strategy/design-menubar.md`.
