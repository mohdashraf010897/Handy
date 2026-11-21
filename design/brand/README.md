# lowyap brand

**lowyap** — always lowercase ("lowkey" + "yap"). Tagline: _lowkey yap. it types._

## Concept

A speech bubble whose tail sits **low** (bottom-left), with a typing cursor
(`_`) resting low inside it: you talk, it types. Original artwork, drawn on a
64-unit grid; it shares nothing with the upstream Handy name, mascot or icons.

| State        | Mark inside the bubble                   |
| ------------ | ---------------------------------------- |
| idle         | cursor `_` (cut out)                     |
| recording    | ringed "live" dot (red on Windows/Linux) |
| transcribing | three dots `…` (cut out)                 |
| warning      | amber badge with `!`, top-right, gap-cut |

The mascot is the same bubble with two eyes and a cursor for a mouth.
The wordmark is monoline lowercase (11-unit stroke, round caps) followed by
the violet cursor.

## Colors (`design/brand/brand.json`)

| Token         | Hex       | Use                                                         |
| ------------- | --------- | ----------------------------------------------------------- |
| violet        | `#7C5CFF` | brand mark accent: app icon bubble, wordmark cursor         |
| violetUi      | `#6946E5` | interactive primary UI & light-mode overlay (WCAG AA 5.5:1) |
| violetUiHover | `#5B38D6` | primary button hover                                        |
| violetLight   | `#B9A8FF` | soft washes & light-mode decorative fills                   |
| violetDark    | `#A08CFF` | dark-mode brand & overlay accent (5.4:1 on `#2c2b29`)       |
| ink           | `#121117` | app-icon plate base (`#24222E` -> `#121117`), dark tray     |
| paper         | `#FFFFFF` | light tray glyphs, cursor on violet                         |
| live          | `#FF3B4E` | recording dot (`#E5203A` on light bars)                     |
| warn          | `#FFB020` | warning badge (`#E08600` on light bars)                     |

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

## Paths for `TrayIcon.tsx` (64x64 viewBox, `fillRule="evenodd"`)

```text
bubble       M20 6H44A15 15 0 0 1 59 21V31A15 15 0 0 1 44 46H25.5L11.6 56.4Q7.6 59.4 8 54.4L8.6 46.6A15 15 0 0 1 5 37V21A15 15 0 0 1 20 6Z
idle         bubble + M23 30H41A4 4 0 0 1 41 38H23A4 4 0 0 1 23 30Z
transcribing bubble + M14.75 26A4.75 4.75 0 1 0 24.25 26A4.75 4.75 0 1 0 14.75 26ZM27.25 26A4.75 4.75 0 1 0 36.75 26A4.75 4.75 0 1 0 27.25 26ZM39.75 26A4.75 4.75 0 1 0 49.25 26A4.75 4.75 0 1 0 39.75 26Z
recording    bubble + M18.75 26A13.25 13.25 0 1 0 45.25 26A13.25 13.25 0 1 0 18.75 26Z (+ live #FF3B4E dot r=7.5 at 32,26)
```
