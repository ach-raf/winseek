// Theme registry (Settings › Appearance › Theme). "system" follows Windows; every other entry
// names one concrete palette, and each palette declares the mode it belongs to — applyTheme
// sets `data-theme` on <html> to that resolved palette id, which is what the palette blocks in
// base.css key off ("light" and "dark" are palettes too, the originals; "latte" and "mocha" are
// the Catppuccin pair). "system" itself never lands in data-theme: it is resolved here, through
// matchMedia, to the original palette of the OS's mode, so the OS choice stays live (see
// watchSystemTheme). The choice is remembered in localStorage ("winseek.theme") and applied at
// boot by both windows; a storage event carries a change made in one window to the other
// (main.tsx), like the accent. In Tauri the native window chrome follows the palette's mode
// (library-window titlebar). No imports so `node --test` can load it.

export const THEMES = [
  { id: "system", label: "System" },
  { id: "light", label: "Light" },
  { id: "dark", label: "Dark" },
  { id: "latte", label: "Latte" },
  { id: "mocha", label: "Mocha" },
] as const;

export type ThemeId = (typeof THEMES)[number]["id"];
export type ConcreteTheme = Exclude<ThemeId, "system">;

/** Which family a concrete palette belongs to: "system" resolves to the family's original
 *  ("light" or "dark"), and the native window chrome follows the mode. */
const MODE: Record<ConcreteTheme, "light" | "dark"> = {
  light: "light", latte: "light", dark: "dark", mocha: "dark",
};

export function themeMode(id: ConcreteTheme): "light" | "dark" {
  return MODE[id];
}

const KEY = "winseek.theme";

export function loadTheme(): ThemeId {
  try {
    const v = localStorage.getItem(KEY);
    return THEMES.some((t) => t.id === v) ? (v as ThemeId) : "system";
  } catch {
    return "system"; // storage unavailable: follow the OS (base.css ships light first)
  }
}

export function saveTheme(id: ThemeId): void {
  try { localStorage.setItem(KEY, id); } catch { /* remembered only when storage works */ }
}

/** The concrete palette a setting means right now ("system" = the OS mode's original palette).
 *  Pure. */
export function resolveTheme(id: ThemeId, osPrefersDark: boolean): ConcreteTheme {
  if (id === "system") return osPrefersDark ? "dark" : "light";
  return id;
}

export function applyTheme(id: ThemeId): void {
  let osPrefersDark = false;
  try {
    osPrefersDark = window.matchMedia("(prefers-color-scheme: dark)").matches;
  } catch { /* no matchMedia: OS counts as light */ }
  const resolved = resolveTheme(id, osPrefersDark);
  document.documentElement.dataset.theme = resolved;
  // The native window chrome follows the palette's mode; on "system" hand the decision back to
  // the OS. Best effort: the browser mock has no Tauri, a missing capability rejects the promise,
  // and none of that may break the theme.
  try {
    const w = (window as unknown as {
      __TAURI__?: { window?: { getCurrentWindow?: () => { setTheme?: (t: "light" | "dark" | null) => Promise<void> | void } } };
    }).__TAURI__?.window?.getCurrentWindow?.();
    Promise.resolve(w?.setTheme?.(id === "system" ? null : themeMode(resolved))).catch(() => { /* native chrome keeps the OS theme */ });
  } catch { /* no Tauri or no API: nothing to sync */ }
}

/** Run cb whenever the OS flips its theme, for as long as the setting is "system".
 *  Returns the unsubscribe function. */
export function watchSystemTheme(cb: () => void): () => void {
  try {
    const mq = window.matchMedia("(prefers-color-scheme: dark)");
    const on = () => cb();
    mq.addEventListener("change", on);
    return () => mq.removeEventListener("change", on);
  } catch {
    return () => {}; // no matchMedia: the theme just refreshes on the next applyTheme
  }
}
