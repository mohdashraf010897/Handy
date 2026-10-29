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

## Colors

| Token  | Hex       | Use                                     |
| ------ | --------- | --------------------------------------- |
| violet | `#7C5CFF` | the one accent: bubble, wordmark cursor |
| ink    | `#121117` | app-icon plate, dark tray glyphs        |
| paper  | `#FFFFFF` | light tray glyphs, cursor on violet     |
| live   | `#FF3B4E` | recording dot (`#E5203A` on light bars) |
| warn   | `#FFB020` | warning badge (`#E08600` on light bars) |

Violet contrast: 4.2:1 on `#fbfbfb`, 3.3:1 on `#2c2b29`, 4.3:1 under white.
The in-app theme tokens in `src/styles/theme.css` are still Handy pink; moving
`--*-color-logo-primary` / `--color-background-ui` to violet is a follow-up.

## Files

- `app-icon.svg` — 1024 master (macOS grid: 824 px plate, 100 px inset, ~185 px corners, soft shadow).
- `tray-glyph.svg` — idle tray glyph master (single even-odd path, alpha only).
- `wordmark.svg` — "lowyap" wordmark.
- `tray/*.svg` — generated per-file tray sources (do not hand-edit; edit `build.mjs`).
- `build.mjs` — generator for all rasters.

## Regenerate

```sh
node design/brand/build.mjs                                   # 12 tray/overlay PNGs (64x64) + app-icon-1024.png
./node_modules/.bin/tauri icon design/brand/app-icon-1024.png # rewrites src-tauri/icons/*
cp design/brand/app-icon-1024.png src-tauri/icons/logo.png    # tauri icon does not write logo.png
rm -r src-tauri/icons/android/mipmap-anydpi-v26 src-tauri/icons/android/values  # extras tauri adds; unused
file src-tauri/icons/icon.png                                 # must stay 512 x 512
```

Tray files and semantics (`src-tauri/src/tray.rs`, `get_icon_path`): `tray_*.png`
are light glyphs for dark bars, `tray_*_dark.png` dark glyphs for light bars,
`handy*.png` / `recording.png` / `transcribing.png` are the colored (Linux) set.
macOS loads them as template images, so every state must read from alpha alone:
marks are cut out, never painted in a second color.

## Paths for `TrayIcon.tsx` (64x64 viewBox, `fillRule="evenodd"`)

```text
bubble       M20 6H44A15 15 0 0 1 59 21V31A15 15 0 0 1 44 46H25.5L11.6 56.4Q7.6 59.4 8 54.4L8.6 46.6A15 15 0 0 1 5 37V21A15 15 0 0 1 20 6Z
idle         bubble + M23 30H41A4 4 0 0 1 41 38H23A4 4 0 0 1 23 30Z
transcribing bubble + M14.75 26A4.75 4.75 0 1 0 24.25 26A4.75 4.75 0 1 0 14.75 26ZM27.25 26A4.75 4.75 0 1 0 36.75 26A4.75 4.75 0 1 0 27.25 26ZM39.75 26A4.75 4.75 0 1 0 49.25 26A4.75 4.75 0 1 0 39.75 26Z
recording    bubble + M19.5 26A12.5 12.5 0 1 0 44.5 26A12.5 12.5 0 1 0 19.5 26ZM24 26A8 8 0 1 0 40 26A8 8 0 1 0 24 26Z
```
