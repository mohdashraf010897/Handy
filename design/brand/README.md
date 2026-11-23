# lowyap brand ("Vocal Hardware" — Low-Frequency Terrace)

**lowyap** — always lowercase ("lowkey" + "yap"). Tagline: _lowkey yap. it types._

## Concept

An anodized Machined Carbon double-bevel hardware keycap (`#27272B -> #0E0E10` skirt, `#141417` recessed dish, tactile Signal Vermilion homing bar) framing four asymmetric Warm Stone vocal-cadence terrace bars ("lowkey yap") stepping down onto a horizontal Signal Vermilion **`_`** terminal underscore LED ("it types"). Zero chat-bubble messenger silhouette, zero centered symmetric equalizer clone, and zero SaaS purple.

| State        | Menu-bar glyph (`tray_*.png` / `TrayIcon.tsx`)                  |
| ------------ | --------------------------------------------------------------- |
| idle         | 3 asymmetric terrace bars + `_` cursor (AppKit alpha template)  |
| recording    | 3 terrace bars + live Signal Vermilion LED dot & `_` cursor     |
| transcribing | three rhythmic dots `…` + `_` cursor (AppKit alpha template)    |
| warning      | amber badge with `!`, top-right, cleanly gap-cut                |

## Colors (`design/brand/brand.json`)

| Token            | Hex       | Use                                                           |
| ---------------- | --------- | ------------------------------------------------------------- |
| vermilion        | `#FF6B3D` | dark-mode brand & `_` cursor LED                              |
| vermilionUi      | `#C0320C` | light-mode primary CTA & `_` cursor (`>= 5.07:1` WCAG AA)     |
| vermilionUiHover | `#A32807` | light-mode primary hover                                      |
| carbon           | `#111113` | dark canvas & keycap housing                                  |
| stone            | `#F4F2EC` | light canvas & sculpted `y` legend                            |
| ink              | `#141412` | primary light-mode text & dark tray template                  |
| paper            | `#FFFFFF` | elevated light surface & light tray template                  |
| live             | `#FF5A26` | recording dot (`#C0320C` on light bars)                       |
| warn             | `#FBBF24` | warning badge (`#9A5200` on light bars)                       |

## Files

- `brand.json` — single source of truth for name, tagline, bundle ID, URLs, and color tokens (verified by `cargo test` and `node design/brand/build.mjs --check`).
- `app-icon.svg` — 1024 master (Apple HIG grid: 824 px squircle plate, 100 px inset, 185 px corners, top-lit obsidian gradient + inner specular rim).
- `tray-glyph.svg` — idle tray glyph master (single even-odd path, alpha only).
- `wordmark.svg` — "lowyap" wordmark (`394x108` viewBox).
- `tray/*.svg` — generated per-file tray sources (do not hand-edit; edit `build.mjs`).
- `build.mjs` — generator and parity verifier for all rasters.

## Regenerate

```sh
node design/brand/build.mjs                                   # 12 tray/overlay PNGs (64x64) + app-icon-1024.png
node design/brand/build.mjs --check                           # verifies brand.json parity across app/site
./node_modules/.bin/tauri icon design/brand/app-icon-1024.png # rewrites src-tauri/icons/*
cp design/brand/app-icon-1024.png src-tauri/icons/logo.png    # tauri icon does not write logo.png
rm -rf src-tauri/icons/android/mipmap-anydpi-v26 src-tauri/icons/android/values  # extras tauri adds; unused
file src-tauri/icons/icon.png                                 # must stay 512 x 512
```

Tray files and semantics (`src-tauri/src/tray.rs`, `get_icon_path` & `is_template_icon`):
`tray_*.png` are light glyphs for dark bars, `tray_*_dark.png` dark glyphs for light bars,
`handy*.png` / `recording.png` / `transcribing.png` are the colored (Linux) set.
On macOS, `Idle` and `Transcribing` use `as_template = true` (pure AppKit alpha masks), while
`Recording` and `Idle + warning` switch `as_template = false` so the live coral-red dot and
amber Secure Input `!` badge retain semantic color while still cutting out crisp alpha moats.

## Geometry for `TrayIcon.tsx` & `tray_*.png` (`64x64` viewBox)

```text
bar 1 (mid)     rect x=8    y=26 width=7.4  height=22  rx=3.7
bar 2 (peak)    rect x=20   y=13 width=7.4  height=35  rx=3.7
bar 3 (fall)    rect x=32   y=20 width=7.4  height=28  rx=3.7
underscore `_`  rect x=43.5 y=46 width=15.5 height=7.2 rx=3.6
recording LED   circle cx=51.25 cy=23 r=6.5 (#FF5A26 dark bar / #C0320C light bar)
transcribing    circles cx=13,26,39 cy=31 r=4.6 + underscore `_`
```
