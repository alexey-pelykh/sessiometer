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

## Tokens

**Body (resting / brand)** — warm graphite `#242320` (gradient `#2c2b27` → `#1b1a17`),
mark in bone `#EDE8DF`. The warmth is deliberate: a cold black + pure white
would read as clinical AI-lab monochrome.

> **Superseded — menu-bar status item.** The menu-bar status item now ships as a
> **monochrome template**: state is carried by the glyph **shape**, not colour (a
> menu-bar image is system-tinted, so colour cannot encode health — see #325). The
> free-standing *colour* menu-bar glyph sets described below are being retired — #437
> draws the bespoke monochrome bar glyphs, #439 removes the colour sets from
> `generate.sh`. The colour "living instrument" still governs the **app icon, Dock,
> and in-panel** surfaces; only the **menu bar** is monochrome.

**Status.** The menu-bar glyph is a free-standing *colored* (non-template) image,
so it must clear contrast against the bar it sits on:

| State | Needle | Dark bar (vivid) | Light bar (darkened) |
|---|---|---|---|
| Healthy | full, up-left | `#30D158` | `#248A3D` |
| Warning | half, straight up | `#FF9F0A` | `#9A6A00` |
| Critical | redline, up-right | `#FF453A` | `#FF3B30` |
| Swapping *(transient)* | resting pose | `#0A84FF` | `#007AFF` |
| Resting *(no reading)* | resting pose | template image — macOS tints it | |

Raw system green/amber measure ~2.0–2.2:1 on a **light** bar and visually
dissolve, so light bars get the darkened set. On a **dark** bar the darkened
amber reads muddy-brown, so dark bars get the vivid set. If bar appearance
can't be detected (wallpaper bleed through a translucent bar), ship the
**light-bar set universally** — it clears 3:1 at *both* luminance extremes.

Large surfaces (Dock, README, DMG, Homebrew) sit on a controlled tile, which
removes wallpaper bleed — they always use the **vivid** set.

The **resting** menu-bar glyph ships as a template image (`isTemplate = true`);
macOS tints it to the system label colour. The rendered `-lightbar`/`-darkbar`
resting variants exist only for mockups and screenshots.

## Outputs

| File | Where it goes |
|---|---|
| `AppIcon.appiconset/` | written straight into `apps/menubar/Sources/Assets.xcassets/` |
| `Sessiometer.icns` | DMG / Finder |
| `logo.png` (512²) | `sessiometer/.github` → `profile/assets/logo.png` |
| `social-preview.png` (1200×630) | GitHub social preview + `profile/assets/` |
| `og-image.png` (1200×630) | `sessiometer.github.io` → `public/` |
| `favicon.svg`, `favicon-32.png`, `apple-touch-icon.png` | `sessiometer.github.io` → `public/` |
| `glyph-<state>-<lightbar\|darkbar>.png` (@1x/@2x) | menu-bar status glyphs |
| `icon-<state>_512.png` | the living icon, for docs & screenshots |

## Known open item

`#9A6A00` (light-bar warning) is a dark goldenrod. It clears contrast and is
clearly distinct from red and green, but it wants a taste pass on a real display
to confirm it still reads "amber" and not "brown".

## Guardrails

The mark uses no provider colour or trademark. Sessiometer is **unofficial and
not affiliated with Anthropic**; the status triad is ordinary traffic-light
meter semantics and belongs to no brand.
