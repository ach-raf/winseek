// Accent colour (Settings › Appearance › Accent colour, and custom themes that carry their own).
// `--acc` drives --sel / --sel-line / --mark through color-mix, so setting the two custom
// properties on :root re-themes everything. The choice is remembered in localStorage
// ("winseek.accent") as either a preset id or a raw "#rrggbb" (custom themes hand their accent
// over this way), and applied at boot by both windows; a storage event carries a change made in
// one window to the other. Hex handling borrows theme.ts (this module is not loaded by the
// node tests, which keep theme.ts import-free).
// `ink` is the text colour on accent-filled surfaces (buttons, selected chips): dark on light
// accents, white on deeper ones — presets curate theirs, raw hexes get the luminance pick.

import { normalizeThemeHex } from "./theme";

export const ACCENTS = [
  { id: "amber", label: "Amber", hex: "#ffc533", ink: "#231a00" },
  { id: "coral", label: "Coral", hex: "#ff5b45", ink: "#ffffff" },
  { id: "blue", label: "Blue", hex: "#2f7cf6", ink: "#ffffff" },
  { id: "green", label: "Green", hex: "#16b377", ink: "#ffffff" },
] as const;

export type AccentId = (typeof ACCENTS)[number]["id"];
/** What the setting can hold: a preset id, or a free "#rrggbb" (custom themes, the colour input). */
export type AccentChoice = AccentId | string;

const KEY = "winseek.accent";

/** True when the choice is a free hex rather than a preset id. */
export function isHexAccent(v: AccentChoice): v is string {
  return v.startsWith("#");
}

/** The readable text colour on a filled accent: presets curate theirs; a free hex gets the
 *  luminance pick (YIQ threshold — dark ink on light accents, white on deep ones). */
export function inkFor(hex: string): string {
  const n = normalizeThemeHex(hex);
  if (!n) return "#ffffff";
  const r = parseInt(n.slice(1, 3), 16), g = parseInt(n.slice(3, 5), 16), b = parseInt(n.slice(5, 7), 16);
  return (r * 299 + g * 587 + b * 114) / 1000 / 255 >= 0.6 ? "#24211c" : "#ffffff";
}

export function loadAccent(): AccentChoice {
  try {
    const v = localStorage.getItem(KEY);
    if (v) {
      const hex = normalizeThemeHex(v);
      if (hex) return hex;
      if (ACCENTS.some((a) => a.id === v)) return v as AccentId;
    }
    return "amber";
  } catch {
    return "amber"; // storage unavailable: the base.css default (amber) stays
  }
}

export function saveAccent(v: AccentChoice): void {
  try { localStorage.setItem(KEY, v); } catch { /* remembered only when storage works */ }
}

export function applyAccent(v: AccentChoice): void {
  const preset = ACCENTS.find((x) => x.id === v);
  const hex = preset ? preset.hex : (normalizeThemeHex(v) ?? ACCENTS[0].hex);
  const ink = preset ? preset.ink : inkFor(hex);
  const r = document.documentElement.style;
  r.setProperty("--acc", hex);
  r.setProperty("--acc-ink", ink);
}
