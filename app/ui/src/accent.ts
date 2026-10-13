// Windows accent colour -> CSS custom properties. Palette index 4 (Dark1) on light, 1 (Light2) on dark.
function lum(hex: string): number {
  const c = [1, 3, 5].map((i) => parseInt(hex.slice(i, i + 2), 16) / 255)
    .map((v) => (v <= 0.03928 ? v / 12.92 : Math.pow((v + 0.055) / 1.055, 2.4)));
  return 0.2126 * c[0] + 0.7152 * c[1] + 0.0722 * c[2];
}
const contrast = (a: number, b: number) => (Math.max(a, b) + 0.05) / (Math.min(a, b) + 0.05);

const DARK_FG = "#10161c";

/** Pure: accent + WCAG-picked foreground, or null for an unusable palette. */
export function pickAccent(palette: string[] | null, dark: boolean): { accent: string; fg: string } | null {
  const accent = palette?.[dark ? 1 : 4];
  if (!accent || !/^#[0-9a-f]{6}$/i.test(accent)) return null;
  const l = lum(accent);
  return { accent, fg: contrast(1, l) >= contrast(l, lum(DARK_FG)) ? "#ffffff" : DARK_FG };
}

export function applyAccent(palette: string[] | null, dark: boolean): void {
  const s = document.documentElement.style;
  const a = pickAccent(palette, dark);
  if (!a) { s.removeProperty("--accent"); s.removeProperty("--accent-fg"); return; } // CSS fallbacks
  s.setProperty("--accent", a.accent);
  s.setProperty("--accent-fg", a.fg);
}
