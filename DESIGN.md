# DESIGN.md — lowyap Design System ("Vocal Hardware")

## 1. Visual World & Anti-References

- **Committed World:** **Vocal Hardware** — machined anodized aluminum (`Warm Stone` by day, `Machined Carbon` by night), recessed key dishes, crisp `1px` hairlines, tabular millisecond readouts, and a single physical recording-LED accent (**Signal Vermilion**).
- **Explicit Anti-References:**
  - Generic AI SaaS purple/indigo gradients (`#7C5CFF`, `#5D3AE8`, sparkle icons, glowing neon halos).
  - Toy/utility rounded display fonts (`SF Pro Rounded`, `Comic`, cartoon mascots).
  - Surveillance-bot meeting tools ("lowyap Bot joined the meeting").

## 2. Synchronized Color Tokens (`App` + `Overlay` + `lowyap.com`)

| Role | Token | Light ("Anodized Warm Stone") | Dark ("Machined Carbon") | Verified Contrast |
|---|---|---|---|---|
| Canvas BG | `--color-background` | `#F4F2EC` | `#111113` | — |
| Elevated Card | `--color-surface` | `#FFFFFF` | `#18181C` | Crisp hairline separation |
| Rail / Well | `--color-rail` | `#EAE7DF` | `#0B0B0D` | Recessed chassis |
| Primary Ink | `--color-text` | `#141412` | `#F2F1EE` | `16.48:1` (L) / `16.70:1` (D) |
| Secondary Ink | `--color-ink-2` | `#4A4842` | `#B3B2AC` | `7.40:1` (L) / `9.25:1` (D) |
| Muted / Meta | `--color-ink-3` | `#605D56` | `#908F8A` | `5.31:1` (L) / `5.46:1` (D) |
| Brand / `_` LED | `--color-logo-primary` | `#C0320C` | `#FF6B3D` | `5.07:1` (L) / `6.67:1` (D) |
| Primary CTA Fill | `--color-background-ui` | `#C0320C` (`#A32807` hover) | `#D43B13` (`#B8310D` hover) | `>= 4.68:1` white text |
| Local Seal / Live | `--color-moss` | `#16653E` | `#34D399` | `>= 6.1:1` |
| Warning | `--color-warning` | `#9A5200` | `#FBBF24` | `5.44:1` (L) / `11.15:1` (D) |
| Error | `--color-error` | `#B91C1C` | `#F87171` | `>= 5.8:1` |

## 3. Bundled Typography (`SIL OFL-1.1`, `135 KB` Latin Variable Core)

- **Display (`--font-display`):** `"Bricolage Grotesque Variable"` (`opsz 12..96, wght 200..800`) — expressive vocal ink-traps at hero scale (`lowyap.com`), crisp architectural authority at `26px` Studio workspace titles.
- **UI & Transcript Prose (`--font-ui`):** `"Hanken Grotesk Variable"` (`wght 100..900`, default `440` light / `420` dark anti-halation) — clean open apertures for dense transcripts, settings, and multilingual prose.
- **Hardware & Telemetry (`--font-mono`):** `"Azeret Mono Variable"` (`wght 100..900`, `tabular-nums slashed-zero`) — used strictly for shortcut keycaps (`⌥ Space`), millisecond pipeline telemetry, byte-egress counters (`0 B sent`), and model specs.
- **RTL & Multilingual Parity:** `[dir="rtl"]` and `:lang(ar|ur|he)` enforce `letter-spacing: 0 !important` so connected Perso-Arabic/Nastaliq ligatures never fracture.

## 4. Brand Marks & Native Icons (Permanent Baseline)

- **Standalone Symbol (`VocalKeycapMark` & `app-icon.svg` — "Low-Frequency Terrace"):** Machined double-bevel squircle hardware keycap (`#27272B -> #0E0E10` Machined Carbon skirt, `#141417` spherical top dish, Signal Vermilion `#FF6B3D` front-skirt tactile homing bar) framing four asymmetric Warm Stone vocal-cadence terrace bars ("lowkey yap") stepping down onto a horizontal Signal Vermilion **`_`** terminal underscore ("it types"). Zero chat-bubble messenger silhouette, zero centered symmetric equalizer clone, and zero `#7C5CFF` purple tinting.
- **Wordmark (`HandyTextLogo` & `design/brand/wordmark.svg`):** Optically balanced lowercase `lowyap_` with hooked `l` terminal, machined `w` twin-chevron, signature `y` descender hook, and `var(--color-logo-primary)` `_` cursor.
- **Menu Bar (`TrayIcon` & `tray_*.png`):** SF-Symbol-weight borderless `Low-Frequency Terrace` silhouette (3 asymmetric vocal bars + `_` cursor in AppKit template mode for `idle`, live Signal Vermilion recording LED + `_` for `recording`, and rhythmic 3-dot pulse + `_` for `transcribing`).
