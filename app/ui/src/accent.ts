// Accent colour presets (Settings › General › Accent colour). `--acc` drives --sel / --sel-line /
// --mark through color-mix, so setting the two custom properties on :root re-themes everything.
// The choice is remembered in localStorage ("winseek.accent") and applied at boot by both windows;
// a storage event carries a change made in one window to the other. No imports so `node --test`
// can load it.
// `ink` is the text colour on accent-filled surfaces (buttons, selected chips): dark on the light
// amber, white on the three deeper colours.

export const ACCENTS = [
  { id: "amber", label: "Amber", hex: "#ffc533", ink: "#231a00" },
  { id: "coral", label: "Coral", hex: "#ff5b45", ink: "#ffffff" },
  { id: "blue", label: "Blue", hex: "#2f7cf6", ink: "#ffffff" },
  { id: "green", label: "Green", hex: "#16b377", ink: "#ffffff" },
] as const;

export type AccentId = (typeof ACCENTS)[number]["id"];

const KEY = "winseek.accent";

export function loadAccent(): AccentId {
  try {
    const v = localStorage.getItem(KEY);
    return ACCENTS.some((a) => a.id === v) ? (v as AccentId) : "amber";
  } catch {
    return "amber"; // storage unavailable: the base.css default (amber) stays
  }
}

export function saveAccent(id: AccentId): void {
  try { localStorage.setItem(KEY, id); } catch { /* remembered only when storage works */ }
}

export function applyAccent(id: AccentId): void {
  const a = ACCENTS.find((x) => x.id === id) ?? ACCENTS[0];
  const r = document.documentElement.style;
  r.setProperty("--acc", a.hex);
  r.setProperty("--acc-ink", a.ink);
}
