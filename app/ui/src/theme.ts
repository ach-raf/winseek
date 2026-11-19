// Theme registry (Settings › Appearance › Theme). "system" follows Windows; every other entry
// names one concrete palette, and each palette declares the mode it belongs to — applyTheme
// sets `data-theme` on <html> to that resolved palette id, which is what the palette blocks in
// base.css key off ("light" and "dark" are palettes too, the originals; "latte"/"mocha" are the
// Catppuccin pair, "dracula"/"tokyo" the dark classics, "dawn"/"flexoki" more light takes).
// "system" itself never lands in data-theme: it is resolved here, through
// matchMedia, to the original palette of the OS's mode, so the OS choice stays live (see
// watchSystemTheme). The choice is remembered in localStorage ("winseek.theme") and applied at
// boot by both windows; a storage event carries a change made in one window to the other
// (main.tsx), like the accent. In Tauri the native window chrome follows the palette's mode
// (library-window titlebar). No imports so `node --test` can load it.
//
// Custom themes ("Save your own palette", edited in Settings › Appearance) live in the same
// setting as a reference of the form "custom:<id>". A custom theme = a record (below) holding a
// base palette it started from, its accent, and one colour per editable role. applyTheme paints
// it by pointing data-theme at the record's base — so the family block in base.css (and every
// derived formula in it: the --win-alpha transparency mixes, --sel/--mark off the accent, the
// dark rail's accent mix) keeps doing the heavy lifting — and then laying the record's colours
// over it as inline custom properties on <html>, which beat every stylesheet rule. Switching
// away removes exactly those role tokens again, so nothing leaks into the built-in palettes.
// The registry itself is remembered in localStorage ("winseek.customThemes").

export const THEMES = [
  { id: "system", label: "System" },
  { id: "light", label: "Light" },
  { id: "dark", label: "Dark" },
  { id: "latte", label: "Latte" },
  { id: "mocha", label: "Mocha" },
  { id: "dracula", label: "Dracula" },
  { id: "tokyo", label: "Tokyo Night" },
  { id: "dawn", label: "Dawn" },
  { id: "flexoki", label: "Flexoki" },
] as const;

export type BuiltInThemeId = (typeof THEMES)[number]["id"];
export type ConcreteTheme = Exclude<BuiltInThemeId, "system">;
/** Reference to a user-made palette stored in the custom registry. */
export type CustomThemeId = `custom:${string}`;
export type ThemeId = BuiltInThemeId | CustomThemeId;

/** Which family a concrete palette belongs to: "system" resolves to the family's original
 *  ("light" or "dark"), and the native window chrome follows the mode. */
const MODE: Record<ConcreteTheme, "light" | "dark"> = {
  light: "light", latte: "light", dawn: "light", flexoki: "light",
  dark: "dark", mocha: "dark", dracula: "dark", tokyo: "dark",
};

export function themeMode(id: ConcreteTheme): "light" | "dark" {
  return MODE[id];
}

const KEY = "winseek.theme";

// ---------------------------------------------------------------------------
// Custom themes: the record, the editable roles, and the registry storage.
// ---------------------------------------------------------------------------

/** One colour role the editor exposes. The token must be a *source* token in the palette blocks
 *  of base.css (a plain hex the block's own formulas read from), never a derived one — the
 *  derivations (transparency, selection, rail accent mix) then keep working untouched.
 *  `darkOnly` roles exist only in dark blocks (light rail cards share the cards colour). */
export type ThemeRole = { token: string; label: string; group: "surfaces" | "text" | "code"; darkOnly?: boolean };

export const ROLES: ThemeRole[] = [
  { token: "--bg-0", label: "Canvas", group: "surfaces" },
  { token: "--bg2-0", label: "Cards", group: "surfaces" },
  { token: "--bg3-0", label: "Controls", group: "surfaces" },
  { token: "--line", label: "Hairlines", group: "surfaces" },
  { token: "--side-0", label: "Sidebar rail", group: "surfaces" },
  { token: "--side-card-0", label: "Rail cards", group: "surfaces", darkOnly: true },
  { token: "--paper", label: "Paper sheets", group: "surfaces" },
  { token: "--fg", label: "Text", group: "text" },
  { token: "--fg2", label: "Secondary text", group: "text" },
  { token: "--fg3", label: "Muted text", group: "text" },
  { token: "--tok-keyword", label: "Keywords", group: "code" },
  { token: "--tok-string", label: "Strings", group: "code" },
  { token: "--tok-number", label: "Numbers", group: "code" },
  { token: "--tok-function", label: "Functions", group: "code" },
  { token: "--tok-class", label: "Classes", group: "code" },
  { token: "--tok-deleted", label: "Deleted lines", group: "code" },
  { token: "--tok-inserted", label: "Inserted lines", group: "code" },
];

/** A user-made palette. `colors` is what the user sees now; `baseColors` freezes the palette it
 *  was seeded from, so "reset this colour" and "reset all" work without re-reading CSS, and a
 *  later tweak to a built-in palette never rewrites a saved theme behind the user's back. */
export type CustomTheme = {
  id: string;
  name: string;
  base: ConcreteTheme;
  accent: string; // "#rrggbb", applied through the accent mechanism on activation
  baseAccent: string; // accent at creation — the "reset" target
  colors: Record<string, string>; // role token → "#rrggbb"; missing/unknown tokens inherit the base
  baseColors: Record<string, string>;
  createdAt: number;
};

const CUSTOM_KEY = "winseek.customThemes";
const CUSTOM_PREFIX = "custom:";
const CUSTOM_ID_RE = /^custom:[a-z0-9]{6,20}$/;

export function isCustomThemeId(v: string): v is CustomThemeId {
  return CUSTOM_ID_RE.test(v);
}

/** "c1a2b3" → "custom:c1a2b3". */
export function customThemeRef(id: string): CustomThemeId {
  return `custom:${id}`;
}

/** "custom:c1a2b3" → "c1a2b3", anything else → null. */
export function customThemeIdOf(ref: string): string | null {
  return isCustomThemeId(ref) ? ref.slice(CUSTOM_PREFIX.length) : null;
}

/** Short, unique-enough, and lowercase so it always satisfies CUSTOM_ID_RE. */
export function makeCustomThemeId(): string {
  return `${Date.now().toString(36)}${Math.random().toString(36).slice(2, 6)}`;
}

/** "#ABC" → "#aabbcc", "#AABBCC" → "#aabbcc", anything else → null. */
export function normalizeThemeHex(v: unknown): string | null {
  if (typeof v !== "string") return null;
  if (!/^#([0-9a-f]{3}|[0-9a-f]{6})$/i.test(v)) return null;
  const h = v.slice(1);
  const six = h.length === 3 ? h.split("").map((c) => c + c).join("") : h;
  return `#${six.toLowerCase()}`;
}

/** Keep only known role tokens, keep only well-formed hexes. */
export function sanitizeThemeColors(raw: unknown): Record<string, string> {
  const out: Record<string, string> = {};
  if (!raw || typeof raw !== "object") return out;
  const allowed = new Set(ROLES.map((r) => r.token));
  for (const [k, v] of Object.entries(raw as Record<string, unknown>)) {
    if (!allowed.has(k)) continue;
    const hex = normalizeThemeHex(v);
    if (hex) out[k] = hex;
  }
  return out;
}

const CONCRETE_IDS: readonly string[] = THEMES.map((t) => t.id).filter((id) => id !== "system");

/** Coerce an unknown blob (localStorage, import paste) into a record, or null. Lenient: a
 *  slightly off record is repaired (bad colours dropped, missing snapshots backfilled), never
 *  thrown away wholesale. */
export function sanitizeCustomTheme(raw: unknown): CustomTheme | null {
  if (!raw || typeof raw !== "object") return null;
  const r = raw as Record<string, unknown>;
  const id = typeof r.id === "string" && /^[a-z0-9]{6,20}$/.test(r.id) ? r.id : null;
  const base = CONCRETE_IDS.includes(r.base as string) ? (r.base as ConcreteTheme) : null;
  if (!id || !base) return null;
  const colors = sanitizeThemeColors(r.colors);
  const baseColors = sanitizeThemeColors(r.baseColors);
  return {
    id,
    name: typeof r.name === "string" && r.name.trim() ? r.name.trim().slice(0, 40) : "Custom theme",
    base,
    accent: normalizeThemeHex(r.accent) ?? "#ffc533",
    baseAccent: normalizeThemeHex(r.baseAccent) ?? normalizeThemeHex(r.accent) ?? "#ffc533",
    colors,
    baseColors: Object.keys(baseColors).length ? baseColors : { ...colors },
    createdAt: typeof r.createdAt === "number" && Number.isFinite(r.createdAt) ? r.createdAt : Date.now(),
  };
}

export function loadCustomThemes(): CustomTheme[] {
  try {
    const raw = localStorage.getItem(CUSTOM_KEY);
    if (!raw) return [];
    const parsed: unknown = JSON.parse(raw);
    if (!Array.isArray(parsed)) return [];
    return parsed.map(sanitizeCustomTheme).filter((t): t is CustomTheme => t !== null);
  } catch {
    return []; // unparsable registry: no custom themes (the built-ins are untouched)
  }
}

export function saveCustomThemes(list: CustomTheme[]): void {
  try { localStorage.setItem(CUSTOM_KEY, JSON.stringify(list)); } catch { /* remembered only when storage works */ }
}

/** Insert or replace one record (by id), keeping the others in order. */
export function upsertCustomTheme(rec: CustomTheme): CustomTheme[] {
  const list = loadCustomThemes();
  const at = list.findIndex((t) => t.id === rec.id);
  if (at >= 0) list[at] = rec;
  else list.push(rec);
  saveCustomThemes(list);
  return list;
}

export function removeCustomTheme(id: string): CustomTheme[] {
  const list = loadCustomThemes().filter((t) => t.id !== id);
  saveCustomThemes(list);
  return list;
}

export function loadTheme(): ThemeId {
  try {
    const v = localStorage.getItem(KEY);
    if (v && isCustomThemeId(v)) {
      // A reference only counts while the record still exists (it may have been deleted in the
      // other window).
      return loadCustomThemes().some((t) => customThemeRef(t.id) === v) ? v : "system";
    }
    return THEMES.some((t) => t.id === v) ? (v as BuiltInThemeId) : "system";
  } catch {
    return "system"; // storage unavailable: follow the OS (base.css ships light first)
  }
}

export function saveTheme(id: ThemeId): void {
  try { localStorage.setItem(KEY, id); } catch { /* remembered only when storage works */ }
}

/** The concrete palette currently painted (dataset.theme), or null right after boot.
 *  Custom themes report their base — the palette their colours sit on. */
export function currentConcreteTheme(): ConcreteTheme | null {
  const v = document.documentElement.dataset.theme;
  return CONCRETE_IDS.includes(v as string) ? (v as ConcreteTheme) : null;
}

/** The active custom record, if the setting points at one. */
export function activeCustomTheme(): CustomTheme | null {
  const id = customThemeIdOf(loadTheme());
  return id ? loadCustomThemes().find((t) => t.id === id) ?? null : null;
}

/** The concrete palette a setting means right now ("system" = the OS mode's original palette).
 *  Pure. Custom references are resolved by applyTheme through their record — this function is
 *  only ever called with built-ins. */
export function resolveTheme(id: BuiltInThemeId, osPrefersDark: boolean): ConcreteTheme {
  if (id === "system") return osPrefersDark ? "dark" : "light";
  return id;
}

/** Lay the role colours over the palette blocks as inline custom properties (they beat every
 *  stylesheet rule), or — with null — remove exactly those tokens again so nothing leaks from a
 *  custom theme into the built-in palettes. Unknown tokens and bad hexes never reach the DOM. */
function setThemeColors(colors: Record<string, string> | null): void {
  const st = document.documentElement.style;
  for (const role of ROLES) {
    const hex = colors ? normalizeThemeHex(colors[role.token]) : null;
    if (hex) st.setProperty(role.token, hex);
    else st.removeProperty(role.token);
  }
}

export function applyTheme(id: ThemeId): void {
  let osPrefersDark = false;
  try {
    osPrefersDark = window.matchMedia("(prefers-color-scheme: dark)").matches;
  } catch { /* no matchMedia: OS counts as light */ }
  const rec = isCustomThemeId(id)
    ? loadCustomThemes().find((t) => customThemeRef(t.id) === id) ?? null
    : null;
  // A custom record paints its base palette; a dangling reference (record deleted elsewhere)
  // falls back to the OS mode rather than a half-painted page.
  const resolved: ConcreteTheme = rec ? rec.base : resolveTheme(id === "system" || isCustomThemeId(id) ? "system" : id, osPrefersDark);
  setThemeColors(rec ? rec.colors : null);
  document.documentElement.dataset.theme = resolved;
  // The native window chrome follows the palette's mode; on "system" — or on a dangling custom
  // reference — hand the decision back to the OS. Best effort: the browser mock has no Tauri, a
  // missing capability rejects the promise, and none of that may break the theme.
  const chrome: "light" | "dark" | null =
    id === "system" || (isCustomThemeId(id) && !rec) ? null : themeMode(resolved);
  try {
    const w = (window as unknown as {
      __TAURI__?: { window?: { getCurrentWindow?: () => { setTheme?: (t: "light" | "dark" | null) => Promise<void> | void } } };
    }).__TAURI__?.window?.getCurrentWindow?.();
    Promise.resolve(w?.setTheme?.(chrome)).catch(() => { /* native chrome keeps the OS theme */ });
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
