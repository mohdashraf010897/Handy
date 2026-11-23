#!/usr/bin/env node
// Regenerates lowyap raster assets from brand.json + the vector masters in this folder.
//   node design/brand/build.mjs            -> tray/overlay PNGs + app-icon-1024.png
//   node design/brand/build.mjs --check    -> verifies brand.json parity across app/site files
// Then: ./node_modules/.bin/tauri icon design/brand/app-icon-1024.png
// Requires rsvg-convert (brew install librsvg).
import { execFileSync } from "node:child_process";
import { mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const repo = join(here, "..", "..");
const resources = join(repo, "src-tauri", "resources");
const trayDir = join(here, "tray");

export const BRAND = JSON.parse(readFileSync(join(here, "brand.json"), "utf8"));
export const COLORS = BRAND.colors;

// 64-unit grid — "Low-Frequency Terrace" (V2, locked permanent baseline):
// Borderless SF-Symbol-weight menu bar cadence terrace + terminal `_` on macOS/Windows,
// and Machined Carbon keycap housing on colored Linux panels (`handy*.png`).
export const KEYCAP =
  "M18 5H46A13 13 0 0 1 59 18V46A13 13 0 0 1 46 59H18A13 13 0 0 1 5 46V18A13 13 0 0 1 18 5Z";
export const KEYCAP_SKIRT_GROOVE =
  "M25 52.25H39A1.75 1.75 0 0 1 39 55.75H25A1.75 1.75 0 0 1 25 52.25Z";

const pill = (x, y, w, h) => {
  const r = Math.min(w, h) / 2;
  if (w <= h) {
    const cx = x + r;
    return `M${x} ${y + r}A${r} ${r} 0 0 1 ${x + w} ${y + r}V${y + h - r}A${r} ${r} 0 0 1 ${x} ${y + h - r}Z`;
  }
  return `M${x + r} ${y}H${x + w - r}A${r} ${r} 0 0 1 ${x + w - r} ${y + h}H${x + r}A${r} ${r} 0 0 1 ${x + r} ${y}Z`;
};

// Direct 18pt-optimized Low-Frequency Terrace silhouette (3 asymmetric vocal bars + `_` cursor).
export const TERRACE_BARS = [
  pill(8, 26, 7.4, 22),
  pill(20, 13, 7.4, 35),
  pill(32, 20, 7.4, 28),
].join("");
export const TERRACE_UNDERSCORE = pill(43.5, 46, 15.5, 7.2);

// Inset variant for colored Linux keycap plate (`handy*.png`, inside `5..59` squircle).
export const PLATE_BARS = [
  pill(13.5, 25.5, 5.8, 17.5),
  pill(22.5, 15.5, 5.8, 27.5),
  pill(31.5, 21.5, 5.8, 21.5),
].join("");
export const PLATE_UNDERSCORE = pill(40.5, 42, 11.5, 5.4);

const circle = (cx, cy, r) =>
  `M${cx - r} ${cy}A${r} ${r} 0 1 0 ${cx + r} ${cy}A${r} ${r} 0 1 0 ${cx - r} ${cy}Z`;
export const DOTS = [13, 26, 39].map((x) => circle(x, 31, 4.6)).join("");
export const PLATE_DOTS = [19.5, 32, 44.5].map((x) => circle(x, 28, 4.5)).join("");
export const REC_DOT = circle(51.25, 23, 6.5);
export const PLATE_REC_DOT = circle(32, 28, 7.25);
// Warning badge (top-right), with a clean alpha moat cut from underlying strokes.
const BADGE = { cx: 51, cy: 14, r: 10.5, gap: 3.25 };
const BANG = "M49 7.75H53L52.4 16.5H49.6Z" + circle(51, 19.9, 2.05);

const svg = (body, size = 64) =>
  `<svg xmlns="http://www.w3.org/2000/svg" width="${size}" height="${size}" viewBox="0 0 64 64">${body}</svg>\n`;

/**
 * Generates a 64x64 tray glyph SVG.
 * - Default (`!markFill`): pure SF-Symbol-style borderless Low-Frequency Terrace glyph
 *   so 18pt macOS Menu Bar icons stay crisp, airy, and non-messenger alongside Wi-Fi/Control Center.
 * - Colored Linux panel set (`markFill` set): Machined Carbon keycap squircle plate + Warm Stone/Vermilion marks.
 */
function trayGlyph({ state, fg, accent, warning, badge, markFill }) {
  const parts = [];
  if (markFill) {
    const maskId = "m";
    let mask = `<rect width="64" height="64" fill="#fff"/><path d="${KEYCAP_SKIRT_GROOVE}" fill="#000"/>`;
    if (warning) {
      mask += `<circle cx="${BADGE.cx}" cy="${BADGE.cy}" r="${BADGE.r + BADGE.gap}" fill="#000"/>`;
    }
    parts.push(`<mask id="${maskId}">${mask}</mask>`);
    parts.push(`<path d="${KEYCAP}" fill="${fg}" mask="url(#${maskId})"/>`);
    if (state === "recording") {
      parts.push(`<path d="${PLATE_REC_DOT}" fill="${accent}"/>`);
    } else if (state === "idle") {
      parts.push(`<path d="${PLATE_BARS}" fill="${markFill}"/>`);
      parts.push(`<path d="${PLATE_UNDERSCORE}" fill="${accent || markFill}"/>`);
    } else if (state === "transcribing") {
      parts.push(`<path d="${PLATE_DOTS}" fill="${markFill}"/>`);
    }
  } else {
    let fgGroup = "";
    if (state === "idle") {
      fgGroup = `<path d="${TERRACE_BARS}${TERRACE_UNDERSCORE}" fill="${fg}"/>`;
    } else if (state === "recording") {
      fgGroup =
        `<path d="${TERRACE_BARS}" fill="${fg}"/>` +
        `<path d="${REC_DOT}${TERRACE_UNDERSCORE}" fill="${accent || fg}"/>`;
    } else if (state === "transcribing") {
      fgGroup = `<path d="${DOTS}${TERRACE_UNDERSCORE}" fill="${fg}"/>`;
    }
    if (warning) {
      parts.push(
        `<mask id="m"><rect width="64" height="64" fill="#fff"/><circle cx="${BADGE.cx}" cy="${BADGE.cy}" r="${BADGE.r + BADGE.gap}" fill="#000"/></mask>`,
        `<g mask="url(#m)">${fgGroup}</g>`,
      );
    } else {
      parts.push(fgGroup);
    }
  }
  if (warning) {
    parts.push(
      `<mask id="b"><rect width="64" height="64" fill="#fff"/><path d="${BANG}" fill="#000"/></mask>`,
      `<circle cx="${BADGE.cx}" cy="${BADGE.cy}" r="${BADGE.r}" fill="${badge}" mask="url(#b)"/>`,
    );
  }
  return svg(parts.join(""));
}

const { carbon, vermilion, ink, paper, live, liveDeep, warn, warnDeep } = COLORS;
// Dark taskbar/menu bar -> light glyph (tray_*.png); light bar -> dark glyph (*_dark.png).
const TRAY = {
  "tray_idle.png": { state: "idle", fg: paper },
  "tray_recording.png": { state: "recording", fg: paper, accent: live },
  "tray_transcribing.png": { state: "transcribing", fg: paper },
  "tray_idle_warning.png": {
    state: "idle",
    fg: paper,
    warning: true,
    badge: warn,
  },
  "tray_idle_dark.png": { state: "idle", fg: ink },
  "tray_recording_dark.png": { state: "recording", fg: ink, accent: liveDeep },
  "tray_transcribing_dark.png": { state: "transcribing", fg: ink },
  "tray_idle_warning_dark.png": {
    state: "idle",
    fg: ink,
    warning: true,
    badge: warnDeep,
  },
  // Colored theme (Linux panels of any color): Machined Carbon keycap with Warm Stone terrace bars + Signal Vermilion `_`.
  "handy.png": { state: "idle", fg: carbon, markFill: paper, accent: vermilion },
  "recording.png": {
    state: "recording",
    fg: carbon,
    markFill: paper,
    accent: live,
  },
  "transcribing.png": { state: "transcribing", fg: carbon, markFill: paper },
  "handy_warning.png": {
    state: "idle",
    fg: carbon,
    markFill: paper,
    accent: vermilion,
    warning: true,
    badge: warn,
  },
};

function verifyParity() {
  const checks = [
    ["src/lib/brand.ts", BRAND.name],
    ["src/lib/brand.ts", BRAND.tagline],
    ["src/lib/brand.ts", BRAND.homepage],
    ["src-tauri/src/brand.rs", BRAND.name],
    ["src-tauri/src/brand.rs", BRAND.tagline],
    ["src-tauri/src/brand.rs", BRAND.homepage],
    ["src-tauri/tauri.conf.json", BRAND.identifier],
    ["index.html", `<title>${BRAND.name}</title>`],
    ["site/index.html", BRAND.tagline],
    ["site/index.html", COLORS.vermilionUi.toLowerCase()],
  ];
  for (const [rel, needle] of checks) {
    const text = readFileSync(join(repo, rel), "utf8");
    if (!text.includes(needle)) {
      throw new Error(`Brand parity check failed: ${rel} missing "${needle}"`);
    }
  }
  console.log(
    `${BRAND.name}: brand parity check passed (${checks.length} assertions)`,
  );
}

if (process.argv.includes("--check")) {
  verifyParity();
  process.exit(0);
}

mkdirSync(trayDir, { recursive: true });

const render = (svgFile, pngFile, size) =>
  execFileSync("rsvg-convert", [
    "-w",
    String(size),
    "-h",
    String(size),
    "-o",
    pngFile,
    svgFile,
  ]);

for (const [png, spec] of Object.entries(TRAY)) {
  const svgFile = join(trayDir, png.replace(/\.png$/, ".svg"));
  writeFileSync(svgFile, trayGlyph(spec));
  render(svgFile, join(resources, png), 64);
}

render(join(here, "app-icon.svg"), join(here, "app-icon-1024.png"), 1024);
console.log(`${BRAND.name}: wrote 12 tray PNGs and app-icon-1024.png`);
