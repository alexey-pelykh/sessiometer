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
| `AppIcon.appiconset/` | written straight into `apps/menubar/Sources/Assets.xcassets/` |
| `Sessiometer.icns` | DMG / Finder |
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
