// Light / dark / follow-Windows theme (Settings › General › Theme). applyTheme sets
// `data-theme` ("light" | "dark") on <html>, which is what the token blocks in base.css key
// off — "system" is resolved here, through matchMedia, so the OS choice stays live (see
// watchSystemTheme). The choice is remembered in localStorage ("winseek.theme") and applied
// at boot by both windows; a storage event carries a change made in one window to the other
// (main.tsx), like the accent. In Tauri the native window chrome follows too (library-window
// titlebar). No imports so `node --test` can load it.

export const THEMES = [
  { id: "system", label: "System" },
  { id: "light", label: "Light" },
  { id: "dark", label: "Dark" },
] as const;

export type ThemeId = (typeof THEMES)[number]["id"];

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

/** The concrete theme a setting means right now ("system" = whatever the OS says). Pure. */
export function resolveTheme(id: ThemeId, osPrefersDark: boolean): "light" | "dark" {
  if (id === "dark") return "dark";
  if (id === "light") return "light";
  return osPrefersDark ? "dark" : "light";
}

export function applyTheme(id: ThemeId): void {
  let osPrefersDark = false;
  try {
    osPrefersDark = window.matchMedia("(prefers-color-scheme: dark)").matches;
  } catch { /* no matchMedia: OS counts as light */ }
  const resolved = resolveTheme(id, osPrefersDark);
  document.documentElement.dataset.theme = resolved;
  // The native window chrome follows as well; on "system" hand the decision back to the OS.
  // Best effort: the browser mock has no Tauri, a missing capability rejects the promise, and
  // none of that may break the theme.
  try {
    const w = (window as unknown as {
      __TAURI__?: { window?: { getCurrentWindow?: () => { setTheme?: (t: "light" | "dark" | null) => Promise<void> | void } } };
    }).__TAURI__?.window?.getCurrentWindow?.();
    Promise.resolve(w?.setTheme?.(id === "system" ? null : resolved)).catch(() => { /* native chrome keeps the OS theme */ });
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
