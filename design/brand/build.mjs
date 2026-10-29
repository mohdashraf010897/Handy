#!/usr/bin/env node
// Regenerates lowyap raster assets from the vector masters in this folder.
//   node design/brand/build.mjs            -> tray/overlay PNGs + app-icon-1024.png
// Then: ./node_modules/.bin/tauri icon design/brand/app-icon-1024.png
// Requires rsvg-convert (brew install librsvg).
import { execFileSync } from "node:child_process";
import { mkdirSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const repo = join(here, "..", "..");
const resources = join(repo, "src-tauri", "resources");
const trayDir = join(here, "tray");
mkdirSync(trayDir, { recursive: true });

export const COLORS = {
  violet: "#7C5CFF",
  ink: "#121117",
  paper: "#FFFFFF",
  live: "#FF3B4E",
  liveDeep: "#E5203A",
  warn: "#FFB020",
  warnDeep: "#E08600",
};

// 64-unit grid. One outline: rounded body whose bottom-left corner drops into a low tail.
export const BUBBLE =
  "M20 6H44A15 15 0 0 1 59 21V31A15 15 0 0 1 44 46H25.5L11.6 56.4Q7.6 59.4 8 54.4L8.6 46.6A15 15 0 0 1 5 37V21A15 15 0 0 1 20 6Z";
// Inner marks, also on the 64 grid (cut out of the bubble in mono variants).
export const CURSOR = "M23 30H41A4 4 0 0 1 41 38H23A4 4 0 0 1 23 30Z";
const circle = (cx, cy, r) =>
  `M${cx - r} ${cy}A${r} ${r} 0 1 0 ${cx + r} ${cy}A${r} ${r} 0 1 0 ${cx - r} ${cy}Z`;
export const DOTS = [19.5, 32, 44.5].map((x) => circle(x, 26, 4.75)).join("");
export const REC_RING = circle(32, 26, 12.5);
export const REC_DOT = circle(32, 26, 8);
// Warning badge (top-right), with a gap cut from the bubble.
const BADGE = { cx: 51, cy: 13, r: 11, gap: 3.5 };
const BANG = "M49 6.5H53L52.4 15.5H49.6Z" + circle(51, 19, 2.1);

const svg = (body, size = 64) =>
  `<svg xmlns="http://www.w3.org/2000/svg" width="${size}" height="${size}" viewBox="0 0 64 64">${body}</svg>\n`;

/**
 * A tray glyph. `fg` paints the bubble; marks are cut out (template-safe alpha).
 * `accent` (optional) paints the live dot / badge in color for Windows/Linux;
 * macOS renders these as template images, so only alpha matters there.
 */
function trayGlyph({ state, fg, accent, warning, badge, markFill }) {
  const parts = [];
  const maskId = "m";
  let mask = `<rect width="64" height="64" fill="#fff"/>`;
  if (warning)
    mask += `<circle cx="${BADGE.cx}" cy="${BADGE.cy}" r="${BADGE.r + BADGE.gap}" fill="#000"/>`;
  let marks = "";
  if (state === "idle") marks = CURSOR;
  if (state === "transcribing") marks = DOTS;
  if (state === "recording") marks = REC_RING;
  if (!markFill) mask += `<path d="${marks}" fill="#000"/>`;
  parts.push(`<mask id="${maskId}">${mask}</mask>`);
  parts.push(`<path d="${BUBBLE}" fill="${fg}" mask="url(#${maskId})"/>`);
  if (markFill) {
    // Colored (Linux) variant: draw marks instead of cutting them out.
    if (state === "recording")
      parts.push(`<path d="${REC_DOT}" fill="${accent}"/>`);
    else parts.push(`<path d="${marks}" fill="${markFill}"/>`);
  } else if (state === "recording") {
    parts.push(`<path d="${REC_DOT}" fill="${accent || fg}"/>`);
  }
  if (warning) {
    parts.push(
      `<mask id="b"><rect width="64" height="64" fill="#fff"/><path d="${BANG}" fill="#000"/></mask>`,
      `<circle cx="${BADGE.cx}" cy="${BADGE.cy}" r="${BADGE.r}" fill="${badge}" mask="url(#b)"/>`,
    );
  }
  return svg(parts.join(""));
}

const { violet, ink, paper, live, liveDeep, warn, warnDeep } = COLORS;
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
  // Colored theme (Linux panels of any color): violet bubble with white marks.
  "handy.png": { state: "idle", fg: violet, markFill: paper },
  "recording.png": {
    state: "recording",
    fg: violet,
    markFill: paper,
    accent: live,
  },
  "transcribing.png": { state: "transcribing", fg: violet, markFill: paper },
  "handy_warning.png": {
    state: "idle",
    fg: violet,
    markFill: paper,
    warning: true,
    badge: warn,
  },
};

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
console.log("lowyap: wrote 12 tray PNGs and app-icon-1024.png");
