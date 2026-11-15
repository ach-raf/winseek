// Window transparency (Settings › General › Window transparency): how much of the desktop
// shows through winseek's windows. The choice is stored as a whole percent (40–100) in
// localStorage ("winseek.opacity") and applied by setting --win-alpha on <html>; base.css
// derives the translucent canvas/surface tokens (--bg/--bg2/--bg3/--side, both themes) from
// it with color-mix, so the palette itself stays the single source of truth. The reading
// paper (--paper) deliberately stays opaque. Applied at boot by both windows; a storage event
// carries a change made in one window to the other (main.tsx). No imports so `node --test`
// can load it.

export const OPACITY_MIN = 40;
export const OPACITY_MAX = 100;

const KEY = "winseek.opacity";

export function clampOpacity(pct: number): number {
  return Math.min(OPACITY_MAX, Math.max(OPACITY_MIN, Math.round(pct)));
}

export function loadOpacity(): number {
  try {
    const v = Number(localStorage.getItem(KEY));
    return Number.isFinite(v) && v > 0 ? clampOpacity(v) : OPACITY_MAX;
  } catch {
    return OPACITY_MAX; // storage unavailable: base.css default (--win-alpha: 1) stays
  }
}

export function saveOpacity(pct: number): void {
  try { localStorage.setItem(KEY, String(clampOpacity(pct))); } catch { /* remembered only when storage works */ }
}

export function applyOpacity(pct: number): void {
  const clamped = clampOpacity(pct);
  document.documentElement.style.setProperty("--win-alpha", (clamped / 100).toFixed(2));
}
