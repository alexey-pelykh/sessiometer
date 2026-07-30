# Sessiometer brand assets

Everything here is generated from two SVG masters. **Never hand-edit a generated
file** — edit the master in `src/` and re-run:

```sh
./brand/generate.sh          # → brand/dist/ + the app's AppIcon.appiconset
```

Requires `rsvg-convert` (`brew install librsvg`) and Google Chrome (for the banner's text).
`brand/dist/` is gitignored; it is reproducible output.

## The mark — "Cycle Gauge"

An open gauge arc, a rotation arrowhead, and a needle reading from a centre
pivot: a meter that cycles. It is the product in one shape — a *session meter*
that rotates the account you're running on.

## The system — the icon is a living instrument

An honest gauge reserves colour for the reading. Sessiometer does the same:

| Layer | Carries | Why |
|---|---|---|
| **Body** (warm graphite) | the **brand** | Zero chroma, so it can never be misread as a status signal. |
| **Mark** (recolours) | the **status** | The *whole mark* takes the status hue — a needle alone is only ~6–8px at 16px and reads grey. |
| **Needle angle** (rotates) | the **status, again** | Position encodes state independently of colour, so the icon stays readable for colour-blind users (green and amber collapse to near-identical grey). |

Status is therefore **doubly encoded**: hue *and* needle position.

## The app icon sits on the macOS grid — the master does not

A macOS app icon is **not full-bleed**: its body occupies a fixed fraction of the
canvas and the system composites the margin around it. Ours shipped at 100 % on
every size until #952 — no margin at all — so it read visibly unlike its neighbours
in the Login Items pane, a surface users treat as a legitimacy signal for a
background login item.

| Dimension | Value |
|---|---|
| Body | **824** of a 1024 canvas — **80.47 %** |
| Inset | **100** px per side |
| Scale applied to the master | **0.8046875** |
| Corner radius after scaling | 184.3, against the template's 185.4 |

Apple's published macOS app-icon template is a 1024 canvas carrying an 824×824 body
with a 185.4 corner radius. Rather than take that on trust, it is corroborated by
measurement: five shipping macOS apps — Calculator, Docker, Notes, Mail, Reminders —
all land on 412×412 of a 512 canvas at the geometric edge, zero variance; a separate
pass over six system apps reproduced the same 80.47 % at a 256 canvas. Two
independent lines, the same number to the pixel.

> **Measure at alpha ≥ 128, not any-alpha.** The any-alpha bounding box also catches
> the antialiased shadow fringe every macOS icon carries, and reads 83.0–83.6 % for
> those five — several points high, and high in a way that looks like a plausible
> answer. The half-covered contour is the real geometric edge. Ours measured 100 % on
> *both* thresholds, which is what made the defect unambiguous.

> **The baked `rx` stays — macOS is not iOS.** Every peer measured reads alpha 0 at
> its body-box corners: if macOS masked, a peer could ship square artwork, and none
> does — the rounding lives in the *artwork*. Dropping `icon.svg`'s `rx="229"` would
> ship a hard-cornered square. The radius was never the defect — 229/1024 = 22.36 %,
> and it rides the scale above down to 184.3 on an 824 body. The icon read
> over-rounded because it was over-**sized**.

**`src/icon.svg` stays a full-bleed shared master.** The inset is applied by an
app-icon-only stage in `generate.sh` (`inset_app_icon`, a textual SVG transform in
the same spirit as `derive`), because only the *app-icon* surfaces want it:

| Surface | Grid |
|---|---|
| `AppIcon.appiconset/`, `Sessiometer.icns` | **inset** — both are macOS app icons |
| `apple-touch-icon.png` | full-bleed — Apple touch icons are full-bleed by convention; the OS masks them |
| `logo.png` | full-bleed — GitHub circle-crops it |
| `icon-<state>_512.png` | full-bleed — the resting tile has to match its four siblings |

Insetting inside `icon.svg` instead would silently degrade every row below the first.

## Tokens

**Body (resting / brand)** — warm graphite `#242320` (gradient `#2c2b27` → `#1b1a17`),
mark in bone `#EDE8DF`. The warmth is deliberate: a cold black + pure white
would read as clinical AI-lab monochrome.

> **The menu bar is monochrome — not colour.** The menu-bar status item ships as a
> **monochrome template**: state is carried by the glyph **shape**, not colour (a
> menu-bar image is system-tinted, so colour cannot encode health there at all — see
> #325). Its four bespoke attention-state glyphs are the `.symbolset` family — #437
> artwork, #524 taxonomy — which `generate.sh` writes straight into
> `apps/menubar/Sources/Assets.xcassets/`. The free-standing *colour* menu-bar glyph
> sets (two contrast sets, needle tracking the reading) were **retired in #439**: they
> targeted a colour bar the app does not — and cannot — use. `src/glyph.svg` is kept
> only as the **archived** colour-glyph master: no longer emitted, no longer consumed.
> The colour "living instrument" below still governs the **app icon, Dock, and
> in-panel** surfaces; only the **menu bar** is monochrome.

**Living icon states.** The colour icon sits on a *controlled tile* (app icon, Dock,
DMG, Homebrew, docs & screenshots), which removes wallpaper bleed — so it always uses
the **vivid** set. No contrast-darkened companion set exists any more; that set had
exactly one consumer, the free-standing colour bar glyph retired above.

| State | Needle | Vivid |
|---|---|---|
| Healthy | full, up-left | `#30D158` |
| Warning | half, straight up | `#FF9F0A` |
| Critical | redline, up-right | `#FF453A` |
| Swapping *(transient)* | resting pose | `#0A84FF` |
| Resting *(no reading)* | resting pose | bone mark `#EDE8DF` on the graphite body |

## Outputs

| File | Where it goes |
|---|---|
| `AppIcon.appiconset/` | written straight into `apps/menubar/Sources/Assets.xcassets/` (**inset** — see the grid above) |
| `Sessiometer.icns` | DMG / Finder (**inset**) |
| `logo.png` (512²) | `sessiometer/.github` → `profile/assets/logo.png` |
| `social-preview.png` (1200×630) | GitHub social preview + `profile/assets/` |
| `og-image.png` (1200×630) | `sessiometer.github.io` → `public/` |
| `favicon.svg`, `favicon-32.png`, `apple-touch-icon.png` | `sessiometer.github.io` → `public/` |
| `Gauge{Healthy,Connecting,Attention,NoRunway}.symbolset/` | written straight into `apps/menubar/Sources/Assets.xcassets/` (monochrome bar glyphs — #437) |
| `icon-<state>_512.png` | the living icon, for docs & screenshots |

## Guardrails

The mark uses no provider colour or trademark. Sessiometer is **unofficial and
not affiliated with Anthropic**; the status triad is ordinary traffic-light
meter semantics and belongs to no brand.
