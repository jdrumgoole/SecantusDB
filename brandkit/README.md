# SecantusDB — Brand kit

Visual identity assets for SecantusDB. Open `brand.html` in a browser for the
guided tour.

## Contents

```
mark.svg                          Primary icon, full colour
mark-on-dark.svg                  Icon tuned for dark backgrounds
mark-mono-dark.svg                Single-ink dark
mark-mono-light.svg               Single-ink light (reverse out)
favicon.svg                       Simplified, 16–64px

wordmark-horizontal.svg           Primary lockup (with tagline)
wordmark-horizontal-no-tagline.svg
wordmark-horizontal-on-dark.svg

wordmark-stacked.svg              Vertical lockup, light
wordmark-stacked-on-dark.svg      Vertical lockup, dark
wordmark-text-only.svg            Typography only, no shield

avatar.svg                        Square 1:1, 512×512 — for X / GitHub / LinkedIn
og-image.svg                      Open Graph card, 1200×630

brand.html                        Brand kit reference page
```

All assets are SVG: vector, transparent, infinitely scalable.

## Palette

Slate + cyan, mapping cleanly to Tailwind's `slate-*` and `cyan-*` scales.

| Role             | Token        | Hex      |
|------------------|--------------|----------|
| Shield face      | slate-600    | `#475569` |
| Shield rim       | slate-700    | `#334155` |
| Accent (DB)      | cyan-500     | `#06b6d4` |
| Accent deep      | cyan-700     | `#0e7490` |
| Accent bright    | cyan-400     | `#22d3ee` |
| Ink              | slate-900    | `#0f172a` |

## Typography

Inter, two weights:

- **Wordmark** — Inter 500, letter-spacing −0.02em
- **Tagline** — Inter 400, letter-spacing 0.16em, all caps

Available from Google Fonts. The SVGs `@import` the font; if you need offline
versions, download Inter and update the `font-family` declaration to point at
your local copy (or rasterise the type to paths with `text-to-path` in
something like Inkscape).

## Producing PNG / favicon.ico

The SVGs are the canonical source. To produce raster fallbacks:

```bash
# Single PNG
npx --yes sharp-cli -i mark.svg -o mark.png --width 512

# Multi-resolution favicon.ico
npx --yes svgexport favicon.svg favicon-32.png 32:32
npx --yes png-to-ico favicon-32.png > favicon.ico
```

## Usage

- Use the full-colour mark wherever colour reproduction allows.
- Maintain clear space equal to the height of the "S" in "Secantus" around any
  lockup.
- Minimum size: mark 24px, horizontal lockup with tagline 220px wide, lockup
  without tagline 160px.
- Don't recolour the cyan to other hues, skew, rotate, or re-set the wordmark
  in a different typeface.
